use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter};

use crate::state::{AppState, DeviceInfo, Platform, SessionIndexEntry, SessionInfo};
use super::service::SERVICE_TYPE;

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
        let snapshot = if now_empty { None } else { Some(device.clone()) };
        if now_empty {
            let removed = devices.remove(&fp);
            Some((fp, true, removed))
        } else {
            Some((fp, false, snapshot))
        }
    } else {
        Some((fp, false, None))
    }
}

/// Start mDNS browsing for DashDrop peers.
pub async fn start_browser(
    mdns: Arc<ServiceDaemon>,
    app: AppHandle,
    state: Arc<AppState>,
) -> Result<()> {
    let receiver = mdns.browse(SERVICE_TYPE).context("mDNS browse")?;
    let own_fp = state.identity.fingerprint.clone();

    tokio::spawn(async move {
        loop {
            // mdns-sd channel receiver is Sync, use spawn_blocking to not block tokio
            let event = tokio::task::spawn_blocking({
                let recv = receiver.clone();
                move || recv.recv_timeout(std::time::Duration::from_secs(5))
            }).await;

            match event {
                Ok(Ok(ServiceEvent::ServiceResolved(info))) => {
                    handle_resolved(&info, &own_fp, &app, &state).await;
                }
                Ok(Ok(ServiceEvent::ServiceRemoved(_service_type, fullname))) => {
                    handle_removed(&fullname, &app, &state).await;
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

    let session_id = props.get("id")
        .map(|v| v.val_str())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| info.get_fullname())
        .to_string();
    let service_fullname = info.get_fullname().to_string();

    let name = props.get("name")
        .map(|v| v.val_str())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| info.get_hostname())
        .to_string();

    let platform: Platform = props.get("platform")
        .map(|v| v.val_str())
        .map(Platform::from)
        .unwrap_or(Platform::Unknown);

    let port = info.get_port();

    let addrs: Vec<SocketAddr> = info.get_addresses().iter().map(|ip| {
        SocketAddr::new(*ip, port)
    }).collect();

    if addrs.is_empty() {
        tracing::debug!("No addresses resolved for {name}");
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
        });

        device.sessions.insert(session_id.clone(), SessionInfo {
            session_id: session_id.clone(),
            addrs: addrs.clone(),
            last_seen: Instant::now(),
        });
        device.trusted = state.is_trusted(&fp).await;
        device.last_seen = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
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

    let payload = serde_json::to_value(&device_model).unwrap_or_default();

    if is_new {
        tracing::info!("Device discovered: {name} ({fp})");
        app.emit("device_discovered", &payload).ok();
    } else {
        tracing::debug!("Device updated: {name}");
        app.emit("device_updated", &payload).ok();
    }
}

async fn handle_removed(
    remove_key: &str,
    app: &AppHandle,
    state: &Arc<AppState>,
) {
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
        app.emit("device_lost", serde_json::json!({ "fingerprint": fp })).ok();
    } else if let Some(dev) = updated_device {
        let payload = serde_json::to_value(&dev).unwrap_or_default();
        app.emit("device_updated", &payload).ok();
    }
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
                last_seen: Instant::now(),
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

        assert!(!devices.contains_key(&fp));
        assert!(!idx.contains_key(&fullname));
        assert!(!idx.contains_key(&session_id));
    }
}
