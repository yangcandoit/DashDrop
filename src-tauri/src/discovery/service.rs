use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::collections::HashMap;
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

    let instance_name = sanitize_mdns_name(&device_name);

    let mut ips = Vec::new();
    if let Ok(interfaces) = if_addrs::get_if_addrs() {
        for iface in interfaces {
            if iface.is_loopback() {
                continue;
            }
            ips.push(iface.addr.ip());
        }
    }

    let ip_str = ips
        .iter()
        .map(|ip| ip.to_string())
        .collect::<Vec<_>>()
        .join(",");

    let fqdn = format!("{}.local.", instance_name);

    let service_info = ServiceInfo::new(
        SERVICE_TYPE,
        &instance_name,
        &fqdn,
        ip_str.as_str(),
        port,
        Some(properties),
    )
    .context("build ServiceInfo")?;

    let fullname = service_info.get_fullname().to_string();

    mdns.register(service_info).context("mDNS register")?;
    *state.mdns_service_fullname.write().await = Some(fullname);

    tracing::info!("mDNS registered: instance={instance_name}, port={port}, fp={fp}");
    Ok(())
}

fn sanitize_mdns_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if s.is_empty() {
        "DashDrop".into()
    } else {
        s
    }
}
