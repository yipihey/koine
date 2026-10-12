//! plan-koine-outline Q1a: what makes a `tree` a sidebar. The full expanded
//! list (O-D2), the row events rename/delete/activate/move (O-D3, O-D4), the
//! row menu (O-D5) and the count (O-D6), each through the spec, the
//! validator, the resolver and the reducer.

use impress_surface::{
    reduce, resolve, validate, validate_json, Event, EventKind, ReduceError, RenderKind,
    SurfaceSpec,
};
use serde_json::{json, Value};

fn rows() -> Value {
    json!([
        {"id": "a", "label": "src", "n": 3},
        {"id": "b", "parent": "a", "label": "lib", "n": 0},
        {"id": "c", "parent": "b", "label": "mod.rs"},
        {"id": "d", "label": "docs", "n": null},
    ])
}

fn spec_of(root: Value) -> SurfaceSpec {
    serde_json::from_value(json!({
        "surface": "1.0", "name": "outline",
        "state": {"open": ["a", "b"], "rows": rows()},
        "root": root
    }))
    .expect("a spec")
}

fn handler(to: &str) -> Value {
    json!([{"set": {"path": format!("state.{to}"), "value": "{{event.value}}"}}])
}

/// A tree with every handler, a two-entry menu and a count.
fn sidebar() -> SurfaceSpec {
    spec_of(json!({
        "id": "side",
        "tree": {
            "rows": "{{state.rows}}", "bind": "state.open", "count_field": "n",
            "on_select": handler("picked"),
            "on_rename": handler("renamed"),
            "on_delete": handler("deleted"),
            "on_activate": handler("opened"),
            "on_move": handler("moved"),
            "actions": [
                {"label": "Rename", "on_click": handler("menu_rename")},
                {"label": "Delete", "on_click": handler("menu_delete")}
            ]
        }
    }))
}

fn errors_of(spec: &SurfaceSpec) -> Vec<String> {
    validate(spec)
        .into_iter()
        .filter(|p| p.is_error())
        .map(|p| format!("{}: {}", p.path, p.message))
        .collect()
}

fn send(
    spec: &SurfaceSpec,
    widget: &str,
    kind: EventKind,
    value: Value,
) -> Result<Value, ReduceError> {
    let event = Event {
        widget: widget.to_string(),
        kind,
        value,
    };
    reduce(spec, &spec.state, &Value::Null, &Value::Null, &event).map(|(state, _)| state)
}

fn rendered(spec: &SurfaceSpec, state: &Value) -> RenderKind {
    resolve(spec, state, &Value::Null, &Value::Null).root.node
}

// ── O-D2: the full expanded list ────────────────────────────────────────────

#[test]
fn the_render_tree_carries_the_whole_expanded_list_including_hidden_branches() {
    let s = sidebar();
    // `a` is collapsed; `b` (a hidden grandchild branch) stays in the list.
    let state = json!({"open": ["b"], "rows": rows()});
    match rendered(&s, &state) {
        RenderKind::Tree {
            nodes, expanded, ..
        } => {
            let shown: Vec<&str> = nodes.iter().map(|n| n.label.as_str()).collect();
            assert_eq!(shown, vec!["src", "docs"], "b and c are hidden");
            assert_eq!(expanded, vec![json!("b")], "yet b's state is carried");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn collapsing_then_re_expanding_a_branch_keeps_a_grandchilds_state() {
    let s = sidebar();
    // The renderer collapses `a`: the whole list minus one id.
    let collapsed = send(&s, "side", EventKind::Change, json!(["b"])).unwrap();
    assert_eq!(collapsed["open"], json!(["b"]));
    // It re-expands `a`: the whole list it was handed plus one id.
    let reopened = send(&s, "side", EventKind::Change, json!(["b", "a"])).unwrap();
    match rendered(&s, &reopened) {
        RenderKind::Tree { nodes, .. } => {
            let shown: Vec<(&str, bool)> = nodes
                .iter()
                .map(|n| (n.label.as_str(), n.expanded))
                .collect();
            assert_eq!(
                shown,
                vec![
                    ("src", true),
                    ("lib", true),
                    ("mod.rs", false),
                    ("docs", false)
                ],
                "lib is still open: its state survived the collapse"
            );
        }
        other => panic!("{other:?}"),
    }
}

// ── O-D3/O-D4/O-D5: each event reaches its handler ──────────────────────────

#[test]
fn rename_runs_on_rename_with_id_and_label() {
    let state = send(
        &sidebar(),
        "side",
        EventKind::Rename,
        json!({"id": "b", "label": "library"}),
    )
    .unwrap();
    assert_eq!(state["renamed"], json!({"id": "b", "label": "library"}));
}

#[test]
fn delete_and_activate_run_their_handlers_with_the_id_list() {
    let s = sidebar();
    let state = send(&s, "side", EventKind::Delete, json!(["c"])).unwrap();
    assert_eq!(state["deleted"], json!(["c"]));
    assert!(state.get("opened").is_none(), "only the delete handler ran");
    let state = send(&s, "side", EventKind::Activate, json!(["d"])).unwrap();
    assert_eq!(state["opened"], json!(["d"]));
    assert!(state.get("deleted").is_none());
}

#[test]
fn move_runs_on_move_with_ids_to_and_index() {
    let value = json!({"ids": ["c", "d"], "to": "a", "index": 1});
    let state = send(&sidebar(), "side", EventKind::Move, value.clone()).unwrap();
    assert_eq!(state["moved"], value);
    // To the root: `to` is null.
    let root = json!({"ids": ["c"], "to": null, "index": 0});
    let state = send(&sidebar(), "side", EventKind::Move, root.clone()).unwrap();
    assert_eq!(state["moved"], root);
}

#[test]
fn a_move_that_is_not_an_object_is_refused_by_a_tree_that_handles_it() {
    let err = send(&sidebar(), "side", EventKind::Move, json!(["c"])).unwrap_err();
    assert_eq!(err.code(), "invalid-argument");
}

#[test]
fn menu_runs_the_named_entry_with_the_event_value() {
    let s = sidebar();
    let value = json!({"id": "b", "action": "Delete"});
    let state = send(&s, "side", EventKind::Menu, value.clone()).unwrap();
    assert_eq!(state["menu_delete"], value);
    assert!(state.get("menu_rename").is_none(), "only that entry ran");
    let value = json!({"id": "a", "action": "Rename"});
    let state = send(&s, "side", EventKind::Menu, value.clone()).unwrap();
    assert_eq!(state["menu_rename"], value);
}

#[test]
fn an_unknown_menu_label_is_refused_naming_the_labels() {
    let err = send(
        &sidebar(),
        "side",
        EventKind::Menu,
        json!({"id": "a", "action": "Explode"}),
    )
    .unwrap_err();
    assert_eq!(err.code(), "invalid-argument");
    let message = err.to_string();
    assert!(
        message.contains("Explode") && message.contains("Rename") && message.contains("Delete"),
        "{message}"
    );
    // A menu event with no label at all is refused the same way.
    let err = send(&sidebar(), "side", EventKind::Menu, json!({"id": "a"})).unwrap_err();
    assert!(err.to_string().contains("Rename"), "{err}");
}

#[test]
fn a_table_and_a_list_menu_work_like_a_trees() {
    let s = spec_of(json!({"column": [
        {"id": "t", "table": {
            "rows": "{{state.rows}}", "columns": ["label"], "count_field": "n",
            "actions": [{"label": "Open", "on_click": handler("t_open")}]}},
        {"id": "l", "list": {
            "rows": "{{state.rows}}",
            "actions": [{"label": "Pin", "on_click": handler("l_pin")}]}}
    ]}));
    let value = json!({"id": "a", "action": "Open"});
    let state = send(&s, "t", EventKind::Menu, value.clone()).unwrap();
    assert_eq!(state["t_open"], value);
    let value = json!({"id": "a", "action": "Pin"});
    let state = send(&s, "l", EventKind::Menu, value.clone()).unwrap();
    assert_eq!(state["l_pin"], value);
    for (widget, label) in [("t", "Pin"), ("l", "Open")] {
        let err = send(
            &s,
            widget,
            EventKind::Menu,
            json!({"id": "a", "action": label}),
        )
        .unwrap_err();
        assert_eq!(err.code(), "invalid-argument", "{widget}");
    }
}

#[test]
fn a_node_without_the_handler_ignores_the_event() {
    // A tree that declares nothing but `on_select`: every row event is a no-op.
    let read_only = spec_of(json!({"id": "side", "tree": {
        "rows": "{{state.rows}}", "on_select": handler("picked")}}));
    for (kind, value) in [
        (EventKind::Rename, json!({"id": "a", "label": "x"})),
        (EventKind::Delete, json!(["a"])),
        (EventKind::Activate, json!(["a"])),
        (EventKind::Move, json!(["not even an object"])),
        (EventKind::Menu, json!({"id": "a", "action": "Anything"})),
    ] {
        let state = send(&read_only, "side", kind, value).unwrap();
        assert_eq!(state, read_only.state, "{kind:?} changed state");
    }
    // A table has no `rename`/`delete`/`activate`/`move`, and a menu event on
    // one that declares no menu is ignored; so are all five on a button.
    let other = spec_of(json!({"column": [
        {"id": "t", "table": {"rows": "{{state.rows}}", "columns": ["label"]}},
        {"id": "b", "button": {"label": "Go", "on_click": handler("clicked")}}
    ]}));
    for widget in ["t", "b"] {
        for kind in [
            EventKind::Rename,
            EventKind::Delete,
            EventKind::Activate,
            EventKind::Move,
            EventKind::Menu,
        ] {
            let state = send(&other, widget, kind, json!({"action": "x"})).unwrap();
            assert_eq!(state, other.state, "{widget} {kind:?}");
        }
    }
    // And an event for a widget that is not there is still refused.
    let err = send(&other, "nope", EventKind::Delete, json!(["a"])).unwrap_err();
    assert_eq!(err.code(), "unknown-widget");
}

#[test]
fn the_event_root_names_the_new_kinds_in_snake_case() {
    let s = spec_of(json!({"id": "side", "tree": {
        "rows": "{{state.rows}}",
        "on_rename": [{"set": {"path": "state.k", "value": "{{event.kind}}"}}]}}));
    let state = send(
        &s,
        "side",
        EventKind::Rename,
        json!({"id": "a", "label": "x"}),
    )
    .unwrap();
    assert_eq!(state["k"], json!("rename"));
    for (kind, wire) in [
        (EventKind::Rename, "rename"),
        (EventKind::Delete, "delete"),
        (EventKind::Activate, "activate"),
        (EventKind::Move, "move"),
        (EventKind::Menu, "menu"),
    ] {
        assert_eq!(serde_json::to_value(kind).unwrap(), json!(wire));
        let event: Event =
            serde_json::from_value(json!({"widget": "side", "kind": wire, "value": null})).unwrap();
        assert_eq!(event.kind, kind);
    }
}

// ── O-D5/O-D6: the render tree ──────────────────────────────────────────────

#[test]
fn the_render_tree_carries_menu_labels_and_row_counts() {
    let s = sidebar();
    match rendered(&s, &s.state) {
        RenderKind::Tree { nodes, actions, .. } => {
            assert_eq!(actions, vec!["Rename".to_string(), "Delete".to_string()]);
            let counts: Vec<Option<Value>> = nodes.iter().map(|n| n.count.clone()).collect();
            // src: 3, lib: 0 (zero is a count), mod.rs: no field, docs: null.
            assert_eq!(counts, vec![Some(json!(3)), Some(json!(0)), None, None]);
        }
        other => panic!("{other:?}"),
    }
    let t = spec_of(json!({"column": [
        {"id": "t", "table": {"rows": "{{state.rows}}", "columns": ["label"],
            "count_field": "n", "actions": [{"label": "Open"}]}},
        {"id": "l", "list": {"rows": "{{state.rows}}", "count_field": "n",
            "actions": [{"label": "Pin"}]}}
    ]}));
    match rendered(&t, &t.state) {
        RenderKind::Column { items } => {
            match &items[0].node {
                RenderKind::Table {
                    actions,
                    count_field,
                    ..
                } => {
                    assert_eq!(actions, &vec!["Open".to_string()]);
                    assert_eq!(count_field.as_deref(), Some("n"));
                }
                other => panic!("{other:?}"),
            }
            match &items[1].node {
                RenderKind::List {
                    actions,
                    count_field,
                    ..
                } => {
                    assert_eq!(actions, &vec!["Pin".to_string()]);
                    assert_eq!(count_field.as_deref(), Some("n"));
                }
                other => panic!("{other:?}"),
            }
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_wire_of_a_tree_table_list_that_declare_nothing_new_is_unchanged() {
    let plain = spec_of(json!({"column": [
        {"id": "x", "tree": {"rows": "{{state.rows}}"}},
        {"id": "t", "table": {"rows": "{{state.rows}}", "columns": ["label"]}},
        {"id": "l", "list": {"rows": "{{state.rows}}"}}
    ]}));
    // The spec: no new key appears.
    let text = serde_json::to_string(&plain).unwrap();
    for key in [
        "on_rename",
        "on_delete",
        "on_activate",
        "on_move",
        "actions",
        "count_field",
    ] {
        assert!(!text.contains(key), "{key} leaked into {text}");
    }
    // The render tree: neither.
    let tree = resolve(&plain, &plain.state, &Value::Null, &Value::Null);
    let text = serde_json::to_string(&tree).unwrap();
    for key in [
        "actions",
        "count_field",
        "\"expanded\":[",
        "\"count\"",
        "movable",
    ] {
        assert!(!text.contains(key), "{key} leaked into {text}");
    }
    let wire = serde_json::to_value(&tree.root).unwrap();
    assert_eq!(
        wire["node"]["items"][0]["node"]["nodes"][0],
        json!({"id": "a", "label": "src", "depth": 0, "has_children": true,
               "expanded": false, "row": rows()[0]})
    );
}

#[test]
fn a_tree_that_declares_on_move_is_movable_and_one_that_does_not_says_nothing() {
    let s = sidebar();
    let rendered = resolve(&s, &s.state, &Value::Null, &Value::Null);
    match &rendered.root.node {
        RenderKind::Tree { movable, .. } => assert!(movable),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        serde_json::to_value(&rendered.root.node).unwrap()["movable"],
        json!(true)
    );

    let plain = spec_of(json!({"id": "t", "tree": {"rows": "{{state.rows}}"}}));
    let rendered = resolve(&plain, &plain.state, &Value::Null, &Value::Null);
    match &rendered.root.node {
        RenderKind::Tree { movable, .. } => assert!(!movable),
        other => panic!("{other:?}"),
    }
    let wire = serde_json::to_value(&rendered.root.node).unwrap();
    assert!(wire.get("movable").is_none(), "{wire}");
}

// ── serde ───────────────────────────────────────────────────────────────────

#[test]
fn a_tree_with_every_new_field_round_trips() {
    let s = sidebar();
    let wire = serde_json::to_value(&s).unwrap();
    let tree = &wire["root"]["tree"];
    for key in [
        "on_rename",
        "on_delete",
        "on_activate",
        "on_move",
        "actions",
        "count_field",
    ] {
        assert!(!tree[key].is_null(), "{key} missing from {tree}");
    }
    assert_eq!(tree["count_field"], "n");
    assert_eq!(tree["actions"][1]["label"], "Delete");
    assert_eq!(
        tree["actions"][1]["on_click"][0]["set"]["path"],
        "state.menu_delete"
    );
    let back: SurfaceSpec = serde_json::from_value(wire).unwrap();
    assert_eq!(back, s);

    // The render tree round-trips too, new fields and all.
    let rendered = resolve(&s, &s.state, &Value::Null, &Value::Null);
    let back = serde_json::from_value(serde_json::to_value(&rendered).unwrap()).unwrap();
    assert_eq!(rendered, back);
    let wire = serde_json::to_value(&rendered.root.node).unwrap();
    assert_eq!(wire["actions"], json!(["Rename", "Delete"]));
    assert_eq!(wire["expanded"], json!(["a", "b"]));
    assert_eq!(wire["nodes"][0]["count"], json!(3));
}

// ── validate ────────────────────────────────────────────────────────────────

#[test]
fn a_sidebar_validates_clean() {
    assert_eq!(errors_of(&sidebar()), Vec::<String>::new());
}

#[test]
fn a_duplicate_or_empty_menu_label_is_refused() {
    for (kind, body) in [
        ("tree", json!({"rows": "{{state.rows}}"})),
        (
            "table",
            json!({"rows": "{{state.rows}}", "columns": ["label"]}),
        ),
        ("list", json!({"rows": "{{state.rows}}"})),
    ] {
        let mut dup = body.clone();
        dup["actions"] = json!([{"label": "Open"}, {"label": "Open"}]);
        let errors = errors_of(&spec_of(json!({"id": "w", kind: dup})));
        assert!(
            errors
                .iter()
                .any(|e| e.starts_with(&format!("/root/{kind}/actions/1/label"))
                    && e.contains("twice")),
            "{kind}: {errors:?}"
        );
        let mut empty = body;
        empty["actions"] = json!([{"label": "Open"}, {"label": " "}]);
        let errors = errors_of(&spec_of(json!({"id": "w", kind: empty})));
        assert!(
            errors
                .iter()
                .any(|e| e.starts_with(&format!("/root/{kind}/actions/1/label"))
                    && e.contains("empty")),
            "{kind}: {errors:?}"
        );
    }
}

#[test]
fn count_field_must_be_a_literal_name() {
    for kind in ["tree", "list"] {
        for bad in ["", "{{state.f}}"] {
            let errors = errors_of(&spec_of(
                json!({"id": "w", kind: {"rows": "{{state.rows}}", "count_field": bad}}),
            ));
            assert!(
                errors
                    .iter()
                    .any(|e| e.starts_with(&format!("/root/{kind}/count_field"))),
                "{kind} {bad:?}: {errors:?}"
            );
        }
    }
    let errors = errors_of(&spec_of(json!({"id": "w", "table": {
        "rows": "{{state.rows}}", "columns": ["label"], "count_field": ""}})));
    assert!(
        errors
            .iter()
            .any(|e| e.starts_with("/root/table/count_field")),
        "{errors:?}"
    );
}

#[test]
fn every_new_handler_list_is_checked_like_on_select() {
    // A bad `set` path in each handler, and a malformed verb in a menu entry.
    let bad = json!([{"set": {"path": "nope", "value": 1}}]);
    let s = spec_of(json!({"id": "side", "tree": {
        "rows": "{{state.rows}}",
        "on_rename": bad, "on_delete": bad, "on_activate": bad, "on_move": bad,
        "actions": [{"label": "Go", "on_click": [{"call": {"verb": "NotAVerb"}}]}]
    }}));
    let errors = errors_of(&s);
    for pointer in [
        "/root/tree/on_rename/0",
        "/root/tree/on_delete/0",
        "/root/tree/on_activate/0",
        "/root/tree/on_move/0",
        "/root/tree/actions/0/on_click/0",
    ] {
        assert!(
            errors.iter().any(|e| e.starts_with(pointer)),
            "{pointer}: {errors:?}"
        );
    }
    // `surface_validate` finds verb names through the same pointers.
    let verbs: Vec<String> = impress_surface::walk_actions(&s.root)
        .into_iter()
        .filter(|(_, a)| matches!(a, impress_surface::Action::Call { .. }))
        .map(|(at, _)| at)
        .collect();
    assert_eq!(verbs, vec!["/root/tree/actions/0/on_click/0".to_string()]);
}

#[test]
fn a_misspelt_key_in_a_menu_entry_is_reported_by_validate_json() {
    let raw = json!({
        "surface": "1.0", "name": "x", "state": {"rows": []},
        "root": {"id": "side", "tree": {"rows": "{{state.rows}}",
            "actions": [{"label": "Go", "on_clik": [{"publish": {}}]}]}}
    });
    let (_, problems) = validate_json(&raw);
    assert!(
        problems
            .iter()
            .any(|p| p.is_error() && p.path == "/root/tree/actions/0/on_clik"),
        "{problems:?}"
    );
}
