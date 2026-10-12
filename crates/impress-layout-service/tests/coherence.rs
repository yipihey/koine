//! Several writers on one live row (review RL-L1, RL-L17).
//!
//! A `LayoutSession` caches the tree. The app, `impress-mcp`, `impress-cli`
//! and a second chassis app all hold one on the same row, each in its own
//! registry. These tests pin that none of them can write a stale tree over
//! another's change, that a writer whose tree moved says so and drops its
//! undo rings rather than replaying them onto someone else's tree, and that a
//! live row that no longer decodes is set aside rather than bricking the
//! scope.

use impress_pane_query_compile::test_support::builtin_manifest;
use std::sync::Arc;

use impress_item::item::{ActorKind, Value};
use impress_item::operation::{OperationIntent, OperationSpec, OperationType, RetentionTier};
use impress_item::store::{AttributedStore, ItemStore};
use impress_layout::{Layout, PaneRef, Role, Verb};
use impress_layout_service::session::STALE;
use impress_layout_service::{
    DefaultLayoutService, LayoutService, LayoutStore, PaneRefDto, SessionRegistry,
};
use impress_pane_query::PaneQuery;
use koine_store_dir::DirStore;

const APP: &str = "coherence-test";
const DEVICE: &str = "test-device";

fn device() -> Option<String> {
    Some(DEVICE.to_string())
}

fn kinds(kind: &str) -> PaneQuery {
    PaneQuery {
        kinds: vec![kind.to_string()],
        ..PaneQuery::default()
    }
}

/// Two handles on one root: the shape of two processes (the app and
/// `impress-mcp` as the store sees them). `DirStore` is `shared`: its
/// `change_token` is the clock file, which moves when the OTHER handle writes —
/// the same property `data_version` gave the SQLite connection pair.
fn two_handles() -> (tempfile::TempDir, Arc<DirStore>, Arc<DirStore>) {
    let dir = tempfile::tempdir().unwrap();
    let a = Arc::new(DirStore::open(dir.path()).unwrap());
    let b = Arc::new(DirStore::open(dir.path()).unwrap());
    (dir, a, b)
}

async fn tree(service: &DefaultLayoutService) -> Layout {
    let result = service.get_layout(APP.into(), device()).await;
    assert!(result.ok, "{}", result.message);
    result.layout.expect("a tree")
}

/// Two writers, each with its own registry, interleave verbs; a third reads
/// the stored row. Before revisions, the second writer's cached tree was
/// written back whole and the first writer's change vanished.
async fn interleave(
    a: &DefaultLayoutService,
    b: &DefaultLayoutService,
    reader: &DefaultLayoutService,
) {
    // Both warm: each holds a session from before any change below.
    tree(a).await;
    tree(b).await;
    let start = tree(reader).await.panes().len();

    let split = a
        .split(
            APP.into(),
            device(),
            PaneRefDto::role("detail"),
            "vertical".into(),
            true,
            None,
            Some("agent".into()),
            None,
        )
        .await;
    assert!(split.ok, "{}", split.message);

    // B's session predates A's split.
    let query = b
        .set_query(
            APP.into(),
            device(),
            PaneRefDto::role("list"),
            kinds("manuscript"),
            Some("human".into()),
            None,
        )
        .await;
    assert!(query.ok, "{}", query.message);
    assert!(
        query.message.contains("changed elsewhere"),
        "a writer whose tree moved says so: {}",
        query.message
    );

    // A's session predates B's set-query.
    let close = a
        .close(
            APP.into(),
            device(),
            PaneRefDto::role("navigator"),
            Some("agent".into()),
            None,
        )
        .await;
    assert!(close.ok, "{}", close.message);

    // And B's predates A's close.
    let split_again = b
        .split(
            APP.into(),
            device(),
            PaneRefDto::role("list"),
            "vertical".into(),
            true,
            None,
            Some("human".into()),
            None,
        )
        .await;
    assert!(split_again.ok, "{}", split_again.message);

    let stored = tree(reader).await;
    assert_eq!(
        stored.panes().len(),
        start + 1 - 1 + 1,
        "both splits and the close are all in the stored tree: {stored:#?}"
    );
    let list = stored.pane_with_role(&Role::LIST).expect("a list pane");
    assert_eq!(
        stored.pane(list).unwrap().query,
        kinds("manuscript"),
        "B's set-query survived A's later close"
    );
    assert!(
        stored.pane_with_role(&Role::NAVIGATOR).is_none(),
        "A's close survived B's later split"
    );
    // And every writer now agrees with the store.
    assert_eq!(tree(a).await, stored);
    assert_eq!(tree(b).await, stored);
}

#[tokio::test]
async fn two_services_on_two_connections_never_lose_each_others_change() {
    let (_dir, store_a, store_b) = two_handles();
    let reader = DefaultLayoutService::with_store(store_a.clone(), builtin_manifest());
    interleave(
        &DefaultLayoutService::with_store(store_a, builtin_manifest()),
        &DefaultLayoutService::with_store(store_b, builtin_manifest()),
        &reader,
    )
    .await;
}

/// The same with ONE connection and two registries — the in-process shape of
/// review RL-L10 (a window's registry and the inventory's), where
/// `data_version` never moves because both write through the same handle.
#[tokio::test]
async fn two_registries_on_one_connection_never_lose_each_others_change() {
    let store = Arc::new(koine_store_memory::MemoryStore::new());
    let reader = DefaultLayoutService::with_store(store.clone(), builtin_manifest());
    interleave(
        &DefaultLayoutService::with_store(store.clone(), builtin_manifest()),
        &DefaultLayoutService::with_store(store, builtin_manifest()),
        &reader,
    )
    .await;
}

/// The undo rings describe the tree they were recorded on. When the row moved
/// under a session, the rings go with the old tree — and the ⌘Z that finds
/// them gone says why instead of answering "nothing to undo".
#[tokio::test]
async fn a_writer_whose_row_moved_drops_its_rings_and_says_so() {
    let (_dir, store_a, store_b) = two_handles();
    let gui = DefaultLayoutService::with_store(store_a, builtin_manifest());
    let agent = DefaultLayoutService::with_store(store_b, builtin_manifest());

    let explored = gui
        .set_query(
            APP.into(),
            device(),
            PaneRefDto::role("list"),
            kinds("manuscript"),
            Some("human".into()),
            None,
        )
        .await;
    assert!(explored.ok, "{}", explored.message);
    let moved = agent
        .set_role(
            APP.into(),
            device(),
            PaneRefDto::role("navigator"),
            Some("console".into()),
            Some("agent".into()),
            None,
        )
        .await;
    assert!(moved.ok, "{}", moved.message);

    let undo = gui
        .undo(
            APP.into(),
            device(),
            "exploration".into(),
            Some(PaneRefDto::role("list")),
            Some("human".into()),
            None,
        )
        .await;
    assert!(!undo.ok, "nothing was undone: {undo:?}");
    assert!(
        undo.message.contains("changed elsewhere") && undo.message.contains("1 step"),
        "the refusal names the reload and the dropped step: {}",
        undo.message
    );
    // The query the dropped step would have undone is untouched, and the
    // agent's role change stands.
    let stored = tree(&gui).await;
    let list = stored.pane_with_role(&Role::LIST).unwrap();
    assert_eq!(stored.pane(list).unwrap().query, kinds("manuscript"));
    assert!(stored
        .pane_with_role(&Role::from("console".to_string()))
        .is_some());
}

/// The narrow race: another writer commits between a session's revision
/// check and its write. The write is compare-and-swap, so it is refused,
/// nothing is written, and the session reloads on its next touch.
#[test]
fn a_save_that_loses_the_race_writes_nothing() {
    let (_dir, store_a, store_b) = two_handles();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let registry = SessionRegistry::new();
    let layouts = LayoutStore::new(store_a);
    let other = DefaultLayoutService::with_store(store_b, builtin_manifest());

    registry
        .with(&layouts, APP, DEVICE, ActorKind::Human, |_| ())
        .unwrap();
    let outcome = registry
        .with(&layouts, APP, DEVICE, ActorKind::Human, |session| {
            // Someone else commits after the check `with` just made…
            let theirs = runtime.block_on(other.set_query(
                APP.into(),
                device(),
                PaneRefDto::role("list"),
                kinds("manuscript"),
                Some("agent".into()),
                None,
            ));
            assert!(theirs.ok, "{}", theirs.message);
            // …and this session writes a tree built on the old revision.
            session
                .apply(Verb::Close {
                    target: PaneRef::role(Role::NAVIGATOR),
                })
                .unwrap();
            let saved = session.save(&layouts, ActorKind::Human, "closed a pane");
            (saved, session.is_stale())
        })
        .unwrap();
    assert_eq!(
        outcome,
        (Err(impress_service_core::Refusal::conflict(STALE)), true)
    );

    let stored = runtime.block_on(tree(&other));
    assert!(
        stored.pane_with_role(&Role::NAVIGATOR).is_some(),
        "the stale close was not written"
    );
    let list = stored.pane_with_role(&Role::LIST).unwrap();
    assert_eq!(stored.pane(list).unwrap().query, kinds("manuscript"));

    // The next touch reloads: the session sees the other writer's query.
    let seen = registry
        .with(&layouts, APP, DEVICE, ActorKind::Human, |session| {
            let list = session.layout.pane_with_role(&Role::LIST).unwrap();
            (
                session.layout.pane(list).unwrap().query.clone(),
                session.take_notice().is_some(),
            )
        })
        .unwrap();
    assert_eq!(seen, (kinds("manuscript"), true));
}

/// Review RL-L17: a live row that no longer decodes used to fail every
/// snapshot, verb and read of its scope for good. It is set aside — never
/// deleted — and a fresh preset takes its place, loudly.
#[tokio::test]
async fn an_undecodable_live_row_is_quarantined_and_a_fresh_preset_loaded() {
    let store = Arc::new(koine_store_memory::MemoryStore::new());
    let first = DefaultLayoutService::with_store(store.clone(), builtin_manifest());
    let live = first.get_layout(APP.into(), device()).await;
    let broken_id: uuid::Uuid = live.item_id.unwrap().parse().unwrap();

    // A future build renamed a tile kind; this build cannot read the row.
    store
        .apply_operation(OperationSpec {
            target_id: broken_id,
            op_type: OperationType::SetPayload(
                "layout".into(),
                Value::Object(
                    [("windows".to_string(), Value::String("not a list".into()))]
                        .into_iter()
                        .collect(),
                ),
            ),
            intent: OperationIntent::Routine,
            reason: Some("simulate a row from a newer build".into()),
            batch_id: None,
            author: "test".into(),
            author_kind: ActorKind::System,
            retention: RetentionTier::Ephemeral,
        })
        .unwrap();

    // A fresh registry — the app at its next launch — and the one that was
    // holding the session both recover.
    for service in [
        DefaultLayoutService::with_store(store.clone(), builtin_manifest()),
        first,
    ] {
        let result = service.get_layout(APP.into(), device()).await;
        assert!(result.ok, "the scope still works: {}", result.message);
        assert_ne!(
            result.item_id.as_deref(),
            Some(broken_id.to_string().as_str())
        );
        assert_eq!(result.layout.unwrap().panes().len(), 3, "a fresh preset");
    }

    // The bad row is kept, named where the user will see it, with the reason.
    let kept = store.get(broken_id).unwrap().expect("never deleted");
    assert_eq!(kept.payload.get("is_live"), Some(&Value::Bool(false)));
    let Some(Value::String(name)) = kept.payload.get("name") else {
        panic!("the quarantined row is named: {:?}", kept.payload);
    };
    assert!(name.starts_with("Unreadable layout "), "{name}");
    assert!(matches!(
        kept.payload.get("quarantined_reason"),
        Some(Value::String(reason)) if !reason.is_empty()
    ));
    let listed = DefaultLayoutService::with_store(store.clone(), builtin_manifest())
        .list_layouts(APP.into())
        .await;
    assert!(
        listed
            .layouts
            .iter()
            .any(|row| row.name.as_deref() == Some(name.as_str())),
        "it lists among the saved layouts: {listed:?}"
    );
}

/// The notice reaches the first verb after the quarantine, not only a read.
#[tokio::test]
async fn the_first_verb_after_a_quarantine_reports_it() {
    let store = Arc::new(koine_store_memory::MemoryStore::new());
    let service = DefaultLayoutService::with_store(store.clone(), builtin_manifest());
    let live = service.get_layout(APP.into(), device()).await;
    let id: uuid::Uuid = live.item_id.unwrap().parse().unwrap();
    store
        .apply_operation(OperationSpec {
            target_id: id,
            op_type: OperationType::SetPayload("layout".into(), Value::String("garbage".into())),
            intent: OperationIntent::Routine,
            reason: None,
            batch_id: None,
            author: "test".into(),
            author_kind: ActorKind::System,
            retention: RetentionTier::Ephemeral,
        })
        .unwrap();
    let focus = service
        .focus(
            APP.into(),
            device(),
            PaneRefDto::role("list"),
            Some("human".into()),
            None,
        )
        .await;
    assert!(focus.ok, "{}", focus.message);
    assert!(
        focus.message.contains("could not be read"),
        "{}",
        focus.message
    );
}
