export type Platform = "Mac" | "Windows" | "Linux" | "Android" | "Ios" | "Unknown";
export type ControlPlaneMode = "in_process" | "daemon";

export type ReachabilityStatus = "discovered" | "reachable" | "offline_candidate" | "offline";

export type ListenerPortMode = "fixed" | "fallback_random";

export type FirewallRuleState = "managed" | "user_scope_unmanaged" | "unknown";

export type PowerProfile = "ac" | "battery" | "low_power";

export interface DeviceView {
  fingerprint: string;
  name: string;
  platform: Platform;
  trusted: boolean;
  sessions: Record<string, SessionView>;
  last_seen?: number;
  reachability?: ReachabilityStatus;
  probe_fail_count?: number;
  last_probe_at?: number | null;
}

export interface SessionView {
  session_id: string;
  addrs: string[];
  last_seen_unix: number;
}

export interface TrustedPeer {
  fingerprint: string;
  name: string;
  paired_at: number;
  trust_level?: "legacy_paired" | "signed_link_verified" | "mutual_confirmed" | "frozen";
  last_verification_method?:
    | "manual_pairing"
    | "legacy_unsigned_link"
    | "signed_pairing_link"
    | "mutual_receipt";
  alias?: string | null;
  last_used_at?: number | null;
  remote_confirmation_material_seen_at?: number | null;
  local_confirmation_at?: number | null;
  mutual_confirmed_at?: number | null;
  frozen_at?: number | null;
  freeze_reason?: string | null;
}

export type FileConflictStrategy = "rename" | "overwrite" | "skip";
export type RiskClass = "high" | "normal";
export type ProtocolFileType = "RegularFile" | "Directory";

export interface AppConfig {
  device_name: string;
  auto_accept_trusted_only: boolean;
  download_dir: string | null;
  file_conflict_strategy: FileConflictStrategy;
  max_parallel_streams: number;
  launch_at_login: boolean;
}

export interface ConnectByAddressResult {
  fingerprint: string;
  name: string;
  trusted: boolean;
  address: string;
}

export type TransferDirection = "Send" | "Receive";

export type TransferStatus =
  | "Draft"
  | "PendingAccept"
  | "Transferring"
  | "Completed"
  | "PartialCompleted"
  | "Rejected"
  | "CancelledBySender"
  | "CancelledByReceiver"
  | "Failed";

export interface FileItemMeta {
  file_id: number;
  name: string;
  rel_path: string;
  size: number;
  risk_class?: RiskClass;
}

export interface SourceSnapshot {
  size: number;
  mtime_unix_ms: number;
  head_hash: number[];
}

export interface TransferFileItem extends FileItemMeta {
  file_type: ProtocolFileType;
  modified: number;
  source_snapshot?: SourceSnapshot | null;
}

export interface TransferView {
  id: string;
  batch_id?: string | null;
  direction: TransferDirection;
  peer_fingerprint: string;
  peer_name: string;
  items: FileItemMeta[];
  status: TransferStatus;
  bytes_transferred: number;
  total_bytes: number;
  revision?: number;
  started_at_unix?: number;
  ended_at_unix?: number | null;
  terminal_reason_code?: string | null;
  error?: string;
}

export interface LocalIdentity {
  fingerprint: string;
  device_name: string;
  port: number;
}

export interface BleAssistCapsule {
  version: number;
  issued_at_unix_ms: number;
  expires_at_unix_ms: number;
  rolling_identifier: string;
  integrity_tag: string;
  transport_hint: string;
  qr_fallback_available: boolean;
  short_code_fallback_available: boolean;
  rotation_window_ms: number;
}

export interface TransferStartedPayload {
  transfer_id: string;
  batch_id?: string | null;
  peer_fp: string;
  peer_name: string;
  items: TransferFileItem[];
  total_size: number;
  revision: number;
}

export interface TransferIncomingPayload {
  transfer_id: string;
  batch_id?: string | null;
  notification_id: string;
  sender_name: string;
  sender_fp: string;
  trusted: boolean;
  items: TransferFileItem[];
  total_size: number;
  revision: number;
}

export interface TransferAcceptedPayload {
  transfer_id: string;
  batch_id?: string | null;
  revision: number;
}

export interface TransferProgressPayload {
  transfer_id: string;
  batch_id?: string | null;
  bytes_transferred: number;
  total_bytes: number;
  revision: number;
}

export interface TransferCompletePayload {
  transfer_id: string;
  batch_id?: string | null;
  revision: number;
}

export interface FailedFile {
  file_id: number;
  name: string;
  reason: string;
}

export interface TransferPartialPayload {
  transfer_id: string;
  batch_id?: string | null;
  succeeded_count: number;
  failed: FailedFile[];
  terminal_cause?: string;
  revision: number;
}

export interface TransferRejectedPayload {
  transfer_id: string;
  batch_id?: string | null;
  reason_code: string;
  terminal_cause: string;
  revision: number;
}

export interface TransferCancelledPayload {
  transfer_id: string;
  batch_id?: string | null;
  reason_code: string;
  terminal_cause: string;
  revision: number;
}

export interface TransferFailedPayload {
  transfer_id: string;
  batch_id?: string | null;
  reason_code: string;
  terminal_cause: string;
  phase?: string;
  revision: number;
}

export interface TransferErrorPayload {
  transfer_id: string | null;
  batch_id?: string | null;
  reason_code: string;
  terminal_cause: string;
  phase: string;
  revision: number;
  detail?: string;
}

export interface IdentityMismatchPayload {
  expected_fp?: string;
  actual_fp?: string;
  remote_addr?: string;
  mdns_fp?: string;
  cert_fp?: string;
  phase?: string;
}

export interface FingerprintChangedPayload {
  session_id: string;
  previous_fp: string;
  current_fp: string;
  previous_trust_level?: TrustedPeer["trust_level"];
  remote_addr?: string;
  phase?: string;
}

export interface SystemErrorPayload {
  subsystem: string;
  message: string;
  code?: string;
  attempted_device_name?: string;
  rollback_device_name?: string;
  daemon_status?: string;
  daemon_connect_attempts?: number;
  daemon_connect_strategy?: string;
  daemon_binary_path?: string | null;
}

export interface TrustedPeerUpdatedPayload {
  fingerprint: string;
  action: "paired" | "unpaired" | "alias_updated" | "verification_confirmed";
  trust_level?: TrustedPeer["trust_level"];
  last_verification_method?: TrustedPeer["last_verification_method"];
  mutual_confirmed_at?: number | null;
  alias?: string | null;
}

export interface AppConfigUpdatedPayload {
  config: AppConfig;
}

export interface ExternalSharePayload {
  paths: string[];
  source?: string | null;
}

export interface PairingLinkPayload {
  pairing_link: string;
  source?: string | null;
}

export interface DaemonEventFeedResyncPayload {
  source?: string | null;
  reason?:
    | "generation_changed"
    | "cursor_invalid"
    | "cursor_before_oldest_available"
    | "cursor_after_latest_available"
    | "persisted_catch_up_empty"
    | "persisted_journal_unavailable";
  generation?: string;
  oldest_available_seq?: number | null;
  latest_available_seq?: number;
  replay_source?: RuntimeEventFeedSnapshot["replay_source"];
}

export interface AppNavigationPayload {
  target: "Nearby" | "Transfers" | "History" | "TrustedDevices" | "SecurityEvents" | "Settings";
  source?: string | null;
}

export interface RuntimeEventEnvelope {
  seq: number;
  event: string;
  payload: unknown;
  emitted_at_unix_ms: number;
}

export interface RuntimeEventFeedSnapshot {
  events: RuntimeEventEnvelope[];
  generation: string;
  oldest_available_seq?: number | null;
  latest_available_seq: number;
  resync_required: boolean;
  replay_source?: "memory_hot_window" | "persisted_catch_up" | "resync_required" | "empty";
  resync_reason?:
    | "cursor_before_oldest_available"
    | "cursor_after_latest_available"
    | "persisted_catch_up_empty"
    | "persisted_journal_unavailable";
}

export interface RuntimeEventCheckpoint {
  consumer_id: string;
  generation: string;
  seq: number;
  updated_at_unix_ms: number;
}

export interface SecurityEvent {
  id: number;
  event_type: string;
  occurred_at_unix: number;
  phase: string;
  peer_fingerprint?: string | null;
  reason: string;
}

export interface RuntimeStatus {
  local_port: number;
  mdns_registered: boolean;
  discovered_devices: number;
  trusted_devices: number;
  // Requested startup policy (for example env/profile preference).
  requested_control_plane_mode?: ControlPlaneMode | string;
  // Actual running control plane that the UI should use for reads/events.
  control_plane_mode?: ControlPlaneMode | string;
  runtime_profile?: string;
  daemon_status?: string;
  daemon_connect_attempts?: number;
  daemon_connect_strategy?: string;
  daemon_binary_path?: string | null;
  listener_port_mode?: ListenerPortMode;
  firewall_rule_state?: FirewallRuleState;
  daemon_idle_monitor_enabled?: boolean;
  daemon_idle_timeout_secs?: number | null;
  daemon_idle_deadline_unix_ms?: number | null;
  daemon_idle_blockers?: string[];
}

export interface DiscoveryDiagnostics {
  runtime: RuntimeStatus;
  service_type?: string;
  beacon_port?: number;
  power_profile?: PowerProfile;
  beacon_interval_secs?: number;
  own_fingerprint?: string;
  own_platform?: Platform;
  mdns_daemon_initialized: boolean;
  mdns_service_fullname?: string | null;
  mdns_interface_policy?: string;
  mdns_enabled_interfaces?: string[];
  mdns_last_search_started?: string | null;
  local_instance_name?: string | null;
  listener_mode?: string;
  listener_port_mode?: ListenerPortMode;
  firewall_rule_state?: FirewallRuleState;
  listener_addrs?: string[];
  network_interfaces?: Array<{
    name: string;
    is_loopback: boolean;
    ipv4: string[];
    ipv6: string[];
  }>;
  browser_status?: {
    active: boolean;
    restart_count: number;
    last_disconnect_at?: number | null;
    last_search_started?: string | null;
  };
  session_index_count?: number;
  session_stale_ttl_secs?: number;
  discovery_event_counts?: Record<string, number>;
  discovery_failure_counts?: Record<string, number>;
  quick_hints?: string[];
  runtime_event_replay?: {
    generation?: string;
    latest_seq?: number;
    memory_window_capacity?: number;
    memory_window_len?: number;
    memory_oldest_seq?: number | null;
    persisted_window_capacity?: number;
    persisted_window_max_capacity?: number;
    persisted_segment_size?: number;
    persisted_segment_count?: number;
    oldest_persisted_segment_id?: number | null;
    latest_persisted_segment_id?: number | null;
    compacted_segment_count?: number;
    latest_compacted_segment_id?: number | null;
    compaction_watermark_seq?: number;
    compaction_watermark_segment_id?: number;
    last_compacted_at_unix_ms?: number | null;
    persisted_window_len?: number;
    persisted_oldest_seq?: number | null;
    oldest_recoverable_seq?: number | null;
    persisted_baseline_oldest_seq?: number | null;
    persisted_max_oldest_seq?: number | null;
    retention_mode?: 'empty' | 'baseline' | 'checkpoint_pinned' | 'checkpoint_pinned_max_capped';
    retention_cutoff_reason?: string;
    persisted_journal_health?: 'available' | 'empty' | 'unavailable';
    retention_pinned_checkpoint_count?: number;
    oldest_retention_pinned_checkpoint_seq?: number | null;
    checkpoint_heartbeat_interval_ms?: number;
    checkpoint_active_threshold_ms?: number;
    checkpoint_stale_threshold_ms?: number;
    checkpoint_ttl_ms?: number;
    checkpoint_count?: number;
    active_checkpoint_count?: number;
    idle_checkpoint_count?: number;
    stale_checkpoint_count?: number;
    resync_required_checkpoint_count?: number;
    metrics?: {
      total_feed_requests?: number;
      memory_feed_requests?: number;
      persisted_catch_up_requests?: number;
      persisted_catch_up_events_served?: number;
      resync_required_requests?: number;
      last_replay_source?: "memory_hot_window" | "persisted_catch_up" | "resync_required" | "empty";
      last_replay_source_at_unix_ms?: number | null;
      last_resync_reason?:
        | "cursor_before_oldest_available"
        | "cursor_after_latest_available"
        | "persisted_catch_up_empty"
        | "persisted_journal_unavailable"
        | null;
      checkpoint_loads?: number;
      checkpoint_saves?: number;
      checkpoint_heartbeats?: number;
      expired_checkpoint_misses?: number;
      pruned_checkpoint_count?: number;
      last_persisted_catch_up_at_unix_ms?: number | null;
      last_resync_required_at_unix_ms?: number | null;
      last_checkpoint_heartbeat_at_unix_ms?: number | null;
      last_checkpoint_prune_at_unix_ms?: number | null;
    };
    checkpoints?: Array<{
      consumer_id: string;
      generation: string;
      seq: number;
      updated_at_unix_ms: number;
      lease_expires_at_unix_ms?: number;
      age_ms?: number;
      lag_events?: number;
      lifecycle_state?: 'active' | 'idle' | 'stale';
      recovery_state?: 'up_to_date' | 'hot_window' | 'persisted_catch_up' | 'resync_required';
      recovery_hint?: string | null;
      oldest_recoverable_seq?: number | null;
      retention_cutoff_reason?: string;
      persisted_journal_health?: 'available' | 'empty' | 'unavailable';
      consumer_recovery_mode?: 'incremental_catch_up' | 'authoritative_refresh';
      recovery_safety?: 'safe_incremental' | 'authoritative_refresh_required' | 'generation_mismatch';
      current_oldest_available_seq?: number | null;
      current_latest_available_seq?: number | null;
      current_compaction_watermark_seq?: number | null;
      current_compaction_watermark_segment_id?: number | null;
    }>;
  };
  transfer_progress_persistence?: {
    flush_interval_ms?: number;
    flush_threshold_bytes?: number;
    pending_transfer_count?: number;
    schedule_requests?: number;
    coalesced_updates?: number;
    interval_flushes?: number;
    threshold_flushes?: number;
    force_flushes?: number;
    terminal_flushes?: number;
    successful_writes?: number;
    failed_writes?: number;
    last_flush_at_unix_ms?: number | null;
    last_force_flush_at_unix_ms?: number | null;
  };
  link_capabilities?: {
    ble_baseline_enabled?: boolean;
    ble_supported?: boolean;
    ble_permission_state?: string;
    ble_runtime_available?: boolean;
    single_radio_risk?: boolean;
    softap_capable?: boolean;
    p2p_capable?: boolean;
    rolling_identifier_mode?: string;
    ephemeral_capsule_mode?: string;
    fallback_mode?: string;
    provider_name?: string;
    scanner_state?: string;
    advertiser_state?: string;
    bridge_mode?: string | null;
    bridge_file_path?: string | null;
    advertisement_file_path?: string | null;
    last_started_at_unix_ms?: number | null;
    last_error?: string | null;
    last_capsule_ingested_at_unix_ms?: number | null;
    last_observation_prune_at_unix_ms?: number | null;
    last_bridge_snapshot_at_unix_ms?: number | null;
    last_advertisement_request_at_unix_ms?: number | null;
    advertised_rolling_identifier?: string | null;
    observed_capsule_count?: number;
    recent_capsules?: Array<{
      rolling_identifier: string;
      integrity_tag: string;
      last_seen_at_unix_ms: number;
      expires_at_unix_ms: number;
      transport_hint: string;
    }>;
    notes?: string[];
  };
  device_count: number;
  devices: Array<{
    fingerprint: string;
    name: string;
    platform: Platform;
    trusted: boolean;
    reachability: ReachabilityStatus;
    probe_fail_count: number;
    last_probe_at?: number | null;
    last_seen: number;
    session_count: number;
    best_addrs?: string[];
    scope_less_link_local_ipv6_count?: number;
    last_resolve_stats?: {
      raw_addr_count: number;
      usable_addr_count: number;
      hostname?: string | null;
      port?: number | null;
      at?: number | null;
    };
    last_probe_result?: {
      result?: string | null;
      error?: string | null;
      error_detail?: string | null;
      addr?: string | null;
      attempted_addrs?: string[];
      at?: number | null;
    };
    sessions: Array<{
      session_id: string;
      last_seen_unix: number;
      addrs: string[];
    }>;
  }>;
}

export interface TransferMetrics {
  completed: number;
  partial: number;
  failed: number;
  cancelled_by_sender: number;
  cancelled_by_receiver: number;
  rejected: number;
  bytes_sent: number;
  bytes_received: number;
  average_duration_ms: number;
  failure_distribution: Record<string, number>;
}
