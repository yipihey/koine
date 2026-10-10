//! `reduce` never removes a state key it did not set (S1 deliverable). A small
//! hand-rolled generator over a handful of specs, states and events, rather than
//! a `proptest` dependency: `proptest` is not already a shared
//! `workspace.dependencies` entry (only `impress-layout` adds its own direct
//! copy), and the task's own instruction is to add it only if it already is.

use serde_json::{json, Value};

use impress_surface::{reduce, Event, EventKind, Node, NodeKind, SurfaceSpec};

fn spec_with_field(bind: &str, extra_state: Value) -> SurfaceSpec {
    let mut state = json!({ "freq": 1.0, "bins": 20, "name": "x" });
    if let (Value::Object(base), Value::Object(extra)) = (&mut state, extra_state) {
        base.extend(extra);
    }
    SurfaceSpec {
        surface: "1.0".to_string(),
        name: "property test".to_string(),
        params: Vec::new(),
        state,
        sources: Default::default(),
        root: Node::leaf(NodeKind::Field(impress_surface::FieldKind::Number(json!(
            {}
        ))))
        .with_id("f")
        .with_bind(bind.to_string()),
    }
}

/// A handful of (bind path, new value, extra pre-existing state) cases standing
/// in for a generator: each checks that every key present in the ORIGINAL state
/// is still present afterward (same or overwritten value), which is the whole
/// property — `reduce` clones state and only ever inserts/overwrites the one
/// path a `set`/`change` names.
fn cases() -> Vec<(&'static str, Value, Value)> {
    vec![
        ("state.freq", json!(9.0), json!({})),
        ("state.bins", json!(64), json!({"unrelated": "kept"})),
        (
            "state.nested.deep",
            json!("hi"),
            json!({"other": [1, 2, 3], "flag": true}),
        ),
        ("state.name", Value::Null, json!({"count": 0})),
        (
            "state.a.b.c",
            json!({"x": 1}),
            json!({"a": {"b": {"c": "old", "d": "kept-too"}}}),
        ),
    ]
}

#[test]
fn every_original_state_key_survives_a_change_event() {
    for (bind, new_value, extra) in cases() {
        let spec = spec_with_field(bind, extra);
        let original_keys: Vec<String> = spec.state.as_object().unwrap().keys().cloned().collect();

        let event = Event {
            widget: "f".to_string(),
            kind: EventKind::Change,
            value: new_value,
        };
        let (new_state, _effects) = reduce(&spec, &spec.state, &Value::Null, &Value::Null, &event)
            .unwrap_or_else(|e| panic!("reduce failed for bind {bind}: {e}"));

        let new_obj = new_state.as_object().expect("state stays an object");
        for key in &original_keys {
            assert!(
                new_obj.contains_key(key),
                "bind {bind}: key '{key}' was dropped from state"
            );
        }
    }
}

/// Nested writes must not drop sibling keys at the intermediate level either
/// (`state.a.b.c` must not clobber `state.a.b.d`).
#[test]
fn a_nested_set_keeps_sibling_keys_at_every_level() {
    let spec = spec_with_field(
        "state.a.b.c",
        json!({"a": {"b": {"c": "old", "d": "kept"}, "e": "also-kept"}}),
    );
    let event = Event {
        widget: "f".to_string(),
        kind: EventKind::Change,
        value: json!("new"),
    };
    let (new_state, _) = reduce(&spec, &spec.state, &Value::Null, &Value::Null, &event).unwrap();
    assert_eq!(new_state["a"]["b"]["c"], json!("new"));
    assert_eq!(new_state["a"]["b"]["d"], json!("kept"));
    assert_eq!(new_state["a"]["e"], json!("also-kept"));
}

/// A handler that runs several `set`s in sequence never drops a key any of the
/// earlier ones wrote, or any key none of them touched.
#[test]
fn a_multi_action_handler_keeps_every_key_each_action_did_not_touch() {
    let state = json!({ "x": 1, "y": 2, "z": 3 });
    let spec = SurfaceSpec {
        surface: "1.0".to_string(),
        name: "multi-set".to_string(),
        params: Vec::new(),
        state,
        sources: Default::default(),
        root: Node::leaf(NodeKind::Button(impress_surface::Button {
            label: "go".to_string(),
            on_click: vec![
                impress_surface::Action::Set {
                    path: "state.x".to_string(),
                    value: json!(100),
                },
                impress_surface::Action::Set {
                    path: "state.y".to_string(),
                    value: json!("{{state.x}}"),
                },
            ],
        }))
        .with_id("b"),
    };
    let event = Event {
        widget: "b".to_string(),
        kind: EventKind::Click,
        value: Value::Null,
    };
    let (new_state, _) = reduce(&spec, &spec.state, &Value::Null, &Value::Null, &event).unwrap();
    assert_eq!(new_state["x"], json!(100));
    // The second `set`'s value is exactly one reference (`{{state.x}}`), so it
    // resolves to the raw JSON value the first `set` just wrote, not a string —
    // and it reads it AFTER the first `set` ran.
    assert_eq!(new_state["y"], json!(100));
    assert_eq!(new_state["z"], json!(3), "untouched key must survive");
}
