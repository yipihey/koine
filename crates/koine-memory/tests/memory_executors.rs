//! ADR-0028 D7 — the two memory executors and their spawner, end to end
//! against a real (in-memory) item store and a real sidecar on disk.
//!
//! **No test here constructs a `SemanticSearch`.** Doing so downloads a ~100MB
//! ONNX model on first run, which has no place in a unit test; the
//! `TextEmbedder` seam exists precisely so the mechanics this crate owns —
//! candidate selection, the keyset cursor, the deterministic vector ids, the
//! sidecar write, the cursor advance — are provable without one.
//!
//! Run with: `cargo test -p koine-memory --features executors`

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use impress_item::schema::refs::CONTENT_CHUNK as CONTENT_CHUNK_SCHEMA;
use impress_item::{
    ActorKind, EdgeType, Item, ItemId, ItemQuery, ItemStore, Predicate, Priority, SchemaRef,
    StoreError, Value, Visibility,
};
use koine_embeddings::EmbeddingStore;
use koine_memory::executors::consolidate::{
    CONSOLIDATE_AGENT_ID, CONSUMED_THROUGH_FIELD, DETERMINISTIC_MODEL, MAX_SOURCE_RUNS,
    RUN_FINALIZED_FIELD, SOURCE_KIND_AGENT_RUNS, TRUNCATED_FIELD,
};
use koine_memory::executors::embed::{EMBED_AGENT_ID, KIND_EMBED};
use koine_memory::executors::spawn::{FAILED_COOLOFF_MS, WINDOW_LAG_MS, WINDOW_MS};
use koine_memory::executors::{
    plan_memory_tasks, vector_id, ClaimDistiller, EmbedBackfillExecutor, EmbedderProvider,
    MemoryConsolidationExecutor, MemoryPlanConfig, TextEmbedder, KIND_CONSOLIDATE,
    SOURCE_TYPE_CHUNK, SOURCE_TYPE_MEMORY,
};
use koine_memory::ops::{self as memory_ops, MemoryKind, RecallOptions};
use koine_store_memory::MemoryStore;
use koine_tasks::TaskState;
use koine_tasks::{
    ExecutionOutcome, ItemTaskStore, Scheduler, SchedulerConfig, TaskError, TaskExecutor,
    TaskStoreApi, AGENT_RUN_SCHEMA, TASK_SCHEMA,
};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn store() -> Arc<MemoryStore> {
    Arc::new(MemoryStore::new())
}

/// The task kernel's surface over `store` (`koine_tasks::ItemTaskStore`), as
/// the scheduler sees it.
fn tasks(store: &Arc<MemoryStore>) -> ItemTaskStore<MemoryStore> {
    ItemTaskStore::new(store.clone())
}

fn sidecar() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("embeddings.sqlite");
    (dir, path.to_string_lossy().into_owned())
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// A deterministic stand-in for fastembed. Records what it was asked to embed
/// so a test can assert the *text assembly*, which is the half of the embed
/// path that fails silently: a wrong vector still looks like a vector.
struct StubEmbedder {
    model: String,
    seen: Arc<Mutex<Vec<String>>>,
}

impl TextEmbedder for StubEmbedder {
    fn model_id(&self) -> &str {
        &self.model
    }
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        self.seen
            .lock()
            .expect("seen lock")
            .extend(texts.iter().cloned());
        Ok(texts
            .iter()
            .map(|t| vec![t.len() as f32, t.chars().count() as f32, 1.0])
            .collect())
    }
}

struct StubProvider {
    model: String,
    seen: Arc<Mutex<Vec<String>>>,
    /// When set, `embedder_for` fails with this message — the "model host is
    /// down" path.
    fail_with: Option<String>,
}

impl StubProvider {
    fn new(model: &str) -> Self {
        Self {
            model: model.into(),
            seen: Arc::new(Mutex::new(Vec::new())),
            fail_with: None,
        }
    }
}

impl EmbedderProvider for StubProvider {
    fn embedder_for(&self, _model: &str) -> Result<Arc<dyn TextEmbedder>, String> {
        if let Some(message) = &self.fail_with {
            return Err(message.clone());
        }
        Ok(Arc::new(StubEmbedder {
            model: self.model.clone(),
            seen: self.seen.clone(),
        }))
    }
}

/// A deterministic stand-in for [`ClaimDistiller`]'s LLM transport. One fixed
/// reply per instance — every test scenario the executor needs (a clean
/// array, prose-wrapped JSON with a bad entry, `[]`, a transport failure) is
/// just a different `reply` value, and `calls` records every prompt so a test
/// can assert whether — and how many times — the model was actually asked.
struct StubDistiller {
    model: String,
    reply: Result<String, String>,
    calls: Arc<Mutex<Vec<String>>>,
}

impl StubDistiller {
    fn new(model: &str, reply: Result<String, String>) -> Self {
        Self {
            model: model.into(),
            reply,
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.lock().expect("calls lock").len()
    }
}

#[async_trait]
impl ClaimDistiller for StubDistiller {
    fn model_id(&self) -> &str {
        &self.model
    }

    async fn distill(&self, prompt: &str) -> Result<String, String> {
        self.calls
            .lock()
            .expect("calls lock")
            .push(prompt.to_string());
        self.reply.clone()
    }
}

const STUB_MODEL: &str = "stub/test-v1";

fn bare_item(schema: SchemaRef, payload: BTreeMap<String, Value>) -> Item {
    let now = chrono::Utc::now();
    Item {
        id: Uuid::new_v4(),
        schema,
        payload,
        created: now,
        modified: now,
        author: "test".into(),
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
    }
}

fn seed_memory(store: &MemoryStore, kind: MemoryKind, title: &str, body: &str) -> ItemId {
    let draft = memory_ops::MemoryDraft::new(kind, title, body, "test-human", ActorKind::Human);
    memory_ops::insert_memory_item(store, &draft).expect("seed memory")
}

/// [`seed_memory`] with a pinned `created`/`modified` millisecond and,
/// optionally, a pinned item id. The embed cursor orders on `(created, id)`,
/// so the cluster and straggler scenarios must place rows on that axis
/// deliberately instead of inheriting "now" and a random UUID.
fn seed_memory_at(store: &MemoryStore, title: &str, created_ms: i64, id: Option<Uuid>) -> ItemId {
    let mut payload = BTreeMap::new();
    payload.insert("title".into(), Value::String(title.into()));
    payload.insert("body".into(), Value::String(format!("Body of {title}.")));
    let mut item = bare_item(MemoryKind::Claim.schema_ref(), payload);
    let at = chrono::DateTime::from_timestamp_millis(created_ms).expect("timestamp");
    item.created = at;
    item.modified = at;
    if let Some(id) = id {
        item.id = id;
    }
    ItemStore::insert(store, item).expect("seed memory at")
}

fn seed_chunk(store: &MemoryStore, body: &str) -> ItemId {
    let mut payload = BTreeMap::new();
    payload.insert(
        "title".into(),
        Value::String(CONTENT_CHUNK_SCHEMA.to_string()),
    );
    payload.insert("body".into(), Value::String(body.into()));
    ItemStore::insert(store, bare_item(CONTENT_CHUNK_SCHEMA, payload)).expect("seed chunk")
}

/// A chunk whose text lives only in `payload.data.text` — the structured shape,
/// written when the source service had no `indexed_text` to mirror into `body`.
fn seed_chunk_data_only(store: &MemoryStore, text: &str) -> ItemId {
    let mut data = BTreeMap::new();
    data.insert("text".to_string(), Value::String(text.into()));
    let mut payload = BTreeMap::new();
    payload.insert(
        "title".into(),
        Value::String(CONTENT_CHUNK_SCHEMA.to_string()),
    );
    payload.insert("data".into(), Value::Object(data));
    ItemStore::insert(store, bare_item(CONTENT_CHUNK_SCHEMA, payload)).expect("seed chunk")
}

fn insert_task(store: &MemoryStore, payload: BTreeMap<String, Value>) -> ItemId {
    ItemStore::insert(store, bare_item(TASK_SCHEMA, payload)).expect("insert task")
}

fn embed_task(
    store: &MemoryStore,
    cursor_created_ms: i64,
    cursor_id: &str,
    sidecar_path: &str,
) -> ItemId {
    let mut payload = BTreeMap::new();
    payload.insert("title".into(), Value::String("Backfill".into()));
    payload.insert("task_kind".into(), Value::String(KIND_EMBED.into()));
    payload.insert("state".into(), Value::String("pending".into()));
    payload.insert("cursor_created_ms".into(), Value::Int(cursor_created_ms));
    payload.insert("cursor_id".into(), Value::String(cursor_id.into()));
    payload.insert("batch_limit".into(), Value::Int(64));
    payload.insert("model".into(), Value::String(STUB_MODEL.into()));
    payload.insert(
        "sidecar_path".into(),
        Value::String(sidecar_path.to_string()),
    );
    insert_task(store, payload)
}

fn consolidate_task(store: &MemoryStore, start_ms: i64, end_ms: i64) -> ItemId {
    let mut payload = BTreeMap::new();
    payload.insert("title".into(), Value::String("Consolidate".into()));
    payload.insert("task_kind".into(), Value::String(KIND_CONSOLIDATE.into()));
    payload.insert("state".into(), Value::String("pending".into()));
    payload.insert("window_start_ms".into(), Value::Int(start_ms));
    payload.insert("window_end_ms".into(), Value::Int(end_ms));
    payload.insert(
        "source_kind".into(),
        Value::String(SOURCE_KIND_AGENT_RUNS.into()),
    );
    insert_task(store, payload)
}

/// An `agent-run@1.0.0` written the way one of the two real writers writes it.
/// `status: None` is the kernel shape (`record_agent_run` writes no status);
/// `Some("completed")` / `Some("running")` are `AiStore`'s.
fn seed_run(
    store: &MemoryStore,
    agent_id: &str,
    model: &str,
    summary: &str,
    status: Option<&str>,
) -> ItemId {
    seed_run_at(store, agent_id, model, summary, status, now_ms())
}

/// Back-dated variant. `modified` is what the consolidation window filters on,
/// so a test that must exclude rows written *during* its own pass has to place
/// its fixtures in the past deliberately.
fn seed_run_at(
    store: &MemoryStore,
    agent_id: &str,
    model: &str,
    summary: &str,
    status: Option<&str>,
    modified_ms: i64,
) -> ItemId {
    let mut payload = BTreeMap::new();
    payload.insert("agent_id".into(), Value::String(agent_id.into()));
    payload.insert("model".into(), Value::String(model.into()));
    payload.insert("prompt_hash".into(), Value::String("deadbeef".into()));
    payload.insert("result_summary".into(), Value::String(summary.into()));
    payload.insert("token_count".into(), Value::Int(42));
    if let Some(status) = status {
        payload.insert("status".into(), Value::String(status.into()));
    }
    let mut item = bare_item(AGENT_RUN_SCHEMA, payload);
    let at = chrono::DateTime::from_timestamp_millis(modified_ms).expect("timestamp");
    item.created = at;
    item.modified = at;
    ItemStore::insert(store, item).expect("seed run")
}

fn fetch(store: &MemoryStore, id: ItemId) -> Item {
    ItemStore::get(store, id).expect("get").expect("present")
}

fn payload_i64(item: &Item, field: &str) -> Option<i64> {
    match item.payload.get(field) {
        Some(Value::Int(i)) => Some(*i),
        _ => None,
    }
}

fn payload_string(item: &Item, field: &str) -> Option<String> {
    match item.payload.get(field) {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn payload_string_array(item: &Item, field: &str) -> Vec<String> {
    match item.payload.get(field) {
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

/// The one provenance run this executor wrote — mirrors the predicate every
/// consolidation test already filters on to find it.
fn consolidate_run(store: &MemoryStore) -> Item {
    let runs = ItemStore::query(
        store,
        &ItemQuery {
            schema: Some(AGENT_RUN_SCHEMA),
            predicates: vec![Predicate::Eq(
                "payload.agent_id".into(),
                Value::String(CONSOLIDATE_AGENT_ID.into()),
            )],
            include_tags: false,
            ..Default::default()
        },
    )
    .expect("query consolidate run");
    assert_eq!(runs.len(), 1, "exactly one provenance run per execution");
    runs.into_iter().next().expect("one run")
}

fn count_schema(store: &MemoryStore, schema: SchemaRef) -> usize {
    ItemStore::query(
        store,
        &ItemQuery {
            schema: Some(schema),
            include_tags: false,
            include_references: false,
            ..Default::default()
        },
    )
    .expect("query")
    .len()
}

/// Drive one task through the executor exactly as the scheduler would: fetch
/// the current item (references included), execute, done.
async fn run_task(
    executor: &dyn TaskExecutor,
    store: &Arc<MemoryStore>,
    task_id: ItemId,
) -> Result<ExecutionOutcome, TaskError> {
    let task = fetch(store, task_id);
    executor
        .execute(&task, &tasks(store) as &dyn TaskStoreApi)
        .await
}

// ---------------------------------------------------------------------------
// Embed backfill
// ---------------------------------------------------------------------------

#[tokio::test]
async fn embed_writes_one_vector_per_row_with_the_right_model_and_source_type() {
    let store = store();
    let (_dir, path) = sidecar();
    let provider = Arc::new(StubProvider::new(STUB_MODEL));
    let seen = provider.seen.clone();

    let claim = seed_memory(
        &store,
        MemoryKind::Claim,
        "Flux units",
        "The column is in mJy.",
    );
    let chunk = seed_chunk(&store, "A paragraph of extracted PDF text.");
    let data_chunk = seed_chunk_data_only(&store, "Structured chunk text.");

    let executor = EmbedBackfillExecutor::with_provider(store.clone(), provider);
    let task = embed_task(&store, 0, "", &path);
    assert_eq!(
        run_task(&executor, &store, task).await.expect("execute"),
        ExecutionOutcome::Complete
    );

    let sidecar = EmbeddingStore::open(&path).expect("open sidecar");
    assert_eq!(sidecar.vector_count().expect("count"), 3);

    let by_type = |t: &str| {
        sidecar
            .load_vectors_by_type_and_model(t, STUB_MODEL)
            .expect("load")
    };
    let memory_vectors = by_type(SOURCE_TYPE_MEMORY);
    assert_eq!(memory_vectors.len(), 1);
    assert_eq!(memory_vectors[0].source_id, claim.to_string());
    assert_eq!(
        memory_vectors[0].id,
        vector_id(&claim.to_string(), STUB_MODEL).to_string(),
        "the sidecar id must be the pure function of (source_id, model)"
    );

    let chunk_vectors = by_type(SOURCE_TYPE_CHUNK);
    assert_eq!(chunk_vectors.len(), 2);
    let chunk_sources: Vec<String> = chunk_vectors.iter().map(|v| v.source_id.clone()).collect();
    assert!(chunk_sources.contains(&chunk.to_string()));
    assert!(chunk_sources.contains(&data_chunk.to_string()));

    // The text assembly itself: memory rows embed title + body, chunks embed
    // their text — from `body`, or from `data.text` when `body` is absent.
    let seen = seen.lock().expect("seen").clone();
    assert!(seen.contains(&"Flux units\nThe column is in mJy.".to_string()));
    assert!(seen.contains(&"A paragraph of extracted PDF text.".to_string()));
    assert!(seen.contains(&"Structured chunk text.".to_string()));
}

/// Crash replay: the scheduler re-executes a task left `running`. The
/// already-embedded probe skips the replayed rows outright; beneath it, the
/// deterministic vector id keeps any write that does still happen (a crash
/// between the save and the cursor write, say) an UPSERT rather than a
/// duplicate population.
#[tokio::test]
async fn re_executing_the_same_task_upserts_rather_than_duplicating() {
    let store = store();
    let (_dir, path) = sidecar();
    let executor = EmbedBackfillExecutor::with_provider(
        store.clone(),
        Arc::new(StubProvider::new(STUB_MODEL)),
    );

    seed_memory(&store, MemoryKind::Claim, "One", "First body.");
    seed_memory(&store, MemoryKind::Episode, "Two", "Second body.");

    let task = embed_task(&store, 0, "", &path);
    run_task(&executor, &store, task).await.expect("first");
    let after_first = EmbeddingStore::open(&path)
        .expect("open")
        .vector_count()
        .expect("count");

    run_task(&executor, &store, task).await.expect("replay");
    let after_replay = EmbeddingStore::open(&path)
        .expect("open")
        .vector_count()
        .expect("count");

    assert_eq!(after_first, 2);
    assert_eq!(after_replay, after_first, "replay must not add rows");
}

/// The skip itself: a second task over already-embedded ground must not send
/// a single text back through the embedder (the StubEmbedder's `seen` ledger
/// is the witness), and the run summary must say what actually happened
/// rather than dressing a no-op window up as fresh work.
#[tokio::test]
async fn already_embedded_rows_are_skipped_not_re_embedded() {
    let store = store();
    let (_dir, path) = sidecar();
    let provider = Arc::new(StubProvider::new(STUB_MODEL));
    let seen = provider.seen.clone();
    let executor = EmbedBackfillExecutor::with_provider(store.clone(), provider);

    seed_memory(&store, MemoryKind::Claim, "One", "First body.");
    seed_memory(&store, MemoryKind::Claim, "Two", "Second body.");

    let first = embed_task(&store, 0, "", &path);
    run_task(&executor, &store, first).await.expect("first");
    assert_eq!(seen.lock().expect("seen").len(), 2);

    // A second task over the SAME ground — the shape every rewound window
    // takes after a respawn.
    let second = embed_task(&store, 0, "", &path);
    run_task(&executor, &store, second).await.expect("second");

    assert_eq!(
        seen.lock().expect("seen").len(),
        2,
        "already-embedded rows must not reach the embedder again"
    );
    let (first, second) = (fetch(&store, first), fetch(&store, second));
    assert_eq!(payload_i64(&second, "embedded_count"), Some(0));
    // The cursor still lands on the last row FETCHED: a fully-skipped window
    // records the same frontier, never a lesser one.
    assert_eq!(
        payload_string(&second, "cursor_end_id"),
        payload_string(&first, "cursor_end_id")
    );

    let summaries: Vec<String> = ItemStore::query(
        &*store,
        &ItemQuery {
            schema: Some(AGENT_RUN_SCHEMA),
            predicates: vec![Predicate::Eq(
                "payload.agent_id".into(),
                Value::String(EMBED_AGENT_ID.into()),
            )],
            include_tags: false,
            include_references: false,
            ..Default::default()
        },
    )
    .expect("query runs")
    .iter()
    .filter_map(|r| payload_string(r, "result_summary"))
    .collect();
    assert!(
        summaries.contains(&"2 embedded, 0 already embedded, of 2 candidate(s)".to_string()),
        "{summaries:?}"
    );
    assert!(
        summaries.contains(&"0 embedded, 2 already embedded, of 2 candidate(s)".to_string()),
        "{summaries:?}"
    );
}

#[tokio::test]
async fn the_cursor_advances_on_the_task_payload_and_the_next_window_is_empty() {
    let store = store();
    let (_dir, path) = sidecar();
    let executor = EmbedBackfillExecutor::with_provider(
        store.clone(),
        Arc::new(StubProvider::new(STUB_MODEL)),
    );

    seed_memory(&store, MemoryKind::Claim, "One", "First body.");
    seed_memory(&store, MemoryKind::Claim, "Two", "Second body.");

    let first = embed_task(&store, 0, "", &path);
    run_task(&executor, &store, first).await.expect("execute");
    let first = fetch(&store, first);
    assert_eq!(payload_i64(&first, "embedded_count"), Some(2));
    let cursor_created = payload_i64(&first, "cursor_end_created_ms").expect("cursor_end_created");
    let cursor_id = payload_string(&first, "cursor_end_id").expect("cursor_end_id");
    assert!(!cursor_id.is_empty());

    // Continuing from where the first task stopped must find nothing: that is
    // what "the cursor reached the end" means operationally.
    let second = embed_task(&store, cursor_created, &cursor_id, &path);
    run_task(&executor, &store, second).await.expect("execute");
    let second = fetch(&store, second);
    assert_eq!(payload_i64(&second, "embedded_count"), Some(0));
    assert_eq!(
        payload_i64(&second, "cursor_end_created_ms"),
        Some(cursor_created),
        "an empty window leaves the cursor exactly where it was"
    );
    assert_eq!(payload_string(&second, "cursor_end_id"), Some(cursor_id));
}

/// An empty window must not pay a model load to discover it has nothing to do —
/// on a caught-up store that is the common case, once per poll, forever.
#[tokio::test]
async fn an_empty_window_never_asks_for_an_embedder() {
    let store = store();
    let (_dir, path) = sidecar();
    let mut provider = StubProvider::new(STUB_MODEL);
    provider.fail_with = Some("the model must not be requested".into());
    let executor = EmbedBackfillExecutor::with_provider(store.clone(), Arc::new(provider));

    let task = embed_task(&store, 0, "", &path);
    assert_eq!(
        run_task(&executor, &store, task).await.expect("execute"),
        ExecutionOutcome::Complete
    );
    assert_eq!(payload_i64(&fetch(&store, task), "embedded_count"), Some(0));
}

/// Rows with nothing to embed are a data condition, not a failure — and the
/// cursor must still pass them, or the backfill parks behind them forever.
#[tokio::test]
async fn rows_without_text_are_skipped_but_the_cursor_passes_them() {
    let store = store();
    let (_dir, path) = sidecar();
    let executor = EmbedBackfillExecutor::with_provider(
        store.clone(),
        Arc::new(StubProvider::new(STUB_MODEL)),
    );

    // A memory-schema row with no title and no body. `insert_memory_item`
    // would refuse to write this; a foreign writer or a sync merge can.
    let empty = ItemStore::insert(
        &*store,
        bare_item(MemoryKind::Claim.schema_ref(), BTreeMap::new()),
    )
    .expect("insert empty");

    let task = embed_task(&store, 0, "", &path);
    assert_eq!(
        run_task(&executor, &store, task).await.expect("execute"),
        ExecutionOutcome::Complete
    );
    let task = fetch(&store, task);
    assert_eq!(payload_i64(&task, "embedded_count"), Some(0));
    assert_eq!(
        payload_string(&task, "cursor_end_id"),
        Some(empty.to_string()),
        "the cursor advances past a textless row rather than parking on it"
    );
}

/// The model host being unreachable is transient; the task must retry, not fail.
#[tokio::test]
async fn an_unavailable_model_is_retryable() {
    let store = store();
    let (_dir, path) = sidecar();
    let mut provider = StubProvider::new(STUB_MODEL);
    provider.fail_with = Some("connection refused".into());
    let executor = EmbedBackfillExecutor::with_provider(store.clone(), Arc::new(provider));

    seed_memory(&store, MemoryKind::Claim, "One", "First body.");
    let task = embed_task(&store, 0, "", &path);
    let error = run_task(&executor, &store, task)
        .await
        .expect_err("must fail");
    assert!(matches!(error, TaskError::Retryable(_)), "got {error:?}");
    assert!(executor.is_retryable(&error));
}

/// The `is_retryable` override, both executors. The trait default retries
/// `Retryable` only, so a `SQLITE_BUSY` — which arrives as `TaskError::Store` —
/// would fail the task permanently and leave the cursor/window behind forever.
#[test]
fn store_errors_are_retryable_on_both_executors() {
    let store = store();
    let busy = || {
        TaskError::Store(koine_tasks::TaskStoreError::Store(StoreError::Storage(
            "database is locked".into(),
        )))
    };
    let permanent = TaskError::Permanent("no".into());

    let embed = EmbedBackfillExecutor::with_provider(
        store.clone(),
        Arc::new(StubProvider::new(STUB_MODEL)),
    );
    let consolidate = MemoryConsolidationExecutor::new(store.clone());

    for executor in [
        &embed as &dyn TaskExecutor,
        &consolidate as &dyn TaskExecutor,
    ] {
        assert!(
            executor.is_retryable(&busy()),
            "{}: a store error must retry",
            executor.task_kind()
        );
        assert!(
            executor.is_retryable(&TaskError::Retryable("x".into())),
            "{}: the default behavior must survive the override",
            executor.task_kind()
        );
        assert!(
            !executor.is_retryable(&permanent),
            "{}: permanent stays permanent",
            executor.task_kind()
        );
    }
}

// ---------------------------------------------------------------------------
// Consolidation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn consolidation_distils_terminal_runs_and_skips_running_ones() {
    let store = store();
    let executor = MemoryConsolidationExecutor::new(store.clone());
    let start = now_ms() - 60_000;

    let completed = seed_run(
        &store,
        "impel/keyword-tag",
        "heuristic-v1",
        "3 tag proposal(s)",
        Some("completed"),
    );
    // The kernel shape: `record_agent_run` writes NO status field, and its rows
    // are terminal by construction. A `status == "completed"` filter would skip
    // every one of them.
    let kernel = seed_run(
        &store,
        "impel/metadata-resolve",
        "arxiv",
        "resolved 7 field(s) from arXiv",
        None,
    );
    let running = seed_run(
        &store,
        "impress-ai/inference",
        "local-omlx",
        "streaming",
        Some("running"),
    );

    let task = consolidate_task(&store, start, now_ms() + 60_000);
    assert_eq!(
        run_task(&executor, &store, task).await.expect("execute"),
        ExecutionOutcome::Complete
    );

    let episodes = ItemStore::query(
        &*store,
        &ItemQuery {
            schema: Some(MemoryKind::Episode.schema_ref()),
            include_tags: false,
            ..Default::default()
        },
    )
    .expect("query episodes");
    assert_eq!(episodes.len(), 2, "one episode per terminal run, no more");

    let evidence: Vec<String> = episodes
        .iter()
        .flat_map(|e| match e.payload.get("evidence_refs") {
            Some(Value::Array(values)) => values
                .iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            _ => vec![],
        })
        .collect();
    assert!(evidence.contains(&completed.to_string().to_lowercase()));
    assert!(evidence.contains(&kernel.to_string().to_lowercase()));
    assert!(
        !evidence.contains(&running.to_string().to_lowercase()),
        "a run still in flight is not history yet"
    );

    // Recall-able through the kernel, not merely present as rows.
    let recalled = memory_ops::recall(
        &*store,
        "metadata-resolve",
        &RecallOptions {
            kinds: vec![MemoryKind::Episode],
            ..Default::default()
        },
    )
    .expect("recall");
    assert!(
        recalled.iter().any(|e| e.body.contains("arxiv")),
        "the distilled episode must come back from recall: {recalled:?}"
    );
}

/// The episode carries the `task_kind` of the work the run performed — the one
/// field that makes "how did this kind of task go last time?" a query rather
/// than a re-read. It is a best-effort graph walk (run —ProducedBy→ task), so
/// it needs a run written the way the kernel actually writes one.
#[tokio::test]
async fn an_episode_carries_the_task_kind_of_the_run_it_distils() {
    let store = store();
    let executor = MemoryConsolidationExecutor::new(store.clone());

    let mut payload = BTreeMap::new();
    payload.insert("title".into(), Value::String("Resolve metadata".into()));
    payload.insert("task_kind".into(), Value::String("metadata-resolve".into()));
    payload.insert("state".into(), Value::String("pending".into()));
    let source_task = insert_task(&store, payload);
    TaskStoreApi::record_agent_run(
        &tasks(&store),
        source_task,
        koine_tasks::AgentRunRecord {
            agent_id: "impel/metadata-resolve".into(),
            model: "arxiv".into(),
            prompt_hash: "abc".into(),
            result_summary: Some("resolved 7 field(s)".into()),
            token_count: None,
            duration_ms: Some(120),
            executor_kind: Some(koine_tasks::EXECUTOR_DETERMINISTIC.into()),
        },
    )
    .expect("record run");

    let task = consolidate_task(&store, now_ms() - 60_000, now_ms() + 60_000);
    run_task(&executor, &store, task).await.expect("execute");

    let episodes = ItemStore::query(
        &*store,
        &ItemQuery {
            schema: Some(MemoryKind::Episode.schema_ref()),
            include_tags: false,
            include_references: false,
            ..Default::default()
        },
    )
    .expect("query");
    assert_eq!(episodes.len(), 1);
    let episode = &episodes[0];
    assert_eq!(
        payload_string(episode, "task_kind").as_deref(),
        Some("metadata-resolve"),
        "the run's task must be resolved through its ProducedBy edge"
    );
    assert_eq!(
        payload_string(episode, "outcome").as_deref(),
        Some("terminal")
    );
    let body = payload_string(episode, "body").expect("body");
    assert!(body.contains("Task kind: metadata-resolve."), "{body}");
    assert!(body.contains("Task: Resolve metadata."), "{body}");
    assert!(body.contains("Duration: 120 ms."), "{body}");
}

#[tokio::test]
async fn provenance_is_one_run_with_derived_from_edges_to_every_source() {
    let store = store();
    let executor = MemoryConsolidationExecutor::new(store.clone());
    let sources = [
        seed_run(&store, "agent/a", "m1", "did a thing", None),
        seed_run(
            &store,
            "agent/b",
            "m2",
            "did another thing",
            Some("completed"),
        ),
    ];

    let task = consolidate_task(&store, now_ms() - 60_000, now_ms() + 60_000);
    run_task(&executor, &store, task).await.expect("execute");

    let runs = ItemStore::query(
        &*store,
        &ItemQuery {
            schema: Some(AGENT_RUN_SCHEMA),
            predicates: vec![Predicate::Eq(
                "payload.agent_id".into(),
                Value::String(CONSOLIDATE_AGENT_ID.into()),
            )],
            include_tags: false,
            ..Default::default()
        },
    )
    .expect("query runs");
    assert_eq!(runs.len(), 1, "exactly one provenance run per execution");
    let run = &runs[0];

    let derived: Vec<ItemId> = run
        .references
        .iter()
        .filter(|r| r.edge_type == EdgeType::DerivedFrom)
        .map(|r| r.target)
        .collect();
    for source in sources {
        assert!(
            derived.contains(&source),
            "every consumed run gets a DerivedFrom edge"
        );
    }

    let summary = payload_string(run, "result_summary").expect("summary");
    assert!(
        summary.contains("2 episode(s) from 2 run(s)") && summary.contains("2 inserted"),
        "the summary must carry the real counts: {summary}"
    );

    // Every episode points back at that run, via payload AND the ProducedBy
    // edge `insert_memory_item` mirrors it into.
    let episodes = ItemStore::query(
        &*store,
        &ItemQuery {
            schema: Some(MemoryKind::Episode.schema_ref()),
            include_tags: false,
            ..Default::default()
        },
    )
    .expect("query episodes");
    assert_eq!(episodes.len(), 2);
    for episode in &episodes {
        assert_eq!(
            payload_string(episode, "agent_run_ref"),
            Some(run.id.to_string())
        );
        assert!(episode
            .references
            .iter()
            .any(|r| r.edge_type == EdgeType::ProducedBy && r.target == run.id));
    }
}

/// A second task over the same window must add nothing. Both defences are live
/// here: the D6 gate confirms the near-identical body, and the deterministic
/// key would resolve to the same row even if it did not.
#[tokio::test]
async fn a_second_pass_over_the_same_window_adds_no_episodes() {
    let store = store();
    let executor = MemoryConsolidationExecutor::new(store.clone());
    seed_run(&store, "agent/a", "m1", "did a thing", None);
    seed_run(
        &store,
        "agent/b",
        "m2",
        "did another thing",
        Some("completed"),
    );

    let start = now_ms() - 60_000;
    let end = now_ms() + 60_000;

    let first = consolidate_task(&store, start, end);
    run_task(&executor, &store, first).await.expect("first");
    let after_first = count_schema(&store, MemoryKind::Episode.schema_ref());

    let second = consolidate_task(&store, start, end);
    run_task(&executor, &store, second).await.expect("second");
    let after_second = count_schema(&store, MemoryKind::Episode.schema_ref());

    assert_eq!(after_first, 2);
    assert_eq!(
        after_second, after_first,
        "replay must not duplicate memory"
    );

    // And the second pass did not consolidate the FIRST pass's own run — that
    // feedback loop would grow one episode per window forever.
    let self_episodes = ItemStore::query(
        &*store,
        &ItemQuery {
            schema: Some(MemoryKind::Episode.schema_ref()),
            include_tags: false,
            include_references: false,
            ..Default::default()
        },
    )
    .expect("query")
    .into_iter()
    .filter(|e| payload_string(e, "agent_id").as_deref() == Some(CONSOLIDATE_AGENT_ID))
    .count();
    assert_eq!(self_episodes, 0);
}

/// The scheduler's resume pass re-executes a task a crash left `running`. The
/// short-circuit is what keeps that from re-distilling a window — and it must
/// hold from the reverse edge alone, since the two edges `record_agent_run`
/// writes are separate writes and a crash can leave only one.
#[tokio::test]
async fn a_task_that_already_has_its_run_short_circuits() {
    let store = store();
    let executor = MemoryConsolidationExecutor::new(store.clone());
    seed_run(&store, "agent/a", "m1", "did a thing", None);

    let task_id = consolidate_task(&store, now_ms() - 60_000, now_ms() + 60_000);
    let stale = fetch(&store, task_id); // captured BEFORE the run edge exists
    run_task(&executor, &store, task_id).await.expect("first");

    let runs_after_first = count_schema(&store, AGENT_RUN_SCHEMA);
    // Re-execute with the STALE item: its own `references` carry no ProducedBy
    // edge, so only the reverse lookup can save us.
    assert_eq!(
        executor
            .execute(&stale, &tasks(&store) as &dyn TaskStoreApi)
            .await
            .expect("replay"),
        ExecutionOutcome::Complete
    );
    assert_eq!(
        count_schema(&store, AGENT_RUN_SCHEMA),
        runs_after_first,
        "a short-circuited replay writes no second provenance run"
    );
    assert!(
        matches!(
            consolidate_run(&store).payload.get(RUN_FINALIZED_FIELD),
            Some(Value::Bool(true))
        ),
        "a completed window's run carries the finalization marker"
    );
}

/// The failure the finalization marker exists for: the provenance run must be
/// written BEFORE the episodes (each episode cites it as `agent_run_ref`), so a
/// crash in between leaves a marker with nothing behind it. Short-circuiting on
/// the run's mere existence loses that window permanently — the task is
/// `running`, the resume pass re-executes it, sees a run, and reports success.
#[tokio::test]
async fn an_unfinalized_run_replays_the_window_instead_of_short_circuiting() {
    let store = store();
    let executor = MemoryConsolidationExecutor::new(store.clone());
    seed_run(&store, "agent/a", "m1", "did a thing", None);
    seed_run(&store, "agent/b", "m2", "did another thing", None);

    let task_id = consolidate_task(&store, now_ms() - 60_000, now_ms() + 60_000);
    // Exactly what a crash between `record_agent_run` and the first episode
    // insert leaves behind: the run, both its edges, and no memory at all.
    TaskStoreApi::record_agent_run(
        &tasks(&store),
        task_id,
        koine_tasks::AgentRunRecord {
            agent_id: CONSOLIDATE_AGENT_ID.into(),
            model: DETERMINISTIC_MODEL.into(),
            prompt_hash: "interrupted".into(),
            result_summary: Some("2 episode(s) from 2 run(s)".into()),
            ..Default::default()
        },
    )
    .expect("seed interrupted run");
    assert_eq!(count_schema(&store, MemoryKind::Episode.schema_ref()), 0);

    run_task(&executor, &store, task_id).await.expect("resume");

    assert_eq!(
        count_schema(&store, MemoryKind::Episode.schema_ref()),
        2,
        "the resumed pass distils the window the crash dropped"
    );
    // `consolidate_run` asserts there is exactly one: the resume re-uses the
    // interrupted run rather than leaving a trail of half-finished ones.
    let run = consolidate_run(&store);
    assert_eq!(
        payload_string(&run, "prompt_hash").as_deref(),
        Some("interrupted"),
        "the re-used run is the same item, not a replacement"
    );
    assert!(matches!(
        run.payload.get(RUN_FINALIZED_FIELD),
        Some(Value::Bool(true))
    ));

    // And it is now genuinely finished: another pass changes nothing.
    let again = fetch(&store, task_id);
    assert_eq!(
        executor
            .execute(&again, &tasks(&store) as &dyn TaskStoreApi)
            .await
            .expect("second resume"),
        ExecutionOutcome::Complete
    );
    assert_eq!(count_schema(&store, MemoryKind::Episode.schema_ref()), 2);
}

/// A window with more terminal runs than the cap can carry used to complete
/// normally and let the chain start the next window at its `window_end_ms` —
/// so every run past the cap was skipped forever, silently. The tail is handed
/// to a follow-up window instead, and that follow-up jumps the
/// one-window-per-day debounce, since it is distilling history rather than
/// sampling a fresh slice of time.
#[tokio::test]
async fn a_truncated_window_hands_its_tail_to_the_next_window() {
    let store = store();
    let executor = MemoryConsolidationExecutor::new(store.clone());
    let now = now_ms();
    // An hour back, one run per millisecond, so the cursor has somewhere
    // unambiguous to land.
    let base = now - 3_600_000;
    let overflow = MAX_SOURCE_RUNS + 1;
    for i in 0..overflow {
        // Distinct summaries: identical ones are exactly what the D6 gate is
        // for, and a window of them would confirm rather than insert — hiding
        // whether the tail was reached at all. `r{i}`, not a bare number: run
        // 42's body would otherwise differ from every other run's by one token
        // (the fixture's `Tokens: 42`), close enough to confirm on a backend
        // whose text search retrieves by any token rather than by all.
        seed_run_at(
            &store,
            "agent/bulk",
            "m",
            &format!("bulk run r{i}"),
            None,
            base + i as i64,
        );
    }

    let task_id = consolidate_task(&store, base - 1_000, now - WINDOW_LAG_MS);
    run_task(&executor, &store, task_id).await.expect("execute");

    let task = fetch(&store, task_id);
    assert!(
        matches!(task.payload.get(TRUNCATED_FIELD), Some(Value::Bool(true))),
        "the window says it stopped short of its own end"
    );
    assert_eq!(
        payload_i64(&task, CONSUMED_THROUGH_FIELD),
        Some(base + (MAX_SOURCE_RUNS - 1) as i64),
        "the cursor is the last run actually consumed"
    );
    assert_eq!(
        count_schema(&store, MemoryKind::Episode.schema_ref()),
        MAX_SOURCE_RUNS,
        "exactly the capped window was distilled"
    );
    assert!(consolidate_run(&store)
        .payload
        .get("result_summary")
        .map(|v| matches!(v, Value::String(s) if s.contains("truncated")))
        .unwrap_or(false));

    drive(&store, task_id, TaskState::Done);
    let planned = plan_memory_tasks(&*store, now, &plan_config(false, true)).expect("plan");
    assert_eq!(
        planned.len(),
        1,
        "the tail is planned now, not after the daily debounce"
    );
    let follow_up = fetch(&store, planned[0]);
    assert_eq!(
        payload_i64(&follow_up, "window_start_ms"),
        payload_i64(&task, CONSUMED_THROUGH_FIELD),
        "the follow-up resumes at the cursor, re-scanning that millisecond"
    );

    // Draining the tail returns the chain to its ordinary daily cadence.
    run_task(&executor, &store, planned[0]).await.expect("tail");
    let tail = fetch(&store, planned[0]);
    assert!(
        !tail.payload.contains_key(TRUNCATED_FIELD),
        "the follow-up fits, so it claims no tail of its own"
    );
    assert_eq!(
        count_schema(&store, MemoryKind::Episode.schema_ref()),
        overflow,
        "every seeded run is distilled exactly once across the two windows"
    );
    drive(&store, planned[0], TaskState::Done);
    assert!(
        plan_memory_tasks(&*store, now, &plan_config(false, true))
            .expect("plan")
            .is_empty(),
        "one window per day again"
    );
}

#[tokio::test]
async fn an_empty_window_still_records_a_run() {
    let store = store();
    let executor = MemoryConsolidationExecutor::new(store.clone());
    let task = consolidate_task(&store, now_ms() - 120_000, now_ms() - 60_000);
    assert_eq!(
        run_task(&executor, &store, task).await.expect("execute"),
        ExecutionOutcome::Complete
    );
    assert_eq!(count_schema(&store, MemoryKind::Episode.schema_ref()), 0);
    assert_eq!(
        count_schema(&store, AGENT_RUN_SCHEMA),
        1,
        "an empty window is still a window that was examined"
    );
}

#[tokio::test]
async fn an_unknown_source_kind_is_permanent() {
    let store = store();
    let executor = MemoryConsolidationExecutor::new(store.clone());
    let mut payload = BTreeMap::new();
    payload.insert("title".into(), Value::String("Consolidate".into()));
    payload.insert("task_kind".into(), Value::String(KIND_CONSOLIDATE.into()));
    payload.insert("state".into(), Value::String("pending".into()));
    payload.insert("window_start_ms".into(), Value::Int(0));
    payload.insert("window_end_ms".into(), Value::Int(now_ms()));
    payload.insert("source_kind".into(), Value::String("transcripts".into()));
    let task = insert_task(&store, payload);

    let error = run_task(&executor, &store, task)
        .await
        .expect_err("must fail");
    assert!(matches!(error, TaskError::Permanent(_)), "got {error:?}");
    assert!(!executor.is_retryable(&error));
}

// ---------------------------------------------------------------------------
// Claim-distillation tier (ADR-0028 P6)
// ---------------------------------------------------------------------------

/// `None` — what [`MemoryConsolidationExecutor::new`] sets and every test
/// above this section constructs — must reproduce v1's deterministic-only
/// behavior byte for byte: no claim suffix on the summary, no claim rows, and
/// the provenance run's `model` unchanged. This is the contract that keeps
/// every pre-P6 test in this file passing unmodified.
#[tokio::test]
async fn no_distiller_configured_reproduces_v1_behavior_byte_for_byte() {
    let store = store();
    let executor = MemoryConsolidationExecutor::new(store.clone());
    seed_run(&store, "agent/a", "m1", "did a thing", None);
    seed_run(
        &store,
        "agent/b",
        "m2",
        "did another thing",
        Some("completed"),
    );

    let task = consolidate_task(&store, now_ms() - 60_000, now_ms() + 60_000);
    run_task(&executor, &store, task).await.expect("execute");

    assert_eq!(count_schema(&store, MemoryKind::Claim.schema_ref()), 0);
    let run = consolidate_run(&store);
    assert_eq!(
        payload_string(&run, "model").as_deref(),
        Some(DETERMINISTIC_MODEL)
    );
    assert_eq!(
        payload_string(&run, "result_summary").as_deref(),
        Some("2 episode(s) from 2 run(s) (2 inserted, 0 confirmed)"),
        "byte-for-byte: no claim suffix may be appended when no distiller is configured"
    );
}

/// The core happy path: a clean JSON reply becomes gated claim drafts, and
/// those drafts are actually written — alongside, not instead of, the
/// deterministic episodes.
#[tokio::test]
async fn claims_are_parsed_gated_and_written_alongside_episodes() {
    let store = store();
    let run = seed_run(&store, "agent/a", "m1", "did a thing", None);
    let reply = format!(
        r#"[{{"title":"Flux units","body":"The 2018 catalogue's flux column is in mJy, not Jy.","claim_type":"fact","confidence":0.9,"about_run_ids":["{run}"]}}]"#
    );
    let stub = Arc::new(StubDistiller::new("stub/claim-v1", Ok(reply)));
    let distiller: Arc<dyn ClaimDistiller> = stub.clone();
    let executor =
        MemoryConsolidationExecutor::new(store.clone()).with_claim_distiller(Some(distiller));

    let task = consolidate_task(&store, now_ms() - 60_000, now_ms() + 60_000);
    assert_eq!(
        run_task(&executor, &store, task).await.expect("execute"),
        ExecutionOutcome::Complete
    );

    assert_eq!(stub.call_count(), 1);
    assert_eq!(
        count_schema(&store, MemoryKind::Episode.schema_ref()),
        1,
        "the deterministic tier still runs unchanged"
    );
    let claims = ItemStore::query(
        &*store,
        &ItemQuery {
            schema: Some(MemoryKind::Claim.schema_ref()),
            include_tags: false,
            ..Default::default()
        },
    )
    .expect("query claims");
    assert_eq!(claims.len(), 1);
    let claim = &claims[0];
    assert_eq!(
        payload_string(claim, "title").as_deref(),
        Some("Flux units")
    );
    assert_eq!(payload_string(claim, "claim_type").as_deref(), Some("fact"));
    assert_eq!(claim.author, "impel-memory");
    assert_eq!(claim.author_kind, ActorKind::Agent);
    assert!(
        payload_string_array(claim, "evidence_refs").contains(&run.to_string()),
        "about_run_ids naming a run actually in the window must become evidence_refs"
    );

    let provenance = consolidate_run(&store);
    assert_eq!(
        payload_string(&provenance, "model").as_deref(),
        Some("stub/claim-v1"),
        "the run's model must name the LLM tier that actually ran"
    );
    assert_eq!(
        payload_string(claim, "agent_run_ref").as_deref(),
        Some(provenance.id.to_string().as_str())
    );
    let summary = payload_string(&provenance, "result_summary").expect("summary");
    assert!(
        summary.contains("1 claim(s) distilled (1 inserted, 0 confirmed) via stub/claim-v1"),
        "{summary}"
    );

    // Recall-able through the kernel, not merely present as a row.
    let recalled = memory_ops::recall(
        &*store,
        "flux mJy",
        &RecallOptions {
            kinds: vec![MemoryKind::Claim],
            ..Default::default()
        },
    )
    .expect("recall");
    assert!(
        recalled.iter().any(|c| c.title == "Flux units"),
        "the distilled claim must come back from recall: {recalled:?}"
    );
}

/// A model reply naming a run id that is NOT in this window (or not a real
/// item at all) must cost the citation, not the claim.
#[tokio::test]
async fn about_run_ids_outside_the_window_are_dropped_from_evidence_refs() {
    let store = store();
    seed_run(&store, "agent/a", "m1", "did a thing", None);
    let reply = r#"[{"title":"Stray citation","body":"A claim citing a run outside this window.","claim_type":"fact","confidence":0.5,"about_run_ids":["11111111-1111-4111-8111-111111111111"]}]"#;
    let stub = Arc::new(StubDistiller::new("stub/claim-v1", Ok(reply.to_string())));
    let distiller: Arc<dyn ClaimDistiller> = stub.clone();
    let executor =
        MemoryConsolidationExecutor::new(store.clone()).with_claim_distiller(Some(distiller));

    let task = consolidate_task(&store, now_ms() - 60_000, now_ms() + 60_000);
    run_task(&executor, &store, task).await.expect("execute");

    let claims = ItemStore::query(
        &*store,
        &ItemQuery {
            schema: Some(MemoryKind::Claim.schema_ref()),
            include_tags: false,
            ..Default::default()
        },
    )
    .expect("query claims");
    assert_eq!(claims.len(), 1);
    assert!(
        payload_string_array(&claims[0], "evidence_refs").is_empty(),
        "a run id outside the window must not become evidence"
    );
}

/// Prose around the array, and one malformed entry inside it, must not sink
/// the well-formed claims either side of it — exercised through the full
/// executor, not just the parser.
#[tokio::test]
async fn malformed_json_through_the_executor_drops_bad_entries_keeps_good_ones() {
    let store = store();
    seed_run(&store, "agent/a", "m1", "did a thing", None);
    let reply = "Here is what I found:\n\
        [{\"title\":\"A\",\"body\":\"Uses Rust for the core.\",\"claim_type\":\"fact\",\"confidence\":0.8,\"about_run_ids\":[]},\
        {\"oops\":true},\
        {\"title\":\"B\",\"body\":\"Prefers Typst over LaTeX.\",\"claim_type\":\"preference\",\"confidence\":0.7,\"about_run_ids\":[]}]\n\
        That's everything.";
    let stub = Arc::new(StubDistiller::new("stub/claim-v1", Ok(reply.to_string())));
    let distiller: Arc<dyn ClaimDistiller> = stub.clone();
    let executor =
        MemoryConsolidationExecutor::new(store.clone()).with_claim_distiller(Some(distiller));

    let task = consolidate_task(&store, now_ms() - 60_000, now_ms() + 60_000);
    run_task(&executor, &store, task).await.expect("execute");

    assert_eq!(
        count_schema(&store, MemoryKind::Claim.schema_ref()),
        2,
        "the malformed middle entry must be dropped, the two good ones kept"
    );
}

/// `Ok("[]")` — the model was asked and found nothing durable — is a
/// completed LLM-tier run, not an unconfigured or failed one: the run's
/// `model` must say so even though zero claims were written.
#[tokio::test]
async fn empty_array_reply_yields_zero_claims_but_the_run_records_the_model() {
    let store = store();
    seed_run(&store, "agent/a", "m1", "did a thing", None);
    let stub = Arc::new(StubDistiller::new("stub/claim-v1", Ok("[]".to_string())));
    let distiller: Arc<dyn ClaimDistiller> = stub.clone();
    let executor =
        MemoryConsolidationExecutor::new(store.clone()).with_claim_distiller(Some(distiller));

    let task = consolidate_task(&store, now_ms() - 60_000, now_ms() + 60_000);
    run_task(&executor, &store, task).await.expect("execute");

    assert_eq!(count_schema(&store, MemoryKind::Claim.schema_ref()), 0);
    assert_eq!(
        stub.call_count(),
        1,
        "an empty result still required asking the model"
    );

    let run = consolidate_run(&store);
    assert_eq!(
        payload_string(&run, "model").as_deref(),
        Some("stub/claim-v1")
    );
    let summary = payload_string(&run, "result_summary").expect("summary");
    assert!(
        summary.contains("0 claim(s) distilled (0 inserted, 0 confirmed) via stub/claim-v1"),
        "{summary}"
    );
}

/// The defining P6 safety property: an unreachable model host must degrade
/// the window to "no claims this time", never retry-loop a task whose
/// deterministic half already succeeded.
#[tokio::test]
async fn a_distiller_transport_failure_degrades_to_the_deterministic_result() {
    let store = store();
    seed_run(&store, "agent/a", "m1", "did a thing", None);
    let stub = Arc::new(StubDistiller::new(
        "stub/claim-v1",
        Err("connection refused".to_string()),
    ));
    let distiller: Arc<dyn ClaimDistiller> = stub.clone();
    let executor =
        MemoryConsolidationExecutor::new(store.clone()).with_claim_distiller(Some(distiller));

    let task = consolidate_task(&store, now_ms() - 60_000, now_ms() + 60_000);
    assert_eq!(
        run_task(&executor, &store, task)
            .await
            .expect("must still complete"),
        ExecutionOutcome::Complete,
        "a dead LLM host must not fail or retry-loop the task"
    );

    assert_eq!(
        count_schema(&store, MemoryKind::Episode.schema_ref()),
        1,
        "the deterministic tier is unaffected by the LLM transport failure"
    );
    assert_eq!(count_schema(&store, MemoryKind::Claim.schema_ref()), 0);

    let run = consolidate_run(&store);
    assert_eq!(
        payload_string(&run, "model").as_deref(),
        Some(DETERMINISTIC_MODEL),
        "the LLM tier did not complete, so the run's model must not claim it did"
    );
    let summary = payload_string(&run, "result_summary").expect("summary");
    assert!(
        summary.contains("llm: \"unavailable\""),
        "the summary must note the LLM tier was unavailable: {summary}"
    );
}

/// Nothing to distil, and a call would be a wasted round trip — the same
/// discipline the embed executor already applies to its own model dependency.
#[tokio::test]
async fn an_empty_window_with_a_distiller_configured_never_calls_it() {
    let store = store();
    let stub = Arc::new(StubDistiller::new(
        "stub/claim-v1",
        Err("must not be called".to_string()),
    ));
    let distiller: Arc<dyn ClaimDistiller> = stub.clone();
    let executor =
        MemoryConsolidationExecutor::new(store.clone()).with_claim_distiller(Some(distiller));

    let task = consolidate_task(&store, now_ms() - 120_000, now_ms() - 60_000);
    assert_eq!(
        run_task(&executor, &store, task).await.expect("execute"),
        ExecutionOutcome::Complete
    );
    assert_eq!(
        stub.call_count(),
        0,
        "an empty window must not pay a round trip for nothing"
    );
}

/// The D6 dedup gate is the ONLY defence against the same claim recurring
/// across DIFFERENT windows (the deterministic_key is window-scoped, so it
/// cannot catch this on its own — see `claim_draft`'s doc comment). Two
/// disjoint windows, the same fixed reply both times: the second pass must
/// confirm the first pass's claim rather than writing a second row.
#[tokio::test]
async fn a_duplicate_claim_across_two_windows_confirms_not_duplicates() {
    let store = store();
    let reply = Ok(r#"[{"title":"Prefers Typst","body":"The user prefers Typst over LaTeX for manuscript authoring.","claim_type":"preference","confidence":0.8,"about_run_ids":[]}]"#.to_string());
    let stub = Arc::new(StubDistiller::new("stub/claim-v1", reply));
    let distiller: Arc<dyn ClaimDistiller> = stub.clone();
    let executor =
        MemoryConsolidationExecutor::new(store.clone()).with_claim_distiller(Some(distiller));

    let now = now_ms();
    // Two disjoint windows, each with its own seeded run, so the tier is
    // actually invoked both times rather than skipped as empty.
    seed_run_at(&store, "agent/a", "m1", "window one", None, now - 180_000);
    let first = consolidate_task(&store, now - 200_000, now - 100_000);
    run_task(&executor, &store, first)
        .await
        .expect("first window");

    seed_run_at(&store, "agent/b", "m2", "window two", None, now - 30_000);
    let second = consolidate_task(&store, now - 90_000, now + 10_000);
    run_task(&executor, &store, second)
        .await
        .expect("second window");

    assert_eq!(stub.call_count(), 2, "each window must ask the model once");
    assert_eq!(
        count_schema(&store, MemoryKind::Claim.schema_ref()),
        1,
        "the same claim re-derived in a different window must confirm, not duplicate"
    );

    let claims = ItemStore::query(
        &*store,
        &ItemQuery {
            schema: Some(MemoryKind::Claim.schema_ref()),
            include_tags: false,
            include_references: false,
            ..Default::default()
        },
    )
    .expect("query claims");
    assert_eq!(claims.len(), 1);
    assert_eq!(
        payload_i64(&claims[0], "confirmations"),
        Some(1),
        "the second window's pass must have confirmed the first window's claim"
    );
}

/// Claims are capped at 5 per window even when a stub (standing in for a
/// model that ignored the prompt's instruction) returns more.
#[tokio::test]
async fn claims_are_capped_at_five_per_window_through_the_executor() {
    let store = store();
    seed_run(&store, "agent/a", "m1", "did a thing", None);
    let items: Vec<String> = (0..8)
        .map(|i| {
            format!(
                r#"{{"title":"T{i}","body":"Distinct durable claim body number {i}, long enough to avoid collisions.","claim_type":"fact","confidence":0.5,"about_run_ids":[]}}"#
            )
        })
        .collect();
    let reply = format!("[{}]", items.join(","));
    let stub = Arc::new(StubDistiller::new("stub/claim-v1", Ok(reply)));
    let distiller: Arc<dyn ClaimDistiller> = stub.clone();
    let executor =
        MemoryConsolidationExecutor::new(store.clone()).with_claim_distiller(Some(distiller));

    let task = consolidate_task(&store, now_ms() - 60_000, now_ms() + 60_000);
    run_task(&executor, &store, task).await.expect("execute");

    assert_eq!(count_schema(&store, MemoryKind::Claim.schema_ref()), 5);
}

// ---------------------------------------------------------------------------
// The spawner
// ---------------------------------------------------------------------------

fn plan_config(enabled_embed: bool, enabled_consolidate: bool) -> MemoryPlanConfig {
    MemoryPlanConfig {
        embed_enabled: enabled_embed,
        consolidate_enabled: enabled_consolidate,
        model: STUB_MODEL.into(),
        sidecar_path: Some("/dev/null/unused".into()),
        ..MemoryPlanConfig::default()
    }
}

/// Move a task through the kernel's own transitions, so the fixture is a state
/// the scheduler could actually have produced.
fn drive(store: &Arc<MemoryStore>, task: ItemId, to: TaskState) {
    let tasks = tasks(store);
    TaskStoreApi::transition(&tasks, task, TaskState::Running, "test", None).expect("running");
    if to != TaskState::Running {
        TaskStoreApi::transition(&tasks, task, to, "test", None).expect("terminal");
    }
}

#[test]
fn nothing_is_planned_while_the_gates_are_off() {
    let store = store();
    seed_memory(&store, MemoryKind::Claim, "One", "First body.");
    let planned = plan_memory_tasks(&*store, now_ms(), &MemoryPlanConfig::default()).expect("plan");
    assert!(planned.is_empty(), "a deploy is inert until switched on");
}

#[test]
fn the_first_embed_task_looks_back_one_window_from_an_empty_cursor() {
    let store = store();
    seed_memory(&store, MemoryKind::Claim, "One", "First body.");
    let now = now_ms();

    let planned = plan_memory_tasks(&*store, now, &plan_config(true, false)).expect("plan");
    assert_eq!(planned.len(), 1);
    let task = fetch(&store, planned[0]);
    assert_eq!(
        payload_string(&task, "task_kind").as_deref(),
        Some(KIND_EMBED)
    );
    assert_eq!(
        payload_i64(&task, "cursor_created_ms"),
        Some(now - WINDOW_MS)
    );
    assert_eq!(payload_string(&task, "cursor_id").as_deref(), Some(""));
    assert_eq!(payload_string(&task, "model").as_deref(), Some(STUB_MODEL));
    assert_eq!(payload_string(&task, "state").as_deref(), Some("pending"));
}

#[test]
fn no_embed_task_is_planned_when_nothing_is_past_the_cursor() {
    let store = store();
    let planned = plan_memory_tasks(&*store, now_ms(), &plan_config(true, false)).expect("plan");
    assert!(
        planned.is_empty(),
        "a task whose only work is to find no work must not be spawned"
    );
}

#[test]
fn an_open_task_blocks_a_second_spawn_of_the_same_kind() {
    let store = store();
    seed_memory(&store, MemoryKind::Claim, "One", "First body.");
    let config = plan_config(true, true);
    let now = now_ms();

    let first = plan_memory_tasks(&*store, now, &config).expect("plan");
    assert_eq!(first.len(), 2, "one of each kind");

    let second = plan_memory_tasks(&*store, now, &config).expect("plan");
    assert!(second.is_empty(), "pending tasks gate both kinds");

    // `running` gates too — the whole point is one sweep at a time.
    for id in &first {
        drive(&store, *id, TaskState::Running);
    }
    let third = plan_memory_tasks(&*store, now, &config).expect("plan");
    assert!(third.is_empty(), "a running sweep gates the next one");
}

#[test]
fn a_failed_task_cools_off_for_an_hour() {
    let store = store();
    seed_memory(&store, MemoryKind::Claim, "One", "First body.");
    let config = plan_config(true, false);
    let now = now_ms();

    let first = plan_memory_tasks(&*store, now, &config).expect("plan");
    assert_eq!(first.len(), 1);
    drive(&store, first[0], TaskState::Failed);

    assert!(
        plan_memory_tasks(&*store, now, &config)
            .expect("plan")
            .is_empty(),
        "a fresh failure must not be retried on the very next pass"
    );
    let later = plan_memory_tasks(&*store, now + FAILED_COOLOFF_MS + 1_000, &config).expect("plan");
    assert_eq!(later.len(), 1, "the cooloff expires");
}

#[test]
fn the_embed_cursor_chains_through_completed_tasks() {
    let store = store();
    // Pinned, distinct milliseconds: the plan gate probes the EXACT recorded
    // cursor, so the second row must sit strictly after the first for the
    // second spawn to be deterministic. Seeding both at "now" made this a
    // coin flip on UUID order whenever they landed in one millisecond — the
    // CI flake (17746a23) the window rewind was introduced for.
    let base_ms = now_ms() - 60_000;
    let one = seed_memory_at(&store, "One", base_ms, None);
    seed_memory_at(&store, "Two", base_ms + 10, None);
    let config = plan_config(true, false);
    let now = now_ms();

    let first = plan_memory_tasks(&*store, now, &config).expect("plan")[0];
    // Stand in for the executor: record where it stopped, then complete.
    let cursor_created = fetch(&store, one).created.timestamp_millis();
    for (field, value) in [
        ("cursor_end_created_ms", Value::Int(cursor_created)),
        ("cursor_end_id", Value::String(one.to_string())),
    ] {
        TaskStoreApi::apply(
            &tasks(&store),
            impress_item::OperationSpec {
                target_id: first,
                op_type: impress_item::OperationType::SetPayload(field.into(), value),
                intent: impress_item::OperationIntent::Routine,
                reason: None,
                batch_id: None,
                author: "test".into(),
                author_kind: ActorKind::Agent,
                retention: impress_item::RetentionTier::Compactable,
            },
        )
        .expect("set cursor");
    }
    drive(&store, first, TaskState::Done);

    let second = plan_memory_tasks(&*store, now, &config).expect("plan");
    assert_eq!(second.len(), 1, "there is still a row past the cursor");
    let task = fetch(&store, second[0]);
    assert_eq!(
        payload_i64(&task, "cursor_created_ms"),
        Some(cursor_created - 1),
        "the next window overlaps the boundary millisecond (same-ms rows \
         whose id sorts below the recorded cursor must not be skipped)"
    );
    assert_eq!(
        payload_string(&task, "cursor_id"),
        Some(String::new()),
        "the id resets with the overlap; upserts make the re-scan free"
    );
}

#[test]
fn consolidation_windows_are_contiguous_and_daily() {
    let store = store();
    let config = plan_config(false, true);
    let now = now_ms();

    let first = plan_memory_tasks(&*store, now, &config).expect("plan");
    assert_eq!(first.len(), 1);
    let first_task = fetch(&store, first[0]);
    assert_eq!(
        payload_i64(&first_task, "window_start_ms"),
        Some(now - WINDOW_MS)
    );
    assert_eq!(
        payload_i64(&first_task, "window_end_ms"),
        Some(now - WINDOW_LAG_MS),
        "the window ends behind now, so a settling run is judged after it finishes"
    );
    drive(&store, first[0], TaskState::Done);

    assert!(
        plan_memory_tasks(&*store, now, &config)
            .expect("plan")
            .is_empty(),
        "one window per day — not one per poll interval"
    );

    let next_day = now + WINDOW_MS;
    let second = plan_memory_tasks(&*store, next_day, &config).expect("plan");
    assert_eq!(second.len(), 1);
    let second_task = fetch(&store, second[0]);
    assert_eq!(
        payload_i64(&second_task, "window_start_ms"),
        payload_i64(&first_task, "window_end_ms"),
        "windows abut: no slice of time is examined twice or skipped"
    );
}

// ---------------------------------------------------------------------------
// The embed cursor chain, end to end (spawn → execute → complete → spawn)
// ---------------------------------------------------------------------------

/// An embed-only plan config pointed at a real sidecar, with a batch small
/// enough for a test to build clusters larger than it.
fn chain_config(sidecar_path: &str, batch_limit: i64) -> MemoryPlanConfig {
    MemoryPlanConfig {
        embed_enabled: true,
        consolidate_enabled: false,
        batch_limit,
        model: STUB_MODEL.into(),
        sidecar_path: Some(sidecar_path.to_string()),
    }
}

/// Drive spawn → execute → complete until the spawner plans nothing, exactly
/// as taskd would, and return how many tasks ran. Panics past `max_rounds`:
/// a chain that will not quiesce is this crate's livelock defect, and a
/// hanging test is the worst possible way to report it.
async fn run_embed_chain(
    store: &Arc<MemoryStore>,
    executor: &EmbedBackfillExecutor,
    config: &MemoryPlanConfig,
    max_rounds: usize,
) -> usize {
    for round in 0..max_rounds {
        let planned = plan_memory_tasks(store.as_ref(), now_ms(), config).expect("plan");
        if planned.is_empty() {
            return round;
        }
        assert_eq!(planned.len(), 1, "an embed-only config plans one task");
        run_task(executor, store, planned[0])
            .await
            .expect("execute");
        drive(store, planned[0], TaskState::Done);
        // `newest_done_task` orders by `modified` at millisecond resolution;
        // a paused beat keeps each completion strictly newer than the last,
        // so the next plan reads THIS task's cursor rather than tying with
        // an earlier one.
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
    panic!("embed chain did not quiesce within {max_rounds} rounds — cursor livelock?");
}

/// THE livelock this crate shipped with: one created-millisecond holding more
/// embeddable rows than `batch_limit`. The rewound window re-selected the
/// same first `batch_limit` rows on every task — each run reporting a full
/// batch embedded — while the rest of the cluster and ALL later data never
/// got a vector. The executor's already-embedded skip pages through the
/// cluster within one run; the chain must both finish it and then stop.
#[tokio::test]
async fn a_same_millisecond_cluster_larger_than_batch_limit_completes_and_quiesces() {
    let store = store();
    let (_dir, path) = sidecar();
    let executor = EmbedBackfillExecutor::with_provider(
        store.clone(),
        Arc::new(StubProvider::new(STUB_MODEL)),
    );
    let config = chain_config(&path, 4);

    // 3x batch_limit rows in ONE millisecond, as a bulk import writes them.
    let cluster_ms = now_ms() - 60_000;
    let rows: Vec<ItemId> = (0..12)
        .map(|i| seed_memory_at(&store, &format!("Row {i}"), cluster_ms, None))
        .collect();

    let rounds = run_embed_chain(&store, &executor, &config, 10).await;
    assert!(
        rounds >= 3,
        "12 rows at batch 4 need at least three windows, got {rounds}"
    );

    let sidecar = EmbeddingStore::open(&path).expect("open sidecar");
    for row in &rows {
        assert!(
            !sidecar
                .get_vectors(&row.to_string())
                .expect("get")
                .is_empty(),
            "row {row} never received a vector — the cluster livelock is back"
        );
    }
    assert!(
        plan_memory_tasks(&*store, now_ms(), &config)
            .expect("plan")
            .is_empty(),
        "a completed cluster must not spawn another task"
    );
}

/// The quiescence half of the same defect: a caught-up store used to re-spawn
/// — and re-embed — its boundary rows once per poll interval, forever,
/// because the plan gate probed the rewound window instead of the exact
/// recorded cursor.
#[tokio::test]
async fn a_caught_up_store_plans_no_further_embed_task() {
    let store = store();
    let (_dir, path) = sidecar();
    let provider = Arc::new(StubProvider::new(STUB_MODEL));
    let seen = provider.seen.clone();
    let executor = EmbedBackfillExecutor::with_provider(store.clone(), provider);
    let config = chain_config(&path, 4);

    seed_memory_at(&store, "One", now_ms() - 60_000, None);
    seed_memory_at(&store, "Two", now_ms() - 50_000, None);
    run_embed_chain(&store, &executor, &config, 10).await;

    assert_eq!(
        seen.lock().expect("seen").len(),
        2,
        "each row is embedded exactly once on the way to caught-up"
    );
    // Quiescence is not one lucky pass: poll again, now and later.
    for now in [now_ms(), now_ms() + 60_000, now_ms() + WINDOW_MS] {
        assert!(
            plan_memory_tasks(&*store, now, &config)
                .expect("plan")
                .is_empty(),
            "a caught-up store must plan nothing (t = {now})"
        );
    }
}

/// The straggler the window rewind exists for (17746a23): a row written into
/// the boundary millisecond AFTER that window completed, with an id sorting
/// below the recorded cursor. The exact gate cannot see it — that is the
/// documented price of quiescence — so it must ride along on the next spawn
/// that genuinely new work triggers.
#[tokio::test]
async fn a_boundary_straggler_is_embedded_on_the_next_new_work_spawn() {
    let store = store();
    let (_dir, path) = sidecar();
    let executor = EmbedBackfillExecutor::with_provider(
        store.clone(),
        Arc::new(StubProvider::new(STUB_MODEL)),
    );
    let config = chain_config(&path, 4);

    // A pinned high-sorting id, so the straggler below deterministically
    // sorts under the recorded cursor instead of 50/50 on random UUIDs — the
    // coin flip that motivated the rewind in the first place.
    let boundary_ms = now_ms() - 60_000;
    let high = Uuid::parse_str("eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee").expect("uuid");
    seed_memory_at(&store, "Boundary", boundary_ms, Some(high));
    run_embed_chain(&store, &executor, &config, 10).await;

    // Same millisecond as the recorded cursor, lower-sorting id. On its own
    // it must NOT wake the chain.
    let low = Uuid::parse_str("11111111-1111-4111-8111-111111111111").expect("uuid");
    let straggler = seed_memory_at(&store, "Straggler", boundary_ms, Some(low));
    assert!(
        plan_memory_tasks(&*store, now_ms(), &config)
            .expect("plan")
            .is_empty(),
        "a straggler alone is invisible to the exact gate — picked up \
         eventually, not immediately"
    );

    // Genuinely new work arrives; the rewound window carries the straggler in.
    let fresh = seed_memory_at(&store, "Fresh", now_ms() - 1_000, None);
    run_embed_chain(&store, &executor, &config, 10).await;

    let sidecar = EmbeddingStore::open(&path).expect("open sidecar");
    for (label, id) in [("straggler", straggler), ("fresh row", fresh)] {
        assert!(
            !sidecar
                .get_vectors(&id.to_string())
                .expect("get")
                .is_empty(),
            "{label} must be embedded by the rewound window"
        );
    }
}

// ---------------------------------------------------------------------------
// Through the real scheduler
// ---------------------------------------------------------------------------

/// The executors must work through `Scheduler::run_once`, not merely when
/// called directly: that is where acquire/transition/complete actually happen,
/// and where an executor that transitioned state itself would break.
#[tokio::test]
async fn the_scheduler_drives_both_kinds_to_done() {
    let store = store();
    let (_dir, path) = sidecar();
    let mut scheduler = Scheduler::new(
        Arc::new(tasks(&store)),
        SchedulerConfig {
            actor: "impel-test".into(),
            batch: 8,
            start_delay: std::time::Duration::ZERO,
            poll_interval: std::time::Duration::ZERO,
            retry_base_ms: 0, // tight-loop tests: no retry delay
        },
    );
    scheduler.register(Arc::new(
        EmbedBackfillExecutor::with_provider(
            store.clone(),
            Arc::new(StubProvider::new(STUB_MODEL)),
        )
        .with_sidecar_path(path.clone()),
    ));
    scheduler.register(Arc::new(MemoryConsolidationExecutor::new(store.clone())));

    let claim = seed_memory(&store, MemoryKind::Claim, "One", "First body.");
    // Back-dated, and the window closes a second before this pass begins. Both
    // executors write rows the *other* one would otherwise pick up — the embed
    // executor's provenance run is a consolidatable agent-run, and a distilled
    // episode is an embeddable memory item — and `ready_tasks` orders by
    // `created` alone, which does not separate two rows written in the same
    // millisecond. Pinning the window is what makes this assertion about the
    // wiring rather than about which task happened to go first.
    let now = now_ms();
    seed_run_at(&store, "agent/a", "m1", "did a thing", None, now - 10_000);
    let embed = embed_task(&store, 0, "", &path);
    let consolidate = consolidate_task(&store, now - 30_000, now - 1_000);

    let report = scheduler.run_once().await.expect("pass");
    assert_eq!(report.acquired, 2);
    assert_eq!(report.completed, 2);
    assert_eq!(report.failed, 0);

    for id in [embed, consolidate] {
        assert_eq!(
            payload_string(&fetch(&store, id), "state").as_deref(),
            Some("done")
        );
    }
    assert!(
        !EmbeddingStore::open(&path)
            .expect("open")
            .get_vectors(&claim.to_string())
            .expect("get vectors")
            .is_empty(),
        "the seeded claim must have been embedded"
    );
    assert_eq!(count_schema(&store, MemoryKind::Episode.schema_ref()), 1);
}
