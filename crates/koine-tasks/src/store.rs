//! The task store port (ADR-0005 §6, ADR-0015 D5), moved from
//! `impel-core::task_store` (docs/plan-koine-ai-tasks.md Q-D1, Q-D2).
//!
//! The scheduler and every executor touch the item graph through
//! [`TaskStoreApi`]. It is object-safe, so executors receive
//! `&dyn TaskStoreApi` and tests can substitute fakes.
//!
//! [`ItemTaskStore`] implements the port over any [`AttributedStore`]: the
//! ready queue is a query ([`ready_tasks_by_query`]) and every other method
//! is a [`kernel`] function. A backend with a faster ready queue implements
//! the port itself and calls the same [`kernel`] functions for the rest
//! (`impress-core` does, with SQLite's single-statement `ready_tasks`).

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use chrono::Utc;
use impress_item::item::{ActorId, ActorKind, Item, ItemId, Priority, Value, Visibility};
use impress_item::operation::{OperationIntent, OperationSpec, OperationType, RetentionTier};
use impress_item::query::{ItemQuery, Predicate, SortDescriptor};
use impress_item::reference::{EdgeType, TypedReference};
use impress_item::store::{AttributedStore, StoreError};
use impress_item::SchemaRef;
use uuid::Uuid;

use crate::state::{transition_op, TaskState, TaskTransitionError};

/// `task@1.0.0`: the one spelling of a task row (schema-refs.json).
pub const TASK_SCHEMA: SchemaRef = impress_item::schema::refs::TASK;
/// `agent-run@1.0.0`: one executor or model invocation.
pub const AGENT_RUN_SCHEMA: SchemaRef = impress_item::schema::refs::AGENT_RUN;
/// `review-request@1.0.0`: a human checkpoint on a task.
pub const REVIEW_REQUEST_SCHEMA: SchemaRef = impress_item::schema::refs::REVIEW_REQUEST;
/// `task-event@1.0.0`: one progress event of a job (see [`crate::job`]).
pub const TASK_EVENT_SCHEMA: SchemaRef = impress_item::schema::refs::TASK_EVENT;

/// The author the kernel's own rows carry (agent runs and their edges). The
/// name predates koine; readers in impress filter on it, so it stays.
pub const KERNEL_AUTHOR: &str = "impel";

/// Errors crossing the store boundary.
#[derive(Debug, thiserror::Error)]
pub enum TaskStoreError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Transition(#[from] TaskTransitionError),
    #[error("task {0} not found")]
    TaskNotFound(ItemId),
}

/// Provenance record for one agent/tool invocation (ADR-0005 §5).
#[derive(Debug, Clone, Default)]
pub struct AgentRunRecord {
    pub agent_id: String,
    /// Identifier of what produced the result. NOT always a model — see
    /// [`Self::executor_kind`], without which `heuristic-v1` reads like an
    /// LLM in every surface that shows it.
    pub model: String,
    pub prompt_hash: String,
    pub result_summary: Option<String>,
    pub token_count: Option<i64>,
    pub duration_ms: Option<i64>,
    /// How the result was produced: [`EXECUTOR_DETERMINISTIC`] for code
    /// that cannot vary given the same input (keyword tables, API
    /// pipelines), [`EXECUTOR_MODEL`] for an inference call. Readers
    /// render it beside `model` so a reader can tell a lookup table from
    /// a language model at a glance.
    pub executor_kind: Option<String>,
}

/// `executor_kind`: produced by deterministic code, not inference.
pub const EXECUTOR_DETERMINISTIC: &str = "deterministic";
/// `executor_kind`: produced by a model inference call.
pub const EXECUTOR_MODEL: &str = "model";

/// A human-review checkpoint request (ADR-0005 §8 `AwaitHumanResponse`).
#[derive(Debug, Clone)]
pub struct ReviewRequest {
    /// The question the human must answer.
    pub question: String,
    /// Optional structured context (proposal payload, confidence, …).
    pub context: Option<BTreeMap<String, Value>>,
}

/// The store facade the scheduler and executors run against.
pub trait TaskStoreApi: Send + Sync {
    fn create_item(&self, item: Item) -> Result<ItemId, TaskStoreError>;
    fn get_item(&self, id: ItemId) -> Result<Option<Item>, TaskStoreError>;
    fn apply(&self, spec: OperationSpec) -> Result<ItemId, TaskStoreError>;

    /// Add a typed edge on `source` pointing at `target`, as an operation
    /// item (attributed provenance, like every other kernel write).
    fn add_edge(
        &self,
        source: ItemId,
        target: ItemId,
        edge: EdgeType,
        author: &str,
    ) -> Result<(), TaskStoreError>;

    /// Pending tasks whose `DependsOn` targets are all `done` (ADR-0005 §4).
    fn ready_tasks(&self, limit: usize) -> Result<Vec<Item>, TaskStoreError>;

    /// Validated state transition; writes the `SetPayload("state", …)`
    /// operation item (ADR-0015 D1). The ONLY way task state moves.
    fn transition(
        &self,
        task_id: ItemId,
        to: TaskState,
        actor: &str,
        intent: Option<OperationIntent>,
    ) -> Result<(), TaskStoreError>;

    /// Record an agent invocation and link `task —ProducedBy→ run`.
    fn record_agent_run(
        &self,
        task_id: ItemId,
        run: AgentRunRecord,
    ) -> Result<ItemId, TaskStoreError>;

    /// Open a review checkpoint: creates a `review-request@1.0.0` item
    /// linked `review —OperatesOn→ task`. The task stays `running` until
    /// the review's `resolution` payload field is set.
    fn open_review(
        &self,
        task_id: ItemId,
        request: ReviewRequest,
        author: &str,
    ) -> Result<ItemId, TaskStoreError>;

    /// The task's review items, newest first: `(unresolved, resolved)`.
    fn reviews_for(&self, task_id: ItemId) -> Result<(Vec<Item>, Vec<Item>), TaskStoreError>;

    /// Tasks currently `running` (used for suspension recovery on restart).
    fn running_tasks(&self, assigned_to: &str) -> Result<Vec<Item>, TaskStoreError>;

    /// Apply several operations atomically (one transaction, one batch id).
    /// A crash or a sibling-writer BUSY timeout either applies ALL of them
    /// or NONE — the primitive `acquire_task` needs so a task can never be
    /// stranded `running` with no `assigned_to`.
    fn apply_batch(&self, specs: Vec<OperationSpec>) -> Result<(), TaskStoreError>;

    /// Atomically acquire `task`: transition → `running`, stamp
    /// `assigned_to` and the incremented `attempts` — one transaction.
    fn acquire_task(&self, task: &Item, actor: &str) -> Result<(), TaskStoreError>;

    /// Target task ids of every UNRESOLVED review-request in the store —
    /// the suspension set, derived in ONE query per pass instead of one
    /// `reviews_for` per running task (the old shape cost ~1,019 queries
    /// per 5s pass at the live backlog).
    fn unresolved_review_targets(&self) -> Result<HashSet<ItemId>, TaskStoreError>;

    /// `running` tasks with NO assignee — the wedge a non-atomic acquire
    /// used to strand (invisible to both `ready_tasks` and
    /// `running_tasks`). The scheduler adopts them back to `pending`.
    fn orphaned_running_tasks(&self) -> Result<Vec<Item>, TaskStoreError>;

    /// Tasks that declare `DependsOn → task_id` (direct dependents), for
    /// failure/cancellation propagation (ADR-0005 §4).
    fn dependents_of(&self, task_id: ItemId) -> Result<Vec<Item>, TaskStoreError>;
}

/// Build a bare item envelope for kernel-created items.
pub fn kernel_item(
    schema: SchemaRef,
    payload: BTreeMap<String, Value>,
    author: &str,
    references: Vec<TypedReference>,
) -> Item {
    Item {
        id: Uuid::new_v4(),
        schema,
        payload,
        created: Utc::now(),
        modified: Utc::now(),
        author: ActorId::from(author),
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
        references,
        parent: None,
        extra: Default::default(),
    }
}

fn is_done(state: Option<&Value>) -> bool {
    matches!(state, Some(Value::String(s)) if s == "done" || s == "completed")
}

fn non_empty(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::String(s)) if !s.is_empty())
}

/// The ready queue as a query, the rule `SqliteItemStore::ready_tasks`
/// keeps in one SQL statement (ADR-0015 D2): `task@1.0.0` rows that are
/// `pending` (or the bridge's `queued`), carry a non-empty `task_kind`,
/// whose `next_attempt_at` (the retry backoff; absent = 0) has passed, and
/// whose every `DependsOn` target exists and is `done` (or `completed`),
/// oldest first, at most `limit`.
pub fn ready_tasks_by_query<S: AttributedStore + ?Sized>(
    store: &S,
    limit: usize,
) -> Result<Vec<Item>, StoreError> {
    let candidates = store.query(&ItemQuery {
        schema: Some(TASK_SCHEMA),
        predicates: vec![Predicate::In(
            "payload.state".into(),
            vec![
                Value::String("pending".into()),
                Value::String("queued".into()),
            ],
        )],
        sort: vec![SortDescriptor {
            field: "created".into(),
            ascending: true,
        }],
        include_tags: false,
        include_references: true,
        ..Default::default()
    })?;
    let now_ms = Utc::now().timestamp_millis();
    let mut ready = Vec::new();
    for task in candidates {
        if ready.len() >= limit {
            break;
        }
        if !non_empty(task.payload.get("task_kind")) {
            continue;
        }
        let not_before = match task.payload.get("next_attempt_at") {
            Some(Value::Int(at)) => *at,
            _ => 0,
        };
        if not_before > now_ms {
            continue;
        }
        let mut gated = false;
        for reference in &task.references {
            if reference.edge_type != EdgeType::DependsOn {
                continue;
            }
            match store.get(reference.target)? {
                Some(dep) if is_done(dep.payload.get("state")) => {}
                _ => {
                    gated = true;
                    break;
                }
            }
        }
        if !gated {
            ready.push(task);
        }
    }
    Ok(ready)
}

/// The port's methods as functions over any store, so a backend that
/// implements [`TaskStoreApi`] itself writes only what it does differently.
pub mod kernel {
    use super::*;

    pub fn create_item<S: AttributedStore + ?Sized>(
        store: &S,
        item: Item,
    ) -> Result<ItemId, TaskStoreError> {
        Ok(store.insert(item)?)
    }

    pub fn get_item<S: AttributedStore + ?Sized>(
        store: &S,
        id: ItemId,
    ) -> Result<Option<Item>, TaskStoreError> {
        Ok(store.get(id)?)
    }

    pub fn apply<S: AttributedStore + ?Sized>(
        store: &S,
        spec: OperationSpec,
    ) -> Result<ItemId, TaskStoreError> {
        Ok(store.apply_operation(spec)?)
    }

    pub fn add_edge<S: AttributedStore + ?Sized>(
        store: &S,
        source: ItemId,
        target: ItemId,
        edge: EdgeType,
        author: &str,
    ) -> Result<(), TaskStoreError> {
        let spec = OperationSpec {
            target_id: source,
            op_type: OperationType::AddReference(TypedReference {
                target,
                edge_type: edge,
                metadata: None,
            }),
            intent: OperationIntent::Routine,
            reason: None,
            batch_id: None,
            author: ActorId::from(author),
            author_kind: ActorKind::Agent,
            retention: RetentionTier::Durable,
        };
        store.apply_operation(spec)?;
        Ok(())
    }

    pub fn transition<S: AttributedStore + ?Sized>(
        store: &S,
        task_id: ItemId,
        to: TaskState,
        actor: &str,
        intent: Option<OperationIntent>,
    ) -> Result<(), TaskStoreError> {
        let task = store
            .get(task_id)?
            .ok_or(TaskStoreError::TaskNotFound(task_id))?;
        let spec = transition_op(&task, to, ActorId::from(actor), ActorKind::Agent, intent)?;
        store.apply_operation(spec)?;
        Ok(())
    }

    pub fn record_agent_run<S: AttributedStore + ?Sized>(
        store: &S,
        task_id: ItemId,
        run: AgentRunRecord,
    ) -> Result<ItemId, TaskStoreError> {
        let mut payload = BTreeMap::new();
        payload.insert("agent_id".into(), Value::String(run.agent_id));
        payload.insert("model".into(), Value::String(run.model));
        payload.insert("prompt_hash".into(), Value::String(run.prompt_hash));
        if let Some(s) = run.result_summary {
            payload.insert("result_summary".into(), Value::String(s));
        }
        if let Some(t) = run.token_count {
            payload.insert("token_count".into(), Value::Int(t));
        }
        if let Some(d) = run.duration_ms {
            payload.insert("duration_ms".into(), Value::Int(d));
        }
        if let Some(kind) = run.executor_kind {
            payload.insert("executor_kind".into(), Value::String(kind));
        }
        // The run knows which task triggered it…
        let run_item = kernel_item(
            AGENT_RUN_SCHEMA,
            payload,
            KERNEL_AUTHOR,
            vec![TypedReference {
                target: task_id,
                edge_type: EdgeType::ProducedBy,
                metadata: None,
            }],
        );
        let run_id = store.insert(run_item)?;
        // …and the task points at the run for provenance (ADR-0005 §5).
        add_edge(store, task_id, run_id, EdgeType::ProducedBy, KERNEL_AUTHOR)?;
        Ok(run_id)
    }

    pub fn open_review<S: AttributedStore + ?Sized>(
        store: &S,
        task_id: ItemId,
        request: ReviewRequest,
        author: &str,
    ) -> Result<ItemId, TaskStoreError> {
        let mut payload = BTreeMap::new();
        payload.insert("question".into(), Value::String(request.question));
        if let Some(ctx) = request.context {
            for (k, v) in ctx {
                payload.insert(format!("context_{k}"), v);
            }
        }
        let review = kernel_item(
            REVIEW_REQUEST_SCHEMA,
            payload,
            author,
            vec![TypedReference {
                target: task_id,
                edge_type: EdgeType::OperatesOn,
                metadata: None,
            }],
        );
        Ok(store.insert(review)?)
    }

    pub fn reviews_for<S: AttributedStore + ?Sized>(
        store: &S,
        task_id: ItemId,
    ) -> Result<(Vec<Item>, Vec<Item>), TaskStoreError> {
        let q = ItemQuery {
            schema: Some(REVIEW_REQUEST_SCHEMA),
            predicates: vec![Predicate::HasReference(EdgeType::OperatesOn, task_id)],
            // Newest first, for real — the doc promised it while the query
            // had no ORDER BY, so `resolved.first()` picked an arbitrary
            // review whenever a task had several.
            sort: vec![SortDescriptor {
                field: "created".into(),
                ascending: false,
            }],
            include_tags: false,
            include_references: false,
            ..Default::default()
        };
        let mut unresolved = Vec::new();
        let mut resolved = Vec::new();
        for item in store.query(&q)? {
            if non_empty(item.payload.get("resolution")) {
                resolved.push(item);
            } else {
                unresolved.push(item);
            }
        }
        Ok((unresolved, resolved))
    }

    pub fn running_tasks<S: AttributedStore + ?Sized>(
        store: &S,
        assigned_to: &str,
    ) -> Result<Vec<Item>, TaskStoreError> {
        let q = ItemQuery {
            schema: Some(TASK_SCHEMA),
            predicates: vec![
                Predicate::Eq(
                    "payload.state".into(),
                    Value::String(TaskState::Running.as_str().into()),
                ),
                Predicate::Eq(
                    "payload.assigned_to".into(),
                    Value::String(assigned_to.into()),
                ),
            ],
            // Executors re-find their subject via references; tags are
            // never read on the scheduler path.
            include_tags: false,
            ..Default::default()
        };
        Ok(store.query(&q)?)
    }

    pub fn apply_batch<S: AttributedStore + ?Sized>(
        store: &S,
        specs: Vec<OperationSpec>,
    ) -> Result<(), TaskStoreError> {
        store.apply_operation_batch(specs)?;
        Ok(())
    }

    /// The three writes of an acquire: the transition to `running`, the
    /// assignee and the incremented attempt count.
    pub fn acquire_ops(task: &Item, actor: &str) -> Result<Vec<OperationSpec>, TaskStoreError> {
        let transition = transition_op(
            task,
            TaskState::Running,
            ActorId::from(actor),
            ActorKind::Agent,
            None,
        )?;
        let attempts = match task.payload.get("attempts") {
            Some(Value::Int(i)) => *i,
            _ => 0,
        } + 1;
        let sibling = |field: &str, value: Value| OperationSpec {
            target_id: task.id,
            op_type: OperationType::SetPayload(field.into(), value),
            intent: OperationIntent::Routine,
            reason: None,
            batch_id: None,
            author: ActorId::from(actor),
            author_kind: ActorKind::Agent,
            retention: RetentionTier::Compactable,
        };
        Ok(vec![
            transition,
            sibling("assigned_to", Value::String(actor.into())),
            sibling("attempts", Value::Int(attempts)),
        ])
    }

    pub fn acquire_task<S: AttributedStore + ?Sized>(
        store: &S,
        task: &Item,
        actor: &str,
    ) -> Result<(), TaskStoreError> {
        apply_batch(store, acquire_ops(task, actor)?)
    }

    pub fn unresolved_review_targets<S: AttributedStore + ?Sized>(
        store: &S,
    ) -> Result<HashSet<ItemId>, TaskStoreError> {
        let q = ItemQuery {
            schema: Some(REVIEW_REQUEST_SCHEMA),
            include_tags: false,
            // References ARE the payload here: the OperatesOn edge names
            // the suspended task.
            include_references: true,
            ..Default::default()
        };
        let mut targets = HashSet::new();
        for review in store.query(&q)? {
            if non_empty(review.payload.get("resolution")) {
                continue;
            }
            for r in &review.references {
                if r.edge_type == EdgeType::OperatesOn {
                    targets.insert(r.target);
                }
            }
        }
        Ok(targets)
    }

    pub fn orphaned_running_tasks<S: AttributedStore + ?Sized>(
        store: &S,
    ) -> Result<Vec<Item>, TaskStoreError> {
        let q = ItemQuery {
            schema: Some(TASK_SCHEMA),
            predicates: vec![Predicate::Eq(
                "payload.state".into(),
                Value::String(TaskState::Running.as_str().into()),
            )],
            include_tags: false,
            include_references: false,
            ..Default::default()
        };
        Ok(store
            .query(&q)?
            .into_iter()
            .filter(|t| !non_empty(t.payload.get("assigned_to")))
            .collect())
    }

    pub fn dependents_of<S: AttributedStore + ?Sized>(
        store: &S,
        task_id: ItemId,
    ) -> Result<Vec<Item>, TaskStoreError> {
        let q = ItemQuery {
            schema: Some(TASK_SCHEMA),
            predicates: vec![Predicate::HasReference(EdgeType::DependsOn, task_id)],
            include_tags: false,
            include_references: true,
            ..Default::default()
        };
        Ok(store.query(&q)?)
    }
}

/// The task port over any store (Q-D2).
pub struct ItemTaskStore<S: ?Sized> {
    store: Arc<S>,
}

impl<S: ?Sized> ItemTaskStore<S> {
    pub fn new(store: Arc<S>) -> Self {
        Self { store }
    }

    /// The store underneath.
    pub fn inner(&self) -> &Arc<S> {
        &self.store
    }
}

impl<S: AttributedStore + ?Sized> TaskStoreApi for ItemTaskStore<S> {
    fn create_item(&self, item: Item) -> Result<ItemId, TaskStoreError> {
        kernel::create_item(&*self.store, item)
    }

    fn get_item(&self, id: ItemId) -> Result<Option<Item>, TaskStoreError> {
        kernel::get_item(&*self.store, id)
    }

    fn apply(&self, spec: OperationSpec) -> Result<ItemId, TaskStoreError> {
        kernel::apply(&*self.store, spec)
    }

    fn add_edge(
        &self,
        source: ItemId,
        target: ItemId,
        edge: EdgeType,
        author: &str,
    ) -> Result<(), TaskStoreError> {
        kernel::add_edge(&*self.store, source, target, edge, author)
    }

    fn ready_tasks(&self, limit: usize) -> Result<Vec<Item>, TaskStoreError> {
        Ok(ready_tasks_by_query(&*self.store, limit)?)
    }

    fn transition(
        &self,
        task_id: ItemId,
        to: TaskState,
        actor: &str,
        intent: Option<OperationIntent>,
    ) -> Result<(), TaskStoreError> {
        kernel::transition(&*self.store, task_id, to, actor, intent)
    }

    fn record_agent_run(
        &self,
        task_id: ItemId,
        run: AgentRunRecord,
    ) -> Result<ItemId, TaskStoreError> {
        kernel::record_agent_run(&*self.store, task_id, run)
    }

    fn open_review(
        &self,
        task_id: ItemId,
        request: ReviewRequest,
        author: &str,
    ) -> Result<ItemId, TaskStoreError> {
        kernel::open_review(&*self.store, task_id, request, author)
    }

    fn reviews_for(&self, task_id: ItemId) -> Result<(Vec<Item>, Vec<Item>), TaskStoreError> {
        kernel::reviews_for(&*self.store, task_id)
    }

    fn running_tasks(&self, assigned_to: &str) -> Result<Vec<Item>, TaskStoreError> {
        kernel::running_tasks(&*self.store, assigned_to)
    }

    fn apply_batch(&self, specs: Vec<OperationSpec>) -> Result<(), TaskStoreError> {
        kernel::apply_batch(&*self.store, specs)
    }

    fn acquire_task(&self, task: &Item, actor: &str) -> Result<(), TaskStoreError> {
        kernel::acquire_task(&*self.store, task, actor)
    }

    fn unresolved_review_targets(&self) -> Result<HashSet<ItemId>, TaskStoreError> {
        kernel::unresolved_review_targets(&*self.store)
    }

    fn orphaned_running_tasks(&self) -> Result<Vec<Item>, TaskStoreError> {
        kernel::orphaned_running_tasks(&*self.store)
    }

    fn dependents_of(&self, task_id: ItemId) -> Result<Vec<Item>, TaskStoreError> {
        kernel::dependents_of(&*self.store, task_id)
    }
}
