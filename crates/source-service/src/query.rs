//! Helpers over `koine_sources::Query` and `ExplainRecord` the service and
//! the adapters share: the leaf clauses of a query, whether a record
//! declares an operation, and the id-only merge of two works (S-D7).

use koine_sources::kind::{Location, Work};
use koine_sources::{Diagnostic, ExplainRecord, Query, Status};

use crate::federation;

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

/// One identifier as it is compared for a merge: a DOI lowercased, an
/// arXiv id lowercased and without its version (`2101.00001v2` is
/// `2101.00001`), a PMID or bibcode trimmed.
pub fn normalise_id(kind: &str, value: &str) -> String {
    let value = value.trim();
    match kind {
        "doi" => value.to_ascii_lowercase(),
        "arxiv" => {
            let lower = value.to_ascii_lowercase();
            let lower = lower.strip_prefix("arxiv:").unwrap_or(&lower);
            match lower.rfind('v') {
                Some(i)
                    if i > 0
                        && lower[i + 1..].chars().all(|c| c.is_ascii_digit())
                        && !lower[i + 1..].is_empty() =>
                {
                    lower[..i].to_string()
                }
                _ => lower.to_string(),
            }
        }
        _ => value.to_string(),
    }
}

/// Whether two works share one of the [`MERGE_IDS`] (normalised: a DOI is
/// case-insensitive, an arXiv id versionless).
pub fn shares_id(a: &Work, b: &Work) -> bool {
    MERGE_IDS
        .iter()
        .any(|k| match (a.ids.get(*k), b.ids.get(*k)) {
            (Some(x), Some(y)) => normalise_id(k, x) == normalise_id(k, y),
            _ => false,
        })
}

/// Whether two locations are one: the same `url` when both have one, else
/// the same `source` and `source_id`.
pub fn same_location(a: &Location, b: &Location) -> bool {
    match (&a.url, &b.url) {
        (Some(x), Some(y)) => x == y,
        _ => a.source == b.source && a.source_id == b.source_id,
    }
}

/// Fold `other` into `into`: ids and locations unioned (a second primary
/// location is demoted, a location is deduplicated by url), empty fields
/// filled, the first row's body kept, every source's count kept in
/// `metrics.cited_by_all`, and `provenance.sources` naming every
/// contributor. The body a merge keeps is the higher-priority source's;
/// [`federation::merge_works`] orders the fold.
pub fn merge(into: &mut Work, other: Work) {
    for (k, v) in other.ids {
        into.ids.entry(k).or_insert(v);
    }
    let has_primary = into.locations.iter().any(|l| l.primary);
    for mut loc in other.locations {
        if into
            .locations
            .iter()
            .any(|l: &Location| same_location(l, &loc))
        {
            continue;
        }
        if has_primary {
            loc.primary = false;
        }
        into.locations.push(loc);
    }
    if let Some(theirs) = other.provenance {
        match into.provenance.as_mut() {
            Some(mine) => {
                if mine.sources.is_empty() {
                    mine.sources.push(mine.source.clone());
                }
                if !mine.sources.contains(&theirs.source) {
                    mine.sources.push(theirs.source);
                }
            }
            None => into.provenance = Some(theirs),
        }
    }
    if let Some(theirs) = other.metrics {
        match into.metrics.as_mut() {
            Some(mine) => {
                if mine.cited_by_all.is_empty() {
                    mine.cited_by_all.extend(mine.cited_by.clone());
                }
                for count in theirs.cited_by.into_iter().chain(theirs.cited_by_all) {
                    if !mine.cited_by_all.iter().any(|c| c.source == count.source) {
                        mine.cited_by_all.push(count);
                    }
                }
                if mine.cited_by.is_none() {
                    mine.cited_by = mine.cited_by_all.first().cloned();
                }
            }
            None => into.metrics = Some(theirs),
        }
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
    if into.volume.is_none() {
        into.volume = other.volume;
    }
    if into.issue.is_none() {
        into.issue = other.issue;
    }
    if into.page.is_none() {
        into.page = other.page;
    }
    if into.language.is_none() {
        into.language = other.language;
    }
    if into.url.is_none() {
        into.url = other.url;
    }
    if into.ty.is_empty() {
        into.ty = other.ty;
    }
}

/// Add `row` to `rows`, folding it into an existing row that shares an id;
/// the higher-priority source's body wins ([`federation::merge_works`]).
pub fn merge_into(rows: &mut Vec<Work>, row: Work) {
    if let Some(existing) = rows.iter_mut().find(|r| shares_id(r, &row)) {
        let kept = std::mem::take(existing);
        *existing = federation::merge_works(vec![kept, row]);
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
        assert_eq!(normalise_id("arxiv", "arXiv:2101.00001v3"), "2101.00001");
        assert_eq!(normalise_id("arxiv", "hep-th/9901001v2"), "hep-th/9901001");
        assert_eq!(
            normalise_id("doi", "10.1126/Science.1063991 "),
            "10.1126/science.1063991"
        );
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
