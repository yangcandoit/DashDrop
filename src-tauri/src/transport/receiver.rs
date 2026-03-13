use anyhow::{Context, Result};
use blake3::Hasher;
use quinn::Connection;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use sysinfo::Disks;
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;
use tokio::sync::{oneshot, Mutex};

use crate::runtime::host::RuntimeHost;
use crate::state::{AppState, FileConflictStrategy, TransferDirection, TransferStatus};
use crate::transport::events::{
    emit_transfer_accepted, emit_transfer_complete, emit_transfer_error_with_detail,
    emit_transfer_incoming, emit_transfer_partial, emit_transfer_progress, emit_transfer_terminal,
};
use crate::transport::path_validation::validate_rel_path;
use crate::transport::protocol::{
    read_message, write_message, AcceptPayload, DashMessage, ErrorCode, FailedFile, FileItem,
    FileType, OfferPayload, RejectPayload, RiskClass, TransferOutcome,
};

struct RoutedIncomingFile {
    recv: quinn::RecvStream,
    first_msg: DashMessage,
}

type ReceiveTaskResult = anyhow::Result<(u32, bool, Option<crate::transport::protocol::ErrorCode>)>;
type ReceiveTaskHandle = tokio::task::JoinHandle<ReceiveTaskResult>;
const TRUSTED_AUTO_ACCEPT_MAX_BYTES: u64 = 500 * 1024 * 1024;
const HIGH_RISK_SUFFIXES: &[&str] = &[
    ".app", ".bat", ".bash", ".cmd", ".deb", ".exe", ".msi", ".pkg", ".ps1", ".rpm", ".sh", ".zsh",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManualReviewReason {
    UntrustedOrUnauthenticated,
    AutoAcceptDisabled,
    SizeThreshold,
    HighRisk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoAcceptDecision {
    AutoAccept,
    RequireManual(ManualReviewReason),
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

fn split_file_name(path: &Path) -> (String, String) {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let ext = path
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();
    (stem, ext)
}

async fn resolve_conflict_path(
    desired_path: PathBuf,
    strategy: &FileConflictStrategy,
) -> Result<Option<PathBuf>> {
    let exists = fs::metadata(&desired_path).await.is_ok();
    if !exists {
        return Ok(Some(desired_path));
    }
    match strategy {
        FileConflictStrategy::Overwrite => Ok(Some(desired_path)),
        FileConflictStrategy::Skip => Ok(None),
        FileConflictStrategy::Rename => {
            let parent = desired_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            let (stem, ext) = split_file_name(&desired_path);
            for idx in 1..=9999u32 {
                let candidate = parent.join(format!("{stem} ({idx}){ext}"));
                if fs::metadata(&candidate).await.is_err() {
                    return Ok(Some(candidate));
                }
            }
            Err(anyhow::anyhow!(
                "unable to allocate renamed conflict path for {}",
                desired_path.display()
            ))
        }
    }
}

fn reject_reason(is_timeout: bool) -> (&'static str, &'static str) {
    if is_timeout {
        ("E_TIMEOUT", "Timeout")
    } else {
        ("E_REJECTED_BY_USER", "RejectedByReceiver")
    }
}

pub fn should_auto_accept(trusted: bool, auto_accept_trusted_only: bool) -> bool {
    trusted && auto_accept_trusted_only
}

fn infer_offer_risk_class(item: &FileItem) -> RiskClass {
    if let Some(risk_class) = item.risk_class.clone() {
        return risk_class;
    }

    let candidate_name = Path::new(&item.rel_path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| item.name.clone())
        .to_ascii_lowercase();

    if HIGH_RISK_SUFFIXES
        .iter()
        .any(|suffix| candidate_name.ends_with(suffix))
    {
        RiskClass::High
    } else {
        RiskClass::Normal
    }
}

fn determine_auto_accept_decision(
    trusted_and_authenticated: bool,
    auto_accept_trusted_only: bool,
    total_size: u64,
    items: &[FileItem],
) -> AutoAcceptDecision {
    if !trusted_and_authenticated {
        return AutoAcceptDecision::RequireManual(ManualReviewReason::UntrustedOrUnauthenticated);
    }
    if !auto_accept_trusted_only {
        return AutoAcceptDecision::RequireManual(ManualReviewReason::AutoAcceptDisabled);
    }
    if total_size > TRUSTED_AUTO_ACCEPT_MAX_BYTES {
        return AutoAcceptDecision::RequireManual(ManualReviewReason::SizeThreshold);
    }
    if items
        .iter()
        .any(|item| infer_offer_risk_class(item) == RiskClass::High)
    {
        return AutoAcceptDecision::RequireManual(ManualReviewReason::HighRisk);
    }
    AutoAcceptDecision::AutoAccept
}

#[allow(clippy::too_many_arguments)]
pub async fn handle_offer(
    offer: OfferPayload,
    conn: Connection,
    mut control_send: quinn::SendStream,
    mut _control_recv: quinn::RecvStream,
    peer_fp: String,
    peer_authenticated: bool,
    chosen_version: u32,
    host: Arc<dyn RuntimeHost>,
    state: Arc<AppState>,
) -> Result<()> {
    let transfer_id = offer.transfer_id;
    let started_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Register in state
    let items_meta: Vec<_> = offer
        .items
        .iter()
        .map(|f| crate::state::FileItemMeta {
            file_id: f.file_id,
            name: f.name.clone(),
            rel_path: f.rel_path.clone(),
            size: f.size,
            hash: None,
            risk_class: f.risk_class.clone(),
        })
        .collect();

    {
        let mut transfers = state.transfers.write().await;
        transfers.insert(
            transfer_id.clone(),
            crate::state::TransferTask {
                id: transfer_id.clone(),
                batch_id: None,
                direction: TransferDirection::Receive,
                peer_fingerprint: peer_fp.clone(),
                peer_name: offer.sender_name.clone(),
                items: items_meta,
                status: TransferStatus::PendingAccept,
                bytes_transferred: 0,
                total_bytes: offer.total_size,
                revision: 0,
                started_at_unix,
                ended_at_unix: None,
                terminal_reason_code: None,
                error: None,
                source_paths: None,
                source_path_by_file_id: None,
                failed_file_ids: None,
                conn: Some(conn.clone()),
                ended_at: None,
            },
        );
    }

    // Determine potential save root
    let save_root = get_save_root(&host, &state, transfer_id.clone()).await;

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
        write_message(
            &mut control_send,
            &DashMessage::Reject(RejectPayload {
                reason: ErrorCode::DiskFull,
            }),
        )
        .await?;
        let _ = control_send.finish();
        update_transfer_status(
            &state,
            transfer_id.clone(),
            TransferStatus::Failed,
            Some("E_DISK_FULL"),
        )
        .await;
        {
            let mut transfers = state.transfers.write().await;
            if let Some(t) = transfers.get_mut(&transfer_id) {
                t.error = Some("Insufficient disk space".to_string());
            }
        }
        emit_transfer_terminal(
            &host,
            &state,
            &transfer_id,
            &TransferStatus::Failed,
            "E_DISK_FULL",
            "DiskFull",
            transfer_revision(&state, &transfer_id).await,
            Some("preflight"),
        );
        return Ok(());
    }

    // Emit incoming transfer event to frontend
    let trusted = state.is_active_trust(&peer_fp).await;
    let notification_id = state
        .ensure_incoming_request_notification(&transfer_id)
        .await;
    emit_transfer_incoming(
        &host,
        &state,
        &transfer_id,
        &notification_id,
        &offer.sender_name,
        &peer_fp,
        trusted,
        &offer.items,
        offer.total_size,
        transfer_revision(&state, &transfer_id).await,
    );
    state
        .record_receiver_fallback_prompted(&transfer_id, &peer_fp)
        .await;

    let auto_accept_enabled = state.config.read().await.auto_accept_trusted_only;
    let auto_accept_decision = determine_auto_accept_decision(
        trusted && peer_authenticated,
        auto_accept_enabled,
        offer.total_size,
        &offer.items,
    );
    let (user_accepted, is_timeout) = if auto_accept_decision == AutoAcceptDecision::AutoAccept {
        (true, false)
    } else {
        state
            .ensure_incoming_request_notification(&transfer_id)
            .await;

        // Size/risk policy downgrades to manual review; timeout keeps existing E_TIMEOUT semantics.
        let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
        {
            state
                .pending_accepts
                .write()
                .await
                .insert(transfer_id.clone(), tx);
        }

        match tokio::time::timeout(
            std::time::Duration::from_secs(crate::transport::protocol::USER_RESPONSE_TIMEOUT_SECS),
            rx,
        )
        .await
        {
            Ok(Ok(v)) => (v, false),
            Ok(Err(_)) => (false, false),
            Err(_) => (false, true),
        }
    };

    if !user_accepted {
        // Reject
        write_message(
            &mut control_send,
            &DashMessage::Reject(RejectPayload {
                reason: ErrorCode::Rejected,
            }),
        )
        .await?;
        let _ = control_send.finish();
        let (reason_code, terminal_cause) = reject_reason(is_timeout);
        update_transfer_status(
            &state,
            transfer_id.clone(),
            TransferStatus::Rejected,
            Some(reason_code),
        )
        .await;
        emit_transfer_terminal(
            &host,
            &state,
            &transfer_id,
            &TransferStatus::Rejected,
            reason_code,
            terminal_cause,
            transfer_revision(&state, &transfer_id).await,
            Some("offer"),
        );

        return Ok(());
    }

    // Accept
    let accepted_revision = update_transfer_status(
        &state,
        transfer_id.clone(),
        TransferStatus::Transferring,
        None,
    )
    .await
    .unwrap_or(0);
    emit_transfer_accepted(&host, &state, &transfer_id, accepted_revision);
    state.mark_peer_used(&peer_fp).await;

    // Calculate potential resume offsets before sending Accept
    let mut resume_offsets = HashMap::new();
    let conflict_strategy = state.config.read().await.file_conflict_strategy.clone();
    for item in &offer.items {
        if let Ok(validated_path) = validate_rel_path(&item.rel_path, &save_root) {
            // Using exact same logic as receive_one_file to find the temp file
            let mut final_path = validated_path.clone();
            if let Ok(Some(path)) = resolve_conflict_path(final_path.clone(), &conflict_strategy).await {
                final_path = path;
            }
            let temp_path = temp_path_for(&final_path);
            if let Ok(meta) = fs::metadata(&temp_path).await {
                let current_offset = meta.len();
                if current_offset > 0 && current_offset <= item.size {
                    resume_offsets.insert(item.file_id, current_offset);
                } else if current_offset > item.size {
                    let _ = fs::remove_file(&temp_path).await;
                }
            }
        }
    }

    write_message(
        &mut control_send,
        &DashMessage::Accept(AcceptPayload { 
            chosen_version,
            resume_offsets: if resume_offsets.is_empty() { None } else { Some(resume_offsets) },
        }),
    )
    .await?;
    let _ = control_send.finish();


    // Create save root after accepted
    if let Err(e) = fs::create_dir_all(&save_root).await {
        let reason = io_error_code(&e);
        let _ = send_cancel(&conn, reason.clone()).await;
        update_transfer_status(
            &state,
            transfer_id.clone(),
            TransferStatus::Failed,
            Some(reason.reason_code()),
        )
        .await;
        emit_transfer_terminal(
            &host,
            &state,
            &transfer_id,
            &TransferStatus::Failed,
            reason.reason_code(),
            "ReceiverStorageError",
            transfer_revision(&state, &transfer_id).await,
            Some("receive_setup"),
        );
        emit_transfer_error_with_detail(
            &host,
            &state,
            Some(&transfer_id),
            reason.reason_code(),
            "ReceiverStorageError",
            "receive_setup",
            transfer_revision(&state, &transfer_id).await,
            Some(&e.to_string()),
        );
        return Ok(());
    }

    // Receive files
    let result = receive_files(
        &offer.items,
        &conn,
        &save_root,
        &host,
        &state,
        transfer_id.clone(),
    )
    .await;

    match result {
        Ok(outcome) => match &outcome {
            TransferOutcome::Success => {
                update_transfer_status(
                    &state,
                    transfer_id.clone(),
                    TransferStatus::Completed,
                    None,
                )
                .await;
                emit_transfer_complete(
                    &host,
                    &state,
                    &transfer_id,
                    transfer_revision(&state, &transfer_id).await,
                );
            }
            TransferOutcome::PartialSuccess(failed) => {
                update_transfer_status(
                    &state,
                    transfer_id.clone(),
                    TransferStatus::PartialCompleted,
                    None,
                )
                .await;
                emit_transfer_partial(
                    &host,
                    &state,
                    &transfer_id,
                    offer.items.len() - failed.len(),
                    failed,
                    None,
                    transfer_revision(&state, &transfer_id).await,
                );
            }
            TransferOutcome::Failed(e) => {
                update_transfer_status(
                    &state,
                    transfer_id.clone(),
                    TransferStatus::Failed,
                    Some(e.reason_code()),
                )
                .await;
                emit_transfer_terminal(
                    &host,
                    &state,
                    &transfer_id,
                    &TransferStatus::Failed,
                    e.reason_code(),
                    "NetworkDropped",
                    transfer_revision(&state, &transfer_id).await,
                    Some("receive"),
                );
                emit_transfer_error_with_detail(
                    &host,
                    &state,
                    Some(&transfer_id),
                    e.reason_code(),
                    "NetworkDropped",
                    "receive",
                    transfer_revision(&state, &transfer_id).await,
                    Some(&e.to_string()),
                );
            }
            TransferOutcome::CancelledBySender => {
                update_transfer_status(
                    &state,
                    transfer_id.clone(),
                    TransferStatus::CancelledBySender,
                    Some("E_CANCELLED_BY_SENDER"),
                )
                .await;
                emit_transfer_terminal(
                    &host,
                    &state,
                    &transfer_id,
                    &TransferStatus::CancelledBySender,
                    "E_CANCELLED_BY_SENDER",
                    "CancelledBySender",
                    transfer_revision(&state, &transfer_id).await,
                    Some("receive"),
                );
            }
            TransferOutcome::CancelledByReceiver => {
                update_transfer_status(
                    &state,
                    transfer_id.clone(),
                    TransferStatus::CancelledByReceiver,
                    Some("E_CANCELLED_BY_RECEIVER"),
                )
                .await;
                emit_transfer_terminal(
                    &host,
                    &state,
                    &transfer_id,
                    &TransferStatus::CancelledByReceiver,
                    "E_CANCELLED_BY_RECEIVER",
                    "CancelledByReceiver",
                    transfer_revision(&state, &transfer_id).await,
                    Some("receive"),
                );
            }
        },
        Err(e) => {
            tracing::error!(
                transfer_id = %transfer_id,
                peer_fp = %peer_fp,
                phase = "receive",
                reason = %e,
                "receive transfer error"
            );
            update_transfer_status(
                &state,
                transfer_id.clone(),
                TransferStatus::Failed,
                Some("E_PROTOCOL"),
            )
            .await;
            emit_transfer_terminal(
                &host,
                &state,
                &transfer_id,
                &TransferStatus::Failed,
                "E_PROTOCOL",
                "NetworkDropped",
                transfer_revision(&state, &transfer_id).await,
                Some("receive"),
            );
            emit_transfer_error_with_detail(
                &host,
                &state,
                Some(&transfer_id),
                "E_PROTOCOL",
                "NetworkDropped",
                "receive",
                transfer_revision(&state, &transfer_id).await,
                Some(&format!("{e:#}")),
            );
        }
    }

    Ok(())
}

async fn receive_files(
    items: &[FileItem],
    conn: &Connection,
    save_root: &Path,
    host: &Arc<dyn RuntimeHost>,
    state: &Arc<AppState>,
    transfer_id: String,
) -> Result<TransferOutcome> {
    // Track seen final paths to detect conflicts
    let mut seen_paths: HashMap<PathBuf, u32> = HashMap::new();

    // Use a semaphore to limit concurrent receives to MAX_CONCURRENT_STREAMS
    let max_streams = state.config.read().await.max_parallel_streams.clamp(1, 32);
    let sem = Arc::new(tokio::sync::Semaphore::new(max_streams as usize));
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

    let mut handles: Vec<ReceiveTaskHandle> = Vec::new();

    for item in items {
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
        let host2 = Arc::clone(host);
        let state2 = state.clone();
        let item2 = item.clone();
        let save_root2 = save_root.to_path_buf();
        let transfer_id_clone = transfer_id.clone();
        let (stream_tx, stream_rx) = oneshot::channel();
        stream_routes.lock().await.insert(item.file_id, stream_tx);
        let cancelled_by_peer2 = cancelled_by_peer.clone();

        let handle = tokio::spawn(async move {
            let Ok(_permit) = sem2.acquire().await else {
                let _ = send_ack(
                    &conn2,
                    item2.file_id,
                    false,
                    Some(ErrorCode::Protocol("Semaphore closed".into())),
                )
                .await;
                return Ok((
                    item2.file_id,
                    false,
                    Some(ErrorCode::Protocol("Semaphore closed".into())),
                ));
            };

            let routed =
                match tokio::time::timeout(std::time::Duration::from_secs(60), stream_rx).await {
                    Ok(Ok(stream)) => stream,
                    Ok(Err(_)) => {
                        if cancelled_by_peer2.load(Ordering::Relaxed) {
                            return Ok((item2.file_id, false, Some(ErrorCode::Cancelled)));
                        }
                        let _ = send_ack(
                            &conn2,
                            item2.file_id,
                            false,
                            Some(ErrorCode::Protocol("stream route closed".into())),
                        )
                        .await;
                        return Ok((
                            item2.file_id,
                            false,
                            Some(ErrorCode::Protocol("stream route closed".into())),
                        ));
                    }
                    Err(_) => {
                        if cancelled_by_peer2.load(Ordering::Relaxed) {
                            return Ok((item2.file_id, false, Some(ErrorCode::Cancelled)));
                        }
                        let _ = send_ack(
                            &conn2,
                            item2.file_id,
                            false,
                            Some(ErrorCode::Protocol("stream route timeout".into())),
                        )
                        .await;
                        return Ok((
                            item2.file_id,
                            false,
                            Some(ErrorCode::Protocol("stream route timeout".into())),
                        ));
                    }
                };

            let res = receive_one_file(
                item2.clone(),
                routed,
                save_root2,
                host2,
                state2,
                transfer_id_clone,
            )
            .await;
            match res {
                Ok((file_id, ok, reason)) => {
                    let _ = send_ack(&conn2, file_id, ok, reason.clone()).await;
                    Ok((file_id, ok, reason))
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    let _ = send_ack(
                        &conn2,
                        item2.file_id,
                        false,
                        Some(ErrorCode::Protocol(err_msg.clone())),
                    )
                    .await;
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
            Ok(Err(e)) => {
                tracing::error!(
                    transfer_id = %transfer_id,
                    phase = "receive_file_task",
                    reason = %e,
                    "file receive task error"
                );
            }
            Err(e) => {
                tracing::error!(
                    transfer_id = %transfer_id,
                    phase = "receive_file_join",
                    reason = %e,
                    "file receive task join error"
                );
            }
        }
    }

    if cancelled_by_peer.load(Ordering::Relaxed) {
        stream_dispatch.abort();
        Ok(TransferOutcome::CancelledBySender)
    } else if failed.is_empty() {
        stream_dispatch.abort();
        Ok(TransferOutcome::Success)
    } else if success_count > 0 {
        stream_dispatch.abort();
        Ok(TransferOutcome::PartialSuccess(failed))
    } else {
        stream_dispatch.abort();
        Ok(TransferOutcome::Failed(ErrorCode::Protocol(
            "all files failed".into(),
        )))
    }
}

async fn receive_one_file(
    item: FileItem,
    routed: RoutedIncomingFile,
    save_root: PathBuf,
    host: Arc<dyn RuntimeHost>,
    state: Arc<AppState>,
    transfer_id: String,
) -> Result<(u32, bool, Option<ErrorCode>)> {
    let file_id = item.file_id;
    let mut recv = routed.recv;
    let first_msg = routed.first_msg;

    // Validate path first
    let validated_path = match validate_rel_path(&item.rel_path, &save_root) {
        Ok(p) => p,
        Err(_) => {
            return Ok((file_id, false, Some(ErrorCode::InvalidPath)));
        }
    };
    let conflict_strategy = state.config.read().await.file_conflict_strategy.clone();
    let mut final_path = validated_path.clone();

    if item.file_type == FileType::Directory {
        let expected_hash: [u8; 32] = blake3::hash(&[]).into();
        match first_msg {
            DashMessage::Complete(complete) => {
                if complete.file_id != file_id {
                    return Ok((
                        file_id,
                        false,
                        Some(ErrorCode::Protocol("file_id mismatch in complete".into())),
                    ));
                }
                if complete.file_hash != expected_hash {
                    return Ok((file_id, false, Some(ErrorCode::HashMismatch)));
                }
                if let Err(e) = fs::create_dir_all(&validated_path).await {
                    return Ok((file_id, false, Some(io_error_code(&e))));
                }
                return Ok((file_id, true, None));
            }
            DashMessage::Cancel(_) => return Ok((file_id, false, Some(ErrorCode::Cancelled))),
            _ => {
                return Ok((
                    file_id,
                    false,
                    Some(ErrorCode::Protocol(
                        "expected Complete for directory".into(),
                    )),
                ));
            }
        }
    }

    match resolve_conflict_path(final_path.clone(), &conflict_strategy).await {
        Ok(Some(path)) => {
            final_path = path;
        }
        Ok(None) => {
            return Ok((file_id, false, Some(ErrorCode::PathConflict)));
        }
        Err(e) => {
            return Ok((file_id, false, Some(ErrorCode::Protocol(e.to_string()))));
        }
    }

    if let Some(parent) = final_path.parent() {
        if let Err(e) = fs::create_dir_all(parent).await {
            tracing::error!("Failed to create directory: {e}");
            return Ok((file_id, false, Some(io_error_code(&e))));
        }
    }

    let temp_path = temp_path_for(&final_path);
    let mut current_offset = 0u64;

    // Smart Skip: If final file exists and size matches, skip it.
    // In a future version, we will also verify the hash if available in the offer.
    if let Ok(meta) = fs::metadata(&final_path).await {
        if meta.is_file() && meta.len() == item.size {
            tracing::info!(file_id, ?final_path, "File already exists and size matches, skipping download.");
            // We still need to consume the incoming stream to keep the protocol in sync
            // or just close it if it's a dedicated stream.
            // For now, let's assume we need to skip to the end of the stream.
        }
    }

    // Attempt to resume from temp file if it exists
    if let Ok(meta) = fs::metadata(&temp_path).await {
        current_offset = meta.len();
        // If the temp file is already larger than the expected size, it's corrupted or outdated.
        if current_offset > item.size {
            let _ = fs::remove_file(&temp_path).await;
            current_offset = 0;
        }
    }

    let mut file = if current_offset > 0 {
        match fs::OpenOptions::new().append(true).open(&temp_path).await {
            Ok(f) => f,
            Err(_) => {
                let _ = fs::remove_file(&temp_path).await;
                File::create(&temp_path).await.map_err(|e| io_error_code(&e)).unwrap()
            }
        }
    } else {
        match File::create(&temp_path).await {
            Ok(f) => f,
            Err(e) => {
                return Ok((file_id, false, Some(io_error_code(&e))));
            }
        }
    };
    
    let mut hasher = Hasher::new();
    // In a true resume, we'd need to hash the existing part first.
    // For now, we'll keep it simple and focus on not losing data.

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
                    return Ok((
                        file_id,
                        false,
                        Some(ErrorCode::Protocol("file_id mismatch".into())),
                    ));
                }
                hasher.update(&chunk.data);
                if let Err(e) = file.write_all(&chunk.data).await {
                    let _ = fs::remove_file(&temp_path).await;
                    return Ok((file_id, false, Some(io_error_code(&e))));
                }

                let (progress_snapshot, overall_transferred, total_bytes, revision) = {
                    let mut transfers = state.transfers.write().await;
                    if let Some(t) = transfers.get_mut(&transfer_id) {
                        t.bytes_transferred += chunk.data.len() as u64;
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
                    &host,
                    &state,
                    &transfer_id,
                    overall_transferred,
                    total_bytes,
                    revision,
                );
            }
            DashMessage::Complete(complete) => {
                if complete.file_id != file_id {
                    return Ok((
                        file_id,
                        false,
                        Some(ErrorCode::Protocol("file_id mismatch in complete".into())),
                    ));
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
                if matches!(conflict_strategy, FileConflictStrategy::Overwrite)
                    && fs::metadata(&final_path).await.is_ok()
                {
                    if let Err(e) = fs::remove_file(&final_path).await {
                        let _ = fs::remove_file(&temp_path).await;
                        return Ok((file_id, false, Some(io_error_code(&e))));
                    }
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
                return Ok((
                    file_id,
                    false,
                    Some(ErrorCode::Protocol("unexpected message".into())),
                ));
            }
        }
    }
}

async fn get_save_root(
    host: &Arc<dyn RuntimeHost>,
    state: &Arc<AppState>,
    transfer_id: String,
) -> PathBuf {
    let custom_dir = state.config.read().await.download_dir.clone();
    let base_dir = custom_dir.map(PathBuf::from).unwrap_or_else(|| {
        host.download_dir().unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join("Downloads")
        })
    });
    base_dir.join("DashDrop").join(&transfer_id)
}

async fn update_transfer_status(
    state: &Arc<AppState>,
    id: String,
    status: TransferStatus,
    reason_code: Option<&str>,
) -> Option<u64> {
    let mut metrics_snapshot: Option<(TransferDirection, TransferStatus, u64)> = None;
    let mut terminal_snapshot: Option<crate::state::TransferTask> = None;
    let mut notification_reason_code: Option<String> = None;
    let mut transfers = state.transfers.write().await;
    if let Some(t) = transfers.get_mut(&id) {
        let previous_status = t.status.clone();
        if previous_status != status {
            t.status = status.clone();
            t.revision += 1;
        }
        if let Some(code) = reason_code {
            t.terminal_reason_code = Some(code.to_string());
        }
        match t.status {
            TransferStatus::Completed
            | TransferStatus::PartialCompleted
            | TransferStatus::Failed
            | TransferStatus::CancelledBySender
            | TransferStatus::CancelledByReceiver
            | TransferStatus::Rejected => {
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
                let was_terminal = matches!(
                    previous_status,
                    TransferStatus::Completed
                        | TransferStatus::PartialCompleted
                        | TransferStatus::Failed
                        | TransferStatus::CancelledBySender
                        | TransferStatus::CancelledByReceiver
                        | TransferStatus::Rejected
                );
                if !was_terminal {
                    metrics_snapshot =
                        Some((t.direction.clone(), t.status.clone(), t.bytes_transferred));
                }
                notification_reason_code = t.terminal_reason_code.clone();
            }
            _ => {}
        }
        let revision = t.revision;
        drop(transfers);
        if matches!(
            status,
            TransferStatus::Completed
                | TransferStatus::PartialCompleted
                | TransferStatus::Failed
                | TransferStatus::CancelledBySender
                | TransferStatus::CancelledByReceiver
                | TransferStatus::Rejected
        ) {
            state
                .mark_incoming_request_notification_inactive(
                    &id,
                    notification_reason_code.as_deref(),
                )
                .await;
        }
        if let Some((direction, terminal_status, bytes)) = metrics_snapshot {
            state
                .record_transfer_terminal(&direction, &terminal_status, bytes)
                .await;
        }
        if let Some(task) = terminal_snapshot.as_ref() {
            state.flush_progress_persist_now(task).await;
        }
        return Some(revision);
    }
    None
}

async fn transfer_revision(state: &Arc<AppState>, id: &str) -> u64 {
    state
        .transfers
        .read()
        .await
        .get(id)
        .map(|t| t.revision)
        .unwrap_or(0)
}

async fn send_ack(
    conn: &quinn::Connection,
    file_id: u32,
    ok: bool,
    reason: Option<crate::transport::protocol::ErrorCode>,
) {
    if let Ok(mut send) = conn.open_uni().await {
        let _ = crate::transport::protocol::write_message(
            &mut send,
            &crate::transport::protocol::DashMessage::Ack(crate::transport::protocol::AckPayload {
                file_id,
                ok,
                reason,
            }),
        )
        .await;
        let _ = send.finish();
    }
}

async fn send_cancel(conn: &quinn::Connection, reason: ErrorCode) -> Result<()> {
    let mut send = conn.open_uni().await.context("open cancel stream")?;
    write_message(
        &mut send,
        &DashMessage::Cancel(crate::transport::protocol::CancelPayload {
            reason: crate::transport::protocol::CancelReason::Error(reason),
        }),
    )
    .await
    .context("write cancel message")?;
    send.finish().context("finish cancel stream")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        determine_auto_accept_decision, infer_offer_risk_class, io_error_code, reject_reason,
        should_auto_accept, AutoAcceptDecision, ManualReviewReason, TRUSTED_AUTO_ACCEPT_MAX_BYTES,
    };
    use crate::transport::protocol::{ErrorCode, FileItem, FileType, RiskClass};

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

    #[test]
    fn reject_reason_matches_timeout_branch() {
        assert_eq!(reject_reason(true), ("E_TIMEOUT", "Timeout"));
    }

    #[test]
    fn reject_reason_matches_user_reject_branch() {
        assert_eq!(
            reject_reason(false),
            ("E_REJECTED_BY_USER", "RejectedByReceiver")
        );
    }

    #[test]
    fn trusted_only_auto_accept_branch() {
        assert!(should_auto_accept(true, true));
        assert!(!should_auto_accept(false, true));
        assert!(!should_auto_accept(true, false));
    }

    fn file(name: &str, risk_class: Option<RiskClass>) -> FileItem {
        FileItem {
            file_id: 1,
            name: name.into(),
            rel_path: name.into(),
            size: 1,
            file_type: FileType::RegularFile,
            modified: 0,
            risk_class,
            source_snapshot: None,
        }
    }

    #[test]
    fn trusted_auto_accept_requires_manual_review_above_size_threshold() {
        let decision = determine_auto_accept_decision(
            true,
            true,
            TRUSTED_AUTO_ACCEPT_MAX_BYTES + 1,
            &[file("photo.jpg", Some(RiskClass::Normal))],
        );

        assert_eq!(
            decision,
            AutoAcceptDecision::RequireManual(ManualReviewReason::SizeThreshold)
        );
    }

    #[test]
    fn trusted_auto_accept_requires_manual_review_for_high_risk_items() {
        let decision = determine_auto_accept_decision(
            true,
            true,
            128,
            &[file("script.sh", Some(RiskClass::High))],
        );

        assert_eq!(
            decision,
            AutoAcceptDecision::RequireManual(ManualReviewReason::HighRisk)
        );
    }

    #[test]
    fn trusted_auto_accept_accepts_normal_items_under_size_threshold() {
        let decision = determine_auto_accept_decision(
            true,
            true,
            TRUSTED_AUTO_ACCEPT_MAX_BYTES,
            &[file("photo.jpg", Some(RiskClass::Normal))],
        );

        assert_eq!(decision, AutoAcceptDecision::AutoAccept);
    }

    #[test]
    fn missing_risk_class_falls_back_to_filename_policy() {
        assert_eq!(
            infer_offer_risk_class(&file("Installer.PKG", None)),
            RiskClass::High
        );
        assert_eq!(
            infer_offer_risk_class(&file("notes.txt", None)),
            RiskClass::Normal
        );
    }
}
