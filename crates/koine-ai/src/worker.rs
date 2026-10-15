//! Device-local lifecycle protocol for the Impress model worker.
//!
//! The worker status is deliberately a file beside the shared database, not
//! an item in the synced graph. A phone and a laptop can legitimately observe
//! different worker reachability while still agreeing on every queued task.

use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fs_lock::{self, FileLock};

pub const WORKER_PROTOCOL_VERSION: u32 = 1;
pub const WORKER_HEARTBEAT_INTERVAL_SECS: u64 = 5;
pub const WORKER_STALE_AFTER_SECS: u64 = 20;

const RUNTIME_DIRECTORY: &str = "runtime";
const STATUS_FILE: &str = "impel-taskd.status.json";
const LEASE_FILE: &str = "impel-taskd.lock";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerLifecycleState {
    Starting,
    Settling,
    Ready,
    Stopping,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerStatusSnapshot {
    pub protocol_version: u32,
    pub worker_id: String,
    pub pid: u32,
    pub state: WorkerLifecycleState,
    pub started_at_ms: i64,
    pub heartbeat_at_ms: i64,
    pub poll_interval_secs: u64,
    pub endpoint_id: String,
    pub last_pass_at_ms: Option<i64>,
    pub last_error: Option<String>,
    pub acquired_total: u64,
    pub completed_total: u64,
    /// GAUGE, not a counter: the CURRENT number of suspended
    /// (running-with-unresolved-review) tasks as of the last pass. The
    /// scheduler re-counts the standing population every pass, so summing
    /// it inflated by the whole backlog every 5 seconds (~17M/day at a
    /// 1,018-task backlog) — useless for the one question it exists to
    /// answer. Field name kept for reader compatibility.
    pub suspended_total: u64,
    pub retried_total: u64,
    pub failed_total: u64,
    /// Executions triggered by a resolved review (counter).
    #[serde(default)]
    pub resumed_total: u64,
    /// Ready tasks deferred because their kind's readiness probe failed
    /// (counter) — the visible trace of "not acquiring doomed work".
    #[serde(default)]
    pub deferred_total: u64,
    /// Wedged running-without-assignee tasks adopted back to pending
    /// (counter).
    #[serde(default)]
    pub adopted_total: u64,
    /// Pending dependents cancelled after an upstream failure (counter).
    #[serde(default)]
    pub cancelled_total: u64,
}

impl WorkerStatusSnapshot {
    pub fn new(
        worker_id: String,
        pid: u32,
        now_ms: i64,
        poll_interval_secs: u64,
        endpoint_id: String,
    ) -> Self {
        Self {
            protocol_version: WORKER_PROTOCOL_VERSION,
            worker_id,
            pid,
            state: WorkerLifecycleState::Starting,
            started_at_ms: now_ms,
            heartbeat_at_ms: now_ms,
            poll_interval_secs,
            endpoint_id,
            last_pass_at_ms: None,
            last_error: None,
            acquired_total: 0,
            completed_total: 0,
            suspended_total: 0,
            retried_total: 0,
            failed_total: 0,
            resumed_total: 0,
            deferred_total: 0,
            adopted_total: 0,
            cancelled_total: 0,
        }
    }

    pub fn is_fresh_at(&self, now_ms: i64) -> bool {
        now_ms.saturating_sub(self.heartbeat_at_ms) <= (WORKER_STALE_AFTER_SECS as i64) * 1_000
    }
}

pub fn worker_runtime_directory(workspace: impl AsRef<Path>) -> PathBuf {
    workspace.as_ref().join(RUNTIME_DIRECTORY)
}

pub fn worker_status_path(workspace: impl AsRef<Path>) -> PathBuf {
    worker_runtime_directory(workspace).join(STATUS_FILE)
}

pub fn read_worker_status(workspace: impl AsRef<Path>) -> io::Result<Option<WorkerStatusSnapshot>> {
    let path = worker_status_path(workspace);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|error| {
        io::Error::new(
            ErrorKind::InvalidData,
            format!("decode worker status {}: {error}", path.display()),
        )
    })
}

pub fn write_worker_status(
    workspace: impl AsRef<Path>,
    status: &WorkerStatusSnapshot,
) -> io::Result<()> {
    write_worker_status_inner(workspace, status, true)
}

/// Like [`write_worker_status`] but WITHOUT the `sync_all` device flush.
///
/// Heartbeats and idle-pass publishes happen every ~5 seconds forever; on
/// macOS each `sync_all` is an `F_FULLFSYNC` full device-cache barrier
/// (~34,560/day at idle, on the same volume as the store's WAL — the
/// status file was out-fsyncing the database). A liveness file with a 20s
/// staleness window tolerates losing its last write on power loss; the
/// atomic tmp-then-rename still guarantees readers never see a torn file.
/// Lifecycle STATE changes should keep using the durable variant.
pub fn write_worker_status_fast(
    workspace: impl AsRef<Path>,
    status: &WorkerStatusSnapshot,
) -> io::Result<()> {
    write_worker_status_inner(workspace, status, false)
}

fn write_worker_status_inner(
    workspace: impl AsRef<Path>,
    status: &WorkerStatusSnapshot,
    durable: bool,
) -> io::Result<()> {
    let runtime = worker_runtime_directory(workspace);
    fs::create_dir_all(&runtime)?;
    let destination = runtime.join(STATUS_FILE);
    let temporary = runtime.join(format!(".{STATUS_FILE}.{}.tmp", status.pid));
    let bytes = serde_json::to_vec(status).map_err(io::Error::other)?;

    let write_result = (|| {
        let mut file = File::create(&temporary)?;
        file.write_all(&bytes)?;
        if durable {
            file.sync_all()?;
        }
        fs::rename(&temporary, &destination)
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

/// Exclusive process-lifetime lock for one worker per workspace.
///
/// The lock file intentionally remains on disk. The kernel lock is released
/// automatically on crash, avoiding stale-file deletion races.
#[derive(Debug)]
pub struct WorkerLease {
    #[allow(dead_code)]
    lock: FileLock,
}

impl WorkerLease {
    pub fn acquire(workspace: impl AsRef<Path>) -> io::Result<Self> {
        let path = worker_runtime_directory(workspace).join(LEASE_FILE);
        let lock = FileLock::try_exclusive(&path)
            .map_err(fs_lock::already_locked(&path, "impel-taskd worker"))?;
        Ok(Self { lock })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_round_trips_atomically_and_reports_freshness() {
        let directory = tempfile::tempdir().unwrap();
        let mut status =
            WorkerStatusSnapshot::new("worker-a".into(), 42, 10_000, 5, "local-omlx".into());
        status.state = WorkerLifecycleState::Ready;
        status.completed_total = 3;
        write_worker_status(directory.path(), &status).unwrap();

        let decoded = read_worker_status(directory.path()).unwrap().unwrap();
        assert_eq!(decoded, status);
        assert!(decoded.is_fresh_at(29_999));
        assert!(!decoded.is_fresh_at(30_001));
    }

    #[test]
    fn lease_is_exclusive_and_released_on_drop() {
        let directory = tempfile::tempdir().unwrap();
        let first = WorkerLease::acquire(directory.path()).unwrap();
        let error = WorkerLease::acquire(directory.path()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::AlreadyExists);
        drop(first);
        WorkerLease::acquire(directory.path()).unwrap();
    }
}
