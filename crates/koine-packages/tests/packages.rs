//! Shipped surfaces and packages over a memory store with the surface
//! runtime linked (docs/plan-koine-research-os.md § What pins this).
//! Process-wide seams, so the tests take turns.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use impress_item::AttributedStore;
use impress_service_core::{call, VerbHandle};
use koine_store_memory::MemoryStore;
use serde_json::{json, Value};

static TURN: Mutex<()> = Mutex::new(());

fn fresh() -> MutexGuard<'static, ()> {
    let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    std::hint::black_box(impress_surface_service::DefaultImpressSurfaceService::new as *const ());
    koine_packages::ensure_linked();
    let store: Arc<dyn AttributedStore> = Arc::new(MemoryStore::new());
    koine_store::process::install(store);
    impress_pane_query::process_manifest::install(Arc::new(impress_pane_query::KindManifest {
        kinds: [(
            "surface",
            impress_item::schema::refs::IMPRESS_UI_SURFACE.as_str(),
        )]
        .into_iter()
        .map(|(kind, r)| (kind.into(), vec![r.to_string()]))
        .collect(),
        ..Default::default()
    }));
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

fn spec(text: &str) -> Value {
    json!({"surface": "1.0", "name": "Triage", "state": {}, "sources": {},
           "root": {"column": [{"id": "t", "text": text}]}})
}

/// Write a one-surface package `triage` at `version` into `dir`.
fn write_package(dir: &Path, version: &str, text: &str) {
    std::fs::create_dir_all(dir.join("surfaces")).unwrap();
    std::fs::write(
        dir.join("koine-package.json"),
        serde_json::to_vec_pretty(&json!({
            "package": "1.0", "name": "triage", "version": version,
            "description": "Paper triage",
            "surfaces": [{"name": "Triage", "file": "surfaces/triage.surface.json"}],
            "skills": [{"name": "triage", "file": "SKILL.md"}],
            "requires": {"verbs": ["scix-service_search"], "secrets": []}
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.join("surfaces/triage.surface.json"),
        serde_json::to_vec(&spec(text)).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.join("SKILL.md"), "---\nname: triage\n---\n").unwrap();
}

fn install(dir: &Path) -> Value {
    let path = dir.display().to_string();
    let review = verb("package-service_inspect", json!({"path": path}));
    assert_eq!(review["ok"], true, "{review}");
    verb(
        "package-service_install",
        json!({"path": path, "digest": review["digest"]}),
    )
}

fn copy_text(surface: &str) -> Value {
    verb(
        "impress-surface-service_surface-get",
        json!({"id": surface}),
    )["spec"]["root"]["column"][0]["text"]
        .clone()
}

fn status_of(surface: &str) -> String {
    let rows = verb("package-service_shipped-status", json!({}));
    rows["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["surface_id"] == surface)
        .map(|r| r["status"].as_str().unwrap().to_string())
        .unwrap()
}

#[test]
fn every_verb_is_documented_and_every_example_answers_its_expect() {
    let _turn = fresh();
    let verbs: Vec<VerbHandle> = call::descriptors()
        .filter(|v| v.service() == "package-service")
        .collect();
    assert_eq!(verbs.len(), 9);
    for verb in verbs {
        assert!(!verb.description().is_empty(), "{}", verb.name());
        for example in verb.examples() {
            let label = format!("{} / {}", verb.name(), example.name);
            let result = call::call(verb.name(), example.args_value()).unwrap();
            let expect: Value = serde_json::from_str(example.expect.unwrap()).unwrap();
            assert!(contains(&result, &expect), "{label}: {result} vs {expect}");
        }
    }
}

#[test]
fn the_kit_installs_its_surfaces_once_and_they_validate() {
    let _turn = fresh();
    for (name, _, json) in koine_packages::kit::surfaces() {
        let spec: Value = serde_json::from_str(json).unwrap();
        let checked = verb(
            "impress-surface-service_surface-validate",
            json!({"spec": spec}),
        );
        assert_eq!(checked["ok"], true, "{name}: {checked}");
    }
    let first = koine_packages::kit::install_blocking().unwrap();
    assert_eq!(first.surfaces.len(), 3);
    assert!(first
        .surfaces
        .iter()
        .all(|(_, _, o)| *o == koine_packages::shipped::Outcome::Created));
    let again = koine_packages::kit::install_blocking().unwrap();
    assert!(again
        .surfaces
        .iter()
        .all(|(_, _, o)| *o == koine_packages::shipped::Outcome::Unchanged));
    assert_eq!(first.surfaces[0].1, again.surfaces[0].1);
    let listed = verb("package-service_list", json!({}));
    assert_eq!(listed["packages"][0]["name"], "koine-kit");
    assert_eq!(listed["packages"][0]["source"], "builtin:koine-kit");
}

#[test]
fn an_upgrade_is_three_way_and_reset_and_take_are_reversible() {
    let _turn = fresh();
    let dir = tempfile::tempdir().unwrap();
    write_package(dir.path(), "1.0.0", "one");
    let installed = install(dir.path());
    assert_eq!(
        installed["surfaces"][0]["outcome"], "created",
        "{installed}"
    );
    let surface = installed["surfaces"][0]["surface_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(status_of(&surface), "pristine");

    // Untouched by the person: a new version replaces it.
    write_package(dir.path(), "1.1.0", "two");
    let upgraded = install(dir.path());
    assert_eq!(upgraded["surfaces"][0]["outcome"], "updated", "{upgraded}");
    assert_eq!(copy_text(&surface), "two");

    // Changed by the person: kept, the new version waits.
    let mut mine = spec("mine");
    mine["name"] = json!("Triage");
    verb(
        "impress-surface-service_surface-update",
        json!({"id": surface, "spec": mine}),
    );
    assert_eq!(status_of(&surface), "modified");
    let diff = verb("package-service_shipped-diff", json!({"id": surface}));
    assert_eq!(
        diff["changes"][0]["pointer"], "/root/column/0/text",
        "{diff}"
    );
    assert_eq!(diff["changes"][0]["shipped"], "\"two\"");
    assert_eq!(diff["changes"][0]["yours"], "\"mine\"");

    write_package(dir.path(), "1.2.0", "three");
    let diverged = install(dir.path());
    assert_eq!(diverged["surfaces"][0]["outcome"], "diverged");
    assert_eq!(copy_text(&surface), "mine");
    assert_eq!(status_of(&surface), "diverged");

    // Take the waiting one; the undo gives "mine" back.
    let taken = verb(
        "package-service_shipped-take",
        json!({"id": surface, "adverbs": {"reversibly": true}}),
    );
    assert_eq!(taken["ok"], true, "{taken}");
    assert_eq!(copy_text(&surface), "three");
    assert_eq!(taken["row"]["version"], "1.2.0");
    let undo = &taken["undo"];
    assert_eq!(undo["verb"], "impress-surface-service_surface-update");
    verb(undo["verb"].as_str().unwrap(), undo["args"].clone());
    assert_eq!(copy_text(&surface), "mine");

    // Reset puts the shipped one back, reversibly.
    let reset = verb(
        "package-service_shipped-reset",
        json!({"id": surface, "adverbs": {"reversibly": true}}),
    );
    assert_eq!(reset["row"]["status"], "pristine", "{reset}");
    assert_eq!(copy_text(&surface), "three");
    assert_eq!(
        reset["undo"]["args"]["spec"]["root"]["column"][0]["text"],
        "mine"
    );

    // Nothing waiting now: take and keep refuse.
    let nothing = verb("package-service_shipped-keep", json!({"id": surface}));
    assert_eq!(nothing["code"], "conflict");
}

#[test]
fn keep_makes_the_waiting_version_the_one_reset_restores() {
    let _turn = fresh();
    let dir = tempfile::tempdir().unwrap();
    write_package(dir.path(), "1.0.0", "one");
    let surface = install(dir.path())["surfaces"][0]["surface_id"]
        .as_str()
        .unwrap()
        .to_string();
    verb(
        "impress-surface-service_surface-update",
        json!({"id": surface, "spec": spec("mine")}),
    );
    write_package(dir.path(), "2.0.0", "two");
    install(dir.path());
    let kept = verb("package-service_shipped-keep", json!({"id": surface}));
    assert_eq!(kept["row"]["status"], "modified", "{kept}");
    assert_eq!(copy_text(&surface), "mine");
    verb("package-service_shipped-reset", json!({"id": surface}));
    assert_eq!(copy_text(&surface), "two");
}

#[test]
fn what_is_installed_is_what_was_reviewed() {
    let _turn = fresh();
    let dir = tempfile::tempdir().unwrap();
    write_package(dir.path(), "1.0.0", "one");
    let path = dir.path().display().to_string();
    let review = verb("package-service_inspect", json!({"path": path}));
    assert_eq!(review["valid"], true);
    assert_eq!(review["surfaces"][0]["would"], "create");
    assert_eq!(review["missing_verbs"], json!(["scix-service_search"]));
    assert_eq!(review["skills"], json!(["triage"]));
    // The directory changes after the review.
    write_package(dir.path(), "1.0.0", "swapped");
    let refused = verb(
        "package-service_install",
        json!({"path": path, "digest": review["digest"]}),
    );
    assert_eq!(refused["code"], "digest-mismatch", "{refused}");
    assert_eq!(
        verb("package-service_list", json!({}))["packages"],
        json!([])
    );

    // A path outside the package is refused at review.
    std::fs::write(
        dir.path().join("koine-package.json"),
        r#"{"package": "1.0", "name": "bad", "version": "1",
            "surfaces": [{"name": "X", "file": "../x.json"}]}"#,
    )
    .unwrap();
    let outside = verb("package-service_inspect", json!({"path": path}));
    assert_eq!(outside["code"], "invalid-argument", "{outside}");
}

#[test]
fn removing_a_package_keeps_what_the_person_changed() {
    let _turn = fresh();
    let a = tempfile::tempdir().unwrap();
    write_package(a.path(), "1.0.0", "one");
    let surface = install(a.path())["surfaces"][0]["surface_id"]
        .as_str()
        .unwrap()
        .to_string();
    verb(
        "impress-surface-service_surface-update",
        json!({"id": surface, "spec": spec("mine")}),
    );
    let removed = verb("package-service_remove", json!({"name": "triage"}));
    assert_eq!(removed["kept"], json!([surface]), "{removed}");
    assert_eq!(copy_text(&surface), "mine");
    assert_eq!(
        verb("package-service_shipped-status", json!({}))["rows"],
        json!([])
    );

    // A pristine one is deleted with its package.
    write_package(a.path(), "1.0.0", "fresh");
    let pristine = install(a.path())["surfaces"][0]["surface_id"]
        .as_str()
        .unwrap()
        .to_string();
    let removed = verb("package-service_remove", json!({"name": "triage"}));
    assert_eq!(removed["deleted"], json!([pristine]), "{removed}");
    let gone = verb(
        "impress-surface-service_surface-get",
        json!({"id": pristine}),
    );
    assert_eq!(gone["code"], "not-found");
}
