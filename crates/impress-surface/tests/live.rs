//! plan-koine-live P2 in the grammar: the `{rows, next}` envelope and
//! `append`, the `file` field, the `code` node and the bound plot.

use impress_surface::{
    reduce, resolve, validate, Action, Event, EventKind, Node, RenderKind, SurfaceSpec,
};
use serde_json::{json, Value};

fn spec(state: Value, root: Value) -> SurfaceSpec {
    serde_json::from_value(json!({
        "surface": "1.0", "name": "live", "state": state, "root": root
    }))
    .expect("a spec")
}

fn rendered(spec: &SurfaceSpec) -> RenderKind {
    resolve(spec, &spec.state, &Value::Null, &Value::Null)
        .root
        .node
}

fn no_errors(spec: &SurfaceSpec) {
    let problems = validate(spec);
    assert!(
        problems.iter().all(|p| !p.is_error()),
        "unexpected problems: {problems:?}"
    );
}

fn errors_of(spec: &SurfaceSpec) -> Vec<String> {
    validate(spec)
        .into_iter()
        .filter(|p| p.is_error())
        .map(|p| format!("{}: {}", p.path, p.message))
        .collect()
}

// ── The envelope ────────────────────────────────────────────────────────────

#[test]
fn a_table_reads_rows_columns_and_id_field_from_an_envelope() {
    let s = spec(
        json!({"result": {
            "rows": [{"id": "a", "n": 1}, {"id": "b", "n": 2}],
            "columns": ["n", "id"],
            "id_field": "id",
            "next": "c2"
        }}),
        json!({"id": "t", "table": {"rows": "{{state.result}}", "columns": []}}),
    );
    match rendered(&s) {
        RenderKind::Table {
            rows,
            columns,
            id_field,
        } => {
            assert_eq!(rows, json!([{"id": "a", "n": 1}, {"id": "b", "n": 2}]));
            assert_eq!(columns, vec!["n", "id"]);
            assert_eq!(id_field.as_deref(), Some("id"));
        }
        other => panic!("expected a table, got {other:?}"),
    }
}

#[test]
fn the_nodes_own_columns_and_id_field_win_over_the_envelopes() {
    let s = spec(
        json!({"result": {"rows": [{"id": "a", "n": 1}], "columns": ["n"], "id_field": "n"}}),
        json!({"id": "t", "table": {
            "rows": "{{state.result}}", "columns": ["id"], "id_field": "id"}}),
    );
    match rendered(&s) {
        RenderKind::Table {
            columns, id_field, ..
        } => {
            assert_eq!(columns, vec!["id"]);
            assert_eq!(id_field.as_deref(), Some("id"));
        }
        other => panic!("expected a table, got {other:?}"),
    }
}

#[test]
fn an_envelope_without_columns_shows_the_first_rows_keys_sorted() {
    let s = spec(
        json!({"result": {"rows": [{"year": 2020, "title": "x"}], "next": null}}),
        json!({"id": "t", "table": {"rows": "{{state.result}}", "columns": []}}),
    );
    match rendered(&s) {
        RenderKind::Table { columns, .. } => assert_eq!(columns, vec!["title", "year"]),
        other => panic!("expected a table, got {other:?}"),
    }
}

#[test]
fn a_bare_array_with_no_columns_is_as_it_was() {
    let s = spec(
        json!({"result": [{"year": 2020}]}),
        json!({"id": "t", "table": {"rows": "{{state.result}}", "columns": []}}),
    );
    match rendered(&s) {
        RenderKind::Table { rows, columns, .. } => {
            assert_eq!(rows, json!([{"year": 2020}]));
            assert!(columns.is_empty());
        }
        other => panic!("expected a table, got {other:?}"),
    }
}

#[test]
fn a_list_reads_the_rows_of_an_envelope() {
    let s = spec(
        json!({"result": {"rows": ["a", "b"], "next": "c"}}),
        json!({"id": "l", "list": {"rows": "{{state.result}}"}}),
    );
    match rendered(&s) {
        RenderKind::List { rows } => assert_eq!(rows, json!(["a", "b"])),
        other => panic!("expected a list, got {other:?}"),
    }
}

// ── append ──────────────────────────────────────────────────────────────────

fn more_button(append_value: &str) -> Value {
    json!({"id": "more", "button": {"label": "More", "on_click": [
        {"append": {"path": "state.result.rows", "value": append_value}}
    ]}})
}

fn click(spec: &SurfaceSpec) -> Result<Value, impress_surface::ReduceError> {
    let event = Event {
        widget: "more".to_string(),
        kind: EventKind::Click,
        value: Value::Null,
    };
    reduce(spec, &spec.state, &Value::Null, &Value::Null, &event).map(|(state, _)| state)
}

#[test]
fn append_adds_the_rows_of_an_envelope_to_the_array_at_the_path() {
    let s = spec(
        json!({"result": {"rows": [1, 2], "next": "c"},
               "more": {"rows": [3], "next": null}}),
        more_button("{{state.more}}"),
    );
    no_errors(&s);
    let state = click(&s).unwrap();
    assert_eq!(state["result"]["rows"], json!([1, 2, 3]));
    assert_eq!(state["result"]["next"], json!("c"));
}

#[test]
fn append_takes_an_array_whole() {
    let s = spec(
        json!({"result": {"rows": [1]}, "more": [2, 3]}),
        more_button("{{state.more}}"),
    );
    assert_eq!(click(&s).unwrap()["result"]["rows"], json!([1, 2, 3]));
}

#[test]
fn append_to_a_missing_or_null_target_makes_it_the_values_rows() {
    let s = spec(
        json!({"result": {"rows": null}, "more": {"rows": [9]}}),
        more_button("{{state.more}}"),
    );
    assert_eq!(click(&s).unwrap()["result"]["rows"], json!([9]));
    let s = spec(
        json!({"result": {}, "more": {"rows": [9]}}),
        more_button("{{state.more}}"),
    );
    assert_eq!(click(&s).unwrap()["result"]["rows"], json!([9]));
}

#[test]
fn append_of_something_that_is_not_rows_is_invalid_argument_naming_the_shape() {
    let s = spec(
        json!({"result": {"rows": [1]}, "more": "text"}),
        more_button("{{state.more}}"),
    );
    let error = click(&s).unwrap_err();
    assert_eq!(error.code(), "invalid-argument");
    assert!(error.to_string().contains("a string"), "{error}");
    // An object without a rows array is not an envelope.
    let s = spec(
        json!({"result": {"rows": [1]}, "more": {"items": []}}),
        more_button("{{state.more}}"),
    );
    let error = click(&s).unwrap_err();
    assert_eq!(error.code(), "invalid-argument");
    assert!(error.to_string().contains("an object"), "{error}");
}

#[test]
fn append_onto_something_that_is_not_an_array_is_invalid_argument() {
    let s = spec(
        json!({"result": {"rows": "x"}, "more": [1]}),
        more_button("{{state.more}}"),
    );
    let error = click(&s).unwrap_err();
    assert_eq!(error.code(), "invalid-argument");
    assert!(error.to_string().contains("not an array"), "{error}");
}

#[test]
fn append_is_validated_like_set() {
    let wire = json!({"append": {"path": "state.result.rows", "value": "{{state.more}}"}});
    let action: Action = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(&action).unwrap(), wire);

    let bad_path = spec(
        json!({"more": []}),
        json!({"id": "b", "button": {"label": "x", "on_click": [
            {"append": {"path": "result.rows", "value": "{{state.more}}"}}]}}),
    );
    assert!(errors_of(&bad_path)
        .iter()
        .any(|e| e.contains("append/path")));
    let bad_ref = spec(
        json!({"result": {"rows": []}}),
        json!({"id": "b", "button": {"label": "x", "on_click": [
            {"append": {"path": "state.result.rows", "value": "{{state.nope}}"}}]}}),
    );
    assert!(errors_of(&bad_ref).iter().any(|e| e.contains("state.nope")));
}

// ── The file field ──────────────────────────────────────────────────────────

#[test]
fn a_file_field_round_trips_resolves_and_binds_a_blob_reference() {
    let wire = json!({"id": "up", "bind": "state.file", "field": {"file": {"accept": ".csv"}}});
    let node: Node = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(&node).unwrap(), wire);

    let s = spec(json!({"file": null}), wire);
    no_errors(&s);
    match rendered(&s) {
        RenderKind::Field { field, bind, value } => {
            assert_eq!(field, json!({"file": {"accept": ".csv"}}));
            assert_eq!(bind.as_deref(), Some("state.file"));
            assert_eq!(value, Value::Null);
        }
        other => panic!("expected a field, got {other:?}"),
    }
    let blob = json!({"blob": {"item": "s1", "name": "data.csv"}});
    let event = Event {
        widget: "up".to_string(),
        kind: EventKind::Change,
        value: blob.clone(),
    };
    let (state, effects) =
        reduce(&s, &s.state, &Value::Null, &Value::Null, &event).expect("a change");
    assert!(effects.is_empty());
    assert_eq!(state["file"], blob);
}

// ── The code node ───────────────────────────────────────────────────────────

#[test]
fn a_code_node_round_trips_resolves_its_text_and_validates() {
    let wire = json!({"id": "c", "code": {"language": "diff", "text": "{{state.patch}}"}});
    let node: Node = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(&node).unwrap(), wire);
    assert_eq!(node.kind_name(), "code");

    let s = spec(json!({"patch": "@@ -1 +1 @@\n-a\n+b"}), wire);
    no_errors(&s);
    match rendered(&s) {
        RenderKind::Code { language, text, .. } => {
            assert_eq!(language.as_deref(), Some("diff"));
            assert_eq!(text, "@@ -1 +1 @@\n-a\n+b");
        }
        other => panic!("expected code, got {other:?}"),
    }
    // A language is optional and a wire without one stays without.
    let plain = json!({"code": {"text": "x"}});
    let node: Node = serde_json::from_value(plain.clone()).unwrap();
    assert_eq!(serde_json::to_value(&node).unwrap(), plain);
}

#[test]
fn a_code_text_that_is_not_a_string_is_an_error() {
    let s = spec(json!({}), json!({"id": "c", "code": {"text": 3}}));
    assert!(errors_of(&s).iter().any(|e| e.contains("code/text")));
}

// ── The bound plot ──────────────────────────────────────────────────────────

fn plot_spec(plot: Value) -> SurfaceSpec {
    spec(
        json!({"rows": [
            {"t": 0, "a": 1.5, "b": 10, "label": "one"},
            {"t": 1, "a": 2.5, "b": "11", "label": "two"},
            {"t": "x", "a": 3.5, "b": 12, "label": "three"},
            {"t": 3, "a": null, "b": 13, "label": "four"}
        ]}),
        json!({"id": "p", "plot": plot}),
    )
}

#[test]
fn a_bound_plot_builds_one_series_per_y_and_skips_what_is_not_a_number() {
    let s = plot_spec(json!({"rows": "{{state.rows}}", "x": "t", "y": ["a", "b"]}));
    no_errors(&s);
    match rendered(&s) {
        RenderKind::Plot { spec } => {
            assert_eq!(spec["x_axis"], json!({"label": "t"}));
            let series = spec["series"].as_array().unwrap();
            assert_eq!(series.len(), 2);
            assert_eq!(series[0]["label"], json!("a"));
            assert_eq!(series[0]["x"], json!([0, 1]));
            assert_eq!(series[0]["y"], json!([1.5, 2.5]));
            assert_eq!(series[0]["style"], json!("Line"));
            // "11" is a string, "x" is a string: neither is a number here.
            assert_eq!(series[1]["label"], json!("b"));
            assert_eq!(series[1]["x"], json!([0, 3]));
            assert_eq!(series[1]["y"], json!([10, 13]));
        }
        other => panic!("expected a plot, got {other:?}"),
    }
}

#[test]
fn a_bound_plot_takes_a_single_y_and_an_envelope() {
    let s = spec(
        json!({"r": {"rows": [{"t": 1, "a": 2}, {"t": 2, "a": 4}], "next": null}}),
        json!({"id": "p", "plot": {"rows": "{{state.r}}", "x": "t", "y": "a"}}),
    );
    no_errors(&s);
    match rendered(&s) {
        RenderKind::Plot { spec } => {
            assert_eq!(spec["series"][0]["x"], json!([1, 2]));
            assert_eq!(spec["series"][0]["y"], json!([2, 4]));
            assert_eq!(spec["series"].as_array().unwrap().len(), 1);
        }
        other => panic!("expected a plot, got {other:?}"),
    }
}

#[test]
fn the_spec_form_of_a_plot_is_unchanged() {
    let wire = json!({"plot": {"spec": {"series": []}}});
    let node: Node = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(&node).unwrap(), wire);
    let s = spec(
        json!({}),
        json!({"id": "p", "plot": {"spec": {"series": []}}}),
    );
    no_errors(&s);
    assert_eq!(
        rendered(&s),
        RenderKind::Plot {
            spec: json!({"series": []})
        }
    );
}

#[test]
fn a_plot_is_exactly_one_of_spec_or_rows_x_y() {
    let both = plot_spec(json!({
        "spec": {"series": []}, "rows": "{{state.rows}}", "x": "t", "y": "a"}));
    assert!(errors_of(&both).iter().any(|e| e.contains("not both")));
    let neither = plot_spec(json!({}));
    assert!(errors_of(&neither)
        .iter()
        .any(|e| e.contains("needs `spec`")));
    let partial = plot_spec(json!({"rows": "{{state.rows}}", "x": "t"}));
    assert!(errors_of(&partial).iter().any(|e| e.contains("missing y")));
    let spec_and_x = plot_spec(json!({"spec": {"series": []}, "x": "t"}));
    assert!(!errors_of(&spec_and_x).is_empty());
    let templated_y = plot_spec(json!({"rows": "{{state.rows}}", "x": "t", "y": "{{state.y}}"}));
    assert!(errors_of(&templated_y).iter().any(|e| e.contains("plot/y")));
    let empty_ys = plot_spec(json!({"rows": "{{state.rows}}", "x": "t", "y": []}));
    assert!(errors_of(&empty_ys).iter().any(|e| e.contains("plot/y")));
}

#[test]
fn the_bound_plots_spec_parses_as_a_koine_plot_spec_wire() {
    // The renderers hand `RenderKind::Plot.spec` to koine-plot; its serde
    // names are the contract, so the series this builds must be the ones it
    // reads. (koine-plot is not a dependency here: the check is on names.)
    let s = plot_spec(json!({"rows": "{{state.rows}}", "x": "t", "y": "a"}));
    let RenderKind::Plot { spec } = rendered(&s) else {
        panic!("a plot")
    };
    let series = &spec["series"][0];
    for key in ["label", "x", "y", "style", "color"] {
        assert!(series.get(key).is_some(), "series lacks {key}: {series}");
    }
}
