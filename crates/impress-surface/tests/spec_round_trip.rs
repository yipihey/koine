//! Serde round-trip of every node, source and action kind the vocabulary lists
//! (`docs/plan-agent-surfaces.md` "The vocabulary (normative)"), each written by
//! hand in the plan's own JSON shape: deserialize, re-serialize, compare as
//! `serde_json::Value` (never as text — key order is never significant).

use impress_surface::{Action, Node, Source, SurfaceSpec};
use serde_json::{json, Value};

fn round_trip_node(raw: Value) {
    let node: Node = serde_json::from_value(raw.clone())
        .unwrap_or_else(|e| panic!("failed to parse {raw}: {e}"));
    let back = serde_json::to_value(&node).unwrap();
    assert_eq!(back, raw, "node did not round-trip");
}

fn round_trip_source(raw: Value) {
    let source: Source = serde_json::from_value(raw.clone())
        .unwrap_or_else(|e| panic!("failed to parse {raw}: {e}"));
    let back = serde_json::to_value(&source).unwrap();
    assert_eq!(back, raw, "source did not round-trip");
}

fn round_trip_action(raw: Value) {
    let action: Action = serde_json::from_value(raw.clone())
        .unwrap_or_else(|e| panic!("failed to parse {raw}: {e}"));
    let back = serde_json::to_value(&action).unwrap();
    assert_eq!(back, raw, "action did not round-trip");
}

// ── Containers ──────────────────────────────────────────────────────────────

#[test]
fn column_round_trips() {
    round_trip_node(json!({ "column": [ { "text": "a" }, { "text": "b" } ] }));
}

#[test]
fn row_round_trips() {
    round_trip_node(json!({ "row": [ { "text": "a" } ] }));
}

#[test]
fn grid_round_trips() {
    round_trip_node(
        json!({ "grid": { "columns": 2, "items": [ { "text": "a" }, { "text": "b" } ] } }),
    );
}

#[test]
fn section_round_trips() {
    round_trip_node(json!({
        "section": { "title": "Details", "collapsed": true, "body": { "text": "hi" } }
    }));
}

#[test]
fn tabs_round_trips() {
    round_trip_node(json!({
        "tabs": [
            { "title": "One", "body": { "text": "a" } },
            { "title": "Two", "body": { "text": "b" } }
        ]
    }));
}

// ── Widgets ──────────────────────────────────────────────────────────────────

#[test]
fn text_round_trips() {
    round_trip_node(json!({ "text": "# Signal explorer" }));
}

#[test]
fn table_round_trips() {
    round_trip_node(json!({
        "table": {
            "rows": "{{source.papers}}",
            "columns": ["title", "year"],
            "on_select": [ { "publish": {} } ]
        }
    }));
}

#[test]
fn list_round_trips() {
    round_trip_node(json!({
        "list": { "rows": "{{source.papers}}", "on_select": [ { "publish": {} } ] }
    }));
}

#[test]
fn plot_round_trips() {
    round_trip_node(json!({ "plot": { "spec": "{{source.hist.plot}}" } }));
}

#[test]
fn image_round_trips_blob_and_url() {
    round_trip_node(json!({ "image": { "blob": "{{source.thumb}}" } }));
    round_trip_node(json!({ "image": { "url": "https://example.org/x.png" } }));
}

#[test]
fn field_round_trips_every_kind() {
    round_trip_node(json!({ "field": { "text": {} }, "label": "Name", "bind": "state.name" }));
    round_trip_node(json!({ "field": { "number": { "min": 0, "max": 10 } }, "bind": "state.n" }));
    round_trip_node(json!({
        "field": { "slider": { "min": 0.5, "max": 8, "step": 0.5 } },
        "label": "Frequency",
        "bind": "state.freq"
    }));
    round_trip_node(json!({
        "field": { "select": { "options": ["a", "b"] } },
        "bind": "state.choice"
    }));
    round_trip_node(json!({ "field": { "toggle": {} }, "bind": "state.on" }));
    round_trip_node(json!({ "field": { "date": {} }, "bind": "state.when" }));
}

#[test]
fn button_round_trips() {
    round_trip_node(json!({
        "button": {
            "label": "Use these bins",
            "on_click": [ { "emit": { "name": "bins-chosen", "payload": { "bins": "{{state.bins}}" } } } ]
        }
    }));
}

#[test]
fn status_round_trips() {
    round_trip_node(json!({ "status": { "level": "warning", "message": "low disk space" } }));
}

#[test]
fn log_round_trips() {
    round_trip_node(json!({ "log": { "lines": ["a", "b"] } }));
}

#[test]
fn kv_round_trips() {
    round_trip_node(json!({ "kv": { "pairs": { "x": 1, "y": 2 } } }));
}

#[test]
fn divider_round_trips() {
    round_trip_node(json!({ "divider": {} }));
}

#[test]
fn spacer_round_trips() {
    round_trip_node(json!({ "spacer": {} }));
}

#[test]
fn common_keys_sit_alongside_the_kind_tag() {
    round_trip_node(json!({
        "id": "my-field",
        "label": "Frequency",
        "help": "Hz",
        "when": { "path": "state.advanced" },
        "bind": "state.freq",
        "on_change": [ { "set": { "path": "state.touched", "value": true } } ],
        "field": { "slider": { "min": 0.5, "max": 8, "step": 0.5 } }
    }));
}

/// The ADR-0033 default this crate implements on top of the plan: a tag key
/// this build does not recognize is kept, not rejected.
#[test]
fn an_unrecognized_kind_key_still_round_trips() {
    round_trip_node(json!({ "sparkline": { "values": [1, 2, 3] } }));
}

// ── Sources ──────────────────────────────────────────────────────────────────

#[test]
fn value_source_round_trips() {
    round_trip_source(json!({ "value": 42 }));
}

#[test]
fn verb_source_round_trips() {
    round_trip_source(json!({
        "verb": "surface-demo-service_series",
        "args": { "freq": "{{state.freq}}", "n": 512 }
    }));
}

#[test]
fn query_source_round_trips() {
    round_trip_source(json!({
        "query": {
            "kinds": ["publication"],
            "scope": { "scope": "all" },
            "filters": [],
            "text": null,
            "relation": null,
            "sort": [],
            "limit": null
        }
    }));
}

// ── Actions ──────────────────────────────────────────────────────────────────

#[test]
fn set_action_round_trips() {
    round_trip_action(json!({ "set": { "path": "state.bins", "value": 32 } }));
}

#[test]
fn call_action_round_trips() {
    round_trip_action(json!({
        "call": { "verb": "imbib-service_search", "args": { "q": "dark matter" }, "into": "state.results" }
    }));
}

#[test]
fn publish_action_round_trips_with_and_without_ids() {
    round_trip_action(json!({ "publish": {} }));
    round_trip_action(json!({ "publish": { "ids": "state.selected" } }));
}

#[test]
fn emit_action_round_trips() {
    round_trip_action(
        json!({ "emit": { "name": "bins-chosen", "payload": { "bins": "{{state.bins}}" } } }),
    );
}

#[test]
fn open_action_round_trips() {
    round_trip_action(json!({
        "open": { "query": { "kinds": ["publication"] }, "view_kind": "list", "target": "detail" }
    }));
}

#[test]
fn refresh_action_round_trips() {
    round_trip_action(json!({ "refresh": { "source": "series" } }));
}

// ── The whole spec ───────────────────────────────────────────────────────────

#[test]
fn the_whole_worked_example_round_trips_as_json() {
    let raw: Value =
        serde_json::from_str(include_str!("../examples/signal-explorer.surface.json")).unwrap();
    let spec: SurfaceSpec = serde_json::from_value(raw.clone()).unwrap();
    let back = serde_json::to_value(&spec).unwrap();
    assert_eq!(back, raw);
}
