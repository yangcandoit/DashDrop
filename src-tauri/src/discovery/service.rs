use anyhow::{Context, Result};
use mdns_sd::{IfKind, ServiceDaemon, ServiceInfo};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::state::{AppState, Platform};

pub const SERVICE_TYPE: &str = "_dashdrop._udp.local.";

/// Register this device's DashDrop service via mDNS-SD.
pub async fn register_service(state: Arc<AppState>) -> Result<ServiceDaemon> {
    let mdns = ServiceDaemon::new().context("create mDNS daemon")?;
    register_on_daemon(&mdns, &state).await?;
    Ok(mdns)
}

/// Re-register service on existing daemon (e.g. after device name change).
pub async fn reregister_service(state: Arc<AppState>) -> Result<()> {
    let mdns = state
        .mdns
        .get()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("mDNS daemon not initialized"))?;

    let old_fullname = state.mdns_service_fullname.read().await.clone();
    if let Some(fullname) = old_fullname {
        if let Ok(receiver) = mdns.unregister(&fullname) {
            let _ = receiver.recv_timeout(std::time::Duration::from_millis(500));
        }
    }

    register_on_daemon(&mdns, &state).await?;
    Ok(())
}

async fn register_on_daemon(mdns: &ServiceDaemon, state: &Arc<AppState>) -> Result<()> {
    configure_mdns_interfaces(mdns, state).await?;

    let port = *state.local_port.read().await;
    let fp = state.identity.fingerprint.clone();
    let device_name = state.config.read().await.device_name.clone();
    let session_id = uuid::Uuid::new_v4().to_string();

    let mut properties = HashMap::new();
    properties.insert("id".to_string(), session_id.clone());
    properties.insert("name".to_string(), device_name.clone());
    properties.insert("fp".to_string(), fp.clone());
    properties.insert("platform".to_string(), Platform::current().to_string());
    properties.insert("caps".to_string(), "file".to_string());

    let instance_name = sanitize_mdns_instance_name(&device_name);
    let host_label = sanitize_mdns_host_label(&device_name, &fp);
    let fqdn = format!("{host_label}.local.");

    let service_info = ServiceInfo::new(
        SERVICE_TYPE,
        &instance_name,
        &fqdn,
        "", // empty addr = use all interfaces
        port,
        Some(properties),
    )
    .context("build ServiceInfo")?
    .enable_addr_auto();

    let fullname = service_info.get_fullname().to_string();

    mdns.register(service_info).context("mDNS register")?;
    *state.mdns_service_fullname.write().await = Some(fullname);

    tracing::info!("mDNS registered: instance={instance_name}, port={port}, fp={fp}");
    Ok(())
}

async fn configure_mdns_interfaces(mdns: &ServiceDaemon, state: &Arc<AppState>) -> Result<()> {
    let selected = select_preferred_mdns_interfaces();
    if selected.is_empty() {
        *state.mdns_interface_policy.write().await = "all".to_string();
        state.mdns_enabled_interfaces.write().await.clear();
        tracing::warn!("mDNS interface filter skipped (no preferred LAN interfaces found); using all interfaces");
        return Ok(());
    }

    mdns.disable_interface(IfKind::All)
        .context("disable all mDNS interfaces before selective enable")?;
    for ifname in &selected {
        mdns.enable_interface(ifname.as_str())
            .with_context(|| format!("enable mDNS interface {ifname}"))?;
    }
    *state.mdns_interface_policy.write().await = "filtered".to_string();
    *state.mdns_enabled_interfaces.write().await = selected.clone();
    tracing::info!("mDNS interface filter enabled: {}", selected.join(", "));
    Ok(())
}

#[derive(Default)]
struct InterfaceStats {
    is_loopback: bool,
    has_ipv4_lan: bool,
    has_ipv6_non_link_local: bool,
}

pub(crate) fn is_virtual_or_filtered_interface(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("utun")
        || n.starts_with("awdl")
        || n.starts_with("llw")
        || n.starts_with("lo")
        || n.contains("loopback")
        || n.starts_with("docker")
        || n.starts_with("veth")
        || n.starts_with("vmnet")
        || n.contains("vmware")
        || n.contains("virtualbox")
        || n.contains("vethernet")
        || n.contains("tailscale")
        || n.contains("zerotier")
        || n.contains("wireguard")
        || n.contains("npcap")
        || n.contains("bridge")
        || n.contains("tap")
        || n.contains("tun")
}

fn select_preferred_mdns_interfaces() -> Vec<String> {
    let Ok(ifaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };

    let mut by_name: BTreeMap<String, InterfaceStats> = BTreeMap::new();
    for iface in ifaces {
        let entry = by_name.entry(iface.name.clone()).or_default();
        entry.is_loopback = entry.is_loopback || iface.is_loopback();
        match iface.addr {
            if_addrs::IfAddr::V4(v4) => {
                let ip = v4.ip;
                if !ip.is_loopback() && !ip.is_link_local() {
                    entry.has_ipv4_lan = true;
                }
            }
            if_addrs::IfAddr::V6(v6) => {
                let ip = v6.ip;
                if !ip.is_loopback() && !ip.is_unicast_link_local() {
                    entry.has_ipv6_non_link_local = true;
                }
            }
        }
    }

    let mut preferred: Vec<String> = by_name
        .iter()
        .filter(|(name, stats)| {
            !stats.is_loopback
                && !is_virtual_or_filtered_interface(name)
                && (stats.has_ipv4_lan || stats.has_ipv6_non_link_local)
        })
        .map(|(name, _)| name.clone())
        .collect();

    if preferred.is_empty() {
        preferred = by_name
            .iter()
            .filter(|(name, stats)| !stats.is_loopback && !is_virtual_or_filtered_interface(name))
            .map(|(name, _)| name.clone())
            .collect();
    }

    preferred
}

fn sanitize_mdns_instance_name(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "DashDrop".into()
    } else {
        s
    }
}

fn sanitize_mdns_host_label(device_name: &str, fingerprint: &str) -> String {
    // Host labels must be DNS-safe. Include fp suffix to reduce collisions.
    let mut base = device_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    base = base.trim_matches('-').to_string();
    if base.is_empty() {
        base = "dashdrop".to_string();
    }

    let short_fp = fingerprint
        .chars()
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();
    let mut label = format!("{base}-{short_fp}");
    if label.len() > 63 {
        label.truncate(63);
        label = label.trim_matches('-').to_string();
    }
    if label.is_empty() {
        "dashdrop".to_string()
    } else {
        label
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn filters_virtual_interface_names() {
        assert!(super::is_virtual_or_filtered_interface("utun4"));
        assert!(super::is_virtual_or_filtered_interface("awdl0"));
        assert!(super::is_virtual_or_filtered_interface(
            "Loopback Pseudo-Interface 1"
        ));
        assert!(super::is_virtual_or_filtered_interface(
            "vEthernet (Default Switch)"
        ));
    }

    #[test]
    fn keeps_normal_lan_interface_names() {
        assert!(!super::is_virtual_or_filtered_interface("en0"));
        assert!(!super::is_virtual_or_filtered_interface("Ethernet"));
        assert!(!super::is_virtual_or_filtered_interface("以太网"));
    }
}
