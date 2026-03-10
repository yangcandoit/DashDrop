use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter};

use crate::dto::DeviceView;
use crate::state::{AppState, DeviceInfo, Platform, SessionIndexEntry, SessionInfo};

pub const DISCOVERY_BEACON_PORT: u16 = 53318;
const BEACON_INTERVAL_SECS: u64 = 3;
const BEACON_KIND: &str = "dashdrop_beacon_v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BeaconPacket {
    kind: String,
    fp: String,
    name: String,
    platform: String,
    port: u16,
    instance_id: String,
    ts: u64,
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn is_usable_peer_addr(addr: &SocketAddr) -> bool {
    if addr.ip().is_loopback() || addr.ip().is_unspecified() || addr.ip().is_multicast() {
        return false;
    }
    true
}

fn local_broadcast_targets() -> Vec<SocketAddr> {
    let mut targets = Vec::new();
    targets.push(SocketAddr::V4(SocketAddrV4::new(
        Ipv4Addr::BROADCAST,
        DISCOVERY_BEACON_PORT,
    )));

    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for iface in ifaces {
            if iface.is_loopback()
                || crate::discovery::service::is_virtual_or_filtered_interface(&iface.name)
            {
                continue;
            }
            if let if_addrs::IfAddr::V4(v4) = iface.addr {
                if let Some(broadcast) = v4.broadcast {
                    let target = SocketAddr::V4(SocketAddrV4::new(broadcast, DISCOVERY_BEACON_PORT));
                    targets.push(target);
                }
            }
        }
    }

    let mut seen = HashSet::new();
    targets.retain(|addr| seen.insert(*addr));
    targets
}

fn build_beacon_packet(state: &Arc<AppState>, instance_id: &str, local_port: u16) -> BeaconPacket {
    BeaconPacket {
        kind: BEACON_KIND.to_string(),
        fp: state.identity.fingerprint.clone(),
        name: state.identity.device_name.clone(),
        platform: Platform::current().to_string(),
        port: local_port,
        instance_id: instance_id.to_string(),
        ts: now_unix_secs(),
    }
}

pub async fn start_beacon(app: AppHandle, state: Arc<AppState>) -> Result<()> {
    let listen_socket = tokio::net::UdpSocket::bind(("0.0.0.0", DISCOVERY_BEACON_PORT))
        .await
        .context("bind discovery beacon listener")?;
    let send_socket = tokio::net::UdpSocket::bind(("0.0.0.0", 0))
        .await
        .context("bind discovery beacon sender")?;
    send_socket
        .set_broadcast(true)
        .context("enable UDP broadcast for discovery beacon")?;
    let instance_id = uuid::Uuid::new_v4().to_string();

    let send_state = state.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(BEACON_INTERVAL_SECS));
        loop {
            ticker.tick().await;
            let local_port = *send_state.local_port.read().await;
            if local_port == 0 {
                continue;
            }
            let packet = build_beacon_packet(&send_state, &instance_id, local_port);
            let payload = match serde_json::to_vec(&packet) {
                Ok(bytes) => bytes,
                Err(e) => {
                    tracing::warn!("beacon serialize failed: {e}");
                    send_state
                        .bump_discovery_failure("beacon_serialize_failed")
                        .await;
                    continue;
                }
            };

            for target in local_broadcast_targets() {
                if let Err(e) = send_socket.send_to(&payload, target).await {
                    tracing::debug!("beacon send failed to {target}: {e}");
                    send_state.bump_discovery_failure("beacon_send_failed").await;
                }
            }
            send_state.bump_discovery_event("beacon_sent").await;
        }
    });

    let recv_state = state.clone();
    let recv_app = app.clone();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 2048];
        loop {
            let (size, src) = match listen_socket.recv_from(&mut buf).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("beacon recv failed: {e}");
                    recv_state.bump_discovery_failure("beacon_recv_failed").await;
                    continue;
                }
            };

            let packet: BeaconPacket = match serde_json::from_slice(&buf[..size]) {
                Ok(pkt) => pkt,
                Err(_) => {
                    recv_state.bump_discovery_failure("beacon_parse_failed").await;
                    continue;
                }
            };

            if packet.kind != BEACON_KIND {
                continue;
            }
            if packet.fp == recv_state.identity.fingerprint {
                recv_state.bump_discovery_event("beacon_self_filtered").await;
                continue;
            }
            let candidate = SocketAddr::new(src.ip(), packet.port);
            if !is_usable_peer_addr(&candidate) {
                recv_state.bump_discovery_failure("beacon_unusable_addr").await;
                continue;
            }

            recv_state.bump_discovery_event("beacon_received").await;
            upsert_from_beacon(&recv_app, &recv_state, &packet, src.ip(), candidate).await;
        }
    });

    Ok(())
}

async fn upsert_from_beacon(
    app: &AppHandle,
    state: &Arc<AppState>,
    packet: &BeaconPacket,
    source_ip: IpAddr,
    candidate_addr: SocketAddr,
) {
    let fp = packet.fp.clone();
    let name = packet.name.clone();
    let platform = Platform::from(packet.platform.as_str());
    let trusted = state.is_trusted(&fp).await;
    let now_unix = now_unix_secs();
    let session_id = format!("beacon:{}", packet.instance_id);
    let addrs = vec![candidate_addr];

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
        device.last_resolve_raw_addr_count = 1;
        device.last_resolve_usable_addr_count = 1;
        device.last_resolve_hostname = Some(source_ip.to_string());
        device.last_resolve_port = Some(packet.port);
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
        idx.insert(
            session_id.clone(),
            SessionIndexEntry {
                fingerprint: fp.clone(),
                session_id: session_id.clone(),
            },
        );
    }

    let payload = DeviceView::from(&device_model);
    if is_new {
        app.emit("device_discovered", &payload).ok();
    } else {
        app.emit("device_updated", &payload).ok();
    }

    let probe_app = app.clone();
    let probe_state = state.clone();
    tokio::spawn(async move {
        crate::discovery::browser::run_probe_update(&probe_state, &probe_app, &fp, addrs).await;
    });
}
