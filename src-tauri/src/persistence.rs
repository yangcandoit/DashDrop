use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::state::{AppConfig, TrustedPeer};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PersistedState {
    pub app_config: Option<AppConfig>,
    pub trusted_peers: Vec<TrustedPeer>,
}

fn state_file_path(app: &AppHandle) -> PathBuf {
    let config_dir = std::env::var("DASHDROP_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            app.path()
                .app_config_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("dashdrop")
        });
    config_dir.join("state.json")
}

pub fn load_state(app: &AppHandle) -> Result<PersistedState> {
    let path = state_file_path(app);
    if !path.exists() {
        return Ok(PersistedState::default());
    }
    let bytes = fs::read(&path).context("read persisted state")?;
    let state: PersistedState = serde_json::from_slice(&bytes).context("parse persisted state")?;
    Ok(state)
}

pub fn save_state(
    app: &AppHandle,
    app_config: &AppConfig,
    trusted_peers: &HashMap<String, TrustedPeer>,
) -> Result<()> {
    let path = state_file_path(app);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("create persistence directory")?;
    }
    let payload = PersistedState {
        app_config: Some(app_config.clone()),
        trusted_peers: trusted_peers.values().cloned().collect(),
    };
    let bytes = serde_json::to_vec_pretty(&payload).context("serialize persisted state")?;
    fs::write(&path, bytes).context("write persisted state")?;
    Ok(())
}
