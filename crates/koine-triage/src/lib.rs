//! Triage over any item: star, flag, tag and status, as a kernel and as
//! the `triage-service_*` verbs (ADR-0022 D5; docs/plan-koine-kit-moves.md KM2).
//!
//! * [`ops`] is the kernel, written against the store port
//!   (`impress_item::ItemStore`), so it runs over the memory, directory and
//!   SQLite backends alike. impress-core re-exports it as `triage_ops`.
//! * [`service`] (feature `service`) is the `#[impress_service]` `TriageService`. Its default
//!   deployment reads the process store (`koine_store::process`).
//! * [`package`] (feature `package`) is the capability's content as the built-in package
//!   `builtin:triage` (KM-D5): a skill describing the verbs.

pub mod ops;
#[cfg(feature = "service")]
pub mod service;

#[cfg(feature = "service")]
pub use service::{DefaultTriageService, TriageResult, TriageService};

#[cfg(feature = "package")]
mod pkg;
#[cfg(feature = "package")]
pub use pkg::{package, PACKAGE, SKILL, VERBS};
