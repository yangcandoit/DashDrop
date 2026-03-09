use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use serde::{Deserialize, Serialize};

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
        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux", target_os = "android", target_os = "ios")))]
        return "Unknown";
    }
}

// ─── Device Discovery State ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub addrs: Vec<SocketAddr>,
    #[serde(skip)]
    pub last_seen: Instant,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    pub fingerprint: String,
    pub name: String,
    pub platform: Platform,
    pub trusted: bool,
    /// Active sessions (keyed by session_id). Device is "online" if non-empty.
    pub sessions: HashMap<String, SessionInfo>,
    pub last_seen: u64,
}

#[derive(Debug, Clone)]
pub struct SessionIndexEntry {
    pub fingerprint: String,
    pub session_id: String,
}

impl DeviceInfo {
    /// Pick the best reachable addresses (most recently seen session).
    pub fn best_addrs(&self) -> Option<Vec<SocketAddr>> {
        self.sessions
            .values()
            .max_by_key(|s| s.last_seen)
            .map(|s| s.addrs.clone())
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
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    Send,
    Receive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransferStatus {
    AwaitingAccept,
    Transferring,
    Complete,
    PartialSuccess,
    Failed,
    Cancelled,
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
    pub error: Option<String>,
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
}

// ─── App Config ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub device_name: String,
    pub auto_accept: bool,
    pub download_dir: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            device_name: "DashDrop Device".into(),
            auto_accept: false,
            download_dir: None,
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

    /// Initialized QUIC listener endpoint multiplexed for outgoing dials
    pub endpoint: tokio::sync::OnceCell<quinn::Endpoint>,

    /// mDNS Daemon reference to keep it alive
    pub mdns: tokio::sync::OnceCell<Arc<mdns_sd::ServiceDaemon>>,
    /// Last registered local mDNS service fullname for rename/re-registration.
    pub mdns_service_fullname: Arc<RwLock<Option<String>>>,
}

impl AppState {
    pub fn new(identity: Identity, config: AppConfig) -> Self {
        AppState {
            identity,
            devices: Arc::new(RwLock::new(HashMap::new())),
            session_index: Arc::new(RwLock::new(HashMap::new())),
            transfers: Arc::new(RwLock::new(HashMap::new())),
            trusted_peers: Arc::new(RwLock::new(HashMap::new())),
            config: Arc::new(RwLock::new(config)),
            local_port: Arc::new(RwLock::new(0)),
            pending_accepts: Arc::new(RwLock::new(HashMap::new())),
            endpoint: tokio::sync::OnceCell::new(),
            mdns: tokio::sync::OnceCell::new(),
            mdns_service_fullname: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn is_trusted(&self, fp: &str) -> bool {
        self.trusted_peers.read().await.contains_key(fp)
    }

    pub async fn add_trust(&self, fp: String, name: String) {
        self.trusted_peers.write().await.insert(fp.clone(), TrustedPeer {
            fingerprint: fp,
            name,
            paired_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        });
    }
}
