//! A source that failed to fetch says WHY in the placeholder it leaves
//! behind. Before `resolve_with_source_errors`, a typo in the spec, an app
//! that was not running and a verb that refused all rendered as the same
//! "did not resolve" (Mac, 2026-09-23).

use std::collections::BTreeMap;

use impress_surface::{example_paper_triage, resolve, resolve_with_source_errors, RenderTree};
use serde_json::{json, Value};

/// Every placeholder's `reason`, found by walking the serialized tree — the
/// same JSON the Swift kit and `/api/surface/<id>/render` read.
fn placeholders(tree: &RenderTree) -> Vec<Option<String>> {
    fn walk(value: &Value, out: &mut Vec<Option<String>>) {
        match value {
            Value::Object(map) => {
                if map.get("kind") == Some(&Value::String("placeholder".into())) {
                    out.push(
                        map.get("reason")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    );
                }
                map.values().for_each(|v| walk(v, out));
            }
            Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(&serde_json::to_value(tree).unwrap(), &mut out);
    out
}

#[test]
fn a_failed_source_names_its_failure_in_the_placeholder() {
    let spec = example_paper_triage();
    let state = spec.state.clone();
    let mut errors = BTreeMap::new();
    errors.insert(
        "papers".to_string(),
        "imbib is not running (connection refused on 23120)".to_string(),
    );

    let tree = resolve_with_source_errors(&spec, &state, &json!({}), &json!({}), Some(&errors));
    let reasons = placeholders(&tree);
    let reason = reasons
        .iter()
        .flatten()
        .find(|r| r.contains("source 'papers' failed"))
        .unwrap_or_else(|| panic!("no placeholder carried the source failure: {reasons:?}"));
    assert!(reason.contains("imbib is not running"), "{reason}");
    assert!(
        reason.contains("source.papers"),
        "still names the path: {reason}"
    );
}

#[test]
fn a_missing_source_with_no_recorded_error_keeps_the_plain_reason() {
    let spec = example_paper_triage();
    let state = spec.state.clone();
    let plain = resolve(&spec, &state, &json!({}), &json!({}));
    let with_empty = resolve_with_source_errors(
        &spec,
        &state,
        &json!({}),
        &json!({}),
        Some(&BTreeMap::new()),
    );
    assert_eq!(plain, with_empty);
    let reasons = placeholders(&plain);
    assert!(
        reasons.iter().flatten().all(|r| !r.contains("failed:")),
        "{reasons:?}"
    );
}
