use anyhow::{bail, Context, Result};
use blake3::Hasher;
use quinn::Connection;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use tauri::{AppHandle, Emitter};
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tokio::sync::{oneshot, Mutex};

use crate::state::{AppState, TransferDirection, TransferStatus};
use crate::transport::protocol::{
    read_message, write_message, ChunkPayload,
    CompletePayload, DashMessage, ErrorCode, FailedFile, FileItem, FileType,
    OfferPayload, TransferOutcome, CHUNK_SIZE, MAX_CONCURRENT_STREAMS,
};

/// Send files to a remote peer.
///
/// Flow: connect → Hello → Offer → Accept → Chunk streams → Complete → Ack
pub async fn send_files(
    peer_fp: String,
    paths: Vec<PathBuf>,
    conn: Connection,
    app: AppHandle,
    state: Arc<AppState>,
) -> Result<TransferOutcome> {
    let transfer_id = uuid::Uuid::new_v4().to_string();

    // Gather file items
    let items_with_paths = build_file_items(&paths).await?;
    if items_with_paths.is_empty() {
        bail!("no sendable files found");
    }
    
    let items: Vec<FileItem> = items_with_paths.iter().map(|(i, _)| i.clone()).collect();

    let total_size: u64 = items.iter().map(|f| f.size).sum();
    let peer_name = {
        let devices = state.devices.read().await;
        devices.get(&peer_fp).map(|d| d.name.clone()).unwrap_or_else(|| "Unknown".into())
    };

    // Register transfer
    {
        let mut transfers = state.transfers.write().await;
        transfers.insert(transfer_id.clone(), crate::state::TransferTask {
            id: transfer_id.clone(),
            direction: TransferDirection::Send,
            peer_fingerprint: peer_fp.clone(),
            peer_name: peer_name.clone(),
            items: items.iter().map(|f| crate::state::FileItemMeta {
                file_id: f.file_id,
                name: f.name.clone(),
                rel_path: f.rel_path.clone(),
                size: f.size,
            }).collect(),
            status: TransferStatus::AwaitingAccept,
            bytes_transferred: 0,
            total_bytes: total_size,
            error: None,
            conn: Some(conn.clone()),
            ended_at: None,
        });
    }

    // Hello handshake
    let (mut control_send, mut control_recv, _chosen_version) =
        super::handshake::do_hello_as_initiator(&conn).await?;

    // Send Offer
    let sender_name = state.config.read().await.device_name.clone();
    let offer_payload = OfferPayload {
        transfer_id: transfer_id.clone(),
        items: items.clone(),
        total_size,
        sender_name,
        sender_fingerprint: state.identity.fingerprint.clone(),
    };
    write_message(&mut control_send, &DashMessage::Offer(offer_payload)).await?;

    app.emit("transfer_started", serde_json::json!({
        "transfer_id": transfer_id,
        "peer_fp": peer_fp,
        "peer_name": peer_name,
        "item_count": items.len(),
        "total_size": total_size,
    })).unwrap_or_else(|e| tracing::warn!("Emit failed: {e}"));

    let accept = match read_message(&mut control_recv).await? {
        DashMessage::Accept(a) => a,
        DashMessage::Reject(r) => {
            let mut transfers = state.transfers.write().await;
            if let Some(t) = transfers.get_mut(&transfer_id) {
                t.status = TransferStatus::Cancelled;
                t.ended_at = Some(std::time::Instant::now());
            }
            return Ok(TransferOutcome::Failed(r.reason));
        }
        other => bail!("expected Accept/Reject, got {:?}", other),
    };

    if accept.chosen_version != _chosen_version {
        bail!("Peer chose a different protocol version after handshake");
    }

    // Update status
    {
        let mut transfers = state.transfers.write().await;
        if let Some(t) = transfers.get_mut(&transfer_id) {
            t.status = TransferStatus::Transferring;
        }
    }

    // Send files concurrently, max 4 streams
    let sem = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_STREAMS as usize));
    let mut handles = Vec::new();
    let ack_waiters: Arc<Mutex<HashMap<u32, oneshot::Sender<(bool, Option<ErrorCode>)>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let cancelled_by_peer = Arc::new(AtomicBool::new(false));

    // Single consumer for all inbound Ack streams to avoid concurrent accept_uni races.
    let ack_dispatch_conn = conn.clone();
    let ack_dispatch_waiters = ack_waiters.clone();
    let ack_dispatch_cancelled = cancelled_by_peer.clone();
    let ack_dispatch = tokio::spawn(async move {
        loop {
            let mut ack_recv = match ack_dispatch_conn.accept_uni().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!("Ack dispatcher stopped: {e}");
                    break;
                }
            };

            match read_message(&mut ack_recv).await {
                Ok(DashMessage::Ack(ack)) => {
                    let mut waiters = ack_dispatch_waiters.lock().await;
                    if let Some(tx) = waiters.remove(&ack.file_id) {
                        let _ = tx.send((ack.ok, ack.reason));
                    } else {
                        tracing::warn!("Ack for unknown file_id {}", ack.file_id);
                    }
                }
                Ok(DashMessage::Cancel(_)) => {
                    ack_dispatch_cancelled.store(true, Ordering::Relaxed);
                    let mut waiters = ack_dispatch_waiters.lock().await;
                    for (_file_id, tx) in waiters.drain() {
                        let _ = tx.send((false, Some(ErrorCode::Cancelled)));
                    }
                    break;
                }
                Ok(other) => {
                    tracing::warn!("Ack dispatcher got unexpected message: {:?}", other);
                }
                Err(e) => {
                    tracing::warn!("Ack dispatcher read error: {e:#}");
                }
            }
        }
    });

    // Build proper file_id → path mapping from items_with_paths
    let file_id_to_path: HashMap<u32, PathBuf> = items_with_paths.into_iter()
        .filter(|(i, _)| i.file_type == FileType::RegularFile)
        .map(|(i, p)| (i.file_id, p))
        .collect();

    for item in items.iter().filter(|i| i.file_type == FileType::RegularFile) {
        let src_path = match file_id_to_path.get(&item.file_id) {
            Some(p) => p.clone(),
            None => continue,
        };

        let conn2 = conn.clone();
        let app2 = app.clone();
        let state2 = state.clone();
        let item2 = item.clone();
            let sem2 = sem.clone();
            let transfer_id_clone = transfer_id.clone();
            let ack_waiters2 = ack_waiters.clone();

            let handle = tokio::spawn(async move {
                let Ok(_permit) = sem2.acquire().await else {
                    return Ok((item2.file_id, false, Some(ErrorCode::Protocol("Semaphore closed".into()))));
                };
                send_one_file(item2, src_path, conn2, app2, state2, transfer_id_clone, ack_waiters2).await
            });
            handles.push(handle);
        }

    // Wait for all Acks
    let mut ack_results: Vec<(u32, bool, Option<ErrorCode>)> = Vec::new();
    for handle in handles {
        match handle.await {
            Ok(Ok(r)) => ack_results.push(r),
            Ok(Err(e)) => {
                tracing::error!("Send task error: {e:#}");
            }
            Err(e) => {
                tracing::error!("Task join error: {e}");
            }
        }
    }

    let outcome = determine_outcome(
        cancelled_by_peer.load(Ordering::Relaxed),
        ack_results,
        &items,
    );

    // Update final state
    {
        let mut transfers = state.transfers.write().await;
        if let Some(t) = transfers.get_mut(&transfer_id) {
            t.status = match &outcome {
                TransferOutcome::Success => TransferStatus::Complete,
                TransferOutcome::PartialSuccess(_) => TransferStatus::PartialSuccess,
                TransferOutcome::Failed(_) => TransferStatus::Failed,
                TransferOutcome::Cancelled => TransferStatus::Cancelled,
            };
            t.ended_at = Some(std::time::Instant::now());
        }
    }

    ack_dispatch.abort();

    Ok(outcome)
}

fn determine_outcome(
    cancelled_by_peer: bool,
    ack_results: Vec<(u32, bool, Option<ErrorCode>)>,
    items: &[FileItem],
) -> TransferOutcome {
    let mut success_count = 0usize;
    let mut failed: Vec<FailedFile> = Vec::new();

    for (file_id, ok, reason) in ack_results {
        if ok {
            success_count += 1;
        } else {
            let name = items
                .iter()
                .find(|i| i.file_id == file_id)
                .map(|i| i.name.clone())
                .unwrap_or_default();
            failed.push(FailedFile {
                file_id,
                name,
                reason: reason.unwrap_or(ErrorCode::Protocol("unknown".into())),
            });
        }
    }

    if cancelled_by_peer {
        TransferOutcome::Cancelled
    } else if failed.is_empty() {
        TransferOutcome::Success
    } else if success_count > 0 {
        TransferOutcome::PartialSuccess(failed)
    } else {
        TransferOutcome::Failed(ErrorCode::Protocol("all files failed".into()))
    }
}

async fn send_one_file(
    item: FileItem,
    src_path: PathBuf,
    conn: Connection,
    app: AppHandle,
    state: Arc<AppState>,
    transfer_id: String,
    ack_waiters: Arc<Mutex<HashMap<u32, oneshot::Sender<(bool, Option<ErrorCode>)>>>>,
) -> Result<(u32, bool, Option<ErrorCode>)> {
    let file_id = item.file_id;
    let (ack_tx, ack_rx) = oneshot::channel();
    ack_waiters.lock().await.insert(file_id, ack_tx);

    // Open a unidirectional stream for this file
    let mut send = conn.open_uni().await.context("open uni stream")?;

    let mut file = File::open(&src_path).await.context("open source file")?;
    let mut hasher = Hasher::new();
    let mut chunk_id = 0u32;
    let mut offset = 0u64;
    let mut buf = vec![0u8; CHUNK_SIZE];

    loop {
        let n = file.read(&mut buf).await.context("read file")?;
        if n == 0 {
            break;
        }
        let data = buf[..n].to_vec();
        hasher.update(&data);

        let chunk = DashMessage::Chunk(ChunkPayload {
            file_id,
            chunk_id,
            offset,
            data,
        });
        write_message(&mut send, &chunk).await?;

        offset += n as u64;
        chunk_id += 1;

        let overall_transferred = {
            let mut transfers = state.transfers.write().await;
            if let Some(t) = transfers.get_mut(&transfer_id) {
                t.bytes_transferred += n as u64;
                t.bytes_transferred
            } else {
                continue;
            }
        };

        app.emit("transfer_progress", serde_json::json!({
            "transfer_id": transfer_id,
            "bytes_transferred": overall_transferred,
        })).unwrap_or_else(|e| tracing::warn!("Emit failed: {e}"));
    }

    // Send Complete with BLAKE3 hash
    let file_hash: [u8; 32] = hasher.finalize().into();
    write_message(&mut send, &DashMessage::Complete(CompletePayload { file_id, file_hash })).await?;

    // Finish the send stream
    send.finish().context("finish stream")?;

    match tokio::time::timeout(std::time::Duration::from_secs(30), ack_rx).await {
        Ok(Ok((ok, reason))) => Ok((file_id, ok, reason)),
        Ok(Err(_)) => Ok((file_id, false, Some(ErrorCode::Protocol("ack channel closed".into())))),
        Err(_) => {
            ack_waiters.lock().await.remove(&file_id);
            Ok((file_id, false, Some(ErrorCode::Protocol("ack timeout".into()))))
        }
    }
}

/// Build FileItem list from a set of source paths.
/// Recursively expands directories.
async fn build_file_items(paths: &[PathBuf]) -> Result<Vec<(FileItem, PathBuf)>> {
    let mut items = Vec::new();
    let mut next_id = 0u32;

    for path in paths {
        collect_items(path, path, &mut items, &mut next_id).await?;
    }

    Ok(items)
}

async fn collect_items(
    path: &PathBuf,
    base: &PathBuf,
    items: &mut Vec<(FileItem, PathBuf)>,
    next_id: &mut u32,
) -> Result<()> {
    // Use symlink_metadata so we don't follow symlinks
    let meta = tokio::fs::symlink_metadata(path).await.context("stat path")?;
    
    // Skip symlinks, sockets, devices etc. per spec
    if meta.file_type().is_symlink() {
        tracing::debug!("Skipping symlink: {:?}", path);
        return Ok(());
    };

    let parent = base.parent().filter(|p| p != base);
    let rel_path = match parent {
        Some(p) => path.strip_prefix(p).unwrap_or(path),
        None => path.strip_prefix(base).unwrap_or(path),
    }
    .to_string_lossy()
    .replace('\\', "/")
    .trim_start_matches('/')
    .to_string();
    let rel_path = if rel_path.is_empty() {
        path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    } else {
        rel_path
    };

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| rel_path.clone());

    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if meta.is_dir() {
        items.push((FileItem {
            file_id: *next_id,
            name,
            rel_path,
            size: 0,
            file_type: FileType::Directory,
            modified,
        }, path.clone()));
        *next_id += 1;

        let mut dir = tokio::fs::read_dir(path).await?;
        while let Some(entry) = dir.next_entry().await? {
            let child_path = entry.path();
            Box::pin(collect_items(&child_path, base, items, next_id)).await?;
        }
    } else if meta.is_file() {
        // Only regular files; symlinks already skipped above
        items.push((FileItem {
            file_id: *next_id,
            name,
            rel_path,
            size: meta.len(),
            file_type: FileType::RegularFile,
            modified,
        }, path.clone()));
        *next_id += 1;
    }
    // Skip symlinks, sockets, devices etc. (MVP spec)

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::determine_outcome;
    use crate::transport::protocol::{ErrorCode, FileItem, FileType, TransferOutcome};

    fn file(file_id: u32, name: &str) -> FileItem {
        FileItem {
            file_id,
            name: name.to_string(),
            rel_path: name.to_string(),
            size: 1,
            file_type: FileType::RegularFile,
            modified: 0,
        }
    }

    #[test]
    fn cancelled_outcome_has_priority() {
        let items = vec![file(1, "a.txt")];
        let outcome = determine_outcome(true, vec![(1, true, None)], &items);
        assert!(matches!(outcome, TransferOutcome::Cancelled));
    }

    #[test]
    fn partial_outcome_contains_failed_file() {
        let items = vec![file(1, "a.txt"), file(2, "b.txt")];
        let outcome = determine_outcome(
            false,
            vec![(1, true, None), (2, false, Some(ErrorCode::HashMismatch))],
            &items,
        );
        match outcome {
            TransferOutcome::PartialSuccess(failed) => {
                assert_eq!(failed.len(), 1);
                assert_eq!(failed[0].file_id, 2);
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }
}
