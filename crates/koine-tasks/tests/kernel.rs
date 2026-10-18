//! The ADR-0015 task kernel's whole ADR-0005 §6 loop — spawn → DAG gating
//! → acquire → execute → provenance → retry / escalation / suspension —
//! over the memory and directory stores (docs/plan-koine-ai-tasks.md Q-D2).
//! impress runs the same tests over SQLite (`impel-core`'s
//! `tests/task_kernel_loop.rs`); between them every backend keeps one rule.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use impress_item::item::{ActorKind, Item, Value};
use impress_item::operation::{OperationIntent, OperationSpec, OperationType, RetentionTier};
use impress_item::reference::EdgeType;
use impress_item::store::AttributedStore;
use koine_tasks::{
    create_task_dag, AgentRunRecord, ExecutionOutcome, ItemTaskStore, ReviewRequest, Scheduler,
    SchedulerConfig, TaskError, TaskExecutor, TaskSpec, TaskState, TaskStoreApi,
};

// ── backends ───────────────────────────────────────────────────────────

type Make = fn() -> Arc<dyn AttributedStore>;
type Store = ItemTaskStore<dyn AttributedStore>;

fn memory() -> Arc<dyn AttributedStore> {
    Arc::new(koine_store_memory::MemoryStore::new())
}

fn dir() -> Arc<dyn AttributedStore> {
    let root = tempfile::tempdir().expect("tempdir").keep();
    Arc::new(koine_store_dir::DirStore::open(&root).expect("dir store"))
}

// ── helpers ────────────────────────────────────────────────────────────

fn store(make: Make) -> Arc<Store> {
    Arc::new(ItemTaskStore::new(make()))
}

fn scheduler(store: Arc<Store>) -> Scheduler {
    Scheduler::new(
        store,
        SchedulerConfig {
            actor: "impel-test".into(),
            batch: 16,
            start_delay: std::time::Duration::ZERO,
            poll_interval: std::time::Duration::ZERO,
            // Tests drive run_once in a tight loop; a real backoff would
            // make every retried task invisible to the next pass.
            retry_base_ms: 0,
        },
    )
}

fn state_of(store: &Store, id: impress_item::item::ItemId) -> TaskState {
    let item = TaskStoreApi::get_item(store, id).unwrap().unwrap();
    match item.payload.get("state") {
        Some(Value::String(s)) => TaskState::parse(s).expect("canonical state"),
        other => panic!("no state: {other:?}"),
    }
}

/// Values written by `SetPayload("state", …)` operation items on `id`,
/// oldest first. Operation items encode their type in payload:
/// `op_type = "set_payload"`, `op_data = {field, value}`.
fn state_op_values(store: &Store, id: impress_item::item::ItemId) -> Vec<String> {
    store
        .inner()
        .operations_for(id, None)
        .unwrap()
        .iter()
        .filter_map(|op| {
            if !matches!(op.payload.get("op_type"), Some(Value::String(t)) if t == "set_payload") {
                return None;
            }
            match op.payload.get("op_data") {
                Some(Value::Object(m)) => match (m.get("field"), m.get("value")) {
                    (Some(Value::String(f)), Some(Value::String(v))) if f == "state" => {
                        Some(v.clone())
                    }
                    _ => None,
                },
                _ => None,
            }
        })
        .collect()
}

/// Executor whose per-invocation behavior is scripted.
enum Step {
    Complete,
    RetryableFail,
    PermanentFail,
    /// Open a review and suspend.
    Suspend(&'static str),
}

struct Scripted {
    kind: &'static str,
    script: Mutex<Vec<Step>>, // popped front-to-back
    calls: AtomicUsize,
    record_run: bool,
}

impl Scripted {
    fn new(kind: &'static str, script: Vec<Step>) -> Arc<Self> {
        Arc::new(Self {
            kind,
            script: Mutex::new(script),
            calls: AtomicUsize::new(0),
            record_run: false,
        })
    }
    fn with_agent_run(kind: &'static str, script: Vec<Step>) -> Arc<Self> {
        Arc::new(Self {
            kind,
            script: Mutex::new(script),
            calls: AtomicUsize::new(0),
            record_run: true,
        })
    }
}

#[async_trait]
impl TaskExecutor for Scripted {
    fn task_kind(&self) -> &str {
        self.kind
    }
    fn max_retries(&self) -> u32 {
        2
    }
    async fn execute(
        &self,
        task: &Item,
        store: &dyn TaskStoreApi,
    ) -> Result<ExecutionOutcome, TaskError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.record_run {
            store.record_agent_run(
                task.id,
                AgentRunRecord {
                    agent_id: "scripted".into(),
                    model: "none".into(),
                    prompt_hash: "deadbeef".into(),
                    result_summary: Some("scripted run".into()),
                    token_count: Some(0),
                    duration_ms: Some(1),
                    executor_kind: Some(koine_tasks::EXECUTOR_DETERMINISTIC.into()),
                },
            )?;
        }
        let step = self.script.lock().unwrap().pop();
        match step {
            Some(Step::Complete) | None => Ok(ExecutionOutcome::Complete),
            Some(Step::RetryableFail) => Err(TaskError::Retryable("net down".into())),
            Some(Step::PermanentFail) => Err(TaskError::Permanent("bad schema".into())),
            Some(Step::Suspend(q)) => {
                store.open_review(
                    task.id,
                    ReviewRequest {
                        question: q.into(),
                        context: None,
                    },
                    "scripted",
                )?;
                Ok(ExecutionOutcome::Suspended)
            }
        }
    }
}

fn two_task_dag() -> Vec<TaskSpec> {
    vec![
        TaskSpec {
            kind: "alpha".into(),
            description: None,
            depends_on: vec![],
            operates_on: None,
            output_schema: None,
        },
        TaskSpec {
            kind: "beta".into(),
            description: None,
            depends_on: vec![0],
            operates_on: None,
            output_schema: None,
        },
    ]
}

// ── tests ──────────────────────────────────────────────────────────────

async fn dag_gates_downstream_until_dependency_done(make: Make) {
    let s = store(make);
    let ids = create_task_dag(s.as_ref(), &two_task_dag(), "spawner").unwrap();

    let mut sched = scheduler(s.clone());
    sched.register(Scripted::new("alpha", vec![Step::Complete]));
    sched.register(Scripted::new("beta", vec![Step::Complete]));

    // Pass 1: only alpha is ready (beta's DependsOn target not done).
    let r1 = sched.run_once().await.unwrap();
    assert_eq!(r1.acquired, 1, "only alpha acquirable: {r1:?}");
    assert_eq!(state_of(&s, ids[0]), TaskState::Done);
    assert_eq!(state_of(&s, ids[1]), TaskState::Pending);

    // Pass 2: beta unblocked.
    let r2 = sched.run_once().await.unwrap();
    assert_eq!(r2.acquired, 1);
    assert_eq!(state_of(&s, ids[1]), TaskState::Done);
}

async fn state_transitions_are_operation_items(make: Make) {
    let s = store(make);
    let ids = create_task_dag(
        s.as_ref(),
        &[TaskSpec {
            kind: "alpha".into(),
            description: None,
            depends_on: vec![],
            operates_on: None,
            output_schema: None,
        }],
        "spawner",
    )
    .unwrap();
    let mut sched = scheduler(s.clone());
    sched.register(Scripted::new("alpha", vec![Step::Complete]));
    sched.run_once().await.unwrap();

    // The operation stream is the authoritative history (ADR-0005 §3):
    // pending→running and running→done must both appear as SetPayload ops.
    assert_eq!(
        state_op_values(&s, ids[0]),
        vec!["running".to_string(), "done".to_string()]
    );
}

async fn retry_resets_to_pending_then_succeeds(make: Make) {
    let s = store(make);
    let ids = create_task_dag(
        s.as_ref(),
        &[TaskSpec {
            kind: "flaky".into(),
            description: None,
            depends_on: vec![],
            operates_on: None,
            output_schema: None,
        }],
        "spawner",
    )
    .unwrap();
    let mut sched = scheduler(s.clone());
    // Script is popped back-to-front: first call fails, second completes.
    sched.register(Scripted::new(
        "flaky",
        vec![Step::Complete, Step::RetryableFail],
    ));

    let r1 = sched.run_once().await.unwrap();
    assert_eq!(r1.retried, 1, "{r1:?}");
    assert_eq!(state_of(&s, ids[0]), TaskState::Pending);

    let r2 = sched.run_once().await.unwrap();
    assert_eq!(r2.completed, 1, "{r2:?}");
    assert_eq!(state_of(&s, ids[0]), TaskState::Done);

    // Retry ledger: running→pending reset is in the op history.
    let resets = state_op_values(&s, ids[0])
        .iter()
        .filter(|v| v.as_str() == "pending")
        .count();
    assert_eq!(resets, 1);
}

async fn permanent_failure_fails_immediately_with_error(make: Make) {
    let s = store(make);
    let ids = create_task_dag(
        s.as_ref(),
        &[TaskSpec {
            kind: "doomed".into(),
            description: None,
            depends_on: vec![],
            operates_on: None,
            output_schema: None,
        }],
        "spawner",
    )
    .unwrap();
    let mut sched = scheduler(s.clone());
    sched.register(Scripted::new("doomed", vec![Step::PermanentFail]));

    let r = sched.run_once().await.unwrap();
    assert_eq!(r.failed, 1);
    assert_eq!(state_of(&s, ids[0]), TaskState::Failed);
    let item = TaskStoreApi::get_item(s.as_ref(), ids[0]).unwrap().unwrap();
    assert!(
        matches!(item.payload.get("error"), Some(Value::String(e)) if e.contains("bad schema"))
    );
}

async fn retries_exhaust_then_fail(make: Make) {
    let s = store(make);
    let ids = create_task_dag(
        s.as_ref(),
        &[TaskSpec {
            kind: "hopeless".into(),
            description: None,
            depends_on: vec![],
            operates_on: None,
            output_schema: None,
        }],
        "spawner",
    )
    .unwrap();
    let mut sched = scheduler(s.clone());
    // Always retryable-fails; max_retries = 2 → attempts 1,2 retry; attempt 3 fails.
    sched.register(Scripted::new(
        "hopeless",
        vec![
            Step::RetryableFail,
            Step::RetryableFail,
            Step::RetryableFail,
            Step::RetryableFail,
        ],
    ));

    let mut failed = false;
    for _ in 0..5 {
        let r = sched.run_once().await.unwrap();
        if r.failed > 0 {
            failed = true;
            break;
        }
    }
    assert!(failed, "task should eventually fail after retries exhaust");
    assert_eq!(state_of(&s, ids[0]), TaskState::Failed);
}

async fn suspension_waits_for_review_resolution(make: Make) {
    let s = store(make);
    let ids = create_task_dag(
        s.as_ref(),
        &[TaskSpec {
            kind: "careful".into(),
            description: None,
            depends_on: vec![],
            operates_on: None,
            output_schema: None,
        }],
        "spawner",
    )
    .unwrap();
    let mut sched = scheduler(s.clone());
    // First call suspends with a review; the resumed call completes.
    sched.register(Scripted::new(
        "careful",
        vec![Step::Complete, Step::Suspend("keep these tags?")],
    ));

    let r1 = sched.run_once().await.unwrap();
    assert_eq!(r1.suspended, 1, "{r1:?}");
    assert_eq!(state_of(&s, ids[0]), TaskState::Running);

    // Still suspended on the next pass — review unresolved.
    let r2 = sched.run_once().await.unwrap();
    assert_eq!(r2.suspended, 1, "{r2:?}");
    assert_eq!(state_of(&s, ids[0]), TaskState::Running);

    // Human resolves the review (a SetPayload op by a human actor).
    let (unresolved, _) = TaskStoreApi::reviews_for(s.as_ref(), ids[0]).unwrap();
    assert_eq!(unresolved.len(), 1);
    s.inner()
        .apply_operation(OperationSpec {
            target_id: unresolved[0].id,
            op_type: OperationType::SetPayload(
                "resolution".into(),
                Value::String("approved".into()),
            ),
            intent: OperationIntent::Editorial,
            reason: None,
            batch_id: None,
            author: "tom".into(),
            author_kind: ActorKind::Human,
            retention: RetentionTier::Durable,
        })
        .unwrap();

    // Resume pass completes the task.
    let r3 = sched.run_once().await.unwrap();
    assert_eq!(r3.resumed, 1, "{r3:?}");
    assert_eq!(state_of(&s, ids[0]), TaskState::Done);
}

async fn agent_runs_are_recorded_with_provenance_edges(make: Make) {
    let s = store(make);
    let ids = create_task_dag(
        s.as_ref(),
        &[TaskSpec {
            kind: "tracked".into(),
            description: None,
            depends_on: vec![],
            operates_on: None,
            output_schema: None,
        }],
        "spawner",
    )
    .unwrap();
    let mut sched = scheduler(s.clone());
    sched.register(Scripted::with_agent_run("tracked", vec![Step::Complete]));
    sched.run_once().await.unwrap();

    // Task carries a ProducedBy edge to the run item (ADR-0005 §5).
    let task = TaskStoreApi::get_item(s.as_ref(), ids[0]).unwrap().unwrap();
    let run_edges: Vec<_> = task
        .references
        .iter()
        .filter(|r| r.edge_type == EdgeType::ProducedBy)
        .collect();
    assert_eq!(run_edges.len(), 1);
    let run = TaskStoreApi::get_item(s.as_ref(), run_edges[0].target)
        .unwrap()
        .unwrap();
    assert_eq!(run.schema, impress_item::schema::refs::AGENT_RUN);
    assert!(matches!(run.payload.get("prompt_hash"), Some(Value::String(h)) if h == "deadbeef"));
}

async fn missing_executor_escalates_to_failed(make: Make) {
    let s = store(make);
    let ids = create_task_dag(
        s.as_ref(),
        &[TaskSpec {
            kind: "orphan".into(),
            description: None,
            depends_on: vec![],
            operates_on: None,
            output_schema: None,
        }],
        "spawner",
    )
    .unwrap();
    let sched = scheduler(s.clone()); // nothing registered
    let r = sched.run_once().await.unwrap();
    assert_eq!(r.failed, 1);
    assert_eq!(state_of(&s, ids[0]), TaskState::Failed);
    let item = TaskStoreApi::get_item(s.as_ref(), ids[0]).unwrap().unwrap();
    assert!(
        matches!(item.payload.get("error"), Some(Value::String(e)) if e.contains("no executor"))
    );
}

// ── the 2026-09 review fixes ───────────────────────────────────────────

/// Retry stamps `next_attempt_at` and `ready_tasks` honors it: with a
/// real backoff base the retried task is INVISIBLE to the next pass
/// instead of head-of-line-blocking the batch every 5 seconds. This is
/// the counterfactual for the outage livelock: under the old scheduler
/// the second pass re-acquired the same doomed task immediately.
async fn retry_backoff_defers_the_next_attempt(make: Make) {
    let s = store(make);
    let ids = create_task_dag(
        s.as_ref(),
        &[TaskSpec {
            kind: "alpha".into(),
            description: None,
            depends_on: vec![],
            operates_on: None,
            output_schema: None,
        }],
        "spawner",
    )
    .unwrap();

    let mut sched = Scheduler::new(
        s.clone(),
        SchedulerConfig {
            actor: "impel-test".into(),
            batch: 16,
            start_delay: std::time::Duration::ZERO,
            poll_interval: std::time::Duration::ZERO,
            retry_base_ms: 60_000, // a REAL backoff, unlike the shared helper
        },
    );
    // Scripted pops from the BACK: last element runs first.
    sched.register(Scripted::new(
        "alpha",
        vec![Step::Complete, Step::RetryableFail],
    ));

    let r1 = sched.run_once().await.unwrap();
    assert_eq!(r1.retried, 1, "{r1:?}");
    assert_eq!(state_of(&s, ids[0]), TaskState::Pending);
    let task = TaskStoreApi::get_item(s.as_ref(), ids[0]).unwrap().unwrap();
    let not_before = match task.payload.get("next_attempt_at") {
        Some(Value::Int(ms)) => *ms,
        other => panic!("retry did not stamp next_attempt_at: {other:?}"),
    };
    assert!(
        not_before > chrono::Utc::now().timestamp_millis() + 30_000,
        "backoff at least ~60s out, got {not_before}"
    );

    // The doomed task no longer monopolizes the queue: nothing acquirable.
    let r2 = sched.run_once().await.unwrap();
    assert_eq!(r2.acquired, 0, "backoff must defer re-acquisition: {r2:?}");
    assert_eq!(state_of(&s, ids[0]), TaskState::Pending);
}

/// A terminal upstream failure cancels its pending dependents (ADR-0005
/// §4). Under the old scheduler the dependent stayed `pending` forever —
/// never ready (the readiness SQL blocks on any dep not done), never
/// failed, invisible to every surface.
async fn terminal_failure_cancels_pending_dependents(make: Make) {
    let s = store(make);
    let ids = create_task_dag(s.as_ref(), &two_task_dag(), "spawner").unwrap();

    let mut sched = scheduler(s.clone());
    sched.register(Scripted::new("alpha", vec![Step::PermanentFail]));
    sched.register(Scripted::new("beta", vec![Step::Complete]));

    let r1 = sched.run_once().await.unwrap();
    assert_eq!(r1.failed, 1, "{r1:?}");
    assert_eq!(r1.cancelled, 1, "dependent must be cancelled: {r1:?}");
    assert_eq!(state_of(&s, ids[0]), TaskState::Failed);
    assert_eq!(state_of(&s, ids[1]), TaskState::Cancelled);
    let beta = TaskStoreApi::get_item(s.as_ref(), ids[1]).unwrap().unwrap();
    assert!(
        matches!(beta.payload.get("error"),
                 Some(Value::String(e)) if e.contains("dependency")),
        "cancellation reason recorded"
    );

    // And nothing is left for later passes to chew on.
    let r2 = sched.run_once().await.unwrap();
    assert_eq!(r2.acquired + r2.failed + r2.cancelled, 0, "{r2:?}");
}

/// A `running` task with no `assigned_to` — the wedge a crash mid-acquire
/// used to strand — is adopted back to `pending` and then runs. Under the
/// old scheduler it matched neither `ready_tasks` nor `running_tasks` and
/// was permanently invisible.
async fn orphaned_running_task_is_adopted_and_completes(make: Make) {
    let s = store(make);
    let ids = create_task_dag(
        s.as_ref(),
        &[TaskSpec {
            kind: "alpha".into(),
            description: None,
            depends_on: vec![],
            operates_on: None,
            output_schema: None,
        }],
        "spawner",
    )
    .unwrap();

    // Simulate the stranded state: running, but no assignee ever written.
    s.inner()
        .apply_operation(OperationSpec {
            target_id: ids[0],
            op_type: OperationType::SetPayload("state".into(), Value::String("running".into())),
            intent: OperationIntent::Routine,
            reason: None,
            batch_id: None,
            author: "crash".into(),
            author_kind: ActorKind::Agent,
            retention: RetentionTier::Compactable,
        })
        .unwrap();

    let mut sched = scheduler(s.clone());
    sched.register(Scripted::new("alpha", vec![Step::Complete]));

    let r1 = sched.run_once().await.unwrap();
    assert_eq!(r1.adopted, 1, "orphan must be adopted: {r1:?}");
    assert_eq!(r1.completed, 1, "and then executed: {r1:?}");
    assert_eq!(state_of(&s, ids[0]), TaskState::Done);
}

/// An executor whose `readiness()` fails defers its whole kind: tasks are
/// NOT acquired, burn no attempts, and stay cleanly pending. Under the
/// old scheduler every such task was acquired, failed against the dead
/// dependency, and burned its full retry budget in ~15 seconds.
async fn unready_kind_is_deferred_without_burning_attempts(make: Make) {
    struct NeverReady;
    #[async_trait]
    impl TaskExecutor for NeverReady {
        fn task_kind(&self) -> &str {
            "alpha"
        }
        async fn readiness(&self) -> Result<(), String> {
            Err("provider down".into())
        }
        async fn execute(
            &self,
            _task: &Item,
            _store: &dyn TaskStoreApi,
        ) -> Result<ExecutionOutcome, TaskError> {
            panic!("must never execute while unready");
        }
    }

    let s = store(make);
    let ids = create_task_dag(
        s.as_ref(),
        &[TaskSpec {
            kind: "alpha".into(),
            description: None,
            depends_on: vec![],
            operates_on: None,
            output_schema: None,
        }],
        "spawner",
    )
    .unwrap();

    let mut sched = scheduler(s.clone());
    sched.register(Arc::new(NeverReady));

    let r1 = sched.run_once().await.unwrap();
    assert_eq!(r1.deferred, 1, "{r1:?}");
    assert_eq!(r1.acquired, 0, "{r1:?}");
    assert_eq!(state_of(&s, ids[0]), TaskState::Pending);
    let task = TaskStoreApi::get_item(s.as_ref(), ids[0]).unwrap().unwrap();
    assert!(
        !task.payload.contains_key("attempts"),
        "no attempts burned while deferred"
    );

    // A ready scheduler (fresh instance, no negative cache) runs it fine.
    let mut ready_sched = scheduler(s.clone());
    ready_sched.register(Scripted::new("alpha", vec![Step::Complete]));
    let r2 = ready_sched.run_once().await.unwrap();
    assert_eq!(r2.completed, 1, "{r2:?}");
}

/// Spawn provenance (ADR-0005 §10): every task records the RULE that
/// created it and carries a `triggered-by` edge to the item that caused
/// it. Before this, `assigned_to` (the executor that later picked the
/// task up) was the only origin story a task could tell, and the
/// `triggered-by` edge — declared in the task schema's expected edges
/// since it was written — had no writer anywhere in the tree.
async fn spawned_tasks_record_their_rule_and_trigger(make: Make) {
    use koine_tasks::{create_task_dag_from, SpawnProvenance};

    let s = store(make);
    // A trigger item distinct from the subject, the case that carries
    // information (a throughline sync is triggered by an edited section
    // but operates on the throughline).
    let trigger = TaskStoreApi::create_item(s.as_ref(), {
        let mut item = impress_item::item::Item {
            id: uuid::Uuid::new_v4(),
            schema: impress_item::schema::refs::MANUSCRIPT_SECTION,
            payload: std::collections::BTreeMap::new(),
            created: chrono::Utc::now(),
            modified: chrono::Utc::now(),
            author: "tester".into(),
            author_kind: ActorKind::Human,
            logical_clock: 0,
            origin: None,
            canonical_id: None,
            tags: vec![],
            flag: None,
            is_read: false,
            is_starred: false,
            priority: impress_item::item::Priority::Normal,
            visibility: impress_item::item::Visibility::Private,
            message_type: None,
            produced_by: None,
            version: None,
            batch_id: None,
            references: vec![],
            parent: None,
            extra: Default::default(),
        };
        item.payload
            .insert("title".into(), Value::String("edited section".into()));
        item
    })
    .unwrap();
    let subject = TaskStoreApi::create_item(s.as_ref(), {
        let mut item = impress_item::item::Item {
            id: uuid::Uuid::new_v4(),
            schema: impress_item::schema::refs::THROUGHLINE,
            payload: std::collections::BTreeMap::new(),
            created: chrono::Utc::now(),
            modified: chrono::Utc::now(),
            author: "tester".into(),
            author_kind: ActorKind::Human,
            logical_clock: 0,
            origin: None,
            canonical_id: None,
            tags: vec![],
            flag: None,
            is_read: false,
            is_starred: false,
            priority: impress_item::item::Priority::Normal,
            visibility: impress_item::item::Visibility::Private,
            message_type: None,
            produced_by: None,
            version: None,
            batch_id: None,
            references: vec![],
            parent: None,
            extra: Default::default(),
        };
        item.payload
            .insert("title".into(), Value::String("the throughline".into()));
        item
    })
    .unwrap();

    let ids = create_task_dag_from(
        s.as_ref(),
        &[TaskSpec {
            kind: "throughline-sync".into(),
            description: None,
            depends_on: vec![],
            operates_on: Some(subject),
            output_schema: None,
        }],
        "impel-taskd",
        Some(&SpawnProvenance {
            rule_id: "impel/throughline-spawn".into(),
            trigger: Some(trigger),
        }),
    )
    .unwrap();

    let task = TaskStoreApi::get_item(s.as_ref(), ids[0]).unwrap().unwrap();
    assert!(
        matches!(task.payload.get("spawned_by"),
                 Some(Value::String(rule)) if rule == "impel/throughline-spawn"),
        "the rule that created the task is named: {:?}",
        task.payload.get("spawned_by")
    );
    let triggered: Vec<_> = task
        .references
        .iter()
        .filter(|r| r.edge_type == EdgeType::Custom("triggered-by".into()))
        .map(|r| r.target)
        .collect();
    assert_eq!(triggered, vec![trigger], "trigger edge points at the cause");
    assert!(
        task.references
            .iter()
            .any(|r| r.edge_type == EdgeType::OperatesOn && r.target == subject),
        "subject edge is untouched"
    );

    // When trigger IS the subject (enrichment), the redundant second edge
    // is skipped — the rule name still records the cause.
    let self_triggered = create_task_dag_from(
        s.as_ref(),
        &[TaskSpec {
            kind: "keyword-tag".into(),
            description: None,
            depends_on: vec![],
            operates_on: Some(subject),
            output_schema: None,
        }],
        "impel-taskd",
        Some(&SpawnProvenance {
            rule_id: "impel/enrichment-spawn".into(),
            trigger: Some(subject),
        }),
    )
    .unwrap();
    let task2 = TaskStoreApi::get_item(s.as_ref(), self_triggered[0])
        .unwrap()
        .unwrap();
    assert_eq!(
        task2
            .references
            .iter()
            .filter(|r| r.target == subject)
            .count(),
        1,
        "no duplicate edge to the same item"
    );
    assert!(matches!(task2.payload.get("spawned_by"),
                     Some(Value::String(rule)) if rule == "impel/enrichment-spawn"));

    // The provenance-free form stays provenance-free.
    let plain = create_task_dag(s.as_ref(), &two_task_dag(), "spawner").unwrap();
    let plain_task = TaskStoreApi::get_item(s.as_ref(), plain[0])
        .unwrap()
        .unwrap();
    assert!(!plain_task.payload.contains_key("spawned_by"));
}

/// ADR-0034 D6: a cancel asked for while a task sits suspended on a review
/// is honoured on the next resume pass — the row goes `cancelled`, and the
/// executor is never re-run.
async fn a_cancel_requested_on_a_suspended_task_is_honoured_on_resume(make: Make) {
    let s = store(make);
    let ids = create_task_dag(
        s.as_ref(),
        &[TaskSpec {
            kind: "careful".into(),
            description: None,
            depends_on: vec![],
            operates_on: None,
            output_schema: None,
        }],
        "spawner",
    )
    .unwrap();
    let mut sched = scheduler(s.clone());
    sched.register(Scripted::new(
        "careful",
        vec![Step::Complete, Step::Suspend("keep these tags?")],
    ));
    let r1 = sched.run_once().await.unwrap();
    assert_eq!(r1.suspended, 1, "{r1:?}");

    assert_eq!(
        koine_tasks::job::request_cancel(s.inner().as_ref(), ids[0], "tom").unwrap(),
        TaskState::Running
    );
    let r2 = sched.run_once().await.unwrap();
    assert_eq!(r2.cancelled, 1, "{r2:?}");
    assert_eq!(r2.resumed, 0, "{r2:?}");
    assert_eq!(state_of(&s, ids[0]), TaskState::Cancelled);
}

// ── every test, on every backend ───────────────────────────────────────

macro_rules! on_every_backend {
    ($($name:ident),* $(,)?) => {
        mod memory_store {
            $(#[tokio::test]
            async fn $name() {
                super::$name(super::memory).await
            })*
        }
        mod dir_store {
            $(#[tokio::test]
            async fn $name() {
                super::$name(super::dir).await
            })*
        }
    };
}

on_every_backend!(
    dag_gates_downstream_until_dependency_done,
    state_transitions_are_operation_items,
    retry_resets_to_pending_then_succeeds,
    permanent_failure_fails_immediately_with_error,
    retries_exhaust_then_fail,
    suspension_waits_for_review_resolution,
    agent_runs_are_recorded_with_provenance_edges,
    missing_executor_escalates_to_failed,
    retry_backoff_defers_the_next_attempt,
    terminal_failure_cancels_pending_dependents,
    orphaned_running_task_is_adopted_and_completes,
    unready_kind_is_deferred_without_burning_attempts,
    spawned_tasks_record_their_rule_and_trigger,
    a_cancel_requested_on_a_suspended_task_is_honoured_on_resume,
);
