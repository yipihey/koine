//! The agent desk and the skill over a memory store, with the layout and
//! surface runtimes linked as a host links them
//! (docs/plan-koine-research-os.md § What pins this). Process-wide seams, so
//! the tests take turns.

use std::sync::{Arc, Mutex, MutexGuard};

use impress_item::AttributedStore;
use impress_service_core::{call, VerbHandle};
use koine_agent::{skill, AppInfo};
use koine_store::RecordingStore;
use koine_store_memory::MemoryStore;
use serde_json::{json, Value};

static TURN: Mutex<()> = Mutex::new(());

fn fresh() -> MutexGuard<'static, ()> {
    let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    std::hint::black_box([
        impress_layout_service::DefaultLayoutService::new as *const (),
        impress_surface_service::DefaultImpressSurfaceService::new as *const (),
    ]);
    koine_agent::ensure_linked();
    koine_notebook::ensure_linked();
    let store: Arc<dyn AttributedStore> =
        Arc::new(RecordingStore::new(Arc::new(MemoryStore::new())));
    koine_store::process::install(store.clone());
    // The ui kinds a host's manifest has (koine_host::manifest::minimal).
    impress_pane_query::process_manifest::install(Arc::new(impress_pane_query::KindManifest {
        kinds: [
            (
                "layout",
                impress_item::schema::refs::IMPRESS_UI_LAYOUT.as_str(),
            ),
            (
                "surface",
                impress_item::schema::refs::IMPRESS_UI_SURFACE.as_str(),
            ),
            ("note", "note"),
        ]
        .into_iter()
        .map(|(kind, r)| (kind.into(), vec![r.to_string()]))
        .collect(),
        ..Default::default()
    }));
    koine_notebook::install(
        store,
        koine_notebook::SinkConfig::new("h", "desk-test", "1"),
    );
    koine_agent::set_app(AppInfo {
        app_id: "desk-test".into(),
        version: "1.2.3".into(),
        kinds: vec!["note".into()],
    });
    turn
}

fn verb(name: &str, args: Value) -> Value {
    call::call(name, args).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn contains(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => e
            .iter()
            .all(|(key, want)| a.get(key).is_some_and(|got| contains(got, want))),
        _ => actual == expected,
    }
}

#[test]
fn every_verb_is_documented_and_every_example_answers_its_expect() {
    let _turn = fresh();
    let verbs: Vec<VerbHandle> = call::descriptors()
        .filter(|v| v.service() == "agent-service")
        .collect();
    assert_eq!(verbs.len(), 11);
    for verb in verbs {
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
            match example.expect {
                Some(expect) => {
                    let expect: Value = serde_json::from_str(expect).unwrap();
                    assert!(contains(&result, &expect), "{label}: {result} vs {expect}");
                }
                None => assert_ne!(result["ok"], json!(false), "{label}: {result}"),
            }
        }
    }
}

#[test]
fn the_skill_names_every_linked_verb_and_is_the_same_twice() {
    let _turn = fresh();
    let app = koine_agent::app();
    let first = skill::render(&app);
    assert_eq!(first, skill::render(&app));
    assert!(first.starts_with("---\nname: desk-test\n"), "{first}");
    assert!(first.contains(skill::GENERATED_MARK));
    for verb in call::descriptors() {
        assert!(
            first.contains(&format!("`{}`", verb.name())),
            "{} missing",
            verb.name()
        );
    }
    assert!(first.contains("- `note`"));
    let answered = verb("agent-service_skill", json!({}));
    assert_eq!(answered["markdown"], json!(first));
    assert_eq!(answered["name"], "desk-test");
}

#[test]
fn skill_install_writes_where_agents_look_and_keeps_a_foreign_file() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    std::fs::create_dir_all(home.path().join(".agents")).unwrap();
    let targets = skill::default_targets(home.path());
    assert_eq!(
        targets,
        vec![
            home.path().join(".claude/skills"),
            home.path().join(".agents/skills")
        ]
    );
    let text = format!("---\nname: x\n---\n<!-- {} -->\n", skill::GENERATED_MARK);
    let written = skill::install(&text, "x", &targets, false);
    assert!(written
        .iter()
        .all(|w| matches!(w, skill::Installed::Written(_))));
    // A file the person wrote themselves is kept, unless forced.
    let foreign = home.path().join(".claude/skills/x/SKILL.md");
    std::fs::write(&foreign, "mine").unwrap();
    let again = skill::install(&text, "x", &targets, false);
    assert!(matches!(&again[0], skill::Installed::KeptForeign(p) if *p == foreign));
    assert_eq!(std::fs::read_to_string(&foreign).unwrap(), "mine");
    skill::install(&text, "x", &targets, true);
    assert!(std::fs::read_to_string(&foreign)
        .unwrap()
        .contains(skill::GENERATED_MARK));
}

#[test]
fn a_request_moves_from_ask_to_answer_and_its_surface_can_be_shown() {
    let _turn = fresh();
    let asked = verb(
        "agent-service_ask",
        json!({"prompt": "Summarise the selected notes"}),
    );
    assert_eq!(asked["ok"], true, "{asked}");
    let id = asked["request"]["id"].as_str().unwrap().to_string();
    assert_eq!(asked["request"]["context"]["kind"], "pointing");
    assert_eq!(
        asked["request"]["context"]["pointing"]["app_id"],
        "desk-test"
    );

    // An impress app names its own layout as the context.
    let other = verb(
        "agent-service_ask",
        json!({"prompt": "and here?", "app_id": "imbib"}),
    );
    assert_eq!(other["request"]["context"]["pointing"]["app_id"], "imbib");
    verb(
        "agent-service_decline",
        json!({"id": other["request"]["id"], "reason": "test"}),
    );

    let next = verb("agent-service_next", json!({"timeout_ms": 0}));
    assert_eq!(next["request"]["id"], id);
    let claimed = call::call_as(
        "agent-service_claim",
        impress_service_core::pipeline::CallerIdentity::agent("claude"),
        json!({"id": id}),
    )
    .unwrap();
    assert_eq!(claimed["request"]["status"], "claimed", "{claimed}");
    assert_eq!(claimed["request"]["claimed_by"], "agent:claude");
    // Not open any more: a second claim is a conflict, and next skips it.
    let again = verb("agent-service_claim", json!({"id": id}));
    assert_eq!(again["code"], "conflict");
    assert!(verb("agent-service_next", json!({}))["request"].is_null());

    let spec = json!({"surface": "1.0", "name": "Answer", "state": {}, "sources": {},
                      "root": {"column": [{"text": "Three notes, one theme."}]}});
    let created = verb(
        "impress-surface-service_surface-create",
        json!({"spec": spec, "name": "Answer"}),
    );
    let surface = created["id"].as_str().unwrap().to_string();
    let answered = verb(
        "agent-service_answer",
        json!({"id": id, "text": "See the surface.", "surface_id": surface}),
    );
    assert_eq!(answered["request"]["status"], "answered");
    assert_eq!(answered["request"]["surface_id"], surface);

    let listed = verb("agent-service_requests", json!({"status": "answered"}));
    assert_eq!(listed["rows"][0]["id"], id);
    assert_eq!(listed["rows"][0]["context"], Value::Null);
    let shown = verb("agent-service_show", json!({"id": id}));
    assert_eq!(shown["ok"], true, "{shown}");
}

#[test]
fn diagnose_carries_the_calls_record_and_make_surface_what_to_build_with() {
    let _turn = fresh();
    verb("notebook-service_run-start", json!({"name": "d"}));
    let timeline = verb(
        "notebook-service_timeline",
        json!({"verb": "notebook-service_run-start"}),
    );
    let call_id = timeline["rows"][0]["call_id"].as_str().unwrap().to_string();
    let filed = verb("agent-service_diagnose", json!({"call_id": call_id}));
    assert_eq!(filed["ok"], true, "{filed}");
    let request = &filed["request"];
    assert_eq!(request["kind"], "diagnose");
    assert_eq!(request["call_id"], call_id);
    assert_eq!(
        request["context"]["record"]["verb"],
        "notebook-service_run-start"
    );
    assert_eq!(request["context"]["verb"]["safety"], "mutating");
    let prompt = request["prompt"].as_str().unwrap();
    assert!(
        prompt.contains("run-start") && prompt.contains("where its time went"),
        "{prompt}"
    );
    verb("notebook-service_run-stop", json!({}));

    let made = verb(
        "agent-service_make-surface",
        json!({"description": "A table of notes"}),
    );
    let context = &made["request"]["context"];
    assert_eq!(made["request"]["kind"], "make-surface");
    assert!(context["verbs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["name"] == "impress-surface-service_surface-create"));
    let example = &context["example_spec"];
    let checked = verb(
        "impress-surface-service_surface-validate",
        json!({"spec": example}),
    );
    assert_eq!(checked["ok"], true, "{checked}");
}

#[test]
fn the_agent_desk_surface_is_a_valid_spec() {
    let spec: Value = serde_json::from_str(koine_agent::AGENT_DESK_SURFACE).unwrap();
    assert_eq!(spec["name"], koine_agent::AGENT_DESK_SURFACE_NAME);
    let (parsed, problems) = impress_surface::validate_json(&spec);
    let errors: Vec<_> = problems
        .iter()
        .filter(|p| p.severity == impress_surface::Severity::Error)
        .collect();
    assert!(parsed.is_some() && errors.is_empty(), "{problems:?}");
}
