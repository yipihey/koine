//! S1: the scenario verbs, `impress/scenario@1.0.0` records, and the Tier
//! A/B runners (docs/plan-self-reflective-layer.md § Scenarios).
//!
//! * [`store`] — `ScenarioStore`, the store side of a stored scenario.
//! * [`tier_a`] — [`tier_a::TierACaller`], the pipeline against a
//!   per-scenario scratch store (H-P2-3).
//! * `tier_b` — [`TierBCaller`], the shared loopback client against a
//!   running app. S2b moved the implementation to
//!   `impress_layout_service::scenario_caller` (its module docs say why: it
//!   needs `impress_service_core::loopback_token`, which a `pure`-tier kit crate
//!   like `impress-scenario` may not reach) so
//!   `impress-layout-service`/`impress-surface-service` can run their own
//!   Tier B scenario documents through it too — one runner, not three
//!   copies (SC-1). This crate re-exports it under its original name so
//!   nothing downstream of `impress_scenario_service::TierBCaller` moved.
//! * [`service`] — `ImpressScenarioService`, the five verbs.

pub mod dto;
pub mod record;
mod record_store;
pub mod service;
pub mod store;
pub mod tier_a;

pub use impress_layout_service::scenario_caller::{LoopbackClient, TierBCaller};
pub use service::{DefaultImpressScenarioService, ImpressScenarioService};
pub use store::{ScenarioRow, ScenarioStore};
pub use tier_a::TierACaller;
