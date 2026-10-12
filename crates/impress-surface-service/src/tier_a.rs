//! Tier A capabilities — pure Rust against [`ImpressSurfaceService`] over a
//! private in-memory store. No app, no UI, no network: every capability
//! opens its own store, so they cannot see each other's surfaces and can run
//! in any order — the same discipline
//! `impress-layout-service::tier_a`'s module docs describe.
//!
//! Every capability here uses ONLY verbs this crate's own inventory always
//! carries (`impress-surface-service_*`) — never `surface-demo-service_*` —
//! so `run()` (and therefore the `surface-selftest-service_run-selftest` MCP
//! tool) works in any binary that links `impress-surface-service`,
//! regardless of which OTHER capabilities happen to be linked alongside it.
//! `Source::Verb`/`Action::Call` are exercised by calling
//! `impress-surface-service_surface-examples` — a verb of THIS crate's own
//! trait, always present — rather than a capability from elsewhere.
//!
//! The full loop against the REAL `surface-demo-service` verbs (series,
//! histogram, the signal-explorer example) is a separate `#[cfg(test)]`
//! integration test at the bottom of this file — see its module docs for
//! why it is dev-only rather than part of this catalogue.
//!
//! Two cases that drive the suite's own store service (a generated settings
//! pane and the paper-triage loop over `triage-service`) are not here: this
//! crate does not know that service. impress-apps runs them from
//! `impress-capabilities/tests/surface_over_suite_store.rs`.

use std::sync::Arc;

use impress_item::store::AttributedStore;
use impress_layout_service::dto::PaneRefDto;
use impress_layout_service::{DefaultLayoutService, LayoutService};
use impress_pane_query::process_manifest;
use impress_surface::{Event, EventKind, Source, SurfaceSpec};
use koine_store_memory::MemoryStore;
use serde_json::{json, Value};

use crate::dto::{ShowTargetDto, SplitTargetDto};
use crate::report::{CapabilityResult, Tier};
use crate::service::{DefaultImpressSurfaceService, ImpressSurfaceService};
use crate::store::EVENT_RING_CAPACITY;
use crate::{check, Result};

const APP: &str = "surface-selftest";
/// Fixed, so the catalogue never depends on the host's name.
const DEVICE: &str = "surface-selftest-device";

/// How a capability opens its private store. `None`: a fresh
/// [`MemoryStore`] — the catalogue needs only the port.
static STORE_FACTORY: std::sync::RwLock<Option<StoreFactory>> = std::sync::RwLock::new(None);

/// Opens a fresh, empty store.
type StoreFactory = fn() -> Arc<dyn AttributedStore>;

/// Run the catalogue over another backend of the store port: `open` is called
/// once per capability and must return a fresh, empty store. The effects
/// proof in `impress-capabilities` uses this to run it over `SqliteItemStore`,
/// the one backend that reports which kinds each call read and wrote.
pub fn set_store_factory(open: StoreFactory) {
    *STORE_FACTORY.write().unwrap_or_else(|e| e.into_inner()) = Some(open);
}

fn open_store() -> Arc<dyn AttributedStore> {
    match *STORE_FACTORY.read().unwrap_or_else(|e| e.into_inner()) {
        Some(open) => open(),
        None => Arc::new(MemoryStore::new()),
    }
}

struct World {
    #[allow(dead_code)]
    store: Arc<dyn AttributedStore>,
    service: DefaultImpressSurfaceService,
}

impl World {
    /// A private store ([`open_store`]: in memory by default).
    fn open() -> Self {
        Self::over(open_store())
    }

    fn over(store: Arc<dyn AttributedStore>) -> Self {
        // `surface_show` composes the layout verbs through the inventory,
        // which read the process's kind manifest; a host installs it. In
        // this crate's own tests the host is the test.
        #[cfg(test)]
        crate::install_test_manifest();
        Self {
            service: DefaultImpressSurfaceService::with_store(
                store.clone(),
                process_manifest::current(),
            ),
            store,
        }
    }
}

fn want(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into().into())
    }
}

/// A self-contained spec exercising every node/action/source kind this
/// catalogue needs, with every widget's id given explicitly so a capability
/// never has to guess the auto-derived one. Its one verb reference
/// (`impress-surface-service_surface-examples`) is always in this binary's
/// own inventory — see the module docs.
fn fixture_spec() -> SurfaceSpec {
    serde_json::from_value(json!({
        "surface": "1.0",
        "name": "Tier A fixture",
        "state": { "greeting": "", "bins": 1 },
        "sources": {
            "value_src": { "value": { "n": 42 } },
            "verb_src": { "verb": "impress-surface-service_surface-examples", "args": {} }
        },
        "root": { "column": [
            { "text": "{{source.value_src.n}}", "id": "n-text" },
            { "text": "{{source.verb_src}}", "id": "verb-text" },
            { "field": { "text": {} }, "label": "Greeting", "bind": "state.greeting",
              "id": "greeting-field" },
            { "button": { "label": "Say hi", "on_click": [
                { "set": { "path": "state.greeting", "value": "hi" } },
                { "emit": { "name": "said-hi",
                            "payload": { "greeting": "{{state.greeting}}" } } }
              ] }, "id": "say-hi" }
        ] }
    }))
    .expect("fixture spec is well-formed JSON against SurfaceSpec")
}

/// Run every Tier A capability and collect the results.
pub async fn run() -> Vec<CapabilityResult> {
    vec![
        cap_schema().await,
        cap_validate_clean().await,
        cap_validate_catches_missing_verb().await,
        cap_create_get_list_delete().await,
        cap_update().await,
        cap_show_composes_layout().await,
        cap_render_resolves_value_and_verb_sources().await,
        cap_state_get_set().await,
        cap_dispatch_change_sets_bound_state().await,
        cap_dispatch_click_runs_set_and_emit().await,
        cap_events_after_cursor().await,
        cap_wait_returns_an_existing_event_immediately().await,
        cap_wait_times_out_with_nothing_new().await,
        cap_examples().await,
        cap_event_ring_is_pruned().await,
    ]
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

async fn cap_schema() -> CapabilityResult {
    check(
        "schema",
        "surface_schema returns a JSON Schema and the signal-explorer worked example",
        Tier::A,
        || async {
            let world = World::open();
            let schema = world.service.surface_schema().await;
            want(schema.schema.is_object(), "schema is not a JSON object")?;
            want(
                schema.example.name == "Signal explorer",
                "example is not the signal-explorer worked example",
            )?;
            Ok("schema + example present".to_string())
        },
    )
    .await
}

async fn cap_validate_clean() -> CapabilityResult {
    check(
        "validate-clean",
        "a well-formed spec, verbs included, validates with no problems",
        Tier::A,
        || async {
            let world = World::open();
            let result = world.service.surface_validate(fixture_spec().into()).await;
            want(
                result.ok,
                format!("unexpected problems: {:?}", result.problems),
            )?;
            Ok("fixture spec validates clean".to_string())
        },
    )
    .await
}

async fn cap_validate_catches_missing_verb() -> CapabilityResult {
    check(
        "validate-missing-verb",
        "a spec naming a verb outside the linked inventory is a validation problem (ADR-0033 D4)",
        Tier::A,
        || async {
            let world = World::open();
            let mut spec = fixture_spec();
            spec.sources.insert(
                "bogus".to_string(),
                Source::Verb {
                    verb: "no-such-service_no-such-verb".to_string(),
                    args: json!({}),
                },
            );
            let result = world.service.surface_validate(spec.into()).await;
            want(
                result
                    .problems
                    .iter()
                    .any(|p| p.message.contains("no such verb")),
                format!("expected a missing-verb problem: {:?}", result.problems),
            )?;
            Ok("missing verb flagged by path".to_string())
        },
    )
    .await
}

async fn cap_create_get_list_delete() -> CapabilityResult {
    check(
        "create-get-list-delete",
        "create a surface, read it back verbatim, list it, then delete it",
        Tier::A,
        || async {
            let world = World::open();
            let spec = fixture_spec();
            let created = world
                .service
                .surface_create(
                    spec.clone().into(),
                    Some("Fixture".to_string()),
                    Some(vec!["demo".to_string()]),
                )
                .await;
            want(created.ok, format!("create failed: {}", created.message))?;
            let id = created.id.clone().ok_or("create returned no id")?;
            want(
                created.name.as_deref() == Some("Fixture"),
                "name override was not applied",
            )?;
            want(
                created.tags == vec!["demo".to_string()],
                "tags were not stored",
            )?;

            let got = world.service.surface_get(id.clone()).await;
            want(got.ok, format!("get failed: {}", got.message))?;
            want(
                got.spec.as_ref() == Some(&spec),
                "get did not return the same spec",
            )?;

            let listed = world.service.surface_list().await;
            want(
                listed.surfaces.iter().any(|s| s.id == id),
                "list did not include the created surface",
            )?;

            let deleted = world.service.surface_delete(id.clone()).await;
            want(deleted.ok, format!("delete failed: {}", deleted.message))?;
            let after = world.service.surface_get(id).await;
            want(!after.ok, "surface still readable after delete")?;
            Ok("create/get/list/delete round-trip".to_string())
        },
    )
    .await
}

async fn cap_update() -> CapabilityResult {
    check(
        "update",
        "surface_update replaces the spec, bumps the revision, keeps the row's name unless \
         given one, and refuses a stale expected_revision without writing",
        Tier::A,
        || async {
            let world = World::open();
            let created = world
                .service
                .surface_create(fixture_spec().into(), Some("Chosen name".into()), None)
                .await;
            want(
                created.revision == Some(1),
                format!("a new surface is revision 1, got {:?}", created.revision),
            )?;
            let id = created.id.ok_or("create returned no id")?;
            let mut spec2 = fixture_spec();
            spec2.name = "Renamed in the spec".to_string();
            let updated = world
                .service
                .surface_update(id.clone(), spec2.clone().into(), None, Some(1))
                .await;
            want(updated.ok, format!("update failed: {}", updated.message))?;
            want(
                updated.name.as_deref() == Some("Chosen name"),
                format!(
                    "the create-time name was replaced by the spec's (RS-S25): {:?}",
                    updated.name
                ),
            )?;
            want(
                updated.revision == Some(2),
                format!(
                    "an update bumps the revision to 2, got {:?}",
                    updated.revision
                ),
            )?;

            // A writer that read revision 1 and did not see the update above.
            let mut stale = fixture_spec();
            stale.name = "Lost update".to_string();
            let refused = world
                .service
                .surface_update(id.clone(), stale.into(), Some("Lost".into()), Some(1))
                .await;
            want(!refused.ok, "a stale expected_revision was accepted")?;
            want(
                refused.message.starts_with("conflict:"),
                format!("the refusal does not say conflict: {}", refused.message),
            )?;
            let after = world.service.surface_get(id.clone()).await;
            want(
                after.revision == Some(2) && after.spec.as_ref() == Some(&spec2),
                "a refused update still wrote",
            )?;

            let renamed = world
                .service
                .surface_update(id, spec2.into(), Some("New name".into()), None)
                .await;
            want(
                renamed.ok && renamed.name.as_deref() == Some("New name"),
                format!("an explicit name was not applied: {}", renamed.message),
            )?;
            Ok("update: revision 1 → 2 → 3, name kept, stale write refused".to_string())
        },
    )
    .await
}

async fn cap_show_composes_layout() -> CapabilityResult {
    check(
        "show",
        "surface_show composes layout-service verbs: a split whose new pane's query is \
         item(id) of the surface kind, view kind 'surface'",
        Tier::A,
        || async {
            let world = World::open();
            let created = world
                .service
                .surface_create(fixture_spec().into(), None, None)
                .await;
            let id = created.id.ok_or("create returned no id")?;

            let shown = world
                .service
                .surface_show(
                    id,
                    ShowTargetDto {
                        split: Some(SplitTargetDto {
                            direction: "vertical".to_string(),
                            from_focused: true,
                        }),
                        ..ShowTargetDto::default()
                    },
                    APP.to_string(),
                    Some(DEVICE.to_string()),
                )
                .await;
            want(shown.ok, format!("show failed: {}", shown.message))?;
            want(shown.focused, "the shown pane was not left focused")?;
            let tile = shown.tile.ok_or("show returned no tile")?;

            // Confirm through an independent LayoutService handle (a fresh
            // session, reloaded from the SAME store) that the pane really
            // is a `surface` pane — not just that `surface_show` claimed so.
            let layout =
                DefaultLayoutService::with_store(world.store.clone(), process_manifest::current());
            let pane = layout
                .get_pane(
                    APP.to_string(),
                    Some(DEVICE.to_string()),
                    PaneRefDto::tile(impress_layout::TileId::new(tile)),
                )
                .await;
            want(pane.ok, format!("get_pane failed: {}", pane.message))?;
            let spec = pane.spec.ok_or("get_pane returned no pane spec")?;
            want(
                spec.view_kind.to_string() == "surface",
                format!("expected view kind 'surface', got '{}'", spec.view_kind),
            )?;
            Ok(format!("surface shown in tile {tile}, view kind 'surface'"))
        },
    )
    .await
}

async fn cap_render_resolves_value_and_verb_sources() -> CapabilityResult {
    check(
        "render",
        "surface_render fetches a value source and a verb source and resolves the tree",
        Tier::A,
        || async {
            let world = World::open();
            let created = world
                .service
                .surface_create(fixture_spec().into(), None, None)
                .await;
            let id = created.id.ok_or("create returned no id")?;

            let rendered = world.service.surface_render(id, None, None).await;
            want(rendered.ok, format!("render failed: {}", rendered.message))?;
            let tree = rendered.tree.ok_or("render returned no tree")?;
            let text = serde_json::to_string(&tree).map_err(|e| e.to_string())?;
            want(
                text.contains("42"),
                "the value source did not resolve into the tree",
            )?;
            want(
                !text.contains("\"placeholder\""),
                format!("the tree contains a placeholder node: {text}"),
            )?;
            Ok("value source and verb source both resolved with no placeholder".to_string())
        },
    )
    .await
}

async fn cap_state_get_set() -> CapabilityResult {
    check(
        "state-get-set",
        "surface_state_get answers the spec's own initial state before any dispatch; \
         surface_state_set overwrites it directly",
        Tier::A,
        || async {
            let world = World::open();
            let created = world
                .service
                .surface_create(fixture_spec().into(), None, None)
                .await;
            let id = created.id.ok_or("create returned no id")?;

            let initial = world.service.surface_state_get(id.clone(), None).await;
            want(initial.ok, format!("state_get failed: {}", initial.message))?;
            want(
                initial
                    .state
                    .as_ref()
                    .and_then(|s| s.get("greeting"))
                    .and_then(Value::as_str)
                    == Some(""),
                "initial state did not match the spec's own state block",
            )?;

            let set = world
                .service
                .surface_state_set(id.clone(), json!({"greeting": "hello", "bins": 5}), None)
                .await;
            want(set.ok, format!("state_set failed: {}", set.message))?;

            let got = world.service.surface_state_get(id, None).await;
            want(
                got.state
                    .as_ref()
                    .and_then(|s| s.get("greeting"))
                    .and_then(Value::as_str)
                    == Some("hello"),
                "state_set did not persist",
            )?;
            Ok("state_get/state_set round-trip".to_string())
        },
    )
    .await
}

async fn cap_dispatch_change_sets_bound_state() -> CapabilityResult {
    check(
        "dispatch-change",
        "dispatching a `change` event sets the field's bound state path",
        Tier::A,
        || async {
            let world = World::open();
            let created = world
                .service
                .surface_create(fixture_spec().into(), None, None)
                .await;
            let id = created.id.ok_or("create returned no id")?;

            let event = Event {
                widget: "greeting-field".to_string(),
                kind: EventKind::Change,
                value: json!("changed via dispatch"),
            };
            let dispatched = world
                .service
                .surface_dispatch(id.clone(), event, None, None)
                .await;
            want(
                dispatched.ok,
                format!("dispatch failed: {}", dispatched.message),
            )?;
            want(
                dispatched.tree.is_some(),
                "dispatch did not return a re-rendered tree",
            )?;

            let state = world.service.surface_state_get(id, None).await;
            want(
                state
                    .state
                    .as_ref()
                    .and_then(|s| s.get("greeting"))
                    .and_then(Value::as_str)
                    == Some("changed via dispatch"),
                "dispatch(change) did not set state.greeting",
            )?;
            Ok("dispatch(change) sets bound state and persists it".to_string())
        },
    )
    .await
}

async fn cap_dispatch_click_runs_set_and_emit() -> CapabilityResult {
    check(
        "dispatch-click",
        "dispatching a button `click` runs its on_click actions (set, then emit) and the \
         event is recorded",
        Tier::A,
        || async {
            let world = World::open();
            let created = world
                .service
                .surface_create(fixture_spec().into(), None, None)
                .await;
            let id = created.id.ok_or("create returned no id")?;

            let click = Event {
                widget: "say-hi".to_string(),
                kind: EventKind::Click,
                value: Value::Null,
            };
            let dispatched = world
                .service
                .surface_dispatch(id.clone(), click, None, None)
                .await;
            want(
                dispatched.ok,
                format!("dispatch failed: {}", dispatched.message),
            )?;
            want(
                dispatched.effects.iter().any(|e| e.kind == "emit" && e.ok),
                format!(
                    "no successful emit effect reported: {:?}",
                    dispatched.effects
                ),
            )?;

            let events = world.service.surface_events(id, Some(0), None).await;
            want(
                events.ok,
                format!("surface_events failed: {}", events.message),
            )?;
            want(
                events.events.iter().any(|e| e.name == "said-hi"),
                format!("'said-hi' event not found: {:?}", events.events),
            )?;
            Ok("click runs set+emit; the emitted event is readable".to_string())
        },
    )
    .await
}

async fn cap_events_after_cursor() -> CapabilityResult {
    check(
        "events-after-cursor",
        "surface_events pages by seq, returning only events past after_seq",
        Tier::A,
        || async {
            let world = World::open();
            let created = world
                .service
                .surface_create(fixture_spec().into(), None, None)
                .await;
            let id = created.id.ok_or("create returned no id")?;

            for _ in 0..3 {
                let click = Event {
                    widget: "say-hi".to_string(),
                    kind: EventKind::Click,
                    value: Value::Null,
                };
                world
                    .service
                    .surface_dispatch(id.clone(), click, None, None)
                    .await;
            }
            let all = world
                .service
                .surface_events(id.clone(), Some(0), None)
                .await;
            want(
                all.events.len() == 3,
                format!("expected 3 events, got {}", all.events.len()),
            )?;
            let after_one = world.service.surface_events(id, Some(1), None).await;
            want(
                after_one.events.len() == 2,
                format!(
                    "after_seq=1 should leave 2 events, got {}",
                    after_one.events.len()
                ),
            )?;
            Ok("events_after filters by seq".to_string())
        },
    )
    .await
}

async fn cap_wait_returns_an_existing_event_immediately() -> CapabilityResult {
    check(
        "wait-immediate",
        "surface_wait returns an already-emitted event without waiting out the timeout",
        Tier::A,
        || async {
            let world = World::open();
            let created = world
                .service
                .surface_create(fixture_spec().into(), None, None)
                .await;
            let id = created.id.ok_or("create returned no id")?;

            let click = Event {
                widget: "say-hi".to_string(),
                kind: EventKind::Click,
                value: Value::Null,
            };
            world
                .service
                .surface_dispatch(id.clone(), click, None, None)
                .await;

            let started = std::time::Instant::now();
            let waited = world.service.surface_wait(id, Some(0), 5_000, None).await;
            want(
                waited.ok && !waited.timed_out,
                format!("{waited:?}", waited = waited.message),
            )?;
            want(
                !waited.events.is_empty(),
                "wait returned no events though one already existed",
            )?;
            want(
                started.elapsed() < std::time::Duration::from_secs(2),
                "wait took long enough to suggest it slept instead of returning immediately",
            )?;
            Ok("wait returns a past event immediately".to_string())
        },
    )
    .await
}

async fn cap_wait_times_out_with_nothing_new() -> CapabilityResult {
    check(
        "wait-timeout",
        "surface_wait times out cleanly (timed_out: true, no events) when nothing new happens",
        Tier::A,
        || async {
            let world = World::open();
            let created = world
                .service
                .surface_create(fixture_spec().into(), None, None)
                .await;
            let id = created.id.ok_or("create returned no id")?;

            let waited = world.service.surface_wait(id, Some(0), 300, None).await;
            want(
                waited.ok && waited.timed_out && waited.events.is_empty(),
                format!(
                    "expected a clean timeout, got ok={} timed_out={} events={}",
                    waited.ok,
                    waited.timed_out,
                    waited.events.len()
                ),
            )?;
            Ok("wait times out with nothing new".to_string())
        },
    )
    .await
}

async fn cap_examples() -> CapabilityResult {
    check(
        "examples",
        "surface_examples returns at least the signal-explorer worked example",
        Tier::A,
        || async {
            let world = World::open();
            let examples = world.service.surface_examples().await;
            want(!examples.examples.is_empty(), "no examples returned")?;
            want(
                examples.examples[0].name == "Signal explorer",
                "expected the signal-explorer example first",
            )?;
            Ok(format!("{} example(s)", examples.examples.len()))
        },
    )
    .await
}

async fn cap_event_ring_is_pruned() -> CapabilityResult {
    check(
        "event-ring-pruned",
        "the event ring keeps only the last 200 rows per (surface, host) (ADR-0033 D5)",
        Tier::A,
        || async {
            let world = World::open();
            let created = world
                .service
                .surface_create(fixture_spec().into(), None, None)
                .await;
            let id = created.id.ok_or("create returned no id")?;

            let extra = EVENT_RING_CAPACITY + 5;
            for _ in 0..extra {
                let click = Event {
                    widget: "say-hi".to_string(),
                    kind: EventKind::Click,
                    value: Value::Null,
                };
                world
                    .service
                    .surface_dispatch(id.clone(), click, None, None)
                    .await;
            }
            let all = world.service.surface_events(id, Some(0), None).await;
            want(
                all.events.len() == EVENT_RING_CAPACITY,
                format!(
                    "expected {EVENT_RING_CAPACITY} events after pruning, got {}",
                    all.events.len()
                ),
            )?;
            let min_seq = all.events.iter().map(|e| e.seq).min().unwrap_or(0);
            want(
                min_seq as usize == extra - EVENT_RING_CAPACITY + 1,
                format!(
                    "pruning did not keep the newest {EVENT_RING_CAPACITY} rows: oldest \
                     surviving seq is {min_seq}"
                ),
            )?;
            Ok(format!(
                "{extra} events pruned down to {EVENT_RING_CAPACITY}"
            ))
        },
    )
    .await
}

// ---------------------------------------------------------------------------
// The real-verb loop test (dev-only)
// ---------------------------------------------------------------------------

/// The signal-explorer loop against the REAL `surface-demo-service` verbs
/// (ADR-0033 S9): create → show into a split → dispatch a `change` on the
/// bins slider → render shows the new value → dispatch a `click` on "Use
/// these bins" → `surface_events` returns `bins-chosen` with the payload.
///
/// `surface-demo-service` is a `[dev-dependencies]`-only dependency of this
/// crate (see `Cargo.toml`): the production `run()` catalogue above never
/// needs it (and must not — whether that crate is linked into a given binary
/// is a Cargo *feature* decision made in `impress-capabilities`, independent
/// of this one), but a `cargo test` binary links dev-dependencies too, so
/// `use surface_demo_service as _;` below force-links its
/// `#[impress_service]` inventory entries into THIS test binary the same way
/// `impress-capabilities`' own force-link list does (see that crate's
/// module docs) — without it, the linker is free to drop the whole rlib for
/// lack of a real reference and `series`/`histogram` would not be in the
/// inventory `surface_validate`/`surface_render` reach for.
#[cfg(test)]
mod real_verb_loop {
    use super::*;

    #[allow(unused_imports)]
    use surface_demo_service as _force_link_surface_demo_service;

    fn signal_explorer() -> SurfaceSpec {
        impress_surface::example_signal_explorer()
    }

    #[tokio::test]
    async fn the_signal_explorer_loop_runs_against_the_real_demo_verbs() {
        let world = World::open();

        // 0. The example validates clean against the REAL linked inventory
        // now that `series`/`histogram` are force-linked above — this is
        // the one assertion that would fail loudly if the force-link were
        // ever removed by accident.
        let validated = world
            .service
            .surface_validate(signal_explorer().into())
            .await;
        assert!(
            validated.ok,
            "unexpected problems: {:?}",
            validated.problems
        );

        // 1. create
        let created = world
            .service
            .surface_create(signal_explorer().into(), None, None)
            .await;
        assert!(created.ok, "{}", created.message);
        let id = created.id.expect("created surface has an id");

        // 2. show into a split
        let shown = world
            .service
            .surface_show(
                id.clone(),
                ShowTargetDto {
                    split: Some(SplitTargetDto {
                        direction: "vertical".to_string(),
                        from_focused: true,
                    }),
                    ..ShowTargetDto::default()
                },
                APP.to_string(),
                Some(DEVICE.to_string()),
            )
            .await;
        assert!(shown.ok, "{}", shown.message);

        // First render: the sliders' default state (freq 1.0, bins 20)
        // drives `series`/`histogram` through the REAL verbs.
        let first_render = world.service.surface_render(id.clone(), None, None).await;
        assert!(first_render.ok, "{}", first_render.message);
        let first_tree = serde_json::to_string(&first_render.tree.unwrap()).unwrap();
        assert!(
            !first_tree.contains("\"placeholder\""),
            "first render has a placeholder: {first_tree}"
        );

        // 3. dispatch a `change` on the bins slider
        let change = Event {
            widget: "bins-slider".to_string(),
            kind: EventKind::Change,
            value: json!(40),
        };
        let dispatched = world
            .service
            .surface_dispatch(id.clone(), change, None, None)
            .await;
        assert!(dispatched.ok, "{}", dispatched.message);

        // 4. render shows the new value
        let state = world.service.surface_state_get(id.clone(), None).await;
        assert_eq!(
            state
                .state
                .as_ref()
                .and_then(|s| s.get("bins"))
                .and_then(Value::as_u64),
            Some(40),
            "state.bins did not change: {:?}",
            state.state
        );
        let rendered = world.service.surface_render(id.clone(), None, None).await;
        assert!(rendered.ok, "{}", rendered.message);
        let tree_json = serde_json::to_string(&rendered.tree.unwrap()).unwrap();
        assert!(
            !tree_json.contains("\"placeholder\""),
            "render after bins change has a placeholder: {tree_json}"
        );

        // 5. click "Use these bins"
        let click = Event {
            widget: "use-bins-btn".to_string(),
            kind: EventKind::Click,
            value: Value::Null,
        };
        let clicked = world
            .service
            .surface_dispatch(id.clone(), click, None, None)
            .await;
        assert!(clicked.ok, "{}", clicked.message);
        assert!(
            clicked.effects.iter().any(|e| e.kind == "emit" && e.ok),
            "no successful emit effect: {:?}",
            clicked.effects
        );

        // 6. surface_events returns bins-chosen with the payload
        let events = world.service.surface_events(id, Some(0), None).await;
        assert!(events.ok, "{}", events.message);
        let bins_chosen = events
            .events
            .iter()
            .find(|e| e.name == "bins-chosen")
            .unwrap_or_else(|| panic!("no 'bins-chosen' event among {:?}", events.events));
        assert_eq!(
            bins_chosen.payload.get("bins").and_then(Value::as_u64),
            Some(40)
        );
    }
}
