//! Tier A for the tour (docs/plan-koine-tour.md T-D5): every spec validates,
//! seeding into a memory host makes exactly the tour's surfaces (and a
//! second seeding none), each renders with no placeholder node, `inventory`
//! maps the demo's verbs, and the note verbs read what they wrote.
//!
//! The host's job, done once here: a `MemoryStore` as the process store and
//! the kind manifest a koine host would build (`manifest::from_inventory`
//! maps `note` to `note`; `task` to `task@1.0.0`). The verbs then run through
//! the same pipeline MCP, the CLI and the page use.
//!
//! The process store is one per test binary, so the cases that touch it
//! take turns (`TURN`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, Once};

use impress_item::schema::refs;
use impress_pane_query::{process_manifest, KindManifest};
use impress_service_core::pipeline::{self, Call};
use impress_service_core::VerbDescriptor;
use koine_store_memory::MemoryStore;
use serde_json::{json, Value};
use surface_demo_service::{notes, tour};

static TURN: Mutex<()> = Mutex::new(());

fn host() -> std::sync::MutexGuard<'static, ()> {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // The surface verbs register themselves only in a binary that
        // references their crate (`koine_host::ensure_linked` does this for
        // a host); nothing else in this test names it.
        std::hint::black_box(
            impress_surface_service::DefaultImpressSurfaceService::new as *const (),
        );
        // tests/concierge.rs shares this binary and the process store: whichever
        // harness runs first installs it, the other keeps it.
        if !koine_store::process::is_installed() {
            koine_store::process::install(Arc::new(MemoryStore::new()));
        }
        let kinds: BTreeMap<String, Vec<String>> = [
            ("surface", refs::IMPRESS_UI_SURFACE.as_str()),
            ("surface-state", refs::IMPRESS_UI_SURFACE_STATE.as_str()),
            ("surface-event", refs::IMPRESS_UI_SURFACE_EVENT.as_str()),
            ("layout", refs::IMPRESS_UI_LAYOUT.as_str()),
            ("preset", refs::IMPRESS_UI_PRESET.as_str()),
            ("task", refs::TASK.as_str()),
            // What `koine_host::manifest::from_inventory` adds for a verb
            // with `returns_kind = "note"`: the kind, spelled as itself.
            (notes::KIND, notes::SCHEMA_REF),
        ]
        .into_iter()
        .map(|(kind, schema)| (kind.to_string(), vec![schema.to_string()]))
        .collect();
        process_manifest::install(Arc::new(KindManifest {
            kinds,
            contains_members: BTreeMap::new(),
        }));
    });
    TURN.lock().unwrap_or_else(|e| e.into_inner())
}

fn call(tool: &str, args: Value) -> Value {
    let verb = VerbDescriptor::find(tool).unwrap_or_else(|| panic!("{tool} is not linked"));
    pipeline::invoke_blocking(verb, Call::agent("tour-test", args))
        .unwrap_or_else(|e| panic!("{tool} failed in transport: {e}"))
}

/// Every `placeholder` node of a render tree, with its reason.
fn placeholders(tree: &Value) -> Vec<String> {
    fn walk(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                if map.get("kind") == Some(&json!("placeholder")) {
                    out.push(format!(
                        "{} ({})",
                        map.get("reason")
                            .and_then(Value::as_str)
                            .unwrap_or("no reason"),
                        map.get("id").and_then(Value::as_str).unwrap_or("?")
                    ));
                }
                map.values().for_each(|v| walk(v, out));
            }
            Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(tree, &mut out);
    out
}

#[test]
fn every_tour_spec_validates_with_no_problems() {
    let _turn = host();
    for (name, json) in tour::specs() {
        let spec: Value = serde_json::from_str(json).unwrap_or_else(|e| panic!("{name}: {e}"));
        let checked = call(
            "impress-surface-service_surface-validate",
            json!({ "spec": spec }),
        );
        assert_eq!(checked["ok"], json!(true), "{name}: {checked}");
        assert_eq!(
            checked["problems"],
            json!([]),
            "{name} has problems: {}",
            checked["problems"]
        );
    }
}

#[test]
fn seeding_is_idempotent_and_every_surface_renders_without_a_placeholder() {
    let _turn = host();
    let expected: BTreeSet<&str> = tour::all_specs().iter().map(|(n, _)| *n).collect();

    let first = tour::seed_blocking();
    assert!(first.ok(), "first seed: {:?}", first.errors);
    let created: BTreeSet<&str> = first.created.iter().map(|(n, _)| n.as_str()).collect();
    assert!(
        created.is_subset(&expected),
        "created {created:?} is not within the tour {expected:?}"
    );
    // Every tour surface is there now, whichever call made it.
    let all: BTreeSet<&str> = first
        .created
        .iter()
        .chain(first.existing.iter())
        .map(|(n, _)| n.as_str())
        .collect();
    assert_eq!(all, expected);
    // The six notes are there (seeded here, or by an earlier seed in this
    // binary), each under the record id the materialiser would use.
    let listed = call("surface-demo-service_notes", json!({}));
    let keys: BTreeSet<&str> = listed
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["id"].as_str())
        .collect();
    for (key, _, _, _) in tour::SEED_NOTES {
        assert!(keys.contains(key), "seed note {key} missing from {keys:?}");
    }

    let second = tour::seed_blocking();
    assert!(second.ok(), "second seed: {:?}", second.errors);
    assert!(
        second.created.is_empty(),
        "second seed created {:?}",
        second.created
    );
    assert_eq!(second.notes_seeded, 0);
    let existing: BTreeSet<&str> = second.existing.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(existing, expected);

    // Exactly the tour's surfaces carry the tag — one each.
    let list = call("impress-surface-service_surface-list", json!({}));
    let tagged: Vec<&str> = list["surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| {
            s["tags"]
                .as_array()
                .is_some_and(|t| t.contains(&json!(tour::TAG)))
        })
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert_eq!(tagged.len(), expected.len(), "{tagged:?}");
    assert_eq!(tagged.iter().copied().collect::<BTreeSet<_>>(), expected);

    for (name, id) in &second.existing {
        let rendered = call(
            "impress-surface-service_surface-render",
            json!({ "id": id, "host": "tour-test" }),
        );
        assert_eq!(rendered["ok"], json!(true), "{name}: {rendered}");
        assert_eq!(
            rendered["source_errors"],
            Value::Null,
            "{name}: a source failed: {}",
            rendered["source_errors"]
        );
        let found = placeholders(&rendered["tree"]);
        assert!(found.is_empty(), "{name} renders placeholders: {found:?}");
    }

    // The Notes tree opened unfolded: its stored `state.open` names the
    // root notes' record ids, and the render shows the children.
    let notes_id = second.id_of(tour::NOTES_NAME).unwrap();
    let rendered = call(
        "impress-surface-service_surface-render",
        json!({ "id": notes_id, "host": "tour-test" }),
    );
    let labels: Vec<String> = {
        fn walk(v: &Value, out: &mut Vec<String>) {
            match v {
                Value::Object(map) => {
                    if map.get("kind") == Some(&json!("tree")) {
                        for node in map["nodes"].as_array().unwrap_or(&Vec::new()) {
                            if let Some(label) = node["label"].as_str() {
                                out.push(label.to_string());
                            }
                        }
                    }
                    map.values().for_each(|v| walk(v, out));
                }
                Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        walk(&rendered["tree"], &mut out);
        out
    };
    for (_, _, title, _) in tour::SEED_NOTES {
        assert!(
            labels.contains(&title.to_string()),
            "{title} not drawn: {labels:?}"
        );
    }
}

#[test]
fn inventory_maps_the_demos_verbs_their_kinds_and_the_seeded_surfaces() {
    let _turn = host();
    let seeded = tour::seed_blocking();
    assert!(seeded.ok(), "{:?}", seeded.errors);
    let map = call("surface-demo-service_inventory", json!({}));
    assert!(map.get("error").is_none(), "{map}");
    let ids: BTreeSet<&str> = map["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["id"].as_str())
        .collect();
    for verb in [
        "series",
        "histogram",
        "tick",
        "notes",
        "note-add",
        "note-rename",
        "note-move",
        "note-delete",
        "inventory",
    ] {
        let id = format!("surface-demo-service_{verb}");
        assert!(ids.contains(id.as_str()), "{id} missing");
    }
    assert!(ids.contains("kind:note"));
    let edges = map["edges"].as_array().unwrap();
    let map_id = format!("surface:{}", seeded.id_of(tour::MAP_NAME).unwrap());
    assert!(
        ids.contains(map_id.as_str()),
        "the seeded Map surface is a node"
    );
    assert!(
        edges.iter().any(|e| e["source"] == json!(map_id)
            && e["target"] == json!("surface-demo-service_inventory")
            && e["relation"] == json!("calls")),
        "the Map surface calls `inventory`"
    );
    for edge in edges {
        assert!(ids.contains(edge["source"].as_str().unwrap()), "{edge}");
        assert!(ids.contains(edge["target"].as_str().unwrap()), "{edge}");
    }
}

#[test]
fn a_note_added_is_listed_then_renamed_moved_and_deleted_with_its_children() {
    let _turn = host();
    let title = format!("Tier A note {}", uuid_like());
    let added = call(
        "surface-demo-service_note-add",
        json!({ "title": title, "parent": null }),
    );
    assert!(added.get("error").is_none(), "{added}");
    let key = added["note"]["id"].as_str().unwrap().to_string();
    let record = added["note"]["record"].as_str().unwrap().to_string();
    assert_eq!(record, notes::record_id(&key).to_string());

    let row_of = |listed: &Value, key: &str| -> Option<Value> {
        listed
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == json!(key))
            .cloned()
    };
    let listed = call("surface-demo-service_notes", json!({}));
    let row = row_of(&listed, &key).expect("the added note is listed");
    assert_eq!(row["title"], json!(title));
    assert_eq!(row["parent"], Value::Null);

    // A child, by the parent's record uuid (what the tree sends).
    let child = call(
        "surface-demo-service_note-add",
        json!({ "title": "child", "parent": record }),
    );
    assert!(child.get("error").is_none(), "{child}");
    let child_key = child["note"]["id"].as_str().unwrap().to_string();
    assert_eq!(child["note"]["parent"], json!(key));

    // Rename by key, move the child to the root by its uuid, refuse a cycle.
    let renamed = call(
        "surface-demo-service_note-rename",
        json!({ "id": key, "title": "Renamed" }),
    );
    assert_eq!(renamed["note"]["title"], json!("Renamed"), "{renamed}");
    let child_record = child["note"]["record"].as_str().unwrap();
    let cycle = call(
        "surface-demo-service_note-move",
        json!({ "id": key, "parent": child_record }),
    );
    assert_eq!(cycle["error"]["code"], json!("cycle"), "{cycle}");
    let moved = call(
        "surface-demo-service_note-move",
        json!({ "id": child_record, "parent": null }),
    );
    assert_eq!(moved["note"]["parent"], Value::Null, "{moved}");
    let back = call(
        "surface-demo-service_note-move",
        json!({ "id": child_key, "parent": key }),
    );
    assert_eq!(back["note"]["parent"], json!(key), "{back}");

    // Delete the parent: the child goes with it.
    let deleted = call("surface-demo-service_note-delete", json!({ "id": key }));
    assert_eq!(deleted["deleted"], json!([key, child_key]), "{deleted}");
    let listed = call("surface-demo-service_notes", json!({}));
    assert!(row_of(&listed, &key).is_none());
    assert!(row_of(&listed, &child_key).is_none());
    let again = call("surface-demo-service_note-delete", json!({ "id": key }));
    assert_eq!(again["error"]["code"], json!("not-found"), "{again}");

    // An empty title is refused before anything is written.
    let empty = call(
        "surface-demo-service_note-add",
        json!({ "title": "   ", "parent": null }),
    );
    assert_eq!(empty["error"]["code"], json!("invalid-argument"), "{empty}");
}

fn uuid_like() -> String {
    notes::fresh_key()
}

/// `notes` declares `returns_kind = "note"` with `rows(id = "id", title =
/// "title", columns = […])`; the descriptor's own check says those are
/// fields of the row object the output schema describes (what a host's
/// Tier A report asserts for every linked verb).
#[test]
fn the_notes_rows_declaration_matches_its_output_schema() {
    let verb = VerbDescriptor::find("surface-demo-service_notes").unwrap();
    assert_eq!(verb.returns_kind, Some(notes::KIND));
    assert_eq!(verb.row_schema_check(), Ok(()));
    let rows = verb.rows.expect("rows(…) declared");
    assert_eq!(rows.id, "id");
}

/// The stored row of the tour surface named `name`: `(id, tags, spec)`.
fn stored(name: &str) -> (String, Vec<String>, Value) {
    let list = call("impress-surface-service_surface-list", json!({}));
    let row = list["surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| {
            s["name"] == json!(name)
                && s["tags"]
                    .as_array()
                    .is_some_and(|t| t.contains(&json!(tour::TAG)))
        })
        .unwrap_or_else(|| panic!("no tour surface {name}"))
        .clone();
    let id = row["id"].as_str().unwrap().to_string();
    let got = call("impress-surface-service_surface-get", json!({ "id": id }));
    let tags = row["tags"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t.as_str().map(str::to_string))
        .collect();
    (id, tags, got["spec"].clone())
}

/// Overwrite a surface row's `tags` the way a 0.3.0 seed left them.
fn set_tags(id: &str, tags: &[&str]) {
    use impress_item::{FieldMutation, Value as ItemValue};
    let (store, _) = koine_store::process::acquire_or_stand_in();
    store
        .update(
            id.parse().unwrap(),
            vec![FieldMutation::SetPayload(
                "tags".into(),
                ItemValue::Array(
                    tags.iter()
                        .map(|t| ItemValue::String(t.to_string()))
                        .collect(),
                ),
            )],
        )
        .unwrap();
}

/// A-D8: a stored tour surface whose spec differs from the file's (here, an
/// older text and no content tag, as 0.3.0 left it) is replaced in place
/// once, keeping its id; the next seed finds it current. A surface without
/// the tour tag is never touched.
#[test]
fn seeding_replaces_a_changed_spec_once_then_is_stable() {
    let _turn = host();
    let first = tour::seed_blocking();
    assert!(first.ok(), "{:?}", first.errors);
    let settled = tour::seed_blocking();
    assert!(settled.ok(), "{:?}", settled.errors);
    assert!(
        settled.updated.is_empty(),
        "same spec: {:?}",
        settled.updated
    );
    assert!(settled.created.is_empty());

    let name = tour::SIGNAL_EXPLORER_NAME;
    let (id, tags, spec) = stored(name);
    let current =
        tour::spec_tag(tour::specs().iter().find(|(n, _)| *n == name).unwrap().1).unwrap();
    assert!(tags.contains(&current), "{tags:?} carries {current}");

    // Last release's spec: another description, and only the `tour` tag.
    let mut old = spec.clone();
    old["root"]["column"][0]["text"] = json!("An older description of this surface.");
    let replaced = call(
        "impress-surface-service_surface-update",
        json!({ "id": id, "spec": old }),
    );
    assert_eq!(replaced["ok"], json!(true), "{replaced}");
    set_tags(&id, &[tour::TAG]);
    // Someone's own surface with the same name: no tour tag, never touched.
    let mine = call(
        "impress-surface-service_surface-create",
        json!({ "spec": old, "name": name }),
    );
    assert_eq!(mine["ok"], json!(true), "{mine}");
    let mine_id = mine["id"].as_str().unwrap().to_string();

    let again = tour::seed_blocking();
    assert!(again.ok(), "{:?}", again.errors);
    assert_eq!(again.updated, vec![(name.to_string(), id.clone())]);
    assert!(again.created.is_empty(), "{:?}", again.created);
    let (same_id, tags, spec) = stored(name);
    assert_eq!(same_id, id, "replaced in place");
    assert_eq!(
        tags,
        vec![tour::TAG.to_string(), current.clone()],
        "the content tag is refreshed"
    );
    assert_ne!(
        spec["root"]["column"][0]["text"],
        json!("An older description of this surface."),
        "the file's spec is back"
    );
    let untouched = call(
        "impress-surface-service_surface-get",
        json!({ "id": mine_id }),
    );
    assert_eq!(
        untouched["spec"]["root"]["column"][0]["text"],
        json!("An older description of this surface.")
    );
    assert_eq!(untouched["revision"], mine["revision"]);

    let stable = tour::seed_blocking();
    assert!(stable.ok(), "{:?}", stable.errors);
    assert!(
        stable.updated.is_empty(),
        "then stable: {:?}",
        stable.updated
    );
    call(
        "impress-surface-service_surface-delete",
        json!({ "id": mine_id }),
    );
}

/// A-D9: a body is written and read back; a note added with a note as its
/// parent lands beside it, in the same collection.
#[test]
fn a_note_has_a_body_and_a_note_added_under_a_note_lands_beside_it() {
    let _turn = host();
    let collection = call(
        "surface-demo-service_note-add",
        json!({ "title": format!("Collection {}", uuid_like()), "parent": null }),
    );
    let collection_key = collection["note"]["id"].as_str().unwrap().to_string();
    assert_eq!(collection["note"]["body"], json!(""));
    let note = call(
        "surface-demo-service_note-add",
        json!({ "title": "A note", "parent": collection_key }),
    );
    let note_record = note["note"]["record"].as_str().unwrap().to_string();
    assert_eq!(note["note"]["parent"], json!(collection_key));

    let written = call(
        "surface-demo-service_note-write",
        json!({ "id": note_record, "body": "First line.\nSecond line." }),
    );
    assert_eq!(
        written["note"]["body"],
        json!("First line.\nSecond line."),
        "{written}"
    );
    let got = call(
        "surface-demo-service_note-get",
        json!({ "id": note_record }),
    );
    assert_eq!(
        got["note"]["body"],
        json!("First line.\nSecond line."),
        "{got}"
    );
    assert_eq!(got["note"]["title"], json!("A note"));
    let listed = call("surface-demo-service_notes", json!({}));
    assert!(listed
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["record"] == json!(note_record)
            && n["body"] == json!("First line.\nSecond line.")));

    let beside = call(
        "surface-demo-service_note-add",
        json!({ "title": "Beside", "parent": note_record }),
    );
    assert_eq!(beside["note"]["parent"], json!(collection_key), "{beside}");

    let missing = call(
        "surface-demo-service_note-write",
        json!({ "id": "no-such-note", "body": "x" }),
    );
    assert_eq!(missing["error"]["code"], json!("not-found"), "{missing}");
    let missing = call(
        "surface-demo-service_note-get",
        json!({ "id": "no-such-note" }),
    );
    assert_eq!(missing["error"]["code"], json!("not-found"), "{missing}");

    call(
        "surface-demo-service_note-delete",
        json!({ "id": collection_key }),
    );
}

/// The Notes surface drives the verbs: selecting a row loads its body into
/// the editor, Save writes it, and the render shows it.
#[test]
fn the_notes_surface_selects_writes_and_shows_a_body() {
    let _turn = host();
    let seeded = tour::seed_blocking();
    assert!(seeded.ok(), "{:?}", seeded.errors);
    let id = seeded.id_of(tour::NOTES_NAME).unwrap().to_string();
    let host_name = format!("notes-{}", uuid_like());
    let ideas = notes::record_id("ideas").to_string();
    let dispatch = |widget: &str, kind: &str, value: Value| {
        let answer = call(
            "impress-surface-service_surface-dispatch",
            json!({ "id": id, "host": host_name,
                    "event": { "widget": widget, "kind": kind, "value": value } }),
        );
        assert_eq!(answer["ok"], json!(true), "{widget} {kind}: {answer}");
        answer
    };
    let state = |path: &str| {
        let got = call(
            "impress-surface-service_surface-state-get",
            json!({ "id": id, "host": host_name }),
        );
        got["state"].pointer(path).cloned().unwrap_or(Value::Null)
    };
    dispatch("outline", "select", json!([ideas]));
    assert_eq!(state("/selected"), json!(ideas));
    let seeded_body = tour::SEED_NOTES
        .iter()
        .find(|(k, _, _, _)| *k == "ideas")
        .unwrap()
        .3;
    assert_eq!(state("/body"), json!(seeded_body));

    let text = format!("Rewritten {}", uuid_like());
    dispatch("body", "change", json!(text));
    dispatch("save-btn", "click", Value::Null);
    let got = call("surface-demo-service_note-get", json!({ "id": "ideas" }));
    assert_eq!(got["note"]["body"], json!(text));
    let rendered = call(
        "impress-surface-service_surface-render",
        json!({ "id": id, "host": host_name }),
    );
    assert!(
        rendered["tree"].to_string().contains(&text),
        "the editor shows the written body"
    );
    // Put the seeded body back for the other cases.
    call(
        "surface-demo-service_note-write",
        json!({ "id": "ideas", "body": seeded_body }),
    );
}

/// A-D4: the demo's layout shows three surface tiles side by side, once.
#[test]
fn seeding_the_layout_shows_three_surfaces_once() {
    let _turn = host();
    let app = format!("tour-layout-{}", uuid_like());
    let first = tour::seed_with_layout_blocking(&app, Some("tour-test"));
    assert!(first.ok(), "{:?}", first.errors);
    let names: Vec<&str> = first.layout.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, tour::LAYOUT.map(|(n, _)| n).to_vec());

    let tree = call(
        "layout-service_get-layout",
        json!({ "app_id": app, "device": "tour-test" }),
    );
    let tiles = tree["layout"]["tiles"].as_object().unwrap();
    let surfaces: BTreeSet<String> = tiles
        .values()
        .filter(|t| t["pane"]["view_kind"] == json!("surface"))
        .filter_map(|t| t["pane"]["query"]["scope"]["id"]["id"].as_str())
        .map(str::to_string)
        .collect();
    let wanted: BTreeSet<String> = tour::LAYOUT
        .iter()
        .map(|(n, _)| first.id_of(n).unwrap().to_string())
        .collect();
    assert_eq!(surfaces, wanted);
    let split = tiles
        .values()
        .find_map(|t| t["container"]["linear"].as_object())
        .expect("a split");
    assert_eq!(split["dir"], json!("horizontal"));
    assert_eq!(split["shares"], json!([1.0, 1.0, 1.0]));

    let again = tour::seed_with_layout_blocking(&app, Some("tour-test"));
    assert!(again.ok(), "{:?}", again.errors);
    assert!(again.layout.is_empty(), "once: {:?}", again.layout);
}
