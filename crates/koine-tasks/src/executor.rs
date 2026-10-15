//! `TaskExecutor` — the ADR-0005 §7 execution contract (ADR-0015 D5), moved
//! from `impel-core::task_executor` (docs/plan-koine-ai-tasks.md Q-D1).
//!
//! Replaces the earlier synchronous string-in/string-out stub (which had
//! no implementors). One refinement over the ADR's literal signature:
//! `execute` returns [`ExecutionOutcome`] instead of `()`, because the
//! §8 `AwaitHumanResponse` checkpoint needs a way for an executor to say
//! "I opened a review and am suspending" that `Ok(())` cannot express.
//! The scheduler treats `Suspended` by leaving the task `running`
//! (ADR-0005 §8) and re-invoking `execute` after the review resolves —
//! executors re-derive their state from the graph on resume (ADR-0005
//! "Mitigations": re-run task setup rather than serialize executor state).

use async_trait::async_trait;

use impress_item::item::Item;

use crate::store::TaskStoreApi;

/// What an executor's run concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionOutcome {
    /// Work finished; the scheduler transitions the task to `done`.
    Complete,
    /// A `review-request` was opened (via `TaskStoreApi::open_review`);
    /// the scheduler leaves the task `running` and re-executes after the
    /// review's `resolution` is set.
    Suspended,
}

/// Execution failure, split by retryability (ADR-0005 §7).
#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    /// Transient — the scheduler may retry (`running → pending` reset,
    /// visible in the operation history as the retry ledger).
    #[error("retryable: {0}")]
    Retryable(String),
    /// Permanent — no retry; the task fails immediately.
    #[error("permanent: {0}")]
    Permanent(String),
    /// Store access failed while executing.
    #[error(transparent)]
    Store(#[from] crate::store::TaskStoreError),
}

/// Trait implemented by each task type handler (ADR-0005 §7).
#[async_trait]
pub trait TaskExecutor: Send + Sync {
    /// The `task_kind` this executor handles (matched against the task
    /// item's `task_kind` payload field, falling back to `title`).
    fn task_kind(&self) -> &str;

    /// Is this executor's external dependency reachable RIGHT NOW?
    ///
    /// Consulted once per kind per scheduler pass (with a negative cache)
    /// BEFORE any task of the kind is acquired. `Err(reason)` defers the
    /// whole kind: its tasks stay `pending`, burn no attempts, write no
    /// state, and become eligible again the moment a later probe passes.
    /// This is the "aware of its own state" contract: an executor whose
    /// sole dependency (a local model server, a required app) is provably
    /// down must say so here instead of failing tasks one by one.
    /// Default: always ready (pure/heuristic executors).
    async fn readiness(&self) -> Result<(), String> {
        Ok(())
    }

    /// Does this executor need the task's `OperatesOn` target to exist?
    ///
    /// The scheduler checks this BEFORE acquiring, and cancels a task whose
    /// required target is gone instead of dispatching it. `false` (the
    /// default) is for sweep-shaped executors that operate on a window rather
    /// than an item — the memory embed and consolidation passes carry no
    /// target at all, and a blanket rule would cancel every one of them.
    ///
    /// Why it exists: deleting a publication cascades away the `OperatesOn`
    /// edge of every task pointing at it, and the tasks stay `pending`. The
    /// scheduler dispatched them, the executor discovered the missing target,
    /// and each one became a permanent failure — 1,694 of them on the live
    /// store, all reading `permanent: task has no OperatesOn target`, all
    /// unrunnable from the moment the paper was deleted. Cancelled is what
    /// they are; failed is a claim that something went wrong.
    fn requires_operates_on(&self) -> bool {
        false
    }

    /// Execute the task. Output items are written via `store`; the
    /// executor must NOT transition task state — that is the scheduler's
    /// job (ADR-0005 §7).
    async fn execute(
        &self,
        task: &Item,
        store: &dyn TaskStoreApi,
    ) -> Result<ExecutionOutcome, TaskError>;

    /// Maximum retry attempts before the failure escalates
    /// (`OperationIntent::Escalation`).
    fn max_retries(&self) -> u32 {
        3
    }

    /// Whether `error` is worth retrying. Default: retry `Retryable` only.
    fn is_retryable(&self, error: &TaskError) -> bool {
        matches!(error, TaskError::Retryable(_))
    }
}
