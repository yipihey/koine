//! The row's proof, run as a black-box integration test against this
//! crate's public API only (unit tests inside `src/` already cover the
//! same ground from the inside — this file is the one a reviewer reads to
//! see the fixtures without opening `src/`).

use std::collections::BTreeMap;

use impress_surface::spec::Action;
use impress_workflow::{
    plan, validate, Author, Guards, Review, Trigger, WorkflowSpec, WorkflowState,
};
use serde_json::json;

fn manual(steps: Vec<Action>) -> WorkflowSpec {
    WorkflowSpec {
        wire_version: 1,
        name: "fixture".into(),
        description: String::new(),
        state: WorkflowState::Enabled,
        author: Author {
            kind: "person".into(),
            name: None,
        },
        trigger: Trigger::Manual {},
        guards: Guards::default(),
        params: vec![],
        sources: BTreeMap::new(),
        steps,
        review: Review::default(),
    }
}

#[test]
fn validator_fixture_schedule_under_60s_is_refused() {
    let mut wf = manual(vec![]);
    wf.trigger = Trigger::Schedule {
        every: "10s".into(),
        at: None,
    };
    assert!(validate(&wf)
        .iter()
        .any(|p| p.path == "/trigger/schedule/every"));
}

#[test]
fn dry_run_returns_would_call_never_executed() {
    let wf = manual(vec![Action::Call {
        verb: "imbib-library-service_delete-publications-undoable".into(),
        args: json!({"ids": []}),
        into: None,
        each: None,
    }]);
    let (_, effects) = plan(&wf, json!({}), &json!({}), &json!({}), &json!({})).unwrap();
    // The proof: `plan` (what `dry-run` exposes as `would_call`) hands back
    // an effect describing the call — it never invokes anything, because
    // this crate has no pipeline, no store and no async runtime to invoke
    // one with.
    assert_eq!(effects.len(), 1);
}
