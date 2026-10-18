//! Wave 7 T2: one truth for a surface, and it is the store.
//!
//! Each test here pins one review finding from
//! `docs/review-2026-09-25-gui-layer.md` and fails on the code before it:
//!
//! * RS-S1 = SK-K1 = AC-F1 — a runtime cached in one registry sees an update
//!   or a state write made through another registry (another handle, another
//!   process), and a dispatch builds on it rather than overwriting it.
//! * RS-S2 = AC-F16 — a store write to a kind a `query` source reads re-runs
//!   that source, and nothing else.
//! * AC-F2 + RS-S23 — `seq` is unique across interleaved writers on two
//!   connections, and a reader's cursor never skips an event.
//! * RS-S14 — a dispatch that changes nothing writes nothing.
//! * RS-S15 — a failing source is not asked again on every render.
//! * RS-S25 — a remembered pane is re-checked against the layout.

use impress_service_core::Refusal;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use impress_item::item::{ActorKind, Item, ItemId, Priority, Value as ItemValue, Visibility};
use impress_item::operation::{OperationIntent, OperationSpec, OperationType, RetentionTier};
use impress_item::store::{AttributedStore, ItemStore};
use impress_layout_service::{DefaultLayoutService, LayoutService};
use impress_pane_query::Bindings;
use impress_surface::{Event, EventKind, PaneQuery, SurfaceSpec};
use impress_surface_service::dto::{ShowTargetDto, SplitTargetDto};
use impress_surface_service::runtime::FAILED_SOURCE_BACKOFF;
use impress_surface_service::{
    DefaultExecutor, DefaultImpressSurfaceService, Executor, ImpressSurfaceService, PaneHandle,
    SessionRegistry, SurfaceStore,
};
use koine_store_dir::DirStore;
use koine_store_memory::MemoryStore;
use serde_json::{json, Value};

const HOST: &str = "coherence-host";
const PUBLICATION_REF: &str = "imbib/bibliography-entry";

fn spec(value: Value) -> SurfaceSpec {
    serde_json::from_value(value).expect("fixture spec parses")
}

fn text_spec(greeting: &str) -> SurfaceSpec {
    spec(json!({
        "surface": "1.0",
        "name": "Coherence fixture",
        "state": { "bins": 1 },
        "root": { "column": [
            { "text": greeting, "id": "greeting" },
            { "field": { "number": {} }, "label": "Bins", "bind": "state.bins", "id": "bins" },
            { "button": { "label": "Ping", "on_click": [
                { "emit": { "name": "ping", "payload": {} } }
            ] }, "id": "ping" }
        ] }
    }))
}

fn tree_json(tree: &impress_surface::RenderTree) -> String {
    serde_json::to_string(tree).unwrap()
}

async fn render(
    registry: &SessionRegistry,
    surfaces: &SurfaceStore,
    executor: &DefaultExecutor,
    id: ItemId,
) -> String {
    let executor = executor.clone();
    let tree = registry
        .with(surfaces, id, HOST, move |rt| {
            Box::pin(async move { Ok(rt.render(&executor).await) })
        })
        .await
        .expect("render");
    tree_json(&tree)
}

async fn dispatch(
    registry: &SessionRegistry,
    surfaces: &SurfaceStore,
    executor: &DefaultExecutor,
    id: ItemId,
    event: Event,
) -> Vec<impress_surface_service::dto::EffectOutcomeDto> {
    let executor = executor.clone();
    let writer = surfaces.clone();
    registry
        .with(surfaces, id, HOST, move |rt| {
            Box::pin(async move {
                rt.dispatch(&executor, &writer, &event, ActorKind::Human)
                    .await
            })
        })
        .await
        .expect("dispatch")
        .1
}

fn change(widget: &str, value: Value) -> Event {
    Event {
        widget: widget.to_string(),
        kind: EventKind::Change,
        value,
    }
}

// ─── RS-S1 = SK-K1 = AC-F1 ────────────────────────────────────────────────

/// Two processes on one file: two store handles (two connections), each with
/// its own registry — the app's pane and `impress-mcp`. An update and a state
/// write through the second are what the first renders next, and the first's
/// next dispatch builds on the second's state instead of writing its stale
/// copy back over it.
#[tokio::test]
async fn an_update_through_another_process_is_what_the_next_render_shows() {
    let dir = tempfile::tempdir().unwrap();
    let app_store = Arc::new(DirStore::open(dir.path()).unwrap());
    let mcp_store = Arc::new(DirStore::open(dir.path()).unwrap());

    let app_surfaces = SurfaceStore::new(app_store.clone());
    let app_registry = SessionRegistry::with_manifest(
        impress_pane_query_compile::test_support::builtin_manifest(),
    );
    let app_executor = DefaultExecutor::with_store(
        app_store.clone(),
        impress_pane_query_compile::test_support::builtin_manifest(),
    );
    let mcp_surfaces = SurfaceStore::new(mcp_store.clone());

    let row = app_surfaces
        .create(&text_spec("before"), None, &[], ActorKind::Agent)
        .unwrap();
    let first = render(&app_registry, &app_surfaces, &app_executor, row.id).await;
    assert!(first.contains("before"), "{first}");

    // The agent updates the spec from its own process.
    mcp_surfaces
        .update(row.id, &text_spec("after"), None, Some(1), ActorKind::Agent)
        .unwrap();
    let second = render(&app_registry, &app_surfaces, &app_executor, row.id).await;
    assert!(
        second.contains("after") && !second.contains("before"),
        "the cached runtime kept serving the old spec: {second}"
    );

    // The agent sets state; the human then edits another field in the pane.
    mcp_surfaces
        .set_state(
            row.id,
            HOST,
            &json!({ "bins": 7, "agent": "was here" }),
            ActorKind::Agent,
        )
        .unwrap();
    let third = render(&app_registry, &app_surfaces, &app_executor, row.id).await;
    assert!(
        third.contains('7'),
        "the agent's state is not rendered: {third}"
    );
    dispatch(
        &app_registry,
        &app_surfaces,
        &app_executor,
        row.id,
        change("bins", json!(9)),
    )
    .await;
    let stored = mcp_surfaces.get_state(row.id, HOST).unwrap().unwrap();
    assert_eq!(
        stored,
        json!({ "bins": 9, "agent": "was here" }),
        "the human's dispatch wrote a stale state over the agent's"
    );
}

/// The in-process half: two handles in one app (the pane and the HTTP
/// bridge) share ONE registry through `SharedStore`, but even two registries
/// on one store agree, because each re-reads the row.
#[tokio::test]
async fn two_registries_on_one_store_render_one_spec() {
    let store = Arc::new(MemoryStore::new());
    let surfaces = SurfaceStore::new(store.clone());
    let executor = DefaultExecutor::with_store(
        store.clone(),
        impress_pane_query_compile::test_support::builtin_manifest(),
    );
    let pane = SessionRegistry::with_manifest(
        impress_pane_query_compile::test_support::builtin_manifest(),
    );
    let http = DefaultImpressSurfaceService::with_store(
        store.clone(),
        impress_pane_query_compile::test_support::builtin_manifest(),
    );

    let row = surfaces
        .create(&text_spec("one"), None, &[], ActorKind::Agent)
        .unwrap();
    assert!(render(&pane, &surfaces, &executor, row.id)
        .await
        .contains("one"));
    let updated = http
        .surface_update(row.id.to_string(), text_spec("two").into(), None, None)
        .await;
    assert!(updated.ok, "{}", updated.message);
    assert!(render(&pane, &surfaces, &executor, row.id)
        .await
        .contains("two"));

    // A deleted surface is an error, and the entry is dropped.
    assert!(http.surface_delete(row.id.to_string()).await.ok);
    let gone = pane
        .with(&surfaces, row.id, HOST, |_| Box::pin(async { Ok(()) }))
        .await;
    let gone = gone.unwrap_err();
    assert!(gone.message.starts_with("no surface"));
    assert_eq!(gone.code, "not-found");
    assert!(pane.is_empty());
}

/// Two concurrent calls on one `(surface, host)` run one after the other:
/// each dispatch sees the other's result, so none is lost.
#[tokio::test]
async fn concurrent_dispatches_on_one_instance_do_not_overwrite_each_other() {
    let store = Arc::new(MemoryStore::new());
    let surfaces = SurfaceStore::new(store.clone());
    let executor = DefaultExecutor::with_store(
        store.clone(),
        impress_pane_query_compile::test_support::builtin_manifest(),
    );
    let registry = Arc::new(SessionRegistry::with_manifest(
        impress_pane_query_compile::test_support::builtin_manifest(),
    ));
    let row = surfaces
        .create(&text_spec("x"), None, &[], ActorKind::Agent)
        .unwrap();

    let mut handles = Vec::new();
    for _ in 0..8 {
        let (registry, surfaces, executor) = (registry.clone(), surfaces.clone(), executor.clone());
        let id = row.id;
        handles.push(tokio::spawn(async move {
            let click = Event {
                widget: "ping".into(),
                kind: EventKind::Click,
                value: Value::Null,
            };
            dispatch(&registry, &surfaces, &executor, id, click).await
        }));
    }
    for handle in handles {
        let outcomes = handle.await.unwrap();
        assert!(outcomes.iter().all(|o| o.ok), "{outcomes:?}");
    }
    let events = surfaces.events_after(row.id, HOST, 0, 0).unwrap();
    let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, (1..=8).collect::<Vec<_>>());
}

// ─── RS-S2 = AC-F16 ───────────────────────────────────────────────────────

/// Counts every fetch, and answers a query with the real store.
struct Counting {
    inner: DefaultExecutor,
    queries: AtomicUsize,
    verbs: AtomicUsize,
    fail_verbs: bool,
}

#[async_trait::async_trait]
impl Executor for Counting {
    async fn call_verb(&self, _name: &str, _args: Value) -> Result<Value, Refusal> {
        self.verbs.fetch_add(1, Ordering::SeqCst);
        if self.fail_verbs {
            Err("the app that owns this verb is not running".into())
        } else {
            Ok(json!({ "answer": 42 }))
        }
    }
    async fn run_query(
        &self,
        query: &PaneQuery,
        decls: &[impress_surface::ParamDecl],
        bindings: &Bindings,
    ) -> Result<Value, Refusal> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        self.inner.run_query(query, decls, bindings).await
    }
    async fn apply_layout(
        &self,
        _: &PaneHandle,
        _: Vec<impress_layout::Verb>,
        _: ActorKind,
    ) -> Result<(), Refusal> {
        Ok(())
    }
    async fn emit(
        &self,
        _: ItemId,
        _: &str,
        _: &str,
        _: Value,
        _: ActorKind,
    ) -> Result<u64, Refusal> {
        Ok(0)
    }
}

fn papers_spec() -> SurfaceSpec {
    spec(json!({
        "surface": "1.0",
        "name": "Papers",
        "state": {},
        "sources": {
            "papers": { "query": { "kinds": ["publication"] } },
            "answer": { "verb": "anything-service_answer", "args": {} }
        },
        "root": { "column": [
            { "table": { "columns": ["title"], "rows": "{{source.papers}}" }, "id": "papers" },
            { "text": "{{source.answer.answer}}", "id": "answer" }
        ] }
    }))
}

#[tokio::test]
async fn provider_inventory_changes_refresh_catalogue_without_repeating_other_sources() {
    let store = Arc::new(MemoryStore::new());
    let surfaces = SurfaceStore::new(store.clone());
    let registry = SessionRegistry::with_manifest(
        impress_pane_query_compile::test_support::builtin_manifest(),
    );
    let executor = Arc::new(Counting {
        inner: DefaultExecutor::with_store(
            store,
            impress_pane_query_compile::test_support::builtin_manifest(),
        ),
        queries: AtomicUsize::new(0),
        verbs: AtomicUsize::new(0),
        fail_verbs: false,
    });
    let row = surfaces.create(&spec(json!({
        "surface":"1.0", "name":"Provider catalogue", "state":{},
        "sources":{
            "catalogue":{"verb":"capabilities-service_list-verbs","args":{}},
            "other":{"verb":"anything-service_answer","args":{}}
        },
        "root":{"column":[{"text":"{{source.catalogue.answer}}"},{"text":"{{source.other.answer}}"}]}
    })), None, &[], ActorKind::Agent).unwrap();
    for iteration in 0..3 {
        if iteration == 2 {
            assert_eq!(registry.invalidate_provider_inventory(), vec![row.id]);
        }
        let executor_for_render = executor.clone();
        registry
            .with(&surfaces, row.id, HOST, move |runtime| {
                Box::pin(async move { Ok(runtime.render(executor_for_render.as_ref()).await) })
            })
            .await
            .unwrap();
        assert_eq!(
            executor.verbs.load(Ordering::SeqCst),
            if iteration == 2 { 3 } else { 2 }
        );
    }
}

fn insert_paper(store: &dyn ItemStore, title: &str) -> ItemId {
    let mut payload = BTreeMap::new();
    payload.insert("title".to_string(), ItemValue::String(title.to_string()));
    let now = chrono::Utc::now();
    store
        .insert(Item {
            id: uuid::Uuid::new_v4(),
            schema: impress_item::schema::refs::IMBIB_BIBLIOGRAPHY_ENTRY,
            payload,
            created: now,
            modified: now,
            author: "test".into(),
            author_kind: ActorKind::Human,
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
            extra: Default::default(),
        })
        .unwrap()
}

async fn render_with(
    registry: &SessionRegistry,
    surfaces: &SurfaceStore,
    executor: Arc<Counting>,
    id: ItemId,
) -> String {
    let tree = registry
        .with(surfaces, id, HOST, move |rt| {
            Box::pin(async move { Ok(rt.render(executor.as_ref()).await) })
        })
        .await
        .unwrap();
    tree_json(&tree)
}

/// A paper written (and then starred) through the store re-runs the query
/// that reads papers, and only it: the verb source, and a write to a kind no
/// source reads, re-run nothing.
#[tokio::test]
async fn a_store_write_to_a_queried_kind_reruns_that_source_and_nothing_else() {
    let store = Arc::new(MemoryStore::new());
    let surfaces = SurfaceStore::new(store.clone());
    let registry = SessionRegistry::with_manifest(
        impress_pane_query_compile::test_support::builtin_manifest(),
    );
    let executor = Arc::new(Counting {
        inner: DefaultExecutor::with_store(
            store.clone(),
            impress_pane_query_compile::test_support::builtin_manifest(),
        ),
        queries: AtomicUsize::new(0),
        verbs: AtomicUsize::new(0),
        fail_verbs: false,
    });
    let first_paper = insert_paper(store.as_ref(), "A dark matter survey");
    let row = surfaces
        .create(&papers_spec(), None, &[], ActorKind::Agent)
        .unwrap();

    let before = render_with(&registry, &surfaces, executor.clone(), row.id).await;
    assert!(before.contains("A dark matter survey"), "{before}");
    assert_eq!(executor.queries.load(Ordering::SeqCst), 1);
    assert_eq!(executor.verbs.load(Ordering::SeqCst), 1);
    assert_eq!(
        registry.watched_refs(),
        BTreeSet::from([PUBLICATION_REF.to_string()])
    );

    // Nothing named: nothing re-runs.
    render_with(&registry, &surfaces, executor.clone(), row.id).await;
    assert_eq!(executor.queries.load(Ordering::SeqCst), 1);

    // A write to a kind no source reads marks nothing.
    let unrelated = BTreeSet::from(["manuscript".to_string()]);
    assert!(registry.invalidate_refs(&unrelated).is_empty());
    render_with(&registry, &surfaces, executor.clone(), row.id).await;
    assert_eq!(executor.queries.load(Ordering::SeqCst), 1);

    // A new paper: the query re-runs, the verb does not.
    insert_paper(store.as_ref(), "A second paper");
    let named = BTreeSet::from([PUBLICATION_REF.to_string()]);
    assert_eq!(registry.invalidate_refs(&named), vec![row.id]);
    let after = render_with(&registry, &surfaces, executor.clone(), row.id).await;
    assert!(after.contains("A second paper"), "{after}");
    assert_eq!(executor.queries.load(Ordering::SeqCst), 2);
    assert_eq!(
        executor.verbs.load(Ordering::SeqCst),
        1,
        "the verb source re-ran"
    );

    // Starring a paper (an operation on it) is a write to its kind too.
    store
        .apply_operation(OperationSpec {
            target_id: first_paper,
            op_type: OperationType::SetStarred(true),
            intent: OperationIntent::Routine,
            reason: None,
            batch_id: None,
            author: "test".into(),
            author_kind: ActorKind::Human,
            retention: RetentionTier::Durable,
        })
        .unwrap();
    assert_eq!(registry.invalidate_refs(&named), vec![row.id]);
    let starred = render_with(&registry, &surfaces, executor.clone(), row.id).await;
    assert!(starred.contains("\"is_starred\":true"), "{starred}");
    assert_eq!(executor.queries.load(Ordering::SeqCst), 3);
    assert_eq!(executor.verbs.load(Ordering::SeqCst), 1);
}

// ─── RS-S15 ───────────────────────────────────────────────────────────────

/// A verb that fails is asked once, not once per round and once per render,
/// until its backoff passes or an action refreshes it; its error stays on
/// the tree meanwhile.
#[tokio::test]
async fn a_failing_source_is_not_asked_again_on_every_render() {
    let store = Arc::new(MemoryStore::new());
    let surfaces = SurfaceStore::new(store.clone());
    let registry = SessionRegistry::with_manifest(
        impress_pane_query_compile::test_support::builtin_manifest(),
    );
    let executor = Arc::new(Counting {
        inner: DefaultExecutor::with_store(
            store.clone(),
            impress_pane_query_compile::test_support::builtin_manifest(),
        ),
        queries: AtomicUsize::new(0),
        verbs: AtomicUsize::new(0),
        fail_verbs: true,
    });
    let row = surfaces
        .create(&papers_spec(), None, &[], ActorKind::Agent)
        .unwrap();
    for _ in 0..3 {
        let tree = render_with(&registry, &surfaces, executor.clone(), row.id).await;
        assert!(
            tree.contains("not running"),
            "the error left the tree: {tree}"
        );
    }
    assert_eq!(executor.verbs.load(Ordering::SeqCst), 1);
    assert!(FAILED_SOURCE_BACKOFF.as_secs() >= 1);

    // A `refresh` action is an explicit ask: the next render calls it again.
    let refreshing = spec(json!({
        "surface": "1.0", "name": "Refreshing", "state": {},
        "sources": { "answer": { "verb": "anything-service_answer", "args": {} } },
        "root": { "column": [
            { "text": "{{source.answer.answer}}", "id": "answer" },
            { "button": { "label": "Retry", "on_click": [ { "refresh": { "source": "answer" } } ] },
              "id": "retry" }
        ] }
    }));
    let row = surfaces
        .create(&refreshing, None, &[], ActorKind::Agent)
        .unwrap();
    render_with(&registry, &surfaces, executor.clone(), row.id).await;
    assert_eq!(executor.verbs.load(Ordering::SeqCst), 2);
    let writer = surfaces.clone();
    let counting = executor.clone();
    registry
        .with(&surfaces, row.id, HOST, move |rt| {
            Box::pin(async move {
                let click = Event {
                    widget: "retry".into(),
                    kind: EventKind::Click,
                    value: Value::Null,
                };
                rt.dispatch(counting.as_ref(), &writer, &click, ActorKind::Human)
                    .await
                    .map(|_| ())
            })
        })
        .await
        .unwrap();
    assert_eq!(executor.verbs.load(Ordering::SeqCst), 3);
}

// ─── AC-F2 + RS-S23 ───────────────────────────────────────────────────────

/// Two connections to one file (the app and `impress-mcp`), each appending
/// from its own threads at once: every event gets its own `seq`, and the
/// sequence has no holes, so a cursor at N misses nothing.
#[test]
fn interleaved_writers_on_two_connections_never_share_a_seq() {
    let dir = tempfile::tempdir().unwrap();
    let a = SurfaceStore::new(Arc::new(DirStore::open(dir.path()).unwrap()));
    let b = SurfaceStore::new(Arc::new(DirStore::open(dir.path()).unwrap()));
    let surface = a
        .create(&text_spec("seq"), None, &[], ActorKind::Agent)
        .unwrap()
        .id;

    const PER_WRITER: usize = 25;
    let mut threads = Vec::new();
    for (store, who) in [
        (a.clone(), "a"),
        (b.clone(), "b"),
        (a.clone(), "a2"),
        (b.clone(), "b2"),
    ] {
        threads.push(std::thread::spawn(move || {
            for i in 0..PER_WRITER {
                store
                    .append_event(surface, HOST, who, &json!({ "i": i }), ActorKind::Agent)
                    .expect("append");
            }
        }));
    }
    for t in threads {
        t.join().unwrap();
    }
    let rows = b.events_after(surface, HOST, 0, 0).unwrap();
    let seqs: Vec<u64> = rows.iter().map(|r| r.seq).collect();
    assert_eq!(
        seqs,
        (1..=(4 * PER_WRITER) as u64).collect::<Vec<_>>(),
        "duplicate or missing seq"
    );
}

// ─── AC-F22, across processes ─────────────────────────────────────────────

/// Two store handles on one file — two processes, as far as SQLite is
/// concerned; nothing in this crate serialises them any more — race
/// `surface_update` with the revision they both read. Every round has exactly
/// one winner and one `conflict`, and the revision moves by one per round:
/// the check and the write are one conditional transaction, not a read then
/// a write with a window between them.
#[test]
fn two_processes_updating_from_one_revision_cannot_both_win() {
    let dir = tempfile::tempdir().unwrap();
    let a = SurfaceStore::new(Arc::new(DirStore::open(dir.path()).unwrap()));
    let b = SurfaceStore::new(Arc::new(DirStore::open(dir.path()).unwrap()));
    let surface = a
        .create(&text_spec("round 0"), None, &[], ActorKind::Agent)
        .unwrap()
        .id;

    const ROUNDS: u64 = 30;
    let barrier = Arc::new(std::sync::Barrier::new(2));
    // Each thread records (revision read, revision written or the refusal
    // code) and asserts nothing itself: a panic inside would strand the
    // other thread on the barrier.
    let run = |store: SurfaceStore, who: &'static str| {
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            let mut rounds = Vec::new();
            for round in 0..ROUNDS {
                let read = store.get(surface).unwrap().unwrap().revision;
                barrier.wait();
                let outcome = store
                    .update(
                        surface,
                        &text_spec(&format!("{who} {round}")),
                        None,
                        Some(read),
                        ActorKind::Agent,
                    )
                    .map(|row| row.revision)
                    .map_err(|refusal| refusal.code);
                rounds.push((read, outcome));
                // Both have written or been refused before the next read.
                barrier.wait();
            }
            rounds
        })
    };
    let (ta, tb) = (run(a.clone(), "a"), run(b.clone(), "b"));
    let (ra, rb) = (ta.join().unwrap(), tb.join().unwrap());
    for (round, ((read_a, a), (read_b, b))) in ra.iter().zip(rb.iter()).enumerate() {
        let expected = round as u64 + 1;
        assert_eq!((*read_a, *read_b), (expected, expected), "round {round}");
        let won = [a, b].iter().filter(|o| o.is_ok()).count();
        assert_eq!(
            won, 1,
            "round {round}: exactly one writer wins: {a:?} {b:?}"
        );
        for outcome in [a, b] {
            match outcome {
                Ok(revision) => assert_eq!(*revision, expected + 1, "round {round}"),
                Err(code) => assert_eq!(code, "conflict", "round {round}"),
            }
        }
    }
    let row = b.get(surface).unwrap().unwrap();
    assert_eq!(row.revision, ROUNDS + 1);
}

/// With no `expected_revision` an update is last writer wins — but it never
/// loses a revision number: two handles on one file bumping at once end at
/// exactly one plus the number of updates.
#[test]
fn concurrent_updates_on_two_connections_never_share_a_revision() {
    let dir = tempfile::tempdir().unwrap();
    let a = SurfaceStore::new(Arc::new(DirStore::open(dir.path()).unwrap()));
    let b = SurfaceStore::new(Arc::new(DirStore::open(dir.path()).unwrap()));
    let surface = a
        .create(&text_spec("bump"), None, &[], ActorKind::Agent)
        .unwrap()
        .id;

    const PER_WRITER: u64 = 20;
    let mut threads = Vec::new();
    for (store, who) in [(a.clone(), "a"), (b.clone(), "b"), (a.clone(), "a2")] {
        threads.push(std::thread::spawn(move || {
            let mut seen = Vec::new();
            for i in 0..PER_WRITER {
                let row = store
                    .update(
                        surface,
                        &text_spec(&format!("{who} {i}")),
                        None,
                        None,
                        ActorKind::Agent,
                    )
                    .expect("an unguarded update always lands");
                seen.push(row.revision);
            }
            seen
        }));
    }
    let mut revisions: Vec<u64> = threads
        .into_iter()
        .flat_map(|t| t.join().unwrap())
        .collect();
    revisions.sort_unstable();
    assert_eq!(
        revisions,
        (2..=(3 * PER_WRITER + 1)).collect::<Vec<_>>(),
        "two updates took one revision"
    );
}

/// The cursor is the last row read. An event appended right after a read is
/// the whole of the next read — before, `next_seq` came from a second query,
/// and an event landing between the two was counted but never returned.
#[tokio::test]
async fn the_cursor_is_the_last_event_returned() {
    let store = Arc::new(MemoryStore::new());
    let service = DefaultImpressSurfaceService::with_store(
        store.clone(),
        impress_pane_query_compile::test_support::builtin_manifest(),
    );
    let surfaces = SurfaceStore::new(store.clone());
    let id = surfaces
        .create(&text_spec("cursor"), None, &[], ActorKind::Agent)
        .unwrap()
        .id;
    for i in 0..3 {
        surfaces
            .append_event(id, HOST, "e", &json!({ "i": i }), ActorKind::Agent)
            .unwrap();
    }
    let first = service
        .surface_events(id.to_string(), Some(0), Some(HOST.into()))
        .await;
    assert_eq!(first.next_seq, 3);
    surfaces
        .append_event(id, HOST, "late", &json!({}), ActorKind::Agent)
        .unwrap();
    let second = service
        .surface_wait(id.to_string(), Some(first.next_seq), 10, Some(HOST.into()))
        .await;
    assert_eq!(second.events.len(), 1);
    assert_eq!(second.events[0].name, "late");
    assert_eq!(second.next_seq, 4);
    assert!(!second.gap);

    // Nothing new: the cursor stands.
    let idle = service
        .surface_wait(id.to_string(), Some(4), 1, Some(HOST.into()))
        .await;
    assert!(idle.timed_out);
    assert_eq!(idle.next_seq, 4);
}

// ─── RS-S14 ───────────────────────────────────────────────────────────────

/// A click whose only action is an `emit` leaves the state as it was, so no
/// state row is written; a change writes exactly one.
#[tokio::test]
async fn a_dispatch_that_changes_no_state_writes_none() {
    let store = Arc::new(MemoryStore::new());
    let surfaces = SurfaceStore::new(store.clone());
    let executor = DefaultExecutor::with_store(
        store.clone(),
        impress_pane_query_compile::test_support::builtin_manifest(),
    );
    let registry = SessionRegistry::with_manifest(
        impress_pane_query_compile::test_support::builtin_manifest(),
    );
    let id = surfaces
        .create(&text_spec("quiet"), None, &[], ActorKind::Agent)
        .unwrap()
        .id;
    let click = Event {
        widget: "ping".into(),
        kind: EventKind::Click,
        value: Value::Null,
    };
    dispatch(&registry, &surfaces, &executor, id, click).await;
    assert_eq!(surfaces.get_state(id, HOST).unwrap(), None);

    let rx = store
        .subscribe(impress_item::query::ItemQuery {
            schema: Some(impress_item::schema::refs::IMPRESS_UI_SURFACE_STATE),
            ..Default::default()
        })
        .unwrap();
    dispatch(
        &registry,
        &surfaces,
        &executor,
        id,
        change("bins", json!(3)),
    )
    .await;
    let state_writes = rx.try_iter().count();
    assert_eq!(state_writes, 1, "one change, one state write");
}

// ─── RS-S25 ───────────────────────────────────────────────────────────────

/// A pane remembered from `surface_show` that has since been given another
/// query is not where a `publish` lands.
#[tokio::test]
async fn a_publish_does_not_land_on_a_pane_that_no_longer_shows_the_surface() {
    let store = Arc::new(MemoryStore::new());
    let service = DefaultImpressSurfaceService::with_store(
        store.clone(),
        impress_pane_query_compile::test_support::builtin_manifest(),
    );
    let layout = DefaultLayoutService::with_store(
        store.clone(),
        impress_pane_query_compile::test_support::builtin_manifest(),
    );
    let app = "coherence-app";
    const PICKED: &str = "2b995442-c45a-4922-8c22-600d98900fc1";
    let table = spec(json!({
        "surface": "1.0", "name": "Picker", "state": {},
        "sources": { "rows": { "value": [ { "id": PICKED, "title": "A" } ] } },
        "root": { "table": { "columns": ["title"], "rows": "{{source.rows}}",
                             "on_select": [ { "publish": {} } ] }, "id": "picker" }
    }));
    let id = service
        .surface_create(table.into(), None, None)
        .await
        .id
        .unwrap();
    let shown = service
        .surface_show(
            id.clone(),
            ShowTargetDto {
                split: Some(SplitTargetDto {
                    direction: "vertical".into(),
                    from_focused: true,
                }),
                ..ShowTargetDto::default()
            },
            app.into(),
            Some(HOST.into()),
        )
        .await;
    assert!(shown.ok, "{}", shown.message);
    let select = Event {
        widget: "picker".into(),
        kind: EventKind::Select,
        value: json!([PICKED]),
    };
    let while_shown = service
        .surface_dispatch(id.clone(), select.clone(), Some(HOST.into()), None)
        .await;
    assert!(while_shown.effects[0].ok, "{:?}", while_shown.effects);

    // The pane is given something else to show.
    let tile = shown.tile.unwrap();
    let other: PaneQuery = serde_json::from_value(json!({ "kinds": ["publication"] })).unwrap();
    let moved = layout
        .set_query(
            app.into(),
            Some(HOST.into()),
            impress_layout_service::dto::PaneRefDto::tile(impress_layout::TileId::new(tile)),
            other,
            None,
            None,
        )
        .await;
    assert!(moved.ok, "{}", moved.message);
    let after = service
        .surface_dispatch(id, select, Some(HOST.into()), None)
        .await;
    assert!(
        !after.effects[0].ok,
        "the publish landed on tile {tile}, which no longer shows the surface: {:?}",
        after.effects
    );
}
