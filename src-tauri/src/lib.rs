mod ble;
mod commands;
#[allow(dead_code)]
mod core_service;
mod crypto;
#[allow(dead_code)]
mod daemon;
pub mod db;
pub mod discovery;
mod dto;
mod local_ipc;
mod pairing;
mod persistence;
mod persistence_progress;
mod runtime;
pub mod state;
pub mod transport;

pub use crypto::Identity;
use daemon::client::LocalIpcClient;
use local_ipc::{LocalIpcCommand, LocalIpcEndpointKind};
use runtime::bootstrap::{
    collect_external_share_paths_from_args, collect_pairing_links_from_args, initialize_state_at,
    resolve_config_dir_from_base,
};
use runtime::host::{NoopRuntimeHost, TauriRuntimeHost};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{
    menu::{MenuBuilder, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager,
};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

struct UiShellState {
    background_hide_notice_emitted: std::sync::Mutex<bool>,
}

// Shared UI-shell contract: these ids and event names are intentionally kept
// stable because tray actions, second-instance activation, deep-link delivery,
// and the frontend shell router all depend on them across worktrees.
const TRAY_ICON_ID: &str = "dashdrop-tray";
const TRAY_MENU_SHOW_ID: &str = "tray_show";
const TRAY_MENU_NEARBY_ID: &str = "tray_nearby";
const TRAY_MENU_TRANSFERS_ID: &str = "tray_transfers";
const TRAY_MENU_HISTORY_ID: &str = "tray_history";
const TRAY_MENU_TRUSTED_DEVICES_ID: &str = "tray_trusted_devices";
const TRAY_MENU_SECURITY_EVENTS_ID: &str = "tray_security_events";
const TRAY_MENU_SETTINGS_ID: &str = "tray_settings";
const TRAY_MENU_REVIEW_ATTENTION_ID: &str = "tray_review_attention";
const TRAY_MENU_QUIT_ID: &str = "tray_quit";
const TRAY_MENU_SUMMARY_INCOMING_ID: &str = "tray_summary_incoming";
const TRAY_MENU_SUMMARY_ACTIVE_ID: &str = "tray_summary_active";
const TRAY_MENU_SUMMARY_FAILURES_ID: &str = "tray_summary_failures";
const APP_WINDOW_REVEALED_EVENT: &str = "app_window_revealed";
const APP_NAVIGATION_REQUESTED_EVENT: &str = "app_navigation_requested";
const PAIRING_LINK_RECEIVED_EVENT: &str = "pairing_link_received";
const DASHDROP_PAIRING_DEEP_LINK_PREFIX: &str = "dashdrop://pair?";

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TrayAttentionState {
    // Keep this field set in lockstep with the Tauri command payload accepted by
    // `set_shell_attention_state` and the frontend aggregation in `store.ts`.
    pub pending_incoming_count: u32,
    pub active_transfer_count: u32,
    pub recent_failure_count: u32,
    pub notifications_degraded: bool,
}

fn write_startup_error_log(config_dir: &std::path::Path, message: &str) {
    let _ = std::fs::create_dir_all(config_dir);
    let path = config_dir.join("startup-error.log");
    let _ = std::fs::write(&path, message);
}

fn show_startup_error_dialog<R: tauri::Runtime, T: tauri::Manager<R>>(app: &T, message: &str) {
    app.dialog()
        .message(message.to_string())
        .title("DashDrop Startup Error")
        .kind(MessageDialogKind::Error)
        .show(|_| {});
}

fn handoff_to_running_instance(
    config_dir: &std::path::Path,
    paths: &[String],
    pairing_links: &[String],
) -> bool {
    handoff_to_running_ui_instance(config_dir, paths, pairing_links)
}

fn handoff_to_running_ui_instance(
    config_dir: &std::path::Path,
    paths: &[String],
    pairing_links: &[String],
) -> bool {
    // `app/activate` is the cross-process shell handoff contract. It forwards
    // queued external file paths plus pairing deep links, but it does not own
    // runtime state or attempt to dispatch transfer actions directly.
    let client =
        LocalIpcClient::from_config_dir_for_kind(config_dir, LocalIpcEndpointKind::UiActivation);
    tauri::async_runtime::block_on(async {
        client
            .send(LocalIpcCommand::AppActivate {
                paths: paths.to_vec(),
                pairing_links: pairing_links.to_vec(),
            })
            .await
            .is_ok()
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequestedControlPlaneMode {
    InProcess,
    Daemon,
}

impl RequestedControlPlaneMode {
    fn as_env_value(self) -> &'static str {
        match self {
            Self::InProcess => "in_process",
            Self::Daemon => "daemon",
        }
    }
}

fn runtime_profile_name() -> &'static str {
    if tauri::is_dev() {
        "dev"
    } else {
        "packaged"
    }
}

fn default_requested_control_plane_mode() -> RequestedControlPlaneMode {
    if tauri::is_dev() {
        RequestedControlPlaneMode::InProcess
    } else {
        RequestedControlPlaneMode::Daemon
    }
}

fn requested_control_plane_mode() -> RequestedControlPlaneMode {
    match std::env::var("DASHDROP_CONTROL_PLANE_MODE")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("daemon") => RequestedControlPlaneMode::Daemon,
        Some("in_process") => RequestedControlPlaneMode::InProcess,
        _ => default_requested_control_plane_mode(),
    }
}

fn daemon_control_plane_mode_from_sources(
    state_mode: Option<&str>,
    env_mode: Option<&str>,
) -> bool {
    let normalized = |value: Option<&str>| match value.map(|raw| raw.trim().to_ascii_lowercase()) {
        Some(mode) if mode == "daemon" => Some(mode),
        Some(mode) if mode == "in_process" => Some(mode),
        _ => None,
    };

    matches!(
        normalized(state_mode)
            .as_deref()
            .or(normalized(env_mode).as_deref()),
        Some("daemon")
    )
}

fn daemon_control_plane_mode_enabled<R: tauri::Runtime, T: tauri::Manager<R>>(app: &T) -> bool {
    let state_mode = app
        .try_state::<Arc<crate::state::AppState>>()
        .and_then(|state| {
            let mode = tauri::async_runtime::block_on(async {
                state.control_plane_mode.read().await.clone()
            });
            let mode = mode.trim().to_ascii_lowercase();
            if mode.is_empty() {
                None
            } else {
                Some(mode)
            }
        });
    let env_mode = std::env::var("DASHDROP_CONTROL_PLANE_MODE")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase());

    daemon_control_plane_mode_from_sources(state_mode.as_deref(), env_mode.as_deref())
}

#[derive(Debug, Clone)]
struct DaemonServiceResolution {
    available: bool,
    binary_path: Option<PathBuf>,
    status: String,
    connect_attempts: u32,
    connect_strategy: String,
}

fn probe_daemon_service(config_dir: &std::path::Path, delays_ms: &[u64]) -> (bool, u32) {
    let client = LocalIpcClient::from_config_dir(config_dir);
    tauri::async_runtime::block_on(async {
        let mut attempts = 0;
        for delay_ms in delays_ms {
            if *delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(*delay_ms)).await;
            }
            attempts += 1;
            if client.send(LocalIpcCommand::AuthIssue).await.is_ok() {
                return (true, attempts);
            }
        }
        (false, attempts)
    })
}

fn daemon_binary_name_prefix() -> &'static str {
    "dashdropd"
}

fn daemon_binary_name_candidates() -> Vec<String> {
    let mut candidates = Vec::new();
    let base_name = if cfg!(windows) {
        format!("{}.exe", daemon_binary_name_prefix())
    } else {
        daemon_binary_name_prefix().to_string()
    };
    candidates.push(base_name);

    if let Some(target_triple) = std::env::var("TAURI_ENV_TARGET_TRIPLE")
        .ok()
        .or_else(|| std::env::var("TARGET").ok())
    {
        let sidecar_name = if cfg!(windows) {
            format!("{}-{}.exe", daemon_binary_name_prefix(), target_triple)
        } else {
            format!("{}-{}", daemon_binary_name_prefix(), target_triple)
        };
        candidates.push(sidecar_name);
    }
    candidates
}

fn daemon_search_directories<R: tauri::Runtime, T: tauri::Manager<R>>(app: &T) -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if let Ok(current) = std::env::current_exe() {
        if let Some(parent) = current.parent() {
            push_unique_path(&mut directories, parent.to_path_buf());
        }
    }
    if let Ok(resource_dir) = app.path().resource_dir() {
        push_unique_path(&mut directories, resource_dir);
    }
    if tauri::is_dev() {
        append_dev_daemon_search_directories(
            &mut directories,
            std::env::current_dir().ok().as_deref(),
            Path::new(env!("CARGO_MANIFEST_DIR")),
        );
    }
    directories
}

fn resolve_daemon_binary_from_directory(directory: &Path) -> Option<PathBuf> {
    for candidate in daemon_binary_name_candidates() {
        let path = directory.join(candidate);
        if path.exists() {
            return Some(path);
        }
    }

    let entries = std::fs::read_dir(directory).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if file_name == daemon_binary_name_prefix()
            || file_name == format!("{}.exe", daemon_binary_name_prefix())
            || file_name.starts_with(&format!("{}-", daemon_binary_name_prefix()))
        {
            return Some(path);
        }
    }

    None
}

fn resolve_daemon_binary_path<R: tauri::Runtime, T: tauri::Manager<R>>(app: &T) -> Option<PathBuf> {
    daemon_search_directories(app)
        .into_iter()
        .find_map(|directory| resolve_daemon_binary_from_directory(&directory))
}

fn dashdropd_binary_file_name() -> String {
    if cfg!(windows) {
        format!("{}.exe", daemon_binary_name_prefix())
    } else {
        daemon_binary_name_prefix().to_string()
    }
}

fn push_unique_path(directories: &mut Vec<PathBuf>, path: PathBuf) {
    if directories.iter().any(|existing| existing == &path) {
        return;
    }
    directories.push(path);
}

fn append_dev_daemon_search_directories(
    directories: &mut Vec<PathBuf>,
    current_dir: Option<&Path>,
    manifest_dir: &Path,
) {
    for path in [
        manifest_dir.join("binaries"),
        manifest_dir.join("target").join("debug"),
        manifest_dir.join("target").join("release"),
    ] {
        push_unique_path(directories, path);
    }

    let Some(current_dir) = current_dir else {
        return;
    };

    for path in [
        current_dir.join("binaries"),
        current_dir.join("target").join("debug"),
        current_dir.join("target").join("release"),
        current_dir.join("src-tauri").join("binaries"),
        current_dir.join("src-tauri").join("target").join("debug"),
        current_dir.join("src-tauri").join("target").join("release"),
    ] {
        push_unique_path(directories, path);
    }
}

fn spawn_daemon_process_from_path(
    binary_path: &Path,
    config_dir: &std::path::Path,
) -> anyhow::Result<bool> {
    let mut command = std::process::Command::new(binary_path);
    command
        .env("DASHDROP_CONFIG_DIR", config_dir)
        .env_remove("DASHDROP_CONTROL_PLANE_MODE")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    command.spawn().map(|_| true).map_err(|e| {
        anyhow::anyhow!(
            "failed to spawn dashdropd from {}: {e}",
            binary_path.display()
        )
    })
}

fn build_dev_daemon_binary() -> anyhow::Result<Option<PathBuf>> {
    if !tauri::is_dev() {
        return Ok(None);
    }

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest_path = manifest_dir.join("Cargo.toml");
    let target_dir = manifest_dir.join("target").join("debug");
    let binary_path = target_dir.join(dashdropd_binary_file_name());

    if binary_path.exists() {
        return Ok(Some(binary_path));
    }

    let status = std::process::Command::new("cargo")
        .arg("build")
        .arg("--manifest-path")
        .arg(&manifest_path)
        .arg("--bin")
        .arg("dashdropd")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| anyhow::anyhow!("failed to start cargo build for dashdropd: {e}"))?;

    if !status.success() {
        anyhow::bail!("cargo build --bin dashdropd exited with status {status}");
    }

    if binary_path.exists() {
        Ok(Some(binary_path))
    } else {
        Ok(None)
    }
}

fn ensure_daemon_service_available(
    app: &tauri::App,
    config_dir: &std::path::Path,
    requested_mode: RequestedControlPlaneMode,
) -> DaemonServiceResolution {
    if requested_mode == RequestedControlPlaneMode::InProcess {
        return DaemonServiceResolution {
            available: false,
            binary_path: None,
            status: "disabled".to_string(),
            connect_attempts: 0,
            connect_strategy: "in_process_only".to_string(),
        };
    }

    let binary_path = resolve_daemon_binary_path(app);
    let (available, existing_attempts) = probe_daemon_service(config_dir, &[0, 120, 250, 500, 800]);

    if available {
        return DaemonServiceResolution {
            available: true,
            binary_path,
            status: if existing_attempts <= 1 {
                "connected_existing".to_string()
            } else {
                "connected_after_retry".to_string()
            },
            connect_attempts: existing_attempts,
            connect_strategy: "attach_existing".to_string(),
        };
    }

    if let Some(binary_path) = binary_path.clone() {
        match spawn_daemon_process_from_path(&binary_path, config_dir) {
            Ok(true) => {
                let (available, spawn_attempts) = probe_daemon_service(
                    config_dir,
                    &[120, 180, 300, 450, 700, 1_000, 1_400, 1_900, 2_500],
                );
                return DaemonServiceResolution {
                    available,
                    binary_path: Some(binary_path),
                    status: if available {
                        "spawned".to_string()
                    } else {
                        "spawn_timeout".to_string()
                    },
                    connect_attempts: existing_attempts + spawn_attempts,
                    connect_strategy: "spawn_sidecar".to_string(),
                };
            }
            Ok(false) => unreachable!("spawn_daemon_process_from_path only returns true or error"),
            Err(err) => {
                tracing::warn!("failed to spawn dashdropd automatically: {err:#}");
                return DaemonServiceResolution {
                    available: false,
                    binary_path: Some(binary_path),
                    status: "spawn_failed".to_string(),
                    connect_attempts: existing_attempts,
                    connect_strategy: "spawn_sidecar".to_string(),
                };
            }
        }
    }

    match build_dev_daemon_binary() {
        Ok(Some(binary_path)) => match spawn_daemon_process_from_path(&binary_path, config_dir) {
            Ok(true) => {
                let (available, spawn_attempts) = probe_daemon_service(
                    config_dir,
                    &[120, 180, 300, 450, 700, 1_000, 1_400, 1_900, 2_500],
                );
                DaemonServiceResolution {
                    available,
                    binary_path: Some(binary_path),
                    status: if available {
                        "spawned_built".to_string()
                    } else {
                        "spawn_timeout_after_build".to_string()
                    },
                    connect_attempts: existing_attempts + spawn_attempts,
                    connect_strategy: "build_and_spawn".to_string(),
                }
            }
            Ok(false) => unreachable!("spawn_daemon_process_from_path only returns true or error"),
            Err(err) => {
                tracing::warn!("failed to spawn built dashdropd automatically: {err:#}");
                DaemonServiceResolution {
                    available: false,
                    binary_path: Some(binary_path),
                    status: "spawn_failed_after_build".to_string(),
                    connect_attempts: existing_attempts,
                    connect_strategy: "build_and_spawn".to_string(),
                }
            }
        },
        Ok(None) => DaemonServiceResolution {
            available: false,
            binary_path: None,
            status: "binary_missing".to_string(),
            connect_attempts: existing_attempts,
            connect_strategy: "binary_missing".to_string(),
        },
        Err(err) => {
            tracing::warn!("failed to build dashdropd automatically in dev mode: {err:#}");
            DaemonServiceResolution {
                available: false,
                binary_path: None,
                status: "build_failed".to_string(),
                connect_attempts: existing_attempts,
                connect_strategy: "build_and_spawn".to_string(),
            }
        }
    }
}

fn reveal_main_window<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
    let _ = app.emit(
        APP_WINDOW_REVEALED_EVENT,
        serde_json::json!({
            "source": "reopen"
        }),
    );
}

fn emit_app_navigation_requested<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    target: &str,
    source: &str,
) {
    let _ = app.emit(
        APP_NAVIGATION_REQUESTED_EVENT,
        serde_json::json!({
            "target": target,
            "source": source,
        }),
    );
}

fn handle_tray_navigation_request<R: tauri::Runtime>(app: &tauri::AppHandle<R>, target: &str) {
    reveal_main_window(app);
    emit_app_navigation_requested(app, target, "tray_menu");
}

fn handle_tray_menu_event<R: tauri::Runtime>(app: &tauri::AppHandle<R>, menu_id: &str) {
    match menu_id {
        TRAY_MENU_SHOW_ID => reveal_main_window(app),
        TRAY_MENU_REVIEW_ATTENTION_ID => handle_tray_navigation_request(app, "Transfers"),
        TRAY_MENU_NEARBY_ID => handle_tray_navigation_request(app, "Nearby"),
        TRAY_MENU_TRANSFERS_ID => handle_tray_navigation_request(app, "Transfers"),
        TRAY_MENU_HISTORY_ID => handle_tray_navigation_request(app, "History"),
        TRAY_MENU_TRUSTED_DEVICES_ID => handle_tray_navigation_request(app, "TrustedDevices"),
        TRAY_MENU_SECURITY_EVENTS_ID => handle_tray_navigation_request(app, "SecurityEvents"),
        TRAY_MENU_SETTINGS_ID => handle_tray_navigation_request(app, "Settings"),
        TRAY_MENU_QUIT_ID => app.exit(0),
        _ => {}
    }
}

fn build_tray_menu<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    attention: TrayAttentionState,
) -> Result<tauri::menu::Menu<R>, tauri::Error> {
    let has_attention = attention.pending_incoming_count > 0
        || attention.active_transfer_count > 0
        || attention.recent_failure_count > 0;
    let summary_incoming = MenuItem::with_id(
        app,
        TRAY_MENU_SUMMARY_INCOMING_ID,
        format!("Pending Incoming: {}", attention.pending_incoming_count),
        false,
        None::<&str>,
    )?;
    let summary_active = MenuItem::with_id(
        app,
        TRAY_MENU_SUMMARY_ACTIVE_ID,
        format!("Active Transfers: {}", attention.active_transfer_count),
        false,
        None::<&str>,
    )?;
    let summary_failures = MenuItem::with_id(
        app,
        TRAY_MENU_SUMMARY_FAILURES_ID,
        if attention.notifications_degraded {
            format!(
                "Recent Issues: {} • notifications degraded",
                attention.recent_failure_count
            )
        } else {
            format!("Recent Issues: {}", attention.recent_failure_count)
        },
        false,
        None::<&str>,
    )?;
    let review_attention = MenuItem::with_id(
        app,
        TRAY_MENU_REVIEW_ATTENTION_ID,
        if attention.pending_incoming_count > 0 {
            format!(
                "Review {} Pending Request{}",
                attention.pending_incoming_count,
                if attention.pending_incoming_count == 1 {
                    ""
                } else {
                    "s"
                }
            )
        } else if attention.recent_failure_count > 0 {
            format!(
                "Review {} Recent Issue{}",
                attention.recent_failure_count,
                if attention.recent_failure_count == 1 {
                    ""
                } else {
                    "s"
                }
            )
        } else if attention.active_transfer_count > 0 {
            format!(
                "View {} Active Transfer{}",
                attention.active_transfer_count,
                if attention.active_transfer_count == 1 {
                    ""
                } else {
                    "s"
                }
            )
        } else {
            "No Pending Attention".to_string()
        },
        has_attention,
        None::<&str>,
    )?;
    let transfers_item = MenuItem::with_id(
        app,
        TRAY_MENU_TRANSFERS_ID,
        if attention.pending_incoming_count > 0 || attention.recent_failure_count > 0 {
            format!(
                "Transfers • {} pending / {} issue{}",
                attention.pending_incoming_count,
                attention.recent_failure_count,
                if attention.recent_failure_count == 1 {
                    ""
                } else {
                    "s"
                }
            )
        } else if attention.active_transfer_count > 0 {
            format!("Transfers • {} active", attention.active_transfer_count)
        } else {
            "Transfers".to_string()
        },
        true,
        None::<&str>,
    )?;
    let separator = PredefinedMenuItem::separator(app)?;

    MenuBuilder::new(app)
        .item(&summary_incoming)
        .item(&summary_active)
        .item(&summary_failures)
        .item(&review_attention)
        .item(&separator)
        .text(TRAY_MENU_SHOW_ID, "Show DashDrop")
        .separator()
        .text(TRAY_MENU_NEARBY_ID, "Nearby")
        .item(&transfers_item)
        .text(TRAY_MENU_HISTORY_ID, "History")
        .text(TRAY_MENU_TRUSTED_DEVICES_ID, "Trusted Devices")
        .text(TRAY_MENU_SECURITY_EVENTS_ID, "Security Events")
        .text(TRAY_MENU_SETTINGS_ID, "Settings")
        .separator()
        .text(TRAY_MENU_QUIT_ID, "Quit DashDrop")
        .build()
}

pub(crate) fn update_tray_attention<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    attention: TrayAttentionState,
) -> Result<(), String> {
    let Some(tray) = app.tray_by_id(TRAY_ICON_ID) else {
        return Ok(());
    };

    let mut parts = Vec::new();
    if attention.pending_incoming_count > 0 {
        parts.push(format!(
            "{} pending incoming transfer{}",
            attention.pending_incoming_count,
            if attention.pending_incoming_count == 1 {
                ""
            } else {
                "s"
            }
        ));
    }
    if attention.active_transfer_count > 0 {
        parts.push(format!(
            "{} active transfer{}",
            attention.active_transfer_count,
            if attention.active_transfer_count == 1 {
                ""
            } else {
                "s"
            }
        ));
    }
    if attention.recent_failure_count > 0 {
        parts.push(format!(
            "{} recent issue{}",
            attention.recent_failure_count,
            if attention.recent_failure_count == 1 {
                ""
            } else {
                "s"
            }
        ));
    }
    if attention.notifications_degraded {
        parts.push("notifications degraded".to_string());
    }

    let tooltip = if parts.is_empty() {
        "DashDrop".to_string()
    } else {
        format!("DashDrop • {}", parts.join(" • "))
    };
    tray.set_tooltip(Some(tooltip))
        .map_err(|err| format!("failed to update tray tooltip: {err}"))?;

    let attention_count = attention
        .pending_incoming_count
        .saturating_add(attention.active_transfer_count)
        .saturating_add(attention.recent_failure_count);
    if let Err(err) = tray.set_title(if attention_count > 0 {
        Some(attention_count.to_string())
    } else {
        None
    }) {
        tracing::debug!("failed to update tray title: {err}");
    }

    let menu = build_tray_menu(app, attention)
        .map_err(|err| format!("failed to build tray menu: {err}"))?;
    tray.set_menu(Some(menu))
        .map_err(|err| format!("failed to update tray menu: {err}"))?;
    Ok(())
}

fn setup_tray_icon<R: tauri::Runtime>(app: &tauri::App<R>) -> Result<(), tauri::Error> {
    let menu = build_tray_menu(app.app_handle(), TrayAttentionState::default())?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ICON_ID)
        .menu(&menu)
        .tooltip("DashDrop")
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| handle_tray_menu_event(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                reveal_main_window(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    } else {
        tracing::warn!("default window icon missing; tray icon may be invisible on some platforms");
    }

    let _tray = builder.build(app)?;
    Ok(())
}

fn collect_pairing_links_from_url_strings<I, S>(values: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    values
        .into_iter()
        .map(|value| value.as_ref().trim().to_string())
        .filter(|value| value.starts_with(DASHDROP_PAIRING_DEEP_LINK_PREFIX))
        .collect()
}

fn emit_pairing_link_received<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    pairing_links: &[String],
    source: &str,
) {
    if pairing_links.is_empty() {
        return;
    }

    for pairing_link in pairing_links {
        let _ = app.emit(
            PAIRING_LINK_RECEIVED_EVENT,
            serde_json::json!({
                "pairing_link": pairing_link,
                "source": source,
            }),
        );
    }
    reveal_main_window(app);
}

fn emit_daemon_startup_warning(
    host: &Arc<dyn runtime::host::RuntimeHost>,
    requested_mode: RequestedControlPlaneMode,
    actual_mode: RequestedControlPlaneMode,
    resolution: &DaemonServiceResolution,
) {
    if actual_mode == RequestedControlPlaneMode::Daemon {
        return;
    }

    let (code, message) = if requested_mode == RequestedControlPlaneMode::Daemon
        && resolution.binary_path.is_none()
    {
        (
            "DAEMON_BINARY_MISSING",
            "DashDrop was asked to start in daemon mode, but no dashdropd sidecar was found. Falling back to in-process control plane."
                .to_string(),
        )
    } else if resolution.binary_path.is_some() {
        (
            "DAEMON_CONTROL_PLANE_FALLBACK",
            format!(
                "DashDrop found a daemon sidecar but stayed on the in-process control plane (status: {}).",
                resolution.status
            ),
        )
    } else {
        return;
    };

    let _ = host.emit_json(
        "system_error",
        serde_json::json!({
            "code": code,
            "subsystem": "daemon_control_plane",
            "message": message,
            "daemon_status": resolution.status,
            "daemon_connect_attempts": resolution.connect_attempts,
            "daemon_connect_strategy": resolution.connect_strategy,
            "daemon_binary_path": resolution.binary_path.as_ref().map(|path| path.display().to_string()),
        }),
    );
}

pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();
}

fn headless_idle_exit_timeout_secs_from_env(raw: Option<&str>) -> Option<u64> {
    raw.and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
}

fn headless_idle_exit_timeout_secs() -> Option<u64> {
    headless_idle_exit_timeout_secs_from_env(
        std::env::var("DASHDROP_HEADLESS_IDLE_EXIT_SECS")
            .ok()
            .as_deref(),
    )
}

async fn await_headless_idle_shutdown(state: Arc<crate::state::AppState>, timeout_secs: u64) {
    let timeout = std::time::Duration::from_secs(timeout_secs);
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
    let mut idle_started_at: Option<std::time::Instant> = None;

    loop {
        interval.tick().await;
        let blockers = state.headless_idle_blockers().await;
        if blockers.is_empty() {
            let idle_started = idle_started_at.get_or_insert_with(std::time::Instant::now);
            let remaining = timeout.saturating_sub(idle_started.elapsed());
            let deadline_unix_ms = crate::state::AppState::now_unix_millis()
                .saturating_add(remaining.as_millis().min(u64::MAX as u128) as u64);
            state
                .set_daemon_idle_monitor_state(
                    true,
                    Some(timeout_secs),
                    Some(deadline_unix_ms),
                    Vec::new(),
                )
                .await;

            if idle_started.elapsed() >= timeout {
                *state.daemon_status.write().await = "headless_idle_shutdown".to_string();
                state.record_runtime_event(
                    "daemon_idle_shutdown_triggered",
                    serde_json::json!({
                        "reason": "idle_timeout_elapsed",
                        "timeout_secs": timeout_secs,
                    }),
                );
                tracing::info!(
                    timeout_secs,
                    "Headless daemon idle timeout reached; exiting"
                );
                return;
            }
        } else {
            idle_started_at = None;
            state
                .set_daemon_idle_monitor_state(true, Some(timeout_secs), None, blockers.clone())
                .await;
        }
    }
}

pub async fn run_headless_daemon() -> anyhow::Result<()> {
    init_tracing();

    let config_dir = runtime::bootstrap::resolve_headless_config_dir()?;
    let state = initialize_state_at(&config_dir).await?;

    *state.control_plane_mode.write().await =
        RequestedControlPlaneMode::Daemon.as_env_value().to_string();
    *state.requested_control_plane_mode.write().await =
        RequestedControlPlaneMode::Daemon.as_env_value().to_string();
    *state.runtime_profile.write().await = "headless".to_string();
    *state.daemon_status.write().await = "headless_active".to_string();
    *state.daemon_connect_attempts.write().await = 0;
    *state.daemon_connect_strategy.write().await = "headless_direct".to_string();

    tracing::info!(
        "Headless daemon skeleton ready: fp={}, name={}",
        state.identity.fingerprint,
        state.identity.device_name
    );

    let host = NoopRuntimeHost::shared(Arc::clone(&state));
    let runtime = runtime::supervisor::start(Arc::clone(&state), host, &config_dir)?;

    tracing::info!(
        network_runtime_enabled = runtime.network_runtime_enabled(),
        "DashDrop daemon skeleton started"
    );
    let idle_timeout_secs = headless_idle_exit_timeout_secs();
    if let Some(timeout_secs) = idle_timeout_secs {
        tracing::info!(
            timeout_secs,
            "Headless daemon idle auto-exit enabled via DASHDROP_HEADLESS_IDLE_EXIT_SECS"
        );
        state
            .set_daemon_idle_monitor_state(
                true,
                Some(timeout_secs),
                None,
                state.headless_idle_blockers().await,
            )
            .await;
    } else {
        state
            .set_daemon_idle_monitor_state(false, None, None, Vec::new())
            .await;
    }

    tracing::info!("Waiting for shutdown signal or idle auto-exit");

    if let Some(timeout_secs) = idle_timeout_secs {
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                result?;
                tracing::info!("Shutdown signal received, stopping daemon skeleton");
            }
            _ = await_headless_idle_shutdown(Arc::clone(&state), timeout_secs) => {}
        }
    } else {
        tokio::signal::ctrl_c().await?;
        tracing::info!("Shutdown signal received, stopping daemon skeleton");
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn setup_windows_shell_context_menu() {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, REG_SZ,
    };

    let exe_path = match std::env::current_exe() {
        Ok(path) => path,
        Err(_) => return,
    };
    let exe_str = exe_path.to_string_lossy().to_string();

    // Registry keys to create:
    // HKCU\Software\Classes\*\shell\DashDrop -> "Send with DashDrop"
    // HKCU\Software\Classes\*\shell\DashDrop\Icon -> "path\to\exe,0"
    // HKCU\Software\Classes\*\shell\DashDrop\command -> "path\to\exe" "%1"

    let command_value = format!("\"{}\" \"%1\"", exe_str);
    let keys = [
        (
            "Software\\Classes\\*\\shell\\DashDrop",
            Some("Send with DashDrop"),
        ),
        (
            "Software\\Classes\\*\\shell\\DashDrop\\command",
            Some(command_value.as_str()),
        ),
    ];

    for (key_path, value) in keys {
        let mut hkey: HKEY = std::ptr::null_mut();
        let wide_key: Vec<u16> = std::ffi::OsStr::new(key_path)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            if RegCreateKeyExW(
                HKEY_CURRENT_USER,
                wide_key.as_ptr(),
                0,
                std::ptr::null_mut(),
                0,
                0x0002 | 0x0004, // KEY_SET_VALUE | KEY_CREATE_SUB_KEY
                std::ptr::null(),
                &mut hkey,
                std::ptr::null_mut(),
            ) == 0
            {
                if let Some(v) = value {
                    let wide_value: Vec<u16> = std::ffi::OsStr::new(v)
                        .encode_wide()
                        .chain(std::iter::once(0))
                        .collect();
                    RegSetValueExW(
                        hkey,
                        std::ptr::null(),
                        0,
                        REG_SZ,
                        wide_value.as_ptr() as *const u8,
                        (wide_value.len() * 2) as u32,
                    );

                    // Also set the icon for the main shell entry
                    if key_path.ends_with("DashDrop") {
                        let icon_value: Vec<u16> = std::ffi::OsStr::new(&exe_str)
                            .encode_wide()
                            .chain(std::iter::once(0))
                            .collect();
                        let icon_name: Vec<u16> = std::ffi::OsStr::new("Icon")
                            .encode_wide()
                            .chain(std::iter::once(0))
                            .collect();
                        RegSetValueExW(
                            hkey,
                            icon_name.as_ptr(),
                            0,
                            REG_SZ,
                            icon_value.as_ptr() as *const u8,
                            (icon_value.len() * 2) as u32,
                        );
                    }
                }
                RegCloseKey(hkey);
            }
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    init_tracing();

    #[cfg(target_os = "windows")]
    if !tauri::is_dev() {
        setup_windows_shell_context_menu();
    }

    let app = match tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_deep_link::init())
        .on_window_event(|window, event| {
            if !daemon_control_plane_mode_enabled(window.app_handle()) || window.label() != "main" {
                return;
            }

            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if let Err(err) = window.hide() {
                    tracing::warn!("failed to hide main window on close request: {err}");
                    return;
                }

                let Ok(shell_state) = window.app_handle().try_state::<UiShellState>().ok_or(()) else {
                    return;
                };
                let Ok(mut emitted) = shell_state.background_hide_notice_emitted.lock() else {
                    return;
                };
                if !*emitted {
                    *emitted = true;
                    window
                        .app_handle()
                        .emit(
                            "system_error",
                            serde_json::json!({
                                "code": "DAEMON_UI_HIDDEN",
                                "subsystem": "ui_shell",
                                "message": "DashDrop is still running in the background. Active transfers and discovery stay attached to the daemon control plane."
                            }),
                        )
                        .ok();
                }
            }
        })
        .setup(|app| {
            let handle = app.handle().clone();
            #[cfg(any(target_os = "linux", all(debug_assertions, target_os = "windows")))]
            if let Err(err) = app.deep_link().register_all() {
                tracing::warn!("failed to register desktop deep-link schemes during setup: {err:#}");
            }
            app.deep_link().on_open_url({
                let handle = handle.clone();
                move |event| {
                    let pairing_links =
                        collect_pairing_links_from_url_strings(event.urls().iter().map(|url| url.as_str()));
                    emit_pairing_link_received(&handle, &pairing_links, "deep_link_plugin");
                }
            });
            app.manage(UiShellState {
                background_hide_notice_emitted: std::sync::Mutex::new(false),
            });
            app.manage(commands::DaemonIpcAuthState::default());
            if let Err(err) = setup_tray_icon(app) {
                tracing::warn!("failed to initialize tray icon: {err:#}");
            }

            // Config directory: ~/.config/dashdrop/ (or via env var for testing)
            let base_config_dir = match app.path().app_config_dir() {
                Ok(path) => path,
                Err(e) => {
                    let message = format!("Failed to resolve configuration directory.\n\n{e:#}");
                    tracing::error!("{message}");
                    show_startup_error_dialog(app, &message);
                    return Err(anyhow::anyhow!(message).into());
                }
            };

            let config_dir = match resolve_config_dir_from_base(Some(base_config_dir)) {
                Ok(path) => path,
                Err(e) => {
                    let message = format!("Failed to resolve configuration directory.\n\n{e:#}");
                    tracing::error!("{message}");
                    show_startup_error_dialog(app, &message);
                    return Err(anyhow::anyhow!(message).into());
                }
            };

            let startup_share_paths = collect_external_share_paths_from_args();
            let startup_pairing_links = collect_pairing_links_from_args();
            if handoff_to_running_instance(
                &config_dir,
                &startup_share_paths,
                &startup_pairing_links,
            ) {
                tracing::info!("Forwarded activation/share payload to running DashDrop instance");
                app.handle().exit(0);
                return Ok(());
            }

            let requested_mode = requested_control_plane_mode();
            let daemon_resolution =
                ensure_daemon_service_available(app, &config_dir, requested_mode);
            let actual_mode = if daemon_resolution.available {
                RequestedControlPlaneMode::Daemon
            } else {
                RequestedControlPlaneMode::InProcess
            };
            std::env::set_var("DASHDROP_CONTROL_PLANE_MODE", actual_mode.as_env_value());

            let state = match tauri::async_runtime::block_on(initialize_state_at(&config_dir)) {
                Ok(state) => state,
                Err(e) => {
                    let message = format!("Failed to initialize application runtime.\n\n{e:#}");
                    tracing::error!("{message}");
                    write_startup_error_log(&config_dir, &message);
                    show_startup_error_dialog(app, &message);
                    return Err(anyhow::anyhow!(message).into());
                }
            };

            tracing::info!(
                "Identity ready: fp={}, name={}",
                state.identity.fingerprint,
                state.identity.device_name
            );

            let launch_at_login_enabled =
                tauri::async_runtime::block_on(async { state.config.read().await.launch_at_login });
            if let Err(err) =
                crate::runtime::autostart::sync_launch_at_login(app.handle(), launch_at_login_enabled)
            {
                tracing::warn!("failed to sync launch-at-login registration during startup: {err}");
            }

            tauri::async_runtime::block_on(async {
                *state.requested_control_plane_mode.write().await =
                    requested_mode.as_env_value().to_string();
                *state.control_plane_mode.write().await = actual_mode.as_env_value().to_string();
                *state.runtime_profile.write().await = runtime_profile_name().to_string();
                *state.daemon_status.write().await =
                    if actual_mode == RequestedControlPlaneMode::Daemon {
                        daemon_resolution.status.clone()
                    } else if requested_mode == RequestedControlPlaneMode::Daemon {
                        format!("fallback:{}", daemon_resolution.status)
                    } else if runtime_profile_name() == "dev" {
                        "dev_default_in_process".to_string()
                    } else {
                        "in_process".to_string()
                    };
                *state.daemon_connect_attempts.write().await = daemon_resolution.connect_attempts;
                *state.daemon_connect_strategy.write().await =
                    daemon_resolution.connect_strategy.clone();
                *state.daemon_binary_path.write().await = daemon_resolution
                    .binary_path
                    .as_ref()
                    .map(|path| path.display().to_string());
            });

            let host = TauriRuntimeHost::shared(handle.clone(), Arc::clone(&state));

            if !crypto::secret_store::secure_store_available() {
                host.emit_json("system_error", serde_json::json!({
                    "subsystem": "security",
                    "message": "System secure key store unavailable. Falling back to local key file storage."
                })).ok();
                if let Ok(db) = state.db.lock() {
                    let _ = db::log_security_event(
                        &db,
                        "key_storage_degraded",
                        "startup",
                        Some(&state.identity.fingerprint),
                        "secure key store unavailable; using local key file storage",
                    );
                }
            }

            emit_daemon_startup_warning(&host, requested_mode, actual_mode, &daemon_resolution);

            // Manage state with Tauri
            app.manage(Arc::clone(&state));
            if actual_mode == RequestedControlPlaneMode::Daemon {
                tracing::info!(
                    requested = requested_mode.as_env_value(),
                    "Attached UI shell to daemon control plane"
                );
                match crate::daemon::server::spawn(
                    Arc::clone(&state),
                    Some(handle.clone()),
                    &config_dir,
                    LocalIpcEndpointKind::UiActivation,
                ) {
                    Ok(activation_server) => {
                        app.manage(activation_server);
                        runtime::supervisor::dispatch_startup_share(
                            Arc::clone(&host),
                            startup_share_paths,
                        );
                        runtime::supervisor::dispatch_startup_pairing_links(
                            host,
                            startup_pairing_links,
                        );
                    }
                    Err(err) => {
                        tracing::error!("Failed to start UI activation server: {err:#}");
                        handle
                            .emit(
                                "system_error",
                                serde_json::json!({
                                    "code": "UI_ACTIVATION_SERVER_START_FAILED",
                                    "subsystem": "ui_activation",
                                    "message": format!("UI activation handoff unavailable: {err:#}")
                                }),
                            )
                            .ok();
                    }
                }
            } else {
                if requested_mode == RequestedControlPlaneMode::Daemon {
                    tracing::warn!("Explicit daemon mode requested but dashdropd was unavailable; falling back to in-process runtime");
                }
                match runtime::supervisor::start(Arc::clone(&state), Arc::clone(&host), &config_dir) {
                    Ok(runtime) => {
                        app.manage(runtime);
                        runtime::supervisor::dispatch_startup_share(
                            Arc::clone(&host),
                            startup_share_paths,
                        );
                        runtime::supervisor::dispatch_startup_pairing_links(
                            host,
                            startup_pairing_links,
                        );
                    }
                    Err(e) => {
                        tracing::error!("Failed to start runtime supervisor: {e:#}");
                        handle
                            .emit(
                                "system_error",
                                serde_json::json!({
                                    "code": "RUNTIME_SUPERVISOR_START_FAILED",
                                    "subsystem": "runtime_supervisor",
                                    "message": format!("Runtime supervisor unavailable: {e:#}")
                                }),
                            )
                            .ok();
                    }
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_devices,
            commands::get_trusted_peers,
            commands::get_pending_incoming_requests,
            commands::get_control_plane_mode,
            commands::pair_device,
            commands::unpair_device,
            commands::set_trusted_alias,
            commands::confirm_trusted_peer_verification,
            commands::send_files_cmd,
            commands::connect_by_address,
            commands::accept_transfer,
            commands::accept_and_pair_transfer,
            commands::reject_transfer,
            commands::cancel_transfer,
            commands::cancel_all_transfers,
            commands::retry_transfer,
            commands::open_transfer_folder,
            commands::get_app_config,
            commands::set_app_config,
            commands::get_local_identity,
            commands::get_local_ble_assist_capsule,
            commands::ingest_ble_assist_capsule,
            pairing::get_local_pairing_link,
            pairing::validate_pairing_input,
            commands::get_transfers,
            commands::get_transfer,
            commands::get_transfer_history,
            commands::get_security_events,
            commands::get_security_posture,
            commands::get_runtime_status,
            commands::get_runtime_events,
            commands::get_runtime_event_checkpoint,
            commands::set_runtime_event_checkpoint,
            commands::copy_to_clipboard,
            commands::set_shell_attention_state,
            commands::get_discovery_diagnostics,
            commands::get_transfer_metrics,
        ])
        .build(tauri::generate_context!())
    {
        Ok(app) => app,
        Err(e) => {
            let message = format!("Fatal runtime error while running DashDrop:\n\n{e}");
            eprintln!("{message}");
            let fallback = std::env::temp_dir().join("dashdrop-startup-error.log");
            let _ = std::fs::write(fallback, message);
            return;
        }
    };

    app.run(|app_handle, event| {
        if let tauri::RunEvent::ExitRequested { .. } = event {
            tauri::async_runtime::block_on(
                commands::best_effort_revoke_cached_daemon_access_grant(app_handle),
            );
        }

        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen {
            has_visible_windows,
            ..
        } = event
        {
            if daemon_control_plane_mode_enabled(app_handle) && !has_visible_windows {
                reveal_main_window(app_handle);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{
        append_dev_daemon_search_directories, collect_pairing_links_from_url_strings,
        daemon_control_plane_mode_from_sources, headless_idle_exit_timeout_secs_from_env,
    };
    use std::path::{Path, PathBuf};

    #[test]
    fn daemon_control_plane_mode_prefers_runtime_state_over_env() {
        assert!(daemon_control_plane_mode_from_sources(
            Some("daemon"),
            Some("in_process")
        ));
        assert!(!daemon_control_plane_mode_from_sources(
            Some("in_process"),
            Some("daemon")
        ));
    }

    #[test]
    fn daemon_control_plane_mode_falls_back_to_env_and_defaults_false() {
        assert!(daemon_control_plane_mode_from_sources(None, Some("daemon")));
        assert!(!daemon_control_plane_mode_from_sources(
            None,
            Some("in_process")
        ));
        assert!(!daemon_control_plane_mode_from_sources(None, None));
    }

    #[test]
    fn headless_idle_exit_timeout_ignores_invalid_values() {
        assert_eq!(
            headless_idle_exit_timeout_secs_from_env(Some("300")),
            Some(300)
        );
        assert_eq!(headless_idle_exit_timeout_secs_from_env(Some("0")), None);
        assert_eq!(headless_idle_exit_timeout_secs_from_env(Some("-1")), None);
        assert_eq!(headless_idle_exit_timeout_secs_from_env(Some("abc")), None);
        assert_eq!(headless_idle_exit_timeout_secs_from_env(None), None);
    }

    #[test]
    fn daemon_control_plane_mode_ignores_empty_or_invalid_state_values() {
        assert!(daemon_control_plane_mode_from_sources(
            Some(" "),
            Some("daemon")
        ));
        assert!(daemon_control_plane_mode_from_sources(
            Some("invalid"),
            Some("daemon")
        ));
        assert!(!daemon_control_plane_mode_from_sources(
            Some("invalid"),
            Some("in_process")
        ));
    }

    #[test]
    fn dev_daemon_search_directories_include_workspace_and_manifest_targets() {
        let mut directories = Vec::new();
        append_dev_daemon_search_directories(
            &mut directories,
            Some(Path::new("/tmp/dashdrop-workspace")),
            Path::new("/tmp/dashdrop-workspace/src-tauri"),
        );

        let expected = [
            PathBuf::from("/tmp/dashdrop-workspace/src-tauri/binaries"),
            PathBuf::from("/tmp/dashdrop-workspace/src-tauri/target/debug"),
            PathBuf::from("/tmp/dashdrop-workspace/src-tauri/target/release"),
            PathBuf::from("/tmp/dashdrop-workspace/binaries"),
            PathBuf::from("/tmp/dashdrop-workspace/target/debug"),
            PathBuf::from("/tmp/dashdrop-workspace/target/release"),
        ];

        for path in expected {
            assert!(
                directories.contains(&path),
                "expected dev daemon search directories to include {}",
                path.display()
            );
        }
    }

    #[test]
    fn dev_daemon_search_directories_dedupe_manifest_and_current_src_tauri_paths() {
        let mut directories = Vec::new();
        append_dev_daemon_search_directories(
            &mut directories,
            Some(Path::new("/tmp/dashdrop-workspace")),
            Path::new("/tmp/dashdrop-workspace/src-tauri"),
        );

        let src_tauri_binaries = PathBuf::from("/tmp/dashdrop-workspace/src-tauri/binaries");
        let count = directories
            .iter()
            .filter(|path| **path == src_tauri_binaries)
            .count();

        assert_eq!(count, 1, "src-tauri binaries path should be unique");
    }

    #[test]
    fn dashdropd_binary_file_name_matches_platform() {
        let file_name = super::dashdropd_binary_file_name();
        if cfg!(windows) {
            assert_eq!(file_name, "dashdropd.exe");
        } else {
            assert_eq!(file_name, "dashdropd");
        }
    }

    #[test]
    fn collect_pairing_links_from_url_strings_filters_pair_links() {
        let urls = vec![
            "dashdrop://pair?data=abc",
            "https://example.com/pair",
            " dashdrop://pair?data=def ",
        ];

        assert_eq!(
            collect_pairing_links_from_url_strings(urls),
            vec![
                "dashdrop://pair?data=abc".to_string(),
                "dashdrop://pair?data=def".to_string()
            ]
        );
    }
}
