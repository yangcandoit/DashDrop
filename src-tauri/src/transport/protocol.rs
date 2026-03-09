use serde::{Deserialize, Serialize};

// ─── Protocol Version ─────────────────────────────────────────────────────────

pub const WIRE_VERSION: u8 = 0;
pub const SUPPORTED_VERSIONS: &[u32] = &[1];

pub const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024; // 16 MiB
pub const CHUNK_SIZE: usize = 1024 * 1024; // 1 MiB
pub const MAX_CONCURRENT_STREAMS: u32 = 4;
pub const USER_RESPONSE_TIMEOUT_SECS: u64 = 60;

// ─── Error Codes ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ErrorCode {
    Rejected,
    DiskFull,
    HashMismatch,
    VersionMismatch,
    RateLimited,
    Timeout,
    Cancelled,
    PermissionDenied,
    IdentityMismatch,
    InvalidPath,
    PathConflict,
    UnsupportedFileType,
    Protocol(String),
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ErrorCode::Rejected => write!(f, "Rejected by receiver"),
            ErrorCode::DiskFull => write!(f, "Receiver disk full"),
            ErrorCode::HashMismatch => write!(f, "File hash mismatch"),
            ErrorCode::VersionMismatch => write!(f, "Protocol version mismatch"),
            ErrorCode::RateLimited => write!(f, "Rate limited"),
            ErrorCode::Timeout => write!(f, "Connection timeout"),
            ErrorCode::Cancelled => write!(f, "Cancelled"),
            ErrorCode::PermissionDenied => write!(f, "Permission denied"),
            ErrorCode::IdentityMismatch => write!(f, "Identity mismatch"),
            ErrorCode::InvalidPath => write!(f, "Invalid path"),
            ErrorCode::PathConflict => write!(f, "Path conflict"),
            ErrorCode::UnsupportedFileType => write!(f, "Unsupported file type"),
            ErrorCode::Protocol(s) => write!(f, "Protocol error: {s}"),
        }
    }
}

impl ErrorCode {
    pub fn reason_code(&self) -> &'static str {
        match self {
            ErrorCode::Rejected => "E_REJECTED_BY_PEER",
            ErrorCode::DiskFull => "E_DISK_FULL",
            ErrorCode::HashMismatch => "E_HASH_MISMATCH",
            ErrorCode::VersionMismatch => "E_VERSION_MISMATCH",
            ErrorCode::RateLimited => "E_RATE_LIMITED",
            ErrorCode::Timeout => "E_TIMEOUT",
            ErrorCode::Cancelled => "E_CANCELLED",
            ErrorCode::PermissionDenied => "E_PERMISSION_DENIED",
            ErrorCode::IdentityMismatch => "E_IDENTITY_MISMATCH",
            ErrorCode::InvalidPath => "E_INVALID_PATH",
            ErrorCode::PathConflict => "E_PATH_CONFLICT",
            ErrorCode::UnsupportedFileType => "E_UNSUPPORTED_FILE_TYPE",
            ErrorCode::Protocol(_) => "E_PROTOCOL",
        }
    }
}

// ─── File Types and Items ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FileType {
    RegularFile,
    Directory,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileItem {
    pub file_id: u32,
    pub name: String,
    pub rel_path: String,
    pub size: u64,
    pub file_type: FileType,
    pub modified: u64,
}

// ─── Message Payloads ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloPayload {
    /// Fixed at 0 forever — the outer shell that must always be parseable.
    pub wire_version: u8,
    /// Business protocol versions this peer supports.
    pub supported_versions: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfferPayload {
    pub transfer_id: String,
    pub items: Vec<FileItem>,
    pub total_size: u64,
    pub sender_name: String,
    pub sender_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptPayload {
    pub chosen_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectPayload {
    pub reason: ErrorCode,
}

/// One 1 MiB block of file data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkPayload {
    pub file_id: u32,
    pub chunk_id: u32,
    pub offset: u64,
    pub data: Vec<u8>,
}

/// Sent after all chunks for a file have been sent. Contains BLAKE3 hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletePayload {
    pub file_id: u32,
    pub file_hash: [u8; 32],
}

/// Sent by receiver after verifying a complete file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AckPayload {
    pub file_id: u32,
    pub ok: bool,
    pub reason: Option<ErrorCode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelPayload {
    pub reason: CancelReason,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CancelReason {
    UserCancelled,
    Error(ErrorCode),
}

// ─── Top-level Message Enum ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DashMessage {
    Hello(HelloPayload),
    Offer(OfferPayload),
    Accept(AcceptPayload),
    Reject(RejectPayload),
    Chunk(ChunkPayload),
    Complete(CompletePayload),
    Ack(AckPayload),
    Cancel(CancelPayload),
}

// ─── Transfer Outcome ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedFile {
    pub file_id: u32,
    pub name: String,
    pub reason: ErrorCode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TransferOutcome {
    Success,
    PartialSuccess(Vec<FailedFile>),
    Failed(ErrorCode),
    CancelledBySender,
    CancelledByReceiver,
}

// ─── CBOR Codec ──────────────────────────────────────────────────────────────

use anyhow::{Context, Result};

/// Write a length-prefixed CBOR message to a QUIC send stream.
pub async fn write_message(send: &mut quinn::SendStream, msg: &DashMessage) -> Result<()> {
    let mut buf = Vec::new();
    ciborium::into_writer(msg, &mut buf).context("CBOR serialize")?;
    let len = buf.len() as u32;
    if len as usize > MAX_MESSAGE_SIZE {
        anyhow::bail!("message too large to send: {} bytes", len);
    }
    send.write_all(&len.to_be_bytes())
        .await
        .context("write len")?;
    send.write_all(&buf).await.context("write body")?;
    Ok(())
}

/// Read a length-prefixed CBOR message from a QUIC receive stream.
pub async fn read_message(recv: &mut quinn::RecvStream) -> Result<DashMessage> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await.context("read len")?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_MESSAGE_SIZE {
        anyhow::bail!("message too large: {len} bytes (max {MAX_MESSAGE_SIZE})");
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await.context("read body")?;
    let msg: DashMessage = ciborium::from_reader(buf.as_slice()).context("CBOR deserialize")?;
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::ErrorCode;

    #[test]
    fn reason_codes_match_protocol_shape() {
        assert_eq!(ErrorCode::HashMismatch.reason_code(), "E_HASH_MISMATCH");
        assert_eq!(ErrorCode::Timeout.reason_code(), "E_TIMEOUT");
        assert_eq!(ErrorCode::RateLimited.reason_code(), "E_RATE_LIMITED");
        assert_eq!(ErrorCode::Protocol("x".into()).reason_code(), "E_PROTOCOL");
    }
}
