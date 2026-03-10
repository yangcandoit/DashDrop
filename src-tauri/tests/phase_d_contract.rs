use std::collections::HashMap;

use dashdrop_lib::state::{DeviceInfo, Platform, ReachabilityStatus};

#[test]
fn cancel_semantics_stay_split() {
    assert_eq!(
        dashdrop_lib::transport::events::terminal_event_name(
            &dashdrop_lib::state::TransferStatus::CancelledBySender
        ),
        Some("transfer_cancelled_by_sender")
    );
    assert_eq!(
        dashdrop_lib::transport::events::terminal_event_name(
            &dashdrop_lib::state::TransferStatus::CancelledByReceiver
        ),
        Some("transfer_cancelled_by_receiver")
    );
}

#[test]
fn db_direction_invalid_value_returns_error() {
    let conn = rusqlite::Connection::open_in_memory().expect("open db");
    conn.execute(
        "CREATE TABLE transfers_history (
            id TEXT PRIMARY KEY,
            direction TEXT NOT NULL,
            peer_fingerprint TEXT NOT NULL,
            peer_name TEXT NOT NULL,
            items TEXT NOT NULL,
            status TEXT NOT NULL,
            bytes_transferred INTEGER NOT NULL,
            total_bytes INTEGER NOT NULL,
            revision INTEGER NOT NULL DEFAULT 0,
            started_at INTEGER NOT NULL DEFAULT 0,
            ended_at INTEGER NOT NULL,
            reason_code TEXT,
            error TEXT
        )",
        [],
    )
    .expect("create history table");
    conn.execute(
        "INSERT INTO transfers_history
            (id, direction, peer_fingerprint, peer_name, items, status, bytes_transferred, total_bytes, revision, started_at, ended_at, reason_code, error)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0, 0, 0, 1, NULL, NULL)",
        rusqlite::params![
            "bad-direction",
            "BROKEN_DIRECTION",
            "fp",
            "peer",
            "[]",
            "Completed"
        ],
    )
    .expect("insert row");

    let err =
        dashdrop_lib::db::get_history(&conn, 10, 0).expect_err("must fail on invalid direction");
    assert!(err.to_string().contains("invalid transfer direction"));
}

#[test]
fn trusted_only_auto_accept_branch() {
    assert!(dashdrop_lib::transport::receiver::should_auto_accept(
        true, true
    ));
    assert!(!dashdrop_lib::transport::receiver::should_auto_accept(
        true, false
    ));
    assert!(!dashdrop_lib::transport::receiver::should_auto_accept(
        false, true
    ));
}

#[test]
fn probe_state_transitions_to_offline_after_grace_period() {
    let device = DeviceInfo {
        fingerprint: "peer-fp".into(),
        name: "peer".into(),
        platform: Platform::Mac,
        trusted: false,
        sessions: HashMap::new(),
        last_seen: 100,
        reachability: ReachabilityStatus::OfflineCandidate,
        probe_fail_count: 1,
        last_probe_at: Some(100),
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
    };

    assert!(!dashdrop_lib::discovery::browser::should_mark_offline(
        &device, 114
    ));
    assert!(dashdrop_lib::discovery::browser::should_mark_offline(
        &device, 115
    ));
}

#[test]
fn connect_identity_mismatch_branch_detected() {
    assert!(dashdrop_lib::transport::handshake::is_identity_mismatch(
        "expected-fingerprint",
        "actual-fingerprint"
    ));
}

#[test]
fn offline_device_emits_lost_after_retention_window() {
    let device = DeviceInfo {
        fingerprint: "peer-fp".into(),
        name: "peer".into(),
        platform: Platform::Mac,
        trusted: false,
        sessions: HashMap::new(),
        last_seen: 100,
        reachability: ReachabilityStatus::Offline,
        probe_fail_count: 2,
        last_probe_at: Some(100),
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
    };
    assert!(!dashdrop_lib::discovery::browser::should_emit_device_lost(
        &device, 144
    ));
    assert!(dashdrop_lib::discovery::browser::should_emit_device_lost(
        &device, 145
    ));
}
