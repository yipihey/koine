//! `task-service`: schedule a verb and watch it run
//! (docs/plan-koine-ai-tasks.md Q-D3).
//!
//! `enqueue` files a `koine.verb` task; whichever scheduler runs over this
//! store (a host started with `--tasks`, impress's `impel-taskd`) acquires
//! it, runs the verb as its agent and records the answer. `list`, `show`,
//! `cancel` and `retry` read and steer every task, verb task or not.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::Utc;
use impress_item::item::{ActorKind, Item, ItemId, Priority, Value, Visibility};
use impress_item::query::{ItemQuery, Predicate, SortDescriptor};
use impress_item::reference::{EdgeType, TypedReference};
use impress_item::store::AttributedStore;
use impress_service_core::async_trait;
use impress_service_core::pipeline::{context as call_context, CallerIdentity};
use impress_service_core::refusal::codes;
#[allow(unused_imports)]
use impress_service_macros::impress_method;
use impress_service_macros::{impress_service, impress_service_impl};
use uuid::Uuid;

use crate::dto::*;
use crate::job::{self, field::task as field};
use crate::state::TaskState;
use crate::store::TASK_SCHEMA;
use crate::verb::VERB_TASK_KIND;

/// The most rows `list` answers.
pub const MAX_ROWS: usize = 500;

#[impress_service]
pub trait TaskService: Send + Sync + 'static {
    /// Schedule `verb` with `args` as a task. A scheduler over this store
    /// runs it as its agent (never as the person), after every task in
    /// `depends_on` is done and not before `not_before_ms`, retrying a
    /// refusal that says "not now" (conflict, an unavailable store or app,
    /// a timeout) with backoff. Refuses a verb this process does not link.
    #[impress_method(
        safety = mutating,
        effects(reads = ["task@1.0.0"], writes = ["task@1.0.0"])
    )]
    #[impress_example(
        name = "default",
        args = r#"{"verb": "task-service_list", "args": {"state": "done"}}"#,
        expect = r#"{"ok": true}"#
    )]
    async fn enqueue(
        &self,
        verb: String,
        args: Option<serde_json::Value>,
        depends_on: Option<Vec<String>>,
        not_before_ms: Option<i64>,
        title: Option<String>,
    ) -> TaskResult;

    /// Tasks, newest first: every kind, or only `kind` (`koine.verb` for
    /// verb tasks), only in `state`; at most `limit` (default 100, at most
    /// 500).
    #[impress_method]
    #[impress_example(
        name = "default",
        args = r#"{"state": "pending"}"#,
        expect = r#"{"ok": true}"#
    )]
    async fn list(
        &self,
        state: Option<String>,
        kind: Option<String>,
        limit: Option<u32>,
    ) -> TasksResult;

    /// One task: its state, its verb and arguments, its answer or error,
    /// its attempts and what it waits for.
    #[impress_method]
    #[impress_example(
        name = "missing",
        args = r#"{"id": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn show(&self, id: String) -> TaskResult;

    /// Stop a task: a pending one is cancelled at once; a running one is
    /// asked to stop (its scheduler cancels it at the next check). A
    /// finished task is refused.
    #[impress_method(
        safety = mutating,
        effects(reads = ["task@1.0.0"], writes = ["task@1.0.0"])
    )]
    #[impress_example(
        name = "missing",
        args = r#"{"id": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn cancel(&self, id: String) -> TaskResult;

    /// File a failed or cancelled verb task again, as a new task naming the
    /// old one (`retry_of`). A finished task never runs twice in place.
    #[impress_method(
        safety = mutating,
        effects(reads = ["task@1.0.0"], writes = ["task@1.0.0"])
    )]
    #[impress_example(
        name = "missing",
        args = r#"{"id": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn retry(&self, id: String) -> TaskResult;
}

pub struct DefaultTaskService;

impl DefaultTaskService {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DefaultTaskService {
    fn default() -> Self {
        Self::new()
    }
}

fn caller() -> CallerIdentity {
    call_context::current()
        .map(|c| c.caller.clone())
        .unwrap_or(CallerIdentity::Person)
}

fn actor_kind(caller: &CallerIdentity) -> ActorKind {
    match caller {
        CallerIdentity::Person | CallerIdentity::App(_) => ActorKind::Human,
        CallerIdentity::Agent(_) => ActorKind::Agent,
        _ => ActorKind::System,
    }
}

fn refuse(code: &str, message: impl Into<String>) -> TaskResult {
    TaskResult {
        ok: false,
        code: Some(code.into()),
        message: Some(message.into()),
        ..Default::default()
    }
}

/// A refusal on its way to an answer.
struct Refused {
    code: &'static str,
    message: String,
}

impl From<Refused> for TaskResult {
    fn from(refused: Refused) -> Self {
        refuse(refused.code, refused.message)
    }
}

impl From<Refused> for TasksResult {
    fn from(refused: Refused) -> Self {
        TasksResult {
            ok: false,
            code: Some(refused.code.into()),
            message: Some(refused.message),
            ..Default::default()
        }
    }
}

/// The store the task verbs use; a write to a stand-in is refused.
fn store(for_write: bool) -> Result<Arc<dyn AttributedStore>, Refused> {
    let (store, fallback) = koine_store::process::acquire_or_stand_in();
    if for_write && fallback {
        return Err(Refused {
            code: codes::STORE_UNAVAILABLE,
            message:
                "no store is installed in this process; tasks need a host (nothing was written)"
                    .into(),
        });
    }
    Ok(store)
}

fn parse_id(id: &str) -> Result<ItemId, Refused> {
    Uuid::parse_str(id.trim()).map_err(|_| Refused {
        code: codes::INVALID_ARGUMENT,
        message: format!("'{id}' is not a task id"),
    })
}

fn text(item: &Item, name: &str) -> Option<String> {
    match item.payload.get(name) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn json_text(item: &Item, name: &str) -> Option<serde_json::Value> {
    text(item, name).map(|s| serde_json::from_str(&s).unwrap_or(serde_json::Value::String(s)))
}

/// A task row as the verbs show it.
pub fn task_row(item: &Item) -> TaskRow {
    let int = |name: &str| match item.payload.get(name) {
        Some(Value::Int(n)) => Some(*n),
        _ => None,
    };
    TaskRow {
        id: item.id.to_string(),
        title: text(item, field::TITLE).unwrap_or_default(),
        kind: text(item, field::TASK_KIND).unwrap_or_default(),
        state: text(item, field::STATE)
            .map(|s| {
                TaskState::parse_compat(&s)
                    .map(|state| state.as_str().to_string())
                    .unwrap_or(s)
            })
            .unwrap_or_default(),
        verb: text(item, field::VERB),
        args: json_text(item, field::ARGS),
        result: json_text(item, field::RESULT),
        error: text(item, field::ERROR),
        attempts: int(field::ATTEMPTS).unwrap_or(0),
        filed_by: text(item, "filed_by"),
        assigned_to: text(item, field::ASSIGNED_TO),
        next_attempt_at: int("next_attempt_at").filter(|at| *at > 0),
        cancel_requested: matches!(
            item.payload.get(field::CANCEL_REQUESTED),
            Some(Value::Bool(true))
        ),
        depends_on: item
            .references
            .iter()
            .filter(|r| r.edge_type == EdgeType::DependsOn)
            .map(|r| r.target.to_string())
            .collect(),
        retry_of: text(item, "retry_of"),
        created: item.created.to_rfc3339(),
        modified: item.modified.to_rfc3339(),
    }
}

/// What `enqueue` files.
pub struct VerbTaskDraft {
    pub verb: String,
    pub args: serde_json::Value,
    pub depends_on: Vec<ItemId>,
    pub not_before_ms: Option<i64>,
    pub title: Option<String>,
    pub retry_of: Option<ItemId>,
}

/// File a `koine.verb` task in `store`, as `filed_by`. The dependencies
/// must exist; the verb is not checked here (a host may file work for a
/// scheduler in another process).
pub fn file_verb_task(
    store: &dyn AttributedStore,
    draft: VerbTaskDraft,
    filed_by: &CallerIdentity,
) -> Result<Item, (String, String)> {
    for dep in &draft.depends_on {
        match store.get(*dep) {
            Ok(Some(item)) if item.schema == TASK_SCHEMA => {}
            Ok(_) => {
                return Err((
                    codes::NOT_FOUND.into(),
                    format!("depends_on names {dep}, which is not a task"),
                ))
            }
            Err(e) => return Err((codes::STORE_ERROR.into(), e.to_string())),
        }
    }
    let now = Utc::now();
    let mut payload: BTreeMap<String, Value> = BTreeMap::new();
    payload.insert(
        field::TITLE.into(),
        Value::String(draft.title.unwrap_or_else(|| draft.verb.clone())),
    );
    payload.insert(
        field::TASK_KIND.into(),
        Value::String(VERB_TASK_KIND.into()),
    );
    payload.insert(
        field::STATE.into(),
        Value::String(TaskState::Pending.as_str().into()),
    );
    payload.insert(field::VERB.into(), Value::String(draft.verb));
    payload.insert(field::ARGS.into(), Value::String(draft.args.to_string()));
    payload.insert("filed_by".into(), Value::String(filed_by.author()));
    payload.insert("spawned_by".into(), Value::String("task-service".into()));
    if let Some(at) = draft.not_before_ms {
        payload.insert("next_attempt_at".into(), Value::Int(at));
    }
    if let Some(old) = draft.retry_of {
        payload.insert("retry_of".into(), Value::String(old.to_string()));
    }
    let item = Item {
        id: Uuid::new_v4(),
        schema: TASK_SCHEMA,
        payload,
        created: now,
        modified: now,
        author: filed_by.author(),
        author_kind: actor_kind(filed_by),
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
        references: draft
            .depends_on
            .iter()
            .map(|target| TypedReference {
                target: *target,
                edge_type: EdgeType::DependsOn,
                metadata: None,
            })
            .collect(),
        parent: None,
    };
    let id = store
        .insert(item)
        .map_err(|e| (codes::STORE_ERROR.to_string(), e.to_string()))?;
    store
        .get(id)
        .ok()
        .flatten()
        .ok_or_else(|| (codes::STORE_ERROR.into(), "the task vanished".into()))
}

fn shown(store: &dyn AttributedStore, id: ItemId) -> TaskResult {
    match store.get(id) {
        Ok(Some(item)) if item.schema == TASK_SCHEMA => TaskResult {
            ok: true,
            task: Some(task_row(&item)),
            ..Default::default()
        },
        Ok(_) => refuse(codes::NOT_FOUND, format!("no task {id}")),
        Err(e) => refuse(codes::STORE_ERROR, e.to_string()),
    }
}

#[async_trait::async_trait]
impl TaskService for DefaultTaskService {
    async fn enqueue(
        &self,
        verb: String,
        args: Option<serde_json::Value>,
        depends_on: Option<Vec<String>>,
        not_before_ms: Option<i64>,
        title: Option<String>,
    ) -> TaskResult {
        let verb = verb.trim().to_string();
        let Some(handle) = impress_service_core::call::find(&verb) else {
            return refuse(
                codes::INVALID_ARGUMENT,
                format!("no verb named '{verb}' is linked in this process"),
            );
        };
        let args = args.unwrap_or_else(|| serde_json::json!({}));
        if !args.is_object() {
            return refuse(codes::INVALID_ARGUMENT, "args must be an object");
        }
        let mut deps = Vec::new();
        for dep in depends_on.unwrap_or_default() {
            match parse_id(&dep) {
                Ok(id) => deps.push(id),
                Err(refused) => return refused.into(),
            }
        }
        let store = match store(true) {
            Ok(store) => store,
            Err(refused) => return refused.into(),
        };
        let draft = VerbTaskDraft {
            // The canonical name, when `verb` was an alias.
            verb: handle.name().to_string(),
            args,
            depends_on: deps,
            not_before_ms,
            title,
            retry_of: None,
        };
        match file_verb_task(store.as_ref(), draft, &caller()) {
            Ok(item) => TaskResult {
                ok: true,
                task: Some(task_row(&item)),
                ..Default::default()
            },
            Err((code, message)) => refuse(&code, message),
        }
    }

    async fn list(
        &self,
        state: Option<String>,
        kind: Option<String>,
        limit: Option<u32>,
    ) -> TasksResult {
        let store = match store(false) {
            Ok(store) => store,
            Err(refused) => return refused.into(),
        };
        let mut predicates = Vec::new();
        if let Some(state) = state.filter(|s| !s.trim().is_empty()) {
            let Some(parsed) = TaskState::parse_compat(state.trim()) else {
                return TasksResult {
                    ok: false,
                    code: Some(codes::INVALID_ARGUMENT.into()),
                    message: Some(format!(
                        "'{state}' is not a task state (pending, running, done, failed, cancelled)"
                    )),
                    ..Default::default()
                };
            };
            let mut spellings = vec![Value::String(parsed.as_str().into())];
            match parsed {
                TaskState::Pending => spellings.push(Value::String("queued".into())),
                TaskState::Done => spellings.push(Value::String("completed".into())),
                _ => {}
            }
            predicates.push(Predicate::In("payload.state".into(), spellings));
        }
        if let Some(kind) = kind.filter(|k| !k.trim().is_empty()) {
            predicates.push(Predicate::Eq(
                "payload.task_kind".into(),
                Value::String(kind.trim().into()),
            ));
        }
        let limit = (limit.unwrap_or(100) as usize).clamp(1, MAX_ROWS);
        match store.query(&ItemQuery {
            schema: Some(TASK_SCHEMA),
            predicates,
            sort: vec![SortDescriptor {
                field: "created".into(),
                ascending: false,
            }],
            limit: Some(limit),
            include_tags: false,
            include_references: true,
            ..Default::default()
        }) {
            Ok(items) => TasksResult {
                ok: true,
                rows: items.iter().map(task_row).collect(),
                ..Default::default()
            },
            Err(e) => TasksResult {
                ok: false,
                code: Some(codes::STORE_ERROR.into()),
                message: Some(e.to_string()),
                ..Default::default()
            },
        }
    }

    async fn show(&self, id: String) -> TaskResult {
        let id = match parse_id(&id) {
            Ok(id) => id,
            Err(refused) => return refused.into(),
        };
        match store(false) {
            Ok(store) => shown(store.as_ref(), id),
            Err(refused) => refused.into(),
        }
    }

    async fn cancel(&self, id: String) -> TaskResult {
        let id = match parse_id(&id) {
            Ok(id) => id,
            Err(refused) => return refused.into(),
        };
        let store = match store(true) {
            Ok(store) => store,
            Err(refused) => return refused.into(),
        };
        match job::request_cancel(store.as_ref(), id, &caller().author()) {
            Ok(_) => shown(store.as_ref(), id),
            Err(job::JobError::NotFound(_)) | Err(job::JobError::NotATask(..)) => {
                refuse(codes::NOT_FOUND, format!("no task {id}"))
            }
            Err(job::JobError::Transition(e)) => refuse(codes::CONFLICT, e.to_string()),
            Err(e) => refuse(codes::STORE_ERROR, e.to_string()),
        }
    }

    async fn retry(&self, id: String) -> TaskResult {
        let id = match parse_id(&id) {
            Ok(id) => id,
            Err(refused) => return refused.into(),
        };
        let store = match store(true) {
            Ok(store) => store,
            Err(refused) => return refused.into(),
        };
        let item = match store.get(id) {
            Ok(Some(item)) if item.schema == TASK_SCHEMA => item,
            Ok(_) => return refuse(codes::NOT_FOUND, format!("no task {id}")),
            Err(e) => return refuse(codes::STORE_ERROR, e.to_string()),
        };
        let row = task_row(&item);
        if row.kind != VERB_TASK_KIND {
            return refuse(
                codes::INVALID_ARGUMENT,
                format!(
                    "{id} is a '{}' task; only verb tasks are filed again",
                    row.kind
                ),
            );
        }
        if row.state != "failed" && row.state != "cancelled" {
            return refuse(
                codes::CONFLICT,
                format!(
                    "{id} is {}; only a failed or cancelled task is retried",
                    row.state
                ),
            );
        }
        let draft = VerbTaskDraft {
            verb: row.verb.unwrap_or_default(),
            args: row.args.unwrap_or_else(|| serde_json::json!({})),
            depends_on: Vec::new(),
            not_before_ms: None,
            title: Some(row.title),
            retry_of: Some(id),
        };
        match file_verb_task(store.as_ref(), draft, &caller()) {
            Ok(item) => TaskResult {
                ok: true,
                task: Some(task_row(&item)),
                ..Default::default()
            },
            Err((code, message)) => refuse(&code, message),
        }
    }
}

impress_service_impl! {
    service = TaskService,
    safety = read_only,
    since = "0.2.0",
    effects = {
        reads: ["task@1.0.0"],
        writes: [],
        reach: [],
    },
    impl = DefaultTaskService,
    instance = DefaultTaskService::new,
    methods = [
        /// Schedule a verb as a task.
        enqueue(
            /// The verb to run.
            verb: String,
            /// Its arguments (an object; `{}` when absent).
            args: Option<serde_json::Value>,
            /// Task ids that must be done first.
            depends_on: Option<Vec<String>>,
            /// Not before this time, in ms since the epoch.
            not_before_ms: Option<i64>,
            /// A title; the verb's name when absent.
            title: Option<String>
        ) -> TaskResult,
        /// Tasks, newest first.
        list(
            /// Only tasks in this state: pending, running, done, failed, cancelled.
            state: Option<String>,
            /// Only tasks of this kind (`koine.verb` for verb tasks).
            kind: Option<String>,
            /// At most this many (default 100, at most 500).
            limit: Option<u32>
        ) -> TasksResult,
        /// One task.
        show(
            /// The task's id.
            id: String
        ) -> TaskResult,
        /// Stop a task.
        cancel(
            /// The task's id.
            id: String
        ) -> TaskResult,
        /// File a failed or cancelled verb task again.
        retry(
            /// The task's id.
            id: String
        ) -> TaskResult,
    ],
}
