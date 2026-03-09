use anyhow::{bail, Context, Result};
use blake3::Hasher;
use quinn::Connection;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tauri::AppHandle;
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tokio::sync::{oneshot, Mutex};

use crate::state::{AppState, TransferDirection, TransferStatus};
use crate::transport::events::{
    emit_transfer_accepted, emit_transfer_complete, emit_transfer_error, emit_transfer_partial,
    emit_transfer_progress, emit_transfer_started, emit_transfer_terminal,
};
use crate::transport::protocol::{
    read_message, write_message, ChunkPayload, CompletePayload, DashMessage, ErrorCode, FailedFile,
    FileItem, FileType, OfferPayload, TransferOutcome, CHUNK_SIZE, USER_RESPONSE_TIMEOUT_SECS,
};

type AckResult = (u32, bool, Option<ErrorCode>);
type AckWaiters = Arc<Mutex<HashMap<u32, oneshot::Sender<(bool, Option<ErrorCode>)>>>>;

fn error_code_key(code: &ErrorCode) -> String {
    code.reason_code().to_string()
}

/// Send files to a remote peer.
///
/// Flow: connect → Hello → Offer → Accept → Chunk streams → Complete → Ack
pub async fn send_files(
    transfer_id: String,
    peer_fp: String,
    paths: Vec<PathBuf>,
    conn: Connection,
    app: AppHandle,
    state: Arc<AppState>,
) -> Result<TransferOutcome> {
    tracing::info!(
        transfer_id = %transfer_id,
        peer_fp = %peer_fp,
        phase = "send_prepare",
        path_count = paths.len(),
        "starting send transfer"
    );

    // Gather file items
    let items_with_paths = build_file_items(&paths).await?;
    if items_with_paths.is_empty() {
        bail!("no sendable files found");
    }

    let items: Vec<FileItem> = items_with_paths.iter().map(|(i, _)| i.clone()).collect();

    let total_size: u64 = items.iter().map(|f| f.size).sum();
    let peer_name = {
        let devices = state.devices.read().await;
        devices
            .get(&peer_fp)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| "Unknown".into())
    };

    // Register transfer
    {
        let mut transfers = state.transfers.write().await;
        transfers.insert(
            transfer_id.clone(),
            crate::state::TransferTask {
                id: transfer_id.clone(),
                direction: TransferDirection::Send,
                peer_fingerprint: peer_fp.clone(),
                peer_name: peer_name.clone(),
                items: items
                    .iter()
                    .map(|f| crate::state::FileItemMeta {
                        file_id: f.file_id,
                        name: f.name.clone(),
                        rel_path: f.rel_path.clone(),
                        size: f.size,
                    })
                    .collect(),
                status: TransferStatus::PendingAccept,
                bytes_transferred: 0,
                total_bytes: total_size,
                revision: 0,
                ended_at_unix: None,
                error: None,
                source_paths: Some(
                    paths
                        .iter()
                        .map(|p| p.to_string_lossy().to_string())
                        .collect(),
                ),
                conn: Some(conn.clone()),
                ended_at: None,
            },
        );
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

    emit_transfer_started(
        &app,
        &transfer_id,
        &peer_fp,
        &peer_name,
        &items,
        total_size,
        0,
    );

    let accept = match tokio::time::timeout(
        std::time::Duration::from_secs(USER_RESPONSE_TIMEOUT_SECS),
        read_message(&mut control_recv),
    )
    .await
    {
        Ok(Ok(DashMessage::Accept(a))) => a,
        Ok(Ok(DashMessage::Reject(r))) => {
            let mut metrics_snapshot: Option<(
                crate::state::TransferDirection,
                crate::state::TransferStatus,
                u64,
            )> = None;
            let mut transfers = state.transfers.write().await;
            if let Some(t) = transfers.get_mut(&transfer_id) {
                t.status = TransferStatus::Rejected;
                t.revision += 1;
                t.ended_at = Some(std::time::Instant::now());
                t.ended_at_unix = Some(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                );
                if let Ok(guard) = state.db.lock() {
                    let _ = crate::db::save_transfer(&guard, t);
                }
                emit_transfer_terminal(
                    &app,
                    &transfer_id,
                    &t.status,
                    &error_code_key(&r.reason),
                    "RejectedByReceiver",
                    t.revision,
                    Some("offer"),
                );
                metrics_snapshot =
                    Some((t.direction.clone(), t.status.clone(), t.bytes_transferred));
            }
            drop(transfers);
            if let Some((direction, status, bytes)) = metrics_snapshot {
                state
                    .record_transfer_terminal(&direction, &status, bytes)
                    .await;
            }
            state.mark_peer_used(&peer_fp).await;
            return Ok(TransferOutcome::Failed(r.reason));
        }
        Ok(Ok(other)) => bail!("expected Accept/Reject, got {:?}", other),
        Ok(Err(e)) => return Err(e),
        Err(_) => {
            let mut metrics_snapshot: Option<(
                crate::state::TransferDirection,
                crate::state::TransferStatus,
                u64,
            )> = None;
            let mut transfers = state.transfers.write().await;
            if let Some(t) = transfers.get_mut(&transfer_id) {
                t.status = TransferStatus::Failed;
                t.revision += 1;
                t.error = Some("Offer response timeout".to_string());
                t.ended_at = Some(std::time::Instant::now());
                t.ended_at_unix = Some(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                );
                if let Ok(guard) = state.db.lock() {
                    let _ = crate::db::save_transfer(&guard, t);
                }
                emit_transfer_terminal(
                    &app,
                    &transfer_id,
                    &t.status,
                    ErrorCode::Timeout.reason_code(),
                    "Timeout",
                    t.revision,
                    Some("offer"),
                );
                emit_transfer_error(
                    &app,
                    Some(&transfer_id),
                    ErrorCode::Timeout.reason_code(),
                    "Timeout",
                    "offer",
                    t.revision,
                );
                metrics_snapshot =
                    Some((t.direction.clone(), t.status.clone(), t.bytes_transferred));
            }
            drop(transfers);
            if let Some((direction, status, bytes)) = metrics_snapshot {
                state
                    .record_transfer_terminal(&direction, &status, bytes)
                    .await;
            }
            return Ok(TransferOutcome::Failed(ErrorCode::Timeout));
        }
    };

    if accept.chosen_version != _chosen_version {
        bail!("Peer chose a different protocol version after handshake");
    }

    // Update status
    let transferring_revision = {
        let mut transfers = state.transfers.write().await;
        if let Some(t) = transfers.get_mut(&transfer_id) {
            t.status = TransferStatus::Transferring;
            t.revision += 1;
            t.revision
        } else {
            0
        }
    };
    emit_transfer_accepted(&app, &transfer_id, transferring_revision);
    state.mark_peer_used(&peer_fp).await;

    // Send files concurrently, bounded by runtime config
    let max_streams = state.config.read().await.max_parallel_streams.clamp(1, 32);
    let sem = Arc::new(tokio::sync::Semaphore::new(max_streams as usize));
    let mut handles = Vec::new();
    let ack_waiters: AckWaiters = Arc::new(Mutex::new(HashMap::new()));
    let cancelled_by_peer = Arc::new(AtomicBool::new(false));
    let dispatch_error: Arc<Mutex<Option<ErrorCode>>> = Arc::new(Mutex::new(None));

    // Single consumer for all inbound Ack streams to avoid concurrent accept_uni races.
    let ack_dispatch_conn = conn.clone();
    let ack_dispatch_waiters = ack_waiters.clone();
    let ack_dispatch_cancelled = cancelled_by_peer.clone();
    let ack_dispatch_error = dispatch_error.clone();
    let ack_dispatch = tokio::spawn(async move {
        loop {
            let mut ack_recv = match ack_dispatch_conn.accept_uni().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!("Ack dispatcher stopped: {e}");
                    let reason = ErrorCode::Protocol(format!("ack dispatcher accept failed: {e}"));
                    *ack_dispatch_error.lock().await = Some(reason.clone());
                    let mut waiters = ack_dispatch_waiters.lock().await;
                    for (_file_id, tx) in waiters.drain() {
                        let _ = tx.send((false, Some(reason.clone())));
                    }
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
                Ok(DashMessage::Cancel(cancel)) => {
                    ack_dispatch_cancelled.store(true, Ordering::Relaxed);
                    let reason = match cancel.reason {
                        crate::transport::protocol::CancelReason::UserCancelled => {
                            ErrorCode::Cancelled
                        }
                        crate::transport::protocol::CancelReason::Error(error) => {
                            *ack_dispatch_error.lock().await = Some(error.clone());
                            error
                        }
                    };
                    let mut waiters = ack_dispatch_waiters.lock().await;
                    for (_file_id, tx) in waiters.drain() {
                        let _ = tx.send((false, Some(reason.clone())));
                    }
                    break;
                }
                Ok(other) => {
                    tracing::warn!("Ack dispatcher got unexpected message: {:?}", other);
                }
                Err(e) => {
                    tracing::warn!("Ack dispatcher read error: {e:#}");
                    let reason = ErrorCode::Protocol(format!("ack dispatcher read failed: {e:#}"));
                    *ack_dispatch_error.lock().await = Some(reason.clone());
                    let mut waiters = ack_dispatch_waiters.lock().await;
                    for (_file_id, tx) in waiters.drain() {
                        let _ = tx.send((false, Some(reason.clone())));
                    }
                    break;
                }
            }
        }
    });

    // Build proper file_id → path mapping from items_with_paths
    let file_id_to_path: HashMap<u32, PathBuf> = items_with_paths
        .into_iter()
        .map(|(i, p)| (i.file_id, p))
        .collect();

    for item in &items {
        let conn2 = conn.clone();
        let app2 = app.clone();
        let state2 = state.clone();
        let item2 = item.clone();
        let sem2 = sem.clone();
        let transfer_id_clone = transfer_id.clone();
        let ack_waiters2 = ack_waiters.clone();

        let src_path = file_id_to_path.get(&item.file_id).cloned();
        let handle = tokio::spawn(async move {
            let Ok(_permit) = sem2.acquire().await else {
                return Ok((
                    item2.file_id,
                    false,
                    Some(ErrorCode::Protocol("Semaphore closed".into())),
                ));
            };
            match item2.file_type {
                FileType::RegularFile => {
                    let Some(path) = src_path else {
                        return Ok((
                            item2.file_id,
                            false,
                            Some(ErrorCode::Protocol("missing source path".into())),
                        ));
                    };
                    send_one_file(
                        item2,
                        path,
                        conn2,
                        app2,
                        state2,
                        transfer_id_clone,
                        ack_waiters2,
                    )
                    .await
                }
                FileType::Directory => send_one_directory(item2, conn2, ack_waiters2).await,
            }
        });
        handles.push(handle);
    }

    // Wait for all Acks
    let mut ack_results: Vec<(u32, bool, Option<ErrorCode>)> = Vec::new();
    for handle in handles {
        match handle.await {
            Ok(Ok(r)) => ack_results.push(r),
            Ok(Err(e)) => {
                tracing::error!(
                    transfer_id = %transfer_id,
                    peer_fp = %peer_fp,
                    phase = "file_send",
                    reason = %e,
                    "send task error"
                );
            }
            Err(e) => {
                tracing::error!(
                    transfer_id = %transfer_id,
                    peer_fp = %peer_fp,
                    phase = "file_send_join",
                    reason = %e,
                    "send task join error"
                );
            }
        }
    }

    let outcome = determine_outcome(
        cancelled_by_peer.load(Ordering::Relaxed),
        ack_results,
        &items,
        dispatch_error.lock().await.clone(),
    );

    // Update final state
    let mut metrics_snapshot: Option<(
        crate::state::TransferDirection,
        crate::state::TransferStatus,
        u64,
    )> = None;
    {
        let mut transfers = state.transfers.write().await;
        if let Some(t) = transfers.get_mut(&transfer_id) {
            t.status = match &outcome {
                TransferOutcome::Success => TransferStatus::Completed,
                TransferOutcome::PartialSuccess(_) => TransferStatus::PartialCompleted,
                TransferOutcome::Failed(_) => TransferStatus::Failed,
                TransferOutcome::CancelledBySender => TransferStatus::CancelledBySender,
                TransferOutcome::CancelledByReceiver => TransferStatus::CancelledByReceiver,
            };
            t.revision += 1;
            t.ended_at = Some(std::time::Instant::now());
            t.ended_at_unix = Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            );
            if let Ok(guard) = state.db.lock() {
                let _ = crate::db::save_transfer(&guard, t);
            }
            match &outcome {
                TransferOutcome::Success => {
                    emit_transfer_complete(&app, &transfer_id, t.revision);
                }
                TransferOutcome::PartialSuccess(failed) => {
                    emit_transfer_partial(
                        &app,
                        &transfer_id,
                        items.len().saturating_sub(failed.len()),
                        failed,
                        None,
                        t.revision,
                    );
                }
                TransferOutcome::Failed(code) => emit_transfer_terminal(
                    &app,
                    &transfer_id,
                    &t.status,
                    &error_code_key(code),
                    "TransferFailed",
                    t.revision,
                    Some("send"),
                ),
                TransferOutcome::CancelledByReceiver => emit_transfer_terminal(
                    &app,
                    &transfer_id,
                    &t.status,
                    "E_CANCELLED_BY_RECEIVER",
                    "CancelledByReceiver",
                    t.revision,
                    Some("send"),
                ),
                TransferOutcome::CancelledBySender => emit_transfer_terminal(
                    &app,
                    &transfer_id,
                    &t.status,
                    "E_CANCELLED_BY_SENDER",
                    "CancelledBySender",
                    t.revision,
                    Some("send"),
                ),
            }
            if let TransferOutcome::Failed(code) = &outcome {
                emit_transfer_error(
                    &app,
                    Some(&transfer_id),
                    &error_code_key(code),
                    "TransferFailed",
                    "send",
                    t.revision,
                );
            }
            tracing::info!(
                transfer_id = %transfer_id,
                peer_fp = %peer_fp,
                phase = "send_terminal",
                status = ?t.status,
                revision = t.revision,
                "send transfer reached terminal state"
            );
            metrics_snapshot = Some((t.direction.clone(), t.status.clone(), t.bytes_transferred));
        }
    }
    if let Some((direction, status, bytes)) = metrics_snapshot {
        state
            .record_transfer_terminal(&direction, &status, bytes)
            .await;
    }

    ack_dispatch.abort();

    Ok(outcome)
}

fn determine_outcome(
    cancelled_by_peer: bool,
    ack_results: Vec<AckResult>,
    items: &[FileItem],
    dispatch_error: Option<ErrorCode>,
) -> TransferOutcome {
    if let Some(error) = dispatch_error {
        return TransferOutcome::Failed(error);
    }

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
        TransferOutcome::CancelledByReceiver
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
    ack_waiters: AckWaiters,
) -> Result<AckResult> {
    let file_id = item.file_id;
    let (ack_tx, ack_rx) = oneshot::channel();
    ack_waiters.lock().await.insert(file_id, ack_tx);

    // Open a unidirectional stream for this file
    let mut send = match conn.open_uni().await {
        Ok(stream) => stream,
        Err(e) => {
            ack_waiters.lock().await.remove(&file_id);
            return Err(e).context("open uni stream");
        }
    };

    let mut file = match File::open(&src_path).await {
        Ok(file) => file,
        Err(e) => {
            ack_waiters.lock().await.remove(&file_id);
            return Err(e).context("open source file");
        }
    };
    let mut hasher = Hasher::new();
    let mut chunk_id = 0u32;
    let mut offset = 0u64;
    let mut buf = vec![0u8; CHUNK_SIZE];

    loop {
        let n = match file.read(&mut buf).await {
            Ok(n) => n,
            Err(e) => {
                ack_waiters.lock().await.remove(&file_id);
                return Err(e).context("read file");
            }
        };
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
        if let Err(e) = write_message(&mut send, &chunk).await {
            ack_waiters.lock().await.remove(&file_id);
            return Err(e);
        }

        offset += n as u64;
        chunk_id += 1;

        let (overall_transferred, total_bytes, revision) = {
            let mut transfers = state.transfers.write().await;
            if let Some(t) = transfers.get_mut(&transfer_id) {
                t.bytes_transferred += n as u64;
                (t.bytes_transferred, t.total_bytes, t.revision)
            } else {
                continue;
            }
        };

        emit_transfer_progress(
            &app,
            &transfer_id,
            overall_transferred,
            total_bytes,
            revision,
        );
    }

    // Send Complete with BLAKE3 hash
    let file_hash: [u8; 32] = hasher.finalize().into();
    if let Err(e) = write_message(
        &mut send,
        &DashMessage::Complete(CompletePayload { file_id, file_hash }),
    )
    .await
    {
        ack_waiters.lock().await.remove(&file_id);
        return Err(e);
    }

    // Finish the send stream
    if let Err(e) = send.finish() {
        ack_waiters.lock().await.remove(&file_id);
        return Err(e).context("finish stream");
    }

    match tokio::time::timeout(std::time::Duration::from_secs(30), ack_rx).await {
        Ok(Ok((ok, reason))) => Ok((file_id, ok, reason)),
        Ok(Err(_)) => Ok((
            file_id,
            false,
            Some(ErrorCode::Protocol("ack channel closed".into())),
        )),
        Err(_) => {
            ack_waiters.lock().await.remove(&file_id);
            Ok((
                file_id,
                false,
                Some(ErrorCode::Protocol("ack timeout".into())),
            ))
        }
    }
}

async fn send_one_directory(
    item: FileItem,
    conn: Connection,
    ack_waiters: AckWaiters,
) -> Result<AckResult> {
    let file_id = item.file_id;
    let (ack_tx, ack_rx) = oneshot::channel();
    ack_waiters.lock().await.insert(file_id, ack_tx);

    let mut send = match conn.open_uni().await {
        Ok(stream) => stream,
        Err(e) => {
            ack_waiters.lock().await.remove(&file_id);
            return Err(e).context("open uni stream for directory");
        }
    };

    let file_hash: [u8; 32] = blake3::hash(&[]).into();
    if let Err(e) = write_message(
        &mut send,
        &DashMessage::Complete(CompletePayload { file_id, file_hash }),
    )
    .await
    {
        ack_waiters.lock().await.remove(&file_id);
        return Err(e).context("send directory complete");
    }

    if let Err(e) = send.finish() {
        ack_waiters.lock().await.remove(&file_id);
        return Err(e).context("finish directory stream");
    }

    match tokio::time::timeout(std::time::Duration::from_secs(30), ack_rx).await {
        Ok(Ok((ok, reason))) => Ok((file_id, ok, reason)),
        Ok(Err(_)) => Ok((
            file_id,
            false,
            Some(ErrorCode::Protocol("ack channel closed".into())),
        )),
        Err(_) => {
            ack_waiters.lock().await.remove(&file_id);
            Ok((
                file_id,
                false,
                Some(ErrorCode::Protocol("ack timeout".into())),
            ))
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
    let meta = tokio::fs::symlink_metadata(path)
        .await
        .context("stat path")?;

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
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
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
        items.push((
            FileItem {
                file_id: *next_id,
                name,
                rel_path,
                size: 0,
                file_type: FileType::Directory,
                modified,
            },
            path.clone(),
        ));
        *next_id += 1;

        let mut dir = tokio::fs::read_dir(path).await?;
        while let Some(entry) = dir.next_entry().await? {
            let child_path = entry.path();
            Box::pin(collect_items(&child_path, base, items, next_id)).await?;
        }
    } else if meta.is_file() {
        // Only regular files; symlinks already skipped above
        items.push((
            FileItem {
                file_id: *next_id,
                name,
                rel_path,
                size: meta.len(),
                file_type: FileType::RegularFile,
                modified,
            },
            path.clone(),
        ));
        *next_id += 1;
    }
    // Skip symlinks, sockets, devices etc. (MVP spec)

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::determine_outcome;
    use crate::transport::protocol::{ErrorCode, FileItem, FileType, TransferOutcome};
    use tokio::task::JoinHandle;

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
        let outcome = determine_outcome(true, vec![(1, true, None)], &items, None);
        assert!(matches!(outcome, TransferOutcome::CancelledByReceiver));
    }

    #[test]
    fn partial_outcome_contains_failed_file() {
        let items = vec![file(1, "a.txt"), file(2, "b.txt")];
        let outcome = determine_outcome(
            false,
            vec![(1, true, None), (2, false, Some(ErrorCode::HashMismatch))],
            &items,
            None,
        );
        match outcome {
            TransferOutcome::PartialSuccess(failed) => {
                assert_eq!(failed.len(), 1);
                assert_eq!(failed[0].file_id, 2);
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[test]
    fn dispatch_error_forces_failed_outcome() {
        let items = vec![file(1, "a.txt")];
        let outcome = determine_outcome(
            false,
            vec![(1, true, None)],
            &items,
            Some(ErrorCode::Timeout),
        );
        assert!(matches!(
            outcome,
            TransferOutcome::Failed(ErrorCode::Timeout)
        ));
    }

    #[test]
    fn all_failed_outcome_is_failed() {
        let items = vec![file(1, "a.txt"), file(2, "b.txt")];
        let outcome = determine_outcome(
            false,
            vec![
                (1, false, Some(ErrorCode::Rejected)),
                (2, false, Some(ErrorCode::Rejected)),
            ],
            &items,
            None,
        );
        assert!(matches!(outcome, TransferOutcome::Failed(_)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn stress_regression_multi_file_over_100_rounds() {
        let item_count = 128u32;
        let rounds = 120u32;
        let items: Vec<FileItem> = (0..item_count)
            .map(|id| file(id, &format!("file-{id}.bin")))
            .collect();

        for round in 0..rounds {
            let cancelled_by_peer = round % 17 == 0;
            let dispatch_error = if round % 23 == 0 {
                Some(ErrorCode::Protocol("dispatch_error".into()))
            } else {
                None
            };

            let mut workers: Vec<JoinHandle<(u32, bool, Option<ErrorCode>)>> = Vec::new();
            for file_id in 0..item_count {
                let should_fail = !cancelled_by_peer && (file_id + round) % 11 == 0;
                workers.push(tokio::spawn(async move {
                    if should_fail {
                        (
                            file_id,
                            false,
                            Some(ErrorCode::Protocol("simulated_failure".into())),
                        )
                    } else if cancelled_by_peer {
                        (file_id, false, Some(ErrorCode::Cancelled))
                    } else {
                        (file_id, true, None)
                    }
                }));
            }

            let mut ack_results = Vec::with_capacity(item_count as usize);
            for worker in workers {
                ack_results.push(worker.await.expect("stress worker join"));
            }

            let expected_failed = if cancelled_by_peer {
                0
            } else {
                (0..item_count)
                    .filter(|file_id| (file_id + round) % 11 == 0)
                    .count()
            };

            let outcome = determine_outcome(
                cancelled_by_peer,
                ack_results,
                &items,
                dispatch_error.clone(),
            );

            if dispatch_error.is_some() {
                assert!(matches!(outcome, TransferOutcome::Failed(_)));
            } else if cancelled_by_peer {
                assert!(matches!(outcome, TransferOutcome::CancelledByReceiver));
            } else if expected_failed == 0 {
                assert!(matches!(outcome, TransferOutcome::Success));
            } else if expected_failed < item_count as usize {
                match outcome {
                    TransferOutcome::PartialSuccess(failed) => {
                        assert_eq!(failed.len(), expected_failed);
                    }
                    _ => panic!("expected PartialSuccess in round {round}"),
                }
            } else {
                assert!(matches!(outcome, TransferOutcome::Failed(_)));
            }

            // Failure recovery: after each stress round, a clean all-success batch
            // must still converge to Success.
            let clean_results: Vec<(u32, bool, Option<ErrorCode>)> = (0..item_count)
                .map(|file_id| (file_id, true, None))
                .collect();
            let recovery_outcome = determine_outcome(false, clean_results, &items, None);
            assert!(
                matches!(recovery_outcome, TransferOutcome::Success),
                "recovery must be Success at round {round}"
            );
        }
    }
}
