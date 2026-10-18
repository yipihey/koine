//! A verb is a task (docs/plan-koine-ai-tasks.md Q-D3).
//!
//! A `task@1.0.0` row with `task_kind: "koine.verb"` names a linked verb
//! (`verb`) and its arguments (`args`, JSON text, the job convention). The
//! scheduler acquires it like any task: ordered by `DependsOn`, deferred by
//! `next_attempt_at`, retried with backoff, cancelled by flag. This executor
//! runs the verb through the pipeline and writes its answer to `result`.
//!
//! The verb runs as the agent this executor was built with, never as the
//! person, whoever filed the task: a row in the store is not proof of who
//! wrote it, so scheduled work is never more trusted than an agent, and a
//! review policy that queues agent writes queues these. `filed_by` on the row
//! says who asked.

use async_trait::async_trait;
use impress_item::item::{ActorKind, Item, Value};
use impress_item::operation::{OperationIntent, OperationSpec, OperationType, RetentionTier};
use impress_service_core::call::{self, CallError};
use impress_service_core::pipeline::CallerIdentity;
use impress_service_core::refusal::codes;

use crate::executor::{ExecutionOutcome, TaskError, TaskExecutor};
use crate::job::field::task as field;
use crate::store::TaskStoreApi;

/// The `task_kind` of a verb task.
pub const VERB_TASK_KIND: &str = "koine.verb";

/// The agent a host's verb tasks run as when it names none.
pub const DEFAULT_AGENT: &str = "tasks";

/// Refusal codes that say "not now" rather than "never": the scheduler
/// retries these with backoff.
pub const RETRYABLE_CODES: &[&str] = &[
    codes::CONFLICT,
    codes::STORE_UNAVAILABLE,
    codes::HOST_UNAVAILABLE,
    codes::TIMED_OUT,
];

/// Runs `koine.verb` tasks.
pub struct VerbTaskExecutor {
    agent: String,
}

impl VerbTaskExecutor {
    /// Verb tasks run as `agent:<agent>`.
    pub fn new(agent: impl Into<String>) -> Self {
        Self {
            agent: agent.into(),
        }
    }

    /// The identity the verbs run as.
    pub fn caller(&self) -> CallerIdentity {
        CallerIdentity::agent(self.agent.clone())
    }
}

impl Default for VerbTaskExecutor {
    fn default() -> Self {
        Self::new(DEFAULT_AGENT)
    }
}

fn text(task: &Item, name: &str) -> Option<String> {
    match task.payload.get(name) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// A verb task's arguments: the `args` JSON text, or an object stored as
/// one; `{}` when absent.
pub fn args_of(task: &Item) -> Result<serde_json::Value, String> {
    match task.payload.get(field::ARGS) {
        None | Some(Value::Null) => Ok(serde_json::json!({})),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(serde_json::json!({})),
        Some(Value::String(s)) => {
            serde_json::from_str(s).map_err(|e| format!("args are not JSON: {e}"))
        }
        Some(other) => serde_json::to_value(other).map_err(|e| e.to_string()),
    }
}

/// `args` with a `once` key derived from the task (plan-koine-trust TR-D1):
/// a retried attempt answers from the first attempt's stored answer instead
/// of running the verb again. A key the filer chose is kept.
pub fn with_task_once(mut args: serde_json::Value, task: &Item) -> serde_json::Value {
    let Some(object) = args.as_object_mut() else {
        return args;
    };
    let adverbs = object
        .entry("adverbs")
        .or_insert_with(|| serde_json::json!({}));
    if let Some(adverbs) = adverbs.as_object_mut() {
        adverbs
            .entry("once")
            .or_insert_with(|| serde_json::Value::String(format!("task:{}", task.id)));
    }
    args
}

fn write_result(
    store: &dyn TaskStoreApi,
    task: &Item,
    answer: &serde_json::Value,
    actor: &str,
) -> Result<(), TaskError> {
    store.apply(OperationSpec {
        target_id: task.id,
        op_type: OperationType::SetPayload(field::RESULT.into(), Value::String(answer.to_string())),
        intent: OperationIntent::Routine,
        reason: None,
        batch_id: None,
        author: actor.into(),
        author_kind: ActorKind::Agent,
        retention: RetentionTier::Durable,
    })?;
    Ok(())
}

#[async_trait]
impl TaskExecutor for VerbTaskExecutor {
    fn task_kind(&self) -> &str {
        VERB_TASK_KIND
    }

    async fn execute(
        &self,
        task: &Item,
        store: &dyn TaskStoreApi,
    ) -> Result<ExecutionOutcome, TaskError> {
        let verb = text(task, field::VERB)
            .ok_or_else(|| TaskError::Permanent("the verb task names no verb".into()))?;
        let args = with_task_once(args_of(task).map_err(TaskError::Permanent)?, task);
        let author = self.caller().author();
        match call::call_async_as(&verb, self.caller(), args).await {
            Err(CallError::UnknownTool(name)) => Err(TaskError::Permanent(format!(
                "no verb named '{name}' is linked in this process"
            ))),
            Err(CallError::Unavailable(message)) => Err(TaskError::Retryable(message)),
            Err(CallError::Handler(message)) => Err(TaskError::Permanent(message)),
            Ok(answer) => {
                write_result(store, task, &answer, &author)?;
                if answer.get("ok") == Some(&serde_json::Value::Bool(false)) {
                    let code = answer["code"].as_str().unwrap_or(codes::VERB_FAILED);
                    let message = answer["message"].as_str().unwrap_or("refused");
                    let why = format!("{verb} refused ({code}): {message}");
                    return Err(if RETRYABLE_CODES.contains(&code) {
                        TaskError::Retryable(why)
                    } else {
                        TaskError::Permanent(why)
                    });
                }
                Ok(ExecutionOutcome::Complete)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task() -> Item {
        serde_json::from_value(json!({
            "id": "6f1c1f5e-5d4b-4b1a-9a3e-1d2c3b4a5f60",
            "schema": "task@1.0.0",
            "payload": {},
            "created": "2026-10-17T00:00:00Z",
            "author": "agent:x",
            "author_kind": "Agent",
            "logical_clock": 0,
            "origin": null,
            "canonical_id": null,
            "tags": [],
            "flag": null,
            "is_read": false,
            "is_starred": false,
            "priority": "normal",
            "visibility": "private",
            "message_type": null,
            "produced_by": null,
            "version": null,
            "batch_id": null,
            "references": [],
            "parent": null
        }))
        .unwrap()
    }

    #[test]
    fn a_verb_task_derives_its_once_key_from_the_task_and_keeps_the_filers() {
        let t = task();
        let derived = with_task_once(json!({"n": 1}), &t);
        assert_eq!(derived["adverbs"]["once"], format!("task:{}", t.id));
        assert_eq!(derived["n"], 1);
        let kept = with_task_once(json!({"adverbs": {"once": "mine", "within": 5}}), &t);
        assert_eq!(kept["adverbs"]["once"], "mine");
        assert_eq!(kept["adverbs"]["within"], 5);
    }
}
