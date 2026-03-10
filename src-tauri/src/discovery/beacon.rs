use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter};

use crate::dto::DeviceView;
use crate::state::{AppState, DeviceInfo, Platform, SessionIndexEntry, SessionInfo};

pub const DISCOVERY_BEACON_PORT: u16 = 53318;
const BEACON_INTERVAL_AC_SECS: u64 = 3;
const BEACON_INTERVAL_BATTERY_SECS: u64 = 6;
const BEACON_INTERVAL_LOW_POWER_SECS: u64 = 12;
const BEACON_KIND: &str = "dashdrop_beacon_v1";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PowerProfile {
    Ac,
    Battery,
    LowPower,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct BeaconCadence {
    pub power_profile: PowerProfile,
    pub interval_secs: u64,
}

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

pub(crate) fn parse_power_profile(raw: &str) -> Option<PowerProfile> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "ac" | "plugged" | "plugged_in" => Some(PowerProfile::Ac),
        "battery" | "discharging" => Some(PowerProfile::Battery),
        "low_power" | "low-power" | "low power" | "powersaver" | "power_saver" => {
            Some(PowerProfile::LowPower)
        }
        _ => None,
    }
}

pub(crate) fn beacon_interval_secs_for_profile(power_profile: PowerProfile) -> u64 {
    match power_profile {
        PowerProfile::Ac => BEACON_INTERVAL_AC_SECS,
        PowerProfile::Battery => BEACON_INTERVAL_BATTERY_SECS,
        PowerProfile::LowPower => BEACON_INTERVAL_LOW_POWER_SECS,
    }
}

pub fn current_beacon_cadence() -> BeaconCadence {
    let power_profile = current_power_profile();
    BeaconCadence {
        power_profile,
        interval_secs: beacon_interval_secs_for_profile(power_profile),
    }
}

fn current_power_profile() -> PowerProfile {
    if let Ok(raw) = std::env::var("DASHDROP_POWER_PROFILE") {
        if let Some(power_profile) = parse_power_profile(&raw) {
            return power_profile;
        }
    }

    detect_power_profile().unwrap_or(PowerProfile::Ac)
}

fn detect_power_profile() -> Option<PowerProfile> {
    #[cfg(target_os = "macos")]
    {
        return detect_macos_power_profile();
    }

    #[cfg(target_os = "linux")]
    {
        return detect_linux_power_profile();
    }

    #[cfg(target_os = "windows")]
    {
        return detect_windows_power_profile();
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn detect_macos_power_profile() -> Option<PowerProfile> {
    let batt = run_command_output("pmset", &["-g", "batt"]);
    let settings = run_command_output("pmset", &["-g"]);
    parse_macos_power_profile(batt.as_deref(), settings.as_deref())
}

#[cfg(target_os = "linux")]
fn detect_linux_power_profile() -> Option<PowerProfile> {
    if let Some(profile) = read_linux_powerprofilesctl() {
        return Some(profile);
    }
    parse_linux_power_profile("/sys/class/power_supply")
}

#[cfg(target_os = "windows")]
fn detect_windows_power_profile() -> Option<PowerProfile> {
    let scheme = run_command_output("powercfg", &["/getactivescheme"]);
    let battery = run_command_output(
        "WMIC",
        &["Path", "Win32_Battery", "Get", "BatteryStatus", "/value"],
    );
    parse_windows_power_profile(scheme.as_deref(), battery.as_deref())
}

fn run_command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

#[cfg(target_os = "macos")]
fn parse_macos_power_profile(
    batt_output: Option<&str>,
    settings_output: Option<&str>,
) -> Option<PowerProfile> {
    if settings_output.map(low_power_mode_enabled).unwrap_or(false) {
        return Some(PowerProfile::LowPower);
    }

    let batt = batt_output?.to_ascii_lowercase();
    if batt.contains("battery power") {
        Some(PowerProfile::Battery)
    } else if batt.contains("ac power") || batt.contains("charged") || batt.contains("charging") {
        Some(PowerProfile::Ac)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn low_power_mode_enabled(settings_output: &str) -> bool {
    settings_output.lines().any(|line| {
        let normalized = line.trim().to_ascii_lowercase();
        normalized == "lowpowermode 1"
            || normalized.ends_with("lowpowermode 1")
            || normalized.contains("low power mode: on")
            || normalized.contains("low power mode: 1")
    })
}

#[cfg(target_os = "linux")]
fn read_linux_powerprofilesctl() -> Option<PowerProfile> {
    let output = run_command_output("powerprofilesctl", &["get"])?;
    match output.trim().to_ascii_lowercase().as_str() {
        "power-saver" | "power saver" => Some(PowerProfile::LowPower),
        "balanced" | "performance" => None,
        _ => None,
    }
}

#[cfg(target_os = "linux")]
fn parse_linux_power_profile(base_path: &str) -> Option<PowerProfile> {
    let entries = fs::read_dir(base_path).ok()?;
    let mut on_ac = false;
    let mut battery_present = false;
    let mut battery_low = false;

    for entry in entries.flatten() {
        let path = entry.path();
        let power_type = read_trimmed(path.join("type"));
        match power_type.as_deref() {
            Some("Mains") | Some("USB") | Some("USB_C") => {
                if read_trimmed(path.join("online")).as_deref() == Some("1") {
                    on_ac = true;
                }
            }
            Some("Battery") => {
                battery_present = true;
                let status = read_trimmed(path.join("status"))
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let capacity =
                    read_trimmed(path.join("capacity")).and_then(|value| value.parse::<u8>().ok());
                if status.contains("discharging") {
                    if capacity.map(|value| value <= 20).unwrap_or(false) {
                        battery_low = true;
                    }
                }
            }
            _ => {}
        }
    }

    if battery_low {
        Some(PowerProfile::LowPower)
    } else if on_ac {
        Some(PowerProfile::Ac)
    } else if battery_present {
        Some(PowerProfile::Battery)
    } else {
        None
    }
}

#[cfg(target_os = "windows")]
fn parse_windows_power_profile(
    scheme_output: Option<&str>,
    battery_output: Option<&str>,
) -> Option<PowerProfile> {
    let scheme = scheme_output.unwrap_or_default().to_ascii_lowercase();
    if scheme.contains("power saver") || scheme.contains("battery saver") {
        return Some(PowerProfile::LowPower);
    }

    let battery = battery_output.unwrap_or_default().to_ascii_lowercase();
    if battery.contains("batterystatus=1") {
        Some(PowerProfile::Battery)
    } else if battery.contains("batterystatus=2")
        || battery.contains("batterystatus=6")
        || battery.contains("batterystatus=7")
        || battery.contains("batterystatus=8")
        || battery.contains("batterystatus=9")
    {
        Some(PowerProfile::Ac)
    } else {
        None
    }
}

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    Some(fs::read_to_string(path).ok()?.trim().to_string())
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
                    let target =
                        SocketAddr::V4(SocketAddrV4::new(broadcast, DISCOVERY_BEACON_PORT));
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
        let mut first_tick = true;
        loop {
            let cadence = current_beacon_cadence();
            if !first_tick {
                tokio::time::sleep(std::time::Duration::from_secs(cadence.interval_secs)).await;
            }
            first_tick = false;
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
                    send_state
                        .bump_discovery_failure("beacon_send_failed")
                        .await;
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
                    recv_state
                        .bump_discovery_failure("beacon_recv_failed")
                        .await;
                    continue;
                }
            };

            let packet: BeaconPacket = match serde_json::from_slice(&buf[..size]) {
                Ok(pkt) => pkt,
                Err(_) => {
                    recv_state
                        .bump_discovery_failure("beacon_parse_failed")
                        .await;
                    continue;
                }
            };

            if packet.kind != BEACON_KIND {
                continue;
            }
            if packet.fp == recv_state.identity.fingerprint {
                recv_state
                    .bump_discovery_event("beacon_self_filtered")
                    .await;
                continue;
            }
            let candidate = SocketAddr::new(src.ip(), packet.port);
            if !is_usable_peer_addr(&candidate) {
                recv_state
                    .bump_discovery_failure("beacon_unusable_addr")
                    .await;
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

#[cfg(test)]
mod tests {
    use super::{beacon_interval_secs_for_profile, parse_power_profile, PowerProfile};

    #[test]
    fn beacon_interval_selection_matches_power_profile() {
        assert_eq!(beacon_interval_secs_for_profile(PowerProfile::Ac), 3);
        assert_eq!(beacon_interval_secs_for_profile(PowerProfile::Battery), 6);
        assert_eq!(beacon_interval_secs_for_profile(PowerProfile::LowPower), 12);
    }

    #[test]
    fn power_profile_parser_accepts_backward_safe_aliases() {
        assert_eq!(parse_power_profile("ac"), Some(PowerProfile::Ac));
        assert_eq!(parse_power_profile("battery"), Some(PowerProfile::Battery));
        assert_eq!(
            parse_power_profile("low-power"),
            Some(PowerProfile::LowPower)
        );
        assert_eq!(parse_power_profile("unknown"), None);
    }
}
