use std::path::Path;
#[cfg(windows)]
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use crate::local_ipc::{
    decode_message, read_frame_bytes, resolve_local_ipc_endpoint, write_framed_message,
    LocalIpcCommand, LocalIpcEndpoint, LocalIpcResult, LocalIpcWireRequest, LocalIpcWireResponse,
    LocalResponseEnvelope, LOCAL_IPC_PROTO_VERSION,
};

#[allow(dead_code)]
pub struct LocalIpcClient {
    endpoint: LocalIpcEndpoint,
}

#[allow(dead_code)]
impl LocalIpcClient {
    fn decode_response(bytes: &[u8]) -> Result<LocalIpcWireResponse> {
        if let Ok(response) = decode_message::<LocalIpcWireResponse>(bytes) {
            return Ok(response);
        }

        let envelope: LocalResponseEnvelope =
            decode_message(bytes).context("decode legacy local IPC envelope")?;
        Ok(match envelope.command {
            LocalIpcResult::Ok { response } => {
                response.into_wire_response(envelope.request_id, envelope.proto_version)
            }
            LocalIpcResult::Err { error } => {
                LocalIpcWireResponse::error(envelope.request_id, envelope.proto_version, error)
            }
        })
    }

    pub fn from_config_dir(config_dir: &Path) -> Self {
        Self {
            endpoint: resolve_local_ipc_endpoint(config_dir),
        }
    }

    pub fn from_endpoint(endpoint: LocalIpcEndpoint) -> Self {
        Self { endpoint }
    }

    pub async fn send(&self, command: LocalIpcCommand) -> Result<LocalIpcWireResponse> {
        self.send_envelope(command.to_wire_request(uuid::Uuid::new_v4().to_string(), None))
            .await
    }

    pub async fn send_envelope(
        &self,
        request: LocalIpcWireRequest,
    ) -> Result<LocalIpcWireResponse> {
        #[cfg(unix)]
        {
            let path = match &self.endpoint {
                LocalIpcEndpoint::UnixSocket { path } => path,
            };

            let mut stream = tokio::net::UnixStream::connect(path)
                .await
                .with_context(|| format!("connect local IPC socket {}", path.display()))?;
            write_framed_message(&mut stream, &request)
                .await
                .context("write local IPC request")?;
            let response_bytes = read_frame_bytes(&mut stream)
                .await
                .context("read local IPC response")?;
            let response =
                Self::decode_response(&response_bytes).context("decode local IPC response")?;
            Self::validate_response(&request, response)
        }

        #[cfg(windows)]
        {
            let LocalIpcEndpoint::WindowsNamedPipe { path } = &self.endpoint else {
                return Err(anyhow!(
                    "windows local IPC client expected a named pipe endpoint"
                ));
            };
            let mut stream = open_windows_named_pipe(path).await?;
            write_framed_message(&mut stream, &request)
                .await
                .context("write local IPC request")?;
            let response_bytes = read_frame_bytes(&mut stream)
                .await
                .context("read local IPC response")?;
            let response =
                Self::decode_response(&response_bytes).context("decode local IPC response")?;
            Self::validate_response(&request, response)
        }

        #[cfg(not(any(unix, windows)))]
        {
            let _ = request;
            Err(anyhow!("local IPC is unsupported on this platform"))
        }
    }

    fn validate_response(
        request: &LocalIpcWireRequest,
        response: LocalIpcWireResponse,
    ) -> Result<LocalIpcWireResponse> {
        let response_proto_version = match &response {
            LocalIpcWireResponse::Ok(ok) => ok.proto_version,
            LocalIpcWireResponse::Err(err) => err.proto_version,
        };
        if response_proto_version != LOCAL_IPC_PROTO_VERSION {
            return Err(anyhow!(
                "local IPC response proto mismatch: expected {}, got {}",
                LOCAL_IPC_PROTO_VERSION,
                response_proto_version
            ));
        }
        let response_request_id = match &response {
            LocalIpcWireResponse::Ok(ok) => &ok.request_id,
            LocalIpcWireResponse::Err(err) => &err.request_id,
        };
        if response_request_id != &request.request_id {
            return Err(anyhow!(
                "local IPC response request id mismatch: expected {}, got {}",
                request.request_id,
                response_request_id
            ));
        }
        Ok(response)
    }
}

#[cfg(windows)]
fn with_tokio_io_context<T>(
    operation: impl FnOnce() -> std::io::Result<T>,
) -> std::io::Result<T> {
    if tokio::runtime::Handle::try_current().is_ok() {
        operation()
    } else {
        tauri::async_runtime::block_on(async { operation() })
    }
}

#[cfg(windows)]
async fn open_windows_named_pipe(
    path: &str,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    use tokio::net::windows::named_pipe::ClientOptions;

    const ERROR_PIPE_BUSY: i32 = 231;
    const MAX_ATTEMPTS: usize = 80;

    for attempt in 0..MAX_ATTEMPTS {
        match with_tokio_io_context(|| ClientOptions::new().open(path)) {
            Ok(client) => return Ok(client),
            Err(err)
                if err.raw_os_error() == Some(ERROR_PIPE_BUSY)
                    || err.kind() == std::io::ErrorKind::NotFound =>
            {
                if attempt + 1 == MAX_ATTEMPTS {
                    return Err(err)
                        .with_context(|| format!("connect local IPC named pipe {path}"));
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(err) => {
                return Err(err).with_context(|| format!("connect local IPC named pipe {path}"));
            }
        }
    }

    unreachable!("windows named pipe open loop must return or error before exhausting attempts")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Instant;

    use crate::crypto::Identity;
    use crate::daemon::server;
    use crate::local_ipc::{LocalIpcCommand, LocalIpcWireResponse};
    use crate::state::{
        AppConfig, AppState, DeviceInfo, Platform, ReachabilityStatus, SessionInfo,
    };

    use super::LocalIpcClient;

    fn build_test_state() -> Arc<AppState> {
        let config_dir =
            std::env::temp_dir().join(format!("dashdrop-ipc-state-{}", uuid::Uuid::new_v4()));
        let identity = Identity::load_or_create(&config_dir).expect("identity");
        Arc::new(AppState::new(
            identity,
            AppConfig::default(),
            rusqlite::Connection::open_in_memory().expect("db"),
        ))
    }

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn config_get_round_trips_over_local_ipc_server() {
        let state = build_test_state();
        state.config.write().await.device_name = "Desk".into();
        state.devices.write().await.insert(
            "fp-1".into(),
            DeviceInfo {
                fingerprint: "fp-1".into(),
                name: "Peer".into(),
                platform: Platform::Mac,
                trusted: false,
                sessions: HashMap::from([(
                    "s1".into(),
                    SessionInfo {
                        session_id: "s1".into(),
                        addrs: vec!["127.0.0.1:9443".parse().expect("addr")],
                        last_seen_unix: 1,
                        last_seen_instant: Instant::now(),
                    },
                )]),
                last_seen: 1,
                reachability: ReachabilityStatus::Discovered,
                probe_fail_count: 0,
                last_probe_at: None,
                last_probe_result: None,
                last_probe_error: None,
                last_probe_error_detail: None,
                last_probe_addr: None,
                last_probe_attempted_addrs: Vec::new(),
                last_resolve_raw_addr_count: 0,
                last_resolve_usable_addr_count: 0,
                last_resolve_hostname: None,
                last_resolve_port: None,
                last_resolve_at: None,
            },
        );

        let config_dir =
            std::env::temp_dir().join(format!("dashdrop-ipc-contract-{}", uuid::Uuid::new_v4()));
        let server = server::spawn(Arc::clone(&state), None, &config_dir).expect("spawn server");
        let client = LocalIpcClient::from_config_dir(&config_dir);
        let request = LocalIpcCommand::ConfigGet.to_wire_request("req-contract", None);

        let response = client.send_envelope(request).await.expect("ipc response");

        match response {
            LocalIpcWireResponse::Ok(ok) => {
                assert_eq!(ok.request_id, "req-contract");
                assert_eq!(
                    ok.payload
                        .as_ref()
                        .and_then(|payload| payload.get("config"))
                        .and_then(|config| config.get("device_name")),
                    Some(&serde_json::json!("Desk"))
                );
            }
            other => panic!("unexpected response envelope: {other:?}"),
        }

        server.shutdown().await;
    }
}
