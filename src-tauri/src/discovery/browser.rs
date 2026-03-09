use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::collections::HashSet;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter};

use super::service::SERVICE_TYPE;
use crate::dto::DeviceView;
use crate::state::{AppState, DeviceInfo, Platform, SessionIndexEntry, SessionInfo};

const OFFLINE_GRACE_SECS: u64 = 15;
const DEVICE_LOST_RETENTION_SECS: u64 = 45;

fn is_usable_peer_addr(addr: &SocketAddr) -> bool {
    if addr.ip().is_loopback() || addr.ip().is_unspecified() || addr.ip().is_multicast() {
        return false;
    }
    match addr {
        SocketAddr::V4(v4) => !v4.ip().is_link_local(),
        // Link-local IPv6 without scope id is typically not routable to peer.
        SocketAddr::V6(v6) => !(v6.ip().is_unicast_link_local() && v6.scope_id() == 0),
    }
}

fn remove_session_from_state(
    remove_key: &str,
    index: &mut std::collections::HashMap<String, SessionIndexEntry>,
    devices: &mut std::collections::HashMap<String, DeviceInfo>,
) -> Option<(String, bool, Option<DeviceInfo>)> {
    let entry = index.remove(remove_key)?;
    index.remove(&entry.session_id);
    let fp = entry.fingerprint.clone();
    if let Some(device) = devices.get_mut(&fp) {
        device.sessions.remove(&entry.session_id);
        let now_empty = device.sessions.is_empty();
        if now_empty {
            device.reachability = crate::state::ReachabilityStatus::OfflineCandidate;
            Some((fp, false, Some(device.clone())))
        } else {
            Some((fp, false, Some(device.clone())))
        }
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
    tokio::spawn(async move {
        loop {
            // mdns-sd channel receiver is Sync, use spawn_blocking to not block tokio
            let event = tokio::task::spawn_blocking({
                let recv = receiver.clone();
                move || recv.recv_timeout(std::time::Duration::from_secs(5))
            })
            .await;

            match event {
                Ok(Ok(ServiceEvent::ServiceResolved(info))) => {
                    handle_resolved(&info, &own_fp, &app_events, &state_events).await;
                }
                Ok(Ok(ServiceEvent::ServiceRemoved(_service_type, fullname))) => {
                    handle_removed(&fullname, &app_events, &state_events).await;
                }
                Ok(Ok(_)) => {} // SearchStarted, other events
                Ok(Err(_)) => {
                    // Timeout or channel closed — just loop
                }
                Err(e) => {
                    tracing::warn!("mDNS browser task error: {e}");
                    break;
                }
            }
        }
        tracing::info!("mDNS browser stopped");
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
            let lost = {
                let mut devices = state_for_offline.devices.write().await;
                for device in devices.values_mut() {
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
            for payload in updates {
                app_for_offline.emit("device_updated", payload).ok();
            }
            for fp in lost {
                app_for_offline
                    .emit("device_lost", serde_json::json!({ "fingerprint": fp }))
                    .ok();
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

    let fp = match props.get("fp") {
        Some(v) => v.val_str().to_string(),
        None => {
            tracing::debug!("mDNS peer missing fp, skipping");
            return;
        }
    };

    if fp.is_empty() || fp == own_fp {
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

    let mut addrs: Vec<SocketAddr> = info
        .get_addresses()
        .iter()
        .map(|ip| SocketAddr::new(*ip, port))
        .filter(is_usable_peer_addr)
        .collect();

    if addrs.is_empty() {
        let host = info.get_hostname().trim_end_matches('.').to_string();
        let resolve_target = format!("{host}:{port}");
        if let Ok(resolved) = tokio::task::spawn_blocking(move || {
            resolve_target
                .to_socket_addrs()
                .map(|iter| iter.collect::<Vec<SocketAddr>>())
        })
        .await
        {
            if let Ok(extra) = resolved {
                addrs.extend(extra.into_iter().filter(is_usable_peer_addr));
            }
        }
    }

    // Prefer IPv4 first for cross-platform LAN interoperability.
    addrs.sort_by_key(|a| if a.is_ipv4() { 0 } else { 1 });
    let mut seen = HashSet::new();
    addrs.retain(|a| seen.insert(*a));

    if addrs.is_empty() {
        tracing::debug!(
            "No usable addresses resolved for {name} (hostname={}, port={})",
            info.get_hostname(),
            port
        );
        return;
    }

    let trusted = state.is_trusted(&fp).await;

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
        });

        device.sessions.insert(
            session_id.clone(),
            SessionInfo {
                session_id: session_id.clone(),
                addrs: addrs.clone(),
                last_seen_unix: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                last_seen_instant: Instant::now(),
            },
        );
        device.trusted = trusted;
        device.last_seen = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
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
        run_probe_update(&probe_state, &probe_app, &fp, addrs).await;
    });
}

async fn handle_removed(remove_key: &str, app: &AppHandle, state: &Arc<AppState>) {
    let (fp, device_gone, updated_device) = {
        let mut idx = state.session_index.write().await;
        let mut devices = state.devices.write().await;
        match remove_session_from_state(remove_key, &mut idx, &mut devices) {
            Some((fp, gone, dev)) => (fp, gone, dev),
            None => return,
        }
    };

    if device_gone {
        tracing::info!("Device offline: fp={fp}");
        app.emit("device_lost", serde_json::json!({ "fingerprint": fp }))
            .ok();
    } else if let Some(dev) = updated_device {
        let payload = DeviceView::from(&dev);
        app.emit("device_updated", &payload).ok();
    }
}

async fn run_probe_update(
    state: &Arc<AppState>,
    app: &AppHandle,
    fp: &str,
    addrs: Vec<SocketAddr>,
) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut ok = false;
    for addr in addrs {
        if crate::transport::probe::probe_addr(state, addr)
            .await
            .is_ok()
        {
            ok = true;
            break;
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
        } else {
            device.probe_fail_count = device.probe_fail_count.saturating_add(1);
            device.reachability = crate::state::ReachabilityStatus::OfflineCandidate;
        }
        DeviceView::from(&*device)
    };
    app.emit("device_updated", payload).ok();
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::str::FromStr;
    use std::time::Instant;

    use crate::state::{DeviceInfo, Platform, SessionIndexEntry, SessionInfo};

    #[test]
    fn removing_last_session_removes_device() {
        let mut devices: HashMap<String, DeviceInfo> = HashMap::new();
        let mut idx: HashMap<String, SessionIndexEntry> = HashMap::new();
        let fp = "peer-fp".to_string();
        let session_id = "session-1".to_string();
        let fullname = "service._dashdrop._udp.local.".to_string();
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
            },
        );
        let entry = SessionIndexEntry {
            fingerprint: fp.clone(),
            session_id: session_id.clone(),
        };
        idx.insert(fullname.clone(), entry.clone());
        idx.insert(session_id.clone(), entry);

        let removed = super::remove_session_from_state(&fullname, &mut idx, &mut devices);
        assert!(removed.is_some());

        assert!(devices.contains_key(&fp));
        assert_eq!(
            devices.get(&fp).expect("device kept").reachability,
            crate::state::ReachabilityStatus::OfflineCandidate
        );
        assert!(!idx.contains_key(&fullname));
        assert!(!idx.contains_key(&session_id));
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
        };
        assert!(!super::should_emit_device_lost(&device, 144));
        assert!(super::should_emit_device_lost(&device, 145));
    }
}
