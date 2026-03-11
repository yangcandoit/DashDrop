use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use tauri::AppHandle;
use tokio::sync::oneshot;

use crate::core_service::AppCoreService;
use crate::local_ipc::{
    decode_message, read_frame_bytes, resolve_local_ipc_endpoint, write_framed_message,
    LocalIpcCodecError, LocalIpcEndpoint, LocalIpcError, LocalIpcWireRequest, LocalIpcWireResponse,
    LOCAL_IPC_PROTO_VERSION,
};
use crate::state::AppState;

pub struct LocalIpcServerHandle {
    endpoint: LocalIpcEndpoint,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
    task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

impl LocalIpcServerHandle {
    pub fn endpoint(&self) -> &LocalIpcEndpoint {
        &self.endpoint
    }

    #[allow(dead_code)]
    pub async fn shutdown(&self) {
        if let Some(sender) = self.shutdown.lock().take() {
            let _ = sender.send(());
        }
        let task = { self.task.lock().take() };
        if let Some(task) = task {
            let _ = task.await;
        }
        cleanup_endpoint(&self.endpoint);
    }
}

impl Drop for LocalIpcServerHandle {
    fn drop(&mut self) {
        if let Some(sender) = self.shutdown.get_mut().take() {
            let _ = sender.send(());
        }
        if let Some(task) = self.task.get_mut().take() {
            task.abort();
        }
        cleanup_endpoint(&self.endpoint);
    }
}

pub fn spawn(
    state: Arc<AppState>,
    app: Option<AppHandle>,
    config_dir: &Path,
) -> Result<LocalIpcServerHandle> {
    let endpoint = resolve_local_ipc_endpoint(config_dir);

    #[cfg(unix)]
    {
        let listener = bind_unix_listener(&endpoint)?;
        let endpoint_for_task = endpoint.clone();
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let task = tauri::async_runtime::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => {
                        match accepted {
                            Ok((stream, _)) => {
                                let state = Arc::clone(&state);
                                let app = app.clone();
                                tauri::async_runtime::spawn(async move {
                                    if let Err(err) = handle_unix_connection(stream, state, app).await {
                                        tracing::warn!("local IPC connection ended with error: {err:#}");
                                    }
                                });
                            }
                            Err(err) => {
                                tracing::warn!("local IPC accept failed: {err}");
                            }
                        }
                    }
                }
            }

            cleanup_endpoint(&endpoint_for_task);
        });

        Ok(LocalIpcServerHandle {
            endpoint,
            shutdown: Mutex::new(Some(shutdown_tx)),
            task: Mutex::new(Some(task)),
        })
    }

    #[cfg(windows)]
    {
        let LocalIpcEndpoint::WindowsNamedPipe { path } = &endpoint else {
            unreachable!("windows local IPC endpoint must resolve to named pipe");
        };

        Err(anyhow!(
            "windows named pipe server placeholder is not implemented yet ({path})"
        ))
    }

    #[cfg(not(any(unix, windows)))]
    {
        Err(anyhow!("local IPC is unsupported on this platform"))
    }
}

#[cfg(unix)]
async fn handle_unix_connection(
    mut stream: tokio::net::UnixStream,
    state: Arc<AppState>,
    app: Option<AppHandle>,
) -> Result<()> {
    loop {
        let frame = match read_frame_bytes(&mut stream).await {
            Ok(frame) => frame,
            Err(LocalIpcCodecError::Io(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(());
            }
            Err(err) => return Err(err.into()),
        };

        let request: LocalIpcWireRequest = match decode_message(&frame) {
            Ok(request) => request,
            Err(err) => {
                let response = LocalIpcWireResponse::error(
                    "decode-error",
                    LOCAL_IPC_PROTO_VERSION,
                    LocalIpcError::invalid_request(err.to_string()),
                );
                write_framed_message(&mut stream, &response)
                    .await
                    .context("write decode error response")?;
                continue;
            }
        };

        let response = dispatch_request(request, Arc::clone(&state), app.clone()).await;
        write_framed_message(&mut stream, &response)
            .await
            .context("write local IPC response")?;
    }
}

async fn dispatch_request(
    request: LocalIpcWireRequest,
    state: Arc<AppState>,
    app: Option<AppHandle>,
) -> LocalIpcWireResponse {
    let service = if let Some(app) = app {
        AppCoreService::with_app(state, app)
    } else {
        AppCoreService::new(state)
    };

    service.dispatch_wire(&request).await
}

#[cfg(unix)]
fn bind_unix_listener(endpoint: &LocalIpcEndpoint) -> Result<tokio::net::UnixListener> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    let path = match endpoint {
        LocalIpcEndpoint::UnixSocket { path } => path,
    };

    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("local IPC socket path has no parent: {}", path.display()))?;
    if !parent.exists() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create local IPC runtime dir {}", parent.display()))?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("chmod local IPC runtime dir {}", parent.display()))?;
    }

    if path.exists() {
        let metadata = std::fs::symlink_metadata(path)
            .with_context(|| format!("read existing local IPC socket {}", path.display()))?;
        if metadata.file_type().is_socket() {
            std::fs::remove_file(path)
                .with_context(|| format!("remove stale local IPC socket {}", path.display()))?;
        } else {
            return Err(anyhow!(
                "refusing to replace non-socket local IPC path {}",
                path.display()
            ));
        }
    }

    let listener = tokio::net::UnixListener::bind(path)
        .with_context(|| format!("bind local IPC socket {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod local IPC socket {}", path.display()))?;
    Ok(listener)
}

fn cleanup_endpoint(endpoint: &LocalIpcEndpoint) {
    #[cfg(unix)]
    {
        let path = match endpoint {
            LocalIpcEndpoint::UnixSocket { path } => path,
        };
        let _ = std::fs::remove_file(path);
    }
}
