use crate::state::{TransferStatus, TransferTask};
use anyhow::{anyhow, Result};
use rusqlite::Connection;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot, OnceCell};
use tokio::time::{Duration, Instant};

const DEFAULT_PROGRESS_FLUSH_INTERVAL: Duration = Duration::from_secs(3);
const DEFAULT_PROGRESS_FLUSH_THRESHOLD_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone)]
pub struct TransferProgressPersistence {
    db: Arc<Mutex<Connection>>,
    flush_interval: Duration,
    flush_threshold_bytes: u64,
    tx: Arc<OnceCell<mpsc::UnboundedSender<Command>>>,
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
        }
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
                tokio::spawn(async move {
                    run_worker(db, rx, flush_interval, flush_threshold_bytes).await;
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
                        handle_command(command, &db, &mut pending, flush_interval, flush_threshold_bytes).await;
                    }
                    _ = tokio::time::sleep_until(deadline) => {
                        flush_due_entries(&db, &mut pending, flush_interval).await;
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
                )
                .await;
            }
        }
    }

    flush_all_entries(&db, &mut pending, flush_interval).await;
}

async fn handle_command(
    command: Command,
    db: &Arc<Mutex<Connection>>,
    pending: &mut HashMap<String, PendingTransfer>,
    flush_interval: Duration,
    flush_threshold_bytes: u64,
) {
    match command {
        Command::Schedule(task) => {
            if is_terminal(&task.status) {
                persist_task(db, &task);
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
                flush_transfer_by_id(db, pending, &transfer_id, flush_interval).await;
            }
        }
        Command::FlushNow(task, reply_tx) => {
            pending.remove(&task.id);
            let result = persist_task_result(db, &task);
            let _ = reply_tx.send(result);
        }
    }
}

async fn flush_due_entries(
    db: &Arc<Mutex<Connection>>,
    pending: &mut HashMap<String, PendingTransfer>,
    flush_interval: Duration,
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
        flush_transfer_by_id(db, pending, &transfer_id, flush_interval).await;
    }
}

async fn flush_all_entries(
    db: &Arc<Mutex<Connection>>,
    pending: &mut HashMap<String, PendingTransfer>,
    flush_interval: Duration,
) {
    let transfer_ids: Vec<String> = pending.keys().cloned().collect();
    for transfer_id in transfer_ids {
        flush_transfer_by_id(db, pending, &transfer_id, flush_interval).await;
    }
}

async fn flush_transfer_by_id(
    db: &Arc<Mutex<Connection>>,
    pending: &mut HashMap<String, PendingTransfer>,
    transfer_id: &str,
    _flush_interval: Duration,
) {
    let Some(mut entry) = pending.remove(transfer_id) else {
        return;
    };

    persist_task(db, &entry.latest);
    entry.last_persisted_bytes = entry.latest.bytes_transferred;
    entry.deadline = None;

    if !is_terminal(&entry.latest.status) {
        pending.insert(transfer_id.to_string(), entry);
    }
}

fn persist_task(db: &Arc<Mutex<Connection>>, task: &TransferTask) {
    if let Err(error) = persist_task_result(db, task) {
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
}
