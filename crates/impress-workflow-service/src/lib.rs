//! W1 (plan-self-reflective-layer.md): the workflow verbs — `validate`,
//! `create`, `get`, `list`, `dry-run`, `enable`, `disable` — over
//! `impress-workflow`'s pure spec, validator and planner, store-tier (the
//! same reach `impress-surface-service` has into `impress-core`).
//!
//! `create` from an agent stores `state: proposed` and `enable` from an
//! agent is refused `review-pending` (D-R6) — see [`service`]'s module
//! docs for exactly how.

pub mod dto;
pub mod runner;
pub mod service;
pub mod store;

/// What [`WorkflowEngine::run_once`] runs against: the host's store as the
/// port and as its concrete type, which each step's call keeps.
pub use koine_store::process::CallStore;
pub use runner::{RunOutcome, WorkflowEngine, WorkflowLease};
pub use service::{DefaultWorkflowService, ImpressWorkflowService};
