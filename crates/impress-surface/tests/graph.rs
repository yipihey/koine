//! plan-koine-tree-stream T3b: the `graph` node, `on_click` on a plot and an
//! image, and the highlighted spans of a `code` node.

use impress_surface::{reduce, resolve, validate, Event, EventKind, RenderKind, SurfaceSpec};
use serde_json::{json, Value};

fn spec(state: Value, node: Value) -> SurfaceSpec {
    serde_json::from_value(json!({
        "surface": "1.0", "name": "graph", "state": state,
        "root": node
    }))
    .expect("a spec")
}

fn errors_of(spec: &SurfaceSpec) -> Vec<String> {
    validate(spec)
        .into_iter()
        .filter(|p| p.is_error())
        .map(|p| format!("{}: {}", p.path, p.message))
        .collect()
}

fn event(widget: &str, kind: EventKind, value: Value) -> Event {
    Event {
        widget: widget.to_string(),
        kind,
        value,
    }
}

fn graph_state() -> Value {
    json!({
        "nodes": [
            {"id": 1, "label": "root"},
            {"id": "b", "label": "leaf"},
            {"label": "no id"},
        ],
        "edges": [
            {"source": 1, "target": "b"},
            {"source": 1},
        ],
    })
}

fn graph_spec() -> SurfaceSpec {
    spec(
        graph_state(),
        json!({"id": "g", "graph": {
            "nodes": "{{state.nodes}}", "edges": "{{state.edges}}",
            "on_select": [{"set": {"path": "state.picked", "value": "{{event.value}}"}}]
        }}),
    )
}

#[test]
fn a_graph_round_trips_and_resolves_ids_as_text_with_an_svg() {
    let s = graph_spec();
    let back: SurfaceSpec = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
    assert_eq!(back, s);
    assert_eq!(s.root.kind_name(), "graph");
    assert!(errors_of(&s).is_empty(), "{:?}", errors_of(&s));

    let tree = resolve(&s, &s.state, &Value::Null, &Value::Null);
    match &tree.root.node {
        RenderKind::Graph { spec, svg } => {
            // A number is its text; a row without an id and an edge without an
            // end are skipped.
            assert_eq!(spec["nodes"][0], json!({"id": "1", "label": "root"}));
            assert_eq!(spec["nodes"][1], json!({"id": "b", "label": "leaf"}));
            assert_eq!(spec["nodes"].as_array().unwrap().len(), 2);
            assert_eq!(spec["edges"], json!([{"source": "1", "target": "b"}]));
            assert!(svg.starts_with("<svg"), "{svg}");
            assert!(svg.contains("data-id=\"1\"") && svg.contains("data-id=\"b\""));
        }
        other => panic!("expected a graph, got {other:?}"),
    }
    assert_eq!(tree.focus_order, vec!["g".to_string()]);
}

#[test]
fn a_graph_reads_its_own_field_names_and_envelopes() {
    let s = spec(
        json!({
            "n": {"rows": [{"key": "x", "name": "X"}], "next": null},
            "e": [{"from": "x", "to": "x"}],
        }),
        json!({"id": "g", "graph": {
            "nodes": "{{state.n}}", "edges": "{{state.e}}",
            "id_field": "key", "label_field": "name",
            "source_field": "from", "target_field": "to",
        }}),
    );
    let tree = resolve(&s, &s.state, &Value::Null, &Value::Null);
    match &tree.root.node {
        RenderKind::Graph { spec, .. } => {
            assert_eq!(spec["nodes"], json!([{"id": "x", "label": "X"}]));
            assert_eq!(spec["edges"], json!([{"source": "x", "target": "x"}]));
        }
        other => panic!("expected a graph, got {other:?}"),
    }
}

#[test]
fn rows_that_are_not_an_array_make_a_placeholder_and_the_rest_renders() {
    let s = spec(
        json!({"nodes": 3, "edges": []}),
        json!({"column": [
            {"id": "g", "graph": {"nodes": "{{state.nodes}}", "edges": "{{state.edges}}"}},
            {"text": "still here"},
        ]}),
    );
    let tree = resolve(&s, &s.state, &Value::Null, &Value::Null);
    let RenderKind::Column { items } = &tree.root.node else {
        panic!("a column")
    };
    assert!(matches!(items[0].node, RenderKind::Placeholder { .. }));
    assert!(matches!(items[1].node, RenderKind::Text { .. }));
    assert!(tree.focus_order.is_empty());
}

#[test]
fn validate_checks_a_graphs_field_names_and_on_select() {
    let s = spec(
        graph_state(),
        json!({"id": "g", "graph": {
            "nodes": "{{state.nodes}}", "edges": "{{state.edges}}",
            "id_field": "{{x}}", "target_field": "",
            "on_select": [{"set": {"path": "picked", "value": 1}}],
        }}),
    );
    let errors = errors_of(&s);
    for needle in [
        "/graph/id_field",
        "/graph/target_field",
        "/graph/on_select/0",
    ] {
        assert!(
            errors.iter().any(|e| e.contains(needle)),
            "{needle}: {errors:?}"
        );
    }
}

#[test]
fn a_select_on_a_graph_runs_on_select_with_the_id_list() {
    let s = graph_spec();
    let (new_state, _) = reduce(
        &s,
        &s.state,
        &Value::Null,
        &Value::Null,
        &event("g", EventKind::Select, json!(["b"])),
    )
    .unwrap();
    assert_eq!(new_state["picked"], json!(["b"]));
}

// ── on_click on a plot and an image ─────────────────────────────────────────

#[test]
fn a_click_on_a_plot_runs_on_click_with_the_coordinates() {
    let s = spec(
        json!({}),
        json!({"id": "p", "plot": {
            "spec": {"series": [{"x": [1, 2], "y": [3, 4]}]},
            "on_click": [{"set": {"path": "state.at", "value": "{{event.value}}"}},
                         {"set": {"path": "state.x", "value": "{{event.value.x}}"}}],
        }}),
    );
    assert!(errors_of(&s).is_empty(), "{:?}", errors_of(&s));
    let back: SurfaceSpec = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
    assert_eq!(back, s);
    let (new_state, _) = reduce(
        &s,
        &s.state,
        &Value::Null,
        &Value::Null,
        &event("p", EventKind::Click, json!({"x": 1.5, "y": 3.5})),
    )
    .unwrap();
    assert_eq!(new_state["at"], json!({"x": 1.5, "y": 3.5}));
    assert_eq!(new_state["x"], json!(1.5));

    // The TUI's click has no position: both are null.
    let (new_state, _) = reduce(
        &s,
        &s.state,
        &Value::Null,
        &Value::Null,
        &event("p", EventKind::Click, json!({"x": null, "y": null})),
    )
    .unwrap();
    assert_eq!(new_state["at"], json!({"x": null, "y": null}));

    // A plot with an on_click is a keyboard stop; one without is not.
    let tree = resolve(&s, &s.state, &Value::Null, &Value::Null);
    assert_eq!(tree.focus_order, vec!["p".to_string()]);
    let bare = spec(json!({}), json!({"id": "p", "plot": {"spec": {}}}));
    let tree = resolve(&bare, &bare.state, &Value::Null, &Value::Null);
    assert!(tree.focus_order.is_empty());
    let wire = serde_json::to_value(&bare).unwrap();
    assert!(wire["root"]["plot"].get("on_click").is_none(), "{wire}");
}

#[test]
fn a_click_on_an_image_runs_on_click_with_fractions() {
    let s = spec(
        json!({}),
        json!({"id": "i", "image": {
            "url": "http://x/y.png",
            "on_click": [{"set": {"path": "state.fy", "value": "{{event.value.y}}"}}],
        }}),
    );
    assert!(errors_of(&s).is_empty(), "{:?}", errors_of(&s));
    let (new_state, _) = reduce(
        &s,
        &s.state,
        &Value::Null,
        &Value::Null,
        &event("i", EventKind::Click, json!({"x": 0.25, "y": 0.75})),
    )
    .unwrap();
    assert_eq!(new_state["fy"], json!(0.75));
    let tree = resolve(&s, &s.state, &Value::Null, &Value::Null);
    assert_eq!(tree.focus_order, vec!["i".to_string()]);
}

#[test]
fn validate_checks_the_actions_of_a_plot_and_an_image_on_click() {
    for node in [
        json!({"id": "p", "plot": {"spec": {}, "on_click": [{"set": {"path": "a", "value": 1}}]}}),
        json!({"id": "i", "image": {"url": "u", "on_click": [{"set": {"path": "a", "value": 1}}]}}),
    ] {
        let s = spec(json!({}), node);
        assert!(
            errors_of(&s).iter().any(|e| e.contains("on_click/0")),
            "{:?}",
            errors_of(&s)
        );
    }
}

// ── highlighted spans ───────────────────────────────────────────────────────

fn code_spec(language: &str) -> SurfaceSpec {
    spec(
        json!({"src": "fn main() {}\nlet x = 1;"}),
        json!({"code": {"language": language, "text": "{{state.src}}"}}),
    )
}

fn spans_of(s: &SurfaceSpec) -> Vec<Vec<(String, String)>> {
    match resolve(s, &s.state, &Value::Null, &Value::Null).root.node {
        RenderKind::Code { spans, .. } => spans,
        other => panic!("expected code, got {other:?}"),
    }
}

#[cfg(feature = "highlight")]
#[test]
fn a_rust_snippet_resolves_to_a_keyword_span_for_fn() {
    let spans = spans_of(&code_spec("rust"));
    assert_eq!(spans.len(), 2);
    assert!(
        spans[0].contains(&("keyword".to_string(), "fn".to_string())),
        "{spans:?}"
    );
    // On the wire: an array of lines of [class, text] pairs.
    let tree = resolve(
        &code_spec("rust"),
        &json!({"src": "fn a() {}"}),
        &Value::Null,
        &Value::Null,
    );
    let wire = serde_json::to_value(&tree.root.node).unwrap();
    assert_eq!(wire["spans"][0][0], json!(["keyword", "fn"]));
}

#[cfg(feature = "highlight")]
#[test]
fn a_diff_and_an_unknown_language_have_no_spans() {
    assert!(spans_of(&code_spec("diff")).is_empty());
    assert!(spans_of(&code_spec("klingon")).is_empty());
    let none = spec(json!({}), json!({"code": {"text": "fn a() {}"}}));
    assert!(spans_of(&none).is_empty());
}

#[cfg(not(feature = "highlight"))]
#[test]
fn without_the_feature_the_spans_are_empty_and_off_the_wire() {
    let s = code_spec("rust");
    assert!(spans_of(&s).is_empty());
    let wire =
        serde_json::to_value(resolve(&s, &s.state, &Value::Null, &Value::Null).root.node).unwrap();
    assert!(wire.get("spans").is_none(), "{wire}");
}
