//! The workflow vocabulary (plan-self-reflective-layer.md § Workflows).
//!
//! A workflow is a stored, reviewable action sequence: a trigger (what
//! starts it), guards (when it may not run), the surface's own `params` /
//! `sources` / `steps` vocabulary unchanged, and review metadata (D-R6). No
//! new action kinds — `steps` is `Vec<impress_surface::Action>`, the same
//! type a surface's `on_submit` carries, so `impress_surface::reduce` needs
//! no change to run one (see [`crate::plan::plan`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use impress_surface::spec::{Action, ParamDecl, Source};

/// The one wire version this crate understands.
pub const WORKFLOW_WIRE_VERSION: u64 = 1;

/// The canonical stored-record ref (`schema-refs.json`,
/// `impress_store_service::history_service::WORKFLOW_SCHEMA`). Kept here as
/// the second half of that pair's single source of truth: the schema module
/// in `impress-core` names this same string in its own doc comment.
pub const WORKFLOW_SCHEMA_REF: &str = crate::schema_names::IMPRESS_WORKFLOW;

/// A workflow's lifecycle state (D-R6). `Proposed`: written by an agent, not
/// yet reviewed by a person. `ReviewPending`: an agent asked to enable a
/// `Proposed` workflow, which is a review rather than an enable (ADR-0034
/// D3) — the workflow stays `Proposed` in storage; `ReviewPending` is the
/// *answer* `workflow-service_enable` gives an agent caller, not a fourth
/// stored state (`enable_from_agent_stays_proposed` below).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum WorkflowState {
    Enabled,
    Disabled,
    Proposed,
    /// P3's rename pass removed a verb a stored workflow's step still names
    /// (plan § Workflows, "Fails loudly").
    Broken,
}

impl WorkflowState {
    pub fn as_str(&self) -> &'static str {
        match self {
            WorkflowState::Enabled => "enabled",
            WorkflowState::Disabled => "disabled",
            WorkflowState::Proposed => "proposed",
            WorkflowState::Broken => "broken",
        }
    }
}

/// Who authored the workflow: a person, or an agent naming itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Author {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Author {
    pub fn is_agent(&self) -> bool {
        self.kind == "agent"
    }
}

/// Exactly one of these starts a run (plan § Workflows). Untagged by shape,
/// like [`impress_surface::spec::Source`]: the variant is picked by which
/// key is present, and more than one (or none) is a validation error rather
/// than a silently-picked default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Schedule {
        /// A duration string (`"24h"`, `"90s"`); validated ≥ 60s.
        every: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<String>,
    },
    Store {
        kinds: Vec<String>,
        #[serde(default)]
        ops: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        debounce_ms: Option<u64>,
    },
    Job {
        verb: String,
        state: String,
    },
    /// A store trigger on a message kind, named for readers (plan § Workflows).
    Message {
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        folder: Option<String>,
    },
    Call {
        verb: String,
    },
    Manual {},
}

/// Guards a trigger's firing (plan § Workflows). `not_before_startup_s`
/// mirrors `SchedulerConfig::start_delay` (W2 wires the runtime that reads
/// it); `requires` is a list of free-text capability tags (`"app:imbib"`)
/// the runtime checks before running.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Guards {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before_startup_s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_runs_per_hour: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
}

/// Review metadata (D-R6): a `Proposed` workflow may not be enabled by an
/// agent caller without a person's confirm.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Review {
    #[serde(default)]
    pub required: bool,
}

/// The stored document (`impress/workflow@1.0.0`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WorkflowSpec {
    #[serde(default = "default_wire_version")]
    pub wire_version: u64,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub state: WorkflowState,
    pub author: Author,
    pub trigger: Trigger,
    #[serde(default)]
    pub guards: Guards,
    #[serde(default)]
    pub params: Vec<ParamDecl>,
    #[serde(default)]
    pub sources: BTreeMap<String, Source>,
    pub steps: Vec<Action>,
    #[serde(default)]
    pub review: Review,
}

fn default_wire_version() -> u64 {
    WORKFLOW_WIRE_VERSION
}

impl WorkflowSpec {
    /// The workflow trigger's own event payload, wrapped exactly as the
    /// plan describes it (plan § Workflows, "The trigger payload is the
    /// event"): `{"widget": "trigger", "kind": "submit", "value": {…}}`.
    /// `plan::plan` builds this from the trigger source the runtime hands
    /// it; this method exists so a test or a caller can build the same
    /// shape without duplicating it.
    pub fn trigger_event(value: Value) -> impress_surface::spec::Event {
        impress_surface::spec::Event {
            widget: TRIGGER_WIDGET_ID.into(),
            kind: impress_surface::spec::EventKind::Submit,
            value,
        }
    }
}

/// The synthetic node id `plan::plan` gives the workflow's steps, and the
/// event's `widget` a trigger always names — never rendered, never shown to
/// a person; it only exists so `impress_surface::reduce` has a node to look
/// up (see that crate's `walk_with_ids`).
pub const TRIGGER_WIDGET_ID: &str = "trigger";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_schedule_trigger() {
        let spec = WorkflowSpec {
            wire_version: 1,
            name: "imbib.retention-cleanup".into(),
            description: "…".into(),
            state: WorkflowState::Enabled,
            author: Author {
                kind: "person".into(),
                name: None,
            },
            trigger: Trigger::Schedule {
                every: "24h".into(),
                at: Some("03:00".into()),
            },
            guards: Guards {
                not_before_startup_s: Some(90),
                ..Default::default()
            },
            params: vec![],
            sources: BTreeMap::new(),
            steps: vec![],
            review: Review { required: false },
        };
        let json = serde_json::to_value(&spec).unwrap();
        let back: WorkflowSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn agent_author_is_recognized() {
        assert!(Author {
            kind: "agent".into(),
            name: Some("history-service".into())
        }
        .is_agent());
        assert!(!Author {
            kind: "person".into(),
            name: None
        }
        .is_agent());
    }
}
