use crate::core_service::AppCoreService;
use crate::daemon::client::LocalIpcClient;
use crate::dto::{DeviceView, TransferView, TrustedPeerView};
use crate::local_ipc::{
    ConnectByAddressResult, LocalAuthContext, LocalIpcAccessGrant, LocalIpcCommand,
    LocalIpcWireResponse, PendingIncomingRequestPayload,
};
use crate::runtime::bootstrap::resolve_config_dir_from_base;
use crate::state::{
    AppState, DeviceInfo, Platform, ReachabilityStatus, RuntimeEventCheckpoint,
    RuntimeEventFeedSnapshot, TrustVerificationMethod,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager, State};

type AppStateRef<'a> = State<'a, Arc<AppState>>;

const DAEMON_CONTROL_PLANE_MODE: &str = "daemon";
const IN_PROCESS_CONTROL_PLANE_MODE: &str = "in_process";
const DAEMON_PROXY_READ_RETRY_DELAYS_MS: &[u64] = &[0, 100, 250, 500];
const DAEMON_PROXY_WRITE_RETRY_DELAYS_MS: &[u64] = &[0];
const DAEMON_ACCESS_GRANT_EXPIRY_SKEW_MS: u64 = 5_000;

#[derive(Debug, Clone)]
struct CachedDaemonAccessGrant {
    access_token: String,
    expires_at_unix_ms: u64,
    refresh_after_unix_ms: u64,
}

pub struct DaemonIpcAuthState {
    cached_grant: Mutex<Option<CachedDaemonAccessGrant>>,
}

impl Default for DaemonIpcAuthState {
    fn default() -> Self {
        Self {
            cached_grant: Mutex::new(None),
        }
    }
}

impl From<LocalIpcAccessGrant> for CachedDaemonAccessGrant {
    fn from(grant: LocalIpcAccessGrant) -> Self {
        Self {
            access_token: grant.access_token,
            expires_at_unix_ms: grant.expires_at_unix_ms,
            refresh_after_unix_ms: grant.refresh_after_unix_ms,
        }
    }
}

impl CachedDaemonAccessGrant {
    fn is_usable_at(&self, now_unix_ms: u64) -> bool {
        now_unix_ms.saturating_add(DAEMON_ACCESS_GRANT_EXPIRY_SKEW_MS) < self.expires_at_unix_ms
    }

    fn should_refresh_at(&self, now_unix_ms: u64) -> bool {
        now_unix_ms.saturating_add(DAEMON_ACCESS_GRANT_EXPIRY_SKEW_MS) >= self.refresh_after_unix_ms
    }

    fn auth_context(&self) -> LocalAuthContext {
        LocalAuthContext {
            access_token: Some(self.access_token.clone()),
        }
    }
}

fn now_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn env_flag(name: &str) -> Option<bool> {
    std::env::var(name).ok().and_then(|value| {
        let normalized = value.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        }
    })
}

fn build_link_capabilities(
    own_platform: &str,
    ble_runtime_status: &crate::state::BleRuntimeStatus,
) -> serde_json::Value {
    let ble_baseline_enabled =
        env_flag("DASHDROP_BLE_BASELINE_ENABLED").unwrap_or(cfg!(debug_assertions));
    let ble_supported = matches!(own_platform, "Mac" | "Windows" | "Linux");
    let provider_runtime_available = matches!(
        ble_runtime_status.scanner_state.as_str(),
        "hardware_ready_scaffold"
            | "observing_capsules"
            | "bridge_snapshot_ready"
            | "bridge_scanning"
            | "bridge_authorized_idle"
    );
    let ble_permission_state =
        ble_runtime_status
            .permission_state
            .as_deref()
            .unwrap_or(if !ble_supported {
                "not_supported"
            } else {
                "not_requested"
            });
    let ble_runtime_available = ble_baseline_enabled
        && ble_supported
        && (env_flag("DASHDROP_BLE_RUNTIME_AVAILABLE").unwrap_or(false)
            || provider_runtime_available
            || ble_runtime_status.last_bridge_snapshot_at_unix_ms.is_some());
    let single_radio_risk = env_flag("DASHDROP_SINGLE_RADIO_RISK").unwrap_or(false);
    let softap_capable = matches!(own_platform, "Windows" | "Linux")
        && env_flag("DASHDROP_SOFTAP_CAPABLE").unwrap_or(false);
    let p2p_capable = matches!(own_platform, "Mac" | "Windows" | "Linux")
        && env_flag("DASHDROP_P2P_CAPABLE").unwrap_or(false);

    serde_json::json!({
        "ble_baseline_enabled": ble_baseline_enabled,
        "ble_supported": ble_supported,
        "ble_permission_state": ble_permission_state,
        "ble_runtime_available": ble_runtime_available,
        "single_radio_risk": single_radio_risk,
        "softap_capable": softap_capable,
        "p2p_capable": p2p_capable,
        "rolling_identifier_mode": if ble_baseline_enabled { "planned_ephemeral" } else { "disabled" },
        "ephemeral_capsule_mode": if ble_baseline_enabled { "diagnostics_only" } else { "disabled" },
        "fallback_mode": "qr_or_short_code",
        "provider_name": ble_runtime_status.provider_name,
        "scanner_state": ble_runtime_status.scanner_state,
        "advertiser_state": ble_runtime_status.advertiser_state,
        "bridge_mode": ble_runtime_status.bridge_mode,
        "bridge_file_path": ble_runtime_status.bridge_file_path,
        "advertisement_file_path": ble_runtime_status.advertisement_file_path,
        "last_started_at_unix_ms": ble_runtime_status.last_started_at_unix_ms,
        "last_error": ble_runtime_status.last_error,
        "last_capsule_ingested_at_unix_ms": ble_runtime_status.last_capsule_ingested_at_unix_ms,
        "last_observation_prune_at_unix_ms": ble_runtime_status.last_observation_prune_at_unix_ms,
        "last_bridge_snapshot_at_unix_ms": ble_runtime_status.last_bridge_snapshot_at_unix_ms,
        "last_advertisement_request_at_unix_ms": ble_runtime_status.last_advertisement_request_at_unix_ms,
        "advertised_rolling_identifier": ble_runtime_status.advertised_rolling_identifier,
        "notes": [
            "BLE baseline exposes capability and diagnostics only in this release line.",
            "QUIC/TLS fingerprint verification remains the source of identity truth until BLE assist is promoted from diagnostics."
        ],
    })
}

fn daemon_ipc_auth_state(app: &AppHandle) -> Result<State<'_, DaemonIpcAuthState>, String> {
    app.try_state::<DaemonIpcAuthState>()
        .ok_or_else(|| "daemon IPC auth state unavailable".to_string())
}

fn cached_daemon_access_grant(app: &AppHandle) -> Result<Option<CachedDaemonAccessGrant>, String> {
    let auth_state = daemon_ipc_auth_state(app)?;
    let guard = auth_state
        .cached_grant
        .lock()
        .map_err(|_| "daemon IPC auth cache lock poisoned".to_string())?;
    Ok(guard.clone())
}

fn set_cached_daemon_access_grant(
    app: &AppHandle,
    grant: Option<CachedDaemonAccessGrant>,
) -> Result<(), String> {
    let auth_state = daemon_ipc_auth_state(app)?;
    let mut guard = auth_state
        .cached_grant
        .lock()
        .map_err(|_| "daemon IPC auth cache lock poisoned".to_string())?;
    *guard = grant;
    Ok(())
}

fn clear_cached_daemon_access_grant(app: &AppHandle) -> Result<(), String> {
    set_cached_daemon_access_grant(app, None)
}

async fn issue_daemon_access_grant(
    app: &AppHandle,
    current_access_token: Option<&str>,
) -> Result<CachedDaemonAccessGrant, String> {
    let client = daemon_client_for_app(app)?;
    let grant = client
        .issue_access_grant(current_access_token)
        .await
        .map_err(|err| format!("failed to issue daemon access token: {err:#}"))?;
    let cached = CachedDaemonAccessGrant::from(grant);
    set_cached_daemon_access_grant(app, Some(cached.clone()))?;
    Ok(cached)
}

pub async fn revoke_cached_daemon_access_grant(app: &AppHandle) -> Result<(), String> {
    let Some(grant) = cached_daemon_access_grant(app)? else {
        return Ok(());
    };
    clear_cached_daemon_access_grant(app)?;

    if !should_proxy_via_daemon(app) {
        return Ok(());
    }

    let client = daemon_client_for_app(app)?;
    client
        .revoke_access_grant(&grant.access_token)
        .await
        .map_err(|err| format!("failed to revoke daemon access token: {err:#}"))
}

pub async fn best_effort_revoke_cached_daemon_access_grant(app: &AppHandle) {
    if app.try_state::<DaemonIpcAuthState>().is_none() {
        return;
    }

    if let Err(err) = revoke_cached_daemon_access_grant(app).await {
        tracing::warn!("failed to revoke cached daemon access token during shutdown: {err}");
    }
}

async fn daemon_access_grant_for_command(
    app: &AppHandle,
    force_refresh: bool,
) -> Result<CachedDaemonAccessGrant, String> {
    let now_unix_ms = now_unix_millis();
    let cached = cached_daemon_access_grant(app)?;

    if let Some(grant) = cached {
        if !force_refresh
            && grant.is_usable_at(now_unix_ms)
            && !grant.should_refresh_at(now_unix_ms)
        {
            return Ok(grant);
        }

        if !force_refresh && grant.is_usable_at(now_unix_ms) {
            match issue_daemon_access_grant(app, Some(&grant.access_token)).await {
                Ok(fresh) => return Ok(fresh),
                Err(err) => {
                    tracing::warn!(
                        command_auth_refresh = true,
                        "failed to refresh daemon access token, falling back to cached grant: {err}"
                    );
                    return Ok(grant);
                }
            }
        }
    }

    issue_daemon_access_grant(app, None).await
}

fn env_control_plane_mode() -> Option<String> {
    std::env::var("DASHDROP_CONTROL_PLANE_MODE")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
}

fn normalize_control_plane_mode(value: Option<&str>) -> Option<&'static str> {
    match value.map(|raw| raw.trim().to_ascii_lowercase()) {
        Some(mode) if mode == DAEMON_CONTROL_PLANE_MODE => Some(DAEMON_CONTROL_PLANE_MODE),
        Some(mode) if mode == IN_PROCESS_CONTROL_PLANE_MODE => Some(IN_PROCESS_CONTROL_PLANE_MODE),
        _ => None,
    }
}

fn control_plane_mode_from_sources(
    state_mode: Option<&str>,
    env_mode: Option<&str>,
) -> &'static str {
    normalize_control_plane_mode(state_mode)
        .or_else(|| normalize_control_plane_mode(env_mode))
        .unwrap_or(IN_PROCESS_CONTROL_PLANE_MODE)
}

fn control_plane_mode_for_app(app: &AppHandle) -> String {
    let state_mode = app.try_state::<Arc<AppState>>().and_then(|state| {
        let mode = tokio::task::block_in_place(|| {
            tauri::async_runtime::block_on(async {
                state.control_plane_mode.read().await.clone()
            })
        });
        if mode.trim().is_empty() {
            None
        } else {
            Some(mode)
        }
    });
    control_plane_mode_from_sources(state_mode.as_deref(), env_control_plane_mode().as_deref())
        .to_string()
}

fn should_proxy_via_daemon(app: &AppHandle) -> bool {
    control_plane_mode_for_app(app) == DAEMON_CONTROL_PLANE_MODE
}

fn daemon_client_for_app(app: &AppHandle) -> Result<LocalIpcClient, String> {
    let base_config_dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("failed to resolve app config directory: {e}"))?;
    let config_dir = resolve_config_dir_from_base(Some(base_config_dir))
        .map_err(|e| format!("failed to resolve control-plane config dir: {e:#}"))?;
    Ok(LocalIpcClient::from_config_dir(&config_dir))
}

fn payload_from_response(response: LocalIpcWireResponse) -> Result<serde_json::Value, String> {
    match response {
        LocalIpcWireResponse::Ok(ok) => Ok(ok.payload.unwrap_or(serde_json::Value::Null)),
        LocalIpcWireResponse::Err(err) => Err(format!(
            "daemon control-plane request failed: {} ({})",
            err.error.message, err.error.code
        )),
    }
}

fn parse_payload_field<T>(payload: serde_json::Value, field: &str) -> Result<T, String>
where
    T: for<'de> serde::Deserialize<'de>,
{
    let value = payload
        .as_object()
        .and_then(|object| object.get(field))
        .cloned()
        .ok_or_else(|| format!("daemon response missing payload.{field}"))?;
    serde_json::from_value(value)
        .map_err(|err| format!("daemon response payload.{field} is invalid: {err}"))
}

fn parse_payload<T>(payload: serde_json::Value) -> Result<T, String>
where
    T: for<'de> serde::Deserialize<'de>,
{
    serde_json::from_value(payload)
        .map_err(|err| format!("daemon response payload is invalid: {err}"))
}

async fn proxy_command_payload_with_retries(
    app: &AppHandle,
    command: LocalIpcCommand,
    retry_delays_ms: &[u64],
) -> Result<Option<serde_json::Value>, String> {
    if !should_proxy_via_daemon(app) {
        return Ok(None);
    }

    let client = daemon_client_for_app(app)?;
    let mut last_error = None;
    let requires_auth = command.requires_auth();

    for (attempt_index, delay_ms) in retry_delays_ms.iter().enumerate() {
        if *delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(*delay_ms)).await;
        }

        let mut retried_after_unauthorized = false;

        loop {
            let response = if requires_auth {
                let grant =
                    daemon_access_grant_for_command(app, retried_after_unauthorized).await?;
                client
                    .send_envelope(command.to_wire_request(
                        uuid::Uuid::new_v4().to_string(),
                        Some(grant.auth_context()),
                    ))
                    .await
            } else {
                client.send(command.clone()).await
            };

            match response {
                Ok(response @ LocalIpcWireResponse::Ok(_)) => {
                    return Ok(Some(payload_from_response(response)?));
                }
                Ok(LocalIpcWireResponse::Err(err))
                    if requires_auth
                        && err.error.code == "unauthorized"
                        && !retried_after_unauthorized =>
                {
                    clear_cached_daemon_access_grant(app)?;
                    retried_after_unauthorized = true;
                    tracing::warn!(
                        attempt = attempt_index + 1,
                        command = command.name(),
                        "daemon access token rejected; refreshing and retrying request"
                    );
                }
                Ok(LocalIpcWireResponse::Err(err)) => {
                    return Err(format!(
                        "daemon control-plane request failed: {} ({})",
                        err.error.message, err.error.code
                    ));
                }
                Err(err) => {
                    last_error = Some(format!("{err:#}"));
                    tracing::warn!(
                        attempt = attempt_index + 1,
                        command = command.name(),
                        "daemon control-plane connect attempt failed"
                    );
                    break;
                }
            }
        }
    }

    let attempts = retry_delays_ms.len();
    let detail = last_error.unwrap_or_else(|| "unknown local IPC error".to_string());
    let response =
        format!("daemon control-plane connect failed after {attempts} attempt(s): {detail}");
    Err(response)
}

async fn proxy_read<T>(
    app: &AppHandle,
    command: LocalIpcCommand,
    field: &str,
) -> Result<Option<T>, String>
where
    T: for<'de> serde::Deserialize<'de>,
{
    let Some(payload) =
        proxy_command_payload_with_retries(app, command, DAEMON_PROXY_READ_RETRY_DELAYS_MS).await?
    else {
        return Ok(None);
    };
    Ok(Some(parse_payload_field(payload, field)?))
}

async fn proxy_write_result<T>(
    app: &AppHandle,
    command: LocalIpcCommand,
    field: &str,
) -> Result<Option<T>, String>
where
    T: for<'de> serde::Deserialize<'de>,
{
    let Some(payload) =
        proxy_command_payload_with_retries(app, command, DAEMON_PROXY_WRITE_RETRY_DELAYS_MS)
            .await?
    else {
        return Ok(None);
    };
    Ok(Some(parse_payload_field(payload, field)?))
}

async fn proxy_ack(app: &AppHandle, command: LocalIpcCommand) -> Result<bool, String> {
    Ok(
        proxy_command_payload_with_retries(app, command, DAEMON_PROXY_WRITE_RETRY_DELAYS_MS)
            .await?
            .is_some(),
    )
}

async fn pending_incoming_requests(
    state: &Arc<AppState>,
) -> Result<Vec<PendingIncomingRequestPayload>, String> {
    AppCoreService::new(Arc::clone(state))
        .get_pending_incoming_requests()
        .await
}
// ─── Device commands ────────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_devices(
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<Vec<DeviceView>, String> {
    if let Some(devices) =
        proxy_read::<Vec<DeviceView>>(&app, LocalIpcCommand::DiscoverList, "devices").await?
    {
        return Ok(devices);
    }
    let devices = state.devices.read().await;
    Ok(devices.values().map(DeviceView::from).collect())
}

#[tauri::command]
pub async fn get_trusted_peers(
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<Vec<TrustedPeerView>, String> {
    if let Some(trusted) =
        proxy_read::<Vec<TrustedPeerView>>(&app, LocalIpcCommand::TrustList, "trusted_peers")
            .await?
    {
        return Ok(trusted);
    }
    let trusted = state.trusted_peers.read().await;
    Ok(trusted.values().map(TrustedPeerView::from).collect())
}

#[tauri::command]
pub async fn get_pending_incoming_requests(
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<Vec<PendingIncomingRequestPayload>, String> {
    if let Some(requests) = proxy_read::<Vec<PendingIncomingRequestPayload>>(
        &app,
        LocalIpcCommand::TransferPendingIncoming,
        "requests",
    )
    .await?
    {
        return Ok(requests);
    }
    pending_incoming_requests(&state).await
}

#[tauri::command]
pub fn get_control_plane_mode(app: AppHandle) -> String {
    control_plane_mode_for_app(&app)
}

#[tauri::command]
pub async fn pair_device(fp: String, app: AppHandle, state: AppStateRef<'_>) -> Result<(), String> {
    if proxy_ack(
        &app,
        LocalIpcCommand::TrustPair {
            fingerprint: fp.clone(),
        },
    )
    .await?
    {
        return Ok(());
    }
    let service = AppCoreService::with_app(Arc::clone(&state), app);
    service.pair_device(&fp).await
}

#[tauri::command]
pub async fn unpair_device(
    fp: String,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    if proxy_ack(
        &app,
        LocalIpcCommand::TrustUnpair {
            fingerprint: fp.clone(),
        },
    )
    .await?
    {
        return Ok(());
    }
    let service = AppCoreService::with_app(Arc::clone(&state), app);
    service.unpair_device(&fp).await
}

#[tauri::command]
pub async fn set_trusted_alias(
    fp: String,
    alias: Option<String>,
    _app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    if proxy_ack(
        &_app,
        LocalIpcCommand::TrustSetAlias {
            fingerprint: fp.clone(),
            alias: alias.clone(),
        },
    )
    .await?
    {
        return Ok(());
    }
    let service = AppCoreService::with_app(Arc::clone(&state), _app);
    service.set_trusted_alias(&fp, alias).await
}

#[tauri::command]
pub async fn confirm_trusted_peer_verification(
    fp: String,
    verification_method: TrustVerificationMethod,
    mutual_confirmation: bool,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    if proxy_ack(
        &app,
        LocalIpcCommand::TrustConfirmVerification {
            fingerprint: fp.clone(),
            verification_method: verification_method.clone(),
            mutual_confirmation,
        },
    )
    .await?
    {
        return Ok(());
    }
    let service = AppCoreService::with_app(Arc::clone(&state), app);
    service
        .confirm_trusted_peer_verification(&fp, verification_method, mutual_confirmation)
        .await
}

// ─── Transfer commands ───────────────────────────────────────────────────────

#[tauri::command]
pub async fn send_files_cmd(
    peer_fp: String,
    paths: Vec<String>,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    if proxy_ack(
        &app,
        LocalIpcCommand::TransferSend {
            peer_fingerprint: peer_fp.clone(),
            paths: paths.clone(),
        },
    )
    .await?
    {
        return Ok(());
    }
    let service = AppCoreService::with_app(Arc::clone(&state), app);
    service.send_files(peer_fp, paths).await
}

#[tauri::command]
pub async fn connect_by_address(
    address: String,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<ConnectByAddressResult, String> {
    if let Some(result) = proxy_write_result::<ConnectByAddressResult>(
        &app,
        LocalIpcCommand::DiscoverConnectByAddress {
            address: address.clone(),
        },
        "result",
    )
    .await?
    {
        return Ok(result);
    }
    let service = AppCoreService::with_app(Arc::clone(&state), app);
    service.connect_by_address(address).await
}

#[tauri::command]
pub async fn accept_transfer(
    transfer_id: String,
    notification_id: String,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    if proxy_ack(
        &app,
        LocalIpcCommand::TransferAccept {
            transfer_id: transfer_id.clone(),
            notification_id: notification_id.clone(),
        },
    )
    .await?
    {
        return Ok(());
    }
    let service = AppCoreService::with_app(Arc::clone(&state), app);
    service
        .accept_transfer(&transfer_id, &notification_id)
        .await
}

#[tauri::command]
pub async fn accept_and_pair_transfer(
    transfer_id: String,
    notification_id: String,
    sender_fp: String,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    accept_transfer(
        transfer_id,
        notification_id,
        app.clone(),
        State::clone(&state),
    )
    .await?;
    pair_device(sender_fp, app, State::clone(&state)).await
}

#[tauri::command]
pub async fn reject_transfer(
    transfer_id: String,
    notification_id: String,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    if proxy_ack(
        &app,
        LocalIpcCommand::TransferReject {
            transfer_id: transfer_id.clone(),
            notification_id: notification_id.clone(),
        },
    )
    .await?
    {
        return Ok(());
    }
    let service = AppCoreService::with_app(Arc::clone(&state), app);
    service
        .reject_transfer(&transfer_id, &notification_id)
        .await
}

#[tauri::command]
pub async fn cancel_transfer(
    transfer_id: String,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    if proxy_ack(
        &app,
        LocalIpcCommand::TransferCancel {
            transfer_id: transfer_id.clone(),
        },
    )
    .await?
    {
        return Ok(());
    }
    let service = AppCoreService::with_app(Arc::clone(&state), app);
    service.cancel_transfer(&transfer_id).await
}

#[tauri::command]
pub async fn cancel_all_transfers(app: AppHandle, state: AppStateRef<'_>) -> Result<u32, String> {
    if let Some(count) =
        proxy_write_result::<u32>(&app, LocalIpcCommand::TransferCancelAll, "count").await?
    {
        return Ok(count);
    }
    let service = AppCoreService::with_app(Arc::clone(&state), app);
    service.cancel_all_transfers().await
}

#[tauri::command]
pub async fn retry_transfer(
    transfer_id: String,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    if proxy_ack(
        &app,
        LocalIpcCommand::TransferRetry {
            transfer_id: transfer_id.clone(),
        },
    )
    .await?
    {
        return Ok(());
    }
    let service = AppCoreService::with_app(Arc::clone(&state), app);
    service.retry_transfer(&transfer_id).await
}

#[cfg(test)]
mod diagnostics_tests {
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::crypto::Identity;
    use crate::discovery::beacon::{BeaconCadence, PowerProfile};
    use crate::state::{
        AppConfig, RuntimeEventCheckpoint, RUNTIME_EVENT_CHECKPOINT_ACTIVE_THRESHOLD_MS,
        RUNTIME_EVENT_CHECKPOINT_STALE_THRESHOLD_MS,
    };

    use super::{build_discovery_diagnostics, windows_non_admin_firewall_hint};

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

    #[tokio::test]
    async fn discovery_diagnostics_include_runtime_event_replay_snapshot() {
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

        let event = state.record_runtime_event("device_updated", serde_json::json!({ "index": 1 }));
        state
            .save_runtime_event_checkpoint("shared-ui", &state.runtime_event_generation, event.seq)
            .await
            .expect("save checkpoint");

        let diagnostics = build_discovery_diagnostics(
            &state,
            BeaconCadence {
                power_profile: PowerProfile::Ac,
                interval_secs: 3,
            },
        )
        .await;

        assert_eq!(
            diagnostics["runtime_event_replay"]["latest_seq"].as_u64(),
            Some(event.seq)
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["memory_window_capacity"].as_u64(),
            Some(crate::state::RUNTIME_EVENT_FEED_CAPACITY as u64)
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["persisted_window_capacity"].as_u64(),
            Some(crate::state::RUNTIME_EVENT_PERSISTED_JOURNAL_CAPACITY as u64)
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["persisted_window_max_capacity"].as_u64(),
            Some(crate::state::RUNTIME_EVENT_PERSISTED_JOURNAL_MAX_CAPACITY as u64)
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["persisted_segment_size"].as_u64(),
            Some(crate::state::RUNTIME_EVENT_PERSISTED_SEGMENT_SIZE as u64)
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["persisted_segment_count"].as_u64(),
            Some(1)
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["compacted_segment_count"].as_u64(),
            Some(0)
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["compaction_watermark_seq"].as_u64(),
            Some(0)
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["compaction_watermark_segment_id"].as_u64(),
            Some(0)
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["checkpoint_count"].as_u64(),
            Some(1)
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["active_checkpoint_count"].as_u64(),
            Some(1)
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["checkpoints"][0]["consumer_id"].as_str(),
            Some("shared-ui")
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["checkpoints"][0]["recovery_state"].as_str(),
            Some("up_to_date")
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["metrics"]["checkpoint_saves"].as_u64(),
            Some(1)
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["checkpoint_heartbeat_interval_ms"].as_u64(),
            Some(crate::state::RUNTIME_EVENT_CHECKPOINT_HEARTBEAT_INTERVAL_MS)
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["retention_mode"].as_str(),
            Some("baseline")
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["oldest_recoverable_seq"].as_u64(),
            Some(event.seq)
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["retention_cutoff_reason"].as_str(),
            Some("baseline_capacity")
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["persisted_journal_health"].as_str(),
            Some("available")
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["retention_pinned_checkpoint_count"].as_u64(),
            Some(1)
        );
        assert!(
            diagnostics["runtime_event_replay"]["checkpoints"][0]["lease_expires_at_unix_ms"]
                .as_u64()
                .is_some()
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["checkpoints"][0]["consumer_recovery_mode"]
                .as_str(),
            Some("incremental_catch_up")
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["checkpoints"][0]["recovery_safety"].as_str(),
            Some("safe_incremental")
        );
        assert_eq!(
            diagnostics["transfer_progress_persistence"]["flush_interval_ms"].as_u64(),
            Some(3_000)
        );
        assert_eq!(
            diagnostics["transfer_progress_persistence"]["flush_threshold_bytes"].as_u64(),
            Some(32 * 1024 * 1024)
        );
        assert_eq!(
            diagnostics["transfer_progress_persistence"]["successful_writes"].as_u64(),
            Some(0)
        );
        assert_eq!(
            diagnostics["link_capabilities"]["fallback_mode"].as_str(),
            Some("qr_or_short_code")
        );
    }

    #[tokio::test]
    async fn discovery_diagnostics_include_ble_assist_observation_snapshot() {
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
        state
            .mark_ble_runtime_started("noop", "idle_noop", "capsule_preview_only")
            .await;
        let observed_at = super::now_unix_millis();
        state
            .record_ble_assist_observation(&crate::ble::BleAssistCapsule {
                version: 1,
                issued_at_unix_ms: observed_at,
                expires_at_unix_ms: observed_at.saturating_add(60_000),
                rolling_identifier: "capsule-1".into(),
                integrity_tag: "tag-1".into(),
                transport_hint: "qr_or_short_code_fallback".into(),
                qr_fallback_available: true,
                short_code_fallback_available: true,
                rotation_window_ms: 30_000,
            })
            .await;

        let diagnostics = build_discovery_diagnostics(
            &state,
            BeaconCadence {
                power_profile: PowerProfile::Ac,
                interval_secs: 3,
            },
        )
        .await;

        assert_eq!(
            diagnostics["link_capabilities"]["observed_capsule_count"].as_u64(),
            Some(1)
        );
        assert_eq!(
            diagnostics["link_capabilities"]["recent_capsules"][0]["rolling_identifier"].as_str(),
            Some("capsule-1")
        );
        assert_eq!(
            diagnostics["link_capabilities"]["provider_name"].as_str(),
            Some("noop")
        );
    }

    #[tokio::test]
    async fn discovery_diagnostics_include_ble_bridge_snapshot_metadata() {
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
        state
            .mark_ble_runtime_started(
                "macos_native",
                "bridge_authorized_idle",
                "scaffold_unimplemented",
            )
            .await;
        state
            .update_ble_runtime_bridge(
                Some("granted".into()),
                Some("json_file_bridge".into()),
                Some("/tmp/dashdrop-ble-bridge.json".into()),
                Some(1_700_000_000_000),
                Some("bridge-roll".into()),
            )
            .await;
        state
            .update_ble_runtime_advertisement(
                Some("/tmp/dashdrop-ble-advertisement.json".into()),
                Some(1_700_000_000_100),
                Some("request-roll".into()),
            )
            .await;

        let diagnostics = build_discovery_diagnostics(
            &state,
            BeaconCadence {
                power_profile: PowerProfile::Ac,
                interval_secs: 3,
            },
        )
        .await;

        assert_eq!(
            diagnostics["link_capabilities"]["ble_permission_state"].as_str(),
            Some("granted")
        );
        assert_eq!(
            diagnostics["link_capabilities"]["bridge_mode"].as_str(),
            Some("json_file_bridge")
        );
        assert_eq!(
            diagnostics["link_capabilities"]["bridge_file_path"].as_str(),
            Some("/tmp/dashdrop-ble-bridge.json")
        );
        assert_eq!(
            diagnostics["link_capabilities"]["last_bridge_snapshot_at_unix_ms"].as_u64(),
            Some(1_700_000_000_000)
        );
        assert_eq!(
            diagnostics["link_capabilities"]["advertisement_file_path"].as_str(),
            Some("/tmp/dashdrop-ble-advertisement.json")
        );
        assert_eq!(
            diagnostics["link_capabilities"]["last_advertisement_request_at_unix_ms"].as_u64(),
            Some(1_700_000_000_100)
        );
        assert_eq!(
            diagnostics["link_capabilities"]["advertised_rolling_identifier"].as_str(),
            Some("request-roll")
        );
        assert_eq!(
            diagnostics["link_capabilities"]["ble_runtime_available"].as_bool(),
            Some(cfg!(debug_assertions))
        );
    }

    #[tokio::test]
    async fn discovery_diagnostics_include_last_replay_source_and_resync_reason() {
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

        {
            let mut feed = state
                .runtime_event_feed
                .lock()
                .expect("runtime event feed lock poisoned");
            for seq in 9_077..=10_100 {
                feed.push_back(crate::state::RuntimeEventEnvelope {
                    seq,
                    event: "device_updated".to_string(),
                    payload: serde_json::json!({ "index": seq }),
                    emitted_at_unix_ms: seq,
                });
            }
        }
        state
            .runtime_event_seq
            .store(10_100, std::sync::atomic::Ordering::SeqCst);
        state
            .runtime_event_persisted_oldest_seq
            .store(101, std::sync::atomic::Ordering::SeqCst);

        let snapshot = state.runtime_events_since(10, 25);
        assert!(snapshot.resync_required);

        let diagnostics = build_discovery_diagnostics(
            &state,
            BeaconCadence {
                power_profile: PowerProfile::Ac,
                interval_secs: 3,
            },
        )
        .await;

        assert_eq!(
            diagnostics["runtime_event_replay"]["metrics"]["last_replay_source"].as_str(),
            Some("resync_required")
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["metrics"]["last_resync_reason"].as_str(),
            Some("cursor_before_oldest_available")
        );
        assert!(
            diagnostics["runtime_event_replay"]["metrics"]["last_replay_source_at_unix_ms"]
                .as_u64()
                .is_some()
        );
        assert!(
            diagnostics["runtime_event_replay"]["metrics"]["last_resync_required_at_unix_ms"]
                .as_u64()
                .is_some()
        );
    }

    #[tokio::test]
    async fn discovery_diagnostics_recommend_connect_by_address_when_discovery_isolation_is_likely()
    {
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

        state.bump_discovery_event("search_started").await;
        state.bump_discovery_event("beacon_sent").await;

        let diagnostics = build_discovery_diagnostics(
            &state,
            BeaconCadence {
                power_profile: PowerProfile::Ac,
                interval_secs: 3,
            },
        )
        .await;

        let quick_hints = diagnostics["quick_hints"]
            .as_array()
            .expect("quick hints array");
        assert!(quick_hints.iter().any(|hint| {
            hint.as_str()
                .map(|value| {
                    value.contains("Connect by Address")
                        && (value.contains("VLAN") || value.contains("subnet"))
                })
                .unwrap_or(false)
        }));
    }

    #[tokio::test]
    async fn discovery_diagnostics_summarize_checkpoint_lifecycle_and_recovery_states() {
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

        for index in 0..1_500 {
            state.record_runtime_event("device_updated", serde_json::json!({ "index": index }));
        }

        let generation = state.runtime_event_generation.clone();
        let now_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("unix time")
            .as_millis() as u64;

        {
            let guard = state.db.lock().expect("db lock");
            for checkpoint in [
                RuntimeEventCheckpoint {
                    consumer_id: "active-up-to-date".into(),
                    generation: generation.clone(),
                    seq: 1_500,
                    updated_at_unix_ms: now_unix_ms,
                    created_at_unix_ms: Some(now_unix_ms),
                    last_read_at_unix_ms: None,
                    lease_expires_at_unix_ms: None,
                    revision: Some(1),
                    last_transition: Some("created".into()),
                    recovery_hint: Some("up_to_date".into()),
                    current_oldest_available_seq: None,
                    current_latest_available_seq: None,
                    current_compaction_watermark_seq: None,
                    current_compaction_watermark_segment_id: None,
                },
                RuntimeEventCheckpoint {
                    consumer_id: "active-hot-window".into(),
                    generation: generation.clone(),
                    seq: 1_499,
                    updated_at_unix_ms: now_unix_ms,
                    created_at_unix_ms: Some(now_unix_ms),
                    last_read_at_unix_ms: None,
                    lease_expires_at_unix_ms: None,
                    revision: Some(1),
                    last_transition: Some("created".into()),
                    recovery_hint: Some("hot_window".into()),
                    current_oldest_available_seq: None,
                    current_latest_available_seq: None,
                    current_compaction_watermark_seq: None,
                    current_compaction_watermark_segment_id: None,
                },
                RuntimeEventCheckpoint {
                    consumer_id: "idle-persisted".into(),
                    generation: generation.clone(),
                    seq: 400,
                    updated_at_unix_ms: now_unix_ms
                        .saturating_sub(RUNTIME_EVENT_CHECKPOINT_ACTIVE_THRESHOLD_MS + 1_000),
                    created_at_unix_ms: Some(now_unix_ms),
                    last_read_at_unix_ms: None,
                    lease_expires_at_unix_ms: None,
                    revision: Some(1),
                    last_transition: Some("created".into()),
                    recovery_hint: Some("persisted_catch_up".into()),
                    current_oldest_available_seq: None,
                    current_latest_available_seq: None,
                    current_compaction_watermark_seq: None,
                    current_compaction_watermark_segment_id: None,
                },
                RuntimeEventCheckpoint {
                    consumer_id: "stale-resync".into(),
                    generation: "older-generation".into(),
                    seq: 10,
                    updated_at_unix_ms: now_unix_ms
                        .saturating_sub(RUNTIME_EVENT_CHECKPOINT_STALE_THRESHOLD_MS + 1_000),
                    created_at_unix_ms: Some(now_unix_ms),
                    last_read_at_unix_ms: None,
                    lease_expires_at_unix_ms: None,
                    revision: Some(1),
                    last_transition: Some("generation_reset".into()),
                    recovery_hint: Some("resync_required".into()),
                    current_oldest_available_seq: None,
                    current_latest_available_seq: None,
                    current_compaction_watermark_seq: None,
                    current_compaction_watermark_segment_id: None,
                },
            ] {
                crate::db::save_runtime_event_checkpoint(&guard, &checkpoint)
                    .expect("save checkpoint");
            }
        }

        let diagnostics = build_discovery_diagnostics(
            &state,
            BeaconCadence {
                power_profile: PowerProfile::Ac,
                interval_secs: 3,
            },
        )
        .await;

        assert_eq!(
            diagnostics["runtime_event_replay"]["checkpoint_count"].as_u64(),
            Some(4)
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["active_checkpoint_count"].as_u64(),
            Some(2)
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["idle_checkpoint_count"].as_u64(),
            Some(1)
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["stale_checkpoint_count"].as_u64(),
            Some(1)
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["resync_required_checkpoint_count"].as_u64(),
            Some(1)
        );

        let checkpoints = diagnostics["runtime_event_replay"]["checkpoints"]
            .as_array()
            .expect("checkpoint diagnostics array");

        let find_checkpoint = |consumer_id: &str| {
            checkpoints
                .iter()
                .find(|row| row["consumer_id"].as_str() == Some(consumer_id))
                .unwrap_or_else(|| panic!("missing checkpoint row for {consumer_id}"))
        };

        assert_eq!(
            find_checkpoint("active-up-to-date")["lifecycle_state"].as_str(),
            Some("active")
        );
        assert_eq!(
            find_checkpoint("active-up-to-date")["recovery_state"].as_str(),
            Some("up_to_date")
        );
        assert_eq!(
            find_checkpoint("active-hot-window")["recovery_state"].as_str(),
            Some("hot_window")
        );
        assert_eq!(
            find_checkpoint("idle-persisted")["lifecycle_state"].as_str(),
            Some("idle")
        );
        assert_eq!(
            find_checkpoint("idle-persisted")["recovery_state"].as_str(),
            Some("persisted_catch_up")
        );
        assert_eq!(
            find_checkpoint("stale-resync")["lifecycle_state"].as_str(),
            Some("stale")
        );
        assert_eq!(
            find_checkpoint("stale-resync")["recovery_state"].as_str(),
            Some("resync_required")
        );
    }

    #[tokio::test]
    async fn discovery_diagnostics_report_checkpoint_pinned_max_capped_retention_mode() {
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

        let latest_seq = 120_000u64;
        let pinned_oldest_seq = 20_001u64;
        let now_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("unix time")
            .as_millis() as u64;

        state.runtime_event_seq.store(latest_seq, Ordering::SeqCst);
        state
            .runtime_event_persisted_oldest_seq
            .store(pinned_oldest_seq, Ordering::SeqCst);

        {
            let guard = state.db.lock().expect("db lock");
            crate::db::save_runtime_event_checkpoint(
                &guard,
                &RuntimeEventCheckpoint {
                    consumer_id: "shared-ui".into(),
                    generation: state.runtime_event_generation.clone(),
                    seq: 25_000,
                    updated_at_unix_ms: now_unix_ms,
                    created_at_unix_ms: Some(now_unix_ms),
                    last_read_at_unix_ms: None,
                    lease_expires_at_unix_ms: None,
                    revision: Some(1),
                    last_transition: Some("advanced".into()),
                    recovery_hint: Some("persisted_catch_up".into()),
                    current_oldest_available_seq: None,
                    current_latest_available_seq: None,
                    current_compaction_watermark_seq: None,
                    current_compaction_watermark_segment_id: None,
                },
            )
            .expect("save checkpoint");
        }

        let diagnostics = build_discovery_diagnostics(
            &state,
            BeaconCadence {
                power_profile: PowerProfile::Ac,
                interval_secs: 3,
            },
        )
        .await;

        assert_eq!(
            diagnostics["runtime_event_replay"]["persisted_oldest_seq"].as_u64(),
            Some(pinned_oldest_seq)
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["persisted_baseline_oldest_seq"].as_u64(),
            Some(latest_seq - crate::state::RUNTIME_EVENT_PERSISTED_JOURNAL_CAPACITY as u64 + 1)
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["persisted_max_oldest_seq"].as_u64(),
            Some(
                latest_seq - crate::state::RUNTIME_EVENT_PERSISTED_JOURNAL_MAX_CAPACITY as u64 + 1
            )
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["retention_mode"].as_str(),
            Some("checkpoint_pinned_max_capped")
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["retention_pinned_checkpoint_count"].as_u64(),
            Some(1)
        );
        assert_eq!(
            diagnostics["runtime_event_replay"]["oldest_retention_pinned_checkpoint_seq"].as_u64(),
            Some(25_000)
        );
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
    app: tauri::AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    use tauri::Manager;
    let custom_dir = if let Some(config) =
        proxy_read::<crate::state::AppConfig>(&app, LocalIpcCommand::ConfigGet, "config").await?
    {
        config.download_dir
    } else {
        state.config.read().await.download_dir.clone()
    };
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
pub async fn get_app_config(
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<crate::state::AppConfig, String> {
    if let Some(config) =
        proxy_read::<crate::state::AppConfig>(&app, LocalIpcCommand::ConfigGet, "config").await?
    {
        return Ok(config);
    }
    Ok(state.config.read().await.clone())
}

#[tauri::command]
pub async fn set_app_config(
    config: crate::state::AppConfig,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    let previous_config = if let Some(previous) =
        proxy_read::<crate::state::AppConfig>(&app, LocalIpcCommand::ConfigGet, "config").await?
    {
        previous
    } else {
        state.config.read().await.clone()
    };

    let launch_at_login_changed = previous_config.launch_at_login != config.launch_at_login;

    let persisted_via_proxy = proxy_ack(
        &app,
        LocalIpcCommand::ConfigSet {
            config: config.clone(),
        },
    )
    .await?;
    if !persisted_via_proxy {
        let service = AppCoreService::with_app(Arc::clone(&state), app.clone());
        service.set_app_config(config.clone()).await?;
    }

    if !launch_at_login_changed {
        return Ok(());
    }

    if let Err(err) = crate::runtime::autostart::sync_launch_at_login(&app, config.launch_at_login)
    {
        let rollback_result = if persisted_via_proxy {
            let rolled_back = proxy_ack(
                &app,
                LocalIpcCommand::ConfigSet {
                    config: previous_config.clone(),
                },
            )
            .await?;
            if rolled_back {
                Ok(())
            } else {
                Err(
                    "daemon-backed config rollback unexpectedly fell back to local path"
                        .to_string(),
                )
            }
        } else {
            let service = AppCoreService::with_app(Arc::clone(&state), app.clone());
            service.set_app_config(previous_config.clone()).await
        };
        if let Err(rollback_err) = rollback_result {
            tracing::warn!(
                "failed to roll back app config after launch-at-login sync error: {rollback_err}"
            );
        }
        return Err(format!(
            "failed to update launch-at-login registration: {err}"
        ));
    }

    Ok(())
}

#[tauri::command]
pub fn set_shell_attention_state(
    pending_incoming_count: u32,
    active_transfer_count: u32,
    recent_failure_count: u32,
    notifications_degraded: bool,
    app: AppHandle,
) -> Result<(), String> {
    // Shared shell-attention contract: keep these exact field names and
    // semantics aligned with `src/ipc.ts` and `src/store.ts`. This command is
    // intentionally narrow and only mirrors already-derived UI shell state.
    super::update_tray_attention(
        &app,
        super::TrayAttentionState {
            pending_incoming_count,
            active_transfer_count,
            recent_failure_count,
            notifications_degraded,
        },
    )
}

// ─── App info commands ───────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_local_identity(
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<crate::state::LocalIdentityView, String> {
    if let Some(identity) = proxy_read::<crate::state::LocalIdentityView>(
        &app,
        LocalIpcCommand::AppGetLocalIdentity,
        "identity",
    )
    .await?
    {
        return Ok(identity);
    }
    Ok(crate::dto::local_identity_view(
        state.identity.fingerprint.clone(),
        state.config.read().await.device_name.clone(),
        *state.local_port.read().await,
    ))
}

#[tauri::command]
pub async fn get_local_ble_assist_capsule(
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<crate::ble::BleAssistCapsule, String> {
    if let Some(capsule) = proxy_read::<crate::ble::BleAssistCapsule>(
        &app,
        LocalIpcCommand::AppGetBleAssistCapsule,
        "capsule",
    )
    .await?
    {
        return Ok(capsule);
    }
    crate::ble::build_ble_assist_capsule(&state.identity).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn ingest_ble_assist_capsule(
    capsule: crate::ble::BleAssistCapsule,
    source: Option<String>,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    if proxy_ack(
        &app,
        LocalIpcCommand::AppIngestBleAssistCapsule {
            capsule: capsule.clone(),
            source: source.clone(),
        },
    )
    .await?
    {
        return Ok(());
    }
    let service = AppCoreService::with_app(Arc::clone(&state), app);
    service.ingest_ble_assist_capsule(capsule, source).await
}

#[tauri::command]
pub async fn get_transfers(
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<Vec<TransferView>, String> {
    if let Some(transfers) =
        proxy_read::<Vec<TransferView>>(&app, LocalIpcCommand::TransferList, "transfers").await?
    {
        return Ok(transfers);
    }
    let transfers = state.transfers.read().await;
    Ok(transfers.values().map(TransferView::from).collect())
}

#[tauri::command]
pub async fn get_transfer(
    app: AppHandle,
    transfer_id: String,
    state: AppStateRef<'_>,
) -> Result<Option<TransferView>, String> {
    if let Some(transfer) = proxy_read::<Option<TransferView>>(
        &app,
        LocalIpcCommand::TransferGet {
            transfer_id: transfer_id.clone(),
        },
        "transfer",
    )
    .await?
    {
        return Ok(transfer);
    }
    let transfers = state.transfers.read().await;
    Ok(transfers.get(&transfer_id).map(TransferView::from))
}

#[tauri::command]
pub async fn get_transfer_history(
    app: AppHandle,
    limit: u32,
    offset: u32,
    state: AppStateRef<'_>,
) -> Result<Vec<TransferView>, String> {
    if let Some(history) = proxy_read::<Vec<TransferView>>(
        &app,
        LocalIpcCommand::TransferHistory { limit, offset },
        "history",
    )
    .await?
    {
        return Ok(history);
    }
    let guard = state.db.lock().map_err(|_| "DB lock poisoned")?;
    let history = crate::db::get_history(&guard, limit, offset).map_err(|e| e.to_string())?;
    Ok(history.iter().map(TransferView::from).collect())
}

#[tauri::command]
pub async fn get_security_events(
    app: AppHandle,
    limit: u32,
    offset: u32,
    state: AppStateRef<'_>,
) -> Result<Vec<crate::state::SecurityEvent>, String> {
    if let Some(events) = proxy_read::<Vec<crate::state::SecurityEvent>>(
        &app,
        LocalIpcCommand::SecurityGetEvents { limit, offset },
        "events",
    )
    .await?
    {
        return Ok(events);
    }
    let guard = state.db.lock().map_err(|_| "DB lock poisoned")?;
    crate::db::get_security_events(&guard, limit, offset).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_security_posture(app: AppHandle) -> Result<serde_json::Value, String> {
    if let Some(posture) =
        proxy_read::<serde_json::Value>(&app, LocalIpcCommand::SecurityGetPosture, "posture")
            .await?
    {
        return Ok(posture);
    }
    Ok(serde_json::json!({
        "secure_store_available": crate::crypto::secret_store::secure_store_available(),
    }))
}

#[tauri::command]
pub async fn get_runtime_status(
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<crate::state::RuntimeStatus, String> {
    if let Some(status) = proxy_read::<crate::state::RuntimeStatus>(
        &app,
        LocalIpcCommand::AppGetRuntimeStatus,
        "runtime_status",
    )
    .await?
    {
        return Ok(status);
    }
    Ok(state.runtime_status().await)
}

#[tauri::command]
pub async fn get_runtime_events(
    after_seq: u64,
    limit: u32,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<RuntimeEventFeedSnapshot, String> {
    if let Some(payload) = proxy_command_payload_with_retries(
        &app,
        LocalIpcCommand::AppGetEventFeed { after_seq, limit },
        DAEMON_PROXY_READ_RETRY_DELAYS_MS,
    )
    .await?
    {
        return parse_payload(payload);
    }
    Ok(state.runtime_events_since(after_seq, limit as usize))
}

#[tauri::command]
pub async fn get_runtime_event_checkpoint(
    consumer_id: String,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<Option<RuntimeEventCheckpoint>, String> {
    if let Some(checkpoint) = proxy_read::<Option<RuntimeEventCheckpoint>>(
        &app,
        LocalIpcCommand::AppGetEventCheckpoint {
            consumer_id: consumer_id.clone(),
        },
        "checkpoint",
    )
    .await?
    {
        return Ok(checkpoint);
    }
    state.runtime_event_checkpoint(&consumer_id).await
}

#[tauri::command]
pub async fn set_runtime_event_checkpoint(
    consumer_id: String,
    generation: String,
    seq: u64,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<bool, String> {
    if proxy_ack(
        &app,
        LocalIpcCommand::AppSetEventCheckpoint {
            consumer_id: consumer_id.clone(),
            generation: generation.clone(),
            seq,
        },
    )
    .await?
    {
        return Ok(true);
    }
    state
        .save_runtime_event_checkpoint(&consumer_id, &generation, seq)
        .await?;
    Ok(true)
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
            "No peers resolved from mDNS browse yet; likely multicast traffic is blocked across firewall/VLAN/subnet. Automatic discovery is not expected to cross VLAN/subnet boundaries by default, so use Connect by Address for a manual host:port path."
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
            "Some peers resolved without usable addresses; inspect virtual adapters/VPN interfaces and peer IP advertisement, or fall back to Connect by Address on a known LAN endpoint."
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
            "No inbound discovery packets seen from mDNS or UDP beacon; check AP isolation, VLAN segmentation, or host firewall multicast/broadcast rules. If the peers are on different subnets, use Connect by Address instead of waiting for Nearby."
                .to_string(),
        );
    } else if ctx.beacon_received > 0 && ctx.resolved_events == 0 {
        quick_hints.push(
            "UDP beacon fallback is receiving peers while mDNS is silent; mDNS multicast is likely blocked on this network. Nearby discovery may stay partial until you use Connect by Address or adjust multicast policy."
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
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<serde_json::Value, String> {
    if let Some(diagnostics) = proxy_read::<serde_json::Value>(
        &app,
        LocalIpcCommand::AppGetDiscoveryDiagnostics,
        "diagnostics",
    )
    .await?
    {
        return Ok(diagnostics);
    }
    let state = Arc::clone(&state);
    let cadence = crate::discovery::beacon::current_beacon_cadence();
    Ok(build_discovery_diagnostics(&state, cadence).await)
}

#[allow(clippy::too_many_arguments)]
fn runtime_event_checkpoint_diagnostics(
    checkpoint: &RuntimeEventCheckpoint,
    generation: &str,
    latest_seq: u64,
    memory_oldest_seq: Option<u64>,
    persisted_oldest_seq: Option<u64>,
    oldest_recoverable_seq: Option<u64>,
    retention_cutoff_reason: &str,
    persisted_journal_health: &str,
    now_unix_ms: u64,
) -> serde_json::Value {
    let age_ms = now_unix_ms.saturating_sub(checkpoint.updated_at_unix_ms);
    let lag_events = if checkpoint.generation == generation {
        latest_seq.saturating_sub(checkpoint.seq)
    } else {
        latest_seq
    };
    let lease_expires_at_unix_ms = checkpoint
        .updated_at_unix_ms
        .saturating_add(crate::state::RUNTIME_EVENT_CHECKPOINT_ACTIVE_THRESHOLD_MS);
    let lifecycle_state = if age_ms <= crate::state::RUNTIME_EVENT_CHECKPOINT_ACTIVE_THRESHOLD_MS {
        "active"
    } else if age_ms <= crate::state::RUNTIME_EVENT_CHECKPOINT_STALE_THRESHOLD_MS {
        "idle"
    } else {
        "stale"
    };
    let recovery_state = if checkpoint.generation != generation {
        "resync_required"
    } else if latest_seq == 0 || checkpoint.seq >= latest_seq {
        "up_to_date"
    } else if memory_oldest_seq
        .map(|oldest_seq| checkpoint.seq >= oldest_seq.saturating_sub(1))
        .unwrap_or(true)
    {
        "hot_window"
    } else if persisted_oldest_seq
        .map(|oldest_seq| checkpoint.seq >= oldest_seq.saturating_sub(1))
        .unwrap_or(true)
    {
        "persisted_catch_up"
    } else {
        "resync_required"
    };
    let consumer_recovery_mode = if recovery_state == "resync_required" {
        "authoritative_refresh"
    } else {
        "incremental_catch_up"
    };
    let recovery_safety = if checkpoint.generation != generation {
        "generation_mismatch"
    } else if recovery_state == "resync_required" {
        "authoritative_refresh_required"
    } else {
        "safe_incremental"
    };

    serde_json::json!({
        "consumer_id": checkpoint.consumer_id,
        "generation": checkpoint.generation,
        "seq": checkpoint.seq,
        "updated_at_unix_ms": checkpoint.updated_at_unix_ms,
        "lease_expires_at_unix_ms": lease_expires_at_unix_ms,
        "age_ms": age_ms,
        "lag_events": lag_events,
        "lifecycle_state": lifecycle_state,
        "recovery_state": recovery_state,
        "recovery_hint": checkpoint.recovery_hint,
        "oldest_recoverable_seq": oldest_recoverable_seq,
        "retention_cutoff_reason": retention_cutoff_reason,
        "persisted_journal_health": persisted_journal_health,
        "consumer_recovery_mode": consumer_recovery_mode,
        "recovery_safety": recovery_safety,
        "current_oldest_available_seq": checkpoint.current_oldest_available_seq,
        "current_latest_available_seq": checkpoint.current_latest_available_seq,
        "current_compaction_watermark_seq": checkpoint.current_compaction_watermark_seq,
        "current_compaction_watermark_segment_id": checkpoint.current_compaction_watermark_segment_id,
    })
}

pub(crate) async fn build_discovery_diagnostics(
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
    let ble_runtime_status = state.ble_runtime_status_snapshot().await;
    let listener_mode = state.listener_mode.read().await.clone();
    let listener_port_mode = state.listener_port_mode.read().await.clone();
    let firewall_rule_state = state.firewall_rule_state.read().await.clone();
    let listener_addrs = state.listener_addrs.read().await.clone();
    let network_interfaces = collect_network_interfaces();
    let slo_observability = state.slo_observability_snapshot().await;
    let runtime_event_feed_len = state
        .runtime_event_feed
        .lock()
        .map(|feed| feed.len())
        .unwrap_or_default();
    let runtime_event_memory_oldest_seq = state
        .runtime_event_feed
        .lock()
        .ok()
        .and_then(|feed| feed.front().map(|event| event.seq));
    let runtime_event_latest_seq = state
        .runtime_event_seq
        .load(std::sync::atomic::Ordering::SeqCst);
    let runtime_event_persisted_oldest_seq = match state
        .runtime_event_persisted_oldest_seq
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        0 => None,
        seq => Some(seq),
    };
    let runtime_event_replay_metrics = state.runtime_event_replay_metrics_snapshot();
    let transfer_progress_persistence = state.progress_persistence.diagnostics_snapshot();
    let ble_assist_observations = state.ble_assist_observations_snapshot().await;
    let runtime_event_checkpoints = state.runtime_event_checkpoints().await.unwrap_or_default();
    let journal_stats = state
        .db
        .lock()
        .ok()
        .and_then(|guard| crate::db::load_runtime_event_journal_stats(&guard).ok());
    let retention_pinned_checkpoints: Vec<&RuntimeEventCheckpoint> = runtime_event_checkpoints
        .iter()
        .filter(|checkpoint| {
            checkpoint.generation == state.runtime_event_generation
                && now_unix_millis().saturating_sub(checkpoint.updated_at_unix_ms)
                    <= crate::state::RUNTIME_EVENT_CHECKPOINT_STALE_THRESHOLD_MS
        })
        .collect();
    let retention_pinned_checkpoint_count = retention_pinned_checkpoints.len();
    let oldest_retention_pinned_checkpoint_seq = retention_pinned_checkpoints
        .iter()
        .map(|checkpoint| checkpoint.seq)
        .min();
    let persisted_window_len = runtime_event_persisted_oldest_seq
        .map(|oldest_seq| {
            runtime_event_latest_seq
                .saturating_sub(oldest_seq)
                .saturating_add(1)
        })
        .unwrap_or(0);
    let oldest_recoverable_seq =
        runtime_event_persisted_oldest_seq.or(runtime_event_memory_oldest_seq);
    let persisted_baseline_oldest_seq = if runtime_event_latest_seq == 0 {
        None
    } else {
        Some(
            runtime_event_latest_seq
                .saturating_sub(crate::state::RUNTIME_EVENT_PERSISTED_JOURNAL_CAPACITY as u64 - 1)
                .max(1),
        )
    };
    let persisted_max_oldest_seq = if runtime_event_latest_seq == 0 {
        None
    } else {
        Some(
            runtime_event_latest_seq
                .saturating_sub(
                    crate::state::RUNTIME_EVENT_PERSISTED_JOURNAL_MAX_CAPACITY as u64 - 1,
                )
                .max(1),
        )
    };
    let retention_mode = match (
        runtime_event_persisted_oldest_seq,
        persisted_baseline_oldest_seq,
    ) {
        (None, _) => "empty",
        (Some(oldest_seq), Some(baseline_oldest_seq)) if oldest_seq < baseline_oldest_seq => {
            if persisted_max_oldest_seq == Some(oldest_seq) {
                "checkpoint_pinned_max_capped"
            } else {
                "checkpoint_pinned"
            }
        }
        _ => "baseline",
    };
    let persisted_journal_health = if journal_stats.is_none() {
        "unavailable"
    } else if runtime_event_latest_seq == 0 {
        "empty"
    } else {
        "available"
    };
    let retention_cutoff_reason = match retention_mode {
        "empty" => "journal_empty",
        "checkpoint_pinned_max_capped" => "checkpoint_pinned_max_capacity",
        "checkpoint_pinned" => "checkpoint_pinned",
        _ => "baseline_capacity",
    };
    let mut link_capabilities = build_link_capabilities(own_platform, &ble_runtime_status);
    if let Some(object) = link_capabilities.as_object_mut() {
        let recent_capsules = ble_assist_observations
            .iter()
            .map(|observation| {
                serde_json::json!({
                    "rolling_identifier": observation.rolling_identifier,
                    "integrity_tag": observation.integrity_tag,
                    "last_seen_at_unix_ms": observation.last_seen_at_unix_ms,
                    "expires_at_unix_ms": observation.expires_at_unix_ms,
                    "transport_hint": observation.transport_hint,
                })
            })
            .collect::<Vec<_>>();
        object.insert(
            "observed_capsule_count".to_string(),
            serde_json::json!(ble_assist_observations.len()),
        );
        object.insert(
            "recent_capsules".to_string(),
            serde_json::json!(recent_capsules),
        );
    }
    let runtime_event_checkpoint_rows: Vec<serde_json::Value> = runtime_event_checkpoints
        .iter()
        .map(|checkpoint| {
            runtime_event_checkpoint_diagnostics(
                checkpoint,
                &state.runtime_event_generation,
                runtime_event_latest_seq,
                runtime_event_memory_oldest_seq,
                runtime_event_persisted_oldest_seq,
                oldest_recoverable_seq,
                retention_cutoff_reason,
                persisted_journal_health,
                now_unix_millis(),
            )
        })
        .collect();
    let runtime_event_checkpoint_resync_required_count = runtime_event_checkpoint_rows
        .iter()
        .filter(|row| row["recovery_state"].as_str() == Some("resync_required"))
        .count();
    let runtime_event_checkpoint_stale_count = runtime_event_checkpoint_rows
        .iter()
        .filter(|row| row["lifecycle_state"].as_str() == Some("stale"))
        .count();
    let runtime_event_checkpoint_idle_count = runtime_event_checkpoint_rows
        .iter()
        .filter(|row| row["lifecycle_state"].as_str() == Some("idle"))
        .count();
    let runtime_event_checkpoint_active_count = runtime_event_checkpoint_rows
        .iter()
        .filter(|row| row["lifecycle_state"].as_str() == Some("active"))
        .count();
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
    let runtime_event_replay = serde_json::json!({
        "generation": state.runtime_event_generation,
        "latest_seq": runtime_event_latest_seq,
        "memory_window_capacity": crate::state::RUNTIME_EVENT_FEED_CAPACITY,
        "memory_window_len": runtime_event_feed_len,
        "memory_oldest_seq": runtime_event_memory_oldest_seq,
        "persisted_window_capacity": crate::state::RUNTIME_EVENT_PERSISTED_JOURNAL_CAPACITY,
        "persisted_window_max_capacity": crate::state::RUNTIME_EVENT_PERSISTED_JOURNAL_MAX_CAPACITY,
        "persisted_segment_size": crate::state::RUNTIME_EVENT_PERSISTED_SEGMENT_SIZE,
        "persisted_segment_count": journal_stats.as_ref().map(|stats| stats.active_segment_count).unwrap_or(0),
        "oldest_persisted_segment_id": journal_stats.as_ref().and_then(|stats| stats.oldest_active_segment_id),
        "latest_persisted_segment_id": journal_stats.as_ref().and_then(|stats| stats.latest_active_segment_id),
        "compacted_segment_count": journal_stats.as_ref().map(|stats| stats.compacted_segment_count).unwrap_or(0),
        "latest_compacted_segment_id": journal_stats.as_ref().and_then(|stats| stats.latest_compacted_segment_id),
        "compaction_watermark_seq": journal_stats.as_ref().map(|stats| stats.compaction_watermark_seq).unwrap_or(0),
        "compaction_watermark_segment_id": journal_stats.as_ref().map(|stats| stats.compaction_watermark_segment_id).unwrap_or(0),
        "last_compacted_at_unix_ms": journal_stats.as_ref().and_then(|stats| stats.last_compacted_at_unix_ms),
        "persisted_window_len": persisted_window_len,
        "persisted_oldest_seq": runtime_event_persisted_oldest_seq,
        "oldest_recoverable_seq": oldest_recoverable_seq,
        "persisted_baseline_oldest_seq": persisted_baseline_oldest_seq,
        "persisted_max_oldest_seq": persisted_max_oldest_seq,
        "retention_mode": retention_mode,
        "retention_cutoff_reason": retention_cutoff_reason,
        "persisted_journal_health": persisted_journal_health,
        "retention_pinned_checkpoint_count": retention_pinned_checkpoint_count,
        "oldest_retention_pinned_checkpoint_seq": oldest_retention_pinned_checkpoint_seq,
        "checkpoint_heartbeat_interval_ms": crate::state::RUNTIME_EVENT_CHECKPOINT_HEARTBEAT_INTERVAL_MS,
        "checkpoint_active_threshold_ms": crate::state::RUNTIME_EVENT_CHECKPOINT_ACTIVE_THRESHOLD_MS,
        "checkpoint_stale_threshold_ms": crate::state::RUNTIME_EVENT_CHECKPOINT_STALE_THRESHOLD_MS,
        "checkpoint_ttl_ms": crate::state::RUNTIME_EVENT_CHECKPOINT_TTL_MS,
        "checkpoint_count": runtime_event_checkpoint_rows.len(),
        "active_checkpoint_count": runtime_event_checkpoint_active_count,
        "idle_checkpoint_count": runtime_event_checkpoint_idle_count,
        "stale_checkpoint_count": runtime_event_checkpoint_stale_count,
        "resync_required_checkpoint_count": runtime_event_checkpoint_resync_required_count,
        "metrics": runtime_event_replay_metrics,
        "checkpoints": runtime_event_checkpoint_rows,
    });

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
        "runtime_event_replay": runtime_event_replay,
        "transfer_progress_persistence": transfer_progress_persistence,
        "link_capabilities": link_capabilities,
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
    use super::{
        collect_network_interfaces, control_plane_mode_from_sources, discovery_device_row,
    };
    use crate::state::{DeviceInfo, Platform, ReachabilityStatus, SessionInfo};
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::str::FromStr;
    use std::time::Instant;

    #[test]
    fn control_plane_mode_prefers_runtime_state_over_env() {
        assert_eq!(
            control_plane_mode_from_sources(Some("in_process"), Some("daemon")),
            "in_process"
        );
        assert_eq!(
            control_plane_mode_from_sources(Some("daemon"), Some("in_process")),
            "daemon"
        );
    }

    #[test]
    fn control_plane_mode_falls_back_to_env_and_default() {
        assert_eq!(
            control_plane_mode_from_sources(None, Some("daemon")),
            "daemon"
        );
        assert_eq!(
            control_plane_mode_from_sources(Some("invalid"), Some("in_process")),
            "in_process"
        );
        assert_eq!(control_plane_mode_from_sources(None, None), "in_process");
    }

    #[test]
    fn control_plane_mode_ignores_empty_or_invalid_state_values() {
        assert_eq!(
            control_plane_mode_from_sources(Some(" "), Some("daemon")),
            "daemon"
        );
        assert_eq!(
            control_plane_mode_from_sources(Some("invalid"), Some("daemon")),
            "daemon"
        );
        assert_eq!(
            control_plane_mode_from_sources(Some("invalid"), Some("in_process")),
            "in_process"
        );
    }

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
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<crate::state::TransferMetrics, String> {
    if let Some(metrics) = proxy_read::<crate::state::TransferMetrics>(
        &app,
        LocalIpcCommand::TransferGetMetrics,
        "metrics",
    )
    .await?
    {
        return Ok(metrics);
    }
    let guard = state.db.lock().map_err(|_| "DB lock poisoned")?;
    crate::db::get_transfer_metrics(&guard).map_err(|e| e.to_string())
}
