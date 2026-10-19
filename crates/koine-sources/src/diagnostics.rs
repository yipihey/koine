//! What a source did with a request, named (S-D7).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The outcome of one source, or of one clause a source could not answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The source answered.
    Done,
    /// The source needs a secret the caller has not supplied.
    Refused,
    /// The source did not answer in time.
    TimedOut,
    /// The query names an index or relation the source's explain record lacks
    /// (S-D2): the clause was dropped, visibly.
    Unsupported,
    /// The source's etiquette limit was reached (S-D8).
    RateLimited,
}

/// One line of a result's `diagnostics`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Diagnostic {
    /// The source's id (`crossref`).
    pub source: String,
    pub status: Status,
    /// What the status is about: for `unsupported`, the clause
    /// (`bib.first_author`); otherwise a short human reason.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

impl Diagnostic {
    pub fn new(source: impl Into<String>, status: Status, detail: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            status,
            detail: detail.into(),
        }
    }

    /// A source answered.
    pub fn done(source: impl Into<String>) -> Self {
        Self::new(source, Status::Done, "")
    }

    /// A clause the source cannot answer.
    pub fn unsupported(source: impl Into<String>, what: impl Into<String>) -> Self {
        Self::new(source, Status::Unsupported, what)
    }
}
