use std::collections::{HashMap, HashSet};
use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use tauri::{AppHandle, Emitter};

use crate::dto::{DeviceView, TrustedPeerView};
use crate::local_ipc::{
    ConnectByAddressResult, LocalIpcCommand, LocalIpcError, LocalIpcResponse, LocalIpcWireRequest,
    LocalIpcWireResponse,
};
use crate::state::{AppConfig, AppState, DeviceInfo, Platform, ReachabilityStatus, SessionInfo};
use crate::transport::connect_to_peer;
use crate::transport::sender::send_files as transport_send_files;

const REQUEST_EXPIRED_CODE: &str = "E_REQUEST_EXPIRED";
const REQUEST_EXPIRED_CAUSE: &str = "NotificationExpired";
const REQUEST_EXPIRED_PHASE: &str = "notification_action";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IncomingRequestActionState {
    Pending,
    Expired,
    StaleNotification,
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

pub struct AppCoreService {
    state: Arc<AppState>,
    app: Option<AppHandle>,
}

impl AppCoreService {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state, app: None }
    }

    pub fn with_app(state: Arc<AppState>, app: AppHandle) -> Self {
        Self {
            state,
            app: Some(app),
        }
    }

    pub async fn dispatch(&self, command: LocalIpcCommand) -> Result<LocalIpcResponse, String> {
        match command {
            LocalIpcCommand::DiscoverList => Ok(LocalIpcResponse::Devices {
                devices: self.get_devices().await?,
            }),
            LocalIpcCommand::DiscoverConnectByAddress { address } => {
                Ok(LocalIpcResponse::ConnectByAddress {
                    result: self.connect_by_address(address).await?,
                })
            }
            LocalIpcCommand::TrustList => Ok(LocalIpcResponse::TrustedPeers {
                trusted_peers: self.get_trusted_peers().await?,
            }),
            LocalIpcCommand::TrustPair { fingerprint } => {
                self.pair_device(&fingerprint).await?;
                Ok(LocalIpcResponse::Ack)
            }
            LocalIpcCommand::TrustUnpair { fingerprint } => {
                self.unpair_device(&fingerprint).await?;
                Ok(LocalIpcResponse::Ack)
            }
            LocalIpcCommand::TrustSetAlias { fingerprint, alias } => {
                self.set_trusted_alias(&fingerprint, alias).await?;
                Ok(LocalIpcResponse::Ack)
            }
            LocalIpcCommand::ConfigGet => Ok(LocalIpcResponse::AppConfig {
                config: self.get_app_config().await?,
            }),
            LocalIpcCommand::ConfigSet { config } => {
                self.set_app_config(config).await?;
                Ok(LocalIpcResponse::Ack)
            }
            LocalIpcCommand::TransferSend {
                peer_fingerprint,
                paths,
            } => {
                self.send_files(peer_fingerprint, paths).await?;
                Ok(LocalIpcResponse::Ack)
            }
            LocalIpcCommand::TransferAccept {
                transfer_id,
                notification_id,
            } => {
                self.accept_transfer(&transfer_id, &notification_id).await?;
                Ok(LocalIpcResponse::Ack)
            }
            LocalIpcCommand::TransferReject {
                transfer_id,
                notification_id,
            } => {
                self.reject_transfer(&transfer_id, &notification_id).await?;
                Ok(LocalIpcResponse::Ack)
            }
            LocalIpcCommand::TransferCancel { transfer_id } => {
                self.cancel_transfer(&transfer_id).await?;
                Ok(LocalIpcResponse::Ack)
            }
            LocalIpcCommand::TransferCancelAll => Ok(LocalIpcResponse::CancelledTransfers {
                count: self.cancel_all_transfers().await?,
            }),
            LocalIpcCommand::TransferRetry { transfer_id } => {
                self.retry_transfer(&transfer_id).await?;
                Ok(LocalIpcResponse::Ack)
            }
            LocalIpcCommand::AppGetLocalIdentity => Ok(LocalIpcResponse::LocalIdentity {
                identity: self.get_local_identity().await?,
            }),
            LocalIpcCommand::AppGetRuntimeStatus => Ok(LocalIpcResponse::RuntimeStatus {
                runtime_status: self.get_runtime_status().await?,
            }),
            LocalIpcCommand::SecurityGetPosture => Ok(LocalIpcResponse::SecurityPosture {
                posture: self.get_security_posture().await?,
            }),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn dispatch_wire(&self, request: &LocalIpcWireRequest) -> LocalIpcWireResponse {
        let request_id = request.request_id.clone();
        let proto_version = request.proto_version;

        match LocalIpcCommand::from_wire_request(request) {
            Ok(command) => match self.dispatch(command).await {
                Ok(response) => response.into_wire_response(request_id, proto_version),
                Err(message) => LocalIpcWireResponse::error(
                    request_id,
                    proto_version,
                    LocalIpcError::dispatch_failed(message),
                ),
            },
            Err(error) => LocalIpcWireResponse::error(request_id, proto_version, error),
        }
    }

    pub async fn get_devices(&self) -> Result<Vec<DeviceView>, String> {
        let devices = self.state.devices.read().await;
        Ok(devices.values().map(DeviceView::from).collect())
    }

    pub async fn get_trusted_peers(&self) -> Result<Vec<TrustedPeerView>, String> {
        let trusted = self.state.trusted_peers.read().await;
        Ok(trusted.values().map(TrustedPeerView::from).collect())
    }

    pub async fn connect_by_address(
        &self,
        address: String,
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
            match connect_to_peer(&self.state, addr).await {
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
            if let Ok(db) = self.state.db.lock() {
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

        let trusted = self.state.is_trusted(&fingerprint).await;
        let (name, payload, is_new) = {
            let mut devices = self.state.devices.write().await;
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

        if let Some(app) = &self.app {
            if is_new {
                app.emit("device_discovered", &payload).ok();
            } else {
                app.emit("device_updated", &payload).ok();
            }
        }

        Ok(ConnectByAddressResult {
            fingerprint,
            name,
            trusted,
            address: selected_addr.to_string(),
        })
    }

    pub async fn pair_device(&self, fingerprint: &str) -> Result<(), String> {
        let name = {
            let mut devices = self.state.devices.write().await;
            if let Some(device) = devices.get_mut(fingerprint) {
                device.trusted = true;
                if let Some(app) = &self.app {
                    let payload = DeviceView::from(&*device);
                    app.emit("device_updated", &payload).ok();
                }
                device.name.clone()
            } else {
                "Unknown".into()
            }
        };
        self.state.add_trust(fingerprint.to_string(), name).await;
        persist_runtime_state(&self.state).await
    }

    pub async fn unpair_device(&self, fingerprint: &str) -> Result<(), String> {
        let removed = self.state.trusted_peers.write().await.remove(fingerprint);
        if removed.is_none() {
            return Err(format!("trusted device {fingerprint} not found"));
        }
        {
            let mut devices = self.state.devices.write().await;
            if let Some(device) = devices.get_mut(fingerprint) {
                device.trusted = false;
                if let Some(app) = &self.app {
                    let payload = DeviceView::from(&*device);
                    app.emit("device_updated", &payload).ok();
                }
            }
        }
        persist_runtime_state(&self.state).await
    }

    pub async fn set_trusted_alias(
        &self,
        fingerprint: &str,
        alias: Option<String>,
    ) -> Result<(), String> {
        let normalized = alias.and_then(|value| {
            let trimmed = value.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        });
        let mut trusted = self.state.trusted_peers.write().await;
        let Some(peer) = trusted.get_mut(fingerprint) else {
            return Err(format!("trusted device {fingerprint} not found"));
        };
        peer.alias = normalized;
        drop(trusted);
        persist_runtime_state(&self.state).await
    }

    pub async fn get_app_config(&self) -> Result<AppConfig, String> {
        Ok(self.state.config.read().await.clone())
    }

    pub async fn set_app_config(&self, config: AppConfig) -> Result<(), String> {
        if config.max_parallel_streams == 0 || config.max_parallel_streams > 32 {
            return Err("max_parallel_streams must be between 1 and 32".to_string());
        }

        let attempted_device_name = config.device_name.clone();
        let old_name = self.state.config.read().await.device_name.clone();
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

        let previous_config = self.state.config.read().await.clone();
        *self.state.config.write().await = config;
        if old_name != self.state.config.read().await.device_name {
            if let Err(e) =
                crate::discovery::service::reregister_service(Arc::clone(&self.state)).await
            {
                *self.state.config.write().await = previous_config;
                if let Some(app) = &self.app {
                    app.emit("system_error", serde_json::json!({
                        "code": "MDNS_REREGISTER_FAILED",
                        "subsystem": "mdns",
                        "message": format!("Device name update rolled back because mDNS refresh failed: {e:#}"),
                        "attempted_device_name": attempted_device_name,
                        "rollback_device_name": self.state.config.read().await.device_name.clone(),
                    }))
                    .ok();
                }
                return Err(format!(
                    "device name update rolled back because mDNS refresh failed: {e:#}"
                ));
            }
        }

        persist_runtime_state(&self.state).await
    }

    pub async fn send_files(&self, peer_fp: String, paths: Vec<String>) -> Result<(), String> {
        let transfer_id = uuid::Uuid::new_v4().to_string();
        self.state
            .record_sender_dispatch(&transfer_id, &peer_fp)
            .await;

        let remote_addrs = {
            let devices = self.state.devices.read().await;
            let device = devices
                .get(&peer_fp)
                .ok_or_else(|| format!("device {peer_fp} not found"))?;
            device
                .best_addrs()
                .ok_or_else(|| "device has no reachable address".to_string())?
        };

        let path_bufs: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
        for path in &path_bufs {
            if !path.exists() {
                return Err(format!("Source path does not exist: {}", path.display()));
            }
        }

        let mut conn_opt = None;
        let mut connect_errors: Vec<String> = Vec::new();
        for addr in remote_addrs {
            tracing::debug!("Trying to connect to {}", addr);
            match connect_to_peer(&self.state, addr).await {
                Ok(conn) => {
                    conn_opt = Some(conn);
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

        let (fp_match, actual_fp) =
            crate::transport::handshake::peer_fp_matches(&conn, &peer_fp)
                .map_err(|e| format!("failed to verify peer identity: {e:#}"))?;
        if !fp_match {
            conn.close(quinn::VarInt::from_u32(2), b"identity mismatch");
            if let Some(app) = &self.app {
                app.emit(
                    "identity_mismatch",
                    serde_json::json!({
                        "expected_fp": peer_fp.clone(),
                        "actual_fp": actual_fp,
                        "phase": "connect",
                    }),
                )
                .ok();
            }
            if let Ok(db) = self.state.db.lock() {
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

        let app = self
            .app
            .clone()
            .ok_or_else(|| "transfer/send requires app handle".to_string())?;
        let state = Arc::clone(&self.state);
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
                let outcome = transport_send_files(
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

    pub async fn accept_transfer(
        &self,
        transfer_id: &str,
        notification_id: &str,
    ) -> Result<(), String> {
        let result = accept_pending_transfer(&self.state, transfer_id, notification_id).await;
        if matches!(result.as_ref(), Err(err) if err == REQUEST_EXPIRED_CODE) {
            if let Some(app) = &self.app {
                emit_request_expired(app, &self.state, transfer_id).await;
            }
        }
        result
    }

    pub async fn reject_transfer(
        &self,
        transfer_id: &str,
        notification_id: &str,
    ) -> Result<(), String> {
        let result = reject_pending_transfer(&self.state, transfer_id, notification_id).await;
        if matches!(result.as_ref(), Err(err) if err == REQUEST_EXPIRED_CODE) {
            if let Some(app) = &self.app {
                emit_request_expired(app, &self.state, transfer_id).await;
            }
        }
        result
    }

    pub async fn cancel_transfer(&self, transfer_id: &str) -> Result<(), String> {
        if !cancel_transfer_inner(transfer_id, self.app.as_ref(), &self.state).await {
            return Err(format!("transfer {transfer_id} not found"));
        }
        Ok(())
    }

    pub async fn cancel_all_transfers(&self) -> Result<u32, String> {
        let active_ids: Vec<String> = {
            let transfers = self.state.transfers.read().await;
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
            if cancel_transfer_inner(&transfer_id, self.app.as_ref(), &self.state).await {
                count += 1;
            }
        }
        Ok(count)
    }

    pub async fn retry_transfer(&self, transfer_id: &str) -> Result<(), String> {
        let (peer_fp, retry_paths) = {
            let transfers = self.state.transfers.read().await;
            let Some(task) = transfers.get(transfer_id) else {
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
                return Err(
                    "retry is only available after transfer reaches a terminal state".into(),
                );
            }
            (task.peer_fingerprint.clone(), select_retry_paths(task)?)
        };
        self.send_files(peer_fp, retry_paths).await
    }

    pub async fn get_local_identity(&self) -> Result<crate::state::LocalIdentityView, String> {
        Ok(crate::dto::local_identity_view(
            self.state.identity.fingerprint.clone(),
            self.state.config.read().await.device_name.clone(),
            *self.state.local_port.read().await,
        ))
    }

    pub async fn get_runtime_status(&self) -> Result<crate::state::RuntimeStatus, String> {
        Ok(self.state.runtime_status().await)
    }

    pub async fn get_security_posture(&self) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({
            "secure_store_available": crate::crypto::secret_store::secure_store_available(),
        }))
    }
}

async fn accept_pending_transfer(
    state: &Arc<AppState>,
    transfer_id: &str,
    notification_id: &str,
) -> Result<(), String> {
    match incoming_request_action_state(state, transfer_id, notification_id).await {
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
        IncomingRequestActionState::StaleNotification => {
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
        match incoming_request_action_state(state, transfer_id, notification_id).await {
            IncomingRequestActionState::Expired => {
                state
                    .mark_incoming_request_notification_inactive(
                        transfer_id,
                        Some(REQUEST_EXPIRED_CODE),
                    )
                    .await;
                Err(REQUEST_EXPIRED_CODE.to_string())
            }
            IncomingRequestActionState::StaleNotification => Err(REQUEST_EXPIRED_CODE.to_string()),
            IncomingRequestActionState::Pending | IncomingRequestActionState::Missing => Err(
                format!("transfer {transfer_id} not found or already handled"),
            ),
        }
    }
}

async fn reject_pending_transfer(
    state: &Arc<AppState>,
    transfer_id: &str,
    notification_id: &str,
) -> Result<(), String> {
    match incoming_request_action_state(state, transfer_id, notification_id).await {
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
        IncomingRequestActionState::StaleNotification => {
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
        match incoming_request_action_state(state, transfer_id, notification_id).await {
            IncomingRequestActionState::Expired => {
                state
                    .mark_incoming_request_notification_inactive(
                        transfer_id,
                        Some(REQUEST_EXPIRED_CODE),
                    )
                    .await;
                Err(REQUEST_EXPIRED_CODE.to_string())
            }
            IncomingRequestActionState::StaleNotification => Err(REQUEST_EXPIRED_CODE.to_string()),
            IncomingRequestActionState::Pending | IncomingRequestActionState::Missing => Ok(()),
        }
    }
}

async fn incoming_request_action_state(
    state: &Arc<AppState>,
    transfer_id: &str,
    notification_id: &str,
) -> IncomingRequestActionState {
    let notification = state.incoming_request_notification(transfer_id).await;
    let transfer_status = {
        let transfers = state.transfers.read().await;
        transfers.get(transfer_id).map(|task| task.status.clone())
    };

    match transfer_status {
        Some(crate::state::TransferStatus::PendingAccept) => match notification {
            Some(entry) if !entry.active => IncomingRequestActionState::Expired,
            Some(entry) if entry.notification_id != notification_id => {
                IncomingRequestActionState::StaleNotification
            }
            Some(_) => IncomingRequestActionState::Pending,
            None => IncomingRequestActionState::Missing,
        },
        Some(_) => IncomingRequestActionState::Expired,
        None => {
            if notification
                .as_ref()
                .map(|entry| !entry.active)
                .unwrap_or(false)
            {
                IncomingRequestActionState::Expired
            } else if notification
                .as_ref()
                .map(|entry| entry.notification_id != notification_id)
                .unwrap_or(false)
            {
                IncomingRequestActionState::StaleNotification
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

async fn cancel_transfer_inner(
    transfer_id: &str,
    app: Option<&AppHandle>,
    state: &Arc<AppState>,
) -> bool {
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
        if let Some(app) = app {
            crate::transport::events::emit_transfer_terminal(
                app,
                transfer_id,
                &t.status,
                "E_CANCELLED_BY_USER",
                "UserCancelled",
                t.revision,
                Some("cancel"),
            );
        }
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Instant;

    use crate::crypto::Identity;
    use crate::local_ipc::{LocalIpcCommand, LocalIpcResponse, LocalIpcWireRequest};
    use crate::state::{
        AppConfig, AppState, DeviceInfo, FileItemMeta, Platform, ReachabilityStatus, SessionInfo,
        TransferDirection, TransferStatus, TransferTask,
    };
    use serde_json::json;
    use tokio::sync::oneshot;

    use super::{select_retry_paths, AppCoreService, REQUEST_EXPIRED_CODE};

    fn build_test_state() -> Arc<AppState> {
        let config_dir =
            std::env::temp_dir().join(format!("dashdrop-core-{}", uuid::Uuid::new_v4()));
        let identity = Identity::load_or_create(&config_dir).expect("identity");
        Arc::new(AppState::new(
            identity,
            AppConfig::default(),
            rusqlite::Connection::open_in_memory().expect("db"),
        ))
    }

    fn make_transfer_task(
        id: &str,
        direction: TransferDirection,
        status: TransferStatus,
    ) -> TransferTask {
        TransferTask {
            id: id.to_string(),
            batch_id: None,
            direction,
            peer_fingerprint: "peer-fp".into(),
            peer_name: "Peer".into(),
            items: vec![FileItemMeta {
                file_id: 1,
                name: "file.txt".into(),
                rel_path: "file.txt".into(),
                size: 1,
            }],
            status,
            bytes_transferred: 0,
            total_bytes: 1,
            revision: 0,
            started_at_unix: 1,
            ended_at_unix: None,
            terminal_reason_code: None,
            error: None,
            source_paths: Some(vec!["/tmp/file.txt".into()]),
            source_path_by_file_id: Some(HashMap::from([(1, "/tmp/file.txt".into())])),
            failed_file_ids: Some(vec![1]),
            conn: None,
            ended_at: None,
        }
    }

    #[tokio::test]
    async fn discover_list_dispatch_returns_devices() {
        let state = build_test_state();
        state.devices.write().await.insert(
            "fp-1".into(),
            DeviceInfo {
                fingerprint: "fp-1".into(),
                name: "Peer".into(),
                platform: Platform::Mac,
                trusted: false,
                sessions: HashMap::from([(
                    "s1".into(),
                    SessionInfo {
                        session_id: "s1".into(),
                        addrs: vec!["127.0.0.1:9443".parse().expect("addr")],
                        last_seen_unix: 1,
                        last_seen_instant: Instant::now(),
                    },
                )]),
                last_seen: 1,
                reachability: ReachabilityStatus::Discovered,
                probe_fail_count: 0,
                last_probe_at: None,
                last_probe_result: None,
                last_probe_error: None,
                last_probe_error_detail: None,
                last_probe_addr: None,
                last_probe_attempted_addrs: Vec::new(),
                last_resolve_raw_addr_count: 0,
                last_resolve_usable_addr_count: 0,
                last_resolve_hostname: None,
                last_resolve_port: None,
                last_resolve_at: None,
            },
        );

        let service = AppCoreService::new(Arc::clone(&state));
        let response = service
            .dispatch(LocalIpcCommand::DiscoverList)
            .await
            .expect("dispatch");

        match response {
            LocalIpcResponse::Devices { devices } => {
                assert_eq!(devices.len(), 1);
                assert_eq!(devices[0].fingerprint, "fp-1");
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn config_get_dispatch_returns_current_config() {
        let state = build_test_state();
        state.config.write().await.device_name = "Desk".into();
        let service = AppCoreService::new(Arc::clone(&state));

        let response = service
            .dispatch(LocalIpcCommand::ConfigGet)
            .await
            .expect("dispatch");

        match response {
            LocalIpcResponse::AppConfig { config } => {
                assert_eq!(config.device_name, "Desk");
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn transfer_accept_dispatch_sends_accept_signal() {
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
            make_transfer_task(
                &transfer_id,
                TransferDirection::Receive,
                TransferStatus::PendingAccept,
            ),
        );
        let notification_id = state
            .ensure_incoming_request_notification(&transfer_id)
            .await;

        let service = AppCoreService::new(Arc::clone(&state));
        let response = service
            .dispatch(LocalIpcCommand::TransferAccept {
                transfer_id: transfer_id.clone(),
                notification_id,
            })
            .await
            .expect("dispatch");

        assert!(matches!(response, LocalIpcResponse::Ack));
        assert!(rx.await.expect("receiver should get value"));
    }

    #[tokio::test]
    async fn transfer_reject_dispatch_sends_reject_signal() {
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
            make_transfer_task(
                &transfer_id,
                TransferDirection::Receive,
                TransferStatus::PendingAccept,
            ),
        );
        let notification_id = state
            .ensure_incoming_request_notification(&transfer_id)
            .await;

        let service = AppCoreService::new(Arc::clone(&state));
        let response = service
            .dispatch(LocalIpcCommand::TransferReject {
                transfer_id: transfer_id.clone(),
                notification_id,
            })
            .await
            .expect("dispatch");

        assert!(matches!(response, LocalIpcResponse::Ack));
        assert!(!rx.await.expect("receiver should get value"));
    }

    #[tokio::test]
    async fn expired_transfer_accept_returns_request_expired_code() {
        let state = build_test_state();
        let transfer_id = "transfer-expired".to_string();
        state.transfers.write().await.insert(
            transfer_id.clone(),
            make_transfer_task(
                &transfer_id,
                TransferDirection::Receive,
                TransferStatus::Rejected,
            ),
        );
        let notification_id = state
            .ensure_incoming_request_notification(&transfer_id)
            .await;
        state
            .mark_incoming_request_notification_inactive(&transfer_id, Some("E_TIMEOUT"))
            .await;

        let service = AppCoreService::new(Arc::clone(&state));
        let err = service
            .dispatch(LocalIpcCommand::TransferAccept {
                transfer_id: transfer_id.clone(),
                notification_id,
            })
            .await
            .expect_err("expired request should fail");

        assert_eq!(err, REQUEST_EXPIRED_CODE);
    }

    #[tokio::test]
    async fn cancel_all_dispatch_returns_count_and_updates_terminal_state() {
        let state = build_test_state();
        state.transfers.write().await.insert(
            "send-1".into(),
            make_transfer_task(
                "send-1",
                TransferDirection::Send,
                TransferStatus::Transferring,
            ),
        );
        state.transfers.write().await.insert(
            "recv-1".into(),
            make_transfer_task(
                "recv-1",
                TransferDirection::Receive,
                TransferStatus::PendingAccept,
            ),
        );
        state.transfers.write().await.insert(
            "done-1".into(),
            make_transfer_task("done-1", TransferDirection::Send, TransferStatus::Completed),
        );

        let service = AppCoreService::new(Arc::clone(&state));
        let response = service
            .dispatch(LocalIpcCommand::TransferCancelAll)
            .await
            .expect("dispatch");

        match response {
            LocalIpcResponse::CancelledTransfers { count } => assert_eq!(count, 2),
            other => panic!("unexpected response: {other:?}"),
        }

        let transfers = state.transfers.read().await;
        assert_eq!(
            transfers.get("send-1").map(|task| task.status.clone()),
            Some(TransferStatus::CancelledBySender)
        );
        assert_eq!(
            transfers.get("recv-1").map(|task| task.status.clone()),
            Some(TransferStatus::CancelledByReceiver)
        );
        assert_eq!(
            transfers
                .get("recv-1")
                .and_then(|task| task.terminal_reason_code.clone())
                .as_deref(),
            Some("E_CANCELLED_BY_USER")
        );
        assert_eq!(
            transfers.get("done-1").map(|task| task.status.clone()),
            Some(TransferStatus::Completed)
        );
    }

    #[tokio::test]
    async fn transfer_send_dispatch_rejects_missing_device() {
        let state = build_test_state();
        let service = AppCoreService::new(Arc::clone(&state));

        let err = service
            .dispatch(LocalIpcCommand::TransferSend {
                peer_fingerprint: "missing-peer".into(),
                paths: vec!["/tmp/file.txt".into()],
            })
            .await
            .expect_err("missing device should fail");

        assert!(err.contains("device missing-peer not found"));
    }

    #[tokio::test]
    async fn retry_dispatch_rejects_non_send_transfer() {
        let state = build_test_state();
        state.transfers.write().await.insert(
            "recv-1".into(),
            make_transfer_task("recv-1", TransferDirection::Receive, TransferStatus::Failed),
        );
        let service = AppCoreService::new(Arc::clone(&state));

        let err = service
            .dispatch(LocalIpcCommand::TransferRetry {
                transfer_id: "recv-1".into(),
            })
            .await
            .expect_err("retry should reject receive transfers");

        assert_eq!(err, "retry is only supported for outgoing transfers");
    }

    #[tokio::test]
    async fn connect_by_address_dispatch_rejects_invalid_address() {
        let state = build_test_state();
        let service = AppCoreService::new(Arc::clone(&state));

        let err = service
            .dispatch(LocalIpcCommand::DiscoverConnectByAddress {
                address: "not-an-address".into(),
            })
            .await
            .expect_err("invalid address should fail");

        assert!(err.contains("invalid address"));
    }

    #[test]
    fn retry_path_selection_prefers_failed_file_subset() {
        let mut task =
            make_transfer_task("retry-1", TransferDirection::Send, TransferStatus::Failed);
        task.source_paths = Some(vec!["/tmp/a.txt".into(), "/tmp/b.txt".into()]);
        task.source_path_by_file_id = Some(HashMap::from([
            (1, "/tmp/a.txt".into()),
            (2, "/tmp/b.txt".into()),
        ]));
        task.failed_file_ids = Some(vec![2]);
        task.items = vec![
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
        ];

        let selected = select_retry_paths(&task).expect("retry paths");
        assert_eq!(selected, vec!["/tmp/b.txt".to_string()]);
    }

    #[tokio::test]
    async fn dispatch_wire_returns_frozen_ok_response_shape() {
        let state = build_test_state();
        state.config.write().await.device_name = "Desk".into();
        let service = AppCoreService::new(Arc::clone(&state));

        let response = service
            .dispatch_wire(&LocalIpcCommand::ConfigGet.to_wire_request("req-1", None))
            .await;
        let value = serde_json::to_value(&response).expect("serialize response");

        assert_eq!(value["ok"], json!(true));
        assert_eq!(value["request_id"], json!("req-1"));
        assert_eq!(value["payload"]["config"]["device_name"], json!("Desk"));
    }

    #[tokio::test]
    async fn dispatch_wire_returns_error_for_reserved_phase_a_command() {
        let state = build_test_state();
        let service = AppCoreService::new(Arc::clone(&state));

        let response = service
            .dispatch_wire(&LocalIpcWireRequest {
                proto_version: 1,
                request_id: "req-transfer".into(),
                command: "transfer/send".into(),
                payload: Some(json!({})),
                auth_context: None,
            })
            .await;
        let value = serde_json::to_value(&response).expect("serialize response");

        assert_eq!(value["ok"], json!(false));
        assert_eq!(value["error"]["code"], json!("invalid_request"));
    }

    #[tokio::test]
    async fn dispatch_wire_returns_transfer_cancel_all_payload_shape() {
        let state = build_test_state();
        let service = AppCoreService::new(Arc::clone(&state));

        let response = service
            .dispatch_wire(&LocalIpcCommand::TransferCancelAll.to_wire_request("req-2", None))
            .await;
        let value = serde_json::to_value(&response).expect("serialize response");

        assert_eq!(value["ok"], json!(true));
        assert_eq!(value["payload"]["count"], json!(0));
    }
}
