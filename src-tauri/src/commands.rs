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

type PendingAcceptMap =
    Arc<tokio::sync::RwLock<std::collections::HashMap<String, tokio::sync::oneshot::Sender<bool>>>>;

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

async fn accept_pending_transfer(
    pending_accepts: &PendingAcceptMap,
    transfer_id: &str,
) -> Result<(), String> {
    let tx = pending_accepts.write().await.remove(transfer_id);
    if let Some(sender) = tx {
        let _ = sender.send(true);
        Ok(())
    } else {
        Err(format!(
            "transfer {transfer_id} not found or already handled"
        ))
    }
}

async fn reject_pending_transfer(pending_accepts: &PendingAcceptMap, transfer_id: &str) {
    let tx = pending_accepts.write().await.remove(transfer_id);
    if let Some(sender) = tx {
        let _ = sender.send(false);
    }
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
    let transfer_id = uuid::Uuid::new_v4().to_string();
    let transfer_id_clone = transfer_id.clone();

    tokio::spawn(async move {
        let outcome = send_files(
            transfer_id_clone.clone(),
            peer_fp,
            path_bufs,
            conn,
            app.clone(),
            state.clone(),
        )
        .await;
        match outcome {
            Ok(crate::transport::protocol::TransferOutcome::Success) => {
                tracing::info!("Transfer complete");
            }
            Ok(other) => {
                tracing::warn!("Transfer ended: {:?}", other);
            }
            Err(e) => {
                tracing::error!("Send failed: {e:#}");
                let detail = format!("{e:#}");
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
pub async fn accept_transfer(transfer_id: String, state: AppStateRef<'_>) -> Result<(), String> {
    accept_pending_transfer(&state.pending_accepts, &transfer_id).await
}

#[tauri::command]
pub async fn accept_and_pair_transfer(
    transfer_id: String,
    sender_fp: String,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    // Pair first
    pair_device(sender_fp, app, State::clone(&state)).await?;
    // Then accept
    accept_transfer(transfer_id, state).await
}

#[tauri::command]
pub async fn reject_transfer(transfer_id: String, state: AppStateRef<'_>) -> Result<(), String> {
    reject_pending_transfer(&state.pending_accepts, &transfer_id).await;
    Ok(())
}

async fn cancel_transfer_inner(transfer_id: &str, app: &AppHandle, state: &Arc<AppState>) -> bool {
    let tx = state.pending_accepts.write().await.remove(transfer_id);
    if let Some(sender) = tx {
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

    use crate::state::{FileItemMeta, TransferDirection, TransferStatus, TransferTask};
    use tokio::sync::oneshot;
    use tokio::sync::RwLock;

    use super::{accept_pending_transfer, reject_pending_transfer, select_retry_paths};

    #[tokio::test]
    async fn accept_pending_transfer_sends_true() {
        let pending_accepts: Arc<RwLock<HashMap<String, oneshot::Sender<bool>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let transfer_id = "transfer-accept".to_string();
        let (tx, rx) = oneshot::channel::<bool>();
        pending_accepts
            .write()
            .await
            .insert(transfer_id.clone(), tx);

        accept_pending_transfer(&pending_accepts, &transfer_id)
            .await
            .expect("accept should succeed");
        let accepted = rx.await.expect("receiver should get value");
        assert!(accepted);
    }

    #[tokio::test]
    async fn reject_pending_transfer_sends_false() {
        let pending_accepts: Arc<RwLock<HashMap<String, oneshot::Sender<bool>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let transfer_id = "transfer-reject".to_string();
        let (tx, rx) = oneshot::channel::<bool>();
        pending_accepts
            .write()
            .await
            .insert(transfer_id.clone(), tx);

        reject_pending_transfer(&pending_accepts, &transfer_id).await;
        let accepted = rx.await.expect("receiver should get value");
        assert!(!accepted);
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
            direction: TransferDirection::Send,
            peer_fingerprint: "fp".into(),
            peer_name: "peer".into(),
            items: vec![
                FileItemMeta {
                    file_id: 1,
                    name: "a.txt".into(),
                    rel_path: "a.txt".into(),
                    size: 1,
                },
                FileItemMeta {
                    file_id: 2,
                    name: "b.txt".into(),
                    rel_path: "b.txt".into(),
                    size: 1,
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

#[tauri::command]
pub async fn get_discovery_diagnostics(
    state: AppStateRef<'_>,
) -> Result<serde_json::Value, String> {
    let runtime = state.runtime_status().await;
    let mdns_service_fullname = state.mdns_service_fullname.read().await.clone();
    let mdns_last_search_started = state.mdns_last_search_started.read().await.clone();
    let mdns_daemon_initialized = state.mdns.get().is_some();
    let session_index_count = state.session_index.read().await.len();
    let discovery_event_counts = state.discovery_event_counts_snapshot().await;
    let discovery_failure_counts = state.discovery_failure_counts_snapshot().await;
    let browser_status = state.browser_status_snapshot().await;
    let listener_mode = state.listener_mode.read().await.clone();
    let listener_addrs = state.listener_addrs.read().await.clone();
    let network_interfaces = collect_network_interfaces();
    let devices = state.devices.read().await;

    let device_rows: Vec<serde_json::Value> = devices
        .values()
        .map(discovery_device_row)
        .collect();

    let resolved_events = discovery_event_counts
        .get("service_resolved")
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
    let mut quick_hints = Vec::new();
    if !mdns_daemon_initialized || mdns_service_fullname.is_none() {
        quick_hints.push(
            "Local mDNS responder is not fully initialized; this device may not be discoverable."
                .to_string(),
        );
    }
    if !browser_status.active {
        quick_hints.push(
            "mDNS browser is currently inactive and auto-restarting; discovery may be temporarily stale."
                .to_string(),
        );
    }
    if search_started_events == 0 && browser_status.restart_count == 0 {
        quick_hints.push(
            "mDNS browser has not reported SearchStarted; check local-network permission and multicast interface availability."
                .to_string(),
        );
    } else if resolved_events == 0 {
        quick_hints.push(
            "No peers resolved from mDNS browse yet; likely multicast traffic is blocked across firewall/VLAN/subnet."
                .to_string(),
        );
    }
    if resolved_events > 0 && reachable_devices == 0 {
        quick_hints.push(
            "Peers were discovered but none are probe-reachable; verify firewall rules for UDP listener port and QUIC traffic."
                .to_string(),
        );
    }
    if listener_mode == "ipv4_only_fallback" {
        quick_hints.push(
            "Listener is running in IPv4-only fallback mode; IPv6-only peers may fail to connect."
                .to_string(),
        );
        if ipv6_only_candidates > 0 {
            quick_hints.push(
                "Some discovered peers currently advertise IPv6-only candidate addresses while listener is IPv4-only."
                    .to_string(),
            );
        }
    }
    if discovery_failure_counts
        .get("resolved_no_usable_addrs")
        .copied()
        .unwrap_or_default()
        > 0
    {
        quick_hints.push(
            "Some peers resolved without usable addresses; inspect virtual adapters/VPN interfaces and peer IP advertisement."
                .to_string(),
        );
    }
    if scope_less_link_local_peers > 0 {
        quick_hints.push(
            "Some peers are advertising scope-less IPv6 link-local addresses (fe80:: without interface scope); these are often not connectable across platforms."
                .to_string(),
        );
    }

    Ok(serde_json::json!({
        "runtime": runtime,
        "service_type": crate::discovery::service::SERVICE_TYPE,
        "own_fingerprint": state.identity.fingerprint.clone(),
        "own_platform": Platform::current(),
        "mdns_daemon_initialized": mdns_daemon_initialized,
        "mdns_service_fullname": mdns_service_fullname,
        "mdns_last_search_started": mdns_last_search_started,
        "local_instance_name": local_instance_name,
        "listener_mode": listener_mode,
        "listener_addrs": listener_addrs,
        "network_interfaces": network_interfaces,
        "browser_status": serde_json::json!({
            "active": browser_status.active,
            "restart_count": browser_status.restart_count,
            "last_disconnect_at": browser_status.last_disconnect_at,
            "last_search_started": mdns_last_search_started,
        }),
        "session_index_count": session_index_count,
        "discovery_event_counts": discovery_event_counts,
        "discovery_failure_counts": discovery_failure_counts,
        "quick_hints": quick_hints,
        "device_count": device_rows.len(),
        "devices": device_rows,
    }))
}

fn collect_network_interfaces() -> Vec<serde_json::Value> {
    let mut grouped: BTreeMap<String, (bool, Vec<String>, Vec<String>)> = BTreeMap::new();
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for iface in ifaces {
            let entry = grouped
                .entry(iface.name.clone())
                .or_insert((iface.is_loopback(), Vec::new(), Vec::new()));
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
        assert_eq!(row["last_resolve_stats"]["raw_addr_count"].as_u64(), Some(2));
        assert_eq!(row["last_resolve_stats"]["usable_addr_count"].as_u64(), Some(1));
        assert_eq!(
            row["last_probe_result"]["result"].as_str(),
            Some("failed")
        );
        assert_eq!(
            row["last_probe_result"]["error"].as_str(),
            Some("timeout")
        );
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
