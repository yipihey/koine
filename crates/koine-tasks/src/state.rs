//! The task state machine (ADR-0005 §2, ADR-0015 D1), moved from
//! `impress-core::task` (docs/plan-koine-ai-tasks.md Q-D1).
//!
//! This module is pure — no store access. It defines the five canonical
//! states, the allowed-transition table, and a validated constructor for
//! the `SetPayload("state", …)` operation that IS a state transition.
//! Stores expose a convenience `transition_task` built on these functions;
//! bridges from foreign vocabularies (impel's CounselEngine GRDB store)
//! enter through [`TaskState::parse_compat`].

use impress_item::item::{ActorId, ActorKind, Item, ItemId, Value};
use impress_item::operation::{OperationIntent, OperationSpec, OperationType, RetentionTier};

/// The canonical task states (ADR-0005 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskState {
    Pending,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl TaskState {
    /// Strict parse of the canonical vocabulary.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    /// Compat parse accepting the CounselEngine (GRDB) vocabulary in
    /// addition to the canonical one (ADR-0015 D1): `queued → pending`,
    /// `completed → done`. Bridges use this at the boundary; nothing
    /// inside the kernel ever emits the foreign strings.
    pub fn parse_compat(s: &str) -> Option<Self> {
        Self::parse(s).or(match s {
            "queued" => Some(Self::Pending),
            "completed" => Some(Self::Done),
            _ => None,
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Terminal states admit no further transitions.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

impl std::fmt::Display for TaskState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The ADR-0005 §2 allowed-transition table, verbatim:
/// `pending → running | cancelled`,
/// `running → done | failed | cancelled | pending` (the last is the retry
/// reset — it appears in the operation history as the retry ledger).
pub fn allowed_transition(from: TaskState, to: TaskState) -> bool {
    use TaskState::*;
    matches!(
        (from, to),
        (Pending, Running)
            | (Pending, Cancelled)
            | (Running, Done)
            | (Running, Failed)
            | (Running, Cancelled)
            | (Running, Pending)
    )
}

/// Why a transition was refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TaskTransitionError {
    #[error("item {0} has no 'state' payload field — not a task item")]
    NotATask(ItemId),
    #[error("item {item} has unparseable task state '{raw}'")]
    UnknownState { item: ItemId, raw: String },
    #[error("illegal task transition {from} → {to} on item {item}")]
    Illegal {
        item: ItemId,
        from: TaskState,
        to: TaskState,
    },
}

/// Build the validated `SetPayload("state", …)` operation that transitions
/// `task` to `to`. The returned spec is ready for `apply_operation`; this
/// function is the single source of transition legality.
///
/// Intent defaults follow ADR-0005 §3: `Routine` for normal transitions,
/// `Anomaly` for `→ failed`. Callers escalate retry-exhausted failures by
/// passing `Some(OperationIntent::Escalation)`.
pub fn transition_op(
    task: &Item,
    to: TaskState,
    author: ActorId,
    author_kind: ActorKind,
    intent_override: Option<OperationIntent>,
) -> Result<OperationSpec, TaskTransitionError> {
    let raw = match task.payload.get("state") {
        Some(Value::String(s)) => s.clone(),
        _ => return Err(TaskTransitionError::NotATask(task.id)),
    };
    let from = TaskState::parse_compat(&raw).ok_or_else(|| TaskTransitionError::UnknownState {
        item: task.id,
        raw: raw.clone(),
    })?;
    if !allowed_transition(from, to) {
        return Err(TaskTransitionError::Illegal {
            item: task.id,
            from,
            to,
        });
    }
    let intent = intent_override.unwrap_or(match to {
        TaskState::Failed => OperationIntent::Anomaly,
        _ => OperationIntent::Routine,
    });
    Ok(OperationSpec {
        target_id: task.id,
        op_type: OperationType::SetPayload("state".into(), Value::String(to.as_str().into())),
        intent,
        reason: None,
        batch_id: None,
        author,
        author_kind,
        // Task state transitions are routine infrastructure (ADR-0005 §9).
        retention: RetentionTier::Compactable,
    })
}

/// Delivery semantics for task-produced writes (ADR-0005 §8, ADR-0015 D4).
///
/// `FireAndForget` is the default write path. `ConfirmStored` requires the
/// write transaction to be committed before returning — with the SQLite
/// store every successful `insert`/`apply_operation` already commits, so
/// the variant is a documented promise rather than an extra fsync.
/// `AwaitHumanResponse` is realized by the scheduler: it writes a
/// `review-request` item linked `OperatesOn → task` and keeps the task in
/// `running` until the review's `resolution` payload field is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeliveryHint {
    #[default]
    FireAndForget,
    ConfirmStored,
    AwaitHumanResponse,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use impress_item::item::{Item, Priority, Visibility};
    use std::collections::BTreeMap;
    use uuid::Uuid;

    fn task_item(state: &str) -> Item {
        let mut payload = BTreeMap::new();
        payload.insert("title".to_string(), Value::String("t".into()));
        payload.insert("state".to_string(), Value::String(state.into()));
        Item {
            id: Uuid::new_v4(),
            schema: impress_item::schema::refs::TASK,
            payload,
            created: Utc::now(),
            modified: Utc::now(),
            author: "tester".into(),
            author_kind: ActorKind::Agent,
            logical_clock: 0,
            origin: None,
            canonical_id: None,
            tags: vec![],
            flag: None,
            is_read: false,
            is_starred: false,
            priority: Priority::Normal,
            visibility: Visibility::Private,
            message_type: None,
            produced_by: None,
            version: None,
            batch_id: None,
            references: vec![],
            parent: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn canonical_round_trip() {
        for s in ["pending", "running", "done", "failed", "cancelled"] {
            assert_eq!(TaskState::parse(s).unwrap().as_str(), s);
        }
    }

    #[test]
    fn compat_vocabulary_maps() {
        assert_eq!(TaskState::parse_compat("queued"), Some(TaskState::Pending));
        assert_eq!(TaskState::parse_compat("completed"), Some(TaskState::Done));
        assert_eq!(TaskState::parse("queued"), None);
    }

    #[test]
    fn transition_table_matches_adr_0005() {
        use TaskState::*;
        let allowed = [
            (Pending, Running),
            (Pending, Cancelled),
            (Running, Done),
            (Running, Failed),
            (Running, Cancelled),
            (Running, Pending),
        ];
        for from in [Pending, Running, Done, Failed, Cancelled] {
            for to in [Pending, Running, Done, Failed, Cancelled] {
                assert_eq!(
                    allowed_transition(from, to),
                    allowed.contains(&(from, to)),
                    "{from} → {to}"
                );
            }
        }
    }

    #[test]
    fn terminal_states_are_terminal() {
        use TaskState::*;
        for s in [Done, Failed, Cancelled] {
            assert!(s.is_terminal());
            for to in [Pending, Running, Done, Failed, Cancelled] {
                assert!(!allowed_transition(s, to));
            }
        }
    }

    #[test]
    fn transition_op_builds_setpayload() {
        let task = task_item("pending");
        let spec = transition_op(
            &task,
            TaskState::Running,
            "impel".into(),
            ActorKind::Agent,
            None,
        )
        .unwrap();
        assert_eq!(spec.target_id, task.id);
        assert_eq!(spec.intent, OperationIntent::Routine);
        assert_eq!(spec.retention, RetentionTier::Compactable);
        match spec.op_type {
            OperationType::SetPayload(ref f, Value::String(ref v)) => {
                assert_eq!(f, "state");
                assert_eq!(v, "running");
            }
            ref other => panic!("wrong op type: {other:?}"),
        }
    }

    #[test]
    fn transition_op_rejects_illegal() {
        let task = task_item("done");
        let err = transition_op(
            &task,
            TaskState::Running,
            "impel".into(),
            ActorKind::Agent,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, TaskTransitionError::Illegal { .. }));
    }

    #[test]
    fn failed_defaults_to_anomaly_intent() {
        let task = task_item("running");
        let spec = transition_op(
            &task,
            TaskState::Failed,
            "impel".into(),
            ActorKind::Agent,
            None,
        )
        .unwrap();
        assert_eq!(spec.intent, OperationIntent::Anomaly);
    }

    #[test]
    fn compat_state_in_item_is_transitionable() {
        // A bridge-written item carrying the GRDB vocabulary can still be
        // legally transitioned — the kernel reads it via parse_compat.
        let task = task_item("queued");
        let spec = transition_op(
            &task,
            TaskState::Running,
            "impel".into(),
            ActorKind::Agent,
            None,
        );
        assert!(spec.is_ok());
    }
}
