//! `source-service`: the source convention's verbs
//! (docs/plan-source-convention.md), one `#[impress_service]` over every
//! adapter a host links.
//!
//! - [`types`] — the wire types: `bibliographic-work@1` ([`Work`]), the
//!   explain record ([`ExplainRecord`], S-D1), the query ([`Query`], S-D2)
//!   and [`Diagnostic`] (S-D7). To be replaced by `koine-sources` (S1).
//! - [`adapter`] — the [`Adapter`] trait a source package implements (S-D9).
//! - [`registry`] — the process-wide inventory of adapters; `explain` reads it.
//! - [`etiquette`] — the contact string and the per-source token bucket (S-D8).
//! - [`service`] — the six verbs (S-D3): `source-service_{explain, search,
//!   fetch, match-citation (alias match), related, export}`.
//!
//! A host links an adapter by registering it once at start:
//! `source_service::registry::register(Arc::new(source_crossref::Crossref::new()))`,
//! and names its contact with [`etiquette::set_contact`] (or
//! `KOINE_SOURCE_CONTACT`).

pub mod adapter;
pub mod dto;
pub mod etiquette;
pub mod ids;
pub mod registry;
pub mod service;
pub mod types;

#[cfg(test)]
mod selftest;

pub use adapter::{Adapter, ExportFormat, Page, PageRequest, Relation};
pub use registry::Registry;
pub use service::{DefaultSourceService, SourceService};
pub use types::{Diagnostic, DiagnosticStatus, ExplainRecord, Query, Work};

/// Keep the verbs' inventory registrations in a host that names nothing
/// else of this crate.
pub fn ensure_linked() {
    std::hint::black_box(DefaultSourceService::new as *const ());
}
