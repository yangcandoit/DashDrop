export type Platform = "Mac" | "Windows" | "Linux" | "Android" | "Ios" | "Unknown";

export interface DeviceInfo {
    fingerprint: string;
    name: string;
    platform: Platform;
    trusted: boolean;
    sessions: Record<string, SessionInfo>;
    last_seen?: number;
}

export interface SessionInfo {
    session_id: string;
    addrs: string[];
    last_seen: {
        secs: number;
        nanos: number;
    };
}

export interface TrustedPeer {
    fingerprint: string;
    name: string;
    paired_at: string;
}

export type TransferDirection = 'Send' | 'Receive';

export type TransferStatus =
    | 'AwaitingAccept'
    | 'Transferring'
    | 'Complete'
    | 'PartialSuccess'
    | 'Failed'
    | 'Cancelled';

export interface FileItemMeta {
    file_id: number;
    name: string;
    rel_path: string;
    size: number;
}

export interface TransferTask {
    id: string; // UUID
    direction: TransferDirection;
    peer_fingerprint: string;
    peer_name: string;
    items: FileItemMeta[];
    status: TransferStatus;
    bytes_transferred: number;
    total_bytes: number;
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
    total_size: number;
}

export interface TransferIncomingPayload {
    transfer_id: string;
    sender_name: string;
    sender_fp: string;
    trusted: boolean;
    items: FileItemMeta[];
    total_size: number;
}

export interface TransferProgressPayload {
    transfer_id: string;
    bytes_transferred: number;
}

export interface TransferCompletePayload {
    transfer_id: string;
}

export interface TransferPartialPayload {
    transfer_id: string;
    succeeded_count: number;
    failed: unknown[];
}

export interface TransferFailedPayload {
    transfer_id: string;
    reason: string;
    phase?: string;
}

export interface TransferErrorPayload {
    transfer_id: string | null;
    reason: string;
    phase: string;
}

export interface IdentityMismatchPayload {
    expected_fp?: string;
    actual_fp?: string;
    remote_addr?: string;
    mdns_fp?: string;
    cert_fp?: string;
    phase?: string;
}
