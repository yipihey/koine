//! The canonical query (S-D2) mapped to the ADS Lucene syntax SciX's
//! `search/query` takes as `q` (the explain record's `native: "ads-lucene"`,
//! so a `{"native": "…"}` query passes through untouched, parenthesised
//! when it sits inside an `and`/`or`). A clause the explain record does not
//! cover is an `unsupported` diagnostic and is dropped, visibly.
//!
//! | index | ADS field |
//! |---|---|
//! | `any` | unfielded terms |
//! | `title` | `title:(…)` / `title:"…"` (`adj`) |
//! | `abstract` | `abstract:(…)` |
//! | `author` / `first_author` | `author:"Abel, T"` / `first_author:"…"` |
//! | `year` | `year:2002`, `year:[2020 TO 2024]`, `[* TO y-1]`, `[y+1 TO *]` |
//! | `bibcode` / `doi` | `bibcode:…` / `doi:"…"` |
//! | `arxiv` | `identifier:"arXiv:…"` |
//! | `container` | `bibstem:Sci` for a bibstem-shaped term, else `pub:"Science"` |
//! | `keyword` | `keyword:"…"` |

use koine_sources::{Diagnostic, Query};

use crate::SOURCE;

/// What a query becomes: the `q`, or nothing when no clause mapped.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Mapped {
    pub q: Option<String>,
    pub diagnostics: Vec<Diagnostic>,
}

/// A term without the characters that would change the grammar.
fn clean(term: &str) -> String {
    term.chars()
        .filter(|c| !matches!(c, '"' | '(' | ')' | '[' | ']' | ':' | '\\'))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn words(field: Option<&str>, term: &str) -> Option<String> {
    let t = clean(term);
    let parts: Vec<&str> = t.split(' ').filter(|w| !w.is_empty()).collect();
    let group = match parts.len() {
        0 => return None,
        1 => parts[0].to_string(),
        _ => format!("({})", parts.join(" AND ")),
    };
    Some(match field {
        Some(f) => format!("{f}:{group}"),
        None => group,
    })
}

fn phrase(field: &str, term: &str) -> Option<String> {
    let t = clean(term);
    (!t.is_empty()).then(|| format!("{field}:\"{t}\""))
}

/// A bibstem is short, has no space: `Sci`, `ApJ`, `MNRAS`, `A&A`.
fn looks_like_bibstem(t: &str) -> bool {
    !t.contains(' ')
        && t.len() <= 12
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '&' || c == '.')
}

fn year(relation: &str, term: &str) -> Option<String> {
    let years: Vec<i32> = term
        .split(|c: char| c.is_whitespace() || c == '-' || c == ',' || c == ':')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<i32>())
        .collect::<Result<_, _>>()
        .ok()?;
    Some(match (relation, years.as_slice()) {
        ("=", [y]) => format!("year:{y}"),
        ("<", [y]) => format!("year:[* TO {}]", y - 1),
        (">", [y]) => format!("year:[{} TO *]", y + 1),
        ("within", [a, b]) => format!("year:[{} TO {}]", a.min(b), a.max(b)),
        _ => return None,
    })
}

fn clause(index: &str, relation: &str, term: &str) -> Result<Option<String>, String> {
    Ok(match (index, relation) {
        ("any", "any") => words(None, term),
        ("title", "any") => words(Some("title"), term),
        ("title", "adj") => phrase("title", term),
        ("abstract", "any") => words(Some("abstract"), term),
        ("author", "any") => phrase("author", term),
        ("first_author", "any") => phrase("first_author", term),
        ("container", "any") => {
            let t = clean(term);
            if looks_like_bibstem(&t) {
                (!t.is_empty()).then(|| format!("bibstem:{t}"))
            } else {
                phrase("pub", &t)
            }
        }
        ("keyword", "any") => phrase("keyword", term),
        ("bibcode", "=") => {
            let t = clean(term);
            (!t.is_empty()).then(|| format!("bibcode:{t}"))
        }
        ("doi", "=") => phrase("doi", term),
        ("arxiv", "=") => {
            let t = clean(term);
            let t = t.strip_prefix("arXiv ").unwrap_or(&t).to_string();
            (!t.is_empty()).then(|| format!("identifier:\"arXiv:{t}\""))
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

fn walk(query: &Query, nested: bool, out: &mut Vec<Diagnostic>) -> Option<String> {
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
        Query::Native { native } => {
            let n = native.trim();
            (!n.is_empty()).then(|| {
                if nested {
                    format!("({n})")
                } else {
                    n.to_string()
                }
            })
        }
        Query::And { and: qs } => join(qs, " AND ", out),
        Query::Or { or: qs } => join(qs, " OR ", out),
        Query::Not { not } => {
            walk(not, true, out);
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
                if let Some(t) = walk(not, true, out) {
                    negatives.push(t);
                }
            }
            other => {
                if let Some(t) = walk(other, true, out) {
                    positives.push(t);
                }
            }
        }
    }
    if positives.is_empty() {
        if !negatives.is_empty() {
            out.push(Diagnostic::unsupported(
                SOURCE,
                "not (a negation needs a positive clause beside it)",
            ));
        }
        return None;
    }
    let mut q = match positives.len() {
        1 => positives.into_iter().next().unwrap_or_default(),
        _ => format!("({})", positives.join(op)),
    };
    for n in negatives {
        q = format!("({q} NOT {n})");
    }
    Some(q)
}

/// Map `query`.
pub fn map(query: &Query) -> Mapped {
    let mut diagnostics = Vec::new();
    let q = walk(query, false, &mut diagnostics);
    Mapped { q, diagnostics }
}

/// `sort` → ADS's `sort` parameter.
pub fn sort_param(sort: &str) -> Option<&'static str> {
    match sort {
        "relevance" => Some("score desc"),
        "year" => Some("date desc"),
        "cited_by" => Some("citation_count desc"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_example_maps_to_ads_fields() {
        let q: Query = serde_json::from_str(
            r#"{"and": [
              {"index": "author", "relation": "any", "term": "Abel, T"},
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
            Some("(author:\"Abel, T\" AND year:[2020 TO 2024] AND (title:reionization OR (21cm AND spectra)))")
        );
    }

    #[test]
    fn every_declared_index_and_relation() {
        for (index, rel, term, want) in [
            ("title", "adj", "first star", "title:\"first star\""),
            (
                "abstract",
                "any",
                "dark matter",
                "abstract:(dark AND matter)",
            ),
            ("first_author", "any", "Abel, T", "first_author:\"Abel, T\""),
            (
                "bibcode",
                "=",
                "2002Sci...295...93A",
                "bibcode:2002Sci...295...93A",
            ),
            (
                "doi",
                "=",
                "10.1126/science.1063991",
                "doi:\"10.1126/science.1063991\"",
            ),
            (
                "arxiv",
                "=",
                "astro-ph/0112088",
                "identifier:\"arXiv:astro-ph/0112088\"",
            ),
            ("container", "any", "Sci", "bibstem:Sci"),
            (
                "container",
                "any",
                "Monthly Notices",
                "pub:\"Monthly Notices\"",
            ),
            ("keyword", "any", "cosmology", "keyword:\"cosmology\""),
            ("year", "<", "2000", "year:[* TO 1999]"),
            ("year", ">", "2000", "year:[2001 TO *]"),
            ("year", "=", "2002", "year:2002"),
        ] {
            assert_eq!(
                map(&Query::clause(index, rel, term)).q.as_deref(),
                Some(want),
                "{index} {rel}"
            );
        }
    }

    #[test]
    fn a_native_clause_passes_through_and_is_parenthesised_inside_a_group() {
        let m = map(&Query::Native {
            native: "author:\"Abel, T\" year:2002 bibstem:Sci".into(),
        });
        assert_eq!(
            m.q.as_deref(),
            Some("author:\"Abel, T\" year:2002 bibstem:Sci")
        );
        let q = Query::And {
            and: vec![
                Query::Native {
                    native: "citations(bibcode:X) OR bibcode:Y".into(),
                },
                Query::clause("year", ">", "2010"),
            ],
        };
        assert_eq!(
            map(&q).q.as_deref(),
            Some("((citations(bibcode:X) OR bibcode:Y) AND year:[2011 TO *])")
        );
    }

    #[test]
    fn unsupported_clauses_are_diagnostics_and_terms_cannot_inject() {
        let m = map(&Query::clause("pmid", "=", "1"));
        assert_eq!(m.q, None);
        assert_eq!(m.diagnostics[0].detail, "bib.pmid =");
        let m = map(&Query::clause("author", "any", "x\" OR bibcode:\"y"));
        assert_eq!(m.q.as_deref(), Some("author:\"x OR bibcodey\""));
    }

    #[test]
    fn and_not() {
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
            Some("(title:crispr NOT title:review)")
        );
    }
}
