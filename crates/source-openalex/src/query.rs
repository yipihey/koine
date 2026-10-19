//! The canonical query (S-D2) mapped to OpenAlex `/works` parameters:
//! `any` → `search`, `title` → `filter=title.search:`, `abstract` →
//! `abstract.search:`, `author` → `raw_author_name.search:`, `year` →
//! `publication_year:` (`2002`, `<2002`, `>2002`, `2000-2003`), `doi` →
//! `doi:`, `pmid` → `ids.pmid:`, `issn` → `primary_location.source.issn:`,
//! `orcid` → `authorships.author.orcid:`, `cites = W…` → `cites:W…` (the
//! works that cite it), `cited_by = W…` → `cited_by:W…` (the works it
//! cites). A lone `doi =` is `GET /works/https://doi.org/{doi}`.
//!
//! Filters combine with AND only, so an `or` or a `not` is an
//! `unsupported` diagnostic and its clauses are dropped, visibly. OpenAlex
//! has no works filter on a journal's *name*, so `container` is not
//! declared (a source id or an ISSN is: use `issn`).

use koine_sources::{Diagnostic, Query};
use source_service::query::clauses;

use crate::SOURCE;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mapped {
    /// `GET /works/https://doi.org/{doi}`.
    Lookup(String),
    /// `GET /works?…`: the parameters (without paging) in order.
    List {
        params: Vec<(String, String)>,
        diagnostics: Vec<Diagnostic>,
    },
}

/// Commas and pipes are the filter syntax; a term must not carry them.
fn safe(term: &str) -> String {
    term.replace([',', '|'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A phrase is quoted for a `*.search` filter.
fn phrase(relation: &str, term: &str) -> String {
    let t = safe(term).replace('"', "");
    if relation == "adj" || relation == "exact" {
        format!("\"{t}\"")
    } else {
        t
    }
}

fn year(relation: &str, term: &str) -> Option<String> {
    let years: Vec<i32> = term
        .split(|c: char| c.is_whitespace() || c == '-' || c == ',')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<i32>())
        .collect::<Result<_, _>>()
        .ok()?;
    match (relation, years.as_slice()) {
        ("=", [y]) => Some(y.to_string()),
        ("<", [y]) => Some(format!("<{y}")),
        (">", [y]) => Some(format!(">{y}")),
        ("within", [a, b]) => Some(format!("{}-{}", a.min(b), a.max(b))),
        _ => None,
    }
}

/// `W2040912209`, `https://openalex.org/W2040912209` → `W2040912209`.
pub fn work_id(term: &str) -> Option<String> {
    let t = term.trim().rsplit('/').next()?;
    let digits = t.strip_prefix('W').or_else(|| t.strip_prefix('w'))?;
    (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())).then(|| format!("W{digits}"))
}

fn unsupported(diagnostics: &mut Vec<Diagnostic>, what: String) {
    diagnostics.push(Diagnostic::unsupported(SOURCE, what));
}

pub fn map(query: &Query) -> Mapped {
    let all = clauses(query);
    if let [one] = all.as_slice() {
        if one.index == "doi" && one.relation == "=" && matches!(query, Query::Clause { .. }) {
            return Mapped::Lookup(one.term.trim().to_string());
        }
    }
    let mut diagnostics = Vec::new();
    let mut searches: Vec<String> = Vec::new();
    let mut filters: Vec<String> = Vec::new();
    let top: Vec<&Query> = match query {
        Query::And { and } => and.iter().collect(),
        other => vec![other],
    };
    for node in top {
        match node {
            Query::Clause {
                index,
                relation,
                term,
            } => clause(
                index,
                relation,
                term,
                &mut searches,
                &mut filters,
                &mut diagnostics,
            ),
            Query::And { .. } => {
                // A nested and is still an and: flatten it.
                for c in clauses(node) {
                    clause(
                        c.index,
                        c.relation,
                        c.term,
                        &mut searches,
                        &mut filters,
                        &mut diagnostics,
                    );
                }
            }
            Query::Or { .. } => unsupported(
                &mut diagnostics,
                "bib.or: OpenAlex filters combine with AND only; the or was not sent".into(),
            ),
            Query::Not { .. } => unsupported(
                &mut diagnostics,
                "bib.not: OpenAlex filters combine with AND only; the not was not sent".into(),
            ),
            Query::Native { .. } => unsupported(
                &mut diagnostics,
                "native: OpenAlex declares no native query dialect".into(),
            ),
        }
    }
    let mut params = Vec::new();
    if !searches.is_empty() {
        params.push(("search".to_string(), searches.join(" ")));
    }
    if !filters.is_empty() {
        params.push(("filter".to_string(), filters.join(",")));
    }
    Mapped::List {
        params,
        diagnostics,
    }
}

fn clause(
    index: &str,
    relation: &str,
    term: &str,
    searches: &mut Vec<String>,
    filters: &mut Vec<String>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let t = safe(term);
    if t.is_empty() {
        unsupported(diagnostics, format!("bib.{index} {relation}: empty term"));
        return;
    }
    match (index, relation) {
        ("any", "any") => searches.push(t),
        ("title", "any" | "adj") => {
            filters.push(format!("title.search:{}", phrase(relation, term)))
        }
        ("abstract", "any") => filters.push(format!("abstract.search:{t}")),
        ("author", "any") => filters.push(format!("raw_author_name.search:{t}")),
        ("year", rel) => match year(rel, term) {
            Some(y) => filters.push(format!("publication_year:{y}")),
            None => unsupported(
                diagnostics,
                format!("bib.year {rel} `{t}`: not a year (or two, for within)"),
            ),
        },
        ("doi", "=") => filters.push(format!("doi:{t}")),
        ("pmid", "=") => filters.push(format!("ids.pmid:{t}")),
        ("issn", "=") => filters.push(format!("primary_location.source.issn:{t}")),
        ("orcid", "=") => filters.push(format!("authorships.author.orcid:{t}")),
        ("cites" | "cited_by", "=") => match work_id(term) {
            Some(w) => filters.push(format!("{index}:{w}")),
            None => unsupported(
                diagnostics,
                format!("bib.{index} = `{t}`: OpenAlex needs an openalex work id (W…)"),
            ),
        },
        _ => unsupported(diagnostics, format!("bib.{index} {relation}")),
    }
}

/// `sort` → OpenAlex's `sort`.
pub fn sort_param(sort: &str, has_search: bool) -> Option<&'static str> {
    match sort {
        "relevance" if has_search => Some("relevance_score:desc"),
        "relevance" => Some("cited_by_count:desc"),
        "year" => Some("publication_year:desc"),
        "cited_by" => Some("cited_by_count:desc"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(q: &Query) -> (Vec<(String, String)>, Vec<Diagnostic>) {
        match map(q) {
            Mapped::List {
                params,
                diagnostics,
            } => (params, diagnostics),
            other => panic!("a list, got {other:?}"),
        }
    }

    #[test]
    fn clauses_become_search_and_filters() {
        let q = Query::And {
            and: vec![
                Query::clause("any", "any", "reionization 21cm"),
                Query::clause("title", "adj", "first star"),
                Query::clause("author", "any", "Abel"),
                Query::clause("year", "within", "2020 2024"),
                Query::clause("cites", "=", "https://openalex.org/W2040912209"),
            ],
        };
        let (params, d) = list(&q);
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(params[0], ("search".into(), "reionization 21cm".into()));
        assert_eq!(
            params[1].1,
            "title.search:\"first star\",raw_author_name.search:Abel,publication_year:2020-2024,cites:W2040912209"
        );
    }

    #[test]
    fn a_lone_doi_is_a_lookup_and_the_rest_are_diagnostics() {
        assert_eq!(
            map(&Query::clause("doi", "=", " 10.1126/science.1063991 ")),
            Mapped::Lookup("10.1126/science.1063991".into())
        );
        let q = Query::And {
            and: vec![
                Query::clause("container", "any", "Science"),
                Query::clause("cited_by", "=", "10.1126/x"),
                Query::clause("year", "<", "1999"),
                Query::Or {
                    or: vec![Query::clause("any", "any", "a")],
                },
            ],
        };
        let (params, d) = list(&q);
        assert_eq!(params, [("filter".into(), "publication_year:<1999".into())]);
        assert_eq!(d.len(), 3);
        assert_eq!(d[0].detail, "bib.container any");
        assert!(d[1].detail.contains("needs an openalex work id"));
        assert!(d[2].detail.starts_with("bib.or"));
    }

    #[test]
    fn filter_syntax_characters_cannot_leak_out_of_a_term() {
        let (params, _) = list(&Query::clause("author", "any", "Abel,doi:evil|x"));
        assert_eq!(params[0].1, "raw_author_name.search:Abel doi:evil x");
    }
}
