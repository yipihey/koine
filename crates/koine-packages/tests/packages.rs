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
    // The Review surface calls the review verbs (plan-koine-trust TR-D8).
    koine_tasks::ensure_linked();
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
    assert_eq!(verbs.len(), 15);
    for verb in verbs {
        assert!(!verb.description().is_empty(), "{}", verb.name());
        for example in verb.examples() {
            let label = format!("{} / {}", verb.name(), example.name);
            let result = call::call(verb.name(), example.args_value()).unwrap();
            let expect: Value = serde_json::from_str(example.expect.unwrap())
                .unwrap_or_else(|_| panic!("{label} has no expect"));
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
    assert_eq!(first.surfaces.len(), 4);
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

    // So is a symlink inside the package that points outside it.
    #[cfg(unix)]
    {
        let secret = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(secret.path(), "{}").unwrap();
        write_package(dir.path(), "1.0.0", "one");
        let link = dir.path().join("surfaces/triage.surface.json");
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(secret.path(), &link).unwrap();
        let linked = verb("package-service_inspect", json!({"path": path}));
        assert_eq!(linked["code"], "invalid-argument", "{linked}");
        assert!(linked["message"].as_str().unwrap().contains("outside"));
    }
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

// ------------------------------------------------------------------ registry

/// Publish `write_package(…, version, text)` into the registry at `registry`.
fn publish(registry: &Path, version: &str, text: &str) -> koine_packages::registry::Published {
    let src = tempfile::tempdir().unwrap();
    write_package(src.path(), version, text);
    koine_packages::registry::publish(
        src.path(),
        registry,
        Some(&registry.join("index.json")),
        None,
        "test-registry",
    )
    .unwrap()
}

#[test]
fn a_package_goes_from_a_registry_to_installed_at_one_digest() {
    let _turn = fresh();
    let registry = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    koine_packages::registry::set_cache_dir(cache.path().to_path_buf());
    let published = publish(registry.path(), "1.0.0", "one");
    assert!(published.bundle.ends_with("triage-1.0.0.koinepkg.json"));

    let url = registry.path().display().to_string();
    let added = verb("package-service_registry-add", json!({"url": url}));
    assert_eq!(added["ok"], true, "{added}");
    assert_eq!(added["packages"], 1);
    assert_eq!(added["registries"][0]["name"], "test-registry");

    let found = verb("package-service_search", json!({"query": "triage"}));
    assert_eq!(found["rows"][0]["name"], "triage", "{found}");
    assert_eq!(found["rows"][0]["state"], "install");
    assert_eq!(found["rows"][0]["digest"], json!(published.entry.digest));

    // Fetch answers the review, at the digest the index pins.
    let review = verb("package-service_fetch", json!({"name": "triage"}));
    assert_eq!(review["ok"], true, "{review}");
    assert_eq!(review["digest"], json!(published.entry.digest));
    let path = review["path"].as_str().unwrap().to_string();
    assert!(
        path.starts_with(&cache.path().display().to_string()),
        "{path}"
    );
    let installed = verb(
        "package-service_install",
        json!({"path": path, "digest": review["digest"]}),
    );
    assert_eq!(
        installed["surfaces"][0]["outcome"], "created",
        "{installed}"
    );
    assert_eq!(
        verb("package-service_search", json!({}))["rows"][0]["state"],
        "installed"
    );

    // A newer version is an update, and fetching without a version takes it.
    publish(registry.path(), "1.1.0", "two");
    let updates = verb("package-service_updates", json!({}));
    assert_eq!(updates["rows"][0]["version"], "1.1.0", "{updates}");
    assert_eq!(updates["rows"][0]["state"], "update");
    let newer = verb("package-service_fetch", json!({"name": "triage"}));
    assert_eq!(newer["version"], "1.1.0");
    assert_eq!(newer["surfaces"][0]["would"], "update", "{newer}");
    let older = verb(
        "package-service_fetch",
        json!({"name": "triage", "version": "1.0.0"}),
    );
    assert_eq!(older["version"], "1.0.0");

    let removed = verb(
        "package-service_registry-remove",
        json!({"url": "test-registry"}),
    );
    assert_eq!(removed["registries"], json!([]), "{removed}");
    assert_eq!(verb("package-service_search", json!({}))["rows"], json!([]));
}

#[test]
fn a_bundle_that_is_not_what_the_index_pins_is_refused() {
    let _turn = fresh();
    let registry = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    koine_packages::registry::set_cache_dir(cache.path().to_path_buf());
    let published = publish(registry.path(), "1.0.0", "one");
    // Someone swaps the bundle on the server after the index was written.
    let mut bundle: Value =
        serde_json::from_slice(&std::fs::read(&published.bundle).unwrap()).unwrap();
    let file = "surfaces/triage.surface.json";
    bundle["files"][file] = json!(serde_json::to_string(&spec("tampered")).unwrap());
    std::fs::write(&published.bundle, serde_json::to_vec(&bundle).unwrap()).unwrap();

    verb(
        "package-service_registry-add",
        json!({"url": registry.path().display().to_string()}),
    );
    let refused = verb("package-service_fetch", json!({"name": "triage"}));
    assert_eq!(refused["code"], "digest-mismatch", "{refused}");

    // An unreadable registry is reported, not fatal.
    std::fs::remove_file(registry.path().join("index.json")).unwrap();
    let found = verb("package-service_search", json!({}));
    assert_eq!(found["ok"], true);
    assert_eq!(found["unreadable"].as_array().unwrap().len(), 1, "{found}");
}

#[test]
fn a_bundle_round_trips_with_the_directorys_digest() {
    let dir = tempfile::tempdir().unwrap();
    write_package(dir.path(), "1.0.0", "one");
    let package = koine_packages::package::Package::load(dir.path()).unwrap();
    let bundle = package.to_bundle().unwrap();
    let back = koine_packages::package::Package::from_bundle(&bundle, "test").unwrap();
    assert_eq!(back.digest(), package.digest());
    assert_eq!(back.manifest, package.manifest);
    // A bundle carrying a file its manifest does not name is refused.
    let mut extra: Value = serde_json::from_slice(&bundle).unwrap();
    extra["files"]["sneaky.json"] = json!("{}");
    let refused =
        koine_packages::package::Package::from_bundle(&serde_json::to_vec(&extra).unwrap(), "test")
            .unwrap_err();
    assert!(
        refused.message.contains("sneaky.json"),
        "{}",
        refused.message
    );
}

/// The same flow over HTTP, from a minimal server that serves a directory.
#[test]
fn a_registry_is_read_over_http() {
    let _turn = fresh();
    let registry = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    koine_packages::registry::set_cache_dir(cache.path().to_path_buf());
    publish(registry.path(), "2.0.0", "web");
    let root = registry.path().to_path_buf();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader, Write};
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap() <= 2 {
                    break;
                }
            }
            let file = root.join(path.trim_start_matches('/'));
            let mut stream = stream;
            match std::fs::read(&file) {
                Ok(body) => {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(&body);
                }
                Err(_) => {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    );
                }
            }
        }
    });
    let url = format!("http://127.0.0.1:{port}/");
    let added = verb("package-service_registry-add", json!({"url": url}));
    assert_eq!(added["ok"], true, "{added}");
    let review = verb("package-service_fetch", json!({"name": "triage"}));
    assert_eq!(review["ok"], true, "{review}");
    assert_eq!(review["version"], "2.0.0");
    let missing = verb(
        "package-service_registry-add",
        json!({"url": format!("http://127.0.0.1:{port}/nowhere/")}),
    );
    assert_eq!(missing["code"], "not-found", "{missing}");
    verb("package-service_registry-remove", json!({"url": url}));
}

/// The example registry checked into this repository (`registry/`) is what
/// its sources publish: every entry's bundle has the digest the index pins,
/// and rebundling each source gives the same digest.
#[test]
fn the_example_registry_matches_its_sources() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry");
    let index =
        koine_packages::registry::Index::parse(&std::fs::read(root.join("index.json")).unwrap())
            .unwrap();
    assert!(!index.packages.is_empty());
    for entry in &index.packages {
        let bundle = std::fs::read(root.join(&entry.bundle)).unwrap();
        let package = koine_packages::package::Package::from_bundle(&bundle, "test").unwrap();
        assert_eq!(
            package.digest(),
            entry.digest,
            "{} is stale: rerun package bundle",
            entry.name
        );
        let source =
            koine_packages::package::Package::load(&root.join("src").join(&entry.name)).unwrap();
        assert_eq!(source.digest(), entry.digest, "{}'s source changed: rerun `koine-host package bundle registry/src/{} --out registry --index registry/index.json`", entry.name, entry.name);
        for surface in &package.manifest.surfaces {
            let spec = package.surface_spec(surface).unwrap();
            let (_, problems) = impress_surface::validate_json(&spec);
            assert!(
                problems
                    .iter()
                    .all(|p| p.severity != impress_surface::Severity::Error),
                "{problems:?}"
            );
        }
    }
}

// ------------------------------------------- workflows and categories (P-D9)

fn workflow(steps_verb: &str) -> Value {
    json!({
        "wire_version": 1, "name": "Nightly list", "description": "List the packages",
        "state": "enabled", "author": {"kind": "human"},
        "trigger": {"schedule": {"every": "24h"}},
        "steps": [{"call": {"verb": steps_verb, "args": {}}}]
    })
}

/// Write a package `lab` at `version` shipping one workflow and one category.
fn write_lab(dir: &Path, version: &str, steps_verb: &str) {
    std::fs::create_dir_all(dir.join("workflows")).unwrap();
    std::fs::write(
        dir.join("koine-package.json"),
        serde_json::to_vec_pretty(&json!({
            "package": "1.0", "name": "lab", "version": version,
            "workflows": [{"name": "Nightly list", "file": "workflows/nightly.workflow.json"}],
            "ai_categories": [{"id": "research.lab-notes", "name": "Lab notes",
                               "parent": "research", "apps": ["notes"]}]
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.join("workflows/nightly.workflow.json"),
        serde_json::to_vec(&workflow(steps_verb)).unwrap(),
    )
    .unwrap();
}

fn store() -> Arc<dyn AttributedStore> {
    koine_store::process::current().unwrap()
}

fn workflow_row(id: &str) -> Option<impress_item::Item> {
    store().get(id.parse().unwrap()).unwrap()
}

fn set_field(id: &str, field: &str, value: Value) {
    store()
        .update(
            id.parse().unwrap(),
            vec![impress_item::FieldMutation::SetPayload(
                field.into(),
                serde_json::from_value(value).unwrap(),
            )],
        )
        .unwrap();
}

#[test]
fn a_packages_workflows_install_proposed_and_its_categories_list() {
    let _turn = fresh();
    let dir = tempfile::tempdir().unwrap();
    write_lab(dir.path(), "0.1.0", "package-service_list");
    let path = dir.path().display().to_string();

    let review = verb("package-service_inspect", json!({"path": path}));
    assert_eq!(review["valid"], true, "{review}");
    let flow = &review["workflows"][0];
    assert_eq!(flow["would"], "propose", "{review}");
    assert_eq!(flow["verbs"], json!(["package-service_list"]));
    assert_eq!(review["ai_categories"][0]["id"], "research.lab-notes");

    let installed = install(dir.path());
    let row = &installed["workflows"][0];
    assert_eq!(row["outcome"], "proposed", "{installed}");
    let id = row["workflow_id"].as_str().unwrap().to_string();
    let stored = workflow_row(&id).expect("the workflow row");
    assert_eq!(stored.schema, impress_item::schema::refs::IMPRESS_WORKFLOW);
    assert!(
        matches!(stored.payload.get("state"), Some(impress_item::Value::String(s)) if s == "proposed"),
        "a package's workflow waits for a person, whatever its file says"
    );
    assert_eq!(installed["ai_categories"], json!(["research.lab-notes"]));
    let categories = koine_ai::categories::all_in(store().as_ref());
    let lab = categories
        .iter()
        .find(|c| c.id == "research.lab-notes")
        .expect("listed beside the built-in table");
    assert_eq!(lab.package.as_deref(), Some("lab"));

    // The person enables it; the same version again changes nothing.
    set_field(&id, "state", json!("enabled"));
    let again = install(dir.path());
    assert_eq!(again["workflows"][0]["outcome"], "unchanged", "{again}");
    assert!(matches!(
        workflow_row(&id).unwrap().payload.get("state"),
        Some(impress_item::Value::String(s)) if s == "enabled"
    ));

    // New content is proposed again, even though it was enabled.
    write_lab(dir.path(), "0.2.0", "package-service_registries");
    let upgraded = install(dir.path());
    assert_eq!(
        upgraded["workflows"][0]["outcome"], "proposed",
        "{upgraded}"
    );
    assert!(matches!(
        workflow_row(&id).unwrap().payload.get("state"),
        Some(impress_item::Value::String(s)) if s == "proposed"
    ));

    // A workflow the person edited is kept on upgrade and on remove.
    set_field(&id, "description", json!("mine now"));
    write_lab(dir.path(), "0.3.0", "package-service_list");
    let review = verb("package-service_inspect", json!({"path": path}));
    assert_eq!(review["workflows"][0]["would"], "keep yours", "{review}");
    let kept = install(dir.path());
    assert_eq!(kept["workflows"][0]["outcome"], "kept yours", "{kept}");
    let removed = verb("package-service_remove", json!({"name": "lab"}));
    assert_eq!(removed["workflows_kept"], json!([id]), "{removed}");
    assert!(workflow_row(&id).is_some());
    assert!(koine_ai::categories::installed_in(store().as_ref()).is_empty());
}

#[test]
fn a_pristine_workflow_goes_with_its_package_and_a_broken_one_is_refused() {
    let _turn = fresh();
    let dir = tempfile::tempdir().unwrap();
    write_lab(dir.path(), "0.1.0", "package-service_list");
    let installed = install(dir.path());
    let id = installed["workflows"][0]["workflow_id"]
        .as_str()
        .unwrap()
        .to_string();
    let removed = verb("package-service_remove", json!({"name": "lab"}));
    assert_eq!(removed["workflows_deleted"], json!([id]), "{removed}");
    assert!(workflow_row(&id).is_none());

    // A workflow that publishes (it has no pane) does not install.
    let broken = tempfile::tempdir().unwrap();
    write_lab(broken.path(), "0.1.0", "package-service_list");
    let mut spec = workflow("package-service_list");
    spec["steps"] = json!([{"publish": {}}]);
    std::fs::write(
        broken.path().join("workflows/nightly.workflow.json"),
        serde_json::to_vec(&spec).unwrap(),
    )
    .unwrap();
    let path = broken.path().display().to_string();
    let review = verb("package-service_inspect", json!({"path": path}));
    assert_eq!(review["valid"], false, "{review}");
    assert_eq!(review["workflows"][0]["would"], "refuse");
    let refused = verb(
        "package-service_install",
        json!({"path": path, "digest": review["digest"]}),
    );
    assert_eq!(refused["code"], "invalid-spec", "{refused}");

    // A category whose id is not under its parent is refused at read.
    let odd = tempfile::tempdir().unwrap();
    std::fs::write(
        odd.path().join("koine-package.json"),
        serde_json::to_vec(&json!({
            "package": "1.0", "name": "odd", "version": "0.1.0",
            "ai_categories": [{"id": "lab-notes", "name": "Lab", "parent": "research"}]
        }))
        .unwrap(),
    )
    .unwrap();
    let review = verb(
        "package-service_inspect",
        json!({"path": odd.path().display().to_string()}),
    );
    assert_eq!(review["code"], "invalid-argument", "{review}");
}
