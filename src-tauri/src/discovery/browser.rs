use anyhow::{Context, Result};
use flume::RecvTimeoutError;
use mdns_sd::{HostnameResolutionEvent, ServiceDaemon, ServiceEvent};
use std::collections::HashSet;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use tauri::{AppHandle, Emitter};

use super::service::SERVICE_TYPE;
use crate::dto::DeviceView;
use crate::state::{AppState, DeviceInfo, Platform, SessionIndexEntry, SessionInfo};

const OFFLINE_GRACE_SECS: u64 = 15;
const DEVICE_LOST_RETENTION_SECS: u64 = 45;
const SESSION_STALE_SECS: u64 = 90;
const HOSTNAME_RESOLVE_TIMEOUT_MS: u64 = 1500;

enum BrowserReceiveAction {
    Continue,
    Restart,
}

fn is_usable_peer_addr(addr: &SocketAddr) -> bool {
    if addr.ip().is_loopback() || addr.ip().is_unspecified() || addr.ip().is_multicast() {
        return false;
    }
    match addr {
        SocketAddr::V4(_v4) => true,
        SocketAddr::V6(_v6) => true,
    }
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn sort_dedupe_addrs(addrs: &mut Vec<SocketAddr>) {
    addrs.sort_by_key(|a| if a.is_ipv4() { 0 } else { 1 });
    let mut seen = HashSet::new();
    addrs.retain(|a| seen.insert(*a));
}

fn is_connectable_probe_addr(addr: &SocketAddr) -> bool {
    match addr {
        SocketAddr::V4(_) => true,
        SocketAddr::V6(v6) => !(v6.ip().is_unicast_link_local() && v6.scope_id() == 0),
    }
}

fn select_probe_addrs(mut addrs: Vec<SocketAddr>) -> Vec<SocketAddr> {
    sort_dedupe_addrs(&mut addrs);
    let filtered: Vec<SocketAddr> = addrs
        .iter()
        .copied()
        .filter(is_connectable_probe_addr)
        .collect();
    if filtered.is_empty() {
        addrs
    } else {
        filtered
    }
}

fn should_apply_deferred_remove(
    current_last_seen: u64,
    observed_last_seen: u64,
    now_unix: u64,
) -> bool {
    current_last_seen <= observed_last_seen
        && now_unix.saturating_sub(current_last_seen) >= OFFLINE_GRACE_SECS
}

fn is_session_stale(elapsed: Duration) -> bool {
    elapsed >= Duration::from_secs(SESSION_STALE_SECS)
}

fn browser_receive_action(err: &RecvTimeoutError) -> BrowserReceiveAction {
    match err {
        RecvTimeoutError::Timeout => BrowserReceiveAction::Continue,
        RecvTimeoutError::Disconnected => BrowserReceiveAction::Restart,
    }
}

fn pending_remove_key(fp: &str, session_id: &str) -> String {
    format!("{fp}|{session_id}")
}

fn classify_probe_error(detail: &str) -> (&'static str, &'static str) {
    let s = detail.to_ascii_lowercase();
    if s.contains("timeout") || s.contains("timed out") {
        ("probe_timeout", "timeout")
    } else if s.contains("handshake") {
        ("probe_handshake", "handshake")
    } else if s.contains("refused")
        || s.contains("unreachable")
        || s.contains("invalid remote address")
    {
        ("probe_connect", "connect")
    } else {
        ("probe_other", "other")
    }
}

async fn resolve_hostname_with_mdns(
    state: &Arc<AppState>,
    hostname: &str,
    port: u16,
) -> Vec<SocketAddr> {
    let Some(mdns) = state.mdns.get().cloned() else {
        return Vec::new();
    };
    let lookup = if hostname.ends_with('.') {
        hostname.to_string()
    } else {
        format!("{hostname}.")
    };

    let receiver = match mdns.resolve_hostname(&lookup, Some(HOSTNAME_RESOLVE_TIMEOUT_MS)) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("mDNS hostname resolve start failed for {lookup}: {e}");
            state
                .bump_discovery_failure("hostname_mdns_resolve_start_failed")
                .await;
            return Vec::new();
        }
    };

    let deadline = std::time::Instant::now()
        + Duration::from_millis(HOSTNAME_RESOLVE_TIMEOUT_MS.saturating_add(300));
    let mut addrs = Vec::new();
    while std::time::Instant::now() < deadline {
        let wait = deadline
            .saturating_duration_since(std::time::Instant::now())
            .min(Duration::from_millis(250));
        let event = tokio::task::spawn_blocking({
            let recv = receiver.clone();
            move || recv.recv_timeout(wait)
        })
        .await;

        match event {
            Ok(Ok(HostnameResolutionEvent::AddressesFound(_, found))) => {
                addrs.extend(
                    found
                        .into_iter()
                        .map(|ip| SocketAddr::new(ip, port))
                        .filter(is_usable_peer_addr),
                );
                if !addrs.is_empty() {
                    break;
                }
            }
            Ok(Ok(HostnameResolutionEvent::SearchTimeout(_)))
            | Ok(Ok(HostnameResolutionEvent::SearchStopped(_))) => break,
            Ok(Ok(_)) => {}
            Ok(Err(_)) => break,
            Err(e) => {
                tracing::warn!("mDNS hostname resolve join error for {lookup}: {e}");
                state
                    .bump_discovery_failure("hostname_mdns_resolve_join_error")
                    .await;
                break;
            }
        }
    }

    let _ = mdns.stop_resolve_hostname(&lookup);
    sort_dedupe_addrs(&mut addrs);
    if addrs.is_empty() {
        state
            .bump_discovery_failure("hostname_mdns_resolve_no_usable_addr")
            .await;
    }
    addrs
}

async fn resolve_hostname_with_system_dns(hostname: &str, port: u16) -> Vec<SocketAddr> {
    let resolve_target = format!("{}:{port}", hostname.trim_end_matches('.'));
    let mut addrs = Vec::new();
    if let Ok(Ok(extra)) = tokio::task::spawn_blocking(move || {
        resolve_target
            .to_socket_addrs()
            .map(|iter| iter.collect::<Vec<SocketAddr>>())
    })
    .await
    {
        addrs.extend(extra.into_iter().filter(is_usable_peer_addr));
    }
    sort_dedupe_addrs(&mut addrs);
    addrs
}

fn remove_session_from_state(
    remove_key: &str,
    index: &mut std::collections::HashMap<String, SessionIndexEntry>,
    devices: &mut std::collections::HashMap<String, DeviceInfo>,
) -> Option<(String, bool, Option<DeviceInfo>)> {
    let entry = index
        .get(remove_key)
        .cloned()
        .or_else(|| index.values().find(|e| e.session_id == remove_key).cloned())?;
    // Remove all lookup aliases for this session id so stale fullnames can't poison future removals.
    index.retain(|_, v| v.session_id != entry.session_id);
    let fp = entry.fingerprint.clone();
    if let Some(device) = devices.get_mut(&fp) {
        device.sessions.remove(&entry.session_id);
        if device.sessions.is_empty() {
            device.reachability = crate::state::ReachabilityStatus::OfflineCandidate;
        }
        Some((fp, false, Some(device.clone())))
    } else {
        Some((fp, false, None))
    }
}

pub fn should_mark_offline(device: &DeviceInfo, now_unix: u64) -> bool {
    device.sessions.is_empty()
        && device.reachability == crate::state::ReachabilityStatus::OfflineCandidate
        && now_unix.saturating_sub(device.last_seen) >= OFFLINE_GRACE_SECS
}

pub fn should_emit_device_lost(device: &DeviceInfo, now_unix: u64) -> bool {
    device.sessions.is_empty()
        && device.reachability == crate::state::ReachabilityStatus::Offline
        && now_unix.saturating_sub(device.last_seen) >= DEVICE_LOST_RETENTION_SECS
}

/// Start mDNS browsing for DashDrop peers.
pub async fn start_browser(
    mdns: Arc<ServiceDaemon>,
    app: AppHandle,
    state: Arc<AppState>,
) -> Result<()> {
    let receiver = mdns.browse(SERVICE_TYPE).context("mDNS browse")?;
    let own_fp = state.identity.fingerprint.clone();

    let app_events = app.clone();
    let state_events = state.clone();
    {
        let mut status = state.browser_status.write().await;
        status.active = true;
    }
    tokio::spawn(async move {
        let mut receiver = receiver;
        let mut join_error_backoff_ms: u64 = 100;
        loop {
            // mdns-sd channel receiver is Sync, use spawn_blocking to not block tokio
            let event = tokio::task::spawn_blocking({
                let recv = receiver.clone();
                move || recv.recv_timeout(std::time::Duration::from_secs(5))
            })
            .await;

            match event {
                Ok(Ok(ServiceEvent::SearchStarted(payload))) => {
                    *state_events.mdns_last_search_started.write().await = Some(payload.clone());
                    state_events.bump_discovery_event("search_started").await;
                    tracing::info!("mDNS browse started: {payload}");
                }
                Ok(Ok(ServiceEvent::ServiceResolved(info))) => {
                    join_error_backoff_ms = 100;
                    state_events.bump_discovery_event("service_resolved").await;
                    handle_resolved(&info, &own_fp, &app_events, &state_events).await;
                }
                Ok(Ok(ServiceEvent::ServiceRemoved(_service_type, fullname))) => {
                    join_error_backoff_ms = 100;
                    state_events.bump_discovery_event("service_removed").await;
                    handle_removed(&fullname, &app_events, &state_events).await;
                }
                Ok(Ok(_)) => {} // SearchStarted, other events
                Ok(Err(err)) => match browser_receive_action(&err) {
                    BrowserReceiveAction::Continue => {
                        // Timeout — normal idle cycle.
                    }
                    BrowserReceiveAction::Restart => {
                        state_events
                            .bump_discovery_failure("browser_channel_disconnected")
                            .await;
                        {
                            let mut status = state_events.browser_status.write().await;
                            status.active = false;
                            status.restart_count = status.restart_count.saturating_add(1);
                            status.last_disconnect_at = Some(now_unix_secs());
                        }
                        tokio::time::sleep(Duration::from_millis(join_error_backoff_ms)).await;
                        match mdns.browse(SERVICE_TYPE) {
                            Ok(next_receiver) => {
                                receiver = next_receiver;
                                join_error_backoff_ms = 100;
                                state_events.bump_discovery_event("browser_restarted").await;
                                let mut status = state_events.browser_status.write().await;
                                status.active = true;
                            }
                            Err(e) => {
                                state_events
                                    .bump_discovery_failure("browser_restart_failed")
                                    .await;
                                tracing::warn!("mDNS browser restart failed: {e}");
                                join_error_backoff_ms =
                                    (join_error_backoff_ms.saturating_mul(2)).min(5000);
                            }
                        }
                    }
                },
                Err(e) => {
                    tracing::warn!("mDNS browser task error: {e}");
                    state_events
                        .bump_discovery_failure("browser_spawn_blocking_join_error")
                        .await;
                    {
                        let mut status = state_events.browser_status.write().await;
                        status.active = false;
                    }
                    tokio::time::sleep(Duration::from_millis(join_error_backoff_ms)).await;
                    match mdns.browse(SERVICE_TYPE) {
                        Ok(next_receiver) => {
                            receiver = next_receiver;
                            join_error_backoff_ms = 100;
                            state_events.bump_discovery_event("browser_restarted").await;
                            let mut status = state_events.browser_status.write().await;
                            status.active = true;
                        }
                        Err(restart_err) => {
                            state_events
                                .bump_discovery_failure("browser_restart_failed")
                                .await;
                            tracing::warn!(
                                "mDNS browser restart after task error failed: {restart_err}"
                            );
                            join_error_backoff_ms =
                                (join_error_backoff_ms.saturating_mul(2)).min(5000);
                        }
                    }
                }
            }
        }
    });

    let app_for_offline = app.clone();
    let state_for_offline = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            interval.tick().await;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let mut updates = Vec::new();
            let mut stale_sessions_removed = 0u64;
            let mut pruned_session_pairs: Vec<(String, String)> = Vec::new();
            let lost = {
                let mut devices = state_for_offline.devices.write().await;
                for device in devices.values_mut() {
                    let stale_session_ids: Vec<String> = device
                        .sessions
                        .iter()
                        .filter(|(_, s)| is_session_stale(s.last_seen_instant.elapsed()))
                        .map(|(sid, _)| sid.clone())
                        .collect();
                    if !stale_session_ids.is_empty() {
                        for sid in stale_session_ids {
                            if device.sessions.remove(&sid).is_some() {
                                stale_sessions_removed = stale_sessions_removed.saturating_add(1);
                                pruned_session_pairs
                                    .push((device.fingerprint.clone(), sid.clone()));
                            }
                        }
                        if device.sessions.is_empty() {
                            device.reachability =
                                crate::state::ReachabilityStatus::OfflineCandidate;
                        }
                        updates.push(DeviceView::from(&*device));
                    }
                    if should_mark_offline(device, now) {
                        device.reachability = crate::state::ReachabilityStatus::Offline;
                        updates.push(DeviceView::from(&*device));
                    }
                }
                let evicted: Vec<String> = devices
                    .iter()
                    .filter(|(_, device)| should_emit_device_lost(device, now))
                    .map(|(fp, _)| fp.clone())
                    .collect();
                for fp in &evicted {
                    devices.remove(fp);
                }
                evicted
            };
            if !pruned_session_pairs.is_empty() || !lost.is_empty() {
                let mut session_index = state_for_offline.session_index.write().await;
                session_index.retain(|_, e| {
                    !lost.contains(&e.fingerprint)
                        && !pruned_session_pairs
                            .iter()
                            .any(|(fp, sid)| e.fingerprint == *fp && e.session_id == *sid)
                });
            }
            for payload in updates {
                app_for_offline.emit("device_updated", payload).ok();
            }
            for fp in lost {
                app_for_offline
                    .emit("device_lost", serde_json::json!({ "fingerprint": fp }))
                    .ok();
            }
            if stale_sessions_removed > 0 {
                state_for_offline
                    .bump_discovery_event("stale_session_pruned")
                    .await;
            }
        }
    });

    Ok(())
}

async fn handle_resolved(
    info: &mdns_sd::ServiceInfo,
    own_fp: &str,
    app: &AppHandle,
    state: &Arc<AppState>,
) {
    let props = info.get_properties();

    let Some(fp_prop) = props.get("fp") else {
        tracing::debug!("mDNS peer missing fp, skipping");
        state
            .bump_discovery_failure("resolved_missing_fp_txt")
            .await;
        return;
    };
    let fp = fp_prop.val_str().to_string();
    if fp.is_empty() {
        state.bump_discovery_failure("resolved_empty_fp_txt").await;
        return;
    }
    if fp == own_fp {
        state.bump_discovery_event("resolved_self_filtered").await;
        return;
    }

    let session_id = props
        .get("id")
        .map(|v| v.val_str())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| info.get_fullname())
        .to_string();
    let service_fullname = info.get_fullname().to_string();

    let name = props
        .get("name")
        .map(|v| v.val_str())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| info.get_hostname())
        .to_string();

    let platform: Platform = props
        .get("platform")
        .map(|v| v.val_str())
        .map(Platform::from)
        .unwrap_or(Platform::Unknown);

    let port = info.get_port();

    let raw_addrs: Vec<SocketAddr> = info
        .get_addresses()
        .iter()
        .map(|ip| SocketAddr::new(*ip, port))
        .collect();
    let mut addrs: Vec<SocketAddr> = raw_addrs
        .iter()
        .copied()
        .filter(is_usable_peer_addr)
        .collect();

    if addrs.is_empty() {
        state.bump_discovery_event("resolved_empty_addr_set").await;
        addrs.extend(resolve_hostname_with_mdns(state, info.get_hostname(), port).await);
    }

    if addrs.is_empty() {
        addrs.extend(resolve_hostname_with_system_dns(info.get_hostname(), port).await);
        if !addrs.is_empty() {
            state
                .bump_discovery_event("resolved_addr_via_system_dns")
                .await;
        } else {
            state
                .bump_discovery_failure("resolved_no_usable_addrs")
                .await;
        }
    }

    // Prefer IPv4 first for cross-platform LAN interoperability.
    sort_dedupe_addrs(&mut addrs);

    if addrs.is_empty() {
        tracing::debug!(
            "No usable addresses resolved for {name} (hostname={}, port={})",
            info.get_hostname(),
            port
        );
        // Keep unresolved peers visible for diagnostics and later refreshes.
        state
            .bump_discovery_event("resolved_kept_without_addrs")
            .await;
    }
    let usable_addr_count = addrs.len() as u32;
    let raw_addr_count = raw_addrs.len() as u32;

    let trusted = state.is_trusted(&fp).await;
    let now_unix = now_unix_secs();

    let (is_new, device_model) = {
        let mut devices = state.devices.write().await;
        let is_new = !devices.contains_key(&fp);

        let device = devices.entry(fp.clone()).or_insert_with(|| DeviceInfo {
            fingerprint: fp.clone(),
            name: name.clone(),
            platform: platform.clone(),
            trusted,
            sessions: Default::default(),
            last_seen: 0,
            reachability: crate::state::ReachabilityStatus::Discovered,
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
        });

        device.sessions.insert(
            session_id.clone(),
            SessionInfo {
                session_id: session_id.clone(),
                addrs: addrs.clone(),
                last_seen_unix: now_unix,
                last_seen_instant: Instant::now(),
            },
        );
        device.trusted = trusted;
        device.last_seen = now_unix;
        device.last_resolve_raw_addr_count = raw_addr_count;
        device.last_resolve_usable_addr_count = usable_addr_count;
        device.last_resolve_hostname = Some(info.get_hostname().to_string());
        device.last_resolve_port = Some(port);
        device.last_resolve_at = Some(now_unix);
        if !device.sessions.is_empty()
            && matches!(
                device.reachability,
                crate::state::ReachabilityStatus::Offline
                    | crate::state::ReachabilityStatus::OfflineCandidate
            )
        {
            device.reachability = crate::state::ReachabilityStatus::Discovered;
        }
        (is_new, device.clone())
    };

    {
        let mut idx = state.session_index.write().await;
        let entry = SessionIndexEntry {
            fingerprint: fp.clone(),
            session_id: session_id.clone(),
        };
        // Keep both keys so ServiceRemoved(fullname) and session-id lookups resolve consistently.
        idx.insert(session_id.clone(), entry.clone());
        idx.insert(service_fullname, entry);
    }

    state.record_device_visibility(&fp).await;

    let payload = DeviceView::from(&device_model);

    if is_new {
        tracing::info!("Device discovered: {name} ({fp})");
        app.emit("device_discovered", &payload).ok();
    } else {
        tracing::debug!("Device updated: {name}");
        app.emit("device_updated", &payload).ok();
    }

    let probe_app = app.clone();
    let probe_state = state.clone();
    tokio::spawn(async move {
        if addrs.is_empty() {
            return;
        }
        run_probe_update(&probe_state, &probe_app, &fp, addrs).await;
    });
}

async fn handle_removed(remove_key: &str, app: &AppHandle, state: &Arc<AppState>) {
    let Some(entry) = ({
        let idx = state.session_index.read().await;
        idx.get(remove_key)
            .cloned()
            .or_else(|| idx.values().find(|e| e.session_id == remove_key).cloned())
    }) else {
        state
            .bump_discovery_failure("service_removed_without_index_entry")
            .await;
        return;
    };

    let observed_last_seen = {
        let devices = state.devices.read().await;
        devices
            .get(&entry.fingerprint)
            .and_then(|d| d.sessions.get(&entry.session_id))
            .map(|s| s.last_seen_unix)
    };
    let Some(observed_last_seen) = observed_last_seen else {
        state
            .bump_discovery_failure("service_removed_without_session")
            .await;
        return;
    };

    let pending_key = pending_remove_key(&entry.fingerprint, &entry.session_id);
    let should_schedule = {
        let mut pending = state.pending_removed_sessions.lock().await;
        pending.insert(pending_key.clone())
    };
    if !should_schedule {
        state
            .bump_discovery_event("service_removed_deferred_duplicate")
            .await;
        return;
    }

    state.bump_discovery_event("service_removed_deferred").await;
    let fp = entry.fingerprint.clone();
    let session_id = entry.session_id.clone();
    let app2 = app.clone();
    let state2 = state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(OFFLINE_GRACE_SECS)).await;
        let now_unix = now_unix_secs();
        let (removed, refreshed) = {
            let mut idx = state2.session_index.write().await;
            let mut devices = state2.devices.write().await;
            if let Some(device) = devices.get_mut(&fp) {
                let should_remove = match device.sessions.get(&session_id) {
                    Some(s) => {
                        should_apply_deferred_remove(s.last_seen_unix, observed_last_seen, now_unix)
                    }
                    None => false,
                };
                if should_remove {
                    (
                        remove_session_from_state(&session_id, &mut idx, &mut devices),
                        false,
                    )
                } else {
                    let refreshed = device
                        .sessions
                        .get(&session_id)
                        .map(|s| s.last_seen_unix > observed_last_seen)
                        .unwrap_or(false);
                    (None, refreshed)
                }
            } else {
                (None, false)
            }
        };

        if let Some((fp, device_gone, updated_device)) = removed {
            state2.bump_discovery_event("service_removed_applied").await;
            if device_gone {
                tracing::info!("Device offline: fp={fp}");
                app2.emit("device_lost", serde_json::json!({ "fingerprint": fp }))
                    .ok();
            } else if let Some(dev) = updated_device {
                let payload = DeviceView::from(&dev);
                app2.emit("device_updated", &payload).ok();
            }
            let mut pending = state2.pending_removed_sessions.lock().await;
            pending.remove(&pending_key);
            return;
        }

        if refreshed {
            state2
                .bump_discovery_event("service_removed_skipped_refreshed")
                .await;
        } else {
            state2
                .bump_discovery_event("service_removed_skipped_stale")
                .await;
        }
        let mut pending = state2.pending_removed_sessions.lock().await;
        pending.remove(&pending_key);
    });
}

pub(crate) async fn run_probe_update(
    state: &Arc<AppState>,
    app: &AppHandle,
    fp: &str,
    addrs: Vec<SocketAddr>,
) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let addrs = select_probe_addrs(addrs);
    let attempted_addrs = addrs
        .iter()
        .map(|addr| addr.to_string())
        .collect::<Vec<_>>();
    let mut ok = false;
    let mut success_addr: Option<String> = None;
    let mut last_error_reason: Option<String> = None;
    let mut last_error_detail: Option<String> = None;
    let mut last_error_addr: Option<String> = None;
    for addr in addrs {
        match crate::transport::probe::probe_addr(state, addr).await {
            Ok(()) => {
                ok = true;
                success_addr = Some(addr.to_string());
                break;
            }
            Err(e) => {
                let detail = format!("{e:#}");
                let (counter_key, reason) = classify_probe_error(&detail);
                state.bump_discovery_failure(counter_key).await;
                last_error_reason = Some(reason.to_string());
                last_error_detail = Some(detail);
                last_error_addr = Some(addr.to_string());
            }
        }
    }

    let payload = {
        let mut devices = state.devices.write().await;
        let Some(device) = devices.get_mut(fp) else {
            return;
        };
        device.last_probe_at = Some(now);
        if ok {
            device.reachability = crate::state::ReachabilityStatus::Reachable;
            device.probe_fail_count = 0;
            device.last_probe_result = Some("ok".to_string());
            device.last_probe_error = None;
            device.last_probe_error_detail = None;
            device.last_probe_addr = success_addr;
        } else {
            let next_fail_count = device.probe_fail_count.saturating_add(1);
            device.probe_fail_count = next_fail_count;
            if next_fail_count >= 3 {
                device.reachability = crate::state::ReachabilityStatus::OfflineCandidate;
            } else if matches!(
                device.reachability,
                crate::state::ReachabilityStatus::Offline
                    | crate::state::ReachabilityStatus::OfflineCandidate
            ) {
                device.reachability = crate::state::ReachabilityStatus::Discovered;
            }
            device.last_probe_result = Some("failed".to_string());
            device.last_probe_error = last_error_reason;
            device.last_probe_error_detail = last_error_detail;
            device.last_probe_addr = last_error_addr;
        }
        device.last_probe_attempted_addrs = attempted_addrs;
        DeviceView::from(&*device)
    };
    if !ok {
        state.bump_discovery_failure("probe_failed").await;
    }
    app.emit("device_updated", payload).ok();
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::net::SocketAddr;
    use std::str::FromStr;
    use std::time::{Duration, Instant};

    use crate::state::{DeviceInfo, Platform, SessionIndexEntry, SessionInfo};

    #[test]
    fn removing_last_session_removes_device() {
        let mut devices: HashMap<String, DeviceInfo> = HashMap::new();
        let mut idx: HashMap<String, SessionIndexEntry> = HashMap::new();
        let fp = "peer-fp".to_string();
        let session_id = "session-1".to_string();
        let fullname = "service._dashdrop._udp.local.".to_string();
        let alias_fullname = "service-alias._dashdrop._udp.local.".to_string();
        let mut sessions = HashMap::new();
        sessions.insert(
            session_id.clone(),
            SessionInfo {
                session_id: session_id.clone(),
                addrs: vec![SocketAddr::from_str("127.0.0.1:9001").unwrap()],
                last_seen_unix: 0,
                last_seen_instant: Instant::now(),
            },
        );
        devices.insert(
            fp.clone(),
            DeviceInfo {
                fingerprint: fp.clone(),
                name: "peer".to_string(),
                platform: Platform::Mac,
                trusted: false,
                sessions,
                last_seen: 0,
                reachability: crate::state::ReachabilityStatus::Discovered,
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
        let entry = SessionIndexEntry {
            fingerprint: fp.clone(),
            session_id: session_id.clone(),
        };
        idx.insert(fullname.clone(), entry.clone());
        idx.insert(alias_fullname.clone(), entry.clone());
        idx.insert(session_id.clone(), entry);

        let removed = super::remove_session_from_state(&fullname, &mut idx, &mut devices);
        assert!(removed.is_some());

        assert!(devices.contains_key(&fp));
        assert_eq!(
            devices.get(&fp).expect("device kept").reachability,
            crate::state::ReachabilityStatus::OfflineCandidate
        );
        assert!(!idx.contains_key(&fullname));
        assert!(!idx.contains_key(&alias_fullname));
        assert!(!idx.contains_key(&session_id));
    }

    #[test]
    fn deferred_remove_is_skipped_when_session_refreshed() {
        assert!(!super::should_apply_deferred_remove(101, 100, 140));
    }

    #[test]
    fn deferred_remove_requires_grace_window() {
        assert!(!super::should_apply_deferred_remove(100, 100, 114));
        assert!(super::should_apply_deferred_remove(100, 100, 115));
    }

    #[test]
    fn stale_session_threshold_is_applied() {
        assert!(!super::is_session_stale(Duration::from_secs(89)));
        assert!(super::is_session_stale(Duration::from_secs(90)));
    }

    #[test]
    fn link_local_ipv6_without_scope_is_not_filtered() {
        let addr = SocketAddr::from_str("[fe80::1]:9443").expect("valid addr");
        assert!(super::is_usable_peer_addr(&addr));
    }

    #[test]
    fn browser_timeout_keeps_loop_running() {
        let action = super::browser_receive_action(&flume::RecvTimeoutError::Timeout);
        assert!(matches!(action, super::BrowserReceiveAction::Continue));
    }

    #[test]
    fn browser_disconnect_requests_restart() {
        let action = super::browser_receive_action(&flume::RecvTimeoutError::Disconnected);
        assert!(matches!(action, super::BrowserReceiveAction::Restart));
    }

    #[test]
    fn pending_remove_key_is_stable_and_deduplicates() {
        let key = super::pending_remove_key("fp-a", "session-a");
        assert_eq!(key, "fp-a|session-a");
        let mut pending = HashSet::new();
        assert!(pending.insert(key.clone()));
        assert!(!pending.insert(key));
    }

    #[test]
    fn select_probe_addrs_prefers_connectable_candidates() {
        let input = vec![
            SocketAddr::from_str("[fe80::1]:9443").expect("addr"),
            SocketAddr::from_str("192.168.1.9:9443").expect("addr"),
        ];
        let selected = super::select_probe_addrs(input);
        assert_eq!(
            selected,
            vec![SocketAddr::from_str("192.168.1.9:9443").expect("addr")]
        );
    }

    #[test]
    fn select_probe_addrs_keeps_scope_less_link_local_when_only_choice() {
        let input = vec![SocketAddr::from_str("[fe80::1]:9443").expect("addr")];
        let selected = super::select_probe_addrs(input.clone());
        assert_eq!(selected, input);
    }

    #[test]
    fn offline_candidate_promotes_after_grace_period() {
        let device = DeviceInfo {
            fingerprint: "peer-fp".into(),
            name: "peer".into(),
            platform: Platform::Mac,
            trusted: false,
            sessions: HashMap::new(),
            last_seen: 100,
            reachability: crate::state::ReachabilityStatus::OfflineCandidate,
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
        };
        assert!(!super::should_mark_offline(&device, 114));
        assert!(super::should_mark_offline(&device, 115));
    }

    #[test]
    fn offline_device_evicted_after_retention() {
        let device = DeviceInfo {
            fingerprint: "peer-fp".into(),
            name: "peer".into(),
            platform: Platform::Mac,
            trusted: false,
            sessions: HashMap::new(),
            last_seen: 100,
            reachability: crate::state::ReachabilityStatus::Offline,
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
        };
        assert!(!super::should_emit_device_lost(&device, 144));
        assert!(super::should_emit_device_lost(&device, 145));
    }
}
