//! The task kernel over the store port (docs/plan-koine-ai-tasks.md).
//!
//! * [`state`]: the five states, the transition table, the one validated
//!   way to move a task (ADR-0005 §2, ADR-0015 D1).
//! * [`store`]: the port the scheduler and executors run against
//!   ([`TaskStoreApi`]) and its implementation over any store
//!   ([`ItemTaskStore`]; Q-D2).
//! * [`job`]: a long-running verb's handle and its event ring (Q-D4).
//! * With `runtime`: [`executor`], [`scheduler`], [`spawn`], the verb
//!   executor ([`verb`]; Q-D3) and the task verbs ([`service`]).
//!
//! The row shapes are the ones every writer already uses (`task@1.0.0`,
//! `agent-run@1.0.0`, `review-request@1.0.0`, `task-event@1.0.0`), so a
//! store that impress's daemon schedules and a koine host schedules are the
//! same store.

pub mod job;
pub mod state;
pub mod store;

#[cfg(feature = "runtime")]
pub mod dto;
#[cfg(feature = "runtime")]
pub mod executor;
#[cfg(feature = "runtime")]
pub mod inline;
#[cfg(feature = "runtime")]
pub mod review;
#[cfg(feature = "runtime")]
pub mod scheduler;
#[cfg(feature = "runtime")]
pub mod service;
#[cfg(feature = "runtime")]
pub mod spawn;
#[cfg(feature = "runtime")]
pub mod verb;

pub use state::{allowed_transition, transition_op, DeliveryHint, TaskState, TaskTransitionError};
pub use store::{
    kernel, ready_tasks_by_query, AgentRunRecord, ItemTaskStore, ReviewRequest, TaskStoreApi,
    TaskStoreError, AGENT_RUN_SCHEMA, EXECUTOR_DETERMINISTIC, EXECUTOR_MODEL,
    REVIEW_REQUEST_SCHEMA, TASK_EVENT_SCHEMA, TASK_SCHEMA,
};

#[cfg(feature = "runtime")]
pub use executor::{ExecutionOutcome, TaskError, TaskExecutor};
#[cfg(feature = "runtime")]
pub use scheduler::{PassReport, Scheduler, SchedulerConfig};
#[cfg(feature = "runtime")]
pub use service::{DefaultTaskService, TaskService};
#[cfg(feature = "runtime")]
pub use spawn::{
    create_task_dag, create_task_dag_from, SpawnError, SpawnProvenance, SpawnRule, TaskSpec,
};
#[cfg(feature = "runtime")]
pub use verb::{VerbTaskExecutor, VERB_TASK_KIND};

/// Keep the task verbs in a binary whose code names nothing else here.
#[cfg(feature = "runtime")]
pub fn ensure_linked() {
    std::hint::black_box(DefaultTaskService::new as *const ());
    std::hint::black_box(review::DefaultReviewService::new as *const ());
}
