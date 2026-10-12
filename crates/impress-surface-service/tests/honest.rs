//! Wave 7 T5: a surface refusal carries a machine-readable `code`; a
//! dispatch is `ok` only when every effect happened, and reports each one;
//! an event records who caused it (reviews AC-F19, RS-S12 = AC-F11,
//! SK-K5 = AC-F5).

use std::sync::Arc;

use impress_item::item::ActorKind;
use impress_surface::{Event, EventKind, SurfaceSpec};
use impress_surface_service::runtime::{DefaultExecutor, SessionRegistry};
use impress_surface_service::{DefaultImpressSurfaceService, ImpressSurfaceService, SurfaceStore};
use koine_store_memory::MemoryStore;
use serde_json::{json, Value};

const HOST: &str = "honest-host";

/// A button that emits (always works) and publishes (fails: no pane shows
/// this surface in a store with no layout).
fn spec() -> SurfaceSpec {
    serde_json::from_value(json!({
        "surface": "1.0",
        "name": "Honest",
        "state": { "clicked": false },
        "root": { "column": [
            { "button": { "label": "Go", "on_click": [
                { "set": { "path": "state.clicked", "value": true } },
                { "emit": { "name": "went", "payload": {} } },
                { "publish": { "ids": "state.clicked" } }
            ] }, "id": "go" }
        ] }
    }))
    .unwrap()
}

fn click() -> Event {
    Event {
        widget: "go".to_string(),
        kind: EventKind::Click,
        value: Value::Null,
    }
}

fn world() -> (Arc<MemoryStore>, DefaultImpressSurfaceService) {
    let store = Arc::new(MemoryStore::new());
    let service = DefaultImpressSurfaceService::with_store(
        store.clone(),
        impress_pane_query_compile::test_support::builtin_manifest(),
    );
    (store, service)
}

#[tokio::test]
async fn a_dispatch_whose_effect_failed_is_not_ok_and_names_the_failure() {
    let (_store, svc) = world();
    let created = svc.surface_create(spec().into(), None, None).await;
    let id = created.id.unwrap();

    let d = svc
        .surface_dispatch(id.clone(), click(), Some(HOST.into()), None)
        .await;
    assert!(!d.ok, "a failed publish used to answer ok: {}", d.message);
    assert_eq!(d.code.as_deref(), Some("effect-failed"));
    assert_eq!(d.effects_failed, 1);
    assert!(d.tree.is_some(), "the re-rendered tree still comes back");
    let kinds: Vec<(&str, bool)> = d.effects.iter().map(|e| (e.kind.as_str(), e.ok)).collect();
    assert_eq!(kinds, vec![("emit", true), ("publish", false)]);
    let publish = &d.effects[1];
    assert_eq!(publish.code.as_deref(), Some("no-pane"));
    assert!(
        d.message.contains("1 failed") && d.message.contains("publish:"),
        "the message names what failed: {}",
        d.message
    );

    // The event was reduced and its state saved: the failure is the
    // effect's, not the dispatch's.
    let state = svc.surface_state_get(id, Some(HOST.into())).await;
    assert_eq!(state.state.unwrap()["clicked"], true);

    let json = serde_json::to_value(&d).unwrap();
    assert_eq!(json["code"], "effect-failed");
    assert_eq!(json["effects"][1]["code"], "no-pane");
}

#[tokio::test]
async fn a_dispatch_that_cannot_be_reduced_carries_the_reducers_code() {
    let (_store, svc) = world();
    let id = svc
        .surface_create(spec().into(), None, None)
        .await
        .id
        .unwrap();
    let d = svc
        .surface_dispatch(
            id,
            Event {
                widget: "nope".into(),
                kind: EventKind::Click,
                value: Value::Null,
            },
            Some(HOST.into()),
            None,
        )
        .await;
    assert!(!d.ok);
    assert_eq!(d.code.as_deref(), Some("unknown-widget"), "{}", d.message);
    assert!(d.tree.is_none());
}

#[tokio::test]
async fn lookups_say_not_found_and_malformed_ids_say_invalid_argument() {
    let (_store, svc) = world();
    let missing = svc
        .surface_get("00000000-0000-4000-8000-000000000000".into())
        .await;
    assert_eq!(
        missing.code.as_deref(),
        Some("not-found"),
        "{}",
        missing.message
    );
    let malformed = svc.surface_get("not-an-id".into()).await;
    assert_eq!(malformed.code.as_deref(), Some("invalid-argument"));
    let deleted = svc
        .surface_delete("00000000-0000-4000-8000-000000000000".into())
        .await;
    assert_eq!(deleted.code.as_deref(), Some("not-found"));
    let events = svc
        .surface_events("00000000-0000-4000-8000-000000000000".into(), Some(0), None)
        .await;
    assert_eq!(events.code.as_deref(), Some("not-found"));
}

#[tokio::test]
async fn a_stale_update_is_a_conflict() {
    let (_store, svc) = world();
    let id = svc
        .surface_create(spec().into(), None, None)
        .await
        .id
        .unwrap();
    let stale = svc.surface_update(id, spec().into(), None, Some(7)).await;
    assert!(!stale.ok);
    assert_eq!(stale.code.as_deref(), Some("conflict"), "{}", stale.message);
}

/// The pane's dispatch is the human's: its event says so, and an agent's
/// dispatch over MCP says `agent`.
#[tokio::test]
async fn an_event_records_who_caused_it() {
    let (store, svc) = world();
    let id = svc
        .surface_create(spec().into(), None, None)
        .await
        .id
        .unwrap();
    let surface_id = id.parse().unwrap();

    // As the pane does it: the runtime, dispatched as the human.
    let registry = SessionRegistry::with_manifest(
        impress_pane_query_compile::test_support::builtin_manifest(),
    );
    let surfaces = SurfaceStore::new(store.clone());
    let executor = DefaultExecutor::with_store(
        store.clone(),
        impress_pane_query_compile::test_support::builtin_manifest(),
    );
    let writer = surfaces.clone();
    registry
        .with(&surfaces, surface_id, HOST, move |rt| {
            Box::pin(async move {
                rt.dispatch(&executor, &writer, &click(), ActorKind::Human)
                    .await
                    .map(|_| ())
            })
        })
        .await
        .unwrap();

    // As an agent does it: the MCP verb.
    svc.surface_dispatch(id.clone(), click(), Some(HOST.into()), None)
        .await;

    let events = svc.surface_events(id, Some(0), Some(HOST.into())).await;
    let actors: Vec<&str> = events.events.iter().map(|e| e.actor.as_str()).collect();
    assert_eq!(actors, vec!["human", "agent"], "{:?}", events.events);
}
