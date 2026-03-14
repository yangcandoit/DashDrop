#[cfg(target_os = "windows")]
use serde::{Deserialize, Serialize};
#[cfg(target_os = "windows")]
use std::collections::HashMap;
#[cfg(target_os = "windows")]
use std::env;
#[cfg(target_os = "windows")]
use std::fs;
#[cfg(target_os = "windows")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "windows")]
use std::sync::{Arc, Mutex};
#[cfg(target_os = "windows")]
use std::thread;
#[cfg(target_os = "windows")]
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(target_os = "windows")]
use windows::Devices::Bluetooth::Advertisement::{
    BluetoothLEAdvertisementPublisher, BluetoothLEAdvertisementPublisherStatus,
    BluetoothLEAdvertisementReceivedEventArgs,
    BluetoothLEAdvertisementWatcher, BluetoothLEManufacturerData,
};
#[cfg(target_os = "windows")]
use windows::Storage::Streams::DataWriter;

#[cfg(target_os = "windows")]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BleAssistCapsule {
    version: u8,
    issued_at_unix_ms: u64,
    expires_at_unix_ms: u64,
    rolling_identifier: String,
    integrity_tag: String,
    transport_hint: String,
    qr_fallback_available: bool,
    short_code_fallback_available: bool,
    rotation_window_ms: u64,
}

#[cfg(target_os = "windows")]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdvertisementRequest {
    updated_at_unix_ms: u64,
    capsule: BleAssistCapsule,
}

#[cfg(target_os = "windows")]
#[derive(Debug, Clone, Serialize)]
struct BridgeSnapshot {
    permission_state: String,
    scanner_state: String,
    advertiser_state: String,
    advertised_rolling_identifier: Option<String>,
    capsules: Vec<BleAssistCapsule>,
}

#[cfg(target_os = "windows")]
struct CompactCodec;

#[cfg(target_os = "windows")]
impl CompactCodec {
    const COMPANY_ID: u16 = 0xFFFF;
    const ROLLING_BYTES: usize = 7;
    const INTEGRITY_BYTES: usize = 7;

    fn encode(capsule: &BleAssistCapsule) -> Option<Vec<u8>> {
        let rolling = decode_base64_url(&capsule.rolling_identifier)?;
        let integrity = decode_base64_url(&capsule.integrity_tag)?;
        if rolling.len() < Self::ROLLING_BYTES || integrity.len() < Self::INTEGRITY_BYTES {
            return None;
        }

        let issued_at_secs = (capsule.issued_at_unix_ms / 1000) as u32;
        let ttl_secs = ((capsule
            .expires_at_unix_ms
            .saturating_sub(capsule.issued_at_unix_ms))
            / 1000) as u16;
        let mut flags: u8 = 0;
        if capsule.qr_fallback_available {
            flags |= 0x01;
        }
        if capsule.short_code_fallback_available {
            flags |= 0x02;
        }

        let mut payload = Vec::with_capacity(2 + 1 + 1 + 4 + 2 + 7 + 7);
        // We omit COMPANY_ID here because WinRT's ManufacturerData adds it for us
        payload.push(capsule.version);
        payload.push(flags);
        payload.extend_from_slice(&issued_at_secs.to_le_bytes());
        payload.extend_from_slice(&ttl_secs.to_le_bytes());
        payload.extend_from_slice(&rolling[..Self::ROLLING_BYTES]);
        payload.extend_from_slice(&integrity[..Self::INTEGRITY_BYTES]);
        Some(payload)
    }

    fn decode(company_id: u16, data: &[u8]) -> Option<BleAssistCapsule> {
        if company_id != Self::COMPANY_ID || data.len() < 1 + 1 + 4 + 2 + 7 + 7 {
            return None;
        }
        let version = data[0];
        let flags = data[1];
        let issued_at_secs = u32::from_le_bytes(data[2..6].try_into().ok()?);
        let ttl_secs = u16::from_le_bytes(data[6..8].try_into().ok()?);
        let rolling_identifier = encode_base64_url(&data[8..15]);
        let integrity_tag = encode_base64_url(&data[15..22]);

        let issued_at_unix_ms = (issued_at_secs as u64) * 1000;
        let expires_at_unix_ms = issued_at_unix_ms + (ttl_secs as u64 * 1000);

        Some(BleAssistCapsule {
            version,
            issued_at_unix_ms,
            expires_at_unix_ms,
            rolling_identifier,
            integrity_tag,
            transport_hint: "ble_manufacturer_data".to_string(),
            qr_fallback_available: (flags & 0x01) != 0,
            short_code_fallback_available: (flags & 0x02) != 0,
            rotation_window_ms: 30_000,
        })
    }
}

#[cfg(target_os = "windows")]
fn decode_base64_url(s: &str) -> Option<Vec<u8>> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    URL_SAFE_NO_PAD.decode(s).ok()
}

#[cfg(target_os = "windows")]
fn encode_base64_url(data: &[u8]) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    URL_SAFE_NO_PAD.encode(data)
}

#[cfg(target_os = "windows")]
fn now_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(target_os = "windows")]
fn parse_flag(flag: &str) -> Option<String> {
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == flag {
            return args.next();
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn atomic_write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("create parent dir: {err}"))?;
    }
    let payload = serde_json::to_vec_pretty(value).map_err(|err| format!("serialize: {err}"))?;
    let temp_path = path.with_extension("tmp");
    fs::write(&temp_path, payload).map_err(|err| format!("write: {err}"))?;
    fs::rename(&temp_path, path).map_err(|err| format!("replace: {err}"))?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn run_windows() -> Result<(), String> {
    let snapshot_file = parse_flag("--snapshot-file").ok_or("missing --snapshot-file")?;
    let advertisement_file = parse_flag("--advertisement-file").ok_or("missing --advertisement-file")?;
    let snapshot_path = PathBuf::from(snapshot_file);
    let advertisement_path = PathBuf::from(advertisement_file);
    let _parent_pid = unsafe { windows_sys::Win32::System::Threading::GetCurrentProcessId() };

    let discovered_capsules = Arc::new(Mutex::new(HashMap::<String, BleAssistCapsule>::new()));
    let watcher = BluetoothLEAdvertisementWatcher::new().map_err(|e| format!("watcher init: {e}"))?;
    
    let capsules_clone = Arc::clone(&discovered_capsules);
    watcher.Received(&windows::Foundation::TypedEventHandler::<BluetoothLEAdvertisementWatcher, BluetoothLEAdvertisementReceivedEventArgs>::new(move |_, args| {
        if let Some(args) = args {
            let advertisement = args.Advertisement()?;
            let manufacturers = advertisement.ManufacturerData()?;
            for manufacturer in manufacturers {
                let company_id = manufacturer.CompanyId().unwrap_or(0);
                let Ok(data_buf) = manufacturer.Data() else { continue };
                let data_reader = windows::Storage::Streams::DataReader::FromBuffer(&data_buf).unwrap();
                let mut data = vec![0u8; data_reader.UnconsumedBufferLength().unwrap_or(0) as usize];
                let _ = data_reader.ReadBytes(&mut data);
                if let Some(capsule) = CompactCodec::decode(company_id, &data) {
                    if let Ok(mut map) = capsules_clone.lock() {
                        map.insert(capsule.rolling_identifier.clone(), capsule);
                    }
                }
            }
        }
        Ok(())
    })).map_err(|e| format!("watcher event: {e}"))?;
    watcher.Start().map_err(|e| format!("watcher start: {e}"))?;

    let publisher = BluetoothLEAdvertisementPublisher::new().map_err(|e| format!("publisher init: {e}"))?;
    let mut last_advertised_payload: Option<Vec<u8>> = None;
    let mut advertised_rolling_id: Option<String> = None;

    loop {
        // Parent PID monitoring on Windows is tricky, but we can check if the parent process still exists
        // Simple heuristic: if we can't open the parent process, it might be gone (depending on permissions).
        // For CLI tools, normally we just poll the advertisement file.
        
        let capsules = {
            let mut map = discovered_capsules.lock().expect("lock poisoned");
            let now = now_unix_millis();
            map.retain(|_, c| c.expires_at_unix_ms > now);
            let mut list: Vec<_> = map.values().cloned().collect();
            list.sort_by_key(|c| c.rolling_identifier.clone());
            list
        };

        let request_raw = fs::read_to_string(&advertisement_path).ok().unwrap_or_default();
        let request: Option<AdvertisementRequest> = serde_json::from_str(&request_raw).ok();
        
        let mut advertiser_state = "idle";
        if let Some(req) = request {
            let now = now_unix_millis();
            if req.capsule.expires_at_unix_ms > now {
                if let Some(payload) = CompactCodec::encode(&req.capsule) {
                    if Some(&payload) != last_advertised_payload.as_ref() {
                        let _ = publisher.Stop();
                        let advertisement = publisher.Advertisement().unwrap();
                        let _ = advertisement.ManufacturerData().map(|v| v.Clear());
                        let manufacturer_data = BluetoothLEManufacturerData::new().unwrap();
                        manufacturer_data.SetCompanyId(CompactCodec::COMPANY_ID).unwrap();
                        let writer = DataWriter::new().unwrap();
                        writer.WriteBytes(&payload).unwrap();
                        manufacturer_data.SetData(&writer.DetachBuffer().unwrap()).unwrap();
                        advertisement.ManufacturerData().unwrap().Append(&manufacturer_data).unwrap();
                        let _ = publisher.Start();
                        last_advertised_payload = Some(payload);
                        advertised_rolling_id = Some(req.capsule.rolling_identifier.clone());
                    }
                    advertiser_state = "advertising_capsule";
                } else {
                    advertiser_state = "encode_failed";
                }
            } else {
                let _ = publisher.Stop();
                last_advertised_payload = None;
                advertised_rolling_id = None;
                advertiser_state = "expired";
            }
        } else {
            let _ = publisher.Stop();
            last_advertised_payload = None;
            advertised_rolling_id = None;
        }

        let publisher_status = publisher.Status().unwrap_or(BluetoothLEAdvertisementPublisherStatus::Aborted);
        let status_str = match publisher_status {
            BluetoothLEAdvertisementPublisherStatus::Started => "started",
            BluetoothLEAdvertisementPublisherStatus::Waiting => "waiting",
            BluetoothLEAdvertisementPublisherStatus::Stopped => "stopped",
            BluetoothLEAdvertisementPublisherStatus::Aborted => "aborted",
            _ => "unknown",
        };

        let snapshot = BridgeSnapshot {
            permission_state: "granted".to_string(), // WinRT handles this via OS prompt if needed
            scanner_state: "scanning".to_string(),
            advertiser_state: format!("{}:{}", advertiser_state, status_str),
            advertised_rolling_identifier: advertised_rolling_id.clone(),
            capsules,
        };
        let _ = atomic_write_json(&snapshot_path, &snapshot);

        thread::sleep(Duration::from_secs(2));
    }
}

#[cfg(not(target_os = "windows"))]
fn run_windows() -> Result<(), String> {
    Err("Windows only".to_string())
}

fn main() {
    let _ = run_windows();
}
