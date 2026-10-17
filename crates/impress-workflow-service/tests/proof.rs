//! W1's row proof (plan-self-reflective-layer.md), run against the real
//! pipeline — `impress_service_core::pipeline::invoke_on` with
//! `VerbDescriptor::find`, never the service struct's methods directly.
//! That is deliberate, not incidental: `workflow_create`/`workflow_enable`
//! decide D-R6 from `pipeline::context::current()`, which only exists
//! *inside* a call the pipeline itself set up — calling
//! `DefaultWorkflowService::workflow_create(...)` straight would see no
//! caller at all and could never reproduce the agent/person distinction
//! this file proves. Every call below therefore goes through [`call`],
//! which wraps `invoke_on`: the one path in or out.

use std::sync::Arc;

use impress_service_core::pipeline::{self, Call};
use impress_service_core::VerbDescriptor;
use koine_store_memory::MemoryStore;
use serde_json::{json, Value};

fn store() -> Arc<MemoryStore> {
    Arc::new(MemoryStore::new())
}

/// The one path every test in this file calls a verb through — see the
/// module docs for why this is the enforcement, not a convenience.
fn call(store: &Arc<MemoryStore>, verb: &str, call: Call) -> Value {
    let descriptor = VerbDescriptor::find(verb).unwrap_or_else(|| panic!("verb '{verb}' linked"));
    impress_service_core::runtime::block_on(pipeline::invoke_on(store.clone(), descriptor, call))
        .expect("the pipeline itself did not error")
}

fn manual_spec(state: &str) -> Value {
    json!({"spec": {
        "wire_version": 1,
        "name": "fixture",
        "state": state,
        "author": {"kind": "agent", "name": "test"},
        "trigger": {"manual": {}},
        "steps": []
    }})
}

/// Fixture: the validator's schedule-under-60s finding, reachable end to
/// end through `workflow_validate`.
#[test]
fn validator_fixture_reaches_the_verb() {
    let store = store();
    let spec = json!({
        "wire_version": 1,
        "name": "fixture",
        "state": "enabled",
        "author": {"kind": "person"},
        "trigger": {"schedule": {"every": "10s"}},
        "steps": []
    });
    let result = call(
        &store,
        "impress-workflow-service_workflow-validate",
        Call::agent("test", json!({"spec": spec})),
    );
    assert_eq!(result["ok"], false, "{result}");
    assert!(result["problems"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["path"] == "/trigger/schedule/every"));
}

/// The proof: `dry-run` returns `would_call`, and nothing is executed (the
/// verb it names, `imbib-library-service_delete-publications-undoable`, is
/// never linked into this test binary, so an execution would panic on an
/// unknown verb rather than silently succeed).
#[test]
fn dry_run_returns_would_call() {
    let store = store();
    let spec = json!({
        "wire_version": 1,
        "name": "fixture",
        "state": "enabled",
        "author": {"kind": "person"},
        "trigger": {"manual": {}},
        "steps": [
            {"call": {"verb": "imbib-library-service_delete-publications-undoable",
                       "args": {"ids": ["{{event.value.id}}"]}}}
        ]
    });
    let created = call(
        &store,
        "impress-workflow-service_workflow-create",
        Call::person(json!({"spec": spec})),
    );
    assert_eq!(created["ok"], true, "{created}");
    let id = created["id"].as_str().unwrap().to_string();

    let dry_run = call(
        &store,
        "impress-workflow-service_workflow-dry-run",
        Call::agent("test", json!({"id": id, "event": {"id": "p-1"}})),
    );
    assert_eq!(dry_run["ok"], true, "{dry_run}");
    let would_call = dry_run["would_call"].as_array().unwrap();
    assert_eq!(would_call.len(), 1);
    assert_eq!(
        would_call[0]["verb"],
        "imbib-library-service_delete-publications-undoable"
    );
    assert_eq!(would_call[0]["args"]["ids"][0], "p-1");
}

/// The proof: an agent's `create` stores `proposed`, whatever the spec
/// itself asked for.
#[test]
fn agent_create_stores_proposed() {
    let store = store();
    let result = call(
        &store,
        "impress-workflow-service_workflow-create",
        Call::agent("test-agent", manual_spec("enabled")),
    );
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["spec"]["state"], "proposed");
}

/// A person's `create` keeps the state the spec named.
#[test]
fn person_create_keeps_the_named_state() {
    let store = store();
    let result = call(
        &store,
        "impress-workflow-service_workflow-create",
        Call::person(manual_spec("enabled")),
    );
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["spec"]["state"], "enabled");
}

/// The proof: `enable` from an agent is `review-pending`, and the row is
/// left exactly as it was.
#[test]
fn agent_enable_is_review_pending() {
    let store = store();
    let created = call(
        &store,
        "impress-workflow-service_workflow-create",
        Call::agent("test-agent", manual_spec("proposed")),
    );
    let id = created["id"].as_str().unwrap().to_string();

    let enabled = call(
        &store,
        "impress-workflow-service_workflow-enable",
        Call::agent("test-agent", json!({"id": id.clone()})),
    );
    assert_eq!(enabled["ok"], false, "{enabled}");
    assert_eq!(enabled["code"], "review-pending");

    let after = call(
        &store,
        "impress-workflow-service_workflow-get",
        Call::person(json!({"id": id})),
    );
    assert_eq!(
        after["spec"]["state"], "proposed",
        "unchanged by the review"
    );
}

/// A person's `enable` on the same `proposed` row runs it.
#[test]
fn person_enable_runs_it() {
    let store = store();
    let created = call(
        &store,
        "impress-workflow-service_workflow-create",
        Call::agent("test-agent", manual_spec("proposed")),
    );
    let id = created["id"].as_str().unwrap().to_string();

    let enabled = call(
        &store,
        "impress-workflow-service_workflow-enable",
        Call::person(json!({"id": id.clone()})),
    );
    assert_eq!(enabled["ok"], true, "{enabled}");
    assert_eq!(enabled["spec"]["state"], "enabled");

    let after = call(
        &store,
        "impress-workflow-service_workflow-get",
        Call::person(json!({"id": id})),
    );
    assert_eq!(after["spec"]["state"], "enabled");
}

/// `disable` carries no review restriction, even for an agent.
#[test]
fn agent_disable_is_unrestricted() {
    let store = store();
    let created = call(
        &store,
        "impress-workflow-service_workflow-create",
        Call::person(manual_spec("enabled")),
    );
    let id = created["id"].as_str().unwrap().to_string();

    let disabled = call(
        &store,
        "impress-workflow-service_workflow-disable",
        Call::agent("test-agent", json!({"id": id})),
    );
    assert_eq!(disabled["ok"], true, "{disabled}");
    assert_eq!(disabled["spec"]["state"], "disabled");
}

/// `list` names the stored workflow without its spec.
#[test]
fn list_shows_a_summary() {
    let store = store();
    call(
        &store,
        "impress-workflow-service_workflow-create",
        Call::person(manual_spec("enabled")),
    );
    let listed = call(
        &store,
        "impress-workflow-service_workflow-list",
        Call::person(json!({})),
    );
    assert_eq!(listed["ok"], true, "{listed}");
    assert_eq!(listed["workflows"].as_array().unwrap().len(), 1);
    assert_eq!(listed["workflows"][0]["name"], "fixture");
}

/// Strict args: an unknown field is refused, naming it.
#[test]
fn strict_args_refuse_an_unknown_field() {
    let store = store();
    let result = call(
        &store,
        "impress-workflow-service_workflow-list",
        Call::person(json!({"bogus": true})),
    );
    assert_eq!(result["ok"], false, "{result}");
    assert_eq!(result["code"], "invalid-argument");
}
