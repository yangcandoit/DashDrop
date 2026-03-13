use crate::state::{
    AppConfig, RuntimeEventCheckpoint, RuntimeEventEnvelope, SecurityEvent, TransferDirection,
    TransferMetrics, TransferTask, TrustLevel, TrustVerificationMethod, TrustedPeer,
};
use crate::transport::protocol::SourceSnapshot;
use anyhow::Result;
use rusqlite::types::Type;
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;


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

pub fn db_path_for_config_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("history.db")
}

pub fn init_db_at(config_dir: &Path) -> Result<Connection> {
    std::fs::create_dir_all(config_dir).ok();
    let db_path = db_path_for_config_dir(config_dir);

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
            trust_level TEXT NOT NULL DEFAULT 'legacy_paired',
            last_verification_method TEXT NOT NULL DEFAULT 'manual_pairing',
            alias TEXT,
            last_used_at INTEGER,
            remote_confirmation_material_seen_at INTEGER,
            local_confirmation_at INTEGER,
            mutual_confirmed_at INTEGER,
            frozen_at INTEGER,
            freeze_reason TEXT
        )",
        [],
    )?;
    let _ = conn.execute(
        "ALTER TABLE trusted_peers_store ADD COLUMN trust_level TEXT NOT NULL DEFAULT 'legacy_paired'",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE trusted_peers_store ADD COLUMN last_verification_method TEXT NOT NULL DEFAULT 'manual_pairing'",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE trusted_peers_store ADD COLUMN remote_confirmation_material_seen_at INTEGER",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE trusted_peers_store ADD COLUMN local_confirmation_at INTEGER",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE trusted_peers_store ADD COLUMN mutual_confirmed_at INTEGER",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE trusted_peers_store ADD COLUMN frozen_at INTEGER",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE trusted_peers_store ADD COLUMN freeze_reason TEXT",
        [],
    );

    ensure_runtime_event_tables(&conn)?;

    Ok(conn)
}

pub fn migrate_trusted_peer_identity(
    conn: &Connection,
    old_fingerprint: &str,
    new_fingerprint: &str,
    new_name: &str,
) -> Result<()> {
    // 1. Load the old trust record
    let mut stmt = conn.prepare(
        "SELECT paired_at, trust_level, last_verification_method, alias, last_used_at, 
                remote_confirmation_material_seen_at, local_confirmation_at, mutual_confirmed_at 
         FROM trusted_peers_store WHERE fingerprint = ?"
    )?;
    
    let old_peer: Option<TrustedPeer> = stmt.query_row([old_fingerprint], |row| {
        Ok(TrustedPeer {
            fingerprint: old_fingerprint.to_string(),
            name: "".to_string(), // we'll use the new name
            paired_at: row.get(0)?,
            trust_level: match row.get::<_, String>(1)?.as_str() {
                "mutual_confirmed" => TrustLevel::MutualConfirmed,
                "signed_link_verified" => TrustLevel::SignedLinkVerified,
                "frozen" => TrustLevel::Frozen,
                _ => TrustLevel::LegacyPaired,
            },
            last_verification_method: match row.get::<_, String>(2)?.as_str() {
                "mutual_receipt" | "mutual_shared_code" => TrustVerificationMethod::MutualReceipt,
                "signed_pairing_link" | "signed_link" => TrustVerificationMethod::SignedPairingLink,
                "legacy_unsigned_link" => TrustVerificationMethod::LegacyUnsignedLink,
                _ => TrustVerificationMethod::ManualPairing,
            },
            alias: row.get(3)?,
            last_used_at: row.get(4)?,
            remote_confirmation_material_seen_at: row.get(5)?,
            local_confirmation_at: row.get(6)?,
            mutual_confirmed_at: row.get(7)?,
            frozen_at: None,
            freeze_reason: None,
        })
    }).ok();

    if let Some(mut peer) = old_peer {
        // 2. Perform the swap inside a transaction
        conn.execute("BEGIN TRANSACTION", [])?;
        
        // Remove old fingerprint
        conn.execute("DELETE FROM trusted_peers_store WHERE fingerprint = ?", [old_fingerprint])?;
        
        // Insert new fingerprint with inherited trust
        peer.fingerprint = new_fingerprint.to_string();
        peer.name = new_name.to_string();
        save_trusted_peer(conn, &peer)?;

        // Log the security event
        let reason =
            format!("Successfully migrated identity from old fingerprint: {}", old_fingerprint);
        log_security_event(
            conn,
            "identity_migrated",
            "pairing",
            Some(new_fingerprint),
            &reason,
        )?;

        conn.execute("COMMIT", [])?;
    } else {
        anyhow::bail!("Old fingerprint {} not found in trusted peers", old_fingerprint);
    }

    Ok(())
}
fn configure_sqlite_connection(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

fn ensure_runtime_event_tables(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS runtime_event_meta (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            generation TEXT NOT NULL,
            last_seq INTEGER NOT NULL
        )",
        [],
    )?;
    let _ = conn.execute(
        "ALTER TABLE runtime_event_meta ADD COLUMN compaction_watermark_seq INTEGER NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE runtime_event_meta ADD COLUMN compaction_watermark_segment_id INTEGER NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE runtime_event_meta ADD COLUMN last_compacted_at_unix_ms INTEGER",
        [],
    );
    conn.execute(
        "CREATE TABLE IF NOT EXISTS runtime_event_journal (
            seq INTEGER PRIMARY KEY,
            generation TEXT NOT NULL,
            envelope_json TEXT NOT NULL
        )",
        [],
    )?;
    let _ = conn.execute(
        "ALTER TABLE runtime_event_journal ADD COLUMN segment_id INTEGER NOT NULL DEFAULT 0",
        [],
    );
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_runtime_event_journal_segment_id
         ON runtime_event_journal(segment_id)",
        [],
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS runtime_event_segments (
            generation TEXT NOT NULL,
            segment_id INTEGER NOT NULL,
            first_seq INTEGER NOT NULL,
            last_seq INTEGER NOT NULL,
            event_count INTEGER NOT NULL,
            created_at_unix_ms INTEGER NOT NULL DEFAULT 0,
            sealed_at_unix_ms INTEGER,
            compacted_at_unix_ms INTEGER,
            PRIMARY KEY (generation, segment_id)
        )",
        [],
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS runtime_event_checkpoints (
            consumer_id TEXT PRIMARY KEY,
            generation TEXT NOT NULL,
            seq INTEGER NOT NULL,
            updated_at_unix_ms INTEGER NOT NULL
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_runtime_event_checkpoints_generation_updated_seq
         ON runtime_event_checkpoints(generation, updated_at_unix_ms DESC, seq ASC)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_runtime_event_checkpoints_generation_recovery_seq
         ON runtime_event_checkpoints(generation, recovery_hint, seq ASC)",
        [],
    )
    .or_else(|_| {
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_runtime_event_checkpoints_generation_seq
             ON runtime_event_checkpoints(generation, seq ASC)",
            [],
        )
    })?;
    let _ = conn.execute(
        "ALTER TABLE runtime_event_checkpoints ADD COLUMN created_at_unix_ms INTEGER",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE runtime_event_checkpoints ADD COLUMN last_read_at_unix_ms INTEGER",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE runtime_event_checkpoints ADD COLUMN lease_expires_at_unix_ms INTEGER",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE runtime_event_checkpoints ADD COLUMN revision INTEGER",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE runtime_event_checkpoints ADD COLUMN last_transition TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE runtime_event_checkpoints ADD COLUMN recovery_hint TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE runtime_event_checkpoints ADD COLUMN current_oldest_available_seq INTEGER",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE runtime_event_checkpoints ADD COLUMN current_latest_available_seq INTEGER",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE runtime_event_checkpoints ADD COLUMN current_compaction_watermark_seq INTEGER",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE runtime_event_checkpoints ADD COLUMN current_compaction_watermark_segment_id INTEGER",
        [],
    );
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_runtime_event_checkpoints_generation_recovery_seq
         ON runtime_event_checkpoints(generation, recovery_hint, seq ASC)",
        [],
    )?;

    let existing_meta_count: u64 = conn.query_row(
        "SELECT COUNT(*) FROM runtime_event_meta WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    if existing_meta_count == 0 {
        conn.execute(
            "INSERT INTO runtime_event_meta (id, generation, last_seq) VALUES (1, ?1, 0)",
            params![uuid::Uuid::new_v4().to_string()],
        )?;
    }
    conn.execute(
        "UPDATE runtime_event_journal
         SET segment_id = ((seq - 1) / ?1) + 1
         WHERE seq > 0 AND segment_id = 0",
        params![crate::state::RUNTIME_EVENT_PERSISTED_SEGMENT_SIZE as u64],
    )?;
    conn.execute(
        "INSERT INTO runtime_event_segments
         (generation, segment_id, first_seq, last_seq, event_count, created_at_unix_ms)
         SELECT generation, segment_id, MIN(seq), MAX(seq), COUNT(*), 0
         FROM runtime_event_journal
         GROUP BY generation, segment_id
         ON CONFLICT(generation, segment_id) DO UPDATE SET
             first_seq = MIN(runtime_event_segments.first_seq, excluded.first_seq),
             last_seq = MAX(runtime_event_segments.last_seq, excluded.last_seq),
             event_count = MAX(runtime_event_segments.event_count, excluded.event_count)",
        [],
    )?;

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeEventJournalStats {
    pub generation: String,
    pub active_segment_count: u64,
    pub oldest_active_segment_id: Option<u64>,
    pub latest_active_segment_id: Option<u64>,
    pub compacted_segment_count: u64,
    pub latest_compacted_segment_id: Option<u64>,
    pub compaction_watermark_seq: u64,
    pub compaction_watermark_segment_id: u64,
    pub last_compacted_at_unix_ms: Option<u64>,
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

pub fn load_runtime_event_state(
    conn: &Connection,
    limit: usize,
) -> Result<(String, Option<u64>, u64, Vec<RuntimeEventEnvelope>)> {
    ensure_runtime_event_tables(conn)?;

    let (generation, last_seq): (String, u64) = conn.query_row(
        "SELECT generation, last_seq FROM runtime_event_meta WHERE id = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let oldest_seq = conn.query_row(
        "SELECT MIN(seq) FROM runtime_event_journal WHERE generation = ?1",
        params![generation.clone()],
        |row| row.get::<_, Option<u64>>(0),
    )?;

    let mut stmt = conn.prepare(
        "SELECT envelope_json
         FROM runtime_event_journal
         WHERE generation = ?2
         ORDER BY seq DESC
         LIMIT ?1",
    )?;
    let iter = stmt.query_map(params![limit as u64, generation], |row| {
        row.get::<_, String>(0)
    })?;
    let mut events = Vec::new();
    for item in iter {
        let envelope_json = item?;
        events.push(serde_json::from_str::<RuntimeEventEnvelope>(
            &envelope_json,
        )?);
    }
    events.reverse();

    Ok((generation, oldest_seq, last_seq, events))
}

pub fn load_runtime_events_after(
    conn: &Connection,
    after_seq: u64,
    limit: usize,
) -> Result<Vec<RuntimeEventEnvelope>> {
    ensure_runtime_event_tables(conn)?;
    let generation: String = conn.query_row(
        "SELECT generation FROM runtime_event_meta WHERE id = 1",
        [],
        |row| row.get(0),
    )?;

    let mut stmt = conn.prepare(
        "SELECT envelope_json
         FROM runtime_event_journal
         WHERE seq > ?1 AND generation = ?2
         ORDER BY seq ASC
         LIMIT ?3",
    )?;
    let iter = stmt.query_map(params![after_seq, generation, limit as u64], |row| {
        row.get::<_, String>(0)
    })?;
    let mut events = Vec::new();
    for item in iter {
        let envelope_json = item?;
        events.push(serde_json::from_str::<RuntimeEventEnvelope>(
            &envelope_json,
        )?);
    }

    Ok(events)
}

pub fn load_runtime_event_journal_stats(conn: &Connection) -> Result<RuntimeEventJournalStats> {
    ensure_runtime_event_tables(conn)?;
    let (generation, compaction_watermark_seq, compaction_watermark_segment_id, last_compacted_at_unix_ms): (
        String,
        u64,
        u64,
        Option<u64>,
    ) = conn.query_row(
        "SELECT generation, compaction_watermark_seq, compaction_watermark_segment_id, last_compacted_at_unix_ms
         FROM runtime_event_meta
         WHERE id = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let (active_segment_count, oldest_active_segment_id, latest_active_segment_id): (
        u64,
        Option<u64>,
        Option<u64>,
    ) = conn.query_row(
        "SELECT COUNT(*), MIN(segment_id), MAX(segment_id)
         FROM runtime_event_segments
         WHERE generation = ?1 AND compacted_at_unix_ms IS NULL",
        params![generation.clone()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let (compacted_segment_count, latest_compacted_segment_id): (u64, Option<u64>) = conn
        .query_row(
            "SELECT COUNT(*), MAX(segment_id)
             FROM runtime_event_segments
             WHERE generation = ?1 AND compacted_at_unix_ms IS NOT NULL",
            params![generation.clone()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

    Ok(RuntimeEventJournalStats {
        generation,
        active_segment_count,
        oldest_active_segment_id,
        latest_active_segment_id,
        compacted_segment_count,
        latest_compacted_segment_id,
        compaction_watermark_seq,
        compaction_watermark_segment_id,
        last_compacted_at_unix_ms,
    })
}

pub fn load_runtime_event_checkpoint(
    conn: &Connection,
    consumer_id: &str,
) -> Result<Option<RuntimeEventCheckpoint>> {
    ensure_runtime_event_tables(conn)?;

    let mut stmt = conn.prepare(
        "SELECT consumer_id, generation, seq, updated_at_unix_ms,
                created_at_unix_ms, last_read_at_unix_ms, lease_expires_at_unix_ms, revision,
                last_transition, recovery_hint, current_oldest_available_seq,
                current_latest_available_seq, current_compaction_watermark_seq,
                current_compaction_watermark_segment_id
         FROM runtime_event_checkpoints
         WHERE consumer_id = ?1",
    )?;
    let mut rows = stmt.query(params![consumer_id])?;
    if let Some(row) = rows.next()? {
        Ok(Some(RuntimeEventCheckpoint {
            consumer_id: row.get(0)?,
            generation: row.get(1)?,
            seq: row.get(2)?,
            updated_at_unix_ms: row.get(3)?,
            created_at_unix_ms: row.get(4)?,
            last_read_at_unix_ms: row.get(5)?,
            lease_expires_at_unix_ms: row.get(6)?,
            revision: row.get(7)?,
            last_transition: row.get(8)?,
            recovery_hint: row.get(9)?,
            current_oldest_available_seq: row.get(10)?,
            current_latest_available_seq: row.get(11)?,
            current_compaction_watermark_seq: row.get(12)?,
            current_compaction_watermark_segment_id: row.get(13)?,
        }))
    } else {
        Ok(None)
    }
}

pub fn save_runtime_event_checkpoint(
    conn: &Connection,
    checkpoint: &RuntimeEventCheckpoint,
) -> Result<()> {
    ensure_runtime_event_tables(conn)?;
    conn.execute(
        "INSERT INTO runtime_event_checkpoints (
             consumer_id,
             generation,
             seq,
             updated_at_unix_ms,
             created_at_unix_ms,
             last_read_at_unix_ms,
             lease_expires_at_unix_ms,
             revision,
             last_transition,
             recovery_hint,
             current_oldest_available_seq,
             current_latest_available_seq,
             current_compaction_watermark_seq,
             current_compaction_watermark_segment_id
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(consumer_id) DO UPDATE SET
             generation = excluded.generation,
             seq = excluded.seq,
             updated_at_unix_ms = excluded.updated_at_unix_ms,
             created_at_unix_ms = excluded.created_at_unix_ms,
             last_read_at_unix_ms = excluded.last_read_at_unix_ms,
             lease_expires_at_unix_ms = excluded.lease_expires_at_unix_ms,
             revision = excluded.revision,
             last_transition = excluded.last_transition,
             recovery_hint = excluded.recovery_hint,
             current_oldest_available_seq = excluded.current_oldest_available_seq,
             current_latest_available_seq = excluded.current_latest_available_seq,
             current_compaction_watermark_seq = excluded.current_compaction_watermark_seq,
             current_compaction_watermark_segment_id = excluded.current_compaction_watermark_segment_id",
        params![
            checkpoint.consumer_id,
            checkpoint.generation,
            checkpoint.seq,
            checkpoint.updated_at_unix_ms,
            checkpoint.created_at_unix_ms,
            checkpoint.last_read_at_unix_ms,
            checkpoint.lease_expires_at_unix_ms,
            checkpoint.revision,
            checkpoint.last_transition,
            checkpoint.recovery_hint,
            checkpoint.current_oldest_available_seq,
            checkpoint.current_latest_available_seq,
            checkpoint.current_compaction_watermark_seq,
            checkpoint.current_compaction_watermark_segment_id
        ],
    )?;
    Ok(())
}

pub fn delete_runtime_event_checkpoint(conn: &Connection, consumer_id: &str) -> Result<()> {
    ensure_runtime_event_tables(conn)?;
    conn.execute(
        "DELETE FROM runtime_event_checkpoints WHERE consumer_id = ?1",
        params![consumer_id],
    )?;
    Ok(())
}

pub fn prune_runtime_event_checkpoints(
    conn: &Connection,
    stale_before_unix_ms: u64,
) -> Result<usize> {
    ensure_runtime_event_tables(conn)?;
    let removed = conn.execute(
        "DELETE FROM runtime_event_checkpoints WHERE updated_at_unix_ms <= ?1",
        params![stale_before_unix_ms],
    )?;
    Ok(removed)
}

pub fn list_runtime_event_checkpoints(conn: &Connection) -> Result<Vec<RuntimeEventCheckpoint>> {
    ensure_runtime_event_tables(conn)?;
    let mut stmt = conn.prepare(
        "SELECT consumer_id, generation, seq, updated_at_unix_ms,
                created_at_unix_ms, last_read_at_unix_ms, lease_expires_at_unix_ms, revision,
                last_transition, recovery_hint, current_oldest_available_seq,
                current_latest_available_seq, current_compaction_watermark_seq,
                current_compaction_watermark_segment_id
         FROM runtime_event_checkpoints
         ORDER BY updated_at_unix_ms DESC, consumer_id ASC",
    )?;
    let iter = stmt.query_map([], |row| {
        Ok(RuntimeEventCheckpoint {
            consumer_id: row.get(0)?,
            generation: row.get(1)?,
            seq: row.get(2)?,
            updated_at_unix_ms: row.get(3)?,
            created_at_unix_ms: row.get(4)?,
            last_read_at_unix_ms: row.get(5)?,
            lease_expires_at_unix_ms: row.get(6)?,
            revision: row.get(7)?,
            last_transition: row.get(8)?,
            recovery_hint: row.get(9)?,
            current_oldest_available_seq: row.get(10)?,
            current_latest_available_seq: row.get(11)?,
            current_compaction_watermark_seq: row.get(12)?,
            current_compaction_watermark_segment_id: row.get(13)?,
        })
    })?;
    let mut checkpoints = Vec::new();
    for item in iter {
        checkpoints.push(item?);
    }
    Ok(checkpoints)
}

pub fn append_runtime_event(
    conn: &Connection,
    generation: &str,
    event: &RuntimeEventEnvelope,
    keep_latest: usize,
    max_keep_latest: usize,
    checkpoint_active_after_unix_ms: u64,
) -> Result<Option<u64>> {
    ensure_runtime_event_tables(conn)?;
    let segment_size = crate::state::RUNTIME_EVENT_PERSISTED_SEGMENT_SIZE as u64;
    let segment_id = ((event.seq.saturating_sub(1)) / segment_size) + 1;

    conn.execute(
        "INSERT OR REPLACE INTO runtime_event_journal (seq, generation, envelope_json, segment_id)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            event.seq,
            generation,
            serde_json::to_string(event)?,
            segment_id
        ],
    )?;
    conn.execute(
        "UPDATE runtime_event_meta
         SET generation = ?1, last_seq = ?2
         WHERE id = 1",
        params![generation, event.seq],
    )?;
    conn.execute(
        "INSERT INTO runtime_event_segments
         (generation, segment_id, first_seq, last_seq, event_count, created_at_unix_ms)
         VALUES (?1, ?2, ?3, ?4, 1, ?5)
         ON CONFLICT(generation, segment_id) DO UPDATE SET
             last_seq = MAX(runtime_event_segments.last_seq, excluded.last_seq),
             event_count = MAX(runtime_event_segments.event_count, excluded.last_seq - runtime_event_segments.first_seq + 1)",
        params![
            generation,
            segment_id,
            event.seq,
            event.seq,
            event.emitted_at_unix_ms
        ],
    )?;
    conn.execute(
        "UPDATE runtime_event_segments
         SET sealed_at_unix_ms = COALESCE(sealed_at_unix_ms, ?3)
         WHERE generation = ?1
           AND segment_id < ?2
           AND compacted_at_unix_ms IS NULL
           AND sealed_at_unix_ms IS NULL",
        params![generation, segment_id, event.emitted_at_unix_ms],
    )?;

    if max_keep_latest == 0 {
        conn.execute("DELETE FROM runtime_event_journal", [])?;
        conn.execute(
            "UPDATE runtime_event_segments
             SET compacted_at_unix_ms = COALESCE(compacted_at_unix_ms, ?2)
             WHERE generation = ?1 AND compacted_at_unix_ms IS NULL",
            params![generation, event.emitted_at_unix_ms],
        )?;
    } else {
        let baseline_retained_from_seq = if keep_latest == 0 {
            event.seq.saturating_add(1)
        } else {
            event.seq.saturating_sub(keep_latest as u64 - 1).max(1)
        };
        let max_retained_from_seq = event.seq.saturating_sub(max_keep_latest as u64 - 1).max(1);
        let oldest_pinned_checkpoint_seq = conn.query_row(
            "SELECT MIN(seq)
             FROM runtime_event_checkpoints
             WHERE generation = ?1
               AND updated_at_unix_ms > ?2
               AND seq >= ?3
               AND COALESCE(recovery_hint, 'persisted_catch_up') != 'resync_required'",
            params![
                generation,
                checkpoint_active_after_unix_ms,
                max_retained_from_seq.saturating_sub(1)
            ],
            |row| row.get::<_, Option<u64>>(0),
        )?;
        let protected_retained_from_seq = oldest_pinned_checkpoint_seq
            .map(|seq| seq.saturating_add(1).max(1))
            .unwrap_or(baseline_retained_from_seq);
        let retained_from_seq = std::cmp::max(
            max_retained_from_seq,
            std::cmp::min(baseline_retained_from_seq, protected_retained_from_seq),
        );
        let retained_from_segment_id = ((retained_from_seq.saturating_sub(1)) / segment_size) + 1;
        conn.execute(
            "UPDATE runtime_event_segments
             SET compacted_at_unix_ms = COALESCE(compacted_at_unix_ms, ?3)
             WHERE generation = ?1
               AND segment_id < ?2
               AND compacted_at_unix_ms IS NULL",
            params![
                generation,
                retained_from_segment_id,
                event.emitted_at_unix_ms
            ],
        )?;
        conn.execute(
            "DELETE FROM runtime_event_journal WHERE generation = ?1 AND segment_id < ?2",
            params![generation, retained_from_segment_id],
        )?;
    }

    let (compaction_watermark_seq, compaction_watermark_segment_id, last_compacted_at_unix_ms): (
        u64,
        u64,
        Option<u64>,
    ) = conn.query_row(
        "SELECT COALESCE(MAX(last_seq), 0),
                COALESCE(MAX(segment_id), 0),
                MAX(compacted_at_unix_ms)
         FROM runtime_event_segments
         WHERE generation = ?1
           AND compacted_at_unix_ms IS NOT NULL",
        params![generation],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    conn.execute(
        "UPDATE runtime_event_meta
         SET compaction_watermark_seq = ?1,
             compaction_watermark_segment_id = ?2,
             last_compacted_at_unix_ms = ?3
         WHERE id = 1",
        params![
            compaction_watermark_seq,
            compaction_watermark_segment_id,
            last_compacted_at_unix_ms
        ],
    )?;

    let oldest_retained_seq = conn.query_row(
        "SELECT MIN(seq) FROM runtime_event_journal WHERE generation = ?1",
        params![generation],
        |row| row.get::<_, Option<u64>>(0),
    )?;

    Ok(oldest_retained_seq)
}

pub fn load_trusted_peers(conn: &Connection) -> Result<Vec<TrustedPeer>> {
    let mut stmt = conn.prepare(
        "SELECT fingerprint, name, paired_at, trust_level, last_verification_method, alias, last_used_at,
                remote_confirmation_material_seen_at, local_confirmation_at, mutual_confirmed_at,
                frozen_at, freeze_reason
         FROM trusted_peers_store
         ORDER BY paired_at ASC",
    )?;
    let iter = stmt.query_map([], |row| {
        let trust_level = match row.get::<_, String>(3)?.as_str() {
            "signed_link_verified" => TrustLevel::SignedLinkVerified,
            "mutual_confirmed" => TrustLevel::MutualConfirmed,
            "frozen" => TrustLevel::Frozen,
            _ => TrustLevel::LegacyPaired,
        };
        let last_verification_method = match row.get::<_, String>(4)?.as_str() {
            "legacy_unsigned_link" => TrustVerificationMethod::LegacyUnsignedLink,
            "signed_pairing_link" => TrustVerificationMethod::SignedPairingLink,
            "mutual_receipt" => TrustVerificationMethod::MutualReceipt,
            _ => TrustVerificationMethod::ManualPairing,
        };
        Ok(TrustedPeer {
            fingerprint: row.get(0)?,
            name: row.get(1)?,
            paired_at: row.get(2)?,
            trust_level,
            last_verification_method,
            alias: row.get(5)?,
            last_used_at: row.get(6)?,
            remote_confirmation_material_seen_at: row.get(7)?,
            local_confirmation_at: row.get(8)?,
            mutual_confirmed_at: row.get(9)?,
            frozen_at: row.get(10)?,
            freeze_reason: row.get(11)?,
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
             (fingerprint, name, paired_at, trust_level, last_verification_method, alias, last_used_at,
              remote_confirmation_material_seen_at, local_confirmation_at, mutual_confirmed_at, frozen_at, freeze_reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                peer.fingerprint,
                peer.name,
                peer.paired_at,
                serde_json::to_string(&peer.trust_level)?.trim_matches('"').to_string(),
                serde_json::to_string(&peer.last_verification_method)?
                    .trim_matches('"')
                    .to_string(),
                peer.alias,
                peer.last_used_at,
                peer.remote_confirmation_material_seen_at,
                peer.local_confirmation_at,
                peer.mutual_confirmed_at,
                peer.frozen_at,
                peer.freeze_reason
            ],
        )?;
    }
    Ok(())
}

pub fn save_trusted_peer(conn: &Connection, peer: &TrustedPeer) -> Result<()> {
    conn.execute(
        "INSERT INTO trusted_peers_store
         (fingerprint, name, paired_at, trust_level, last_verification_method, alias, last_used_at,
          remote_confirmation_material_seen_at, local_confirmation_at, mutual_confirmed_at, frozen_at, freeze_reason)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(fingerprint) DO UPDATE SET
             name = excluded.name,
             paired_at = excluded.paired_at,
             trust_level = excluded.trust_level,
             last_verification_method = excluded.last_verification_method,
             alias = excluded.alias,
             last_used_at = excluded.last_used_at,
             remote_confirmation_material_seen_at = excluded.remote_confirmation_material_seen_at,
             local_confirmation_at = excluded.local_confirmation_at,
             mutual_confirmed_at = excluded.mutual_confirmed_at,
             frozen_at = excluded.frozen_at,
             freeze_reason = excluded.freeze_reason",
        params![
            peer.fingerprint,
            peer.name,
            peer.paired_at,
            serde_json::to_string(&peer.trust_level)?.trim_matches('"').to_string(),
            serde_json::to_string(&peer.last_verification_method)?
                .trim_matches('"')
                .to_string(),
            peer.alias,
            peer.last_used_at,
            peer.remote_confirmation_material_seen_at,
            peer.local_confirmation_at,
            peer.mutual_confirmed_at,
            peer.frozen_at,
            peer.freeze_reason
        ],
    )?;
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
        append_runtime_event, configure_sqlite_connection, load_runtime_event_checkpoint,
        load_runtime_event_journal_stats, load_runtime_event_state, load_runtime_events_after,
        load_transfer_source_snapshots, load_trusted_peers, parse_direction,
        prune_runtime_event_checkpoints, replace_trusted_peers, save_runtime_event_checkpoint,
        save_transfer, save_transfer_source_snapshots,
    };
    use crate::state::{
        FileItemMeta, RuntimeEventCheckpoint, RuntimeEventEnvelope, TransferDirection,
        TransferStatus, TransferTask, TrustLevel, TrustVerificationMethod, TrustedPeer,
    };
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
                hash: None,
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
    fn trusted_peer_freeze_fields_round_trip() {
        let config_dir =
            std::env::temp_dir().join(format!("dashdrop-trusted-freeze-{}", uuid::Uuid::new_v4()));
        let conn = super::init_db_at(&config_dir).expect("init db");
        let trusted = HashMap::from([(
            "peer-fp".to_string(),
            TrustedPeer {
                fingerprint: "peer-fp".to_string(),
                name: "Peer".to_string(),
                paired_at: 1,
                trust_level: TrustLevel::Frozen,
                last_verification_method: TrustVerificationMethod::MutualReceipt,
                alias: Some("Laptop".to_string()),
                last_used_at: Some(2),
                remote_confirmation_material_seen_at: Some(3),
                local_confirmation_at: Some(4),
                mutual_confirmed_at: Some(5),
                frozen_at: Some(6),
                freeze_reason: Some("fingerprint changed".to_string()),
            },
        )]);

        replace_trusted_peers(&conn, &trusted).expect("save trusted peers");
        let loaded = load_trusted_peers(&conn).expect("load trusted peers");

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].trust_level, TrustLevel::Frozen);
        assert_eq!(loaded[0].frozen_at, Some(6));
        assert_eq!(
            loaded[0].freeze_reason.as_deref(),
            Some("fingerprint changed")
        );

        let _ = std::fs::remove_dir_all(config_dir);
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
    fn runtime_event_journal_round_trips_and_trims_to_latest_window() {
        let config_dir =
            std::env::temp_dir().join(format!("dashdrop-runtime-events-{}", uuid::Uuid::new_v4()));
        let conn = super::init_db_at(&config_dir).expect("init db");
        let segment_size = crate::state::RUNTIME_EVENT_PERSISTED_SEGMENT_SIZE as u64;

        for seq in 1..=(segment_size * 2 + 1) {
            append_runtime_event(
                &conn,
                "gen-1",
                &RuntimeEventEnvelope {
                    seq,
                    event: "device_updated".into(),
                    payload: serde_json::json!({ "index": seq }),
                    emitted_at_unix_ms: seq * 10,
                },
                segment_size as usize,
                segment_size as usize,
                0,
            )
            .expect("append runtime event");
        }

        let (generation, oldest_seq, last_seq, events) =
            load_runtime_event_state(&conn, segment_size as usize).expect("load runtime events");
        assert_eq!(generation, "gen-1");
        assert_eq!(oldest_seq, Some(segment_size + 1));
        assert_eq!(last_seq, segment_size * 2 + 1);
        assert_eq!(events.len(), segment_size as usize);
        assert_eq!(events[0].seq, segment_size + 2);
        assert_eq!(
            events.last().map(|event| event.seq),
            Some(segment_size * 2 + 1)
        );

        let _ = std::fs::remove_dir_all(config_dir);
    }

    #[test]
    fn runtime_event_journal_loads_events_after_cursor() {
        let config_dir =
            std::env::temp_dir().join(format!("dashdrop-runtime-after-{}", uuid::Uuid::new_v4()));
        let conn = super::init_db_at(&config_dir).expect("init db");

        for seq in 1..=4 {
            append_runtime_event(
                &conn,
                "gen-1",
                &RuntimeEventEnvelope {
                    seq,
                    event: "device_updated".into(),
                    payload: serde_json::json!({ "index": seq }),
                    emitted_at_unix_ms: seq * 10,
                },
                10,
                10,
                0,
            )
            .expect("append runtime event");
        }

        let events = load_runtime_events_after(&conn, 2, 10).expect("load events after cursor");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 3);
        assert_eq!(events[1].seq, 4);

        let _ = std::fs::remove_dir_all(config_dir);
    }

    #[test]
    fn runtime_event_checkpoint_round_trips() {
        let config_dir = std::env::temp_dir().join(format!(
            "dashdrop-runtime-checkpoint-{}",
            uuid::Uuid::new_v4()
        ));
        let conn = super::init_db_at(&config_dir).expect("init db");

        save_runtime_event_checkpoint(
            &conn,
            &RuntimeEventCheckpoint {
                consumer_id: "shared-ui".into(),
                generation: "gen-1".into(),
                seq: 42,
                updated_at_unix_ms: 1234,
                created_at_unix_ms: Some(1200),
                last_read_at_unix_ms: Some(1235),
                lease_expires_at_unix_ms: Some(1334),
                revision: Some(7),
                last_transition: Some("advanced".into()),
                recovery_hint: Some("hot_window".into()),
                current_oldest_available_seq: Some(1),
                current_latest_available_seq: Some(42),
                current_compaction_watermark_seq: Some(0),
                current_compaction_watermark_segment_id: Some(0),
            },
        )
        .expect("save checkpoint");

        let checkpoint =
            load_runtime_event_checkpoint(&conn, "shared-ui").expect("load checkpoint");
        assert_eq!(
            checkpoint,
            Some(RuntimeEventCheckpoint {
                consumer_id: "shared-ui".into(),
                generation: "gen-1".into(),
                seq: 42,
                updated_at_unix_ms: 1234,
                created_at_unix_ms: Some(1200),
                last_read_at_unix_ms: Some(1235),
                lease_expires_at_unix_ms: Some(1334),
                revision: Some(7),
                last_transition: Some("advanced".into()),
                recovery_hint: Some("hot_window".into()),
                current_oldest_available_seq: Some(1),
                current_latest_available_seq: Some(42),
                current_compaction_watermark_seq: Some(0),
                current_compaction_watermark_segment_id: Some(0),
            })
        );

        let _ = std::fs::remove_dir_all(config_dir);
    }

    #[test]
    fn runtime_event_checkpoint_prune_removes_expired_rows() {
        let config_dir = std::env::temp_dir().join(format!(
            "dashdrop-runtime-checkpoint-prune-{}",
            uuid::Uuid::new_v4()
        ));
        let conn = super::init_db_at(&config_dir).expect("init db");

        save_runtime_event_checkpoint(
            &conn,
            &RuntimeEventCheckpoint {
                consumer_id: "shared-ui".into(),
                generation: "gen-1".into(),
                seq: 42,
                updated_at_unix_ms: 100,
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
        .expect("save checkpoint");

        let removed = prune_runtime_event_checkpoints(&conn, 100).expect("prune checkpoints");
        assert_eq!(removed, 1);
        assert_eq!(
            load_runtime_event_checkpoint(&conn, "shared-ui").expect("load checkpoint"),
            None
        );

        let _ = std::fs::remove_dir_all(config_dir);
    }

    #[test]
    fn runtime_event_checkpoint_migrates_legacy_rows_with_nullable_metadata() {
        let conn = Connection::open_in_memory().expect("in-memory db");
        configure_sqlite_connection(&conn).expect("configure sqlite");
        conn.execute(
            "CREATE TABLE runtime_event_checkpoints (
                consumer_id TEXT PRIMARY KEY,
                generation TEXT NOT NULL,
                seq INTEGER NOT NULL,
                updated_at_unix_ms INTEGER NOT NULL
            )",
            [],
        )
        .expect("create legacy checkpoint table");
        conn.execute(
            "INSERT INTO runtime_event_checkpoints (consumer_id, generation, seq, updated_at_unix_ms)
             VALUES ('shared-ui', 'gen-1', 42, 1234)",
            [],
        )
        .expect("insert legacy checkpoint");

        let checkpoint =
            load_runtime_event_checkpoint(&conn, "shared-ui").expect("load migrated checkpoint");
        assert_eq!(
            checkpoint,
            Some(RuntimeEventCheckpoint {
                consumer_id: "shared-ui".into(),
                generation: "gen-1".into(),
                seq: 42,
                updated_at_unix_ms: 1234,
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
            })
        );
    }

    #[test]
    fn runtime_event_checkpoint_tables_install_retention_indexes() {
        let config_dir = std::env::temp_dir().join(format!(
            "dashdrop-runtime-checkpoint-indexes-{}",
            uuid::Uuid::new_v4()
        ));
        let conn = super::init_db_at(&config_dir).expect("init db");

        let mut stmt = conn
            .prepare("PRAGMA index_list('runtime_event_checkpoints')")
            .expect("prepare index_list");
        let iter = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query index_list");
        let mut index_names = Vec::new();
        for item in iter {
            index_names.push(item.expect("index row"));
        }

        assert!(index_names
            .contains(&"idx_runtime_event_checkpoints_generation_updated_seq".to_string()));
        assert!(index_names
            .contains(&"idx_runtime_event_checkpoints_generation_recovery_seq".to_string()));

        let _ = std::fs::remove_dir_all(config_dir);
    }

    #[test]
    fn runtime_event_journal_extends_retention_for_recent_checkpoint() {
        let config_dir = std::env::temp_dir().join(format!(
            "dashdrop-runtime-retention-pin-{}",
            uuid::Uuid::new_v4()
        ));
        let conn = super::init_db_at(&config_dir).expect("init db");
        let segment_size = crate::state::RUNTIME_EVENT_PERSISTED_SEGMENT_SIZE as u64;

        save_runtime_event_checkpoint(
            &conn,
            &RuntimeEventCheckpoint {
                consumer_id: "shared-ui".into(),
                generation: "gen-1".into(),
                seq: segment_size,
                updated_at_unix_ms: 10_000,
                created_at_unix_ms: None,
                last_read_at_unix_ms: None,
                lease_expires_at_unix_ms: None,
                revision: None,
                last_transition: None,
                recovery_hint: Some("persisted_catch_up".into()),
                current_oldest_available_seq: None,
                current_latest_available_seq: None,
                current_compaction_watermark_seq: None,
                current_compaction_watermark_segment_id: None,
            },
        )
        .expect("save checkpoint");

        for seq in 1..=(segment_size * 2 + 200) {
            append_runtime_event(
                &conn,
                "gen-1",
                &RuntimeEventEnvelope {
                    seq,
                    event: "device_updated".into(),
                    payload: serde_json::json!({ "index": seq }),
                    emitted_at_unix_ms: seq * 10,
                },
                segment_size as usize,
                (segment_size * 3) as usize,
                0,
            )
            .expect("append runtime event");
        }

        let (_, oldest_seq, last_seq, events) =
            load_runtime_event_state(&conn, (segment_size * 3) as usize)
                .expect("load runtime events");
        assert_eq!(oldest_seq, Some(segment_size + 1));
        assert_eq!(last_seq, segment_size * 2 + 200);
        assert_eq!(
            events.first().map(|event| event.seq),
            Some(segment_size + 1)
        );
        assert_eq!(
            events.last().map(|event| event.seq),
            Some(segment_size * 2 + 200)
        );

        let _ = std::fs::remove_dir_all(config_dir);
    }

    #[test]
    fn runtime_event_journal_ignores_resync_required_checkpoint_for_retention_pin() {
        let config_dir = std::env::temp_dir().join(format!(
            "dashdrop-runtime-retention-resync-{}",
            uuid::Uuid::new_v4()
        ));
        let conn = super::init_db_at(&config_dir).expect("init db");
        let segment_size = crate::state::RUNTIME_EVENT_PERSISTED_SEGMENT_SIZE as u64;

        save_runtime_event_checkpoint(
            &conn,
            &RuntimeEventCheckpoint {
                consumer_id: "shared-ui".into(),
                generation: "gen-1".into(),
                seq: 1,
                updated_at_unix_ms: 10_000,
                created_at_unix_ms: None,
                last_read_at_unix_ms: None,
                lease_expires_at_unix_ms: None,
                revision: None,
                last_transition: None,
                recovery_hint: Some("resync_required".into()),
                current_oldest_available_seq: None,
                current_latest_available_seq: None,
                current_compaction_watermark_seq: None,
                current_compaction_watermark_segment_id: None,
            },
        )
        .expect("save checkpoint");

        for seq in 1..=(segment_size * 2 + 200) {
            append_runtime_event(
                &conn,
                "gen-1",
                &RuntimeEventEnvelope {
                    seq,
                    event: "device_updated".into(),
                    payload: serde_json::json!({ "index": seq }),
                    emitted_at_unix_ms: seq * 10,
                },
                segment_size as usize,
                (segment_size * 3) as usize,
                0,
            )
            .expect("append runtime event");
        }

        let (_, oldest_seq, _, _) = load_runtime_event_state(&conn, (segment_size * 3) as usize)
            .expect("load runtime events");
        assert_eq!(oldest_seq, Some(segment_size + 1));

        let _ = std::fs::remove_dir_all(config_dir);
    }

    #[test]
    fn runtime_event_journal_ignores_unrecoverable_old_checkpoint_when_newer_pin_exists() {
        let config_dir = std::env::temp_dir().join(format!(
            "dashdrop-runtime-retention-oldest-mask-{}",
            uuid::Uuid::new_v4()
        ));
        let conn = super::init_db_at(&config_dir).expect("init db");
        let segment_size = crate::state::RUNTIME_EVENT_PERSISTED_SEGMENT_SIZE as u64;

        save_runtime_event_checkpoint(
            &conn,
            &RuntimeEventCheckpoint {
                consumer_id: "too-old-ui".into(),
                generation: "gen-1".into(),
                seq: 100,
                updated_at_unix_ms: 10_000,
                created_at_unix_ms: None,
                last_read_at_unix_ms: None,
                lease_expires_at_unix_ms: None,
                revision: None,
                last_transition: None,
                recovery_hint: Some("persisted_catch_up".into()),
                current_oldest_available_seq: None,
                current_latest_available_seq: None,
                current_compaction_watermark_seq: None,
                current_compaction_watermark_segment_id: None,
            },
        )
        .expect("save too-old checkpoint");
        save_runtime_event_checkpoint(
            &conn,
            &RuntimeEventCheckpoint {
                consumer_id: "recoverable-ui".into(),
                generation: "gen-1".into(),
                seq: segment_size * 2 + 200,
                updated_at_unix_ms: 10_000,
                created_at_unix_ms: None,
                last_read_at_unix_ms: None,
                lease_expires_at_unix_ms: None,
                revision: None,
                last_transition: None,
                recovery_hint: Some("persisted_catch_up".into()),
                current_oldest_available_seq: None,
                current_latest_available_seq: None,
                current_compaction_watermark_seq: None,
                current_compaction_watermark_segment_id: None,
            },
        )
        .expect("save recoverable checkpoint");

        for seq in 1..=(segment_size * 3 + 500) {
            append_runtime_event(
                &conn,
                "gen-1",
                &RuntimeEventEnvelope {
                    seq,
                    event: "device_updated".into(),
                    payload: serde_json::json!({ "index": seq }),
                    emitted_at_unix_ms: seq * 10,
                },
                segment_size as usize,
                (segment_size * 2 + 500) as usize,
                0,
            )
            .expect("append runtime event");
        }

        let (_, oldest_seq, _, _) = load_runtime_event_state(&conn, (segment_size * 3) as usize)
            .expect("load runtime events");
        assert_eq!(oldest_seq, Some(segment_size * 2 + 1));

        let _ = std::fs::remove_dir_all(config_dir);
    }

    #[test]
    fn runtime_event_journal_ignores_checkpoint_older_than_max_retention_window() {
        let config_dir = std::env::temp_dir().join(format!(
            "dashdrop-runtime-retention-cap-{}",
            uuid::Uuid::new_v4()
        ));
        let conn = super::init_db_at(&config_dir).expect("init db");
        let segment_size = crate::state::RUNTIME_EVENT_PERSISTED_SEGMENT_SIZE as u64;

        save_runtime_event_checkpoint(
            &conn,
            &RuntimeEventCheckpoint {
                consumer_id: "shared-ui".into(),
                generation: "gen-1".into(),
                seq: segment_size,
                updated_at_unix_ms: 10_000,
                created_at_unix_ms: None,
                last_read_at_unix_ms: None,
                lease_expires_at_unix_ms: None,
                revision: None,
                last_transition: None,
                recovery_hint: Some("persisted_catch_up".into()),
                current_oldest_available_seq: None,
                current_latest_available_seq: None,
                current_compaction_watermark_seq: None,
                current_compaction_watermark_segment_id: None,
            },
        )
        .expect("save checkpoint");

        for seq in 1..=(segment_size * 6 + 50) {
            append_runtime_event(
                &conn,
                "gen-1",
                &RuntimeEventEnvelope {
                    seq,
                    event: "device_updated".into(),
                    payload: serde_json::json!({ "index": seq }),
                    emitted_at_unix_ms: seq * 10,
                },
                segment_size as usize,
                (segment_size * 2) as usize,
                0,
            )
            .expect("append runtime event");
        }

        let (_, oldest_seq, last_seq, events) =
            load_runtime_event_state(&conn, (segment_size * 2) as usize)
                .expect("load runtime events");
        assert_eq!(oldest_seq, Some(segment_size * 5 + 1));
        assert_eq!(last_seq, segment_size * 6 + 50);
        assert_eq!(
            events.first().map(|event| event.seq),
            Some(segment_size * 5 + 1)
        );
        assert_eq!(
            events.last().map(|event| event.seq),
            Some(segment_size * 6 + 50)
        );

        let _ = std::fs::remove_dir_all(config_dir);
    }

    #[test]
    fn runtime_event_journal_reports_segment_stats() {
        let config_dir = std::env::temp_dir().join(format!(
            "dashdrop-runtime-segments-{}",
            uuid::Uuid::new_v4()
        ));
        let conn = super::init_db_at(&config_dir).expect("init db");

        for seq in 1..=1_500 {
            append_runtime_event(
                &conn,
                "gen-1",
                &RuntimeEventEnvelope {
                    seq,
                    event: "device_updated".into(),
                    payload: serde_json::json!({ "index": seq }),
                    emitted_at_unix_ms: seq * 10,
                },
                2_000,
                2_000,
                0,
            )
            .expect("append runtime event");
        }

        let stats = load_runtime_event_journal_stats(&conn).expect("load journal stats");
        assert_eq!(stats.generation, "gen-1");
        assert_eq!(stats.active_segment_count, 2);
        assert_eq!(stats.oldest_active_segment_id, Some(1));
        assert_eq!(stats.latest_active_segment_id, Some(2));
        assert_eq!(stats.compacted_segment_count, 0);
        assert_eq!(stats.latest_compacted_segment_id, None);
        assert_eq!(stats.compaction_watermark_seq, 0);
        assert_eq!(stats.compaction_watermark_segment_id, 0);
        assert_eq!(stats.last_compacted_at_unix_ms, None);

        let _ = std::fs::remove_dir_all(config_dir);
    }

    #[test]
    fn runtime_event_journal_tracks_compaction_watermark_metadata() {
        let config_dir = std::env::temp_dir().join(format!(
            "dashdrop-runtime-compaction-{}",
            uuid::Uuid::new_v4()
        ));
        let conn = super::init_db_at(&config_dir).expect("init db");
        let segment_size = crate::state::RUNTIME_EVENT_PERSISTED_SEGMENT_SIZE as u64;

        for seq in 1..=(segment_size * 2 + 25) {
            append_runtime_event(
                &conn,
                "gen-1",
                &RuntimeEventEnvelope {
                    seq,
                    event: "device_updated".into(),
                    payload: serde_json::json!({ "index": seq }),
                    emitted_at_unix_ms: seq * 10,
                },
                segment_size as usize,
                segment_size as usize,
                0,
            )
            .expect("append runtime event");
        }

        let stats = load_runtime_event_journal_stats(&conn).expect("load journal stats");
        assert_eq!(stats.generation, "gen-1");
        assert_eq!(stats.active_segment_count, 2);
        assert_eq!(stats.oldest_active_segment_id, Some(2));
        assert_eq!(stats.latest_active_segment_id, Some(3));
        assert_eq!(stats.compacted_segment_count, 1);
        assert_eq!(stats.latest_compacted_segment_id, Some(1));
        assert_eq!(stats.compaction_watermark_seq, segment_size);
        assert_eq!(stats.compaction_watermark_segment_id, 1);
        assert_eq!(stats.last_compacted_at_unix_ms, Some(segment_size * 2 * 10));

        let _ = std::fs::remove_dir_all(config_dir);
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
