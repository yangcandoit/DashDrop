use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use crate::state::{AppState, DeviceInfo, TrustedPeer};
use crate::transport::connect_to_peer;
use crate::transport::sender::send_files;

type AppStateRef<'a> = State<'a, Arc<AppState>>;

// ─── Device commands ────────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_devices(state: AppStateRef<'_>) -> Result<Vec<DeviceInfo>, String> {
    let devices = state.devices.read().await;
    Ok(devices.values().cloned().collect())
}

#[tauri::command]
pub async fn get_trusted_peers(state: AppStateRef<'_>) -> Result<Vec<TrustedPeer>, String> {
    let trusted = state.trusted_peers.read().await;
    Ok(trusted.values().cloned().collect())
}

#[tauri::command]
pub async fn pair_device(fp: String, app: AppHandle, state: AppStateRef<'_>) -> Result<(), String> {
    let name = {
        let devices = state.devices.read().await;
        devices.get(&fp).map(|d| d.name.clone()).unwrap_or_else(|| "Unknown".into())
    };
    state.add_trust(fp, name).await;
    {
        let trusted = state.trusted_peers.read().await;
        let config = state.config.read().await;
        crate::persistence::save_state(&app, &config, &trusted)
            .map_err(|e| format!("failed to persist pairing data: {e:#}"))?;
    }
    Ok(())
}

#[tauri::command]
pub async fn unpair_device(fp: String, app: AppHandle, state: AppStateRef<'_>) -> Result<(), String> {
    state.trusted_peers.write().await.remove(&fp);
    {
        let trusted = state.trusted_peers.read().await;
        let config = state.config.read().await;
        crate::persistence::save_state(&app, &config, &trusted)
            .map_err(|e| format!("failed to persist pairing data: {e:#}"))?;
    }
    Ok(())
}

// ─── Transfer commands ───────────────────────────────────────────────────────

#[tauri::command]
pub async fn send_files_cmd(
    peer_fp: String,
    paths: Vec<String>,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    let state = Arc::clone(&state);

    // Get peer addresses
    let remote_addrs = {
        let devices = state.devices.read().await;
        let device = devices.get(&peer_fp)
            .ok_or_else(|| format!("device {peer_fp} not found"))?;
        device.best_addrs().ok_or_else(|| "device has no reachable address".to_string())?
    };

    let path_bufs: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();

    // Validate files exist before attempting network connection
    for p in &path_bufs {
        if !p.exists() {
            return Err(format!("Source path does not exist: {}", p.display()));
        }
    }

    // Connect (try available addrs)
    let mut conn_opt = None;
    for addr in remote_addrs {
        tracing::debug!("Trying to connect to {}", addr);
        match connect_to_peer(&state, addr).await {
            Ok(c) => {
                conn_opt = Some(c);
                break;
            }
            Err(e) => {
                tracing::warn!("Failed to connect to {}: {e:#}", addr);
            }
        }
    }
    
    let conn = conn_opt.ok_or_else(|| "All connection attempts failed".to_string())?;

    // Hard-bind selected device fingerprint to the peer certificate.
    let (fp_match, actual_fp) = crate::transport::handshake::peer_fp_matches(&conn, &peer_fp)
        .map_err(|e| format!("failed to verify peer identity: {e:#}"))?;
    if !fp_match {
        conn.close(quinn::VarInt::from_u32(2), b"identity mismatch");
        app.emit("identity_mismatch", serde_json::json!({
            "expected_fp": peer_fp.clone(),
            "actual_fp": actual_fp,
            "phase": "connect",
        })).ok();
        return Err("identity mismatch: peer certificate does not match selected device".to_string());
    }

    // Spawn sender task
    tokio::spawn(async move {
        let outcome = send_files(peer_fp, path_bufs, conn, app.clone(), state).await;
        match outcome {
            Ok(crate::transport::protocol::TransferOutcome::Success) => {
                tracing::info!("Transfer complete");
            }
            Ok(other) => {
                tracing::warn!("Transfer ended: {:?}", other);
            }
            Err(e) => {
                tracing::error!("Send failed: {e:#}");
                app.emit("transfer_error", serde_json::json!({
                    "transfer_id": serde_json::Value::Null,
                    "reason": e.to_string(),
                    "phase": "send",
                })).ok();
            }
        }
    });

    Ok(())
}

#[tauri::command]
pub async fn accept_transfer(
    transfer_id: String,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    let tx = state.pending_accepts.write().await.remove(&transfer_id);
    if let Some(sender) = tx {
        let _ = sender.send(true);
        Ok(())
    } else {
        Err(format!("transfer {transfer_id} not found or already handled"))
    }
}

#[tauri::command]
pub async fn accept_and_pair_transfer(
    transfer_id: String,
    sender_fp: String,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    // Pair first
    pair_device(sender_fp, app, State::clone(&state)).await?;
    // Then accept
    accept_transfer(transfer_id, state).await
}

#[tauri::command]
pub async fn reject_transfer(
    transfer_id: String,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    let tx = state.pending_accepts.write().await.remove(&transfer_id);
    if let Some(sender) = tx {
        let _ = sender.send(false);
    }
    Ok(())
}

#[tauri::command]
pub async fn cancel_transfer(
    transfer_id: String,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    // Signal rejection (cancel while in-progress is handled by dropping connection)
    let tx = state.pending_accepts.write().await.remove(&transfer_id);
    if let Some(sender) = tx {
        let _ = sender.send(false);
    }
    // Update status and close connection if active
    let mut transfers = state.transfers.write().await;
    if let Some(t) = transfers.get_mut(&transfer_id) {
        t.status = crate::state::TransferStatus::Cancelled;
        t.ended_at = Some(std::time::Instant::now());
        if let Some(conn) = &t.conn {
            if let Ok(mut cancel_stream) = conn.open_uni().await {
                let _ = crate::transport::protocol::write_message(
                    &mut cancel_stream,
                    &crate::transport::protocol::DashMessage::Cancel(
                        crate::transport::protocol::CancelPayload {
                            reason: crate::transport::protocol::CancelReason::UserCancelled,
                        },
                    ),
                )
                .await;
                let _ = cancel_stream.finish();
            }
            conn.close(quinn::VarInt::from_u32(1), b"Cancelled by user");
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn open_transfer_folder(
    transfer_id: String,
    state: AppStateRef<'_>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    use tauri::Manager;
    let custom_dir = state.config.read().await.download_dir.clone();
    let base_dir = custom_dir.map(std::path::PathBuf::from).unwrap_or_else(|| {
        app.path().download_dir()
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                std::path::PathBuf::from(home).join("Downloads")
            })
    });
    let save_root = base_dir.join("DashDrop").join(transfer_id);

    use tauri_plugin_opener::OpenerExt;
    app.opener().reveal_item_in_dir(&save_root)
        .or_else(|_| app.opener().open_path(save_root.to_string_lossy().to_string(), None::<&str>))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_app_config(state: AppStateRef<'_>) -> Result<crate::state::AppConfig, String> {
    Ok(state.config.read().await.clone())
}

#[tauri::command]
pub async fn set_app_config(
    config: crate::state::AppConfig,
    app: AppHandle,
    state: AppStateRef<'_>,
) -> Result<(), String> {
    let old_name = state.config.read().await.device_name.clone();
    if let Some(download_dir) = &config.download_dir {
        let dir = std::path::PathBuf::from(download_dir);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("download directory is not usable: {e}"))?;
        let probe = dir.join(".dashdrop-write-test");
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&probe)
            .map_err(|e| format!("download directory is not writable: {e}"))?;
        let _ = std::fs::remove_file(probe);
    }

    let previous_config = state.config.read().await.clone();
    *state.config.write().await = config;
    if old_name != state.config.read().await.device_name {
        if let Err(e) = crate::discovery::service::reregister_service(Arc::clone(&state)).await {
            *state.config.write().await = previous_config;
            return Err(format!("device name updated locally but mDNS refresh failed: {e:#}"));
        }
    }
    {
        let trusted = state.trusted_peers.read().await;
        let config = state.config.read().await;
        crate::persistence::save_state(&app, &config, &trusted)
            .map_err(|e| format!("failed to persist app config: {e:#}"))?;
    }
    Ok(())
}

// ─── App info commands ───────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_local_identity(state: AppStateRef<'_>) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "fingerprint": state.identity.fingerprint,
        "device_name": state.config.read().await.device_name,
        "port": *state.local_port.read().await,
    }))
}

#[tauri::command]
pub async fn get_transfers(state: AppStateRef<'_>) -> Result<Vec<crate::state::TransferTask>, String> {
    let transfers = state.transfers.read().await;
    Ok(transfers.values().cloned().collect())
}
