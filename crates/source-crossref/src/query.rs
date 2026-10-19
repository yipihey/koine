//! The canonical query (S-D2) mapped to Crossref's `/works` parameters:
//! `any` → `query`, `author` → `query.author`, `title` and `container` →
//! `query.bibliographic` (never the deprecated `query.title`), `year` →
//! `filter=from-pub-date:…,until-pub-date:…`, `doi =` → `GET /works/{doi}`.
//! A clause the explain record does not cover is an `unsupported`
//! diagnostic and the clause is dropped, visibly.

use source_service::types::{Diagnostic, Query};

use crate::SOURCE;

/// What a query becomes: parameters for `/works`, or one DOI lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mapped {
    /// `GET /works/{doi}`.
    Lookup(String),
    /// `GET /works?…`: the query parameters (without paging) in order.
    List {
        params: Vec<(String, String)>,
        diagnostics: Vec<Diagnostic>,
    },
}

fn push_or_join(params: &mut Vec<(String, String)>, key: &str, value: &str) {
    match params.iter_mut().find(|(k, _)| k == key) {
        Some((_, existing)) => {
            existing.push(' ');
            existing.push_str(value);
        }
        None => params.push((key.to_string(), value.to_string())),
    }
}

/// A `year` term: `2002` (`=`, `<`, `>`) or `2020 2024` (`within`).
fn year_filter(relation: &str, term: &str) -> Option<Vec<String>> {
    let years: Vec<i32> = term
        .split(|c: char| c.is_whitespace() || c == '-' || c == ',')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<i32>())
        .collect::<Result<_, _>>()
        .ok()?;
    match (relation, years.as_slice()) {
        ("=", [y]) => Some(vec![
            format!("from-pub-date:{y}-01-01"),
            format!("until-pub-date:{y}-12-31"),
        ]),
        ("<", [y]) => Some(vec![format!("until-pub-date:{}-12-31", y - 1)]),
        (">", [y]) => Some(vec![format!("from-pub-date:{}-01-01", y + 1)]),
        ("within", [a, b]) => Some(vec![
            format!("from-pub-date:{}-01-01", a.min(b)),
            format!("until-pub-date:{}-12-31", a.max(b)),
        ]),
        _ => None,
    }
}

/// Map `query`. A query that is one `doi =` clause is a lookup; everything
/// else is a list request. `None` when nothing at all could be mapped.
pub fn map(query: &Query) -> Mapped {
    let clauses = query.clauses();
    if let [one] = clauses.as_slice() {
        if one.index == "doi" && one.relation == "=" {
            return Mapped::Lookup(one.term.trim().to_string());
        }
    }
    let mut params: Vec<(String, String)> = Vec::new();
    let mut filters: Vec<String> = Vec::new();
    let mut diagnostics = Vec::new();
    for clause in &clauses {
        let supported = match (clause.index, clause.relation) {
            ("any", "any") => {
                push_or_join(&mut params, "query", clause.term);
                true
            }
            ("author", "any") => {
                push_or_join(&mut params, "query.author", clause.term);
                true
            }
            ("title", "any" | "adj") | ("container", "any") => {
                push_or_join(&mut params, "query.bibliographic", clause.term);
                true
            }
            ("year", rel) => match year_filter(rel, clause.term) {
                Some(f) => {
                    filters.extend(f);
                    true
                }
                None => {
                    diagnostics.push(
                        Diagnostic::unsupported(SOURCE, "year").with_message(format!(
                            "year {rel} `{}` is not a year (or two, for within)",
                            clause.term
                        )),
                    );
                    continue;
                }
            },
            ("doi", "=") => {
                // A DOI beside other clauses: Crossref filters by it.
                filters.push(format!("doi:{}", clause.term.trim()));
                true
            }
            ("issn", "=") => {
                filters.push(format!("issn:{}", clause.term.trim()));
                true
            }
            ("orcid", "=") => {
                filters.push(format!("orcid:{}", clause.term.trim()));
                true
            }
            _ => false,
        };
        if !supported {
            diagnostics.push(
                Diagnostic::unsupported(SOURCE, clause.index).with_message(format!(
                    "crossref has no `{}` index with relation `{}`",
                    clause.index, clause.relation
                )),
            );
        }
    }
    if !filters.is_empty() {
        params.push(("filter".into(), filters.join(",")));
    }
    Mapped::List {
        params,
        diagnostics,
    }
}

/// `sort` → Crossref's `sort` parameter (`relevance`, `issued`,
/// `is-referenced-by-count`).
pub fn sort_param(sort: &str) -> Option<&'static str> {
    match sort {
        "relevance" => Some("relevance"),
        "year" => Some("issued"),
        "cited_by" => Some("is-referenced-by-count"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_example_maps_to_bibliographic_author_and_a_date_filter() {
        let q: Query = serde_json::from_str(
            r#"{"and": [
              {"index": "author", "relation": "any", "term": "Abel"},
              {"index": "year",   "relation": "within", "term": "2020 2024"},
              {"or": [{"index": "title", "relation": "any", "term": "reionization"},
                      {"index": "any",   "relation": "any", "term": "21cm"}]}
            ]}"#,
        )
        .unwrap();
        let Mapped::List { params, diagnostics } = map(&q) else {
            panic!("a list")
        };
        assert!(diagnostics.is_empty());
        assert_eq!(
            params,
            vec![
                ("query.author".to_string(), "Abel".to_string()),
                ("query.bibliographic".to_string(), "reionization".to_string()),
                ("query".to_string(), "21cm".to_string()),
                (
                    "filter".to_string(),
                    "from-pub-date:2020-01-01,until-pub-date:2024-12-31".to_string()
                ),
            ]
        );
        assert!(!params.iter().any(|(k, _)| k == "query.title"), "deprecated");
    }

    #[test]
    fn a_lone_doi_is_a_lookup_and_an_unknown_index_a_diagnostic() {
        assert_eq!(
            map(&Query::clause("doi", "=", " 10.1126/science.1063991 ")),
            Mapped::Lookup("10.1126/science.1063991".into())
        );
        let q = Query::all(vec![
            Query::clause("first_author", "any", "Abel"),
            Query::clause("container", "any", "Science"),
            Query::clause("year", "=", "2002"),
            Query::clause("year", ">", "two thousand"),
        ]);
        let Mapped::List { params, diagnostics } = map(&q) else {
            panic!("a list")
        };
        assert_eq!(params[0], ("query.bibliographic".into(), "Science".into()));
        assert_eq!(
            params[1],
            ("filter".into(), "from-pub-date:2002-01-01,until-pub-date:2002-12-31".into())
        );
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].unsupported.as_deref(), Some("bib.first_author"));
        assert_eq!(diagnostics[1].unsupported.as_deref(), Some("bib.year"));
    }

    #[test]
    fn year_relations() {
        assert_eq!(year_filter("<", "2000"), Some(vec!["until-pub-date:1999-12-31".into()]));
        assert_eq!(year_filter(">", "2000"), Some(vec!["from-pub-date:2001-01-01".into()]));
        assert_eq!(year_filter("within", "2024-2020").unwrap()[0], "from-pub-date:2020-01-01");
        assert_eq!(year_filter("within", "2020"), None);
    }
}
