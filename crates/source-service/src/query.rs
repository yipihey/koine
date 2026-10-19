//! Helpers over `koine_sources::Query` and `ExplainRecord` the service and
//! the adapters share: the leaf clauses of a query, whether a record
//! declares an operation, and the id-only merge of two works (S-D7).

use koine_sources::kind::{Location, Work};
use koine_sources::{Diagnostic, ExplainRecord, Query, Status};

/// One leaf clause, borrowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clause<'a> {
    pub index: &'a str,
    pub relation: &'a str,
    pub term: &'a str,
}

/// Every leaf clause of `query`, in document order (a `not` clause included).
pub fn clauses(query: &Query) -> Vec<Clause<'_>> {
    let mut out = Vec::new();
    collect(query, &mut out);
    out
}

fn collect<'a>(query: &'a Query, out: &mut Vec<Clause<'a>>) {
    match query {
        Query::And { and: qs } | Query::Or { or: qs } => qs.iter().for_each(|q| collect(q, out)),
        Query::Not { not } => collect(not, out),
        Query::Native { .. } => {}
        Query::Clause {
            index,
            relation,
            term,
        } => out.push(Clause {
            index,
            relation,
            term,
        }),
    }
}

/// The native passthrough text, when `query` is one.
pub fn native_text(query: &Query) -> Option<&str> {
    match query {
        Query::Native { native } => Some(native),
        _ => None,
    }
}

/// Whether a query has no leaf clause and no native text: nothing to send.
pub fn is_empty(query: &Query) -> bool {
    native_text(query).is_none() && clauses(query).is_empty()
}

/// Whether `record` declares `operation` (S-D1 `operations`).
pub fn supports(record: &ExplainRecord, operation: &str) -> bool {
    record.operations.iter().any(|o| o == operation)
}

/// Whether every diagnostic is `done`: a page with a dropped clause is not.
pub fn all_done(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().all(|d| d.status == Status::Done)
}

/// The id kinds that merge two rows into one work (S-D7: by identifier only).
pub const MERGE_IDS: &[&str] = &["doi", "arxiv", "pmid", "bibcode"];

/// Whether two works share one of the [`MERGE_IDS`] (case-insensitive).
pub fn shares_id(a: &Work, b: &Work) -> bool {
    MERGE_IDS
        .iter()
        .any(|k| match (a.ids.get(*k), b.ids.get(*k)) {
            (Some(x), Some(y)) => x.eq_ignore_ascii_case(y),
            _ => false,
        })
}

/// Fold `other` into `into`: ids and locations unioned (a second primary
/// location is demoted), empty fields filled, the first row's body kept.
pub fn merge(into: &mut Work, other: Work) {
    for (k, v) in other.ids {
        into.ids.entry(k).or_insert(v);
    }
    let has_primary = into.locations.iter().any(|l| l.primary);
    for mut loc in other.locations {
        if into
            .locations
            .iter()
            .any(|l: &Location| l.source == loc.source && l.source_id == loc.source_id)
        {
            continue;
        }
        if has_primary {
            loc.primary = false;
        }
        into.locations.push(loc);
    }
    if into.title.is_none() {
        into.title = other.title;
    }
    if into.author.is_empty() {
        into.author = other.author;
    }
    if into.issued.is_none() {
        into.issued = other.issued;
    }
    if into.container_title.is_none() {
        into.container_title = other.container_title;
    }
    if into.abstract_.is_none() {
        into.abstract_ = other.abstract_;
    }
    if into.doi.is_none() {
        into.doi = other.doi;
    }
    if into.metrics.is_none() {
        into.metrics = other.metrics;
    }
}

/// Add `row` to `rows`, folding it into an existing row that shares an id.
pub fn merge_into(rows: &mut Vec<Work>, row: Work) {
    if let Some(existing) = rows.iter_mut().find(|r| shares_id(r, &row)) {
        merge(existing, row);
    } else {
        rows.push(row);
    }
}

/// The source and id a row came from: its provenance, else its primary
/// location, else its first location.
pub fn source_of(work: &Work) -> Option<(&str, &str)> {
    if let Some(p) = &work.provenance {
        return Some((&p.source, &p.source_id));
    }
    work.locations
        .iter()
        .find(|l| l.primary)
        .or_else(|| work.locations.first())
        .map(|l| (l.source.as_str(), l.source_id.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use koine_sources::kind::Version;
    use std::collections::BTreeMap;

    fn work(source: &str, ids: &[(&str, &str)], version: Version) -> Work {
        Work {
            ty: "article-journal".into(),
            ids: ids
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            locations: vec![Location {
                source: source.into(),
                source_id: ids[0].1.into(),
                version,
                url: None,
                pdf: None,
                license: None,
                oa_status: None,
                primary: true,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn the_plan_example_lists_its_clauses_in_order() {
        let q: Query = serde_json::from_str(
            r#"{"and": [
              {"index": "author", "relation": "any", "term": "Abel"},
              {"index": "year",   "relation": "within", "term": "2020 2024"},
              {"or": [{"index": "title", "relation": "any", "term": "reionization"},
                      {"index": "any",   "relation": "any", "term": "21cm"}]}
            ]}"#,
        )
        .unwrap();
        let idx: Vec<&str> = clauses(&q).iter().map(|c| c.index).collect();
        assert_eq!(idx, ["author", "year", "title", "any"]);
        assert!(!is_empty(&q));
        assert!(is_empty(&Query::And { and: vec![] }));
    }

    #[test]
    fn works_merge_by_identifier_only_and_keep_one_primary() {
        let mut a = work("crossref", &[("doi", "10.1/X")], Version::Published);
        let b = work(
            "arxiv",
            &[("doi", "10.1/x"), ("arxiv", "1")],
            Version::Submitted,
        );
        let c = Work {
            ty: "article".into(),
            title: Some("T".into()),
            ..Default::default()
        };
        assert!(shares_id(&a, &b));
        assert!(!shares_id(&a, &c), "a title alone never merges");
        let mut rows = vec![a.clone()];
        merge_into(&mut rows, c.clone());
        assert_eq!(rows.len(), 2);
        merge(&mut a, b);
        assert_eq!(a.locations.len(), 2);
        assert_eq!(a.locations.iter().filter(|l| l.primary).count(), 1);
        assert_eq!(a.ids["arxiv"], "1");
        a.validate().unwrap();
        let _ = BTreeMap::<String, String>::new();
    }
}
