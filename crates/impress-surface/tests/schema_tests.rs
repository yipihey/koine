//! `schemars::schema_for!(SurfaceSpec)` serialises, and mentions every node
//! kind — so `surface_schema` (S4) can hand an agent a real JSON Schema
//! document without it reading this crate's source. Compiled only under the
//! `schema` feature (`cargo test -p impress-surface --features schema`).

#![cfg(feature = "schema")]

use impress_surface::SurfaceSpec;
use schemars::schema_for;

#[test]
fn the_surface_spec_schema_serialises() {
    let schema = schema_for!(SurfaceSpec);
    let json = serde_json::to_string_pretty(&schema).expect("schema serialises to JSON");
    assert!(!json.is_empty());
}

#[test]
fn the_schema_mentions_every_node_kind() {
    let schema = schema_for!(SurfaceSpec);
    let json = serde_json::to_string(&schema).expect("schema serialises to JSON");
    for kind in impress_surface::spec::NODE_KIND_NAMES {
        assert!(
            json.contains(kind),
            "generated schema does not mention node kind '{kind}'"
        );
    }
}

#[test]
fn the_schema_mentions_every_action_kind() {
    let schema = schema_for!(SurfaceSpec);
    let json = serde_json::to_string(&schema).expect("schema serialises to JSON");
    for kind in ["set", "call", "publish", "emit", "open", "refresh"] {
        assert!(
            json.contains(kind),
            "generated schema does not mention action kind '{kind}'"
        );
    }
}

// ── The schema is machine-checkable (review AC-F8, RS-S24) ──────────────────

fn validator() -> jsonschema::Validator {
    let schema = serde_json::to_value(schema_for!(SurfaceSpec)).unwrap();
    jsonschema::validator_for(&schema).expect("the emitted schema is a valid JSON Schema")
}

fn errors(validator: &jsonschema::Validator, spec: &serde_json::Value) -> Vec<String> {
    validator
        .iter_errors(spec)
        .map(|e| format!("{} at {}", e, e.instance_path))
        .collect()
}

#[test]
fn both_shipped_examples_validate_against_the_emitted_schema() {
    let validator = validator();
    for (name, text) in [
        (
            "signal-explorer",
            include_str!("../examples/signal-explorer.surface.json"),
        ),
        (
            "paper-triage",
            include_str!("../examples/paper-triage.surface.json"),
        ),
    ] {
        let spec: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(errors(&validator, &spec), Vec::<String>::new(), "{name}");
    }
}

/// A spec with one thing wrong, and the schema must say so — each of these
/// was accepted by the old `{"type": "object"}` Node and Source.
#[test]
fn known_bad_specs_fail_the_emitted_schema() {
    let validator = validator();
    let base = |root: serde_json::Value| serde_json::json!({ "surface": "1.0", "name": "bad", "state": {}, "root": root });
    let cases = [
        (
            "a table with no columns",
            base(serde_json::json!({ "table": { "rows": [] } })),
        ),
        (
            "two kind keys on one node",
            base(serde_json::json!({ "text": "a", "button": { "label": "b" } })),
        ),
        (
            "a misspelt key on a button",
            base(serde_json::json!({ "button": { "label": "b", "onclick": [] } })),
        ),
        (
            "an unknown action",
            base(
                serde_json::json!({ "button": { "label": "b", "on_click": [ { "launch": {} } ] } }),
            ),
        ),
        (
            "a column of strings",
            base(serde_json::json!({ "column": ["not a node"] })),
        ),
        (
            "a wrong surface version",
            serde_json::json!({ "surface": "2.0", "name": "x", "root": { "spacer": {} } }),
        ),
        (
            "an unknown top-level key",
            serde_json::json!({ "surface": "1.0", "name": "x", "root": { "spacer": {} }, "extra": 1 }),
        ),
        (
            "a source with two kinds",
            serde_json::json!({
                "surface": "1.0", "name": "x", "root": { "spacer": {} },
                "sources": { "s": { "value": 1, "verb": "a-service_b" } }
            }),
        ),
        (
            "a malformed verb name",
            serde_json::json!({
                "surface": "1.0", "name": "x", "root": { "spacer": {} },
                "sources": { "s": { "verb": "NotAVerb" } }
            }),
        ),
        (
            "a query source whose query has a misspelt key",
            serde_json::json!({
                "surface": "1.0", "name": "x", "root": { "spacer": {} },
                "sources": { "s": { "query": { "kind": ["publication"] } } }
            }),
        ),
    ];
    for (what, spec) in cases {
        assert!(
            !errors(&validator, &spec).is_empty(),
            "the schema accepted {what}: {spec}"
        );
    }
}

/// Forward compatibility: a kind this build has never heard of is valid (it
/// renders as a placeholder that keeps the node).
#[test]
fn an_unknown_node_kind_is_valid_against_the_schema() {
    let spec = serde_json::json!({
        "surface": "1.0", "name": "x",
        "root": { "column": [ { "sparkline": { "values": [1, 2] } }, { "spacer": {} } ] }
    });
    assert_eq!(errors(&validator(), &spec), Vec::<String>::new());
}
