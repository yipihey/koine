//! `StoreQueryService` — grouped global search, cross-kind relations, and
//! generic get/browse (ADR-0022 D6 / D8 / D5-G6).
//!
//! The reads that only make sense over the *whole* store. Everything else an
//! agent can ask for is scoped to one record kind and lives in that kind's app
//! service; these exist precisely because the answer is mixed-kind:
//!
//! * [`StoreQueryService::search_all`] — one FTS query over `items_fts`, which
//!   already indexes every kind, returning `schema_ref` on every hit so the
//!   caller buckets by kind (D6).
//! * [`StoreQueryService::related_items`] — the typed edge graph walked in both
//!   directions across all edge types, so a task can name the message that
//!   produced it and a figure can name the manuscript that embeds it (D8).
//! * [`StoreQueryService::get_item`] — the envelope of ONE item of any kind,
//!   plus its payload, so the thing a search hit named can actually be read
//!   without knowing which app owns it.
//! * [`StoreQueryService::list_items`] — a page of envelopes for one kind (or
//!   every kind), so the store can be *browsed* rather than only searched.
//!
//! The first two delegate to [`crate::ops`], over the store port. The last
//! two are envelope projections over `ItemStore::get` plus one ordering
//! query; no domain logic lives here: it converts arguments, maps rows to
//! DTOs, and turns errors into `ok: false` plus the kernel's own message.

use std::collections::BTreeMap;
use std::sync::Arc;

use impress_item::{AttributedStore, Item, ItemId, ItemQuery, SortDescriptor, StoreError, Value};
use impress_service_core::async_trait;
use impress_service_macros::{impress_service, impress_service_impl};
use serde::{Deserialize, Serialize};

#[allow(unused_imports)]
use impress_service_macros::impress_method;

use crate::ops::{self, RelatedItem, SearchHit};

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// One search hit, kind-tagged.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SearchHitDto {
    /// Lowercase UUID string.
    pub id: String,
    /// The item's schema — the bucket key ("manuscript", "figure",
    /// "email-message", "imbib/bibliography-entry", …).
    pub schema_ref: String,
    /// Display title (title / subject / name, in that order).
    pub title: String,
    /// Snippet of the best-matching indexed column; falls back to the title.
    pub snippet: String,
    /// BM25 relevance. **Lower is better** — hits arrive already sorted.
    pub rank: f64,
}

impl From<SearchHit> for SearchHitDto {
    fn from(h: SearchHit) -> Self {
        Self {
            id: h.id,
            schema_ref: h.schema_ref,
            title: h.title,
            snippet: h.snippet,
            rank: h.rank,
        }
    }
}

/// Result of a grouped search.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SearchResult {
    pub ok: bool,
    /// Hits ordered by kind, then relevance — so each kind's rows are
    /// contiguous and can be grouped by walking the list once.
    pub hits: Vec<SearchHitDto>,
    /// Distinct `schema_ref` values present in `hits`, in the order they
    /// appear. The kind buckets, named, so a caller does not have to derive
    /// them.
    pub kinds: Vec<String>,
    pub message: String,
}

/// One related item.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RelatedItemDto {
    /// Lowercase UUID of the OTHER item — never the one that was asked about.
    pub id: String,
    /// The other item's schema, so a mixed-kind list can render per entry.
    pub schema_ref: String,
    /// Display title (title / subject / name, in that order).
    pub title: String,
    /// Edge type name ("Cites", "Contains", "InResponseTo", "ProducedBy", …);
    /// a custom edge reports its bare custom name.
    pub edge_type: String,
    /// "outgoing" when the subject item is the edge SOURCE (this manuscript
    /// contains that figure), "incoming" when it is the TARGET.
    pub direction: String,
}

impl From<RelatedItem> for RelatedItemDto {
    fn from(r: RelatedItem) -> Self {
        Self {
            id: r.id,
            schema_ref: r.schema_ref,
            title: r.title,
            edge_type: r.edge_type,
            direction: r.direction,
        }
    }
}

/// Result of a relations walk.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RelatedResult {
    pub ok: bool,
    /// Related items ordered by edge type, then title — so one heading per
    /// edge type can be rendered by walking the list once.
    pub related: Vec<RelatedItemDto>,
    pub message: String,
}

/// The universal envelope every item carries, whatever its kind.
///
/// These fields exist on a publication, a manuscript, a figure, a message, a
/// task and an agent run alike — they live in the `items` table columns and
/// the tag join, not in the schema-specific payload. Everything kind-specific
/// is in the payload, which [`ItemResult::payload`] carries separately.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ItemEnvelopeDto {
    /// Lowercase UUID string.
    pub id: String,
    /// The record kind ("manuscript", "figure", "email-message",
    /// "imbib/bibliography-entry", …).
    pub schema_ref: String,
    /// Display title, resolved exactly the way the search index resolves it:
    /// payload `title`, else `subject`, else `name`, else empty.
    pub title: String,
    /// Payload `status` when the kind uses status-change semantics
    /// (`dismissed`, `archived`, a manuscript's `draft`/`submitted`, …).
    pub status: Option<String>,
    /// Flag colour, or null when unflagged — the same value
    /// `triage-service_set-flag` writes.
    pub flag: Option<String>,
    pub starred: bool,
    /// Slash-separated hierarchical tag paths, sorted.
    pub tags: Vec<String>,
    /// The ENVELOPE parent — the owning library/account/folder. **Not** the
    /// collection tree, which lives in payload `parent_id`; see
    /// `collection-service_tree`.
    pub parent_id: Option<String>,
    /// RFC 3339 / ISO 8601, UTC.
    pub created: String,
    /// RFC 3339 / ISO 8601, UTC.
    pub modified: String,
}

/// One item in full: envelope plus payload.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ItemResult {
    pub ok: bool,
    /// Null when the item does not exist or the id was malformed.
    pub item: Option<ItemEnvelopeDto>,
    /// The kind-specific payload, as a JSON **object string** — re-parse it to
    /// read fields. Empty when `ok` is false. Bounded: see `truncated`.
    pub payload: String,
    /// True when the payload was longer than
    /// [`MAX_PAYLOAD_BYTES`] and `payload` is therefore a PREFIX, not valid
    /// JSON. A manuscript's `body_content` is routinely megabytes.
    pub truncated: bool,
    /// Present only when something about the answer needs saying — today,
    /// that the payload was cut and where to get the rest.
    pub note: Option<String>,
    pub message: String,
}

/// A page of envelopes for store browsing.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ItemListResult {
    pub ok: bool,
    /// Envelopes only — no payloads. Ordered most-recently-modified first.
    pub items: Vec<ItemEnvelopeDto>,
    /// How many items match the requested kind in total, ignoring
    /// `limit`/`offset` — so a caller knows whether to page again.
    pub total: u32,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Mixed-kind reads over the whole shared store: search and relations.
///
/// Neither method is scoped to a record kind — that is the point. Use these to
/// find something when you do not know which app owns it, and to discover what
/// an item is connected to.
#[impress_service]
pub trait StoreQueryService: Send + Sync + 'static {
    /// Full-text search across EVERY record kind at once — publications,
    /// manuscripts, figures, messages, tasks, notes, agent runs.
    ///
    /// Each hit carries `schema_ref`, the record kind, and hits are ordered so
    /// that each kind's rows are contiguous; `kinds` lists the buckets in the
    /// order they appear. `limit_per_schema` caps each kind SEPARATELY, so one
    /// noisy mailbox cannot crowd every manuscript out of the answer — pass 0
    /// for the default (20); values above 200 are clamped.
    ///
    /// The query is treated as words, never as FTS5 syntax: operators,
    /// parentheses and stray quotes are searched for literally rather than
    /// parsed, and terms are prefix-matched ("scal" finds "Scaling"). A query
    /// with no letters or digits returns no hits rather than an error.
    ///
    /// Items with `status: "dismissed"` are deliberately withheld; everything
    /// else, archived included, stays findable.
    #[impress_method(effects(reads = [any("searches every kind")]), risk(private))]
    #[impress_example(
        name = "default",
        args = r#"{"query": "effects", "limit_per_schema": 3}"#
    )]
    async fn search_all(&self, query: String, limit_per_schema: i64) -> SearchResult;

    /// Everything connected to one item, in BOTH directions, across ALL edge
    /// types — the papers a manuscript cites, the messages that produced a
    /// task, the figures embedded in a draft, and each of those seen from the
    /// other end.
    ///
    /// `id` is a lowercase UUID string of any record kind. Each row names the
    /// OTHER item, its kind, the `edge_type`, and a `direction` of "outgoing"
    /// (the subject is the edge source) or "incoming" (the subject is the
    /// target). Rows are ordered by edge type then title. Edges whose other end
    /// no longer exists are skipped. Pass 0 for the default limit (50); values
    /// above 500 are clamped.
    #[impress_method(effects(reads = [target(id), any("walks references across kinds")]), risk(private))]
    #[impress_example(
        name = "missing-item",
        args = r#"{"id": "00000000-0000-0000-0000-000000000000", "limit": 10}"#,
        expect = r#"{"ok":false}"#
    )]
    async fn related_items(&self, id: String, limit: i64) -> RelatedResult;

    /// Read ONE item of ANY record kind: its universal envelope (title,
    /// status, flag, star, tags, envelope parent, created/modified as ISO
    /// 8601) plus its kind-specific payload.
    ///
    /// This is the follow-up to `search_all` and `related_items`, which name
    /// items by id but do not open them. It needs no knowledge of which app
    /// owns the record.
    ///
    /// The payload arrives as a JSON **object string** — parse it to read
    /// fields. It is capped at 32 KiB: a manuscript's `body_content` is
    /// routinely megabytes, and a tool result that large is unusable. When the
    /// cap bites, `truncated` is true, `payload` is a prefix and therefore NOT
    /// parseable, and `note` says so. Fetch the full text through the owning
    /// app's service (e.g. imprint's document tools) instead.
    ///
    /// An unknown id is `ok: false` with "not found", never an empty success.
    #[impress_method(risk(private))]
    #[impress_example(
        name = "missing-item",
        args = r#"{"id": "00000000-0000-0000-0000-000000000000"}"#,
        expect = r#"{"ok":false}"#
    )]
    async fn get_item(&self, id: String) -> ItemResult;

    /// Browse the store: a page of envelopes for one record kind, newest
    /// modification first.
    ///
    /// `schema_ref` is the exact kind string (`"manuscript"`, `"figure"`,
    /// `"email-message"`, `"imbib/bibliography-entry"`, …) — the same value
    /// `search_all` reports on every hit, and the `impress://store/schemas`
    /// resource lists every one that exists with its item count. Pass an empty
    /// string (or `"any"`) to walk EVERY kind at once.
    ///
    /// Payloads are deliberately omitted; call `get_item` for the ones that
    /// look interesting. Pass 0 for the default limit (50); values above 200
    /// are clamped and negatives are read as "unspecified". `total` reports
    /// how many items the kind has in all, so you know whether to page again.
    ///
    /// Nothing is withheld here — dismissed items included. Unlike a search,
    /// a browse that silently omits rows makes its own counts a lie.
    // D-P2, G7c: one of `pipeline_bench.rs`'s own hot cases. Generous on
    // purpose — see `imbib-service`'s `decode_latex` budget comment; the
    // breach mechanism is proven by a dedicated test, not by this one
    // tripping under Tier A's cold first call.
    #[impress_method(effects(reads = [any("the schema_ref argument names the kind")]),
        budget_ms = 200, risk(private))]
    #[impress_example(
        name = "default",
        args = r#"{"schema_ref": "manuscript", "limit": 5, "offset": 0}"#
    )]
    async fn list_items(&self, schema_ref: String, limit: i64, offset: i64) -> ItemListResult;
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

/// Store-backed `StoreQueryService`. `new()` uses the shared store (opened
/// lazily); `with_store` takes an explicit one, as the tests do.
#[derive(Clone, Default)]
pub struct DefaultStoreQueryService {
    store: Option<Arc<dyn AttributedStore>>,
}

impl DefaultStoreQueryService {
    pub fn new() -> Self {
        Self { store: None }
    }

    pub fn with_store(store: Arc<dyn AttributedStore>) -> Self {
        Self { store: Some(store) }
    }

    fn store(&self) -> Arc<dyn AttributedStore> {
        self.store
            .clone()
            .unwrap_or_else(|| koine_store::process::acquire_or_stand_in().0)
    }
}

/// Kernel errors carry the diagnosis; the tool surface just must not swallow
/// them.
fn describe(err: StoreError) -> String {
    err.to_string()
}

/// A tool argument arrives as a signed integer; the kernel takes an unsigned
/// cap where 0 means "the default". Anything negative is a caller mistake that
/// should behave like "unspecified", not like an error dialog.
fn clamp_limit(limit: i64) -> u32 {
    limit.clamp(0, u32::MAX as i64) as u32
}

/// How much payload JSON `get_item` will return inline before cutting it.
///
/// 32 KiB is roughly ten pages of prose: enough that ordinary records
/// (publications, tasks, figures, messages) always arrive whole, small enough
/// that a manuscript with its `body_content` cannot blow up a tool result.
pub const MAX_PAYLOAD_BYTES: usize = 32 * 1024;

/// Page size `list_items` uses when the caller passes 0.
pub const DEFAULT_LIST_LIMIT: i64 = 50;

/// Hard ceiling on a browse page. A browse surface shows tens of rows, and an
/// unbounded page turns a typo into a full table scan rendered into a prompt.
pub const MAX_LIST_LIMIT: i64 = 200;

use crate::ops::display_title;

/// The payload `status` string, if the item carries one.
fn payload_status(payload: &BTreeMap<String, Value>) -> Option<String> {
    match payload.get(ops::STATUS_FIELD) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// Project the universal envelope. Everything here is a column on `items` (or
/// the tag join), which is why one projection serves every record kind.
fn envelope_of(item: &Item) -> ItemEnvelopeDto {
    ItemEnvelopeDto {
        id: item.id.to_string(),
        schema_ref: item.schema.to_string(),
        title: display_title(&item.payload),
        status: payload_status(&item.payload),
        flag: item.flag.as_ref().map(|f| f.color.clone()),
        starred: item.is_starred,
        tags: item.tags.clone(),
        parent_id: item.parent.map(|p| p.to_string()),
        created: item.created.to_rfc3339(),
        modified: item.modified.to_rfc3339(),
    }
}

/// The payload as JSON, cut to [`MAX_PAYLOAD_BYTES`] if it is enormous.
///
/// The cut lands on a UTF-8 character boundary — a byte-exact truncation would
/// produce a string that is not merely unparseable but invalid, and the note
/// has to be readable to be useful.
fn bounded_payload(payload: &BTreeMap<String, Value>) -> (String, bool, Option<String>) {
    let json = serde_json::to_string(payload)
        .unwrap_or_else(|e| format!("{{\"_serialization_error\":\"{e}\"}}"));
    if json.len() <= MAX_PAYLOAD_BYTES {
        return (json, false, None);
    }
    let full = json.len();
    let mut cut = MAX_PAYLOAD_BYTES;
    while cut > 0 && !json.is_char_boundary(cut) {
        cut -= 1;
    }
    let note = format!(
        "Payload truncated to {cut} of {full} bytes, so this is a PREFIX and will not parse as \
         JSON. Long fields (a manuscript's body_content, a message's body) are the usual cause — \
         read those through the owning app's service."
    );
    (json[..cut].to_string(), true, Some(note))
}

/// Clamp a browse page size: 0 (or anything negative, which is a caller
/// mistake that should behave like "unspecified") means the default.
fn list_limit(limit: i64) -> i64 {
    match limit {
        n if n <= 0 => DEFAULT_LIST_LIMIT,
        n => n.min(MAX_LIST_LIMIT),
    }
}

/// `""`, `"any"` and `"*"` all mean "every record kind" — the three things a
/// caller plausibly types for it.
fn is_every_kind(schema_ref: &str) -> bool {
    let s = schema_ref.trim();
    s.is_empty() || s == "*" || s.eq_ignore_ascii_case("any")
}

/// The distinct kinds present, in result order. Cheap because hits are already
/// grouped by `schema_ref`.
fn kinds_of(hits: &[SearchHitDto]) -> Vec<String> {
    let mut kinds: Vec<String> = Vec::new();
    for hit in hits {
        if kinds.last().map(String::as_str) != Some(hit.schema_ref.as_str())
            && !kinds.contains(&hit.schema_ref)
        {
            kinds.push(hit.schema_ref.clone());
        }
    }
    kinds
}

#[async_trait::async_trait]
impl StoreQueryService for DefaultStoreQueryService {
    async fn search_all(&self, query: String, limit_per_schema: i64) -> SearchResult {
        match ops::search_all(self.store().as_ref(), &query, clamp_limit(limit_per_schema)) {
            Ok(hits) => {
                let hits: Vec<SearchHitDto> = hits.into_iter().map(SearchHitDto::from).collect();
                let kinds = kinds_of(&hits);
                SearchResult {
                    ok: true,
                    message: format!("{} hit(s) across {} kind(s).", hits.len(), kinds.len()),
                    hits,
                    kinds,
                }
            }
            Err(e) => SearchResult {
                ok: false,
                hits: vec![],
                kinds: vec![],
                message: describe(e),
            },
        }
    }

    async fn related_items(&self, id: String, limit: i64) -> RelatedResult {
        match ops::related_items(self.store().as_ref(), &id, clamp_limit(limit)) {
            Ok(rows) => RelatedResult {
                ok: true,
                message: format!("{} related item(s) for {id}.", rows.len()),
                related: rows.into_iter().map(RelatedItemDto::from).collect(),
            },
            Err(e) => RelatedResult {
                ok: false,
                related: vec![],
                message: describe(e),
            },
        }
    }

    async fn get_item(&self, id: String) -> ItemResult {
        let not_found = |message: String| ItemResult {
            ok: false,
            item: None,
            payload: String::new(),
            truncated: false,
            note: None,
            message,
        };

        let Ok(uuid) = ItemId::parse_str(&id) else {
            return not_found(describe(StoreError::Validation(format!(
                "invalid UUID: {id}"
            ))));
        };
        match self.store().get(uuid) {
            Ok(Some(item)) => {
                let (payload, truncated, note) = bounded_payload(&item.payload);
                let envelope = envelope_of(&item);
                ItemResult {
                    ok: true,
                    message: format!("{} {id}.", envelope.schema_ref),
                    item: Some(envelope),
                    payload,
                    truncated,
                    note,
                }
            }
            Ok(None) => not_found(describe(StoreError::NotFound(uuid))),
            Err(e) => not_found(describe(e)),
        }
    }

    async fn list_items(&self, schema_ref: String, limit: i64, offset: i64) -> ItemListResult {
        let store = self.store();
        let every_kind = is_every_kind(&schema_ref);
        let schema = schema_ref.trim().to_string();
        let limit = list_limit(limit);
        let offset = offset.max(0);
        let failed = |e: StoreError| ItemListResult {
            ok: false,
            items: vec![],
            total: 0,
            message: describe(e),
        };

        // Newest first, ties by id: `modified` is kept to the millisecond, so
        // without the id two items written in one tick would page
        // nondeterministically.
        let query = ItemQuery {
            schema: (!every_kind).then(|| impress_item::SchemaRef::from_stored(schema.clone())),
            sort: vec![
                SortDescriptor {
                    field: "modified".into(),
                    ascending: false,
                },
                SortDescriptor {
                    field: "id".into(),
                    ascending: true,
                },
            ],
            limit: Some(limit as usize),
            offset: Some(offset as usize),
            include_tags: false,
            include_references: false,
            ..Default::default()
        };
        let ordered_ids: Vec<String> = match store.query(&query) {
            Ok(rows) => rows.into_iter().map(|i| i.id.to_string()).collect(),
            Err(e) => return failed(e),
        };
        let total = match store.count(&ItemQuery {
            schema: query.schema.clone(),
            ..Default::default()
        }) {
            Ok(n) => n as u32,
            Err(e) => return failed(e),
        };

        let mut items = Vec::with_capacity(ordered_ids.len());
        for raw in &ordered_ids {
            let Ok(uuid) = ItemId::parse_str(raw) else {
                continue;
            };
            match store.get(uuid) {
                // A row that vanished between the ordering query and the read
                // is a concurrent delete, not a failure of this call.
                Ok(Some(item)) => items.push(envelope_of(&item)),
                Ok(None) => continue,
                Err(e) => return failed(e),
            }
        }

        let kind = if every_kind {
            "every kind".to_string()
        } else {
            schema
        };
        ItemListResult {
            ok: true,
            message: format!(
                "{} of {total} item(s) of {kind}, offset {offset}.",
                items.len()
            ),
            items,
            total,
        }
    }
}

impress_service_impl! {
    service = StoreQueryService,
    safety = read_only,
    since = "0.1.0",
    effects = {
        reads: [target(id)],
        writes: [],
        reach: [],
    },
    impl = DefaultStoreQueryService,
    instance = DefaultStoreQueryService::new,
    methods = [
        search_all(
            /// The search text, treated as prefix-matched words, never FTS5
            /// syntax.
            query: String,
            /// Cap per record kind; 0 for the default (20), clamped above 200.
            limit_per_schema: i64,
        ) -> SearchResult,
        related_items(
            /// The item's id, a lowercase UUID string, of any record kind.
            id: String,
            /// Cap on the number of edges returned; 0 for the default (50),
            /// clamped above 500.
            limit: i64,
        ) -> RelatedResult,
        get_item(
            /// The item's id, a lowercase UUID string, of any record kind.
            id: String,
        ) -> ItemResult,
        list_items(
            /// The exact kind string (`"manuscript"`, `"imbib/bibliography-entry"`,
            /// …); empty string or `"any"` walks every kind at once.
            schema_ref: String,
            /// Page size; 0 for the default (50), clamped above 200.
            limit: i64,
            /// Rows to skip before the page starts.
            offset: i64,
        ) -> ItemListResult,
    ],
}

#[cfg(test)]
mod tests {
    use super::*;
    use impress_item::{ActorKind, EdgeType, FieldMutation, Priority, TypedReference, Visibility};

    fn test_store() -> Arc<dyn AttributedStore> {
        Arc::new(koine_store_memory::MemoryStore::new())
    }

    fn make_item(store: &dyn AttributedStore, schema: &str) -> String {
        make_item_named(store, schema, "Untitled")
    }

    /// An item of `schema` (historical opaque refs such as `task` and
    /// `figure` included) with a `title` payload field.
    fn make_item_named(store: &dyn AttributedStore, schema: &str, title: &str) -> String {
        let now = chrono::Utc::now();
        let mut payload: BTreeMap<String, Value> = BTreeMap::new();
        payload.insert("title".into(), Value::String(title.into()));
        let item = Item {
            id: uuid::Uuid::new_v4(),
            schema: serde_json::from_value(serde_json::Value::String(schema.to_owned()))
                .expect("fixture schema decodes"),
            payload,
            created: now,
            modified: now,
            author: "test".into(),
            author_kind: ActorKind::Human,
            logical_clock: 0,
            origin: None,
            canonical_id: None,
            tags: vec![],
            flag: None,
            is_read: false,
            is_starred: false,
            priority: Priority::Normal,
            visibility: Visibility::Private,
            message_type: None,
            produced_by: None,
            version: None,
            batch_id: None,
            references: vec![],
            parent: None,
            extra: Default::default(),
        };
        store.insert(item).expect("insert item").to_string()
    }

    fn svc() -> (Arc<dyn AttributedStore>, DefaultStoreQueryService) {
        let store = test_store();
        let service = DefaultStoreQueryService::with_store(store.clone());
        (store, service)
    }

    fn link(store: &dyn AttributedStore, source: &str, target: &str, edge: EdgeType) {
        store
            .update(
                uuid::Uuid::parse_str(source).unwrap(),
                vec![FieldMutation::AddReference(TypedReference {
                    target: uuid::Uuid::parse_str(target).unwrap(),
                    edge_type: edge,
                    metadata: None,
                })],
            )
            .expect("add reference");
    }

    fn dismiss(store: &dyn AttributedStore, id: &str) {
        store
            .update(
                uuid::Uuid::parse_str(id).unwrap(),
                vec![FieldMutation::SetPayload(
                    "status".into(),
                    Value::String("dismissed".into()),
                )],
            )
            .expect("dismiss");
    }

    // ─── search_all ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn search_all_returns_kind_tagged_hits_and_names_the_buckets() {
        let (store, s) = svc();
        make_item_named(&*store, "manuscript", "Nucleosynthesis in dwarfs");
        make_item_named(&*store, "figure", "Nucleosynthesis yields");
        make_item_named(&*store, "task", "Rewrite the nucleosynthesis section");
        make_item_named(&*store, "manuscript", "Something else entirely");

        let out = s.search_all("nucleosynthesis".into(), 10).await;
        assert!(out.ok, "{}", out.message);
        assert_eq!(out.hits.len(), 3, "{:?}", out.hits);
        assert_eq!(
            out.kinds,
            vec!["figure", "manuscript", "task"],
            "kinds are named in result order"
        );
        assert!(out.hits.iter().all(|h| !h.snippet.is_empty()));
        assert!(out.message.contains("3 hit(s)"), "{}", out.message);
    }

    #[tokio::test]
    async fn search_all_caps_each_kind_separately() {
        let (store, s) = svc();
        for n in 0..12 {
            make_item_named(&*store, "email-message", &format!("budget thread {n}"));
        }
        let manuscript = make_item_named(&*store, "manuscript", "Budget narrative");

        let out = s.search_all("budget".into(), 2).await;
        assert!(out.ok, "{}", out.message);
        assert_eq!(
            out.hits
                .iter()
                .filter(|h| h.schema_ref == "email-message")
                .count(),
            2,
            "the per-kind cap must bind: {:?}",
            out.hits
        );
        assert!(
            out.hits.iter().any(|h| h.id == manuscript),
            "the manuscript must survive 12 messages"
        );
    }

    #[tokio::test]
    async fn search_all_treats_the_query_as_words_not_syntax() {
        let (store, s) = svc();
        make_item_named(&*store, "manuscript", "Dark matter halos");

        for hostile in [
            "foo AND (",
            "\"unbalanced",
            "NEAR(",
            "*",
            ")))",
            "'; DROP TABLE items; --",
        ] {
            let out = s.search_all(hostile.into(), 5).await;
            assert!(out.ok, "{hostile:?} must not fail: {}", out.message);
        }

        // Empty input is an empty answer, not an error.
        let empty = s.search_all("   ".into(), 5).await;
        assert!(empty.ok);
        assert!(empty.hits.is_empty());
        assert!(empty.kinds.is_empty());

        // A real query still works.
        let real = s.search_all("dark".into(), 5).await;
        assert_eq!(real.hits.len(), 1, "{:?}", real.hits);
        assert_eq!(real.hits[0].title, "Dark matter halos");
    }

    #[tokio::test]
    async fn search_all_withholds_dismissed_items_only() {
        let (store, s) = svc();
        let live = make_item_named(&*store, "task", "Recalibrate the detector");
        let gone = make_item_named(&*store, "task", "Recalibrate someday");
        dismiss(&*store, &gone);

        let out = s.search_all("recalibrate".into(), 10).await;
        let ids: Vec<&str> = out.hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, vec![live.as_str()], "{:?}", out.hits);
    }

    #[tokio::test]
    async fn search_all_accepts_zero_and_negative_limits() {
        let (store, s) = svc();
        make_item_named(&*store, "manuscript", "Widget study");
        for limit in [0i64, -1, i64::MIN, i64::MAX] {
            let out = s.search_all("widget".into(), limit).await;
            assert!(out.ok, "limit {limit}: {}", out.message);
            assert_eq!(out.hits.len(), 1, "limit {limit}");
        }
    }

    // ─── related_items ───────────────────────────────────────────────────

    #[tokio::test]
    async fn related_items_walks_both_directions_across_kinds() {
        let (store, s) = svc();
        let manuscript = make_item_named(&*store, "manuscript", "Draft II");
        let figure = make_item_named(&*store, "figure", "Rotation curve");
        let paper = make_item_named(&*store, "imbib/bibliography-entry", "Zwicky 1933");
        let task = make_item_named(&*store, "task", "Redo the fit");
        let message = make_item_named(&*store, "email-message", "Re: Draft II");

        link(&*store, &manuscript, &figure, EdgeType::Contains);
        link(&*store, &manuscript, &paper, EdgeType::Cites);
        link(&*store, &task, &manuscript, EdgeType::ProducedBy);
        link(&*store, &message, &manuscript, EdgeType::InResponseTo);

        let out = s.related_items(manuscript.clone(), 10).await;
        assert!(out.ok, "{}", out.message);
        assert_eq!(out.related.len(), 4, "{:?}", out.related);

        let contains = out
            .related
            .iter()
            .find(|r| r.edge_type == "Contains")
            .unwrap();
        assert_eq!(contains.id, figure);
        assert_eq!(contains.schema_ref, "figure");
        assert_eq!(contains.direction, "outgoing");

        let produced = out
            .related
            .iter()
            .find(|r| r.edge_type == "ProducedBy")
            .unwrap();
        assert_eq!(produced.id, task);
        assert_eq!(produced.direction, "incoming");

        // The other end sees the mirror image.
        let back = s.related_items(figure, 10).await;
        assert_eq!(back.related.len(), 1);
        assert_eq!(back.related[0].id, manuscript);
        assert_eq!(back.related[0].direction, "incoming");
        assert_eq!(back.related[0].title, "Draft II");
    }

    #[tokio::test]
    async fn related_items_skips_deleted_ends() {
        let (store, s) = svc();
        let manuscript = make_item_named(&*store, "manuscript", "Draft II");
        let kept = make_item_named(&*store, "figure", "Panel A");
        let doomed = make_item_named(&*store, "figure", "Panel B");
        link(&*store, &manuscript, &kept, EdgeType::Contains);
        link(&*store, &manuscript, &doomed, EdgeType::Contains);
        assert_eq!(
            s.related_items(manuscript.clone(), 10).await.related.len(),
            2
        );

        store
            .delete(uuid::Uuid::parse_str(&doomed).unwrap())
            .unwrap();
        let out = s.related_items(manuscript, 10).await;
        assert_eq!(out.related.len(), 1, "{:?}", out.related);
        assert_eq!(out.related[0].id, kept);
    }

    #[tokio::test]
    async fn related_items_fails_loudly_on_bad_ids() {
        let (store, s) = svc();
        let malformed = s.related_items("not-a-uuid".into(), 10).await;
        assert!(!malformed.ok);
        assert!(
            malformed.message.contains("invalid UUID"),
            "{}",
            malformed.message
        );

        let missing = s.related_items(uuid::Uuid::new_v4().to_string(), 10).await;
        assert!(!missing.ok);
        assert!(missing.message.contains("not found"), "{}", missing.message);

        // An item with no edges is an empty list, not a failure — the two
        // answers must be distinguishable.
        let lonely = make_item_named(&*store, "manuscript", "Untouched");
        let empty = s.related_items(lonely, 10).await;
        assert!(empty.ok);
        assert!(empty.related.is_empty());
    }

    // ─── get_item ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn get_item_returns_the_whole_envelope_and_the_payload() {
        let (store, s) = svc();
        let id = make_item_named(&*store, "manuscript", "Draft II");
        let uuid = uuid::Uuid::parse_str(&id).unwrap();
        let parent = make_item_named(&*store, "manuscript-collection", "Chapters");
        store
            .update(
                uuid,
                vec![
                    FieldMutation::SetStarred(true),
                    FieldMutation::SetFlag(Some(impress_item::FlagState {
                        color: "amber".into(),
                        style: None,
                        length: None,
                    })),
                    FieldMutation::AddTag("writing/active".into()),
                    FieldMutation::SetPayload(
                        "status".into(),
                        Value::String("internal-review".into()),
                    ),
                    FieldMutation::SetPayload("word_count".into(), Value::Int(4200)),
                    FieldMutation::SetParent(Some(uuid::Uuid::parse_str(&parent).unwrap())),
                ],
            )
            .expect("triage the manuscript");

        let out = s.get_item(id.clone()).await;
        assert!(out.ok, "{}", out.message);
        let item = out.item.expect("an envelope");
        assert_eq!(item.id, id);
        assert_eq!(item.schema_ref, "manuscript");
        assert_eq!(item.title, "Draft II");
        assert_eq!(item.status.as_deref(), Some("internal-review"));
        assert_eq!(item.flag.as_deref(), Some("amber"));
        assert!(item.starred);
        assert_eq!(item.tags, vec!["writing/active"]);
        assert_eq!(item.parent_id.as_deref(), Some(parent.as_str()));
        // ISO 8601 / RFC 3339, and both stamps parse.
        for stamp in [&item.created, &item.modified] {
            chrono::DateTime::parse_from_rfc3339(stamp)
                .unwrap_or_else(|e| panic!("{stamp} is not ISO 8601: {e}"));
        }

        // The payload arrives whole and re-parses, which is the contract that
        // makes it worth shipping as a string at all.
        assert!(!out.truncated);
        assert!(out.note.is_none());
        let payload: serde_json::Value =
            serde_json::from_str(&out.payload).expect("payload parses");
        assert_eq!(payload["title"], "Draft II");
        assert_eq!(payload["word_count"], 4200);
    }

    #[tokio::test]
    async fn get_item_falls_back_through_subject_and_name_for_the_title() {
        let (store, s) = svc();
        let with_subject = make_item(&*store, "email-message");
        store
            .update(
                uuid::Uuid::parse_str(&with_subject).unwrap(),
                vec![
                    FieldMutation::RemovePayload("title".into()),
                    FieldMutation::SetPayload("subject".into(), Value::String("Re: budget".into())),
                ],
            )
            .unwrap();
        assert_eq!(
            s.get_item(with_subject).await.item.unwrap().title,
            "Re: budget",
            "subject is the mail title, exactly as the search index reads it"
        );

        let untitled = make_item(&*store, "figure");
        store
            .update(
                uuid::Uuid::parse_str(&untitled).unwrap(),
                vec![FieldMutation::RemovePayload("title".into())],
            )
            .unwrap();
        let out = s.get_item(untitled).await;
        assert!(
            out.ok,
            "an untitled item is still readable: {}",
            out.message
        );
        assert_eq!(out.item.unwrap().title, "");
    }

    #[tokio::test]
    async fn get_item_truncates_an_enormous_payload_and_says_so() {
        let (store, s) = svc();
        let id = make_item_named(&*store, "manuscript", "Long draft");
        let body = "x".repeat(MAX_PAYLOAD_BYTES * 2);
        store
            .update(
                uuid::Uuid::parse_str(&id).unwrap(),
                vec![FieldMutation::SetPayload(
                    "body_content".into(),
                    Value::String(body),
                )],
            )
            .expect("write a huge body");

        let out = s.get_item(id).await;
        assert!(out.ok, "{}", out.message);
        assert!(out.truncated, "a 64 KiB payload must be cut");
        assert!(out.payload.len() <= MAX_PAYLOAD_BYTES);
        let note = out.note.expect("truncation must be announced");
        assert!(note.contains("truncated"), "{note}");
        assert!(
            serde_json::from_str::<serde_json::Value>(&out.payload).is_err(),
            "a truncated payload is a prefix; the note is what tells the caller"
        );
        // The envelope still arrives whole — the cap is on the payload only.
        assert_eq!(out.item.unwrap().title, "Long draft");
    }

    #[tokio::test]
    async fn get_item_truncation_cuts_on_a_character_boundary() {
        let (store, s) = svc();
        let id = make_item_named(&*store, "manuscript", "Multibyte");
        store
            .update(
                uuid::Uuid::parse_str(&id).unwrap(),
                vec![FieldMutation::SetPayload(
                    "body_content".into(),
                    // Four-byte characters, so a byte-exact cut lands mid-glyph
                    // for three offsets out of four.
                    Value::String("𝄞".repeat(MAX_PAYLOAD_BYTES)),
                )],
            )
            .unwrap();

        let out = s.get_item(id).await;
        assert!(out.truncated);
        // Reaching this line at all proves the cut was legal: an invalid slice
        // would have panicked inside the service.
        assert!(out.payload.is_char_boundary(out.payload.len()));
        assert!(out.payload.len() <= MAX_PAYLOAD_BYTES);
    }

    #[tokio::test]
    async fn get_item_fails_loudly_on_unknown_and_malformed_ids() {
        let (_store, s) = svc();
        let missing = s.get_item(uuid::Uuid::new_v4().to_string()).await;
        assert!(!missing.ok);
        assert!(missing.item.is_none());
        assert!(missing.payload.is_empty());
        assert!(missing.message.contains("not found"), "{}", missing.message);

        let malformed = s.get_item("not-a-uuid".into()).await;
        assert!(!malformed.ok);
        assert!(
            malformed.message.contains("invalid UUID"),
            "{}",
            malformed.message
        );
    }

    // ─── list_items ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_items_pages_one_kind_newest_first() {
        let (store, s) = svc();
        let mut ids = Vec::new();
        for n in 0..5 {
            ids.push(make_item_named(&*store, "figure", &format!("Panel {n}")));
        }
        make_item_named(&*store, "manuscript", "Not a figure");

        // Touch them in a known order so `modified` is a known order.
        for id in &ids {
            store
                .update(
                    uuid::Uuid::parse_str(id).unwrap(),
                    vec![FieldMutation::SetStarred(true)],
                )
                .unwrap();
        }

        let first = s.list_items("figure".into(), 2, 0).await;
        assert!(first.ok, "{}", first.message);
        assert_eq!(first.total, 5, "total ignores the page window");
        assert_eq!(first.items.len(), 2);
        assert!(
            first.items.iter().all(|i| i.schema_ref == "figure"),
            "the manuscript must not leak into a figure browse: {:?}",
            first.items
        );

        // Paging is a partition: every row exactly once, no repeats.
        let second = s.list_items("figure".into(), 2, 2).await;
        let third = s.list_items("figure".into(), 2, 4).await;
        assert_eq!(third.items.len(), 1, "the tail page is short, not empty");
        let mut seen: Vec<String> = first
            .items
            .iter()
            .chain(second.items.iter())
            .chain(third.items.iter())
            .map(|i| i.id.clone())
            .collect();
        seen.sort();
        let mut expected = ids.clone();
        expected.sort();
        assert_eq!(seen, expected, "paging lost or duplicated a row");

        // Ordering is most-recently-modified first, and stable across calls.
        let all = s.list_items("figure".into(), 0, 0).await;
        let again = s.list_items("figure".into(), 0, 0).await;
        let order: Vec<&str> = all.items.iter().map(|i| i.id.as_str()).collect();
        let order_again: Vec<&str> = again.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(order, order_again, "browse order must be deterministic");
        assert!(
            all.items.windows(2).all(|w| w[0].modified >= w[1].modified),
            "expected modified-desc, got {:?}",
            all.items.iter().map(|i| &i.modified).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn list_items_walks_every_kind_when_no_schema_is_named() {
        let (store, s) = svc();
        make_item_named(&*store, "manuscript", "Draft");
        make_item_named(&*store, "figure", "Panel");
        make_item_named(&*store, "task", "Redo the fit");

        for every in ["", "any", "  ", "*"] {
            let out = s.list_items(every.into(), 0, 0).await;
            assert!(out.ok, "{every:?}: {}", out.message);
            assert_eq!(out.total, 3, "{every:?} must mean every kind");
            let mut kinds: Vec<&str> = out.items.iter().map(|i| i.schema_ref.as_str()).collect();
            kinds.sort();
            assert_eq!(kinds, vec!["figure", "manuscript", "task"]);
        }
    }

    #[tokio::test]
    async fn list_items_clamps_limits_and_offsets() {
        let (store, s) = svc();
        for n in 0..3 {
            make_item_named(&*store, "task", &format!("Task {n}"));
        }

        // 0 and negatives mean "unspecified", never an error or an empty page.
        for limit in [0i64, -1, i64::MIN] {
            let out = s.list_items("task".into(), limit, 0).await;
            assert!(out.ok, "limit {limit}: {}", out.message);
            assert_eq!(out.items.len(), 3, "limit {limit}");
        }
        // A negative offset reads as the start.
        assert_eq!(s.list_items("task".into(), 10, -5).await.items.len(), 3);
        // The ceiling binds without erroring.
        let huge = s.list_items("task".into(), i64::MAX, 0).await;
        assert!(huge.ok, "{}", huge.message);
        assert_eq!(huge.items.len(), 3);
        // Past the end is an empty page, not a failure — and `total` still
        // reports the truth, which is how a caller knows it overshot.
        let past = s.list_items("task".into(), 10, 99).await;
        assert!(past.ok, "{}", past.message);
        assert!(past.items.is_empty());
        assert_eq!(past.total, 3);
    }

    #[tokio::test]
    async fn list_items_reports_an_unknown_kind_as_empty_not_broken() {
        let (store, s) = svc();
        make_item_named(&*store, "manuscript", "Draft");
        let out = s.list_items("no-such-kind".into(), 0, 0).await;
        assert!(out.ok, "{}", out.message);
        assert!(out.items.is_empty());
        assert_eq!(out.total, 0);
    }

    #[tokio::test]
    async fn list_items_omits_payloads_but_keeps_the_triage_envelope() {
        let (store, s) = svc();
        let id = make_item_named(&*store, "task", "Recalibrate");
        store
            .update(
                uuid::Uuid::parse_str(&id).unwrap(),
                vec![
                    FieldMutation::SetStarred(true),
                    FieldMutation::AddTag("lab/urgent".into()),
                    FieldMutation::SetPayload("status".into(), Value::String("archived".into())),
                ],
            )
            .unwrap();

        let out = s.list_items("task".into(), 0, 0).await;
        let row = &out.items[0];
        assert!(row.starred);
        assert_eq!(row.tags, vec!["lab/urgent"]);
        assert_eq!(
            row.status.as_deref(),
            Some("archived"),
            "a browse that hid archived rows would make its own total a lie"
        );
    }
}
