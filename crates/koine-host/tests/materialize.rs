//! Rows become records: the host's materialiser over a memory store, driven
//! through the pipeline's one call site (`materialize::after_verb`).

use std::sync::{Arc, Mutex};

use impress_item::{
    AttributedStore, FieldMutation, Item, ItemQuery, SchemaRef, Value as ItemValue,
};
use impress_service_core::pipeline::materialize;
use impress_service_core::{
    inventory, Effects, McpToolDescriptor, RowSchema, Safety, SafetyClass, ServiceFuture, Source,
    VerbDescriptor,
};
use koine_host::manifest;
use koine_host::materialize::StoreMaterializer;
use serde_json::{json, Value};

/// A verb that declares `returns_kind = "paper"` and `rows(id = "bibcode")`.
static PAPERS: VerbDescriptor = VerbDescriptor {
    name: "rows-fixture_papers",
    service: "rows-fixture",
    method: "papers",
    description: "Test fixture: answers papers.",
    input_schema: schema,
    output_schema: schema,
    safety: Safety {
        class: SafetyClass::ReadOnly,
        idempotent: true,
    },
    since: "0.1.0",
    effects: Effects::NONE,
    deprecated: None,
    aliases: &[],
    supports: &[],
    adjective: None,
    examples: &[],
    strict: false,
    budget_ms: None,
    replay_full: false,
    returns_kind: Some("paper"),
    rows: Some(RowSchema {
        id: "bibcode",
        title: Some("title"),
        columns: &[],
    }),
    secrets: &[],
    source: Source::Linked,
    handler: papers,
    consent: ::impress_service_core::Consent::DEFAULT,
};
inventory::submit! { McpToolDescriptor::of(&PAPERS) }

/// A verb that returns a kind but declares no `rows(…)`: nothing to key on.
static UNKEYED: VerbDescriptor = VerbDescriptor {
    name: "rows-fixture_unkeyed",
    method: "unkeyed",
    returns_kind: Some("unkeyed-kind"),
    rows: None,
    ..PAPERS
};
inventory::submit! { McpToolDescriptor::of(&UNKEYED) }

fn schema() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": { "bibcode": { "type": "string" }, "title": { "type": "string" } }
        }
    })
}

/// Two rows, so a call through the host (below) has something to keep.
fn papers(_: Value) -> ServiceFuture {
    Box::pin(async { Ok(json!([{ "bibcode": "A", "title": "t1" }, { "bibcode": "B" }])) })
}

// The materialiser slot is process-wide: the cases take turns.
static TURN: Mutex<()> = Mutex::new(());

fn paper_items(store: &dyn AttributedStore) -> Vec<Item> {
    let mut items = store
        .query(&ItemQuery {
            schema: Some(SchemaRef::from_stored("paper".into())),
            ..ItemQuery::default()
        })
        .unwrap();
    items.sort_by_key(|i| i.id);
    items
}

fn title_of(item: &Item) -> Option<&ItemValue> {
    item.payload.get("title")
}

fn open() -> (
    Arc<dyn AttributedStore>,
    Arc<impress_pane_query::KindManifest>,
) {
    let store: Arc<dyn AttributedStore> = Arc::from(koine_store::open("memory:").unwrap());
    (store, Arc::new(manifest::from_inventory()))
}

#[test]
fn the_inventory_manifest_knows_the_kinds_the_verbs_return() {
    let manifest = manifest::from_inventory();
    assert_eq!(
        manifest.schema_refs("paper"),
        Some(&["paper".to_string()][..])
    );
    assert!(manifest.schema_refs("unkeyed-kind").is_some());
    // The ui kinds are still there, with their canonical refs.
    assert_eq!(
        manifest.schema_refs("surface"),
        Some(&["impress/ui/surface@1.0.0".to_string()][..])
    );
}

#[test]
fn rows_become_records_and_a_second_run_updates_only_what_changed() {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let (store, manifest) = open();
    koine_host::materialize::install(store.clone(), manifest);

    materialize::after_verb(
        &PAPERS,
        &json!([{ "bibcode": "A", "title": "t1", "year": 2020 }, { "bibcode": "B" }]),
    );
    let first = paper_items(&*store);
    assert_eq!(first.len(), 2);
    let a_id = StoreMaterializer::record_id("paper", "A");
    let a = first.iter().find(|i| i.id == a_id).expect("A's record id");
    assert_eq!(title_of(a), Some(&ItemValue::String("t1".into())));
    assert_eq!(a.payload.get("year"), Some(&ItemValue::Int(2020)));
    assert_eq!(
        a.payload.get("_verb"),
        Some(&ItemValue::String("rows-fixture_papers".into()))
    );
    assert_eq!(a.author, "system:host");
    assert_eq!(a.schema.as_str(), "paper");
    let b_id = StoreMaterializer::record_id("paper", "B");
    assert!(first.iter().any(|i| i.id == b_id));

    // A tag put on A survives the next refresh.
    store
        .update(a_id, vec![FieldMutation::AddTag("keep".into())])
        .unwrap();
    let clock_a = store.get(a_id).unwrap().unwrap().logical_clock;
    let clock_b = store.get(b_id).unwrap().unwrap().logical_clock;

    // The same rows again: no write at all.
    materialize::after_verb(
        &PAPERS,
        &json!([{ "bibcode": "A", "title": "t1", "year": 2020 }, { "bibcode": "B" }]),
    );
    assert_eq!(store.get(a_id).unwrap().unwrap().logical_clock, clock_a);
    assert_eq!(store.get(b_id).unwrap().unwrap().logical_clock, clock_b);

    // A changed title: one update, the same ids, the tag still there.
    materialize::after_verb(
        &PAPERS,
        &json!({ "rows": [{ "bibcode": "A", "title": "t2", "year": 2020 }, { "bibcode": "B" }], "next": null }),
    );
    let second = paper_items(&*store);
    assert_eq!(second.len(), 2, "no duplicate records");
    let a2 = store.get(a_id).unwrap().unwrap();
    assert_eq!(title_of(&a2), Some(&ItemValue::String("t2".into())));
    assert!(a2.logical_clock > clock_a, "A was written");
    assert!(a2.tags.contains(&"keep".to_string()), "{:?}", a2.tags);
    assert_eq!(
        store.get(b_id).unwrap().unwrap().logical_clock,
        clock_b,
        "B did not change, so B was not written"
    );

    // A row the verb no longer answers is not deleted.
    materialize::after_verb(&PAPERS, &json!([{ "bibcode": "A", "title": "t2" }]));
    assert_eq!(paper_items(&*store).len(), 2);
    materialize::clear();
}

#[test]
fn the_manifest_finds_the_records_a_pane_would_query() {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let (store, manifest) = open();
    koine_host::materialize::install(store.clone(), manifest.clone());
    materialize::after_verb(&PAPERS, &json!([{ "bibcode": "A" }, { "bibcode": "B" }]));
    // What a pane over kind `paper` asks the store: the manifest's refs.
    let refs = manifest.schema_refs("paper").unwrap();
    let found: usize = refs
        .iter()
        .map(|r| {
            store
                .count(&ItemQuery {
                    schema: Some(SchemaRef::from_stored(r.clone())),
                    ..ItemQuery::default()
                })
                .unwrap()
        })
        .sum();
    assert_eq!(found, 2);
    materialize::clear();
}

#[test]
fn rows_without_an_id_or_a_refusal_or_no_rows_declaration_write_nothing() {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let (store, manifest) = open();
    koine_host::materialize::install(store.clone(), manifest);

    // Rows without the id field are skipped; the one with it is kept.
    materialize::after_verb(
        &PAPERS,
        &json!([{ "title": "anonymous" }, { "bibcode": "C" }]),
    );
    assert_eq!(paper_items(&*store).len(), 1);
    // A refused result and a verb with no `rows(…)` write nothing.
    materialize::after_verb(
        &PAPERS,
        &json!({ "ok": false, "rows": [{ "bibcode": "D" }] }),
    );
    materialize::after_verb(&UNKEYED, &json!([{ "bibcode": "E" }]));
    assert_eq!(paper_items(&*store).len(), 1);

    // Without a materialiser installed, nothing happens at all.
    materialize::clear();
    materialize::after_verb(&PAPERS, &json!([{ "bibcode": "F" }]));
    assert_eq!(paper_items(&*store).len(), 1);
}

/// The whole path: a host serves, a verb is called over HTTP, and its rows
/// are records of the host's store afterwards (the pipeline's call site,
/// the host's install at `serve`, the inventory manifest).
#[test]
fn a_verb_called_over_http_leaves_its_rows_in_the_hosts_store() {
    // The turn is taken outside the async body: a std guard must not be held
    // across an await, and the runtime below is this test's alone.
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(call_over_http());
}

async fn call_over_http() {
    koine_host::ensure_linked();
    let workspace = tempfile::tempdir().unwrap();
    let served = koine_host::serve(koine_host::Config::new("memory:", workspace.path()))
        .await
        .expect("the host starts");
    let client = reqwest::Client::new();
    let answer = client
        .post(format!("{}/api/verb/rows-fixture_papers", served.url()))
        .header("Authorization", format!("Bearer {}", served.token()))
        .json(&json!({}))
        .send()
        .await
        .expect("the call reaches the host");
    assert_eq!(answer.status(), 200);

    let (store, _) = koine_store::process::acquire().expect("serve installed the store");
    let rows = paper_items(&*store);
    assert_eq!(rows.len(), 2, "both rows of the fixture are records");
    assert!(rows.iter().all(|r| r.schema.as_str() == "paper"));
    served.shutdown().await;
}
