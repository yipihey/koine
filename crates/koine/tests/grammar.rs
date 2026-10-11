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
        source: Source::Linked,
        handler: |_| Box::pin(async { Ok(Value::Null) }),
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
fn the_adverb_vocabulary_is_the_six_the_grammar_lists() {
    let spelled: Vec<&str> = Adverb::ALL.iter().map(|a| a.as_str()).collect();
    assert_eq!(
        spelled,
        [
            "tentatively",
            "with_review",
            "once",
            "reversibly",
            "within",
            "in_background"
        ]
    );
    assert_eq!(Adverbs::KEY, "adverbs");
}
