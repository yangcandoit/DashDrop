#[cfg(target_os = "windows")]
use windows::Devices::WiFiDirect::{
    WiFiDirectAdvertisementListenState, WiFiDirectAdvertisementPublisher,
    WiFiDirectAdvertisementPublisherStatus,
};
#[cfg(target_os = "windows")]
use anyhow::{anyhow, Result};
#[cfg(target_os = "windows")]
use std::sync::{Arc, Mutex};

#[cfg(target_os = "windows")]
pub struct WindowsSoftApManager {
    publisher: Arc<Mutex<Option<WiFiDirectAdvertisementPublisher>>>,
}

#[cfg(target_os = "windows")]
impl Default for WindowsSoftApManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "windows")]
impl WindowsSoftApManager {
    pub fn new() -> Self {
        Self {
            publisher: Arc::new(Mutex::new(None)),
        }
    }

    /// Starts a Wi-Fi Direct advertisement.
    pub fn start_p2p_group(&self) -> Result<()> {
        let mut pub_lock = self.publisher.lock().unwrap();
        if pub_lock.is_some() {
            return Ok(());
        }

        let publisher = WiFiDirectAdvertisementPublisher::new()
            .map_err(|e| anyhow!("Failed to create WFD publisher: {e}"))?;

        let advertisement = publisher
            .Advertisement()
            .map_err(|e| anyhow!("Failed to get advertisement: {e}"))?;

        // Make the device discoverable for P2P connections
        advertisement
            .SetListenState(WiFiDirectAdvertisementListenState::Discoverable)
            .map_err(|e| anyhow!("Failed to set listen state: {e}"))?;

        publisher
            .Start()
            .map_err(|e| anyhow!("Failed to start WFD publisher: {e}"))?;

        *pub_lock = Some(publisher);
        tracing::info!("Windows Wi-Fi Direct advertisement started.");
        Ok(())
    }

    pub fn stop_p2p_group(&self) -> Result<()> {
        let mut pub_lock = self.publisher.lock().unwrap();
        if let Some(publisher) = pub_lock.take() {
            publisher.Stop().map_err(|e| anyhow!("Failed to stop WFD publisher: {e}"))?;
            tracing::info!("Windows Wi-Fi Direct advertisement stopped.");
        }
        Ok(())
    }

    pub fn status(&self) -> String {
        let pub_lock = self.publisher.lock().unwrap();
        if let Some(publisher) = pub_lock.as_ref() {
            match publisher.Status().unwrap_or(WiFiDirectAdvertisementPublisherStatus::Aborted) {
                WiFiDirectAdvertisementPublisherStatus::Started => "started".to_string(),
                WiFiDirectAdvertisementPublisherStatus::Stopped => "stopped".to_string(),
                WiFiDirectAdvertisementPublisherStatus::Aborted => "aborted".to_string(),
                _ => "unknown".to_string(),
            }
        } else {
            "idle".to_string()
        }
    }

    pub fn join_wifi(&self, ssid: &str, password: Option<&str>) -> Result<()> {
        use std::process::Command;
        
        // Windows 'netsh' connection requires a pre-defined profile or a simple add.
        // For ad-hoc connection with password, we use a simpler approach if possible
        // but netsh typically requires a profile XML.
        // Simplified: we try to connect if it's already known, or we'd need to generate XML.
        // For now, we'll try the direct connect command.
        
        let mut cmd = Command::new("netsh");
        cmd.args(&["wlan", "connect", &format!("name={}", ssid)]);
        
        if let Some(p) = password {
            // This part usually requires a profile. We'll log a warning.
            tracing::warn!("Windows Wi-Fi join with password via netsh requires a profile. Password ignored.");
        }

        let status = cmd.status().map_err(|e| anyhow!("Failed to execute netsh: {e}"))?;
        if !status.success() {
            return Err(anyhow!("netsh wlan connect failed"));
        }
        Ok(())
    }
}
