//! plan-koine-tree-stream T1: the `tree` node in the grammar.

use impress_surface::{reduce, resolve, validate, Event, EventKind, RenderKind, SurfaceSpec};
use serde_json::{json, Value};

fn spec(state: Value, tree: Value) -> SurfaceSpec {
    serde_json::from_value(json!({
        "surface": "1.0", "name": "tree", "state": state,
        "root": {"id": "files", "tree": tree}
    }))
    .expect("a spec")
}

fn state() -> Value {
    json!({
        "open": ["a"],
        "rows": [
            {"id": "a", "label": "src"},
            {"id": "b", "parent": "a", "label": "main.rs"},
            {"id": "c", "label": "docs"},
        ]
    })
}

fn tree_spec() -> SurfaceSpec {
    spec(
        state(),
        json!({
            "rows": "{{state.rows}}", "bind": "state.open",
            "on_select": [{"set": {"path": "state.picked", "value": "{{event.value}}"}}]
        }),
    )
}

fn errors_of(spec: &SurfaceSpec) -> Vec<String> {
    validate(spec)
        .into_iter()
        .filter(|p| p.is_error())
        .map(|p| format!("{}: {}", p.path, p.message))
        .collect()
}

#[test]
fn a_tree_round_trips_and_resolves_to_the_visible_outline() {
    let s = tree_spec();
    let back: SurfaceSpec = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
    assert_eq!(back, s);
    let tree = resolve(&s, &s.state, &Value::Null, &Value::Null);
    match &tree.root.node {
        RenderKind::Tree {
            nodes,
            bind,
            expanded,
            actions,
        } => {
            assert_eq!(bind.as_deref(), Some("state.open"));
            assert_eq!(expanded, &vec![json!("a")]);
            assert!(actions.is_empty());
            let shown: Vec<(&str, u32, bool, bool)> = nodes
                .iter()
                .map(|n| (n.label.as_str(), n.depth, n.has_children, n.expanded))
                .collect();
            assert_eq!(
                shown,
                vec![
                    ("src", 0, true, true),
                    ("main.rs", 1, false, false),
                    ("docs", 0, false, false)
                ]
            );
            assert_eq!(nodes[1].id, json!("b"));
        }
        other => panic!("expected a tree, got {other:?}"),
    }
    assert_eq!(tree.focus_order, vec!["files".to_string()]);
}

#[test]
fn a_missing_expanded_list_is_all_collapsed_and_an_envelope_is_read() {
    let s = spec(
        json!({"result": {"rows": state()["rows"].clone(), "next": null}}),
        json!({"rows": "{{state.result}}", "bind": "state.nowhere"}),
    );
    let tree = resolve(&s, &s.state, &Value::Null, &Value::Null);
    match &tree.root.node {
        RenderKind::Tree { nodes, .. } => assert_eq!(nodes.len(), 2),
        other => panic!("expected a tree, got {other:?}"),
    }
}

#[test]
fn the_wire_shape_is_pinned() {
    let s = tree_spec();
    let tree = resolve(&s, &s.state, &Value::Null, &Value::Null);
    let wire = serde_json::to_value(&tree.root.node).unwrap();
    assert_eq!(wire["kind"], "tree");
    assert_eq!(wire["bind"], "state.open");
    assert_eq!(
        wire["nodes"][0],
        json!({
            "id": "a", "label": "src", "depth": 0, "has_children": true,
            "expanded": true, "row": {"id": "a", "label": "src"}
        })
    );
}

#[test]
fn validate_accepts_a_tree_and_checks_bind_and_field_names() {
    assert_eq!(errors_of(&tree_spec()), Vec::<String>::new());
    let bad = spec(
        state(),
        json!({"rows": "{{state.rows}}", "bind": "open", "id_field": "",
               "parent_field": "{{state.x}}", "label_field": "name"}),
    );
    let errors = errors_of(&bad);
    assert!(
        errors.iter().any(|e| e.contains("/tree/bind")),
        "{errors:?}"
    );
    assert!(
        errors.iter().any(|e| e.contains("/tree/id_field")),
        "{errors:?}"
    );
    assert!(
        errors.iter().any(|e| e.contains("/tree/parent_field")),
        "{errors:?}"
    );
    assert!(
        !errors.iter().any(|e| e.contains("/tree/label_field")),
        "{errors:?}"
    );
}

#[test]
fn validate_checks_a_trees_on_select_actions_and_wants_an_id() {
    let s = spec(
        state(),
        json!({"rows": "{{state.rows}}", "on_select": [{"set": {"path": "picked", "value": 1}}]}),
    );
    let problems = validate(&s);
    assert!(
        problems
            .iter()
            .any(|p| p.is_error() && p.path.contains("/tree/on_select/0")),
        "{problems:?}"
    );
    let mut s = tree_spec();
    s.root.id = None;
    assert!(validate(&s).iter().any(|p| p.message.contains("no `id`")));
}

fn event(kind: EventKind, value: Value) -> Event {
    Event {
        widget: "files".to_string(),
        kind,
        value,
    }
}

#[test]
fn a_change_writes_the_new_expanded_list_to_the_bind_path() {
    let s = tree_spec();
    let (new_state, effects) = reduce(
        &s,
        &s.state,
        &Value::Null,
        &Value::Null,
        &event(EventKind::Change, json!(["a", "c"])),
    )
    .unwrap();
    assert_eq!(new_state["open"], json!(["a", "c"]));
    assert!(effects.is_empty());
    // And the next resolve shows it.
    let tree = resolve(&s, &new_state, &Value::Null, &Value::Null);
    match &tree.root.node {
        RenderKind::Tree { nodes, .. } => {
            assert!(nodes[2].label == "docs" && !nodes[2].has_children)
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_change_on_a_tree_without_bind_is_refused() {
    let s = spec(state(), json!({"rows": "{{state.rows}}"}));
    let err = reduce(
        &s,
        &s.state,
        &Value::Null,
        &Value::Null,
        &event(EventKind::Change, json!([])),
    )
    .unwrap_err();
    assert_eq!(err.code(), "not-bindable");
}

#[test]
fn a_select_runs_on_select_with_the_id_list() {
    let s = tree_spec();
    let (new_state, _) = reduce(
        &s,
        &s.state,
        &Value::Null,
        &Value::Null,
        &event(EventKind::Select, json!(["b"])),
    )
    .unwrap();
    assert_eq!(new_state["picked"], json!(["b"]));
}
