use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use crate::daemon::server::LocalIpcServerHandle;
use crate::local_ipc::LocalIpcEndpointKind;
use crate::runtime::host::RuntimeHost;
use crate::state::{AppState, TransferStatus};

pub struct RuntimeSupervisorHandle {
    _local_ipc_server: LocalIpcServerHandle,
    network_runtime_enabled: bool,
}

impl RuntimeSupervisorHandle {
    pub fn network_runtime_enabled(&self) -> bool {
        self.network_runtime_enabled
    }
}

pub fn start(
    state: Arc<AppState>,
    host: Arc<dyn RuntimeHost>,
    config_dir: &Path,
) -> Result<RuntimeSupervisorHandle> {
    let local_ipc_server = crate::daemon::server::spawn(
        Arc::clone(&state),
        host.app_handle(),
        config_dir,
        LocalIpcEndpointKind::Service,
    )?;
    tracing::info!(
        "Local IPC server listening on {}",
        local_ipc_server.endpoint().describe()
    );

    start_network_runtime(Arc::clone(&state), Arc::clone(&host), config_dir);
    let network_runtime_enabled = true;

    start_memory_cleanup_loop(Arc::clone(&state));
    start_notification_maintenance_loop(state);

    Ok(RuntimeSupervisorHandle {
        _local_ipc_server: local_ipc_server,
        network_runtime_enabled,
    })
}

pub fn dispatch_startup_share(host: Arc<dyn RuntimeHost>, startup_share_paths: Vec<String>) {
    if startup_share_paths.is_empty() {
        return;
    }

    if host.app_handle().is_none() {
        tracing::info!(
            paths = startup_share_paths.len(),
            "Ignoring startup share dispatch in headless runtime skeleton"
        );
        return;
    }

    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        if let Err(err) = host.emit_json(
            "external_share_received",
            serde_json::json!({
                "paths": startup_share_paths,
                "source": "startup_args"
            }),
        ) {
            tracing::warn!("Failed to dispatch startup share payload: {err}");
        }
    });
}

pub fn dispatch_startup_pairing_links(
    host: Arc<dyn RuntimeHost>,
    startup_pairing_links: Vec<String>,
) {
    if startup_pairing_links.is_empty() {
        return;
    }

    if host.app_handle().is_none() {
        tracing::info!(
            links = startup_pairing_links.len(),
            "Ignoring startup pairing-link dispatch in headless runtime skeleton"
        );
        return;
    }

    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        for pairing_link in startup_pairing_links {
            if let Err(err) = host.emit_json(
                "pairing_link_received",
                serde_json::json!({
                    "pairing_link": pairing_link,
                    "source": "startup_args"
                }),
            ) {
                tracing::warn!("Failed to dispatch startup pairing link payload: {err}");
            }
        }
    });
}

fn emit_system_error(host: &Arc<dyn RuntimeHost>, code: &str, subsystem: &str, message: String) {
    if let Err(err) = host.emit_json(
        "system_error",
        serde_json::json!({
            "code": code,
            "subsystem": subsystem,
            "message": message,
        }),
    ) {
        tracing::warn!("Failed to emit system error {code}: {err}");
    }
}

fn start_network_runtime(state: Arc<AppState>, host: Arc<dyn RuntimeHost>, config_dir: &Path) {
    let config_dir = config_dir.to_path_buf();
    tauri::async_runtime::spawn(async move {
        let port = match crate::transport::start_server(
            state.identity.clone(),
            Arc::clone(&host),
            Arc::clone(&state),
        )
        .await
        {
            Ok(port) => port,
            Err(err) => {
                tracing::error!("Failed to start QUIC server: {err:#}");
                emit_system_error(
                    &host,
                    "QUIC_SERVER_START_FAILED",
                    "quic_server",
                    format!("Failed to start network listener: {err:#}"),
                );
                return;
            }
        };

        tracing::info!("QUIC server on port {port}");

        let mdns = match crate::discovery::register_service(Arc::clone(&state)).await {
            Ok(mdns) => Some(Arc::new(mdns)),
            Err(err) => {
                tracing::error!("Failed to register mDNS: {err:#}");
                emit_system_error(
                    &host,
                    "MDNS_REGISTER_FAILED",
                    "mdns",
                    format!("Device discovery unavailable: {err:#}. Other devices won't see you."),
                );
                None
            }
        };

        let state_for_beacon = Arc::clone(&state);

        if let Some(mdns) = mdns {
            let _ = state.mdns.set(Arc::clone(&mdns));
            if let Err(err) =
                crate::discovery::start_browser(mdns, Arc::clone(&host), Arc::clone(&state)).await
            {
                tracing::error!("Failed to start mDNS browser: {err:#}");
                emit_system_error(
                    &host,
                    "MDNS_BROWSER_FAILED",
                    "mdns_browser",
                    format!("Scanning for nearby devices failed: {err:#}"),
                );
            }
        }

        if let Err(err) = crate::discovery::start_beacon(Arc::clone(&host), state_for_beacon).await
        {
            tracing::error!("Failed to start beacon discovery: {err:#}");
            emit_system_error(
                &host,
                "BEACON_DISCOVERY_FAILED",
                "beacon_discovery",
                format!("Fallback local discovery failed: {err:#}"),
            );
        }

        crate::ble::start_runtime(Arc::clone(&state), Arc::clone(&host), &config_dir);
    });
}

fn start_memory_cleanup_loop(state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
        loop {
            interval.tick().await;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let mut transfers = state.transfers.write().await;
            transfers.retain(|_, transfer| {
                if let Some(ended) = transfer.ended_at_unix {
                    now.saturating_sub(ended) < 60
                } else {
                    true
                }
            });
        }
    });
}

fn start_notification_maintenance_loop(state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            interval.tick().await;
            let transfer_states = {
                let transfers = state.transfers.read().await;
                transfers
                    .values()
                    .map(|task| {
                        (
                            task.id.clone(),
                            task.status.clone(),
                            task.terminal_reason_code.clone(),
                        )
                    })
                    .collect::<Vec<_>>()
            };

            let mut pending_ids = HashSet::new();
            let mut inactive_ids = Vec::new();

            for (transfer_id, status, terminal_reason_code) in transfer_states {
                if status == TransferStatus::PendingAccept {
                    pending_ids.insert(transfer_id.clone());
                    state
                        .ensure_incoming_request_notification(&transfer_id)
                        .await;
                } else {
                    inactive_ids.push((transfer_id, terminal_reason_code));
                }
            }

            {
                let mut pending_accepts = state.pending_accepts.write().await;
                pending_accepts.retain(|transfer_id, _| pending_ids.contains(transfer_id));
            }

            for (transfer_id, terminal_reason_code) in inactive_ids {
                state
                    .mark_incoming_request_notification_inactive(
                        &transfer_id,
                        terminal_reason_code.as_deref(),
                    )
                    .await;
            }
        }
    });
}
