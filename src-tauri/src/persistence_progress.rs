use crate::state::{TransferStatus, TransferTask};
use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot, OnceCell};
use tokio::time::{Duration, Instant};

const DEFAULT_PROGRESS_FLUSH_INTERVAL: Duration = Duration::from_secs(3);
const DEFAULT_PROGRESS_FLUSH_THRESHOLD_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct TransferProgressPersistenceDiagnostics {
    pub flush_interval_ms: u64,
    pub flush_threshold_bytes: u64,
    pub pending_transfer_count: u64,
    pub schedule_requests: u64,
    pub coalesced_updates: u64,
    pub interval_flushes: u64,
    pub threshold_flushes: u64,
    pub force_flushes: u64,
    pub terminal_flushes: u64,
    pub successful_writes: u64,
    pub failed_writes: u64,
    pub last_flush_at_unix_ms: Option<u64>,
    pub last_force_flush_at_unix_ms: Option<u64>,
}

#[derive(Clone)]
pub struct TransferProgressPersistence {
    db: Arc<Mutex<Connection>>,
    flush_interval: Duration,
    flush_threshold_bytes: u64,
    tx: Arc<OnceCell<mpsc::UnboundedSender<Command>>>,
    diagnostics: Arc<Mutex<TransferProgressPersistenceDiagnostics>>,
}

#[derive(Debug)]
enum Command {
    Schedule(TransferTask),
    FlushNow(TransferTask, oneshot::Sender<Result<()>>),
}

#[derive(Debug, Clone)]
struct PendingTransfer {
    latest: TransferTask,
    last_persisted_bytes: u64,
    deadline: Option<Instant>,
}

#[derive(Debug, Clone, Copy)]
enum PersistReason {
    Interval,
    Threshold,
    Force,
    Terminal,
}

impl TransferProgressPersistence {
    pub fn new(db: Arc<Mutex<Connection>>) -> Self {
        Self::with_policy(
            db,
            DEFAULT_PROGRESS_FLUSH_INTERVAL,
            DEFAULT_PROGRESS_FLUSH_THRESHOLD_BYTES,
        )
    }

    fn with_policy(
        db: Arc<Mutex<Connection>>,
        flush_interval: Duration,
        flush_threshold_bytes: u64,
    ) -> Self {
        Self {
            db,
            flush_interval,
            flush_threshold_bytes,
            tx: Arc::new(OnceCell::new()),
            diagnostics: Arc::new(Mutex::new(TransferProgressPersistenceDiagnostics {
                flush_interval_ms: flush_interval.as_millis().min(u64::MAX as u128) as u64,
                flush_threshold_bytes,
                pending_transfer_count: 0,
                schedule_requests: 0,
                coalesced_updates: 0,
                interval_flushes: 0,
                threshold_flushes: 0,
                force_flushes: 0,
                terminal_flushes: 0,
                successful_writes: 0,
                failed_writes: 0,
                last_flush_at_unix_ms: None,
                last_force_flush_at_unix_ms: None,
            })),
        }
    }

    pub fn diagnostics_snapshot(&self) -> TransferProgressPersistenceDiagnostics {
        self.diagnostics
            .lock()
            .expect("progress diagnostics lock poisoned")
            .clone()
    }

    pub async fn schedule(&self, task: &TransferTask) -> Result<()> {
        let tx = self.tx().await;
        tx.send(Command::Schedule(progress_snapshot(task)))
            .map_err(|_| anyhow!("progress persistence worker stopped"))?;
        Ok(())
    }

    pub async fn flush_now(&self, task: &TransferTask) -> Result<()> {
        let tx = self.tx().await;
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(Command::FlushNow(progress_snapshot(task), reply_tx))
            .map_err(|_| anyhow!("progress persistence worker stopped"))?;
        reply_rx
            .await
            .map_err(|_| anyhow!("progress persistence worker dropped flush reply"))?
    }

    async fn tx(&self) -> mpsc::UnboundedSender<Command> {
        self.tx
            .get_or_init(|| async {
                let (tx, rx) = mpsc::unbounded_channel();
                let db = Arc::clone(&self.db);
                let flush_interval = self.flush_interval;
                let flush_threshold_bytes = self.flush_threshold_bytes;
                let diagnostics = Arc::clone(&self.diagnostics);
                tokio::spawn(async move {
                    run_worker(db, rx, flush_interval, flush_threshold_bytes, diagnostics).await;
                });
                tx
            })
            .await
            .clone()
    }
}

pub(crate) fn progress_snapshot(task: &TransferTask) -> TransferTask {
    TransferTask {
        id: task.id.clone(),
        batch_id: task.batch_id.clone(),
        direction: task.direction.clone(),
        peer_fingerprint: task.peer_fingerprint.clone(),
        peer_name: task.peer_name.clone(),
        items: task.items.clone(),
        status: task.status.clone(),
        bytes_transferred: task.bytes_transferred,
        total_bytes: task.total_bytes,
        revision: task.revision,
        started_at_unix: task.started_at_unix,
        ended_at_unix: task.ended_at_unix,
        terminal_reason_code: task.terminal_reason_code.clone(),
        error: task.error.clone(),
        source_paths: None,
        source_path_by_file_id: None,
        failed_file_ids: task.failed_file_ids.clone(),
        conn: None,
        ended_at: None,
    }
}

fn is_terminal(status: &TransferStatus) -> bool {
    matches!(
        status,
        TransferStatus::Completed
            | TransferStatus::PartialCompleted
            | TransferStatus::Rejected
            | TransferStatus::CancelledBySender
            | TransferStatus::CancelledByReceiver
            | TransferStatus::Failed
    )
}

fn is_newer_than(current: &TransferTask, candidate: &TransferTask) -> bool {
    candidate.revision > current.revision
        || (candidate.revision == current.revision
            && candidate.bytes_transferred >= current.bytes_transferred)
}

async fn run_worker(
    db: Arc<Mutex<Connection>>,
    mut rx: mpsc::UnboundedReceiver<Command>,
    flush_interval: Duration,
    flush_threshold_bytes: u64,
    diagnostics: Arc<Mutex<TransferProgressPersistenceDiagnostics>>,
) {
    let mut pending: HashMap<String, PendingTransfer> = HashMap::new();

    loop {
        let next_deadline = pending.values().filter_map(|entry| entry.deadline).min();

        match next_deadline {
            Some(deadline) => {
                tokio::select! {
                    biased;
                    maybe_command = rx.recv() => {
                        let Some(command) = maybe_command else {
                            break;
                        };
                        handle_command(
                            command,
                            &db,
                            &mut pending,
                            flush_interval,
                            flush_threshold_bytes,
                            &diagnostics,
                        ).await;
                    }
                    _ = tokio::time::sleep_until(deadline) => {
                        flush_due_entries(&db, &mut pending, &diagnostics).await;
                    }
                }
            }
            None => {
                let Some(command) = rx.recv().await else {
                    break;
                };
                handle_command(
                    command,
                    &db,
                    &mut pending,
                    flush_interval,
                    flush_threshold_bytes,
                    &diagnostics,
                )
                .await;
            }
        }
    }

    flush_all_entries(&db, &mut pending, &diagnostics).await;
}

async fn handle_command(
    command: Command,
    db: &Arc<Mutex<Connection>>,
    pending: &mut HashMap<String, PendingTransfer>,
    flush_interval: Duration,
    flush_threshold_bytes: u64,
    diagnostics: &Arc<Mutex<TransferProgressPersistenceDiagnostics>>,
) {
    match command {
        Command::Schedule(task) => {
            update_diagnostics(diagnostics, |snapshot| {
                snapshot.schedule_requests = snapshot.schedule_requests.saturating_add(1);
            });
            if is_terminal(&task.status) {
                pending.remove(&task.id);
                sync_pending_transfer_count(diagnostics, pending.len());
                persist_task(db, &task, diagnostics, PersistReason::Terminal);
                pending.remove(&task.id);
                return;
            }

            let now = Instant::now();
            let transfer_id = task.id.clone();
            match pending.get_mut(&transfer_id) {
                Some(entry) => {
                    if !is_newer_than(&entry.latest, &task) {
                        return;
                    }
                    entry.latest = task;
                    update_diagnostics(diagnostics, |snapshot| {
                        snapshot.coalesced_updates = snapshot.coalesced_updates.saturating_add(1);
                    });
                }
                None => {
                    pending.insert(
                        transfer_id.clone(),
                        PendingTransfer {
                            latest: task,
                            last_persisted_bytes: 0,
                            deadline: Some(now + flush_interval),
                        },
                    );
                    sync_pending_transfer_count(diagnostics, pending.len());
                }
            }

            if let Some(entry) = pending.get_mut(&transfer_id) {
                if entry.latest.bytes_transferred > entry.last_persisted_bytes {
                    entry.deadline = Some(now + flush_interval);
                }
            }

            let should_flush = pending
                .get(&transfer_id)
                .map(|entry| {
                    entry
                        .latest
                        .bytes_transferred
                        .saturating_sub(entry.last_persisted_bytes)
                        >= flush_threshold_bytes
                })
                .unwrap_or(false);

            if should_flush {
                flush_transfer_by_id(
                    db,
                    pending,
                    &transfer_id,
                    diagnostics,
                    PersistReason::Threshold,
                )
                .await;
            }
        }
        Command::FlushNow(task, reply_tx) => {
            pending.remove(&task.id);
            sync_pending_transfer_count(diagnostics, pending.len());
            let result = persist_task_result(db, &task);
            record_persist_result(diagnostics, &result, PersistReason::Force);
            let _ = reply_tx.send(result);
        }
    }
}

async fn flush_due_entries(
    db: &Arc<Mutex<Connection>>,
    pending: &mut HashMap<String, PendingTransfer>,
    diagnostics: &Arc<Mutex<TransferProgressPersistenceDiagnostics>>,
) {
    let now = Instant::now();
    let due_ids: Vec<String> = pending
        .iter()
        .filter_map(|(transfer_id, entry)| {
            if entry.deadline.is_some() && entry.deadline <= Some(now) {
                Some(transfer_id.clone())
            } else {
                None
            }
        })
        .collect();

    for transfer_id in due_ids {
        flush_transfer_by_id(
            db,
            pending,
            &transfer_id,
            diagnostics,
            PersistReason::Interval,
        )
        .await;
    }
}

async fn flush_all_entries(
    db: &Arc<Mutex<Connection>>,
    pending: &mut HashMap<String, PendingTransfer>,
    diagnostics: &Arc<Mutex<TransferProgressPersistenceDiagnostics>>,
) {
    let transfer_ids: Vec<String> = pending.keys().cloned().collect();
    for transfer_id in transfer_ids {
        flush_transfer_by_id(
            db,
            pending,
            &transfer_id,
            diagnostics,
            PersistReason::Interval,
        )
        .await;
    }
}

async fn flush_transfer_by_id(
    db: &Arc<Mutex<Connection>>,
    pending: &mut HashMap<String, PendingTransfer>,
    transfer_id: &str,
    diagnostics: &Arc<Mutex<TransferProgressPersistenceDiagnostics>>,
    reason: PersistReason,
) {
    let Some(mut entry) = pending.remove(transfer_id) else {
        return;
    };

    persist_task(db, &entry.latest, diagnostics, reason);
    entry.last_persisted_bytes = entry.latest.bytes_transferred;
    entry.deadline = None;

    if !is_terminal(&entry.latest.status) {
        pending.insert(transfer_id.to_string(), entry);
    }
    sync_pending_transfer_count(diagnostics, pending.len());
}

fn persist_task(
    db: &Arc<Mutex<Connection>>,
    task: &TransferTask,
    diagnostics: &Arc<Mutex<TransferProgressPersistenceDiagnostics>>,
    reason: PersistReason,
) {
    let result = persist_task_result(db, task);
    record_persist_result(diagnostics, &result, reason);
    if let Err(error) = result {
        tracing::warn!(
            transfer_id = %task.id,
            revision = task.revision,
            bytes_transferred = task.bytes_transferred,
            status = ?task.status,
            reason = %error,
            "progress persistence write failed"
        );
    }
}

fn persist_task_result(db: &Arc<Mutex<Connection>>, task: &TransferTask) -> Result<()> {
    let guard = db
        .lock()
        .map_err(|_| anyhow!("SQLite connection lock poisoned"))?;
    crate::db::save_transfer(&guard, task)
}

fn update_diagnostics(
    diagnostics: &Arc<Mutex<TransferProgressPersistenceDiagnostics>>,
    update: impl FnOnce(&mut TransferProgressPersistenceDiagnostics),
) {
    if let Ok(mut snapshot) = diagnostics.lock() {
        update(&mut snapshot);
    }
}

fn sync_pending_transfer_count(
    diagnostics: &Arc<Mutex<TransferProgressPersistenceDiagnostics>>,
    pending_len: usize,
) {
    update_diagnostics(diagnostics, |snapshot| {
        snapshot.pending_transfer_count = pending_len as u64;
    });
}

fn record_persist_result(
    diagnostics: &Arc<Mutex<TransferProgressPersistenceDiagnostics>>,
    result: &Result<()>,
    reason: PersistReason,
) {
    let now_unix_ms = now_unix_millis();
    update_diagnostics(diagnostics, |snapshot| {
        match reason {
            PersistReason::Interval => {
                snapshot.interval_flushes = snapshot.interval_flushes.saturating_add(1);
            }
            PersistReason::Threshold => {
                snapshot.threshold_flushes = snapshot.threshold_flushes.saturating_add(1);
            }
            PersistReason::Force => {
                snapshot.force_flushes = snapshot.force_flushes.saturating_add(1);
                snapshot.last_force_flush_at_unix_ms = Some(now_unix_ms);
            }
            PersistReason::Terminal => {
                snapshot.terminal_flushes = snapshot.terminal_flushes.saturating_add(1);
            }
        }

        match result {
            Ok(()) => {
                snapshot.successful_writes = snapshot.successful_writes.saturating_add(1);
                snapshot.last_flush_at_unix_ms = Some(now_unix_ms);
            }
            Err(_) => {
                snapshot.failed_writes = snapshot.failed_writes.saturating_add(1);
            }
        }
    });
}

fn now_unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::TransferProgressPersistence;
    use crate::state::{FileItemMeta, TransferDirection, TransferStatus, TransferTask};
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};
    use tokio::time::Duration;

    fn setup_test_db() -> Arc<Mutex<Connection>> {
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
        Arc::new(Mutex::new(conn))
    }

    fn task(
        id: &str,
        bytes_transferred: u64,
        status: TransferStatus,
        revision: u64,
    ) -> TransferTask {
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
                size: 128,
                hash: None,
                risk_class: None,
            }],
            status,
            bytes_transferred,
            total_bytes: 128,
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

    fn persisted_snapshot(conn: &Arc<Mutex<Connection>>, transfer_id: &str) -> (String, u64, u64) {
        let guard = conn.lock().expect("db lock");
        guard
            .query_row(
                "SELECT status, bytes_transferred, revision FROM transfers_history WHERE id = ?1",
                [transfer_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("persisted row")
    }

    #[tokio::test]
    async fn coalesces_progress_until_flush_interval() {
        let db = setup_test_db();
        let persistence =
            TransferProgressPersistence::with_policy(db.clone(), Duration::from_millis(30), 1024);

        persistence
            .schedule(&task("transfer-1", 8, TransferStatus::Transferring, 1))
            .await
            .expect("schedule first");
        persistence
            .schedule(&task("transfer-1", 16, TransferStatus::Transferring, 1))
            .await
            .expect("schedule second");

        tokio::time::sleep(Duration::from_millis(60)).await;

        let (status, bytes, revision) = persisted_snapshot(&db, "transfer-1");
        assert_eq!(status, "Transferring");
        assert_eq!(bytes, 16);
        assert_eq!(revision, 1);
    }

    #[tokio::test]
    async fn flushes_early_when_byte_threshold_is_reached() {
        let db = setup_test_db();
        let persistence =
            TransferProgressPersistence::with_policy(db.clone(), Duration::from_secs(5), 32);

        persistence
            .schedule(&task("transfer-2", 40, TransferStatus::Transferring, 1))
            .await
            .expect("schedule threshold");

        tokio::time::sleep(Duration::from_millis(50)).await;

        let (status, bytes, revision) = persisted_snapshot(&db, "transfer-2");
        assert_eq!(status, "Transferring");
        assert_eq!(bytes, 40);
        assert_eq!(revision, 1);
    }

    #[tokio::test]
    async fn terminal_flush_stays_terminal_after_delayed_progress() {
        let db = setup_test_db();
        let persistence =
            TransferProgressPersistence::with_policy(db.clone(), Duration::from_millis(40), 1024);

        persistence
            .schedule(&task("transfer-3", 12, TransferStatus::Transferring, 1))
            .await
            .expect("schedule progress");
        persistence
            .flush_now(&task("transfer-3", 12, TransferStatus::Completed, 2))
            .await
            .expect("flush terminal");

        tokio::time::sleep(Duration::from_millis(80)).await;

        let (status, bytes, revision) = persisted_snapshot(&db, "transfer-3");
        assert_eq!(status, "Completed");
        assert_eq!(bytes, 12);
        assert_eq!(revision, 2);
    }

    #[tokio::test]
    async fn diagnostics_track_flush_policy_and_reasons() {
        let db = setup_test_db();
        let persistence =
            TransferProgressPersistence::with_policy(db.clone(), Duration::from_millis(30), 32);

        persistence
            .schedule(&task("transfer-4", 8, TransferStatus::Transferring, 1))
            .await
            .expect("schedule first");
        persistence
            .schedule(&task("transfer-4", 48, TransferStatus::Transferring, 2))
            .await
            .expect("schedule threshold flush");
        persistence
            .flush_now(&task("transfer-4", 64, TransferStatus::Completed, 3))
            .await
            .expect("force flush");

        let diagnostics = persistence.diagnostics_snapshot();
        assert_eq!(diagnostics.flush_interval_ms, 30);
        assert_eq!(diagnostics.flush_threshold_bytes, 32);
        assert_eq!(diagnostics.schedule_requests, 2);
        assert_eq!(diagnostics.coalesced_updates, 1);
        assert_eq!(diagnostics.threshold_flushes, 1);
        assert_eq!(diagnostics.force_flushes, 1);
        assert_eq!(diagnostics.successful_writes, 2);
        assert_eq!(diagnostics.failed_writes, 0);
        assert_eq!(diagnostics.pending_transfer_count, 0);
        assert!(diagnostics.last_flush_at_unix_ms.is_some());
        assert!(diagnostics.last_force_flush_at_unix_ms.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn stress_regression_multi_file_over_100_rounds() {
        let db = setup_test_db();
        // Simulate extreme concurrency: 100 concurrent transfers, 50 updates each.
        // We use tiny thresholds to force many real SQLite writes.
        let persistence = TransferProgressPersistence::with_policy(
            db.clone(),
            Duration::from_millis(5), // Aggressive interval
            10,                       // Tiny byte threshold
        );

        let mut handles = Vec::new();
        for i in 0..100 {
            let p = persistence.clone();
            let id = format!("transfer-{}", i);
            handles.push(tokio::spawn(async move {
                for j in 0..50 {
                    p.schedule(&task(&id, j * 2, TransferStatus::Transferring, j))
                        .await
                        .expect("schedule");
                }
                // Force a terminal flush to ensure final state is correctly persisted.
                p.flush_now(&task(&id, 100, TransferStatus::Completed, 100))
                    .await
                    .expect("flush terminal");
            }));
        }

        for h in handles {
            h.await.expect("task finished");
        }

        let diagnostics = persistence.diagnostics_snapshot();
        // Verify we had a significant number of writes and zero failures.
        assert!(diagnostics.successful_writes > 100);
        assert_eq!(diagnostics.failed_writes, 0);

        // Authoritatively verify the database state.
        let guard = db.lock().unwrap();
        let count: u32 = guard
            .query_row(
                "SELECT COUNT(*) FROM transfers_history WHERE status = 'Completed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 100,
            "Expected all 100 transfers to be 'Completed' in SQLite"
        );
    }
}
