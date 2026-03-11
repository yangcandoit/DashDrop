use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};

use crate::state::{AppState, TransferStatus};
use crate::transport::protocol::FileItem;

fn emit_json(app: &AppHandle, event: &str, payload: serde_json::Value) {
    app.emit(event, payload)
        .unwrap_or_else(|e| tracing::warn!("Emit {event} failed: {e}"));
}

fn transfer_batch_id(app: &AppHandle, transfer_id: &str) -> Option<String> {
    let state = app.try_state::<Arc<AppState>>()?;
    let transfers = state.transfers.try_read().ok()?;
    transfers
        .get(transfer_id)
        .and_then(|task| task.batch_id.clone())
}

fn with_optional_batch_id(
    mut payload: serde_json::Value,
    batch_id: Option<&str>,
) -> serde_json::Value {
    if let Some(batch_id) = batch_id {
        payload["batch_id"] = serde_json::json!(batch_id);
    }
    payload
}

pub fn emit_transfer_started(
    app: &AppHandle,
    transfer_id: &str,
    peer_fp: &str,
    peer_name: &str,
    items: &[FileItem],
    total_size: u64,
    revision: u64,
) {
    let batch_id = transfer_batch_id(app, transfer_id);
    emit_json(
        app,
        "transfer_started",
        with_optional_batch_id(
            serde_json::json!({
                "transfer_id": transfer_id,
                "peer_fp": peer_fp,
                "peer_name": peer_name,
                "items": items,
                "total_size": total_size,
                "revision": revision,
            }),
            batch_id.as_deref(),
        ),
    );
}

#[allow(clippy::too_many_arguments)]
pub fn emit_transfer_incoming(
    app: &AppHandle,
    transfer_id: &str,
    notification_id: &str,
    sender_name: &str,
    sender_fp: &str,
    trusted: bool,
    items: &[FileItem],
    total_size: u64,
    revision: u64,
) {
    let batch_id = transfer_batch_id(app, transfer_id);
    emit_json(
        app,
        "transfer_incoming",
        with_optional_batch_id(
            serde_json::json!({
                "transfer_id": transfer_id,
                "notification_id": notification_id,
                "sender_name": sender_name,
                "sender_fp": sender_fp,
                "trusted": trusted,
                "items": items,
                "total_size": total_size,
                "revision": revision,
            }),
            batch_id.as_deref(),
        ),
    );
}

pub fn emit_transfer_accepted(app: &AppHandle, transfer_id: &str, revision: u64) {
    let batch_id = transfer_batch_id(app, transfer_id);
    emit_json(
        app,
        "transfer_accepted",
        with_optional_batch_id(
            serde_json::json!({
                "transfer_id": transfer_id,
                "revision": revision,
            }),
            batch_id.as_deref(),
        ),
    );
}

pub fn emit_transfer_progress(
    app: &AppHandle,
    transfer_id: &str,
    bytes_transferred: u64,
    total_bytes: u64,
    revision: u64,
) {
    let batch_id = transfer_batch_id(app, transfer_id);
    emit_json(
        app,
        "transfer_progress",
        with_optional_batch_id(
            serde_json::json!({
                "transfer_id": transfer_id,
                "bytes_transferred": bytes_transferred,
                "total_bytes": total_bytes,
                "revision": revision,
            }),
            batch_id.as_deref(),
        ),
    );
}

pub fn emit_transfer_complete(app: &AppHandle, transfer_id: &str, revision: u64) {
    let batch_id = transfer_batch_id(app, transfer_id);
    emit_json(
        app,
        "transfer_complete",
        with_optional_batch_id(
            serde_json::json!({
                "transfer_id": transfer_id,
                "revision": revision,
            }),
            batch_id.as_deref(),
        ),
    );
}

pub fn emit_transfer_partial(
    app: &AppHandle,
    transfer_id: &str,
    succeeded_count: usize,
    failed: &[crate::transport::protocol::FailedFile],
    terminal_cause: Option<&str>,
    revision: u64,
) {
    let batch_id = transfer_batch_id(app, transfer_id);
    let mut payload = with_optional_batch_id(
        serde_json::json!({
            "transfer_id": transfer_id,
            "succeeded_count": succeeded_count,
            "failed": failed,
            "revision": revision,
        }),
        batch_id.as_deref(),
    );

    if let Some(cause) = terminal_cause {
        payload["terminal_cause"] = serde_json::json!(cause);
    }

    emit_json(app, "transfer_partial", payload);
}

pub fn emit_transfer_terminal(
    app: &AppHandle,
    transfer_id: &str,
    status: &TransferStatus,
    reason_code: &str,
    terminal_cause: &str,
    revision: u64,
    phase: Option<&str>,
) {
    let Some(event) = terminal_event_name(status) else {
        return;
    };

    let batch_id = transfer_batch_id(app, transfer_id);
    let mut payload = with_optional_batch_id(
        serde_json::json!({
            "transfer_id": transfer_id,
            "reason_code": reason_code,
            "terminal_cause": terminal_cause,
            "revision": revision,
        }),
        batch_id.as_deref(),
    );

    if let Some(p) = phase {
        payload["phase"] = serde_json::json!(p);
    }

    emit_json(app, event, payload);
}

pub fn terminal_event_name(status: &TransferStatus) -> Option<&'static str> {
    match status {
        TransferStatus::Rejected => Some("transfer_rejected"),
        TransferStatus::CancelledBySender => Some("transfer_cancelled_by_sender"),
        TransferStatus::CancelledByReceiver => Some("transfer_cancelled_by_receiver"),
        TransferStatus::Failed => Some("transfer_failed"),
        _ => None,
    }
}

pub fn emit_transfer_error(
    app: &AppHandle,
    transfer_id: Option<&str>,
    reason_code: &str,
    terminal_cause: &str,
    phase: &str,
    revision: u64,
) {
    emit_transfer_error_with_detail(
        app,
        transfer_id,
        reason_code,
        terminal_cause,
        phase,
        revision,
        None,
    );
}

pub fn emit_transfer_error_with_detail(
    app: &AppHandle,
    transfer_id: Option<&str>,
    reason_code: &str,
    terminal_cause: &str,
    phase: &str,
    revision: u64,
    detail: Option<&str>,
) {
    let batch_id = transfer_id.and_then(|id| transfer_batch_id(app, id));
    let mut payload = with_optional_batch_id(
        serde_json::json!({
            "transfer_id": transfer_id,
            "reason_code": reason_code,
            "terminal_cause": terminal_cause,
            "phase": phase,
            "revision": revision,
        }),
        batch_id.as_deref(),
    );
    if let Some(d) = detail {
        payload["detail"] = serde_json::json!(d);
    }
    emit_json(app, "transfer_error", payload);
}

#[cfg(test)]
mod tests {
    use super::{terminal_event_name, with_optional_batch_id};
    use crate::state::TransferStatus;
    use serde_json::json;

    #[test]
    fn maps_terminal_statuses_to_fixed_event_names() {
        assert_eq!(
            terminal_event_name(&TransferStatus::Rejected),
            Some("transfer_rejected")
        );
        assert_eq!(
            terminal_event_name(&TransferStatus::CancelledBySender),
            Some("transfer_cancelled_by_sender")
        );
        assert_eq!(
            terminal_event_name(&TransferStatus::CancelledByReceiver),
            Some("transfer_cancelled_by_receiver")
        );
        assert_eq!(
            terminal_event_name(&TransferStatus::Failed),
            Some("transfer_failed")
        );
    }

    #[test]
    fn non_terminal_status_has_no_terminal_event_name() {
        assert_eq!(terminal_event_name(&TransferStatus::PendingAccept), None);
        assert_eq!(terminal_event_name(&TransferStatus::Transferring), None);
        assert_eq!(terminal_event_name(&TransferStatus::Completed), None);
        assert_eq!(terminal_event_name(&TransferStatus::PartialCompleted), None);
    }

    #[test]
    fn payload_extension_omits_batch_id_when_absent() {
        let payload = with_optional_batch_id(json!({ "transfer_id": "t-1" }), None);
        assert_eq!(payload.get("transfer_id"), Some(&json!("t-1")));
        assert!(payload.get("batch_id").is_none());
    }

    #[test]
    fn payload_extension_preserves_fixed_event_contract_and_adds_batch_id() {
        let payload = with_optional_batch_id(json!({ "transfer_id": "t-1" }), Some("batch-1"));
        assert_eq!(payload.get("transfer_id"), Some(&json!("t-1")));
        assert_eq!(payload.get("batch_id"), Some(&json!("batch-1")));
        assert_eq!(
            terminal_event_name(&TransferStatus::CancelledBySender),
            Some("transfer_cancelled_by_sender")
        );
    }
}
