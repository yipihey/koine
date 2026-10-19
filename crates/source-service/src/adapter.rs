//! The adapter: one online database, reachable through one implementation
//! of [`Adapter`] (S-D1, S-D9). An adapter emits its explain record, maps a
//! canonical `Query` to its native syntax (or answers a diagnostic for a
//! clause it does not cover), parses its responses into
//! `bibliographic-work@1.0.0` rows, and claims the identifiers and URLs it
//! can resolve (`detect`, Zotero's `detectWeb`).

use async_trait::async_trait;
use koine_sources::{Diagnostic, ExplainRecord, Query, Work};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A relation `related` walks (S-D3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    /// The works this one cites (its references).
    Cites,
    /// The works that cite this one.
    CitedBy,
    /// Works the source judges similar.
    Similar,
    /// Works read together with this one (ADS's co-reads).
    Coreads,
}

impl Relation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Relation::Cites => "cites",
            Relation::CitedBy => "cited_by",
            Relation::Similar => "similar",
            Relation::Coreads => "coreads",
        }
    }
}

/// A format `export` emits (S-D3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ExportFormat {
    Bibtex,
    Ris,
    CslJson,
}

impl ExportFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExportFormat::Bibtex => "bibtex",
            ExportFormat::Ris => "ris",
            ExportFormat::CslJson => "csl-json",
        }
    }
}

/// What one request asks of a source's page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageRequest {
    /// Rows wanted; the adapter caps it at its explain record's `page.max`.
    pub limit: u32,
    /// The source's own opaque cursor from a previous page's `next`.
    pub cursor: Option<String>,
    /// One of the explain record's `sortable` fields, when the caller asked.
    pub sort: Option<String>,
}

/// One page of rows from one source: the L-D5 envelope, per source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Page {
    pub rows: Vec<Work>,
    /// The source's cursor for the next page; `None` at the end.
    pub next: Option<String>,
    /// What was dropped or went wrong, without hiding the rows.
    pub diagnostics: Vec<Diagnostic>,
}

/// One online database. `Err(Diagnostic)` is "this source could not answer
/// at all" (rate-limited, refused, down); a partial answer is an `Ok` page
/// carrying diagnostics.
#[async_trait]
pub trait Adapter: Send + Sync + 'static {
    /// The declaration (S-D1): the facts, all of them.
    fn explain(&self) -> ExplainRecord;

    /// Whether this adapter claims the canonical id `kind:value`
    /// (`doi:10.1126/…`, `arxiv:2101.00001`): the id grammar it can look up.
    fn detect(&self, id: &str) -> bool;

    /// Search: every clause the explain record covers is mapped; the others
    /// are `unsupported` diagnostics on the page.
    async fn search(&self, query: &Query, page: PageRequest) -> Result<Page, Diagnostic>;

    /// One record by a canonical id this adapter `detect`s; `Ok(None)` when
    /// the source has no such record.
    async fn fetch(&self, id: &str) -> Result<Option<Work>, Diagnostic>;

    /// Works related to `id` by `relation`; a relation the source cannot walk
    /// is an `Err(unsupported)`.
    async fn related(
        &self,
        id: &str,
        relation: Relation,
        page: PageRequest,
    ) -> Result<Page, Diagnostic>;

    /// The records `ids` name, as `format` text.
    async fn export(&self, ids: &[String], format: ExportFormat) -> Result<String, Diagnostic>;
}
