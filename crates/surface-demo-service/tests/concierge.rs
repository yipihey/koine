//! The concierge, one round at a time (docs/plan-koine-tour.md T-D3, T-D5):
//! given a dispatched question, it creates a surface and writes the
//! transcript — every step a verb call through the pipeline, over a memory
//! store installed as the process store the way a host installs its own.
//!
//! The surface verbs come from `impress-surface-service`, a dev-dependency
//! force-linked below: an unreferenced crate that only holds
//! `inventory::submit!` statics is dropped from the test binary.
//!
//! The process store is one per test binary and `tests/tour.rs` installs
//! one too: whichever harness runs first installs, and the other keeps it
//! (`setup` installs only when nothing is installed yet, and its manifest
//! is a superset of the tour's). Nothing here carries the tour's tag, so the
//! tour's own counts never see these surfaces.

use std::sync::{Arc, OnceLock};

use impress_item::schema::refs;
use impress_pane_query::{process_manifest, KindManifest, RecordKindId};
use koine_store_memory::MemoryStore;
use serde_json::{json, Value};
use surface_demo_service::concierge::{self, Concierge, ConciergeConfig, Round, NAME, SPEC};
use surface_demo_service::notes;

#[allow(unused_imports)]
use impress_surface_service as _force_link_impress_surface_service;

const APP: &str = "concierge-test";
/// A private instance, so the rounds never touch this machine's device.
const HOST: &str = "concierge-test-device";
/// The tag the test's own concierge rows carry — not the tour's.
const TEST_TAG: &str = "concierge-test";

const VALIDATE: &str = "impress-surface-service_surface-validate";
const CREATE: &str = "impress-surface-service_surface-create";
const LIST: &str = "impress-surface-service_surface-list";
const RENDER: &str = "impress-surface-service_surface-render";
const STATE_GET: &str = "impress-surface-service_surface-state-get";
const STATE_SET: &str = "impress-surface-service_surface-state-set";
const DISPATCH: &str = "impress-surface-service_surface-dispatch";

/// The ui kinds a host with no domain installs (`koine_host::manifest::minimal`,
/// spelled here from the same refs so a test needs no host), plus the
/// tour's `note` kind, so a tour test that runs after this harness
/// installed finds what it needs too.
fn manifest() -> KindManifest {
    let kinds = [
        ("layout", refs::IMPRESS_UI_LAYOUT.as_str()),
        ("preset", refs::IMPRESS_UI_PRESET.as_str()),
        ("surface", refs::IMPRESS_UI_SURFACE.as_str()),
        ("surface-state", refs::IMPRESS_UI_SURFACE_STATE.as_str()),
        ("surface-event", refs::IMPRESS_UI_SURFACE_EVENT.as_str()),
        ("task", refs::TASK.as_str()),
        (notes::KIND, notes::SCHEMA_REF),
    ]
    .into_iter()
    .map(|(kind, schema)| {
        (
            RecordKindId::from(kind.to_string()),
            vec![schema.to_string()],
        )
    })
    .collect();
    KindManifest {
        kinds,
        contains_members: Default::default(),
    }
}

/// One process store and manifest for the binary, installed once and only
/// when no other harness in the binary installed one first (see the module
/// docs); the tests that answer a round take `turn()` so their surface
/// counts do not see each other's rounds.
fn setup() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        if !koine_store::process::is_installed() {
            koine_store::process::install(Arc::new(MemoryStore::new()));
        }
        if process_manifest::current().kinds.is_empty() {
            process_manifest::install(Arc::new(manifest()));
        }
        std::hint::black_box(
            impress_surface_service::DefaultImpressSurfaceService::new as *const (),
        );
    });
}

fn turn() -> &'static tokio::sync::Mutex<()> {
    static TURN: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    TURN.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn call(name: &str, args: Value) -> Value {
    concierge::call(name, args)
        .await
        .unwrap_or_else(|e| panic!("{name} refused: {e}"))
}

/// Validate [`SPEC`] through the verb, create it under [`NAME`] as the tour
/// does, and answer its id.
async fn seed_concierge() -> String {
    let spec: Value = serde_json::from_str(SPEC).expect("concierge.surface.json is JSON");
    let validated = call(VALIDATE, json!({ "spec": spec })).await;
    assert_eq!(validated["ok"], true, "{validated}");
    let created = call(
        CREATE,
        json!({ "spec": spec, "name": NAME, "tags": [TEST_TAG] }),
    )
    .await;
    assert_eq!(created["ok"], true, "{created}");
    created["id"].as_str().expect("an id").to_string()
}

/// Type the question (a state write, as the field's `change` would leave
/// it) and press Ask (a dispatched click, whose `emit` lands in the ring).
async fn ask(id: &str, question: &str) {
    let set = call(
        STATE_SET,
        json!({
            "id": id,
            "host": HOST,
            "state": { "question": question, "transcript": [], "status": "ready" },
        }),
    )
    .await;
    assert_eq!(set["ok"], true, "{set}");
    let dispatched = call(
        DISPATCH,
        json!({
            "id": id,
            "host": HOST,
            "event": { "widget": "ask", "kind": "click", "value": null },
        }),
    )
    .await;
    assert_eq!(dispatched["ok"], true, "{dispatched}");
}

async fn state(id: &str) -> Value {
    let got = call(STATE_GET, json!({ "id": id, "host": HOST })).await;
    assert_eq!(got["ok"], true, "{got}");
    got["state"].clone()
}

async fn surfaces_named(name: &str) -> Vec<String> {
    let listed = call(LIST, json!({})).await;
    listed["surfaces"]
        .as_array()
        .expect("surfaces")
        .iter()
        .filter(|row| row["name"] == name)
        .map(|row| row["id"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The ids of every surface the concierge made (it tags them [`NAME`]).
async fn surfaces_tagged(tag: &str) -> Vec<String> {
    let listed = call(LIST, json!({})).await;
    listed["surfaces"]
        .as_array()
        .expect("surfaces")
        .iter()
        .filter(|row| {
            row["tags"]
                .as_array()
                .is_some_and(|t| t.contains(&json!(tag)))
        })
        .map(|row| row["id"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn concierge_for(id: &str) -> Concierge {
    Concierge::for_surface(
        ConciergeConfig::new(APP).with_host(HOST).with_wait_ms(500),
        id,
    )
}

/// Every `kind` key in a render tree, depth first.
fn kinds(node: &Value, out: &mut Vec<String>) {
    if let Some(kind) = node.get("kind").and_then(Value::as_str) {
        out.push(kind.to_string());
    }
    match node {
        Value::Object(map) => map.values().for_each(|v| kinds(v, out)),
        Value::Array(items) => items.iter().for_each(|v| kinds(v, out)),
        _ => {}
    }
}

#[tokio::test]
async fn every_verb_the_concierge_calls_is_in_this_binary() {
    setup();
    for name in concierge::VERBS {
        assert!(
            impress_service_core::McpToolDescriptor::iter().any(|t| t.name == name),
            "{name} is not linked"
        );
    }
}

#[tokio::test]
async fn a_sine_wave_question_makes_and_shows_a_plot() {
    setup();
    let _turn = turn().lock().await;
    let id = seed_concierge().await;
    let before = surfaces_named("Your plot").await;

    ask(&id, "a sine wave at 5 Hz").await;
    let round = concierge_for(&id).run_once().await;
    let Round::Answered(answer) = round else {
        panic!("expected an answer, got {round:?}");
    };
    assert_eq!(answer.question, "a sine wave at 5 Hz");
    let created = answer.created.clone().expect("the answer made a surface");
    assert_eq!(
        answer.shown.as_deref(),
        Some(created.as_str()),
        "the plot was not shown: {}",
        answer.reply
    );
    assert!(answer.reply.contains("5 Hz"), "{}", answer.reply);

    // A new surface titled "Your plot" exists — exactly one more.
    let after = surfaces_named("Your plot").await;
    assert_eq!(after.len(), before.len() + 1, "{after:?}");
    assert!(after.contains(&created));

    // Its render has a plot node and no placeholder (error) node.
    let rendered = call(RENDER, json!({ "id": created, "host": HOST })).await;
    assert_eq!(rendered["ok"], true, "{rendered}");
    let mut found = Vec::new();
    kinds(&rendered["tree"], &mut found);
    assert!(found.iter().any(|k| k == "plot"), "{found:?}");
    assert!(
        !found.iter().any(|k| k == "placeholder"),
        "a placeholder node: {}",
        rendered["tree"]
    );
    assert_eq!(
        rendered["source_errors"].as_array().map_or(0, Vec::len),
        0,
        "{}",
        rendered["source_errors"]
    );

    // The exchange is in the transcript; the question is cleared.
    let state = state(&id).await;
    let transcript = state["transcript"].as_array().expect("transcript");
    assert_eq!(transcript.len(), 2, "{transcript:?}");
    assert_eq!(transcript[0], json!("you: a sine wave at 5 Hz"));
    assert!(
        transcript[1]
            .as_str()
            .is_some_and(|line| line.starts_with("concierge: ")),
        "{transcript:?}"
    );
    assert_eq!(state["question"], json!(""));
    assert_eq!(state["status"], json!("answered with a plot"));
}

#[tokio::test]
async fn help_answers_in_the_transcript_and_makes_no_surface() {
    setup();
    let _turn = turn().lock().await;
    let id = seed_concierge().await;
    let before = surfaces_tagged(NAME).await;

    ask(&id, "help").await;
    let mut concierge = concierge_for(&id);
    let round = concierge.run_once().await;
    let Round::Answered(answer) = round else {
        panic!("expected an answer, got {round:?}");
    };
    assert_eq!(answer.created, None);
    assert_eq!(answer.shown, None);
    assert!(answer.reply.contains("plot"), "{}", answer.reply);

    // No surface the concierge makes appeared (the tour's own seeding, in
    // the same binary, may add its surfaces meanwhile; those are not ours).
    assert_eq!(surfaces_tagged(NAME).await, before, "help made a surface");

    let state = state(&id).await;
    let transcript = state["transcript"].as_array().expect("transcript");
    assert_eq!(transcript.len(), 2, "{transcript:?}");
    assert_eq!(transcript[0], json!("you: help"));
    assert_eq!(transcript[1], json!(format!("concierge: {}", answer.reply)));
    assert_eq!(state["question"], json!(""));

    // Nothing more to answer: the cursor moved past the ask and the
    // concierge's own state writes woke only waits that were already open,
    // so the next round times out quietly.
    assert_eq!(concierge.run_once().await, Round::Quiet);
}
