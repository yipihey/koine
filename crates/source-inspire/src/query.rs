//! The canonical query (S-D2) mapped to INSPIRE's SPIRES-style `q`:
//! the query tree is kept (`and`/`or`/`not` become `and`/`or`/`not` with
//! parentheses), because INSPIRE's grammar has all three; each clause becomes
//! one SPIRES term.
//!
//! | clause | INSPIRE |
//! |---|---|
//! | `any` | the bare text |
//! | `title` any / adj | `t a and t b` / `t "a b"` |
//! | `author` / `first_author` | `a Abel, T.` (a comma keeps INSPIRE's name matching) / `a Tom and a Abel` / `fa …` |
//! | `year` `=` `<` `>` `within` | `date 2002` / `date 1000->2001` / `date 2003->2999` / `date 2020->2024` |
//! | `doi` / `arxiv` | `doi 10.x/y` / `eprint astro-ph/0112088` |
//! | `container` | `j Science` |
//! | `keyword` | `k "first stars"` |
//! | `cites` = recid | `refersto:recid:N` (the works that cite N) |
//! | `cited_by` = recid | `citedby:recid:N` (the works N cites) |
//!
//! `cites`/`cited_by` take an INSPIRE recid only: `refersto:arxiv:…` was
//! measured to match 677,185 records on 2026-10-19, i.e. nothing useful, so
//! any other id is an `unsupported` diagnostic naming what to do instead.
//! A clause the explain record does not cover is dropped, visibly.

use koine_sources::{Diagnostic, Query};

use crate::SOURCE;

/// A mapped query: the `q` string, or `None` when nothing could be mapped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapped {
    pub q: Option<String>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Quote a phrase for SPIRES: inner double quotes become spaces.
fn quote(term: &str) -> String {
    let cleaned: String = term
        .chars()
        .map(|c| if c == '"' { ' ' } else { c })
        .collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("\"{cleaned}\"")
}

fn words(term: &str) -> Vec<String> {
    term.split_whitespace()
        .map(|w| w.replace('"', ""))
        .filter(|w| !w.is_empty())
        .collect()
}

/// `field w1 and field w2`.
fn per_word(field: &str, term: &str) -> Option<String> {
    let parts: Vec<String> = words(term)
        .into_iter()
        .map(|w| format!("{field} {w}"))
        .collect();
    join(&parts, " and ")
}

fn join(parts: &[String], op: &str) -> Option<String> {
    match parts {
        [] => None,
        [one] => Some(one.clone()),
        many => Some(format!("({})", many.join(op))),
    }
}

fn years(relation: &str, term: &str) -> Option<String> {
    let ys: Vec<i32> = term
        .split(|c: char| c.is_whitespace() || c == '-' || c == ',')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<i32>())
        .collect::<Result<_, _>>()
        .ok()?;
    // INSPIRE's open-ended `date 2003->` matched nothing (measured
    // 2026-10-19), so the open end is a bound far past either side.
    match (relation, ys.as_slice()) {
        ("=", [y]) => Some(format!("date {y}")),
        ("<", [y]) => Some(format!("date 1000->{}", y - 1)),
        (">", [y]) => Some(format!("date {}->2999", y + 1)),
        ("within", [a, b]) => Some(format!("date {}->{}", a.min(b), a.max(b))),
        _ => None,
    }
}

fn recid(term: &str) -> Option<&str> {
    let t = term.trim();
    let t = t
        .strip_prefix("inspire:")
        .or_else(|| t.strip_prefix("recid:"))
        .unwrap_or(t);
    (!t.is_empty() && t.chars().all(|c| c.is_ascii_digit())).then_some(t)
}

fn arxiv_id(term: &str) -> String {
    let t = term.trim();
    let t = t
        .strip_prefix("arXiv:")
        .or_else(|| t.strip_prefix("arxiv:"))
        .unwrap_or(t);
    // Drop a version suffix: INSPIRE's eprint index holds the bare id.
    match t.rfind('v') {
        Some(i)
            if i > 0
                && !t[i + 1..].is_empty()
                && t[i + 1..].chars().all(|c| c.is_ascii_digit()) =>
        {
            t[..i].to_string()
        }
        _ => t.to_string(),
    }
}

fn clause(
    index: &str,
    relation: &str,
    term: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<String> {
    let term = term.trim();
    if term.is_empty() {
        return None;
    }
    let mapped = match (index, relation) {
        ("any", "any") => join(&words(term), " and "),
        ("title", "any") => per_word("t", term),
        ("title", "adj") => Some(format!("t {}", quote(term))),
        ("author", "any") if term.contains(',') => Some(format!("a {}", term.replace('"', ""))),
        ("author", "any") => per_word("a", term),
        ("first_author", "any") if term.contains(',') => {
            Some(format!("fa {}", term.replace('"', "")))
        }
        ("first_author", "any") => per_word("fa", term),
        ("container", "any") => Some(if term.contains(char::is_whitespace) {
            format!("j {}", quote(term))
        } else {
            format!("j {term}")
        }),
        ("keyword", "any") => Some(if term.contains(char::is_whitespace) {
            format!("k {}", quote(term))
        } else {
            format!("k {term}")
        }),
        ("doi", "=") => Some(format!("doi {term}")),
        ("arxiv", "=") => Some(format!("eprint {}", arxiv_id(term))),
        ("year", rel) => match years(rel, term) {
            Some(q) => Some(q),
            None => {
                diagnostics.push(Diagnostic::unsupported(
                    SOURCE,
                    format!("bib.year {rel} `{term}`: not a year (or two, for within)"),
                ));
                return None;
            }
        },
        ("cites", "=") | ("cited_by", "=") => match recid(term) {
            Some(n) => Some(if index == "cites" {
                format!("refersto:recid:{n}")
            } else {
                format!("citedby:recid:{n}")
            }),
            None => {
                diagnostics.push(Diagnostic::unsupported(
                    SOURCE,
                    format!(
                        "bib.{index} = `{term}`: INSPIRE walks citations by recid only (use `inspire:<recid>`; fetch the work first to learn it)"
                    ),
                ));
                return None;
            }
        },
        _ => {
            diagnostics.push(Diagnostic::unsupported(
                SOURCE,
                format!("bib.{index} {relation}"),
            ));
            return None;
        }
    };
    mapped
}

fn node(q: &Query, diagnostics: &mut Vec<Diagnostic>) -> Option<String> {
    match q {
        Query::And { and } => {
            let parts: Vec<String> = and.iter().filter_map(|c| node(c, diagnostics)).collect();
            join(&parts, " and ")
        }
        Query::Or { or } => {
            let parts: Vec<String> = or.iter().filter_map(|c| node(c, diagnostics)).collect();
            join(&parts, " or ")
        }
        Query::Not { not } => node(not, diagnostics).map(|inner| format!("not ({inner})")),
        Query::Native { native } => {
            let n = native.trim();
            (!n.is_empty()).then(|| format!("({n})"))
        }
        Query::Clause {
            index,
            relation,
            term,
        } => clause(index, relation, term, diagnostics),
    }
}

/// Drop one pair of parentheses that wraps the whole string.
fn strip_outer(s: &str) -> &str {
    if !s.starts_with('(') {
        return s;
    }
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return if i == s.len() - 1 { &s[1..i] } else { s };
                }
            }
            _ => {}
        }
    }
    s
}

/// Map `query` to a `q`. A top-level native query is passed through as is.
pub fn map(query: &Query) -> Mapped {
    let mut diagnostics = Vec::new();
    let q = match query {
        Query::Native { native } => {
            let n = native.trim();
            (!n.is_empty()).then(|| n.to_string())
        }
        other => node(other, &mut diagnostics).map(|s| strip_outer(&s).to_string()),
    };
    Mapped { q, diagnostics }
}

/// `sort` → INSPIRE's `sort` parameter.
pub fn sort_param(sort: &str) -> Option<&'static str> {
    match sort {
        "relevance" => Some("bestmatch"),
        "year" => Some("mostrecent"),
        "cited_by" => Some("mostcited"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(json: &str) -> Query {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn the_plan_example_keeps_its_tree() {
        let m = map(&q(r#"{"and": [
              {"index": "author", "relation": "any", "term": "Abel"},
              {"index": "year",   "relation": "within", "term": "2020 2024"},
              {"or": [{"index": "title", "relation": "any", "term": "reionization"},
                      {"index": "any",   "relation": "any", "term": "21cm"}]}
            ]}"#));
        assert!(m.diagnostics.is_empty());
        assert_eq!(
            m.q.as_deref(),
            Some("a Abel and date 2020->2024 and (t reionization or 21cm)")
        );
    }

    #[test]
    fn titles_authors_containers_keywords() {
        let m = map(&Query::And {
            and: vec![
                Query::clause("title", "adj", "first star"),
                Query::clause("author", "any", "Abel, T."),
                Query::clause("first_author", "any", "Tom Abel"),
                Query::clause("container", "any", "Mon. Not. Roy. Astron. Soc."),
                Query::clause("keyword", "any", "first stars"),
            ],
        });
        assert_eq!(
            m.q.unwrap(),
            "t \"first star\" and a Abel, T. and (fa Tom and fa Abel) and j \"Mon. Not. Roy. Astron. Soc.\" and k \"first stars\""
        );
    }

    #[test]
    fn year_relations_are_closed_ranges() {
        assert_eq!(
            map(&Query::clause("year", "=", "2002")).q.unwrap(),
            "date 2002"
        );
        assert_eq!(
            map(&Query::clause("year", "<", "2002")).q.unwrap(),
            "date 1000->2001"
        );
        assert_eq!(
            map(&Query::clause("year", ">", "2002")).q.unwrap(),
            "date 2003->2999"
        );
        assert_eq!(
            map(&Query::clause("year", "within", "2024-2020"))
                .q
                .unwrap(),
            "date 2020->2024"
        );
        let bad = map(&Query::clause("year", "=", "soon"));
        assert!(bad.q.is_none());
        assert_eq!(bad.diagnostics.len(), 1);
    }

    #[test]
    fn identifiers_and_citation_walks() {
        assert_eq!(
            map(&Query::clause("doi", "=", "10.1126/science.1063991"))
                .q
                .unwrap(),
            "doi 10.1126/science.1063991"
        );
        assert_eq!(
            map(&Query::clause("arxiv", "=", "arXiv:2101.00001v2"))
                .q
                .unwrap(),
            "eprint 2101.00001"
        );
        assert_eq!(
            map(&Query::clause("cites", "=", "inspire:728142"))
                .q
                .unwrap(),
            "refersto:recid:728142"
        );
        assert_eq!(
            map(&Query::clause("cited_by", "=", "728142")).q.unwrap(),
            "citedby:recid:728142"
        );
        let bad = map(&Query::clause("cites", "=", "doi:10.1/x"));
        assert!(bad.q.is_none());
        assert!(bad.diagnostics[0].detail.contains("recid"));
    }

    #[test]
    fn not_native_and_what_inspire_lacks() {
        let m = map(&q(r#"{"and": [
              {"index": "author", "relation": "any", "term": "Abel"},
              {"not": {"index": "title", "relation": "any", "term": "simulation"}}
            ]}"#));
        assert_eq!(m.q.unwrap(), "a Abel and not (t simulation)");
        assert_eq!(
            map(&Query::Native {
                native: "t \"dark matter\" and date 2020".into()
            })
            .q
            .unwrap(),
            "t \"dark matter\" and date 2020"
        );
        let m = map(&Query::And {
            and: vec![
                Query::clause("pmid", "=", "123"),
                Query::clause("abstract", "any", "x"),
                Query::clause("title", "any", "first"),
            ],
        });
        assert_eq!(m.q.unwrap(), "t first");
        assert_eq!(m.diagnostics.len(), 2);
        assert_eq!(m.diagnostics[0].detail, "bib.pmid =");
    }
}
