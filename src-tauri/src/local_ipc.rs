use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::dto::{DeviceView, TrustedPeerView};
use crate::state::{AppConfig, LocalIdentityView, RuntimeStatus};

pub const LOCAL_IPC_PROTO_VERSION: u16 = 1;
pub const LOCAL_IPC_MAX_FRAME_LEN: usize = 1024 * 1024;
pub const RESERVED_PHASE_A_COMMANDS: &[&str] = &["discover/diagnostics"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectByAddressResult {
    pub fingerprint: String,
    pub name: String,
    pub trusted: bool,
    pub address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalAuthContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LocalIpcWireRequest {
    pub proto_version: u16,
    pub request_id: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_context: Option<LocalAuthContext>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum LocalIpcWireResponse {
    Ok(LocalIpcWireSuccess),
    Err(LocalIpcWireErrorResponse),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LocalIpcWireSuccess {
    pub proto_version: u16,
    pub request_id: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalIpcWireErrorResponse {
    pub proto_version: u16,
    pub request_id: String,
    pub ok: bool,
    pub error: LocalIpcError,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalEnvelope<T> {
    pub proto_version: u16,
    pub request_id: String,
    pub command: T,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_context: Option<LocalAuthContext>,
}

#[allow(dead_code)]
pub type LocalRequestEnvelope = LocalEnvelope<LocalIpcCommand>;
#[allow(dead_code)]
pub type LocalResponseEnvelope = LocalEnvelope<LocalIpcResult>;

#[allow(dead_code)]
impl LocalRequestEnvelope {
    #[allow(dead_code)]
    pub fn new(request_id: impl Into<String>, command: LocalIpcCommand) -> Self {
        Self {
            proto_version: LOCAL_IPC_PROTO_VERSION,
            request_id: request_id.into(),
            command,
            auth_context: None,
        }
    }
}

#[allow(dead_code)]
impl LocalResponseEnvelope {
    pub fn success(request: &LocalRequestEnvelope, response: LocalIpcResponse) -> Self {
        Self {
            proto_version: request.proto_version,
            request_id: request.request_id.clone(),
            command: LocalIpcResult::ok(response),
            auth_context: None,
        }
    }

    pub fn error(request_id: impl Into<String>, proto_version: u16, error: LocalIpcError) -> Self {
        Self {
            proto_version,
            request_id: request_id.into(),
            command: LocalIpcResult::err(error),
            auth_context: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum LocalIpcCommand {
    DiscoverList,
    DiscoverConnectByAddress {
        address: String,
    },
    TrustList,
    TrustPair {
        fingerprint: String,
    },
    TrustUnpair {
        fingerprint: String,
    },
    TrustSetAlias {
        fingerprint: String,
        alias: Option<String>,
    },
    ConfigGet,
    ConfigSet {
        config: AppConfig,
    },
    TransferSend {
        peer_fingerprint: String,
        paths: Vec<String>,
    },
    TransferAccept {
        transfer_id: String,
        notification_id: String,
    },
    TransferReject {
        transfer_id: String,
        notification_id: String,
    },
    TransferCancel {
        transfer_id: String,
    },
    TransferCancelAll,
    TransferRetry {
        transfer_id: String,
    },
    AppGetLocalIdentity,
    AppGetRuntimeStatus,
    SecurityGetPosture,
}

impl LocalIpcCommand {
    pub fn name(&self) -> &'static str {
        match self {
            Self::DiscoverList => "discover/list",
            Self::DiscoverConnectByAddress { .. } => "discover/connect_by_address",
            Self::TrustList => "trust/list",
            Self::TrustPair { .. } => "trust/pair",
            Self::TrustUnpair { .. } => "trust/unpair",
            Self::TrustSetAlias { .. } => "trust/set_alias",
            Self::ConfigGet => "config/get",
            Self::ConfigSet { .. } => "config/set",
            Self::TransferSend { .. } => "transfer/send",
            Self::TransferAccept { .. } => "transfer/accept",
            Self::TransferReject { .. } => "transfer/reject",
            Self::TransferCancel { .. } => "transfer/cancel",
            Self::TransferCancelAll => "transfer/cancel_all",
            Self::TransferRetry { .. } => "transfer/retry",
            Self::AppGetLocalIdentity => "app/get_local_identity",
            Self::AppGetRuntimeStatus => "app/get_runtime_status",
            Self::SecurityGetPosture => "security/get_posture",
        }
    }

    pub fn to_wire_request(
        &self,
        request_id: impl Into<String>,
        auth_context: Option<LocalAuthContext>,
    ) -> LocalIpcWireRequest {
        LocalIpcWireRequest {
            proto_version: LOCAL_IPC_PROTO_VERSION,
            request_id: request_id.into(),
            command: self.name().to_string(),
            payload: self.payload(),
            auth_context,
        }
    }

    pub fn from_wire_request(request: &LocalIpcWireRequest) -> Result<Self, LocalIpcError> {
        if request.proto_version != LOCAL_IPC_PROTO_VERSION {
            return Err(LocalIpcError::proto_mismatch(
                LOCAL_IPC_PROTO_VERSION,
                request.proto_version,
            ));
        }

        Self::from_name_and_payload(&request.command, request.payload.as_ref())
    }

    fn payload(&self) -> Option<Value> {
        match self {
            Self::DiscoverList
            | Self::TrustList
            | Self::ConfigGet
            | Self::TransferCancelAll
            | Self::AppGetLocalIdentity
            | Self::AppGetRuntimeStatus
            | Self::SecurityGetPosture => None,
            Self::DiscoverConnectByAddress { address } => Some(json!({ "address": address })),
            Self::TrustPair { fingerprint } | Self::TrustUnpair { fingerprint } => {
                Some(json!({ "fingerprint": fingerprint }))
            }
            Self::TrustSetAlias { fingerprint, alias } => Some(json!({
                "fingerprint": fingerprint,
                "alias": alias,
            })),
            Self::ConfigSet { config } => Some(json!({ "config": config })),
            Self::TransferSend {
                peer_fingerprint,
                paths,
            } => Some(json!({
                "peer_fingerprint": peer_fingerprint,
                "paths": paths,
            })),
            Self::TransferAccept {
                transfer_id,
                notification_id,
            }
            | Self::TransferReject {
                transfer_id,
                notification_id,
            } => Some(json!({
                "transfer_id": transfer_id,
                "notification_id": notification_id,
            })),
            Self::TransferCancel { transfer_id }
            | Self::TransferRetry { transfer_id } => Some(json!({ "transfer_id": transfer_id })),
        }
    }

    fn from_name_and_payload(
        command: &str,
        payload: Option<&Value>,
    ) -> Result<Self, LocalIpcError> {
        match command {
            "discover/list" => Ok(Self::DiscoverList),
            "discover/connect_by_address" => Ok(Self::DiscoverConnectByAddress {
                address: required_string(payload, "address")?,
            }),
            "trust/list" => Ok(Self::TrustList),
            "trust/pair" => Ok(Self::TrustPair {
                fingerprint: required_string(payload, "fingerprint")?,
            }),
            "trust/unpair" => Ok(Self::TrustUnpair {
                fingerprint: required_string(payload, "fingerprint")?,
            }),
            "trust/set_alias" => Ok(Self::TrustSetAlias {
                fingerprint: required_string(payload, "fingerprint")?,
                alias: optional_string(payload, "alias")?,
            }),
            "config/get" => Ok(Self::ConfigGet),
            "config/set" => Ok(Self::ConfigSet {
                config: required_value(payload, "config")?,
            }),
            "transfer/send" => Ok(Self::TransferSend {
                peer_fingerprint: required_string(payload, "peer_fingerprint")?,
                paths: required_string_vec(payload, "paths")?,
            }),
            "transfer/accept" => Ok(Self::TransferAccept {
                transfer_id: required_string(payload, "transfer_id")?,
                notification_id: required_string(payload, "notification_id")?,
            }),
            "transfer/reject" => Ok(Self::TransferReject {
                transfer_id: required_string(payload, "transfer_id")?,
                notification_id: required_string(payload, "notification_id")?,
            }),
            "transfer/cancel" => Ok(Self::TransferCancel {
                transfer_id: required_string(payload, "transfer_id")?,
            }),
            "transfer/cancel_all" => Ok(Self::TransferCancelAll),
            "transfer/retry" => Ok(Self::TransferRetry {
                transfer_id: required_string(payload, "transfer_id")?,
            }),
            "app/get_local_identity" => Ok(Self::AppGetLocalIdentity),
            "app/get_runtime_status" => Ok(Self::AppGetRuntimeStatus),
            "security/get_posture" => Ok(Self::SecurityGetPosture),
            reserved if RESERVED_PHASE_A_COMMANDS.contains(&reserved) => Err(
                LocalIpcError::invalid_request(format!(
                    "command {reserved} is reserved for the frozen Phase A IPC contract and is not implemented in the single-process baseline"
                )),
            ),
            other => Err(LocalIpcError::invalid_request(format!(
                "unknown local IPC command {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LocalIpcResponse {
    Ack,
    Devices { devices: Vec<DeviceView> },
    ConnectByAddress { result: ConnectByAddressResult },
    TrustedPeers { trusted_peers: Vec<TrustedPeerView> },
    AppConfig { config: AppConfig },
    CancelledTransfers { count: u32 },
    LocalIdentity { identity: LocalIdentityView },
    RuntimeStatus { runtime_status: RuntimeStatus },
    SecurityPosture { posture: serde_json::Value },
}

impl LocalIpcResponse {
    pub fn into_wire_response(
        self,
        request_id: impl Into<String>,
        proto_version: u16,
    ) -> LocalIpcWireResponse {
        LocalIpcWireResponse::Ok(LocalIpcWireSuccess {
            proto_version,
            request_id: request_id.into(),
            ok: true,
            payload: self.payload(),
        })
    }

    fn payload(self) -> Option<Value> {
        match self {
            Self::Ack => None,
            Self::Devices { devices } => Some(json!({ "devices": devices })),
            Self::ConnectByAddress { result } => Some(json!({ "result": result })),
            Self::TrustedPeers { trusted_peers } => Some(json!({ "trusted_peers": trusted_peers })),
            Self::AppConfig { config } => Some(json!({ "config": config })),
            Self::CancelledTransfers { count } => Some(json!({ "count": count })),
            Self::LocalIdentity { identity } => Some(json!({ "identity": identity })),
            Self::RuntimeStatus { runtime_status } => {
                Some(json!({ "runtime_status": runtime_status }))
            }
            Self::SecurityPosture { posture } => Some(json!({ "posture": posture })),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[allow(dead_code)]
pub enum LocalIpcResult {
    Ok { response: LocalIpcResponse },
    Err { error: LocalIpcError },
}

#[allow(dead_code)]
impl LocalIpcResult {
    pub fn ok(response: LocalIpcResponse) -> Self {
        Self::Ok { response }
    }

    pub fn err(error: LocalIpcError) -> Self {
        Self::Err { error }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalIpcError {
    pub code: String,
    pub message: String,
}

impl LocalIpcError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    #[allow(dead_code)]
    pub fn unsupported_platform(message: impl Into<String>) -> Self {
        Self::new("unsupported_platform", message)
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new("invalid_request", message)
    }

    pub fn proto_mismatch(expected: u16, actual: u16) -> Self {
        Self::new(
            "proto_mismatch",
            format!("unsupported local IPC protocol version {actual}; expected {expected}"),
        )
    }

    pub fn dispatch_failed(message: impl Into<String>) -> Self {
        Self::new("dispatch_failed", message)
    }
}

impl LocalIpcWireResponse {
    pub fn error(request_id: impl Into<String>, proto_version: u16, error: LocalIpcError) -> Self {
        Self::Err(LocalIpcWireErrorResponse {
            proto_version,
            request_id: request_id.into(),
            ok: false,
            error,
        })
    }
}

fn payload_object(
    payload: Option<&Value>,
) -> Result<&serde_json::Map<String, Value>, LocalIpcError> {
    payload
        .and_then(Value::as_object)
        .ok_or_else(|| LocalIpcError::invalid_request("payload must be an object"))
}

fn required_string(payload: Option<&Value>, field: &str) -> Result<String, LocalIpcError> {
    payload_object(payload)?
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| LocalIpcError::invalid_request(format!("payload.{field} must be a string")))
}

fn optional_string(payload: Option<&Value>, field: &str) -> Result<Option<String>, LocalIpcError> {
    match payload_object(payload)?.get(field) {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(LocalIpcError::invalid_request(format!(
            "payload.{field} must be a string or null"
        ))),
    }
}

fn required_string_vec(payload: Option<&Value>, field: &str) -> Result<Vec<String>, LocalIpcError> {
    let value = payload_object(payload)?
        .get(field)
        .ok_or_else(|| LocalIpcError::invalid_request(format!("payload.{field} is required")))?;
    let Some(items) = value.as_array() else {
        return Err(LocalIpcError::invalid_request(format!(
            "payload.{field} must be an array of strings"
        )));
    };
    items
        .iter()
        .map(|item| {
            item.as_str().map(str::to_string).ok_or_else(|| {
                LocalIpcError::invalid_request(format!(
                    "payload.{field} must be an array of strings"
                ))
            })
        })
        .collect()
}

fn required_value<T>(payload: Option<&Value>, field: &str) -> Result<T, LocalIpcError>
where
    T: for<'de> Deserialize<'de>,
{
    let value = payload_object(payload)?
        .get(field)
        .cloned()
        .ok_or_else(|| LocalIpcError::invalid_request(format!("payload.{field} is required")))?;
    serde_json::from_value(value)
        .map_err(|err| LocalIpcError::invalid_request(format!("payload.{field} is invalid: {err}")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalIpcEndpoint {
    #[cfg(unix)]
    UnixSocket { path: PathBuf },
    #[cfg(windows)]
    WindowsNamedPipe { path: String },
    #[cfg(not(any(unix, windows)))]
    Unsupported,
}

impl LocalIpcEndpoint {
    pub fn describe(&self) -> String {
        match self {
            #[cfg(unix)]
            Self::UnixSocket { path } => format!("unix://{}", path.display()),
            #[cfg(windows)]
            Self::WindowsNamedPipe { path } => format!("pipe://{path}"),
            #[cfg(not(any(unix, windows)))]
            Self::Unsupported => "unsupported".to_string(),
        }
    }
}

pub fn resolve_local_ipc_endpoint(config_dir: &Path) -> LocalIpcEndpoint {
    let key = hashed_endpoint_key(config_dir);

    #[cfg(unix)]
    {
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            let path = PathBuf::from(runtime_dir).join(format!("dashdrop-{key}.sock"));
            return LocalIpcEndpoint::UnixSocket { path };
        }

        let dir = std::env::temp_dir().join(format!("dashdrop-{key}"));
        LocalIpcEndpoint::UnixSocket {
            path: dir.join("ipc-v1.sock"),
        }
    }

    #[cfg(windows)]
    {
        return LocalIpcEndpoint::WindowsNamedPipe {
            path: format!(r"\\.\pipe\dashdrop-service-v1-{key}"),
        };
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = key;
        LocalIpcEndpoint::Unsupported
    }
}

fn hashed_endpoint_key(config_dir: &Path) -> String {
    let hash = blake3::hash(config_dir.to_string_lossy().as_bytes())
        .to_hex()
        .to_string();
    hash[..12].to_string()
}

#[derive(Debug, Error)]
pub enum LocalIpcCodecError {
    #[error("frame too large: {0} bytes")]
    FrameTooLarge(usize),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to serialize cbor: {0}")]
    Serialize(String),
    #[error("failed to deserialize cbor: {0}")]
    Deserialize(String),
}

pub fn encode_message<T: Serialize>(value: &T) -> Result<Vec<u8>, LocalIpcCodecError> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(value, &mut bytes)
        .map_err(|err| LocalIpcCodecError::Serialize(err.to_string()))?;
    if bytes.len() > LOCAL_IPC_MAX_FRAME_LEN {
        return Err(LocalIpcCodecError::FrameTooLarge(bytes.len()));
    }
    Ok(bytes)
}

pub fn decode_message<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, LocalIpcCodecError> {
    ciborium::de::from_reader(bytes).map_err(|err| LocalIpcCodecError::Deserialize(err.to_string()))
}

pub async fn write_frame_bytes<W: AsyncWrite + Unpin>(
    writer: &mut W,
    bytes: &[u8],
) -> Result<(), LocalIpcCodecError> {
    if bytes.len() > LOCAL_IPC_MAX_FRAME_LEN {
        return Err(LocalIpcCodecError::FrameTooLarge(bytes.len()));
    }

    writer.write_u32(bytes.len() as u32).await?;
    writer.write_all(bytes).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_frame_bytes<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Vec<u8>, LocalIpcCodecError> {
    let frame_len = reader.read_u32().await? as usize;
    if frame_len > LOCAL_IPC_MAX_FRAME_LEN {
        return Err(LocalIpcCodecError::FrameTooLarge(frame_len));
    }

    let mut bytes = vec![0_u8; frame_len];
    reader.read_exact(&mut bytes).await?;
    Ok(bytes)
}

pub async fn write_framed_message<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> Result<(), LocalIpcCodecError> {
    let bytes = encode_message(value)?;
    write_frame_bytes(writer, &bytes).await
}

#[allow(dead_code)]
pub async fn read_framed_message<R: AsyncRead + Unpin, T: for<'de> Deserialize<'de>>(
    reader: &mut R,
) -> Result<T, LocalIpcCodecError> {
    let bytes = read_frame_bytes(reader).await?;
    decode_message(&bytes)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        decode_message, encode_message, read_framed_message, resolve_local_ipc_endpoint,
        write_framed_message, LocalAuthContext, LocalEnvelope, LocalIpcCommand, LocalIpcResponse,
        LocalIpcResult, LocalIpcWireResponse, LocalRequestEnvelope, LOCAL_IPC_PROTO_VERSION,
    };
    use crate::state::AppConfig;
    use serde_json::json;

    #[test]
    fn local_command_names_match_target_shape() {
        assert_eq!(LocalIpcCommand::DiscoverList.name(), "discover/list");
        assert_eq!(
            LocalIpcCommand::DiscoverConnectByAddress {
                address: "127.0.0.1:9443".into(),
            }
            .name(),
            "discover/connect_by_address"
        );
        assert_eq!(LocalIpcCommand::TrustList.name(), "trust/list");
        assert_eq!(LocalIpcCommand::ConfigGet.name(), "config/get");
        assert_eq!(
            LocalIpcCommand::TransferSend {
                peer_fingerprint: "fp-1".into(),
                paths: vec!["/tmp/a.txt".into()],
            }
            .name(),
            "transfer/send"
        );
        assert_eq!(
            LocalIpcCommand::AppGetRuntimeStatus.name(),
            "app/get_runtime_status"
        );
    }

    #[test]
    fn request_envelope_round_trips_with_cbor() {
        let request = LocalRequestEnvelope::new(
            "req-1",
            LocalIpcCommand::ConfigSet {
                config: AppConfig::default(),
            },
        );
        let bytes = encode_message(&request).expect("serialize request");
        let decoded: LocalEnvelope<LocalIpcCommand> =
            decode_message(&bytes).expect("deserialize request");
        assert_eq!(decoded.proto_version, LOCAL_IPC_PROTO_VERSION);
        assert_eq!(decoded.request_id, "req-1");
        assert!(matches!(decoded.command, LocalIpcCommand::ConfigSet { .. }));
    }

    #[test]
    fn wire_request_uses_frozen_command_payload_and_access_token_fields() {
        let request = LocalIpcCommand::ConfigSet {
            config: AppConfig::default(),
        }
        .to_wire_request(
            "req-wire",
            Some(LocalAuthContext {
                access_token: Some("token-1".into()),
            }),
        );

        let value = serde_json::to_value(&request).expect("serialize request");
        assert_eq!(value["proto_version"], json!(LOCAL_IPC_PROTO_VERSION));
        assert_eq!(value["request_id"], json!("req-wire"));
        assert_eq!(value["command"], json!("config/set"));
        assert_eq!(
            value["payload"]["config"]["device_name"],
            json!("DashDrop Device")
        );
        assert_eq!(value["auth_context"]["access_token"], json!("token-1"));
    }

    #[test]
    fn wire_request_parses_back_into_internal_command() {
        let request = LocalIpcCommand::TrustPair {
            fingerprint: "fp-1".into(),
        }
        .to_wire_request("req-pair", None);

        let decoded = LocalIpcCommand::from_wire_request(&request).expect("parse request");
        assert!(matches!(
            decoded,
            LocalIpcCommand::TrustPair { fingerprint } if fingerprint == "fp-1"
        ));
    }

    #[test]
    fn transfer_send_wire_request_parses_paths_and_fingerprint() {
        let request = super::LocalIpcWireRequest {
            proto_version: LOCAL_IPC_PROTO_VERSION,
            request_id: "req-transfer".into(),
            command: "transfer/send".into(),
            payload: Some(json!({
                "peer_fingerprint": "fp-1",
                "paths": ["/tmp/a.txt", "/tmp/b.txt"],
            })),
            auth_context: None,
        };

        let decoded = LocalIpcCommand::from_wire_request(&request).expect("parse request");
        assert!(matches!(
            decoded,
            LocalIpcCommand::TransferSend { peer_fingerprint, paths }
                if peer_fingerprint == "fp-1"
                    && paths == vec!["/tmp/a.txt".to_string(), "/tmp/b.txt".to_string()]
        ));
    }

    #[test]
    fn reserved_phase_a_command_name_is_rejected_until_implemented() {
        let err = LocalIpcCommand::from_wire_request(&super::LocalIpcWireRequest {
            proto_version: LOCAL_IPC_PROTO_VERSION,
            request_id: "req-transfer".into(),
            command: "discover/diagnostics".into(),
            payload: Some(json!({})),
            auth_context: None,
        })
        .expect_err("reserved command should fail before implementation");

        assert_eq!(err.code, "invalid_request");
        assert!(err.message.contains("discover/diagnostics"));
    }

    #[test]
    fn response_envelope_round_trips_with_cbor() {
        let response = LocalEnvelope {
            proto_version: LOCAL_IPC_PROTO_VERSION,
            request_id: "req-2".into(),
            command: LocalIpcResult::ok(LocalIpcResponse::AppConfig {
                config: AppConfig::default(),
            }),
            auth_context: None,
        };
        let bytes = encode_message(&response).expect("serialize response");
        let decoded: LocalEnvelope<LocalIpcResult> =
            decode_message(&bytes).expect("deserialize response");
        assert_eq!(decoded.request_id, "req-2");
        assert!(matches!(
            decoded.command,
            LocalIpcResult::Ok {
                response: LocalIpcResponse::AppConfig { .. }
            }
        ));
    }

    #[test]
    fn wire_response_uses_stable_ok_payload_shape() {
        let response = LocalIpcResponse::AppConfig {
            config: AppConfig::default(),
        }
        .into_wire_response("req-ok", LOCAL_IPC_PROTO_VERSION);
        let value = serde_json::to_value(&response).expect("serialize response");

        assert_eq!(value["proto_version"], json!(LOCAL_IPC_PROTO_VERSION));
        assert_eq!(value["request_id"], json!("req-ok"));
        assert_eq!(value["ok"], json!(true));
        assert_eq!(
            value["payload"]["config"]["device_name"],
            json!("DashDrop Device")
        );
        assert!(value.get("status").is_none());
        assert!(value.get("type").is_none());
    }

    #[test]
    fn connect_by_address_response_uses_stable_payload_shape() {
        let response = LocalIpcResponse::ConnectByAddress {
            result: super::ConnectByAddressResult {
                fingerprint: "fp-1".into(),
                name: "Peer".into(),
                trusted: true,
                address: "127.0.0.1:9443".into(),
            },
        }
        .into_wire_response("req-connect", LOCAL_IPC_PROTO_VERSION);
        let value = serde_json::to_value(&response).expect("serialize response");

        assert_eq!(value["ok"], json!(true));
        assert_eq!(value["payload"]["result"]["fingerprint"], json!("fp-1"));
        assert_eq!(
            value["payload"]["result"]["address"],
            json!("127.0.0.1:9443")
        );
    }

    #[test]
    fn wire_response_uses_stable_error_shape() {
        let response = LocalIpcWireResponse::error(
            "req-err",
            LOCAL_IPC_PROTO_VERSION,
            super::LocalIpcError::dispatch_failed("boom"),
        );
        let value = serde_json::to_value(&response).expect("serialize response");

        assert_eq!(value["ok"], json!(false));
        assert_eq!(value["error"]["code"], json!("dispatch_failed"));
        assert_eq!(value["error"]["message"], json!("boom"));
    }

    #[tokio::test]
    async fn framed_messages_round_trip() {
        let request = LocalRequestEnvelope::new("req-3", LocalIpcCommand::ConfigGet);
        let (mut writer, mut reader) = tokio::io::duplex(1024);

        write_framed_message(&mut writer, &request)
            .await
            .expect("write frame");
        let decoded: LocalRequestEnvelope =
            read_framed_message(&mut reader).await.expect("read frame");

        assert_eq!(decoded.request_id, "req-3");
        assert!(matches!(decoded.command, LocalIpcCommand::ConfigGet));
    }

    #[test]
    fn endpoint_is_stable_for_same_config_dir() {
        let config_dir = PathBuf::from("/tmp/dashdrop-config");
        let lhs = resolve_local_ipc_endpoint(&config_dir);
        let rhs = resolve_local_ipc_endpoint(&config_dir);
        assert_eq!(lhs, rhs);
    }
}
