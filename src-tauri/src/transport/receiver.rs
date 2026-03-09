use anyhow::{Context, Result};
use blake3::Hasher;
use quinn::Connection;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use sysinfo::Disks;
use tauri::{AppHandle, Emitter, Manager};
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;
use tokio::sync::{oneshot, Mutex};

use crate::state::{AppState, TransferDirection, TransferStatus};
use crate::transport::protocol::{
    read_message, write_message, AcceptPayload,
    DashMessage, ErrorCode, FailedFile, FileItem, FileType,
    OfferPayload, RejectPayload, TransferOutcome,
};
use crate::transport::path_validation::validate_rel_path;

struct RoutedIncomingFile {
    recv: quinn::RecvStream,
    first_msg: DashMessage,
}

fn routed_file_id(msg: &DashMessage) -> Option<u32> {
    match msg {
        DashMessage::Chunk(chunk) => Some(chunk.file_id),
        DashMessage::Complete(complete) => Some(complete.file_id),
        _ => None,
    }
}

fn io_error_code(e: &std::io::Error) -> ErrorCode {
    #[cfg(unix)]
    {
        if e.raw_os_error() == Some(28) {
            return ErrorCode::DiskFull;
        }
    }
    if e.kind() == std::io::ErrorKind::StorageFull {
        return ErrorCode::DiskFull;
    }
    ErrorCode::Protocol(e.to_string())
}

fn temp_path_for(final_path: &Path) -> PathBuf {
    let file_name = final_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    final_path.with_file_name(format!("{file_name}.dashdrop.part"))
}

pub async fn handle_offer(
    offer: OfferPayload,
    conn: Connection,
    mut control_send: quinn::SendStream,
    mut _control_recv: quinn::RecvStream,
    peer_fp: String,
    chosen_version: u32,
    app: AppHandle,
    state: Arc<AppState>,
) -> Result<()> {
    let transfer_id = offer.transfer_id;

    // Register in state
    let items_meta: Vec<_> = offer.items.iter().map(|f| crate::state::FileItemMeta {
        file_id: f.file_id,
        name: f.name.clone(),
        rel_path: f.rel_path.clone(),
        size: f.size,
    }).collect();

    {
        let mut transfers = state.transfers.write().await;
        transfers.insert(transfer_id.clone(), crate::state::TransferTask {
            id: transfer_id.clone(),
            direction: TransferDirection::Receive,
            peer_fingerprint: peer_fp.clone(),
            peer_name: offer.sender_name.clone(),
            items: items_meta,
            status: TransferStatus::AwaitingAccept,
            bytes_transferred: 0,
            total_bytes: offer.total_size,
            error: None,
            conn: Some(conn.clone()),
            ended_at: None,
        });
    }

    // Determine potential save root
    let save_root = get_save_root(&app, &state, transfer_id.clone()).await;

    // Check disk space before prompting
    let mut sufficient_space = true;
    let disks = Disks::new_with_refreshed_list();
    let mut max_prefix_len = 0;
    let mut matched_disk = None;

    for disk in &disks {
        if save_root.starts_with(disk.mount_point()) {
            let len = disk.mount_point().as_os_str().len();
            if len > max_prefix_len {
                max_prefix_len = len;
                matched_disk = Some(disk);
            }
        }
    }

    if let Some(disk) = matched_disk {
        if disk.available_space() < offer.total_size {
            sufficient_space = false;
        }
    }

    if !sufficient_space {
        write_message(&mut control_send, &DashMessage::Reject(RejectPayload {
            reason: ErrorCode::DiskFull,
        })).await?;
        update_transfer_status(&state, transfer_id.clone(), TransferStatus::Failed).await;
        {
            let mut transfers = state.transfers.write().await;
            if let Some(t) = transfers.get_mut(&transfer_id) {
                t.error = Some("Insufficient disk space".to_string());
            }
        }
        app.emit("transfer_failed", serde_json::json!({
            "transfer_id": transfer_id,
            "reason": "Receiver disk full",
            "phase": "preflight",
        })).unwrap_or_else(|e| tracing::warn!("Emit failed: {e}"));
        return Ok(());
    }

    // Emit incoming transfer event to frontend
    let trusted = state.is_trusted(&peer_fp).await;
    app.emit("transfer_incoming", serde_json::json!({
        "transfer_id": transfer_id,
        "sender_name": offer.sender_name,
        "sender_fp": peer_fp,
        "trusted": trusted,
        "items": offer.items,
        "total_size": offer.total_size,
    })).unwrap_or_else(|e| tracing::warn!("Emit failed: {e}"));

    // Wait for user accept/reject
    let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
    {
        state.pending_accepts.write().await.insert(transfer_id.clone(), tx);
    }

    let (user_accepted, is_timeout) = match tokio::time::timeout(
        std::time::Duration::from_secs(crate::transport::protocol::USER_RESPONSE_TIMEOUT_SECS),
        rx,
    )
    .await {
        Ok(Ok(v)) => (v, false),
        Ok(Err(_)) => (false, false),
        Err(_) => (false, true),
    };

    if !user_accepted {
        // Reject
        write_message(&mut control_send, &DashMessage::Reject(RejectPayload {
            reason: ErrorCode::Rejected,
        })).await?;
        update_transfer_status(&state, transfer_id.clone(), TransferStatus::Cancelled).await;
        
        if is_timeout {
            app.emit("transfer_failed", serde_json::json!({
                "transfer_id": transfer_id,
                "reason": "Transfer timed out waiting for user acceptance",
                "phase": "await_accept",
            })).unwrap_or_else(|e| tracing::warn!("Emit failed: {e}"));
        }
        
        return Ok(());
    }

    // Accept
    update_transfer_status(&state, transfer_id.clone(), TransferStatus::Transferring).await;
    write_message(&mut control_send, &DashMessage::Accept(AcceptPayload {
        chosen_version,
    })).await?;

    // Create save root after accepted
    fs::create_dir_all(&save_root).await.context("create save root")?;

    // Receive files
    let result = receive_files(&offer.items, &conn, &save_root, &app, &state, transfer_id.clone()).await;

    match result {
        Ok(outcome) => {
            match &outcome {
                TransferOutcome::Success => {
                    update_transfer_status(&state, transfer_id.clone(), TransferStatus::Complete).await;
                    app.emit("transfer_complete", serde_json::json!({ "transfer_id": transfer_id })).unwrap_or_else(|e| tracing::warn!("Emit failed: {e}"));
                }
                TransferOutcome::PartialSuccess(failed) => {
                    update_transfer_status(&state, transfer_id.clone(), TransferStatus::PartialSuccess).await;
                    app.emit("transfer_partial", serde_json::json!({
                        "transfer_id": transfer_id,
                        "succeeded_count": offer.items.len() - failed.len(),
                        "failed": failed,
                    })).unwrap_or_else(|e| tracing::warn!("Emit failed: {e}"));
                }
                TransferOutcome::Failed(e) => {
                    update_transfer_status(&state, transfer_id.clone(), TransferStatus::Failed).await;
                    app.emit("transfer_failed", serde_json::json!({
                        "transfer_id": transfer_id,
                        "reason": e.to_string(),
                        "phase": "receive",
                    })).unwrap_or_else(|e| tracing::warn!("Emit failed: {e}"));
                }
                TransferOutcome::Cancelled => {
                    update_transfer_status(&state, transfer_id.clone(), TransferStatus::Cancelled).await;
                }
            }
        }
        Err(e) => {
            tracing::error!("Receive error: {e:#}");
            update_transfer_status(&state, transfer_id.clone(), TransferStatus::Failed).await;
            app.emit("transfer_failed", serde_json::json!({
                "transfer_id": transfer_id,
                "reason": e.to_string(),
                "phase": "receive",
            })).unwrap_or_else(|e| tracing::warn!("Emit failed: {e}"));
        }
    }

    Ok(())
}

async fn receive_files(
    items: &[FileItem],
    conn: &Connection,
    save_root: &Path,
    app: &AppHandle,
    state: &Arc<AppState>,
    transfer_id: String,
) -> Result<TransferOutcome> {
    // Track seen final paths to detect conflicts
    let mut seen_paths: HashMap<PathBuf, u32> = HashMap::new();
    
    // Use a semaphore to limit concurrent receives to MAX_CONCURRENT_STREAMS
    let sem = Arc::new(tokio::sync::Semaphore::new(
        crate::transport::protocol::MAX_CONCURRENT_STREAMS as usize,
    ));
    let stream_routes: Arc<Mutex<HashMap<u32, oneshot::Sender<RoutedIncomingFile>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let cancelled_by_peer = Arc::new(AtomicBool::new(false));
    let route_conn = conn.clone();
    let route_map = stream_routes.clone();
    let route_cancelled = cancelled_by_peer.clone();
    let stream_dispatch = tokio::spawn(async move {
        loop {
            let mut recv = match route_conn.accept_uni().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!("File stream dispatcher stopped: {e}");
                    break;
                }
            };

            let first_msg = match read_message(&mut recv).await {
                Ok(msg) => msg,
                Err(e) => {
                    tracing::warn!("Failed to read first file stream message: {e:#}");
                    continue;
                }
            };

            if matches!(first_msg, DashMessage::Cancel(_)) {
                route_cancelled.store(true, Ordering::Relaxed);
                route_map.lock().await.clear();
                break;
            }

            let Some(file_id) = routed_file_id(&first_msg) else {
                tracing::warn!("Unexpected first message on file stream: {:?}", first_msg);
                continue;
            };

            let mut routes = route_map.lock().await;
            if let Some(tx) = routes.remove(&file_id) {
                let _ = tx.send(RoutedIncomingFile { recv, first_msg });
            } else {
                tracing::warn!("No waiting receiver task for file_id {}", file_id);
            }
        }
    });

    let mut handles: Vec<tokio::task::JoinHandle<anyhow::Result<(u32, bool, Option<crate::transport::protocol::ErrorCode>)>>> = Vec::new();

    for item in items {
        // Create directories immediately, no stream needed
        if item.file_type == FileType::Directory {
            match validate_rel_path(&item.rel_path, save_root) {
                Ok(dir_path) => {
                    if let Err(e) = fs::create_dir_all(&dir_path).await {
                        tracing::warn!("Failed to create directory: {e}");
                        // We cannot safely push to failed here without acquiring it; wait, failed is a local mut Vec before spawn loops
                        // Acutally failed is collected later. So doing it directly is problematic since this loop skips Handles.
                        // Let's spawn a dummy handle that yields the failure.
                        let file_id = item.file_id;
                        let err_msg = e.to_string();
                        handles.push(tokio::spawn(async move {
                            Ok((file_id, false, Some(crate::transport::protocol::ErrorCode::Protocol(err_msg))))
                        }));
                    } else {
                        // Success, also spawn a dummy handle that yields success
                        let file_id = item.file_id;
                        handles.push(tokio::spawn(async move {
                            Ok((file_id, true, None))
                        }));
                    }
                }
                Err(_) => {
                    let file_id = item.file_id;
                    handles.push(tokio::spawn(async move {
                        Ok((file_id, false, Some(crate::transport::protocol::ErrorCode::InvalidPath)))
                    }));
                }
            }
            continue;
        }

        // Validate path
        let final_path = match validate_rel_path(&item.rel_path, save_root) {
            Ok(p) => p,
            Err(_) => {
                let file_id = item.file_id;
                let conn2 = conn.clone();
                handles.push(tokio::spawn(async move {
                    let _ = send_ack(&conn2, file_id, false, Some(ErrorCode::InvalidPath)).await;
                    Ok((file_id, false, Some(ErrorCode::InvalidPath)))
                }));
                continue;
            }
        };

        // Check for path conflicts
        if seen_paths.contains_key(&final_path) {
            tracing::warn!("Path conflict for {:?}", final_path);
            let file_id = item.file_id;
            let conn2 = conn.clone();
            handles.push(tokio::spawn(async move {
                let _ = send_ack(&conn2, file_id, false, Some(ErrorCode::PathConflict)).await;
                Ok((file_id, false, Some(ErrorCode::PathConflict)))
            }));
            continue;
        }
        seen_paths.insert(final_path.clone(), item.file_id);

        let sem2 = sem.clone();
        let conn2 = conn.clone();
        let app2 = app.clone();
        let state2 = state.clone();
        let item2 = item.clone();
        let save_root2 = save_root.to_path_buf();
        let transfer_id_clone = transfer_id.clone();
        let (stream_tx, stream_rx) = oneshot::channel();
        stream_routes.lock().await.insert(item.file_id, stream_tx);
        let cancelled_by_peer2 = cancelled_by_peer.clone();

        let handle = tokio::spawn(async move {
            let Ok(_permit) = sem2.acquire().await else {
                let _ = send_ack(&conn2, item2.file_id, false, Some(ErrorCode::Protocol("Semaphore closed".into()))).await;
                return Ok((item2.file_id, false, Some(ErrorCode::Protocol("Semaphore closed".into()))));
            };

            let routed = match tokio::time::timeout(std::time::Duration::from_secs(60), stream_rx).await {
                Ok(Ok(stream)) => stream,
                Ok(Err(_)) => {
                    if cancelled_by_peer2.load(Ordering::Relaxed) {
                        return Ok((item2.file_id, false, Some(ErrorCode::Cancelled)));
                    }
                    let _ = send_ack(&conn2, item2.file_id, false, Some(ErrorCode::Protocol("stream route closed".into()))).await;
                    return Ok((item2.file_id, false, Some(ErrorCode::Protocol("stream route closed".into()))));
                }
                Err(_) => {
                    if cancelled_by_peer2.load(Ordering::Relaxed) {
                        return Ok((item2.file_id, false, Some(ErrorCode::Cancelled)));
                    }
                    let _ = send_ack(&conn2, item2.file_id, false, Some(ErrorCode::Protocol("stream route timeout".into()))).await;
                    return Ok((item2.file_id, false, Some(ErrorCode::Protocol("stream route timeout".into()))));
                }
            };
            
            let res = receive_one_file(item2.clone(), routed, save_root2, app2, state2, transfer_id_clone).await;
            match res {
                Ok((file_id, ok, reason)) => {
                    let _ = send_ack(&conn2, file_id, ok, reason.clone()).await;
                    Ok((file_id, ok, reason))
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    let _ = send_ack(&conn2, item2.file_id, false, Some(ErrorCode::Protocol(err_msg.clone()))).await;
                    Ok((item2.file_id, false, Some(ErrorCode::Protocol(err_msg))))
                }
            }
        });
        handles.push(handle);
    }

    let mut success_count = 0usize;
    let mut failed: Vec<FailedFile> = Vec::new();

    for handle in handles {
        match handle.await {
            Ok(Ok((file_id, ok, reason))) => {
                if ok {
                    success_count += 1;
                } else {
                    let name = items.iter()
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
            Ok(Err(e)) => {
                tracing::error!("File receive task error: {e:#}");
            }
            Err(e) => {
                tracing::error!("Task join error: {e}");
            }
        }
    }

    if cancelled_by_peer.load(Ordering::Relaxed) {
        stream_dispatch.abort();
        Ok(TransferOutcome::Cancelled)
    } else if failed.is_empty() {
        stream_dispatch.abort();
        Ok(TransferOutcome::Success)
    } else if success_count > 0 {
        stream_dispatch.abort();
        Ok(TransferOutcome::PartialSuccess(failed))
    } else {
        stream_dispatch.abort();
        Ok(TransferOutcome::Failed(ErrorCode::Protocol("all files failed".into())))
    }
}

async fn receive_one_file(
    item: FileItem,
    routed: RoutedIncomingFile,
    save_root: PathBuf,
    app: AppHandle,
    state: Arc<AppState>,
    transfer_id: String,
) -> Result<(u32, bool, Option<ErrorCode>)> {
    let file_id = item.file_id;
    let mut recv = routed.recv;
    let first_msg = routed.first_msg;

    // Validate path first
    let final_path = match validate_rel_path(&item.rel_path, &save_root) {
        Ok(p) => p,
        Err(_) => {
            return Ok((file_id, false, Some(ErrorCode::InvalidPath)));
        }
    };

    if let Some(parent) = final_path.parent() {
        if let Err(e) = fs::create_dir_all(parent).await {
            tracing::error!("Failed to create directory: {e}");
            return Ok((file_id, false, Some(io_error_code(&e))));
        }
    }

    let temp_path = temp_path_for(&final_path);
    let _ = fs::remove_file(&temp_path).await;

    let mut file = match File::create(&temp_path).await {
        Ok(f) => f,
        Err(e) => {
            return Ok((file_id, false, Some(io_error_code(&e))));
        }
    };
    let mut hasher = Hasher::new();

    // Receive chunks
    let mut first = Some(first_msg);
    loop {
        let msg = if let Some(m) = first.take() {
            m
        } else {
            match read_message(&mut recv).await {
                Ok(m) => m,
                Err(e) => {
                    tracing::error!("Stream read error for file {file_id}: {e}");
                    let _ = fs::remove_file(&temp_path).await;
                    return Ok((file_id, false, Some(ErrorCode::Protocol(e.to_string()))));
                }
            }
        };

        match msg {
            DashMessage::Chunk(chunk) => {
                if chunk.file_id != file_id {
                    return Ok((file_id, false, Some(ErrorCode::Protocol("file_id mismatch".into()))));
                }
                hasher.update(&chunk.data);
                if let Err(e) = file.write_all(&chunk.data).await {
                    let _ = fs::remove_file(&temp_path).await;
                    return Ok((file_id, false, Some(io_error_code(&e))));
                }

                let overall_transferred = {
                    let mut transfers = state.transfers.write().await;
                    if let Some(t) = transfers.get_mut(&transfer_id) {
                        t.bytes_transferred += chunk.data.len() as u64;
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
            DashMessage::Complete(complete) => {
                if complete.file_id != file_id {
                    return Ok((file_id, false, Some(ErrorCode::Protocol("file_id mismatch in complete".into()))));
                }
                // Flush and verify
                if let Err(e) = file.flush().await {
                    let _ = fs::remove_file(&temp_path).await;
                    return Ok((file_id, false, Some(io_error_code(&e))));
                }
                let computed_hash: [u8; 32] = hasher.finalize().into();
                if computed_hash != complete.file_hash {
                    tracing::error!("Hash mismatch for file {file_id}");
                    let _ = fs::remove_file(&temp_path).await;
                    return Ok((file_id, false, Some(ErrorCode::HashMismatch)));
                }
                if let Err(e) = fs::rename(&temp_path, &final_path).await {
                    let _ = fs::remove_file(&temp_path).await;
                    return Ok((file_id, false, Some(io_error_code(&e))));
                }
                return Ok((file_id, true, None));
            }
            DashMessage::Cancel(_) => {
                let _ = fs::remove_file(&temp_path).await;
                return Ok((file_id, false, Some(ErrorCode::Cancelled)));
            }
            _ => {
                let _ = fs::remove_file(&temp_path).await;
                return Ok((file_id, false, Some(ErrorCode::Protocol("unexpected message".into()))));
            }
        }
    }
}

async fn get_save_root(app: &AppHandle, state: &Arc<AppState>, transfer_id: String) -> PathBuf {
    let custom_dir = state.config.read().await.download_dir.clone();
    let base_dir = custom_dir.map(PathBuf::from).unwrap_or_else(|| {
        app.path().download_dir()
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                PathBuf::from(home).join("Downloads")
            })
    });
    base_dir
        .join("DashDrop")
        .join(transfer_id.to_string())
}

async fn update_transfer_status(state: &Arc<AppState>, id: String, status: TransferStatus) {
    let mut transfers = state.transfers.write().await;
    if let Some(t) = transfers.get_mut(&id) {
        t.status = status.clone();
        match status {
            TransferStatus::Complete | TransferStatus::PartialSuccess | TransferStatus::Failed | TransferStatus::Cancelled => {
                t.ended_at = Some(std::time::Instant::now());
            }
            _ => {}
        }
    }
}

async fn send_ack(conn: &quinn::Connection, file_id: u32, ok: bool, reason: Option<crate::transport::protocol::ErrorCode>) {
    if let Ok(mut send) = conn.open_uni().await {
        let _ = crate::transport::protocol::write_message(&mut send, &crate::transport::protocol::DashMessage::Ack(crate::transport::protocol::AckPayload { file_id, ok, reason })).await;
        let _ = send.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::io_error_code;
    use crate::transport::protocol::ErrorCode;

    #[test]
    fn maps_storage_full_error_to_disk_full() {
        let err = std::io::Error::new(std::io::ErrorKind::StorageFull, "disk full");
        assert!(matches!(io_error_code(&err), ErrorCode::DiskFull));
    }

    #[cfg(unix)]
    #[test]
    fn maps_enospc_errno_to_disk_full() {
        let err = std::io::Error::from_raw_os_error(28);
        assert!(matches!(io_error_code(&err), ErrorCode::DiskFull));
    }
}
