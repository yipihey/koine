//! The functions behind the Python module, without an interpreter, over the
//! inventory this test binary links (`perf-service`'s two verbs).

use koine_py::{call_local, list_verb_infos};
use serde_json::json;

fn link() {
    // Nothing else names the crate, and an unreferenced rlib can be dropped.
    let _ = perf_service::DefaultPerfService::new();
}

#[test]
fn list_verb_infos_lists_the_linked_inventory() {
    link();
    let verbs = list_verb_infos();
    let summary = verbs
        .iter()
        .find(|v| v.name == "perf-service_summary")
        .expect("perf-service_summary in the linked inventory");
    assert_eq!(summary.service, "perf-service");
    assert!(!summary.description.is_empty());
    assert!(summary.input_schema.is_object());
}

#[test]
fn call_local_runs_a_verb_through_the_pipeline() {
    link();
    let result = call_local("perf-service_summary", json!({})).expect("summary succeeds");
    assert!(result.is_object() || result.is_array(), "{result}");
}

#[test]
fn call_local_unknown_verb_is_an_error() {
    let error = call_local("does-not-exist_no-such-verb", json!({})).unwrap_err();
    assert!(
        error.contains("no such verb"),
        "unexpected message: {error}"
    );
}

#[test]
fn call_remote_without_a_running_app_refuses() {
    let error = koine_py::call_remote("no-such-app-xyz", "x_y", json!({})).unwrap_err();
    assert!(!error.is_empty());
}
