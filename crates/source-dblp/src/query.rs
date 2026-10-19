//! The canonical query (S-D2) mapped to DBLP's `search/publ/api?q=`.
//!
//! DBLP's query language is words: a space is AND, each word matches as a
//! prefix (`vasw` finds Vaswani), and the one text field the engine searches
//! holds authors, title, venue and year together. There is **no** field
//! restriction in the public API (no `author:`/`title:`/`venue:` form; checked
//! against the API documentation, not live: dblp.org answered every request
//! with a bot-check page while this adapter was written), so:
//!
//! | clause | `q` | diagnostic |
//! |---|---|---|
//! | `any` | its words | none |
//! | `title` / `author` / `container` | its words | `unsupported` — "sent as a free-text term; DBLP cannot restrict it to that field" |
//! | `year` `=` | the four digits | `unsupported` — same, a year is a word among words |
//!
//! `year` `<` `>` `within`, `or`, `not` and `native` have no counterpart
//! (`|` is DBLP's OR, but its precedence against the space is not documented,
//! so no branch is guessed): the clause is dropped with an `unsupported`
//! diagnostic. A restriction the engine cannot make is reported, never
//! implied.

use koine_sources::{Diagnostic, Query};

use crate::SOURCE;

/// What a query becomes: the `q` of `GET /search/publ/api` (None when
/// nothing mapped) and what could not be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapped {
    pub q: Option<String>,
    pub diagnostics: Vec<Diagnostic>,
}

fn words(term: &str) -> Vec<String> {
    term.split(|c: char| c.is_whitespace() || c == '"')
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

fn unrestricted(index: &str) -> Diagnostic {
    Diagnostic::unsupported(
        SOURCE,
        format!("bib.{index}: sent as a free-text term; DBLP's public API cannot restrict a term to that field"),
    )
}

fn walk(q: &Query, terms: &mut Vec<String>, diagnostics: &mut Vec<Diagnostic>) {
    match q {
        Query::And { and } => and.iter().for_each(|c| walk(c, terms, diagnostics)),
        Query::Or { .. } => diagnostics.push(Diagnostic::unsupported(
            SOURCE,
            "or: DBLP's OR (`|`) has no documented precedence; the branches were not sent",
        )),
        Query::Not { .. } => diagnostics.push(Diagnostic::unsupported(SOURCE, "not")),
        Query::Native { .. } => diagnostics.push(Diagnostic::unsupported(SOURCE, "native")),
        Query::Clause {
            index,
            relation,
            term,
        } => match (index.as_str(), relation.as_str()) {
            ("any", "any") => terms.extend(words(term)),
            ("title", "any" | "adj") | ("author", "any") | ("container", "any") => {
                terms.extend(words(term));
                diagnostics.push(unrestricted(index));
            }
            ("year", "=")
                if term.trim().len() == 4 && term.trim().chars().all(|c| c.is_ascii_digit()) =>
            {
                terms.push(term.trim().to_string());
                diagnostics.push(unrestricted(index));
            }
            ("year", rel) => diagnostics.push(Diagnostic::unsupported(
                SOURCE,
                format!(
                    "bib.year {rel} `{}`: only a four-digit year with `=`",
                    term.trim()
                ),
            )),
            (i, r) => diagnostics.push(Diagnostic::unsupported(SOURCE, format!("bib.{i} {r}"))),
        },
    }
}

/// Map `query`.
pub fn map(query: &Query) -> Mapped {
    let mut terms = Vec::new();
    let mut diagnostics = Vec::new();
    walk(query, &mut terms, &mut diagnostics);
    Mapped {
        q: (!terms.is_empty()).then(|| terms.join(" ")),
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_joined_and_restrictions_are_reported() {
        let m = map(&Query::And {
            and: vec![
                Query::clause("author", "any", "Vaswani"),
                Query::clause("title", "any", "attention is all"),
                Query::clause("container", "any", "NIPS"),
                Query::clause("year", "=", "2017"),
                Query::clause("any", "any", "transformer"),
            ],
        });
        let Mapped { q, diagnostics } = m;
        assert_eq!(
            q.as_deref(),
            Some("Vaswani attention is all NIPS 2017 transformer")
        );
        assert_eq!(diagnostics.len(), 4, "author, title, container, year");
        assert!(diagnostics
            .iter()
            .all(|d| d.detail.contains("cannot restrict")));
        assert_eq!(
            diagnostics[0].detail,
            "bib.author: sent as a free-text term; DBLP's public API cannot restrict a term to that field"
        );
    }

    #[test]
    fn what_dblp_cannot_express_is_dropped_visibly() {
        let Mapped { q, diagnostics } = map(&Query::And {
            and: vec![
                Query::clause("year", ">", "2000"),
                Query::Not {
                    not: Box::new(Query::clause("any", "any", "x")),
                },
                Query::Or {
                    or: vec![
                        Query::clause("any", "any", "a"),
                        Query::clause("any", "any", "b"),
                    ],
                },
                Query::Native { native: "x".into() },
                Query::clause("pmid", "=", "1"),
            ],
        });
        assert_eq!(q, None);
        assert_eq!(diagnostics.len(), 5);
        assert!(diagnostics[0].detail.starts_with("bib.year >"));
    }
}
