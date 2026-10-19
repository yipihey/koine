//! Tier A, end to end over the one built-in code (`demo-code`,
//! `CARGO_BIN_EXE_demo-code`) and the R1 fixtures, every call through the
//! inventory (docs/capability.md § 8), on one memory store for the process.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use impress_service_core::call;
use koine_store_memory::MemoryStore;
use serde_json::{json, Value};

fn demo() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_demo-code"))
}

fn setup() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        koine_store::process::install(Arc::new(MemoryStore::new()));
        koine_agent::ensure_linked();
        koine_tasks::ensure_linked();
        run_service::ensure_linked();
        run_service::demo_code::register_at(demo()).expect("demo-code registers from its binary");
    });
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

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../run-convention/fixtures")
        .join(name)
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        if e.path().is_file() {
            std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
        }
    }
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn status(id: &str) -> Value {
    ok("run-service_status", json!({"id": id}))
}

fn start(params: Value, dir: &Path, extra: Value) -> Value {
    let mut args =
        json!({"code": "demo-code", "params": params, "dir": dir.to_string_lossy(), "wait": true});
    if let Some(o) = extra.as_object() {
        for (k, v) in o {
            args[k] = v.clone();
        }
    }
    verb("run-service_start", args)
}

#[test]
fn the_binary_emits_the_pinned_schema() {
    let out = std::process::Command::new(demo())
        .arg("--schema")
        .output()
        .unwrap();
    assert!(out.status.success());
    let emitted: Value = serde_json::from_slice(&out.stdout).unwrap();
    let pinned: Value =
        serde_json::from_str(include_str!("../fixtures/demo-code.schema.json")).unwrap();
    assert_eq!(
        emitted, pinned,
        "regenerate fixtures/demo-code.schema.json from `demo-code --schema`"
    );
    assert!(run_convention::check_parameter_schema(&emitted).is_empty());
}

#[test]
fn codes_lists_the_demo_code_with_its_groups_and_metrics() {
    setup();
    let r = ok("run-service_codes", json!({}));
    let code = r["codes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == json!("demo-code"))
        .expect("demo-code registered");
    let mut groups: Vec<&str> = code["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g.as_str().unwrap())
        .collect();
    groups.sort_unstable();
    assert_eq!(groups, ["failure", "io", "run"]);
    assert_eq!(code["metrics"], json!(["energy"]));
    assert!(code["parameters"]
        .as_array()
        .unwrap()
        .contains(&json!("demo.cadence")));
    assert_eq!(code["launch"]["resume_flag"], json!("--from"));
}

#[test]
fn ingest_good_is_idempotent_and_fills_the_ring() {
    setup();
    let r = ok(
        "run-service_ingest",
        json!({"dir": fixture("good").to_string_lossy()}),
    );
    assert_eq!(
        r["run"]["id"],
        json!("5b0f6a3e-2c1d-4e7a-9f55-0a1b2c3d4e5f")
    );
    assert_eq!(r["run"]["state"], json!("done"));
    assert_eq!(r["run"]["last_event_seq"], json!(8));
    assert_eq!(r["run"]["progress"]["step"], json!(100));
    assert_eq!(r["run"]["metrics"]["mach"], json!(1.42));
    assert_eq!(r["run"]["exit"]["code"], json!(0));
    assert_eq!(r["events_added"], json!(8));
    let again = ok(
        "run-service_ingest",
        json!({"dir": fixture("good").to_string_lossy()}),
    );
    assert_eq!(
        again["events_added"],
        json!(0),
        "a second ingest advances from last_event_seq"
    );

    let id = "5b0f6a3e-2c1d-4e7a-9f55-0a1b2c3d4e5f";
    let ev = ok(
        "run-service_events",
        json!({"id": id, "after_seq": 0, "limit": 3}),
    );
    let seqs: Vec<u64> = ev["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, [1, 2, 3]);
    assert_eq!(ev["cursor"], json!(3));
    assert_eq!(ev["gap"], json!(false));
    let ev = ok("run-service_events", json!({"id": id, "after_seq": 3}));
    let kinds: Vec<&str> = ev["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        ["checkpoint", "output", "changed", "warning", "finished"]
    );
    assert_eq!(ev["events"][2]["event"]["by"], json!("control:7f3a"));

    let s = status(id);
    assert_eq!(s["outputs"].as_array().unwrap().len(), 2);
    assert_eq!(s["notes"]["tags"], json!(["figure-3"]));
    assert_eq!(s["run"]["orphaned"], json!(false));

    let l = ok(
        "run-service_list",
        json!({"key": "1868d5c1a7f061da807f685184bed5b797f87d6b516f86c56bd70a61c1170087"}),
    );
    assert_eq!(l["runs"][0]["id"], json!(id));
    let l = ok(
        "run-service_list",
        json!({"code": "titan", "state": "done"}),
    );
    assert!(l["runs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["id"] == json!(id)));
    let l = ok("run-service_list", json!({"problem": "no-such-problem"}));
    assert_eq!(l["runs"].as_array().unwrap().len(), 0);

    // titan is not a registered code: steering it is refused by name.
    let r = refused(
        "run-service_set",
        json!({"id": id, "param": "io.every", "value": 10}),
        "not-found",
    );
    assert!(r["message"].as_str().unwrap().contains("titan"), "{r}");
}

#[test]
fn ingest_refuses_a_gapped_sequence_and_a_bad_key_but_takes_an_import_with_findings() {
    setup();
    let r = refused(
        "run-service_ingest",
        json!({"dir": fixture("gapped-seq").to_string_lossy()}),
        "invalid-argument",
    );
    assert!(r["message"].as_str().unwrap().contains("[seq]"), "{r}");
    let r = refused(
        "run-service_ingest",
        json!({"dir": fixture("bad-key").to_string_lossy()}),
        "invalid-argument",
    );
    assert!(r["message"].as_str().unwrap().contains("[key]"), "{r}");

    // The same gapped directory, marked imported (R-D11): ingested, the
    // findings kept on the row.
    let t = tempfile::tempdir().unwrap();
    copy_dir(&fixture("gapped-seq"), t.path());
    let mut run: Value =
        serde_json::from_str(&std::fs::read_to_string(t.path().join("run.json")).unwrap()).unwrap();
    run["provenance"] = json!("imported");
    run["id"] = json!("6c1f7b4f-3d2e-4f8b-a066-1b2c3d4e5f60");
    std::fs::write(t.path().join("run.json"), run.to_string()).unwrap();
    let r = ok(
        "run-service_ingest",
        json!({"dir": t.path().to_string_lossy()}),
    );
    assert_eq!(r["run"]["provenance"], json!("imported"));
    assert!(
        r["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f.as_str().unwrap().contains("[seq]")),
        "{r}"
    );
}

#[test]
fn start_runs_the_demo_code_to_done_and_reuses_the_key() {
    setup();
    let t = tempfile::tempdir().unwrap();
    let params = json!({"demo.steps": 6, "demo.label": "reuse-me"});
    let r = start(
        params.clone(),
        &t.path().join("a"),
        json!({"name": "first"}),
    );
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["reused"], json!(false));
    let run = &r["run"];
    assert_eq!(run["state"], json!("done"), "{run}");
    assert_eq!(run["exit"]["code"], json!(0));
    assert_eq!(run["code"]["name"], json!("demo-code"));
    assert_eq!(run["problem"], json!({"name": "demo", "version": "1"}));
    assert_eq!(run["progress"]["step"], json!(6));
    assert!(run["metrics"]["energy"].is_number());
    assert_eq!(
        run["key"], r["key"],
        "the predicted key is the one the code wrote"
    );
    assert_eq!(
        run["name"],
        json!("reuse-me"),
        "the code's own name wins over the alias"
    );
    let id = run["id"].as_str().unwrap().to_string();
    let dir = PathBuf::from(run["dir"].as_str().unwrap());
    assert!(
        run_convention::check_dir(&dir).ok(),
        "{}",
        run_convention::check_dir(&dir)
    );

    let s = status(&id);
    assert!(s["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["path"] == json!("series.json")));
    let ev = ok("run-service_events", json!({"id": id}));
    let kinds: Vec<&str> = ev["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds.first(), Some(&"started"));
    assert_eq!(kinds.last(), Some(&"finished"));
    assert!(kinds.contains(&"progress") && kinds.contains(&"metric") && kinds.contains(&"output"));

    // Reuse: same parameters, same problem → the done run, not a launch.
    let again = start(params.clone(), &t.path().join("b"), json!({}));
    assert_eq!(again["reused"], json!(true), "{again}");
    assert_eq!(again["run"]["id"], json!(id));
    assert!(
        !t.path().join("b").exists(),
        "a reused start makes no directory"
    );
    // Parameter order and defaults do not change the key.
    let again = start(
        json!({"demo.label": "reuse-me", "demo.steps": 6, "demo.cadence": 2}),
        &t.path().join("b2"),
        json!({}),
    );
    assert_eq!(again["reused"], json!(true), "{again}");
    // force launches anyway.
    let forced = start(params.clone(), &t.path().join("c"), json!({"force": true}));
    assert_eq!(forced["reused"], json!(false));
    assert_ne!(forced["run"]["id"], json!(id));
    // A different value is a different key.
    let other = start(
        json!({"demo.steps": 7, "demo.label": "reuse-me"}),
        &t.path().join("d"),
        json!({}),
    );
    assert_eq!(other["reused"], json!(false));
    assert_ne!(other["key"], r["key"]);
    // A different problem too.
    let other = start(
        params,
        &t.path().join("e"),
        json!({"problem": {"name": "sod", "version": "2"}}),
    );
    assert_eq!(other["reused"], json!(false), "{other}");

    // A directory that already holds a run is refused.
    refused(
        "run-service_start",
        json!({"code": "demo-code", "params": {"demo.steps": 1}, "dir": dir.to_string_lossy(), "force": true}),
        "conflict",
    );
    // An unknown name refuses the run, named.
    let r = refused(
        "run-service_start",
        json!({"code": "demo-code", "params": {"demo.stepz": 1}}),
        "invalid-argument",
    );
    assert!(
        r["message"].as_str().unwrap().contains("`demo.stepz`"),
        "{r}"
    );
    let l = ok("run-service_list", json!({"code": "demo-code"}));
    assert!(l["runs"].as_array().unwrap().len() >= 4);
}

#[test]
fn a_3xx_exit_files_a_diagnose_request_and_never_matches_for_reuse() {
    setup();
    let t = tempfile::tempdir().unwrap();
    let params = json!({"demo.steps": 5, "demo.fail_at": 3, "demo.label": "doomed"});
    let r = start(params.clone(), &t.path().join("a"), json!({}));
    assert_eq!(r["ok"], json!(true), "{r}");
    let run = &r["run"];
    assert_eq!(run["state"], json!("failed"));
    assert_eq!(run["exit"]["code"], json!(320));
    assert_eq!(run["exit"]["invalidates_reuse"], json!(true));
    let request = run["diagnose_request"]
        .as_str()
        .expect("a diagnose request was filed")
        .to_string();
    let filed = koine_agent::desk::get(&request).expect("the request is on the desk");
    let row = koine_agent::desk::row_of(&filed, true);
    assert_eq!(row.kind, "diagnose");
    assert!(row.prompt.contains("320"), "{}", row.prompt);
    assert_eq!(row.context["kind"], json!("run"));
    assert_eq!(row.context["run"]["id"], run["id"]);
    let events = row.context["events"].as_array().unwrap();
    assert!(events.len() <= 20 && !events.is_empty());
    assert_eq!(events.last().unwrap()["kind"], json!("finished"));
    // The second ingest does not file a second request.
    let again = ok(
        "run-service_ingest",
        json!({"dir": run["dir"].as_str().unwrap()}),
    );
    assert_eq!(again["run"]["diagnose_request"], json!(request));
    // invalidates_reuse: the same key launches again.
    let r2 = start(params, &t.path().join("b"), json!({}));
    assert_eq!(r2["reused"], json!(false), "{r2}");
    assert_ne!(r2["run"]["id"], run["id"]);
}

#[test]
fn set_honours_the_mutability_class_and_status_reports_the_outcome() {
    setup();
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("steer");
    let r = verb(
        "run-service_start",
        json!({
            "code": "demo-code",
            "params": {"demo.steps": 60, "demo.sleep_ms": 40, "demo.cadence": 4, "demo.label": "steer"},
            "dir": dir.to_string_lossy()
        }),
    );
    assert_eq!(r["ok"], json!(true), "{r}");
    let id = r["run"]["id"].as_str().unwrap().to_string();
    assert_eq!(r["run"]["state"], json!("running"));
    assert!(r["run"]["pid"].is_number());

    // never: refused here, nothing written.
    let r = refused(
        "run-service_set",
        json!({"id": id, "param": "demo.steps", "value": 3}),
        "invalid-argument",
    );
    assert!(r["message"].as_str().unwrap().contains("`never`"), "{r}");
    // unknown: refused, named.
    refused(
        "run-service_set",
        json!({"id": id, "param": "demo.nope", "value": 3}),
        "invalid-argument",
    );
    // a wrong type: refused.
    refused(
        "run-service_set",
        json!({"id": id, "param": "demo.cadence", "value": "fast"}),
        "invalid-argument",
    );
    // restart: written, and the run marked.
    let r = ok(
        "run-service_set",
        json!({"id": id, "param": "demo.dt", "value": 2.5}),
    );
    let dt_control = r["control"].as_str().unwrap().to_string();
    assert_eq!(r["run"]["needs_restart"][0]["param"], json!("demo.dt"));
    // always: written for the code.
    let r = ok(
        "run-service_set",
        json!({"id": id, "param": "demo.cadence", "value": 1}),
    );
    let cadence_control = r["control"].as_str().unwrap().to_string();
    let r = ok("run-service_checkpoint", json!({"id": id}));
    let ckpt_control = r["control"].as_str().unwrap().to_string();

    wait_until("the run to finish", || {
        status(&id)["run"]["state"] == json!("done")
    });
    let s = status(&id);
    let outcome = |c: &str| -> Value {
        s["controls"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["id"] == json!(c))
            .cloned()
            .unwrap_or_else(|| panic!("{c} in {s}"))
    };
    assert_eq!(outcome(&cadence_control)["status"], json!("applied"));
    assert_eq!(outcome(&ckpt_control)["status"], json!("applied"));
    let refused_dt = outcome(&dt_control);
    assert_eq!(refused_dt["status"], json!("refused"));
    assert!(
        refused_dt["reason"].as_str().unwrap().contains("restart"),
        "{refused_dt}"
    );
    assert_eq!(
        s["run"]["needs_restart"][0]["param"],
        json!("demo.dt"),
        "still pending for a resume"
    );
    assert!(s["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["kind"] == json!("checkpoint")));
    let ev = ok("run-service_events", json!({"id": id, "limit": 200}));
    let changed = ev["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == json!("changed"))
        .expect("a changed event");
    assert_eq!(
        changed["event"]["by"],
        json!(format!("control:{cadence_control}"))
    );
    assert_eq!(changed["event"]["to"], json!(1));
    assert!(
        run_convention::check_dir(&dir).ok(),
        "{}",
        run_convention::check_dir(&dir)
    );

    // A finished run cannot be steered.
    refused(
        "run-service_set",
        json!({"id": id, "param": "demo.cadence", "value": 2}),
        "conflict",
    );
    refused("run-service_stop", json!({"id": id}), "conflict");
    refused("run-service_checkpoint", json!({"id": id}), "conflict");
}

#[test]
fn stop_after_checkpoint_then_resume_applies_the_restart_values_as_a_child_run() {
    setup();
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("parent");
    let r = verb(
        "run-service_start",
        json!({
            "code": "demo-code",
            "params": {"demo.steps": 200, "demo.sleep_ms": 30, "demo.cadence": 2, "demo.label": "stop-me"},
            "dir": dir.to_string_lossy()
        }),
    );
    assert_eq!(r["ok"], json!(true), "{r}");
    let id = r["run"]["id"].as_str().unwrap().to_string();
    // Resuming a running run is refused.
    refused("run-service_resume", json!({"id": id}), "conflict");
    ok(
        "run-service_set",
        json!({"id": id, "param": "demo.dt", "value": 4.0}),
    );
    refused(
        "run-service_stop",
        json!({"id": id, "after": "later"}),
        "invalid-argument",
    );
    let r = ok("run-service_stop", json!({"id": id, "after": "checkpoint"}));
    assert_eq!(r["request"]["after"], json!("checkpoint"));
    wait_until("the run to stop", || {
        status(&id)["run"]["state"] == json!("cancelled")
    });
    let s = status(&id);
    assert_eq!(s["run"]["exit"]["code"], json!(110));
    assert!(s["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["kind"] == json!("checkpoint")));

    // Resume from the last checkpoint, with a shorter run: demo.steps is
    // `never`, so the code refuses that and the start reports it.
    let r = refused(
        "run-service_resume",
        json!({"id": id, "params": {"demo.steps": 10}, "wait": true}),
        "process-failed",
    );
    assert!(
        r["message"]
            .as_str()
            .unwrap()
            .contains("`demo.steps` is `never`"),
        "{r}"
    );
    // Resume as it is: dt 4.0 pending from the set rides along.
    let r = verb(
        "run-service_resume",
        json!({"id": id, "wait": true, "params": {"demo.cadence": 50}}),
    );
    assert_eq!(r["ok"], json!(true), "{r}");
    let child = &r["run"];
    assert_eq!(child["state"], json!("done"), "{child}");
    assert_eq!(child["lineage"]["parent"]["run"], json!(id));
    assert_eq!(child["lineage"]["parent"]["relation"], json!("resume"));
    assert_ne!(child["id"], json!(id));
    let ev = ok(
        "run-service_events",
        json!({"id": child["id"], "limit": 200}),
    );
    let changed: Vec<&Value> = ev["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == json!("changed"))
        .collect();
    assert!(
        changed
            .iter()
            .any(|c| c["event"]["param"] == json!("demo.dt")
                && c["event"]["to"] == json!(4.0)
                && c["event"]["by"] == json!("restart")),
        "{changed:?}"
    );
    assert!(
        changed
            .iter()
            .any(|c| c["event"]["param"] == json!("demo.cadence")
                && c["event"]["by"] == json!("restart")),
        "{changed:?}"
    );
    let parent = status(&id);
    assert!(
        parent["run"]["needs_restart"]
            .as_array()
            .is_none_or(|a| a.is_empty()),
        "cleared by the resume: {parent}"
    );
    let child_dir = PathBuf::from(child["dir"].as_str().unwrap());
    assert!(
        run_convention::check_dir(&child_dir).ok(),
        "{}",
        run_convention::check_dir(&child_dir)
    );
}

#[test]
fn a_run_whose_process_died_is_reported_orphaned() {
    setup();
    let t = tempfile::tempdir().unwrap();
    let r = verb(
        "run-service_start",
        json!({
            "code": "demo-code",
            "params": {"demo.steps": 1000, "demo.sleep_ms": 50, "demo.label": "orphan"},
            "dir": t.path().join("o").to_string_lossy()
        }),
    );
    assert_eq!(r["ok"], json!(true), "{r}");
    let id = r["run"]["id"].as_str().unwrap().to_string();
    let pid = r["run"]["pid"].as_u64().unwrap();
    assert!(std::process::Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .status()
        .unwrap()
        .success());
    wait_until("the orphan to be noticed", || {
        status(&id)["run"]["orphaned"] == json!(true)
    });
    let s = status(&id);
    assert_eq!(
        s["run"]["state"],
        json!("running"),
        "orphaned is derived; run.json is never rewritten"
    );
    // An orphan can be resumed from its checkpoint, if it has one; this one has none.
    refused("run-service_resume", json!({"id": id}), "not-found");
}

#[test]
fn a_sweep_expands_into_start_tasks_and_the_parent_job_finishes_when_the_last_run_has() {
    setup();
    let t = tempfile::tempdir().unwrap();
    // A misspelt name files nothing.
    let r = refused(
        "run-service_sweep",
        json!({"sweep": {"code": "demo-code", "base": {"demo.stepz": 2}, "vary": [{"param": "demo.dt", "values": [1, 2]}]}, "dir": t.path().join("bad").to_string_lossy()}),
        "invalid-argument",
    );
    assert!(r["message"].as_str().unwrap().contains("demo.stepz"), "{r}");
    assert!(!t.path().join("bad").join("sweep.json").exists());
    refused(
        "run-service_sweep",
        json!({"sweep": {"code": "no-such-code", "base": {}, "vary": [{"param": "x.y", "values": [1]}]}}),
        "not-found",
    );

    let dir = t.path().join("sweep");
    let r = ok(
        "run-service_sweep",
        json!({
            "sweep": {"code": "demo-code", "name": "dt-scan", "base": {"demo.steps": 3, "demo.label": "sweep"},
                      "vary": [{"param": "demo.dt", "values": [1.0, 2.0, 3.0]}]},
            "dir": dir.to_string_lossy()
        }),
    );
    assert_eq!(r["planned"], json!(3));
    let tasks = r["tasks"].as_array().unwrap().clone();
    assert_eq!(tasks.len(), 3);
    let job = r["job"].as_str().unwrap().to_string();
    let sweep_id = r["sweep"].as_str().unwrap().to_string();
    let sweep: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("sweep.json")).unwrap()).unwrap();
    assert_eq!(sweep["id"], json!(sweep_id));
    assert_eq!(sweep["runs"], json!([]));
    assert_eq!(sweep["koine"]["job"], json!(job));
    let shown = ok("task-service_show", json!({"id": job}));
    assert_eq!(shown["task"]["state"], json!("running"));

    // Play the scheduler: run each filed task's verb with its arguments.
    for task in &tasks {
        let t = ok("task-service_show", json!({"id": task}));
        assert_eq!(t["task"]["verb"], json!("run-service_start"));
        let args: Value = t["task"]["args"].clone();
        let args = if let Some(s) = args.as_str() {
            serde_json::from_str(s).unwrap()
        } else {
            args
        };
        assert_eq!(args["sweep"], json!(sweep_id));
        let r = ok("run-service_start", args);
        assert_eq!(r["run"]["state"], json!("done"));
        assert_eq!(r["run"]["sweep"], json!(sweep_id));
    }
    let sweep: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("sweep.json")).unwrap()).unwrap();
    assert_eq!(sweep["runs"].as_array().unwrap().len(), 3);
    assert!(sweep["koine"]["finished_at"].is_string(), "{sweep}");
    let shown = ok("task-service_show", json!({"id": job}));
    assert_eq!(shown["task"]["state"], json!("done"), "{shown}");
    let dts: Vec<Value> = sweep["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| {
            let s = status(id.as_str().unwrap());
            let p: Value = serde_json::from_str(
                &std::fs::read_to_string(
                    Path::new(s["run"]["dir"].as_str().unwrap()).join("params.json"),
                )
                .unwrap(),
            )
            .unwrap();
            p["demo.dt"]["value"].clone()
        })
        .collect();
    assert_eq!(dts, vec![json!(1.0), json!(2.0), json!(3.0)]);
}

#[test]
fn the_form_of_the_demo_code_validates_and_groups() {
    setup();
    let r = ok("run-service_form", json!({"code": "demo-code"}));
    let spec: impress_surface::spec::SurfaceSpec =
        serde_json::from_value(r["spec"].clone()).unwrap();
    let problems = impress_surface::validate::validate(&spec);
    assert!(problems.iter().all(|p| !p.is_error()), "{problems:?}");
    let text = r["spec"].to_string();
    assert!(
        text.contains("\"group-run\"")
            && text.contains("\"group-io\"")
            && text.contains("\"group-failure\"")
    );
}
