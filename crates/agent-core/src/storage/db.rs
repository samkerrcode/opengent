use std::path::Path;
use std::sync::mpsc::{sync_channel, SyncSender};
use std::thread;

use rusqlite::Connection;
use tokio::sync::oneshot;

use crate::{Error, Result};

use super::migrations;

type Job = Box<dyn FnOnce(&mut Connection) + Send + 'static>;

/// Bound on queued database jobs before submitters are back-pressured.
const QUEUE_DEPTH: usize = 512;

/// Handle to the database worker. Cheap to clone.
#[derive(Clone)]
pub struct Db {
    tx: SyncSender<Job>,
}

impl Db {
    /// Opens (creating if needed) the database at `path`, applies migrations and runs startup
    /// recovery. `Db::in_memory` is available for tests.
    pub fn open(path: &Path) -> Result<Self> {
        let mut conn = Connection::open(path).map_err(sql_err)?;
        Self::start(&mut conn)?;
        Ok(Self::spawn(conn))
    }

    pub fn in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory().map_err(sql_err)?;
        Self::start(&mut conn)?;
        Ok(Self::spawn(conn))
    }

    fn start(conn: &mut Connection) -> Result<()> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;
             PRAGMA temp_store = MEMORY;",
        )
        .map_err(sql_err)?;
        migrations::apply(conn)?;
        super::chats::recover_interrupted(conn)?;
        Ok(())
    }

    fn spawn(mut conn: Connection) -> Self {
        let (tx, rx) = sync_channel::<Job>(QUEUE_DEPTH);
        thread::Builder::new()
            .name("sqlite-worker".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    job(&mut conn);
                }
                let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
            })
            .expect("failed to spawn sqlite worker thread");
        Self { tx }
    }

    /// Runs `f` on the worker thread and awaits its result.
    pub async fn call<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let (reply_tx, reply_rx) = oneshot::channel();
        let job: Job = Box::new(move |conn| {
            let _ = reply_tx.send(f(conn));
        });
        self.tx
            .send(job)
            .map_err(|_| Error::Internal("database worker is gone".into()))?;
        reply_rx
            .await
            .map_err(|_| Error::Internal("database worker dropped the job".into()))?
    }

    /// Fire-and-forget variant for hot paths (streaming checkpoints). Errors are logged.
    pub fn spawn_job<F>(&self, f: F)
    where
        F: FnOnce(&mut Connection) -> Result<()> + Send + 'static,
    {
        let job: Job = Box::new(move |conn| {
            if let Err(e) = f(conn) {
                tracing::error!(error = %e, "background database job failed");
            }
        });
        if self.tx.send(job).is_err() {
            tracing::error!("database worker is gone; dropping job");
        }
    }

    /// Synchronous variant for use outside the async runtime (startup, tests).
    pub fn call_blocking<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(1);
        let job: Job = Box::new(move |conn| {
            let _ = reply_tx.send(f(conn));
        });
        self.tx
            .send(job)
            .map_err(|_| Error::Internal("database worker is gone".into()))?;
        reply_rx
            .recv()
            .map_err(|_| Error::Internal("database worker dropped the job".into()))?
    }
}

pub(crate) fn sql_err(e: rusqlite::Error) -> Error {
    Error::Io(format!("database error: {e}"))
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
