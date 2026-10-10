//! The scenario spec, validator and interpreter (docs/plan-self-reflective-layer.md
//! S1, § Scenarios; finding SC-1/SC-2).
//!
//! Pure kit tier (docs/kit-manifest.md's discipline: no `impress-core`, no
//! workspace crate outside `impress-surface` and `impress-service-core`).
//! `impress-scenario-service` is the store-tier crate that runs a scenario
//! for real, against a scratch store (Tier A, through
//! `impress_service_core::pipeline::invoke_on`, H-P2-3) or a running app
//! (Tier B, over the shared HTTP client).

pub mod interpret;
pub mod spec;
pub mod template;
pub mod validate;

pub use interpret::{run, CallOutcome, Caller};
pub use spec::{
    BestEffortCall, BestEffortStep, CallStep, Check, EventBody, EventStep, Expect, ExpectEffects,
    FieldExpect, GestureStep, LogCursorCapture, LogWait, Requires, Scenario, SeedRecord, Step,
    StorePredicate, StoreStep, Tier, WaitBody, WaitStep,
};
pub use validate::{validate, Problem};
