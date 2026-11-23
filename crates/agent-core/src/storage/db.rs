use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;
use std::sync::mpsc::{sync_channel, SyncSender};
use std::thread;

use rusqlite::Connection;
use tokio::sync::oneshot;

use crate::config::DataRoot;
use crate::{Error, Result};

use super::migrations;

/// Held for the lifetime of a process that uses a data root (desktop app, CLI): a shared
/// lock on `DataRoot::lock_path`, released by the OS when the process exits or crashes.
pub struct ProcessLock {
    _file: File,
    alone: bool,
}

impl ProcessLock {
    /// Whether no other process had the data root open when this one started (so this one
    /// ran migrations and startup recovery).
    pub fn was_alone(&self) -> bool {
        self.alone
    }
}

type Job = Box<dyn FnOnce(&mut Connection) + Send + 'static>;

/// Bound on queued database jobs before submitters are back-pressured.
const QUEUE_DEPTH: usize = 512;

/// Handle to the database worker. Cheap to clone.
#[derive(Clone)]
pub struct Db {
    tx: SyncSender<Job>,
}

impl Db {
    /// Opens the data root's database for a process that may share it with others (the
    /// desktop app and any number of CLI processes). The process first tries the lock file
    /// exclusively: only a process that is alone migrates the schema and marks replies left
    /// `streaming` by an earlier process as interrupted (`open`). One that is not alone must
    /// not touch them — they may belong to a live run elsewhere — and needs the schema to be
    /// exactly its own (`open_secondary`). Either way it then holds a shared lock until the
    /// returned `ProcessLock` is dropped.
    pub fn open_root(root: &DataRoot) -> Result<(Self, ProcessLock)> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.lock_path())?;
        match file.try_lock() {
            Ok(()) => {
                let db = Self::open(&root.database_path());
                // std has no downgrade, so release and re-take shared. A process that gets
                // the exclusive lock in between finds this one's startup already finished,
                // and this one waits for it before running anything.
                file.unlock()?;
                let db = db?;
                file.lock_shared()?;
                Ok((
                    db,
                    ProcessLock {
                        _file: file,
                        alone: true,
                    },
                ))
            }
            Err(TryLockError::WouldBlock) => {
                file.lock_shared()?;
                Ok((
                    Self::open_secondary(&root.database_path())?,
                    ProcessLock {
                        _file: file,
                        alone: false,
                    },
                ))
            }
            Err(TryLockError::Error(e)) => Err(e.into()),
        }
    }

    /// Opens (creating if needed) the database at `path`, applies migrations and runs startup
    /// recovery. Only for a process that is alone on the data root (see `open_root`);
    /// `Db::in_memory` is available for tests.
    pub fn open(path: &Path) -> Result<Self> {
        let mut conn = Connection::open(path).map_err(sql_err)?;
        Self::start(&mut conn, true)?;
        Ok(Self::spawn(conn))
    }

    /// Opens a database another process is using: no migrations, no recovery, and the
    /// schema must be exactly this build's.
    pub fn open_secondary(path: &Path) -> Result<Self> {
        let mut conn = Connection::open(path).map_err(sql_err)?;
        Self::start(&mut conn, false)?;
        Ok(Self::spawn(conn))
    }

    pub fn in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory().map_err(sql_err)?;
        Self::start(&mut conn, true)?;
        Ok(Self::spawn(conn))
    }

    fn start(conn: &mut Connection, primary: bool) -> Result<()> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;
             PRAGMA temp_store = MEMORY;",
        )
        .map_err(sql_err)?;
        if primary {
            migrations::apply(conn)?;
            super::chats::recover_interrupted(conn)?;
        } else {
            migrations::require_current(conn)?;
        }
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
