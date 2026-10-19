//! The source convention's wire types, minimal.
//!
//! **To be replaced by `koine-sources` (S1).** That crate owns the
//! `bibliographic-work@1` kind, [`ExplainRecord`], [`Query`] and
//! [`Diagnostic`]; until it lands, these are the same names with the field
//! names copied exactly from docs/plan-source-convention.md (S-D1's example,
//! S-D2's example, S-D4's list), so the swap is an import change in
//! `lib.rs` and nothing else.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The kind every row of this convention is (S-D4).
pub const WORK_KIND: &str = "bibliographic-work@1";

/// The `bib.*` context set (S-D2): every index a canonical query may name.
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

/// Strip the context-set prefix: `bib.author` and `author` name one index.
pub fn bare_index(index: &str) -> &str {
    index.strip_prefix("bib.").unwrap_or(index)
}

// ---------------------------------------------------------------- S-D1

/// What one index of a source supports (S-D1 `indexes.<name>`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IndexInfo {
    /// The relations this index answers (`any`, `adj`, `=`, `<`, `>`, `within`).
    pub relations: Vec<String>,
    /// True for an identifier lookup: one term, one record.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exact: bool,
}

/// How the source pages (S-D1 `page`): `offset` (with a cap), `cursor` or
/// `token`; the client treats all three as opaque.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PageInfo {
    /// `offset`, `cursor` or `token`.
    pub model: String,
    /// The most rows one request may ask for.
    pub max: u32,
}

/// How to be a good caller of the source (S-D1 `etiquette`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EtiquetteInfo {
    /// How the contact travels: `mailto` (query and User-Agent), `header`, or `none`.
    pub contact: String,
    /// Where the limit is read from: `headers`, `declared` or `none`.
    pub limit: String,
    /// Whether a key is `required`, `optional` or `none`.
    pub key: String,
}

/// What a source can do: the facts, all of them (S-D1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExplainRecord {
    /// The source's id (`crossref`): what `sources` and every row's `source` name.
    pub source: String,
    /// The source's display name.
    pub name: String,
    /// The source API version the adapter was written against.
    pub version: String,
    /// The canonical kind the source answers (`bibliographic-work@1`).
    pub kind: String,
    /// Which of the fixed verbs (S-D3) this source answers.
    pub operations: Vec<String>,
    /// The context-set indexes it can search and the relations each supports.
    pub indexes: BTreeMap<String, IndexInfo>,
    /// Fields a result list can be ordered by.
    pub sortable: Vec<String>,
    /// Which canonical id kinds (S-D4 `ids`) it can look up directly.
    pub identifiers: Vec<String>,
    /// Which canonical fields this source populates (absent vs unknown).
    pub fills: Vec<String>,
    /// The paging model and its cap.
    pub page: PageInfo,
    /// The etiquette the service enforces for it (S-D8).
    pub etiquette: EtiquetteInfo,
    /// The F-D1 secret names a call may read; never an argument.
    #[serde(default)]
    pub secrets: Vec<String>,
    /// The native query dialect accepted as a passthrough, when any (S-D2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<String>,
}

impl ExplainRecord {
    /// Whether the record declares `operation`.
    pub fn supports(&self, operation: &str) -> bool {
        self.operations.iter().any(|o| o == operation)
    }

    /// Whether the record declares `index` (with or without `bib.`), and, when
    /// `relation` is given, that relation on it.
    pub fn covers(&self, index: &str, relation: Option<&str>) -> bool {
        match self.indexes.get(bare_index(index)) {
            None => false,
            Some(info) => relation.is_none_or(|r| info.relations.iter().any(|x| x == r)),
        }
    }
}

// ---------------------------------------------------------------- S-D2

/// A canonical query: CQL's shape as JSON (S-D2). One node is exactly one of:
/// a clause (`index`, `relation`, `term`), a conjunction (`and`), a
/// disjunction (`or`), or a native passthrough (`native`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Query {
    /// Every sub-query must hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub and: Option<Vec<Query>>,
    /// Any sub-query may hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub or: Option<Vec<Query>>,
    /// The `bib.*` index of a clause (`author`, `title`, `year`, `doi`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,
    /// The relation of a clause: `any`, `adj`, `=`, `<`, `>`, `within`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation: Option<String>,
    /// The term of a clause; for `within`, two values separated by a space.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub term: Option<String>,
    /// A source-native query passed through unchanged (declared by `explain.native`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<String>,
}

/// One leaf clause of a [`Query`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clause<'a> {
    pub index: &'a str,
    pub relation: &'a str,
    pub term: &'a str,
}

impl Query {
    /// A single clause.
    pub fn clause(index: &str, relation: &str, term: &str) -> Self {
        Query {
            index: Some(index.into()),
            relation: Some(relation.into()),
            term: Some(term.into()),
            ..Default::default()
        }
    }

    /// A conjunction.
    pub fn all(parts: Vec<Query>) -> Self {
        Query {
            and: Some(parts),
            ..Default::default()
        }
    }

    /// What is wrong with the shape, if anything: a node must be exactly one
    /// of clause / and / or / native, and a clause needs all three parts.
    pub fn validate(&self) -> Result<(), String> {
        let is_clause = self.index.is_some() || self.relation.is_some() || self.term.is_some();
        let forms = [
            self.and.is_some(),
            self.or.is_some(),
            is_clause,
            self.native.is_some(),
        ]
        .iter()
        .filter(|f| **f)
        .count();
        if forms == 0 {
            return Err("a query node is a clause {index, relation, term}, {and: [...]}, {or: [...]} or {native: \"...\"}; this one is empty".into());
        }
        if forms > 1 {
            return Err("a query node is exactly one of a clause, `and`, `or` or `native`".into());
        }
        if is_clause {
            let (Some(index), Some(relation), Some(term)) =
                (&self.index, &self.relation, &self.term)
            else {
                return Err("a clause needs `index`, `relation` and `term`".into());
            };
            if !INDEXES.contains(&bare_index(index)) {
                return Err(format!(
                    "`{index}` is not a bib.* index; one of {}",
                    INDEXES.join(", ")
                ));
            }
            if relation.is_empty() {
                return Err("a clause's `relation` is empty".into());
            }
            if term.trim().is_empty() {
                return Err(format!("the `{index}` clause has an empty term"));
            }
        }
        for part in self.and.iter().chain(self.or.iter()).flatten() {
            part.validate()?;
        }
        Ok(())
    }

    /// Every leaf clause, in document order.
    pub fn clauses(&self) -> Vec<Clause<'_>> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect<'a>(&'a self, out: &mut Vec<Clause<'a>>) {
        if let (Some(index), Some(relation), Some(term)) = (&self.index, &self.relation, &self.term)
        {
            out.push(Clause {
                index: bare_index(index),
                relation,
                term,
            });
        }
        for part in self.and.iter().chain(self.or.iter()).flatten() {
            part.collect(out);
        }
    }

    /// The native passthrough, if this query is one.
    pub fn native_text(&self) -> Option<&str> {
        self.native.as_deref()
    }
}

// ---------------------------------------------------------------- S-D7

/// How one source's part of a call went (S-D2, S-D7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DiagnosticStatus {
    /// The source answered.
    Done,
    /// The source needs a secret or contact the caller lacks.
    Refused,
    /// The source did not answer within the timeout.
    TimedOut,
    /// A clause named an index or a relation the source's explain record does
    /// not cover; the clause was dropped, visibly.
    Unsupported,
    /// The source asked us to back off; nothing was retried.
    RateLimited,
    /// The id named nothing at this source.
    NotFound,
    /// The transport or the parser failed; `message` says how.
    Error,
}

/// One diagnostic in a result's `diagnostics` (S-D2's `{"unsupported": …}`,
/// S-D7's `{"source": …, "status": …}`), carried as one shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Diagnostic {
    /// The source this is about.
    pub source: String,
    pub status: DiagnosticStatus,
    /// The index (`bib.first_author`) an `unsupported` diagnostic names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsupported: Option<String>,
    /// A sentence, when there is more to say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Seconds to wait before asking this source again (`rate-limited`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_s: Option<u64>,
}

impl Diagnostic {
    pub fn new(source: &str, status: DiagnosticStatus) -> Self {
        Diagnostic {
            source: source.into(),
            status,
            unsupported: None,
            message: None,
            retry_after_s: None,
        }
    }

    pub fn done(source: &str) -> Self {
        Self::new(source, DiagnosticStatus::Done)
    }

    pub fn unsupported(source: &str, index: &str) -> Self {
        let mut d = Self::new(source, DiagnosticStatus::Unsupported);
        d.unsupported = Some(format!("bib.{}", bare_index(index)));
        d
    }

    pub fn rate_limited(source: &str, retry_after_s: Option<u64>, message: impl Into<String>) -> Self {
        let mut d = Self::new(source, DiagnosticStatus::RateLimited);
        d.retry_after_s = retry_after_s;
        d.message = Some(message.into());
        d
    }

    pub fn not_found(source: &str, id: &str) -> Self {
        Self::new(source, DiagnosticStatus::NotFound).with_message(format!("{id} is not at {source}"))
    }

    pub fn error(source: &str, message: impl Into<String>) -> Self {
        Self::new(source, DiagnosticStatus::Error).with_message(message)
    }

    pub fn refused(source: &str, message: impl Into<String>) -> Self {
        Self::new(source, DiagnosticStatus::Refused).with_message(message)
    }

    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }
}

// ---------------------------------------------------------------- S-D4

/// One author (CSL `author[]` with an ORCID).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Author {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub given: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orcid: Option<String>,
}

/// A CSL date: `date-parts`, each `[year, month?, day?]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DateParts {
    #[serde(rename = "date-parts", default)]
    pub date_parts: Vec<Vec<i32>>,
}

impl DateParts {
    pub fn year(&self) -> Option<i32> {
        self.date_parts.first().and_then(|p| p.first().copied())
    }
}

/// Where one source has the work (S-D4 `locations[]`): the DRIVER `version`
/// is `submitted`, `accepted` or `published`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Location {
    pub source: String,
    pub source_id: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pdf: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oa_status: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub primary: bool,
}

/// Where and when a record was fetched (S-D4 `provenance`): the field
/// imbib's import drops today.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Provenance {
    /// RFC 3339.
    pub fetched_at: String,
    pub source: String,
    pub source_id: String,
    /// The query that produced it, when a search did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<Query>,
}

/// A count with its source, since counts differ by source (S-D4 `metrics`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Metrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cited_by: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// A `bibliographic-work@1` record (S-D4): the CSL-JSON body, beside it the
/// `ids` map, `locations[]`, `provenance` and `metrics`. (The open question
/// of whether these live under CSL's `custom` on the wire is answered here
/// as "beside", the readable choice; the kind is the same either way.)
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Work {
    /// The row's id: the best canonical identifier, `doi:…`, `arxiv:…`, else
    /// `<source>:<source_id>`.
    pub id: String,
    /// The source that answered this row and its id there (S-D3).
    pub source: String,
    pub source_id: String,
    /// CSL `type` (`article-journal`, `book`, …).
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub author: Vec<Author>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued: Option<DateParts>,
    #[serde(rename = "container-title", default, skip_serializing_if = "Option::is_none")]
    pub container_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#abstract: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Every identifier any source supplied: `doi`, `arxiv`, `pmid`, `pmcid`,
    /// `bibcode`, `isbn`, `openalex`, `s2`, `mag`, `wos`, `dblp`.
    #[serde(default)]
    pub ids: BTreeMap<String, String>,
    #[serde(default)]
    pub locations: Vec<Location>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Metrics>,
}

/// The id kinds that merge two rows into one work (S-D7: by identifier only).
pub const MERGE_IDS: &[&str] = &["doi", "arxiv", "pmid", "bibcode"];

impl Work {
    /// The publication year.
    pub fn year(&self) -> Option<i32> {
        self.issued.as_ref().and_then(DateParts::year)
    }

    /// Whether two works share one of the [`MERGE_IDS`] (case-insensitive).
    pub fn shares_id(&self, other: &Work) -> bool {
        MERGE_IDS.iter().any(|k| match (self.ids.get(*k), other.ids.get(*k)) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            _ => false,
        })
    }

    /// Fold `other` into `self`: ids and locations unioned, empty fields
    /// filled, the first row's body kept.
    pub fn merge(&mut self, other: Work) {
        for (k, v) in other.ids {
            self.ids.entry(k).or_insert(v);
        }
        for loc in other.locations {
            if !self
                .locations
                .iter()
                .any(|l| l.source == loc.source && l.source_id == loc.source_id)
            {
                self.locations.push(loc);
            }
        }
        if self.title.is_none() {
            self.title = other.title;
        }
        if self.author.is_empty() {
            self.author = other.author;
        }
        if self.issued.is_none() {
            self.issued = other.issued;
        }
        if self.container_title.is_none() {
            self.container_title = other.container_title;
        }
        if self.r#abstract.is_none() {
            self.r#abstract = other.r#abstract;
        }
        if self.metrics.is_none() {
            self.metrics = other.metrics;
        }
    }

    /// The CSL-JSON body alone (what `export` with `csl-json` emits), with the
    /// `ids` map under CSL's `custom`.
    pub fn csl_json(&self) -> serde_json::Value {
        let mut v = serde_json::json!({ "id": self.id });
        let obj = v.as_object_mut().expect("object");
        if let Some(t) = &self.r#type {
            obj.insert("type".into(), t.clone().into());
        }
        if let Some(t) = &self.title {
            obj.insert("title".into(), t.clone().into());
        }
        if !self.author.is_empty() {
            obj.insert("author".into(), serde_json::to_value(&self.author).unwrap_or_default());
        }
        if let Some(i) = &self.issued {
            obj.insert("issued".into(), serde_json::to_value(i).unwrap_or_default());
        }
        for (key, value) in [
            ("container-title", &self.container_title),
            ("volume", &self.volume),
            ("issue", &self.issue),
            ("page", &self.page),
            ("abstract", &self.r#abstract),
            ("language", &self.language),
        ] {
            if let Some(value) = value {
                obj.insert(key.into(), value.clone().into());
            }
        }
        if let Some(doi) = self.ids.get("doi") {
            obj.insert("DOI".into(), doi.clone().into());
        }
        obj.insert("custom".into(), serde_json::json!({ "ids": self.ids }));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_example_query_parses_and_lists_its_clauses() {
        let q: Query = serde_json::from_str(
            r#"{"and": [
              {"index": "author", "relation": "any", "term": "Abel"},
              {"index": "year",   "relation": "within", "term": "2020 2024"},
              {"or": [{"index": "title", "relation": "any", "term": "reionization"},
                      {"index": "any",   "relation": "any", "term": "21cm"}]}
            ]}"#,
        )
        .unwrap();
        q.validate().unwrap();
        let idx: Vec<&str> = q.clauses().iter().map(|c| c.index).collect();
        assert_eq!(idx, ["author", "year", "title", "any"]);
    }

    #[test]
    fn a_clause_with_a_bib_prefix_or_an_unknown_index_is_judged() {
        assert!(Query::clause("bib.author", "any", "x").validate().is_ok());
        assert!(Query::clause("colour", "any", "x").validate().is_err());
        assert!(Query::default().validate().is_err());
        let mut both = Query::clause("title", "any", "x");
        both.native = Some("x".into());
        assert!(both.validate().is_err());
    }

    #[test]
    fn works_merge_by_identifier_only() {
        let mut a = Work {
            id: "doi:10.1/x".into(),
            ids: BTreeMap::from([("doi".into(), "10.1/X".into())]),
            locations: vec![Location {
                source: "crossref".into(),
                source_id: "10.1/x".into(),
                version: "published".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let b = Work {
            id: "arxiv:1".into(),
            title: Some("T".into()),
            ids: BTreeMap::from([
                ("doi".into(), "10.1/x".into()),
                ("arxiv".into(), "1".into()),
            ]),
            locations: vec![Location {
                source: "arxiv".into(),
                source_id: "1".into(),
                version: "submitted".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let c = Work {
            id: "x".into(),
            title: Some("T".into()),
            ..Default::default()
        };
        assert!(a.shares_id(&b));
        assert!(!a.shares_id(&c), "title alone never merges");
        a.merge(b);
        assert_eq!(a.locations.len(), 2);
        assert_eq!(a.ids["arxiv"], "1");
        assert_eq!(a.title.as_deref(), Some("T"));
    }

    #[test]
    fn explain_record_round_trips_the_plan_example() {
        let text = r#"{
          "source": "crossref", "name": "Crossref", "version": "rest-v1",
          "kind": "bibliographic-work@1",
          "operations": ["search", "fetch", "match"],
          "indexes": {
            "any":      {"relations": ["any"]},
            "title":    {"relations": ["any", "adj"]},
            "author":   {"relations": ["any"]},
            "doi":      {"relations": ["="], "exact": true},
            "year":     {"relations": ["=", "<", ">", "within"]}
          },
          "sortable": ["relevance", "year", "cited_by"],
          "identifiers": ["doi"],
          "fills": ["title", "authors", "year", "container", "doi", "issued", "cited_by", "license"],
          "page": {"model": "cursor", "max": 1000},
          "etiquette": {"contact": "mailto", "limit": "headers", "key": "optional"},
          "secrets": ["crossref-plus-token"]
        }"#;
        let rec: ExplainRecord = serde_json::from_str(text).unwrap();
        assert!(rec.covers("bib.year", Some("within")));
        assert!(!rec.covers("first_author", None));
        assert!(rec.indexes["doi"].exact);
        let back: serde_json::Value = serde_json::to_value(&rec).unwrap();
        let orig: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(back, orig);
    }
}
