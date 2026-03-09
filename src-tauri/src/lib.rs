mod commands;
mod crypto;
pub mod db;
pub mod discovery;
mod dto;
mod persistence;
pub mod state;
pub mod transport;

use crypto::Identity;
use state::{AppConfig, AppState};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

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

fn resolve_config_dir<R: tauri::Runtime, T: tauri::Manager<R>>(app: &T) -> anyhow::Result<PathBuf> {
    if let Ok(override_dir) = std::env::var("DASHDROP_CONFIG_DIR") {
        return Ok(PathBuf::from(override_dir));
    }

    let base = app
        .path()
        .app_config_dir()
        .map_err(|e| anyhow::anyhow!("failed to resolve app config directory: {e}"))?;
    Ok(base.join("dashdrop"))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .setup(|app| {
            let handle = app.handle().clone();

            // Config directory: ~/.config/dashdrop/ (or via env var for testing)
            let config_dir = match resolve_config_dir(app) {
                Ok(path) => path,
                Err(e) => {
                    let message = format!("Failed to resolve configuration directory.\n\n{e:#}");
                    tracing::error!("{message}");
                    show_startup_error_dialog(app, &message);
                    return Err(anyhow::anyhow!(message).into());
                }
            };

            // Initialize identity
            let identity = match Identity::load_or_create(&config_dir) {
                Ok(identity) => identity,
                Err(e) => {
                    let message = format!("Failed to initialize identity.\n\n{e:#}");
                    tracing::error!("{message}");
                    write_startup_error_log(&config_dir, &message);
                    show_startup_error_dialog(app, &message);
                    return Err(anyhow::anyhow!(message).into());
                }
            };

            tracing::info!(
                "Identity ready: fp={}, name={}",
                identity.fingerprint,
                identity.device_name
            );

            let db_conn = match db::init_db(&handle) {
                Ok(conn) => conn,
                Err(e) => {
                    let message = format!("Failed to initialize local database.\n\n{e:#}");
                    tracing::error!("{message}");
                    write_startup_error_log(&config_dir, &message);
                    show_startup_error_dialog(app, &message);
                    return Err(anyhow::anyhow!(message).into());
                }
            };
            let mut db_config = db::load_app_config(&db_conn).unwrap_or(None);
            let mut db_trusted = db::load_trusted_peers(&db_conn).unwrap_or_default();

            if db_config.is_none() && db_trusted.is_empty() {
                if let Ok(legacy) = persistence::load_state(&handle) {
                    if let Some(cfg) = legacy.app_config.clone() {
                        let _ = db::save_app_config(&db_conn, &cfg);
                        db_config = Some(cfg);
                    }
                    if !legacy.trusted_peers.is_empty() {
                        let trusted_map: std::collections::HashMap<String, state::TrustedPeer> =
                            legacy
                                .trusted_peers
                                .iter()
                                .cloned()
                                .map(|peer| (peer.fingerprint.clone(), peer))
                                .collect();
                        let _ = db::replace_trusted_peers(&db_conn, &trusted_map);
                        db_trusted = legacy.trusted_peers;
                    }
                }
            }

            // Create AppState
            let mut config = db_config.unwrap_or_else(|| AppConfig {
                device_name: identity.device_name.clone(),
                ..Default::default()
            });
            if config.device_name.trim().is_empty() {
                config.device_name = identity.device_name.clone();
            }

            let state = Arc::new(AppState::new(identity, config, db_conn));
            if !crypto::secret_store::secure_store_available() {
                handle.emit("system_error", serde_json::json!({
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
            tauri::async_runtime::block_on(async {
                let mut trusted = state.trusted_peers.write().await;
                trusted.clear();
                for peer in db_trusted {
                    trusted.insert(peer.fingerprint.clone(), peer);
                }
            });

            // Manage state with Tauri
            app.manage(Arc::clone(&state));

            // Start async subsystems
            let state2 = Arc::clone(&state);
            let handle2 = handle.clone();
            tauri::async_runtime::spawn(async move {
                // 1. Start QUIC server → get port
                let port = match transport::start_server(
                    state2.identity.clone(),
                    handle2.clone(),
                    Arc::clone(&state2),
                ).await {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::error!("Failed to start QUIC server: {e:#}");
                        handle2.emit("system_error", serde_json::json!({
                            "code": "QUIC_SERVER_START_FAILED",
                            "subsystem": "quic_server",
                            "message": format!("Failed to start network listener: {e:#}")
                        })).ok();
                        return;
                    }
                };

                tracing::info!("QUIC server on port {port}");

                // 2. Register mDNS (now we have the real port)
                let mdns = match discovery::register_service(Arc::clone(&state2)).await {
                    Ok(d) => Arc::new(d),
                    Err(e) => {
                        tracing::error!("Failed to register mDNS: {e:#}");
                        handle2.emit("system_error", serde_json::json!({
                            "code": "MDNS_REGISTER_FAILED",
                            "subsystem": "mdns",
                            "message": format!("Device discovery unavailable: {e:#}. Other devices won't see you.")
                        })).ok();
                        return;
                    }
                };

                // Store mDNS in state to keep it alive
                let _ = state2.mdns.set(Arc::clone(&mdns));

                // 3. Start mDNS browser
                if let Err(e) = discovery::start_browser(mdns, handle2.clone(), state2).await {
                    tracing::error!("Failed to start mDNS browser: {e:#}");
                    handle2.emit("system_error", serde_json::json!({
                        "code": "MDNS_BROWSER_FAILED",
                        "subsystem": "mdns_browser",
                        "message": format!("Scanning for nearby devices failed: {e:#}")
                    })).ok();
                }
            });

            // Start Memory Cleanup Background loop
            let state_cleanup = Arc::clone(&state);
            tauri::async_runtime::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
                loop {
                    interval.tick().await;
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let mut transfers = state_cleanup.transfers.write().await;
                    transfers.retain(|_, t| {
                        if let Some(ended) = t.ended_at_unix {
                            now.saturating_sub(ended) < 60
                        } else {
                            true
                        }
                    });
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_devices,
            commands::get_trusted_peers,
            commands::pair_device,
            commands::unpair_device,
            commands::set_trusted_alias,
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
            commands::get_transfers,
            commands::get_transfer,
            commands::get_transfer_history,
            commands::get_security_events,
            commands::get_security_posture,
            commands::get_runtime_status,
            commands::get_transfer_metrics,
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            let message = format!("Fatal runtime error while running DashDrop:\n\n{e}");
            eprintln!("{message}");
            let fallback = std::env::temp_dir().join("dashdrop-startup-error.log");
            let _ = std::fs::write(fallback, message);
        });
}
