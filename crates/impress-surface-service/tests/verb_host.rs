//! V1 (wave 5, ADR-0033 D4 amendment 2026-09-23): the host verb bridge.
//!
//! A surface rendered in the app can only call the four kit crates' verbs
//! through this crate's own linked `#[impress_service]` inventory. Wave 5
//! gives the executor and the service a second, host-supplied inventory —
//! [`impress_surface_service::VerbHost`] — consulted only when the linked
//! one lacks the name. These tests exercise that bridge with a fake host
//! that knows exactly one verb, against a real in-memory store, through the
//! same `DefaultImpressSurfaceService`/`DefaultExecutor` the app and MCP
//! both use.

use std::sync::Arc;

use impress_surface_service::runtime::VerbHost;
use impress_surface_service::{DefaultImpressSurfaceService, ImpressSurfaceService};
use koine_store_memory::MemoryStore;
use serde_json::{json, Value};

/// A verb this process never links: the fixture surface calls it, and only
/// a host that knows its name can answer it.
const FAKE_VERB: &str = "fake-service_echo";

/// Always in this crate's own linked inventory (S4's `surface_examples`) —
/// used to prove the inventory wins on a name collision.
const LINKED_VERB: &str = "impress-surface-service_surface-examples";

/// A [`VerbHost`] that knows `FAKE_VERB` and, for the collision test, claims
/// to know `LINKED_VERB` too — with a poisoned answer that must never be
/// seen, because the inventory should win before the host is ever asked.
struct FakeHost {
    /// When set, `call_verb` fails every call with this message instead of
    /// answering — test (d), a host verb whose call fails.
    fail_with: Option<String>,
}

impl FakeHost {
    fn ok() -> Arc<Self> {
        Arc::new(Self { fail_with: None })
    }

    fn failing(message: &str) -> Arc<Self> {
        Arc::new(Self {
            fail_with: Some(message.to_string()),
        })
    }
}

impl VerbHost for FakeHost {
    fn has_verb(&self, name: &str) -> bool {
        name == FAKE_VERB || name == LINKED_VERB
    }

    fn call_verb(&self, name: &str, args: Value) -> Result<Value, impress_service_core::Refusal> {
        if let Some(message) = &self.fail_with {
            return Err(message.clone().into());
        }
        if name == LINKED_VERB {
            // A poisoned answer for the name the real inventory also
            // serves. If this is ever returned, the inventory-wins rule
            // broke.
            return Ok(json!({"poisoned": true}));
        }
        Ok(json!({"echoed": args}))
    }
}

/// A self-contained spec whose one source calls [`FAKE_VERB`] and whose
/// root displays the echoed field, so a render either shows the value or
/// (with no working host) degrades to a placeholder — never panics.
fn fixture_spec() -> impress_surface::SurfaceSpec {
    serde_json::from_value(json!({
        "surface": "1.0",
        "name": "Host verb bridge fixture",
        "state": {},
        "sources": {
            "echo": { "verb": FAKE_VERB, "args": { "x": 42 } }
        },
        "root": { "column": [
            { "text": "{{source.echo.echoed.x}}", "id": "echoed-text" }
        ] }
    }))
    .expect("fixture spec is well-formed JSON against SurfaceSpec")
}

/// A spec whose one verb reference is [`LINKED_VERB`] — already in this
/// crate's own inventory, with no host needed.
fn linked_verb_spec() -> impress_surface::SurfaceSpec {
    serde_json::from_value(json!({
        "surface": "1.0",
        "name": "Linked verb collision fixture",
        "state": {},
        "sources": {
            "examples": { "verb": LINKED_VERB, "args": {} }
        },
        "root": { "column": [
            { "text": "{{source.examples}}", "id": "examples-text" }
        ] }
    }))
    .expect("fixture spec is well-formed JSON against SurfaceSpec")
}

fn store() -> Arc<MemoryStore> {
    Arc::new(MemoryStore::new())
}

/// (a) A spec whose `source` names a host-only verb renders with the
/// echoed value in the tree once a host that knows it is installed.
#[tokio::test]
async fn a_host_verb_renders_through_the_service() {
    let service = DefaultImpressSurfaceService::with_store(
        store(),
        impress_pane_query_compile::test_support::builtin_manifest(),
    )
    .with_verb_host(FakeHost::ok());

    let created = service
        .surface_create(fixture_spec().into(), None, None)
        .await;
    assert!(created.ok, "{}", created.message);
    let id = created.id.expect("created surface has an id");

    let rendered = service.surface_render(id, None, None).await;
    assert!(rendered.ok, "{}", rendered.message);
    let tree_json = serde_json::to_string(&rendered.tree).expect("encode tree");
    assert!(
        !tree_json.contains("\"placeholder\""),
        "the tree contains a placeholder node, the host verb did not resolve: {tree_json}"
    );
    assert!(
        tree_json.contains("42"),
        "the echoed value is not in the render tree: {tree_json}"
    );
}

/// (b) `surface_validate` reports a host-only verb as missing with no host
/// installed, and clean once one is.
#[tokio::test]
async fn validate_accepts_a_host_verb_only_once_a_host_is_installed() {
    let without_host = DefaultImpressSurfaceService::with_store(
        store(),
        impress_pane_query_compile::test_support::builtin_manifest(),
    );
    let result = without_host.surface_validate(fixture_spec().into()).await;
    assert!(
        !result.ok,
        "a host-only verb must be reported missing with no host installed"
    );
    assert!(
        result
            .problems
            .iter()
            .any(|p| p.message.contains(FAKE_VERB)),
        "{:?}",
        result.problems
    );

    let with_host = DefaultImpressSurfaceService::with_store(
        store(),
        impress_pane_query_compile::test_support::builtin_manifest(),
    )
    .with_verb_host(FakeHost::ok());
    let result = with_host.surface_validate(fixture_spec().into()).await;
    assert!(result.ok, "{:?}", result.problems);
}

/// (c) A verb the linked inventory ALSO has is answered by the inventory,
/// never the host — proven by giving the host a poisoned answer for that
/// name and checking the real result (not the poison) arrives.
#[tokio::test]
async fn the_linked_inventory_wins_over_the_host_on_a_shared_name() {
    let poisoned_host = FakeHost::ok();
    let service = DefaultImpressSurfaceService::with_store(
        store(),
        impress_pane_query_compile::test_support::builtin_manifest(),
    )
    .with_verb_host(poisoned_host);

    let created = service
        .surface_create(linked_verb_spec().into(), None, None)
        .await;
    assert!(created.ok, "{}", created.message);
    let id = created.id.expect("created surface has an id");

    let rendered = service.surface_render(id, None, None).await;
    assert!(rendered.ok, "{}", rendered.message);
    let tree_json = serde_json::to_string(&rendered.tree).expect("encode tree");
    assert!(
        !tree_json.contains("poisoned"),
        "the host's poisoned answer leaked through instead of the real inventory result: \
         {tree_json}"
    );
    // `impress-surface-service_surface-examples`'s real answer is the
    // signal-explorer worked example — its name is the cheapest thing to
    // assert on without re-deriving the whole example spec here.
    assert!(
        tree_json.contains("Signal explorer"),
        "the real inventory result did not arrive: {tree_json}"
    );
}

/// (d) A host verb whose call fails yields the same source-error shape in
/// the render tree a failing inventory verb yields today — a placeholder
/// carrying a reason, never a panic.
#[tokio::test]
async fn a_failing_host_verb_yields_a_placeholder_not_a_panic() {
    let service = DefaultImpressSurfaceService::with_store(
        store(),
        impress_pane_query_compile::test_support::builtin_manifest(),
    )
    .with_verb_host(FakeHost::failing("the fake host refuses"));

    let created = service
        .surface_create(fixture_spec().into(), None, None)
        .await;
    assert!(created.ok, "{}", created.message);
    let id = created.id.expect("created surface has an id");

    let rendered = service.surface_render(id, None, None).await;
    assert!(
        rendered.ok,
        "a failing SOURCE must not fail the whole render: {}",
        rendered.message
    );
    let tree_json = serde_json::to_string(&rendered.tree).expect("encode tree");
    assert!(
        tree_json.contains("\"placeholder\""),
        "a source whose verb failed must degrade to a placeholder, not silently vanish: \
         {tree_json}"
    );
    assert!(
        tree_json.contains("did not resolve to a value"),
        "the placeholder's reason should be the usual unresolved-template message: {tree_json}"
    );
}
