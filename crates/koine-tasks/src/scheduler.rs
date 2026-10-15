//! The task scheduler — ADR-0005 §6's execution loop (ADR-0015 D5), moved from
//! `impel-core::task_scheduler` (docs/plan-koine-ai-tasks.md Q-D1).
//!
//! Responsibilities (and nothing more): acquire ready tasks, dispatch to
//! the registered [`TaskExecutor`], drive every state transition through
//! the kernel's validated path, manage retry with escalation on
//! exhaustion, and honor `AwaitHumanResponse` suspensions. Executors
//! never transition state; impress-core never sees execution concerns.
//!
//! Startup guard: callers construct with [`SchedulerConfig::start_delay`]
//! ≥ 90 s in app contexts (CLAUDE.md invariant — background services must
//! not mutate during launch settling). `run_once` itself is undelayed so
//! tests and CLIs can drive the loop directly.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::state::TaskState;
use impress_item::item::{Item, Value};
use impress_item::operation::{OperationIntent, OperationSpec, OperationType, RetentionTier};
use impress_item::reference::EdgeType;

use crate::executor::{ExecutionOutcome, TaskError, TaskExecutor};
use crate::store::{TaskStoreApi, TaskStoreError};

/// Scheduler tuning.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// Actor id written as `assigned_to` and as operation author.
    pub actor: String,
    /// Max tasks acquired per `run_once` pass.
    pub batch: usize,
    /// Delay before the first pass of `run` (the 90-second guard).
    pub start_delay: Duration,
    /// Polling interval between passes of `run`.
    pub poll_interval: Duration,
    /// Base of the exponential retry backoff (`base · 3^(attempt−1)`,
    /// capped at 30 minutes) stamped as `next_attempt_at` on every retry.
    /// 0 disables the delay — tests drive `run_once` in a tight loop.
    pub retry_base_ms: i64,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            actor: "impel".into(),
            batch: 8,
            start_delay: Duration::from_secs(90),
            poll_interval: Duration::from_secs(5),
            retry_base_ms: 45_000,
        }
    }
}

/// Summary of one scheduler pass (for logs/tests).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PassReport {
    pub acquired: usize,
    pub completed: usize,
    pub suspended: usize,
    pub resumed: usize,
    pub retried: usize,
    pub failed: usize,
    /// Ready tasks NOT acquired because their kind's `readiness()` probe
    /// failed — they stay `pending`, burning no attempts.
    pub deferred: usize,
    /// Wedged `running`-without-assignee tasks adopted back to `pending`.
    pub adopted: usize,
    /// Pending dependents cancelled because an upstream task failed.
    pub cancelled: usize,
}

/// Retry backoff (ADR-0005 §9, ADR-0015 D5 — a port of the SEMANTICS of
/// `imbib_core::enrichment::retry::RetryPolicy`, kept local so the kernel
/// does not depend on an app core crate): base · 3^(n−1), capped at 30
/// minutes. At the default 45s base: attempt 1 → 45s, 2 → 2m15s,
/// 3 → 6m45s, 4 → 20m15s, 5+ → 30m. No jitter: one daemon per store
/// (WorkerLease) means no thundering herd.
fn retry_backoff_ms(attempt: u32, base_ms: i64) -> i64 {
    const CAP_MS: i64 = 30 * 60 * 1000;
    let exp = attempt.saturating_sub(1).min(8);
    base_ms
        .saturating_mul(3_i64.saturating_pow(exp))
        .min(CAP_MS)
}

/// How long a failed `readiness()` verdict is trusted before re-probing.
const READINESS_NEGATIVE_CACHE: Duration = Duration::from_secs(60);

/// kind → (probed_at, verdict) for the per-pass readiness gate.
type ReadinessCache = std::sync::Mutex<HashMap<String, (std::time::Instant, Result<(), String>)>>;

pub struct Scheduler {
    store: Arc<dyn TaskStoreApi>,
    executors: HashMap<String, Arc<dyn TaskExecutor>>,
    config: SchedulerConfig,
    /// Ok verdicts are re-checked every pass boundary anyway (probes are
    /// cheap when healthy); failed verdicts are held for
    /// `READINESS_NEGATIVE_CACHE` so a down provider costs one probe a
    /// minute, not one per pass.
    readiness: ReadinessCache,
}

impl Scheduler {
    pub fn new(store: Arc<dyn TaskStoreApi>, config: SchedulerConfig) -> Self {
        Self {
            store,
            executors: HashMap::new(),
            config,
            readiness: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Register an executor for its `task_kind`. Last registration wins.
    pub fn register(&mut self, executor: Arc<dyn TaskExecutor>) {
        self.executors
            .insert(executor.task_kind().to_string(), executor);
    }

    /// One full pass: adopt orphans, resume suspended tasks whose reviews
    /// resolved, then acquire + execute ready tasks (skipping kinds whose
    /// `readiness()` probe fails). Returns what happened.
    pub async fn run_once(&self) -> Result<PassReport, TaskStoreError> {
        let mut report = PassReport::default();

        // ── Adoption pass: heal running-without-assignee wedges ────────
        // A crash (or, before acquires were atomic, a mid-acquire BUSY)
        // could strand a task `running` with no `assigned_to` — invisible
        // to both `ready_tasks` and `running_tasks`. Re-pend it.
        for task in self.store.orphaned_running_tasks()? {
            if self
                .store
                .transition(task.id, TaskState::Pending, &self.config.actor, None)
                .is_ok()
            {
                report.adopted += 1;
            }
        }

        // ── Resume pass: running tasks assigned to us ──────────────────
        // The suspension set is derived in ONE query (unresolved reviews →
        // their OperatesOn targets) instead of one reviews_for per task.
        // A running task in the set is suspended — skip. Otherwise
        // re-execute: executors re-derive state from the graph (resolved
        // review, or a crash left it running).
        let suspended_targets = self.store.unresolved_review_targets()?;
        for task in self.store.running_tasks(&self.config.actor)? {
            // ADR-0034 D6: a cancel asked for while the task ran (or while
            // it sat suspended on a review) is honoured before any re-run.
            if matches!(
                task.payload.get("cancel_requested"),
                Some(Value::Bool(true))
            ) {
                self.store.transition(
                    task.id,
                    TaskState::Cancelled,
                    &self.config.actor,
                    Some(OperationIntent::Routine),
                )?;
                report.cancelled += 1;
                continue;
            }
            if suspended_targets.contains(&task.id) {
                report.suspended += 1;
                continue;
            }
            let (_, resolved) = self.store.reviews_for(task.id)?;
            if !resolved.is_empty() {
                report.resumed += 1;
            }
            self.execute_and_finalize(&task, &mut report).await?;
        }

        // ── Acquire pass ───────────────────────────────────────────────
        // Readiness gate (per KIND, not per task): an executor whose
        // dependency is provably down defers its whole kind — tasks stay
        // `pending`, burn no attempts, and wake when a later probe passes.
        for task in self.store.ready_tasks(self.config.batch)? {
            let kind = task_kind(&task);
            if let Err(_reason) = self.kind_readiness(&kind).await {
                report.deferred += 1;
                continue;
            }
            // An executor that works ON something cannot work on nothing.
            // Deleting a publication cascades away the `OperatesOn` edge of
            // every task aimed at it, and those tasks stay `pending` looking
            // perfectly runnable. Dispatching one burns an attempt and files a
            // permanent failure for work that was never possible — cancel it
            // here instead, where the fact is already in hand.
            if self.is_orphaned(&kind, &task) {
                self.store.transition(
                    task.id,
                    TaskState::Cancelled,
                    &self.config.actor,
                    Some(OperationIntent::Routine),
                )?;
                report.cancelled += 1;
                continue;
            }
            // Atomic: transition + assigned_to + attempts in one
            // transaction — a BUSY here leaves the task cleanly `pending`.
            self.store.acquire_task(&task, &self.config.actor)?;
            report.acquired += 1;
            // Re-fetch: acquire wrote state/assigned_to/attempts.
            let task = self
                .store
                .get_item(task.id)?
                .ok_or(TaskStoreError::TaskNotFound(task.id))?;
            self.execute_and_finalize(&task, &mut report).await?;
        }
        Ok(report)
    }

    /// Whether this task's executor needs an `OperatesOn` target the task no
    /// longer has. An unknown kind is never orphaned — `execute_and_finalize`
    /// escalates it properly, and guessing here would cancel work whose
    /// executor simply is not registered in this process.
    fn is_orphaned(&self, kind: &str, task: &Item) -> bool {
        self.executors
            .get(kind)
            .is_some_and(|executor| executor.requires_operates_on())
            && !task
                .references
                .iter()
                .any(|r| r.edge_type == EdgeType::OperatesOn)
    }

    /// Cached `readiness()` verdict for one executor kind. Unknown kinds
    /// report ready — `execute_and_finalize` escalates them properly.
    async fn kind_readiness(&self, kind: &str) -> Result<(), String> {
        let Some(executor) = self.executors.get(kind) else {
            return Ok(());
        };
        if let Ok(cache) = self.readiness.lock() {
            if let Some((probed_at, verdict)) = cache.get(kind) {
                if verdict.is_err() && probed_at.elapsed() < READINESS_NEGATIVE_CACHE {
                    return verdict.clone();
                }
            }
        }
        let verdict = executor.readiness().await;
        if let Ok(mut cache) = self.readiness.lock() {
            cache.insert(
                kind.to_string(),
                (std::time::Instant::now(), verdict.clone()),
            );
        }
        verdict
    }

    /// The production loop: waits `start_delay`, then polls forever.
    pub async fn run(&self) -> Result<(), TaskStoreError> {
        tokio::time::sleep(self.config.start_delay).await;
        loop {
            let _ = self.run_once().await?;
            tokio::time::sleep(self.config.poll_interval).await;
        }
    }

    // ── internals ──────────────────────────────────────────────────────

    async fn execute_and_finalize(
        &self,
        task: &Item,
        report: &mut PassReport,
    ) -> Result<(), TaskStoreError> {
        let kind = task_kind(task);
        let Some(executor) = self.executors.get(&kind) else {
            // No executor for this kind: not our task — leave it running?
            // No: a task WE acquired but cannot run is a permanent failure
            // with escalation (misconfiguration needs human eyes).
            self.store.transition(
                task.id,
                TaskState::Failed,
                &self.config.actor,
                Some(OperationIntent::Escalation),
            )?;
            self.set_error(task.id, &format!("no executor registered for '{kind}'"))?;
            report.failed += 1;
            return Ok(());
        };

        // Backstop against a wedged executor (a black-holed connection, a
        // hung subprocess): no single task may stall the sequential pass
        // loop forever. Generous — the longest legitimate work (a full
        // oMLX generation, an ONNX embed batch) finishes well inside it.
        const EXECUTOR_TIMEOUT: Duration = Duration::from_secs(20 * 60);
        let outcome = match tokio::time::timeout(
            EXECUTOR_TIMEOUT,
            executor.execute(task, self.store.as_ref()),
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(_elapsed) => Err(TaskError::Retryable(format!(
                "executor '{kind}' timed out after {}s",
                EXECUTOR_TIMEOUT.as_secs()
            ))),
        };
        match outcome {
            Ok(ExecutionOutcome::Complete) => {
                self.store
                    .transition(task.id, TaskState::Done, &self.config.actor, None)?;
                report.completed += 1;
            }
            Ok(ExecutionOutcome::Suspended) => {
                // Stays running; the review item is the suspension record.
                report.suspended += 1;
            }
            Err(err) => {
                let attempts = payload_i64(task, "attempts").unwrap_or(1) as u32;
                if executor.is_retryable(&err) && attempts <= executor.max_retries() {
                    // Retry with exponential backoff (ADR-0005 §9): stamp
                    // the not-before that `ready_tasks` honors, then reset
                    // running → pending — visible in the op history as the
                    // retry ledger (ADR-0005 §2). Without the stamp the
                    // oldest doomed tasks re-sorted to the FRONT of the
                    // queue every 5s pass and monopolized the batch.
                    let not_before = chrono::Utc::now().timestamp_millis()
                        + retry_backoff_ms(attempts, self.config.retry_base_ms);
                    self.store.apply(OperationSpec {
                        target_id: task.id,
                        op_type: OperationType::SetPayload(
                            "next_attempt_at".into(),
                            Value::Int(not_before),
                        ),
                        intent: OperationIntent::Routine,
                        reason: None,
                        batch_id: None,
                        author: self.config.actor.clone(),
                        author_kind: impress_item::item::ActorKind::Agent,
                        retention: RetentionTier::Compactable,
                    })?;
                    self.store
                        .transition(task.id, TaskState::Pending, &self.config.actor, None)?;
                    report.retried += 1;
                } else {
                    let intent = if executor.is_retryable(&err) {
                        // Retries exhausted → escalate (ADR-0005 §3).
                        Some(OperationIntent::Escalation)
                    } else {
                        None // transition_op defaults → Anomaly for failed
                    };
                    self.store.transition(
                        task.id,
                        TaskState::Failed,
                        &self.config.actor,
                        intent,
                    )?;
                    self.set_error(task.id, &err.to_string())?;
                    report.failed += 1;
                    // Failure propagation (ADR-0005 §4): a dependent whose
                    // prerequisite terminally failed can never become
                    // ready — without this it sat `pending` forever as an
                    // invisible zombie. Cancel the whole downstream chain.
                    self.cancel_dependents(task.id, report)?;
                }
            }
        }
        Ok(())
    }

    /// Cancel every PENDING transitive dependent of `failed_task`. Running
    /// dependents (impossible while the dep gate holds, but defensive) are
    /// left to finish; terminal ones are already settled.
    fn cancel_dependents(
        &self,
        failed_task: impress_item::item::ItemId,
        report: &mut PassReport,
    ) -> Result<(), TaskStoreError> {
        let mut frontier = vec![failed_task];
        let mut seen = std::collections::HashSet::new();
        while let Some(upstream) = frontier.pop() {
            if !seen.insert(upstream) {
                continue;
            }
            for dependent in self.store.dependents_of(upstream)? {
                let is_pending = matches!(dependent.payload.get("state"),
                                          Some(Value::String(s))
                                              if TaskState::parse_compat(s)
                                                  == Some(TaskState::Pending));
                if is_pending
                    && self
                        .store
                        .transition(dependent.id, TaskState::Cancelled, &self.config.actor, None)
                        .is_ok()
                {
                    self.set_error(
                        dependent.id,
                        &format!("cancelled: dependency {upstream} failed"),
                    )?;
                    report.cancelled += 1;
                }
                frontier.push(dependent.id);
            }
        }
        Ok(())
    }

    fn set_error(
        &self,
        task_id: impress_item::item::ItemId,
        msg: &str,
    ) -> Result<(), TaskStoreError> {
        self.store.apply(OperationSpec {
            target_id: task_id,
            op_type: OperationType::SetPayload("error".into(), Value::String(msg.into())),
            intent: OperationIntent::Anomaly,
            reason: None,
            batch_id: None,
            author: self.config.actor.clone(),
            author_kind: impress_item::item::ActorKind::Agent,
            retention: RetentionTier::Durable,
        })?;
        Ok(())
    }
}

fn task_kind(task: &Item) -> String {
    match task.payload.get("task_kind") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => match task.payload.get("title") {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        },
    }
}

fn payload_i64(task: &Item, field: &str) -> Option<i64> {
    match task.payload.get(field) {
        Some(Value::Int(i)) => Some(*i),
        _ => None,
    }
}
