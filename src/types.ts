export type Platform = "Mac" | "Windows" | "Linux" | "Android" | "Ios" | "Unknown";

export type ReachabilityStatus = "discovered" | "reachable" | "offline_candidate" | "offline";

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
  alias?: string | null;
  last_used_at?: number | null;
}

export type FileConflictStrategy = "rename" | "overwrite" | "skip";

export interface AppConfig {
  device_name: string;
  auto_accept_trusted_only: boolean;
  download_dir: string | null;
  file_conflict_strategy: FileConflictStrategy;
  max_parallel_streams: number;
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
}

export interface TransferView {
  id: string;
  direction: TransferDirection;
  peer_fingerprint: string;
  peer_name: string;
  items: FileItemMeta[];
  status: TransferStatus;
  bytes_transferred: number;
  total_bytes: number;
  revision?: number;
  ended_at_unix?: number | null;
  error?: string;
}

export interface LocalIdentity {
  fingerprint: string;
  device_name: string;
  port: number;
}

export interface TransferStartedPayload {
  transfer_id: string;
  peer_fp: string;
  peer_name: string;
  items: FileItemMeta[];
  total_size: number;
  revision: number;
}

export interface TransferIncomingPayload {
  transfer_id: string;
  sender_name: string;
  sender_fp: string;
  trusted: boolean;
  items: FileItemMeta[];
  total_size: number;
  revision: number;
}

export interface TransferAcceptedPayload {
  transfer_id: string;
  revision: number;
}

export interface TransferProgressPayload {
  transfer_id: string;
  bytes_transferred: number;
  total_bytes: number;
  revision: number;
}

export interface TransferCompletePayload {
  transfer_id: string;
  revision: number;
}

export interface FailedFile {
  file_id: number;
  name: string;
  reason: string;
}

export interface TransferPartialPayload {
  transfer_id: string;
  succeeded_count: number;
  failed: FailedFile[];
  terminal_cause?: string;
  revision: number;
}

export interface TransferRejectedPayload {
  transfer_id: string;
  reason_code: string;
  terminal_cause: string;
  revision: number;
}

export interface TransferCancelledPayload {
  transfer_id: string;
  reason_code: string;
  terminal_cause: string;
  revision: number;
}

export interface TransferFailedPayload {
  transfer_id: string;
  reason_code: string;
  terminal_cause: string;
  phase?: string;
  revision: number;
}

export interface TransferErrorPayload {
  transfer_id: string | null;
  reason_code: string;
  terminal_cause: string;
  phase: string;
  revision: number;
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
  remote_addr?: string;
  phase?: string;
}

export interface SystemErrorPayload {
  subsystem: string;
  message: string;
  code?: string;
  attempted_device_name?: string;
  rollback_device_name?: string;
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
}
