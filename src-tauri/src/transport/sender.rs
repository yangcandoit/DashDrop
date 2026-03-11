use anyhow::{bail, Context, Result};
use blake3::Hasher;
use quinn::Connection;
use std::collections::HashMap;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
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
    FileItem, FileType, OfferPayload, RiskClass, SourceSnapshot, TransferOutcome, CHUNK_SIZE,
    USER_RESPONSE_TIMEOUT_SECS,
};

type AckResult = (u32, bool, Option<ErrorCode>);
type AckWaiters = Arc<Mutex<HashMap<u32, oneshot::Sender<(bool, Option<ErrorCode>)>>>>;
const SOURCE_SNAPSHOT_HEAD_BYTES: usize = 1024 * 1024;
const SHEBANG_PROBE_BYTES: usize = 256;
const HIGH_RISK_SUFFIXES: &[&str] = &[
    ".app", ".bat", ".bash", ".cmd", ".deb", ".exe", ".msi", ".pkg", ".ps1", ".rpm", ".sh", ".zsh",
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum ResumeSourcePolicy {
    ReuseExistingProgress,
    RestartFull { reason: String },
}

fn error_code_key(code: &ErrorCode) -> String {
    code.reason_code().to_string()
}

fn infer_risk_class_from_name(name: &str, file_type: &FileType) -> RiskClass {
    if matches!(file_type, FileType::Directory) {
        let lowered = name.to_ascii_lowercase();
        if HIGH_RISK_SUFFIXES
            .iter()
            .any(|suffix| lowered.ends_with(suffix))
        {
            return RiskClass::High;
        }
        return RiskClass::Normal;
    }

    let lowered = name.to_ascii_lowercase();
    if HIGH_RISK_SUFFIXES
        .iter()
        .any(|suffix| lowered.ends_with(suffix))
    {
        RiskClass::High
    } else {
        RiskClass::Normal
    }
}

#[cfg(unix)]
fn has_executable_bit(meta: &std::fs::Metadata) -> bool {
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn has_executable_bit(_meta: &std::fs::Metadata) -> bool {
    false
}

async fn has_shebang(path: &PathBuf) -> Result<bool> {
    let file = File::open(path)
        .await
        .with_context(|| format!("open risk probe file {}", path.display()))?;
    let mut limited = file.take(SHEBANG_PROBE_BYTES as u64);
    let mut buf = vec![0u8; SHEBANG_PROBE_BYTES];
    let n = limited
        .read(&mut buf)
        .await
        .with_context(|| format!("read risk probe head {}", path.display()))?;
    Ok(n >= 2 && &buf[..2] == b"#!")
}

async fn detect_risk_class(
    path: &PathBuf,
    meta: &std::fs::Metadata,
    file_type: &FileType,
) -> Result<RiskClass> {
    let inferred = infer_risk_class_from_name(
        &path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default(),
        file_type,
    );
    if inferred == RiskClass::High || matches!(file_type, FileType::Directory) {
        return Ok(inferred);
    }

    if has_executable_bit(meta) || has_shebang(path).await? {
        Ok(RiskClass::High)
    } else {
        Ok(RiskClass::Normal)
    }
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
    let current_source_snapshots: HashMap<u32, SourceSnapshot> = items
        .iter()
        .filter_map(|item| {
            item.source_snapshot
                .clone()
                .map(|snapshot| (item.file_id, snapshot))
        })
        .collect();
    let existing_source_snapshots = state
        .db
        .lock()
        .ok()
        .and_then(|guard| crate::db::load_transfer_source_snapshots(&guard, &transfer_id).ok())
        .flatten();
    let resume_source_policy =
        validate_resume_source_snapshots(existing_source_snapshots.as_ref(), &items);

    let total_size: u64 = items.iter().map(|f| f.size).sum();
    let started_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let source_path_by_file_id: HashMap<u32, String> = items_with_paths
        .iter()
        .map(|(item, path)| (item.file_id, path.to_string_lossy().to_string()))
        .collect();
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
                batch_id: None,
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
                        risk_class: f.risk_class.clone(),
                    })
                    .collect(),
                status: TransferStatus::PendingAccept,
                bytes_transferred: 0,
                total_bytes: total_size,
                revision: 0,
                started_at_unix,
                ended_at_unix: None,
                terminal_reason_code: None,
                error: None,
                source_paths: Some(
                    paths
                        .iter()
                        .map(|p| p.to_string_lossy().to_string())
                        .collect(),
                ),
                source_path_by_file_id: Some(source_path_by_file_id),
                failed_file_ids: None,
                conn: Some(conn.clone()),
                ended_at: None,
            },
        );
    }
    if let ResumeSourcePolicy::RestartFull { reason } = &resume_source_policy {
        tracing::warn!(
            transfer_id = %transfer_id,
            peer_fp = %peer_fp,
            reason = %reason,
            "source snapshot changed before resume; falling back to full retransmit"
        );
        if let Ok(guard) = state.db.lock() {
            let _ = crate::db::log_security_event(
                &guard,
                "resume_source_changed",
                "send_prepare",
                Some(&peer_fp),
                reason,
            );
        }
    }
    if let Ok(guard) = state.db.lock() {
        let _ = crate::db::save_transfer_source_snapshots(
            &guard,
            &transfer_id,
            &current_source_snapshots,
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
    let _ = control_send.finish();

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
                t.terminal_reason_code = Some(error_code_key(&r.reason));
                t.failed_file_ids = Some(t.items.iter().map(|item| item.file_id).collect());
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
        Ok(Err(e)) => {
            let close_reason = conn
                .close_reason()
                .map(|r| format!("{r:?}"))
                .unwrap_or_else(|| "unknown".to_string());
            return Err(anyhow::anyhow!(
                "read Accept/Reject failed: {e:#}; connection close reason: {close_reason}"
            ));
        }
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
                t.terminal_reason_code = Some(ErrorCode::Timeout.reason_code().to_string());
                t.failed_file_ids = Some(t.items.iter().map(|item| item.file_id).collect());
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
        .iter()
        .map(|(i, p)| (i.file_id, p.clone()))
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
    let mut terminal_snapshot: Option<crate::state::TransferTask> = None;
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
            t.failed_file_ids = match &outcome {
                TransferOutcome::PartialSuccess(failed) => {
                    Some(failed.iter().map(|entry| entry.file_id).collect())
                }
                TransferOutcome::Success => None,
                TransferOutcome::Failed(_) => {
                    Some(t.items.iter().map(|item| item.file_id).collect())
                }
                TransferOutcome::CancelledBySender | TransferOutcome::CancelledByReceiver => {
                    Some(t.items.iter().map(|item| item.file_id).collect())
                }
            };
            t.terminal_reason_code = match &outcome {
                TransferOutcome::Success => None,
                TransferOutcome::PartialSuccess(failed) => {
                    failed.first().map(|entry| error_code_key(&entry.reason))
                }
                TransferOutcome::Failed(code) => Some(error_code_key(code)),
                TransferOutcome::CancelledByReceiver => Some("E_CANCELLED_BY_RECEIVER".to_string()),
                TransferOutcome::CancelledBySender => Some("E_CANCELLED_BY_SENDER".to_string()),
            };
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
            terminal_snapshot = Some(crate::persistence_progress::progress_snapshot(t));
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
    if let Some(task) = terminal_snapshot.as_ref() {
        state.flush_progress_persist_now(task).await;
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

fn validate_resume_source_snapshots(
    previous: Option<&HashMap<u32, SourceSnapshot>>,
    items: &[FileItem],
) -> ResumeSourcePolicy {
    let Some(previous) = previous else {
        return ResumeSourcePolicy::ReuseExistingProgress;
    };

    let current: HashMap<u32, (&str, &SourceSnapshot)> = items
        .iter()
        .filter_map(|item| {
            item.source_snapshot
                .as_ref()
                .map(|snapshot| (item.file_id, (item.rel_path.as_str(), snapshot)))
        })
        .collect();

    let mut changed = Vec::new();
    let mut previous_ids: Vec<u32> = previous.keys().copied().collect();
    previous_ids.sort_unstable();
    for file_id in previous_ids {
        let Some(previous_snapshot) = previous.get(&file_id) else {
            continue;
        };
        match current.get(&file_id) {
            Some((rel_path, current_snapshot)) => {
                let mut fields = Vec::new();
                if previous_snapshot.size != current_snapshot.size {
                    fields.push("size");
                }
                if previous_snapshot.mtime_unix_ms != current_snapshot.mtime_unix_ms {
                    fields.push("mtime");
                }
                if previous_snapshot.head_hash != current_snapshot.head_hash {
                    fields.push("head_hash");
                }
                if !fields.is_empty() {
                    changed.push(format!(
                        "{rel_path}(file_id={file_id}):{}",
                        fields.join(",")
                    ));
                }
            }
            None => changed.push(format!("file_id={file_id}:missing")),
        }
    }

    let mut current_only_ids: Vec<u32> = current
        .keys()
        .copied()
        .filter(|file_id| !previous.contains_key(file_id))
        .collect();
    current_only_ids.sort_unstable();
    for file_id in current_only_ids {
        if let Some((rel_path, _)) = current.get(&file_id) {
            changed.push(format!("{rel_path}(file_id={file_id}):added"));
        }
    }

    if changed.is_empty() {
        ResumeSourcePolicy::ReuseExistingProgress
    } else {
        ResumeSourcePolicy::RestartFull {
            reason: format!("source snapshot mismatch: {}", changed.join("; ")),
        }
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
            let _ = send.finish();
            ack_waiters.lock().await.remove(&file_id);
            return Err(e);
        }

        offset += n as u64;
        chunk_id += 1;

        let (progress_snapshot, overall_transferred, total_bytes, revision) = {
            let mut transfers = state.transfers.write().await;
            if let Some(t) = transfers.get_mut(&transfer_id) {
                t.bytes_transferred += n as u64;
                (
                    Some(crate::persistence_progress::progress_snapshot(t)),
                    t.bytes_transferred,
                    t.total_bytes,
                    t.revision,
                )
            } else {
                continue;
            }
        };
        if let Some(task) = progress_snapshot.as_ref() {
            state.schedule_progress_persist(task).await;
        }

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
        let _ = send.finish();
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
        let _ = send.finish();
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
        let risk_class = infer_risk_class_from_name(&name, &FileType::Directory);
        items.push((
            FileItem {
                file_id: *next_id,
                name,
                rel_path,
                size: 0,
                file_type: FileType::Directory,
                modified,
                risk_class: Some(risk_class),
                source_snapshot: None,
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
        let source_snapshot =
            Some(build_source_snapshot(path, meta.len(), meta.modified().ok()).await?);
        let risk_class = detect_risk_class(path, &meta, &FileType::RegularFile).await?;
        // Only regular files; symlinks already skipped above
        items.push((
            FileItem {
                file_id: *next_id,
                name,
                rel_path,
                size: meta.len(),
                file_type: FileType::RegularFile,
                modified,
                risk_class: Some(risk_class),
                source_snapshot,
            },
            path.clone(),
        ));
        *next_id += 1;
    }
    // Skip symlinks, sockets, devices etc. (MVP spec)

    Ok(())
}

async fn build_source_snapshot(
    path: &PathBuf,
    file_size: u64,
    modified: Option<std::time::SystemTime>,
) -> Result<SourceSnapshot> {
    let file = File::open(path)
        .await
        .with_context(|| format!("open source snapshot file {}", path.display()))?;
    let mut limited = file.take(std::cmp::min(SOURCE_SNAPSHOT_HEAD_BYTES as u64, file_size));
    let mut hasher = Hasher::new();
    let mut buf = vec![0u8; 64 * 1024];

    loop {
        let n = limited
            .read(&mut buf)
            .await
            .with_context(|| format!("read source snapshot head {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    let mtime_unix_ms = modified
        .and_then(|mtime| mtime.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);

    Ok(SourceSnapshot {
        size: file_size,
        mtime_unix_ms,
        head_hash: hasher.finalize().into(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        build_source_snapshot, detect_risk_class, determine_outcome, infer_risk_class_from_name,
        validate_resume_source_snapshots, ResumeSourcePolicy, SOURCE_SNAPSHOT_HEAD_BYTES,
    };
    use crate::transport::protocol::{ErrorCode, FileItem, FileType, RiskClass, TransferOutcome};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::Duration;
    use tokio::task::JoinHandle;

    fn file(file_id: u32, name: &str) -> FileItem {
        FileItem {
            file_id,
            name: name.to_string(),
            rel_path: name.to_string(),
            size: 1,
            file_type: FileType::RegularFile,
            modified: 0,
            risk_class: Some(RiskClass::Normal),
            source_snapshot: None,
        }
    }

    #[test]
    fn executable_suffix_is_high_risk() {
        assert_eq!(
            infer_risk_class_from_name("installer.pkg", &FileType::RegularFile),
            RiskClass::High
        );
        assert_eq!(
            infer_risk_class_from_name("notes.txt", &FileType::RegularFile),
            RiskClass::Normal
        );
    }

    fn temp_file_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dashdrop-{name}-{}", uuid::Uuid::new_v4()))
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

    #[tokio::test]
    async fn source_snapshot_hashes_entire_small_file_under_one_megabyte() {
        let path = temp_file_path("small-snapshot.bin");
        let data = vec![0x5Au8; SOURCE_SNAPSHOT_HEAD_BYTES - 17];
        tokio::fs::write(&path, &data)
            .await
            .expect("write test file");

        let snapshot = build_source_snapshot(&path, data.len() as u64, None)
            .await
            .expect("snapshot");

        assert_eq!(snapshot.size, data.len() as u64);
        let expected_hash: [u8; 32] = blake3::hash(&data).into();
        assert_eq!(snapshot.head_hash, expected_hash);

        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn modified_source_forces_full_restart_before_resume() {
        let path = temp_file_path("resume-change.bin");
        tokio::fs::write(&path, b"original-content")
            .await
            .expect("write original file");
        let previous_snapshot = build_source_snapshot(&path, 16, None)
            .await
            .expect("previous snapshot");

        tokio::time::sleep(Duration::from_millis(5)).await;
        tokio::fs::write(&path, b"updated-content!")
            .await
            .expect("rewrite file");
        let metadata = tokio::fs::metadata(&path).await.expect("metadata");
        let current_snapshot =
            build_source_snapshot(&path, metadata.len(), metadata.modified().ok())
                .await
                .expect("current snapshot");
        let items = vec![FileItem {
            file_id: 1,
            name: "resume-change.bin".into(),
            rel_path: "resume-change.bin".into(),
            size: metadata.len(),
            file_type: FileType::RegularFile,
            modified: 0,
            risk_class: Some(RiskClass::Normal),
            source_snapshot: Some(current_snapshot),
        }];

        let policy = validate_resume_source_snapshots(
            Some(&HashMap::from([(1u32, previous_snapshot)])),
            &items,
        );

        match policy {
            ResumeSourcePolicy::RestartFull { reason } => {
                assert!(reason.contains("resume-change.bin"));
            }
            ResumeSourcePolicy::ReuseExistingProgress => {
                panic!("modified source must force a full restart")
            }
        }

        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn shebang_without_extension_is_high_risk() {
        let path = temp_file_path("script-no-ext");
        tokio::fs::write(&path, b"#!/bin/sh\necho hi\n")
            .await
            .expect("write script");
        let meta = tokio::fs::metadata(&path).await.expect("metadata");

        let risk = detect_risk_class(&path, &meta, &FileType::RegularFile)
            .await
            .expect("risk");

        assert_eq!(risk, RiskClass::High);

        let _ = tokio::fs::remove_file(&path).await;
    }
}
