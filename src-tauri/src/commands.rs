use crate::core_service::AppCoreService;
use crate::dto::{DeviceView, TransferView, TrustedPeerView};
use crate::local_ipc::ConnectByAddressResult;
use crate::state::{
    AppState, DeviceInfo, FileItemMeta, Platform, ReachabilityStatus, TransferDirection,
    TransferStatus,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use tauri::{AppHandle, State};

type AppStateRef<'a> = State<'a, Arc<AppState>>;

#[derive(Debug, Clone, Serialize)]
pub struct PendingIncomingRequestPayload {
    pub transfer_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
    pub notification_id: String,
    pub sender_name: String,
    pub sender_fp: String,
    pub trusted: bool,
    pub items: Vec<FileItemMeta>,
    pub total_size: u64,
    pub revision: u64,
}
async fn pending_incoming_requests(state: &Arc<AppState>) -> Vec<PendingIncomingRequestPayload> {
    let notifications = state.incoming_request_notifications.read().await.clone();
    let trusted = state.trusted_peers.read().await.clone();
    let transfers = state.transfers.read().await;

    let mut requests = transfers
        .values()
        .filter(|task| {
            task.direction == TransferDirection::Receive
                && task.status == TransferStatus::PendingAccept
        })
        .filter_map(|task| {
            let notification = notifications.get(&task.id)?;
            if !notification.active {
                return None;
            }

            Some(PendingIncomingRequestPayload {
                transfer_id: task.id.clone(),
                batch_id: task.batch_id.clone(),
                notification_id: notification.notification_id.clone(),
                sender_name: task.peer_name.clone(),
                sender_fp: task.peer_fingerprint.clone(),
                trusted: trusted.contains_key(&task.peer_fingerprint),
                items: task.items.clone(),
                total_size: task.total_bytes,
                revision: task.revision,
            })
        })
        .collect::<Vec<_>>();

    requests.sort_by(|left, right| left.transfer_id.cmp(&right.transfer_id));
    requests
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
pub async fn get_pending_incoming_requests(
    state: AppStateRef<'_>,
) -> Result<Vec<PendingIncomingRequestPayload>, String> {
    Ok(pending_incoming_requests(&state).await)
}

#[tauri::command]
pub async fn pair_device(fp: String, app: AppHandle, state: AppStateRef<'_>) -> Result<(), String> {
    let service = AppCoreService::with_app(Arc::clone(&state), app);
    service.pair_device(&fp).await
}

#[tauri::command]
pub async fn unpair_device(
    fp: String,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
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
    let service = AppCoreService::with_app(Arc::clone(&state), _app);
    service.set_trusted_alias(&fp, alias).await
}

// ─── Transfer commands ───────────────────────────────────────────────────────

#[tauri::command]
pub async fn send_files_cmd(
    peer_fp: String,
    paths: Vec<String>,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    let service = AppCoreService::with_app(Arc::clone(&state), app);
    service.send_files(peer_fp, paths).await
}

#[tauri::command]
pub async fn connect_by_address(
    address: String,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<ConnectByAddressResult, String> {
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
    let service = AppCoreService::with_app(Arc::clone(&state), app);
    service.accept_transfer(&transfer_id, &notification_id).await
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
    let service = AppCoreService::with_app(Arc::clone(&state), app);
    service.reject_transfer(&transfer_id, &notification_id).await
}

#[tauri::command]
pub async fn cancel_transfer(
    transfer_id: String,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    let service = AppCoreService::with_app(Arc::clone(&state), app);
    service.cancel_transfer(&transfer_id).await
}

#[tauri::command]
pub async fn cancel_all_transfers(app: AppHandle, state: AppStateRef<'_>) -> Result<u32, String> {
    let service = AppCoreService::with_app(Arc::clone(&state), app);
    service.cancel_all_transfers().await
}

#[tauri::command]
pub async fn retry_transfer(
    transfer_id: String,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    let service = AppCoreService::with_app(Arc::clone(&state), app);
    service.retry_transfer(&transfer_id).await
}

#[cfg(test)]
mod diagnostics_tests {
    use std::sync::Arc;

    use crate::crypto::Identity;
    use crate::discovery::beacon::{BeaconCadence, PowerProfile};
    use crate::state::AppConfig;

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
    let service = AppCoreService::with_app(Arc::clone(&state), app);
    service.set_app_config(config).await
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
