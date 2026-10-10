//! `plan(workflow, trigger_event, state) -> Vec<Effect>` (plan § Workflows).
//!
//! A workflow is a [`impress_surface::spec::SurfaceSpec`] with no `root` of
//! its own — this module builds one synthetic node (id `"trigger"`, kind
//! [`impress_surface::spec::NodeKind::Divider`], chosen only because it
//! carries no other fields to keep in step) whose `on_submit` is the
//! workflow's `steps`, and drives `impress_surface::reduce` with a `submit`
//! event on that node. Nothing in `reduce` changes to run a workflow —
//! that is the whole point of reusing it rather than forking it (WF-2).
//!
//! Running a workflow **on a schedule** (turning `(workflows, clock,
//! cursors)` into which workflows should fire right now) is W2's planner,
//! not this one; this crate's `plan` answers "given this one trigger event
//! already fired, what does the workflow do", which is the piece W1 owns
//! and `dry-run` needs.

pub use impress_surface::reduce::{Effect, ReduceError};
use impress_surface::spec::{Event, Node, NodeKind, SurfaceSpec};

use crate::spec::{WorkflowSpec, TRIGGER_WIDGET_ID};

/// Builds the synthetic single-node `SurfaceSpec` a workflow's `steps` run
/// inside. `state` is the workflow's current state document (the surface's
/// `state`, not [`crate::spec::WorkflowState`]).
pub fn to_surface_spec(workflow: &WorkflowSpec, state: serde_json::Value) -> SurfaceSpec {
    let root = Node::leaf(NodeKind::Divider)
        .with_id(TRIGGER_WIDGET_ID)
        .with_on_submit(workflow.steps.clone());
    SurfaceSpec {
        surface: impress_surface::spec::SURFACE_VERSION.into(),
        name: workflow.name.clone(),
        params: workflow.params.clone(),
        state,
        sources: workflow.sources.clone(),
        root,
    }
}

/// See the module docs. `trigger_event` is the trigger's own payload
/// (whatever the store row, the finished job, or the schedule tick handed
/// the runtime) — [`crate::spec::WorkflowSpec::trigger_event`] wraps it as
/// the `{"widget": "trigger", "kind": "submit", "value": …}` event this
/// function drives `reduce` with. `sources` are the workflow's sources,
/// already resolved by the caller (this crate does no I/O); pass
/// `serde_json::json!({})` when the workflow declares none.
pub fn plan(
    workflow: &WorkflowSpec,
    trigger_value: serde_json::Value,
    state: &serde_json::Value,
    params: &serde_json::Value,
    sources: &serde_json::Value,
) -> Result<(serde_json::Value, Vec<Effect>), ReduceError> {
    let surface = to_surface_spec(workflow, state.clone());
    let event: Event = crate::spec::WorkflowSpec::trigger_event(trigger_value);
    impress_surface::reduce::reduce(&surface, state, params, sources, &event)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use impress_surface::spec::Action;
    use serde_json::json;

    use super::*;
    use crate::spec::{Author, Guards, Review, Trigger, WorkflowState};

    fn manual_workflow(steps: Vec<Action>) -> WorkflowSpec {
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

    /// The proof: `dry-run` (here, the pure `plan`) returns the call as an
    /// `Effect::Call`, never executed.
    #[test]
    fn a_call_step_plans_as_a_call_effect_not_an_execution() {
        let wf = manual_workflow(vec![Action::Call {
            verb: "imbib-library-service_delete-publications-undoable".into(),
            args: json!({"ids": ["{{event.value.stale_id}}"]}),
            into: None,
            each: None,
        }]);
        let (_, effects) = plan(
            &wf,
            json!({"stale_id": "p-1"}),
            &json!({}),
            &json!({}),
            &json!({}),
        )
        .expect("plans");
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::Call { verb, args, .. } => {
                assert_eq!(verb, "imbib-library-service_delete-publications-undoable");
                assert_eq!(args["ids"][0], "p-1");
            }
            other => panic!("expected a Call effect, got {other:?}"),
        }
    }

    #[test]
    fn a_set_step_updates_state_directly_with_no_effect() {
        let wf = manual_workflow(vec![Action::Set {
            path: "state.last_run".into(),
            value: json!("{{event.value.now}}"),
        }]);
        let (state, effects) = plan(
            &wf,
            json!({"now": "2026-09-27T00:00:00Z"}),
            &json!({}),
            &json!({}),
            &json!({}),
        )
        .expect("plans");
        assert!(effects.is_empty());
        assert_eq!(state["last_run"], "2026-09-27T00:00:00Z");
    }

    #[test]
    fn each_fans_out_one_effect_per_selected_id() {
        let wf = manual_workflow(vec![Action::Call {
            verb: "triage-service_set-starred".into(),
            args: json!({"id": "{{item}}", "starred": false}),
            into: None,
            each: Some("event.value.ids".into()),
        }]);
        let (_, effects) = plan(
            &wf,
            json!({"ids": ["a", "b", "c"]}),
            &json!({}),
            &json!({}),
            &json!({}),
        )
        .expect("plans");
        assert_eq!(effects.len(), 3);
    }
}
