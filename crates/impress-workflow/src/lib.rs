//! W1 (plan-self-reflective-layer.md): the workflow spec, its validator and
//! its pure planner — `pure` tier (`docs/kit-manifest.md`).
//!
//! A workflow (`impress/workflow@1.0.0`) is a stored, reviewable action
//! sequence: a trigger instead of a widget, the surface action vocabulary
//! for its `steps` (reused, never forked — WF-2), guards and review
//! metadata (D-R6). This crate owns:
//!
//! - [`spec`] — [`spec::WorkflowSpec`], [`spec::Trigger`], [`spec::Guards`],
//!   [`spec::Review`], serde + (behind `schema`) `JsonSchema`.
//! - [`validate::validate`] — every structural and workflow-only problem a
//!   spec can have.
//! - [`plan::plan`] — given one trigger event already fired, what the
//!   workflow's steps do: `impress_surface::reduce` over a synthetic
//!   single-node spec, returning the same `Effect`s a surface's own
//!   `reduce` would (a `Call` is returned, never executed here).
//!
//! - [`trigger::tick`] (W2) — pure: `(enabled workflows, clock, signals) ->
//!   which fire right now`, with the `start_delay` startup rule built in.
//!
//! **No I/O, no store, no async, no verb inventory.** A host resolving
//! store/job/call signals and running a fired workflow's steps through the
//! pipeline is `impress-workflow-service` (a sibling `store`-tier crate) and
//! its two hosts (`impel-taskd`, the app's FFI tick) — this crate decides,
//! it never fetches or calls.

pub mod plan;
pub mod spec;
pub mod trigger;
pub mod validate;

pub use plan::{plan, to_surface_spec, Effect, ReduceError};
pub use spec::{
    Author, Guards, Review, Trigger, WorkflowSpec, WorkflowState, WORKFLOW_SCHEMA_REF,
    WORKFLOW_WIRE_VERSION,
};
pub use trigger::{parse_duration_ms, Clock, DueRun, EngineCursors, Signal, SystemClock};
pub use validate::{validate, validate_with, Problem, Severity, VerbEffects};

// Manifest metadata without a runtime dependency back on the store.
#[allow(dead_code)]
mod schema_names {
    include!(concat!(env!("OUT_DIR"), "/schema_ref_names.rs"));
}
