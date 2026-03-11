use std::sync::Arc;

use tauri::{AppHandle, Emitter};

use crate::dto::{DeviceView, TrustedPeerView};
use crate::local_ipc::{
    LocalIpcCommand, LocalIpcError, LocalIpcResponse, LocalIpcWireRequest, LocalIpcWireResponse,
};
use crate::state::{AppConfig, AppState};

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
        AppConfig, AppState, DeviceInfo, Platform, ReachabilityStatus, SessionInfo,
    };
    use serde_json::json;

    use super::AppCoreService;

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
}
