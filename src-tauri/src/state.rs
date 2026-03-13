use crate::persistence_progress::TransferProgressPersistence;
use crate::transport::protocol::RiskClass;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{Mutex, RwLock};

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_class: Option<RiskClass>,
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
    #[serde(default = "default_trust_level")]
    pub trust_level: TrustLevel,
    #[serde(default = "default_trust_verification_method")]
    pub last_verification_method: TrustVerificationMethod,
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(default)]
    pub last_used_at: Option<u64>,
    #[serde(default)]
    pub remote_confirmation_material_seen_at: Option<u64>,
    #[serde(default)]
    pub local_confirmation_at: Option<u64>,
    #[serde(default)]
    pub mutual_confirmed_at: Option<u64>,
    #[serde(default)]
    pub frozen_at: Option<u64>,
    #[serde(default)]
    pub freeze_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    LegacyPaired,
    SignedLinkVerified,
    MutualConfirmed,
    Frozen,
}

impl TrustLevel {
    pub fn max(self, other: Self) -> Self {
        if self >= other {
            self
        } else {
            other
        }
    }

    pub fn apply_verification(self, requested: Self) -> Self {
        match (self, requested) {
            (Self::Frozen, next) if next != Self::Frozen => next,
            (current, next) => current.max(next),
        }
    }

    pub fn allows_sensitive_send(&self) -> bool {
        !matches!(self, Self::Frozen)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrustVerificationMethod {
    ManualPairing,
    LegacyUnsignedLink,
    SignedPairingLink,
    MutualReceipt,
}

fn default_trust_level() -> TrustLevel {
    TrustLevel::LegacyPaired
}

fn default_trust_verification_method() -> TrustVerificationMethod {
    TrustVerificationMethod::ManualPairing
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

fn default_launch_at_login() -> bool {
    false
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
    #[serde(default = "default_launch_at_login")]
    pub launch_at_login: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BleAssistObservation {
    pub rolling_identifier: String,
    pub integrity_tag: String,
    pub first_seen_at_unix_ms: u64,
    pub last_seen_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
    pub transport_hint: String,
    pub qr_fallback_available: bool,
    pub short_code_fallback_available: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct DeviceSloObservability {
    pub remote_peer_online_at: Option<u64>,
    pub local_device_visible_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct TransferSloObservability {
    #[serde(default)]
    pub peer_fingerprint: Option<String>,
    pub sender_dispatch_at: Option<u64>,
    pub receiver_prompted_at: Option<u64>,
    pub receiver_fallback_prompted_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct SloObservabilitySnapshot {
    #[serde(default)]
    pub devices: HashMap<String, DeviceSloObservability>,
    #[serde(default)]
    pub transfers: HashMap<String, TransferSloObservability>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeStatus {
    pub local_port: u16,
    pub mdns_registered: bool,
    pub discovered_devices: usize,
    pub trusted_devices: usize,
    #[serde(default = "default_requested_control_plane_mode")]
    pub requested_control_plane_mode: String,
    #[serde(default = "default_control_plane_mode")]
    pub control_plane_mode: String,
    #[serde(default = "default_runtime_profile")]
    pub runtime_profile: String,
    #[serde(default = "default_daemon_status")]
    pub daemon_status: String,
    #[serde(default)]
    pub daemon_connect_attempts: u32,
    #[serde(default = "default_daemon_connect_strategy")]
    pub daemon_connect_strategy: String,
    #[serde(default)]
    pub daemon_binary_path: Option<String>,
    #[serde(default = "default_listener_port_mode")]
    pub listener_port_mode: String,
    #[serde(default = "default_firewall_rule_state")]
    pub firewall_rule_state: String,
    #[serde(default)]
    pub daemon_idle_monitor_enabled: bool,
    #[serde(default)]
    pub daemon_idle_timeout_secs: Option<u64>,
    #[serde(default)]
    pub daemon_idle_deadline_unix_ms: Option<u64>,
    #[serde(default)]
    pub daemon_idle_blockers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserStatus {
    pub active: bool,
    pub restart_count: u64,
    pub last_disconnect_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BleRuntimeStatus {
    pub provider_name: String,
    pub scanner_state: String,
    pub advertiser_state: String,
    #[serde(default)]
    pub permission_state: Option<String>,
    #[serde(default)]
    pub bridge_mode: Option<String>,
    #[serde(default)]
    pub bridge_file_path: Option<String>,
    #[serde(default)]
    pub advertisement_file_path: Option<String>,
    #[serde(default)]
    pub last_started_at_unix_ms: Option<u64>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_capsule_ingested_at_unix_ms: Option<u64>,
    #[serde(default)]
    pub last_observation_prune_at_unix_ms: Option<u64>,
    #[serde(default)]
    pub last_bridge_snapshot_at_unix_ms: Option<u64>,
    #[serde(default)]
    pub last_advertisement_request_at_unix_ms: Option<u64>,
    #[serde(default)]
    pub advertised_rolling_identifier: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeEventEnvelope {
    pub seq: u64,
    pub event: String,
    pub payload: serde_json::Value,
    pub emitted_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeEventFeedSnapshot {
    pub events: Vec<RuntimeEventEnvelope>,
    pub generation: String,
    pub oldest_available_seq: Option<u64>,
    pub latest_available_seq: u64,
    pub resync_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resync_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeEventCheckpoint {
    pub consumer_id: String,
    pub generation: String,
    pub seq: u64,
    pub updated_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_read_at_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_expires_at_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_transition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_oldest_available_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_latest_available_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_compaction_watermark_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_compaction_watermark_segment_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct RuntimeEventReplayMetrics {
    pub total_feed_requests: u64,
    pub memory_feed_requests: u64,
    pub persisted_catch_up_requests: u64,
    pub persisted_catch_up_events_served: u64,
    pub resync_required_requests: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_replay_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_replay_source_at_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_resync_reason: Option<String>,
    pub checkpoint_loads: u64,
    pub checkpoint_saves: u64,
    pub checkpoint_creates: u64,
    pub checkpoint_advances: u64,
    pub checkpoint_heartbeats: u64,
    pub checkpoint_rewinds: u64,
    pub checkpoint_generation_resets: u64,
    pub checkpoint_metadata_refreshes: u64,
    pub checkpoint_resync_transitions: u64,
    pub expired_checkpoint_misses: u64,
    pub pruned_checkpoint_count: u64,
    pub last_persisted_catch_up_at_unix_ms: Option<u64>,
    pub last_resync_required_at_unix_ms: Option<u64>,
    pub last_checkpoint_heartbeat_at_unix_ms: Option<u64>,
    pub last_checkpoint_transition_at_unix_ms: Option<u64>,
    pub last_checkpoint_metadata_refresh_at_unix_ms: Option<u64>,
    pub last_checkpoint_prune_at_unix_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct LocalIpcAccessTokenRecord {
    pub expires_at_unix_ms: u64,
    pub refresh_after_unix_ms: u64,
}

pub const RUNTIME_EVENT_FEED_CAPACITY: usize = 1024;
pub const RUNTIME_EVENT_PERSISTED_JOURNAL_CAPACITY: usize = 10_000;
pub const RUNTIME_EVENT_PERSISTED_JOURNAL_MAX_CAPACITY: usize = 100_000;
pub const RUNTIME_EVENT_PERSISTED_SEGMENT_SIZE: usize = 1_000;
pub const RUNTIME_EVENT_CHECKPOINT_HEARTBEAT_INTERVAL_MS: u64 = 30 * 1000;
pub const RUNTIME_EVENT_CHECKPOINT_ACTIVE_THRESHOLD_MS: u64 = 5 * 60 * 1000;
pub const RUNTIME_EVENT_CHECKPOINT_STALE_THRESHOLD_MS: u64 = 60 * 60 * 1000;
pub const RUNTIME_EVENT_CHECKPOINT_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1000;

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            device_name: "DashDrop Device".into(),
            auto_accept_trusted_only: false,
            download_dir: None,
            file_conflict_strategy: FileConflictStrategy::Rename,
            max_parallel_streams: default_max_parallel_streams(),
            launch_at_login: default_launch_at_login(),
        }
    }
}

fn default_listener_port_mode() -> String {
    "fallback_random".to_string()
}

fn default_firewall_rule_state() -> String {
    "unknown".to_string()
}

fn default_control_plane_mode() -> String {
    "in_process".to_string()
}

fn default_requested_control_plane_mode() -> String {
    "in_process".to_string()
}

fn default_runtime_profile() -> String {
    "unknown".to_string()
}

fn default_daemon_status() -> String {
    "inactive".to_string()
}

fn default_daemon_connect_strategy() -> String {
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

    /// Short-lived BLE assist capsules keyed by rolling identifier.
    pub ble_assist_observations: Arc<RwLock<HashMap<String, BleAssistObservation>>>,

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
    /// BLE baseline provider/scanner runtime state.
    pub ble_runtime_status: Arc<RwLock<BleRuntimeStatus>>,
    /// Pending deferred remove keys to dedupe ServiceRemoved storms.
    pub pending_removed_sessions: Arc<tokio::sync::Mutex<HashSet<String>>>,
    /// Listener mode ("dual_stack" or "ipv4_only_fallback").
    pub listener_mode: Arc<RwLock<String>>,
    /// Effective control-plane mode ("daemon" or "in_process").
    pub control_plane_mode: Arc<RwLock<String>>,
    /// Requested control-plane mode after startup policy resolution.
    pub requested_control_plane_mode: Arc<RwLock<String>>,
    /// Runtime profile ("dev", "packaged", or "headless").
    pub runtime_profile: Arc<RwLock<String>>,
    /// Daemon startup/attach status for diagnostics.
    pub daemon_status: Arc<RwLock<String>>,
    /// How many attach probes were needed during startup.
    pub daemon_connect_attempts: Arc<RwLock<u32>>,
    /// Strategy used while attaching to the daemon control plane.
    pub daemon_connect_strategy: Arc<RwLock<String>>,
    /// Resolved daemon binary path when available.
    pub daemon_binary_path: Arc<RwLock<Option<String>>>,
    /// Optional headless idle shutdown policy state for diagnostics.
    pub daemon_idle_monitor_enabled: Arc<RwLock<bool>>,
    pub daemon_idle_timeout_secs: Arc<RwLock<Option<u64>>>,
    pub daemon_idle_deadline_unix_ms: Arc<RwLock<Option<u64>>>,
    pub daemon_idle_blockers: Arc<RwLock<Vec<String>>>,
    /// Listener port mode ("fixed" or "fallback_random").
    pub listener_port_mode: Arc<RwLock<String>>,
    /// Firewall rule state for diagnostics.
    pub firewall_rule_state: Arc<RwLock<String>>,
    /// Listener bind addrs exposed in diagnostics.
    pub listener_addrs: Arc<RwLock<Vec<String>>>,

    /// SQLite Database for persistent transfer history.
    pub db: Arc<std::sync::Mutex<rusqlite::Connection>>,

    /// Coalesced progress persistence coordinator.
    pub progress_persistence: TransferProgressPersistence,

    /// Runtime transfer metrics.
    pub metrics: Arc<RwLock<TransferMetrics>>,

    /// Local-only SLO observability timestamps for diagnostics and tests.
    pub slo_observability: Arc<RwLock<SloObservabilitySnapshot>>,

    /// Recent runtime events for daemon-mode UI polling/replay.
    pub runtime_event_feed: Arc<std::sync::Mutex<VecDeque<RuntimeEventEnvelope>>>,
    pub runtime_event_seq: Arc<AtomicU64>,
    pub runtime_event_persisted_oldest_seq: Arc<AtomicU64>,
    pub runtime_event_generation: String,
    pub runtime_event_replay_metrics: Arc<std::sync::Mutex<RuntimeEventReplayMetrics>>,
    pub local_ipc_access_tokens: Arc<Mutex<HashMap<String, LocalIpcAccessTokenRecord>>>,
}

impl AppState {
    pub fn new(identity: Identity, config: AppConfig, db: rusqlite::Connection) -> Self {
        let db = Arc::new(std::sync::Mutex::new(db));
        let (
            runtime_event_generation,
            runtime_event_persisted_oldest_seq,
            runtime_event_last_seq,
            runtime_event_feed,
        ) = {
            let guard = db.lock().expect("runtime event db lock poisoned");
            match crate::db::load_runtime_event_state(&guard, RUNTIME_EVENT_FEED_CAPACITY) {
                Ok((generation, oldest_seq, last_seq, events)) => (
                    generation,
                    oldest_seq.unwrap_or(0),
                    last_seq,
                    VecDeque::from(events),
                ),
                Err(error) => {
                    tracing::warn!("failed to load persisted runtime event journal: {error:#}");
                    (uuid::Uuid::new_v4().to_string(), 0, 0, VecDeque::new())
                }
            }
        };
        AppState {
            identity,
            devices: Arc::new(RwLock::new(HashMap::new())),
            session_index: Arc::new(RwLock::new(HashMap::new())),
            transfers: Arc::new(RwLock::new(HashMap::new())),
            trusted_peers: Arc::new(RwLock::new(HashMap::new())),
            ble_assist_observations: Arc::new(RwLock::new(HashMap::new())),
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
            ble_runtime_status: Arc::new(RwLock::new(BleRuntimeStatus {
                provider_name: "uninitialized".to_string(),
                scanner_state: "idle".to_string(),
                advertiser_state: "idle".to_string(),
                ..BleRuntimeStatus::default()
            })),
            pending_removed_sessions: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            listener_mode: Arc::new(RwLock::new("unknown".to_string())),
            control_plane_mode: Arc::new(RwLock::new(default_control_plane_mode())),
            requested_control_plane_mode: Arc::new(RwLock::new(
                default_requested_control_plane_mode(),
            )),
            runtime_profile: Arc::new(RwLock::new(default_runtime_profile())),
            daemon_status: Arc::new(RwLock::new(default_daemon_status())),
            daemon_connect_attempts: Arc::new(RwLock::new(0)),
            daemon_connect_strategy: Arc::new(RwLock::new(default_daemon_connect_strategy())),
            daemon_binary_path: Arc::new(RwLock::new(None)),
            daemon_idle_monitor_enabled: Arc::new(RwLock::new(false)),
            daemon_idle_timeout_secs: Arc::new(RwLock::new(None)),
            daemon_idle_deadline_unix_ms: Arc::new(RwLock::new(None)),
            daemon_idle_blockers: Arc::new(RwLock::new(Vec::new())),
            listener_port_mode: Arc::new(RwLock::new(default_listener_port_mode())),
            firewall_rule_state: Arc::new(RwLock::new(default_firewall_rule_state())),
            listener_addrs: Arc::new(RwLock::new(Vec::new())),
            progress_persistence: TransferProgressPersistence::new(Arc::clone(&db)),
            db,
            metrics: Arc::new(RwLock::new(TransferMetrics::default())),
            slo_observability: Arc::new(RwLock::new(SloObservabilitySnapshot::default())),
            runtime_event_feed: Arc::new(std::sync::Mutex::new(runtime_event_feed)),
            runtime_event_seq: Arc::new(AtomicU64::new(runtime_event_last_seq)),
            runtime_event_persisted_oldest_seq: Arc::new(AtomicU64::new(
                runtime_event_persisted_oldest_seq,
            )),
            runtime_event_generation,
            runtime_event_replay_metrics: Arc::new(std::sync::Mutex::new(
                RuntimeEventReplayMetrics::default(),
            )),
            local_ipc_access_tokens: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn schedule_progress_persist(&self, task: &TransferTask) {
        if let Err(error) = self.progress_persistence.schedule(task).await {
            tracing::warn!(
                transfer_id = %task.id,
                revision = task.revision,
                bytes_transferred = task.bytes_transferred,
                status = ?task.status,
                reason = %error,
                "failed to enqueue coalesced progress persistence"
            );
        }
    }

    pub async fn flush_progress_persist_now(&self, task: &TransferTask) {
        if let Err(error) = self.progress_persistence.flush_now(task).await {
            tracing::warn!(
                transfer_id = %task.id,
                revision = task.revision,
                bytes_transferred = task.bytes_transferred,
                status = ?task.status,
                reason = %error,
                "failed to force progress persistence flush"
            );
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

    pub(crate) fn now_unix_millis() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    fn update_runtime_event_replay_metrics(
        &self,
        update: impl FnOnce(&mut RuntimeEventReplayMetrics),
    ) {
        if let Ok(mut guard) = self.runtime_event_replay_metrics.lock() {
            update(&mut guard);
        }
    }

    fn runtime_event_memory_oldest_seq(&self) -> Option<u64> {
        self.runtime_event_feed
            .lock()
            .ok()
            .and_then(|feed| feed.front().map(|event| event.seq))
    }

    fn runtime_event_persisted_oldest_seq(&self) -> Option<u64> {
        match self
            .runtime_event_persisted_oldest_seq
            .load(Ordering::SeqCst)
        {
            0 => None,
            seq => Some(seq),
        }
    }

    fn runtime_event_checkpoint_transition(
        previous: Option<&RuntimeEventCheckpoint>,
        generation: &str,
        seq: u64,
    ) -> &'static str {
        match previous {
            None => "created",
            Some(previous) if previous.generation != generation => "generation_reset",
            Some(previous) if previous.seq == seq => "heartbeat",
            Some(previous) if previous.seq < seq => "advanced",
            Some(_) => "rewound",
        }
    }

    fn update_runtime_event_checkpoint_transition_metrics(
        &self,
        transition: &str,
        updated_at_unix_ms: u64,
        previous_recovery_hint: Option<&str>,
        next_recovery_hint: Option<&str>,
    ) {
        self.update_runtime_event_replay_metrics(|metrics| {
            match transition {
                "created" => {
                    metrics.checkpoint_creates = metrics.checkpoint_creates.saturating_add(1);
                }
                "advanced" => {
                    metrics.checkpoint_advances = metrics.checkpoint_advances.saturating_add(1);
                }
                "rewound" => {
                    metrics.checkpoint_rewinds = metrics.checkpoint_rewinds.saturating_add(1);
                }
                "generation_reset" => {
                    metrics.checkpoint_generation_resets =
                        metrics.checkpoint_generation_resets.saturating_add(1);
                }
                _ => {}
            }
            metrics.last_checkpoint_transition_at_unix_ms = Some(updated_at_unix_ms);
            if previous_recovery_hint != Some("resync_required")
                && next_recovery_hint == Some("resync_required")
            {
                metrics.checkpoint_resync_transitions =
                    metrics.checkpoint_resync_transitions.saturating_add(1);
            }
        });
    }

    fn runtime_event_checkpoint_recovery_hint(
        &self,
        generation: &str,
        seq: u64,
        latest_seq: u64,
        memory_oldest_seq: Option<u64>,
        persisted_oldest_seq: Option<u64>,
    ) -> &'static str {
        if generation != self.runtime_event_generation {
            "resync_required"
        } else if latest_seq == 0 || seq >= latest_seq {
            "up_to_date"
        } else if memory_oldest_seq
            .map(|oldest_seq| seq >= oldest_seq.saturating_sub(1))
            .unwrap_or(true)
        {
            "hot_window"
        } else if persisted_oldest_seq
            .map(|oldest_seq| seq >= oldest_seq.saturating_sub(1))
            .unwrap_or(true)
        {
            "persisted_catch_up"
        } else {
            "resync_required"
        }
    }

    fn enrich_runtime_event_checkpoint(
        &self,
        checkpoint: &mut RuntimeEventCheckpoint,
        journal_stats: Option<&crate::db::RuntimeEventJournalStats>,
        read_at_unix_ms: Option<u64>,
    ) {
        checkpoint.created_at_unix_ms = checkpoint
            .created_at_unix_ms
            .or(Some(checkpoint.updated_at_unix_ms));
        checkpoint.last_read_at_unix_ms = read_at_unix_ms.or(checkpoint.last_read_at_unix_ms);
        checkpoint.lease_expires_at_unix_ms = Some(
            checkpoint
                .updated_at_unix_ms
                .saturating_add(RUNTIME_EVENT_CHECKPOINT_ACTIVE_THRESHOLD_MS),
        );
        checkpoint.revision = Some(checkpoint.revision.unwrap_or(1).max(1));
        checkpoint.last_transition = checkpoint
            .last_transition
            .clone()
            .or_else(|| Some("created".to_string()));

        let latest_seq = self.runtime_event_seq.load(Ordering::SeqCst);
        let memory_oldest_seq = self.runtime_event_memory_oldest_seq();
        let persisted_oldest_seq = self.runtime_event_persisted_oldest_seq();
        checkpoint.recovery_hint = Some(
            self.runtime_event_checkpoint_recovery_hint(
                &checkpoint.generation,
                checkpoint.seq,
                latest_seq,
                memory_oldest_seq,
                persisted_oldest_seq,
            )
            .to_string(),
        );
        checkpoint.current_oldest_available_seq = persisted_oldest_seq.or(memory_oldest_seq);
        checkpoint.current_latest_available_seq = Some(latest_seq);
        checkpoint.current_compaction_watermark_seq =
            journal_stats.map(|stats| stats.compaction_watermark_seq);
        checkpoint.current_compaction_watermark_segment_id =
            journal_stats.map(|stats| stats.compaction_watermark_segment_id);
    }

    fn refresh_runtime_event_checkpoints_locked(
        &self,
        guard: &rusqlite::Connection,
        read_at_unix_ms: Option<u64>,
    ) -> Result<Vec<RuntimeEventCheckpoint>, String> {
        let journal_stats = crate::db::load_runtime_event_journal_stats(guard).ok();
        let checkpoints = crate::db::list_runtime_event_checkpoints(guard)
            .map_err(|error| format!("failed to list runtime event checkpoints: {error:#}"))?;
        let mut refreshed = Vec::with_capacity(checkpoints.len());
        let mut refreshed_count = 0u64;
        let mut resync_transition_count = 0u64;
        for mut checkpoint in checkpoints {
            let original = checkpoint.clone();
            self.enrich_runtime_event_checkpoint(
                &mut checkpoint,
                journal_stats.as_ref(),
                read_at_unix_ms,
            );
            if checkpoint != original {
                refreshed_count = refreshed_count.saturating_add(1);
                if original.recovery_hint.as_deref() != Some("resync_required")
                    && checkpoint.recovery_hint.as_deref() == Some("resync_required")
                {
                    resync_transition_count = resync_transition_count.saturating_add(1);
                }
                crate::db::save_runtime_event_checkpoint(guard, &checkpoint).map_err(|error| {
                    format!("failed to refresh runtime event checkpoint metadata: {error:#}")
                })?;
            }
            refreshed.push(checkpoint);
        }
        if refreshed_count > 0 {
            self.update_runtime_event_replay_metrics(|metrics| {
                metrics.checkpoint_metadata_refreshes = metrics
                    .checkpoint_metadata_refreshes
                    .saturating_add(refreshed_count);
                metrics.last_checkpoint_metadata_refresh_at_unix_ms = Some(Self::now_unix_millis());
                metrics.checkpoint_resync_transitions = metrics
                    .checkpoint_resync_transitions
                    .saturating_add(resync_transition_count);
            });
        }
        Ok(refreshed)
    }

    fn prune_runtime_event_checkpoints_locked(
        &self,
        guard: &rusqlite::Connection,
        now_unix_ms: u64,
    ) -> Result<usize, String> {
        let stale_before_unix_ms = now_unix_ms.saturating_sub(RUNTIME_EVENT_CHECKPOINT_TTL_MS);
        let removed = crate::db::prune_runtime_event_checkpoints(guard, stale_before_unix_ms)
            .map_err(|error| format!("failed to prune runtime event checkpoints: {error:#}"))?;
        if removed > 0 {
            self.update_runtime_event_replay_metrics(|metrics| {
                metrics.pruned_checkpoint_count = metrics
                    .pruned_checkpoint_count
                    .saturating_add(removed as u64);
                metrics.last_checkpoint_prune_at_unix_ms = Some(now_unix_ms);
            });
        }
        Ok(removed)
    }

    pub fn runtime_event_replay_metrics_snapshot(&self) -> RuntimeEventReplayMetrics {
        self.runtime_event_replay_metrics
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    fn build_local_ipc_access_token() -> String {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        URL_SAFE_NO_PAD.encode(bytes)
    }

    fn record_first_timestamp(slot: &mut Option<u64>, value: u64) {
        if slot.is_none() {
            *slot = Some(value);
        }
    }

    async fn set_transfer_slo_peer_if_missing(
        &self,
        transfer_id: &str,
        peer_fingerprint: Option<&str>,
    ) {
        let Some(peer_fingerprint) = peer_fingerprint else {
            return;
        };
        let mut guard = self.slo_observability.write().await;
        let entry = guard.transfers.entry(transfer_id.to_string()).or_default();
        if entry.peer_fingerprint.is_none() {
            entry.peer_fingerprint = Some(peer_fingerprint.to_string());
        }
    }

    pub async fn record_device_visibility(&self, peer_fingerprint: &str) {
        let observed_at = Self::now_unix_millis();
        let mut guard = self.slo_observability.write().await;
        let entry = guard
            .devices
            .entry(peer_fingerprint.to_string())
            .or_default();
        Self::record_first_timestamp(&mut entry.remote_peer_online_at, observed_at);
        Self::record_first_timestamp(&mut entry.local_device_visible_at, observed_at);
    }

    pub async fn record_sender_dispatch(&self, transfer_id: &str, peer_fingerprint: &str) {
        let observed_at = Self::now_unix_millis();
        let mut guard = self.slo_observability.write().await;
        let entry = guard.transfers.entry(transfer_id.to_string()).or_default();
        if entry.peer_fingerprint.is_none() {
            entry.peer_fingerprint = Some(peer_fingerprint.to_string());
        }
        Self::record_first_timestamp(&mut entry.sender_dispatch_at, observed_at);
    }

    pub async fn record_receiver_prompted(
        &self,
        transfer_id: &str,
        peer_fingerprint: Option<&str>,
    ) {
        let observed_at = Self::now_unix_millis();
        let mut guard = self.slo_observability.write().await;
        let entry = guard.transfers.entry(transfer_id.to_string()).or_default();
        if entry.peer_fingerprint.is_none() {
            entry.peer_fingerprint = peer_fingerprint.map(str::to_string);
        }
        Self::record_first_timestamp(&mut entry.receiver_prompted_at, observed_at);
    }

    pub async fn record_receiver_fallback_prompted(
        &self,
        transfer_id: &str,
        peer_fingerprint: &str,
    ) {
        let observed_at = Self::now_unix_millis();
        let mut guard = self.slo_observability.write().await;
        let entry = guard.transfers.entry(transfer_id.to_string()).or_default();
        if entry.peer_fingerprint.is_none() {
            entry.peer_fingerprint = Some(peer_fingerprint.to_string());
        }
        Self::record_first_timestamp(&mut entry.receiver_fallback_prompted_at, observed_at);
    }

    pub async fn slo_observability_snapshot(&self) -> SloObservabilitySnapshot {
        self.slo_observability.read().await.clone()
    }

    pub async fn ensure_incoming_request_notification(&self, transfer_id: &str) -> String {
        let notification_id = {
            let mut guard = self.incoming_request_notifications.write().await;
            if let Some(existing) = guard.get_mut(transfer_id) {
                existing.active = true;
                existing.updated_at_unix = Self::now_unix();
                existing.notification_id.clone()
            } else {
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
        };

        let peer_fingerprint = {
            let transfers = self.transfers.read().await;
            transfers
                .get(transfer_id)
                .map(|task| task.peer_fingerprint.clone())
        };
        self.set_transfer_slo_peer_if_missing(transfer_id, peer_fingerprint.as_deref())
            .await;
        self.record_receiver_prompted(transfer_id, peer_fingerprint.as_deref())
            .await;
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

    pub async fn ble_runtime_status_snapshot(&self) -> BleRuntimeStatus {
        self.ble_runtime_status.read().await.clone()
    }

    pub async fn is_trusted(&self, fp: &str) -> bool {
        self.trusted_peers.read().await.contains_key(fp)
    }

    pub async fn is_active_trust(&self, fp: &str) -> bool {
        self.trusted_peers
            .read()
            .await
            .get(fp)
            .map(|peer| peer.trust_level.allows_sensitive_send())
            .unwrap_or(false)
    }

    pub async fn add_trust(&self, fp: String, name: String) {
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut trusted = self.trusted_peers.write().await;
        if let Some(existing) = trusted.get_mut(&fp) {
            existing.name = name;
            if existing.paired_at == 0 {
                existing.paired_at = now_unix;
            }
            if existing.local_confirmation_at.is_none() {
                existing.local_confirmation_at = Some(now_unix);
            }
            return;
        }
        trusted.insert(
            fp.clone(),
            TrustedPeer {
                fingerprint: fp,
                name,
                paired_at: now_unix,
                trust_level: TrustLevel::LegacyPaired,
                last_verification_method: TrustVerificationMethod::ManualPairing,
                alias: None,
                last_used_at: None,
                remote_confirmation_material_seen_at: None,
                local_confirmation_at: Some(now_unix),
                mutual_confirmed_at: None,
                frozen_at: None,
                freeze_reason: None,
            },
        );
    }

    pub async fn trust_level_for(&self, fp: &str) -> Option<TrustLevel> {
        self.trusted_peers
            .read()
            .await
            .get(fp)
            .map(|peer| peer.trust_level.clone())
    }

    pub async fn freeze_trusted_peer(
        &self,
        fp: &str,
        reason: &str,
        frozen_at: u64,
    ) -> Option<TrustedPeer> {
        let mut trusted = self.trusted_peers.write().await;
        let peer = trusted.get_mut(fp)?;
        peer.trust_level = TrustLevel::Frozen;
        peer.frozen_at = Some(frozen_at);
        peer.freeze_reason = Some(reason.to_string());
        Some(peer.clone())
    }

    pub async fn record_ble_assist_observation(&self, capsule: &crate::ble::BleAssistCapsule) {
        let now_unix_ms = Self::now_unix_millis();
        let mut observations = self.ble_assist_observations.write().await;
        observations.retain(|_, observation| observation.expires_at_unix_ms > now_unix_ms);
        let entry = observations
            .entry(capsule.rolling_identifier.clone())
            .or_insert(BleAssistObservation {
                rolling_identifier: capsule.rolling_identifier.clone(),
                integrity_tag: capsule.integrity_tag.clone(),
                first_seen_at_unix_ms: now_unix_ms,
                last_seen_at_unix_ms: now_unix_ms,
                expires_at_unix_ms: capsule.expires_at_unix_ms,
                transport_hint: capsule.transport_hint.clone(),
                qr_fallback_available: capsule.qr_fallback_available,
                short_code_fallback_available: capsule.short_code_fallback_available,
            });
        entry.integrity_tag = capsule.integrity_tag.clone();
        entry.last_seen_at_unix_ms = now_unix_ms;
        entry.expires_at_unix_ms = capsule.expires_at_unix_ms;
        entry.transport_hint = capsule.transport_hint.clone();
        entry.qr_fallback_available = capsule.qr_fallback_available;
        entry.short_code_fallback_available = capsule.short_code_fallback_available;
        drop(observations);
        let mut runtime = self.ble_runtime_status.write().await;
        runtime.last_capsule_ingested_at_unix_ms = Some(now_unix_ms);
        runtime.scanner_state = "observing_capsules".to_string();
    }

    pub async fn ble_assist_observations_snapshot(&self) -> Vec<BleAssistObservation> {
        let now_unix_ms = Self::now_unix_millis();
        let mut observations = self.ble_assist_observations.write().await;
        observations.retain(|_, observation| observation.expires_at_unix_ms > now_unix_ms);
        drop(observations);
        let mut runtime = self.ble_runtime_status.write().await;
        runtime.last_observation_prune_at_unix_ms = Some(now_unix_ms);
        drop(runtime);
        self.ble_assist_observations
            .read()
            .await
            .values()
            .cloned()
            .collect()
    }

    pub async fn mark_ble_runtime_started(
        &self,
        provider_name: &str,
        scanner_state: &str,
        advertiser_state: &str,
    ) {
        let mut runtime = self.ble_runtime_status.write().await;
        runtime.provider_name = provider_name.to_string();
        runtime.scanner_state = scanner_state.to_string();
        runtime.advertiser_state = advertiser_state.to_string();
        runtime.last_started_at_unix_ms = Some(Self::now_unix_millis());
        runtime.last_error = None;
    }

    pub async fn mark_ble_runtime_error(&self, provider_name: &str, message: String) {
        let mut runtime = self.ble_runtime_status.write().await;
        runtime.provider_name = provider_name.to_string();
        runtime.last_error = Some(message);
        runtime.scanner_state = "error".to_string();
    }

    pub async fn mark_ble_runtime_idle(
        &self,
        provider_name: &str,
        scanner_state: &str,
        advertiser_state: &str,
    ) {
        let mut runtime = self.ble_runtime_status.write().await;
        runtime.provider_name = provider_name.to_string();
        runtime.scanner_state = scanner_state.to_string();
        runtime.advertiser_state = advertiser_state.to_string();
        runtime.last_error = None;
    }

    pub async fn update_ble_runtime_bridge(
        &self,
        permission_state: Option<String>,
        bridge_mode: Option<String>,
        bridge_file_path: Option<String>,
        snapshot_at_unix_ms: Option<u64>,
        advertised_rolling_identifier: Option<String>,
    ) {
        let mut runtime = self.ble_runtime_status.write().await;
        runtime.permission_state = permission_state;
        runtime.bridge_mode = bridge_mode;
        runtime.bridge_file_path = bridge_file_path;
        runtime.last_bridge_snapshot_at_unix_ms = snapshot_at_unix_ms;
        runtime.advertised_rolling_identifier = advertised_rolling_identifier;
    }

    pub async fn update_ble_runtime_advertisement(
        &self,
        advertisement_file_path: Option<String>,
        request_at_unix_ms: Option<u64>,
        rolling_identifier: Option<String>,
    ) {
        let mut runtime = self.ble_runtime_status.write().await;
        runtime.advertisement_file_path = advertisement_file_path;
        runtime.last_advertisement_request_at_unix_ms = request_at_unix_ms;
        runtime.advertised_rolling_identifier = rolling_identifier;
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
            requested_control_plane_mode: self.requested_control_plane_mode.read().await.clone(),
            control_plane_mode: self.control_plane_mode.read().await.clone(),
            runtime_profile: self.runtime_profile.read().await.clone(),
            daemon_status: self.daemon_status.read().await.clone(),
            daemon_connect_attempts: *self.daemon_connect_attempts.read().await,
            daemon_connect_strategy: self.daemon_connect_strategy.read().await.clone(),
            daemon_binary_path: self.daemon_binary_path.read().await.clone(),
            listener_port_mode: self.listener_port_mode.read().await.clone(),
            firewall_rule_state: self.firewall_rule_state.read().await.clone(),
            daemon_idle_monitor_enabled: *self.daemon_idle_monitor_enabled.read().await,
            daemon_idle_timeout_secs: *self.daemon_idle_timeout_secs.read().await,
            daemon_idle_deadline_unix_ms: *self.daemon_idle_deadline_unix_ms.read().await,
            daemon_idle_blockers: self.daemon_idle_blockers.read().await.clone(),
        }
    }

    pub async fn set_daemon_idle_monitor_state(
        &self,
        enabled: bool,
        timeout_secs: Option<u64>,
        deadline_unix_ms: Option<u64>,
        blockers: Vec<String>,
    ) {
        *self.daemon_idle_monitor_enabled.write().await = enabled;
        *self.daemon_idle_timeout_secs.write().await = timeout_secs;
        *self.daemon_idle_deadline_unix_ms.write().await = deadline_unix_ms;
        *self.daemon_idle_blockers.write().await = blockers;
    }

    pub async fn active_local_ipc_access_grant_count(&self) -> usize {
        let now = Self::now_unix_millis();
        let mut tokens = self.local_ipc_access_tokens.lock().await;
        tokens.retain(|_, record| record.expires_at_unix_ms > now);
        tokens.len()
    }

    pub async fn headless_idle_blockers(&self) -> Vec<String> {
        let active_access_grants = self.active_local_ipc_access_grant_count().await;
        let (draft_count, pending_accept_count, transferring_count) = {
            let transfers = self.transfers.read().await;
            let mut draft_count = 0usize;
            let mut pending_accept_count = 0usize;
            let mut transferring_count = 0usize;
            for transfer in transfers.values() {
                match transfer.status {
                    TransferStatus::Draft => draft_count = draft_count.saturating_add(1),
                    TransferStatus::PendingAccept => {
                        pending_accept_count = pending_accept_count.saturating_add(1)
                    }
                    TransferStatus::Transferring => {
                        transferring_count = transferring_count.saturating_add(1)
                    }
                    _ => {}
                }
            }
            (draft_count, pending_accept_count, transferring_count)
        };

        let mut blockers = Vec::new();
        if active_access_grants > 0 {
            blockers.push(format!("local_ipc_access_grants:{active_access_grants}"));
        }
        if draft_count > 0 {
            blockers.push(format!("draft_transfers:{draft_count}"));
        }
        if pending_accept_count > 0 {
            blockers.push(format!("pending_accept_transfers:{pending_accept_count}"));
        }
        if transferring_count > 0 {
            blockers.push(format!("active_transfers:{transferring_count}"));
        }
        blockers
    }

    pub async fn transfer_metrics(&self) -> TransferMetrics {
        self.metrics.read().await.clone()
    }

    pub fn record_runtime_event(
        &self,
        event: &str,
        payload: serde_json::Value,
    ) -> RuntimeEventEnvelope {
        let envelope = RuntimeEventEnvelope {
            seq: self.runtime_event_seq.fetch_add(1, Ordering::SeqCst) + 1,
            event: event.to_string(),
            payload,
            emitted_at_unix_ms: Self::now_unix_millis(),
        };
        let mut feed = self
            .runtime_event_feed
            .lock()
            .expect("runtime event feed lock poisoned");
        if feed.len() >= RUNTIME_EVENT_FEED_CAPACITY {
            feed.pop_front();
        }
        feed.push_back(envelope.clone());
        drop(feed);

        if let Ok(guard) = self.db.lock() {
            let now_unix_ms = Self::now_unix_millis();
            let _ = self.prune_runtime_event_checkpoints_locked(&guard, now_unix_ms);
            let previous_persisted_oldest_seq = self
                .runtime_event_persisted_oldest_seq
                .load(Ordering::SeqCst);
            if let Err(error) = crate::db::append_runtime_event(
                &guard,
                &self.runtime_event_generation,
                &envelope,
                RUNTIME_EVENT_PERSISTED_JOURNAL_CAPACITY,
                RUNTIME_EVENT_PERSISTED_JOURNAL_MAX_CAPACITY,
                now_unix_ms.saturating_sub(RUNTIME_EVENT_CHECKPOINT_STALE_THRESHOLD_MS),
            )
            .map(|oldest_retained_seq| {
                let next_oldest_retained_seq = oldest_retained_seq.unwrap_or(0);
                self.runtime_event_persisted_oldest_seq
                    .store(next_oldest_retained_seq, Ordering::SeqCst);
                if next_oldest_retained_seq != previous_persisted_oldest_seq {
                    let _ = self.refresh_runtime_event_checkpoints_locked(&guard, None);
                }
            }) {
                tracing::warn!(
                    seq = envelope.seq,
                    event = %envelope.event,
                    "failed to persist runtime event journal entry: {error:#}"
                );
            }
        } else {
            tracing::warn!(
                seq = envelope.seq,
                event = %envelope.event,
                "failed to persist runtime event journal entry: db lock poisoned"
            );
        }
        envelope
    }

    pub fn runtime_events_since(&self, after_seq: u64, limit: usize) -> RuntimeEventFeedSnapshot {
        let request_started_at_unix_ms = Self::now_unix_millis();
        self.update_runtime_event_replay_metrics(|metrics| {
            metrics.total_feed_requests = metrics.total_feed_requests.saturating_add(1);
        });
        let feed = self
            .runtime_event_feed
            .lock()
            .expect("runtime event feed lock poisoned");
        let memory_oldest_available_seq = feed.front().map(|event| event.seq);
        let persisted_oldest_available_seq = match self
            .runtime_event_persisted_oldest_seq
            .load(Ordering::SeqCst)
        {
            0 => None,
            seq => Some(seq),
        };
        let latest_available_seq = self.runtime_event_seq.load(Ordering::SeqCst);
        let oldest_available_seq = if after_seq == 0 {
            memory_oldest_available_seq
        } else {
            persisted_oldest_available_seq.or(memory_oldest_available_seq)
        };
        let mut resync_reason: Option<&'static str> = None;
        let mut resync_required = if after_seq == 0 {
            false
        } else if latest_available_seq == 0 || after_seq > latest_available_seq {
            resync_reason = Some("cursor_after_latest_available");
            true
        } else if let Some(oldest_seq) = oldest_available_seq {
            let requires_resync = after_seq < oldest_seq.saturating_sub(1);
            if requires_resync {
                resync_reason = Some("cursor_before_oldest_available");
            }
            requires_resync
        } else {
            false
        };

        let needs_persisted_catch_up = after_seq > 0
            && !resync_required
            && memory_oldest_available_seq
                .map(|oldest_seq| after_seq < oldest_seq.saturating_sub(1))
                .unwrap_or(false);

        let mut events = if limit == 0 || resync_required {
            Vec::new()
        } else if needs_persisted_catch_up {
            drop(feed);
            self.update_runtime_event_replay_metrics(|metrics| {
                metrics.persisted_catch_up_requests =
                    metrics.persisted_catch_up_requests.saturating_add(1);
            });
            match self.db.lock() {
                Ok(guard) => match crate::db::load_runtime_events_after(&guard, after_seq, limit) {
                    Ok(events) => {
                        self.update_runtime_event_replay_metrics(|metrics| {
                            metrics.persisted_catch_up_events_served = metrics
                                .persisted_catch_up_events_served
                                .saturating_add(events.len() as u64);
                            metrics.last_persisted_catch_up_at_unix_ms =
                                Some(request_started_at_unix_ms);
                        });
                        events
                    }
                    Err(error) => {
                        tracing::warn!(
                            after_seq,
                            limit,
                            "failed to load runtime events from persisted journal: {error:#}"
                        );
                        resync_reason = Some("persisted_journal_unavailable");
                        resync_required = true;
                        Vec::new()
                    }
                },
                Err(_) => {
                    tracing::warn!(
                        after_seq,
                        limit,
                        "failed to load runtime events from persisted journal: db lock poisoned"
                    );
                    resync_reason = Some("persisted_journal_unavailable");
                    resync_required = true;
                    Vec::new()
                }
            }
        } else {
            self.update_runtime_event_replay_metrics(|metrics| {
                metrics.memory_feed_requests = metrics.memory_feed_requests.saturating_add(1);
            });
            feed.iter()
                .filter(|event| event.seq > after_seq)
                .take(limit)
                .cloned()
                .collect()
        };

        if needs_persisted_catch_up
            && !resync_required
            && after_seq < latest_available_seq
            && events.is_empty()
        {
            tracing::warn!(
                after_seq,
                latest_available_seq,
                "persisted runtime event catch-up returned no events before latest seq; forcing resync"
            );
            resync_reason = Some("persisted_catch_up_empty");
            resync_required = true;
        }

        if resync_required {
            self.update_runtime_event_replay_metrics(|metrics| {
                metrics.resync_required_requests =
                    metrics.resync_required_requests.saturating_add(1);
                metrics.last_resync_required_at_unix_ms = Some(request_started_at_unix_ms);
            });
            events.clear();
        }

        let replay_source = if resync_required {
            Some("resync_required".to_string())
        } else if limit == 0 || (after_seq == 0 && latest_available_seq == 0) || events.is_empty() {
            Some("empty".to_string())
        } else if needs_persisted_catch_up {
            Some("persisted_catch_up".to_string())
        } else {
            Some("memory_hot_window".to_string())
        };

        self.update_runtime_event_replay_metrics(|metrics| {
            metrics.last_replay_source = replay_source.clone();
            metrics.last_replay_source_at_unix_ms = Some(request_started_at_unix_ms);
            if resync_required {
                metrics.last_resync_reason = resync_reason.map(str::to_string);
            }
        });

        RuntimeEventFeedSnapshot {
            events,
            generation: self.runtime_event_generation.clone(),
            oldest_available_seq,
            latest_available_seq,
            resync_required,
            replay_source,
            resync_reason: resync_reason.map(str::to_string),
        }
    }

    pub async fn issue_local_ipc_access_grant(
        &self,
        previous_token: Option<&str>,
    ) -> crate::local_ipc::LocalIpcAccessGrant {
        const TOKEN_TTL_MS: u64 = 5 * 60 * 1000;
        const TOKEN_REFRESH_AFTER_MS: u64 = 2 * 60 * 1000;

        let now = Self::now_unix_millis();
        let expires_at_unix_ms = now.saturating_add(TOKEN_TTL_MS);
        let refresh_after_unix_ms = now.saturating_add(TOKEN_REFRESH_AFTER_MS);
        let access_token = Self::build_local_ipc_access_token();

        let mut tokens = self.local_ipc_access_tokens.lock().await;
        tokens.retain(|_, record| record.expires_at_unix_ms > now);
        if let Some(previous_token) = previous_token.filter(|value| !value.trim().is_empty()) {
            tokens.remove(previous_token);
        }
        tokens.insert(
            access_token.clone(),
            LocalIpcAccessTokenRecord {
                expires_at_unix_ms,
                refresh_after_unix_ms,
            },
        );

        crate::local_ipc::LocalIpcAccessGrant {
            access_token,
            expires_at_unix_ms,
            refresh_after_unix_ms,
        }
    }

    pub async fn validate_local_ipc_access_token(
        &self,
        token: Option<&str>,
    ) -> Result<(), &'static str> {
        let Some(token) = token.filter(|value| !value.trim().is_empty()) else {
            return Err("missing access token");
        };

        let now = Self::now_unix_millis();
        let mut tokens = self.local_ipc_access_tokens.lock().await;
        let status = match tokens.get(token) {
            Some(record) if record.expires_at_unix_ms > now => Ok(()),
            Some(_) => Err("expired access token"),
            None => Err("unknown access token"),
        };
        tokens.retain(|_, record| record.expires_at_unix_ms > now);
        status
    }

    pub async fn revoke_local_ipc_access_token(
        &self,
        token: Option<&str>,
    ) -> Result<(), &'static str> {
        let Some(token) = token.filter(|value| !value.trim().is_empty()) else {
            return Err("missing access token");
        };

        let now = Self::now_unix_millis();
        let mut tokens = self.local_ipc_access_tokens.lock().await;
        let status = match tokens.get(token) {
            Some(record) if record.expires_at_unix_ms > now => Ok(()),
            Some(_) => Err("expired access token"),
            None => Err("unknown access token"),
        };
        tokens.retain(|_, record| record.expires_at_unix_ms > now);
        status?;
        tokens.remove(token);
        Ok(())
    }

    pub async fn runtime_event_checkpoint(
        &self,
        consumer_id: &str,
    ) -> Result<Option<RuntimeEventCheckpoint>, String> {
        let consumer_id = consumer_id.trim();
        if consumer_id.is_empty() {
            return Err("runtime event checkpoint consumer_id is required".to_string());
        }

        let guard = self
            .db
            .lock()
            .map_err(|_| "runtime event checkpoint db lock poisoned".to_string())?;
        let now_unix_ms = Self::now_unix_millis();
        self.update_runtime_event_replay_metrics(|metrics| {
            metrics.checkpoint_loads = metrics.checkpoint_loads.saturating_add(1);
        });
        let checkpoint = crate::db::load_runtime_event_checkpoint(&guard, consumer_id)
            .map_err(|error| format!("failed to load runtime event checkpoint: {error:#}"))?;
        let Some(mut checkpoint) = checkpoint else {
            return Ok(None);
        };
        if checkpoint
            .updated_at_unix_ms
            .saturating_add(RUNTIME_EVENT_CHECKPOINT_TTL_MS)
            <= now_unix_ms
        {
            crate::db::delete_runtime_event_checkpoint(&guard, consumer_id).map_err(|error| {
                format!("failed to delete expired runtime event checkpoint: {error:#}")
            })?;
            self.update_runtime_event_replay_metrics(|metrics| {
                metrics.expired_checkpoint_misses =
                    metrics.expired_checkpoint_misses.saturating_add(1);
                metrics.pruned_checkpoint_count = metrics.pruned_checkpoint_count.saturating_add(1);
                metrics.last_checkpoint_prune_at_unix_ms = Some(now_unix_ms);
            });
            return Ok(None);
        }
        let original = checkpoint.clone();
        let journal_stats = crate::db::load_runtime_event_journal_stats(&guard).ok();
        self.enrich_runtime_event_checkpoint(
            &mut checkpoint,
            journal_stats.as_ref(),
            Some(now_unix_ms),
        );
        if checkpoint != original {
            crate::db::save_runtime_event_checkpoint(&guard, &checkpoint).map_err(|error| {
                format!("failed to refresh runtime event checkpoint metadata: {error:#}")
            })?;
            self.update_runtime_event_replay_metrics(|metrics| {
                metrics.checkpoint_metadata_refreshes =
                    metrics.checkpoint_metadata_refreshes.saturating_add(1);
                metrics.last_checkpoint_metadata_refresh_at_unix_ms = Some(now_unix_ms);
                if original.recovery_hint.as_deref() != Some("resync_required")
                    && checkpoint.recovery_hint.as_deref() == Some("resync_required")
                {
                    metrics.checkpoint_resync_transitions =
                        metrics.checkpoint_resync_transitions.saturating_add(1);
                }
            });
        }
        Ok(Some(checkpoint))
    }

    pub async fn save_runtime_event_checkpoint(
        &self,
        consumer_id: &str,
        generation: &str,
        seq: u64,
    ) -> Result<RuntimeEventCheckpoint, String> {
        let consumer_id = consumer_id.trim();
        if consumer_id.is_empty() {
            return Err("runtime event checkpoint consumer_id is required".to_string());
        }

        let generation = generation.trim();
        if generation.is_empty() {
            return Err("runtime event checkpoint generation is required".to_string());
        }

        let now_unix_ms = Self::now_unix_millis();
        let latest_seq = self.runtime_event_seq.load(Ordering::SeqCst);
        let guard = self
            .db
            .lock()
            .map_err(|_| "runtime event checkpoint db lock poisoned".to_string())?;
        let previous_checkpoint = crate::db::load_runtime_event_checkpoint(&guard, consumer_id)
            .map_err(|error| {
                format!("failed to load existing runtime event checkpoint: {error:#}")
            })?;
        let journal_stats = crate::db::load_runtime_event_journal_stats(&guard).ok();
        let checkpoint_seq = seq.min(latest_seq);
        let mut checkpoint = RuntimeEventCheckpoint {
            consumer_id: consumer_id.to_string(),
            generation: generation.to_string(),
            seq: checkpoint_seq,
            updated_at_unix_ms: now_unix_ms,
            created_at_unix_ms: previous_checkpoint
                .as_ref()
                .and_then(|previous| previous.created_at_unix_ms)
                .or(Some(now_unix_ms)),
            last_read_at_unix_ms: previous_checkpoint
                .as_ref()
                .and_then(|previous| previous.last_read_at_unix_ms),
            lease_expires_at_unix_ms: Some(
                now_unix_ms.saturating_add(RUNTIME_EVENT_CHECKPOINT_ACTIVE_THRESHOLD_MS),
            ),
            revision: Some(
                previous_checkpoint
                    .as_ref()
                    .and_then(|previous| previous.revision)
                    .unwrap_or(0)
                    .saturating_add(1),
            ),
            last_transition: Some(
                Self::runtime_event_checkpoint_transition(
                    previous_checkpoint.as_ref(),
                    generation,
                    checkpoint_seq,
                )
                .to_string(),
            ),
            recovery_hint: None,
            current_oldest_available_seq: None,
            current_latest_available_seq: None,
            current_compaction_watermark_seq: None,
            current_compaction_watermark_segment_id: None,
        };
        self.enrich_runtime_event_checkpoint(&mut checkpoint, journal_stats.as_ref(), None);
        crate::db::save_runtime_event_checkpoint(&guard, &checkpoint)
            .map_err(|error| format!("failed to save runtime event checkpoint: {error:#}"))?;
        self.update_runtime_event_checkpoint_transition_metrics(
            checkpoint.last_transition.as_deref().unwrap_or("heartbeat"),
            checkpoint.updated_at_unix_ms,
            previous_checkpoint
                .as_ref()
                .and_then(|previous| previous.recovery_hint.as_deref()),
            checkpoint.recovery_hint.as_deref(),
        );
        self.update_runtime_event_replay_metrics(|metrics| {
            metrics.checkpoint_saves = metrics.checkpoint_saves.saturating_add(1);
            if previous_checkpoint
                .as_ref()
                .map(|previous| {
                    previous.generation == checkpoint.generation && previous.seq == checkpoint.seq
                })
                .unwrap_or(false)
            {
                metrics.checkpoint_heartbeats = metrics.checkpoint_heartbeats.saturating_add(1);
                metrics.last_checkpoint_heartbeat_at_unix_ms = Some(checkpoint.updated_at_unix_ms);
            }
        });
        self.prune_runtime_event_checkpoints_locked(&guard, checkpoint.updated_at_unix_ms)?;
        Ok(checkpoint)
    }

    pub async fn runtime_event_checkpoints(&self) -> Result<Vec<RuntimeEventCheckpoint>, String> {
        let now_unix_ms = Self::now_unix_millis();
        let guard = self
            .db
            .lock()
            .map_err(|_| "runtime event checkpoint db lock poisoned".to_string())?;
        self.prune_runtime_event_checkpoints_locked(&guard, now_unix_ms)?;
        self.refresh_runtime_event_checkpoints_locked(&guard, None)
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
        FileConflictStrategy, IncomingRequestNotification, Platform, ReachabilityStatus,
        RuntimeEventCheckpoint, RuntimeEventEnvelope, RuntimeStatus, SessionInfo,
        TransferDirection, TransferStatus, TransferTask,
        RUNTIME_EVENT_CHECKPOINT_ACTIVE_THRESHOLD_MS,
    };
    use crate::crypto::Identity;
    use serde_json::json;
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::str::FromStr;
    use std::sync::atomic::Ordering;
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

    #[tokio::test]
    async fn runtime_event_feed_tracks_sequence_and_limit() {
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

        let first =
            state.record_runtime_event("device_discovered", json!({ "fingerprint": "one" }));
        let second = state.record_runtime_event("device_updated", json!({ "fingerprint": "two" }));

        assert_eq!(first.seq, 1);
        assert_eq!(second.seq, 2);

        let all = state.runtime_events_since(0, 10);
        assert_eq!(all.events.len(), 2);
        assert_eq!(all.events[0].event, "device_discovered");
        assert_eq!(all.events[1].event, "device_updated");
        assert_eq!(all.oldest_available_seq, Some(1));
        assert_eq!(all.latest_available_seq, 2);
        assert!(!all.resync_required);
        assert_eq!(all.replay_source.as_deref(), Some("memory_hot_window"));
        assert_eq!(all.resync_reason, None);

        let only_second = state.runtime_events_since(1, 10);
        assert_eq!(only_second.events.len(), 1);
        assert_eq!(only_second.events[0].seq, 2);

        let limited = state.runtime_events_since(0, 1);
        assert_eq!(limited.events.len(), 1);
        assert_eq!(limited.events[0].seq, 1);

        let metrics = state.runtime_event_replay_metrics_snapshot();
        assert_eq!(metrics.total_feed_requests, 3);
        assert_eq!(metrics.memory_feed_requests, 3);
        assert_eq!(metrics.persisted_catch_up_requests, 0);
        assert_eq!(metrics.resync_required_requests, 0);
        assert_eq!(
            metrics.last_replay_source.as_deref(),
            Some("memory_hot_window")
        );
        assert!(metrics.last_replay_source_at_unix_ms.is_some());
        assert_eq!(metrics.last_resync_reason, None);
    }

    #[tokio::test]
    async fn runtime_event_feed_requires_resync_when_cursor_falls_outside_window() {
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

        let mut feed = state
            .runtime_event_feed
            .lock()
            .expect("runtime event feed lock poisoned");
        for seq in 9_077..=10_100 {
            feed.push_back(RuntimeEventEnvelope {
                seq,
                event: "device_updated".to_string(),
                payload: json!({ "index": seq }),
                emitted_at_unix_ms: seq,
            });
        }
        drop(feed);
        state.runtime_event_seq.store(10_100, Ordering::SeqCst);
        state
            .runtime_event_persisted_oldest_seq
            .store(101, Ordering::SeqCst);

        let snapshot = state.runtime_events_since(10, 25);
        assert!(snapshot.resync_required);
        assert!(snapshot.events.is_empty());
        assert_eq!(snapshot.oldest_available_seq, Some(101));
        assert_eq!(snapshot.latest_available_seq, 10_100);
        assert_eq!(snapshot.replay_source.as_deref(), Some("resync_required"));
        assert_eq!(
            snapshot.resync_reason.as_deref(),
            Some("cursor_before_oldest_available")
        );

        let metrics = state.runtime_event_replay_metrics_snapshot();
        assert_eq!(metrics.total_feed_requests, 1);
        assert_eq!(metrics.memory_feed_requests, 0);
        assert_eq!(metrics.persisted_catch_up_requests, 0);
        assert_eq!(metrics.resync_required_requests, 1);
        assert_eq!(
            metrics.last_replay_source.as_deref(),
            Some("resync_required")
        );
        assert_eq!(
            metrics.last_resync_reason.as_deref(),
            Some("cursor_before_oldest_available")
        );
        assert!(metrics.last_replay_source_at_unix_ms.is_some());
        assert!(metrics.last_resync_required_at_unix_ms.is_some());
    }

    #[tokio::test]
    async fn runtime_event_feed_uses_persisted_journal_for_catch_up_outside_memory_window() {
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

        for index in 0..1_500 {
            state.record_runtime_event("device_updated", json!({ "index": index }));
        }

        let snapshot = state.runtime_events_since(400, 25);
        assert!(!snapshot.resync_required);
        assert_eq!(snapshot.oldest_available_seq, Some(1));
        assert_eq!(snapshot.latest_available_seq, 1_500);
        assert_eq!(snapshot.events.len(), 25);
        assert_eq!(snapshot.events[0].seq, 401);
        assert_eq!(snapshot.events[24].seq, 425);
        assert_eq!(
            snapshot.replay_source.as_deref(),
            Some("persisted_catch_up")
        );
        assert_eq!(snapshot.resync_reason, None);

        let metrics = state.runtime_event_replay_metrics_snapshot();
        assert_eq!(metrics.total_feed_requests, 1);
        assert_eq!(metrics.memory_feed_requests, 0);
        assert_eq!(metrics.persisted_catch_up_requests, 1);
        assert_eq!(metrics.persisted_catch_up_events_served, 25);
        assert_eq!(metrics.resync_required_requests, 0);
        assert_eq!(
            metrics.last_replay_source.as_deref(),
            Some("persisted_catch_up")
        );
        assert!(metrics.last_replay_source_at_unix_ms.is_some());
        assert_eq!(metrics.last_resync_reason, None);
        assert!(metrics.last_persisted_catch_up_at_unix_ms.is_some());
    }

    #[tokio::test]
    async fn runtime_event_checkpoint_round_trips_through_app_state() {
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

        state.record_runtime_event("device_updated", json!({ "index": 1 }));
        let saved = state
            .save_runtime_event_checkpoint("shared-ui", "gen-1", 999)
            .await
            .expect("save checkpoint");
        assert_eq!(saved.consumer_id, "shared-ui");
        assert_eq!(saved.generation, "gen-1");
        assert_eq!(saved.seq, 1);

        let loaded = state
            .runtime_event_checkpoint("shared-ui")
            .await
            .expect("load checkpoint");
        let expected_last_read_at = loaded.as_ref().and_then(|item| item.last_read_at_unix_ms);
        assert_eq!(
            loaded,
            Some(RuntimeEventCheckpoint {
                consumer_id: "shared-ui".into(),
                generation: "gen-1".into(),
                seq: 1,
                updated_at_unix_ms: saved.updated_at_unix_ms,
                created_at_unix_ms: saved.created_at_unix_ms,
                last_read_at_unix_ms: expected_last_read_at,
                lease_expires_at_unix_ms: saved.lease_expires_at_unix_ms,
                revision: saved.revision,
                last_transition: saved.last_transition.clone(),
                recovery_hint: saved.recovery_hint.clone(),
                current_oldest_available_seq: saved.current_oldest_available_seq,
                current_latest_available_seq: saved.current_latest_available_seq,
                current_compaction_watermark_seq: saved.current_compaction_watermark_seq,
                current_compaction_watermark_segment_id: saved
                    .current_compaction_watermark_segment_id,
            })
        );
    }

    #[tokio::test]
    async fn runtime_event_checkpoint_load_refreshes_last_read_and_lease() {
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

        let event = state.record_runtime_event("device_updated", json!({ "index": 1 }));
        let generation = state.runtime_event_generation.clone();
        let saved = state
            .save_runtime_event_checkpoint("shared-ui", &generation, event.seq)
            .await
            .expect("save checkpoint");

        tokio::time::sleep(std::time::Duration::from_millis(2)).await;

        let loaded = state
            .runtime_event_checkpoint("shared-ui")
            .await
            .expect("load checkpoint")
            .expect("checkpoint should exist");

        assert_eq!(loaded.consumer_id, "shared-ui");
        assert_eq!(loaded.seq, event.seq);
        assert_eq!(loaded.created_at_unix_ms, saved.created_at_unix_ms);
        assert!(
            loaded.last_read_at_unix_ms.unwrap_or(0) >= saved.updated_at_unix_ms,
            "loading a checkpoint should stamp last_read_at"
        );
        assert_eq!(
            loaded.lease_expires_at_unix_ms,
            Some(
                loaded
                    .updated_at_unix_ms
                    .saturating_add(RUNTIME_EVENT_CHECKPOINT_ACTIVE_THRESHOLD_MS)
            ),
            "loading a checkpoint should preserve lease semantics relative to the last heartbeat"
        );

        let metrics = state.runtime_event_replay_metrics_snapshot();
        assert_eq!(metrics.checkpoint_loads, 1);
        assert_eq!(metrics.checkpoint_metadata_refreshes, 1);
        assert!(metrics
            .last_checkpoint_metadata_refresh_at_unix_ms
            .is_some());
    }

    #[tokio::test]
    async fn runtime_event_checkpoint_load_prunes_expired_rows() {
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

        {
            let guard = state.db.lock().expect("db lock");
            crate::db::save_runtime_event_checkpoint(
                &guard,
                &RuntimeEventCheckpoint {
                    consumer_id: "stale-ui".into(),
                    generation: state.runtime_event_generation.clone(),
                    seq: 0,
                    updated_at_unix_ms: 1,
                    created_at_unix_ms: None,
                    last_read_at_unix_ms: None,
                    lease_expires_at_unix_ms: None,
                    revision: None,
                    last_transition: None,
                    recovery_hint: None,
                    current_oldest_available_seq: None,
                    current_latest_available_seq: None,
                    current_compaction_watermark_seq: None,
                    current_compaction_watermark_segment_id: None,
                },
            )
            .expect("save stale checkpoint");
        }

        let loaded = state
            .runtime_event_checkpoint("stale-ui")
            .await
            .expect("load checkpoint");
        assert_eq!(loaded, None);

        let listed = state
            .runtime_event_checkpoints()
            .await
            .expect("list checkpoints");
        assert!(listed.is_empty());

        let metrics = state.runtime_event_replay_metrics_snapshot();
        assert_eq!(metrics.expired_checkpoint_misses, 1);
        assert_eq!(metrics.pruned_checkpoint_count, 1);
    }

    #[tokio::test]
    async fn runtime_event_checkpoint_same_cursor_counts_as_heartbeat() {
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

        let event = state.record_runtime_event("device_updated", json!({ "index": 1 }));
        let generation = state.runtime_event_generation.clone();
        state
            .save_runtime_event_checkpoint("shared-ui", &generation, event.seq)
            .await
            .expect("save checkpoint");
        state
            .save_runtime_event_checkpoint("shared-ui", &generation, event.seq)
            .await
            .expect("renew checkpoint");

        let metrics = state.runtime_event_replay_metrics_snapshot();
        assert_eq!(metrics.checkpoint_saves, 2);
        assert_eq!(metrics.checkpoint_heartbeats, 1);
        assert!(metrics.last_checkpoint_heartbeat_at_unix_ms.is_some());
    }

    #[tokio::test]
    async fn runtime_event_checkpoint_tracks_consumer_lifecycle_metadata() {
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

        let first = state.record_runtime_event("device_updated", json!({ "index": 1 }));
        let second = state.record_runtime_event("device_updated", json!({ "index": 2 }));
        let generation = state.runtime_event_generation.clone();

        let created = state
            .save_runtime_event_checkpoint("shared-ui", &generation, first.seq)
            .await
            .expect("create checkpoint");
        assert_eq!(created.revision, Some(1));
        assert_eq!(created.last_transition.as_deref(), Some("created"));
        assert_eq!(created.recovery_hint.as_deref(), Some("hot_window"));
        assert_eq!(created.current_oldest_available_seq, Some(1));
        assert_eq!(created.current_latest_available_seq, Some(2));
        assert!(created.lease_expires_at_unix_ms.is_some());

        let advanced = state
            .save_runtime_event_checkpoint("shared-ui", &generation, second.seq)
            .await
            .expect("advance checkpoint");
        assert_eq!(advanced.revision, Some(2));
        assert_eq!(advanced.last_transition.as_deref(), Some("advanced"));
        assert_eq!(advanced.recovery_hint.as_deref(), Some("up_to_date"));
        assert_eq!(advanced.created_at_unix_ms, created.created_at_unix_ms);

        let rewound = state
            .save_runtime_event_checkpoint("shared-ui", &generation, first.seq)
            .await
            .expect("rewind checkpoint");
        assert_eq!(rewound.revision, Some(3));
        assert_eq!(rewound.last_transition.as_deref(), Some("rewound"));
        assert_eq!(rewound.recovery_hint.as_deref(), Some("hot_window"));

        let metrics = state.runtime_event_replay_metrics_snapshot();
        assert_eq!(metrics.checkpoint_creates, 1);
        assert_eq!(metrics.checkpoint_advances, 1);
        assert_eq!(metrics.checkpoint_rewinds, 1);
        assert_eq!(metrics.checkpoint_generation_resets, 0);
        assert!(metrics.last_checkpoint_transition_at_unix_ms.is_some());
    }

    #[tokio::test]
    async fn runtime_event_checkpoint_generation_reset_is_tracked() {
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

        let event = state.record_runtime_event("device_updated", json!({ "index": 1 }));
        let generation = state.runtime_event_generation.clone();
        state
            .save_runtime_event_checkpoint("shared-ui", &generation, event.seq)
            .await
            .expect("save checkpoint");

        let reset = state
            .save_runtime_event_checkpoint("shared-ui", "older-generation", event.seq)
            .await
            .expect("save generation reset");
        assert_eq!(reset.last_transition.as_deref(), Some("generation_reset"));
        assert_eq!(reset.recovery_hint.as_deref(), Some("resync_required"));

        let metrics = state.runtime_event_replay_metrics_snapshot();
        assert_eq!(metrics.checkpoint_generation_resets, 1);
        assert_eq!(metrics.checkpoint_resync_transitions, 1);
        assert!(metrics.last_checkpoint_transition_at_unix_ms.is_some());
    }

    #[tokio::test]
    async fn runtime_event_checkpoint_listing_refreshes_recovery_metadata() {
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

        let event = state.record_runtime_event("device_updated", json!({ "index": 1 }));
        let generation = state.runtime_event_generation.clone();
        state
            .save_runtime_event_checkpoint("shared-ui", &generation, event.seq)
            .await
            .expect("save checkpoint");

        {
            let mut feed = state
                .runtime_event_feed
                .lock()
                .expect("runtime event feed lock poisoned");
            feed.clear();
            for seq in 10..=20 {
                feed.push_back(RuntimeEventEnvelope {
                    seq,
                    event: "device_updated".to_string(),
                    payload: json!({ "index": seq }),
                    emitted_at_unix_ms: seq,
                });
            }
        }
        state
            .runtime_event_persisted_oldest_seq
            .store(10, Ordering::SeqCst);
        state.runtime_event_seq.store(20, Ordering::SeqCst);

        let checkpoints = state
            .runtime_event_checkpoints()
            .await
            .expect("list checkpoints");
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(
            checkpoints[0].recovery_hint.as_deref(),
            Some("resync_required")
        );
        assert_eq!(checkpoints[0].current_oldest_available_seq, Some(10));
        assert_eq!(checkpoints[0].current_latest_available_seq, Some(20));

        let metrics = state.runtime_event_replay_metrics_snapshot();
        assert!(metrics.checkpoint_metadata_refreshes >= 1);
        assert_eq!(metrics.checkpoint_resync_transitions, 1);
        assert!(metrics
            .last_checkpoint_metadata_refresh_at_unix_ms
            .is_some());
    }

    #[test]
    fn runtime_event_feed_rehydrates_from_persisted_journal() {
        let config_dir =
            std::env::temp_dir().join(format!("dashdrop-runtime-events-{}", uuid::Uuid::new_v4()));
        let base_identity = Identity {
            fingerprint: "fp".to_string(),
            cert_der: Vec::new(),
            key_der: Vec::new(),
            device_name: "DashDrop Test".to_string(),
        };

        let first_db = crate::db::init_db_at(&config_dir).expect("init first db");
        let first_state = AppState::new(base_identity.clone(), AppConfig::default(), first_db);
        first_state.record_runtime_event("device_discovered", json!({ "fingerprint": "one" }));
        first_state.record_runtime_event("device_updated", json!({ "fingerprint": "two" }));
        let generation = first_state.runtime_event_generation.clone();

        let second_db = crate::db::init_db_at(&config_dir).expect("init second db");
        let second_state = AppState::new(base_identity, AppConfig::default(), second_db);
        let snapshot = second_state.runtime_events_since(0, 10);

        assert_eq!(second_state.runtime_event_generation, generation);
        assert_eq!(snapshot.events.len(), 2);
        assert_eq!(snapshot.events[0].seq, 1);
        assert_eq!(snapshot.events[1].seq, 2);
        assert_eq!(
            second_state
                .runtime_event_persisted_oldest_seq
                .load(Ordering::SeqCst),
            1
        );
        assert_eq!(snapshot.latest_available_seq, 2);

        let _ = std::fs::remove_dir_all(config_dir);
    }

    #[tokio::test]
    async fn issued_local_ipc_access_grant_validates_until_expiry() {
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

        let grant = state.issue_local_ipc_access_grant(None).await;
        assert!(grant.refresh_after_unix_ms < grant.expires_at_unix_ms);
        state
            .validate_local_ipc_access_token(Some(&grant.access_token))
            .await
            .expect("issued token should validate");
        assert!(state.validate_local_ipc_access_token(None).await.is_err());
        assert!(state
            .validate_local_ipc_access_token(Some("unknown-token"))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn refreshing_local_ipc_access_grant_revokes_previous_token() {
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

        let first = state.issue_local_ipc_access_grant(None).await;
        let second = state
            .issue_local_ipc_access_grant(Some(&first.access_token))
            .await;

        assert_ne!(first.access_token, second.access_token);
        assert!(state
            .validate_local_ipc_access_token(Some(&first.access_token))
            .await
            .is_err());
        state
            .validate_local_ipc_access_token(Some(&second.access_token))
            .await
            .expect("replacement token should validate");
    }

    #[tokio::test]
    async fn revoking_local_ipc_access_grant_invalidates_token() {
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

        let grant = state.issue_local_ipc_access_grant(None).await;
        state
            .revoke_local_ipc_access_token(Some(&grant.access_token))
            .await
            .expect("issued token should revoke");
        assert!(state
            .validate_local_ipc_access_token(Some(&grant.access_token))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn headless_idle_blockers_include_access_grants_and_active_transfers() {
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

        let _grant = state.issue_local_ipc_access_grant(None).await;
        state.transfers.write().await.insert(
            "transfer-1".to_string(),
            TransferTask {
                id: "transfer-1".to_string(),
                batch_id: None,
                direction: TransferDirection::Send,
                peer_fingerprint: "peer-fp".to_string(),
                peer_name: "Peer".to_string(),
                items: Vec::new(),
                status: TransferStatus::Transferring,
                bytes_transferred: 12,
                total_bytes: 42,
                revision: 1,
                started_at_unix: 1,
                ended_at_unix: None,
                terminal_reason_code: None,
                error: None,
                source_paths: None,
                source_path_by_file_id: None,
                failed_file_ids: None,
                conn: None,
                ended_at: None,
            },
        );

        let blockers = state.headless_idle_blockers().await;
        assert!(blockers
            .iter()
            .any(|entry| entry == "local_ipc_access_grants:1"));
        assert!(blockers.iter().any(|entry| entry == "active_transfers:1"));
    }

    #[test]
    fn runtime_status_defaults_are_backward_safe() {
        assert_eq!(default_listener_port_mode(), "fallback_random");
        assert_eq!(default_firewall_rule_state(), "unknown");
        let status: RuntimeStatus = serde_json::from_value(json!({
            "local_port": 0,
            "mdns_registered": false,
            "discovered_devices": 0,
            "trusted_devices": 0
        }))
        .expect("legacy runtime status should deserialize");
        assert!(!status.daemon_idle_monitor_enabled);
        assert_eq!(status.daemon_idle_timeout_secs, None);
        assert_eq!(status.daemon_idle_deadline_unix_ms, None);
        assert!(status.daemon_idle_blockers.is_empty());
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
        assert!(!config.launch_at_login);
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

    #[test]
    fn notification_id_field_name_is_stable() {
        let value = serde_json::to_value(IncomingRequestNotification {
            notification_id: "notif-1".into(),
            transfer_id: "transfer-1".into(),
            active: true,
            terminal_reason_code: Some("E_REQUEST_EXPIRED".into()),
            updated_at_unix: 1,
        })
        .expect("serialize notification");

        assert_eq!(value["notification_id"], json!("notif-1"));
        assert!(value.get("notificationId").is_none());
    }
}
