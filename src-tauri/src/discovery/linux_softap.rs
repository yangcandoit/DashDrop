#[cfg(target_os = "linux")]
use std::process::Command;
#[cfg(target_os = "linux")]
use anyhow::{anyhow, Result};

#[cfg(target_os = "linux")]
pub struct LinuxSoftApManager;

#[cfg(target_os = "linux")]
impl LinuxSoftApManager {
    /// Starts a temporary Wi-Fi hotspot using NetworkManager (nmcli).
    /// Returns the SSID and Password generated.
    pub fn start_hotspot(ssid_suffix: &str) -> Result<(String, String)> {
        let ssid = format!("DashDrop-{}", ssid_suffix);
        let password = crate::crypto::generate_random_password(12);

        // nmcli device wifi hotspot [ifname <ifname>] [con-name <name>] [ssid <SSID>] [band a|bg] [channel <channel>] [password <password>]
        let status = Command::new("nmcli")
            .args(&[
                "device", "wifi", "hotspot",
                "ssid", &ssid,
                "password", &password,
            ])
            .status()
            .map_err(|e| anyhow!("Failed to execute nmcli: {e}"))?;

        if !status.success() {
            return Err(anyhow!("nmcli hotspot creation failed with status: {status}"));
        }

        Ok((ssid, password))
    }

    pub fn stop_hotspot() -> Result<()> {
        // We look for connections named 'Hotspot' or similar created by nmcli
        let _ = Command::new("nmcli")
            .args(&["connection", "down", "Hotspot"])
            .status();
        Ok(())
    }

    pub fn join_wifi(ssid: &str, password: Option<&str>) -> Result<()> {
        let mut args = vec!["device", "wifi", "connect", ssid];
        if let Some(p) = password {
            args.push("password");
            args.push(p);
        }

        let status = Command::new("nmcli")
            .args(&args)
            .status()
            .map_err(|e| anyhow!("Failed to execute nmcli connect: {e}"))?;

        if !status.success() {
            return Err(anyhow!("nmcli wifi connect failed with status: {status}"));
        }

        Ok(())
    }
}
