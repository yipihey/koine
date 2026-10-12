//! Review PH-M2, the surface half: one click on a surface is one layout step.
//!
//! A button whose `on_click` is `[publish, open]` used to be two layout verbs
//! and two undo entries, so ⌘Z took back half a click. The runtime now
//! gathers a dispatch's consecutive `publish`/`open` effects and applies them
//! with `apply_verbs_as`: one revision of the layout row, one undo entry, all
//! or none. A `call` or `emit` between them still runs between them.

use std::sync::Arc;

use impress_item::event::ItemEvent;
use impress_item::query::ItemQuery;
use impress_item::schema::refs::IMPRESS_UI_LAYOUT as LAYOUT_SCHEMA_REF;
use impress_item::store::ItemStore;
use impress_layout::{Layout, Role, TileId, ViewKindId};
use impress_layout_service::dto::PaneRefDto;
use impress_layout_service::{DefaultLayoutService, LayoutService, SessionRegistry};
use impress_surface::{Event, EventKind};
use impress_surface_service::dto::{EffectOutcomeDto, ShowTargetDto, SpecArg, SplitTargetDto};
use impress_surface_service::{
    DefaultExecutor, DefaultImpressSurfaceService, ImpressSurfaceService,
};
use koine_store_memory::MemoryStore;
use serde_json::{json, Value};

const APP: &str = "gesture-app";
const HOST: &str = "gesture-host";
const PICKED: &str = "2b995442-c45a-4922-8c22-600d98900fc1";

/// A service whose executor, and so whose layout sessions and undo rings,
/// are the ones `layout` reads and undoes on — as the app's are.
fn world() -> (World, DefaultLayoutService) {
    let store = Arc::new(MemoryStore::new());
    let executor = DefaultExecutor::with_store_and_sessions(
        store.clone(),
        Arc::new(SessionRegistry::new()),
        impress_pane_query_compile::test_support::builtin_manifest(),
    );
    let layout = executor.layout().clone();
    let service = DefaultImpressSurfaceService::with_store(
        store.clone(),
        impress_pane_query_compile::test_support::builtin_manifest(),
    )
    .with_executor(executor);
    (World { service, store }, layout)
}

struct World {
    service: DefaultImpressSurfaceService,
    store: Arc<MemoryStore>,
}

/// A surface with one button whose click runs `on_click`, its state holding
/// a publication selection to publish.
fn button(on_click: Value, picked: &str) -> Value {
    json!({ "surface": "1.0", "name": "Gesture", "state": {
            "picked": { "kind": "publication", "ids": [picked] } },
        "root": { "button": { "label": "Go", "on_click": on_click }, "id": "go" } })
}

fn publish() -> Value {
    json!({ "publish": { "ids": "state.picked" } })
}

fn open_in(role: &str) -> Value {
    json!({ "open": { "query": { "kinds": ["publication"] }, "view_kind": "notes",
                      "target": role } })
}

fn click() -> Event {
    Event {
        widget: "go".into(),
        kind: EventKind::Click,
        value: Value::Null,
    }
}

/// Create the surface, show it in a new split, and return its id and tile.
async fn shown(service: &DefaultImpressSurfaceService, spec: Value) -> (String, u64) {
    let created = service.surface_create(SpecArg(spec), None, None).await;
    assert!(created.ok, "{}", created.message);
    let id = created.id.unwrap();
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
            APP.into(),
            Some(HOST.into()),
        )
        .await;
    assert!(shown.ok, "{}", shown.message);
    (id, shown.tile.unwrap())
}

async fn tree(layout: &DefaultLayoutService) -> (Layout, u64) {
    let result = layout.get_layout(APP.into(), Some(HOST.into())).await;
    assert!(result.ok, "{}", result.message);
    (result.layout.unwrap(), result.revision.unwrap())
}

fn detail(layout: &Layout) -> TileId {
    layout
        .panes()
        .into_iter()
        .find(|tile| layout.pane(*tile).and_then(|p| p.role.as_ref()) == Some(&Role::DETAIL))
        .expect("the cold-start layout has a detail pane")
}

/// The publications selected on any channel (the fixture's windows have one
/// in use), with the surface pane asserted to still exist.
fn picked_on(layout: &Layout, tile: u64) -> Vec<String> {
    assert!(layout.pane(TileId::new(tile)).is_some(), "the surface pane");
    layout
        .channels
        .channels
        .values()
        .filter_map(|kinds| kinds.get("publication"))
        .flatten()
        .map(|id| id.to_string())
        .collect()
}

/// ⌘Z with focus in `tile`: a human undo on that pane's exploration ring.
async fn undo_in(
    layout: &DefaultLayoutService,
    tile: u64,
) -> impress_layout_service::dto::LayoutVerbResult {
    layout
        .undo(
            APP.into(),
            Some(HOST.into()),
            "exploration".into(),
            Some(PaneRefDto::tile(TileId::new(tile))),
            Some("human".into()),
            None,
        )
        .await
}

fn focused(layout: &Layout) -> Option<TileId> {
    layout
        .window(layout.current_window().unwrap())
        .and_then(|w| w.focused)
}

/// Click the button; return what each effect reported and how many times
/// the live layout row was written (a layout step is one write).
async fn dispatch(world: &World, id: &str) -> (Vec<EffectOutcomeDto>, usize) {
    // An operation applied to a row of the layout schema is one write of it.
    let writes = world
        .store
        .subscribe(ItemQuery {
            schema: Some(LAYOUT_SCHEMA_REF),
            ..ItemQuery::default()
        })
        .unwrap();
    let effects = world
        .service
        .surface_dispatch(id.to_string(), click(), Some(HOST.into()), None)
        .await
        .effects;
    let layout_writes = writes
        .try_iter()
        .filter(|event| matches!(event, ItemEvent::OperationApplied { .. }))
        .count();
    (effects, layout_writes)
}

/// `[publish, open]`: one write of the layout row, one undo entry, and one
/// undo on the surface pane (where focus is) takes both back.
#[tokio::test]
async fn publish_then_open_is_one_step_and_one_undo() {
    let (world, layout) = world();
    let spec = button(json!([publish(), open_in("detail")]), PICKED);
    let (id, tile) = shown(&world.service, spec).await;
    let (before, revision) = tree(&layout).await;
    let detail_tile = detail(&before);
    assert_ne!(
        before.pane(detail_tile).unwrap().view_kind,
        ViewKindId::NOTES,
        "the fixture must change the detail pane"
    );
    assert!(picked_on(&before, tile).is_empty());

    let (effects, writes) = dispatch(&world, &id).await;
    assert!(effects.iter().all(|e| e.ok), "{effects:?}");
    assert_eq!(
        effects.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        ["publish", "open"]
    );
    assert_eq!(effects[0].message, "published on kind 'publication'");
    assert_eq!(effects[1].message, "opened a 'notes' pane");
    assert_eq!(writes, 1, "one click, one layout revision");

    let (after, after_revision) = tree(&layout).await;
    assert_ne!(after_revision, revision);
    assert_eq!(picked_on(&after, tile), [PICKED]);
    assert_eq!(
        after.pane(detail_tile).unwrap().view_kind,
        ViewKindId::NOTES
    );
    assert_eq!(focused(&after), focused(&before), "focus stays put");
    assert_eq!(focused(&after), Some(TileId::new(tile)), "on the surface");

    // One ⌘Z in the surface pane takes the whole click back.
    let undone = undo_in(&layout, tile).await;
    assert!(undone.ok, "{}", undone.message);
    let (reverted, _) = tree(&layout).await;
    assert!(
        picked_on(&reverted, tile).is_empty(),
        "the selection is back"
    );
    assert_eq!(
        reverted.pane(detail_tile).unwrap(),
        before.pane(detail_tile).unwrap(),
        "the detail pane is back"
    );
    // ... and it was the only step on the ring: a second undo changes
    // nothing.
    undo_in(&layout, tile).await;
    assert_eq!(
        serde_json::to_value(tree(&layout).await.0).unwrap(),
        serde_json::to_value(&reverted).unwrap(),
        "a second undo found another step of the click"
    );
}

/// A second effect the LAYOUT refuses (no pane holds the role) leaves the
/// layout as it was: the publish before it did not land either, and both
/// report the refusal.
#[tokio::test]
async fn a_refused_second_effect_leaves_the_layout_untouched() {
    let (world, layout) = world();
    let spec = button(json!([publish(), open_in("no-such-role")]), PICKED);
    let (id, tile) = shown(&world.service, spec).await;
    let (before, revision) = tree(&layout).await;

    let (effects, writes) = dispatch(&world, &id).await;
    assert_eq!(effects.len(), 2);
    assert!(effects.iter().all(|e| !e.ok), "{effects:?}");
    assert_eq!(effects[0].code, effects[1].code);
    assert!(
        effects[0].message.contains("none of it applied"),
        "{}",
        effects[0].message
    );
    assert_eq!(writes, 0, "nothing was written");

    let (after, after_revision) = tree(&layout).await;
    assert_eq!(after_revision, revision);
    assert!(picked_on(&after, tile).is_empty(), "the publish landed");
    assert_eq!(
        serde_json::to_value(&after).unwrap(),
        serde_json::to_value(&before).unwrap()
    );
}

/// A second effect refused before it reaches the layout (an id that is not
/// one) keeps the first from applying: the `open` reports `not applied` with
/// the publish's code, the publish its own refusal.
#[tokio::test]
async fn a_second_effect_refused_up_front_applies_none_of_the_click() {
    let (world, layout) = world();
    let spec = button(json!([open_in("detail"), publish()]), "not-an-id");
    let (id, _) = shown(&world.service, spec).await;
    let (before, revision) = tree(&layout).await;

    let (effects, writes) = dispatch(&world, &id).await;
    assert!(effects.iter().all(|e| !e.ok), "{effects:?}");
    assert_eq!(effects[1].kind, "publish");
    assert_eq!(effects[1].code.as_deref(), Some("invalid-argument"));
    assert!(effects[1].message.contains("not an item id"), "{effects:?}");
    assert_eq!(effects[0].kind, "open");
    assert_eq!(effects[0].code.as_deref(), Some("invalid-argument"));
    assert!(
        effects[0].message.starts_with("not applied"),
        "{}",
        effects[0].message
    );
    assert_eq!(writes, 0);

    let (after, after_revision) = tree(&layout).await;
    assert_eq!(after_revision, revision);
    assert_eq!(
        serde_json::to_value(&after).unwrap(),
        serde_json::to_value(&before).unwrap()
    );
}

/// An `emit` between two layout effects runs between them: the publish has
/// landed when it runs, and the open is a step of its own after it.
#[tokio::test]
async fn an_emit_between_layout_effects_runs_between_two_steps() {
    let (world, layout) = world();
    let emit = json!({ "emit": { "name": "between", "payload": {} } });
    let spec = button(json!([publish(), emit, open_in("detail")]), PICKED);
    let (id, tile) = shown(&world.service, spec).await;

    let (effects, writes) = dispatch(&world, &id).await;
    assert!(effects.iter().all(|e| e.ok), "{effects:?}");
    assert_eq!(
        effects.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        ["publish", "emit", "open"]
    );
    assert_eq!(writes, 2, "two gestures, two layout revisions");
    let (after, _) = tree(&layout).await;
    assert_eq!(picked_on(&after, tile), [PICKED]);
    assert_eq!(
        after.pane(detail(&after)).unwrap().view_kind,
        ViewKindId::NOTES
    );
}
