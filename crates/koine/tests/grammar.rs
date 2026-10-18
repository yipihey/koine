//! Pins for docs/grammar.md § Adverbs ("What pins this").

use koine::surface::spec::{walk_actions, Action, SurfaceSpec};
use koine::verbs::descriptor::{Effects, Safety, SafetyClass, Source, VerbDescriptor};
use koine::verbs::{Adverb, Adverbs};
use serde_json::{json, Value};

fn fixture(class: SafetyClass) -> VerbDescriptor {
    VerbDescriptor {
        name: "fixture-service_x",
        service: "fixture-service",
        method: "x",
        description: "A fixture.",
        input_schema: || json!({"type": "object", "properties": {"id": {"type": "string"}}}),
        output_schema: || json!({"type": "object", "properties": {"ok": {"type": "boolean"}}}),
        safety: Safety {
            class,
            idempotent: class.default_idempotent(),
        },
        effects: Effects::NONE,
        since: "0.1.0",
        deprecated: None,
        aliases: &[],
        examples: &[],
        strict: false,
        budget_ms: None,
        replay_full: false,
        supports: &[Adverb::Reversibly, Adverb::Within, Adverb::InBackground],
        adjective: None,
        returns_kind: None,
        rows: None,
        secrets: &[],
        source: Source::Linked,
        handler: |_| Box::pin(async { Ok(Value::Null) }),
        consent: ::impress_service_core::Consent::DEFAULT,
        intent: ::impress_service_core::IntentDecl::NONE,
    }
}

fn assert_adverb_keys_parse(label: &str, spec: &SurfaceSpec) -> usize {
    let mut seen = 0;
    for (at, action) in walk_actions(&spec.root) {
        if let Action::Call {
            adverbs: Some(adverbs),
            ..
        } = action
        {
            let map = adverbs
                .as_object()
                .unwrap_or_else(|| panic!("{label} {at}: adverbs is not an object"));
            for key in map.keys() {
                assert!(
                    Adverb::parse(key).is_some(),
                    "{label} {at}: '{key}' is not in the adverb vocabulary"
                );
                seen += 1;
            }
        }
    }
    seen
}

#[test]
fn every_adverb_key_a_surface_example_uses_is_in_the_pipelines_vocabulary() {
    let examples = [
        ("signal_explorer", koine::surface::example_signal_explorer()),
        ("paper_triage", koine::surface::example_paper_triage()),
        ("job_monitor", koine::surface::example_job_monitor()),
        ("catalogue", koine::verb_surface::catalogue()),
    ];
    for (label, spec) in &examples {
        assert_adverb_keys_parse(label, spec);
    }
    let mut generated = 0;
    for class in [
        SafetyClass::ReadOnly,
        SafetyClass::Mutating,
        SafetyClass::Destructive,
    ] {
        let spec = koine::verb_surface::verb_surface(&fixture(class));
        generated += assert_adverb_keys_parse("generated", &spec);
    }
    assert!(generated > 0, "the generated forms carried no adverbs");
}

#[test]
fn the_adverb_vocabulary_is_the_seven_the_grammar_lists() {
    let spelled: Vec<&str> = Adverb::ALL.iter().map(|a| a.as_str()).collect();
    assert_eq!(
        spelled,
        [
            "tentatively",
            "with_review",
            "once",
            "reversibly",
            "within",
            "in_background",
            "every"
        ]
    );
    assert_eq!(Adverbs::KEY, "adverbs");
}

/// docs/grammar.md § Verbs return rows. A `returns_kind` is the pane-query
/// kind id of a verb's rows. The macro can only check that it is well formed
/// (`is_kind_id`: the host's manifest does not exist at expansion time); a
/// host checks the rest against its own manifest, which this test does for a
/// demo one.
#[test]
fn every_declared_returns_kind_is_a_well_formed_kind_id_and_a_demo_manifest_kind() {
    use koine::pane_query::KindManifest;
    use koine::verbs::is_kind_id;

    let mut demo = KindManifest::default();
    for kind in ["publication", "task", "collection"] {
        demo.kinds
            .insert(kind.to_string(), vec![format!("demo/{kind}")]);
    }

    // The linked inventory (empty in this test binary unless a service is
    // linked; the rule is the same for each verb that is).
    for verb in VerbDescriptor::iter() {
        if let Some(kind) = verb.returns_kind {
            assert!(
                is_kind_id(kind),
                "{}: returns_kind `{kind}` is not a well-formed kind id",
                verb.name
            );
        }
    }

    // A declared kind is carried to the surface and the listing, and a host
    // can resolve it: `publication` is in the demo manifest, `Publication!`
    // is not even well formed.
    let mut rows = fixture(SafetyClass::ReadOnly);
    rows.output_schema = || json!({"type": "array", "items": {"type": "object", "properties": {"id": {"type": "string"}}}});
    rows.returns_kind = Some("publication");
    assert!(is_kind_id(rows.returns_kind.unwrap()));
    assert!(demo.schema_refs(rows.returns_kind.unwrap()).is_some());
    assert_eq!(
        rows.mcp_meta(),
        Some(json!({"returns_kind": "publication"}))
    );
    let spec = koine::verb_surface::verb_surface(&rows);
    assert!(
        serde_json::to_string(&spec)
            .unwrap()
            .contains("Result (publication)"),
        "the generated table is labelled with the kind"
    );
    assert!(!is_kind_id("Publication!"));
    assert!(demo.schema_refs("nonesuch").is_none());
}

/// docs/grammar.md § Verbs return rows, `rows(…)`. The declared id and
/// columns reach `_meta.rows` and the generated table, and a declaration the
/// schema cannot honour fails `row_schema_check`.
#[test]
fn a_rows_declaration_reaches_meta_and_the_table_and_is_checked_against_the_schema() {
    use koine::verbs::descriptor::RowSchema;

    let mut verb = fixture(SafetyClass::ReadOnly);
    verb.output_schema = || {
        json!({"type": "array", "items": {"type": "object", "properties": {
            "bibcode": {"type": "string"}, "title": {"type": "string"}, "year": {"type": "integer"}
        }}})
    };
    verb.returns_kind = Some("paper");
    verb.rows = Some(RowSchema {
        id: "bibcode",
        title: None,
        columns: &[],
    });
    assert_eq!(verb.row_schema_check(), Ok(()));
    assert_eq!(
        verb.mcp_meta(),
        Some(json!({
            "returns_kind": "paper",
            "rows": {"id": "bibcode", "title": "bibcode", "columns": ["bibcode", "title", "year"]},
        }))
    );
    let spec = koine::verb_surface::verb_surface(&verb);
    let wire = serde_json::to_string(&spec).unwrap();
    assert!(wire.contains(r#""id_field":"bibcode""#), "{wire}");
    assert!(wire.contains("state.selected"), "{wire}");

    verb.rows = Some(RowSchema {
        id: "doi",
        title: None,
        columns: &[],
    });
    let err = verb.row_schema_check().unwrap_err();
    assert!(err.contains("id `doi` is not a property"), "{err}");
}
