use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use pkcs8::DecodePrivateKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::process::Command;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
#[cfg(target_os = "macos")]
use std::{fs, path::PathBuf};
#[cfg(target_os = "windows")]
use std::{fs, path::PathBuf};
#[cfg(target_os = "macos")]
use tauri::Manager;
#[cfg(target_os = "windows")]
use tauri::Manager;

use crate::runtime::host::RuntimeHost;

const BLE_ASSIST_CAPSULE_TTL_MS: u64 = 2 * 60 * 1000;
const BLE_ASSIST_ROTATION_WINDOW_MS: u64 = 30 * 1000;
const BLE_RUNTIME_MAINTENANCE_INTERVAL_SECS: u64 = 15;
#[cfg(target_os = "macos")]
const MACOS_BLE_PROBE_INTERVAL_SECS: u64 = 60;
#[cfg(target_os = "macos")]
const MACOS_BLE_BRIDGE_POLL_INTERVAL_SECS: u64 = 5;
#[cfg(target_os = "macos")]
const MACOS_BLE_ADVERTISEMENT_REFRESH_INTERVAL_SECS: u64 = 10;
#[cfg(target_os = "macos")]
const MACOS_BLE_BRIDGE_FILE_NAME: &str = "ble-assist-bridge.json";
#[cfg(target_os = "macos")]
const MACOS_BLE_ADVERTISEMENT_FILE_NAME: &str = "ble-assist-advertisement.json";
#[cfg(target_os = "macos")]
const MACOS_BLE_BRIDGE_SOURCE_FILE_NAME: &str = "BleAssistBridge.swift";
#[cfg(target_os = "macos")]
const MACOS_BLE_BRIDGE_BINARY_NAME: &str = "dashdrop-ble-bridge";
#[cfg(target_os = "windows")]
const WINDOWS_BLE_BRIDGE_FILE_NAME: &str = "ble-assist-bridge-win.json";
#[cfg(target_os = "windows")]
const WINDOWS_BLE_ADVERTISEMENT_FILE_NAME: &str = "ble-assist-advertisement-win.json";
#[cfg(target_os = "windows")]
const WINDOWS_BLE_BRIDGE_SOURCE_FILE_NAME: &str = "BleAssistBridge.ps1";
#[cfg(target_os = "windows")]
const WINDOWS_BLE_BRIDGE_BINARY_NAME: &str = "dashdrop-ble-bridge.exe";
#[cfg(target_os = "windows")]
const WINDOWS_BLE_BRIDGE_POLL_INTERVAL_SECS: u64 = 5;
#[cfg(target_os = "windows")]
const WINDOWS_BLE_ADVERTISEMENT_REFRESH_INTERVAL_SECS: u64 = 10;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BleAssistCapsule {
    pub version: u8,
    pub issued_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
    pub rolling_identifier: String,
    pub integrity_tag: String,
    pub transport_hint: String,
    pub qr_fallback_available: bool,
    pub short_code_fallback_available: bool,
    pub rotation_window_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
struct BleAssistCapsuleBody {
    version: u8,
    issued_at_unix_ms: u64,
    expires_at_unix_ms: u64,
    rolling_identifier: String,
    transport_hint: String,
    qr_fallback_available: bool,
    short_code_fallback_available: bool,
    rotation_window_ms: u64,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Deserialize)]
struct MacOsBleBridgeSnapshot {
    #[serde(default)]
    permission_state: Option<String>,
    #[serde(default)]
    scanner_state: Option<String>,
    #[serde(default)]
    advertiser_state: Option<String>,
    #[serde(default)]
    advertised_rolling_identifier: Option<String>,
    #[serde(default)]
    capsules: Vec<BleAssistCapsule>,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MacOsBleAdvertisementRequest {
    updated_at_unix_ms: u64,
    capsule: BleAssistCapsule,
}

#[cfg(target_os = "windows")]
#[derive(Debug, Clone, Deserialize)]
struct WindowsBleBridgeSnapshot {
    #[serde(default)]
    permission_state: Option<String>,
    #[serde(default)]
    scanner_state: Option<String>,
    #[serde(default)]
    advertiser_state: Option<String>,
    #[serde(default)]
    advertised_rolling_identifier: Option<String>,
    #[serde(default)]
    capsules: Vec<BleAssistCapsule>,
}

#[cfg(target_os = "windows")]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WindowsBleAdvertisementRequest {
    updated_at_unix_ms: u64,
    capsule: BleAssistCapsule,
}

fn now_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn normalize_fingerprint(fingerprint: &str) -> String {
    fingerprint
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_uppercase())
        .collect()
}

pub fn build_ble_assist_capsule(identity: &crate::crypto::Identity) -> Result<BleAssistCapsule> {
    let fingerprint = normalize_fingerprint(&identity.fingerprint);
    if fingerprint.is_empty() {
        return Err(anyhow!("Local BLE assist fingerprint is missing."));
    }

    let issued_at_unix_ms = now_unix_millis();
    let expires_at_unix_ms = issued_at_unix_ms.saturating_add(BLE_ASSIST_CAPSULE_TTL_MS);
    let rotation_slot = issued_at_unix_ms / BLE_ASSIST_ROTATION_WINDOW_MS;

    let mut hasher = Sha256::new();
    hasher.update(fingerprint.as_bytes());
    hasher.update(rotation_slot.to_be_bytes());
    hasher.update(&identity.cert_der);
    let rolling_identifier = URL_SAFE_NO_PAD.encode(&hasher.finalize()[..12]);

    let body = BleAssistCapsuleBody {
        version: 1,
        issued_at_unix_ms,
        expires_at_unix_ms,
        rolling_identifier: rolling_identifier.clone(),
        transport_hint: "qr_or_short_code_fallback".to_string(),
        qr_fallback_available: true,
        short_code_fallback_available: true,
        rotation_window_ms: BLE_ASSIST_ROTATION_WINDOW_MS,
    };
    let signing_key = SigningKey::from_pkcs8_der(&identity.key_der)
        .context("decode local identity key for BLE assist capsule")?;
    let canonical =
        serde_json::to_vec(&body).context("serialize BLE assist capsule body for signing")?;
    let signature = signing_key.sign(&canonical);
    let integrity_tag = URL_SAFE_NO_PAD.encode(&signature.to_bytes()[..12]);

    Ok(BleAssistCapsule {
        version: body.version,
        issued_at_unix_ms: body.issued_at_unix_ms,
        expires_at_unix_ms: body.expires_at_unix_ms,
        rolling_identifier: body.rolling_identifier,
        integrity_tag,
        transport_hint: body.transport_hint,
        qr_fallback_available: body.qr_fallback_available,
        short_code_fallback_available: body.short_code_fallback_available,
        rotation_window_ms: body.rotation_window_ms,
    })
}

pub fn validate_ble_assist_capsule(capsule: &BleAssistCapsule) -> Result<()> {
    let now = now_unix_millis();
    if capsule.version != 1 {
        return Err(anyhow!("Unsupported BLE assist capsule version."));
    }
    if capsule.rolling_identifier.trim().is_empty() {
        return Err(anyhow!("BLE assist capsule rolling identifier is missing."));
    }
    if capsule.integrity_tag.trim().is_empty() {
        return Err(anyhow!("BLE assist capsule integrity tag is missing."));
    }
    if capsule.expires_at_unix_ms <= now {
        return Err(anyhow!("BLE assist capsule expired."));
    }
    if capsule.expires_at_unix_ms <= capsule.issued_at_unix_ms {
        return Err(anyhow!("BLE assist capsule expiry is invalid."));
    }
    Ok(())
}

pub trait BleRuntimeProvider: Send + Sync {
    fn provider_name(&self) -> &'static str;
    fn start(
        &self,
        state: Arc<crate::state::AppState>,
        host: Arc<dyn RuntimeHost>,
        config_dir: &Path,
    );
}

struct NoopBleRuntimeProvider;

impl BleRuntimeProvider for NoopBleRuntimeProvider {
    fn provider_name(&self) -> &'static str {
        "noop"
    }

    fn start(
        &self,
        state: Arc<crate::state::AppState>,
        _host: Arc<dyn RuntimeHost>,
        _config_dir: &Path,
    ) {
        tauri::async_runtime::spawn(async move {
            state
                .mark_ble_runtime_started("noop", "idle_noop", "capsule_preview_only")
                .await;
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(
                BLE_RUNTIME_MAINTENANCE_INTERVAL_SECS,
            ));
            loop {
                interval.tick().await;
                let observed = state.ble_assist_observations_snapshot().await;
                let scanner_state = if observed.is_empty() {
                    "idle_noop"
                } else {
                    "observing_capsules"
                };
                state
                    .mark_ble_runtime_idle("noop", scanner_state, "capsule_preview_only")
                    .await;
            }
        });
    }
}

struct DisabledBleRuntimeProvider;

impl BleRuntimeProvider for DisabledBleRuntimeProvider {
    fn provider_name(&self) -> &'static str {
        "disabled"
    }

    fn start(
        &self,
        state: Arc<crate::state::AppState>,
        _host: Arc<dyn RuntimeHost>,
        _config_dir: &Path,
    ) {
        tauri::async_runtime::spawn(async move {
            state
                .mark_ble_runtime_started("disabled", "disabled", "disabled")
                .await;
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MacOsBluetoothProbe {
    hardware_present: bool,
    controller_powered: bool,
}

#[cfg(target_os = "macos")]
fn macos_bridge_file_path_from_env() -> Option<PathBuf> {
    std::env::var("DASHDROP_BLE_MACOS_BRIDGE_FILE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(target_os = "macos")]
fn resolve_macos_bridge_file_path(config_dir: &Path) -> PathBuf {
    macos_bridge_file_path_from_env().unwrap_or_else(|| config_dir.join(MACOS_BLE_BRIDGE_FILE_NAME))
}

#[cfg(target_os = "macos")]
fn resolve_macos_advertisement_file_path(config_dir: &Path) -> PathBuf {
    config_dir.join(MACOS_BLE_ADVERTISEMENT_FILE_NAME)
}

#[cfg(target_os = "macos")]
fn macos_bridge_binary_from_env() -> Option<PathBuf> {
    std::env::var("DASHDROP_BLE_MACOS_BRIDGE_BINARY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(target_os = "macos")]
fn macos_bridge_source_from_env() -> Option<PathBuf> {
    std::env::var("DASHDROP_BLE_MACOS_BRIDGE_SOURCE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(target_os = "macos")]
fn resolve_macos_bridge_source_path() -> Option<PathBuf> {
    let candidate = macos_bridge_source_from_env().unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("macos")
            .join(MACOS_BLE_BRIDGE_SOURCE_FILE_NAME)
    });
    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn resolve_macos_bridge_binary_path(host: &Arc<dyn RuntimeHost>) -> Option<PathBuf> {
    if let Some(path) = macos_bridge_binary_from_env().filter(|path| path.exists()) {
        return Some(path);
    }

    let binary_names = [
        MACOS_BLE_BRIDGE_BINARY_NAME.to_string(),
        format!(
            "{}-{}-apple-darwin",
            MACOS_BLE_BRIDGE_BINARY_NAME,
            std::env::consts::ARCH
        ),
    ];

    let mut candidates = Vec::new();
    if let Some(app) = host.app_handle() {
        if let Ok(resource_dir) = app.path().resource_dir() {
            for binary_name in &binary_names {
                candidates.push(resource_dir.join(binary_name));
                candidates.push(resource_dir.join("binaries").join(binary_name));
            }
        }
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for binary_name in &binary_names {
        candidates.push(manifest_dir.join("binaries").join(binary_name));
        candidates.push(manifest_dir.join("target").join("debug").join(binary_name));
        candidates.push(
            manifest_dir
                .join("target")
                .join("release")
                .join(binary_name),
        );
    }

    candidates.into_iter().find(|path| path.exists())
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct MacOsBleBridgeSpawnSpec {
    program: PathBuf,
    args: Vec<String>,
    bridge_mode: String,
}

#[cfg(target_os = "macos")]
fn build_macos_bridge_spawn_spec(
    host: &Arc<dyn RuntimeHost>,
    bridge_file_path: &Path,
    advertisement_file_path: &Path,
) -> Option<MacOsBleBridgeSpawnSpec> {
    if std::env::var("DASHDROP_BLE_MACOS_BRIDGE_DISABLE_SPAWN")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
    {
        return None;
    }

    let bridge_file = bridge_file_path.display().to_string();
    let advertisement_file = advertisement_file_path.display().to_string();
    if let Some(binary_path) = resolve_macos_bridge_binary_path(host) {
        return Some(MacOsBleBridgeSpawnSpec {
            program: binary_path,
            args: vec![
                "--snapshot-file".to_string(),
                bridge_file,
                "--advertisement-file".to_string(),
                advertisement_file,
            ],
            bridge_mode: "core_bluetooth_helper".to_string(),
        });
    }

    if tauri::is_dev() {
        if let Some(source_path) = resolve_macos_bridge_source_path() {
            return Some(MacOsBleBridgeSpawnSpec {
                program: PathBuf::from("xcrun"),
                args: vec![
                    "swift".to_string(),
                    source_path.display().to_string(),
                    "--snapshot-file".to_string(),
                    bridge_file,
                    "--advertisement-file".to_string(),
                    advertisement_file,
                ],
                bridge_mode: "core_bluetooth_swift_source".to_string(),
            });
        }
    }

    None
}

#[cfg(target_os = "macos")]
fn spawn_macos_bridge_helper(
    host: &Arc<dyn RuntimeHost>,
    bridge_file_path: &Path,
    advertisement_file_path: &Path,
) -> Result<Option<String>> {
    let Some(spec) = build_macos_bridge_spawn_spec(host, bridge_file_path, advertisement_file_path)
    else {
        return Ok(None);
    };

    if let Some(parent) = bridge_file_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create BLE bridge directory {}", parent.display()))?;
    }

    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    command
        .spawn()
        .with_context(|| format!("spawn macOS BLE bridge helper {}", spec.program.display()))?;

    Ok(Some(spec.bridge_mode))
}

fn parse_macos_system_profiler_output(output: &str) -> MacOsBluetoothProbe {
    let normalized = output.to_ascii_lowercase();
    let hardware_present = !normalized.contains("no bluetooth information found")
        && !normalized.contains("bluetooth: not available");
    let controller_powered = normalized.contains("state: on")
        || normalized.contains("powered: on")
        || normalized.contains("controller power state: on");
    MacOsBluetoothProbe {
        hardware_present,
        controller_powered,
    }
}

#[cfg(target_os = "macos")]
fn probe_macos_bluetooth() -> Result<MacOsBluetoothProbe> {
    let output = Command::new("system_profiler")
        .args(["SPBluetoothDataType"])
        .output()
        .context("run system_profiler SPBluetoothDataType")?;
    if !output.status.success() {
        return Err(anyhow!(
            "system_profiler exited with status {}",
            output.status
        ));
    }
    let stdout = String::from_utf8(output.stdout).context("decode system_profiler output")?;
    Ok(parse_macos_system_profiler_output(&stdout))
}

#[cfg(target_os = "macos")]
fn decode_macos_bridge_snapshot(raw: &str) -> Result<MacOsBleBridgeSnapshot> {
    serde_json::from_str::<MacOsBleBridgeSnapshot>(raw).context("decode BLE bridge snapshot JSON")
}

#[cfg(target_os = "macos")]
fn read_macos_bridge_snapshot(path: &PathBuf) -> Result<Option<(MacOsBleBridgeSnapshot, String)>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("read BLE bridge file {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let snapshot = decode_macos_bridge_snapshot(&raw)?;
    Ok(Some((snapshot, raw)))
}

#[cfg(target_os = "macos")]
fn write_macos_advertisement_request(
    path: &Path,
    identity: &crate::crypto::Identity,
) -> Result<BleAssistCapsule> {
    let capsule = build_ble_assist_capsule(identity)?;
    let request = MacOsBleAdvertisementRequest {
        updated_at_unix_ms: now_unix_millis(),
        capsule: capsule.clone(),
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create BLE advertisement directory {}", parent.display()))?;
    }
    let payload = serde_json::to_vec(&request).context("serialize BLE advertisement request")?;
    fs::write(path, payload)
        .with_context(|| format!("write BLE advertisement request {}", path.display()))?;
    Ok(capsule)
}

#[cfg(target_os = "macos")]
struct MacOsBleRuntimeProvider;

#[cfg(target_os = "macos")]
impl BleRuntimeProvider for MacOsBleRuntimeProvider {
    fn provider_name(&self) -> &'static str {
        "macos_native"
    }

    fn start(
        &self,
        state: Arc<crate::state::AppState>,
        host: Arc<dyn RuntimeHost>,
        config_dir: &Path,
    ) {
        let config_dir = config_dir.to_path_buf();
        tauri::async_runtime::spawn(async move {
            let bridge_file_path = resolve_macos_bridge_file_path(&config_dir);
            let advertisement_file_path = resolve_macos_advertisement_file_path(&config_dir);
            let bridge_mode =
                match spawn_macos_bridge_helper(&host, &bridge_file_path, &advertisement_file_path)
                {
                    Ok(mode) => mode,
                    Err(error) => {
                        let message = format!("macOS BLE bridge helper failed to start: {error:#}");
                        state
                            .mark_ble_runtime_error("macos_native", message.clone())
                            .await;
                        let _ = host.emit_json(
                            "system_error",
                            serde_json::json!({
                                "code": "BLE_MACOS_BRIDGE_START_FAILED",
                                "subsystem": "ble_baseline",
                                "message": message,
                            }),
                        );
                        None
                    }
                };
            let startup_scanner_state = if bridge_mode.is_some() {
                "bridge_bootstrapping"
            } else {
                "probing_hardware_scaffold"
            };
            state
                .mark_ble_runtime_started(
                    "macos_native",
                    startup_scanner_state,
                    "scaffold_unimplemented",
                )
                .await;
            state
                .update_ble_runtime_bridge(
                    None,
                    bridge_mode.or_else(|| {
                        macos_bridge_file_path_from_env()
                            .as_ref()
                            .map(|_| "json_file_bridge_external".to_string())
                    }),
                    Some(bridge_file_path.display().to_string()),
                    None,
                    None,
                )
                .await;
            state
                .update_ble_runtime_advertisement(
                    Some(advertisement_file_path.display().to_string()),
                    None,
                    None,
                )
                .await;
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(
                MACOS_BLE_BRIDGE_POLL_INTERVAL_SECS,
            ));
            let mut last_probe_started_at = 0_u64;
            let mut last_bridge_snapshot_hash: Option<String> = None;
            let mut bridge_snapshot_seen = false;
            let mut last_advertisement_refresh_started_at = 0_u64;
            loop {
                interval.tick().await;
                let now = now_unix_millis();

                if now.saturating_sub(last_advertisement_refresh_started_at)
                    >= MACOS_BLE_ADVERTISEMENT_REFRESH_INTERVAL_SECS.saturating_mul(1000)
                {
                    last_advertisement_refresh_started_at = now;
                    let identity = state.identity.clone();
                    let advertisement_file_path_for_worker = advertisement_file_path.clone();
                    let advertisement_file_path_display =
                        advertisement_file_path.display().to_string();
                    match tokio::task::spawn_blocking(move || {
                        write_macos_advertisement_request(
                            &advertisement_file_path_for_worker,
                            &identity,
                        )
                    })
                    .await
                    {
                        Ok(Ok(capsule)) => {
                            state
                                .update_ble_runtime_advertisement(
                                    Some(advertisement_file_path_display),
                                    Some(now),
                                    Some(capsule.rolling_identifier),
                                )
                                .await;
                        }
                        Ok(Err(error)) => {
                            let message =
                                format!("macOS BLE advertisement request write failed: {error:#}");
                            state
                                .mark_ble_runtime_error("macos_native", message.clone())
                                .await;
                            let _ = host.emit_json(
                                "system_error",
                                serde_json::json!({
                                    "code": "BLE_MACOS_ADVERTISEMENT_WRITE_FAILED",
                                    "subsystem": "ble_baseline",
                                    "message": message,
                                }),
                            );
                        }
                        Err(join_error) => {
                            let message =
                                format!("macOS BLE advertisement worker failed: {join_error}");
                            state
                                .mark_ble_runtime_error("macos_native", message.clone())
                                .await;
                            let _ = host.emit_json(
                                "system_error",
                                serde_json::json!({
                                    "code": "BLE_MACOS_ADVERTISEMENT_WRITE_JOIN_FAILED",
                                    "subsystem": "ble_baseline",
                                    "message": message,
                                }),
                            );
                        }
                    }
                }

                if !bridge_snapshot_seen
                    && now.saturating_sub(last_probe_started_at)
                        >= MACOS_BLE_PROBE_INTERVAL_SECS.saturating_mul(1000)
                {
                    last_probe_started_at = now;
                    match tokio::task::spawn_blocking(probe_macos_bluetooth).await {
                        Ok(Ok(probe)) => {
                            let scanner_state = if !probe.hardware_present {
                                "hardware_unavailable"
                            } else if probe.controller_powered {
                                "hardware_ready_scaffold"
                            } else {
                                "hardware_present_powered_off"
                            };
                            state
                                .mark_ble_runtime_idle(
                                    "macos_native",
                                    scanner_state,
                                    "scaffold_unimplemented",
                                )
                                .await;
                        }
                        Ok(Err(error)) => {
                            let message = format!("macOS BLE scaffold probe failed: {error:#}");
                            state
                                .mark_ble_runtime_error("macos_native", message.clone())
                                .await;
                            let _ = host.emit_json(
                                "system_error",
                                serde_json::json!({
                                    "code": "BLE_MACOS_PROBE_FAILED",
                                    "subsystem": "ble_baseline",
                                    "message": message,
                                }),
                            );
                        }
                        Err(join_error) => {
                            let message =
                                format!("macOS BLE scaffold probe worker failed: {join_error}");
                            state
                                .mark_ble_runtime_error("macos_native", message.clone())
                                .await;
                            let _ = host.emit_json(
                                "system_error",
                                serde_json::json!({
                                    "code": "BLE_MACOS_PROBE_JOIN_FAILED",
                                    "subsystem": "ble_baseline",
                                    "message": message,
                                }),
                            );
                        }
                    }
                }

                match tokio::task::spawn_blocking({
                    let path = bridge_file_path.clone();
                    move || read_macos_bridge_snapshot(&path)
                })
                .await
                {
                    Ok(Ok(Some((snapshot, raw)))) => {
                        bridge_snapshot_seen = true;
                        let snapshot_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(raw.as_bytes()));
                        if last_bridge_snapshot_hash.as_ref() == Some(&snapshot_hash) {
                            continue;
                        }
                        last_bridge_snapshot_hash = Some(snapshot_hash);
                        for capsule in &snapshot.capsules {
                            if let Err(error) = validate_ble_assist_capsule(capsule) {
                                let message = format!(
                                    "macOS BLE bridge provided invalid capsule {}: {error:#}",
                                    capsule.rolling_identifier
                                );
                                state
                                    .mark_ble_runtime_error("macos_native", message.clone())
                                    .await;
                                let _ = host.emit_json(
                                    "system_error",
                                    serde_json::json!({
                                        "code": "BLE_MACOS_BRIDGE_INVALID_CAPSULE",
                                        "subsystem": "ble_baseline",
                                        "message": message,
                                    }),
                                );
                                continue;
                            }
                            state.record_ble_assist_observation(capsule).await;
                        }
                        state
                            .update_ble_runtime_bridge(
                                snapshot.permission_state.clone(),
                                state.ble_runtime_status_snapshot().await.bridge_mode,
                                Some(bridge_file_path.display().to_string()),
                                Some(now),
                                snapshot.advertised_rolling_identifier.clone(),
                            )
                            .await;
                        state
                            .mark_ble_runtime_idle(
                                "macos_native",
                                snapshot
                                    .scanner_state
                                    .as_deref()
                                    .unwrap_or("bridge_snapshot_ready"),
                                snapshot
                                    .advertiser_state
                                    .as_deref()
                                    .unwrap_or("observer_only_bridge"),
                            )
                            .await;
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        let message = format!("macOS BLE bridge snapshot read failed: {error:#}");
                        state
                            .mark_ble_runtime_error("macos_native", message.clone())
                            .await;
                        let _ = host.emit_json(
                            "system_error",
                            serde_json::json!({
                                "code": "BLE_MACOS_BRIDGE_READ_FAILED",
                                "subsystem": "ble_baseline",
                                "message": message,
                            }),
                        );
                    }
                    Err(join_error) => {
                        let message =
                            format!("macOS BLE bridge snapshot worker failed: {join_error}");
                        state
                            .mark_ble_runtime_error("macos_native", message.clone())
                            .await;
                        let _ = host.emit_json(
                            "system_error",
                            serde_json::json!({
                                "code": "BLE_MACOS_BRIDGE_JOIN_FAILED",
                                "subsystem": "ble_baseline",
                                "message": message,
                            }),
                        );
                    }
                }
            }
        });
    }
}

#[cfg(target_os = "windows")]
fn windows_bridge_file_path_from_env() -> Option<PathBuf> {
    std::env::var("DASHDROP_BLE_WINDOWS_BRIDGE_FILE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(target_os = "windows")]
fn resolve_windows_bridge_file_path(config_dir: &Path) -> PathBuf {
    windows_bridge_file_path_from_env()
        .unwrap_or_else(|| config_dir.join(WINDOWS_BLE_BRIDGE_FILE_NAME))
}

#[cfg(target_os = "windows")]
fn resolve_windows_advertisement_file_path(config_dir: &Path) -> PathBuf {
    config_dir.join(WINDOWS_BLE_ADVERTISEMENT_FILE_NAME)
}

#[cfg(target_os = "windows")]
fn windows_bridge_binary_from_env() -> Option<PathBuf> {
    std::env::var("DASHDROP_BLE_WINDOWS_BRIDGE_BINARY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(target_os = "windows")]
fn windows_bridge_source_from_env() -> Option<PathBuf> {
    std::env::var("DASHDROP_BLE_WINDOWS_BRIDGE_SOURCE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(target_os = "windows")]
fn resolve_windows_bridge_source_path() -> Option<PathBuf> {
    let candidate = windows_bridge_source_from_env().unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("windows")
            .join(WINDOWS_BLE_BRIDGE_SOURCE_FILE_NAME)
    });
    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}

#[cfg(target_os = "windows")]
fn resolve_windows_bridge_binary_path(host: &Arc<dyn RuntimeHost>) -> Option<PathBuf> {
    if let Some(path) = windows_bridge_binary_from_env().filter(|path| path.exists()) {
        return Some(path);
    }

    let mut binary_names = vec![WINDOWS_BLE_BRIDGE_BINARY_NAME.to_string()];
    if let Some(target_triple) = std::env::var("TAURI_ENV_TARGET_TRIPLE")
        .ok()
        .or_else(|| std::env::var("TARGET").ok())
    {
        binary_names.push(format!("dashdrop-ble-bridge-{target_triple}.exe"));
    }

    let mut candidates = Vec::new();
    if let Some(app) = host.app_handle() {
        if let Ok(resource_dir) = app.path().resource_dir() {
            for binary_name in &binary_names {
                candidates.push(resource_dir.join(binary_name));
                candidates.push(resource_dir.join("binaries").join(binary_name));
            }
        }
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for binary_name in &binary_names {
        candidates.push(manifest_dir.join("binaries").join(binary_name));
        candidates.push(manifest_dir.join("target").join("debug").join(binary_name));
        candidates.push(
            manifest_dir
                .join("target")
                .join("release")
                .join(binary_name),
        );
    }

    candidates.into_iter().find(|path| path.exists())
}

#[cfg(target_os = "windows")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowsBleBridgeSpawnSpec {
    program: PathBuf,
    args: Vec<String>,
    bridge_mode: String,
}

#[cfg(target_os = "windows")]
fn build_windows_bridge_spawn_spec(
    host: &Arc<dyn RuntimeHost>,
    bridge_file_path: &Path,
    advertisement_file_path: &Path,
) -> Option<WindowsBleBridgeSpawnSpec> {
    if std::env::var("DASHDROP_BLE_WINDOWS_BRIDGE_DISABLE_SPAWN")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
    {
        return None;
    }

    let bridge_file = bridge_file_path.display().to_string();
    let advertisement_file = advertisement_file_path.display().to_string();
    if let Some(binary_path) = resolve_windows_bridge_binary_path(host) {
        return Some(WindowsBleBridgeSpawnSpec {
            program: binary_path,
            args: vec![
                "--snapshot-file".to_string(),
                bridge_file,
                "--advertisement-file".to_string(),
                advertisement_file,
            ],
            bridge_mode: "winrt_native_helper".to_string(),
        });
    }

    if tauri::is_dev() {
        if let Some(source_path) = resolve_windows_bridge_source_path() {
            return Some(WindowsBleBridgeSpawnSpec {
                program: PathBuf::from("powershell.exe"),
                args: vec![
                    "-NoProfile".to_string(),
                    "-ExecutionPolicy".to_string(),
                    "Bypass".to_string(),
                    "-File".to_string(),
                    source_path.display().to_string(),
                    "-SnapshotFile".to_string(),
                    bridge_file,
                    "-AdvertisementFile".to_string(),
                    advertisement_file,
                ],
                bridge_mode: "powershell_scaffold_source".to_string(),
            });
        }
    }

    None
}

#[cfg(target_os = "windows")]
fn spawn_windows_bridge_helper(
    host: &Arc<dyn RuntimeHost>,
    bridge_file_path: &Path,
    advertisement_file_path: &Path,
) -> Result<Option<String>> {
    let Some(spec) =
        build_windows_bridge_spawn_spec(host, bridge_file_path, advertisement_file_path)
    else {
        return Ok(None);
    };

    if let Some(parent) = bridge_file_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create Windows BLE bridge directory {}", parent.display()))?;
    }

    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    command
        .spawn()
        .with_context(|| format!("spawn Windows BLE bridge helper {}", spec.program.display()))?;

    Ok(Some(spec.bridge_mode))
}

#[cfg(target_os = "windows")]
fn decode_windows_bridge_snapshot(raw: &str) -> Result<WindowsBleBridgeSnapshot> {
    serde_json::from_str::<WindowsBleBridgeSnapshot>(raw)
        .context("decode Windows BLE bridge snapshot JSON")
}

#[cfg(target_os = "windows")]
fn read_windows_bridge_snapshot(
    path: &PathBuf,
) -> Result<Option<(WindowsBleBridgeSnapshot, String)>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("read Windows BLE bridge file {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let snapshot = decode_windows_bridge_snapshot(&raw)?;
    Ok(Some((snapshot, raw)))
}

#[cfg(target_os = "windows")]
fn write_windows_advertisement_request(
    path: &Path,
    identity: &crate::crypto::Identity,
) -> Result<BleAssistCapsule> {
    let capsule = build_ble_assist_capsule(identity)?;
    let request = WindowsBleAdvertisementRequest {
        updated_at_unix_ms: now_unix_millis(),
        capsule: capsule.clone(),
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "create Windows BLE advertisement directory {}",
                parent.display()
            )
        })?;
    }
    let payload =
        serde_json::to_vec(&request).context("serialize Windows BLE advertisement request")?;
    fs::write(path, payload)
        .with_context(|| format!("write Windows BLE advertisement request {}", path.display()))?;
    Ok(capsule)
}

#[cfg(target_os = "windows")]
struct WindowsBleRuntimeProvider;

#[cfg(target_os = "windows")]
impl BleRuntimeProvider for WindowsBleRuntimeProvider {
    fn provider_name(&self) -> &'static str {
        "windows_native"
    }

    fn start(
        &self,
        state: Arc<crate::state::AppState>,
        host: Arc<dyn RuntimeHost>,
        config_dir: &Path,
    ) {
        let config_dir = config_dir.to_path_buf();
        tauri::async_runtime::spawn(async move {
            let bridge_file_path = resolve_windows_bridge_file_path(&config_dir);
            let advertisement_file_path = resolve_windows_advertisement_file_path(&config_dir);
            let bridge_mode = match spawn_windows_bridge_helper(
                &host,
                &bridge_file_path,
                &advertisement_file_path,
            ) {
                Ok(mode) => mode,
                Err(error) => {
                    let message = format!("Windows BLE bridge helper failed to start: {error:#}");
                    state
                        .mark_ble_runtime_error("windows_native", message.clone())
                        .await;
                    let _ = host.emit_json(
                        "system_error",
                        serde_json::json!({
                            "code": "BLE_WINDOWS_BRIDGE_START_FAILED",
                            "subsystem": "ble_baseline",
                            "message": message,
                        }),
                    );
                    None
                }
            };

            state
                .mark_ble_runtime_started(
                    "windows_native",
                    if bridge_mode.is_some() {
                        "bridge_bootstrapping"
                    } else {
                        "windows_helper_unavailable"
                    },
                    "scaffold_unimplemented",
                )
                .await;
            state
                .update_ble_runtime_bridge(
                    Some("not_required".to_string()),
                    bridge_mode.or_else(|| {
                        windows_bridge_file_path_from_env()
                            .as_ref()
                            .map(|_| "json_file_bridge_external".to_string())
                    }),
                    Some(bridge_file_path.display().to_string()),
                    None,
                    None,
                )
                .await;
            state
                .update_ble_runtime_advertisement(
                    Some(advertisement_file_path.display().to_string()),
                    None,
                    None,
                )
                .await;

            let mut interval = tokio::time::interval(std::time::Duration::from_secs(
                WINDOWS_BLE_BRIDGE_POLL_INTERVAL_SECS,
            ));
            let mut last_bridge_snapshot_hash: Option<String> = None;
            let mut last_advertisement_refresh_started_at = 0_u64;
            loop {
                interval.tick().await;
                let now = now_unix_millis();

                if now.saturating_sub(last_advertisement_refresh_started_at)
                    >= WINDOWS_BLE_ADVERTISEMENT_REFRESH_INTERVAL_SECS.saturating_mul(1000)
                {
                    last_advertisement_refresh_started_at = now;
                    let identity = state.identity.clone();
                    let advertisement_file_path_for_worker = advertisement_file_path.clone();
                    let advertisement_file_path_display =
                        advertisement_file_path.display().to_string();
                    match tokio::task::spawn_blocking(move || {
                        write_windows_advertisement_request(
                            &advertisement_file_path_for_worker,
                            &identity,
                        )
                    })
                    .await
                    {
                        Ok(Ok(capsule)) => {
                            state
                                .update_ble_runtime_advertisement(
                                    Some(advertisement_file_path_display),
                                    Some(now),
                                    Some(capsule.rolling_identifier),
                                )
                                .await;
                        }
                        Ok(Err(error)) => {
                            let message = format!(
                                "Windows BLE advertisement request write failed: {error:#}"
                            );
                            state
                                .mark_ble_runtime_error("windows_native", message.clone())
                                .await;
                            let _ = host.emit_json(
                                "system_error",
                                serde_json::json!({
                                    "code": "BLE_WINDOWS_ADVERTISEMENT_WRITE_FAILED",
                                    "subsystem": "ble_baseline",
                                    "message": message,
                                }),
                            );
                        }
                        Err(join_error) => {
                            let message =
                                format!("Windows BLE advertisement worker failed: {join_error}");
                            state
                                .mark_ble_runtime_error("windows_native", message.clone())
                                .await;
                            let _ = host.emit_json(
                                "system_error",
                                serde_json::json!({
                                    "code": "BLE_WINDOWS_ADVERTISEMENT_WRITE_JOIN_FAILED",
                                    "subsystem": "ble_baseline",
                                    "message": message,
                                }),
                            );
                        }
                    }
                }

                match tokio::task::spawn_blocking({
                    let path = bridge_file_path.clone();
                    move || read_windows_bridge_snapshot(&path)
                })
                .await
                {
                    Ok(Ok(Some((snapshot, raw)))) => {
                        let snapshot_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(raw.as_bytes()));
                        if last_bridge_snapshot_hash.as_ref() == Some(&snapshot_hash) {
                            continue;
                        }
                        last_bridge_snapshot_hash = Some(snapshot_hash);
                        for capsule in &snapshot.capsules {
                            if let Err(error) = validate_ble_assist_capsule(capsule) {
                                let message = format!(
                                    "Windows BLE bridge provided invalid capsule {}: {error:#}",
                                    capsule.rolling_identifier
                                );
                                state
                                    .mark_ble_runtime_error("windows_native", message.clone())
                                    .await;
                                let _ = host.emit_json(
                                    "system_error",
                                    serde_json::json!({
                                        "code": "BLE_WINDOWS_BRIDGE_INVALID_CAPSULE",
                                        "subsystem": "ble_baseline",
                                        "message": message,
                                    }),
                                );
                                continue;
                            }
                            state.record_ble_assist_observation(capsule).await;
                        }
                        state
                            .update_ble_runtime_bridge(
                                snapshot.permission_state.clone(),
                                state.ble_runtime_status_snapshot().await.bridge_mode,
                                Some(bridge_file_path.display().to_string()),
                                Some(now),
                                snapshot.advertised_rolling_identifier.clone(),
                            )
                            .await;
                        state
                            .mark_ble_runtime_idle(
                                "windows_native",
                                snapshot
                                    .scanner_state
                                    .as_deref()
                                    .unwrap_or("bridge_snapshot_ready"),
                                snapshot
                                    .advertiser_state
                                    .as_deref()
                                    .unwrap_or("observer_only_bridge"),
                            )
                            .await;
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        let message = format!("Windows BLE bridge snapshot read failed: {error:#}");
                        state
                            .mark_ble_runtime_error("windows_native", message.clone())
                            .await;
                        let _ = host.emit_json(
                            "system_error",
                            serde_json::json!({
                                "code": "BLE_WINDOWS_BRIDGE_READ_FAILED",
                                "subsystem": "ble_baseline",
                                "message": message,
                            }),
                        );
                    }
                    Err(join_error) => {
                        let message =
                            format!("Windows BLE bridge snapshot worker failed: {join_error}");
                        state
                            .mark_ble_runtime_error("windows_native", message.clone())
                            .await;
                        let _ = host.emit_json(
                            "system_error",
                            serde_json::json!({
                                "code": "BLE_WINDOWS_BRIDGE_JOIN_FAILED",
                                "subsystem": "ble_baseline",
                                "message": message,
                            }),
                        );
                    }
                }
            }
        });
    }
}

fn provider_from_env() -> (Box<dyn BleRuntimeProvider>, Option<String>) {
    let raw = std::env::var("DASHDROP_BLE_PROVIDER")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match raw.as_str() {
        "" => {
            #[cfg(target_os = "macos")]
            {
                (Box::new(MacOsBleRuntimeProvider), None)
            }
            #[cfg(target_os = "windows")]
            {
                (Box::new(WindowsBleRuntimeProvider), None)
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            {
                (Box::new(NoopBleRuntimeProvider), None)
            }
        }
        "noop" => (Box::new(NoopBleRuntimeProvider), None),
        "disabled" => (Box::new(DisabledBleRuntimeProvider), None),
        #[cfg(target_os = "macos")]
        "macos_native" => (Box::new(MacOsBleRuntimeProvider), None),
        #[cfg(not(target_os = "macos"))]
        "macos_native" => (
            Box::new(NoopBleRuntimeProvider),
            Some(
                "BLE provider 'macos_native' is only available on macOS; falling back to noop baseline."
                    .to_string(),
            ),
        ),
        #[cfg(target_os = "windows")]
        "windows_native" => (Box::new(WindowsBleRuntimeProvider), None),
        #[cfg(not(target_os = "windows"))]
        "windows_native" => (
            Box::new(NoopBleRuntimeProvider),
            Some(
                "BLE provider 'windows_native' is only available on Windows; falling back to noop baseline."
                    .to_string(),
            ),
        ),
        other => (
            Box::new(NoopBleRuntimeProvider),
            Some(format!(
                "BLE provider '{other}' is not implemented yet; falling back to noop baseline."
            )),
        ),
    }
}

pub fn start_runtime(
    state: Arc<crate::state::AppState>,
    host: Arc<dyn RuntimeHost>,
    config_dir: &Path,
) {
    let (provider, warning) = provider_from_env();
    let provider_name = provider.provider_name();
    tracing::info!("Starting BLE baseline runtime provider: {provider_name}");
    provider.start(Arc::clone(&state), Arc::clone(&host), config_dir);
    if let Some(message) = warning {
        let state_for_warning = Arc::clone(&state);
        let host_for_warning = Arc::clone(&host);
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            state_for_warning
                .mark_ble_runtime_error(provider_name, message.clone())
                .await;
            let _ = host_for_warning.emit_json(
                "system_error",
                serde_json::json!({
                    "code": "BLE_PROVIDER_FALLBACK",
                    "subsystem": "ble_baseline",
                    "message": message,
                }),
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_identity_dir(label: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("dashdrop-ble-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("create temp identity dir");
        path
    }

    #[test]
    fn ble_assist_capsule_has_short_lived_rolling_identifier() {
        let dir = temp_identity_dir("capsule");
        let identity = crate::crypto::Identity::load_or_create(&dir).expect("create identity");
        let capsule = build_ble_assist_capsule(&identity).expect("build BLE assist capsule");

        assert_eq!(capsule.version, 1);
        assert!(!capsule.rolling_identifier.is_empty());
        assert!(!capsule.integrity_tag.is_empty());
        assert_eq!(capsule.transport_hint, "qr_or_short_code_fallback");
        assert!(capsule.qr_fallback_available);
        assert!(capsule.short_code_fallback_available);
        assert_eq!(capsule.rotation_window_ms, BLE_ASSIST_ROTATION_WINDOW_MS);
        assert!(capsule.expires_at_unix_ms > capsule.issued_at_unix_ms);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn macos_probe_parser_detects_missing_hardware() {
        let probe = parse_macos_system_profiler_output("No Bluetooth information found.");
        assert!(!probe.hardware_present);
        assert!(!probe.controller_powered);
    }

    #[test]
    fn macos_probe_parser_detects_powered_controller() {
        let probe = parse_macos_system_profiler_output(
            "Bluetooth:\n    Controller Power State: On\n    Discoverable: Off\n",
        );
        assert!(probe.hardware_present);
        assert!(probe.controller_powered);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_bridge_snapshot_parser_decodes_permission_and_capsules() {
        let snapshot = decode_macos_bridge_snapshot(
            r#"{
                "permission_state": "granted",
                "scanner_state": "bridge_scanning",
                "advertiser_state": "bridge_advertising",
                "capsules": [
                    {
                        "version": 1,
                        "issued_at_unix_ms": 1700000000000,
                        "expires_at_unix_ms": 1700000060000,
                        "rolling_identifier": "capsule-1",
                        "integrity_tag": "tag-1",
                        "transport_hint": "qr_or_short_code_fallback",
                        "qr_fallback_available": true,
                        "short_code_fallback_available": true,
                        "rotation_window_ms": 30000
                    }
                ]
            }"#,
        )
        .expect("decode bridge snapshot");

        assert_eq!(snapshot.permission_state.as_deref(), Some("granted"));
        assert_eq!(snapshot.scanner_state.as_deref(), Some("bridge_scanning"));
        assert_eq!(
            snapshot.advertiser_state.as_deref(),
            Some("bridge_advertising")
        );
        assert_eq!(snapshot.capsules.len(), 1);
        assert_eq!(snapshot.capsules[0].rolling_identifier, "capsule-1");
    }
}
