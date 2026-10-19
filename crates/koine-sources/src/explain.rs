//! The explain record: what a source says it can do (S-D1).
//!
//! Facts only, and all of them. A frontend builds its search form from
//! `indexes`, its sort menu from `sortable` and its "look up by" field from
//! `identifiers`; nothing is guessed. ZeeRex is the precedent.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::kind::{BIBLIOGRAPHIC_WORK, BIBLIOGRAPHIC_WORK_MAJOR, IDENTIFIER_KINDS};
use crate::query::{INDEXES, RELATIONS};

/// The fixed verb set a source may answer (S-D3).
pub const OPERATIONS: &[&str] = &["search", "fetch", "match", "related", "export"];

/// What one index supports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IndexDef {
    /// Relations from [`RELATIONS`].
    pub relations: Vec<String>,
    /// An identifier lookup, not a text search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact: Option<bool>,
}

/// How a result list is paged; the client treats all as opaque.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PageModel {
    Offset,
    Cursor,
    Token,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Page {
    pub model: PageModel,
    /// The most rows one page can hold, or the cap on how deep it pages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u32>,
}

/// How to be a good caller (S-D8). Values are the adapter's own words
/// (`mailto`, `headers`, `optional`); the service interprets them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Etiquette {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// One source's declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExplainRecord {
    /// Stable id (`crossref`).
    pub source: String,
    /// Display name.
    pub name: String,
    /// The source API version the adapter was written against.
    pub version: String,
    /// The canonical kind rows come back as (`bibliographic-work@1`).
    pub kind: String,
    /// Which of [`OPERATIONS`] it answers.
    pub operations: Vec<String>,
    /// The `bib.*` indexes it can search and the relations each supports.
    pub indexes: BTreeMap<String, IndexDef>,
    /// Fields a result list can be ordered by.
    #[serde(default)]
    pub sortable: Vec<String>,
    /// Which of [`IDENTIFIER_KINDS`] it can look up directly.
    #[serde(default)]
    pub identifiers: Vec<String>,
    /// Canonical fields it populates, so absent can be told from unknown.
    #[serde(default)]
    pub fills: Vec<String>,
    pub page: Page,
    #[serde(default)]
    pub etiquette: Etiquette,
    /// Secret names a call needs; never an argument.
    #[serde(default)]
    pub secrets: Vec<String>,
    /// The native query dialect it accepts as passthrough (`ads-lucene`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<String>,
}

/// Why an explain record is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExplainError {
    #[error("index `{0}` is not in the bib context set")]
    UnknownIndex(String),
    #[error("index `{index}`: relation `{relation}` is not a bib relation")]
    UnknownRelation { index: String, relation: String },
    #[error("index `{0}` declares no relations")]
    NoRelations(String),
    #[error("kind `{0}` is not bibliographic-work@1")]
    UnknownKind(String),
    #[error("operation `{0}` is not in the fixed verb set")]
    UnknownOperation(String),
    #[error("identifier `{0}` is not a canonical id kind")]
    UnknownIdentifier(String),
}

impl ExplainRecord {
    /// Refuse an index outside the `bib.*` set, a relation outside the fixed
    /// set, an operation outside the verb set or an identifier that is not a
    /// canonical id kind.
    pub fn validate(&self) -> Result<(), ExplainError> {
        if self.kind != BIBLIOGRAPHIC_WORK && self.kind != BIBLIOGRAPHIC_WORK_MAJOR {
            return Err(ExplainError::UnknownKind(self.kind.clone()));
        }
        for op in &self.operations {
            if !OPERATIONS.contains(&op.as_str()) {
                return Err(ExplainError::UnknownOperation(op.clone()));
            }
        }
        for (name, def) in &self.indexes {
            if !INDEXES.contains(&name.as_str()) {
                return Err(ExplainError::UnknownIndex(name.clone()));
            }
            if def.relations.is_empty() {
                return Err(ExplainError::NoRelations(name.clone()));
            }
            if let Some(bad) = def
                .relations
                .iter()
                .find(|r| !RELATIONS.contains(&r.as_str()))
            {
                return Err(ExplainError::UnknownRelation {
                    index: name.clone(),
                    relation: bad.clone(),
                });
            }
        }
        for id in &self.identifiers {
            if !IDENTIFIER_KINDS.contains(&id.as_str()) {
                return Err(ExplainError::UnknownIdentifier(id.clone()));
            }
        }
        Ok(())
    }

    /// Parse and validate a record from JSON.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let record: Self = serde_json::from_str(text).map_err(|e| e.to_string())?;
        record.validate().map_err(|e| e.to_string())?;
        Ok(record)
    }
}
