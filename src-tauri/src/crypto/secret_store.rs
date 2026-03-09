use anyhow::Result;
#[cfg(any(target_os = "windows", target_os = "linux"))]
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};

#[cfg(target_os = "macos")]
pub fn load_private_key(service: &str, account: &str) -> Result<Option<Vec<u8>>> {
    match security_framework::passwords::get_generic_password(service, account) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) => {
            let msg = e.to_string().to_lowercase();
            if msg.contains("itemnotfound") || msg.contains("not found") {
                Ok(None)
            } else {
                Err(anyhow::anyhow!("read keychain item failed: {e}"))
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub fn save_private_key(service: &str, account: &str, key_der: &[u8]) -> Result<()> {
    // Replace existing value if present.
    let _ = security_framework::passwords::delete_generic_password(service, account);
    security_framework::passwords::set_generic_password(service, account, key_der)
        .map_err(|e| anyhow::anyhow!("write keychain item failed: {e}"))?;
    Ok(())
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
pub fn load_private_key(_service: &str, _account: &str) -> Result<Option<Vec<u8>>> {
    let entry = keyring::Entry::new(_service, _account)
        .map_err(|e| anyhow::anyhow!("open keyring entry failed: {e}"))?;
    match entry.get_password() {
        Ok(encoded) => {
            let bytes = BASE64
                .decode(encoded.as_bytes())
                .map_err(|e| anyhow::anyhow!("decode keyring payload failed: {e}"))?;
            Ok(Some(bytes))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(anyhow::anyhow!("read keyring entry failed: {e}")),
    }
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
pub fn save_private_key(_service: &str, _account: &str, _key_der: &[u8]) -> Result<()> {
    let entry = keyring::Entry::new(_service, _account)
        .map_err(|e| anyhow::anyhow!("open keyring entry failed: {e}"))?;
    let encoded = BASE64.encode(_key_der);
    entry
        .set_password(&encoded)
        .map_err(|e| anyhow::anyhow!("write keyring entry failed: {e}"))?;
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub fn load_private_key(_service: &str, _account: &str) -> Result<Option<Vec<u8>>> {
    Ok(None)
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub fn save_private_key(_service: &str, _account: &str, _key_der: &[u8]) -> Result<()> {
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
pub fn secure_store_available() -> bool {
    #[cfg(target_os = "macos")]
    {
        true
    }
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    {
        let entry = match keyring::Entry::new("com.dashdrop.availability", "probe") {
            Ok(entry) => entry,
            Err(e) => {
                tracing::warn!("secure store probe could not open keyring entry: {e}");
                return false;
            }
        };
        match entry.get_password() {
            Ok(_) | Err(keyring::Error::NoEntry) => true,
            Err(e) => {
                tracing::warn!("secure store probe failed: {e}");
                false
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub fn secure_store_available() -> bool {
    false
}
