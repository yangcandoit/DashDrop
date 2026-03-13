pub mod beacon;
pub mod browser;
pub mod service;
pub mod p2p_scheduler;
pub mod linux_softap;
pub mod windows_softap;

pub use beacon::start_beacon;
pub use browser::start_browser;
pub use service::register_service;
pub use p2p_scheduler::start as start_p2p_scheduler;

#[derive(Debug, Clone)]
pub struct HotspotCredentials {
    pub ssid: String,
    pub password: Option<String>,
}

pub enum SoftApProvider {
    #[cfg(target_os = "linux")]
    Linux(linux_softap::LinuxSoftApManager),
    #[cfg(target_os = "windows")]
    Windows(windows_softap::WindowsSoftApManager),
    None,
}

impl SoftApProvider {
    pub fn start_hotspot(&self, suffix: &str) -> anyhow::Result<HotspotCredentials> {
        let _ = suffix;
        match self {
            #[cfg(target_os = "linux")]
            Self::Linux(_) => {
                let (ssid, password) = linux_softap::LinuxSoftApManager::start_hotspot(suffix)?;
                Ok(HotspotCredentials { ssid, password: Some(password) })
            }
            #[cfg(target_os = "windows")]
            Self::Windows(m) => {
                m.start_p2p_group()?;
                Ok(HotspotCredentials { 
                    ssid: format!("DashDrop-{}", suffix), 
                    password: None 
                })
            }
            _ => anyhow::bail!("SoftAP not supported on this platform"),
        }
    }

    pub fn join_hotspot(&self, ssid: &str, password: Option<&str>) -> anyhow::Result<()> {
        let _ = ssid;
        let _ = password;
        match self {
            #[cfg(target_os = "linux")]
            Self::Linux(_) => linux_softap::LinuxSoftApManager::join_wifi(ssid, password),
            #[cfg(target_os = "windows")]
            Self::Windows(m) => m.join_wifi(ssid, password),
            _ => anyhow::bail!("WiFi joining not supported on this platform"),
        }
    }

    pub fn stop_hotspot(&self) -> anyhow::Result<()> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Linux(_) => linux_softap::LinuxSoftApManager::stop_hotspot(),
            #[cfg(target_os = "windows")]
            Self::Windows(m) => m.stop_p2p_group(),
            _ => Ok(()),
        }
    }
}
