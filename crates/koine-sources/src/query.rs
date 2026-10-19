//! The `bib.*` context set and the query grammar over it (S-D2).
//!
//! A query is CQL's shape carried as JSON, so neither side needs a parser:
//!
//! ```json
//! {"and": [
//!   {"index": "author", "relation": "any", "term": "Abel"},
//!   {"index": "year", "relation": "within", "term": "2020 2024"}
//! ]}
//! ```

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::diagnostics::Diagnostic;
use crate::explain::ExplainRecord;

/// The indexes of the `bib` context set.
pub const INDEXES: &[&str] = &[
    "any",
    "title",
    "abstract",
    "author",
    "first_author",
    "year",
    "container",
    "doi",
    "arxiv",
    "pmid",
    "bibcode",
    "isbn",
    "issn",
    "orcid",
    "keyword",
    "subject",
    "cites",
    "cited_by",
    "similar_to",
];

/// The relations a clause may use.
pub const RELATIONS: &[&str] = &["any", "all", "adj", "=", "<", ">", "within", "exact"];

/// A query over the `bib` context set. Serialised untagged:
/// `{"and": [..]}`, `{"or": [..]}`, `{"not": q}`, `{"native": "…"}` or a
/// clause `{"index", "relation", "term"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Query {
    And {
        and: Vec<Query>,
    },
    Or {
        or: Vec<Query>,
    },
    Not {
        not: Box<Query>,
    },
    /// A source's own query syntax, passed through (declared in the explain
    /// record's `native`).
    Native {
        native: String,
    },
    Clause {
        /// One of [`INDEXES`] (without the `bib.` prefix).
        index: String,
        /// One of [`RELATIONS`].
        relation: String,
        term: String,
    },
}

impl Query {
    /// A clause.
    pub fn clause(
        index: impl Into<String>,
        relation: impl Into<String>,
        term: impl Into<String>,
    ) -> Self {
        Query::Clause {
            index: index.into(),
            relation: relation.into(),
            term: term.into(),
        }
    }

    /// Every clause the record does not declare, one diagnostic each: an
    /// index it lacks (`bib.first_author`), a relation it does not support on
    /// an index it has (`bib.year <`), or a `native` passthrough it does not
    /// accept. Empty means the record covers the whole query. A clause that
    /// is not in the `bib` set at all is also unsupported.
    pub fn covered_by(&self, record: &ExplainRecord) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        self.walk(record, &mut out);
        out
    }

    fn walk(&self, record: &ExplainRecord, out: &mut Vec<Diagnostic>) {
        match self {
            Query::And { and: qs } | Query::Or { or: qs } => {
                qs.iter().for_each(|q| q.walk(record, out))
            }
            Query::Not { not } => not.walk(record, out),
            Query::Native { .. } => {
                if record.native.is_none() {
                    out.push(Diagnostic::unsupported(&record.source, "native"));
                }
            }
            Query::Clause {
                index, relation, ..
            } => match record.indexes.get(index) {
                None => out.push(Diagnostic::unsupported(
                    &record.source,
                    format!("bib.{index}"),
                )),
                Some(def) if !def.relations.iter().any(|r| r == relation) => out.push(
                    Diagnostic::unsupported(&record.source, format!("bib.{index} {relation}")),
                ),
                Some(_) => {}
            },
        }
    }

    /// The first thing wrong with the query itself, independent of any
    /// source: an index or relation outside the `bib` set.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Query::And { and: qs } | Query::Or { or: qs } => {
                qs.iter().try_for_each(Query::validate)
            }
            Query::Not { not } => not.validate(),
            Query::Native { .. } => Ok(()),
            Query::Clause {
                index, relation, ..
            } => {
                if !INDEXES.contains(&index.as_str()) {
                    Err(format!("`{index}` is not a bib index"))
                } else if !RELATIONS.contains(&relation.as_str()) {
                    Err(format!("`{relation}` is not a bib relation"))
                } else {
                    Ok(())
                }
            }
        }
    }
}
