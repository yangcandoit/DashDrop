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
}

#[derive(Debug, Clone)]
pub struct SessionIndexEntry {
    pub fingerprint: String,
    pub session_id: String,
}

impl DeviceInfo {
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
            Some(addrs)
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

#[derive(Debug, Clone, Serialize)]
pub struct TransferTask {
    pub id: String,
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
            offer_rate_limits: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            incoming_conn_rate_limits: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            fingerprint_change_alerts: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            endpoint: tokio::sync::OnceCell::new(),
            mdns: tokio::sync::OnceCell::new(),
            mdns_service_fullname: Arc::new(RwLock::new(None)),
            db: std::sync::Mutex::new(db),
            metrics: Arc::new(RwLock::new(TransferMetrics::default())),
        }
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
