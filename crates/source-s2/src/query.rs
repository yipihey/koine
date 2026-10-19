//! The canonical query (S-D2) mapped to Semantic Scholar's Graph API:
//!
//! | clause | request |
//! |---|---|
//! | `any`, `title` | the `query` string of `/paper/search` (search is free text: a title clause is words in the query, not a restriction; clauses are space-joined) |
//! | `year` `=` `<` `>` `within` | `year=2002` / `-2001` / `2003-` / `2020-2024` |
//! | `doi` `arxiv` `pmid` `=` | `GET /paper/{DOI:…\|ARXIV:…\|PMID:…\|id}` — a lookup, other clauses beside it are dropped with a diagnostic |
//! | `cites` `=` id | the works citing id: `/paper/{id}/citations` |
//! | `cited_by` `=` id | the works id cites: `/paper/{id}/references` |
//! | `similar_to` `=` id | `/recommendations/v1/papers/forpaper/{id}` |
//!
//! `author`, `container` and the rest have no field in `/paper/search`; they
//! are `unsupported` diagnostics, not guesses.

use koine_sources::{Diagnostic, Query};
use source_service::query::clauses;

use crate::SOURCE;

/// What a query becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mapped {
    /// `GET /paper/{id}`: the S2 spelling of the id (`DOI:10.x/y`).
    Lookup {
        id: String,
        diagnostics: Vec<Diagnostic>,
    },
    /// A relation walk from the canonical id (`doi:…`, `s2:…`).
    Walk {
        relation: Walk,
        id: String,
        diagnostics: Vec<Diagnostic>,
    },
    /// `GET /paper/search?…`: `query` and optional `year`, without paging.
    Search {
        text: String,
        year: Option<String>,
        diagnostics: Vec<Diagnostic>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Walk {
    /// `cites = X`: the works that cite X.
    Citations,
    /// `cited_by = X`: the works X cites.
    References,
    /// `similar_to = X`.
    Similar,
}

/// A canonical id `kind:value` → the S2 path segment, when S2 resolves it.
pub fn s2_id(kind: &str, value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    match kind {
        "doi" if value.starts_with("10.") && value.contains('/') => Some(format!("DOI:{value}")),
        "arxiv" => Some(format!("ARXIV:{}", strip_version(value))),
        "pmid" if value.chars().all(|c| c.is_ascii_digit()) => Some(format!("PMID:{value}")),
        "pmcid" => Some(format!("PMCID:{value}")),
        "corpusid" if value.chars().all(|c| c.is_ascii_digit()) => {
            Some(format!("CorpusId:{value}"))
        }
        "s2" if is_paper_id(value) => Some(value.to_ascii_lowercase()),
        _ => None,
    }
}

/// A Semantic Scholar paper id: 40 hex digits.
pub fn is_paper_id(s: &str) -> bool {
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn strip_version(arxiv: &str) -> &str {
    match arxiv.rfind('v') {
        Some(i)
            if i > 0
                && !arxiv[i + 1..].is_empty()
                && arxiv[i + 1..].chars().all(|c| c.is_ascii_digit()) =>
        {
            &arxiv[..i]
        }
        _ => arxiv,
    }
}

/// The S2 `year` parameter for one year clause.
fn year_param(relation: &str, term: &str) -> Option<String> {
    let ys: Vec<i32> = term
        .split(|c: char| c.is_whitespace() || c == '-' || c == ',')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<i32>())
        .collect::<Result<_, _>>()
        .ok()?;
    match (relation, ys.as_slice()) {
        ("=", [y]) => Some(y.to_string()),
        ("<", [y]) => Some(format!("-{}", y - 1)),
        (">", [y]) => Some(format!("{}-", y + 1)),
        ("within", [a, b]) => Some(format!("{}-{}", a.min(b), a.max(b))),
        _ => None,
    }
}

/// Map `query`.
pub fn map(query: &Query) -> Mapped {
    let all = clauses(query);
    let mut diagnostics = Vec::new();
    let mut text: Vec<&str> = Vec::new();
    let mut year: Option<String> = None;
    let mut lookup: Option<String> = None;
    let mut walk: Option<(Walk, String)> = None;
    for c in &all {
        let term = c.term.trim();
        match (c.index, c.relation) {
            ("any", "any") | ("title", "any" | "adj") => text.push(term),
            ("year", rel) => match year_param(rel, term) {
                Some(p) if year.is_none() => year = Some(p),
                Some(_) => diagnostics.push(Diagnostic::unsupported(
                    SOURCE,
                    "bib.year: only one year clause per query",
                )),
                None => diagnostics.push(Diagnostic::unsupported(
                    SOURCE,
                    format!("bib.year {rel} `{term}`: not a year (or two, for within)"),
                )),
            },
            ("doi" | "arxiv" | "pmid", "=") => match s2_id(c.index, term) {
                Some(id) if lookup.is_none() => lookup = Some(id),
                _ => diagnostics.push(Diagnostic::unsupported(
                    SOURCE,
                    format!("bib.{} = `{term}`: not an identifier S2 resolves", c.index),
                )),
            },
            ("cites" | "cited_by" | "similar_to", "=") => {
                let kind = match crate::canonical_of(term) {
                    Some(k) => k,
                    None => {
                        diagnostics.push(Diagnostic::unsupported(
                            SOURCE,
                            format!("bib.{} = `{term}`: not an identifier S2 resolves", c.index),
                        ));
                        continue;
                    }
                };
                let w = match c.index {
                    "cites" => Walk::Citations,
                    "cited_by" => Walk::References,
                    _ => Walk::Similar,
                };
                if walk.is_none() {
                    walk = Some((w, kind));
                }
            }
            _ => diagnostics.push(Diagnostic::unsupported(
                SOURCE,
                format!("bib.{} {}", c.index, c.relation),
            )),
        }
    }
    let beside = |what: &str, diagnostics: &mut Vec<Diagnostic>| {
        if !text.is_empty() || year.is_some() {
            diagnostics.push(Diagnostic::unsupported(
                SOURCE,
                format!("text and year clauses are ignored beside {what}"),
            ));
        }
    };
    if let Some(id) = lookup {
        beside("an identifier lookup", &mut diagnostics);
        return Mapped::Lookup { id, diagnostics };
    }
    if let Some((relation, id)) = walk {
        beside("a citation walk", &mut diagnostics);
        return Mapped::Walk {
            relation,
            id,
            diagnostics,
        };
    }
    Mapped::Search {
        text: text.join(" "),
        year,
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_and_year_become_query_and_year() {
        let q = Query::And {
            and: vec![
                Query::clause("title", "any", "attention"),
                Query::clause("any", "any", "transformer"),
                Query::clause("year", "within", "2024 2020"),
            ],
        };
        assert_eq!(
            map(&q),
            Mapped::Search {
                text: "attention transformer".into(),
                year: Some("2020-2024".into()),
                diagnostics: vec![]
            }
        );
        assert_eq!(year_param("<", "2002").unwrap(), "-2001");
        assert_eq!(year_param(">", "2002").unwrap(), "2003-");
        assert_eq!(year_param("within", "2020"), None);
    }

    #[test]
    fn identifiers_are_lookups_in_s2_spelling() {
        for (kind, term, id) in [
            ("doi", "10.18653/v1/N19-1423", "DOI:10.18653/v1/N19-1423"),
            ("arxiv", "1706.03762v7", "ARXIV:1706.03762"),
            ("pmid", "11729294", "PMID:11729294"),
        ] {
            assert_eq!(
                map(&Query::clause(kind, "=", term)),
                Mapped::Lookup {
                    id: id.into(),
                    diagnostics: vec![]
                },
                "{kind}"
            );
        }
        assert_eq!(s2_id("corpusid", "13756489").unwrap(), "CorpusId:13756489");
        assert_eq!(
            s2_id("s2", "204E3073870FAE3D05BCBC2F6A8E263D9B72E776").unwrap(),
            "204e3073870fae3d05bcbc2f6a8e263d9b72e776"
        );
        // `s2` is not a bib index: a clause on it is a diagnostic.
        let Mapped::Search { diagnostics, .. } = map(&Query::clause(
            "s2",
            "=",
            "204e3073870fae3d05bcbc2f6a8e263d9b72e776",
        )) else {
            panic!("a search")
        };
        assert_eq!(diagnostics[0].detail, "bib.s2 =");
        assert_eq!(s2_id("s2", "not-hex"), None);
        assert_eq!(s2_id("bibcode", "2002Sci...295...93A"), None);
    }

    #[test]
    fn walks_and_what_s2_cannot_restrict() {
        let m = map(&Query::clause("cites", "=", "arxiv:1706.03762"));
        assert_eq!(
            m,
            Mapped::Walk {
                relation: Walk::Citations,
                id: "arxiv:1706.03762".into(),
                diagnostics: vec![]
            }
        );
        let m = map(&Query::And {
            and: vec![
                Query::clause("title", "any", "attention"),
                Query::clause("author", "any", "Vaswani"),
                Query::clause("container", "any", "NeurIPS"),
            ],
        });
        let Mapped::Search {
            text, diagnostics, ..
        } = m
        else {
            panic!("a search")
        };
        assert_eq!(text, "attention");
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].detail, "bib.author any");
        let m = map(&Query::And {
            and: vec![
                Query::clause("doi", "=", "10.1/x"),
                Query::clause("title", "any", "ignored"),
            ],
        });
        let Mapped::Lookup { diagnostics, .. } = m else {
            panic!("a lookup")
        };
        assert!(diagnostics[0].detail.contains("ignored beside"));
    }
}
