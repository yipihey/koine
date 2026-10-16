//! Tag query types for filtering publications by tag state.

use serde::{Deserialize, Serialize};

/// A query for filtering publications by tags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TagQuery {
    /// Publication has this tag (or any descendant via inheritance)
    Has(String),
    /// Publication does NOT have this tag
    Not(String),
    /// Both queries must match
    And(Box<TagQuery>, Box<TagQuery>),
    /// Either query must match
    Or(Box<TagQuery>, Box<TagQuery>),
}

/// Parse a tag query from a filter string.
///
/// Syntax:
/// - `tags:methods/hydro` — has tag (with inheritance)
/// - `-tags:methods/hydro` — does not have tag
/// - `tags:a+b` — has both tags (AND)
/// - `tags:a|b` — has either tag (OR)
///
/// Deliberately NOT `uniffi::export`ed. `TagQuery` is recursive through
/// `Box<TagQuery>`, and UniFFI has no lowering for `Box<T>`, so the attribute
/// that used to sit here never compiled. It went unnoticed because no CI gate
/// builds this crate with `--features ffi` — each Rust workflow scopes
/// clippy to one app's core crate.
///
/// Swift loses nothing: it has never called this, and parses tag filters itself
/// in `PublicationManagerCore/Search/LocalFilterService.swift`. To hand Swift
/// the Rust parser, the FFI-facing type has to stop being recursive (e.g. a flat
/// postfix `Vec<TagQueryOp>`); re-adding the attribute alone will not build.
pub fn parse_tag_query(input: &str) -> Option<TagQuery> {
    let input = input.trim();

    // Negated form
    if let Some(rest) = input.strip_prefix("-tags:") {
        let path = rest.trim();
        if path.is_empty() {
            return None;
        }
        return Some(TagQuery::Not(path.to_string()));
    }

    // Positive form
    if let Some(rest) = input.strip_prefix("tags:") {
        let rest = rest.trim();
        if rest.is_empty() {
            return None;
        }

        // Check for AND combinator
        if rest.contains('+') {
            let parts: Vec<&str> = rest.splitn(2, '+').collect();
            if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
                let left = TagQuery::Has(parts[0].trim().to_string());
                let right_str = format!("tags:{}", parts[1].trim());
                let right = parse_tag_query(&right_str)
                    .unwrap_or(TagQuery::Has(parts[1].trim().to_string()));
                return Some(TagQuery::And(Box::new(left), Box::new(right)));
            }
        }

        // Check for OR combinator
        if rest.contains('|') {
            let parts: Vec<&str> = rest.splitn(2, '|').collect();
            if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
                let left = TagQuery::Has(parts[0].trim().to_string());
                let right_str = format!("tags:{}", parts[1].trim());
                let right = parse_tag_query(&right_str)
                    .unwrap_or(TagQuery::Has(parts[1].trim().to_string()));
                return Some(TagQuery::Or(Box::new(left), Box::new(right)));
            }
        }

        return Some(TagQuery::Has(rest.to_string()));
    }

    None
}

impl TagQuery {
    /// Test whether a set of tag paths matches this query.
    ///
    /// The `tag_paths` should include all tags on a publication.
    /// The `has_tag_or_descendant` closure checks whether any tag in the set
    /// matches the path or is a descendant of it (for inheritance).
    pub fn matches(&self, tag_paths: &[String]) -> bool {
        match self {
            TagQuery::Has(path) => tag_paths
                .iter()
                .any(|t| t == path || t.starts_with(&format!("{}/", path))),
            TagQuery::Not(path) => !tag_paths
                .iter()
                .any(|t| t == path || t.starts_with(&format!("{}/", path))),
            TagQuery::And(a, b) => a.matches(tag_paths) && b.matches(tag_paths),
            TagQuery::Or(a, b) => a.matches(tag_paths) || b.matches(tag_paths),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_has() {
        assert_eq!(
            parse_tag_query("tags:methods/hydro"),
            Some(TagQuery::Has("methods/hydro".to_string()))
        );
    }

    #[test]
    fn parse_negated() {
        assert_eq!(
            parse_tag_query("-tags:methods"),
            Some(TagQuery::Not("methods".to_string()))
        );
    }

    #[test]
    fn parse_and() {
        let q = parse_tag_query("tags:methods+topics").unwrap();
        match q {
            TagQuery::And(a, b) => {
                assert_eq!(*a, TagQuery::Has("methods".to_string()));
                assert_eq!(*b, TagQuery::Has("topics".to_string()));
            }
            _ => panic!("Expected And query"),
        }
    }

    #[test]
    fn parse_or() {
        let q = parse_tag_query("tags:methods|topics").unwrap();
        match q {
            TagQuery::Or(a, b) => {
                assert_eq!(*a, TagQuery::Has("methods".to_string()));
                assert_eq!(*b, TagQuery::Has("topics".to_string()));
            }
            _ => panic!("Expected Or query"),
        }
    }

    #[test]
    fn query_matches_with_inheritance() {
        let tags = vec![
            "methods/sims/hydro".to_string(),
            "topics/galaxies".to_string(),
        ];

        assert!(TagQuery::Has("methods".to_string()).matches(&tags)); // inheritance
        assert!(TagQuery::Has("methods/sims".to_string()).matches(&tags)); // partial
        assert!(TagQuery::Has("methods/sims/hydro".to_string()).matches(&tags)); // exact
        assert!(!TagQuery::Has("methods/obs".to_string()).matches(&tags)); // no match
        assert!(TagQuery::Not("methods/obs".to_string()).matches(&tags)); // negated
    }
}
