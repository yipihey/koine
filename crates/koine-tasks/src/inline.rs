//! The inline job runner (ADR-0034 D6, plan P4).
//!
//! A `long_running` verb calls [`start_inline`] with its arguments and the
//! work as a future: the runner mints the `task@1.0.0` handle, spawns the
//! work on the shared service runtime (`impress_service_core::runtime`, the
//! one `block_on` every sync host already uses), and hands the handle back
//! in milliseconds. The work reports through its [`JobContext`] — events
//! into the `task-event@1.0.0` ring, `cancel_requested()` between steps —
//! and its outcome lands on the row through [`crate::job::finish_job`].
//! It works over any backend of the store port (docs/plan-koine-kit-moves.md
//! KM11): [`JobContext`] holds the store as the caller's type, so a verb
//! keeps the store it passed in.
//!
//! "Inline" because it runs in whatever process called the verb: the MCP
//! server, a CLI, the app. No daemon is needed for a job to exist, which is
//! what makes the handle honest in an `impress-mcp` process with no
//! impel-taskd registered. A CLI must [`drain_inline`] before it exits, or
//! its process takes the job with it.
//!
//! The kernel is [`crate::job`]; this module adds only what needs a
//! runtime: the spawn, the cancel poller, the wait loop, the CLI's drain.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use impress_item::{AttributedStore, ItemId};
use impress_service_core::job::JobStarted;
use impress_service_core::refusal::Refusal;
use impress_service_core::runtime;
use serde_json::{json, Value};
use tokio::task::JoinHandle;

use crate::job::{self, EventRow, JobRow};
use crate::TaskState;

/// How often a job's cancel flag is re-read from the store while it runs
/// (the poller behind [`JobContext::cancel_flag`]; `cancel_requested()`
/// reads at most this often too).
pub const CANCEL_POLL_MS: u64 = 250;

/// How often [`wait`] re-reads the ring while nothing is new. The surface
/// wait's number.
pub const WAIT_POLL_MS: u64 = 100;

/// The longest one [`wait`] call is held open. Longer than this outlives
/// the MCP clients' and HTTP callers' request timeouts; a caller loops.
pub const MAX_WAIT_MS: u64 = 55_000;

/// The author every runner write carries.
pub const RUNNER_AUTHOR: &str = "agent:job-runner";

/// What a job body concluded.
#[derive(Debug, Clone, PartialEq)]
pub enum JobOutcome {
    /// The verb's own result. `running → done`.
    Done(Value),
    /// The verb's own (refusing) result and the error the row records.
    /// `running → failed`.
    Failed { result: Value, error: String },
    /// The body saw `cancel_requested` and stopped; `result` is what it had.
    /// `running → cancelled`.
    Cancelled(Value),
}

impl JobOutcome {
    fn state(&self) -> TaskState {
        match self {
            Self::Done(_) => TaskState::Done,
            Self::Failed { .. } => TaskState::Failed,
            Self::Cancelled(_) => TaskState::Cancelled,
        }
    }

    fn result(&self) -> &Value {
        match self {
            Self::Done(v) | Self::Cancelled(v) | Self::Failed { result: v, .. } => v,
        }
    }

    fn error(&self) -> Option<&str> {
        match self {
            Self::Failed { error, .. } => Some(error),
            _ => None,
        }
    }
}

/// What a running job body holds: the store, its own id, and the two
/// things it owes the caller — progress and a cancel check.
pub struct JobContext<S: ?Sized = dyn AttributedStore> {
    store: Arc<S>,
    id: ItemId,
    cancel: Arc<AtomicBool>,
    last_poll: Mutex<Instant>,
}

impl<S: AttributedStore + ?Sized> JobContext<S> {
    pub fn id(&self) -> ItemId {
        self.id
    }

    pub fn store(&self) -> &Arc<S> {
        &self.store
    }

    /// Append a progress event. A failure to write progress is logged, not
    /// raised: the work matters more than its narration.
    pub fn progress(&self, name: &str, payload: Value) -> Option<u64> {
        match job::append_event(&*self.store, self.id, name, &payload, RUNNER_AUTHOR) {
            Ok(seq) => Some(seq),
            Err(e) => {
                eprintln!("[job {}] progress '{name}' not recorded: {e}", self.id);
                None
            }
        }
    }

    /// Has someone asked this job to stop? Cheap to call in a loop: the
    /// store is read at most every [`CANCEL_POLL_MS`], and a `true` sticks.
    pub fn cancel_requested(&self) -> bool {
        if self.cancel.load(Ordering::Relaxed) {
            return true;
        }
        let due = {
            let mut last = self.last_poll.lock().unwrap_or_else(|p| p.into_inner());
            if last.elapsed() < Duration::from_millis(CANCEL_POLL_MS) {
                return false;
            }
            *last = Instant::now();
            true
        };
        if due && job::cancel_requested(&*self.store, self.id).unwrap_or(false) {
            self.cancel.store(true, Ordering::Relaxed);
            return true;
        }
        false
    }

    /// The flag a blocking host (a process runner's wait loop) can check
    /// without a store handle. Set by the runner's poller while the job
    /// runs, and by [`Self::cancel_requested`].
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }
}

/// Every inline job this process started and has not yet drained.
static INLINE: Mutex<Vec<(ItemId, JoinHandle<()>)>> = Mutex::new(Vec::new());

/// Start `verb`'s work as a job and answer with its handle.
///
/// Refuses (no row, no spawn) when `store` is the in-memory fallback: a
/// handle nothing else can read is a lie, exactly like a migration that
/// "succeeds" against the stand-in.
pub fn start_inline<S, F, Fut>(store: Arc<S>, verb: &str, args: &Value, body: F) -> JobStarted
where
    S: AttributedStore + ?Sized + 'static,
    F: FnOnce(Arc<JobContext<S>>) -> Fut + Send + 'static,
    Fut: Future<Output = JobOutcome> + Send + 'static,
{
    let id = match job::create_job(
        &*store,
        verb,
        args,
        &job::inline_runner_name(),
        RUNNER_AUTHOR,
    ) {
        Ok(id) => id,
        Err(e) => return JobStarted::refused(Refusal::store(format!("{verb}: {e}"))),
    };
    let ctx = Arc::new(JobContext {
        store: store.clone(),
        id,
        cancel: Arc::new(AtomicBool::new(false)),
        last_poll: Mutex::new(Instant::now()),
    });
    let verb_name = verb.to_string();
    let supervisor = runtime::spawn(async move {
        // The poller keeps the flag current for hosts that cannot read the
        // store (a process wait loop); it ends with the job.
        let done = Arc::new(AtomicBool::new(false));
        let poller = {
            let store = store.clone();
            let flag = ctx.cancel_flag();
            let done = done.clone();
            runtime::spawn(async move {
                while !done.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(CANCEL_POLL_MS)).await;
                    if flag.load(Ordering::Relaxed) {
                        break;
                    }
                    match job::cancel_requested(&*store, id) {
                        Ok(true) => {
                            flag.store(true, Ordering::Relaxed);
                            break;
                        }
                        Ok(false) => {}
                        // A store that cannot be read is not "no cancel":
                        // say so, or a wedged read looks like a job that
                        // will not stop.
                        Err(e) => eprintln!("[job {id}] cancel poll failed: {e}"),
                    }
                }
            })
        };
        let outcome = match runtime::spawn(body(ctx.clone())).await {
            Ok(outcome) => outcome,
            Err(e) => JobOutcome::Failed {
                result: json!({ "ok": false, "code": "internal", "message": format!("{verb_name}: job panicked: {e}") }),
                error: format!("job panicked: {e}"),
            },
        };
        done.store(true, Ordering::Relaxed);
        poller.abort();
        let state = outcome.state();
        if let Err(e) = job::finish_job(
            &*store,
            id,
            state,
            outcome.result(),
            outcome.error(),
            RUNNER_AUTHOR,
        ) {
            eprintln!("[job {id}] could not record the {state} outcome: {e}");
        }
        // The last event past any cursor: a waiter learns the job ended
        // without a second read of the row.
        let _ = job::append_event(
            &*store,
            id,
            "finished",
            &json!({ "state": state.as_str(), "ok": state == TaskState::Done }),
            RUNNER_AUTHOR,
        );
    });
    if let Ok(mut inline) = INLINE.lock() {
        inline.push((id, supervisor));
    }
    JobStarted::started(id.to_string(), verb)
}

/// Wait for every inline job this process started. A one-shot process (the
/// CLI) calls this before exiting; a long-lived one (MCP, the app) never
/// needs to.
pub async fn drain_inline() {
    loop {
        let batch: Vec<(ItemId, JoinHandle<()>)> = match INLINE.lock() {
            Ok(mut inline) => std::mem::take(&mut *inline),
            Err(_) => Vec::new(),
        };
        if batch.is_empty() {
            return;
        }
        for (_, handle) in batch {
            let _ = handle.await;
        }
    }
}

/// How many inline jobs are still running in this process.
pub fn inline_job_count() -> usize {
    INLINE
        .lock()
        .map(|inline| inline.iter().filter(|(_, h)| !h.is_finished()).count())
        .unwrap_or(0)
}

/// What one [`wait`] concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waited {
    pub job: JobRow,
    /// Events past the cursor, oldest first — empty on timeout and when
    /// the job had already ended with nothing new past the cursor.
    pub events: Vec<EventRow>,
    /// The cursor to pass next time: the last event's `seq`, or the
    /// caller's own when nothing came.
    pub next_seq: u64,
    pub timed_out: bool,
    pub gap: bool,
}

/// Long-poll the job's ring past `after_seq` for up to `timeout` (at most
/// [`MAX_WAIT_MS`]): returns as soon as an event lands, at once when the
/// job is already terminal, or on timeout with the cursor unchanged (never
/// a second read for the cursor — AC-F2, the surface rule).
pub async fn wait<S: AttributedStore + ?Sized>(
    store: &S,
    id: ItemId,
    after_seq: u64,
    timeout: Duration,
) -> Result<Waited, job::JobError> {
    let deadline = Instant::now() + timeout.min(Duration::from_millis(MAX_WAIT_MS));
    loop {
        let rows = job::events_after(store, id, after_seq, 0)?;
        let row = job::get_job(store, id)?;
        if !rows.is_empty() {
            let (next_seq, gap) = job::cursor_after(&rows, after_seq);
            return Ok(Waited {
                job: row,
                events: rows,
                next_seq,
                timed_out: false,
                gap,
            });
        }
        if row.state.is_terminal() || Instant::now() >= deadline {
            let timed_out = !row.state.is_terminal();
            return Ok(Waited {
                job: row,
                events: Vec::new(),
                next_seq: after_seq,
                timed_out,
                gap: false,
            });
        }
        tokio::time::sleep(Duration::from_millis(WAIT_POLL_MS)).await;
    }
}

/// Wait until the job is terminal (tests, the CLI), streaming nothing.
pub async fn wait_until_done<S: AttributedStore + ?Sized>(
    store: &S,
    id: ItemId,
    timeout: Duration,
) -> Result<JobRow, job::JobError> {
    let deadline = Instant::now() + timeout;
    let mut cursor = 0;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let w = wait(store, id, cursor, left).await?;
        cursor = w.next_seq;
        if w.job.state.is_terminal() {
            return Ok(w.job);
        }
        if Instant::now() >= deadline {
            return Ok(w.job);
        }
    }
}

/// The result a finished job stored, parsed. `None` while it runs.
pub fn result_of(row: &JobRow) -> Option<Value> {
    row.result_json
        .as_deref()
        .and_then(|t| serde_json::from_str(t).ok())
}

/// What a CLI does with a verb's answer once it has it (`impress-cli`,
/// `imprint-cli`): a plain result passes through; a job handle is either
/// streamed (`--wait`: every event to stderr as it lands, the stored result
/// on stdout) or drained (the job must finish before this process exits,
/// so the CLI waits silently and prints the handle with its final state).
pub fn cli_finish<S: AttributedStore + ?Sized>(
    store: &S,
    value: Value,
    wait_and_stream: bool,
) -> Value {
    let Some(id) = JobStarted::id_of(&value)
        .and_then(|s| s.parse::<ItemId>().ok())
        .filter(|_| value.get("ok").and_then(Value::as_bool) == Some(true))
    else {
        runtime::block_on(drain_inline());
        return value;
    };
    let final_row = runtime::block_on(async {
        if wait_and_stream {
            let mut cursor = 0;
            loop {
                let w = match wait(store, id, cursor, Duration::from_millis(MAX_WAIT_MS)).await {
                    Ok(w) => w,
                    Err(e) => {
                        eprintln!("job {id}: {e}");
                        break None;
                    }
                };
                if w.gap {
                    eprintln!("job {id}: older events past seq {cursor} were pruned");
                }
                for e in &w.events {
                    eprintln!("[{}] {} {}", e.seq, e.name, e.payload_json);
                }
                cursor = w.next_seq;
                if w.job.state.is_terminal() && w.events.is_empty() {
                    break Some(w.job);
                }
            }
        } else {
            None
        }
    });
    runtime::block_on(drain_inline());
    let row = match final_row {
        Some(row) => row,
        None => match job::get_job(store, id) {
            Ok(row) => row,
            Err(_) => return value,
        },
    };
    if wait_and_stream {
        let result = result_of(&row).unwrap_or(Value::Null);
        let ok = row.state == TaskState::Done
            && result.get("ok").and_then(Value::as_bool).unwrap_or(true);
        json!({
            "ok": ok,
            "job": { "id": id.to_string(), "kind": row.verb, "state": row.state.as_str() },
            "result": result,
            "wire_version": impress_service_core::wire::WIRE_VERSION,
        })
    } else {
        eprintln!(
            "job {id} ran inline and is {}; pass --wait to stream it, or read it with \
             `job-result --id {id}`",
            row.state
        );
        let mut value = value;
        if let Some(state) = value.get_mut("job").and_then(|j| j.get_mut("state")) {
            *state = Value::String(row.state.as_str().into());
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Arc<koine_store_memory::MemoryStore> {
        Arc::new(koine_store_memory::MemoryStore::new())
    }

    /// The handle comes back before the work does; the work's events and
    /// its result are readable through the kernel; the CLI can drain it.
    #[tokio::test]
    async fn a_job_answers_at_once_and_finishes_behind_the_handle() {
        let store = store();
        let started = Instant::now();
        let handle = start_inline(
            store.clone(),
            "t-service_slow",
            &json!({"n": 3}),
            |ctx| async move {
                for i in 1..=3 {
                    ctx.progress("step", json!({"i": i}));
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                JobOutcome::Done(json!({"ok": true, "n": 3}))
            },
        );
        assert!(handle.ok, "{handle:?}");
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "{:?}",
            started.elapsed()
        );
        let id: ItemId = handle.job.as_ref().unwrap().id.parse().unwrap();

        let w = wait(&*store, id, 0, Duration::from_secs(5)).await.unwrap();
        assert!(!w.timed_out);
        assert_eq!(w.events[0].name, "step");
        let row = wait_until_done(&*store, id, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(row.state, TaskState::Done);
        assert_eq!(result_of(&row).unwrap()["n"], 3);
        let all = job::events_after(&*store, id, 0, 0).unwrap();
        assert_eq!(all.last().unwrap().name, "finished");
        assert_eq!(all.len(), 4);
        drain_inline().await;
        assert_eq!(inline_job_count(), 0);
    }

    #[tokio::test]
    async fn a_cancel_request_reaches_the_body_and_the_row_says_cancelled() {
        let store = store();
        let handle = start_inline(
            store.clone(),
            "t-service_loop",
            &json!({}),
            |ctx| async move {
                let mut ticks = 0;
                loop {
                    if ctx.cancel_requested() {
                        return JobOutcome::Cancelled(json!({"ok": false, "ticks": ticks}));
                    }
                    ticks += 1;
                    if ticks > 500 {
                        return JobOutcome::Done(json!({"ok": true, "ticks": ticks}));
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            },
        );
        let id: ItemId = handle.job.unwrap().id.parse().unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            job::request_cancel(&*store, id, "t").unwrap(),
            TaskState::Running
        );
        let row = wait_until_done(&*store, id, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(row.state, TaskState::Cancelled);
        assert!(result_of(&row).unwrap()["ticks"].as_u64().unwrap() < 500);
    }

    #[tokio::test]
    async fn a_panicking_body_fails_the_job_instead_of_leaving_it_running() {
        let store = store();
        let handle = start_inline(
            store.clone(),
            "t-service_boom",
            &json!({}),
            |_ctx| async move {
                panic!("boom");
            },
        );
        let id: ItemId = handle.job.unwrap().id.parse().unwrap();
        let row = wait_until_done(&*store, id, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(row.state, TaskState::Failed);
        assert!(row.error.unwrap().contains("panicked"));
    }

    #[tokio::test]
    async fn waiting_on_a_finished_job_returns_at_once_with_the_cursor_unchanged() {
        let store = store();
        let handle = start_inline(
            store.clone(),
            "t-service_quick",
            &json!({}),
            |_| async move { JobOutcome::Done(json!({"ok": true})) },
        );
        let id: ItemId = handle.job.unwrap().id.parse().unwrap();
        wait_until_done(&*store, id, Duration::from_secs(5))
            .await
            .unwrap();
        // The terminal row is written before its final event. Wait for that
        // event explicitly before treating max_seq as a fully consumed cursor.
        let finished_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let events = job::events_after(&*store, id, 0, 0).unwrap();
            if events.iter().any(|event| event.name == "finished") {
                break;
            }
            assert!(
                Instant::now() < finished_deadline,
                "terminal job never emitted its finished event"
            );
            tokio::task::yield_now().await;
        }
        let last = job::max_seq(&*store, id).unwrap();
        let started = Instant::now();
        let w = wait(&*store, id, last, Duration::from_secs(30))
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!w.timed_out);
        assert!(w.events.is_empty());
        assert_eq!(w.next_seq, last);
        assert_eq!(w.job.state, TaskState::Done);
    }

    #[test]
    fn the_cli_passes_a_plain_result_through() {
        let v = json!({"ok": true, "count": 2});
        assert_eq!(
            cli_finish(&koine_store_memory::MemoryStore::new(), v.clone(), false),
            v
        );
        assert_eq!(
            cli_finish(&koine_store_memory::MemoryStore::new(), v.clone(), true),
            v
        );
    }
}
