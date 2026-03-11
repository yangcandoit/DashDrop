use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::state::{
    DeviceInfo, FileItemMeta, LocalIdentityView, Platform, ReachabilityStatus, SessionInfo,
    TransferDirection, TransferStatus, TransferTask, TrustedPeer,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionView {
    pub session_id: String,
    pub addrs: Vec<String>,
    pub last_seen_unix: u64,
}

impl From<&SessionInfo> for SessionView {
    fn from(value: &SessionInfo) -> Self {
        Self {
            session_id: value.session_id.clone(),
            addrs: value.addrs.iter().map(|addr| addr.to_string()).collect(),
            last_seen_unix: value.last_seen_unix,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceView {
    pub fingerprint: String,
    pub name: String,
    pub platform: Platform,
    pub trusted: bool,
    pub sessions: HashMap<String, SessionView>,
    pub last_seen: u64,
    pub reachability: ReachabilityStatus,
    pub probe_fail_count: u32,
    pub last_probe_at: Option<u64>,
}

impl From<&DeviceInfo> for DeviceView {
    fn from(value: &DeviceInfo) -> Self {
        Self {
            fingerprint: value.fingerprint.clone(),
            name: value.name.clone(),
            platform: value.platform.clone(),
            trusted: value.trusted,
            sessions: value
                .sessions
                .iter()
                .map(|(k, v)| (k.clone(), SessionView::from(v)))
                .collect(),
            last_seen: value.last_seen,
            reachability: value.reachability.clone(),
            probe_fail_count: value.probe_fail_count,
            last_probe_at: value.last_probe_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferView {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
    pub direction: TransferDirection,
    pub peer_fingerprint: String,
    pub peer_name: String,
    pub items: Vec<FileItemMeta>,
    pub status: TransferStatus,
    pub bytes_transferred: u64,
    pub total_bytes: u64,
    pub revision: u64,
    pub started_at_unix: u64,
    pub ended_at_unix: Option<u64>,
    pub terminal_reason_code: Option<String>,
    pub error: Option<String>,
}

impl From<&TransferTask> for TransferView {
    fn from(value: &TransferTask) -> Self {
        Self {
            id: value.id.clone(),
            batch_id: value.batch_id.clone(),
            direction: value.direction.clone(),
            peer_fingerprint: value.peer_fingerprint.clone(),
            peer_name: value.peer_name.clone(),
            items: value.items.clone(),
            status: value.status.clone(),
            bytes_transferred: value.bytes_transferred,
            total_bytes: value.total_bytes,
            revision: value.revision,
            started_at_unix: value.started_at_unix,
            ended_at_unix: value.ended_at_unix,
            terminal_reason_code: value.terminal_reason_code.clone(),
            error: value.error.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedPeerView {
    pub fingerprint: String,
    pub name: String,
    pub paired_at: u64,
    pub alias: Option<String>,
    pub last_used_at: Option<u64>,
}

impl From<&TrustedPeer> for TrustedPeerView {
    fn from(value: &TrustedPeer) -> Self {
        Self {
            fingerprint: value.fingerprint.clone(),
            name: value.name.clone(),
            paired_at: value.paired_at,
            alias: value.alias.clone(),
            last_used_at: value.last_used_at,
        }
    }
}

pub fn local_identity_view(
    fingerprint: String,
    device_name: String,
    port: u16,
) -> LocalIdentityView {
    LocalIdentityView {
        fingerprint,
        device_name,
        port,
    }
}
