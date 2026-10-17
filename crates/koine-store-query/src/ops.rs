//! The mixed-kind reads over the store port: grouped search and the edge
//! walk in both directions (ADR-0022 D6/D8; docs/plan-koine-kit-moves.md
//! KM-D9). No SQL: retrieval is [`ItemStore::search_text`] and incoming
//! edges are [`ItemStore::references_to`], which SQLite answers from its FTS
//! and edge indexes.

use std::collections::BTreeMap;

use impress_item::{
    searchable_text, text_tokens, AttributedStore, EdgeType, Item, ItemId, StoreError, Value,
};

/// Per-kind cap used when the caller passes `0`.
pub const DEFAULT_LIMIT_PER_SCHEMA: u32 = 20;

/// Hard ceiling on the per-kind cap. A search surface renders tens of rows per
/// bucket, not thousands, and an unbounded cap turns a typo into a full scan.
pub const MAX_LIMIT_PER_SCHEMA: u32 = 200;

/// Row cap for [`related_items`] when the caller passes `0`.
pub const DEFAULT_RELATED_LIMIT: u32 = 50;

/// Hard ceiling on the related-items cap. A Related section is a list a human
/// reads.
pub const MAX_RELATED_LIMIT: u32 = 500;

/// `direction` value for edges where the subject item is the edge SOURCE.
pub const DIRECTION_OUTGOING: &str = "outgoing";

/// `direction` value for edges where the subject item is the edge TARGET.
pub const DIRECTION_INCOMING: &str = "incoming";

/// The payload field a status lives in, and the one status search withholds
/// (koine-triage's convention).
pub const STATUS_FIELD: &str = "status";
pub const STATUS_DISMISSED: &str = "dismissed";

/// Words of context in a generated snippet.
const SNIPPET_WORDS: usize = 12;

/// One search result, kind-tagged so the caller can bucket by record kind.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    /// Lowercase UUID string.
    pub id: String,
    /// The item's `schema_ref`: the bucket key.
    pub schema_ref: String,
    /// Display title (title / subject / name, in that order).
    pub title: String,
    /// The words around the first match, or the title when the match has
    /// nothing quotable.
    pub snippet: String,
    /// Relevance, **lower is better** (the store's score negated, so SQLite's
    /// is its bm25); hits arrive already sorted.
    pub rank: f64,
}

/// One item related to the subject by a typed edge.
#[derive(Debug, Clone, PartialEq)]
pub struct RelatedItem {
    /// Lowercase UUID of the *other* item, never the subject.
    pub id: String,
    /// The other item's `schema_ref`.
    pub schema_ref: String,
    /// Display title (title / subject / name, in that order).
    pub title: String,
    /// Edge type name (`"Cites"`, `"Contains"`, …); a custom edge reports its
    /// bare custom name.
    pub edge_type: String,
    /// [`DIRECTION_OUTGOING`] or [`DIRECTION_INCOMING`].
    pub direction: String,
}

/// Display title: payload `title`, else `subject`, else `name`, else empty.
pub fn display_title(payload: &BTreeMap<String, Value>) -> String {
    for field in ["title", "subject", "name"] {
        if let Some(Value::String(s)) = payload.get(field) {
            if !s.is_empty() {
                return s.clone();
            }
        }
    }
    String::new()
}

fn is_dismissed(item: &Item) -> bool {
    matches!(item.payload.get(STATUS_FIELD), Some(Value::String(s)) if s == STATUS_DISMISSED)
}

/// Every query token is a prefix of some word of the item's text: the terms
/// are ANDed and prefix-matched ("scal" finds "Scaling").
fn matches_all(words: &[String], tokens: &[String]) -> bool {
    tokens
        .iter()
        .all(|t| words.iter().any(|w| w.starts_with(t.as_str())))
}

fn words_of(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The words around the first one a token prefixes, from the text fields.
fn snippet_of(text: &str, tokens: &[String]) -> Option<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let at = words.iter().position(|w| {
        let w = w.to_lowercase();
        let w: String = w.chars().filter(|c| c.is_alphanumeric()).collect();
        tokens.iter().any(|t| w.starts_with(t.as_str()))
    })?;
    let start = at.saturating_sub(SNIPPET_WORDS / 2);
    let end = (start + SNIPPET_WORDS).min(words.len());
    let mut out = words[start..end].join(" ");
    if start > 0 {
        out.insert(0, '…');
    }
    if end < words.len() {
        out.push('…');
    }
    Some(out)
}

/// Full-text search across every record kind, capped per kind.
///
/// Ordered by `schema_ref`, then relevance, then id, so buckets are contiguous
/// and the order is stable. `limit_per_schema` is clamped to
/// `1..=`[`MAX_LIMIT_PER_SCHEMA`]; `0` means [`DEFAULT_LIMIT_PER_SCHEMA`]. A
/// query with no searchable characters returns nothing rather than an error.
/// Items with `status: "dismissed"` are withheld.
pub fn search_all(
    store: &dyn AttributedStore,
    query: &str,
    limit_per_schema: u32,
) -> Result<Vec<SearchHit>, StoreError> {
    let tokens = text_tokens(query);
    if tokens.is_empty() {
        return Ok(vec![]);
    }
    let limit = match limit_per_schema {
        0 => DEFAULT_LIMIT_PER_SCHEMA,
        n => n.min(MAX_LIMIT_PER_SCHEMA),
    } as usize;
    // The store ranks by any token; the cap is per kind, so ask for every hit
    // and bucket here (a plain limit would let the noisiest kind fill it).
    let hits = store.search_text(None, query, usize::MAX)?;
    let mut buckets: BTreeMap<String, Vec<SearchHit>> = BTreeMap::new();
    for hit in hits {
        let Some(item) = store.get(hit.id)? else {
            continue;
        };
        if is_dismissed(&item) {
            continue;
        }
        let text = searchable_text(&item);
        let title = display_title(&item.payload);
        let words = words_of(&format!("{text} {title}"));
        if !matches_all(&words, &tokens) {
            continue;
        }
        let snippet = snippet_of(&text, &tokens)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| title.clone());
        let bucket = buckets.entry(item.schema.to_string()).or_default();
        bucket.push(SearchHit {
            id: item.id.to_string(),
            schema_ref: item.schema.to_string(),
            title,
            snippet,
            rank: -(hit.score as f64),
        });
    }
    let mut out = Vec::new();
    for (_, mut bucket) in buckets {
        bucket.sort_by(|a, b| {
            a.rank
                .partial_cmp(&b.rank)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
        bucket.truncate(limit);
        out.extend(bucket);
    }
    Ok(out)
}

/// An edge type's stored spelling (its serde JSON), which is what relations
/// are ordered by, and its display name.
fn edge_key(edge: &EdgeType) -> String {
    serde_json::to_string(edge).unwrap_or_default()
}

fn edge_label(edge: &EdgeType) -> String {
    match edge {
        EdgeType::Custom(name) => name.clone(),
        other => edge_key(other).trim_matches('"').to_string(),
    }
}

/// Every item related to `id`, both directions, all edge types.
///
/// Ordered by edge type, then title, then direction and id, so a section can
/// render one heading per edge type by walking once. Edges whose other end no
/// longer resolves are skipped. `limit` is clamped to
/// `1..=`[`MAX_RELATED_LIMIT`]; `0` means [`DEFAULT_RELATED_LIMIT`]. A
/// subject that does not exist is [`StoreError::NotFound`], so "no relations"
/// and "no such item" stay different answers.
pub fn related_items(
    store: &dyn AttributedStore,
    id: &str,
    limit: u32,
) -> Result<Vec<RelatedItem>, StoreError> {
    let subject: ItemId = id
        .parse()
        .map_err(|_| StoreError::Validation(format!("invalid UUID: {id}")))?;
    let Some(item) = store.get(subject)? else {
        return Err(StoreError::NotFound(subject));
    };
    let limit = match limit {
        0 => DEFAULT_RELATED_LIMIT,
        n => n.min(MAX_RELATED_LIMIT),
    } as usize;

    let mut rows: Vec<(String, RelatedItem)> = Vec::new();
    let mut push = |other: ItemId, edge: &EdgeType, direction: &str| -> Result<(), StoreError> {
        let Some(other_item) = store.get(other)? else {
            return Ok(());
        };
        rows.push((
            edge_key(edge),
            RelatedItem {
                id: other.to_string(),
                schema_ref: other_item.schema.to_string(),
                title: display_title(&other_item.payload),
                edge_type: edge_label(edge),
                direction: direction.to_string(),
            },
        ));
        Ok(())
    };
    for reference in &item.references {
        push(reference.target, &reference.edge_type, DIRECTION_OUTGOING)?;
    }
    for (source, edge) in store.references_to(subject, &[])? {
        push(source, &edge, DIRECTION_INCOMING)?;
    }
    rows.sort_by(|(ka, a), (kb, b)| {
        ka.cmp(kb)
            .then_with(|| a.title.cmp(&b.title))
            .then_with(|| a.direction.cmp(&b.direction))
            .then_with(|| a.id.cmp(&b.id))
    });
    rows.truncate(limit);
    Ok(rows.into_iter().map(|(_, r)| r).collect())
}
