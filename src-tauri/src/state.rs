use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

use crate::crypto::Identity;

// ─── Platform ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Platform {
    Mac,
    Windows,
    Linux,
    Android,
    Ios,
    Unknown,
}

impl From<&str> for Platform {
    fn from(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "mac" | "macos" => Platform::Mac,
            "win" | "windows" => Platform::Windows,
            "linux" => Platform::Linux,
            "android" => Platform::Android,
            "ios" => Platform::Ios,
            _ => Platform::Unknown,
        }
    }
}

impl Platform {
    pub fn current() -> &'static str {
        #[cfg(target_os = "macos")]
        return "Mac";
        #[cfg(target_os = "windows")]
        return "Windows";
        #[cfg(target_os = "linux")]
        return "Linux";
        #[cfg(target_os = "android")]
        return "Android";
        #[cfg(target_os = "ios")]
        return "Ios";
        #[cfg(not(any(
            target_os = "macos",
            target_os = "windows",
            target_os = "linux",
            target_os = "android",
            target_os = "ios"
        )))]
        return "Unknown";
    }
}

// ─── Device Discovery State ──────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub session_id: String,
    pub addrs: Vec<SocketAddr>,
    pub last_seen_unix: u64,
    pub last_seen_instant: Instant,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReachabilityStatus {
    Discovered,
    Reachable,
    OfflineCandidate,
    Offline,
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub fingerprint: String,
    pub name: String,
    pub platform: Platform,
    pub trusted: bool,
    /// Active sessions (keyed by session_id). Device is "online" if non-empty.
    pub sessions: HashMap<String, SessionInfo>,
    pub last_seen: u64,
    pub reachability: ReachabilityStatus,
    pub probe_fail_count: u32,
    pub last_probe_at: Option<u64>,
    pub last_probe_result: Option<String>,
    pub last_probe_error: Option<String>,
    pub last_probe_error_detail: Option<String>,
    pub last_probe_addr: Option<String>,
    pub last_probe_attempted_addrs: Vec<String>,
    pub last_resolve_raw_addr_count: u32,
    pub last_resolve_usable_addr_count: u32,
    pub last_resolve_hostname: Option<String>,
    pub last_resolve_port: Option<u16>,
    pub last_resolve_at: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct SessionIndexEntry {
    pub fingerprint: String,
    pub session_id: String,
}

impl DeviceInfo {
    fn is_connectable_addr(addr: &SocketAddr) -> bool {
        match addr {
            SocketAddr::V4(_) => true,
            SocketAddr::V6(v6) => !(v6.ip().is_unicast_link_local() && v6.scope_id() == 0),
        }
    }

    /// Build address candidates across all known sessions, preferring newest sessions first.
    pub fn best_addrs(&self) -> Option<Vec<SocketAddr>> {
        let mut sessions: Vec<&SessionInfo> = self.sessions.values().collect();
        sessions.sort_by_key(|s| std::cmp::Reverse(s.last_seen_unix));

        let mut addrs = Vec::new();
        let mut seen = HashSet::new();
        for session in sessions {
            for addr in &session.addrs {
                if seen.insert(*addr) {
                    addrs.push(*addr);
                }
            }
        }

        if addrs.is_empty() {
            None
        } else {
            let connectable: Vec<SocketAddr> = addrs
                .iter()
                .copied()
                .filter(Self::is_connectable_addr)
                .collect();
            if connectable.is_empty() {
                Some(addrs)
            } else {
                Some(connectable)
            }
        }
    }
}

// ─── Transfer State ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileItemMeta {
    pub file_id: u32,
    pub name: String,
    pub rel_path: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TransferDirection {
    Send,
    Receive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TransferStatus {
    Draft,
    PendingAccept,
    Transferring,
    Completed,
    PartialCompleted,
    Rejected,
    CancelledBySender,
    CancelledByReceiver,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferTask {
    pub id: String,
    #[serde(default)]
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
    #[serde(skip)]
    pub source_paths: Option<Vec<String>>,
    #[serde(skip)]
    pub source_path_by_file_id: Option<HashMap<u32, String>>,
    #[serde(skip)]
    pub failed_file_ids: Option<Vec<u32>>,
    #[serde(skip)]
    pub conn: Option<quinn::Connection>,
    #[serde(skip)]
    pub ended_at: Option<std::time::Instant>,
}

// ─── Trusted Peers ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedPeer {
    pub fingerprint: String,
    pub name: String,
    pub paired_at: u64, // Unix timestamp
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(default)]
    pub last_used_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileConflictStrategy {
    Overwrite,
    Rename,
    Skip,
}

fn default_conflict_strategy() -> FileConflictStrategy {
    FileConflictStrategy::Rename
}

fn default_max_parallel_streams() -> u32 {
    4
}

// ─── App Config ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub device_name: String,
    #[serde(default, alias = "auto_accept")]
    pub auto_accept_trusted_only: bool,
    pub download_dir: Option<String>,
    #[serde(default = "default_conflict_strategy")]
    pub file_conflict_strategy: FileConflictStrategy,
    #[serde(default = "default_max_parallel_streams")]
    pub max_parallel_streams: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalIdentityView {
    pub fingerprint: String,
    pub device_name: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityEvent {
    pub id: i64,
    pub event_type: String,
    pub occurred_at_unix: u64,
    pub phase: String,
    pub peer_fingerprint: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TransferMetrics {
    pub completed: u64,
    pub partial: u64,
    pub failed: u64,
    pub cancelled_by_sender: u64,
    pub cancelled_by_receiver: u64,
    pub rejected: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub average_duration_ms: u64,
    pub failure_distribution: HashMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeStatus {
    pub local_port: u16,
    pub mdns_registered: bool,
    pub discovered_devices: usize,
    pub trusted_devices: usize,
    #[serde(default = "default_listener_port_mode")]
    pub listener_port_mode: String,
    #[serde(default = "default_firewall_rule_state")]
    pub firewall_rule_state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserStatus {
    pub active: bool,
    pub restart_count: u64,
    pub last_disconnect_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncomingRequestNotification {
    pub notification_id: String,
    pub transfer_id: String,
    pub active: bool,
    #[serde(default)]
    pub terminal_reason_code: Option<String>,
    pub updated_at_unix: u64,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            device_name: "DashDrop Device".into(),
            auto_accept_trusted_only: false,
            download_dir: None,
            file_conflict_strategy: FileConflictStrategy::Rename,
            max_parallel_streams: default_max_parallel_streams(),
        }
    }
}

fn default_listener_port_mode() -> String {
    "fallback_random".to_string()
}

fn default_firewall_rule_state() -> String {
    "unknown".to_string()
}

// ─── AppState ─────────────────────────────────────────────────────────────────

pub struct AppState {
    pub identity: Identity,

    /// Keyed by fingerprint (stable device identity).
    pub devices: Arc<RwLock<HashMap<String, DeviceInfo>>>,

    /// Reverse index: removal_key/session_id -> session mapping (O(1) ServiceRemoved lookup).
    pub session_index: Arc<RwLock<HashMap<String, SessionIndexEntry>>>,

    /// Active and recent transfers, keyed by transfer UUID.
    pub transfers: Arc<RwLock<HashMap<String, TransferTask>>>,

    /// Trusted (paired) peers, keyed by fingerprint.
    pub trusted_peers: Arc<RwLock<HashMap<String, TrustedPeer>>>,

    /// Application configuration.
    pub config: Arc<RwLock<AppConfig>>,

    /// QUIC server port (set after server binds).
    pub local_port: Arc<RwLock<u16>>,

    /// Pending incoming transfers waiting for user accept/reject.
    /// transfer_id → oneshot sender (true = accept, false = reject).
    pub pending_accepts: Arc<RwLock<HashMap<String, tokio::sync::oneshot::Sender<bool>>>>,

    /// Incoming transfer notification lifecycle keyed by transfer_id.
    pub incoming_request_notifications: Arc<RwLock<HashMap<String, IncomingRequestNotification>>>,

    /// Incoming offer rate limiter keyed by peer fingerprint.
    pub offer_rate_limits: Arc<tokio::sync::Mutex<HashMap<String, (u32, std::time::Instant)>>>,

    /// Incoming connection rate limiter keyed by peer certificate fingerprint.
    pub incoming_conn_rate_limits:
        Arc<tokio::sync::Mutex<HashMap<String, (u32, std::time::Instant)>>>,

    /// Deduplicate noisy fingerprint-changed warnings by session/fingerprint tuple.
    /// key = "{session_id}|{previous_fp}|{current_fp}", value = last_emitted_unix
    pub fingerprint_change_alerts: Arc<tokio::sync::Mutex<HashMap<String, u64>>>,

    /// Initialized QUIC listener endpoint multiplexed for outgoing dials
    pub endpoint: tokio::sync::OnceCell<quinn::Endpoint>,

    /// mDNS Daemon reference to keep it alive
    pub mdns: tokio::sync::OnceCell<Arc<mdns_sd::ServiceDaemon>>,
    /// Last registered local mDNS service fullname for rename/re-registration.
    pub mdns_service_fullname: Arc<RwLock<Option<String>>>,
    /// Effective mDNS interface policy ("all" or "filtered").
    pub mdns_interface_policy: Arc<RwLock<String>>,
    /// Enabled mDNS interface names when interface filtering is active.
    pub mdns_enabled_interfaces: Arc<RwLock<Vec<String>>>,
    /// Last mDNS browser SearchStarted payload for interface/permission diagnostics.
    pub mdns_last_search_started: Arc<RwLock<Option<String>>>,
    /// mDNS browser event counters keyed by event name.
    pub discovery_event_counts: Arc<RwLock<HashMap<String, u64>>>,
    /// Discovery failure counters keyed by reason code.
    pub discovery_failure_counts: Arc<RwLock<HashMap<String, u64>>>,
    /// Browser status snapshot for diagnostics and auto-recovery visibility.
    pub browser_status: Arc<RwLock<BrowserStatus>>,
    /// Pending deferred remove keys to dedupe ServiceRemoved storms.
    pub pending_removed_sessions: Arc<tokio::sync::Mutex<HashSet<String>>>,
    /// Listener mode ("dual_stack" or "ipv4_only_fallback").
    pub listener_mode: Arc<RwLock<String>>,
    /// Listener port mode ("fixed" or "fallback_random").
    pub listener_port_mode: Arc<RwLock<String>>,
    /// Firewall rule state for diagnostics.
    pub firewall_rule_state: Arc<RwLock<String>>,
    /// Listener bind addrs exposed in diagnostics.
    pub listener_addrs: Arc<RwLock<Vec<String>>>,

    /// SQLite Database for persistent transfer history.
    pub db: std::sync::Mutex<rusqlite::Connection>,

    /// Runtime transfer metrics.
    pub metrics: Arc<RwLock<TransferMetrics>>,
}

impl AppState {
    pub fn new(identity: Identity, config: AppConfig, db: rusqlite::Connection) -> Self {
        AppState {
            identity,
            devices: Arc::new(RwLock::new(HashMap::new())),
            session_index: Arc::new(RwLock::new(HashMap::new())),
            transfers: Arc::new(RwLock::new(HashMap::new())),
            trusted_peers: Arc::new(RwLock::new(HashMap::new())),
            config: Arc::new(RwLock::new(config)),
            local_port: Arc::new(RwLock::new(0)),
            pending_accepts: Arc::new(RwLock::new(HashMap::new())),
            incoming_request_notifications: Arc::new(RwLock::new(HashMap::new())),
            offer_rate_limits: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            incoming_conn_rate_limits: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            fingerprint_change_alerts: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            endpoint: tokio::sync::OnceCell::new(),
            mdns: tokio::sync::OnceCell::new(),
            mdns_service_fullname: Arc::new(RwLock::new(None)),
            mdns_interface_policy: Arc::new(RwLock::new("all".to_string())),
            mdns_enabled_interfaces: Arc::new(RwLock::new(Vec::new())),
            mdns_last_search_started: Arc::new(RwLock::new(None)),
            discovery_event_counts: Arc::new(RwLock::new(HashMap::new())),
            discovery_failure_counts: Arc::new(RwLock::new(HashMap::new())),
            browser_status: Arc::new(RwLock::new(BrowserStatus {
                active: false,
                restart_count: 0,
                last_disconnect_at: None,
            })),
            pending_removed_sessions: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            listener_mode: Arc::new(RwLock::new("unknown".to_string())),
            listener_port_mode: Arc::new(RwLock::new(default_listener_port_mode())),
            firewall_rule_state: Arc::new(RwLock::new(default_firewall_rule_state())),
            listener_addrs: Arc::new(RwLock::new(Vec::new())),
            db: std::sync::Mutex::new(db),
            metrics: Arc::new(RwLock::new(TransferMetrics::default())),
        }
    }

    async fn bump_counter(map: &Arc<RwLock<HashMap<String, u64>>>, key: &str) {
        let mut guard = map.write().await;
        let next = guard.get(key).copied().unwrap_or(0).saturating_add(1);
        guard.insert(key.to_string(), next);
    }

    fn now_unix() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    pub async fn ensure_incoming_request_notification(&self, transfer_id: &str) -> String {
        let mut guard = self.incoming_request_notifications.write().await;
        if let Some(existing) = guard.get_mut(transfer_id) {
            existing.active = true;
            existing.updated_at_unix = Self::now_unix();
            return existing.notification_id.clone();
        }

        let notification_id = uuid::Uuid::new_v4().to_string();
        guard.insert(
            transfer_id.to_string(),
            IncomingRequestNotification {
                notification_id: notification_id.clone(),
                transfer_id: transfer_id.to_string(),
                active: true,
                terminal_reason_code: None,
                updated_at_unix: Self::now_unix(),
            },
        );
        notification_id
    }

    pub async fn mark_incoming_request_notification_inactive(
        &self,
        transfer_id: &str,
        reason_code: Option<&str>,
    ) -> Option<String> {
        let mut guard = self.incoming_request_notifications.write().await;
        let entry = guard.get_mut(transfer_id)?;
        entry.active = false;
        entry.updated_at_unix = Self::now_unix();
        if let Some(code) = reason_code {
            entry.terminal_reason_code = Some(code.to_string());
        }
        Some(entry.notification_id.clone())
    }

    pub async fn incoming_request_notification(
        &self,
        transfer_id: &str,
    ) -> Option<IncomingRequestNotification> {
        self.incoming_request_notifications
            .read()
            .await
            .get(transfer_id)
            .cloned()
    }

    pub async fn bump_discovery_event(&self, key: &str) {
        Self::bump_counter(&self.discovery_event_counts, key).await;
    }

    pub async fn bump_discovery_failure(&self, key: &str) {
        Self::bump_counter(&self.discovery_failure_counts, key).await;
    }

    pub async fn discovery_event_counts_snapshot(&self) -> HashMap<String, u64> {
        self.discovery_event_counts.read().await.clone()
    }

    pub async fn discovery_failure_counts_snapshot(&self) -> HashMap<String, u64> {
        self.discovery_failure_counts.read().await.clone()
    }

    pub async fn browser_status_snapshot(&self) -> BrowserStatus {
        self.browser_status.read().await.clone()
    }

    pub async fn is_trusted(&self, fp: &str) -> bool {
        self.trusted_peers.read().await.contains_key(fp)
    }

    pub async fn add_trust(&self, fp: String, name: String) {
        self.trusted_peers.write().await.insert(
            fp.clone(),
            TrustedPeer {
                fingerprint: fp,
                name,
                paired_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                alias: None,
                last_used_at: None,
            },
        );
    }

    pub async fn mark_peer_used(&self, fp: &str) {
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Some(peer) = self.trusted_peers.write().await.get_mut(fp) {
            peer.last_used_at = Some(now_unix);
        }
    }

    pub async fn runtime_status(&self) -> RuntimeStatus {
        RuntimeStatus {
            local_port: *self.local_port.read().await,
            mdns_registered: self.mdns_service_fullname.read().await.is_some(),
            discovered_devices: self.devices.read().await.len(),
            trusted_devices: self.trusted_peers.read().await.len(),
            listener_port_mode: self.listener_port_mode.read().await.clone(),
            firewall_rule_state: self.firewall_rule_state.read().await.clone(),
        }
    }

    pub async fn transfer_metrics(&self) -> TransferMetrics {
        self.metrics.read().await.clone()
    }

    pub async fn record_transfer_terminal(
        &self,
        direction: &TransferDirection,
        status: &TransferStatus,
        bytes_transferred: u64,
    ) {
        let mut metrics = self.metrics.write().await;
        match status {
            TransferStatus::Completed => metrics.completed = metrics.completed.saturating_add(1),
            TransferStatus::PartialCompleted => {
                metrics.partial = metrics.partial.saturating_add(1);
            }
            TransferStatus::Rejected => metrics.rejected = metrics.rejected.saturating_add(1),
            TransferStatus::CancelledBySender => {
                metrics.cancelled_by_sender = metrics.cancelled_by_sender.saturating_add(1);
            }
            TransferStatus::CancelledByReceiver => {
                metrics.cancelled_by_receiver = metrics.cancelled_by_receiver.saturating_add(1);
            }
            TransferStatus::Failed => metrics.failed = metrics.failed.saturating_add(1),
            _ => {}
        }
        match direction {
            TransferDirection::Send => {
                metrics.bytes_sent = metrics.bytes_sent.saturating_add(bytes_transferred);
            }
            TransferDirection::Receive => {
                metrics.bytes_received = metrics.bytes_received.saturating_add(bytes_transferred);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        default_firewall_rule_state, default_listener_port_mode, AppConfig, AppState, DeviceInfo,
        FileConflictStrategy, Platform, ReachabilityStatus, SessionInfo, TransferDirection,
        TransferStatus, TransferTask,
    };
    use crate::crypto::Identity;
    use serde_json::json;
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::str::FromStr;
    use std::time::Instant;

    fn sample_device(addrs: Vec<SocketAddr>) -> DeviceInfo {
        let mut sessions = HashMap::new();
        sessions.insert(
            "s1".to_string(),
            SessionInfo {
                session_id: "s1".to_string(),
                addrs,
                last_seen_unix: 1,
                last_seen_instant: Instant::now(),
            },
        );
        DeviceInfo {
            fingerprint: "fp".to_string(),
            name: "peer".to_string(),
            platform: Platform::Windows,
            trusted: false,
            sessions,
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
        }
    }

    #[test]
    fn best_addrs_prefers_connectable_when_mixed_candidates_exist() {
        let device = sample_device(vec![
            SocketAddr::from_str("[fe80::1]:9443").expect("addr"),
            SocketAddr::from_str("192.168.1.8:9443").expect("addr"),
        ]);
        let best = device.best_addrs().expect("best addrs");
        assert_eq!(
            best,
            vec![SocketAddr::from_str("192.168.1.8:9443").expect("addr")]
        );
    }

    #[test]
    fn best_addrs_keeps_scope_less_link_local_when_only_candidate() {
        let device = sample_device(vec![SocketAddr::from_str("[fe80::1]:9443").expect("addr")]);
        let best = device.best_addrs().expect("best addrs");
        assert_eq!(
            best,
            vec![SocketAddr::from_str("[fe80::1]:9443").expect("addr")]
        );
    }

    #[tokio::test]
    async fn runtime_status_exposes_listener_port_and_firewall_state() {
        let state = AppState::new(
            Identity {
                fingerprint: "fp".to_string(),
                cert_der: Vec::new(),
                key_der: Vec::new(),
                device_name: "DashDrop Test".to_string(),
            },
            AppConfig::default(),
            rusqlite::Connection::open_in_memory().expect("in-memory db"),
        );

        *state.local_port.write().await = 53319;
        *state.listener_port_mode.write().await = "fixed".to_string();
        *state.firewall_rule_state.write().await = "managed".to_string();

        let runtime = state.runtime_status().await;
        assert_eq!(runtime.local_port, 53319);
        assert_eq!(runtime.listener_port_mode, "fixed");
        assert_eq!(runtime.firewall_rule_state, "managed");
    }

    #[test]
    fn runtime_status_defaults_are_backward_safe() {
        assert_eq!(default_listener_port_mode(), "fallback_random");
        assert_eq!(default_firewall_rule_state(), "unknown");
    }

    #[test]
    fn app_config_deserializes_legacy_payload_with_safe_defaults() {
        let config: AppConfig = serde_json::from_value(json!({
            "device_name": "DashDrop Legacy",
            "auto_accept": true,
            "download_dir": null
        }))
        .expect("legacy config should deserialize");

        assert!(config.auto_accept_trusted_only);
        assert_eq!(config.file_conflict_strategy, FileConflictStrategy::Rename);
        assert_eq!(config.max_parallel_streams, 4);
    }

    #[test]
    fn transfer_task_deserializes_legacy_payload_without_batch_id() {
        let task: TransferTask = serde_json::from_value(json!({
            "id": "transfer-1",
            "direction": "Send",
            "peer_fingerprint": "fp-1",
            "peer_name": "Peer 1",
            "items": [],
            "status": "PendingAccept",
            "bytes_transferred": 0,
            "total_bytes": 10,
            "revision": 0,
            "started_at_unix": 1,
            "ended_at_unix": null,
            "terminal_reason_code": null,
            "error": null
        }))
        .expect("legacy payload should deserialize");

        assert_eq!(task.id, "transfer-1");
        assert_eq!(task.batch_id, None);
        assert_eq!(task.direction, TransferDirection::Send);
        assert_eq!(task.status, TransferStatus::PendingAccept);
    }
}
