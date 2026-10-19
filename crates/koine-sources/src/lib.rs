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
//! - [`score`]: the citation-resolution [`score::Scorer`], its [`score::Quirks`]
//!   (every recorded Swift defect as a switch) and the acceptance rules,
//!   pinned by `fixtures/citation-scorer-goldens.json` (S-D6).
//! - [`cascade`]: the three-step resolution over [`cascade::LocalLookup`] and
//!   [`cascade::RemoteSearch`] trait objects, pure; and the one
//!   source-priority table (S-D6).
//! - [`latex`]: the query-side LaTeX decoder the scorer runs.
//!
//! This crate has no HTTP and no adapters; an adapter crate depends on it.

pub mod cascade;
pub mod diagnostics;
pub mod explain;
pub mod kind;
pub mod latex;
pub mod query;
pub mod score;

pub use diagnostics::{Diagnostic, Status};
pub use explain::{ExplainError, ExplainRecord};
pub use kind::{ensure_registered, Work, BIBLIOGRAPHIC_WORK};
pub use query::Query;
pub use score::{Acceptance, CitationInput, Evidence, Quirks, Rule, Scored, Scorer};
