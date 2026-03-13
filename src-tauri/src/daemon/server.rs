use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use tauri::AppHandle;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::oneshot;

use crate::core_service::AppCoreService;
use crate::local_ipc::{
    decode_message, read_frame_bytes, resolve_local_ipc_endpoint_for_kind, write_framed_message,
    LocalIpcCodecError, LocalIpcCommand, LocalIpcEndpoint, LocalIpcEndpointKind, LocalIpcError,
    LocalIpcResponse, LocalIpcWireRequest, LocalIpcWireResponse, LOCAL_IPC_PROTO_VERSION,
};
use crate::state::AppState;

#[cfg(unix)]
#[derive(Clone, Copy)]
struct LocalPeerIdentity {
    uid: libc::uid_t,
    pid: Option<libc::pid_t>,
}

#[cfg(windows)]
#[derive(Clone)]
struct LocalPeerIdentity {
    sid: Vec<u8>,
}

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
    endpoint_kind: LocalIpcEndpointKind,
) -> Result<LocalIpcServerHandle> {
    let endpoint = resolve_local_ipc_endpoint_for_kind(config_dir, endpoint_kind);

    #[cfg(unix)]
    {
        let listener = bind_unix_listener(&endpoint)?;
        let endpoint_for_task = endpoint.clone();
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let task = tauri::async_runtime::spawn(async move {
            let listener = match tokio::net::UnixListener::from_std(listener) {
                Ok(listener) => listener,
                Err(err) => {
                    tracing::error!(
                        "failed to convert local IPC socket into tokio listener: {err}"
                    );
                    cleanup_endpoint(&endpoint_for_task);
                    return;
                }
            };

            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => {
                        match accepted {
                            Ok((stream, _)) => {
                                let state = Arc::clone(&state);
                                let app = app.clone();
                                let endpoint_kind = endpoint_kind;
                                tauri::async_runtime::spawn(async move {
                                    if let Err(err) =
                                        handle_unix_connection(stream, state, app, endpoint_kind)
                                            .await
                                    {
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
        let path = path.clone();
        let listener = create_windows_pipe_server(&path, true)
            .with_context(|| format!("bind local IPC named pipe {path}"))?;
        let endpoint_for_task = endpoint.clone();
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let task = tauri::async_runtime::spawn(async move {
            let mut listener = listener;
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.connect() => {
                        match accepted {
                            Ok(()) => {
                                let connected = listener;
                                listener = match create_windows_pipe_server(&path, false) {
                                    Ok(next_listener) => next_listener,
                                    Err(err) => {
                                        tracing::error!("failed to create next local IPC named pipe listener: {err:#}");
                                        break;
                                    }
                                };
                                let state = Arc::clone(&state);
                                let app = app.clone();
                                let endpoint_kind = endpoint_kind;
                                tauri::async_runtime::spawn(async move {
                                    if let Err(err) =
                                        handle_windows_connection(
                                            connected,
                                            state,
                                            app,
                                            endpoint_kind,
                                        )
                                            .await
                                    {
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

    #[cfg(not(any(unix, windows)))]
    {
        Err(anyhow!("local IPC is unsupported on this platform"))
    }
}

async fn handle_connection<S>(
    mut stream: S,
    state: Arc<AppState>,
    app: Option<AppHandle>,
    endpoint_kind: LocalIpcEndpointKind,
    #[allow(unused_variables)] peer_identity: Option<LocalPeerIdentity>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
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

        let response = dispatch_request(
            request,
            Arc::clone(&state),
            app.clone(),
            endpoint_kind,
            peer_identity,
        )
        .await;
        write_framed_message(&mut stream, &response)
            .await
            .context("write local IPC response")?;
    }
}

#[cfg(unix)]
async fn handle_unix_connection(
    stream: tokio::net::UnixStream,
    state: Arc<AppState>,
    app: Option<AppHandle>,
    endpoint_kind: LocalIpcEndpointKind,
) -> Result<()> {
    let credentials = stream
        .peer_cred()
        .context("read local IPC peer credentials")?;
    handle_connection(
        stream,
        state,
        app,
        endpoint_kind,
        Some(LocalPeerIdentity {
            uid: credentials.uid(),
            pid: credentials.pid(),
        }),
    )
    .await
}

#[cfg(windows)]
async fn handle_windows_connection(
    stream: tokio::net::windows::named_pipe::NamedPipeServer,
    state: Arc<AppState>,
    app: Option<AppHandle>,
    endpoint_kind: LocalIpcEndpointKind,
) -> Result<()> {
    let peer_identity = windows_named_pipe_peer_identity(&stream)
        .context("read local IPC named pipe peer identity")?;
    handle_connection(stream, state, app, endpoint_kind, Some(peer_identity)).await
}

async fn dispatch_request(
    request: LocalIpcWireRequest,
    state: Arc<AppState>,
    app: Option<AppHandle>,
    endpoint_kind: LocalIpcEndpointKind,
    #[allow(unused_variables)] peer_identity: Option<LocalPeerIdentity>,
) -> LocalIpcWireResponse {
    let service = if let Some(app) = app {
        AppCoreService::with_app(Arc::clone(&state), app)
    } else {
        AppCoreService::new(Arc::clone(&state))
    };

    let request_id = request.request_id.clone();
    let proto_version = request.proto_version;

    #[cfg(unix)]
    if let Some(identity) = peer_identity {
        let current_uid = unsafe { libc::geteuid() };
        if identity.uid != current_uid {
            let pid_hint = identity
                .pid
                .map(|pid| format!(" (pid {pid})"))
                .unwrap_or_default();
            return LocalIpcWireResponse::error(
                request_id,
                proto_version,
                LocalIpcError::unauthorized(format!(
                    "local IPC peer uid {}{} does not match current uid {}",
                    identity.uid, pid_hint, current_uid
                )),
            );
        }
    }

    #[cfg(windows)]
    if let Some(identity) = peer_identity {
        let current_sid = match current_windows_user_sid() {
            Ok(sid) => sid,
            Err(err) => {
                return LocalIpcWireResponse::error(
                    request_id,
                    proto_version,
                    LocalIpcError::dispatch_failed(format!(
                        "failed to read current Windows user SID for local IPC: {err:#}"
                    )),
                );
            }
        };
        if identity.sid != current_sid {
            return LocalIpcWireResponse::error(
                request_id,
                proto_version,
                LocalIpcError::unauthorized(
                    "local IPC peer SID does not match the current Windows user",
                ),
            );
        }
    }

    match LocalIpcCommand::from_wire_request(&request) {
        Ok(command) => {
            if !command.accepts_on_endpoint(endpoint_kind) {
                return LocalIpcWireResponse::error(
                    request_id,
                    proto_version,
                    LocalIpcError::invalid_request(format!(
                        "command {} must be sent to the {} endpoint",
                        command.name(),
                        command.endpoint_kind().label()
                    )),
                );
            }

            if matches!(command, LocalIpcCommand::AuthIssue) {
                let previous_token = request
                    .auth_context
                    .as_ref()
                    .and_then(|context| context.access_token.as_deref());
                return LocalIpcResponse::AccessGrant {
                    auth: state.issue_local_ipc_access_grant(previous_token).await,
                }
                .into_wire_response(request_id, proto_version);
            }

            if matches!(command, LocalIpcCommand::AuthRevoke) {
                let token = request
                    .auth_context
                    .as_ref()
                    .and_then(|context| context.access_token.as_deref());
                return match state.revoke_local_ipc_access_token(token).await {
                    Ok(()) => LocalIpcResponse::Ack.into_wire_response(request_id, proto_version),
                    Err(reason) => LocalIpcWireResponse::error(
                        request_id,
                        proto_version,
                        LocalIpcError::unauthorized(reason),
                    ),
                };
            }

            if command.requires_auth() {
                let token = request
                    .auth_context
                    .as_ref()
                    .and_then(|context| context.access_token.as_deref());
                if let Err(reason) = state.validate_local_ipc_access_token(token).await {
                    return LocalIpcWireResponse::error(
                        request_id,
                        proto_version,
                        LocalIpcError::unauthorized(reason),
                    );
                }
            }

            match service.dispatch(command).await {
                Ok(response) => response.into_wire_response(request_id, proto_version),
                Err(message) => LocalIpcWireResponse::error(
                    request_id,
                    proto_version,
                    LocalIpcError::dispatch_failed(message),
                ),
            }
        }
        Err(error) => LocalIpcWireResponse::error(request_id, proto_version, error),
    }
}

#[cfg(unix)]
fn bind_unix_listener(endpoint: &LocalIpcEndpoint) -> Result<std::os::unix::net::UnixListener> {
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

    let listener = std::os::unix::net::UnixListener::bind(path)
        .with_context(|| format!("bind local IPC socket {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod local IPC socket {}", path.display()))?;
    listener.set_nonblocking(true).with_context(|| {
        format!(
            "configure local IPC socket as nonblocking {}",
            path.display()
        )
    })?;
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

    #[cfg(windows)]
    {
        let _ = endpoint;
    }
}

#[cfg(windows)]
fn with_tokio_io_context<T>(operation: impl FnOnce() -> std::io::Result<T>) -> std::io::Result<T> {
    if tokio::runtime::Handle::try_current().is_ok() {
        operation()
    } else {
        tauri::async_runtime::block_on(async { operation() })
    }
}

#[cfg(windows)]
fn create_windows_pipe_server(
    path: &str,
    first_pipe_instance: bool,
) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    use tokio::net::windows::named_pipe::{PipeMode, ServerOptions};

    with_tokio_io_context(|| {
        let mut options = ServerOptions::new();
        options.first_pipe_instance(first_pipe_instance);
        options.reject_remote_clients(true);
        options.pipe_mode(PipeMode::Byte);
        let security_attributes = build_windows_pipe_security_attributes()?;
        unsafe {
            options.create_with_security_attributes_raw(path, security_attributes.as_ptr().cast())
        }
    })
}

#[cfg(windows)]
struct WindowsPipeSecurityAttributes {
    attributes: windows_sys::Win32::Security::SECURITY_ATTRIBUTES,
    descriptor: windows_sys::Win32::Security::PSECURITY_DESCRIPTOR,
}

#[cfg(windows)]
impl WindowsPipeSecurityAttributes {
    fn as_ptr(&self) -> *const windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
        &self.attributes
    }
}

#[cfg(windows)]
impl Drop for WindowsPipeSecurityAttributes {
    fn drop(&mut self) {
        if !self.descriptor.is_null() {
            unsafe {
                windows_sys::Win32::Foundation::LocalFree(self.descriptor as isize);
            }
        }
    }
}

#[cfg(windows)]
fn build_windows_pipe_security_attributes() -> std::io::Result<WindowsPipeSecurityAttributes> {
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

    // Owner-only DACL plus SYSTEM access. This narrows who can open the pipe
    // before the request-level SID validation runs.
    const PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;OW)";

    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let mut descriptor_len = 0u32;
    let wide: Vec<u16> = PIPE_SDDL.encode_utf16().chain(std::iter::once(0)).collect();

    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_ptr(),
            SDDL_REVISION_1 as u32,
            &mut descriptor,
            &mut descriptor_len,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }

    Ok(WindowsPipeSecurityAttributes {
        attributes: SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        },
        descriptor,
    })
}

#[cfg(windows)]
fn current_windows_user_sid() -> Result<Vec<u8>> {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::TOKEN_QUERY;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    struct HandleGuard(HANDLE);

    impl Drop for HandleGuard {
        fn drop(&mut self) {
            if self.0 != 0 {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    let mut token = 0;
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(anyhow!(
            "OpenProcessToken failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let _guard = HandleGuard(token);
    query_token_user_sid(token)
}

#[cfg(windows)]
fn windows_named_pipe_peer_identity(
    stream: &tokio::net::windows::named_pipe::NamedPipeServer,
) -> Result<LocalPeerIdentity> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{RevertToSelf, TOKEN_QUERY};
    use windows_sys::Win32::System::Pipes::ImpersonateNamedPipeClient;
    use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

    struct RevertGuard;

    impl Drop for RevertGuard {
        fn drop(&mut self) {
            unsafe {
                RevertToSelf();
            }
        }
    }

    struct HandleGuard(HANDLE);

    impl Drop for HandleGuard {
        fn drop(&mut self) {
            if self.0 != 0 {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    let pipe_handle = stream.as_raw_handle() as HANDLE;
    if unsafe { ImpersonateNamedPipeClient(pipe_handle) } == 0 {
        return Err(anyhow!(
            "ImpersonateNamedPipeClient failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let _revert_guard = RevertGuard;

    let mut token = 0;
    if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } == 0 {
        return Err(anyhow!(
            "OpenThreadToken failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let _guard = HandleGuard(token);

    Ok(LocalPeerIdentity {
        sid: query_token_user_sid(token)?,
    })
}

#[cfg(windows)]
fn query_token_user_sid(token: windows_sys::Win32::Foundation::HANDLE) -> Result<Vec<u8>> {
    use windows_sys::Win32::Security::{GetLengthSid, GetTokenInformation, TokenUser, TOKEN_USER};

    let mut required = 0u32;
    unsafe {
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut required);
    }
    if required == 0 {
        return Err(anyhow!(
            "GetTokenInformation size query failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let mut buffer = vec![0u8; required as usize];
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            required,
            &mut required,
        )
    } == 0
    {
        return Err(anyhow!(
            "GetTokenInformation(TokenUser) failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let token_user = unsafe { &*(buffer.as_ptr().cast::<TOKEN_USER>()) };
    let sid_length = unsafe { GetLengthSid(token_user.User.Sid) };
    if sid_length == 0 {
        return Err(anyhow!(
            "GetLengthSid failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    Ok(unsafe {
        std::slice::from_raw_parts(token_user.User.Sid.cast::<u8>(), sid_length as usize).to_vec()
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::crypto::Identity;
    use crate::daemon::client::LocalIpcClient;
    use crate::local_ipc::{LocalIpcCommand, LocalIpcEndpointKind, LocalIpcWireResponse};
    use crate::state::{AppConfig, AppState};

    use super::spawn;

    fn build_test_state() -> Arc<AppState> {
        let config_dir =
            std::env::temp_dir().join(format!("dashdrop-ipc-server-{}", uuid::Uuid::new_v4()));
        let identity = Identity::load_or_create(&config_dir).expect("identity");
        Arc::new(AppState::new(
            identity,
            AppConfig::default(),
            rusqlite::Connection::open_in_memory().expect("db"),
        ))
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn local_ipc_server_starts_outside_tokio_runtime() {
        let state = build_test_state();
        let config_dir =
            std::env::temp_dir().join(format!("dashdrop-ipc-runtime-{}", uuid::Uuid::new_v4()));

        let server = spawn(
            Arc::clone(&state),
            None,
            &config_dir,
            crate::local_ipc::LocalIpcEndpointKind::Service,
        )
        .expect("spawn server");

        tauri::async_runtime::block_on(async {
            let client = LocalIpcClient::from_config_dir(&config_dir);
            let grant = client.issue_access_grant(None).await.expect("access grant");
            let response = client
                .send_authenticated(LocalIpcCommand::ConfigGet, &grant.access_token)
                .await
                .expect("ipc response");

            match response {
                LocalIpcWireResponse::Ok(ok) => {
                    assert_eq!(
                        ok.payload
                            .as_ref()
                            .and_then(|payload| payload.get("config"))
                            .and_then(|config| config.get("device_name")),
                        Some(&serde_json::json!("DashDrop Device"))
                    );
                }
                other => panic!("unexpected response envelope: {other:?}"),
            }

            server.shutdown().await;
        });
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn service_endpoint_rejects_missing_access_token_for_service_commands() {
        let state = build_test_state();
        let config_dir = std::env::temp_dir().join(format!(
            "dashdrop-ipc-service-auth-{}",
            uuid::Uuid::new_v4()
        ));

        let server = spawn(
            Arc::clone(&state),
            None,
            &config_dir,
            LocalIpcEndpointKind::Service,
        )
        .expect("spawn server");

        tauri::async_runtime::block_on(async {
            let client = LocalIpcClient::from_config_dir(&config_dir);
            let response = client
                .send(LocalIpcCommand::ConfigGet)
                .await
                .expect("ipc response");

            match response {
                LocalIpcWireResponse::Err(err) => {
                    assert_eq!(err.error.code, "unauthorized");
                    assert!(err.error.message.contains("access token"));
                }
                other => panic!("unexpected response envelope: {other:?}"),
            }

            server.shutdown().await;
        });
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn service_endpoint_revokes_access_tokens_explicitly() {
        let state = build_test_state();
        let config_dir =
            std::env::temp_dir().join(format!("dashdrop-ipc-revoke-auth-{}", uuid::Uuid::new_v4()));

        let server = spawn(
            Arc::clone(&state),
            None,
            &config_dir,
            LocalIpcEndpointKind::Service,
        )
        .expect("spawn server");

        tauri::async_runtime::block_on(async {
            let client = LocalIpcClient::from_config_dir(&config_dir);
            let grant = client.issue_access_grant(None).await.expect("access grant");
            client
                .revoke_access_grant(&grant.access_token)
                .await
                .expect("revoke access grant");

            let response = client
                .send_authenticated(LocalIpcCommand::ConfigGet, &grant.access_token)
                .await
                .expect("ipc response");

            match response {
                LocalIpcWireResponse::Err(err) => {
                    assert_eq!(err.error.code, "unauthorized");
                    assert!(err.error.message.contains("access token"));
                }
                other => panic!("unexpected response envelope: {other:?}"),
            }

            server.shutdown().await;
        });
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn service_endpoint_rejects_activation_commands() {
        let state = build_test_state();
        let config_dir =
            std::env::temp_dir().join(format!("dashdrop-ipc-service-{}", uuid::Uuid::new_v4()));

        let server = spawn(
            Arc::clone(&state),
            None,
            &config_dir,
            LocalIpcEndpointKind::Service,
        )
        .expect("spawn server");

        tauri::async_runtime::block_on(async {
            let client = LocalIpcClient::from_config_dir(&config_dir);
            let response = client
                .send(LocalIpcCommand::AppActivate {
                    paths: vec!["/tmp/share.txt".into()],
                    pairing_links: Vec::new(),
                })
                .await
                .expect("ipc response");

            match response {
                LocalIpcWireResponse::Err(err) => {
                    assert_eq!(err.error.code, "invalid_request");
                    assert!(err.error.message.contains("ui activation endpoint"));
                }
                other => panic!("unexpected response envelope: {other:?}"),
            }

            server.shutdown().await;
        });
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn ui_activation_endpoint_rejects_service_commands() {
        let state = build_test_state();
        let config_dir =
            std::env::temp_dir().join(format!("dashdrop-ipc-ui-{}", uuid::Uuid::new_v4()));

        let server = spawn(
            Arc::clone(&state),
            None,
            &config_dir,
            LocalIpcEndpointKind::UiActivation,
        )
        .expect("spawn server");

        tauri::async_runtime::block_on(async {
            let client = LocalIpcClient::from_config_dir_for_kind(
                &config_dir,
                LocalIpcEndpointKind::UiActivation,
            );
            let response = client
                .send(LocalIpcCommand::ConfigGet)
                .await
                .expect("ipc response");

            match response {
                LocalIpcWireResponse::Err(err) => {
                    assert_eq!(err.error.code, "invalid_request");
                    assert!(err.error.message.contains("service endpoint"));
                }
                other => panic!("unexpected response envelope: {other:?}"),
            }

            server.shutdown().await;
        });
    }
}
