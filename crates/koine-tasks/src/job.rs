//! Jobs: a long-running verb on the task kernel (ADR-0034 D6), moved from
//! `impress-core::job` and written against the store port
//! (docs/plan-koine-ai-tasks.md Q-D4).
//!
//! A `long_running` verb answers in milliseconds with a handle — the id of a
//! `task@1.0.0` row — and does its work afterwards, wherever it runs (the
//! verb's own process, or a daemon). This module is the store half of that
//! convention, and it is pure kernel: no runtime, no executor, no verb.
//!
//! * **The handle** is an ordinary task row, created already `running` and
//!   `assigned_to` its runner, so the scheduler (which acquires `pending`
//!   rows and resumes `running` rows assigned to *itself*) never touches
//!   it. Its `verb` and `args` say what started it.
//! * **Progress** is a per-task ring of `task-event@1.0.0` rows — the
//!   surface event ring's shape (`impress-surface-service`'s store: a
//!   gap-free `seq` derived into the row id, a bounded ring, pruning by
//!   `seq` bound), lifted here so a job and a surface are read the same way
//!   (cursor from the rows read, `gap` when the ring was pruned past it).
//! * **Cancel** is a flag on the row (`cancel_requested`). The kernel never
//!   interrupts a thread: the executor polls [`cancel_requested`] and moves
//!   the task `running → cancelled` itself, which ADR-0005 §2 already
//!   allows. `job_cancel` therefore always returns at once.
//! * **The result** is the verb's own JSON, stored on the row when the job
//!   finishes (`result`), beside the terminal state.
//!
//! The scheduler's own executors need none of this: their tasks stay the
//! kernel's, and `cancel_requested` is honoured on their resume pass too.

use std::collections::BTreeMap;

use chrono::Utc;
use impress_item::item::{ActorKind, Item, ItemId, Priority, Value, Visibility};
use impress_item::operation::{OperationIntent, OperationSpec, OperationType, RetentionTier};
use impress_item::query::{ItemQuery, Predicate, SortDescriptor};
use impress_item::store::{AttributedStore, StoreError};
use uuid::Uuid;

use crate::state::{transition_op, TaskState, TaskTransitionError};
use crate::store::{TASK_EVENT_SCHEMA, TASK_SCHEMA};

/// How many events a job keeps. The surface ring's number: what a live
/// reader needs to catch up, not an archive.
pub const EVENT_RING_CAPACITY: usize = 200;

/// Field names of the two kinds this module writes, spelled once.
pub mod field {
    pub mod task {
        pub const STATE: &str = "state";
        pub const TASK_KIND: &str = "task_kind";
        pub const ASSIGNED_TO: &str = "assigned_to";
        pub const ATTEMPTS: &str = "attempts";
        pub const CANCEL_REQUESTED: &str = "cancel_requested";
        pub const VERB: &str = "verb";
        pub const ARGS: &str = "args";
        pub const RESULT: &str = "result";
        pub const RUNNER: &str = "runner";
        pub const ERROR: &str = "error";
        pub const TITLE: &str = "title";
    }
    pub mod event {
        pub const TASK: &str = "task";
        pub const SEQ: &str = "seq";
        pub const NAME: &str = "name";
        pub const PAYLOAD: &str = "payload";
        pub const AT: &str = "at";
    }
}

#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Transition(#[from] TaskTransitionError),
    #[error("no task {0}")]
    NotFound(ItemId),
    #[error("{0} is a {1}, not a task")]
    NotATask(ItemId, String),
    #[error("append event '{0}': {1} writers took the next seq first")]
    SeqContended(String, usize),
    #[error("encode: {0}")]
    Encode(String),
}

pub type Result<T> = std::result::Result<T, JobError>;

/// A job as its row says it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRow {
    pub id: ItemId,
    pub verb: String,
    pub state: TaskState,
    pub cancel_requested: bool,
    /// The verb's result as JSON text, once the job finished.
    pub result_json: Option<String>,
    pub error: Option<String>,
    pub runner: Option<String>,
    pub created: chrono::DateTime<Utc>,
    pub modified: chrono::DateTime<Utc>,
}

/// One `task-event@1.0.0` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    pub seq: u64,
    pub name: String,
    /// JSON text — what was passed to [`append_event`].
    pub payload_json: String,
    pub at: String,
}

/// The name a job runs under in this process. Stamped as `assigned_to` and
/// `runner`, so the scheduler (which resumes only rows assigned to its own
/// actor) leaves it alone, and a reader can see where it ran.
pub fn inline_runner_name() -> String {
    let host = std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "local".into());
    format!("inline:{}@{host}", std::process::id())
}

/// Create the job's task row: `running`, assigned to `runner`, carrying the
/// verb and its arguments. Returns the handle.
///
/// Created `running` rather than `pending` + acquired: a `pending` row with
/// a `task_kind` is exactly what `ready_tasks` selects, and a daemon pass
/// between the two writes would acquire it and fail it for having no
/// executor.
pub fn create_job<S: AttributedStore + ?Sized>(
    store: &S,
    verb: &str,
    args: &serde_json::Value,
    runner: &str,
    author: &str,
) -> Result<ItemId> {
    let args_text = serde_json::to_string(args).map_err(|e| JobError::Encode(e.to_string()))?;
    let mut payload: BTreeMap<String, Value> = BTreeMap::new();
    payload.insert(field::task::TITLE.into(), Value::String(verb.to_string()));
    payload.insert(
        field::task::STATE.into(),
        Value::String(TaskState::Running.as_str().into()),
    );
    payload.insert(
        field::task::TASK_KIND.into(),
        Value::String(verb.to_string()),
    );
    payload.insert(
        field::task::ASSIGNED_TO.into(),
        Value::String(runner.to_string()),
    );
    payload.insert(
        field::task::RUNNER.into(),
        Value::String(runner.to_string()),
    );
    payload.insert(field::task::ATTEMPTS.into(), Value::Int(1));
    payload.insert(field::task::VERB.into(), Value::String(verb.to_string()));
    payload.insert(field::task::ARGS.into(), Value::String(args_text));
    payload.insert(field::task::CANCEL_REQUESTED.into(), Value::Bool(false));
    let now = Utc::now();
    let item = Item {
        id: Uuid::new_v4(),
        schema: TASK_SCHEMA,
        payload,
        created: now,
        modified: now,
        author: author.into(),
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
    };
    Ok(store.insert(item)?)
}

/// The job's row, or `NotFound` / `NotATask`.
pub fn get_job<S: AttributedStore + ?Sized>(store: &S, id: ItemId) -> Result<JobRow> {
    let item = store.get(id)?.ok_or(JobError::NotFound(id))?;
    if item.schema != TASK_SCHEMA {
        return Err(JobError::NotATask(id, item.schema.to_string()));
    }
    Ok(job_row_of(&item))
}

/// Read a task item as a job row. Any task reads as one — a job is a task
/// with `verb` set; a kernel task has no `verb` and answers `""`.
pub fn job_row_of(item: &Item) -> JobRow {
    JobRow {
        id: item.id,
        verb: string_field(item, field::task::VERB).unwrap_or_default(),
        state: string_field(item, field::task::STATE)
            .and_then(|s| TaskState::parse_compat(&s))
            .unwrap_or(TaskState::Pending),
        cancel_requested: matches!(
            item.payload.get(field::task::CANCEL_REQUESTED),
            Some(Value::Bool(true))
        ),
        result_json: string_field(item, field::task::RESULT),
        error: string_field(item, field::task::ERROR),
        runner: string_field(item, field::task::RUNNER),
        created: item.created,
        modified: item.modified,
    }
}

/// Ask a running job to stop. Sets `cancel_requested`; the executor moves
/// the row to `cancelled` when it next checks. Idempotent. A `pending` task
/// is cancelled outright (nothing is running it), a terminal one is refused
/// by the transition table. Returns the state the row is in afterwards.
pub fn request_cancel<S: AttributedStore + ?Sized>(
    store: &S,
    id: ItemId,
    actor: &str,
) -> Result<TaskState> {
    let item = store.get(id)?.ok_or(JobError::NotFound(id))?;
    if item.schema != TASK_SCHEMA {
        return Err(JobError::NotATask(id, item.schema.to_string()));
    }
    let row = job_row_of(&item);
    match row.state {
        TaskState::Pending => {
            let op = transition_op(
                &item,
                TaskState::Cancelled,
                actor.into(),
                ActorKind::Agent,
                None,
            )?;
            store.apply_operation(op)?;
            Ok(TaskState::Cancelled)
        }
        TaskState::Running => {
            if !row.cancel_requested {
                store.apply_operation(set_payload(
                    id,
                    field::task::CANCEL_REQUESTED,
                    Value::Bool(true),
                    actor,
                ))?;
            }
            Ok(TaskState::Running)
        }
        terminal => Err(JobError::Transition(TaskTransitionError::Illegal {
            item: id,
            from: terminal,
            to: TaskState::Cancelled,
        })),
    }
}

/// Has someone asked this job to stop? One store read — poll it between
/// steps, not in a tight loop.
pub fn cancel_requested<S: AttributedStore + ?Sized>(store: &S, id: ItemId) -> Result<bool> {
    Ok(get_job(store, id)?.cancel_requested)
}

/// Finish the job: the terminal transition plus the result (and, for a
/// failure, the error) in ONE batch, so a reader that sees `done` also sees
/// the result. `to` must be terminal.
pub fn finish_job<S: AttributedStore + ?Sized>(
    store: &S,
    id: ItemId,
    to: TaskState,
    result: &serde_json::Value,
    error: Option<&str>,
    actor: &str,
) -> Result<()> {
    debug_assert!(to.is_terminal(), "finish_job takes a terminal state");
    let item = store.get(id)?.ok_or(JobError::NotFound(id))?;
    let transition = transition_op(&item, to, actor.into(), ActorKind::Agent, None)?;
    let result_text = serde_json::to_string(result).map_err(|e| JobError::Encode(e.to_string()))?;
    let mut ops = vec![
        transition,
        set_payload(id, field::task::RESULT, Value::String(result_text), actor),
    ];
    if let Some(error) = error {
        ops.push(set_payload(
            id,
            field::task::ERROR,
            Value::String(error.to_string()),
            actor,
        ));
    }
    store.apply_operation_batch(ops)?;
    Ok(())
}

// ---------------------------------------------------------------- events

/// Append one event, assigning the next `seq` for the task, and prune the
/// ring to [`EVENT_RING_CAPACITY`]. Returns the assigned `seq`.
///
/// Two writers cannot share a `seq`: the row id is DERIVED from `(task,
/// seq)` ([`event_row_id`]) and the primary key refuses a second row under
/// it — in this process or any other on the same file — so the loser reads
/// the maximum again and takes the next number. `seq` stays unique and
/// gap-free, which is what lets a reader treat a jump as pruning.
pub fn append_event<S: AttributedStore + ?Sized>(
    store: &S,
    task: ItemId,
    name: &str,
    payload: &serde_json::Value,
    author: &str,
) -> Result<u64> {
    let payload_text =
        serde_json::to_string(payload).map_err(|e| JobError::Encode(e.to_string()))?;
    const MAX_ATTEMPTS: usize = 64;
    for _ in 0..MAX_ATTEMPTS {
        let seq = max_seq(store, task)? + 1;
        let mut fields: BTreeMap<String, Value> = BTreeMap::new();
        fields.insert(field::event::TASK.into(), Value::String(task.to_string()));
        fields.insert(field::event::SEQ.into(), Value::Int(seq as i64));
        fields.insert(field::event::NAME.into(), Value::String(name.to_string()));
        fields.insert(
            field::event::PAYLOAD.into(),
            Value::String(payload_text.clone()),
        );
        fields.insert(
            field::event::AT.into(),
            Value::String(Utc::now().to_rfc3339()),
        );
        let now = Utc::now();
        let item = Item {
            id: event_row_id(task, seq),
            schema: TASK_EVENT_SCHEMA,
            payload: fields,
            created: now,
            modified: now,
            author: author.into(),
            author_kind: ActorKind::Agent,
            logical_clock: 0,
            origin: None,
            canonical_id: None,
            tags: vec![],
            flag: None,
            is_read: false,
            is_starred: false,
            priority: Priority::None,
            visibility: Visibility::Private,
            message_type: None,
            produced_by: None,
            version: None,
            batch_id: None,
            references: vec![],
            parent: None,
        };
        match store.insert(item) {
            Ok(_) => {
                prune(store, task, seq)?;
                return Ok(seq);
            }
            Err(StoreError::AlreadyExists(_)) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(JobError::SeqContended(name.to_string(), MAX_ATTEMPTS))
}

/// Events of `task` with `seq > after`, oldest first, capped at `limit` (0 =
/// unbounded). ONE store read, filtered and ordered by the store — the
/// cursor a caller hands back comes from these rows, never a second read.
pub fn events_after<S: AttributedStore + ?Sized>(
    store: &S,
    task: ItemId,
    after: u64,
    limit: usize,
) -> Result<Vec<EventRow>> {
    let mut query = ring_rows(task);
    query.predicates.push(Predicate::Gt(
        field::event::SEQ.into(),
        Value::Int(after.min(i64::MAX as u64) as i64),
    ));
    query.sort = vec![SortDescriptor {
        field: format!("payload.{}", field::event::SEQ),
        ascending: true,
    }];
    if limit > 0 {
        query.limit = Some(limit);
    }
    Ok(store.query(&query)?.iter().map(event_row_of).collect())
}

/// The highest `seq` this task has emitted, 0 if none.
pub fn max_seq<S: AttributedStore + ?Sized>(store: &S, task: ItemId) -> Result<u64> {
    let mut query = ring_rows(task);
    query.sort = vec![SortDescriptor {
        field: format!("payload.{}", field::event::SEQ),
        ascending: false,
    }];
    query.limit = Some(1);
    Ok(store
        .query(&query)?
        .first()
        .and_then(|i| int_field(i, field::event::SEQ))
        .unwrap_or(0))
}

/// The cursor to hand back after reading `rows` past `after_seq`, and
/// whether the ring was pruned past the caller's cursor (events between it
/// and the first row are gone). From the rows read, never a second read.
pub fn cursor_after(rows: &[EventRow], after_seq: u64) -> (u64, bool) {
    match (rows.first(), rows.last()) {
        (Some(first), Some(last)) => (last.seq, after_seq > 0 && first.seq > after_seq + 1),
        _ => (after_seq, false),
    }
}

/// Drop every row that fell out of the last [`EVENT_RING_CAPACITY`] once
/// `newest` was written. Hard deletes: the ring exists so a live reader can
/// catch up, not as a log.
fn prune<S: AttributedStore + ?Sized>(store: &S, task: ItemId, newest: u64) -> Result<()> {
    let capacity = EVENT_RING_CAPACITY as u64;
    if newest <= capacity {
        return Ok(());
    }
    let mut query = ring_rows(task);
    query.predicates.push(Predicate::Lte(
        field::event::SEQ.into(),
        Value::Int((newest - capacity) as i64),
    ));
    for item in store.query(&query)? {
        store.delete(item.id)?;
    }
    Ok(())
}

/// Delete a job's whole ring (retention sweeps; tests).
pub fn delete_events<S: AttributedStore + ?Sized>(store: &S, task: ItemId) -> Result<usize> {
    let rows = store.query(&ring_rows(task))?;
    let n = rows.len();
    for item in rows {
        store.delete(item.id)?;
    }
    Ok(n)
}

/// Namespace for the derived event row ids. Fixed forever, like the surface
/// ring's: changing it would let a new build allocate an id an older build
/// already used.
const ROW_ID_NAMESPACE: Uuid = Uuid::from_u128(0x2c9e_7b41_5f0a_4d83_9e6b_1a7f_3d52_c0e8);

/// The one id event `seq` of `task` can be stored under.
pub fn event_row_id(task: ItemId, seq: u64) -> ItemId {
    Uuid::new_v5(
        &ROW_ID_NAMESPACE,
        format!("task-event|{task}|{seq}").as_bytes(),
    )
}

fn ring_rows(task: ItemId) -> ItemQuery {
    ItemQuery {
        schema: Some(TASK_EVENT_SCHEMA),
        predicates: vec![Predicate::Eq(
            field::event::TASK.into(),
            Value::String(task.to_string()),
        )],
        include_tags: false,
        include_references: false,
        assume_schema_rare: true,
        ..Default::default()
    }
}

fn set_payload(id: ItemId, field: &str, value: Value, actor: &str) -> OperationSpec {
    OperationSpec {
        target_id: id,
        op_type: OperationType::SetPayload(field.into(), value),
        intent: OperationIntent::Routine,
        reason: None,
        batch_id: None,
        author: actor.into(),
        author_kind: ActorKind::Agent,
        retention: RetentionTier::Compactable,
    }
}

fn event_row_of(item: &Item) -> EventRow {
    EventRow {
        seq: int_field(item, field::event::SEQ).unwrap_or(0),
        name: string_field(item, field::event::NAME).unwrap_or_default(),
        payload_json: string_field(item, field::event::PAYLOAD).unwrap_or_else(|| "{}".into()),
        at: string_field(item, field::event::AT).unwrap_or_default(),
    }
}

fn string_field(item: &Item, field: &str) -> Option<String> {
    match item.payload.get(field) {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn int_field(item: &Item, field: &str) -> Option<u64> {
    match item.payload.get(field) {
        Some(Value::Int(n)) if *n >= 0 => Some(*n as u64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use impress_item::store::ItemStore;
    use serde_json::json;

    fn store() -> koine_store_memory::MemoryStore {
        koine_store_memory::MemoryStore::new()
    }

    #[test]
    fn a_job_is_born_running_and_assigned_so_the_scheduler_ignores_it() {
        let s = store();
        let id = create_job(&s, "x-service_slow", &json!({"n": 1}), "inline:1@t", "t").unwrap();
        let row = get_job(&s, id).unwrap();
        assert_eq!(row.state, TaskState::Running);
        assert_eq!(row.verb, "x-service_slow");
        assert_eq!(row.runner.as_deref(), Some("inline:1@t"));
        assert!(!row.cancel_requested);
        // Not selectable by the scheduler: not pending…
        assert!(crate::store::ready_tasks_by_query(&s, 10)
            .unwrap()
            .is_empty());
        // …and not an orphan (it has an assignee).
        let item = s.get(id).unwrap().unwrap();
        assert!(
            matches!(item.payload.get("assigned_to"), Some(Value::String(a)) if a == "inline:1@t")
        );
    }

    #[test]
    fn events_are_gap_free_and_read_from_a_cursor() {
        let s = store();
        let id = create_job(&s, "v", &json!({}), "r", "t").unwrap();
        assert_eq!(max_seq(&s, id).unwrap(), 0);
        for i in 1..=3 {
            let seq = append_event(&s, id, "step", &json!({"i": i}), "t").unwrap();
            assert_eq!(seq, i);
        }
        let all = events_after(&s, id, 0, 0).unwrap();
        assert_eq!(all.iter().map(|e| e.seq).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(all[1].payload_json, r#"{"i":2}"#);
        assert_eq!(cursor_after(&all, 0), (3, false));
        let rest = events_after(&s, id, 2, 0).unwrap();
        assert_eq!(rest.len(), 1);
        assert_eq!(cursor_after(&rest, 2), (3, false));
        let none = events_after(&s, id, 3, 0).unwrap();
        assert!(none.is_empty());
        assert_eq!(cursor_after(&none, 3), (3, false));
        // Another job's ring is its own.
        let other = create_job(&s, "v", &json!({}), "r", "t").unwrap();
        assert!(events_after(&s, other, 0, 0).unwrap().is_empty());
    }

    #[test]
    fn the_ring_is_pruned_and_a_stale_cursor_reports_the_gap() {
        let s = store();
        let id = create_job(&s, "v", &json!({}), "r", "t").unwrap();
        let total = EVENT_RING_CAPACITY as u64 + 5;
        for _ in 0..total {
            append_event(&s, id, "tick", &json!({}), "t").unwrap();
        }
        let rows = events_after(&s, id, 0, 0).unwrap();
        assert_eq!(rows.len(), EVENT_RING_CAPACITY);
        assert_eq!(rows.first().unwrap().seq, 6);
        assert_eq!(rows.last().unwrap().seq, total);
        // A cursor at 2 lost 3..=5: gap.
        let from_two = events_after(&s, id, 2, 0).unwrap();
        assert_eq!(cursor_after(&from_two, 2), (total, true));
        // A cursor at 5 lost nothing (6 is the next).
        let from_five = events_after(&s, id, 5, 0).unwrap();
        assert_eq!(cursor_after(&from_five, 5), (total, false));
        assert_eq!(delete_events(&s, id).unwrap(), EVENT_RING_CAPACITY);
    }

    #[test]
    fn cancel_is_a_flag_on_a_running_job_and_a_transition_on_a_pending_task() {
        let s = store();
        let id = create_job(&s, "v", &json!({}), "r", "t").unwrap();
        assert!(!cancel_requested(&s, id).unwrap());
        assert_eq!(request_cancel(&s, id, "t").unwrap(), TaskState::Running);
        assert!(cancel_requested(&s, id).unwrap());
        // Idempotent.
        assert_eq!(request_cancel(&s, id, "t").unwrap(), TaskState::Running);
        // The executor finishes it.
        finish_job(
            &s,
            id,
            TaskState::Cancelled,
            &json!({"ok": false}),
            None,
            "t",
        )
        .unwrap();
        let row = get_job(&s, id).unwrap();
        assert_eq!(row.state, TaskState::Cancelled);
        assert_eq!(row.result_json.as_deref(), Some(r#"{"ok":false}"#));
        // Terminal → refused.
        assert!(matches!(
            request_cancel(&s, id, "t"),
            Err(JobError::Transition(TaskTransitionError::Illegal { .. }))
        ));
    }

    #[test]
    fn finish_writes_state_result_and_error_together() {
        let s = store();
        let id = create_job(&s, "v", &json!({}), "r", "t").unwrap();
        finish_job(
            &s,
            id,
            TaskState::Failed,
            &json!({"ok": false, "message": "boom"}),
            Some("boom"),
            "t",
        )
        .unwrap();
        let row = get_job(&s, id).unwrap();
        assert_eq!(row.state, TaskState::Failed);
        assert_eq!(row.error.as_deref(), Some("boom"));
        assert!(row.result_json.unwrap().contains("boom"));
    }

    #[test]
    fn a_non_task_row_is_refused_by_name() {
        let s = store();
        let id = create_job(&s, "v", &json!({}), "r", "t").unwrap();
        append_event(&s, id, "x", &json!({}), "t").unwrap();
        let event = event_row_id(id, 1);
        assert!(
            matches!(get_job(&s, event), Err(JobError::NotATask(_, k)) if k == TASK_EVENT_SCHEMA.as_str())
        );
        assert!(matches!(
            get_job(&s, Uuid::new_v4()),
            Err(JobError::NotFound(_))
        ));
    }
}
