//! The notebook end to end over a memory store: the sink writes what the
//! pipeline records, runs group calls, and the verbs read the log back
//! (docs/plan-koine-notebook.md § What pins this). The sink, the process store
//! and the open run are process-wide, so the tests take turns.

use std::sync::{Arc, Mutex, MutexGuard};

use impress_item::AttributedStore;
use impress_service_core::pipeline::{self, Call};
use impress_service_core::{call, VerbDescriptor, VerbHandle};
use koine_notebook::{records, SinkConfig};
use koine_store::RecordingStore;
use koine_store_memory::MemoryStore;
use serde_json::{json, Value};

static TURN: Mutex<()> = Mutex::new(());

fn fresh() -> (MutexGuard<'static, ()>, Arc<dyn AttributedStore>) {
    let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    koine_notebook::ensure_linked();
    let store: Arc<dyn AttributedStore> =
        Arc::new(RecordingStore::new(Arc::new(MemoryStore::new())));
    koine_store::process::install(store.clone());
    impress_service_core::pipeline::audit::set_current_run(None);
    koine_notebook::install(
        store.clone(),
        SinkConfig::new("test-host", "test-app", "9.9.9"),
    );
    (turn, store)
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
    let _store = fresh();
    let verbs: Vec<VerbHandle> = call::descriptors()
        .filter(|v| v.service() == "notebook-service")
        .collect();
    assert_eq!(verbs.len(), 10);
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
    verb("notebook-service_run-stop", json!({}));
}

#[test]
fn a_call_is_logged_with_its_steps_run_host_and_build() {
    let (_turn, store) = fresh();
    let started = verb(
        "notebook-service_run-start",
        json!({"name": "sweep", "record_reads": false}),
    );
    let run_id = started["run"]["id"].as_str().unwrap().to_string();

    // An outer call that runs two nested verbs: its record carries both as
    // steps, with self time.
    let outer = VerbDescriptor::find("notebook-service_annotate").unwrap();
    let answer = impress_service_core::runtime::block_on(pipeline::invoke_with(
        outer,
        Call::agent("tester", json!({})),
        |_| async {
            call::call_async("notebook-service_runs", json!({}))
                .await
                .unwrap();
            call::call_async("notebook-service_timeline", json!({}))
                .await
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(3));
            Ok(json!({"ok": true, "call": pipeline::context::current_call_id()}))
        },
    ))
    .unwrap();
    let call_id = answer["call"].as_str().unwrap().to_string();

    let entry = verb("notebook-service_entry", json!({"call_id": call_id}));
    assert_eq!(entry["ok"], true, "{entry}");
    let labels: Vec<&str> = entry["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["label"].as_str().unwrap())
        .collect();
    assert_eq!(
        labels,
        [
            "notebook-service_annotate",
            "  notebook-service_runs",
            "  notebook-service_timeline"
        ]
    );
    assert!(entry["steps"][0]["total_ms"].as_f64().unwrap() >= 3.0);
    assert_eq!(entry["record"]["run"], run_id);
    assert_eq!(entry["record"]["host"], "test-host");
    assert_eq!(entry["record"]["build"]["version"], "9.9.9");
    assert_eq!(entry["summary"]["caller"], "agent:tester");
    koine_plot::parse_spec(&entry["plot"]).expect("the entry's plot is a plot spec");

    // The timeline shows it (the notebook's own verbs only when asked for).
    let timeline = verb(
        "notebook-service_timeline",
        json!({"run": run_id, "verb": "notebook-service_annotate"}),
    );
    assert_eq!(timeline["rows"][0]["call_id"], call_id);
    assert_eq!(timeline["rows"][0]["steps"], 2);
    // Without a verb filter the notebook's own reads are left out; its writes
    // (this run's start, the annotation) are research and stay.
    let default = verb("notebook-service_timeline", json!({"run": run_id}));
    let verbs: Vec<&str> = default["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["verb"].as_str().unwrap())
        .collect();
    assert!(verbs.contains(&"notebook-service_annotate"), "{verbs:?}");
    assert!(!verbs.contains(&"notebook-service_timeline"), "{verbs:?}");

    // A note, then the export carries the call and the note.
    let note = verb(
        "notebook-service_annotate",
        json!({"call_id": call_id, "text": "Load dominates."}),
    );
    assert_eq!(note["ok"], true, "{note}");
    let entry = verb("notebook-service_entry", json!({"call_id": call_id}));
    assert_eq!(entry["notes"][0]["text"], "Load dominates.");

    let stopped = verb("notebook-service_run-stop", json!({}));
    assert_eq!(stopped["run"]["status"], "closed");
    let runs = verb("notebook-service_runs", json!({}));
    assert_eq!(runs["open"], Value::Null);
    assert!(runs["runs"][0]["calls"].as_u64().unwrap() >= 1);

    let export = verb("notebook-service_export", json!({"run": run_id}));
    let markdown = export["markdown"].as_str().unwrap();
    assert!(markdown.contains("# Run: sweep"), "{markdown}");
    assert!(markdown.contains("Load dominates."), "{markdown}");
    assert!(markdown.contains("test-host"), "{markdown}");
    drop(store);
}

#[test]
fn a_run_that_records_reads_logs_read_only_calls_and_a_restart_reopens_it() {
    let (_turn, store) = fresh();
    verb(
        "notebook-service_run-start",
        json!({"name": "reads", "record_reads": true}),
    );
    // A read-only call that is not notebook plumbing is recorded now.
    let ids = store
        .query(&impress_item::ItemQuery::default())
        .unwrap()
        .len();
    let rows = verb("notebook-service_runs", json!({}));
    let run_id = rows["open"].as_str().unwrap().to_string();
    // A read-only call that failed is never plumbing, so the run records it.
    let inside = VerbDescriptor::find("notebook-service_compare").unwrap();
    impress_service_core::runtime::block_on(pipeline::invoke_with(
        inside,
        Call::person(json!({"verb": "x"})),
        |_| async { Ok(json!({"ok": false, "code": "boom", "message": "b"})) },
    ))
    .unwrap();
    let timeline = verb(
        "notebook-service_timeline",
        json!({"run": run_id, "verb": "notebook-service_compare", "failed_only": true}),
    );
    assert_eq!(timeline["total"], 1, "{timeline}");
    assert!(ids > 0);

    // "Restart": a new sink over the same store re-opens the run.
    impress_service_core::pipeline::audit::set_current_run(None);
    koine_notebook::install(store.clone(), SinkConfig::new("h", "a", "1"));
    assert_eq!(
        impress_service_core::pipeline::audit::current_run().map(|r| r.id),
        Some(run_id)
    );
    verb("notebook-service_run-stop", json!({}));
    assert!(impress_service_core::pipeline::audit::current_run().is_none());
}

#[test]
fn replay_runs_a_replayable_call_again_and_refuses_a_summary() {
    let (_turn, store) = fresh();
    let started = verb("notebook-service_run-start", json!({"name": "r"}));
    assert_eq!(started["ok"], true);
    let timeline = verb(
        "notebook-service_timeline",
        json!({"verb": "notebook-service_run-start"}),
    );
    let call_id = timeline["rows"][0]["call_id"].as_str().unwrap().to_string();
    let entry = verb("notebook-service_entry", json!({"call_id": call_id}));
    let replayable = entry["replayable"].as_bool().unwrap();
    let replay = verb("notebook-service_replay", json!({"call_id": call_id}));
    if replayable {
        assert_eq!(replay["ok"], true, "{replay}");
        assert_eq!(replay["replay_of"], call_id);
        assert!(replay["call_id"].is_string(), "{replay}");
        assert_eq!(replay["answer"]["run"]["name"], "r");
    } else {
        assert_eq!(replay["code"], "not-replayable", "{replay}");
    }
    verb("notebook-service_run-stop", json!({}));
    drop(store);
}

#[test]
fn prune_keeps_records_in_a_run_and_cuts_the_oldest_outside() {
    let (_turn, store) = fresh();
    for _ in 0..5 {
        let outside = VerbDescriptor::find("notebook-service_run-stop").unwrap();
        impress_service_core::runtime::block_on(pipeline::invoke_with(
            outside,
            Call::person(json!({})),
            |_| async { Ok(json!({"ok": true})) },
        ))
        .unwrap();
    }
    verb("notebook-service_run-start", json!({"name": "kept"}));
    let pruned = verb("notebook-service_prune", json!({"keep": 1}));
    assert!(pruned["deleted"].as_u64().unwrap() >= 4, "{pruned}");
    let left = records::all_of(store.as_ref(), records::VERB_CALL, &[], true).unwrap();
    let in_run = left
        .iter()
        .filter(|i| {
            matches!(i.payload.get("run"), Some(impress_item::Value::String(s)) if !s.is_empty())
        })
        .count();
    assert!(in_run >= 1);
    verb("notebook-service_run-stop", json!({}));
}

#[test]
fn the_notebook_surface_is_a_valid_spec_over_linked_verbs() {
    let spec: Value = serde_json::from_str(koine_notebook::NOTEBOOK_SURFACE).unwrap();
    assert_eq!(spec["name"], koine_notebook::NOTEBOOK_SURFACE_NAME);
    let (parsed, problems) = impress_surface::validate_json(&spec);
    let errors: Vec<_> = problems
        .iter()
        .filter(|p| p.severity == impress_surface::Severity::Error)
        .collect();
    assert!(parsed.is_some() && errors.is_empty(), "{problems:?}");
}
