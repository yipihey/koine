//! `source-service`: the source convention's verbs
//! (docs/plan-source-convention.md), one `#[impress_service]` over every
//! adapter a host links.
//!
//! The vocabulary is `koine-sources` (S1): the kind
//! `bibliographic-work@1.0.0` ([`Work`]), the explain record
//! ([`ExplainRecord`], S-D1), the query ([`Query`], S-D2) and
//! [`Diagnostic`] (S-D7). [`query`] adds the helpers over them the service
//! and the adapters share (leaf clauses, the id-only merge).
//!
//! - [`adapter`] — the [`Adapter`] trait a source package implements (S-D9).
//! - [`registry`] — the process-wide inventory of adapters; `explain` reads it.
//! - [`etiquette`] — the contact string, the per-source token bucket and
//!   the daily budgets, enforced once at the [`etiquette::Gate`] (S-D8).
//! - [`federation`] — the concurrent fan-out with per-source timeouts and
//!   the identifier-only merge (S-D7).
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
pub mod federation;
pub mod ids;
pub mod query;
pub mod registry;
pub mod service;

#[cfg(test)]
mod selftest;

pub use adapter::{Adapter, ExportFormat, Page, PageRequest, Relation};
pub use koine_sources::{Diagnostic, ExplainRecord, Query, Status, Work};
pub use registry::Registry;
pub use service::{DefaultSourceService, SourceService};

/// Keep the verbs' inventory registrations in a host that names nothing
/// else of this crate.
pub fn ensure_linked() {
    std::hint::black_box(DefaultSourceService::new as *const ());
    // The kind the rows are (`bibliographic-work@1.0.0`), registered once.
    koine_sources::ensure_registered();
}
