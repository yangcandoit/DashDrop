use std::sync::Arc;
use tokio::time::{Duration, interval};
use crate::state::{AppState, ReachabilityStatus};
use crate::runtime::host::RuntimeHost;
use crate::discovery::SoftApProvider;

/// P2P Scheduler monitors devices that are visible over BLE but not reachable via mDNS.
/// It triggers fallback connection attempts (SoftAP) for trusted peers.
pub fn start(state: Arc<AppState>, _host: Arc<dyn RuntimeHost>) {
    let provider = SoftApProvider::None;
    #[cfg(target_os = "linux")]
    let provider = SoftApProvider::Linux(crate::discovery::linux_softap::LinuxSoftApManager);
    #[cfg(target_os = "windows")]
    let provider = SoftApProvider::Windows(crate::discovery::windows_softap::WindowsSoftApManager::new());

    tauri::async_runtime::spawn(async move {
        let mut ticker = interval(Duration::from_secs(10));
        let mut last_hotspot_trigger = std::time::Instant::now() - Duration::from_secs(300);

        loop {
            ticker.tick().await;
            
            let devices = {
                let d = state.devices.read().await;
                d.values().cloned().collect::<Vec<_>>()
            };

            let mut needs_hotspot = false;
            for device in devices {
                // Trigger condition: Trusted + Offline + Recently seen via BLE
                if device.trusted
                    && device.reachability == ReachabilityStatus::Offline
                    && device.last_seen > 0
                {
                    
                    let ble_age = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                        .saturating_sub(device.last_seen);

                    if ble_age < 60 && last_hotspot_trigger.elapsed() > Duration::from_secs(120) {
                        tracing::info!(
                            peer_fp = %device.fingerprint,
                            "Peer recently seen but offline. Activating SoftAP fallback..."
                        );
                        needs_hotspot = true;
                        break;
                    }
                }
            }

            if needs_hotspot {
                let suffix = &state.identity.fingerprint[..4];
                match provider.start_hotspot(suffix) {
                    Ok(creds) => {
                        tracing::info!("SoftAP Active: SSID={}", creds.ssid);
                        last_hotspot_trigger = std::time::Instant::now();
                        // TODO: Update BleAssistCapsule with this SSID info
                    }
                    Err(e) => {
                        tracing::warn!("Failed to start SoftAP: {e}");
                    }
                }
            } else if last_hotspot_trigger.elapsed() > Duration::from_secs(300) {
                // Auto-shutdown hotspot after 5 mins of no active need
                let _ = provider.stop_hotspot();
            }
        }
    });
}
