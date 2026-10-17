//! The port's optional text search (`docs/plan-koine-kit-moves.md` KM-D3): the
//! types, and the default every store gets.
//!
//! The default is deliberately plain: narrow with a `Contains` query, score
//! the candidates by how many of the query's tokens their text fields hold.
//! A store with a real index (SQLite's FTS) overrides it and declares
//! [`StoreCapabilities::text_search_indexed`](crate::StoreCapabilities).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::item::{Item, ItemId, Value};
use crate::query::{ItemQuery, Predicate, SortDescriptor};
use crate::schema::SchemaRef;
use crate::store::{ItemStore, StoreError};

/// The payload fields whose text is searched: the ones SQLite's `items_fts`
/// indexes (`title`/`subject`, `author_text`/`from`, `abstract_text`, `note`,
/// `body_content`/`body`).
pub const TEXT_FIELDS: &[&str] = &[
    "title",
    "subject",
    "author_text",
    "from",
    "abstract_text",
    "note",
    "body_content",
    "body",
];

/// How many candidates the default narrows to before it scores them.
pub const TEXT_SEARCH_CANDIDATES: usize = 2000;

/// One ranked hit of [`ItemStore::search_text`]. A higher score is a better
/// match; scores of different stores are not comparable.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TextHit {
    pub id: ItemId,
    pub score: f32,
}

/// Lowercase alphanumeric tokens of at least two characters, in order, with
/// repeats removed.
pub fn text_tokens(text: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|t| t.chars().count() >= 2 && seen.insert(t.clone()))
        .collect()
}

/// The text an item offers to a search: its [`TEXT_FIELDS`] joined.
pub fn searchable_text(item: &Item) -> String {
    let mut out = String::new();
    for field in TEXT_FIELDS {
        if let Some(Value::String(s)) = item.payload.get(*field) {
            out.push_str(s);
            out.push(' ');
        }
    }
    out
}

/// The default [`ItemStore::search_text`].
pub(crate) fn default_search_text<S: ItemStore + ?Sized>(
    store: &S,
    schema: Option<&str>,
    text: &str,
    limit: usize,
) -> Result<Vec<TextHit>, StoreError> {
    let query_tokens = text_tokens(text);
    if query_tokens.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let narrowing = Predicate::Or(
        query_tokens
            .iter()
            .flat_map(|t| {
                TEXT_FIELDS
                    .iter()
                    .map(move |f| Predicate::Contains((*f).to_owned(), t.clone()))
            })
            .collect(),
    );
    let candidates = store.query(&ItemQuery {
        schema: schema.map(|s| SchemaRef::from_stored(s.to_owned())),
        predicates: vec![narrowing],
        sort: vec![SortDescriptor {
            field: "created".into(),
            ascending: false,
        }],
        limit: Some(TEXT_SEARCH_CANDIDATES),
        include_tags: false,
        include_references: false,
        ..Default::default()
    })?;

    let mut scored: Vec<(f32, usize, &Item)> = Vec::new();
    for item in &candidates {
        let lowered = searchable_text(item).to_lowercase();
        let words: Vec<&str> = lowered.split(|c: char| !c.is_alphanumeric()).collect();
        let mut present = 0usize;
        let mut occurrences = 0usize;
        for token in &query_tokens {
            // A token matches a word it prefixes, as SQLite's FTS (`"tok"*`) does.
            let n = words
                .iter()
                .filter(|w| w.starts_with(token.as_str()))
                .count();
            present += usize::from(n > 0);
            occurrences += n;
        }
        if present > 0 {
            scored.push((
                present as f32 / query_tokens.len() as f32,
                occurrences,
                item,
            ));
        }
    }
    scored.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then(b.1.cmp(&a.1))
            .then(b.2.created.cmp(&a.2.created))
            .then(a.2.id.cmp(&b.2.id))
    });
    Ok(scored
        .into_iter()
        .take(limit)
        .map(|(score, _, item)| TextHit { id: item.id, score })
        .collect())
}

/// A position in a store's arrival order ([`ItemStore::arrivals_after`]).
///
/// Opaque: persist it, compare it, hand it back. Cursors of two stores, or of
/// one store before and after it is rebuilt, mean nothing to each other.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct ArrivalCursor(u64);

impl ArrivalCursor {
    /// Before the first arrival.
    pub const START: ArrivalCursor = ArrivalCursor(0);

    /// The cursor as a number, for a caller that stores it.
    pub const fn to_u64(self) -> u64 {
        self.0
    }

    /// The cursor a stored number stands for. A store hands out cursors; this
    /// is only the other half of [`to_u64`](Self::to_u64).
    pub const fn from_u64(n: u64) -> Self {
        ArrivalCursor(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_lowercase_alphanumeric_two_plus_and_unique() {
        assert_eq!(
            text_tokens("Dark-Matter, a DARK matter: x9 é"),
            vec!["dark", "matter", "x9"]
        );
        assert!(text_tokens("a b - ((").is_empty());
    }

    #[test]
    fn cursors_order_and_round_trip() {
        assert!(ArrivalCursor::START < ArrivalCursor::from_u64(1));
        let c = ArrivalCursor::from_u64(41);
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, "41");
        assert_eq!(serde_json::from_str::<ArrivalCursor>(&json).unwrap(), c);
    }
}
