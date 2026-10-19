//! GADGET-4 through run-service's generic verbs, no per-code wrapper
//! (docs/plan-run-convention.md R-D9.1, R-D11, R-D12): the adapter as a
//! registered code, the fake binary (`fixtures/fake-gadget4.sh`) as GADGET-4,
//! every call through the verb inventory like run-service's own `tier_a.rs`.
//!
//! The one `#[ignore]` test runs the real `Gadget4-capi` the same way.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use impress_service_core::call;
use koine_store_memory::MemoryStore;
use serde_json::{json, Value};

fn adapt() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_gadget4-adapt"))
}

fn fx(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
}

fn verb(name: &str, args: Value) -> Value {
    call::call(name, args).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn ok(name: &str, args: Value) -> Value {
    let r = verb(name, args);
    assert_eq!(r["ok"], json!(true), "{name}: {r}");
    r
}

fn refused(name: &str, args: Value, code: &str) -> Value {
    let r = verb(name, args);
    assert_eq!(r["ok"], json!(false), "{name}: {r}");
    assert_eq!(r["code"], json!(code), "{name}: {r}");
    r
}

fn install_store() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        koine_store::process::install(Arc::new(MemoryStore::new()));
        koine_agent::ensure_linked();
        koine_tasks::ensure_linked();
        run_service::ensure_linked();
    });
}

/// Register GADGET-4 with the fake binary: the registration is
/// `run_gadget4::provider(..)` verbatim, plus the fake's own arguments
/// (a run's length is `TimeMax`, a step takes 50 ms, step 1 writes a snapshot).
fn setup() {
    static ONCE: OnceLock<()> = OnceLock::new();
    install_store();
    ONCE.get_or_init(|| {
        let mut reg = run_gadget4::provider(adapt(), &fx("fake-gadget4.sh"), None);
        reg["launch"]["env"]["GADGET4_ARGS"] = json!(format!(
            "steps=timemax sleep=0.05 snapshot={}",
            fx("small.hdf5").display()
        ));
        ok("run-service_register-code", reg);
    });
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn status(id: &str) -> Value {
    ok("run-service_status", json!({"id": id}))
}

fn events(id: &str) -> Vec<Value> {
    ok("run-service_events", json!({"id": id, "limit": 200}))["events"]
        .as_array()
        .unwrap()
        .clone()
}

/// A start that is its own run (`force`: the key of identical parameters
/// would otherwise answer an earlier run), `steps` long.
fn start(dir: &Path, steps: u32, extra: Value, wait: bool) -> Value {
    let mut params = json!({"time.TimeMax": 0.1 + f64::from(steps) * 0.001});
    if let Some(o) = extra.as_object() {
        for (k, v) in o {
            params[k] = v.clone();
        }
    }
    verb(
        "run-service_start",
        json!({"code": "GADGET-4", "params": params, "dir": dir.to_string_lossy(),
               "wait": wait, "force": true}),
    )
}

fn events_of_kind(id: &str, kind: &str) -> Vec<Value> {
    events(id)
        .into_iter()
        .filter(|e| e["kind"] == json!(kind))
        .collect()
}

#[test]
fn the_binary_emits_a_conforming_schema_without_gadget4() {
    let out = std::process::Command::new(adapt())
        .arg("--schema")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let schema: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(run_convention::check_parameter_schema(&schema).is_empty());
    assert_eq!(schema, run_gadget4::builtin_schema());
    assert_eq!(schema["properties"].as_object().unwrap().len(), 60);
}

#[test]
fn codes_lists_gadget4_with_its_groups_and_form_validates() {
    setup();
    let r = ok("run-service_codes", json!({}));
    let code = r["codes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == json!("GADGET-4"))
        .expect("GADGET-4 registered");
    let groups: Vec<&str> = code["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g.as_str().unwrap())
        .collect();
    for g in [
        "io",
        "time",
        "cosmology",
        "gravity",
        "integration",
        "output",
    ] {
        assert!(groups.contains(&g), "{g} in {groups:?}");
    }
    assert_eq!(code["metrics"], json!(["wall_s", "memory_largest_mb"]));
    assert_eq!(code["launch"]["resume_flag"], json!("--from"));
    assert_eq!(code["launch"]["problem_flag"], json!("--problem"));

    let r = ok("run-service_form", json!({"code": "GADGET-4"}));
    let spec: impress_surface::spec::SurfaceSpec =
        serde_json::from_value(r["spec"].clone()).unwrap();
    let problems = impress_surface::validate::validate(&spec);
    assert!(problems.iter().all(|p| !p.is_error()), "{problems:?}");
    assert!(r["spec"].to_string().contains("\"group-time\""));
}

#[test]
fn a_misspelt_parameter_is_refused_by_name_before_anything_starts() {
    setup();
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("typo");
    let r = refused(
        "run-service_start",
        json!({"code": "GADGET-4", "params": {"time.TimeBegn": 0.2}, "dir": dir.to_string_lossy()}),
        "invalid-argument",
    );
    assert!(
        r["message"].as_str().unwrap().contains("time.TimeBegn"),
        "{r}"
    );
    assert!(!dir.exists(), "nothing was started");
    // A value the schema forbids is refused too.
    refused(
        "run-service_start",
        json!({"code": "GADGET-4", "params": {"time.ComovingIntegrationOn": 7}, "dir": dir.to_string_lossy()}),
        "invalid-argument",
    );
}

#[test]
fn start_runs_to_done_with_progress_output_and_checkpoint_events() {
    setup();
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("full");
    let r = start(&dir, 4, json!({}), true);
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["run"]["state"], json!("done"), "{r}");
    let id = r["run"]["id"].as_str().unwrap().to_string();
    wait_until("done", || status(&id)["run"]["state"] == json!("done"));

    let report = run_convention::check_dir(&dir);
    assert!(report.ok(), "{report}");
    assert!(report.findings.is_empty(), "{report}");

    let kinds: Vec<String> = events(&id)
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_string())
        .collect();
    for k in ["started", "progress", "output", "checkpoint", "finished"] {
        assert!(kinds.iter().any(|x| x == k), "{k} in {kinds:?}");
    }
    assert_eq!(events_of_kind(&id, "progress").len(), 4);
    let s = status(&id);
    assert_eq!(s["run"]["code"]["name"], json!("GADGET-4"));
    assert!(s["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["kind"] == json!("checkpoint")));
    // run.json records the command, and the problem the caller passed.
    let run: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("run.json")).unwrap()).unwrap();
    assert_eq!(
        run["problem"],
        json!({"name": "gadget4", "version": run_gadget4::provider::build_tag()})
    );
    assert!(run["host"]["argv"][1] == json!("param.txt"));
    // The same start again, without force, is the run it already is.
    let again = verb(
        "run-service_start",
        json!({"code": "GADGET-4", "params": {"time.TimeMax": 0.1 + f64::from(4u32) * 0.001}, "dir": t.path().join("x").to_string_lossy()}),
    );
    assert_eq!(again["reused"], json!(true), "{again}");
    assert_eq!(again["run"]["id"], json!(id));
}

#[test]
fn set_honours_the_class_and_the_adapter_refuses_a_never_request_into_control_refused() {
    setup();
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("steer");
    let r = start(&dir, 200, json!({}), false);
    assert_eq!(r["ok"], json!(true), "{r}");
    let id = r["run"]["id"].as_str().unwrap().to_string();
    wait_until("progress", || !events_of_kind(&id, "progress").is_empty());

    // never: refused by the verb, nothing written.
    let r = refused(
        "run-service_set",
        json!({"id": id, "param": "time.TimeBegin", "value": 0.2}),
        "invalid-argument",
    );
    assert!(r["message"].as_str().unwrap().contains("`never`"), "{r}");
    // restart: written, and the run marked for the next restart.
    let r = ok(
        "run-service_set",
        json!({"id": id, "param": "time.TimeMax", "value": 0.31}),
    );
    let control = r["control"].as_str().unwrap().to_string();
    assert_eq!(r["run"]["needs_restart"][0]["param"], json!("time.TimeMax"));
    // A never request that reaches the adapter anyway (a hand-written inbox
    // file) is refused into control/refused/ with the reason.
    let raw = run_service::control::write_request(
        &dir,
        json!({"request": "set", "param": "time.TimeBegin", "value": 0.2}),
    )
    .unwrap();
    let raw_file = dir.join("control/refused").join(format!("{raw}.json"));
    wait_until("the refusal", || raw_file.is_file());
    let body: Value = serde_json::from_str(&std::fs::read_to_string(&raw_file).unwrap()).unwrap();
    assert!(body["reason"].as_str().unwrap().contains("never"), "{body}");
    // The restart-class one was recorded by the adapter, not applied.
    let done = dir.join("control/done").join(format!("{control}.json"));
    wait_until("the restart request to be served", || done.is_file());
    let pending: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("restart-pending.json")).unwrap())
            .unwrap();
    assert_eq!(pending["time.TimeMax"]["value"], json!(0.31));

    ok("run-service_stop", json!({"id": id}));
    wait_until("stopped", || {
        status(&id)["run"]["state"] != json!("running")
    });
}

#[test]
fn stop_is_suspended_in_the_1xx_band_and_resume_is_a_child_run() {
    setup();
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("parent");
    let r = start(&dir, 400, json!({}), false);
    assert_eq!(r["ok"], json!(true), "{r}");
    let id = r["run"]["id"].as_str().unwrap().to_string();
    wait_until("progress", || events_of_kind(&id, "progress").len() >= 2);
    refused("run-service_resume", json!({"id": id}), "conflict");
    // A restart-class value set now rides along into the resume.
    ok(
        "run-service_set",
        json!({"id": id, "param": "restart.CpuTimeBetRestartFile", "value": 100.0}),
    );
    ok("run-service_stop", json!({"id": id}));
    wait_until("stopped", || {
        status(&id)["run"]["state"] != json!("running")
    });
    let s = status(&id);
    assert_eq!(s["run"]["state"], json!("suspended"), "{s}");
    let code = s["run"]["exit"]["code"].as_u64().unwrap();
    assert!((100..200).contains(&code), "{s}");
    assert_eq!(s["run"]["exit"]["label"], json!("stopped"));
    assert!(
        run_convention::check_dir(&dir).ok(),
        "{}",
        run_convention::check_dir(&dir)
    );

    // A changed `never` value is refused, naming it, and no child run starts.
    let child_dir = t.path().join("child-never");
    let r = refused(
        "run-service_resume",
        json!({"id": id, "params": {"time.TimeBegin": 0.05}, "dir": child_dir.to_string_lossy(), "wait": true}),
        "process-failed",
    );
    let msg = r["message"].as_str().unwrap();
    assert!(
        msg.contains("time.TimeBegin") && msg.contains("never"),
        "{r}"
    );
    assert!(msg.contains("207"), "{r}");

    // Resume with a restart-class change: a longer TimeMax.
    let child_dir = t.path().join("child");
    let r = verb(
        "run-service_resume",
        json!({"id": id, "params": {"time.TimeMax": 0.12}, "dir": child_dir.to_string_lossy(), "wait": true}),
    );
    assert_eq!(r["ok"], json!(true), "{r}");
    let child = &r["run"];
    assert_eq!(child["state"], json!("done"), "{child}");
    assert_eq!(child["lineage"]["parent"]["run"], json!(id));
    assert_eq!(child["lineage"]["parent"]["relation"], json!("resume"));
    let cid = child["id"].as_str().unwrap().to_string();
    assert_ne!(cid, id);

    // RestartFlag 1 is the argument after param.txt, never a tag in it (the
    // real code refuses unknown tags), and the restart files were staged.
    let run: Value =
        serde_json::from_str(&std::fs::read_to_string(child_dir.join("run.json")).unwrap())
            .unwrap();
    assert_eq!(run["host"]["argv"][1], json!("param.txt"));
    assert_eq!(run["host"]["argv"][2], json!("1"), "{run}");
    let param_txt = std::fs::read_to_string(child_dir.join("param.txt")).unwrap();
    assert!(!param_txt.contains("RestartFlag"), "{param_txt}");
    assert!(param_txt
        .lines()
        .any(|l| l.starts_with("TimeMax") && l.contains("0.12")));
    let log = std::fs::read_to_string(child_dir.join("stdout.log")).unwrap();
    assert!(log.contains("RestartFlag = 1"), "{log}");
    assert!(run["lineage"]["parent"]["checkpoint"]["sha256"].is_string());

    // The changes are `changed{by: "restart"}` events: the caller's, and the
    // pending one the set left on the parent.
    let changed = events_of_kind(&cid, "changed");
    for (param, to) in [
        ("time.TimeMax", json!(0.12)),
        ("restart.CpuTimeBetRestartFile", json!(100.0)),
    ] {
        assert!(
            changed.iter().any(|c| c["event"]["param"] == json!(param)
                && c["event"]["to"] == to
                && c["event"]["by"] == json!("restart")),
            "{param} in {changed:?}"
        );
    }
    // It continued where the parent stopped: its first progress step is not 0.
    let first = events_of_kind(&cid, "progress")[0]["event"]["step"]
        .as_u64()
        .unwrap();
    assert!(first > 0, "resumed at step {first}");
    let report = run_convention::check_dir(&child_dir);
    assert!(report.ok(), "{report}");
    let parent = status(&id);
    assert!(parent["run"]["needs_restart"]
        .as_array()
        .is_none_or(|a| a.is_empty()));
}

#[test]
fn a_snapshot_resume_is_restart_flag_2() {
    setup();
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("snap-parent");
    let r = start(&dir, 5, json!({}), true);
    assert_eq!(r["run"]["state"], json!("done"), "{r}");
    let id = r["run"]["id"].as_str().unwrap().to_string();
    let child_dir = t.path().join("snap-child");
    let r = verb(
        "run-service_resume",
        json!({"id": id, "from": "output/snapshot_000.hdf5", "dir": child_dir.to_string_lossy(), "wait": true}),
    );
    assert_eq!(r["ok"], json!(true), "{r}");
    let run: Value =
        serde_json::from_str(&std::fs::read_to_string(child_dir.join("run.json")).unwrap())
            .unwrap();
    assert_eq!(run["host"]["argv"][2], json!("2"));
    assert_eq!(run["host"]["argv"][3], json!("0"));
    assert_eq!(run["lineage"]["parent"]["run"], json!(id));
    assert!(run_convention::check_dir(&child_dir).ok());
}

/// The real code, through the same verbs (R7's 16^3 setup, one rank). Needs
/// `~/Projects/gadget4/Gadget4-capi`, `mpirun` and `h5dump`; run with
/// `cargo test -p run-gadget4 --test provider -- --ignored --nocapture`.
#[test]
#[ignore = "needs the real Gadget4-capi, mpirun and the 16^3 setup"]
fn live_gadget4_start_stop_and_resume_through_run_service() {
    install_store();
    let home = std::env::var("HOME").unwrap();
    let bin = Path::new(&home).join("Projects/gadget4/Gadget4-capi");
    assert!(bin.is_file(), "{} missing", bin.display());
    let reg = run_gadget4::provider(adapt(), &bin, Some(1));
    ok("run-service_register-code", reg);
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("live");
    println!("live runs under {}", t.path().display());
    // R7's recorded setup: fixtures/params.json is the parameter set of the run.
    let setup: Value =
        serde_json::from_str(&std::fs::read_to_string(fx("params.json")).unwrap()).unwrap();
    let mut params = serde_json::Map::new();
    for (k, v) in setup.as_object().unwrap() {
        params.insert(k.clone(), v["value"].clone());
    }
    // The real IC are made by the code itself (NGenIC, CREATE_GRID). The one
    // input file it reads, the output-times list, has to exist before the run
    // directory does, so it is named by an absolute path.
    let outputs = t.path().join("outputs.txt");
    std::fs::write(&outputs, "0.105\n0.11\n").unwrap();
    params.insert(
        "output.OutputListFilename".into(),
        json!(outputs.to_string_lossy()),
    );
    let r = verb(
        "run-service_start",
        json!({"code": "GADGET-4", "params": params, "dir": dir.to_string_lossy(), "wait": false, "force": true}),
    );
    assert_eq!(r["ok"], json!(true), "{r}");
    let id = r["run"]["id"].as_str().unwrap().to_string();
    let live = dir.clone();
    wait_until("a few sync points", || {
        let n = events_of_kind(&id, "progress").len();
        if n < 3 && status(&id)["run"]["state"] == json!("failed") {
            panic!(
                "the run failed: {}",
                std::fs::read_to_string(live.join("stdout.log")).unwrap_or_default()
            );
        }
        n >= 3
    });
    ok("run-service_stop", json!({"id": id}));
    wait_until("stopped", || {
        status(&id)["run"]["state"] != json!("running")
    });
    let stopped = status(&id);
    println!(
        "STOPPED {}",
        serde_json::to_string_pretty(&stopped["run"]).unwrap()
    );
    assert_eq!(stopped["run"]["state"], json!("suspended"), "{stopped}");

    let child_dir = t.path().join("live-child");
    let r = verb(
        "run-service_resume",
        json!({"id": id, "params": {"time.TimeMax": 0.112}, "dir": child_dir.to_string_lossy(), "wait": true}),
    );
    assert_eq!(r["ok"], json!(true), "{r}");
    let cid = r["run"]["id"].as_str().unwrap().to_string();
    let child = status(&cid);
    println!(
        "RESUMED {}",
        serde_json::to_string_pretty(&child["run"]).unwrap()
    );
    assert_eq!(child["run"]["state"], json!("done"), "{child}");
    assert!(run_convention::check_dir(&child_dir).ok());
}
