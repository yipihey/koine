//! The canonical query (S-D2) compiled to ADS's Lucene syntax, the boolean
//! tree kept (`AND`, `OR`, `NOT`): `any` → unfielded terms, `title` →
//! `title:`, `abstract` → `abs:`, `author` → `author:"…"`, `first_author` →
//! `first_author:"…"`, `year` → `year:2002` / `year:[* TO 2001]` /
//! `year:[2003 TO *]` / `year:[2000 TO 2003]`, `bibcode`/`doi` → exact
//! fields, `arxiv` → `identifier:"arXiv:…"`, `container` → `bibstem:` for
//! `=` (and `pub:"…"` for `any`), `keyword` → `keyword:"…"`, and the three
//! relation indexes through ADS's operators: `cites = X` → `citations(…)`
//! (the works that cite X), `cited_by = X` → `references(…)` (the works X
//! cites), `similar_to = X` → `similar(…)`, where X is a bibcode, a DOI or
//! an arXiv id. A `{native}` query is passed through untouched. A clause ADS
//! has no field for is an `unsupported` diagnostic and is dropped, visibly.

use koine_sources::{Diagnostic, Query};

use crate::SOURCE;

/// What a query becomes: the `q` parameter (empty when nothing mapped) and
/// what was dropped.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Mapped {
    pub q: String,
    pub diagnostics: Vec<Diagnostic>,
}

/// The kind (`bibcode`, `doi`, `arxiv`) and bare value of an identifier
/// term, with or without an `arXiv:` prefix.
pub fn id_of(term: &str) -> Option<(&'static str, String)> {
    let t = term.trim();
    let bare = t
        .strip_prefix("arXiv:")
        .or_else(|| t.strip_prefix("arxiv:"))
        .unwrap_or(t);
    if crate::is_bibcode(t) {
        Some(("bibcode", t.to_string()))
    } else if t.starts_with("10.") && t.contains('/') {
        Some(("doi", t.to_string()))
    } else if crate::is_arxiv(bare) {
        Some(("arxiv", bare.to_string()))
    } else {
        None
    }
}

/// `bibcode:"…"` / `doi:"…"` / `identifier:"arXiv:…"` for an identifier term.
pub fn id_clause(term: &str) -> Option<String> {
    let (kind, value) = id_of(term)?;
    Some(match kind {
        "bibcode" => format!("bibcode:\"{value}\""),
        "doi" => format!("doi:\"{value}\""),
        _ => format!("identifier:\"arXiv:{value}\""),
    })
}

fn quote(term: &str) -> String {
    format!(
        "\"{}\"",
        term.replace('"', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    )
}

fn words(term: &str) -> Vec<String> {
    term.replace('"', " ")
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// Words under `any` (OR), `all` (AND) or `adj` (phrase), optionally fielded.
fn text(field: Option<&str>, relation: &str, term: &str) -> Option<String> {
    let ws = words(term);
    if ws.is_empty() {
        return None;
    }
    let prefix = field.map(|f| format!("{f}:")).unwrap_or_default();
    Some(match (relation, ws.len()) {
        ("adj" | "exact", n) if n > 1 => format!("{prefix}{}", quote(term)),
        (_, 1) => format!("{prefix}{}", ws[0]),
        ("all", _) => format!("{prefix}({})", ws.join(" AND ")),
        _ => format!("{prefix}({})", ws.join(" OR ")),
    })
}

fn year(relation: &str, term: &str) -> Option<String> {
    let years: Vec<i32> = term
        .split(|c: char| c.is_whitespace() || c == '-' || c == ',')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<i32>())
        .collect::<Result<_, _>>()
        .ok()?;
    match (relation, years.as_slice()) {
        ("=", [y]) => Some(format!("year:{y}")),
        ("<", [y]) => Some(format!("year:[* TO {}]", y - 1)),
        (">", [y]) => Some(format!("year:[{} TO *]", y + 1)),
        ("within", [a, b]) => Some(format!("year:[{} TO {}]", a.min(b), a.max(b))),
        _ => None,
    }
}

struct Compiler {
    diagnostics: Vec<Diagnostic>,
}

impl Compiler {
    fn drop(&mut self, what: String) -> Option<String> {
        self.diagnostics.push(Diagnostic::unsupported(SOURCE, what));
        None
    }

    fn clause(&mut self, index: &str, relation: &str, term: &str) -> Option<String> {
        let t = term.trim();
        if t.is_empty() {
            return self.drop(format!("bib.{index} {relation}: empty term"));
        }
        let compiled = match (index, relation) {
            ("any", "any" | "all" | "adj") => text(None, relation, t),
            ("title", "any" | "all" | "adj") => text(Some("title"), relation, t),
            ("abstract", "any" | "all" | "adj") => text(Some("abs"), relation, t),
            ("author", "any" | "adj") => Some(format!("author:{}", quote(t))),
            ("first_author", "any" | "adj") => Some(format!("first_author:{}", quote(t))),
            ("keyword", "any" | "adj") => Some(format!("keyword:{}", quote(t))),
            ("container", "=") => Some(format!("bibstem:{}", quote(t))),
            ("container", "any") => Some(format!("pub:{}", quote(t))),
            ("year", rel) => match year(rel, t) {
                Some(y) => Some(y),
                None => {
                    return self.drop(format!(
                        "bib.year {rel} `{t}`: not a year (or two, for within)"
                    ))
                }
            },
            ("bibcode", "=") if crate::is_bibcode(t) => Some(format!("bibcode:\"{t}\"")),
            ("doi", "=") => Some(format!("doi:{}", quote(t))),
            ("arxiv", "=") => Some(format!(
                "identifier:\"arXiv:{}\"",
                t.trim_start_matches("arXiv:").trim_start_matches("arxiv:")
            )),
            ("cites" | "cited_by" | "similar_to", "=") => {
                let op = match index {
                    "cites" => "citations",
                    "cited_by" => "references",
                    _ => "similar",
                };
                match id_clause(t) {
                    Some(inner) => Some(format!("{op}({inner})")),
                    None => {
                        return self.drop(format!(
                            "bib.{index} = `{t}`: not a bibcode, DOI or arXiv id"
                        ))
                    }
                }
            }
            _ => return self.drop(format!("bib.{index} {relation}")),
        };
        if compiled.is_none() {
            return self.drop(format!("bib.{index} {relation}: empty term"));
        }
        compiled
    }

    fn node(&mut self, q: &Query) -> Option<String> {
        match q {
            Query::Clause {
                index,
                relation,
                term,
            } => self.clause(index, relation, term),
            Query::Native { native } => {
                let n = native.trim();
                (!n.is_empty()).then(|| format!("({n})"))
            }
            Query::And { and } => {
                let parts: Vec<String> = and.iter().filter_map(|c| self.node(c)).collect();
                join(parts, " AND ")
            }
            Query::Or { or } => {
                let parts: Vec<String> = or.iter().filter_map(|c| self.node(c)).collect();
                join(parts, " OR ")
            }
            Query::Not { not } => self.node(not).map(|n| format!("NOT ({n})")),
        }
    }
}

fn join(parts: Vec<String>, op: &str) -> Option<String> {
    match parts.len() {
        0 => None,
        1 => parts.into_iter().next(),
        _ => Some(format!("({})", parts.join(op))),
    }
}

/// Compile `query`.
pub fn map(query: &Query) -> Mapped {
    let mut c = Compiler {
        diagnostics: Vec::new(),
    };
    let mut q = c.node(query).unwrap_or_default();
    if q.starts_with("NOT ") {
        // Lucene has no pure-negative query: ADS answers an error for it.
        c.diagnostics.push(Diagnostic::unsupported(
            SOURCE,
            "bib.not: a query of only negations has nothing to subtract from",
        ));
        q.clear();
    }
    Mapped {
        q,
        diagnostics: c.diagnostics,
    }
}

/// `sort` → ADS's `sort`.
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
    fn the_tree_compiles_to_lucene() {
        let q: Query = serde_json::from_str(
            r#"{"and": [
              {"index": "author", "relation": "any", "term": "Abel, T"},
              {"index": "year", "relation": "within", "term": "2020 2024"},
              {"or": [{"index": "title", "relation": "adj", "term": "first star"},
                      {"index": "abstract", "relation": "all", "term": "pop iii"}]},
              {"not": {"index": "keyword", "relation": "any", "term": "dark matter"}},
              {"index": "container", "relation": "=", "term": "ApJ"}
            ]}"#,
        )
        .unwrap();
        let m = map(&q);
        assert!(m.diagnostics.is_empty(), "{:?}", m.diagnostics);
        assert_eq!(
            m.q,
            "(author:\"Abel, T\" AND year:[2020 TO 2024] AND (title:\"first star\" OR abs:(pop AND iii)) AND NOT (keyword:\"dark matter\") AND bibstem:\"ApJ\")"
        );
    }

    #[test]
    fn relations_and_identifiers_go_through_ads_operators() {
        for (index, term, expect) in [
            (
                "cites",
                "2002Sci...295...93A",
                "citations(bibcode:\"2002Sci...295...93A\")",
            ),
            (
                "cited_by",
                "10.1126/science.1063991",
                "references(doi:\"10.1126/science.1063991\")",
            ),
            (
                "similar_to",
                "arXiv:astro-ph/0112088",
                "similar(identifier:\"arXiv:astro-ph/0112088\")",
            ),
            ("arxiv", "1706.03762", "identifier:\"arXiv:1706.03762\""),
            (
                "doi",
                "10.1126/science.1063991",
                "doi:\"10.1126/science.1063991\"",
            ),
        ] {
            assert_eq!(map(&Query::clause(index, "=", term)).q, expect, "{index}");
        }
        let m = map(&Query::clause("cites", "=", "not an id"));
        assert_eq!(m.q, "");
        assert!(m.diagnostics[0]
            .detail
            .contains("not a bibcode, DOI or arXiv id"));
    }

    #[test]
    fn year_relations_and_native_passthrough() {
        assert_eq!(
            map(&Query::clause("year", "<", "2000")).q,
            "year:[* TO 1999]"
        );
        assert_eq!(
            map(&Query::clause("year", ">", "2000")).q,
            "year:[2001 TO *]"
        );
        assert_eq!(map(&Query::clause("year", "=", "2002")).q, "year:2002");
        let q = Query::And {
            and: vec![
                Query::Native {
                    native: "author:\"^Abel, T\" property:refereed".into(),
                },
                Query::clause("year", "=", "2002"),
            ],
        };
        assert_eq!(
            map(&q).q,
            "((author:\"^Abel, T\" property:refereed) AND year:2002)"
        );
        assert_eq!(
            map(&Query::Native {
                native: "title:foo".into()
            })
            .q,
            "(title:foo)"
        );
    }

    #[test]
    fn what_ads_has_no_field_for_is_a_diagnostic() {
        let m = map(&Query::And {
            and: vec![
                Query::clause("issn", "=", "0036-8075"),
                Query::clause("title", "any", "stars"),
                Query::clause("year", ">", "twenty"),
                Query::Not {
                    not: Box::new(Query::clause("title", "any", "x")),
                },
            ],
        });
        assert_eq!(m.q, "(title:stars AND NOT (title:x))");
        assert_eq!(m.diagnostics[0].detail, "bib.issn =");
        assert!(m.diagnostics[1].detail.starts_with("bib.year >"));
        let m = map(&Query::Not {
            not: Box::new(Query::clause("title", "any", "x")),
        });
        assert_eq!(m.q, "");
        assert_eq!(m.diagnostics.len(), 1);
    }
}
