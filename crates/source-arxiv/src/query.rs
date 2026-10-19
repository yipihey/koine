//! The canonical query (S-D2) compiled to arXiv's `search_query` (the
//! boolean tree is kept: `AND`, `OR`, `ANDNOT`): `any` → `all:`, `title` →
//! `ti:`, `abstract` → `abs:`, `author` → `au:`, `subject` → `cat:`, `year`
//! → `submittedDate:[…]`. `arxiv =` is not a field of the search: it goes
//! to `id_list`, which arXiv intersects with `search_query`, so it is
//! honoured only as a conjunct. A clause arXiv has no field for is an
//! `unsupported` diagnostic and is dropped, visibly.

use koine_sources::{Diagnostic, Query};

use crate::parse::{is_arxiv_id, versionless};
use crate::SOURCE;

/// What a query becomes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Mapped {
    /// The `search_query`; empty when only ids were named.
    pub search_query: String,
    /// The `id_list`.
    pub id_list: Vec<String>,
    pub diagnostics: Vec<Diagnostic>,
}

fn unsupported(diagnostics: &mut Vec<Diagnostic>, what: String) {
    diagnostics.push(Diagnostic::unsupported(SOURCE, what));
}

/// `field:word` terms for `term` under `relation`: `any` OR-joins the words,
/// `all` AND-joins them, `adj` is the quoted phrase.
fn text_terms(field: &str, relation: &str, term: &str) -> Option<String> {
    let cleaned = term.replace('"', " ");
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    if words.len() == 1 {
        return Some(format!("{field}:{}", words[0]));
    }
    Some(match relation {
        "adj" | "exact" => format!("{field}:\"{}\"", words.join(" ")),
        "all" => format!(
            "({})",
            words
                .iter()
                .map(|w| format!("{field}:{w}"))
                .collect::<Vec<_>>()
                .join(" AND ")
        ),
        _ => format!(
            "({})",
            words
                .iter()
                .map(|w| format!("{field}:{w}"))
                .collect::<Vec<_>>()
                .join(" OR ")
        ),
    })
}

fn year_range(relation: &str, term: &str) -> Option<String> {
    let years: Vec<i32> = term
        .split(|c: char| c.is_whitespace() || c == '-' || c == ',')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<i32>())
        .collect::<Result<_, _>>()
        .ok()?;
    let (lo, hi) = match (relation, years.as_slice()) {
        ("=", [y]) => (*y, *y),
        ("<", [y]) => (1900, y - 1),
        (">", [y]) => (y + 1, 2100),
        ("within", [a, b]) => (*a.min(b), *a.max(b)),
        _ => return None,
    };
    Some(format!(
        "submittedDate:[{lo:04}01010000 TO {hi:04}12312359]"
    ))
}

struct Compiler {
    ids: Vec<String>,
    diagnostics: Vec<Diagnostic>,
}

impl Compiler {
    fn clause(
        &mut self,
        index: &str,
        relation: &str,
        term: &str,
        conjunct: bool,
    ) -> Option<String> {
        let compiled = match (index, relation) {
            ("any", "any" | "all" | "adj") => text_terms("all", relation, term),
            ("title", "any" | "all" | "adj") => text_terms("ti", relation, term),
            ("abstract", "any" | "all" | "adj") => text_terms("abs", relation, term),
            ("author", "any" | "all" | "adj") => text_terms("au", relation, term),
            ("subject", "=") => Some(format!("cat:{}", term.trim())),
            ("year", rel) => match year_range(rel, term) {
                Some(r) => Some(r),
                None => {
                    unsupported(
                        &mut self.diagnostics,
                        format!("bib.year {rel} `{term}`: not a year (or two, for within)"),
                    );
                    return None;
                }
            },
            ("arxiv", "=") => {
                let id = term
                    .trim()
                    .trim_start_matches("arXiv:")
                    .trim_start_matches("arxiv:");
                if !conjunct {
                    unsupported(
                        &mut self.diagnostics,
                        "bib.arxiv =: arXiv takes ids as id_list, which only intersects, so an or/not of ids cannot be asked".into(),
                    );
                } else if is_arxiv_id(id) {
                    self.ids.push(id.to_string());
                } else {
                    unsupported(
                        &mut self.diagnostics,
                        format!("bib.arxiv = `{id}`: not an arXiv identifier"),
                    );
                }
                return None;
            }
            _ => {
                unsupported(&mut self.diagnostics, format!("bib.{index} {relation}"));
                return None;
            }
        };
        if compiled.is_none() {
            unsupported(
                &mut self.diagnostics,
                format!("bib.{index} {relation}: empty term"),
            );
        }
        compiled
    }

    fn node(&mut self, q: &Query, conjunct: bool) -> Option<String> {
        match q {
            Query::Clause {
                index,
                relation,
                term,
            } => self.clause(index, relation, term, conjunct),
            Query::Native { .. } => {
                unsupported(
                    &mut self.diagnostics,
                    "native: arXiv declares no native query dialect".into(),
                );
                None
            }
            Query::Or { or } => {
                let parts: Vec<String> = or.iter().filter_map(|c| self.node(c, false)).collect();
                join(parts, " OR ")
            }
            Query::Not { .. } => {
                unsupported(
                    &mut self.diagnostics,
                    "bib.not: arXiv has only ANDNOT, so a not must sit beside a clause in an and"
                        .into(),
                );
                None
            }
            Query::And { and } => {
                let mut positive = Vec::new();
                let mut negative = Vec::new();
                for child in and {
                    match child {
                        Query::Not { not } => {
                            if let Some(n) = self.node(not, false) {
                                negative.push(n);
                            }
                        }
                        other => {
                            if let Some(p) = self.node(other, conjunct) {
                                positive.push(p);
                            }
                        }
                    }
                }
                let base = join(positive, " AND ");
                match base {
                    Some(mut text) => {
                        for n in negative {
                            text = format!("{text} ANDNOT {}", paren(&n));
                        }
                        Some(text)
                    }
                    None => {
                        if !negative.is_empty() {
                            unsupported(
                                &mut self.diagnostics,
                                "bib.not: nothing positive for ANDNOT to subtract from".into(),
                            );
                        }
                        None
                    }
                }
            }
        }
    }
}

fn paren(s: &str) -> String {
    if s.starts_with('(') && s.ends_with(')') {
        s.to_string()
    } else {
        format!("({s})")
    }
}

fn join(parts: Vec<String>, op: &str) -> Option<String> {
    match parts.len() {
        0 => None,
        1 => parts.into_iter().next(),
        _ => Some(format!(
            "({})",
            parts
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join(op)
        )),
    }
}

/// Compile `query`.
pub fn map(query: &Query) -> Mapped {
    let mut c = Compiler {
        ids: Vec::new(),
        diagnostics: Vec::new(),
    };
    let text = c.node(query, true).unwrap_or_default();
    let mut ids: Vec<String> = Vec::new();
    for id in c.ids {
        if !ids.iter().any(|i| versionless(i) == versionless(&id)) {
            ids.push(id);
        }
    }
    Mapped {
        search_query: text,
        id_list: ids,
        diagnostics: c.diagnostics,
    }
}

/// `sort` → arXiv's `sortBy`.
pub fn sort_param(sort: &str) -> Option<&'static str> {
    match sort {
        "relevance" => Some("relevance"),
        "year" => Some("submittedDate"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tree_is_kept_and_relations_pick_the_operator() {
        let q: Query = serde_json::from_str(
            r#"{"and": [
              {"index": "author", "relation": "any", "term": "Abel"},
              {"index": "year", "relation": "within", "term": "2020 2024"},
              {"or": [{"index": "title", "relation": "adj", "term": "first star"},
                      {"index": "abstract", "relation": "all", "term": "pop iii"}]},
              {"not": {"index": "subject", "relation": "=", "term": "astro-ph.GA"}}
            ]}"#,
        )
        .unwrap();
        let m = map(&q);
        assert!(m.diagnostics.is_empty(), "{:?}", m.diagnostics);
        assert_eq!(
            m.search_query,
            "(au:Abel AND submittedDate:[202001010000 TO 202412312359] AND (ti:\"first star\" OR (abs:pop AND abs:iii))) ANDNOT (cat:astro-ph.GA)"
        );
    }

    #[test]
    fn any_is_or_all_is_and_and_ids_go_to_the_id_list() {
        let m = map(&Query::And {
            and: vec![
                Query::clause("any", "any", "first star"),
                Query::clause("arxiv", "=", "arXiv:astro-ph/0112088v1"),
            ],
        });
        assert_eq!(m.search_query, "(all:first OR all:star)");
        assert_eq!(m.id_list, ["astro-ph/0112088v1"]);
        let m = map(&Query::clause("arxiv", "=", "1706.03762"));
        assert_eq!(m.search_query, "");
        assert_eq!(m.id_list, ["1706.03762"]);
    }

    #[test]
    fn what_arxiv_has_no_field_for_is_a_diagnostic() {
        let m = map(&Query::And {
            and: vec![
                Query::clause("first_author", "any", "Abel"),
                Query::clause("doi", "=", "10.1/x"),
                Query::clause("title", "any", "stars"),
                Query::clause("year", ">", "twenty"),
                Query::Native { native: "x".into() },
            ],
        });
        assert_eq!(m.search_query, "ti:stars");
        let details: Vec<&str> = m.diagnostics.iter().map(|d| d.detail.as_str()).collect();
        assert_eq!(details[0], "bib.first_author any");
        assert_eq!(details[1], "bib.doi =");
        assert!(details[2].starts_with("bib.year >"));
        assert!(details[3].starts_with("native"));
        let m = map(&Query::Or {
            or: vec![Query::clause("arxiv", "=", "1706.03762")],
        });
        assert!(m.id_list.is_empty());
        assert_eq!(m.diagnostics.len(), 1);
    }
}
