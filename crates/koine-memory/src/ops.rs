//! The memory kernel — write, gate, rank and recall suite-scoped memory.
//!
//! The three memory schemas (`memory/claim`, `memory/episode`,
//! `memory/instruction`) say what a memory *is*.
//! This module is everything the suite does *with* one, and it lives here for
//! the same reason [`crate::watched_folder_ops`] does: the agent-facing verbs
//! (`#[impress_service]`), the Swift façade (`impress-store-ffi`) and a
//! headless `cargo test` must all drive the identical logic, and the only
//! place all three can reach without dragging the MCP machinery into every app
//! binary is the kernel crate.
//!
//! The primitives, and how a caller composes them (`remember` is the
//! service-level verb a caller writes; this module supplies its two halves and
//! deliberately does not choose between them on the caller's behalf):
//!
//! ```text
//!   remember ──► gate_fts ──► Insert  ──► insert_memory_item ──► ItemId
//!                    └──────► Confirm ──► confirm(existing)
//!   recall   ──► search_all ──► head filter ──► rank_memory_candidates
//!   brief    ──► recall × 3 (instructions, claims, episodes)
//! ```
//!
//! # Writing: [`insert_memory_item`]
//!
//! One flat payload (`title`, `body`, `claim_type`, … as top-level keys — NOT
//! a `{title, data, body}` nesting), `Visibility::Private`, `version` pinned to
//! the schema's, and the D39 field/edge pair written together: `subject_refs`
//! becomes `RelatesTo` edges, `evidence_refs` becomes `DerivedFrom`,
//! `agent_run_ref` becomes `ProducedBy`.
//!
//! A ref that does not resolve to a live item is **skipped for the edge and
//! kept in the payload**. The payload records what the author asserted; the
//! graph records what is actually there. Failing the whole write because one
//! evidence id was stale would mean an agent loses a true memory over a
//! bookkeeping detail, and silently dropping it from the payload too would
//! erase the only trace that the author believed it.
//!
//! [`MemoryDraft::deterministic_key`] is the idempotency seam. With a key, the
//! id is `UUIDv5(`[`MEMORY_NAMESPACE`]`, key)`, so a writer that runs twice —
//! a retried consolidation pass, a re-imported transcript — produces one row,
//! and the second call returns the existing id **without modifying it**.
//!
//! # The dedup gate: [`gate_fts`]
//!
//! Memory's characteristic failure is not forgetting; it is remembering the
//! same thing 40 times in slightly different words, until recall returns forty
//! paraphrases and nothing else fits in the context window. [`gate_fts`] is
//! the Tier-1 defence: before inserting, look for a near-duplicate by
//! full-text search, score token overlap, and if something is close enough
//! **confirm it instead** ([`confirm`] bumps `confirmations` and stamps
//! `last_confirmed`). A memory seen twice is one memory with more evidence
//! behind it, and [`rank_memory_candidates`] rewards it accordingly.
//!
//! What this tier deliberately does NOT do is decide that a new memory
//! *contradicts* an old one and supersede it automatically. Detecting "the
//! flux column is in mJy" versus "the flux column is in Jy" is a semantic
//! judgement — the two sentences have near-identical token sets, and any
//! threshold that catches the contradiction also merges them. So supersession
//! stays an explicit verb ([`supersede`]) driven by the LLM tier or a human,
//! and this gate only ever chooses between *insert* and *confirm*, both of
//! which are safe when wrong.
//!
//! # Reading: [`recall`] and [`brief`]
//!
//! [`recall`] composes FTS retrieval with three filters that are not
//! negotiable — superseded memories are not returned (that is what
//! supersession *means*), `no_recall` rows are withheld, and an optional
//! subject filter narrows to memories about one item — and then scores what
//! survives with [`rank_memory_candidates`].
//!
//! [`brief`] is the "start of session" shape: instructions first (they bind
//! regardless of topic), then claims (topic-scoped when a topic is given),
//! then recent episodes.
//!
//! # Ranking is a pure function
//!
//! [`rank_memory_candidates`] takes a slice and returns scores. No store, no
//! clock — `now_ms` is a parameter — so it is testable, reproducible and
//! reviewable as the *product decision* about what makes a memory worth
//! surfacing. Ties break on ascending id, the same discipline
//! `search_ops::rank_hybrid_candidates` is pinned to: without it the same
//! corpus ranks two ways between calls and no golden test can exist.

use std::collections::{BTreeMap, BTreeSet};

use uuid::Uuid;

use impress_item::schema::refs::{
    MEMORY_CLAIM as MEMORY_CLAIM_SCHEMA, MEMORY_EPISODE as MEMORY_EPISODE_SCHEMA,
    MEMORY_INSTRUCTION as MEMORY_INSTRUCTION_SCHEMA,
};
use impress_item::{
    ActorKind, AttributedStore, EdgeType, Item, ItemId, ItemQuery, OperationIntent, OperationSpec,
    OperationType, Priority, RetentionTier, SortDescriptor, StoreError, TypedReference, Value,
    Visibility,
};

/// The most rows one text-search retrieval asks for (the SQLite backend's
/// per-schema search cap, which this kernel was tuned against).
const MAX_LIMIT_PER_SCHEMA: u32 = 200;

/// Now, as the second-precision RFC 3339 stamp `last_confirmed` carries.
fn iso8601_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Fixed namespace for every deterministically-keyed memory row.
///
/// This is `UUIDv5(NAMESPACE_URL, "impress-memory")`, computed once and
/// hardcoded as a `const` (the v5 constructor is not `const fn`) — the same
/// arrangement `watched_folder_ops::WATCHED_FOLDER_NAMESPACE` uses, and pinned
/// against its own derivation by `namespace_is_the_documented_v5_uuid`.
///
/// **Never change it.** A new namespace re-keys every idempotent writer in the
/// suite, so the next consolidation pass duplicates every memory it has ever
/// written instead of recognising it.
pub const MEMORY_NAMESPACE: Uuid = Uuid::from_u128(0x05d7_874b_c6e3_5584_91a1_1b72_be33_7228);

/// Token overlap at or above which [`gate_fts`] confirms an existing memory
/// instead of inserting a new one.
///
/// 0.85 is deliberately high. The two errors are not symmetric: confirming
/// when it should have inserted *loses a distinct memory*, while inserting
/// when it should have confirmed costs one redundant row that a later
/// consolidation pass can merge. So the gate only fires on prose that is
/// nearly the same prose.
pub const GATE_CONFIRM_THRESHOLD: f32 = 0.85;

/// How many near-duplicate candidates [`gate_fts`] scores before deciding.
pub const GATE_CANDIDATES: u32 = 8;

/// Tokens of the probe text used to build the FTS query in
/// [`fts_near_duplicates`].
///
/// `search_ops::fts_match_expression` ANDs every token, so probing with a
/// 400-word body would demand that a candidate contain all 400 words — which
/// only an exact copy does — and would build an enormous MATCH expression to
/// do it. Truncating the *probe* keeps retrieval usefully loose; the overlap
/// score that follows is computed against the FULL text, so nothing is lost
/// from the decision itself.
pub const NEAR_DUP_PROBE_TOKENS: usize = 24;

/// Confirmation count past which extra confirmations stop adding score. A
/// memory confirmed 200 times is well-established, not forty times more
/// relevant than one confirmed five times.
pub const CONFIRMATION_CAP: u32 = 5;

/// Score contributed by a memory modified *right now*, decaying by
/// [`MemoryWeights::recency_half_life_ms`]. Additive rather than multiplicative
/// on purpose: a decade-old confirmed instruction must lose its recency bonus,
/// not its whole score.
pub const RECENCY_SCALE: f32 = 10.0;

/// Scale applied to `vector_similarity` (0–1) when an embedding engine
/// supplied one. `None` means "the engine did not see this candidate", which
/// is NOT the same as a similarity of zero and is not scored as one.
pub const VECTOR_SIMILARITY_SCALE: f32 = 12.0;

/// Row cap used when a [`RecallOptions::limit`] of `0` is passed.
pub const DEFAULT_RECALL_LIMIT: u32 = 20;

/// Hard ceiling on one [`recall`] page. Recall feeds a context window or a
/// list a human reads; neither wants thousands.
pub const MAX_RECALL_LIMIT: u32 = 200;

/// Entries per [`brief`] section when [`BriefOptions::max_entries`] is `0`.
pub const DEFAULT_BRIEF_ENTRIES: u32 = 8;

/// How many candidates are retrieved per memory kind for every requested
/// result, before filtering and ranking. Over-fetching is what lets the head
/// filter and the `no_recall` filter remove rows without the caller silently
/// getting a short page.
const RETRIEVAL_OVERSAMPLE: u32 = 4;

/// Payload key holding the withhold-from-recall flag.
const NO_RECALL_FIELD: &str = "no_recall";

/// Payload key holding the confirmation count.
const CONFIRMATIONS_FIELD: &str = "confirmations";

/// Payload key holding the ISO-8601 time of the last confirmation.
const LAST_CONFIRMED_FIELD: &str = "last_confirmed";

/// Metadata key carrying a supersession's justification.
const SUPERSEDE_REASON_KEY: &str = "reason";

// ---------------------------------------------------------------------------
// Kinds
// ---------------------------------------------------------------------------

/// The three memory kinds, each mapping to exactly one schema ref.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MemoryKind {
    /// What is true — [`MEMORY_CLAIM_SCHEMA`].
    Claim,
    /// What happened — [`MEMORY_EPISODE_SCHEMA`].
    Episode,
    /// What to do — [`MEMORY_INSTRUCTION_SCHEMA`].
    Instruction,
}

impl MemoryKind {
    /// The canonical `schema_ref` for this kind. The ONE place the mapping
    /// lives — every query, filter and payload in this module goes through it
    /// rather than spelling a ref, because the store matches by exact equality
    /// and a second spelling is a silently-empty result set forever.
    pub fn schema_ref(self) -> impress_item::SchemaRef {
        match self {
            MemoryKind::Claim => MEMORY_CLAIM_SCHEMA,
            MemoryKind::Episode => MEMORY_EPISODE_SCHEMA,
            MemoryKind::Instruction => MEMORY_INSTRUCTION_SCHEMA,
        }
    }

    /// The kind a `schema_ref` names, or `None` for a ref that is not a memory
    /// schema.
    pub fn from_schema_ref(schema_ref: &impress_item::SchemaRef) -> Option<Self> {
        match schema_ref {
            r if *r == MEMORY_CLAIM_SCHEMA => Some(MemoryKind::Claim),
            r if *r == MEMORY_EPISODE_SCHEMA => Some(MemoryKind::Episode),
            r if *r == MEMORY_INSTRUCTION_SCHEMA => Some(MemoryKind::Instruction),
            _ => None,
        }
    }

    /// Every kind, in the order [`brief`] renders them.
    pub fn all() -> [MemoryKind; 3] {
        [
            MemoryKind::Instruction,
            MemoryKind::Claim,
            MemoryKind::Episode,
        ]
    }
}

/// Every memory `schema_ref`, for callers that filter a mixed result set.
pub fn memory_schemas() -> [impress_item::SchemaRef; 3] {
    [
        MEMORY_CLAIM_SCHEMA,
        MEMORY_EPISODE_SCHEMA,
        MEMORY_INSTRUCTION_SCHEMA,
    ]
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Everything needed to write one memory row.
///
/// `title` and `body` are the two `items_fts` columns and are what makes the
/// row findable at all; everything else is optional refinement.
#[derive(Debug, Clone)]
pub struct MemoryDraft {
    /// Which of the three schemas this row lands in.
    pub kind: MemoryKind,
    /// Short label — one line.
    pub title: String,
    /// The memory itself, as prose.
    pub body: String,
    /// `fact` | `preference` | `method` | `decision` | `result`, or any other
    /// value the writer chose. Only meaningful on [`MemoryKind::Claim`]; the
    /// gate refuses to merge two claims whose types disagree.
    pub claim_type: Option<String>,
    /// 0.0–1.0. `None` means "unstated", which is not scored as zero.
    pub confidence: Option<f64>,
    /// ItemIds this memory is ABOUT. Mirrored as `RelatesTo` edges.
    pub subject_refs: Vec<String>,
    /// ItemIds this memory was DERIVED FROM. Mirrored as `DerivedFrom` edges.
    pub evidence_refs: Vec<String>,
    /// Persona id, when an agent authored this.
    pub agent_id: Option<String>,
    /// `agent-run@1.0.0` ItemId. Mirrored as a `ProducedBy` edge.
    pub agent_run_ref: Option<String>,
    /// Envelope author — who wrote the row.
    pub author: String,
    /// Whether that author is a human, an agent or the system.
    pub author_kind: ActorKind,
    /// When `Some`, the row's id is `UUIDv5(`[`MEMORY_NAMESPACE`]`, key)`, so
    /// a writer that runs twice writes one row. `None` mints a fresh v4 id.
    pub deterministic_key: Option<String>,
    /// Kind-specific fields (`task_kind`, `approach`, `outcome`, `quality`,
    /// `rule`, `applies_to`, …), written into the payload verbatim.
    ///
    /// A key here that collides with one this struct owns is IGNORED — the
    /// typed field wins, so `extra` can never quietly rewrite the body a
    /// caller passed in the field made for it.
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl MemoryDraft {
    /// A draft with the two required fields and nothing else set.
    pub fn new(
        kind: MemoryKind,
        title: impl Into<String>,
        body: impl Into<String>,
        author: impl Into<String>,
        author_kind: ActorKind,
    ) -> Self {
        Self {
            kind,
            title: title.into(),
            body: body.into(),
            claim_type: None,
            confidence: None,
            subject_refs: Vec::new(),
            evidence_refs: Vec::new(),
            agent_id: None,
            agent_run_ref: None,
            author: author.into(),
            author_kind,
            deterministic_key: None,
            extra: BTreeMap::new(),
        }
    }

    /// The id this draft will be written under, without writing it.
    pub fn planned_id(&self) -> Option<ItemId> {
        self.deterministic_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(|k| Uuid::new_v5(&MEMORY_NAMESPACE, k.as_bytes()))
    }
}

/// The payload keys [`MemoryDraft`] owns. `extra` may not overwrite them: the
/// typed field is the one the edge-building pass read, so letting `extra` win
/// would leave the payload and the graph disagreeing about the same relation.
const RESERVED_PAYLOAD_KEYS: [&str; 8] = [
    "title",
    "body",
    "claim_type",
    "confidence",
    "subject_refs",
    "evidence_refs",
    "agent_id",
    "agent_run_ref",
];

/// Write one memory row, with its D39 edges.
///
/// Idempotent when [`MemoryDraft::deterministic_key`] is set: if a row already
/// exists at the derived id **with the same schema**, its id is returned and
/// nothing is written. An id held by a row of a different schema is a
/// [`StoreError::Validation`], not a silent overwrite — that collision means
/// two writers picked the same key for different things, and continuing would
/// make one of them invisible.
///
/// Refs that do not resolve to a live item are kept in the payload and skipped
/// for the edge; see the module docs for why.
pub fn insert_memory_item(
    store: &dyn AttributedStore,
    draft: &MemoryDraft,
) -> Result<ItemId, StoreError> {
    let title = draft.title.trim();
    let body = draft.body.trim();
    if title.is_empty() {
        return Err(StoreError::Validation(
            "memory title must not be empty".into(),
        ));
    }
    if body.is_empty() {
        return Err(StoreError::Validation(
            "memory body must not be empty — a label cannot be confirmed or superseded".into(),
        ));
    }

    let schema = draft.kind.schema_ref();

    let id = match draft.planned_id() {
        Some(deterministic) => {
            if let Some(existing) = store.get(deterministic)? {
                if existing.schema == schema {
                    return Ok(deterministic);
                }
                return Err(StoreError::Validation(format!(
                    "deterministic key collides: {deterministic} is a '{}' item, not a '{schema}'",
                    existing.schema
                )));
            }
            deterministic
        }
        None => Uuid::new_v4(),
    };

    let mut payload: BTreeMap<String, Value> = BTreeMap::new();
    payload.insert("title".into(), Value::String(title.to_string()));
    payload.insert("body".into(), Value::String(body.to_string()));
    if let Some(claim_type) = trimmed(&draft.claim_type) {
        payload.insert("claim_type".into(), Value::String(claim_type));
    }
    if let Some(confidence) = draft.confidence {
        // The schema says 0.0–1.0 and the ranker multiplies confidence
        // straight into the score (`MemoryWeights::confidence`), so an
        // out-of-range value — a caller stating 90 for "90%" — would outrank
        // every honest memory forever. Clamp rather than reject: the write is
        // still a true memory, just an over-stated one. NaN is no statement
        // at all and is dropped like `None`.
        if !confidence.is_nan() {
            payload.insert(
                "confidence".into(),
                Value::Float(confidence.clamp(0.0, 1.0)),
            );
        }
    }
    let subject_refs = normalized_refs(&draft.subject_refs);
    let evidence_refs = normalized_refs(&draft.evidence_refs);
    if !subject_refs.is_empty() {
        payload.insert("subject_refs".into(), string_array(&subject_refs));
    }
    if !evidence_refs.is_empty() {
        payload.insert("evidence_refs".into(), string_array(&evidence_refs));
    }
    if let Some(agent_id) = trimmed(&draft.agent_id) {
        payload.insert("agent_id".into(), Value::String(agent_id));
    }
    let agent_run_ref = trimmed(&draft.agent_run_ref);
    if let Some(run_ref) = &agent_run_ref {
        payload.insert("agent_run_ref".into(), Value::String(run_ref.clone()));
    }
    for (key, value) in &draft.extra {
        if RESERVED_PAYLOAD_KEYS.contains(&key.as_str()) {
            continue;
        }
        payload.insert(key.clone(), json_to_value(value));
    }

    let mut references: Vec<TypedReference> = Vec::new();
    for r in &subject_refs {
        push_edge(store, &mut references, r, EdgeType::RelatesTo)?;
    }
    for r in &evidence_refs {
        push_edge(store, &mut references, r, EdgeType::DerivedFrom)?;
    }
    if let Some(run_ref) = &agent_run_ref {
        push_edge(store, &mut references, run_ref, EdgeType::ProducedBy)?;
    }

    let now = chrono::Utc::now();
    let item = Item {
        id,
        schema,
        payload,
        created: now,
        modified: now,
        author: draft.author.clone(),
        author_kind: draft.author_kind,
        logical_clock: 0,
        origin: None,
        canonical_id: None,
        tags: vec![],
        flag: None,
        is_read: false,
        is_starred: false,
        priority: Priority::Normal,
        // Memory is the user's own working knowledge. Sharing it is a
        // deliberate later act, never the default a write falls into.
        visibility: Visibility::Private,
        message_type: None,
        produced_by: None,
        version: Some("1.0.0".into()),
        batch_id: None,
        references,
        parent: None,
        extra: Default::default(),
    };
    store.insert(item)
}

/// Add `id`'s edge to `references`, or skip it when the target does not
/// resolve to a live item.
fn push_edge(
    store: &dyn AttributedStore,
    references: &mut Vec<TypedReference>,
    id: &str,
    edge_type: EdgeType,
) -> Result<(), StoreError> {
    let Ok(target) = Uuid::parse_str(id) else {
        return Ok(());
    };
    if store.get(target)?.is_none() {
        return Ok(());
    }
    references.push(TypedReference {
        target,
        edge_type,
        metadata: None,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Heads and supersession
// ---------------------------------------------------------------------------

/// The ids of one memory kind's **heads** — rows nothing supersedes — newest
/// first.
///
/// A head is the current version of a memory. `no_recall` rows are excluded
/// here too, so this is the same set [`recall`] draws from with an empty query.
///
/// `limit` is clamped to `1..=`[`MAX_RECALL_LIMIT`]; `0` means
/// [`DEFAULT_RECALL_LIMIT`].
pub fn claim_heads(
    store: &dyn AttributedStore,
    schema_ref: impress_item::SchemaRef,
    limit: u32,
) -> Result<Vec<ItemId>, StoreError> {
    let limit = clamp_limit(limit) as usize;
    // Heads are rows no `Supersedes` edge points at (the edge lives on the
    // NEW memory): one `references_to` probe per row, which SQLite answers
    // from its edge index (KM-D8).
    let mut rows = store.query(&ItemQuery {
        schema: Some(schema_ref),
        include_tags: false,
        include_references: false,
        ..Default::default()
    })?;
    rows.retain(|item| !no_recall(item));
    let mut heads = Vec::with_capacity(rows.len());
    for item in rows {
        if !is_superseded(store, item.id)? {
            heads.push(item);
        }
    }
    let mut rows = heads;
    rows.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.id.cmp(&b.id)));
    Ok(rows.into_iter().map(|item| item.id).take(limit).collect())
}

/// Record that `new_id` replaces `old_id` (ADR-0012 D39 rule 5).
///
/// Writes a `Supersedes` edge from the NEW memory to the OLD one — the
/// direction matters, and it is the direction that makes "is this a head?" a
/// single `NOT EXISTS` on `target_id`. The old row is left completely
/// untouched, which is the point: it stays in the graph for audit and
/// time-travel rather than being edited into agreement with its replacement.
///
/// `reason` is stored in the edge's metadata under `"reason"`, so the
/// justification travels with the relation rather than in a payload field on
/// either side.
///
/// Rejects self-supersession: an item that supersedes itself is never a head
/// again and would vanish from recall with no way back. Both ends must be
/// memory rows — a `Supersedes` edge into or out of an arbitrary record would
/// mint retraction semantics for a kind whose readers never check them, so a
/// mistyped id is a [`StoreError::Validation`], not a silent graph edit.
///
/// **Attribution deviation, deliberate.** `ItemStore::update` hardcodes the
/// store's `default_author`, so passing an author to it would be a lie. This
/// goes through [`AttributedStore::apply_operation`] instead, which takes the
/// author on the `OperationSpec` and writes it into the operation journal — so
/// "who retracted this memory" is genuinely answerable. `triage_ops` uses the
/// plain `update` path because its verbs carry no author.
pub fn supersede(
    store: &dyn AttributedStore,
    old_id: ItemId,
    new_id: ItemId,
    reason: Option<&str>,
    author: &str,
    author_kind: ActorKind,
) -> Result<(), StoreError> {
    if old_id == new_id {
        return Err(StoreError::Validation(
            "an item cannot supersede itself: it would never be a head again".into(),
        ));
    }
    for id in [old_id, new_id] {
        let item = store.get(id)?.ok_or(StoreError::NotFound(id))?;
        if MemoryKind::from_schema_ref(&item.schema).is_none() {
            return Err(StoreError::Validation(format!(
                "{id} is a '{}' item, not a memory row — supersession links memories only",
                item.schema
            )));
        }
    }

    let metadata = reason.map(str::trim).filter(|r| !r.is_empty()).map(|r| {
        let mut m: BTreeMap<String, Value> = BTreeMap::new();
        m.insert(SUPERSEDE_REASON_KEY.into(), Value::String(r.to_string()));
        m
    });

    store.apply_operation(OperationSpec {
        target_id: new_id,
        op_type: OperationType::AddReference(TypedReference {
            target: old_id,
            edge_type: EdgeType::Supersedes,
            metadata,
        }),
        // Editorial, not Routine: retracting a memory is a decision, and the
        // Durable tier keeps it out of the compaction window.
        intent: OperationIntent::Editorial,
        reason: reason.map(str::to_string),
        batch_id: None,
        author: author.to_string(),
        author_kind,
        retention: RetentionTier::Durable,
    })?;
    Ok(())
}

/// Record that a memory was independently re-observed: `confirmations += 1`
/// and `last_confirmed` stamped now.
///
/// This is the alternative to writing a near-duplicate row, and it is what
/// [`gate_fts`] returns when it finds one. Same attribution deviation as
/// [`supersede`] — the author reaches the operation journal through
/// [`AttributedStore::apply_operation`].
///
/// `id` must be a memory row: `confirmations` on, say, a manuscript is not a
/// count anything reads, and writing it would let a mistyped id silently
/// decorate an unrelated record — so anything else is a
/// [`StoreError::Validation`].
pub fn confirm(
    store: &dyn AttributedStore,
    id: ItemId,
    author: &str,
    author_kind: ActorKind,
) -> Result<u32, StoreError> {
    let item = store.get(id)?.ok_or(StoreError::NotFound(id))?;
    if MemoryKind::from_schema_ref(&item.schema).is_none() {
        return Err(StoreError::Validation(format!(
            "{id} is a '{}' item, not a memory row — only memories take confirmations",
            item.schema
        )));
    }
    let next = confirmations_of(&item).saturating_add(1);
    let batch_id = Some(Uuid::new_v4().to_string());

    for op_type in [
        OperationType::SetPayload(CONFIRMATIONS_FIELD.into(), Value::Int(next as i64)),
        OperationType::SetPayload(LAST_CONFIRMED_FIELD.into(), Value::String(iso8601_now())),
    ] {
        store.apply_operation(OperationSpec {
            target_id: id,
            op_type,
            intent: OperationIntent::Routine,
            reason: None,
            batch_id: batch_id.clone(),
            author: author.to_string(),
            author_kind,
            retention: RetentionTier::Durable,
        })?;
    }
    Ok(next)
}

// ---------------------------------------------------------------------------
// The Tier-1 dedup gate
// ---------------------------------------------------------------------------

/// A stored memory that resembles some candidate text.
#[derive(Debug, Clone, PartialEq)]
pub struct NearDup {
    /// The existing memory's id.
    pub id: ItemId,
    /// Jaccard overlap of the two lowercased alphanumeric token sets, 0–1.
    pub overlap: f32,
    /// The existing memory's title, so a caller can show what it matched.
    pub title: String,
}

/// Existing memories of `kind_schema` whose body resembles `text`, best first.
///
/// Two stages, because they answer different questions. FTS **retrieves** —
/// cheaply, over an index, using a truncated probe (see
/// [`NEAR_DUP_PROBE_TOKENS`]) so retrieval stays usefully loose. Jaccard token
/// overlap then **scores**, against the full `text` and the candidate's full
/// stored body, because BM25 rank is a relevance ordering and not a similarity
/// anyone can put a threshold on.
///
/// Ordered by overlap descending, ties broken by ascending id.
pub fn fts_near_duplicates(
    store: &dyn AttributedStore,
    kind_schema: impress_item::SchemaRef,
    text: &str,
    k: u32,
) -> Result<Vec<NearDup>, StoreError> {
    let k = k.max(1);
    let needle = tokens(text);
    if needle.is_empty() {
        return Ok(vec![]);
    }
    let probe = probe_text(text);
    let width = k
        .saturating_mul(RETRIEVAL_OVERSAMPLE)
        .clamp(1, MAX_LIMIT_PER_SCHEMA);

    let hits = store.search_text(Some(kind_schema.as_str()), &probe, width as usize)?;
    let mut scored: Vec<NearDup> = Vec::new();
    for hit in hits {
        let id = hit.id;
        let Some(item) = store.get(id)? else {
            continue;
        };
        let overlap = jaccard(
            &needle,
            &tokens(&string_field(&item, "body").unwrap_or_default()),
        );
        scored.push(NearDup {
            id,
            overlap,
            title: string_field(&item, "title").unwrap_or_default(),
        });
    }

    scored.sort_by(|a, b| {
        b.overlap
            .partial_cmp(&a.overlap)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    scored.truncate(k as usize);
    Ok(scored)
}

/// What [`gate_fts`] decided to do with a draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateOutcome {
    /// Nothing close enough exists — write the draft.
    Insert,
    /// This memory is already stored; confirm the named row instead.
    Confirm(ItemId),
}

/// Whether two claim types may be merged by a dedup gate: equal, or either
/// side unset. The absence of a declaration is not a declaration of
/// difference — but "the user prefers Typst" and "Typst is faster" can be
/// nearly the same sentence and are not the same memory, so two SET types
/// must match exactly. A blank or whitespace-only type counts as unset, so
/// every tier agrees regardless of how a writer spelled "nothing".
pub fn claim_types_compatible(a: Option<&str>, b: Option<&str>) -> bool {
    fn set(s: Option<&str>) -> Option<&str> {
        s.map(str::trim).filter(|s| !s.is_empty())
    }
    match (set(a), set(b)) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
}

/// Whether the row at `id` may absorb a dedup-gate confirmation for a draft
/// carrying `draft_claim_type` — the ONE eligibility policy every gate tier
/// applies (the FTS tier here in [`gate_fts`], the vector tier in
/// `impress-memory-service`), so the tiers can never drift apart candidate
/// by candidate.
///
/// `true` iff the row exists, is a memory row, nothing supersedes it, it is
/// not withheld (`no_recall`), and its `claim_type` is compatible with the
/// draft's ([`claim_types_compatible`]). A superseded memory must never
/// absorb a confirmation — that would strengthen the version its replacement
/// already retracted — and a withheld one must not either: `forget` took it
/// out of recall, and a re-remember of the same prose must fall through to a
/// fresh row rather than silently feeding the hidden one.
///
/// Deliberately a per-id predicate rather than a membership set: a set of
/// eligible rows has to be built through some capped page, and absence from
/// the page then reads as ineligibility. [`claim_heads`] caps at
/// [`MAX_RECALL_LIMIT`], so a page-based check silently stops deduplicating
/// against exactly the oldest, best-established heads once a kind grows past
/// 200 of them.
pub fn absorbs_confirmation(
    store: &dyn AttributedStore,
    id: ItemId,
    draft_claim_type: Option<&str>,
) -> Result<bool, StoreError> {
    let Some(item) = store.get(id)? else {
        return Ok(false);
    };
    if MemoryKind::from_schema_ref(&item.schema).is_none() {
        return Ok(false);
    }
    if is_superseded(store, id)? || no_recall(&item) {
        return Ok(false);
    }
    Ok(claim_types_compatible(
        draft_claim_type,
        string_field(&item, "claim_type").as_deref(),
    ))
}

/// Decide whether a draft is new or a restatement of something already stored.
///
/// Returns [`GateOutcome::Confirm`] for the best-scoring near-duplicate at or
/// above `threshold` that [`absorbs_confirmation`] accepts: a live memory row
/// (nothing supersedes it, not withheld by `forget`) whose `claim_type` is
/// compatible with the draft's — where compatible means equal, or either side
/// unset. A different `claim_type` is never merged: "the user prefers Typst"
/// and "Typst is faster" can be nearly the same sentence and are not the same
/// memory.
///
/// Candidates are walked in descending overlap, so an ineligible
/// near-duplicate does not block a slightly-lower-scoring eligible one; if
/// none qualify the outcome is [`GateOutcome::Insert`].
///
/// This function never writes. The caller applies the outcome, which keeps
/// "what would this do?" answerable without touching the store.
pub fn gate_fts(
    store: &dyn AttributedStore,
    draft: &MemoryDraft,
    threshold: f32,
) -> Result<GateOutcome, StoreError> {
    let candidates =
        fts_near_duplicates(store, draft.kind.schema_ref(), &draft.body, GATE_CANDIDATES)?;
    let draft_type = trimmed(&draft.claim_type);

    for candidate in candidates.iter().take_while(|c| c.overlap >= threshold) {
        if absorbs_confirmation(store, candidate.id, draft_type.as_deref())? {
            return Ok(GateOutcome::Confirm(candidate.id));
        }
    }
    Ok(GateOutcome::Insert)
}

// ---------------------------------------------------------------------------
// Ranking (pure)
// ---------------------------------------------------------------------------

/// The relative pull of each ranking signal.
///
/// Every field is a multiplier on a normalised signal, so the numbers are
/// directly comparable: at the defaults, being human-authored is worth about
/// as much as five confirmations, and a memory touched today outranks an
/// identical one from two half-lives ago by [`RECENCY_SCALE`] × 0.75.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryWeights {
    /// Multiplier on the full-text relevance score (BM25, sign-flipped so
    /// larger is better).
    pub fts_rank: f32,
    /// Multiplier on the confirmation count, itself capped at
    /// [`CONFIRMATION_CAP`].
    pub confirmations: f32,
    /// Half-life of the recency bonus, in milliseconds. 30 days by default:
    /// long enough that last month's decisions still surface, short enough
    /// that a stale claim yields to a fresh one about the same subject.
    pub recency_half_life_ms: f64,
    /// Flat bonus for a human-authored memory. The user's own words outrank an
    /// agent's summary of them, because the agent can regenerate its summary
    /// and cannot regenerate the user.
    pub human_author: f32,
    /// Multiplier on stated confidence (0–1). Unstated confidence scores 0
    /// here — it neither helps nor hurts.
    pub confidence: f32,
}

impl Default for MemoryWeights {
    fn default() -> Self {
        Self {
            fts_rank: 1.0,
            confirmations: 1.5,
            recency_half_life_ms: 30.0 * 24.0 * 60.0 * 60.0 * 1000.0,
            human_author: 7.5,
            confidence: 4.0,
        }
    }
}

/// One memory as the ranker sees it — no store, no payload, just signals.
///
/// A `None` score means "this engine did not return this candidate", which is
/// load-bearing rather than a default: it is not the same as a zero, and is
/// not scored as one.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryCandidate {
    /// Lowercase UUID string. Also the tie-break key, so every caller must
    /// spell it the same way.
    pub id: String,
    /// Full-text relevance, larger is better. `None` when the query was empty
    /// or the text index did not return this row.
    pub fts_score: Option<f32>,
    /// Embedding cosine similarity, 0–1. `None` when no vector engine ran.
    pub vector_similarity: Option<f32>,
    /// How many times this memory has been re-observed.
    pub confirmations: u32,
    /// `Item::modified` in epoch milliseconds.
    pub modified_ms: i64,
    /// Whether the envelope author is a human.
    pub author_kind_human: bool,
    /// Stated confidence, 0–1. `None` means unstated.
    pub confidence: Option<f32>,
}

/// Score and order memory candidates. Pure, deterministic, total.
///
/// Ordering is score descending, **ties broken by ascending id** — the same
/// discipline `search_ops::rank_hybrid_candidates` is pinned to, and for the
/// same reason: without it, two runs over one corpus order equal-scored
/// memories differently and no golden test can exist.
///
/// `now_ms` is a parameter rather than a `Utc::now()` call so the function has
/// no clock of its own and a test can pin the recency term exactly. A
/// `modified_ms` in the future (clock skew across synced devices) is treated as
/// "now" rather than earning an amplified bonus.
pub fn rank_memory_candidates(
    candidates: &[MemoryCandidate],
    weights: &MemoryWeights,
    now_ms: i64,
) -> Vec<(String, f32)> {
    let mut ranked: Vec<(String, f32)> = candidates
        .iter()
        .map(|c| (c.id.clone(), memory_score(c, weights, now_ms)))
        .collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    ranked
}

/// The score for one candidate.
fn memory_score(c: &MemoryCandidate, w: &MemoryWeights, now_ms: i64) -> f32 {
    let mut score = 0.0f32;

    if let Some(fts) = c.fts_score {
        score += fts * w.fts_rank;
    }
    if let Some(similarity) = c.vector_similarity {
        score += similarity * VECTOR_SIMILARITY_SCALE;
    }
    score += c.confirmations.min(CONFIRMATION_CAP) as f32 * w.confirmations;
    score += RECENCY_SCALE * recency_decay(c.modified_ms, now_ms, w.recency_half_life_ms);
    if c.author_kind_human {
        score += w.human_author;
    }
    if let Some(confidence) = c.confidence {
        score += confidence * w.confidence;
    }
    score
}

/// Exponential decay by half-life: 1.0 at `now`, 0.5 one half-life ago.
fn recency_decay(modified_ms: i64, now_ms: i64, half_life_ms: f64) -> f32 {
    if half_life_ms <= 0.0 {
        return 0.0;
    }
    let age = (now_ms - modified_ms).max(0) as f64;
    0.5f64.powf(age / half_life_ms) as f32
}

// ---------------------------------------------------------------------------
// Recall
// ---------------------------------------------------------------------------

/// How to narrow a [`recall`].
#[derive(Debug, Clone, PartialEq)]
pub struct RecallOptions {
    /// Only memories ABOUT this ItemId (its `subject_refs` / `RelatesTo`).
    pub subject_ref: Option<String>,
    /// Rows to return. `0` means [`DEFAULT_RECALL_LIMIT`]; clamped to
    /// [`MAX_RECALL_LIMIT`].
    pub limit: u32,
    /// Include memories something else supersedes. Off by default, because a
    /// superseded memory is a retracted one and returning it silently is how a
    /// corrected fact gets re-asserted.
    pub include_superseded: bool,
    /// Which kinds to search. Empty means all three.
    pub kinds: Vec<MemoryKind>,
}

impl Default for RecallOptions {
    fn default() -> Self {
        Self {
            subject_ref: None,
            limit: DEFAULT_RECALL_LIMIT,
            include_superseded: false,
            kinds: Vec::new(),
        }
    }
}

/// One recalled memory, hydrated and scored.
#[derive(Debug, Clone, PartialEq)]
pub struct RecallEntry {
    /// Lowercase UUID string.
    pub id: String,
    /// One of [`memory_schemas`].
    pub schema_ref: String,
    /// The memory's short label.
    pub title: String,
    /// The memory itself.
    pub body: String,
    /// Claim flavour, when the row carries one.
    pub claim_type: Option<String>,
    /// Stated confidence, 0–1.
    pub confidence: Option<f32>,
    /// How many times this memory has been re-observed.
    pub confirmations: u32,
    /// `Item::modified` in epoch milliseconds.
    pub modified_ms: i64,
    /// The score [`rank_memory_candidates`] gave it.
    pub score: f32,
    /// ItemIds this memory is about.
    pub subject_refs: Vec<String>,
}

/// Retrieve memories matching `query`, filtered and ranked.
///
/// An empty query is not an error and not an empty result: it means "the most
/// relevant memories, with no text to go on", and is served from the heads by
/// recency rather than from FTS. That is what makes [`brief`] a composition of
/// this function instead of a second retrieval path.
///
/// Three filters always apply, in this order:
/// 1. superseded memories are dropped, unless
///    [`RecallOptions::include_superseded`];
/// 2. `no_recall` rows are withheld;
/// 3. when [`RecallOptions::subject_ref`] is set, only memories about that
///    item survive — matched on the payload array OR the `RelatesTo` edge,
///    since either alone would miss rows written by the other convention.
pub fn recall(
    store: &dyn AttributedStore,
    query: &str,
    opts: &RecallOptions,
) -> Result<Vec<RecallEntry>, StoreError> {
    let limit = clamp_limit(opts.limit);
    let kinds: Vec<MemoryKind> = if opts.kinds.is_empty() {
        MemoryKind::all().to_vec()
    } else {
        opts.kinds.clone()
    };
    let width = limit
        .saturating_mul(RETRIEVAL_OVERSAMPLE)
        .clamp(1, MAX_LIMIT_PER_SCHEMA);
    let subject = opts
        .subject_ref
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase);

    // Stage 1 — retrieve ids, with an FTS score when there was a query.
    let mut retrieved: Vec<(ItemId, Option<f32>)> = Vec::new();
    let has_query = !tokens(query).is_empty();
    if has_query {
        // The port's text search scores larger-is-better, the convention
        // every other ranking signal uses.
        for kind in &kinds {
            for hit in store.search_text(Some(kind.schema_ref().as_str()), query, width as usize)? {
                retrieved.push((hit.id, Some(hit.score)));
            }
        }
    } else {
        for kind in &kinds {
            let ids = if opts.include_superseded {
                items_by_recency(store, kind.schema_ref(), width)?
            } else {
                claim_heads(store, kind.schema_ref(), width)?
            };
            retrieved.extend(ids.into_iter().map(|id| (id, None)));
        }
    }

    // Stage 2 — load, filter, and build ranking candidates.
    let id_strings: Vec<String> = retrieved.iter().map(|(id, _)| id.to_string()).collect();
    let superseded = if opts.include_superseded {
        BTreeSet::new()
    } else {
        superseded_ids(store, &id_strings)?
    };

    let mut items: BTreeMap<String, Item> = BTreeMap::new();
    let mut candidates: Vec<MemoryCandidate> = Vec::new();
    for (id, fts_score) in retrieved {
        let key = id.to_string();
        if items.contains_key(&key) {
            continue;
        }
        if superseded.contains(&key) {
            continue;
        }
        let Some(item) = store.get(id)? else {
            continue;
        };
        if no_recall(&item) {
            continue;
        }
        if let Some(subject) = &subject {
            if !is_about(&item, subject) {
                continue;
            }
        }
        candidates.push(MemoryCandidate {
            id: key.clone(),
            fts_score,
            vector_similarity: None,
            confirmations: confirmations_of(&item),
            modified_ms: item.modified.timestamp_millis(),
            author_kind_human: item.author_kind == ActorKind::Human,
            confidence: float_field(&item, "confidence").map(|f| f as f32),
        });
        items.insert(key, item);
    }

    // Stage 3 — rank and hydrate.
    let now_ms = chrono::Utc::now().timestamp_millis();
    let ranked = rank_memory_candidates(&candidates, &MemoryWeights::default(), now_ms);
    Ok(ranked
        .into_iter()
        .take(limit as usize)
        .filter_map(|(id, score)| items.get(&id).map(|item| entry(item, score)))
        .collect())
}

/// One section of a [`brief`].
#[derive(Debug, Clone, PartialEq)]
pub struct BriefSection {
    /// The section's memory kind, spelled as its `schema_ref` (one of
    /// [`memory_schemas`]) so a consumer maps it back without a second
    /// vocabulary to keep in step.
    pub kind: String,
    /// The section's memories, best first. May be empty — every section is
    /// returned every time so the shape is stable and a renderer can rely on
    /// the ordering rather than searching for its heading.
    pub entries: Vec<RecallEntry>,
}

/// What to put in a brief.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BriefOptions {
    /// Topic to scope the CLAIMS section to. Instructions and episodes ignore
    /// it — see [`brief`].
    pub topic: Option<String>,
    /// Narrow every section to memories about this ItemId.
    pub subject_ref: Option<String>,
    /// Entries per section. `0` means [`DEFAULT_BRIEF_ENTRIES`].
    pub max_entries: u32,
}

/// The "here is what you know" digest: instructions, then claims, then
/// episodes.
///
/// The order is the priority order and it is fixed. Standing instructions come
/// first because they constrain what may be done with everything after them;
/// claims come next because they are what the work is about; episodes come
/// last because they are advisory.
///
/// Only the CLAIMS section is topic-scoped. An instruction that does not
/// lexically match the topic still binds — "never open the shared container
/// from a shell" applies while you are thinking about citations — and
/// filtering instructions by topic would quietly drop exactly the ones the
/// current work was not expecting.
pub fn brief(
    store: &dyn AttributedStore,
    opts: &BriefOptions,
) -> Result<Vec<BriefSection>, StoreError> {
    let limit = if opts.max_entries == 0 {
        DEFAULT_BRIEF_ENTRIES
    } else {
        opts.max_entries.min(MAX_RECALL_LIMIT)
    };
    let topic = opts
        .topic
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("");

    let mut sections = Vec::with_capacity(3);
    for kind in MemoryKind::all() {
        let query = if kind == MemoryKind::Claim { topic } else { "" };
        let entries = recall(
            store,
            query,
            &RecallOptions {
                subject_ref: opts.subject_ref.clone(),
                limit,
                include_superseded: false,
                kinds: vec![kind],
            },
        )?;
        sections.push(BriefSection {
            kind: kind.schema_ref().to_string(),
            entries,
        });
    }
    Ok(sections)
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// Whether anything supersedes `id`.
fn is_superseded(store: &dyn AttributedStore, id: ItemId) -> Result<bool, StoreError> {
    Ok(!store.references_to(id, &[EdgeType::Supersedes])?.is_empty())
}

/// Which of `ids` have an incoming `Supersedes` edge.
fn superseded_ids(
    store: &dyn AttributedStore,
    ids: &[String],
) -> Result<BTreeSet<String>, StoreError> {
    let mut found = BTreeSet::new();
    for raw in ids {
        if let Ok(id) = Uuid::parse_str(raw) {
            if is_superseded(store, id)? {
                found.insert(raw.clone());
            }
        }
    }
    Ok(found)
}

/// Ids of one schema by recency, superseded rows included — the
/// `include_superseded` twin of [`claim_heads`].
fn items_by_recency(
    store: &dyn AttributedStore,
    schema_ref: impress_item::SchemaRef,
    limit: u32,
) -> Result<Vec<ItemId>, StoreError> {
    let q = ItemQuery {
        schema: Some(schema_ref),
        sort: vec![SortDescriptor {
            field: "modified".into(),
            ascending: false,
        }],
        limit: Some(limit as usize),
        include_tags: false,
        include_references: false,
        ..Default::default()
    };
    Ok(store.query(&q)?.into_iter().map(|i| i.id).collect())
}

/// Whether a memory is about `subject` — payload array OR `RelatesTo` edge.
fn is_about(item: &Item, subject: &str) -> bool {
    if let Some(refs) = string_array_field(item, "subject_refs") {
        if refs.iter().any(|r| r.to_lowercase() == subject) {
            return true;
        }
    }
    item.references.iter().any(|r| {
        r.edge_type == EdgeType::RelatesTo && r.target.to_string().to_lowercase() == subject
    })
}

/// Whether a memory is withheld from recall.
fn no_recall(item: &Item) -> bool {
    matches!(item.payload.get(NO_RECALL_FIELD), Some(Value::Bool(true)))
}

/// A memory's confirmation count, `0` when absent or malformed.
fn confirmations_of(item: &Item) -> u32 {
    match item.payload.get(CONFIRMATIONS_FIELD) {
        Some(Value::Int(n)) if *n > 0 => (*n).min(u32::MAX as i64) as u32,
        Some(Value::Float(f)) if *f > 0.0 => *f as u32,
        _ => 0,
    }
}

fn entry(item: &Item, score: f32) -> RecallEntry {
    RecallEntry {
        id: item.id.to_string(),
        schema_ref: item.schema.to_string(),
        title: string_field(item, "title").unwrap_or_default(),
        body: string_field(item, "body").unwrap_or_default(),
        claim_type: string_field(item, "claim_type"),
        confidence: float_field(item, "confidence").map(|f| f as f32),
        confirmations: confirmations_of(item),
        modified_ms: item.modified.timestamp_millis(),
        score,
        subject_refs: string_array_field(item, "subject_refs").unwrap_or_default(),
    }
}

fn clamp_limit(limit: u32) -> u32 {
    match limit {
        0 => DEFAULT_RECALL_LIMIT,
        n => n.min(MAX_RECALL_LIMIT),
    }
}

fn string_field(item: &Item, field: &str) -> Option<String> {
    match item.payload.get(field) {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn float_field(item: &Item, field: &str) -> Option<f64> {
    match item.payload.get(field) {
        Some(Value::Float(f)) => Some(*f),
        Some(Value::Int(i)) => Some(*i as f64),
        _ => None,
    }
}

fn string_array_field(item: &Item, field: &str) -> Option<Vec<String>> {
    match item.payload.get(field) {
        Some(Value::Array(values)) => Some(
            values
                .iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
        ),
        _ => None,
    }
}

fn trimmed(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Trim, drop blanks, lowercase and de-duplicate a ref list while preserving
/// first-seen order. Ids are compared case-insensitively everywhere else, so
/// storing them lowercased means a payload filter and an edge lookup agree.
fn normalized_refs(refs: &[String]) -> Vec<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::with_capacity(refs.len());
    for r in refs {
        let r = r.trim().to_lowercase();
        if r.is_empty() || !seen.insert(r.clone()) {
            continue;
        }
        out.push(r);
    }
    out
}

fn string_array(values: &[String]) -> Value {
    Value::Array(values.iter().cloned().map(Value::String).collect())
}

/// Convert a `serde_json::Value` into the store's `Value`. Total — every JSON
/// shape has a counterpart, so a caller's `extra` field never silently
/// disappears.
fn json_to_value(value: &serde_json::Value) -> Value {
    match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => Value::Int(i),
            None => Value::Float(n.as_f64().unwrap_or_default()),
        },
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(items) => Value::Array(items.iter().map(json_to_value).collect()),
        serde_json::Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), json_to_value(v)))
                .collect(),
        ),
    }
}

/// Lowercased alphanumeric token set. Every non-alphanumeric character is a
/// separator, matching `search_ops::fts_match_expression` so the tokens being
/// scored are the tokens that were searched for.
fn tokens(text: &str) -> BTreeSet<String> {
    text.chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(str::to_lowercase)
        .collect()
}

/// Jaccard similarity of two token sets. Two empty sets score `0.0`, not
/// `1.0`: "these are both nothing" is not evidence that they are the same
/// memory, and a `NaN` from `0/0` would poison every comparison downstream.
fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count() as f32;
    let union = (a.len() + b.len()) as f32 - intersection;
    if union <= 0.0 {
        0.0
    } else {
        intersection / union
    }
}

/// The first [`NEAR_DUP_PROBE_TOKENS`] tokens of `text`, as a search string.
fn probe_text(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .take(NEAR_DUP_PROBE_TOKENS)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod ranking_tests {
    use super::*;

    fn candidate(id: &str) -> MemoryCandidate {
        MemoryCandidate {
            id: id.to_string(),
            fts_score: None,
            vector_similarity: None,
            confirmations: 0,
            modified_ms: 0,
            author_kind_human: false,
            confidence: None,
        }
    }

    #[test]
    fn ties_break_on_ascending_id() {
        let ids = ["ffff", "0000", "aaaa"];
        let candidates: Vec<MemoryCandidate> = ids.iter().map(|id| candidate(id)).collect();
        let ranked = rank_memory_candidates(&candidates, &MemoryWeights::default(), 0);
        assert_eq!(
            ranked.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            ["0000", "aaaa", "ffff"]
        );
    }

    #[test]
    fn confirmations_raise_the_score_but_saturate() {
        let w = MemoryWeights::default();
        let mut one = candidate("a");
        one.confirmations = 1;
        let mut capped = candidate("b");
        capped.confirmations = CONFIRMATION_CAP;
        let mut absurd = candidate("c");
        absurd.confirmations = 10_000;

        let scores: BTreeMap<String, f32> = rank_memory_candidates(&[one, capped, absurd], &w, 0)
            .into_iter()
            .collect();
        assert!(scores["b"] > scores["a"]);
        assert_eq!(
            scores["c"], scores["b"],
            "a memory confirmed 10 000 times is not 2 000× more relevant"
        );
    }

    #[test]
    fn recency_decays_by_half_life() {
        let w = MemoryWeights::default();
        let half_life = w.recency_half_life_ms as i64;
        let now = 10 * half_life;

        let mut fresh = candidate("a");
        fresh.modified_ms = now;
        let mut old = candidate("b");
        old.modified_ms = now - half_life;

        let scores: BTreeMap<String, f32> = rank_memory_candidates(&[fresh, old], &w, now)
            .into_iter()
            .collect();
        assert!((scores["a"] - RECENCY_SCALE).abs() < 0.01);
        assert!((scores["b"] - RECENCY_SCALE / 2.0).abs() < 0.01);
    }

    /// Clock skew across synced devices makes future timestamps real. A future
    /// memory must not out-score a present one by amplifying the decay term.
    #[test]
    fn a_future_timestamp_is_treated_as_now() {
        let w = MemoryWeights::default();
        let mut future = candidate("a");
        future.modified_ms = 1_000_000;
        let mut now_ = candidate("b");
        now_.modified_ms = 0;
        let scores: BTreeMap<String, f32> = rank_memory_candidates(&[future, now_], &w, 0)
            .into_iter()
            .collect();
        assert_eq!(scores["a"], scores["b"]);
    }

    /// `None` means "the engine did not see this row", not "similarity 0".
    #[test]
    fn an_absent_vector_score_neither_helps_nor_hurts() {
        let w = MemoryWeights::default();
        let mut unseen = candidate("a");
        unseen.vector_similarity = None;
        let mut zero = candidate("b");
        zero.vector_similarity = Some(0.0);
        let mut similar = candidate("c");
        similar.vector_similarity = Some(1.0);

        let scores: BTreeMap<String, f32> = rank_memory_candidates(&[unseen, zero, similar], &w, 0)
            .into_iter()
            .collect();
        assert_eq!(scores["a"], scores["b"]);
        assert!((scores["c"] - scores["a"] - VECTOR_SIMILARITY_SCALE).abs() < 0.001);
    }

    #[test]
    fn a_human_author_outranks_an_agent_all_else_equal() {
        let w = MemoryWeights::default();
        let mut human = candidate("z-human");
        human.author_kind_human = true;
        let agent = candidate("a-agent");
        let ranked = rank_memory_candidates(&[agent, human], &w, 0);
        assert_eq!(ranked[0].0, "z-human", "id tie-break must not win here");
    }

    #[test]
    fn ranking_is_deterministic_across_input_orderings() {
        let w = MemoryWeights::default();
        let mut a = candidate("aaa");
        a.confirmations = 2;
        let mut b = candidate("bbb");
        b.confidence = Some(0.9);
        let mut c = candidate("ccc");
        c.fts_score = Some(3.0);

        let forward = rank_memory_candidates(&[a.clone(), b.clone(), c.clone()], &w, 0);
        let backward = rank_memory_candidates(&[c, b, a], &w, 0);
        assert_eq!(forward, backward);
    }

    #[test]
    fn an_empty_candidate_set_ranks_to_nothing() {
        assert!(rank_memory_candidates(&[], &MemoryWeights::default(), 0).is_empty());
    }

    #[test]
    fn jaccard_is_symmetric_bounded_and_safe_on_empties() {
        let a = tokens("the flux column is in millijansky");
        let b = tokens("the flux column is in jansky");
        assert!((jaccard(&a, &b) - jaccard(&b, &a)).abs() < f32::EPSILON);
        assert!(jaccard(&a, &b) > 0.0 && jaccard(&a, &b) < 1.0);
        assert_eq!(jaccard(&a, &a), 1.0);
        assert_eq!(jaccard(&tokens(""), &tokens("")), 0.0);
        assert_eq!(jaccard(&a, &tokens("   ---   ")), 0.0);
    }

    /// The namespace is hardcoded because the v5 constructor is not `const`.
    /// This is the assertion that keeps the constant honest.
    #[test]
    fn namespace_is_the_documented_v5_uuid() {
        assert_eq!(
            MEMORY_NAMESPACE,
            Uuid::new_v5(&Uuid::NAMESPACE_URL, b"impress-memory")
        );
    }

    #[test]
    fn kinds_map_to_the_three_canonical_refs_both_ways() {
        for kind in MemoryKind::all() {
            assert_eq!(MemoryKind::from_schema_ref(&kind.schema_ref()), Some(kind));
        }
        assert_eq!(
            MemoryKind::from_schema_ref(&impress_item::schema::refs::MANUSCRIPT),
            None
        );
        let mut refs = memory_schemas().to_vec();
        refs.sort_unstable();
        assert_eq!(
            refs,
            [
                MEMORY_CLAIM_SCHEMA,
                MEMORY_EPISODE_SCHEMA,
                MEMORY_INSTRUCTION_SCHEMA
            ]
        );
    }

    /// The one claim-type rule both gate tiers share: equal, or either side
    /// unset — with every blank spelling of "nothing" counting as unset
    /// rather than as a third type.
    #[test]
    fn claim_type_compatibility_is_equal_or_either_unset() {
        assert!(claim_types_compatible(None, None));
        assert!(claim_types_compatible(Some("fact"), None));
        assert!(claim_types_compatible(None, Some("fact")));
        assert!(claim_types_compatible(Some("fact"), Some("fact")));
        assert!(!claim_types_compatible(Some("fact"), Some("preference")));
        assert!(claim_types_compatible(Some("   "), Some("fact")));
        assert!(claim_types_compatible(Some("fact"), Some("")));
    }

    #[test]
    fn probe_text_is_bounded_and_stripped() {
        let long: String = (0..100)
            .map(|n| format!("word{n} "))
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(
            probe_text(&long).split_whitespace().count(),
            NEAR_DUP_PROBE_TOKENS
        );
        assert_eq!(probe_text("foo, (bar)! baz"), "foo bar baz");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use impress_item::{FieldMutation, ItemStore};
    use koine_store_memory::MemoryStore;

    fn open() -> MemoryStore {
        MemoryStore::new()
    }

    /// A non-memory row, so subject/evidence refs have something real to point
    /// at.
    fn subject_item(store: &dyn AttributedStore, title: &str) -> ItemId {
        let now = chrono::Utc::now();
        let mut payload: BTreeMap<String, Value> = BTreeMap::new();
        payload.insert("title".into(), Value::String(title.into()));
        store
            .insert(Item {
                id: Uuid::new_v4(),
                schema: impress_item::schema::refs::MANUSCRIPT,
                payload,
                created: now,
                modified: now,
                author: "tester".into(),
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
            })
            .expect("insert subject")
    }

    /// A claim written at an explicit `modified` time — `insert` preserves
    /// the timestamps it is handed — for tests that need a deterministic
    /// recency order regardless of wall-clock resolution.
    fn memory_claim_at(
        store: &dyn AttributedStore,
        title: &str,
        body: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> ItemId {
        let mut payload: BTreeMap<String, Value> = BTreeMap::new();
        payload.insert("title".into(), Value::String(title.into()));
        payload.insert("body".into(), Value::String(body.into()));
        store
            .insert(Item {
                id: Uuid::new_v4(),
                schema: MEMORY_CLAIM_SCHEMA,
                payload,
                created: at,
                modified: at,
                author: "tester".into(),
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
                version: Some("1.0.0".into()),
                batch_id: None,
                references: vec![],
                parent: None,
                extra: Default::default(),
            })
            .expect("insert memory claim")
    }

    fn draft(kind: MemoryKind, title: &str, body: &str) -> MemoryDraft {
        MemoryDraft::new(kind, title, body, "tester", ActorKind::Human)
    }

    fn write(store: &dyn AttributedStore, kind: MemoryKind, title: &str, body: &str) -> ItemId {
        insert_memory_item(store, &draft(kind, title, body)).expect("insert memory")
    }

    fn get(store: &dyn AttributedStore, id: ItemId) -> Item {
        store.get(id).expect("get").expect("item present")
    }

    fn edges_of(item: &Item, edge: EdgeType) -> Vec<ItemId> {
        let mut targets: Vec<ItemId> = item
            .references
            .iter()
            .filter(|r| r.edge_type == edge)
            .map(|r| r.target)
            .collect();
        targets.sort();
        targets
    }

    fn ids(entries: &[RecallEntry]) -> BTreeSet<String> {
        entries.iter().map(|e| e.id.clone()).collect()
    }

    // ─── Writing ─────────────────────────────────────────────────────────

    /// D39's field-AND-edge pair, in one assertion: what the payload says and
    /// what the graph says must be the same relation.
    #[test]
    fn an_insert_writes_the_payload_and_the_three_edge_kinds() {
        let store = open();
        let paper = subject_item(&store, "Rotation curves");
        let transcript = subject_item(&store, "Session transcript");
        let run = subject_item(&store, "Enrichment run");

        let mut d = draft(
            MemoryKind::Claim,
            "Flux column is mJy",
            "The 2018 catalogue's flux column is in mJy, not Jy.",
        );
        d.claim_type = Some("fact".into());
        d.confidence = Some(0.9);
        d.subject_refs = vec![paper.to_string()];
        d.evidence_refs = vec![transcript.to_string()];
        d.agent_run_ref = Some(run.to_string());
        d.agent_id = Some("counsel".into());

        let id = insert_memory_item(&store, &d).expect("insert");
        let item = get(&store, id);

        assert_eq!(item.schema, MEMORY_CLAIM_SCHEMA);
        assert_eq!(item.visibility, Visibility::Private);
        assert_eq!(item.version.as_deref(), Some("1.0.0"));
        assert_eq!(item.author, "tester");
        assert_eq!(item.author_kind, ActorKind::Human);

        // Flat payload — NOT a {title, data, body} nesting, because `title`
        // and `body` at the top level are what items_fts indexes.
        assert_eq!(
            string_field(&item, "title").as_deref(),
            Some("Flux column is mJy")
        );
        assert!(string_field(&item, "body").unwrap().contains("mJy"));
        assert_eq!(string_field(&item, "claim_type").as_deref(), Some("fact"));
        assert_eq!(float_field(&item, "confidence"), Some(0.9));
        assert_eq!(
            string_array_field(&item, "subject_refs"),
            Some(vec![paper.to_string()])
        );
        assert_eq!(string_field(&item, "agent_id").as_deref(), Some("counsel"));

        assert_eq!(edges_of(&item, EdgeType::RelatesTo), vec![paper]);
        assert_eq!(edges_of(&item, EdgeType::DerivedFrom), vec![transcript]);
        assert_eq!(edges_of(&item, EdgeType::ProducedBy), vec![run]);

        // And it is findable by full text with no index of its own.
        let hits = store.search_text(None, "catalogue flux", 10).unwrap();
        assert!(hits.iter().any(|h| h.id == id), "{hits:?}");
    }

    /// A stale evidence id must not cost the writer the whole memory, and must
    /// not vanish silently either: the payload keeps the assertion, the graph
    /// keeps only what resolves.
    #[test]
    fn refs_that_do_not_resolve_stay_in_the_payload_and_out_of_the_graph() {
        let store = open();
        let real = subject_item(&store, "Real");
        let ghost = Uuid::new_v4().to_string();

        let mut d = draft(MemoryKind::Claim, "Partial", "Half its refs are stale.");
        d.subject_refs = vec![real.to_string(), ghost.clone(), "not-a-uuid".into()];
        let id = insert_memory_item(&store, &d).expect("insert");
        let item = get(&store, id);

        assert_eq!(
            string_array_field(&item, "subject_refs"),
            Some(vec![real.to_string(), ghost, "not-a-uuid".into()]),
            "the payload records what the author asserted"
        );
        assert_eq!(
            edges_of(&item, EdgeType::RelatesTo),
            vec![real],
            "the graph records only what is actually there"
        );
    }

    #[test]
    fn a_deterministic_key_makes_the_write_idempotent() {
        let store = open();
        let mut d = draft(MemoryKind::Episode, "Recalibration", "Re-ran the fit.");
        d.deterministic_key = Some("session-7/episode-1".into());

        let first = insert_memory_item(&store, &d).expect("first");
        let before = get(&store, first);

        // A second pass with a DIFFERENT body must still be a no-op: the key
        // says "this is the same memory", and the alternative to leaving it
        // alone is a silent overwrite of whatever the first pass observed.
        let mut again = d.clone();
        again.body = "Something else entirely.".into();
        let second = insert_memory_item(&store, &again).expect("second");

        assert_eq!(first, second);
        let after = get(&store, first);
        assert_eq!(before.payload, after.payload);
        assert_eq!(before.logical_clock, after.logical_clock);
        assert_eq!(
            store
                .query(&ItemQuery {
                    schema: Some(MEMORY_EPISODE_SCHEMA),
                    ..Default::default()
                })
                .unwrap()
                .len(),
            1
        );
        assert_eq!(d.planned_id(), Some(first));
    }

    /// Two writers picking the same key for different kinds is a real bug, and
    /// overwriting one with the other would hide it forever.
    #[test]
    fn a_key_collision_across_kinds_is_rejected() {
        let store = open();
        let mut d = draft(MemoryKind::Claim, "A", "Body A.");
        d.deterministic_key = Some("shared-key".into());
        insert_memory_item(&store, &d).expect("first");

        let mut other = draft(MemoryKind::Episode, "B", "Body B.");
        other.deterministic_key = Some("shared-key".into());
        assert!(matches!(
            insert_memory_item(&store, &other),
            Err(StoreError::Validation(_))
        ));
    }

    #[test]
    fn a_memory_needs_both_a_title_and_a_body() {
        let store = open();
        assert!(matches!(
            insert_memory_item(&store, &draft(MemoryKind::Claim, "   ", "body")),
            Err(StoreError::Validation(_))
        ));
        assert!(matches!(
            insert_memory_item(&store, &draft(MemoryKind::Claim, "title", "  ")),
            Err(StoreError::Validation(_))
        ));
    }

    /// A confidence of 90 (a caller stating a percentage) must not become a
    /// ranking signal ninety times stronger than full confidence.
    #[test]
    fn confidence_is_clamped_into_the_unit_interval() {
        let store = open();
        let mut high = draft(MemoryKind::Claim, "High", "Stated as a percentage.");
        high.confidence = Some(90.0);
        let mut low = draft(MemoryKind::Claim, "Low", "Stated below zero somehow.");
        low.confidence = Some(-0.5);

        let high = insert_memory_item(&store, &high).unwrap();
        let low = insert_memory_item(&store, &low).unwrap();
        assert_eq!(float_field(&get(&store, high), "confidence"), Some(1.0));
        assert_eq!(float_field(&get(&store, low), "confidence"), Some(0.0));
    }

    /// Kind-specific fields ride in `extra`; the typed fields win a collision,
    /// so `extra` can never rewrite the body a caller passed explicitly.
    #[test]
    fn extra_fields_are_written_but_cannot_shadow_the_typed_ones() {
        let store = open();
        let mut d = draft(MemoryKind::Episode, "Fit", "It converged.");
        d.extra
            .insert("task_kind".into(), serde_json::json!("recalibrate"));
        d.extra.insert("quality".into(), serde_json::json!(0.8));
        d.extra
            .insert("applies_to".into(), serde_json::json!(["imbib", "imprint"]));
        d.extra.insert("body".into(), serde_json::json!("hijacked"));
        // The one that matters most: `agent_run_ref` has a mirror edge, and an
        // `extra` that could rewrite it would leave the payload and the graph
        // naming different runs.
        d.extra
            .insert("agent_run_ref".into(), serde_json::json!("hijacked"));

        let item = get(&store, insert_memory_item(&store, &d).expect("insert"));
        assert_eq!(
            string_field(&item, "task_kind").as_deref(),
            Some("recalibrate")
        );
        assert_eq!(float_field(&item, "quality"), Some(0.8));
        assert_eq!(
            string_array_field(&item, "applies_to"),
            Some(vec!["imbib".into(), "imprint".into()])
        );
        assert_eq!(
            string_field(&item, "body").as_deref(),
            Some("It converged.")
        );
        assert!(
            string_field(&item, "agent_run_ref").is_none(),
            "the draft set no agent_run_ref, so `extra` must not invent one \
             behind the edge-building pass's back"
        );
    }

    // ─── Heads and supersession ──────────────────────────────────────────

    #[test]
    fn claim_heads_excludes_superseded_and_no_recall_rows() {
        let store = open();
        let old = write(&store, MemoryKind::Claim, "Old", "The flux column is Jy.");
        let new = write(&store, MemoryKind::Claim, "New", "The flux column is mJy.");
        let quiet = write(&store, MemoryKind::Claim, "Quiet", "Withheld on request.");
        let plain = write(&store, MemoryKind::Claim, "Plain", "Still current.");

        supersede(
            &store,
            old,
            new,
            Some("wrong unit"),
            "tester",
            ActorKind::Human,
        )
        .unwrap();
        store
            .update(
                quiet,
                vec![FieldMutation::SetPayload(
                    NO_RECALL_FIELD.into(),
                    Value::Bool(true),
                )],
            )
            .unwrap();

        let heads: BTreeSet<ItemId> = claim_heads(&store, MEMORY_CLAIM_SCHEMA, 50)
            .unwrap()
            .into_iter()
            .collect();
        assert!(heads.contains(&new));
        assert!(heads.contains(&plain));
        assert!(!heads.contains(&old), "a superseded claim is not a head");
        assert!(!heads.contains(&quiet), "no_recall is withheld");

        // Kind-scoped: an episode is never a claim head.
        write(&store, MemoryKind::Episode, "Episode", "Unrelated.");
        assert_eq!(heads.len(), 2);
    }

    #[test]
    fn supersede_writes_a_reasoned_edge_and_leaves_the_old_row_alone() {
        let store = open();
        let old = write(&store, MemoryKind::Claim, "Old", "Flux is Jy.");
        let new = write(&store, MemoryKind::Claim, "New", "Flux is mJy.");
        let before = get(&store, old);

        supersede(
            &store,
            old,
            new,
            Some("unit corrected against the header"),
            "counsel",
            ActorKind::Agent,
        )
        .unwrap();

        let edge = get(&store, new)
            .references
            .into_iter()
            .find(|r| r.edge_type == EdgeType::Supersedes)
            .expect("Supersedes edge on the NEW row");
        assert_eq!(edge.target, old, "the edge points from new to old");
        assert_eq!(
            edge.metadata
                .as_ref()
                .and_then(|m| m.get(SUPERSEDE_REASON_KEY)),
            Some(&Value::String("unit corrected against the header".into()))
        );

        let after = get(&store, old);
        assert_eq!(before.payload, after.payload, "the old row is untouched");
        assert!(after.references.is_empty());
    }

    #[test]
    fn supersede_rejects_self_supersession_and_missing_targets() {
        let store = open();
        let a = write(&store, MemoryKind::Claim, "A", "Body.");
        assert!(matches!(
            supersede(&store, a, a, None, "tester", ActorKind::Human),
            Err(StoreError::Validation(_))
        ));
        assert!(matches!(
            supersede(&store, Uuid::new_v4(), a, None, "tester", ActorKind::Human),
            Err(StoreError::NotFound(_))
        ));
    }

    #[test]
    fn confirm_bumps_the_count_and_stamps_the_time() {
        let store = open();
        let id = write(&store, MemoryKind::Claim, "Fact", "Still true.");
        assert_eq!(confirmations_of(&get(&store, id)), 0);
        assert!(string_field(&get(&store, id), LAST_CONFIRMED_FIELD).is_none());

        assert_eq!(confirm(&store, id, "counsel", ActorKind::Agent).unwrap(), 1);
        assert_eq!(confirm(&store, id, "tester", ActorKind::Human).unwrap(), 2);

        let item = get(&store, id);
        assert_eq!(confirmations_of(&item), 2);
        assert!(string_field(&item, LAST_CONFIRMED_FIELD).is_some());
        assert!(matches!(
            confirm(&store, Uuid::new_v4(), "tester", ActorKind::Human),
            Err(StoreError::NotFound(_))
        ));
    }

    /// A mistyped id must not decorate an arbitrary record with confirmation
    /// fields, or link it into the supersession graph.
    #[test]
    fn confirm_and_supersede_refuse_a_non_memory_target() {
        let store = open();
        let plain = subject_item(&store, "Manuscript");
        let memory = write(&store, MemoryKind::Claim, "M", "A memory body.");

        assert!(matches!(
            confirm(&store, plain, "tester", ActorKind::Human),
            Err(StoreError::Validation(_))
        ));
        assert!(
            !get(&store, plain).payload.contains_key(CONFIRMATIONS_FIELD),
            "the refused confirm must write nothing"
        );

        assert!(matches!(
            supersede(&store, plain, memory, None, "tester", ActorKind::Human),
            Err(StoreError::Validation(_))
        ));
        assert!(matches!(
            supersede(&store, memory, plain, None, "tester", ActorKind::Human),
            Err(StoreError::Validation(_))
        ));
        assert!(
            get(&store, memory).references.is_empty() && get(&store, plain).references.is_empty(),
            "the refused supersessions must write no edge in either direction"
        );
    }

    // ─── The gate ────────────────────────────────────────────────────────

    #[test]
    fn the_gate_confirms_a_restatement_and_inserts_something_new() {
        let store = open();
        let body = "The 2018 catalogue flux column is in millijansky rather than jansky.";
        let existing = write(&store, MemoryKind::Claim, "Flux units", body);

        // The same memory, restated with an identical body.
        let same = draft(MemoryKind::Claim, "Flux units again", body);
        assert_eq!(
            gate_fts(&store, &same, GATE_CONFIRM_THRESHOLD).unwrap(),
            GateOutcome::Confirm(existing)
        );

        // A memory about something else entirely.
        let novel = draft(
            MemoryKind::Claim,
            "Build flags",
            "Debug builds link the arm64-only skinny frameworks.",
        );
        assert_eq!(
            gate_fts(&store, &novel, GATE_CONFIRM_THRESHOLD).unwrap(),
            GateOutcome::Insert
        );

        // Same prose, different KIND: an episode is never a duplicate claim,
        // because the gate is scoped to the draft's own schema.
        let other_kind = draft(MemoryKind::Episode, "Flux units", body);
        assert_eq!(
            gate_fts(&store, &other_kind, GATE_CONFIRM_THRESHOLD).unwrap(),
            GateOutcome::Insert
        );
    }

    /// "The user prefers Typst" and "Typst is faster" can be nearly the same
    /// sentence and are not the same memory.
    #[test]
    fn the_gate_never_merges_two_different_claim_types() {
        let store = open();
        let body = "Typst compiles the manuscript faster than the LaTeX toolchain does.";
        let mut stored = draft(MemoryKind::Claim, "Typst speed", body);
        stored.claim_type = Some("fact".into());
        insert_memory_item(&store, &stored).unwrap();

        let mut preference = draft(MemoryKind::Claim, "Typst preference", body);
        preference.claim_type = Some("preference".into());
        assert_eq!(
            gate_fts(&store, &preference, GATE_CONFIRM_THRESHOLD).unwrap(),
            GateOutcome::Insert
        );

        // An unset type on either side is compatible — the absence of a
        // declaration is not a declaration of difference.
        let untyped = draft(MemoryKind::Claim, "Typst", body);
        assert!(matches!(
            gate_fts(&store, &untyped, GATE_CONFIRM_THRESHOLD).unwrap(),
            GateOutcome::Confirm(_)
        ));
    }

    /// Confirming a retracted memory would strengthen the version its
    /// replacement already corrected.
    #[test]
    fn the_gate_never_confirms_a_superseded_memory() {
        let store = open();
        let body = "The pipeline writes its checkpoints to the shared container.";
        let old = write(&store, MemoryKind::Claim, "Checkpoints", body);
        let new = write(
            &store,
            MemoryKind::Claim,
            "Checkpoints",
            "The pipeline writes its checkpoints to a per-run temp directory.",
        );
        supersede(&store, old, new, Some("moved"), "tester", ActorKind::Human).unwrap();

        let restatement = draft(MemoryKind::Claim, "Checkpoints", body);
        assert_eq!(
            gate_fts(&store, &restatement, GATE_CONFIRM_THRESHOLD).unwrap(),
            GateOutcome::Insert
        );
    }

    /// `forget` withholds a row from recall; the gate must not quietly
    /// strengthen it either. A re-remember of the same prose falls through
    /// to a fresh insert.
    #[test]
    fn the_gate_never_confirms_a_withheld_memory() {
        let store = open();
        let body = "The nightly export writes into the scratch volume.";
        let hidden = write(&store, MemoryKind::Claim, "Export", body);
        store
            .update(
                hidden,
                vec![FieldMutation::SetPayload(
                    NO_RECALL_FIELD.into(),
                    Value::Bool(true),
                )],
            )
            .unwrap();

        let restatement = draft(MemoryKind::Claim, "Export", body);
        assert_eq!(
            gate_fts(&store, &restatement, GATE_CONFIRM_THRESHOLD).unwrap(),
            GateOutcome::Insert
        );
    }

    /// The eligibility policy in one pass: only a live, non-withheld memory
    /// row absorbs, and everything else answers `false` rather than erroring
    /// — the gates walk candidate lists, and one bad id must not abort a
    /// whole gate.
    #[test]
    fn absorbs_confirmation_screens_out_every_ineligible_row() {
        let store = open();
        let live = write(&store, MemoryKind::Claim, "Live", "Still current.");
        let old = write(&store, MemoryKind::Claim, "Old", "Replaced later.");
        supersede(&store, old, live, None, "tester", ActorKind::Human).unwrap();
        let withheld = write(&store, MemoryKind::Claim, "Quiet", "Withheld on request.");
        store
            .update(
                withheld,
                vec![FieldMutation::SetPayload(
                    NO_RECALL_FIELD.into(),
                    Value::Bool(true),
                )],
            )
            .unwrap();
        let plain = subject_item(&store, "Not a memory");

        assert!(absorbs_confirmation(&store, live, None).unwrap());
        assert!(!absorbs_confirmation(&store, old, None).unwrap());
        assert!(!absorbs_confirmation(&store, withheld, None).unwrap());
        assert!(!absorbs_confirmation(&store, plain, None).unwrap());
        assert!(!absorbs_confirmation(&store, Uuid::new_v4(), None).unwrap());
    }

    #[test]
    fn absorbs_confirmation_applies_the_claim_type_rule() {
        let store = open();
        let mut d = draft(MemoryKind::Claim, "Typed", "A typed claim body.");
        d.claim_type = Some("fact".into());
        let typed = insert_memory_item(&store, &d).unwrap();

        assert!(absorbs_confirmation(&store, typed, Some("fact")).unwrap());
        assert!(absorbs_confirmation(&store, typed, None).unwrap());
        assert!(!absorbs_confirmation(&store, typed, Some("preference")).unwrap());
    }

    /// The policy is a per-id predicate, never membership in a head page:
    /// `claim_heads` caps at [`MAX_RECALL_LIMIT`], so a well-established head
    /// older than the newest 200 is absent from that page while being exactly
    /// the row a gate must still confirm.
    #[test]
    fn an_old_head_beyond_the_recency_page_still_absorbs() {
        let store = open();
        let t0 = chrono::Utc::now() - chrono::Duration::hours(1);
        let old = memory_claim_at(&store, "Old head", "The oldest still-true claim.", t0);
        for n in 0..MAX_RECALL_LIMIT {
            memory_claim_at(
                &store,
                &format!("Filler {n}"),
                &format!("Filler body number {n}."),
                t0 + chrono::Duration::seconds(n as i64 + 1),
            );
        }

        let page: BTreeSet<ItemId> = claim_heads(&store, MEMORY_CLAIM_SCHEMA, MAX_RECALL_LIMIT)
            .unwrap()
            .into_iter()
            .collect();
        assert!(
            !page.contains(&old),
            "the scenario needs the old head off the recency page"
        );
        assert!(absorbs_confirmation(&store, old, None).unwrap());
    }

    #[test]
    fn near_duplicates_are_scored_and_ordered_by_overlap() {
        let store = open();
        let exact = "The zero point is applied before the aperture correction.";
        let close = write(&store, MemoryKind::Claim, "Order", exact);
        let looser = write(
            &store,
            MemoryKind::Claim,
            "Order, roughly",
            "The zero point is applied before the aperture correction in every band.",
        );

        let dups = fts_near_duplicates(&store, MEMORY_CLAIM_SCHEMA, exact, 5).unwrap();
        assert_eq!(dups.len(), 2, "{dups:?}");
        assert_eq!(dups[0].id, close);
        assert_eq!(dups[0].overlap, 1.0);
        assert_eq!(dups[1].id, looser);
        assert!(dups[1].overlap < 1.0 && dups[1].overlap > 0.5);
        assert_eq!(dups[0].title, "Order");

        // Nothing to search for is not an error.
        assert!(
            fts_near_duplicates(&store, MEMORY_CLAIM_SCHEMA, "  (((  ", 5)
                .unwrap()
                .is_empty()
        );
    }

    // ─── Recall ──────────────────────────────────────────────────────────

    #[test]
    fn recall_round_trips_a_memory_by_its_text() {
        let store = open();
        let paper = subject_item(&store, "Rotation curves");
        let mut d = draft(
            MemoryKind::Claim,
            "Flux units",
            "The catalogue flux column is in millijansky.",
        );
        d.claim_type = Some("fact".into());
        d.confidence = Some(0.8);
        d.subject_refs = vec![paper.to_string()];
        let id = insert_memory_item(&store, &d).unwrap();
        write(
            &store,
            MemoryKind::Claim,
            "Unrelated",
            "Nothing to do with it.",
        );

        let found = recall(&store, "millijansky", &RecallOptions::default()).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        let entry = &found[0];
        assert_eq!(entry.id, id.to_string());
        assert_eq!(entry.schema_ref, MEMORY_CLAIM_SCHEMA.as_str());
        assert_eq!(entry.title, "Flux units");
        assert_eq!(entry.claim_type.as_deref(), Some("fact"));
        assert_eq!(entry.confidence, Some(0.8));
        assert_eq!(entry.subject_refs, vec![paper.to_string()]);
        assert!(entry.score > 0.0);
    }

    #[test]
    fn recall_withholds_superseded_and_no_recall_rows() {
        let store = open();
        let old = write(
            &store,
            MemoryKind::Claim,
            "Old",
            "Aperture radius is five pixels.",
        );
        let new = write(
            &store,
            MemoryKind::Claim,
            "New",
            "Aperture radius is eight pixels.",
        );
        let quiet = write(
            &store,
            MemoryKind::Claim,
            "Quiet",
            "Aperture radius is private.",
        );
        supersede(&store, old, new, None, "tester", ActorKind::Human).unwrap();
        store
            .update(
                quiet,
                vec![FieldMutation::SetPayload(
                    NO_RECALL_FIELD.into(),
                    Value::Bool(true),
                )],
            )
            .unwrap();

        let found = ids(&recall(&store, "aperture radius", &RecallOptions::default()).unwrap());
        assert_eq!(found, BTreeSet::from([new.to_string()]));

        // include_superseded brings back the retracted one — but never the
        // withheld one, which is a different decision.
        let widened = ids(&recall(
            &store,
            "aperture radius",
            &RecallOptions {
                include_superseded: true,
                ..Default::default()
            },
        )
        .unwrap());
        assert_eq!(
            widened,
            BTreeSet::from([old.to_string(), new.to_string()]),
            "no_recall is not lifted by include_superseded"
        );
    }

    #[test]
    fn recall_narrows_to_one_subject_and_one_kind() {
        let store = open();
        let paper = subject_item(&store, "Paper");
        let other = subject_item(&store, "Other");

        let mut about_paper = draft(MemoryKind::Claim, "About the paper", "Calibration detail.");
        about_paper.subject_refs = vec![paper.to_string()];
        let about_paper = insert_memory_item(&store, &about_paper).unwrap();

        let mut about_other = draft(MemoryKind::Claim, "About the other", "Calibration detail.");
        about_other.subject_refs = vec![other.to_string()];
        insert_memory_item(&store, &about_other).unwrap();

        let mut episode = draft(MemoryKind::Episode, "Episode", "Calibration detail.");
        episode.subject_refs = vec![paper.to_string()];
        let episode = insert_memory_item(&store, &episode).unwrap();

        let scoped = ids(&recall(
            &store,
            "calibration",
            &RecallOptions {
                subject_ref: Some(paper.to_string()),
                ..Default::default()
            },
        )
        .unwrap());
        assert_eq!(
            scoped,
            BTreeSet::from([about_paper.to_string(), episode.to_string()])
        );

        let claims_only = ids(&recall(
            &store,
            "calibration",
            &RecallOptions {
                subject_ref: Some(paper.to_string()),
                kinds: vec![MemoryKind::Claim],
                ..Default::default()
            },
        )
        .unwrap());
        assert_eq!(claims_only, BTreeSet::from([about_paper.to_string()]));
    }

    /// An empty query is "the most relevant memories, with no text to go on",
    /// not an empty result — that is what lets `brief` be a composition of
    /// `recall` rather than a second retrieval path.
    #[test]
    fn an_empty_query_lists_heads_instead_of_nothing() {
        let store = open();
        let a = write(&store, MemoryKind::Claim, "A", "First.");
        let b = write(&store, MemoryKind::Claim, "B", "Second.");
        let superseded = write(&store, MemoryKind::Claim, "C", "Third.");
        supersede(&store, superseded, b, None, "tester", ActorKind::Human).unwrap();

        let found = ids(&recall(&store, "", &RecallOptions::default()).unwrap());
        assert_eq!(found, BTreeSet::from([a.to_string(), b.to_string()]));

        let widened = ids(&recall(
            &store,
            "   ",
            &RecallOptions {
                include_superseded: true,
                ..Default::default()
            },
        )
        .unwrap());
        assert!(widened.contains(&superseded.to_string()));
    }

    #[test]
    fn recall_limits_are_clamped_and_zero_means_the_default() {
        let store = open();
        for n in 0..(DEFAULT_RECALL_LIMIT + 5) {
            write(
                &store,
                MemoryKind::Claim,
                &format!("Claim {n}"),
                &format!("Widget note number {n}."),
            );
        }
        assert_eq!(
            recall(
                &store,
                "widget",
                &RecallOptions {
                    limit: 3,
                    ..Default::default()
                }
            )
            .unwrap()
            .len(),
            3
        );
        assert_eq!(
            recall(&store, "widget", &RecallOptions::default())
                .unwrap()
                .len(),
            DEFAULT_RECALL_LIMIT as usize,
            "0 would mean the default too; RecallOptions::default() states it"
        );
        assert_eq!(
            recall(
                &store,
                "widget",
                &RecallOptions {
                    limit: 0,
                    ..Default::default()
                }
            )
            .unwrap()
            .len(),
            DEFAULT_RECALL_LIMIT as usize
        );
    }

    // ─── Brief ───────────────────────────────────────────────────────────

    #[test]
    fn a_brief_is_instructions_then_claims_then_episodes() {
        let store = open();
        let instruction = write(
            &store,
            MemoryKind::Instruction,
            "No shell scans",
            "Never scan the shared container from a shell.",
        );
        let claim = write(
            &store,
            MemoryKind::Claim,
            "Flux units",
            "The catalogue flux column is in millijansky.",
        );
        let episode = write(
            &store,
            MemoryKind::Episode,
            "Recalibration",
            "Re-ran the fit with the 2019 zero point.",
        );

        let sections = brief(&store, &BriefOptions::default()).unwrap();
        assert_eq!(
            sections.iter().map(|s| s.kind.as_str()).collect::<Vec<_>>(),
            vec![
                MEMORY_INSTRUCTION_SCHEMA.as_str(),
                MEMORY_CLAIM_SCHEMA.as_str(),
                MEMORY_EPISODE_SCHEMA.as_str()
            ],
            "instructions bind what follows, so they lead"
        );
        assert_eq!(
            ids(&sections[0].entries),
            BTreeSet::from([instruction.to_string()])
        );
        assert_eq!(
            ids(&sections[1].entries),
            BTreeSet::from([claim.to_string()])
        );
        assert_eq!(
            ids(&sections[2].entries),
            BTreeSet::from([episode.to_string()])
        );
    }

    /// A topic scopes the CLAIMS. An instruction that does not mention the
    /// topic still binds, and dropping it would drop exactly the constraints
    /// the current work was not expecting.
    #[test]
    fn a_topic_scopes_the_claims_but_never_the_instructions() {
        let store = open();
        let instruction = write(
            &store,
            MemoryKind::Instruction,
            "No shell scans",
            "Never scan the shared container from a shell.",
        );
        let on_topic = write(
            &store,
            MemoryKind::Claim,
            "Flux units",
            "The catalogue flux column is in millijansky.",
        );
        write(
            &store,
            MemoryKind::Claim,
            "Build flags",
            "Debug builds link the skinny frameworks.",
        );

        let sections = brief(
            &store,
            &BriefOptions {
                topic: Some("flux".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            ids(&sections[0].entries),
            BTreeSet::from([instruction.to_string()]),
            "the instruction does not mention flux and still appears"
        );
        assert_eq!(
            ids(&sections[1].entries),
            BTreeSet::from([on_topic.to_string()])
        );
    }

    /// Every section is returned every time, so a renderer can rely on the
    /// ordering rather than searching for its heading.
    #[test]
    fn an_empty_store_still_returns_all_three_sections() {
        let store = open();
        let sections = brief(&store, &BriefOptions::default()).unwrap();
        assert_eq!(sections.len(), 3);
        assert!(sections.iter().all(|s| s.entries.is_empty()));
    }

    #[test]
    fn brief_caps_each_section_independently() {
        let store = open();
        for n in 0..6 {
            write(
                &store,
                MemoryKind::Claim,
                &format!("Claim {n}"),
                &format!("Claim body {n}."),
            );
            write(
                &store,
                MemoryKind::Instruction,
                &format!("Rule {n}"),
                &format!("Rule body {n}."),
            );
        }
        let sections = brief(
            &store,
            &BriefOptions {
                max_entries: 2,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(sections[0].entries.len(), 2);
        assert_eq!(sections[1].entries.len(), 2);
    }
}
