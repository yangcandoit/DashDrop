use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::ble::BleAssistCapsule;
use crate::dto::{DeviceView, TransferView, TrustedPeerView};
use crate::state::{
    AppConfig, LocalIdentityView, RuntimeEventCheckpoint, RuntimeEventFeedSnapshot, RuntimeStatus,
    SecurityEvent, TransferMetrics, TrustVerificationMethod,
};

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalIpcAccessGrant {
    pub access_token: String,
    pub expires_at_unix_ms: u64,
    pub refresh_after_unix_ms: u64,
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
#[serde(deny_unknown_fields)]
pub struct LocalIpcWireSuccess {
    pub proto_version: u16,
    pub request_id: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingIncomingRequestPayload {
    pub transfer_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
    pub notification_id: String,
    pub sender_name: String,
    pub sender_fp: String,
    pub trusted: bool,
    pub items: Vec<crate::state::FileItemMeta>,
    pub total_size: u64,
    pub revision: u64,
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
    AuthIssue,
    AuthRevoke,
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
    TrustConfirmVerification {
        fingerprint: String,
        verification_method: TrustVerificationMethod,
        mutual_confirmation: bool,
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
    TransferList,
    TransferGet {
        transfer_id: String,
    },
    TransferHistory {
        limit: u32,
        offset: u32,
    },
    TransferPendingIncoming,
    AppActivate {
        paths: Vec<String>,
        pairing_links: Vec<String>,
    },
    AppGetLocalIdentity,
    AppGetBleAssistCapsule,
    AppIngestBleAssistCapsule {
        capsule: BleAssistCapsule,
        source: Option<String>,
    },
    AppGetRuntimeStatus,
    AppGetDiscoveryDiagnostics,
    AppGetEventCheckpoint {
        consumer_id: String,
    },
    AppGetEventFeed {
        after_seq: u64,
        limit: u32,
    },
    AppSetEventCheckpoint {
        consumer_id: String,
        generation: String,
        seq: u64,
    },
    AppQueueExternalShare {
        paths: Vec<String>,
    },
    SecurityGetEvents {
        limit: u32,
        offset: u32,
    },
    TransferGetMetrics,
    SecurityGetPosture,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalIpcCommandClass {
    AuthBootstrap,
    ReadSide,
    WriteSide,
    ActivationShareHandoff,
    RuntimeEventReplay,
}

impl LocalIpcCommand {
    pub fn class(&self) -> LocalIpcCommandClass {
        match self {
            Self::AuthIssue | Self::AuthRevoke => LocalIpcCommandClass::AuthBootstrap,
            Self::DiscoverList
            | Self::TrustList
            | Self::ConfigGet
            | Self::TransferList
            | Self::TransferGet { .. }
            | Self::TransferHistory { .. }
            | Self::TransferPendingIncoming
            | Self::AppGetLocalIdentity
            | Self::AppGetBleAssistCapsule
            | Self::AppGetRuntimeStatus
            | Self::AppGetDiscoveryDiagnostics
            | Self::AppGetEventCheckpoint { .. }
            | Self::SecurityGetEvents { .. }
            | Self::TransferGetMetrics
            | Self::SecurityGetPosture => LocalIpcCommandClass::ReadSide,
            Self::AppActivate { .. } | Self::AppQueueExternalShare { .. } => {
                LocalIpcCommandClass::ActivationShareHandoff
            }
            Self::AppGetEventFeed { .. } => LocalIpcCommandClass::RuntimeEventReplay,
            Self::DiscoverConnectByAddress { .. }
            | Self::TrustPair { .. }
            | Self::TrustUnpair { .. }
            | Self::TrustSetAlias { .. }
            | Self::TrustConfirmVerification { .. }
            | Self::ConfigSet { .. }
            | Self::AppIngestBleAssistCapsule { .. }
            | Self::AppSetEventCheckpoint { .. }
            | Self::TransferSend { .. }
            | Self::TransferAccept { .. }
            | Self::TransferReject { .. }
            | Self::TransferCancel { .. }
            | Self::TransferCancelAll
            | Self::TransferRetry { .. } => LocalIpcCommandClass::WriteSide,
        }
    }

    pub fn endpoint_kind(&self) -> LocalIpcEndpointKind {
        match self.class() {
            LocalIpcCommandClass::AuthBootstrap => LocalIpcEndpointKind::Service,
            LocalIpcCommandClass::ActivationShareHandoff => LocalIpcEndpointKind::UiActivation,
            LocalIpcCommandClass::ReadSide
            | LocalIpcCommandClass::WriteSide
            | LocalIpcCommandClass::RuntimeEventReplay => LocalIpcEndpointKind::Service,
        }
    }

    pub fn requires_auth(&self) -> bool {
        matches!(
            self.class(),
            LocalIpcCommandClass::ReadSide
                | LocalIpcCommandClass::WriteSide
                | LocalIpcCommandClass::RuntimeEventReplay
        )
    }

    pub fn accepts_on_endpoint(&self, endpoint_kind: LocalIpcEndpointKind) -> bool {
        self.endpoint_kind() == endpoint_kind
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::AuthIssue => "auth/issue",
            Self::AuthRevoke => "auth/revoke",
            Self::DiscoverList => "discover/list",
            Self::DiscoverConnectByAddress { .. } => "discover/connect_by_address",
            Self::TrustList => "trust/list",
            Self::TrustPair { .. } => "trust/pair",
            Self::TrustUnpair { .. } => "trust/unpair",
            Self::TrustSetAlias { .. } => "trust/set_alias",
            Self::TrustConfirmVerification { .. } => "trust/confirm_verification",
            Self::ConfigGet => "config/get",
            Self::ConfigSet { .. } => "config/set",
            Self::TransferSend { .. } => "transfer/send",
            Self::TransferAccept { .. } => "transfer/accept",
            Self::TransferReject { .. } => "transfer/reject",
            Self::TransferCancel { .. } => "transfer/cancel",
            Self::TransferCancelAll => "transfer/cancel_all",
            Self::TransferRetry { .. } => "transfer/retry",
            Self::TransferList => "transfer/list",
            Self::TransferGet { .. } => "transfer/get",
            Self::TransferHistory { .. } => "transfer/history",
            Self::TransferPendingIncoming => "transfer/pending_incoming",
            Self::AppActivate { .. } => "app/activate",
            Self::AppGetLocalIdentity => "app/get_local_identity",
            Self::AppGetBleAssistCapsule => "app/get_ble_assist_capsule",
            Self::AppIngestBleAssistCapsule { .. } => "app/ingest_ble_assist_capsule",
            Self::AppGetRuntimeStatus => "app/get_runtime_status",
            Self::AppGetDiscoveryDiagnostics => "app/get_discovery_diagnostics",
            Self::AppGetEventCheckpoint { .. } => "app/get_event_checkpoint",
            Self::AppGetEventFeed { .. } => "app/get_event_feed",
            Self::AppSetEventCheckpoint { .. } => "app/set_event_checkpoint",
            Self::AppQueueExternalShare { .. } => "app/queue_external_share",
            Self::SecurityGetEvents { .. } => "security/get_events",
            Self::TransferGetMetrics => "transfer/get_metrics",
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
            Self::AuthIssue
            | Self::AuthRevoke
            | Self::DiscoverList
            | Self::TrustList
            | Self::ConfigGet
            | Self::TransferCancelAll
            | Self::TransferList
            | Self::TransferPendingIncoming
            | Self::AppGetLocalIdentity
            | Self::AppGetBleAssistCapsule
            | Self::AppGetRuntimeStatus
            | Self::AppGetDiscoveryDiagnostics
            | Self::TransferGetMetrics
            | Self::SecurityGetPosture => None,
            Self::DiscoverConnectByAddress { address } => Some(json!({ "address": address })),
            Self::TrustPair { fingerprint } | Self::TrustUnpair { fingerprint } => {
                Some(json!({ "fingerprint": fingerprint }))
            }
            Self::TrustSetAlias { fingerprint, alias } => Some(json!({
                "fingerprint": fingerprint,
                "alias": alias,
            })),
            Self::TrustConfirmVerification {
                fingerprint,
                verification_method,
                mutual_confirmation,
            } => Some(json!({
                "fingerprint": fingerprint,
                "verification_method": verification_method,
                "mutual_confirmation": mutual_confirmation,
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
            Self::TransferCancel { transfer_id } | Self::TransferRetry { transfer_id } => {
                Some(json!({ "transfer_id": transfer_id }))
            }
            Self::TransferGet { transfer_id } => Some(json!({ "transfer_id": transfer_id })),
            Self::TransferHistory { limit, offset } | Self::SecurityGetEvents { limit, offset } => {
                Some(json!({ "limit": limit, "offset": offset }))
            }
            Self::AppGetEventFeed { after_seq, limit } => {
                Some(json!({ "after_seq": after_seq, "limit": limit }))
            }
            Self::AppGetEventCheckpoint { consumer_id } => {
                Some(json!({ "consumer_id": consumer_id }))
            }
            Self::AppSetEventCheckpoint {
                consumer_id,
                generation,
                seq,
            } => Some(json!({
                "consumer_id": consumer_id,
                "generation": generation,
                "seq": seq,
            })),
            Self::AppActivate {
                paths,
                pairing_links,
            } => Some(json!({
                "paths": paths,
                "pairing_links": pairing_links,
            })),
            Self::AppIngestBleAssistCapsule { capsule, source } => Some(json!({
                "capsule": capsule,
                "source": source,
            })),
            Self::AppQueueExternalShare { paths } => Some(json!({ "paths": paths })),
        }
    }

    fn from_name_and_payload(
        command: &str,
        payload: Option<&Value>,
    ) -> Result<Self, LocalIpcError> {
        match command {
            "auth/issue" => Ok(Self::AuthIssue),
            "auth/revoke" => Ok(Self::AuthRevoke),
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
            "trust/confirm_verification" => Ok(Self::TrustConfirmVerification {
                fingerprint: required_string(payload, "fingerprint")?,
                verification_method: required_value(payload, "verification_method")?,
                mutual_confirmation: optional_bool(payload, "mutual_confirmation")?.unwrap_or(false),
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
            "transfer/list" => Ok(Self::TransferList),
            "transfer/get" => Ok(Self::TransferGet {
                transfer_id: required_string(payload, "transfer_id")?,
            }),
            "transfer/history" => Ok(Self::TransferHistory {
                limit: required_value(payload, "limit")?,
                offset: required_value(payload, "offset")?,
            }),
            "transfer/pending_incoming" => Ok(Self::TransferPendingIncoming),
            "app/activate" => Ok(Self::AppActivate {
                paths: required_string_vec(payload, "paths")?,
                pairing_links: optional_string_vec(payload, "pairing_links")?,
            }),
            "app/get_local_identity" => Ok(Self::AppGetLocalIdentity),
            "app/get_ble_assist_capsule" => Ok(Self::AppGetBleAssistCapsule),
            "app/ingest_ble_assist_capsule" => Ok(Self::AppIngestBleAssistCapsule {
                capsule: required_value(payload, "capsule")?,
                source: optional_string(payload, "source")?,
            }),
            "app/get_runtime_status" => Ok(Self::AppGetRuntimeStatus),
            "app/get_discovery_diagnostics" => Ok(Self::AppGetDiscoveryDiagnostics),
            "app/get_event_checkpoint" => Ok(Self::AppGetEventCheckpoint {
                consumer_id: required_string(payload, "consumer_id")?,
            }),
            "app/get_event_feed" => Ok(Self::AppGetEventFeed {
                after_seq: required_value(payload, "after_seq")?,
                limit: required_value(payload, "limit")?,
            }),
            "app/set_event_checkpoint" => Ok(Self::AppSetEventCheckpoint {
                consumer_id: required_string(payload, "consumer_id")?,
                generation: required_string(payload, "generation")?,
                seq: required_value(payload, "seq")?,
            }),
            "app/queue_external_share" => Ok(Self::AppQueueExternalShare {
                paths: required_string_vec(payload, "paths")?,
            }),
            "security/get_events" => Ok(Self::SecurityGetEvents {
                limit: required_value(payload, "limit")?,
                offset: required_value(payload, "offset")?,
            }),
            "transfer/get_metrics" => Ok(Self::TransferGetMetrics),
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
    AccessGrant {
        auth: LocalIpcAccessGrant,
    },
    Ack,
    Devices {
        devices: Vec<DeviceView>,
    },
    ConnectByAddress {
        result: ConnectByAddressResult,
    },
    TrustedPeers {
        trusted_peers: Vec<TrustedPeerView>,
    },
    AppConfig {
        config: AppConfig,
    },
    CancelledTransfers {
        count: u32,
    },
    Transfers {
        transfers: Vec<TransferView>,
    },
    Transfer {
        transfer: Option<TransferView>,
    },
    TransferHistory {
        history: Vec<TransferView>,
    },
    PendingIncomingRequests {
        requests: Vec<PendingIncomingRequestPayload>,
    },
    LocalIdentity {
        identity: LocalIdentityView,
    },
    BleAssistCapsule {
        capsule: BleAssistCapsule,
    },
    RuntimeStatus {
        runtime_status: RuntimeStatus,
    },
    DiscoveryDiagnostics {
        diagnostics: serde_json::Value,
    },
    RuntimeEvents {
        feed: Box<RuntimeEventFeedSnapshot>,
    },
    RuntimeEventCheckpoint {
        checkpoint: Option<RuntimeEventCheckpoint>,
    },
    SecurityEvents {
        events: Vec<SecurityEvent>,
    },
    TransferMetrics {
        metrics: TransferMetrics,
    },
    SecurityPosture {
        posture: serde_json::Value,
    },
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
            Self::AccessGrant { auth } => Some(json!({ "auth": auth })),
            Self::Ack => None,
            Self::Devices { devices } => Some(json!({ "devices": devices })),
            Self::ConnectByAddress { result } => Some(json!({ "result": result })),
            Self::TrustedPeers { trusted_peers } => Some(json!({ "trusted_peers": trusted_peers })),
            Self::AppConfig { config } => Some(json!({ "config": config })),
            Self::CancelledTransfers { count } => Some(json!({ "count": count })),
            Self::Transfers { transfers } => Some(json!({ "transfers": transfers })),
            Self::Transfer { transfer } => Some(json!({ "transfer": transfer })),
            Self::TransferHistory { history } => Some(json!({ "history": history })),
            Self::PendingIncomingRequests { requests } => Some(json!({ "requests": requests })),
            Self::LocalIdentity { identity } => Some(json!({ "identity": identity })),
            Self::BleAssistCapsule { capsule } => Some(json!({ "capsule": capsule })),
            Self::RuntimeStatus { runtime_status } => {
                Some(json!({ "runtime_status": runtime_status }))
            }
            Self::DiscoveryDiagnostics { diagnostics } => {
                Some(json!({ "diagnostics": diagnostics }))
            }
            Self::RuntimeEvents { feed } => serde_json::to_value(feed).ok(),
            Self::RuntimeEventCheckpoint { checkpoint } => {
                Some(json!({ "checkpoint": checkpoint }))
            }
            Self::SecurityEvents { events } => Some(json!({ "events": events })),
            Self::TransferMetrics { metrics } => Some(json!({ "metrics": metrics })),
            Self::SecurityPosture { posture } => Some(json!({ "posture": posture })),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[allow(dead_code)]
#[allow(clippy::large_enum_variant)]
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

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new("unauthorized", message)
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

fn optional_bool(payload: Option<&Value>, field: &str) -> Result<Option<bool>, LocalIpcError> {
    match payload_object(payload)?.get(field) {
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(LocalIpcError::invalid_request(format!(
            "payload.{field} must be a boolean or null"
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

fn optional_string_vec(payload: Option<&Value>, field: &str) -> Result<Vec<String>, LocalIpcError> {
    match payload_object(payload)?.get(field) {
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str().map(str::to_string).ok_or_else(|| {
                    LocalIpcError::invalid_request(format!(
                        "payload.{field} must be an array of strings"
                    ))
                })
            })
            .collect(),
        Some(Value::Null) | None => Ok(Vec::new()),
        Some(_) => Err(LocalIpcError::invalid_request(format!(
            "payload.{field} must be an array of strings"
        ))),
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalIpcEndpointKind {
    Service,
    UiActivation,
}

impl LocalIpcEndpointKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::UiActivation => "ui activation",
        }
    }
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
    resolve_local_ipc_endpoint_for_kind(config_dir, LocalIpcEndpointKind::Service)
}

pub fn resolve_local_ipc_endpoint_for_kind(
    config_dir: &Path,
    kind: LocalIpcEndpointKind,
) -> LocalIpcEndpoint {
    let key = hashed_endpoint_key(config_dir);
    let suffix = match kind {
        LocalIpcEndpointKind::Service => "service",
        LocalIpcEndpointKind::UiActivation => "ui",
    };

    #[cfg(unix)]
    {
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            let path = PathBuf::from(runtime_dir).join(format!("dashdrop-{suffix}-{key}.sock"));
            return LocalIpcEndpoint::UnixSocket { path };
        }

        let base = if Path::new("/tmp").exists() {
            PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        LocalIpcEndpoint::UnixSocket {
            path: base.join(format!("dashdrop-{suffix}-{key}.sock")),
        }
    }

    #[cfg(windows)]
    {
        return LocalIpcEndpoint::WindowsNamedPipe {
            path: format!(r"\\.\pipe\dashdrop-{suffix}-v1-{key}"),
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
        resolve_local_ipc_endpoint_for_kind, write_framed_message, LocalAuthContext, LocalEnvelope,
        LocalIpcCommand, LocalIpcEndpoint, LocalIpcEndpointKind, LocalIpcResponse, LocalIpcResult,
        LocalIpcWireResponse, LocalRequestEnvelope, LOCAL_IPC_PROTO_VERSION,
    };
    use crate::state::{AppConfig, RuntimeEventCheckpoint};
    use serde_json::json;

    #[test]
    fn local_command_names_match_target_shape() {
        assert_eq!(LocalIpcCommand::AuthIssue.name(), "auth/issue");
        assert_eq!(LocalIpcCommand::AuthRevoke.name(), "auth/revoke");
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
        assert_eq!(
            LocalIpcCommand::AppGetDiscoveryDiagnostics.name(),
            "app/get_discovery_diagnostics"
        );
        assert_eq!(
            LocalIpcCommand::AppGetEventCheckpoint {
                consumer_id: "shared-ui".into(),
            }
            .name(),
            "app/get_event_checkpoint"
        );
        assert_eq!(LocalIpcCommand::TransferList.name(), "transfer/list");
        assert_eq!(
            LocalIpcCommand::TransferGet {
                transfer_id: "tx-1".into()
            }
            .name(),
            "transfer/get"
        );
        assert_eq!(
            LocalIpcCommand::TransferHistory {
                limit: 20,
                offset: 0
            }
            .name(),
            "transfer/history"
        );
        assert_eq!(
            LocalIpcCommand::TransferPendingIncoming.name(),
            "transfer/pending_incoming"
        );
        assert_eq!(
            LocalIpcCommand::SecurityGetEvents {
                limit: 20,
                offset: 0
            }
            .name(),
            "security/get_events"
        );
        assert_eq!(
            LocalIpcCommand::TransferGetMetrics.name(),
            "transfer/get_metrics"
        );
        assert_eq!(
            LocalIpcCommand::AppGetEventFeed {
                after_seq: 10,
                limit: 20,
            }
            .name(),
            "app/get_event_feed"
        );
        assert_eq!(
            LocalIpcCommand::AppActivate {
                paths: vec!["/tmp/a.txt".into()],
                pairing_links: Vec::new(),
            }
            .name(),
            "app/activate"
        );
    }

    #[test]
    fn endpoint_kind_resolves_to_distinct_paths() {
        let config_dir = PathBuf::from("/tmp/dashdrop-endpoint-kind-test");
        let service =
            resolve_local_ipc_endpoint_for_kind(&config_dir, LocalIpcEndpointKind::Service);
        let ui =
            resolve_local_ipc_endpoint_for_kind(&config_dir, LocalIpcEndpointKind::UiActivation);

        assert_ne!(service.describe(), ui.describe());

        #[cfg(unix)]
        {
            let service_path = match service {
                LocalIpcEndpoint::UnixSocket { path } => path,
            };
            let ui_path = match ui {
                LocalIpcEndpoint::UnixSocket { path } => path,
            };
            assert!(service_path.to_string_lossy().contains("service"));
            assert!(ui_path.to_string_lossy().contains("ui"));
        }

        #[cfg(windows)]
        {
            let service_path = match service {
                LocalIpcEndpoint::WindowsNamedPipe { path } => path,
            };
            let ui_path = match ui {
                LocalIpcEndpoint::WindowsNamedPipe { path } => path,
            };
            assert!(service_path.contains("service"));
            assert!(ui_path.contains("ui"));
        }
    }

    #[test]
    fn command_classes_capture_control_plane_roles() {
        assert_eq!(
            LocalIpcCommand::AuthIssue.class(),
            super::LocalIpcCommandClass::AuthBootstrap
        );
        assert_eq!(
            LocalIpcCommand::AuthRevoke.class(),
            super::LocalIpcCommandClass::AuthBootstrap
        );
        assert_eq!(
            LocalIpcCommand::DiscoverList.class(),
            super::LocalIpcCommandClass::ReadSide
        );
        assert_eq!(
            LocalIpcCommand::TransferSend {
                peer_fingerprint: "fp-1".into(),
                paths: vec!["/tmp/a.txt".into()],
            }
            .class(),
            super::LocalIpcCommandClass::WriteSide
        );
        assert_eq!(
            LocalIpcCommand::AppActivate {
                paths: vec!["/tmp/a.txt".into()],
                pairing_links: Vec::new(),
            }
            .class(),
            super::LocalIpcCommandClass::ActivationShareHandoff
        );
        assert_eq!(
            LocalIpcCommand::AppGetEventFeed {
                after_seq: 0,
                limit: 10,
            }
            .class(),
            super::LocalIpcCommandClass::RuntimeEventReplay
        );
        assert_eq!(
            LocalIpcCommand::AppGetEventCheckpoint {
                consumer_id: "shared-ui".into(),
            }
            .class(),
            super::LocalIpcCommandClass::ReadSide
        );
    }

    #[test]
    fn command_endpoint_routing_matches_command_role() {
        assert_eq!(
            LocalIpcCommand::AuthIssue.endpoint_kind(),
            LocalIpcEndpointKind::Service
        );
        assert!(!LocalIpcCommand::AuthIssue.requires_auth());
        assert_eq!(
            LocalIpcCommand::AuthRevoke.endpoint_kind(),
            LocalIpcEndpointKind::Service
        );
        assert!(!LocalIpcCommand::AuthRevoke.requires_auth());
        assert_eq!(
            LocalIpcCommand::ConfigGet.endpoint_kind(),
            LocalIpcEndpointKind::Service
        );
        assert!(LocalIpcCommand::ConfigGet.requires_auth());
        assert_eq!(
            LocalIpcCommand::AppGetEventFeed {
                after_seq: 0,
                limit: 10,
            }
            .endpoint_kind(),
            LocalIpcEndpointKind::Service
        );
        assert!(LocalIpcCommand::AppGetEventCheckpoint {
            consumer_id: "shared-ui".into(),
        }
        .requires_auth());
        assert_eq!(
            LocalIpcCommand::AppQueueExternalShare {
                paths: vec!["/tmp/a.txt".into()],
            }
            .endpoint_kind(),
            LocalIpcEndpointKind::UiActivation
        );
        assert!(LocalIpcCommand::AppActivate {
            paths: vec!["/tmp/a.txt".into()],
            pairing_links: Vec::new(),
        }
        .accepts_on_endpoint(LocalIpcEndpointKind::UiActivation));
        assert!(!LocalIpcCommand::AppActivate {
            paths: vec!["/tmp/a.txt".into()],
            pairing_links: Vec::new(),
        }
        .accepts_on_endpoint(LocalIpcEndpointKind::Service));
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
    fn auth_revoke_wire_request_round_trips_without_payload() {
        let request = LocalIpcCommand::AuthRevoke.to_wire_request(
            "req-auth-revoke",
            Some(LocalAuthContext {
                access_token: Some("token-1".into()),
            }),
        );

        let decoded = LocalIpcCommand::from_wire_request(&request).expect("parse request");
        assert!(matches!(decoded, LocalIpcCommand::AuthRevoke));
        assert!(request.payload.is_none());
        assert_eq!(
            request
                .auth_context
                .as_ref()
                .and_then(|ctx| ctx.access_token.as_deref()),
            Some("token-1")
        );
    }

    #[test]
    fn app_event_checkpoint_wire_request_round_trips() {
        let request = LocalIpcCommand::AppSetEventCheckpoint {
            consumer_id: "shared-ui".into(),
            generation: "gen-1".into(),
            seq: 42,
        }
        .to_wire_request("req-checkpoint", None);

        let decoded = LocalIpcCommand::from_wire_request(&request).expect("parse request");
        assert!(matches!(
            decoded,
            LocalIpcCommand::AppSetEventCheckpoint {
                consumer_id,
                generation,
                seq
            } if consumer_id == "shared-ui" && generation == "gen-1" && seq == 42
        ));
    }

    #[test]
    fn runtime_event_checkpoint_response_preserves_optional_metadata() {
        let payload = LocalIpcResponse::RuntimeEventCheckpoint {
            checkpoint: Some(RuntimeEventCheckpoint {
                consumer_id: "shared-ui".into(),
                generation: "gen-1".into(),
                seq: 42,
                updated_at_unix_ms: 1_234,
                created_at_unix_ms: Some(1_200),
                last_read_at_unix_ms: Some(1_235),
                lease_expires_at_unix_ms: Some(1_534),
                revision: Some(3),
                last_transition: Some("advanced".into()),
                recovery_hint: Some("persisted_catch_up".into()),
                current_oldest_available_seq: Some(21),
                current_latest_available_seq: Some(84),
                current_compaction_watermark_seq: Some(20),
                current_compaction_watermark_segment_id: Some(1),
            }),
        }
        .payload()
        .expect("payload");

        assert_eq!(payload["checkpoint"]["consumer_id"], json!("shared-ui"));
        assert_eq!(payload["checkpoint"]["revision"], json!(3));
        assert_eq!(payload["checkpoint"]["last_transition"], json!("advanced"));
        assert_eq!(
            payload["checkpoint"]["recovery_hint"],
            json!("persisted_catch_up")
        );
        assert_eq!(
            payload["checkpoint"]["current_compaction_watermark_seq"],
            json!(20)
        );
    }

    #[test]
    fn app_activate_wire_request_round_trips_pairing_links() {
        let request = LocalIpcCommand::AppActivate {
            paths: vec!["/tmp/example.txt".into()],
            pairing_links: vec!["dashdrop://pair?data=abc".into()],
        }
        .to_wire_request("req-activate", None);

        let decoded = LocalIpcCommand::from_wire_request(&request).expect("parse request");
        assert!(matches!(
            decoded,
            LocalIpcCommand::AppActivate {
                paths,
                pairing_links
            } if paths == vec!["/tmp/example.txt".to_string()]
                && pairing_links == vec!["dashdrop://pair?data=abc".to_string()]
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
    fn error_wire_response_round_trips_as_error_variant() {
        let response = LocalIpcWireResponse::error(
            "req-error",
            LOCAL_IPC_PROTO_VERSION,
            super::LocalIpcError::invalid_request("bad request"),
        );

        let bytes = encode_message(&response).expect("serialize response");
        let decoded: LocalIpcWireResponse = decode_message(&bytes).expect("deserialize response");

        assert!(matches!(
            decoded,
            LocalIpcWireResponse::Err(super::LocalIpcWireErrorResponse { request_id, ok, .. })
                if request_id == "req-error" && !ok
        ));
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

    #[test]
    fn access_grant_response_uses_stable_payload_shape() {
        let response = LocalIpcResponse::AccessGrant {
            auth: super::LocalIpcAccessGrant {
                access_token: "token-1".into(),
                expires_at_unix_ms: 100,
                refresh_after_unix_ms: 50,
            },
        }
        .into_wire_response("req-auth", LOCAL_IPC_PROTO_VERSION);
        let value = serde_json::to_value(&response).expect("serialize response");

        assert_eq!(value["ok"], json!(true));
        assert_eq!(value["payload"]["auth"]["access_token"], json!("token-1"));
        assert_eq!(value["payload"]["auth"]["expires_at_unix_ms"], json!(100));
        assert_eq!(value["payload"]["auth"]["refresh_after_unix_ms"], json!(50));
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
