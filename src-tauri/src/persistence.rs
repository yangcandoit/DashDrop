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

fn state_file_path_for_config_dir(config_dir: &std::path::Path) -> PathBuf {
    config_dir.join("state.json")
}

pub fn load_state_at(config_dir: &std::path::Path) -> Result<PersistedState> {
    let path = state_file_path_for_config_dir(config_dir);
    if !path.exists() {
        return Ok(PersistedState::default());
    }
    let bytes = fs::read(&path).context("read persisted state")?;
    let state: PersistedState = serde_json::from_slice(&bytes).context("parse persisted state")?;
    Ok(state)
}

#[allow(dead_code)]
pub fn load_state(app: &AppHandle) -> Result<PersistedState> {
    let config_dir = std::env::var("DASHDROP_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            app.path()
                .app_config_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("dashdrop")
        });
    load_state_at(&config_dir)
}
