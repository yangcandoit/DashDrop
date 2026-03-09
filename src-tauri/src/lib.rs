mod crypto;
mod state;
mod discovery;
mod transport;
mod commands;
mod persistence;

use std::path::PathBuf;
use std::sync::Arc;
use state::{AppConfig, AppState};
use crypto::Identity;
use tauri::{Manager, Emitter};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
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
            let config_dir = std::env::var("DASHDROP_CONFIG_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    app.path()
                        .app_config_dir()
                        .unwrap_or_else(|_| PathBuf::from("."))
                        .join("dashdrop")
                });

            // Initialize identity
            let identity = Identity::load_or_create(&config_dir)
                .expect("Failed to initialize identity");

            tracing::info!(
                "Identity ready: fp={}, name={}",
                identity.fingerprint,
                identity.device_name
            );

            let persisted = persistence::load_state(&handle).unwrap_or_default();

            // Create AppState
            let mut config = persisted.app_config.unwrap_or_else(|| AppConfig {
                device_name: identity.device_name.clone(),
                ..Default::default()
            });
            if config.device_name.trim().is_empty() {
                config.device_name = identity.device_name.clone();
            }
            let state = Arc::new(AppState::new(identity, config));
            tauri::async_runtime::block_on(async {
                let mut trusted = state.trusted_peers.write().await;
                trusted.clear();
                for peer in persisted.trusted_peers {
                    trusted.insert(peer.fingerprint.clone(), peer);
                }
            });

            // Manage state with Tauri
            app.manage(Arc::clone(&state));

            // Start async subsystems
            let state2 = Arc::clone(&state);
            let handle2 = handle.clone();
            tokio::spawn(async move {
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
                        "subsystem": "mdns_browser",
                        "message": format!("Scanning for nearby devices failed: {e:#}")
                    })).ok();
                }
            });

            // Start Memory Cleanup Background loop
            let state_cleanup = Arc::clone(&state);
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
                loop {
                    interval.tick().await;
                    let mut transfers = state_cleanup.transfers.write().await;
                    transfers.retain(|_, t| {
                        if let Some(ended) = t.ended_at {
                            ended.elapsed() < std::time::Duration::from_secs(crate::transport::protocol::USER_RESPONSE_TIMEOUT_SECS)
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
            commands::send_files_cmd,
            commands::accept_transfer,
            commands::accept_and_pair_transfer,
            commands::reject_transfer,
            commands::cancel_transfer,
            commands::open_transfer_folder,
            commands::get_app_config,
            commands::set_app_config,
            commands::get_local_identity,
            commands::get_transfers,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
