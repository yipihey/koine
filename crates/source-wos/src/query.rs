//! The canonical query (S-D2) mapped to Web of Science's advanced-search
//! syntax, the `q` parameter of the Starter API: a clause becomes
//! `TAG=(term)`, `and`/`or`/`not` become `AND`/`OR`/`NOT` with parentheses,
//! and a clause the explain record does not cover is an `unsupported`
//! diagnostic and is dropped, visibly (S-D2: diagnostics, not fallbacks).
//!
//! | index | WoS field tag |
//! |---|---|
//! | `any` | `TS` (topic: title, abstract, keywords) |
//! | `title` | `TI` |
//! | `author` | `AU` |
//! | `year` | `PY` (`2002`, `2020-2024`) |
//! | `doi` | `DO` |
//! | `container` | `SO` (source title) |
//! | `keyword` | `AK` (author keywords) |
//!
//! `abstract`, `first_author` and the rest are not indexes the Starter API
//! has; they are diagnostics, not a quiet fall back to `TS` or `AU`.

use koine_sources::{Diagnostic, Query};

use crate::SOURCE;

/// What a query becomes: the `q`, or nothing when no clause mapped.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Mapped {
    pub q: Option<String>,
    pub diagnostics: Vec<Diagnostic>,
}

/// A term with the characters that would change WoS's grammar removed:
/// quotes and parentheses are never part of a name or a title word.
fn clean(term: &str) -> String {
    term.chars()
        .filter(|c| !matches!(c, '"' | '(' | ')' | '='))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn words(tag: &str, term: &str) -> Option<String> {
    let t = clean(term);
    let parts: Vec<&str> = t.split(' ').filter(|w| !w.is_empty()).collect();
    (!parts.is_empty()).then(|| format!("{tag}=({})", parts.join(" AND ")))
}

fn phrase(tag: &str, term: &str) -> Option<String> {
    let t = clean(term);
    (!t.is_empty()).then(|| format!("{tag}=(\"{t}\")"))
}

/// `Abel, T.` → `Abel T`: the comma and period are citation style.
fn name(tag: &str, term: &str) -> Option<String> {
    let t = clean(&term.replace([',', '.'], " "));
    (!t.is_empty()).then(|| format!("{tag}=({t})"))
}

/// A `year` term: `2002` (`=`, `<`, `>`) or `2020 2024` (`within`).
fn year(relation: &str, term: &str) -> Option<String> {
    let years: Vec<i32> = term
        .split(|c: char| c.is_whitespace() || c == '-' || c == ',' || c == ':')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<i32>())
        .collect::<Result<_, _>>()
        .ok()?;
    let range = match (relation, years.as_slice()) {
        ("=", [y]) => format!("{y}"),
        ("<", [y]) => format!("1000-{}", y - 1),
        (">", [y]) => format!("{}-3000", y + 1),
        ("within", [a, b]) => format!("{}-{}", a.min(b), a.max(b)),
        _ => return None,
    };
    Some(format!("PY={range}"))
}

fn clause(index: &str, relation: &str, term: &str) -> Result<Option<String>, String> {
    Ok(match (index, relation) {
        ("any", "any") => words("TS", term),
        ("title", "any") => words("TI", term),
        ("title", "adj") => phrase("TI", term),
        ("author", "any") => name("AU", term),
        ("container", "any") => phrase("SO", term),
        ("keyword", "any") => phrase("AK", term),
        ("doi", "=") => {
            let t = clean(term);
            (!t.is_empty()).then(|| format!("DO=({t})"))
        }
        ("year", rel) => match year(rel, term) {
            Some(y) => Some(y),
            None => {
                return Err(format!(
                    "bib.year {rel} `{term}`: not a year (or two, for within)"
                ))
            }
        },
        _ => return Err(format!("bib.{index} {relation}")),
    })
}

fn walk(query: &Query, out: &mut Vec<Diagnostic>) -> Option<String> {
    match query {
        Query::Clause {
            index,
            relation,
            term,
        } => match clause(index, relation, term) {
            Ok(t) => t,
            Err(what) => {
                out.push(Diagnostic::unsupported(SOURCE, what));
                None
            }
        },
        Query::Native { .. } => {
            out.push(Diagnostic::unsupported(SOURCE, "native"));
            None
        }
        Query::And { and: qs } => join(qs, " AND ", out),
        Query::Or { or: qs } => join(qs, " OR ", out),
        Query::Not { not } => {
            walk(not, out);
            out.push(Diagnostic::unsupported(
                SOURCE,
                "not (a negation needs a positive clause beside it)",
            ));
            None
        }
    }
}

fn join(qs: &[Query], op: &str, out: &mut Vec<Diagnostic>) -> Option<String> {
    let mut positives: Vec<String> = Vec::new();
    let mut negatives: Vec<String> = Vec::new();
    for q in qs {
        match q {
            Query::Not { not } if op == " AND " => {
                if let Some(t) = walk(not, out) {
                    negatives.push(t);
                }
            }
            other => {
                if let Some(t) = walk(other, out) {
                    positives.push(t);
                }
            }
        }
    }
    let base = match positives.len() {
        0 => {
            if !negatives.is_empty() {
                out.push(Diagnostic::unsupported(
                    SOURCE,
                    "not (a negation needs a positive clause beside it)",
                ));
            }
            return None;
        }
        1 => positives.into_iter().next().unwrap_or_default(),
        _ => format!("({})", positives.join(op)),
    };
    if negatives.is_empty() {
        Some(base)
    } else {
        Some(format!("({base} NOT ({}))", negatives.join(" OR ")))
    }
}

/// Map `query`.
pub fn map(query: &Query) -> Mapped {
    let mut diagnostics = Vec::new();
    let q = walk(query, &mut diagnostics);
    Mapped { q, diagnostics }
}

/// `sort` → the Starter API's `sortField` (`RS+D` relevance, `PY+D` newest
/// first, `TC+D` most cited first).
pub fn sort_param(sort: &str) -> Option<&'static str> {
    match sort {
        "relevance" => Some("RS+D"),
        "year" => Some("PY+D"),
        "cited_by" => Some("TC+D"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_example_maps_to_field_tags() {
        let q: Query = serde_json::from_str(
            r#"{"and": [
              {"index": "author", "relation": "any", "term": "Abel, T."},
              {"index": "year",   "relation": "within", "term": "2020 2024"},
              {"or": [{"index": "title", "relation": "any", "term": "reionization"},
                      {"index": "any",   "relation": "any", "term": "21cm spectra"}]}
            ]}"#,
        )
        .unwrap();
        let m = map(&q);
        assert!(m.diagnostics.is_empty());
        assert_eq!(
            m.q.as_deref(),
            Some("(AU=(Abel T) AND PY=2020-2024 AND (TI=(reionization) OR TS=(21cm AND spectra)))")
        );
    }

    #[test]
    fn every_declared_index_has_its_tag() {
        for (index, rel, term, want) in [
            ("title", "adj", "first star", "TI=(\"first star\")"),
            ("container", "any", "Science", "SO=(\"Science\")"),
            ("keyword", "any", "dark matter", "AK=(\"dark matter\")"),
            (
                "doi",
                "=",
                " 10.1126/science.1063991 ",
                "DO=(10.1126/science.1063991)",
            ),
            ("year", "=", "2002", "PY=2002"),
            ("year", "<", "2000", "PY=1000-1999"),
            ("year", ">", "2000", "PY=2001-3000"),
        ] {
            assert_eq!(
                map(&Query::clause(index, rel, term)).q.as_deref(),
                Some(want),
                "{index} {rel}"
            );
        }
    }

    #[test]
    fn unsupported_clauses_are_diagnostics_and_a_term_cannot_inject_syntax() {
        let q = Query::And {
            and: vec![
                Query::clause("abstract", "any", "star"),
                Query::clause("first_author", "any", "Abel"),
                Query::clause("title", "adj", "x\") OR UT=(WOS:1"),
            ],
        };
        let m = map(&q);
        assert_eq!(m.q.as_deref(), Some("TI=(\"x OR UTWOS:1\")"));
        assert_eq!(m.diagnostics.len(), 2);
        assert_eq!(m.diagnostics[0].detail, "bib.abstract any");
        assert_eq!(m.diagnostics[1].detail, "bib.first_author any");
        let only = map(&Query::clause("pmid", "=", "1"));
        assert_eq!(only.q, None);
        assert_eq!(
            map(&Query::Native { native: "x".into() }).diagnostics[0].detail,
            "native"
        );
    }

    #[test]
    fn and_not_becomes_wos_not() {
        let q = Query::And {
            and: vec![
                Query::clause("title", "any", "crispr"),
                Query::Not {
                    not: Box::new(Query::clause("title", "any", "review")),
                },
            ],
        };
        assert_eq!(
            map(&q).q.as_deref(),
            Some("(TI=(crispr) NOT (TI=(review)))")
        );
    }
}
