//! Tier A over the inventory (docs/capability.md § 8): every verb is found
//! by name, documented, exemplified, and its example answers through the
//! pipeline on a memory store. The end-to-end tests over the demo binary
//! are `tests/tier_a.rs` (they need `CARGO_BIN_EXE_demo-code`).

use std::sync::{Arc, OnceLock};

use impress_service_core::call;
use impress_service_core::descriptor::Reach;
use impress_service_core::descriptor_handle::VerbHandle;
use koine_store_memory::MemoryStore;
use serde_json::{json, Value};

use crate::codes::{self, CodeProvider, DirVia, Launch, ParamsVia};

/// One store for the process; the tests share it.
fn install_store() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        koine_store::process::install(Arc::new(MemoryStore::new()));
        crate::kind::ensure_registered();
    });
}

fn contains(haystack: &Value, needle: &Value) -> bool {
    match (haystack, needle) {
        (Value::Object(h), Value::Object(n)) => n
            .iter()
            .all(|(k, v)| h.get(k).is_some_and(|hv| contains(hv, v))),
        (Value::Array(h), Value::Array(n)) => {
            h.len() == n.len() && h.iter().zip(n).all(|(a, b)| contains(a, b))
        }
        _ => haystack == needle,
    }
}

pub const VERBS: [&str; 13] = [
    "run-service_checkpoint",
    "run-service_codes",
    "run-service_events",
    "run-service_form",
    "run-service_ingest",
    "run-service_list",
    "run-service_register-code",
    "run-service_resume",
    "run-service_set",
    "run-service_start",
    "run-service_status",
    "run-service_stop",
    "run-service_sweep",
];

#[test]
fn every_verb_is_documented_has_an_example_and_the_example_answers() {
    install_store();
    let verbs: Vec<VerbHandle> = call::descriptors()
        .filter(|v| v.service() == "run-service")
        .collect();
    let mut names: Vec<&str> = verbs.iter().map(|v| v.name()).collect();
    names.sort_unstable();
    assert_eq!(names, VERBS);
    for verb in &verbs {
        assert!(!verb.description().is_empty(), "{}", verb.name());
        assert!(
            !verb.examples().is_empty(),
            "{} has no example",
            verb.name()
        );
        for example in verb.examples() {
            let label = format!("{} / {}", verb.name(), example.name);
            let result = call::call(verb.name(), example.args_value())
                .unwrap_or_else(|e| panic!("{label}: {e}"));
            let expect: Value =
                serde_json::from_str(example.expect.expect("every example states its expect"))
                    .unwrap();
            assert!(contains(&result, &expect), "{label}: {result} vs {expect}");
        }
    }
}

#[test]
fn safety_classes_are_what_the_plan_says() {
    use impress_service_core::descriptor::SafetyClass::*;
    let class = |name: &str| {
        call::descriptors()
            .find(|v| v.name() == name)
            .unwrap_or_else(|| panic!("{name} not linked"))
            .safety()
            .class
    };
    for read in ["codes", "list", "status", "events", "form"] {
        assert_eq!(class(&format!("run-service_{read}")), ReadOnly, "{read}");
    }
    for mutating in ["ingest", "set", "stop", "checkpoint", "sweep"] {
        assert_eq!(
            class(&format!("run-service_{mutating}")),
            Mutating,
            "{mutating}"
        );
    }
    // A verb that launches a process leaves this one (koine-host's effects
    // test: a `subprocess` reach is `external`).
    for external in ["start", "resume", "register-code"] {
        assert_eq!(
            class(&format!("run-service_{external}")),
            External,
            "{external}"
        );
    }
    let reach = |name: &str| -> Vec<Reach> {
        call::descriptors()
            .find(|v| v.name() == name)
            .unwrap()
            .effects()
            .reach
            .to_vec()
    };
    assert!(reach("run-service_start").contains(&Reach::Subprocess));
    assert!(reach("run-service_resume").contains(&Reach::Subprocess));
    assert!(reach("run-service_set").contains(&Reach::Fs));
    assert!(reach("run-service_codes").is_empty());
}

#[test]
fn a_misspelt_argument_is_refused_before_the_handler() {
    install_store();
    let result = call::call("run-service_start", json!({"code": "x", "parms": {}})).unwrap();
    assert_eq!(result["code"], json!("invalid-argument"), "{result}");
    let result = call::call("run-service_ingest", json!({"directory": "/x"})).unwrap();
    assert_eq!(result["code"], json!("invalid-argument"), "{result}");
}

fn schema() -> Value {
    serde_json::from_str(include_str!("../fixtures/demo-code.schema.json")).unwrap()
}

fn provider(name: &str) -> CodeProvider {
    CodeProvider {
        name: name.into(),
        version: "0".into(),
        schema: schema(),
        launch: Launch {
            argv: vec!["/bin/true".into()],
            params: ParamsVia::File,
            params_flag: "--params".into(),
            dir: DirVia::Flag,
            dir_name: "--dir".into(),
            problem_flag: None,
            resume_flag: None,
            env: Default::default(),
        },
        default_problem: json!({"name": "demo", "version": "1"}),
    }
}

#[test]
fn an_unknown_parameter_name_is_refused_and_named() {
    let p = provider("unit-unknown");
    let err = p
        .resolve(
            &json!({"demo.stepz": 3, "demo.steps": 2})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap_err();
    assert!(err.contains("unknown parameter `demo.stepz`"), "{err}");
    assert!(err.contains("demo.steps"), "{err}");
    let values = p
        .resolve(&json!({"demo.steps": 2}).as_object().unwrap().clone())
        .unwrap();
    assert_eq!(values["demo.steps"], json!(2));
    assert_eq!(values["demo.cadence"], json!(2), "defaults fill the rest");
    let err = p
        .resolve(&json!({"demo.steps": 0}).as_object().unwrap().clone())
        .unwrap_err();
    assert!(err.contains("demo.steps"), "{err}");
    let err = p
        .resolve(&json!({"demo.steps": "many"}).as_object().unwrap().clone())
        .unwrap_err();
    assert!(err.contains("demo.steps"), "{err}");
}

#[test]
fn mutability_is_read_from_the_schema_exactly() {
    let p = provider("unit-mutable");
    assert_eq!(p.mutability("demo.steps"), Some("never"));
    assert_eq!(p.mutability("demo.dt"), Some("restart"));
    assert_eq!(p.mutability("demo.cadence"), Some("always"));
    assert_eq!(p.mutability("demo.nope"), None);
    assert_eq!(p.metrics(), vec!["energy".to_string()]);
}

#[test]
fn a_provider_passing_parameters_by_env_needs_every_env_name() {
    let mut p = provider("unit-env");
    p.launch.params = ParamsVia::Env;
    let err = codes::validate(&p).unwrap_err();
    assert!(err.contains("demo.steps"), "{err}");
    let mut p = provider("unit-bad-schema");
    p.schema = json!({"type": "object", "properties": {"flat": {"type": "integer", "description": "not namespaced"}}});
    let err = codes::validate(&p).unwrap_err();
    assert!(err.contains("R-D2.1"), "{err}");
}

#[test]
fn the_form_validates_with_zero_errors_and_groups_by_x_run_group() {
    let p = provider("unit-form");
    let spec = crate::form::start_form(&p);
    let problems = impress_surface::validate::validate(&spec);
    let errors: Vec<_> = problems.iter().filter(|p| p.is_error()).collect();
    assert!(errors.is_empty(), "{errors:?}");
    let json = serde_json::to_value(&spec).unwrap();
    let text = json.to_string();
    for group in ["run", "io", "failure"] {
        assert!(
            text.contains(&format!("\"group-{group}\"")),
            "no section for group {group}: {text}"
        );
    }
    // Every parameter binds under state.params by its slug, and the Run
    // button maps the slug back to the dotted name.
    assert!(text.contains("state.params.demo_steps"), "{text}");
    assert!(
        text.contains("\"demo.steps\":\"{{state.params.demo_steps}}\""),
        "{text}"
    );
    // A unit on the label; an all-advanced group collapsed.
    assert!(text.contains("demo.dt (s)"), "{text}");
    let failure = find_section(&json["root"], "group-failure").expect("failure section");
    assert_eq!(failure["section"]["collapsed"], json!(true), "{failure}");
    let run = find_section(&json["root"], "group-run").expect("run section");
    assert_eq!(run["section"]["collapsed"], json!(false), "{run}");
}

fn find_section<'a>(node: &'a Value, id: &str) -> Option<&'a Value> {
    if node.get("id").and_then(Value::as_str) == Some(id) {
        return Some(node);
    }
    if let Some(children) = node.get("column").and_then(Value::as_array) {
        return children.iter().find_map(|c| find_section(c, id));
    }
    if let Some(body) = node.pointer("/section/body") {
        return find_section(body, id);
    }
    None
}

#[test]
fn a_sweep_expands_product_zip_and_generators() {
    let n = |s: Value| crate::sweep::expand(&s).map(|r| r.len());
    assert_eq!(
        n(json!({"base": {"a.x": 1}, "vary": [
            {"param": "a.y", "values": [1, 2, 3]},
            {"param": "a.z", "generator": {"kind": "linspace", "start": 0, "stop": 1, "n": 4}}
        ]}))
        .unwrap(),
        12
    );
    assert_eq!(
        n(json!({"base": {}, "combine": "zip", "vary": [
            {"param": "a.y", "values": [1, 2]},
            {"param": "a.z", "values": [3, 4]}
        ]}))
        .unwrap(),
        2
    );
    assert_eq!(
        n(json!({"base": {}, "vary": [
            {"param": "a.z", "generator": {"kind": "grid", "start": 0, "stop": 1, "step": 0.25}}
        ]}))
        .unwrap(),
        5
    );
    let runs = crate::sweep::expand(&json!({"base": {"a.x": 1}, "vary": [
        {"param": "a.z", "generator": {"kind": "logspace", "start": 1, "stop": 100, "n": 3}}
    ]}))
    .unwrap();
    assert!((runs[1]["a.z"].as_f64().unwrap() - 10.0).abs() < 1e-9);
    assert_eq!(runs[1]["a.x"], json!(1));
    let err = crate::sweep::expand(&json!({"base": {}, "vary": [
        {"param": "a.z", "generator": {"kind": "sampler", "seed": 1}}
    ]}))
    .unwrap_err();
    assert_eq!(err.code, "unsupported");
    let err = crate::sweep::expand(&json!({"base": {}, "combine": "zip", "vary": [
        {"param": "a.y", "values": [1, 2]},
        {"param": "a.z", "values": [3]}
    ]}))
    .unwrap_err();
    assert_eq!(err.code, "invalid-argument");
}

#[test]
fn the_run_kind_is_registered_and_compatible_with_itself() {
    crate::kind::ensure_registered();
    crate::kind::ensure_registered();
    let s = impress_item::kind::get(crate::kind::RUN.as_str()).expect("registered");
    assert_eq!(s.version, "1.0.0");
    assert!(s.fields.iter().any(|f| f.name == "dir" && f.required));
}
