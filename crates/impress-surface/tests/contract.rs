//! Wave 7 T6a: what the vocabulary promises an authoring agent, pinned. Each
//! test names the review finding it closes and fails with its fix reverted.

use impress_surface::{
    reduce, resolve, validate_json, Event, EventKind, Problem, ReduceError, Severity,
    StatePathError, SurfaceSpec,
};
use serde_json::{json, Value};

fn spec(root: Value) -> Value {
    json!({ "surface": "1.0", "name": "t", "state": { "x": 1, "rows": [ {"n": 1}, {"n": 2} ] }, "root": root })
}

fn errors_of(raw: &Value) -> Vec<Problem> {
    validate_json(raw)
        .1
        .into_iter()
        .filter(Problem::is_error)
        .collect()
}

fn parsed(raw: Value) -> SurfaceSpec {
    let (spec, problems) = validate_json(&raw);
    let errors: Vec<&Problem> = problems.iter().filter(|p| p.is_error()).collect();
    assert!(errors.is_empty(), "{errors:?}");
    spec.expect("parsed")
}

/// RS-S7: a KNOWN kind with a malformed body says what is wrong with it, at
/// its own path — not "unrecognized node kind 'table'".
#[test]
fn a_malformed_known_kind_reports_its_own_error() {
    let problems = errors_of(&spec(
        json!({ "column": [ { "id": "t", "table": { "rows": [] } } ] }),
    ));
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].path, "/root/column/0");
    assert!(
        problems[0].message.contains("missing field `columns`"),
        "{problems:?}"
    );
}

/// RS-S7: zero or several kind keys are named, not reported as kind ''.
#[test]
fn a_node_with_two_kind_keys_names_both() {
    let problems = errors_of(&spec(json!({ "text": "a", "divider": {} })));
    assert!(
        problems
            .iter()
            .any(|p| p.path == "/root" && p.message.contains("[divider, text]")),
        "{problems:?}"
    );
}

/// AC-F13: a structural mistake is a located problem, not a parse failure.
#[test]
fn structural_mistakes_are_located_problems() {
    let (parsed, problems) = validate_json(&json!({ "surface": "1.0", "root": { "spacer": {} },
        "sources": { "s": { "value": 1, "verb": "a-service_b" } }, "extra": true }));
    assert!(parsed.is_none());
    let paths: Vec<&str> = problems.iter().map(|p| p.path.as_str()).collect();
    assert!(paths.contains(&""), "missing name: {problems:?}");
    assert!(paths.contains(&"/sources/s"), "{problems:?}");
    assert!(paths.contains(&"/extra"), "{problems:?}");
}

/// Strict agent inputs: a key nothing reads is an error, wherever it is.
#[test]
fn an_unknown_field_anywhere_is_an_error_at_its_path() {
    let problems = errors_of(&spec(json!({ "column": [
        { "id": "b", "button": { "label": "Go", "onclick": [] , "on_click": [
            { "set": { "path": "state.x", "value": 2, "vaule": 3 } } ] } }
    ] })));
    let paths: Vec<&str> = problems.iter().map(|p| p.path.as_str()).collect();
    // `onclick: []` is empty, so harmless and not reported; `vaule` is.
    assert_eq!(
        paths,
        vec!["/root/column/0/button/on_click/0/set/vaule"],
        "{problems:?}"
    );
}

/// An unknown kind is forward compatibility: a warning, and the spec is valid.
#[test]
fn an_unknown_kind_is_a_warning_only() {
    let (parsed, problems) = validate_json(&spec(json!({ "sparkline": { "values": [1] } })));
    assert!(parsed.is_some());
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].severity, Severity::Warning);
}

/// RS-S3 (the validate half): `{{param.x}}` must name a declared param.
#[test]
fn a_param_reference_must_name_a_declared_param() {
    let mut raw = spec(json!({ "text": "{{param.selected}} {{param.nope}}" }));
    raw["params"] = json!([{ "name": "selected", "kind": "publication", "required": false }]);
    let problems = errors_of(&raw);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].message.contains("'nope'"), "{problems:?}");
}

/// RS-S8 (2): a `call`'s `into` is checked before the verb ever runs.
#[test]
fn a_bad_into_path_is_a_validation_error() {
    let problems = errors_of(&spec(
        json!({ "id": "b", "button": { "label": "Go", "on_click": [
        { "call": { "verb": "a-service_b", "into": "result" } } ] } }),
    ));
    assert!(
        problems
            .iter()
            .any(|p| p.path == "/root/button/on_click/0/call/into"),
        "{problems:?}"
    );
}

/// RS-S8 (4): an `open` with no view kind, or a query that is not one.
#[test]
fn an_open_is_checked_statically() {
    let problems = errors_of(&spec(
        json!({ "id": "b", "button": { "label": "Go", "on_click": [
        { "open": { "query": { "kinds": 3 }, "view_kind": "" } } ] } }),
    ));
    let paths: Vec<&str> = problems.iter().map(|p| p.path.as_str()).collect();
    assert!(
        paths.contains(&"/root/button/on_click/0/open/view_kind"),
        "{problems:?}"
    );
    assert!(
        paths.contains(&"/root/button/on_click/0/open/query"),
        "{problems:?}"
    );
}

fn click(
    spec: &SurfaceSpec,
    sources: &Value,
) -> Result<(Value, Vec<impress_surface::Effect>), ReduceError> {
    let event = Event {
        widget: "b".into(),
        kind: EventKind::Click,
        value: Value::Null,
    };
    reduce(spec, &spec.state, &Value::Null, sources, &event)
}

/// RS-S8 (1): an action may read a source, as a node may.
#[test]
fn an_action_reads_the_sources_the_last_render_saw() {
    let mut raw = spec(json!({ "id": "b", "button": { "label": "Go", "on_click": [
        { "emit": { "name": "picked", "payload": { "first": "{{source.papers.0.id}}" } } } ] } }));
    raw["sources"] = json!({ "papers": { "value": [] } });
    let spec = parsed(raw);
    let (_, effects) = click(&spec, &json!({ "papers": [ { "id": "p-1" } ] })).unwrap();
    assert_eq!(
        effects,
        vec![impress_surface::Effect::Emit {
            name: "picked".into(),
            payload: json!({ "first": "p-1" })
        }]
    );
}

/// RS-S8 (3): publishing a state path that is not there fails; it used to
/// publish `null` and clear the selection.
#[test]
fn publishing_a_missing_state_path_fails() {
    let spec = parsed(spec(
        json!({ "id": "b", "button": { "label": "Go", "on_click": [
        { "publish": { "ids": "state.selected" } } ] } }),
    ));
    let err = click(&spec, &Value::Null).unwrap_err();
    assert_eq!(err.code(), "missing-state-path", "{err}");
}

/// RS-S9: a `set` through an array index writes the element; it used to
/// replace the array with an object keyed "1".
#[test]
fn a_set_through_an_array_index_writes_the_element() {
    let spec = parsed(spec(
        json!({ "id": "b", "button": { "label": "Go", "on_click": [
        { "set": { "path": "state.rows.1.n", "value": 20 } } ] } }),
    ));
    let (state, _) = click(&spec, &Value::Null).unwrap();
    assert_eq!(state["rows"], json!([ {"n": 1}, {"n": 20} ]));

    let bad = parsed(spec_with_set("state.rows.x"));
    let err = click(&bad, &Value::Null).unwrap_err();
    assert!(
        matches!(
            err,
            ReduceError::StatePath(StatePathError::NotAContainer { .. })
        ),
        "{err}"
    );
}

fn spec_with_set(path: &str) -> Value {
    spec(json!({ "id": "b", "button": { "label": "Go", "on_click": [
        { "set": { "path": path, "value": 0 } } ] } }))
}

/// RS-S20: the placeholder for an unknown kind keeps the node; an unreadable
/// node's placeholder says why and keeps it too.
#[test]
fn a_placeholder_keeps_the_node_it_stands_for() {
    // Structurally a spec, so it parses; the bad table is an error that
    // create would refuse, but a stored row from before still renders.
    let spec = validate_json(&spec(json!({ "column": [
        { "sparkline": { "values": [1, 2] }, "label": "Trend" },
        { "table": { "rows": [] } }
    ] })))
    .0
    .expect("parses");
    let tree =
        serde_json::to_value(resolve(&spec, &spec.state, &Value::Null, &Value::Null)).unwrap();
    let items = &tree["root"]["node"]["items"];
    assert_eq!(items[0]["node"]["kind"], "placeholder");
    assert_eq!(items[0]["node"]["unknown_kind"], "sparkline");
    assert_eq!(
        items[0]["node"]["node"],
        json!({ "sparkline": { "values": [1, 2] }, "label": "Trend" })
    );
    assert_eq!(items[1]["node"]["kind"], "placeholder");
    assert!(
        items[1]["node"]["reason"]
            .as_str()
            .unwrap()
            .contains("columns"),
        "{tree}"
    );
    assert_eq!(items[1]["node"]["node"], json!({ "table": { "rows": [] } }));
}

/// RS-S10: a formula is text, and a dotted non-reference is a warning.
#[test]
fn a_formula_renders_as_text_and_a_likely_typo_warns() {
    let raw =
        spec(json!({ "column": [ { "text": "$\\frac{{a}}{b}$" }, { "text": "{{stat.x}}" } ] }));
    let (parsed, problems) = validate_json(&raw);
    let spec = parsed.unwrap();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].severity, Severity::Warning);
    assert_eq!(problems[0].path, "/root/column/1/text");
    let tree =
        serde_json::to_value(resolve(&spec, &spec.state, &Value::Null, &Value::Null)).unwrap();
    assert_eq!(
        tree["root"]["node"]["items"][0]["node"]["text"],
        "$\\frac{{a}}{b}$"
    );
}
