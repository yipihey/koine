//! The canonical query (S-D2) mapped to PubMed's own boolean syntax, which
//! is the esearch `term`: a clause becomes `"term"[Tag]` (words of a
//! text index are tagged one by one and ANDed, because PubMed applies a tag
//! only to the word before it), `and`/`or`/`not` become `AND`/`OR`/`NOT`
//! with parentheses, and a clause the explain record does not cover is an
//! `unsupported` diagnostic and is dropped, visibly. Nothing is rewritten
//! into another index (S-D2: diagnostics, not fallbacks).
//!
//! | index | PubMed field |
//! |---|---|
//! | `any` | (no tag: All Fields) |
//! | `title` | `[Title]` |
//! | `abstract` | `[Abstract]` |
//! | `author` | `[Author]` (unquoted: `Doudna J[Author]`) |
//! | `first_author` | `[First Author]` (unquoted) |
//! | `year` | `[PDAT]` (publication date; `=`, `<`, `>`, `within` as ranges) |
//! | `container` | `[Journal]` |
//! | `pmid` | `[PMID]` |
//! | `doi` | `[AID]` (article identifier) |
//! | `keyword` | `[MeSH Terms]` |

use koine_sources::{Diagnostic, Query};

use crate::SOURCE;

/// What a query becomes: the `term`, or nothing when no clause mapped.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Mapped {
    pub term: Option<String>,
    pub diagnostics: Vec<Diagnostic>,
}

/// A term with the characters that would change PubMed's grammar removed:
/// quotes, brackets and parentheses are never part of a title word or a name.
fn clean(term: &str) -> String {
    term.chars()
        .filter(|c| !matches!(c, '"' | '[' | ']' | '(' | ')'))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A journal or MeSH heading: one quoted phrase. A comma and a period
/// inside it are the citation style, not part of the index entry.
fn phrase(term: &str, tag: &str) -> Option<String> {
    let t = clean(&term.replace([',', '.'], " "));
    (!t.is_empty()).then(|| format!("\"{t}\"[{tag}]"))
}

/// An author: `Doudna J[Author]`, **unquoted**. PubMed's author index
/// answers `Doudna J[Author]` and `Doudna[Author]` but a quoted
/// `"Doudna"[Author]` finds nothing (checked live 2026-10-19: 0 hits for
/// `"Doudna"[Author] AND CRISPR[Title]`, 149 for the unquoted form).
fn author(term: &str, tag: &str) -> Option<String> {
    let t = clean(&term.replace([',', '.'], " "));
    (!t.is_empty()).then(|| format!("{t}[{tag}]"))
}

/// Words, each tagged (or untagged for `any`), ANDed.
fn words(term: &str, tag: Option<&str>) -> Option<String> {
    let t = clean(term);
    let parts: Vec<String> = t
        .split(' ')
        .filter(|w| !w.is_empty())
        .map(|w| match tag {
            Some(tag) => format!("{w}[{tag}]"),
            None => w.to_string(),
        })
        .collect();
    match parts.len() {
        0 => None,
        1 => Some(parts.into_iter().next().unwrap_or_default()),
        _ => Some(format!("({})", parts.join(" AND "))),
    }
}

/// A `year` term: `2002` (`=`, `<`, `>`) or `2020 2024` (`within`).
fn year_range(relation: &str, term: &str) -> Option<String> {
    let years: Vec<i32> = term
        .split(|c: char| c.is_whitespace() || c == '-' || c == ',' || c == ':')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<i32>())
        .collect::<Result<_, _>>()
        .ok()?;
    let range = match (relation, years.as_slice()) {
        ("=", [y]) => format!("{y}:{y}"),
        ("<", [y]) => format!("1000:{}", y - 1),
        (">", [y]) => format!("{}:3000", y + 1),
        ("within", [a, b]) => format!("{}:{}", a.min(b), a.max(b)),
        _ => return None,
    };
    Some(format!("{range}[PDAT]"))
}

fn clause(index: &str, relation: &str, term: &str) -> Result<Option<String>, String> {
    Ok(match (index, relation) {
        ("any", "any") => words(term, None),
        ("title", "any") => words(term, Some("Title")),
        ("title", "adj") => {
            let t = clean(term);
            (!t.is_empty()).then(|| format!("\"{t}\"[Title]"))
        }
        ("abstract", "any") => words(term, Some("Abstract")),
        ("author", "any") => author(term, "Author"),
        ("first_author", "any") => author(term, "First Author"),
        ("container", "any") => phrase(term, "Journal"),
        ("keyword", "any") => phrase(term, "MeSH Terms"),
        ("pmid", "=") => {
            let t = term.trim();
            if t.is_empty() || !t.chars().all(|c| c.is_ascii_digit()) {
                return Err(format!("bib.pmid = `{t}`: not a PMID"));
            }
            Some(format!("{t}[PMID]"))
        }
        ("doi", "=") => {
            let t = clean(term);
            (!t.is_empty()).then(|| format!("\"{t}\"[AID]"))
        }
        ("year", rel) => match year_range(rel, term) {
            Some(r) => Some(r),
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
        // A NOT with nothing to exclude from is dropped: PubMed has no
        // unary NOT, and `NOT x` alone would be an error, not a filter.
        Query::Not { not } => {
            let inner = walk(not, out);
            out.push(Diagnostic::unsupported(
                SOURCE,
                "not (a negation needs a positive clause beside it)",
            ));
            drop(inner);
            None
        }
    }
}

fn join(qs: &[Query], op: &str, out: &mut Vec<Diagnostic>) -> Option<String> {
    // `a AND (NOT b)` is PubMed's `a NOT b`: handled here, where the
    // positive side is known.
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
    let term = walk(query, &mut diagnostics);
    Mapped { term, diagnostics }
}

/// `sort` → esearch's `sort` parameter (`relevance`, `pub_date`).
pub fn sort_param(sort: &str) -> Option<&'static str> {
    match sort {
        "relevance" => Some("relevance"),
        "year" => Some("pub_date"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_example_maps_to_tagged_phrases_and_a_pdat_range() {
        let q: Query = serde_json::from_str(
            r#"{"and": [
              {"index": "author", "relation": "any", "term": "Doudna, J."},
              {"index": "year",   "relation": "within", "term": "2012 2014"},
              {"or": [{"index": "title", "relation": "any", "term": "CRISPR cas9"},
                      {"index": "any",   "relation": "any", "term": "genome"}]}
            ]}"#,
        )
        .unwrap();
        let m = map(&q);
        assert!(m.diagnostics.is_empty());
        assert_eq!(
            m.term.as_deref(),
            Some(
                "(Doudna J[Author] AND 2012:2014[PDAT] AND ((CRISPR[Title] AND cas9[Title]) OR genome))"
            )
        );
    }

    #[test]
    fn unsupported_clauses_are_diagnostics_and_a_query_of_only_those_sends_nothing() {
        let q = Query::And {
            and: vec![
                Query::clause("arxiv", "=", "2101.00001"),
                Query::clause("year", ">", "twenty"),
            ],
        };
        let m = map(&q);
        assert_eq!(m.term, None);
        assert_eq!(m.diagnostics.len(), 2);
        assert_eq!(m.diagnostics[0].detail, "bib.arxiv =");
        assert!(m.diagnostics[1].detail.starts_with("bib.year >"));
        let m = map(&Query::Native { native: "x".into() });
        assert_eq!(m.diagnostics[0].detail, "native");
    }

    #[test]
    fn a_term_cannot_inject_pubmed_syntax() {
        let m = map(&Query::clause(
            "title",
            "adj",
            "x\"[Author] OR (secret)[Title",
        ));
        assert_eq!(m.term.as_deref(), Some("\"xAuthor OR secretTitle\"[Title]"));
    }

    #[test]
    fn year_relations_and_identifier_clauses() {
        assert_eq!(year_range("=", "2002").as_deref(), Some("2002:2002[PDAT]"));
        assert_eq!(year_range("<", "2000").as_deref(), Some("1000:1999[PDAT]"));
        assert_eq!(year_range(">", "2000").as_deref(), Some("2001:3000[PDAT]"));
        assert_eq!(
            year_range("within", "2024-2020").as_deref(),
            Some("2020:2024[PDAT]")
        );
        assert_eq!(year_range("within", "2020"), None);
        assert_eq!(
            map(&Query::clause("doi", "=", " 10.1126/science.1063991 "))
                .term
                .as_deref(),
            Some("\"10.1126/science.1063991\"[AID]")
        );
        assert_eq!(
            map(&Query::clause("pmid", "=", "11711636")).term.as_deref(),
            Some("11711636[PMID]")
        );
        assert!(map(&Query::clause("pmid", "=", "abc")).term.is_none());
    }

    #[test]
    fn and_not_becomes_pubmeds_not() {
        let q = Query::And {
            and: vec![
                Query::clause("title", "any", "crispr"),
                Query::Not {
                    not: Box::new(Query::clause("title", "any", "review")),
                },
            ],
        };
        assert_eq!(
            map(&q).term.as_deref(),
            Some("(crispr[Title] NOT (review[Title]))")
        );
        let lone = Query::Not {
            not: Box::new(Query::clause("title", "any", "review")),
        };
        let m = map(&lone);
        assert!(m.term.is_none());
        assert_eq!(m.diagnostics.len(), 1);
    }
}
