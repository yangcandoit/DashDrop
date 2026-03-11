use crate::dto::{DeviceView, TransferView, TrustedPeerView};
use crate::state::{AppState, DeviceInfo, Platform, ReachabilityStatus, SessionInfo};
use crate::transport::connect_to_peer;
use crate::transport::sender::send_files;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter, State};

type AppStateRef<'a> = State<'a, Arc<AppState>>;

const REQUEST_EXPIRED_CODE: &str = "E_REQUEST_EXPIRED";
const REQUEST_EXPIRED_CAUSE: &str = "NotificationExpired";
const REQUEST_EXPIRED_PHASE: &str = "notification_action";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IncomingRequestActionState {
    Pending,
    Expired,
    Missing,
}

fn classify_runtime_send_error(detail: &str) -> (&'static str, &'static str) {
    let s = detail.to_ascii_lowercase();
    if s.contains("timeout") || s.contains("timed out") {
        ("E_TIMEOUT", "Timeout")
    } else if s.contains("message too large") {
        ("E_PROTOCOL", "OfferTooLarge")
    } else if s.contains("expected offer")
        || s.contains("expected accept/reject")
        || s.contains("expected hello")
        || s.contains("read hello failed")
        || s.contains("read offer failed")
    {
        ("E_PROTOCOL", "ProtocolSequenceError")
    } else if s.contains("identity mismatch") {
        ("E_IDENTITY_MISMATCH", "IdentityMismatch")
    } else if s.contains("version") && s.contains("mismatch") {
        ("E_VERSION_MISMATCH", "VersionMismatch")
    } else if s.contains("rate limit") {
        ("E_RATE_LIMITED", "RateLimited")
    } else if s.contains("read len")
        || s.contains("read body")
        || s.contains("control stream")
        || s.contains("stream closed")
    {
        ("E_PROTOCOL", "ControlStreamClosed")
    } else {
        ("E_PROTOCOL", "SystemError")
    }
}

fn should_retry_send_once(detail: &str) -> bool {
    let s = detail.to_ascii_lowercase();
    s.contains("read accept/reject failed")
        || s.contains("control stream")
        || s.contains("read len")
        || s.contains("read body")
        || s.contains("open bi stream")
        || s.contains("accept bi stream")
}

async fn reconnect_to_peer_by_best_addrs(
    state: &Arc<AppState>,
    peer_fp: &str,
) -> Result<quinn::Connection, String> {
    let addrs = {
        let devices = state.devices.read().await;
        let Some(device) = devices.get(peer_fp) else {
            return Err(format!("device {peer_fp} not found for retry"));
        };
        device
            .best_addrs()
            .ok_or_else(|| "device has no reachable address for retry".to_string())?
    };

    let mut errors = Vec::new();
    for addr in addrs {
        match connect_to_peer(state, addr).await {
            Ok(conn) => return Ok(conn),
            Err(e) => errors.push(format!("{addr}: {e:#}")),
        }
    }

    if errors.is_empty() {
        Err("retry connect failed without candidates".to_string())
    } else {
        Err(format!("retry connect failed ({})", errors.join(" | ")))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectByAddressResult {
    pub fingerprint: String,
    pub name: String,
    pub trusted: bool,
    pub address: String,
}

async fn persist_runtime_state(state: &Arc<AppState>) -> Result<(), String> {
    let config = state.config.read().await.clone();
    let trusted = state.trusted_peers.read().await.clone();
    let guard = state
        .db
        .lock()
        .map_err(|_| "DB lock poisoned".to_string())?;
    crate::db::save_app_config(&guard, &config).map_err(|e| e.to_string())?;
    crate::db::replace_trusted_peers(&guard, &trusted).map_err(|e| e.to_string())?;
    Ok(())
}

async fn accept_pending_transfer(state: &Arc<AppState>, transfer_id: &str) -> Result<(), String> {
    match incoming_request_action_state(state, transfer_id).await {
        IncomingRequestActionState::Expired => {
            let _ = state.pending_accepts.write().await.remove(transfer_id);
            state
                .mark_incoming_request_notification_inactive(
                    transfer_id,
                    Some(REQUEST_EXPIRED_CODE),
                )
                .await;
            return Err(REQUEST_EXPIRED_CODE.to_string());
        }
        IncomingRequestActionState::Missing => {
            return Err(format!(
                "transfer {transfer_id} not found or already handled"
            ));
        }
        IncomingRequestActionState::Pending => {}
    }

    let tx = state.pending_accepts.write().await.remove(transfer_id);
    if let Some(sender) = tx {
        state
            .mark_incoming_request_notification_inactive(transfer_id, None)
            .await;
        let _ = sender.send(true);
        Ok(())
    } else {
        match incoming_request_action_state(state, transfer_id).await {
            IncomingRequestActionState::Expired => {
                state
                    .mark_incoming_request_notification_inactive(
                        transfer_id,
                        Some(REQUEST_EXPIRED_CODE),
                    )
                    .await;
                Err(REQUEST_EXPIRED_CODE.to_string())
            }
            IncomingRequestActionState::Pending | IncomingRequestActionState::Missing => Err(
                format!("transfer {transfer_id} not found or already handled"),
            ),
        }
    }
}

async fn reject_pending_transfer(state: &Arc<AppState>, transfer_id: &str) -> Result<(), String> {
    match incoming_request_action_state(state, transfer_id).await {
        IncomingRequestActionState::Expired => {
            let _ = state.pending_accepts.write().await.remove(transfer_id);
            state
                .mark_incoming_request_notification_inactive(
                    transfer_id,
                    Some(REQUEST_EXPIRED_CODE),
                )
                .await;
            return Err(REQUEST_EXPIRED_CODE.to_string());
        }
        IncomingRequestActionState::Missing => return Ok(()),
        IncomingRequestActionState::Pending => {}
    }

    let tx = state.pending_accepts.write().await.remove(transfer_id);
    if let Some(sender) = tx {
        state
            .mark_incoming_request_notification_inactive(transfer_id, None)
            .await;
        let _ = sender.send(false);
        Ok(())
    } else {
        match incoming_request_action_state(state, transfer_id).await {
            IncomingRequestActionState::Expired => {
                state
                    .mark_incoming_request_notification_inactive(
                        transfer_id,
                        Some(REQUEST_EXPIRED_CODE),
                    )
                    .await;
                Err(REQUEST_EXPIRED_CODE.to_string())
            }
            IncomingRequestActionState::Pending | IncomingRequestActionState::Missing => Ok(()),
        }
    }
}

async fn incoming_request_action_state(
    state: &Arc<AppState>,
    transfer_id: &str,
) -> IncomingRequestActionState {
    let notification = state.incoming_request_notification(transfer_id).await;
    let transfer_status = {
        let transfers = state.transfers.read().await;
        transfers.get(transfer_id).map(|task| task.status.clone())
    };

    match transfer_status {
        Some(crate::state::TransferStatus::PendingAccept) => {
            if notification
                .as_ref()
                .map(|entry| !entry.active)
                .unwrap_or(false)
            {
                IncomingRequestActionState::Expired
            } else {
                IncomingRequestActionState::Pending
            }
        }
        Some(_) => IncomingRequestActionState::Expired,
        None => {
            if notification
                .as_ref()
                .map(|entry| !entry.active)
                .unwrap_or(false)
            {
                IncomingRequestActionState::Expired
            } else if state.pending_accepts.read().await.contains_key(transfer_id) {
                IncomingRequestActionState::Pending
            } else {
                IncomingRequestActionState::Missing
            }
        }
    }
}

async fn emit_request_expired(app: &AppHandle, state: &Arc<AppState>, transfer_id: &str) {
    let revision = {
        let transfers = state.transfers.read().await;
        transfers
            .get(transfer_id)
            .map(|task| task.revision)
            .unwrap_or(0)
    };
    crate::transport::events::emit_transfer_error(
        app,
        Some(transfer_id),
        REQUEST_EXPIRED_CODE,
        REQUEST_EXPIRED_CAUSE,
        REQUEST_EXPIRED_PHASE,
        revision,
    );
}

fn select_retry_paths(task: &crate::state::TransferTask) -> Result<Vec<String>, String> {
    let Some(all_paths) = task.source_paths.clone() else {
        return Err("retry source paths unavailable for this transfer".into());
    };
    let failed_ids = task.failed_file_ids.clone().unwrap_or_default();
    if failed_ids.is_empty() {
        return Ok(all_paths);
    }

    let mut unique = HashSet::new();
    let mut subset_paths = Vec::new();
    if let Some(mapping) = task.source_path_by_file_id.as_ref() {
        for file_id in failed_ids {
            if let Some(path) = mapping.get(&file_id) {
                let path = path.clone();
                if unique.insert(path.clone()) {
                    subset_paths.push(path);
                }
            }
        }
    }

    if subset_paths.is_empty() {
        Ok(all_paths)
    } else {
        Ok(subset_paths)
    }
}

// ─── Device commands ────────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_devices(state: AppStateRef<'_>) -> Result<Vec<DeviceView>, String> {
    let devices = state.devices.read().await;
    Ok(devices.values().map(DeviceView::from).collect())
}

#[tauri::command]
pub async fn get_trusted_peers(state: AppStateRef<'_>) -> Result<Vec<TrustedPeerView>, String> {
    let trusted = state.trusted_peers.read().await;
    Ok(trusted.values().map(TrustedPeerView::from).collect())
}

#[tauri::command]
pub async fn pair_device(fp: String, app: AppHandle, state: AppStateRef<'_>) -> Result<(), String> {
    let name = {
        let mut devices = state.devices.write().await;
        if let Some(device) = devices.get_mut(&fp) {
            device.trusted = true;
            let payload = DeviceView::from(&*device);
            app.emit("device_updated", &payload).ok();
            device.name.clone()
        } else {
            "Unknown".into()
        }
    };
    state.add_trust(fp, name).await;
    persist_runtime_state(&state).await?;
    Ok(())
}

#[tauri::command]
pub async fn unpair_device(
    fp: String,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    let removed = state.trusted_peers.write().await.remove(&fp);
    if removed.is_none() {
        return Err(format!("trusted device {fp} not found"));
    }
    {
        let mut devices = state.devices.write().await;
        if let Some(device) = devices.get_mut(&fp) {
            device.trusted = false;
            let payload = DeviceView::from(&*device);
            app.emit("device_updated", &payload).ok();
        }
    }
    persist_runtime_state(&state).await?;
    Ok(())
}

#[tauri::command]
pub async fn set_trusted_alias(
    fp: String,
    alias: Option<String>,
    _app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    let normalized = alias.and_then(|a| {
        let trimmed = a.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    });
    let changed = {
        let mut trusted = state.trusted_peers.write().await;
        let Some(peer) = trusted.get_mut(&fp) else {
            return Err(format!("trusted device {fp} not found"));
        };
        peer.alias = normalized;
        true
    };
    if changed {
        persist_runtime_state(&state).await?;
    }
    Ok(())
}

// ─── Transfer commands ───────────────────────────────────────────────────────

#[tauri::command]
pub async fn send_files_cmd(
    peer_fp: String,
    paths: Vec<String>,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    let state = Arc::clone(&state);
    let transfer_id = uuid::Uuid::new_v4().to_string();
    state.record_sender_dispatch(&transfer_id, &peer_fp).await;

    // Get peer addresses
    let remote_addrs = {
        let devices = state.devices.read().await;
        let device = devices
            .get(&peer_fp)
            .ok_or_else(|| format!("device {peer_fp} not found"))?;
        device
            .best_addrs()
            .ok_or_else(|| "device has no reachable address".to_string())?
    };

    let path_bufs: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();

    // Validate files exist before attempting network connection
    for p in &path_bufs {
        if !p.exists() {
            return Err(format!("Source path does not exist: {}", p.display()));
        }
    }

    // Connect (try available addrs)
    let mut conn_opt = None;
    let mut connect_errors: Vec<String> = Vec::new();
    for addr in remote_addrs {
        tracing::debug!("Trying to connect to {}", addr);
        match connect_to_peer(&state, addr).await {
            Ok(c) => {
                conn_opt = Some(c);
                break;
            }
            Err(e) => {
                tracing::warn!("Failed to connect to {}: {e:#}", addr);
                connect_errors.push(format!("{addr}: {e:#}"));
            }
        }
    }

    let conn = conn_opt.ok_or_else(|| {
        if connect_errors.is_empty() {
            "All connection attempts failed".to_string()
        } else {
            format!(
                "All connection attempts failed ({})",
                connect_errors.join(" | ")
            )
        }
    })?;

    // Hard-bind selected device fingerprint to the peer certificate.
    let (fp_match, actual_fp) = crate::transport::handshake::peer_fp_matches(&conn, &peer_fp)
        .map_err(|e| format!("failed to verify peer identity: {e:#}"))?;
    if !fp_match {
        conn.close(quinn::VarInt::from_u32(2), b"identity mismatch");
        app.emit(
            "identity_mismatch",
            serde_json::json!({
                "expected_fp": peer_fp.clone(),
                "actual_fp": actual_fp,
                "phase": "connect",
            }),
        )
        .ok();
        if let Ok(db) = state.db.lock() {
            let _ = crate::db::log_security_event(
                &db,
                "identity_mismatch",
                "connect",
                Some(&peer_fp),
                "peer certificate fingerprint mismatch",
            );
        }
        return Err(
            "identity mismatch: peer certificate does not match selected device".to_string(),
        );
    }

    // Spawn sender task
    let transfer_id_clone = transfer_id.clone();
    let retry_paths = path_bufs.clone();
    let retry_peer_fp = peer_fp.clone();

    tokio::spawn(async move {
        let mut first_conn = Some(conn);
        let mut has_retried = false;
        loop {
            let Some(current_conn) = first_conn.take() else {
                break;
            };
            let outcome = send_files(
                transfer_id_clone.clone(),
                retry_peer_fp.clone(),
                retry_paths.clone(),
                current_conn,
                app.clone(),
                state.clone(),
            )
            .await;

            match outcome {
                Ok(crate::transport::protocol::TransferOutcome::Success) => {
                    tracing::info!("Transfer complete");
                    break;
                }
                Ok(other) => {
                    tracing::warn!("Transfer ended: {:?}", other);
                    break;
                }
                Err(e) => {
                    let detail = format!("{e:#}");
                    if !has_retried && should_retry_send_once(&detail) {
                        has_retried = true;
                        tracing::warn!(
                            transfer_id = %transfer_id_clone,
                            "Send attempt hit transient control-stream failure; reconnecting once"
                        );
                        match reconnect_to_peer_by_best_addrs(&state, &retry_peer_fp).await {
                            Ok(retry_conn) => {
                                first_conn = Some(retry_conn);
                                continue;
                            }
                            Err(reconnect_err) => {
                                tracing::warn!(
                                    transfer_id = %transfer_id_clone,
                                    "Retry reconnect failed: {reconnect_err}"
                                );
                            }
                        }
                    }

                    tracing::error!("Send failed: {e:#}");
                    let (reason_code, terminal_cause) = classify_runtime_send_error(&detail);
                    {
                        let mut guard = state.transfers.write().await;
                        if let Some(t) = guard.get_mut(&transfer_id_clone) {
                            let is_terminal = matches!(
                                t.status,
                                crate::state::TransferStatus::Completed
                                    | crate::state::TransferStatus::PartialCompleted
                                    | crate::state::TransferStatus::Rejected
                                    | crate::state::TransferStatus::CancelledBySender
                                    | crate::state::TransferStatus::CancelledByReceiver
                                    | crate::state::TransferStatus::Failed
                            );
                            if !is_terminal {
                                t.status = crate::state::TransferStatus::Failed;
                                t.revision += 1;
                                t.terminal_reason_code = Some(reason_code.to_string());
                                t.failed_file_ids =
                                    Some(t.items.iter().map(|item| item.file_id).collect());
                                t.error = Some(detail.clone());
                                t.ended_at = Some(std::time::Instant::now());
                                t.ended_at_unix = Some(
                                    std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|d| d.as_secs())
                                        .unwrap_or(0),
                                );
                                if let Ok(db) = state.db.lock() {
                                    let _ = crate::db::save_transfer(&db, t);
                                }
                            }
                        }
                    }

                    let revision = {
                        let transfers = state.transfers.read().await;
                        transfers
                            .get(&transfer_id_clone)
                            .map(|t| t.revision)
                            .unwrap_or(0)
                    };
                    crate::transport::events::emit_transfer_terminal(
                        &app,
                        &transfer_id_clone,
                        &crate::state::TransferStatus::Failed,
                        reason_code,
                        terminal_cause,
                        revision,
                        Some("send"),
                    );
                    crate::transport::events::emit_transfer_error_with_detail(
                        &app,
                        Some(&transfer_id_clone),
                        reason_code,
                        terminal_cause,
                        "send",
                        revision,
                        Some(&detail),
                    );
                    break;
                }
            }
        }
    });

    Ok(())
}

#[tauri::command]
pub async fn connect_by_address(
    address: String,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<ConnectByAddressResult, String> {
    let resolved: Vec<_> = address
        .to_socket_addrs()
        .map_err(|e| format!("invalid address {address}: {e}"))?
        .collect();
    if resolved.is_empty() {
        return Err(format!("address {address} resolved to no endpoints"));
    }

    let mut connected = None;
    let mut connect_errors: Vec<String> = Vec::new();
    for addr in resolved {
        match connect_to_peer(&state, addr).await {
            Ok(conn) => {
                connected = Some((addr, conn));
                break;
            }
            Err(e) => {
                tracing::warn!("connect_by_address failed to {addr}: {e:#}");
                connect_errors.push(format!("{addr}: {e:#}"));
            }
        }
    }

    let (selected_addr, conn) = connected.ok_or_else(|| {
        if connect_errors.is_empty() {
            "all connection attempts failed".to_string()
        } else {
            format!(
                "all connection attempts failed ({})",
                connect_errors.join(" | ")
            )
        }
    })?;
    let fingerprint = crate::transport::handshake::extract_peer_fp(&conn)
        .map_err(|e| format!("failed to read peer fingerprint: {e:#}"))?;
    if let Err(e) = crate::transport::handshake::do_hello_as_initiator(&conn).await {
        if let Ok(db) = state.db.lock() {
            let _ = crate::db::log_security_event(
                &db,
                "handshake_failed",
                "connect_by_address",
                Some(&fingerprint),
                &e.to_string(),
            );
        }
        return Err(format!("peer handshake failed: {e:#}"));
    }
    conn.close(quinn::VarInt::from_u32(0), b"probe done");

    let trusted = state.is_trusted(&fingerprint).await;
    let (name, payload, is_new) = {
        let mut devices = state.devices.write().await;
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let session_id = format!("manual:{selected_addr}");
        let fallback_name = format!("Manual {selected_addr}");
        let is_new = !devices.contains_key(&fingerprint);
        let device = devices
            .entry(fingerprint.clone())
            .or_insert_with(|| DeviceInfo {
                fingerprint: fingerprint.clone(),
                name: fallback_name.clone(),
                platform: Platform::Unknown,
                trusted,
                sessions: HashMap::new(),
                last_seen: now_unix,
                reachability: ReachabilityStatus::Reachable,
                probe_fail_count: 0,
                last_probe_at: Some(now_unix),
                last_probe_result: Some("ok".to_string()),
                last_probe_error: None,
                last_probe_error_detail: None,
                last_probe_addr: Some(selected_addr.to_string()),
                last_probe_attempted_addrs: vec![selected_addr.to_string()],
                last_resolve_raw_addr_count: 1,
                last_resolve_usable_addr_count: 1,
                last_resolve_hostname: Some(selected_addr.ip().to_string()),
                last_resolve_port: Some(selected_addr.port()),
                last_resolve_at: Some(now_unix),
            });
        device.sessions.insert(
            session_id.clone(),
            SessionInfo {
                session_id,
                addrs: vec![selected_addr],
                last_seen_unix: now_unix,
                last_seen_instant: Instant::now(),
            },
        );
        device.last_seen = now_unix;
        device.reachability = ReachabilityStatus::Reachable;
        device.last_probe_at = Some(now_unix);
        device.probe_fail_count = 0;
        device.last_probe_result = Some("ok".to_string());
        device.last_probe_error = None;
        device.last_probe_error_detail = None;
        device.last_probe_addr = Some(selected_addr.to_string());
        device.last_probe_attempted_addrs = vec![selected_addr.to_string()];
        device.last_resolve_raw_addr_count = 1;
        device.last_resolve_usable_addr_count = 1;
        device.last_resolve_hostname = Some(selected_addr.ip().to_string());
        device.last_resolve_port = Some(selected_addr.port());
        device.last_resolve_at = Some(now_unix);
        device.trusted = trusted;
        let name = device.name.clone();
        let payload = DeviceView::from(&*device);
        (name, payload, is_new)
    };

    if is_new {
        app.emit("device_discovered", &payload).ok();
    } else {
        app.emit("device_updated", &payload).ok();
    }

    Ok(ConnectByAddressResult {
        fingerprint,
        name,
        trusted,
        address: selected_addr.to_string(),
    })
}

#[tauri::command]
pub async fn accept_transfer(
    transfer_id: String,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    let result = accept_pending_transfer(&state, &transfer_id).await;
    if matches!(result.as_ref(), Err(err) if err == REQUEST_EXPIRED_CODE) {
        emit_request_expired(&app, &state, &transfer_id).await;
    }
    result
}

#[tauri::command]
pub async fn accept_and_pair_transfer(
    transfer_id: String,
    sender_fp: String,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    accept_transfer(transfer_id, app.clone(), State::clone(&state)).await?;
    pair_device(sender_fp, app, State::clone(&state)).await
}

#[tauri::command]
pub async fn reject_transfer(
    transfer_id: String,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    let result = reject_pending_transfer(&state, &transfer_id).await;
    if matches!(result.as_ref(), Err(err) if err == REQUEST_EXPIRED_CODE) {
        emit_request_expired(&app, &state, &transfer_id).await;
    }
    result
}

async fn cancel_transfer_inner(transfer_id: &str, app: &AppHandle, state: &Arc<AppState>) -> bool {
    let tx = state.pending_accepts.write().await.remove(transfer_id);
    if let Some(sender) = tx {
        state
            .mark_incoming_request_notification_inactive(transfer_id, Some("E_CANCELLED_BY_USER"))
            .await;
        let _ = sender.send(false);
    }
    let mut cancelled = false;
    let mut metrics_snapshot: Option<(
        crate::state::TransferDirection,
        crate::state::TransferStatus,
        u64,
    )> = None;
    let mut transfers = state.transfers.write().await;
    if let Some(t) = transfers.get_mut(transfer_id) {
        let next_status = match t.direction {
            crate::state::TransferDirection::Send => {
                crate::state::TransferStatus::CancelledBySender
            }
            crate::state::TransferDirection::Receive => {
                crate::state::TransferStatus::CancelledByReceiver
            }
        };
        if t.status != next_status {
            t.status = next_status;
            t.revision += 1;
        }
        t.terminal_reason_code = Some("E_CANCELLED_BY_USER".to_string());
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
        crate::transport::events::emit_transfer_terminal(
            app,
            transfer_id,
            &t.status,
            "E_CANCELLED_BY_USER",
            "UserCancelled",
            t.revision,
            Some("cancel"),
        );
        if let Some(conn) = &t.conn {
            if let Ok(mut cancel_stream) = conn.open_uni().await {
                let _ = crate::transport::protocol::write_message(
                    &mut cancel_stream,
                    &crate::transport::protocol::DashMessage::Cancel(
                        crate::transport::protocol::CancelPayload {
                            reason: crate::transport::protocol::CancelReason::UserCancelled,
                        },
                    ),
                )
                .await;
                let _ = cancel_stream.finish();
            }
            conn.close(quinn::VarInt::from_u32(1), b"Cancelled by user");
        }
        metrics_snapshot = Some((t.direction.clone(), t.status.clone(), t.bytes_transferred));
        cancelled = true;
    }
    drop(transfers);
    if let Some((direction, status, bytes)) = metrics_snapshot {
        state
            .record_transfer_terminal(&direction, &status, bytes)
            .await;
    }
    cancelled
}

#[tauri::command]
pub async fn cancel_transfer(
    transfer_id: String,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    if !cancel_transfer_inner(&transfer_id, &app, &state).await {
        return Err(format!("transfer {transfer_id} not found"));
    }
    Ok(())
}

#[tauri::command]
pub async fn cancel_all_transfers(app: AppHandle, state: AppStateRef<'_>) -> Result<u32, String> {
    let active_ids: Vec<String> = {
        let transfers = state.transfers.read().await;
        transfers
            .values()
            .filter(|t| {
                matches!(
                    t.status,
                    crate::state::TransferStatus::PendingAccept
                        | crate::state::TransferStatus::Transferring
                )
            })
            .map(|t| t.id.clone())
            .collect()
    };
    let mut count = 0u32;
    for transfer_id in active_ids {
        if cancel_transfer_inner(&transfer_id, &app, &state).await {
            count += 1;
        }
    }
    Ok(count)
}

#[tauri::command]
pub async fn retry_transfer(
    transfer_id: String,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    let (peer_fp, retry_paths) = {
        let transfers = state.transfers.read().await;
        let Some(task) = transfers.get(&transfer_id) else {
            return Err(format!("transfer {transfer_id} not found in active cache"));
        };
        if task.direction != crate::state::TransferDirection::Send {
            return Err("retry is only supported for outgoing transfers".into());
        }
        if matches!(
            task.status,
            crate::state::TransferStatus::Draft
                | crate::state::TransferStatus::PendingAccept
                | crate::state::TransferStatus::Transferring
        ) {
            return Err("retry is only available after transfer reaches a terminal state".into());
        }
        (task.peer_fingerprint.clone(), select_retry_paths(task)?)
    };
    send_files_cmd(peer_fp, retry_paths, app, State::clone(&state)).await
}

#[cfg(test)]
mod diagnostics_tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use crate::crypto::Identity;
    use crate::discovery::beacon::{BeaconCadence, PowerProfile};
    use crate::state::{AppConfig, FileItemMeta, TransferDirection, TransferStatus, TransferTask};
    use tokio::sync::oneshot;

    use super::{
        accept_pending_transfer, build_discovery_diagnostics, incoming_request_action_state,
        reject_pending_transfer, select_retry_paths, windows_non_admin_firewall_hint,
        IncomingRequestActionState, REQUEST_EXPIRED_CODE,
    };

    fn build_test_state() -> Arc<crate::state::AppState> {
        let config_dir =
            std::env::temp_dir().join(format!("dashdrop-commands-{}", uuid::Uuid::new_v4()));
        let identity = Identity::load_or_create(&config_dir).expect("identity");
        Arc::new(crate::state::AppState::new(
            identity,
            crate::state::AppConfig::default(),
            rusqlite::Connection::open_in_memory().expect("db"),
        ))
    }

    #[tokio::test]
    async fn accept_pending_transfer_sends_true() {
        let state = build_test_state();
        let transfer_id = "transfer-accept".to_string();
        let (tx, rx) = oneshot::channel::<bool>();
        state
            .pending_accepts
            .write()
            .await
            .insert(transfer_id.clone(), tx);
        state.transfers.write().await.insert(
            transfer_id.clone(),
            TransferTask {
                id: transfer_id.clone(),
                batch_id: None,
                direction: TransferDirection::Receive,
                peer_fingerprint: "fp".into(),
                peer_name: "peer".into(),
                items: vec![],
                status: TransferStatus::PendingAccept,
                bytes_transferred: 0,
                total_bytes: 0,
                revision: 0,
                started_at_unix: 1,
                ended_at_unix: None,
                terminal_reason_code: None,
                error: None,
                source_paths: None,
                source_path_by_file_id: None,
                failed_file_ids: None,
                conn: None,
                ended_at: None,
            },
        );
        state
            .ensure_incoming_request_notification(&transfer_id)
            .await;

        accept_pending_transfer(&state, &transfer_id)
            .await
            .expect("accept should succeed");
        let accepted = rx.await.expect("receiver should get value");
        assert!(accepted);
    }

    #[tokio::test]
    async fn reject_pending_transfer_sends_false() {
        let state = build_test_state();
        let transfer_id = "transfer-reject".to_string();
        let (tx, rx) = oneshot::channel::<bool>();
        state
            .pending_accepts
            .write()
            .await
            .insert(transfer_id.clone(), tx);
        state.transfers.write().await.insert(
            transfer_id.clone(),
            TransferTask {
                id: transfer_id.clone(),
                batch_id: None,
                direction: TransferDirection::Receive,
                peer_fingerprint: "fp".into(),
                peer_name: "peer".into(),
                items: vec![],
                status: TransferStatus::PendingAccept,
                bytes_transferred: 0,
                total_bytes: 0,
                revision: 0,
                started_at_unix: 1,
                ended_at_unix: None,
                terminal_reason_code: None,
                error: None,
                source_paths: None,
                source_path_by_file_id: None,
                failed_file_ids: None,
                conn: None,
                ended_at: None,
            },
        );
        state
            .ensure_incoming_request_notification(&transfer_id)
            .await;

        reject_pending_transfer(&state, &transfer_id)
            .await
            .expect("reject should succeed");
        let accepted = rx.await.expect("receiver should get value");
        assert!(!accepted);
    }

    #[tokio::test]
    async fn expired_click_returns_request_expired_code() {
        let state = build_test_state();
        let transfer_id = "transfer-expired".to_string();
        state.transfers.write().await.insert(
            transfer_id.clone(),
            TransferTask {
                id: transfer_id.clone(),
                batch_id: None,
                direction: TransferDirection::Receive,
                peer_fingerprint: "fp".into(),
                peer_name: "peer".into(),
                items: vec![],
                status: TransferStatus::Rejected,
                bytes_transferred: 0,
                total_bytes: 0,
                revision: 3,
                started_at_unix: 1,
                ended_at_unix: Some(2),
                terminal_reason_code: Some("E_TIMEOUT".into()),
                error: None,
                source_paths: None,
                source_path_by_file_id: None,
                failed_file_ids: None,
                conn: None,
                ended_at: None,
            },
        );
        state
            .ensure_incoming_request_notification(&transfer_id)
            .await;
        state
            .mark_incoming_request_notification_inactive(&transfer_id, Some("E_TIMEOUT"))
            .await;

        assert_eq!(
            incoming_request_action_state(&state, &transfer_id).await,
            IncomingRequestActionState::Expired
        );

        let err = accept_pending_transfer(&state, &transfer_id)
            .await
            .expect_err("expired request should fail");
        assert_eq!(err, REQUEST_EXPIRED_CODE);
    }

    #[test]
    fn cancel_direction_maps_to_expected_terminal_status() {
        let sender_terminal = match TransferDirection::Send {
            TransferDirection::Send => TransferStatus::CancelledBySender,
            TransferDirection::Receive => TransferStatus::CancelledByReceiver,
        };
        let receiver_terminal = match TransferDirection::Receive {
            TransferDirection::Send => TransferStatus::CancelledBySender,
            TransferDirection::Receive => TransferStatus::CancelledByReceiver,
        };
        assert_eq!(sender_terminal, TransferStatus::CancelledBySender);
        assert_eq!(receiver_terminal, TransferStatus::CancelledByReceiver);
    }

    #[test]
    fn retry_paths_selects_only_failed_file_entries() {
        let mut mapping = HashMap::new();
        mapping.insert(1, "/tmp/a.txt".to_string());
        mapping.insert(2, "/tmp/b.txt".to_string());
        let task = TransferTask {
            id: "t1".into(),
            batch_id: None,
            direction: TransferDirection::Send,
            peer_fingerprint: "fp".into(),
            peer_name: "peer".into(),
            items: vec![
                FileItemMeta {
                    file_id: 1,
                    name: "a.txt".into(),
                    rel_path: "a.txt".into(),
                    size: 1,
                    risk_class: None,
                },
                FileItemMeta {
                    file_id: 2,
                    name: "b.txt".into(),
                    rel_path: "b.txt".into(),
                    size: 1,
                    risk_class: None,
                },
            ],
            status: TransferStatus::PartialCompleted,
            bytes_transferred: 1,
            total_bytes: 2,
            revision: 2,
            started_at_unix: 1,
            ended_at_unix: Some(2),
            terminal_reason_code: Some("E_HASH_MISMATCH".into()),
            error: None,
            source_paths: Some(vec!["/tmp/a.txt".into(), "/tmp/b.txt".into()]),
            source_path_by_file_id: Some(mapping),
            failed_file_ids: Some(vec![2]),
            conn: None,
            ended_at: None,
        };
        let selected = select_retry_paths(&task).expect("retry paths");
        assert_eq!(selected, vec!["/tmp/b.txt".to_string()]);
    }

    #[test]
    fn retry_paths_falls_back_to_all_paths_when_mapping_missing() {
        let task = TransferTask {
            id: "t2".into(),
            batch_id: None,
            direction: TransferDirection::Send,
            peer_fingerprint: "fp".into(),
            peer_name: "peer".into(),
            items: vec![],
            status: TransferStatus::Failed,
            bytes_transferred: 0,
            total_bytes: 0,
            revision: 1,
            started_at_unix: 1,
            ended_at_unix: Some(2),
            terminal_reason_code: Some("E_PROTOCOL".into()),
            error: None,
            source_paths: Some(vec!["/tmp/a.txt".into(), "/tmp/b.txt".into()]),
            source_path_by_file_id: None,
            failed_file_ids: Some(vec![99]),
            conn: None,
            ended_at: None,
        };
        let selected = select_retry_paths(&task).expect("fallback paths");
        assert_eq!(
            selected,
            vec!["/tmp/a.txt".to_string(), "/tmp/b.txt".to_string()]
        );
    }

    #[tokio::test]
    async fn discovery_diagnostics_serializes_power_profile_and_interval() {
        let state = Arc::new(crate::state::AppState::new(
            Identity {
                fingerprint: "self-fp".into(),
                cert_der: Vec::new(),
                key_der: Vec::new(),
                device_name: "Test Device".into(),
            },
            AppConfig::default(),
            rusqlite::Connection::open_in_memory().expect("in-memory db"),
        ));
        *state.local_port.write().await = 9443;

        let diagnostics = build_discovery_diagnostics(
            &state,
            BeaconCadence {
                power_profile: PowerProfile::LowPower,
                interval_secs: 12,
            },
        )
        .await;

        assert_eq!(diagnostics["power_profile"], "low_power");
        assert_eq!(diagnostics["beacon_interval_secs"], 12);
        let quick_hints = diagnostics["quick_hints"]
            .as_array()
            .expect("quick hints array");
        assert!(quick_hints.iter().any(|hint| {
            hint.as_str()
                .map(|value| value.contains("discovery latency is intentionally relaxed"))
                .unwrap_or(false)
        }));
    }

    #[tokio::test]
    async fn discovery_diagnostics_exposes_listener_port_and_firewall_state() {
        let state = Arc::new(crate::state::AppState::new(
            Identity {
                fingerprint: "self-fp".into(),
                cert_der: Vec::new(),
                key_der: Vec::new(),
                device_name: "Windows Host".into(),
            },
            AppConfig::default(),
            rusqlite::Connection::open_in_memory().expect("in-memory db"),
        ));
        *state.local_port.write().await = 54001;
        *state.listener_port_mode.write().await = "fallback_random".to_string();
        *state.firewall_rule_state.write().await = "user_scope_unmanaged".to_string();

        let diagnostics = build_discovery_diagnostics(
            &state,
            BeaconCadence {
                power_profile: PowerProfile::Battery,
                interval_secs: 3,
            },
        )
        .await;

        assert_eq!(
            diagnostics["runtime"]["listener_port_mode"],
            "fallback_random"
        );
        assert_eq!(
            diagnostics["runtime"]["firewall_rule_state"],
            "user_scope_unmanaged"
        );
        assert_eq!(diagnostics["listener_port_mode"], "fallback_random");
        assert_eq!(diagnostics["firewall_rule_state"], "user_scope_unmanaged");
    }

    #[tokio::test]
    async fn discovery_diagnostics_include_slo_observability_snapshot() {
        let state = Arc::new(crate::state::AppState::new(
            Identity {
                fingerprint: "self-fp".into(),
                cert_der: Vec::new(),
                key_der: Vec::new(),
                device_name: "Observer".into(),
            },
            AppConfig::default(),
            rusqlite::Connection::open_in_memory().expect("in-memory db"),
        ));

        state.record_device_visibility("peer-fp").await;
        state.record_sender_dispatch("transfer-1", "peer-fp").await;
        state
            .record_receiver_fallback_prompted("transfer-1", "peer-fp")
            .await;

        let diagnostics = build_discovery_diagnostics(
            &state,
            BeaconCadence {
                power_profile: PowerProfile::Ac,
                interval_secs: 3,
            },
        )
        .await;

        assert!(
            diagnostics["slo_observability"]["devices"]["peer-fp"]["remote_peer_online_at"]
                .as_u64()
                .is_some()
        );
        assert!(
            diagnostics["slo_observability"]["devices"]["peer-fp"]["local_device_visible_at"]
                .as_u64()
                .is_some()
        );
        assert!(
            diagnostics["slo_observability"]["transfers"]["transfer-1"]["sender_dispatch_at"]
                .as_u64()
                .is_some()
        );
        assert!(diagnostics["slo_observability"]["transfers"]["transfer-1"]
            ["receiver_fallback_prompted_at"]
            .as_u64()
            .is_some());
    }

    #[test]
    fn windows_non_admin_hint_mentions_manual_firewall_steps() {
        let hint = windows_non_admin_firewall_hint(54001, "fallback_random");
        assert!(hint.contains("Windows Defender Firewall"));
        assert!(hint.contains("53319"));
        assert!(hint.contains("54001"));
    }
}

#[tauri::command]
pub async fn open_transfer_folder(
    transfer_id: String,
    state: AppStateRef<'_>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    use tauri::Manager;
    let custom_dir = state.config.read().await.download_dir.clone();
    let base_dir = custom_dir.map(std::path::PathBuf::from).unwrap_or_else(|| {
        app.path().download_dir().unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            std::path::PathBuf::from(home).join("Downloads")
        })
    });
    let save_root = base_dir.join("DashDrop").join(transfer_id);

    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .reveal_item_in_dir(&save_root)
        .or_else(|_| {
            app.opener()
                .open_path(save_root.to_string_lossy().to_string(), None::<&str>)
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_app_config(state: AppStateRef<'_>) -> Result<crate::state::AppConfig, String> {
    Ok(state.config.read().await.clone())
}

#[tauri::command]
pub async fn set_app_config(
    config: crate::state::AppConfig,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    if config.max_parallel_streams == 0 || config.max_parallel_streams > 32 {
        return Err("max_parallel_streams must be between 1 and 32".to_string());
    }
    let attempted_device_name = config.device_name.clone();
    let old_name = state.config.read().await.device_name.clone();
    if let Some(download_dir) = &config.download_dir {
        let dir = std::path::PathBuf::from(download_dir);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("download directory is not usable: {e}"))?;
        let probe = dir.join(".dashdrop-write-test");
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&probe)
            .map_err(|e| format!("download directory is not writable: {e}"))?;
        let _ = std::fs::remove_file(probe);
    }

    let previous_config = state.config.read().await.clone();
    *state.config.write().await = config;
    if old_name != state.config.read().await.device_name {
        if let Err(e) = crate::discovery::service::reregister_service(Arc::clone(&state)).await {
            *state.config.write().await = previous_config;
            app.emit("system_error", serde_json::json!({
                "code": "MDNS_REREGISTER_FAILED",
                "subsystem": "mdns",
                "message": format!("Device name update rolled back because mDNS refresh failed: {e:#}"),
                "attempted_device_name": attempted_device_name,
                "rollback_device_name": state.config.read().await.device_name.clone(),
            })).ok();
            return Err(format!(
                "device name update rolled back because mDNS refresh failed: {e:#}"
            ));
        }
    }
    persist_runtime_state(&state).await?;
    Ok(())
}

// ─── App info commands ───────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_local_identity(
    state: AppStateRef<'_>,
) -> Result<crate::state::LocalIdentityView, String> {
    Ok(crate::dto::local_identity_view(
        state.identity.fingerprint.clone(),
        state.config.read().await.device_name.clone(),
        *state.local_port.read().await,
    ))
}

#[tauri::command]
pub async fn get_transfers(state: AppStateRef<'_>) -> Result<Vec<TransferView>, String> {
    let transfers = state.transfers.read().await;
    Ok(transfers.values().map(TransferView::from).collect())
}

#[tauri::command]
pub async fn get_transfer(
    transfer_id: String,
    state: AppStateRef<'_>,
) -> Result<Option<TransferView>, String> {
    let transfers = state.transfers.read().await;
    Ok(transfers.get(&transfer_id).map(TransferView::from))
}

#[tauri::command]
pub async fn get_transfer_history(
    limit: u32,
    offset: u32,
    state: AppStateRef<'_>,
) -> Result<Vec<TransferView>, String> {
    let guard = state.db.lock().map_err(|_| "DB lock poisoned")?;
    let history = crate::db::get_history(&guard, limit, offset).map_err(|e| e.to_string())?;
    Ok(history.iter().map(TransferView::from).collect())
}

#[tauri::command]
pub async fn get_security_events(
    limit: u32,
    offset: u32,
    state: AppStateRef<'_>,
) -> Result<Vec<crate::state::SecurityEvent>, String> {
    let guard = state.db.lock().map_err(|_| "DB lock poisoned")?;
    crate::db::get_security_events(&guard, limit, offset).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_security_posture() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "secure_store_available": crate::crypto::secret_store::secure_store_available(),
    }))
}

#[tauri::command]
pub async fn get_runtime_status(
    state: AppStateRef<'_>,
) -> Result<crate::state::RuntimeStatus, String> {
    Ok(state.runtime_status().await)
}

struct DiscoveryQuickHintContext<'a> {
    own_platform: &'a str,
    mdns_daemon_initialized: bool,
    browser_active: bool,
    browser_restart_count: u64,
    search_started_events: u64,
    resolved_events: u64,
    reachable_devices: usize,
    listener_mode: &'a str,
    listener_port_mode: &'a str,
    firewall_rule_state: &'a str,
    local_port: u16,
    ipv6_only_candidates: usize,
    resolved_no_usable_addrs: u64,
    scope_less_link_local_peers: usize,
    stale_session_pruned: u64,
    beacon_sent: u64,
    beacon_received: u64,
    device_rows_empty: bool,
    self_filtered: u64,
    resolved_missing_fp_txt: u64,
}

fn windows_non_admin_firewall_hint(local_port: u16, listener_port_mode: &str) -> String {
    if listener_port_mode == "fallback_random" && local_port > 0 && local_port != 53319 {
        format!(
            "DashDrop is running without Windows administrator rights, so firewall rules were not managed automatically. Allow the Windows Defender Firewall prompt if shown, or manually add inbound UDP allow rules for DashDrop and ports 53319 and {local_port}."
        )
    } else {
        "DashDrop is running without Windows administrator rights, so firewall rules were not managed automatically. Allow the Windows Defender Firewall prompt if shown, or manually add an inbound UDP allow rule for DashDrop and port 53319.".to_string()
    }
}

fn build_discovery_quick_hints(ctx: &DiscoveryQuickHintContext<'_>) -> Vec<String> {
    let mut quick_hints = Vec::new();
    if !ctx.mdns_daemon_initialized {
        quick_hints.push(
            "Local mDNS responder is not fully initialized; this device may not be discoverable."
                .to_string(),
        );
    }
    if !ctx.browser_active {
        quick_hints.push(
            "mDNS browser is currently inactive and auto-restarting; discovery may be temporarily stale."
                .to_string(),
        );
    }
    if ctx.search_started_events == 0 && ctx.browser_restart_count == 0 {
        quick_hints.push(
            "mDNS browser has not reported SearchStarted; check local-network permission and multicast interface availability."
                .to_string(),
        );
    } else if ctx.resolved_events == 0 {
        quick_hints.push(
            "No peers resolved from mDNS browse yet; likely multicast traffic is blocked across firewall/VLAN/subnet."
                .to_string(),
        );
    }
    if ctx.resolved_events > 0 && ctx.reachable_devices == 0 {
        quick_hints.push(
            "Peers were discovered but none are probe-reachable; verify firewall rules for UDP listener port and QUIC traffic."
                .to_string(),
        );
    }
    if ctx.listener_port_mode == "fallback_random" && ctx.local_port > 0 {
        quick_hints.push(format!(
            "Preferred QUIC port 53319 is unavailable on this host, so DashDrop is listening on UDP {} for this session.",
            ctx.local_port
        ));
    }
    if ctx.listener_mode == "ipv4_only_fallback" {
        quick_hints.push(
            "Listener is running in IPv4-only fallback mode; IPv6-only peers may fail to connect."
                .to_string(),
        );
        if ctx.ipv6_only_candidates > 0 {
            quick_hints.push(
                "Some discovered peers currently advertise IPv6-only candidate addresses while listener is IPv4-only."
                    .to_string(),
            );
        }
    }
    if ctx.resolved_no_usable_addrs > 0 {
        quick_hints.push(
            "Some peers resolved without usable addresses; inspect virtual adapters/VPN interfaces and peer IP advertisement."
                .to_string(),
        );
    }
    if ctx.scope_less_link_local_peers > 0 {
        quick_hints.push(
            "Some peers are advertising scope-less IPv6 link-local addresses (fe80:: without interface scope); these are often not connectable across platforms."
                .to_string(),
        );
    }
    if ctx.stale_session_pruned > 0 {
        quick_hints.push(
            "Stale discovery sessions were pruned locally; if peers keep flapping, compare diagnostics from both ends for mDNS remove/resolved parity."
                .to_string(),
        );
    }
    if ctx.beacon_sent > 0 && ctx.beacon_received == 0 && ctx.resolved_events == 0 {
        quick_hints.push(
            "No inbound discovery packets seen from mDNS or UDP beacon; check AP isolation, VLAN segmentation, or host firewall multicast/broadcast rules."
                .to_string(),
        );
    } else if ctx.beacon_received > 0 && ctx.resolved_events == 0 {
        quick_hints.push(
            "UDP beacon fallback is receiving peers while mDNS is silent; mDNS multicast is likely blocked on this network."
                .to_string(),
        );
    }
    if ctx.resolved_events > 0 && ctx.device_rows_empty && ctx.self_filtered >= ctx.resolved_events
    {
        quick_hints.push(
            "mDNS resolved only self-advertisements on this host; no remote DashDrop peers observed."
                .to_string(),
        );
    }
    if ctx.resolved_missing_fp_txt > 0 {
        quick_hints.push(
            "Some _dashdrop records were missing fp TXT; verify both peers run compatible builds and advertise required TXT keys."
                .to_string(),
        );
    }
    if ctx.own_platform == "Windows" {
        match ctx.firewall_rule_state {
            "user_scope_unmanaged" => {
                quick_hints.push(windows_non_admin_firewall_hint(
                    ctx.local_port,
                    ctx.listener_port_mode,
                ));
            }
            "unknown" => quick_hints.push(
                "Windows firewall rule state is unknown. If peers cannot reach this device, allow DashDrop through Windows Defender Firewall or add an inbound UDP rule for the active listener port."
                    .to_string(),
            ),
            _ => {}
        }
    }
    quick_hints
}

#[tauri::command]
pub async fn copy_to_clipboard(text: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let mut clipboard =
            arboard::Clipboard::new().map_err(|e| format!("native clipboard unavailable: {e}"))?;
        clipboard
            .set_text(text)
            .map_err(|e| format!("native clipboard write failed: {e}"))?;
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("clipboard worker failed: {e}"))?
}

#[tauri::command]
pub async fn get_discovery_diagnostics(
    state: AppStateRef<'_>,
) -> Result<serde_json::Value, String> {
    let state = Arc::clone(&state);
    let cadence = crate::discovery::beacon::current_beacon_cadence();
    Ok(build_discovery_diagnostics(&state, cadence).await)
}

async fn build_discovery_diagnostics(
    state: &Arc<AppState>,
    beacon_cadence: crate::discovery::beacon::BeaconCadence,
) -> serde_json::Value {
    let runtime = state.runtime_status().await;
    let own_platform = Platform::current();
    let mdns_service_fullname = state.mdns_service_fullname.read().await.clone();
    let mdns_interface_policy = state.mdns_interface_policy.read().await.clone();
    let mdns_enabled_interfaces = state.mdns_enabled_interfaces.read().await.clone();
    let mdns_last_search_started = state.mdns_last_search_started.read().await.clone();
    let mdns_daemon_initialized = state.mdns.get().is_some();
    let session_index_count = state.session_index.read().await.len();
    let discovery_event_counts = state.discovery_event_counts_snapshot().await;
    let discovery_failure_counts = state.discovery_failure_counts_snapshot().await;
    let browser_status = state.browser_status_snapshot().await;
    let listener_mode = state.listener_mode.read().await.clone();
    let listener_port_mode = state.listener_port_mode.read().await.clone();
    let firewall_rule_state = state.firewall_rule_state.read().await.clone();
    let listener_addrs = state.listener_addrs.read().await.clone();
    let network_interfaces = collect_network_interfaces();
    let slo_observability = state.slo_observability_snapshot().await;
    let devices = state.devices.read().await;

    let device_rows: Vec<serde_json::Value> = devices.values().map(discovery_device_row).collect();

    let resolved_events = discovery_event_counts
        .get("service_resolved")
        .copied()
        .unwrap_or_default();
    let beacon_sent = discovery_event_counts
        .get("beacon_sent")
        .copied()
        .unwrap_or_default();
    let beacon_received = discovery_event_counts
        .get("beacon_received")
        .copied()
        .unwrap_or_default();
    let search_started_events = discovery_event_counts
        .get("search_started")
        .copied()
        .unwrap_or_default();
    let reachable_devices = devices
        .values()
        .filter(|d| d.reachability == ReachabilityStatus::Reachable)
        .count();
    let ipv6_only_candidates = devices
        .values()
        .filter(|d| {
            let best = d.best_addrs().unwrap_or_default();
            !best.is_empty() && best.iter().all(|addr| addr.is_ipv6())
        })
        .count();
    let scope_less_link_local_peers = devices
        .values()
        .filter(|d| {
            d.sessions.values().any(|s| {
                s.addrs.iter().any(|addr| match addr {
                    std::net::SocketAddr::V6(v6) => {
                        v6.ip().is_unicast_link_local() && v6.scope_id() == 0
                    }
                    std::net::SocketAddr::V4(_) => false,
                })
            })
        })
        .count();
    let local_instance_name = mdns_service_fullname
        .as_ref()
        .and_then(|s| s.split('.').next().map(|part| part.to_string()));
    let self_filtered = discovery_event_counts
        .get("resolved_self_filtered")
        .copied()
        .unwrap_or_default();
    let mut quick_hints = build_discovery_quick_hints(&DiscoveryQuickHintContext {
        own_platform,
        mdns_daemon_initialized: mdns_daemon_initialized && mdns_service_fullname.is_some(),
        browser_active: browser_status.active,
        browser_restart_count: browser_status.restart_count,
        search_started_events,
        resolved_events,
        reachable_devices,
        listener_mode: &listener_mode,
        listener_port_mode: &listener_port_mode,
        firewall_rule_state: &firewall_rule_state,
        local_port: runtime.local_port,
        ipv6_only_candidates,
        resolved_no_usable_addrs: discovery_failure_counts
            .get("resolved_no_usable_addrs")
            .copied()
            .unwrap_or_default(),
        scope_less_link_local_peers,
        stale_session_pruned: discovery_event_counts
            .get("stale_session_pruned")
            .copied()
            .unwrap_or_default(),
        beacon_sent,
        beacon_received,
        device_rows_empty: device_rows.is_empty(),
        self_filtered,
        resolved_missing_fp_txt: discovery_failure_counts
            .get("resolved_missing_fp_txt")
            .copied()
            .unwrap_or_default(),
    });
    if beacon_cadence.power_profile == crate::discovery::beacon::PowerProfile::LowPower {
        quick_hints.push(
            "Low-power mode is active, so discovery latency is intentionally relaxed to reduce energy use; beacon-based peer appearance may take longer than on AC."
                .to_string(),
        );
    }

    serde_json::json!({
        "runtime": runtime,
        "service_type": crate::discovery::service::SERVICE_TYPE,
        "beacon_port": crate::discovery::beacon::DISCOVERY_BEACON_PORT,
        "power_profile": beacon_cadence.power_profile,
        "beacon_interval_secs": beacon_cadence.interval_secs,
        "own_fingerprint": state.identity.fingerprint.clone(),
        "own_platform": own_platform,
        "mdns_daemon_initialized": mdns_daemon_initialized,
        "mdns_service_fullname": mdns_service_fullname,
        "mdns_interface_policy": mdns_interface_policy,
        "mdns_enabled_interfaces": mdns_enabled_interfaces,
        "mdns_last_search_started": mdns_last_search_started,
        "local_instance_name": local_instance_name,
        "listener_mode": listener_mode,
        "listener_port_mode": listener_port_mode,
        "firewall_rule_state": firewall_rule_state,
        "listener_addrs": listener_addrs,
        "network_interfaces": network_interfaces,
        "slo_observability": slo_observability,
        "browser_status": serde_json::json!({
            "active": browser_status.active,
            "restart_count": browser_status.restart_count,
            "last_disconnect_at": browser_status.last_disconnect_at,
            "last_search_started": mdns_last_search_started,
        }),
        "session_index_count": session_index_count,
        "session_stale_ttl_secs": 90,
        "discovery_event_counts": discovery_event_counts,
        "discovery_failure_counts": discovery_failure_counts,
        "quick_hints": quick_hints,
        "device_count": device_rows.len(),
        "devices": device_rows,
    })
}

fn collect_network_interfaces() -> Vec<serde_json::Value> {
    let mut grouped: BTreeMap<String, (bool, Vec<String>, Vec<String>)> = BTreeMap::new();
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for iface in ifaces {
            let entry = grouped.entry(iface.name.clone()).or_insert((
                iface.is_loopback(),
                Vec::new(),
                Vec::new(),
            ));
            entry.0 = entry.0 || iface.is_loopback();
            let ip = iface.ip().to_string();
            if iface.ip().is_ipv4() {
                if !entry.1.contains(&ip) {
                    entry.1.push(ip);
                }
            } else if !entry.2.contains(&ip) {
                entry.2.push(ip);
            }
        }
    }
    grouped
        .into_iter()
        .map(|(name, (is_loopback, ipv4, ipv6))| {
            serde_json::json!({
                "name": name,
                "is_loopback": is_loopback,
                "ipv4": ipv4,
                "ipv6": ipv6,
            })
        })
        .collect()
}

fn discovery_device_row(d: &DeviceInfo) -> serde_json::Value {
    let scope_less_link_local_ipv6 = d
        .sessions
        .values()
        .flat_map(|s| s.addrs.iter())
        .filter(|addr| match addr {
            std::net::SocketAddr::V6(v6) => v6.ip().is_unicast_link_local() && v6.scope_id() == 0,
            std::net::SocketAddr::V4(_) => false,
        })
        .count();
    let mut sessions: Vec<serde_json::Value> = d
        .sessions
        .values()
        .map(|s| {
            serde_json::json!({
                "session_id": s.session_id,
                "last_seen_unix": s.last_seen_unix,
                "addrs": s.addrs.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
            })
        })
        .collect();
    sessions.sort_by_key(|s| std::cmp::Reverse(s["last_seen_unix"].as_u64().unwrap_or_default()));
    let best_addrs = d
        .best_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|a| a.to_string())
        .collect::<Vec<_>>();
    serde_json::json!({
        "fingerprint": d.fingerprint,
        "name": d.name,
        "platform": d.platform,
        "trusted": d.trusted,
        "reachability": d.reachability,
        "probe_fail_count": d.probe_fail_count,
        "last_probe_at": d.last_probe_at,
        "last_seen": d.last_seen,
        "session_count": d.sessions.len(),
        "best_addrs": best_addrs,
        "scope_less_link_local_ipv6_count": scope_less_link_local_ipv6,
        "last_resolve_stats": {
            "raw_addr_count": d.last_resolve_raw_addr_count,
            "usable_addr_count": d.last_resolve_usable_addr_count,
            "hostname": d.last_resolve_hostname,
            "port": d.last_resolve_port,
            "at": d.last_resolve_at,
        },
        "last_probe_result": {
            "result": d.last_probe_result,
            "error": d.last_probe_error,
            "error_detail": d.last_probe_error_detail,
            "addr": d.last_probe_addr,
            "attempted_addrs": d.last_probe_attempted_addrs,
            "at": d.last_probe_at,
        },
        "sessions": sessions,
    })
}

#[cfg(test)]
mod tests {
    use super::{collect_network_interfaces, discovery_device_row};
    use crate::state::{DeviceInfo, Platform, ReachabilityStatus, SessionInfo};
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::str::FromStr;
    use std::time::Instant;

    #[test]
    fn discovery_device_row_contains_resolve_and_probe_details() {
        let mut sessions = HashMap::new();
        sessions.insert(
            "s1".to_string(),
            SessionInfo {
                session_id: "s1".to_string(),
                addrs: vec![SocketAddr::from_str("192.168.1.8:9443").expect("addr")],
                last_seen_unix: 200,
                last_seen_instant: Instant::now(),
            },
        );
        let device = DeviceInfo {
            fingerprint: "fp-1".to_string(),
            name: "peer".to_string(),
            platform: Platform::Windows,
            trusted: false,
            sessions,
            last_seen: 200,
            reachability: ReachabilityStatus::Discovered,
            probe_fail_count: 2,
            last_probe_at: Some(199),
            last_probe_result: Some("failed".to_string()),
            last_probe_error: Some("timeout".to_string()),
            last_probe_error_detail: Some("connection timed out".to_string()),
            last_probe_addr: Some("192.168.1.8:9443".to_string()),
            last_probe_attempted_addrs: vec!["192.168.1.8:9443".to_string()],
            last_resolve_raw_addr_count: 2,
            last_resolve_usable_addr_count: 1,
            last_resolve_hostname: Some("peer.local.".to_string()),
            last_resolve_port: Some(9443),
            last_resolve_at: Some(198),
        };

        let row = discovery_device_row(&device);
        assert_eq!(
            row["last_resolve_stats"]["raw_addr_count"].as_u64(),
            Some(2)
        );
        assert_eq!(
            row["last_resolve_stats"]["usable_addr_count"].as_u64(),
            Some(1)
        );
        assert_eq!(row["last_probe_result"]["result"].as_str(), Some("failed"));
        assert_eq!(row["last_probe_result"]["error"].as_str(), Some("timeout"));
        assert_eq!(
            row["last_probe_result"]["addr"].as_str(),
            Some("192.168.1.8:9443")
        );
    }

    #[test]
    fn collect_network_interfaces_has_family_buckets() {
        let rows = collect_network_interfaces();
        for row in rows {
            assert!(row.get("name").is_some());
            assert!(row.get("is_loopback").is_some());
            assert!(row.get("ipv4").is_some());
            assert!(row.get("ipv6").is_some());
        }
    }
}

#[tauri::command]
pub async fn get_transfer_metrics(
    state: AppStateRef<'_>,
) -> Result<crate::state::TransferMetrics, String> {
    let guard = state.db.lock().map_err(|_| "DB lock poisoned")?;
    crate::db::get_transfer_metrics(&guard).map_err(|e| e.to_string())
}
