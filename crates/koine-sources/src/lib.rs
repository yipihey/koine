//! The source convention's vocabulary (docs/plan-source-convention.md).
//!
//! How an online database declares itself to koine, as four small pieces:
//!
//! - [`kind`]: the canonical record, `bibliographic-work@1.0.0`: CSL-JSON
//!   plus an `ids` map, versioned `locations`, `provenance` and `metrics`
//!   (S-D4), and the typed [`kind::Work`] that reads and writes it.
//! - [`explain`]: the [`explain::ExplainRecord`] an adapter emits to say what
//!   it can search, sort by, look up and fill (S-D1).
//! - [`query`]: the `bib.*` context set and the JSON [`query::Query`] over it
//!   (S-D2). [`query::Query::covered_by`] says which clauses a source cannot
//!   answer.
//! - [`diagnostics`]: the [`diagnostics::Diagnostic`] a source's outcome is
//!   reported as, so a clause or a source that did not answer is visible
//!   instead of silently dropped (S-D7).
//!
//! This crate has no HTTP and no adapters; an adapter crate depends on it.

pub mod diagnostics;
pub mod explain;
pub mod kind;
pub mod query;

pub use diagnostics::{Diagnostic, Status};
pub use explain::{ExplainError, ExplainRecord};
pub use kind::{ensure_registered, Work, BIBLIOGRAPHIC_WORK};
pub use query::Query;
