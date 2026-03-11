use crate::state::{
    AppConfig, SecurityEvent, TransferDirection, TransferMetrics, TransferTask, TrustedPeer,
};
use crate::transport::protocol::SourceSnapshot;
use anyhow::Result;
use rusqlite::types::Type;
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tauri::{AppHandle, Manager};

fn parse_direction(raw: String) -> rusqlite::Result<TransferDirection> {
    match raw.as_str() {
        "Send" => Ok(TransferDirection::Send),
        "Receive" => Ok(TransferDirection::Receive),
        _ => Err(rusqlite::Error::FromSqlConversionFailure(
            1,
            Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid transfer direction: {raw}"),
            )),
        )),
    }
}

pub fn init_db(app: &AppHandle) -> Result<Connection> {
    let config_dir = std::env::var("DASHDROP_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            app.path()
                .app_config_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("dashdrop")
        });
    std::fs::create_dir_all(&config_dir).ok();
    let db_path = config_dir.join("history.db");

    let conn = Connection::open(&db_path)?;
    configure_sqlite_connection(&conn)?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS transfers_history (
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
    )?;

    // Runtime migration for older history table schemas.
    let _ = conn.execute(
        "ALTER TABLE transfers_history ADD COLUMN started_at INTEGER NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE transfers_history ADD COLUMN reason_code TEXT",
        [],
    );

    conn.execute(
        "CREATE TABLE IF NOT EXISTS security_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            event_type TEXT NOT NULL,
            occurred_at INTEGER NOT NULL,
            phase TEXT NOT NULL,
            peer_fingerprint TEXT,
            reason TEXT NOT NULL
        )",
        [],
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS transfer_resume_snapshots (
            transfer_id TEXT PRIMARY KEY,
            snapshots_json TEXT NOT NULL
        )",
        [],
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS app_config_store (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            config_json TEXT NOT NULL
        )",
        [],
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS trusted_peers_store (
            fingerprint TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            paired_at INTEGER NOT NULL,
            alias TEXT,
            last_used_at INTEGER
        )",
        [],
    )?;

    Ok(conn)
}

fn configure_sqlite_connection(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

pub fn load_app_config(conn: &Connection) -> Result<Option<AppConfig>> {
    let mut stmt = conn.prepare("SELECT config_json FROM app_config_store WHERE id = 1")?;
    let mut rows = stmt.query([])?;
    if let Some(row) = rows.next()? {
        let json: String = row.get(0)?;
        let config = serde_json::from_str::<AppConfig>(&json)?;
        Ok(Some(config))
    } else {
        Ok(None)
    }
}

pub fn load_trusted_peers(conn: &Connection) -> Result<Vec<TrustedPeer>> {
    let mut stmt = conn.prepare(
        "SELECT fingerprint, name, paired_at, alias, last_used_at
         FROM trusted_peers_store
         ORDER BY paired_at ASC",
    )?;
    let iter = stmt.query_map([], |row| {
        Ok(TrustedPeer {
            fingerprint: row.get(0)?,
            name: row.get(1)?,
            paired_at: row.get(2)?,
            alias: row.get(3)?,
            last_used_at: row.get(4)?,
        })
    })?;
    let mut out = Vec::new();
    for peer in iter {
        out.push(peer?);
    }
    Ok(out)
}

pub fn save_app_config(conn: &Connection, config: &AppConfig) -> Result<()> {
    let json = serde_json::to_string(config)?;
    conn.execute(
        "INSERT INTO app_config_store (id, config_json) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET config_json = excluded.config_json",
        params![json],
    )?;
    Ok(())
}

pub fn replace_trusted_peers(
    conn: &Connection,
    trusted: &HashMap<String, TrustedPeer>,
) -> Result<()> {
    conn.execute("DELETE FROM trusted_peers_store", [])?;
    for peer in trusted.values() {
        conn.execute(
            "INSERT INTO trusted_peers_store
             (fingerprint, name, paired_at, alias, last_used_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                peer.fingerprint,
                peer.name,
                peer.paired_at,
                peer.alias,
                peer.last_used_at
            ],
        )?;
    }
    Ok(())
}

pub fn save_transfer(conn: &Connection, t: &TransferTask) -> Result<()> {
    if !should_persist_transfer(conn, t)? {
        return Ok(());
    }

    let items_json = serde_json::to_string(&t.items)?;
    let ended_sys = t.ended_at_unix.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0)
    });

    conn.execute(
        "INSERT INTO transfers_history (
            id, direction, peer_fingerprint, peer_name, items, status,
            bytes_transferred, total_bytes, revision, started_at, ended_at, reason_code, error
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
        ON CONFLICT(id) DO UPDATE SET
            direction = excluded.direction,
            peer_fingerprint = excluded.peer_fingerprint,
            peer_name = excluded.peer_name,
            items = excluded.items,
            status = excluded.status,
            bytes_transferred = excluded.bytes_transferred,
            total_bytes = excluded.total_bytes,
            revision = excluded.revision,
            started_at = excluded.started_at,
            ended_at = excluded.ended_at,
            reason_code = excluded.reason_code,
            error = excluded.error",
        params![
            t.id,
            match t.direction {
                TransferDirection::Send => "Send",
                TransferDirection::Receive => "Receive",
            },
            t.peer_fingerprint,
            t.peer_name,
            items_json,
            serde_json::to_string(&t.status)?.trim_matches('"'),
            t.bytes_transferred,
            t.total_bytes,
            t.revision,
            t.started_at_unix,
            ended_sys,
            t.terminal_reason_code,
            t.error,
        ],
    )?;
    Ok(())
}

fn should_persist_transfer(conn: &Connection, task: &TransferTask) -> Result<bool> {
    let existing = conn.query_row(
        "SELECT status, bytes_transferred, revision
         FROM transfers_history
         WHERE id = ?1",
        params![task.id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u64>(1)?,
                row.get::<_, u64>(2)?,
            ))
        },
    );

    let Ok((existing_status, existing_bytes, existing_revision)) = existing else {
        return Ok(true);
    };

    if task.revision > existing_revision {
        return Ok(true);
    }
    if task.revision < existing_revision {
        return Ok(false);
    }

    let incoming_terminal = is_terminal_status(&task.status);
    let existing_terminal = is_terminal_status_str(&existing_status);
    if existing_terminal && !incoming_terminal {
        return Ok(false);
    }

    Ok(task.bytes_transferred >= existing_bytes)
}

fn is_terminal_status(status: &crate::state::TransferStatus) -> bool {
    matches!(
        status,
        crate::state::TransferStatus::Completed
            | crate::state::TransferStatus::PartialCompleted
            | crate::state::TransferStatus::Rejected
            | crate::state::TransferStatus::CancelledBySender
            | crate::state::TransferStatus::CancelledByReceiver
            | crate::state::TransferStatus::Failed
    )
}

fn is_terminal_status_str(status: &str) -> bool {
    matches!(
        status,
        "Completed"
            | "PartialCompleted"
            | "Rejected"
            | "CancelledBySender"
            | "CancelledByReceiver"
            | "Failed"
    )
}

pub fn get_history(conn: &Connection, limit: u32, offset: u32) -> Result<Vec<TransferTask>> {
    let mut stmt = conn.prepare(
        "SELECT id, direction, peer_fingerprint, peer_name, items, status, bytes_transferred,
                total_bytes, revision, started_at, ended_at, reason_code, error
         FROM transfers_history
         ORDER BY ended_at DESC
         LIMIT ?1 OFFSET ?2",
    )?;

    let transfer_iter = stmt.query_map(params![limit, offset], |row| {
        let items_str: String = row.get(4)?;
        let items = serde_json::from_str(&items_str)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(4, Type::Text, Box::new(e)))?;
        let status_str: String = row.get(5)?;
        let status = serde_json::from_str(&format!("\"{}\"", status_str))
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(5, Type::Text, Box::new(e)))?;

        Ok(TransferTask {
            id: row.get(0)?,
            batch_id: None,
            direction: parse_direction(row.get(1)?)?,
            peer_fingerprint: row.get(2)?,
            peer_name: row.get(3)?,
            items,
            status,
            bytes_transferred: row.get(6)?,
            total_bytes: row.get(7)?,
            revision: row.get(8)?,
            started_at_unix: row.get(9)?,
            ended_at_unix: Some(row.get(10)?),
            terminal_reason_code: row.get(11)?,
            error: row.get(12)?,
            source_paths: None,
            source_path_by_file_id: None,
            failed_file_ids: None,
            conn: None,
            ended_at: None,
        })
    })?;

    let mut tasks = Vec::new();
    for t in transfer_iter {
        tasks.push(t?);
    }
    Ok(tasks)
}

pub fn get_transfer_metrics(conn: &Connection) -> Result<TransferMetrics> {
    let mut metrics = TransferMetrics::default();

    let mut status_stmt =
        conn.prepare("SELECT status, COUNT(*) FROM transfers_history GROUP BY status")?;
    let status_iter = status_stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
    })?;
    for status in status_iter {
        let (status, count) = status?;
        match status.as_str() {
            "Completed" => metrics.completed = count,
            "PartialCompleted" => metrics.partial = count,
            "Failed" => metrics.failed = count,
            "CancelledBySender" => metrics.cancelled_by_sender = count,
            "CancelledByReceiver" => metrics.cancelled_by_receiver = count,
            "Rejected" => metrics.rejected = count,
            _ => {}
        }
    }

    metrics.bytes_sent = conn.query_row(
        "SELECT COALESCE(SUM(bytes_transferred), 0) FROM transfers_history WHERE direction = 'Send'",
        [],
        |row| row.get(0),
    )?;
    metrics.bytes_received = conn.query_row(
        "SELECT COALESCE(SUM(bytes_transferred), 0) FROM transfers_history WHERE direction = 'Receive'",
        [],
        |row| row.get(0),
    )?;

    let avg_duration_ms: Option<f64> = conn.query_row(
        "SELECT AVG((ended_at - started_at) * 1000.0)
         FROM transfers_history
         WHERE started_at > 0 AND ended_at >= started_at",
        [],
        |row| row.get(0),
    )?;
    metrics.average_duration_ms = avg_duration_ms.unwrap_or(0.0).round() as u64;

    let mut failure_stmt = conn.prepare(
        "SELECT COALESCE(reason_code, 'UNKNOWN') AS code, COUNT(*)
         FROM transfers_history
         WHERE status IN ('Failed', 'Rejected', 'CancelledBySender', 'CancelledByReceiver', 'PartialCompleted')
         GROUP BY COALESCE(reason_code, 'UNKNOWN')",
    )?;
    let failure_iter = failure_stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
    })?;
    for row in failure_iter {
        let (code, count) = row?;
        metrics.failure_distribution.insert(code, count);
    }

    Ok(metrics)
}

pub fn save_transfer_source_snapshots(
    conn: &Connection,
    transfer_id: &str,
    snapshots: &HashMap<u32, SourceSnapshot>,
) -> Result<()> {
    let snapshots_json = serde_json::to_string(snapshots)?;
    conn.execute(
        "INSERT INTO transfer_resume_snapshots (transfer_id, snapshots_json)
         VALUES (?1, ?2)
         ON CONFLICT(transfer_id) DO UPDATE SET snapshots_json = excluded.snapshots_json",
        params![transfer_id, snapshots_json],
    )?;
    Ok(())
}

pub fn load_transfer_source_snapshots(
    conn: &Connection,
    transfer_id: &str,
) -> Result<Option<HashMap<u32, SourceSnapshot>>> {
    let mut stmt = conn.prepare(
        "SELECT snapshots_json
         FROM transfer_resume_snapshots
         WHERE transfer_id = ?1",
    )?;
    let mut rows = stmt.query(params![transfer_id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let snapshots_json: String = row.get(0)?;
    let snapshots = serde_json::from_str(&snapshots_json)?;
    Ok(Some(snapshots))
}

pub fn log_security_event(
    conn: &Connection,
    event_type: &str,
    phase: &str,
    peer_fingerprint: Option<&str>,
    reason: &str,
) -> Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    conn.execute(
        "INSERT INTO security_events (event_type, occurred_at, phase, peer_fingerprint, reason)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![event_type, now, phase, peer_fingerprint, reason],
    )?;
    Ok(())
}

pub fn get_security_events(
    conn: &Connection,
    limit: u32,
    offset: u32,
) -> Result<Vec<SecurityEvent>> {
    let mut stmt = conn.prepare(
        "SELECT id, event_type, occurred_at, phase, peer_fingerprint, reason
         FROM security_events
         ORDER BY occurred_at DESC, id DESC
         LIMIT ?1 OFFSET ?2",
    )?;
    let iter = stmt.query_map(params![limit, offset], |row| {
        Ok(SecurityEvent {
            id: row.get(0)?,
            event_type: row.get(1)?,
            occurred_at_unix: row.get(2)?,
            phase: row.get(3)?,
            peer_fingerprint: row.get(4)?,
            reason: row.get(5)?,
        })
    })?;
    let mut out = Vec::new();
    for event in iter {
        out.push(event?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{
        configure_sqlite_connection, load_transfer_source_snapshots, parse_direction,
        save_transfer, save_transfer_source_snapshots,
    };
    use crate::state::{FileItemMeta, TransferDirection, TransferStatus, TransferTask};
    use crate::transport::protocol::SourceSnapshot;
    use rusqlite::Connection;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
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
        .expect("history table");
        conn.execute(
            "CREATE TABLE transfer_resume_snapshots (
                transfer_id TEXT PRIMARY KEY,
                snapshots_json TEXT NOT NULL
            )",
            [],
        )
        .expect("snapshot table");
        conn
    }

    fn test_transfer(id: &str, status: TransferStatus, bytes: u64, revision: u64) -> TransferTask {
        TransferTask {
            id: id.to_string(),
            batch_id: None,
            direction: TransferDirection::Send,
            peer_fingerprint: "peer-fp".to_string(),
            peer_name: "Peer".to_string(),
            items: vec![FileItemMeta {
                file_id: 1,
                name: "file.bin".to_string(),
                rel_path: "file.bin".to_string(),
                size: 64,
                risk_class: None,
            }],
            status,
            bytes_transferred: bytes,
            total_bytes: 64,
            revision,
            started_at_unix: 1,
            ended_at_unix: None,
            terminal_reason_code: None,
            error: None,
            source_paths: None,
            source_path_by_file_id: None,
            failed_file_ids: None,
            conn: None,
            ended_at: None,
        }
    }

    fn temp_db_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dashdrop-{name}-{}.db", uuid::Uuid::new_v4()))
    }

    #[test]
    fn parse_direction_rejects_invalid_value() {
        let err = parse_direction("CorruptedValue".to_string())
            .expect_err("must reject unknown direction");
        assert!(matches!(err, rusqlite::Error::FromSqlConversionFailure(..)));
    }

    #[test]
    fn parse_direction_accepts_known_values() {
        assert_eq!(
            parse_direction("Send".to_string()).expect("send direction"),
            TransferDirection::Send
        );
        assert_eq!(
            parse_direction("Receive".to_string()).expect("receive direction"),
            TransferDirection::Receive
        );
    }

    #[test]
    fn transfer_source_snapshots_round_trip() {
        let conn = setup_test_db();
        let mut snapshots = HashMap::new();
        snapshots.insert(
            1,
            SourceSnapshot {
                size: 12,
                mtime_unix_ms: 34,
                head_hash: [7u8; 32],
            },
        );

        save_transfer_source_snapshots(&conn, "transfer-1", &snapshots).expect("save snapshots");
        let loaded = load_transfer_source_snapshots(&conn, "transfer-1").expect("load snapshots");

        assert_eq!(loaded, Some(snapshots));
    }

    #[test]
    fn sqlite_connection_uses_wal_and_busy_timeout() {
        let path = temp_db_path("sqlite-config");
        let conn = Connection::open(&path).expect("open sqlite file");
        configure_sqlite_connection(&conn).expect("configure sqlite");

        let journal_mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .expect("journal mode");
        let busy_timeout_ms: u64 = conn
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .expect("busy timeout");

        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
        assert_eq!(busy_timeout_ms, 5_000);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn stale_progress_snapshot_cannot_overwrite_terminal_row() {
        let conn = setup_test_db();
        let terminal = test_transfer("transfer-1", TransferStatus::Completed, 64, 2);
        let stale_progress = test_transfer("transfer-1", TransferStatus::Transferring, 32, 1);

        save_transfer(&conn, &terminal).expect("save terminal");
        save_transfer(&conn, &stale_progress).expect("save stale progress");

        let (status, bytes, revision): (String, u64, u64) = conn
            .query_row(
                "SELECT status, bytes_transferred, revision
                 FROM transfers_history
                 WHERE id = ?1",
                ["transfer-1"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("load transfer row");

        assert_eq!(status, "Completed");
        assert_eq!(bytes, 64);
        assert_eq!(revision, 2);
    }
}
