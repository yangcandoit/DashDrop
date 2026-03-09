use crate::state::{SecurityEvent, TransferDirection, TransferTask};
use anyhow::Result;
use rusqlite::types::Type;
use rusqlite::{params, Connection};
use std::path::PathBuf;
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
            error TEXT,
            ended_at INTEGER NOT NULL
        )",
        [],
    )?;

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

    Ok(conn)
}

pub fn save_transfer(conn: &Connection, t: &TransferTask) -> Result<()> {
    let items_json = serde_json::to_string(&t.items)?;

    let ended_sys = t.ended_at_unix.unwrap_or_else(|| {
        match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(duration) => duration.as_secs(),
            Err(_) => 0,
        }
    });

    conn.execute(
        "INSERT OR REPLACE INTO transfers_history (
            id, direction, peer_fingerprint, peer_name, items, status,
            bytes_transferred, total_bytes, revision, error, ended_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
            t.error,
            ended_sys,
        ],
    )?;
    Ok(())
}

pub fn get_history(conn: &Connection, limit: u32, offset: u32) -> Result<Vec<TransferTask>> {
    let mut stmt = conn.prepare("SELECT id, direction, peer_fingerprint, peer_name, items, status, bytes_transferred, total_bytes, revision, error, ended_at FROM transfers_history ORDER BY ended_at DESC LIMIT ?1 OFFSET ?2")?;

    let transfer_iter = stmt.query_map(params![limit, offset], |row| {
        let items_str: String = row.get(4)?;
        let items = serde_json::from_str(&items_str)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(4, Type::Text, Box::new(e)))?;
        let status_str: String = row.get(5)?;
        let status = serde_json::from_str(&format!("\"{}\"", status_str))
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(5, Type::Text, Box::new(e)))?;
        Ok(TransferTask {
            id: row.get(0)?,
            direction: parse_direction(row.get(1)?)?,
            peer_fingerprint: row.get(2)?,
            peer_name: row.get(3)?,
            items,
            status,
            bytes_transferred: row.get(6)?,
            total_bytes: row.get(7)?,
            revision: row.get(8)?,
            error: row.get(9)?,
            ended_at_unix: Some(row.get(10)?),
            source_paths: None,
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
    use super::parse_direction;
    use crate::state::TransferDirection;

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
}
