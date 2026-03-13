use std::path::PathBuf;
use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};

use crate::state::AppState;

pub trait RuntimeHost: Send + Sync {
    fn emit_json(&self, event: &str, payload: serde_json::Value) -> Result<(), String>;
    fn reveal_main_window(&self) -> Result<(), String>;
    fn download_dir(&self) -> Option<PathBuf>;
    fn app_handle(&self) -> Option<AppHandle>;
}

pub struct TauriRuntimeHost {
    app: AppHandle,
    state: Arc<AppState>,
}

impl TauriRuntimeHost {
    pub fn shared(app: AppHandle, state: Arc<AppState>) -> Arc<dyn RuntimeHost> {
        Arc::new(Self { app, state })
    }
}

impl RuntimeHost for TauriRuntimeHost {
    fn emit_json(&self, event: &str, payload: serde_json::Value) -> Result<(), String> {
        self.state.record_runtime_event(event, payload.clone());
        self.app
            .emit(event, payload)
            .map_err(|e| format!("failed to emit {event}: {e}"))
    }

    fn reveal_main_window(&self) -> Result<(), String> {
        if let Some(window) = self.app.get_webview_window("main") {
            window
                .show()
                .map_err(|e| format!("failed to show main window: {e}"))?;
            window
                .set_focus()
                .map_err(|e| format!("failed to focus main window: {e}"))?;
        }
        self.emit_json(
            "app_window_revealed",
            serde_json::json!({
                "source": "runtime_host",
            }),
        )?;
        Ok(())
    }

    fn download_dir(&self) -> Option<PathBuf> {
        self.app.path().download_dir().ok()
    }

    fn app_handle(&self) -> Option<AppHandle> {
        Some(self.app.clone())
    }
}

#[allow(dead_code)]
pub struct NoopRuntimeHost {
    state: Arc<AppState>,
}

#[allow(dead_code)]
impl NoopRuntimeHost {
    pub fn shared(state: Arc<AppState>) -> Arc<dyn RuntimeHost> {
        Arc::new(Self { state })
    }
}

impl RuntimeHost for NoopRuntimeHost {
    fn emit_json(&self, event: &str, payload: serde_json::Value) -> Result<(), String> {
        self.state.record_runtime_event(event, payload);
        Ok(())
    }

    fn reveal_main_window(&self) -> Result<(), String> {
        Ok(())
    }

    fn download_dir(&self) -> Option<PathBuf> {
        None
    }

    fn app_handle(&self) -> Option<AppHandle> {
        None
    }
}
