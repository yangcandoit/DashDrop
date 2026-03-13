use std::time::Duration;

use dashdrop_lib::state::{
    AppConfig, AppState, FileItemMeta, TransferDirection, TransferStatus, TransferTask,
};

fn build_state() -> AppState {
    AppState::new(
        dashdrop_lib::Identity {
            fingerprint: "self-fp".to_string(),
            cert_der: Vec::new(),
            key_der: Vec::new(),
            device_name: "DashDrop Test".to_string(),
        },
        AppConfig::default(),
        rusqlite::Connection::open_in_memory().expect("in-memory db"),
    )
}

#[tokio::test]
async fn first_observed_device_and_transfer_slo_timestamps_are_retained() {
    let state = build_state();

    state.record_device_visibility("peer-fp").await;
    state.record_sender_dispatch("transfer-1", "peer-fp").await;
    state
        .record_receiver_fallback_prompted("transfer-1", "peer-fp")
        .await;
    let first = state.slo_observability_snapshot().await;

    tokio::time::sleep(Duration::from_millis(2)).await;

    state.record_device_visibility("peer-fp").await;
    state.record_sender_dispatch("transfer-1", "peer-fp").await;
    state
        .record_receiver_fallback_prompted("transfer-1", "peer-fp")
        .await;
    let second = state.slo_observability_snapshot().await;

    assert_eq!(first.devices, second.devices);
    assert_eq!(first.transfers, second.transfers);
    assert!(second.devices["peer-fp"]
        .remote_peer_online_at
        .zip(second.devices["peer-fp"].local_device_visible_at)
        .is_some());
    assert_eq!(
        second.transfers["transfer-1"].peer_fingerprint.as_deref(),
        Some("peer-fp")
    );
    assert!(second.transfers["transfer-1"].sender_dispatch_at.is_some());
    assert!(second.transfers["transfer-1"]
        .receiver_fallback_prompted_at
        .is_some());
}

#[tokio::test]
async fn notification_activation_records_receiver_prompted_at() {
    let state = build_state();
    let transfer_id = "transfer-notify".to_string();
    state.transfers.write().await.insert(
        transfer_id.clone(),
        TransferTask {
            id: transfer_id.clone(),
            batch_id: None,
            direction: TransferDirection::Receive,
            peer_fingerprint: "peer-fp".into(),
            peer_name: "Peer".into(),
            items: vec![FileItemMeta {
                file_id: 1,
                name: "a.txt".into(),
                rel_path: "a.txt".into(),
                size: 1,
                hash: None,
                risk_class: None,
            }],
            status: TransferStatus::PendingAccept,
            bytes_transferred: 0,
            total_bytes: 1,
            revision: 0,
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

    let notification_id = state
        .ensure_incoming_request_notification(&transfer_id)
        .await;
    let first_prompted_at = state
        .slo_observability_snapshot()
        .await
        .transfers
        .get(&transfer_id)
        .and_then(|entry| entry.receiver_prompted_at)
        .expect("receiver prompted timestamp");

    tokio::time::sleep(Duration::from_millis(2)).await;

    let second_notification_id = state
        .ensure_incoming_request_notification(&transfer_id)
        .await;
    let snapshot = state.slo_observability_snapshot().await;

    assert_eq!(notification_id, second_notification_id);
    assert_eq!(
        snapshot.transfers[&transfer_id].peer_fingerprint.as_deref(),
        Some("peer-fp")
    );
    assert_eq!(
        snapshot.transfers[&transfer_id].receiver_prompted_at,
        Some(first_prompted_at)
    );
}
