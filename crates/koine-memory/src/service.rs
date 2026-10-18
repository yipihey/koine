//! `MemoryService` — the agent-facing surface of the suite memory kernel
//! (ADR-0028, `docs/ADR-0028-memory-kernel.md`).
//!
//! Every `#[impress_method]` here is a thin wrapper over
//! `crate::ops`: this crate converts arguments, maps the
//! kernel's types to DTOs, and turns errors into `ok: false` plus the
//! kernel's own message — the same shape `impress-store-service`'s
//! `TriageService` and `StoreQueryService` use, and for the same reason: no
//! domain logic lives here, so the CLI, MCP and impel all call the identical
//! behaviour a headless `cargo test` already proves.
//!
//! **Two tiers, one always on.** The FTS tier (full-text dedup gate,
//! full-text retrieval, the deterministic ranker) is always live and needs
//! nothing configured. The vector tier (ADR-0028 D6) turns itself on when
//! there is something for it to read — the sidecar already holds
//! `memory-item` vectors for the model this process would use — and COMPOSES
//! with the FTS tier rather than replacing it, per the kernel's own module docs
//! (`memory_ops::MemoryCandidate::vector_similarity`): `remember`'s dedup
//! gate runs FTS first and only falls to the (higher-threshold) vector check
//! when FTS found nothing; `recall` re-ranks the SAME page FTS already
//! selected rather than widening it. [`DefaultMemoryService::memory_status`]
//! reports which tier is actually running — see its own doc for the exact
//! states — rather than a static claim either way.
//!
//! Presence-gating replaced a bare `IMPRESS_MEMORY_VECTORS=1` opt-in, which
//! no process in the suite ever set: `impel-taskd`'s embed sweep wrote
//! vectors nightly and every recall in every app, daemon and MCP session
//! stayed FTS-only, permanently. `IMPRESS_MEMORY_VECTORS` still overrides in
//! both directions — `1` forces the tier on before any vector exists, `0`
//! forces it off on a machine that has them.
//!
//! The vector tier is lazy and process-wide: it is built at most once, on
//! the first `remember` or `recall` call that finds the gate open,
//! and any failure to build it (missing sidecar, model load failure) turns
//! it off for the rest of the process with one `stderr` line — imitating
//! `impress-mcp::tools::SEMANTIC_UNAVAILABLE`'s degraded-mode discipline.
//! `memory_status` never triggers that build (it would pay for a
//! `SemanticSearch` model load just to answer a status query); it only ever
//! peeks at whatever state already exists and opens the sidecar (cheaply,
//! read-only in effect) to count vectors.
//!
//! # Attribution
//!
//! Every write this service makes is stamped `author: "impress-memory-service"`,
//! `author_kind: `[`ActorKind::System`]. None of the trait methods take an
//! author argument — a caller may be a human at the CLI, an MCP client, or an
//! impel agent, and the service has no reliable way to tell which — so, like
//! `impress-store-service`'s own automated writers
//! (`docs_import_service::new_manuscript`, `source_service::put_*`), it
//! attributes its writes to itself rather than guessing. This mirrors, not
//! invents, the established convention for a service-authored write in this
//! crate family.

#[cfg(feature = "embeddings")]
use std::collections::HashMap;
use std::sync::Arc;

use crate::ops::{self as memory_ops, GateOutcome, MemoryDraft, MemoryKind};
#[cfg(feature = "embeddings")]
use crate::vector::*;
use impress_item::{
    ActorKind, AttributedStore, ItemId, OperationIntent, OperationSpec, OperationType,
    RetentionTier, StoreError, Value,
};
use impress_service_core::async_trait;
use impress_service_macros::{impress_service, impress_service_impl};
#[cfg(all(test, feature = "embeddings"))]
use koine_embeddings::EmbeddingStore;
use serde::{Deserialize, Serialize};

#[allow(unused_imports)]
use impress_service_macros::impress_method;

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// Result of [`MemoryService::remember`].
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RememberResult {
    pub ok: bool,
    /// `"inserted"` | `"confirmed"`. Empty when `ok` is false.
    pub action: String,
    /// The id of the memory now representing this claim: the freshly written
    /// row when `action` is `"inserted"`, or the existing row when
    /// `"confirmed"`. Empty when `ok` is false.
    pub claim_id: String,
    /// The near-duplicate the dedup gate matched against, when `action` is
    /// `"confirmed"` (equal to `claim_id` in that case). Empty when
    /// `"inserted"` — nothing was matched — and empty when `ok` is false.
    pub matched_id: String,
    /// Which dedup gate produced the match: `"fts"` (token overlap ≥
    /// `memory_ops::GATE_CONFIRM_THRESHOLD`) or `"vector"` (embedding cosine
    /// ≥ [`VECTOR_CONFIRM_THRESHOLD`], only possible when the vector tier is
    /// live). Empty when `action` is `"inserted"` or `ok` is false.
    ///
    /// Additive field (ADR-0028 D6, phase P6b) — `#[serde(default)]` so a
    /// value serialized before this field existed still deserializes.
    #[serde(default)]
    pub matched_via: String,
    pub message: String,
}

/// Result of [`MemoryService::supersede_claim`].
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SupersedeResult {
    pub ok: bool,
    /// The replacement row's id. On success, the row now representing the
    /// memory. On the one partial failure this verb has — the replacement WAS
    /// written but recording the supersession edge failed — `ok` is `false`
    /// and this still names the written row rather than pretending nothing
    /// happened (the old row simply remains a head alongside it, and calling
    /// again would write a second replacement). Empty when nothing was
    /// written at all.
    pub new_id: String,
    /// The superseded row, echoed back. It is left completely untouched —
    /// audit and `include_superseded` recalls can still see it.
    pub old_id: String,
    pub message: String,
}

/// One recalled memory, hydrated and scored.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RecallEntryDto {
    /// Lowercase UUID string.
    pub id: String,
    /// One of the three memory schema refs (`memory/claim@1.0.0`, …).
    pub schema_ref: String,
    pub title: String,
    /// The memory itself. A pathologically large stored body arrives cut at
    /// [`MAX_BODY_BYTES`] with a trailing `… [truncated]` marker.
    pub body: String,
    /// Only meaningful when `schema_ref` is a claim.
    pub claim_type: Option<String>,
    /// 0.0–1.0. Absent means the author left it unstated, not zero.
    pub confidence: Option<f64>,
    /// How many times this memory has been independently re-observed.
    pub confirmations: u32,
    /// `Item::modified`, epoch milliseconds.
    pub modified_ms: i64,
    /// The ranker's score for this entry, within this call. Comparable across
    /// entries of the SAME call only — not a stable identity of the memory.
    pub score: f64,
    /// Lowercase UUIDs of the items this memory is about.
    pub subject_refs: Vec<String>,
}

/// Hard cap on one `body` crossing the DTO boundary — recall entries and
/// brief sections both — mirroring `impress-store-service`'s
/// `MAX_PAYLOAD_BYTES` (32 KiB) precedent for `get_item`: a recall page feeds
/// a context window or a tool result, and one pathological memory body must
/// not blow either up. Structured truncation, not rejection — the entry still
/// arrives, marked.
pub const MAX_BODY_BYTES: usize = 32 * 1024;

/// Appended to a body cut at [`MAX_BODY_BYTES`], so a consumer can tell a
/// truncated body from one that merely ends abruptly.
const BODY_TRUNCATION_MARKER: &str = "… [truncated]";

/// `body` unchanged when it fits; otherwise cut to [`MAX_BODY_BYTES`] on a
/// UTF-8 character boundary — a byte-exact cut could split a codepoint and
/// yield an invalid string — with [`BODY_TRUNCATION_MARKER`] appended.
fn cap_body(body: String) -> String {
    if body.len() <= MAX_BODY_BYTES {
        return body;
    }
    let mut cut = MAX_BODY_BYTES;
    while cut > 0 && !body.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = body[..cut].to_string();
    out.push_str(BODY_TRUNCATION_MARKER);
    out
}

impl From<memory_ops::RecallEntry> for RecallEntryDto {
    fn from(e: memory_ops::RecallEntry) -> Self {
        Self {
            id: e.id,
            schema_ref: e.schema_ref,
            title: e.title,
            body: cap_body(e.body),
            claim_type: e.claim_type,
            confidence: e.confidence.map(|f| f as f64),
            confirmations: e.confirmations,
            modified_ms: e.modified_ms,
            score: e.score as f64,
            subject_refs: e.subject_refs,
        }
    }
}

/// Result of [`MemoryService::recall`].
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RecallResult {
    pub ok: bool,
    /// Best match first.
    pub entries: Vec<RecallEntryDto>,
    pub message: String,
}

/// One section of a [`MemoryService::memory_brief`].
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BriefSectionDto {
    /// The section's schema ref — which of the three memory kinds this is.
    pub kind: String,
    /// Best match first. May be empty; every section is present even when
    /// empty, so a renderer can rely on position rather than search for a
    /// heading.
    pub entries: Vec<RecallEntryDto>,
}

impl From<memory_ops::BriefSection> for BriefSectionDto {
    fn from(s: memory_ops::BriefSection) -> Self {
        Self {
            kind: s.kind,
            entries: s.entries.into_iter().map(RecallEntryDto::from).collect(),
        }
    }
}

/// Result of [`MemoryService::memory_brief`].
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BriefResult {
    pub ok: bool,
    /// Instructions, then claims, then episodes — always all three, in that
    /// order. Read this for the structured fields (ids, confidence,
    /// confirmations); read `text` for prose.
    pub sections: Vec<BriefSectionDto>,
    /// A compact markdown rendering of `sections` — `### Instructions` /
    /// `### Claims` / `### Episodes` headings (only for non-empty sections),
    /// each entry as `- **title** — body… \`[impress-item:<id>]\``, body cut
    /// to about 200 characters. Ready to paste or inject directly into a
    /// system prompt or session briefing.
    pub text: String,
    pub message: String,
}

/// Result of a single-item action: [`MemoryService::confirm_claim`] and
/// [`MemoryService::forget`].
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ActionResult {
    pub ok: bool,
    /// The item acted on, echoed back.
    pub id: String,
    pub message: String,
}

/// Row counts for one memory schema.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SchemaCountDto {
    pub schema_ref: String,
    /// Current, non-superseded, non-withheld rows — capped at
    /// `memory_ops::MAX_RECALL_LIMIT` (200); a store with more heads than
    /// that in one schema reports the cap, not the true count.
    pub heads: u32,
    /// Every row of this schema, superseded and withheld included.
    pub total: u32,
}

/// Result of [`MemoryService::memory_status`].
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StatusResult {
    pub ok: bool,
    /// One entry per memory schema (claim, episode, instruction).
    pub schemas: Vec<SchemaCountDto>,
    /// Fraction (0.0–1.0) of memory items with a same-model vector in the
    /// vector-tier sidecar, i.e. `N / M` from the `vector_tier` string
    /// below. `0.0` when there is nothing embedded, or when
    /// `IMPRESS_MEMORY_VECTORS` disables the tier; real whenever vectors
    /// exist — computed by opening the sidecar and counting, which needs no
    /// live embedder, so this is accurate even in the
    /// `"initializing lazily"` state.
    pub embedding_coverage: f64,
    /// One of five states, cheapest-to-most-configured:
    /// `"off (IMPRESS_MEMORY_VECTORS disables it)"` — explicitly turned off;
    /// `"idle (no memory vectors embedded yet …)"` — nothing to read yet, so
    /// the tier has not started; it will on its own once the embed sweep
    /// writes the first memory vector;
    /// `"initializing lazily"` — the gate is open, but no `remember`/`recall`
    /// has run in this process yet, so the tier has not tried to build itself;
    /// `"live (model <id>, N/M items embedded)"` — built successfully;
    /// `"unavailable: <reason>"` — it tried and failed (logged once to
    /// `stderr` when that happened). See the module docs for the full
    /// two-tier picture.
    pub vector_tier: String,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Suite-scoped agent memory: write, recall and brief over `memory/claim`,
/// `memory/episode` and `memory/instruction` rows in the shared store
/// (ADR-0028).
///
/// A **claim** is what is true ("the 2018 catalogue's flux column is in mJy,
/// not Jy"); an **episode** is what happened ("re-ran the fit with the 2019
/// zero-point, converged in 3 minutes"); an **instruction** is what to do
/// ("never open the shared container from a shell"), from the user, and
/// retired only by supersession or `forget` — never by an agent deciding it
/// no longer applies.
///
/// The FTS tier — full-text retrieval, a token-overlap dedup gate, and a
/// deterministic ranker over relevance, confirmations, recency, author and
/// confidence — is always live. An opt-in vector tier (`IMPRESS_MEMORY_VECTORS=1`,
/// see `memory_status`) composes with it in `remember` (a second,
/// higher-threshold dedup check) and `recall` (an embedding-similarity
/// re-ranking of the same page FTS already selected).
#[impress_service]
pub trait MemoryService: Send + Sync + 'static {
    /// Write a memory, or — if a near-duplicate already exists — confirm it
    /// instead of writing a second copy.
    ///
    /// `kind` is one of `"claim"`, `"episode"` or `"instruction"`; anything
    /// else fails with `ok: false`. Before writing, this runs the dedup gate:
    /// if an existing memory of the SAME kind has near-identical prose (token
    /// overlap ≥ 0.85) and a compatible `claim_type`, that row is CONFIRMED
    /// (its `confirmations` count goes up) instead of a duplicate being
    /// inserted — `action` reports which happened, and `matched_id` names the
    /// row that was matched. Call this whenever something is worth keeping
    /// across sessions; re-remembering the same fact is safe and cheap by
    /// design; it is very much not free to fail to remember something, so
    /// default to calling this rather than deciding for yourself whether a
    /// fact is "new enough".
    ///
    /// When the vector tier is live (`IMPRESS_MEMORY_VECTORS=1`) and the FTS
    /// gate above found nothing, a SECOND check runs: embed the draft and
    /// compare against stored memory-item vectors by cosine similarity. A
    /// paraphrase the FTS gate's token overlap missed entirely can still
    /// score high here, so this bar is deliberately higher than the FTS
    /// one's (`VECTOR_CONFIRM_THRESHOLD` = 0.92) — see its doc for why. When
    /// it fires, `matched_via` reports `"vector"` instead of `"fts"`.
    ///
    /// `claim_type` (meaningful only when `kind` is `"claim"`: `"fact"` |
    /// `"preference"` | `"method"` | `"decision"` | `"result"`, or any other
    /// label — the vocabulary is open) and `confidence` (0.0–1.0) may be left
    /// as an empty string / a negative number respectively to mean
    /// "unstated" — that is NOT the same as zero and is not scored as zero.
    /// A confidence above 1.0 is stored as 1.0, never as a ranking signal
    /// stronger than full confidence.
    /// `subject_refs` are the lowercase-UUID ids of items this memory is
    /// ABOUT; `evidence_refs` are the ids it was DERIVED FROM (a transcript,
    /// a paper, a run). Either may be empty. An id that does not resolve to a
    /// live item is kept in the record but silently skipped as a graph edge
    /// — it does not fail the write.
    #[impress_method(effects(reads = ["memory/claim@1.0.0", "memory/episode@1.0.0", "memory/instruction@1.0.0"], writes = ["memory/claim@1.0.0", "memory/episode@1.0.0", "memory/instruction@1.0.0"]))]
    #[impress_example(
        name = "new-claim",
        args = r#"{"kind":"claim","title":"G3 calibration units","body":"G3 calibration fluxes use millijanskys.","claim_type":"fact","confidence":0.9,"subject_refs":[],"evidence_refs":[]}"#,
        expect = r#"{"ok":true,"action":"inserted","matched_id":""}"#
    )]
    #[allow(clippy::too_many_arguments)]
    async fn remember(
        &self,
        kind: String,
        title: String,
        body: String,
        claim_type: String,
        confidence: f64,
        subject_refs: Vec<String>,
        evidence_refs: Vec<String>,
    ) -> RememberResult;

    /// Retrieve memories, ranked by relevance, confirmations, recency, author
    /// and confidence.
    ///
    /// `query` is treated as words, not FTS5 syntax. An EMPTY query is not an
    /// empty result — it returns the most relevant heads by recency, which is
    /// what lets `memory_brief` be a composition of this call rather than a
    /// second retrieval path. Pass a non-empty `subject_ref` (a lowercase
    /// UUID) to narrow to memories about one item. `limit` of `0` means the
    /// kernel default (20); values are clamped to 200. Superseded memories
    /// (retracted by a `supersede_claim` correction) are withheld unless
    /// `include_superseded` is true; memories withheld by `forget` are never
    /// returned, regardless.
    ///
    /// When the vector tier is live, this page is RE-RANKED — never widened
    /// — by embedding `query` once and scoring every entry that has a stored
    /// vector by cosine similarity; entries with no vector keep their
    /// FTS/recency position. Off, or against a store with no memory-item
    /// vectors embedded yet, results are byte-identical to the FTS-only
    /// tier.
    #[impress_method(safety = read_only, risk(private))]
    #[impress_example(
        name = "subject-scoped-aperture",
        args = r#"{"query":"aperture","subject_ref":"58000000-0000-4000-8000-000000000009","limit":5,"include_superseded":false}"#,
        expect = r#"{"ok":true,"entries":[{"id":"58000000-0000-4000-8000-000000000001","title":"G3 aperture radius","body":"The G3 aperture radius is five pixels.","schema_ref":"memory/claim@1.0.0"}]}"#
    )]
    async fn recall(
        &self,
        query: String,
        subject_ref: String,
        limit: i64,
        include_superseded: bool,
    ) -> RecallResult;

    /// The "start of session" digest: standing instructions, then claims
    /// (optionally scoped to `topic`), then recent episodes — always in that
    /// order, because instructions constrain everything that follows them.
    /// Call this once at the start of a task to load what you already know,
    /// the same way a human would re-read their own notes.
    ///
    /// `topic` scopes ONLY the claims section (full-text, same rules as
    /// `recall`); an instruction that does not mention the topic still binds
    /// and still appears — filtering instructions by topic would quietly
    /// drop exactly the constraint the current work was not expecting to
    /// need. Pass a non-empty `subject_ref` to narrow every section to
    /// memories about one item. `max_entries` caps each section
    /// independently (`0` means the kernel default of 8).
    ///
    /// `text` is ready to paste or inject directly into a system prompt;
    /// read `sections` instead when the structured fields (id, confidence,
    /// confirmations) matter more than prose.
    #[impress_method(safety = read_only, risk(private))]
    #[impress_example(
        name = "topic-brief",
        args = r#"{"topic":"aperture","subject_ref":"58000000-0000-4000-8000-00000000000a","max_entries":3}"#,
        expect = r#"{"ok":true,"sections":[{"kind":"memory/instruction@1.0.0","entries":[]},{"kind":"memory/claim@1.0.0","entries":[{"id":"58000000-0000-4000-8000-000000000002","title":"G3 aperture correction"}]},{"kind":"memory/episode@1.0.0","entries":[]}]}"#
    )]
    async fn memory_brief(
        &self,
        topic: String,
        subject_ref: String,
        max_entries: i64,
    ) -> BriefResult;

    /// Record that a memory was independently re-observed, WITHOUT writing a
    /// duplicate row: bumps `confirmations` and stamps `last_confirmed`.
    ///
    /// `remember` already calls this automatically when its dedup gate finds
    /// a near-duplicate — call it directly only when you already hold the
    /// exact id (from a prior `recall` or `memory_brief`) and want to record
    /// that it still holds without restating the prose.
    #[impress_method(effects(reads = ["memory/claim@1.0.0"], writes = ["memory/claim@1.0.0"]))]
    #[impress_example(
        name = "confirm-observation",
        args = r#"{"id":"58000000-0000-4000-8000-000000000003"}"#,
        expect = r#"{"ok":true,"id":"58000000-0000-4000-8000-000000000003"}"#
    )]
    async fn confirm_claim(&self, id: String) -> ActionResult;

    /// Retract a memory by REPLACING it: writes a new memory of the SAME
    /// KIND as `old_id` from `title` / `body` — an instruction's correction
    /// stays an instruction, an episode's stays an episode — then records
    /// that it supersedes `old_id`. The old row is left completely untouched
    /// (it stays in the graph for audit — pass `include_superseded: true` to
    /// a later `recall` to see it again); this only stops it being returned
    /// by default. `old_id` must be a memory row; anything else is refused
    /// with nothing written.
    ///
    /// Use this when a previously-remembered fact turns out to be wrong, or
    /// needs correcting or sharpening. `reason` is optional (empty string
    /// omits it) but is recorded on the replacement edge when given, and is
    /// worth the sentence — it is the only place "why was this retracted"
    /// survives. This is NOT how to retire a memory that was never wrong but
    /// is simply unwanted or private; use `forget` for that instead.
    #[impress_method(effects(reads = ["memory/claim@1.0.0"], writes = ["memory/claim@1.0.0"]))]
    #[impress_example(
        name = "correct-flux-unit",
        args = r#"{"old_id":"58000000-0000-4000-8000-000000000004","title":"G3 flux unit correction","body":"G3 fluxes are measured in millijanskys.","reason":"Checked the catalogue header."}"#,
        expect = r#"{"ok":true,"old_id":"58000000-0000-4000-8000-000000000004"}"#
    )]
    async fn supersede_claim(
        &self,
        old_id: String,
        title: String,
        body: String,
        reason: String,
    ) -> SupersedeResult;

    /// Withhold a memory from `recall` and `memory_brief` WITHOUT deleting it
    /// — for something that turned out to be private, unwanted, or wrong in a
    /// way `supersede_claim` does not express (there is no correct
    /// replacement text). The row and its evidence edges stay in the store;
    /// this only flips the flag every read path already honours, so the
    /// action is reversible by a direct store edit even though no verb here
    /// un-forgets it.
    #[impress_method(safety = destructive, effects(reads = ["memory/claim@1.0.0", "memory/episode@1.0.0", "memory/instruction@1.0.0"], writes = ["memory/claim@1.0.0", "memory/episode@1.0.0", "memory/instruction@1.0.0"]))]
    #[impress_example(
        name = "withhold-private-note",
        args = r#"{"id":"58000000-0000-4000-8000-000000000005"}"#,
        expect = r#"{"ok":true,"id":"58000000-0000-4000-8000-000000000005"}"#
    )]
    async fn forget(&self, id: String) -> ActionResult;

    /// Row counts per memory schema (heads and totals), plus which retrieval
    /// tier is running.
    ///
    /// Call this to sanity-check that memory is actually being written to —
    /// an agent that has called `remember` all session but sees zero counts
    /// here has found a real bug, not an empty library — or to decide whether
    /// a semantic (non-lexical) recall would help before deciding one is
    /// worth building. `vector_tier` reports the vector tier's real state
    /// (off / initializing lazily / live / unavailable — see
    /// [`StatusResult::vector_tier`]) and `embedding_coverage` is a real
    /// fraction whenever the tier is at least configured; this call never
    /// pays for a model load itself, so it is always cheap to check.
    #[impress_method(safety = read_only)]
    #[impress_example(name = "default", args = r#"{}"#)]
    async fn memory_status(&self) -> StatusResult;
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

/// Envelope author stamped on every write this service makes. See the module
/// docs ("Attribution") for why this is a constant rather than a
/// caller-supplied argument.
const AUTHOR: &str = "impress-memory-service";

/// Payload key holding the withhold-from-recall flag. Mirrors
/// `crate::ops`'s private `NO_RECALL_FIELD` constant — not a
/// schema ref (it names a payload field, not a record kind), so it is outside
/// `scripts/check-schema-refs.sh`'s scope. Kept honest by
/// `forget_hides_a_memory_from_recall`, which asserts through the kernel's
/// own public `recall` rather than by re-reading this field back.
const NO_RECALL_FIELD: &str = "no_recall";

/// What every write verb refuses with when the shared store could not be
/// opened and the process store fell back to an empty in-memory stand-in
/// (`koine_store::process::acquire_or_stand_in`). A `remember` that "succeeds" against the
/// substitute persists nothing — the memory is gone when the process exits —
/// and reporting `ok: true` about it would be exactly the lie
/// `DefaultCollectionService::migration_store` refuses to tell about
/// migrations.
const FALLBACK_WRITE_REFUSAL: &str =
    "shared store unavailable; refusing to write to the in-memory fallback — nothing would persist";

/// The same condition, stated on read verbs. Reads stay usable — an empty
/// page is recoverable, and `memory_status` is how the condition gets
/// diagnosed — but their emptiness must never pass for the user's actual
/// memory.
const FALLBACK_READ_NOTE: &str =
    "shared store unavailable — this reflects the empty in-memory fallback, not persisted memory";

/// Store-backed `MemoryService`. `new()` uses the shared store (opened
/// lazily); `with_store` takes an explicit one, as the tests do.
#[derive(Clone, Default)]
pub struct DefaultMemoryService {
    store: Option<Arc<dyn AttributedStore>>,
    /// Test-only override for the vector tier. `None` in every production
    /// instance (`new()` never sets it) — meaning "ask the process-wide,
    /// env-gated singleton". `Some` means "use exactly this, unconditionally
    /// LIVE, ignoring `IMPRESS_MEMORY_VECTORS` entirely" — set only by
    /// [`DefaultMemoryService::with_store_and_vector_tier`], so tests never
    /// touch the env var or the process-wide static and so never race each
    /// other over either.
    #[cfg(feature = "embeddings")]
    vector_override: Option<Arc<VectorTierInner>>,
    /// Test-only override forcing
    /// [`DefaultMemoryService::resolved_store_is_fallback`] to answer `true`.
    /// `false` in every production instance — the real answer comes from
    /// `koine_store::process::is_fallback`, which only a failed
    /// lazy open of the PROCESS-WIDE store can set, and which a test must
    /// therefore never trip for real (it would poison every other test in the
    /// binary through the same `OnceLock`s). Set only by
    /// [`DefaultMemoryService::with_store_marked_fallback`].
    fallback_override: bool,
}

impl DefaultMemoryService {
    pub fn new() -> Self {
        Self {
            store: None,
            #[cfg(feature = "embeddings")]
            vector_override: None,
            fallback_override: false,
        }
    }

    pub fn with_store(store: Arc<dyn AttributedStore>) -> Self {
        Self {
            store: Some(store),
            #[cfg(feature = "embeddings")]
            vector_override: None,
            fallback_override: false,
        }
    }

    /// Test seam: a service whose vector tier is exactly `embedding_store` +
    /// `embedder`, unconditionally live, never touching
    /// [`VECTOR_TIER_ENV`]/[`EMBEDDINGS_PATH_ENV`] or the process-wide
    /// [`VECTOR_TIER`] static. See [`DefaultMemoryService::vector_override`].
    #[cfg(all(test, feature = "embeddings"))]
    fn with_store_and_vector_tier(
        store: Arc<dyn AttributedStore>,
        embedding_store: EmbeddingStore,
        embedder: impl Embedder + 'static,
    ) -> Self {
        Self {
            store: Some(store),
            vector_override: Some(Arc::new(VectorTierInner {
                embedding_store,
                embedder: Box::new(embedder),
            })),
            fallback_override: false,
        }
    }

    /// Test seam: a service that behaves as if `store_instance()` had
    /// substituted the in-memory fallback for `store` — without tripping the
    /// process-wide singletons the real flag lives in. See
    /// [`DefaultMemoryService::fallback_override`].
    #[cfg(test)]
    fn with_store_marked_fallback(store: Arc<dyn AttributedStore>) -> Self {
        Self {
            store: Some(store),
            #[cfg(feature = "embeddings")]
            vector_override: None,
            fallback_override: true,
        }
    }

    /// The injected store, else the call's or the process's
    /// (`koine_store::process`), else a stand-in flagged as one.
    fn store(&self) -> Arc<dyn AttributedStore> {
        self.store
            .clone()
            .unwrap_or_else(|| koine_store::process::acquire_or_stand_in().0)
    }

    /// Whether the store [`DefaultMemoryService::store`] resolves to is the
    /// empty in-memory substitute rather than the user's data. An injected
    /// store (`with_store` — tests, embedding hosts) is always trusted; only
    /// the lazily-opened process-wide store carries the flag — the same rule
    /// `DefaultCollectionService::migration_store` applies. Meaningful only
    /// AFTER `store()` has resolved: the flag is recorded by the lazy open
    /// itself, so every caller obtains the store first and asks second.
    fn resolved_store_is_fallback(&self) -> bool {
        if self.fallback_override {
            return true;
        }
        self.store.is_none() && koine_store::process::is_fallback()
    }

    /// The tier `remember`/`recall` use: the test override when set,
    /// otherwise the process-wide lazy singleton (which resolves the env
    /// gate itself). Never called by `memory_status` — see
    /// [`DefaultMemoryService::vector_status`].
    #[cfg(feature = "embeddings")]
    fn vector_tier(&self) -> Option<&VectorTierInner> {
        self.vector_override
            .as_deref()
            .or_else(|| process_vector_tier())
    }

    /// `memory_status`'s vector-tier reporting, kept separate from
    /// `vector_tier` because it must NEVER trigger a `SemanticSearch` build
    /// (the cost rule) — it only peeks at [`VECTOR_TIER`]'s current state
    /// (or, under a test override, treats it as unconditionally live) and
    /// otherwise counts vectors via [`readonly_coverage`], which opens only
    /// the sidecar.
    #[cfg(not(feature = "embeddings"))]
    fn vector_status(&self, _total_items: u32) -> (String, f64) {
        (
            "off (built without the embeddings feature; text search only)".to_string(),
            0.0,
        )
    }

    #[cfg(feature = "embeddings")]
    fn vector_status(&self, total_items: u32) -> (String, f64) {
        if let Some(tier) = &self.vector_override {
            let model = tier.embedder.model_id();
            let embedded = tier
                .embedding_store
                .load_vectors_by_type_and_model(MEMORY_ITEM_SOURCE_TYPE, model)
                .map(|v| v.len())
                .unwrap_or(0);
            return (
                format!("live (model {model}, {embedded}/{total_items} items embedded)"),
                coverage_ratio(embedded, total_items),
            );
        }
        match vector_tier_gate() {
            VectorTierGate::Off => {
                return (format!("off ({VECTOR_TIER_ENV} disables it)"), 0.0);
            }
            // The honest reading of a machine mid-backfill: the tier is not
            // "off", it simply has nothing to read yet, and it starts on its
            // own once the embed sweep writes the first memory vector.
            VectorTierGate::ByPresence if !memory_vectors_present() => {
                return (
                    "idle (no memory vectors embedded yet — starts automatically \
                     once the embed sweep writes some)"
                        .to_string(),
                    0.0,
                );
            }
            _ => {}
        }
        match VECTOR_TIER.get() {
            None => (
                "initializing lazily".to_string(),
                readonly_coverage(koine_embeddings::FASTEMBED_MODEL_ID, total_items),
            ),
            Some(TierState::Live(inner)) => {
                let model = inner.embedder.model_id();
                let embedded = inner
                    .embedding_store
                    .load_vectors_by_type_and_model(MEMORY_ITEM_SOURCE_TYPE, model)
                    .map(|v| v.len())
                    .unwrap_or(0);
                (
                    format!("live (model {model}, {embedded}/{total_items} items embedded)"),
                    coverage_ratio(embedded, total_items),
                )
            }
            Some(TierState::Failed(reason)) => (
                format!("unavailable: {reason}"),
                readonly_coverage(koine_embeddings::FASTEMBED_MODEL_ID, total_items),
            ),
        }
    }
}

fn describe(err: StoreError) -> String {
    err.to_string()
}

/// Trim `s`; `None` when nothing is left. The service-wide convention for
/// "empty string argument means absent", used for `claim_type`,
/// `subject_ref`, `topic` and `reason` — arguments the codegen macro's
/// allow-listed simple types keep as plain `String` rather than
/// `Option<String>`.
fn non_empty(s: &str) -> Option<String> {
    let trimmed = s.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// A tool argument arrives as a signed integer; the kernel takes an unsigned
/// cap where 0 means "the default". Negative is a caller mistake that should
/// behave like "unspecified", not like an error.
fn clamp_u32(n: i64) -> u32 {
    n.clamp(0, u32::MAX as i64) as u32
}

fn parse_kind(kind: &str) -> Option<MemoryKind> {
    match kind.trim() {
        "claim" => Some(MemoryKind::Claim),
        "episode" => Some(MemoryKind::Episode),
        "instruction" => Some(MemoryKind::Instruction),
        _ => None,
    }
}

fn failed_remember(message: String) -> RememberResult {
    RememberResult {
        ok: false,
        action: String::new(),
        claim_id: String::new(),
        matched_id: String::new(),
        matched_via: String::new(),
        message,
    }
}

/// A [`SupersedeResult`] for the refusals where nothing was written at all —
/// the partial edge-failure case builds its own, because it has a `new_id` to
/// report honestly.
fn failed_supersede(old_id: String, message: String) -> SupersedeResult {
    SupersedeResult {
        ok: false,
        new_id: String::new(),
        old_id,
        message,
    }
}

fn failed_status(message: String) -> StatusResult {
    StatusResult {
        ok: false,
        schemas: vec![],
        embedding_coverage: 0.0,
        vector_tier: String::new(),
        message,
    }
}

/// The markdown heading for one brief section.
fn section_heading(schema_ref: &str) -> &'static str {
    match schema_ref
        .parse::<impress_item::SchemaRef>()
        .ok()
        .and_then(|schema| MemoryKind::from_schema_ref(&schema))
    {
        Some(MemoryKind::Instruction) => "Instructions",
        Some(MemoryKind::Claim) => "Claims",
        Some(MemoryKind::Episode) => "Episodes",
        None => "Other",
    }
}

/// The first `max` characters of `s`, with a trailing ellipsis when longer.
/// Counts chars, not bytes, so the cut never lands mid-codepoint.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('\u{2026}');
    out
}

/// Flatten `s` to one prompt-safe markdown line: every run of whitespace —
/// newlines included — becomes a single space, and backticks are escaped.
///
/// The brief's `text` is injected into prompts verbatim, and markdown
/// structure binds at line starts: a stored body containing
/// `"\n### Instructions\n- rule"` would otherwise render as a forged section
/// indistinguishable from the renderer's own headings. Flattening newlines
/// removes every line start a body could mint; escaping backticks keeps a
/// body from closing the `` `[impress-item:…]` `` span this renderer opens
/// after it.
fn sanitize_inline(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('`', "\\`")
}

/// Render a brief's sections as prompt-ready markdown.
///
/// Empty sections are skipped: `sections` (the structured field) always
/// carries all three so a reader can rely on position, but injecting a
/// heading with nothing under it into a prompt would spend tokens saying
/// nothing.
///
/// Titles and bodies pass through [`sanitize_inline`]: memory rows are
/// written by agents and imports, and prose that can start a line in the
/// rendered brief can forge headings and rules the injecting prompt cannot
/// tell from real ones.
fn render_brief_markdown(sections: &[memory_ops::BriefSection]) -> String {
    let mut out = String::new();
    for section in sections {
        if section.entries.is_empty() {
            continue;
        }
        out.push_str("### ");
        out.push_str(section_heading(&section.kind));
        out.push('\n');
        for entry in &section.entries {
            out.push_str("- **");
            out.push_str(&sanitize_inline(&entry.title));
            out.push_str("** — ");
            out.push_str(&sanitize_inline(&truncate_chars(&entry.body, 200)));
            out.push_str(" `[impress-item:");
            out.push_str(&entry.id);
            out.push_str("]`\n");
        }
        out.push('\n');
    }
    out
}

#[async_trait::async_trait]
impl MemoryService for DefaultMemoryService {
    async fn remember(
        &self,
        kind: String,
        title: String,
        body: String,
        claim_type: String,
        confidence: f64,
        subject_refs: Vec<String>,
        evidence_refs: Vec<String>,
    ) -> RememberResult {
        let Some(memory_kind) = parse_kind(&kind) else {
            return failed_remember(format!(
                "kind must be one of claim, episode, instruction; got {kind:?}"
            ));
        };

        let mut draft = MemoryDraft::new(memory_kind, title, body, AUTHOR, ActorKind::System);
        draft.claim_type = non_empty(&claim_type);
        // Negative means "unstated" (the codegen's simple types have no
        // Option<f64>). Above 1.0 is a caller stating a percentage — clamp to
        // full confidence rather than let one 90.0 outrank every honest
        // memory through the ranker's confidence term. The kernel clamps
        // again on insert; this keeps the value the gate and the result see
        // consistent with what is stored.
        draft.confidence = (confidence >= 0.0).then_some(confidence.min(1.0));
        draft.subject_refs = subject_refs;
        draft.evidence_refs = evidence_refs;

        let store = self.store();
        if self.resolved_store_is_fallback() {
            return failed_remember(FALLBACK_WRITE_REFUSAL.into());
        }
        let outcome =
            match memory_ops::gate_fts(&*store, &draft, memory_ops::GATE_CONFIRM_THRESHOLD) {
                Ok(o) => o,
                Err(e) => return failed_remember(describe(e)),
            };

        match outcome {
            // The FTS gate found nothing — try the vector tier's (higher-
            // threshold) semantic check before falling back to a plain
            // insert. `vector_gate_confirm` is a no-op returning `None`
            // whenever the tier is off, so this costs nothing extra when it
            // is.
            GateOutcome::Insert => {
                if let Some(result) = self.vector_gate_confirm(&*store, &draft) {
                    return result;
                }
                match memory_ops::insert_memory_item(&*store, &draft) {
                    Ok(id) => RememberResult {
                        ok: true,
                        action: "inserted".into(),
                        claim_id: id.to_string(),
                        matched_id: String::new(),
                        matched_via: String::new(),
                        message: format!("Remembered as a new {kind}: {id}."),
                    },
                    Err(e) => failed_remember(describe(e)),
                }
            }
            GateOutcome::Confirm(existing) => {
                match memory_ops::confirm(&*store, existing, AUTHOR, ActorKind::System) {
                    Ok(n) => RememberResult {
                        ok: true,
                        action: "confirmed".into(),
                        claim_id: existing.to_string(),
                        matched_id: existing.to_string(),
                        matched_via: "fts".into(),
                        message: format!("Already known; confirmed ({n} total): {existing}."),
                    },
                    Err(e) => failed_remember(describe(e)),
                }
            }
        }
    }

    async fn recall(
        &self,
        query: String,
        subject_ref: String,
        limit: i64,
        include_superseded: bool,
    ) -> RecallResult {
        let store = self.store();
        let opts = memory_ops::RecallOptions {
            subject_ref: non_empty(&subject_ref),
            limit: clamp_u32(limit),
            include_superseded,
            kinds: Vec::new(),
        };
        match memory_ops::recall(&*store, &query, &opts) {
            Ok(entries) => {
                let entries = self.rerank_with_vectors(&query, entries);
                let n = entries.len();
                let mut message = format!("{n} memor{}.", if n == 1 { "y" } else { "ies" });
                if self.resolved_store_is_fallback() {
                    message.push_str(&format!(" ({FALLBACK_READ_NOTE}.)"));
                }
                RecallResult {
                    ok: true,
                    entries: entries.into_iter().map(RecallEntryDto::from).collect(),
                    message,
                }
            }
            Err(e) => RecallResult {
                ok: false,
                entries: vec![],
                message: describe(e),
            },
        }
    }

    async fn memory_brief(
        &self,
        topic: String,
        subject_ref: String,
        max_entries: i64,
    ) -> BriefResult {
        let store = self.store();
        let opts = memory_ops::BriefOptions {
            topic: non_empty(&topic),
            subject_ref: non_empty(&subject_ref),
            max_entries: clamp_u32(max_entries),
        };
        match memory_ops::brief(&*store, &opts) {
            Ok(sections) => {
                let text = render_brief_markdown(&sections);
                let total: usize = sections.iter().map(|s| s.entries.len()).sum();
                let mut message = format!(
                    "Brief assembled: {total} entr{}.",
                    if total == 1 { "y" } else { "ies" }
                );
                if self.resolved_store_is_fallback() {
                    message.push_str(&format!(" ({FALLBACK_READ_NOTE}.)"));
                }
                BriefResult {
                    ok: true,
                    sections: sections.into_iter().map(BriefSectionDto::from).collect(),
                    text,
                    message,
                }
            }
            Err(e) => BriefResult {
                ok: false,
                sections: vec![],
                text: String::new(),
                message: describe(e),
            },
        }
    }

    async fn confirm_claim(&self, id: String) -> ActionResult {
        let Ok(uuid) = ItemId::parse_str(id.trim()) else {
            return ActionResult {
                ok: false,
                id,
                message: "invalid UUID".into(),
            };
        };
        let store = self.store();
        if self.resolved_store_is_fallback() {
            return ActionResult {
                ok: false,
                id,
                message: FALLBACK_WRITE_REFUSAL.into(),
            };
        }
        match memory_ops::confirm(&*store, uuid, AUTHOR, ActorKind::System) {
            Ok(n) => ActionResult {
                ok: true,
                id: id.clone(),
                message: format!("Confirmed {id} ({n} total)."),
            },
            Err(e) => ActionResult {
                ok: false,
                id,
                message: describe(e),
            },
        }
    }

    async fn supersede_claim(
        &self,
        old_id: String,
        title: String,
        body: String,
        reason: String,
    ) -> SupersedeResult {
        let Ok(old_uuid) = ItemId::parse_str(old_id.trim()) else {
            return failed_supersede(old_id.clone(), format!("invalid UUID: {old_id}"));
        };
        let store = self.store();
        if self.resolved_store_is_fallback() {
            return failed_supersede(old_id, FALLBACK_WRITE_REFUSAL.into());
        }
        let old_item = match store.get(old_uuid) {
            Ok(Some(item)) => item,
            Ok(None) => return failed_supersede(old_id.clone(), format!("not found: {old_id}")),
            Err(e) => return failed_supersede(old_id, describe(e)),
        };
        // The replacement keeps the OLD row's kind: a correction changes what
        // a memory says, never what kind of memory it is — an instruction's
        // correction still binds as an instruction, an episode's is still a
        // record of what happened. (The kernel refuses a non-memory old row
        // too; refusing here as well keeps the replacement from being written
        // before that refusal could fire.)
        let Some(old_kind) = MemoryKind::from_schema_ref(&old_item.schema) else {
            return failed_supersede(
                old_id.clone(),
                format!("{old_id} is a '{}' item, not a memory row", old_item.schema),
            );
        };

        // No gate: a correction is deliberate text the caller already wrote,
        // not a candidate for the dedup heuristic.
        let draft = MemoryDraft::new(old_kind, title, body, AUTHOR, ActorKind::System);
        let new_id = match memory_ops::insert_memory_item(&*store, &draft) {
            Ok(id) => id,
            Err(e) => return failed_supersede(old_id, describe(e)),
        };

        let reason_opt = non_empty(&reason);
        match memory_ops::supersede(
            &*store,
            old_uuid,
            new_id,
            reason_opt.as_deref(),
            AUTHOR,
            ActorKind::System,
        ) {
            Ok(()) => SupersedeResult {
                ok: true,
                new_id: new_id.to_string(),
                old_id,
                message: format!("{new_id} supersedes {old_uuid}."),
            },
            // The replacement WAS written even though the supersession edge
            // failed — report its id rather than pretending nothing happened,
            // so the caller is not left with an orphan row they were never
            // told about (see [`SupersedeResult::new_id`]).
            Err(e) => SupersedeResult {
                ok: false,
                new_id: new_id.to_string(),
                old_id,
                message: format!(
                    "inserted {new_id} but failed to record supersession over {old_uuid}: {}",
                    describe(e)
                ),
            },
        }
    }

    async fn forget(&self, id: String) -> ActionResult {
        let Ok(uuid) = ItemId::parse_str(id.trim()) else {
            return ActionResult {
                ok: false,
                id,
                message: "invalid UUID".into(),
            };
        };
        let store = self.store();
        if self.resolved_store_is_fallback() {
            return ActionResult {
                ok: false,
                id,
                message: FALLBACK_WRITE_REFUSAL.into(),
            };
        }
        match store.get(uuid) {
            Ok(Some(item)) if MemoryKind::from_schema_ref(&item.schema).is_some() => {
                let outcome = store.apply_operation(OperationSpec {
                    target_id: uuid,
                    op_type: OperationType::SetPayload(NO_RECALL_FIELD.into(), Value::Bool(true)),
                    intent: OperationIntent::Editorial,
                    reason: None,
                    batch_id: None,
                    author: AUTHOR.to_string(),
                    author_kind: ActorKind::System,
                    retention: RetentionTier::Durable,
                });
                match outcome {
                    Ok(_) => ActionResult {
                        ok: true,
                        id: id.clone(),
                        message: format!("Withheld {id} from recall."),
                    },
                    Err(e) => ActionResult {
                        ok: false,
                        id,
                        message: describe(e),
                    },
                }
            }
            Ok(Some(item)) => ActionResult {
                ok: false,
                id: id.clone(),
                message: format!("{id} is a '{}' item, not a memory row", item.schema),
            },
            Ok(None) => ActionResult {
                ok: false,
                id: id.clone(),
                message: format!("not found: {id}"),
            },
            Err(e) => ActionResult {
                ok: false,
                id,
                message: describe(e),
            },
        }
    }

    async fn memory_status(&self) -> StatusResult {
        let store = self.store();
        let mut schemas = Vec::with_capacity(3);
        for kind in MemoryKind::all() {
            let schema_ref = kind.schema_ref();
            let heads = match memory_ops::claim_heads(
                &*store,
                schema_ref.clone(),
                memory_ops::MAX_RECALL_LIMIT,
            ) {
                Ok(ids) => ids.len() as u32,
                Err(e) => return failed_status(describe(e)),
            };
            let total = match store.count(&impress_item::ItemQuery {
                schema: Some(schema_ref.clone()),
                ..Default::default()
            }) {
                Ok(count) => count as u32,
                Err(e) => return failed_status(describe(e)),
            };
            schemas.push(SchemaCountDto {
                schema_ref: schema_ref.to_string(),
                heads,
                total,
            });
        }
        let total_items: u32 = schemas.iter().map(|s| s.total).sum();
        let (vector_tier, embedding_coverage) = self.vector_status(total_items);

        // The status verb is how the fallback condition gets diagnosed, so
        // it must say so itself — zero counts from an empty substitute look
        // exactly like "no memories yet" otherwise.
        let message = if self.resolved_store_is_fallback() {
            format!("Memory kernel status; {FALLBACK_READ_NOTE}.")
        } else {
            "Memory kernel status.".into()
        };
        StatusResult {
            ok: true,
            schemas,
            embedding_coverage,
            vector_tier,
            message,
        }
    }
}

/// Without the `embeddings` feature there is no vector tier: the gate never
/// confirms by similarity and recall keeps the text-search order.
#[cfg(not(feature = "embeddings"))]
impl DefaultMemoryService {
    fn vector_gate_confirm(
        &self,
        _store: &dyn AttributedStore,
        _draft: &MemoryDraft,
    ) -> Option<RememberResult> {
        None
    }

    fn rerank_with_vectors(
        &self,
        _query: &str,
        entries: Vec<memory_ops::RecallEntry>,
    ) -> Vec<memory_ops::RecallEntry> {
        entries
    }
}

/// The vector tier's contribution to `remember` and `recall`. Kept as a
/// separate `impl` block from the trait's own (above) because neither method
/// here is part of `MemoryService` — they are internals the trait methods
/// call into, mirroring how `impress-mcp::tools`'s three semantic-search
/// tools are hand-written functions layered over the SAME
/// `#[impress_service]`-generated inventory rather than trait methods
/// themselves.
#[cfg(feature = "embeddings")]
impl DefaultMemoryService {
    /// The vector tier's half of `remember`'s dedup gate — reached only when
    /// [`GateOutcome::Insert`] already fired, i.e. the FTS gate found
    /// nothing close enough. `Some(result)` when a semantic near-duplicate
    /// was found, re-checked, and confirmed instead; `None` means "insert as
    /// planned" — the tier is off, the draft had no text to embed, embedding
    /// failed, the sidecar holds no memory-item vectors for this model yet,
    /// or every hit at or above [`VECTOR_CONFIRM_THRESHOLD`] failed the
    /// re-check.
    ///
    /// Walks candidates best-first and re-checks each in turn — the same
    /// discipline `memory_ops::gate_fts` uses and for the identical reason:
    /// a lower-scoring hit that IS a compatible live head must not be
    /// blocked by a higher-scoring one that is superseded, withheld, or a
    /// different `claim_type`.
    fn vector_gate_confirm(
        &self,
        store: &dyn AttributedStore,
        draft: &MemoryDraft,
    ) -> Option<RememberResult> {
        let tier = self.vector_tier()?;

        // Mirrors `impel_memory::embed::embeddable_text`'s composition for a
        // memory row (`"{title}\n{body}"`) so the draft's on-the-fly
        // embedding lands in the same comparison space as the STORED
        // vectors, which were embedded the same way.
        let text = format!("{}\n{}", draft.title.trim(), draft.body.trim());
        if text.trim().is_empty() {
            return None;
        }

        let query_vec = match tier.embedder.embed(&text) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "impress-memory-service: vector gate skipped this call: embed failed: {e}"
                );
                return None;
            }
        };

        let model = tier.embedder.model_id();
        let vectors = match tier
            .embedding_store
            .load_vectors_by_type_and_model(MEMORY_ITEM_SOURCE_TYPE, model)
        {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "impress-memory-service: vector gate skipped this call: sidecar read failed: {e}"
                );
                return None;
            }
        };
        if vectors.is_empty() {
            return None;
        }

        let mut scored: Vec<(ItemId, f32)> = vectors
            .iter()
            .filter_map(|v| {
                ItemId::parse_str(&v.source_id).ok().map(|id| {
                    (
                        id,
                        koine_embeddings::cosine_similarity(&query_vec, &v.vector),
                    )
                })
            })
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });

        let draft_type = draft
            .claim_type
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        for (id, similarity) in scored
            .into_iter()
            .take_while(|(_, s)| *s >= VECTOR_CONFIRM_THRESHOLD)
        {
            // The sidecar holds vectors for every memory kind under one
            // source_type; this gate is scoped to the draft's own schema,
            // like the FTS tier — an episode is never a duplicate claim,
            // however similar the prose.
            let Ok(Some(item)) = store.get(id) else {
                continue;
            };
            if item.schema != draft.kind.schema_ref() {
                continue;
            }
            // Eligibility through the kernel's own per-id policy — never a
            // membership set built from a recency page: `claim_heads` caps at
            // `MAX_RECALL_LIMIT`, so past 200 heads a page-based check reads
            // exactly the oldest, best-established memories as retracted and
            // silently stops deduplicating against them. The policy screens
            // out superseded and withheld rows and incompatible claim types,
            // same rule `gate_fts` applies.
            match memory_ops::absorbs_confirmation(store, id, draft_type) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) => {
                    eprintln!(
                        "impress-memory-service: vector gate skipped this call: eligibility check failed: {e}"
                    );
                    return None;
                }
            }
            return Some(match memory_ops::confirm(store, id, AUTHOR, ActorKind::System) {
                Ok(n) => RememberResult {
                    ok: true,
                    action: "confirmed".into(),
                    claim_id: id.to_string(),
                    matched_id: id.to_string(),
                    matched_via: "vector".into(),
                    message: format!(
                        "Already known (semantic match, {similarity:.2} similarity); confirmed ({n} total): {id}."
                    ),
                },
                Err(e) => failed_remember(describe(e)),
            });
        }
        None
    }

    /// Re-ranks `entries` — already retrieved, filtered and FTS/recency-
    /// ranked by `memory_ops::recall` — using embedding similarity, when the
    /// vector tier is live. Never widens the retrieved SET: this only
    /// reorders the page `memory_ops::recall` already selected, so the
    /// vector tier composes with the FTS one as a pure re-ranking signal, as
    /// `memory_ops::MemoryCandidate::vector_similarity`'s own doc comment
    /// describes.
    ///
    /// Returns `entries` completely UNCHANGED — same `Vec`, same order, same
    /// scores — whenever the tier is off, the sidecar holds no memory-item
    /// vectors for the resolved model, or embedding the query fails. That is
    /// what makes recall's tier-off behavior and its tier-on-with-an-empty-
    /// sidecar behavior provably IDENTICAL rather than merely
    /// same-order-different-numbers.
    fn rerank_with_vectors(
        &self,
        query: &str,
        entries: Vec<memory_ops::RecallEntry>,
    ) -> Vec<memory_ops::RecallEntry> {
        if entries.is_empty() {
            return entries;
        }
        let Some(tier) = self.vector_tier() else {
            return entries;
        };
        let model = tier.embedder.model_id().to_string();
        let stored = match tier
            .embedding_store
            .load_vectors_by_type_and_model(MEMORY_ITEM_SOURCE_TYPE, &model)
        {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "impress-memory-service: vector recall skipped this call: sidecar read failed: {e}"
                );
                return entries;
            }
        };
        if stored.is_empty() {
            return entries;
        }
        let query_vec = match tier.embedder.embed(query) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "impress-memory-service: vector recall skipped this call: embed failed: {e}"
                );
                return entries;
            }
        };

        let by_id: HashMap<&str, &Vec<f32>> = stored
            .iter()
            .map(|v| (v.source_id.as_str(), &v.vector))
            .collect();

        // `modified_ms` / `now_ms` both pinned to 0 rather than a real clock
        // reading: `recency_decay(0, 0, half_life)` is exactly 1.0 regardless
        // of `half_life`, so it contributes the SAME flat recency bonus to
        // every candidate here and never perturbs their relative order —
        // deliberately, since `entries` is already recency-ranked by
        // `memory_ops::recall` (folded into `fts_score` below) and this
        // pass's only job is layering the vector signal on top of that.
        let candidates: Vec<memory_ops::MemoryCandidate> = entries
            .iter()
            .map(|e| memory_ops::MemoryCandidate {
                id: e.id.clone(),
                fts_score: Some(e.score),
                vector_similarity: by_id
                    .get(e.id.as_str())
                    .map(|v| koine_embeddings::cosine_similarity(&query_vec, v.as_slice())),
                confirmations: 0,
                modified_ms: 0,
                author_kind_human: false,
                confidence: None,
            })
            .collect();
        let ranked = memory_ops::rank_memory_candidates(
            &candidates,
            &memory_ops::MemoryWeights::default(),
            0,
        );

        let mut by_id_entry: HashMap<String, memory_ops::RecallEntry> =
            entries.into_iter().map(|e| (e.id.clone(), e)).collect();
        ranked
            .into_iter()
            .filter_map(|(id, score)| {
                by_id_entry.remove(&id).map(|mut e| {
                    e.score = score;
                    e
                })
            })
            .collect()
    }
}

impress_service_impl! {
    service = MemoryService,
    safety = mutating,
    since = "0.1.0",
    effects = {
        reads: ["memory/claim@1.0.0", "memory/episode@1.0.0", "memory/instruction@1.0.0"],
        writes: [],
        reach: [],
    },
    impl = DefaultMemoryService,
    instance = DefaultMemoryService::new,
    methods = [
        remember(
            /// Memory kind: `claim`, `episode`, or `instruction`.
            kind: String,
            /// Short human-readable label for the memory.
            title: String,
            /// Prose to retain across sessions; treated as private in call logs.
            #[impress_private] body: String,
            /// Claim classification, such as `fact` or `decision`; empty when unstated.
            claim_type: String,
            /// Confidence from 0 to 1; a negative value means unstated.
            confidence: f64,
            /// Lowercase item UUIDs this memory is about; may be empty.
            subject_refs: Vec<String>,
            /// Lowercase item UUIDs this memory was derived from; may be empty.
            evidence_refs: Vec<String>
        ) -> RememberResult,
        recall(
            /// Words to search; empty returns recent memory heads.
            query: String,
            /// Lowercase subject item UUID to filter by, or empty for all subjects.
            subject_ref: String,
            /// Maximum entries; zero uses the default of 20, capped at 200.
            limit: i64,
            /// Include corrected memories that have been superseded.
            include_superseded: bool
        ) -> RecallResult,
        memory_brief(
            /// Search words applied to claims only; empty includes recent claims.
            topic: String,
            /// Lowercase subject item UUID to filter every section, or empty.
            subject_ref: String,
            /// Maximum entries per section; zero uses the default of eight.
            max_entries: i64
        ) -> BriefResult,
        confirm_claim(
            /// Lowercase UUID of an existing memory to confirm.
            id: String
        ) -> ActionResult,
        supersede_claim(
            /// Lowercase UUID of the existing memory being corrected.
            old_id: String,
            /// Short label for the replacement memory.
            title: String,
            /// Corrected prose for the replacement memory.
            body: String,
            /// Why the correction was made; empty omits the reason.
            reason: String
        ) -> SupersedeResult,
        forget(
            /// Lowercase UUID of an existing memory to withhold from recall.
            id: String
        ) -> ActionResult,
        memory_status() -> StatusResult,
    ],
}

#[cfg(test)]
mod tests {
    use super::*;
    use impress_item::schema::refs::{
        MEMORY_CLAIM as MEMORY_CLAIM_SCHEMA, MEMORY_EPISODE as MEMORY_EPISODE_SCHEMA,
        MEMORY_INSTRUCTION as MEMORY_INSTRUCTION_SCHEMA,
    };
    use impress_service_core::{CliSubcommand, McpToolDescriptor};

    fn test_store() -> Arc<dyn AttributedStore> {
        Arc::new(koine_store_memory::MemoryStore::new())
    }

    fn svc() -> DefaultMemoryService {
        DefaultMemoryService::with_store(test_store())
    }

    /// A non-memory item, so `forget`'s kind-check has something real to
    /// refuse.
    fn make_plain_item(store: &dyn AttributedStore) -> String {
        use impress_item::{Item, Priority, Visibility};
        let now = chrono::Utc::now();
        let item = Item {
            id: uuid::Uuid::new_v4(),
            schema: impress_item::schema::refs::MANUSCRIPT,
            payload: Default::default(),
            created: now,
            modified: now,
            author: "test".into(),
            author_kind: ActorKind::System,
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
        };
        store.insert(item).expect("insert plain item").to_string()
    }

    // ─── Inventory ───────────────────────────────────────────────────────

    /// Every method must reach the MCP inventory. A missing entry means the
    /// tool silently doesn't exist for agents.
    #[test]
    fn all_methods_registered_in_mcp_inventory() {
        let names: Vec<&str> = McpToolDescriptor::iter().map(|d| d.name).collect();
        for expected in [
            "memory-service_remember",
            "memory-service_recall",
            "memory-service_memory-brief",
            "memory-service_confirm-claim",
            "memory-service_supersede-claim",
            "memory-service_forget",
            "memory-service_memory-status",
        ] {
            assert!(
                names.contains(&expected),
                "{expected} missing from MCP inventory; have {names:?}"
            );
        }
    }

    #[test]
    fn all_methods_registered_in_cli_inventory() {
        let names: Vec<&str> = CliSubcommand::iter().map(|d| d.name).collect();
        for expected in [
            "remember",
            "recall",
            "memory-brief",
            "confirm-claim",
            "supersede-claim",
            "forget",
            "memory-status",
        ] {
            assert!(
                names.contains(&expected),
                "{expected} missing from CLI inventory; have {names:?}"
            );
        }
    }

    #[test]
    fn descriptions_are_not_the_fallback() {
        for d in McpToolDescriptor::iter() {
            if d.name.starts_with("memory-service_") {
                assert!(
                    !d.description.starts_with("Invoke "),
                    "{} has the fallback description",
                    d.name
                );
            }
        }
    }

    // ─── remember / recall ──────────────────────────────────────────────

    #[tokio::test]
    async fn remember_then_recall_finds_it() {
        let svc = svc();
        let r = svc
            .remember(
                "claim".into(),
                "Flux units".into(),
                "The catalogue flux column is in millijansky.".into(),
                "fact".into(),
                0.9,
                vec![],
                vec![],
            )
            .await;
        assert!(r.ok, "{}", r.message);
        assert_eq!(r.action, "inserted");
        assert!(!r.claim_id.is_empty());
        assert!(r.matched_id.is_empty());

        let found = svc
            .recall("millijansky".into(), String::new(), 0, false)
            .await;
        assert!(found.ok, "{}", found.message);
        assert_eq!(found.entries.len(), 1, "{:?}", found.entries);
        assert_eq!(found.entries[0].id, r.claim_id);
        assert_eq!(found.entries[0].schema_ref, MEMORY_CLAIM_SCHEMA.as_str());
        assert_eq!(found.entries[0].claim_type.as_deref(), Some("fact"));
        // The kernel's RecallEntry carries confidence as f32 (rank_memory_candidates'
        // signal type), so a round trip through it loses a little precision —
        // compare with tolerance rather than exact equality.
        let confidence = found.entries[0].confidence.expect("confidence stated");
        assert!((confidence - 0.9).abs() < 1e-6, "{confidence}");
    }

    #[tokio::test]
    async fn remembering_the_same_thing_twice_confirms_the_second_time() {
        let svc = svc();
        let body = "Never open the shared container from a shell.";
        let first = svc
            .remember(
                "instruction".into(),
                "No shell scans".into(),
                body.into(),
                String::new(),
                -1.0,
                vec![],
                vec![],
            )
            .await;
        assert!(first.ok, "{}", first.message);
        assert_eq!(first.action, "inserted");

        let second = svc
            .remember(
                "instruction".into(),
                "No shell scans, restated".into(),
                body.into(),
                String::new(),
                -1.0,
                vec![],
                vec![],
            )
            .await;
        assert!(second.ok, "{}", second.message);
        assert_eq!(second.action, "confirmed");
        assert_eq!(second.claim_id, first.claim_id);
        assert_eq!(second.matched_id, first.claim_id);
        assert_eq!(second.matched_via, "fts");

        // Only one row exists: recall must not return the same memory twice.
        let found = svc.recall("shell".into(), String::new(), 0, false).await;
        assert_eq!(found.entries.len(), 1, "{:?}", found.entries);
        assert_eq!(found.entries[0].confirmations, 1);
    }

    #[tokio::test]
    async fn an_invalid_kind_fails_loudly() {
        let svc = svc();
        let r = svc
            .remember(
                "essay".into(),
                "T".into(),
                "B".into(),
                String::new(),
                -1.0,
                vec![],
                vec![],
            )
            .await;
        assert!(!r.ok);
        assert!(r.message.contains("claim"), "{}", r.message);
        assert!(r.claim_id.is_empty());
    }

    #[tokio::test]
    async fn recall_narrows_to_a_subject() {
        let svc = svc();
        let store = svc.store();
        let paper = make_plain_item(&*store);

        let about = svc
            .remember(
                "claim".into(),
                "About the paper".into(),
                "Calibration detail worth keeping.".into(),
                String::new(),
                -1.0,
                vec![paper.clone()],
                vec![],
            )
            .await;
        assert!(about.ok, "{}", about.message);
        let unrelated = svc
            .remember(
                "claim".into(),
                "Unrelated".into(),
                "Calibration detail about something else.".into(),
                String::new(),
                -1.0,
                vec![],
                vec![],
            )
            .await;
        assert!(unrelated.ok, "{}", unrelated.message);

        let scoped = svc
            .recall("calibration".into(), paper.clone(), 0, false)
            .await;
        assert!(scoped.ok, "{}", scoped.message);
        let ids: Vec<&str> = scoped.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec![about.claim_id.as_str()]);
    }

    // ─── confirm / supersede / forget ───────────────────────────────────

    #[tokio::test]
    async fn confirm_claim_bumps_the_count() {
        let svc = svc();
        let r = svc
            .remember(
                "claim".into(),
                "Fact".into(),
                "Still true.".into(),
                String::new(),
                -1.0,
                vec![],
                vec![],
            )
            .await;
        assert!(r.ok, "{}", r.message);

        let c1 = svc.confirm_claim(r.claim_id.clone()).await;
        assert!(c1.ok, "{}", c1.message);
        let c2 = svc.confirm_claim(r.claim_id.clone()).await;
        assert!(c2.ok, "{}", c2.message);
        assert!(c2.message.contains('2'), "{}", c2.message);
    }

    #[tokio::test]
    async fn supersede_claim_retracts_the_old_row_without_deleting_it() {
        let svc = svc();
        let old = svc
            .remember(
                "claim".into(),
                "Old".into(),
                "Flux is Jy.".into(),
                String::new(),
                -1.0,
                vec![],
                vec![],
            )
            .await;
        assert!(old.ok, "{}", old.message);

        let replaced = svc
            .supersede_claim(
                old.claim_id.clone(),
                "New".into(),
                "Flux is mJy.".into(),
                "unit corrected against the header".into(),
            )
            .await;
        assert!(replaced.ok, "{}", replaced.message);
        assert_ne!(replaced.new_id, old.claim_id);
        assert_eq!(replaced.old_id, old.claim_id);

        let found = svc.recall("flux".into(), String::new(), 0, false).await;
        let ids: Vec<&str> = found.entries.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&replaced.new_id.as_str()), "{ids:?}");
        assert!(
            !ids.contains(&old.claim_id.as_str()),
            "the superseded claim must not recall by default: {ids:?}"
        );

        // Old row still exists, just retracted — proven by widening the
        // recall rather than by reaching into the store directly.
        let widened = svc.recall("flux".into(), String::new(), 0, true).await;
        let widened_ids: Vec<&str> = widened.entries.iter().map(|e| e.id.as_str()).collect();
        assert!(
            widened_ids.contains(&old.claim_id.as_str()),
            "{widened_ids:?}"
        );
    }

    #[tokio::test]
    async fn supersede_claim_fails_loudly_on_a_missing_old_id() {
        let svc = svc();
        let bad = svc
            .supersede_claim(
                uuid::Uuid::new_v4().to_string(),
                "T".into(),
                "B".into(),
                String::new(),
            )
            .await;
        assert!(!bad.ok);
        assert!(bad.message.contains("not found"), "{}", bad.message);
        // Nothing orphaned: no replacement was written for a supersession
        // that never happened.
        assert!(bad.new_id.is_empty());
    }

    #[tokio::test]
    async fn forget_hides_a_memory_from_recall_without_deleting_it() {
        let svc = svc();
        let r = svc
            .remember(
                "claim".into(),
                "Aperture radius".into(),
                "Aperture radius is five pixels.".into(),
                String::new(),
                -1.0,
                vec![],
                vec![],
            )
            .await;
        assert!(r.ok, "{}", r.message);

        let before = svc.recall("aperture".into(), String::new(), 0, false).await;
        assert_eq!(before.entries.len(), 1);

        let forgotten = svc.forget(r.claim_id.clone()).await;
        assert!(forgotten.ok, "{}", forgotten.message);

        let after = svc.recall("aperture".into(), String::new(), 0, false).await;
        assert!(after.entries.is_empty(), "{:?}", after.entries);

        // Unlike supersession, forget is never lifted by include_superseded.
        let widened = svc.recall("aperture".into(), String::new(), 0, true).await;
        assert!(widened.entries.is_empty(), "{:?}", widened.entries);
    }

    #[tokio::test]
    async fn forget_refuses_a_non_memory_item() {
        let svc = svc();
        let store = svc.store();
        let plain = make_plain_item(&*store);

        let r = svc.forget(plain.clone()).await;
        assert!(!r.ok);
        assert!(r.message.contains("not a memory row"), "{}", r.message);
    }

    /// A withheld row must never absorb the dedup gate's confirmation: the
    /// user forgot it, and restating the same prose later is a NEW memory,
    /// not silent evidence for the hidden one.
    #[tokio::test]
    async fn re_remembering_after_forget_inserts_a_fresh_row() {
        let svc = svc();
        let body = "The nightly export lands on the scratch volume.";
        let first = svc
            .remember(
                "claim".into(),
                "Export target".into(),
                body.into(),
                String::new(),
                -1.0,
                vec![],
                vec![],
            )
            .await;
        assert!(first.ok, "{}", first.message);
        assert_eq!(first.action, "inserted");

        let forgotten = svc.forget(first.claim_id.clone()).await;
        assert!(forgotten.ok, "{}", forgotten.message);

        let second = svc
            .remember(
                "claim".into(),
                "Export target again".into(),
                body.into(),
                String::new(),
                -1.0,
                vec![],
                vec![],
            )
            .await;
        assert!(second.ok, "{}", second.message);
        assert_eq!(
            second.action, "inserted",
            "a withheld row must not absorb the confirmation: {second:?}"
        );
        assert_ne!(second.claim_id, first.claim_id);

        // Only the fresh row recalls; the withheld one stays withheld.
        let found = svc
            .recall("export scratch".into(), String::new(), 0, false)
            .await;
        let ids: Vec<&str> = found.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec![second.claim_id.as_str()], "{found:?}");
    }

    /// A correction changes what a memory says, never what kind of memory it
    /// is.
    #[tokio::test]
    async fn superseding_an_instruction_keeps_the_instruction_kind() {
        let svc = svc();
        let old = svc
            .remember(
                "instruction".into(),
                "No shell scans".into(),
                "Never scan the shared container from a shell.".into(),
                String::new(),
                -1.0,
                vec![],
                vec![],
            )
            .await;
        assert!(old.ok, "{}", old.message);

        let replaced = svc
            .supersede_claim(
                old.claim_id.clone(),
                "No shell scans, sharpened".into(),
                "Never scan the shared container from any shell, login or not.".into(),
                String::new(),
            )
            .await;
        assert!(replaced.ok, "{}", replaced.message);
        assert_eq!(replaced.old_id, old.claim_id);

        let store = svc.store();
        let new_uuid = ItemId::parse_str(&replaced.new_id).expect("uuid");
        let item = store.get(new_uuid).expect("get").expect("replacement row");
        assert_eq!(
            item.schema, MEMORY_INSTRUCTION_SCHEMA,
            "an instruction's correction stays an instruction"
        );
    }

    #[tokio::test]
    async fn supersede_and_confirm_refuse_a_non_memory_item() {
        let svc = svc();
        let store = svc.store();
        let plain = make_plain_item(&*store);

        let c = svc.confirm_claim(plain.clone()).await;
        assert!(!c.ok);
        assert!(c.message.contains("not a memory row"), "{}", c.message);

        let s = svc
            .supersede_claim(
                plain.clone(),
                "T".into(),
                "Replacement body.".into(),
                String::new(),
            )
            .await;
        assert!(!s.ok);
        assert!(s.message.contains("not a memory row"), "{}", s.message);
        assert!(
            s.new_id.is_empty(),
            "no replacement may be written for a refused supersession"
        );

        // Nothing was written anywhere: no memory row of any kind exists.
        let status = svc.memory_status().await;
        assert!(
            status.schemas.iter().all(|row| row.total == 0),
            "{:?}",
            status.schemas
        );
    }

    /// A write that "succeeds" against the empty in-memory substitute
    /// persists nothing; every write verb refuses, and every read verb names
    /// the condition instead of presenting the substitute as the user's data.
    #[tokio::test]
    async fn write_verbs_refuse_the_fallback_store_and_reads_name_it() {
        let svc = DefaultMemoryService::with_store_marked_fallback(test_store());

        let r = svc
            .remember(
                "claim".into(),
                "T".into(),
                "A body that must not pretend to persist.".into(),
                String::new(),
                -1.0,
                vec![],
                vec![],
            )
            .await;
        assert!(!r.ok);
        assert!(r.message.contains("nothing would persist"), "{}", r.message);
        assert!(r.claim_id.is_empty());

        let c = svc.confirm_claim(uuid::Uuid::new_v4().to_string()).await;
        assert!(!c.ok);
        assert!(c.message.contains("nothing would persist"), "{}", c.message);

        let s = svc
            .supersede_claim(
                uuid::Uuid::new_v4().to_string(),
                "T".into(),
                "B".into(),
                String::new(),
            )
            .await;
        assert!(!s.ok);
        assert!(s.message.contains("nothing would persist"), "{}", s.message);
        assert!(s.new_id.is_empty());

        let f = svc.forget(uuid::Uuid::new_v4().to_string()).await;
        assert!(!f.ok);
        assert!(f.message.contains("nothing would persist"), "{}", f.message);

        // The refused remember reached the store not at all.
        let status = svc.memory_status().await;
        assert!(status.ok, "{}", status.message);
        assert!(
            status.schemas.iter().all(|row| row.total == 0),
            "{:?}",
            status.schemas
        );
        assert!(status.message.contains("fallback"), "{}", status.message);

        let recalled = svc.recall(String::new(), String::new(), 0, false).await;
        assert!(recalled.ok, "{}", recalled.message);
        assert!(
            recalled.message.contains("fallback"),
            "{}",
            recalled.message
        );

        let brief = svc.memory_brief(String::new(), String::new(), 0).await;
        assert!(brief.ok, "{}", brief.message);
        assert!(brief.message.contains("fallback"), "{}", brief.message);
    }

    #[tokio::test]
    async fn confidence_above_one_is_stored_as_full_confidence() {
        let svc = svc();
        let r = svc
            .remember(
                "claim".into(),
                "Overstated".into(),
                "A claim stated at ninety, meaning ninety percent.".into(),
                String::new(),
                90.0,
                vec![],
                vec![],
            )
            .await;
        assert!(r.ok, "{}", r.message);

        let found = svc.recall("ninety".into(), String::new(), 0, false).await;
        assert_eq!(found.entries.len(), 1, "{:?}", found.entries);
        assert_eq!(found.entries[0].confidence, Some(1.0));
    }

    #[tokio::test]
    async fn an_oversized_body_is_truncated_at_the_dto_boundary() {
        let svc = svc();
        let body = format!("giant payload {}", "x".repeat(MAX_BODY_BYTES * 2));
        let r = svc
            .remember(
                "claim".into(),
                "Giant".into(),
                body,
                String::new(),
                -1.0,
                vec![],
                vec![],
            )
            .await;
        assert!(r.ok, "{}", r.message);

        let found = svc.recall("giant".into(), String::new(), 0, false).await;
        assert_eq!(found.entries.len(), 1, "{:?}", found.entries.len());
        let out = &found.entries[0].body;
        assert!(
            out.ends_with(BODY_TRUNCATION_MARKER),
            "missing marker; tail: {:?}",
            &out[out.len().saturating_sub(32)..]
        );
        assert!(
            out.len() <= MAX_BODY_BYTES + BODY_TRUNCATION_MARKER.len(),
            "{}",
            out.len()
        );

        // Brief sections cross the same boundary and get the same cap.
        let brief = svc.memory_brief(String::new(), String::new(), 0).await;
        let claims = &brief.sections[1];
        assert_eq!(claims.entries.len(), 1);
        assert!(claims.entries[0].body.ends_with(BODY_TRUNCATION_MARKER));
    }

    /// The cut must land on a char boundary — a byte-exact cut through a
    /// multibyte codepoint would yield an invalid string, not merely an ugly
    /// one.
    #[test]
    fn cap_body_cuts_on_a_char_boundary_and_marks_it() {
        assert_eq!(cap_body("short".into()), "short");
        let out = cap_body("𝄞".repeat(MAX_BODY_BYTES)); // 4-byte codepoint
        assert!(out.ends_with(BODY_TRUNCATION_MARKER));
        assert!(out.len() <= MAX_BODY_BYTES + BODY_TRUNCATION_MARKER.len());
        assert!(out.chars().count() > 0, "still valid UTF-8 by construction");
    }

    #[tokio::test]
    async fn bad_ids_fail_loudly_rather_than_silently() {
        let svc = svc();
        let malformed = svc.confirm_claim("not-a-uuid".into()).await;
        assert!(!malformed.ok);

        let missing = svc.forget(uuid::Uuid::new_v4().to_string()).await;
        assert!(!missing.ok);
        assert!(missing.message.contains("not found"), "{}", missing.message);
    }

    // ─── memory_brief ────────────────────────────────────────────────────

    #[tokio::test]
    async fn brief_text_contains_an_inserted_instruction() {
        let svc = svc();
        let r = svc
            .remember(
                "instruction".into(),
                "No shell scans".into(),
                "Never scan the shared container from a shell.".into(),
                String::new(),
                -1.0,
                vec![],
                vec![],
            )
            .await;
        assert!(r.ok, "{}", r.message);

        let brief = svc.memory_brief(String::new(), String::new(), 0).await;
        assert!(brief.ok, "{}", brief.message);
        assert_eq!(brief.sections.len(), 3);
        assert!(brief.text.contains("### Instructions"), "{}", brief.text);
        assert!(brief.text.contains("No shell scans"), "{}", brief.text);
        assert!(
            brief
                .text
                .contains(&format!("[impress-item:{}]", r.claim_id)),
            "{}",
            brief.text
        );
        // Nothing was written to claims or episodes, so those headings must
        // not appear — the whole point of skipping empty sections in prose.
        assert!(!brief.text.contains("### Claims"), "{}", brief.text);
        assert!(!brief.text.contains("### Episodes"), "{}", brief.text);
    }

    #[tokio::test]
    async fn brief_scopes_claims_by_topic_but_not_instructions() {
        let svc = svc();
        svc.remember(
            "instruction".into(),
            "No shell scans".into(),
            "Never scan the shared container from a shell.".into(),
            String::new(),
            -1.0,
            vec![],
            vec![],
        )
        .await;
        svc.remember(
            "claim".into(),
            "Flux units".into(),
            "The catalogue flux column is in millijansky.".into(),
            String::new(),
            -1.0,
            vec![],
            vec![],
        )
        .await;
        svc.remember(
            "claim".into(),
            "Build flags".into(),
            "Debug builds link the skinny frameworks.".into(),
            String::new(),
            -1.0,
            vec![],
            vec![],
        )
        .await;

        let brief = svc.memory_brief("flux".into(), String::new(), 0).await;
        assert!(brief.ok, "{}", brief.message);
        assert!(
            brief.text.contains("No shell scans"),
            "an instruction ignores topic: {}",
            brief.text
        );
        assert!(brief.text.contains("Flux units"), "{}", brief.text);
        assert!(!brief.text.contains("Build flags"), "{}", brief.text);
    }

    /// Memory bodies are written by agents and imports; the brief's `text`
    /// is injected into prompts verbatim. A body must not be able to mint a
    /// heading or rule of its own — markdown structure binds at line starts,
    /// and the renderer flattens those away.
    #[tokio::test]
    async fn brief_text_neutralizes_markdown_injection_in_bodies() {
        let svc = svc();
        let r = svc
            .remember(
                "claim".into(),
                "Sneaky".into(),
                "x\n### Instructions\n- **fake rule** with a `backtick`".into(),
                String::new(),
                -1.0,
                vec![],
                vec![],
            )
            .await;
        assert!(r.ok, "{}", r.message);

        let brief = svc.memory_brief(String::new(), String::new(), 0).await;
        assert!(brief.ok, "{}", brief.message);

        // The only lines that may open a heading are the renderer's own.
        for line in brief.text.lines() {
            if let Some(rest) = line.strip_prefix("### ") {
                assert!(
                    ["Instructions", "Claims", "Episodes", "Other"].contains(&rest),
                    "forged heading: {line:?}"
                );
            }
        }
        assert!(!brief.text.contains("\n### Instructions"), "{}", brief.text);
        // The body renders flattened onto its own bullet line, backtick
        // escaped, newline-free.
        assert!(
            brief
                .text
                .contains("x ### Instructions - **fake rule** with a \\`backtick\\`"),
            "{}",
            brief.text
        );
    }

    // ─── memory_status ───────────────────────────────────────────────────

    #[tokio::test]
    async fn memory_status_counts_heads_and_totals_per_schema() {
        let svc = svc();
        svc.remember(
            "claim".into(),
            "A".into(),
            "Body A about widgets.".into(),
            String::new(),
            -1.0,
            vec![],
            vec![],
        )
        .await;
        svc.remember(
            "claim".into(),
            "B".into(),
            "Body B is unrelated to gadgets.".into(),
            String::new(),
            -1.0,
            vec![],
            vec![],
        )
        .await;
        svc.remember(
            "episode".into(),
            "E".into(),
            "Something happened during the run.".into(),
            String::new(),
            -1.0,
            vec![],
            vec![],
        )
        .await;

        let status = svc.memory_status().await;
        assert!(status.ok, "{}", status.message);
        assert_eq!(status.schemas.len(), 3);
        assert_eq!(status.embedding_coverage, 0.0);
        // `svc()` sets no vector-tier override and this test never touches
        // IMPRESS_MEMORY_VECTORS, so the presence gate decides — and under
        // `cfg(test)` it always answers "nothing embedded", which is the
        // `idle` state (see `DefaultMemoryService::vector_status`); without
        // the `embeddings` feature the tier is simply `off`.
        let expected = if cfg!(feature = "embeddings") {
            "idle"
        } else {
            "off"
        };
        assert!(
            status.vector_tier.contains(expected),
            "{}",
            status.vector_tier
        );

        let claim_row = status
            .schemas
            .iter()
            .find(|s| s.schema_ref == MEMORY_CLAIM_SCHEMA.as_str())
            .expect("claim row");
        assert_eq!(claim_row.heads, 2);
        assert_eq!(claim_row.total, 2);

        let episode_row = status
            .schemas
            .iter()
            .find(|s| s.schema_ref == MEMORY_EPISODE_SCHEMA.as_str())
            .expect("episode row");
        assert_eq!(episode_row.heads, 1);
        assert_eq!(episode_row.total, 1);

        let instruction_row = status
            .schemas
            .iter()
            .find(|s| s.schema_ref == MEMORY_INSTRUCTION_SCHEMA.as_str())
            .expect("instruction row");
        assert_eq!(instruction_row.heads, 0);
        assert_eq!(instruction_row.total, 0);
    }

    #[tokio::test]
    async fn memory_status_total_counts_a_superseded_row_but_not_its_head() {
        let svc = svc();
        let old = svc
            .remember(
                "claim".into(),
                "Old".into(),
                "Flux is Jy.".into(),
                String::new(),
                -1.0,
                vec![],
                vec![],
            )
            .await;
        svc.supersede_claim(
            old.claim_id.clone(),
            "New".into(),
            "Flux is mJy.".into(),
            String::new(),
        )
        .await;

        let status = svc.memory_status().await;
        let claim_row = status
            .schemas
            .iter()
            .find(|s| s.schema_ref == MEMORY_CLAIM_SCHEMA.as_str())
            .expect("claim row");
        assert_eq!(claim_row.total, 2, "old + new both exist");
        assert_eq!(claim_row.heads, 1, "only the replacement is a head");
    }

    #[cfg(feature = "embeddings")]
    mod vector_tier {
        use super::*;
        use koine_embeddings::StoredVector;

        // ─── Vector tier (ADR-0028 D6) ──────────────────────────────────────
        //
        // No test below constructs a `SemanticSearch` — that downloads a
        // ~100MB ONNX model on first use, which has no place in a unit test.
        // `StubEmbedder` is the deterministic seam `vector_gate_confirm` /
        // `rerank_with_vectors` / `vector_status` are written against.
        //
        // None of these tests touch `IMPRESS_MEMORY_VECTORS` or any other env
        // var: `DefaultMemoryService::with_store_and_vector_tier` bypasses the
        // env gate and the process-wide `VECTOR_TIER` singleton entirely, so a
        // test below can never race an env-driven default-off test above
        // regardless of `cargo test`'s thread interleaving — deliberately, so
        // this module needs no `--test-threads=1`.

        /// A deterministic stand-in for `SemanticSearch`: exact input strings
        /// map to fixed vectors via a lookup table (`with`), with a
        /// caller-chosen default for anything not listed. This gives tests full
        /// control over cosine similarity without reasoning about what a real
        /// model would produce.
        struct StubEmbedder {
            model: String,
            vectors: HashMap<String, Vec<f32>>,
            default_vector: Vec<f32>,
        }

        impl StubEmbedder {
            fn new(model: &str) -> Self {
                Self {
                    model: model.to_string(),
                    vectors: HashMap::new(),
                    // Orthogonal to every explicit test vector used below, and
                    // never itself at or above VECTOR_CONFIRM_THRESHOLD against
                    // them — so a text nobody bothered to map never
                    // accidentally satisfies a similarity assertion.
                    default_vector: vec![0.0, 0.0, 1.0],
                }
            }

            fn with(mut self, text: impl Into<String>, vector: Vec<f32>) -> Self {
                self.vectors.insert(text.into(), vector);
                self
            }
        }

        impl Embedder for StubEmbedder {
            fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
                Ok(self
                    .vectors
                    .get(text)
                    .cloned()
                    .unwrap_or_else(|| self.default_vector.clone()))
            }

            fn model_id(&self) -> &str {
                &self.model
            }
        }

        /// A fresh temp-file sidecar path. The `TempDir` must be kept alive by
        /// the caller for as long as the path is used — dropping it deletes the
        /// directory.
        fn temp_sidecar_path() -> (tempfile::TempDir, String) {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir
                .path()
                .join("embeddings.sqlite")
                .to_string_lossy()
                .into_owned();
            (dir, path)
        }

        /// Write one memory-item vector directly into the sidecar at `path`,
        /// bypassing `remember` entirely — this is what a real backfill
        /// executor (`impel_memory::EmbedBackfillExecutor`) would already have
        /// done by the time these tests' scenarios begin. Opens its own
        /// `EmbeddingStore` handle, independent of whatever handle a
        /// `DefaultMemoryService` under test holds on the same path — safe,
        /// since `EmbeddingStore::open` sets WAL mode, which serves multiple
        /// connections to one file.
        fn seed_vector(path: &str, source_id: &str, model: &str, vector: Vec<f32>) {
            let store = EmbeddingStore::open(path).expect("open sidecar to seed");
            store
                .save_vectors(&[StoredVector {
                    id: uuid::Uuid::new_v4().to_string(),
                    source_id: source_id.to_string(),
                    source_type: MEMORY_ITEM_SOURCE_TYPE.to_string(),
                    vector,
                    model: model.to_string(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                }])
                .expect("seed vector");
        }

        /// A service with its own fresh in-memory item store and a vector tier
        /// unconditionally live against the sidecar at `path`.
        fn svc_with_vectors(path: &str, embedder: StubEmbedder) -> DefaultMemoryService {
            let embedding_store = EmbeddingStore::open(path).expect("open sidecar for service");
            DefaultMemoryService::with_store_and_vector_tier(
                test_store(),
                embedding_store,
                embedder,
            )
        }

        // -- remember: the vector half of the dedup gate -------------------

        #[tokio::test]
        async fn vector_confirm_matches_a_paraphrase_fts_misses() {
            let (_dir, path) = temp_sidecar_path();
            let model = "stub/v1";
            let title2 = "Currency exchange note";
            let body2 = "Exchange rates fluctuate throughout the trading day session.";
            let text2 = format!("{title2}\n{body2}");

            let embedder = StubEmbedder::new(model).with(text2.clone(), vec![1.0, 0.0, 0.0]);
            let svc = svc_with_vectors(&path, embedder);

            // First insert: the tier is live from the start, but the sidecar is
            // still empty, so this is a plain insert regardless.
            let first = svc
                .remember(
                    "claim".into(),
                    "Flux units".into(),
                    "The catalogue flux column is in millijansky.".into(),
                    String::new(),
                    -1.0,
                    vec![],
                    vec![],
                )
                .await;
            assert!(first.ok, "{}", first.message);
            assert_eq!(first.action, "inserted");

            seed_vector(&path, &first.claim_id, model, vec![1.0, 0.0, 0.0]);

            // A paraphrase in completely different words — the FTS gate's token
            // overlap misses it entirely — but the stub embeds `text2` to the
            // SAME vector as the one just seeded, so the vector gate must catch
            // it.
            let second = svc
                .remember(
                    "claim".into(),
                    title2.into(),
                    body2.into(),
                    String::new(),
                    -1.0,
                    vec![],
                    vec![],
                )
                .await;
            assert!(second.ok, "{}", second.message);
            assert_eq!(second.action, "confirmed", "{second:?}");
            assert_eq!(second.claim_id, first.claim_id);
            assert_eq!(second.matched_id, first.claim_id);
            assert_eq!(second.matched_via, "vector");
        }

        #[tokio::test]
        async fn vector_confirm_falls_through_when_candidate_is_superseded() {
            let (_dir, path) = temp_sidecar_path();
            let model = "stub/v1";
            let title2 = "New topic altogether";
            let body2 = "Something entirely unrelated to the earlier note.";
            let text2 = format!("{title2}\n{body2}");

            let embedder = StubEmbedder::new(model).with(text2.clone(), vec![1.0, 0.0, 0.0]);
            let svc = svc_with_vectors(&path, embedder);

            let old = svc
                .remember(
                    "claim".into(),
                    "Old topic".into(),
                    "A statement that will later be superseded.".into(),
                    String::new(),
                    -1.0,
                    vec![],
                    vec![],
                )
                .await;
            assert!(old.ok, "{}", old.message);

            let replacement = svc
                .supersede_claim(
                    old.claim_id.clone(),
                    "New version".into(),
                    "A corrected statement, worded very differently from the note above.".into(),
                    String::new(),
                )
                .await;
            assert!(replacement.ok, "{}", replacement.message);

            // The vector points at the OLD, now-superseded row.
            seed_vector(&path, &old.claim_id, model, vec![1.0, 0.0, 0.0]);

            let third = svc
                .remember(
                    "claim".into(),
                    title2.into(),
                    body2.into(),
                    String::new(),
                    -1.0,
                    vec![],
                    vec![],
                )
                .await;
            assert!(third.ok, "{}", third.message);
            assert_eq!(third.action, "inserted", "{third:?}");
            assert_ne!(third.claim_id, old.claim_id);
            assert_ne!(third.claim_id, replacement.new_id);
            assert_eq!(third.matched_via, "");
        }

        #[tokio::test]
        async fn vector_confirm_falls_through_when_candidate_is_forgotten() {
            let (_dir, path) = temp_sidecar_path();
            let model = "stub/v1";
            let title2 = "Yet another topic";
            let body2 = "Completely different wording from the forgotten note.";
            let text2 = format!("{title2}\n{body2}");

            let embedder = StubEmbedder::new(model).with(text2.clone(), vec![1.0, 0.0, 0.0]);
            let svc = svc_with_vectors(&path, embedder);

            let original = svc
                .remember(
                    "claim".into(),
                    "Private note".into(),
                    "A claim that will be withheld from recall.".into(),
                    String::new(),
                    -1.0,
                    vec![],
                    vec![],
                )
                .await;
            assert!(original.ok, "{}", original.message);

            let forgotten = svc.forget(original.claim_id.clone()).await;
            assert!(forgotten.ok, "{}", forgotten.message);

            seed_vector(&path, &original.claim_id, model, vec![1.0, 0.0, 0.0]);

            let third = svc
                .remember(
                    "claim".into(),
                    title2.into(),
                    body2.into(),
                    String::new(),
                    -1.0,
                    vec![],
                    vec![],
                )
                .await;
            assert!(third.ok, "{}", third.message);
            assert_eq!(third.action, "inserted", "{third:?}");
            assert_eq!(third.matched_via, "");
        }

        #[tokio::test]
        async fn vector_confirm_falls_through_on_claim_type_mismatch() {
            let (_dir, path) = temp_sidecar_path();
            let model = "stub/v1";
            let title2 = "A preference, not a fact";
            let body2 = "Worded nothing like the fact claim below, on purpose.";
            let text2 = format!("{title2}\n{body2}");

            let embedder = StubEmbedder::new(model).with(text2.clone(), vec![1.0, 0.0, 0.0]);
            let svc = svc_with_vectors(&path, embedder);

            let fact = svc
                .remember(
                    "claim".into(),
                    "A fact".into(),
                    "Water boils at 100 degrees Celsius at sea level.".into(),
                    "fact".into(),
                    -1.0,
                    vec![],
                    vec![],
                )
                .await;
            assert!(fact.ok, "{}", fact.message);

            seed_vector(&path, &fact.claim_id, model, vec![1.0, 0.0, 0.0]);

            let preference = svc
                .remember(
                    "claim".into(),
                    title2.into(),
                    body2.into(),
                    "preference".into(),
                    -1.0,
                    vec![],
                    vec![],
                )
                .await;
            assert!(preference.ok, "{}", preference.message);
            assert_eq!(preference.action, "inserted", "{preference:?}");
            assert_ne!(preference.claim_id, fact.claim_id);
            assert_eq!(preference.matched_via, "");
        }

        /// The walk is per-candidate: two ineligible rows OUTSCORING an eligible
        /// one must be stepped over, not allowed to block it — and eligibility is
        /// the kernel's per-id policy, never membership in a recency page.
        #[tokio::test]
        async fn vector_gate_skips_ineligible_candidates_and_confirms_an_eligible_one() {
            let (_dir, path) = temp_sidecar_path();
            let model = "stub/v1";
            let title2 = "Fresh wording";
            let body2 = "Worded like none of the notes seeded before it.";
            let text2 = format!("{title2}\n{body2}");

            let embedder = StubEmbedder::new(model).with(text2.clone(), vec![1.0, 0.0, 0.0]);
            let svc = svc_with_vectors(&path, embedder);

            let superseded = svc
                .remember(
                    "claim".into(),
                    "One".into(),
                    "A statement later corrected by its replacement.".into(),
                    String::new(),
                    -1.0,
                    vec![],
                    vec![],
                )
                .await;
            let withheld = svc
                .remember(
                    "claim".into(),
                    "Two".into(),
                    "A private observation the user withdraws.".into(),
                    String::new(),
                    -1.0,
                    vec![],
                    vec![],
                )
                .await;
            let eligible = svc
                .remember(
                    "claim".into(),
                    "Three".into(),
                    "The claim that genuinely deserves the confirmation.".into(),
                    String::new(),
                    -1.0,
                    vec![],
                    vec![],
                )
                .await;
            assert!(
                superseded.ok && withheld.ok && eligible.ok,
                "{superseded:?} {withheld:?} {eligible:?}"
            );
            let replaced = svc
                .supersede_claim(
                    superseded.claim_id.clone(),
                    "One, corrected".into(),
                    "Completely different corrective wording altogether.".into(),
                    String::new(),
                )
                .await;
            assert!(replaced.ok, "{}", replaced.message);
            let forgotten = svc.forget(withheld.claim_id.clone()).await;
            assert!(forgotten.ok, "{}", forgotten.message);

            // All three sit above the 0.92 bar against the draft's [1,0,0], and
            // the two ineligible ones score HIGHER than the eligible one.
            seed_vector(&path, &superseded.claim_id, model, vec![1.0, 0.0, 0.0]); // 1.00
            seed_vector(&path, &withheld.claim_id, model, vec![0.98, 0.198_997, 0.0]); // 0.98
            seed_vector(&path, &eligible.claim_id, model, vec![0.95, 0.312_25, 0.0]); // 0.95

            let out = svc
                .remember(
                    "claim".into(),
                    title2.into(),
                    body2.into(),
                    String::new(),
                    -1.0,
                    vec![],
                    vec![],
                )
                .await;
            assert!(out.ok, "{}", out.message);
            assert_eq!(out.action, "confirmed", "{out:?}");
            assert_eq!(out.claim_id, eligible.claim_id);
            assert_eq!(out.matched_via, "vector");
        }

        // -- recall: vector re-ranking --------------------------------------

        #[tokio::test]
        async fn recall_ranking_uses_vector_similarity() {
            let (_dir, path) = temp_sidecar_path();
            let model = "stub/v1";
            // The query text ("") embeds to something identical to claim A's
            // stored vector and orthogonal to claim B's.
            let embedder = StubEmbedder::new(model).with(String::new(), vec![1.0, 0.0]);
            let svc = svc_with_vectors(&path, embedder);

            let a = svc
                .remember(
                    "claim".into(),
                    "Topic A".into(),
                    "Alpha alpha alpha content, first claim.".into(),
                    String::new(),
                    -1.0,
                    vec![],
                    vec![],
                )
                .await;
            assert!(a.ok, "{}", a.message);
            let b = svc
                .remember(
                    "claim".into(),
                    "Topic B".into(),
                    "Beta beta beta content, second claim entirely.".into(),
                    String::new(),
                    -1.0,
                    vec![],
                    vec![],
                )
                .await;
            assert!(b.ok, "{}", b.message);

            seed_vector(&path, &a.claim_id, model, vec![1.0, 0.0]);
            seed_vector(&path, &b.claim_id, model, vec![0.0, 1.0]);

            let recalled = svc.recall(String::new(), String::new(), 0, false).await;
            assert!(recalled.ok, "{}", recalled.message);
            assert_eq!(recalled.entries.len(), 2, "{:?}", recalled.entries);
            assert_eq!(
                recalled.entries[0].id, a.claim_id,
                "the vector-nearer claim must rank first: {:?}",
                recalled.entries
            );
        }

        #[tokio::test]
        async fn recall_identical_tier_off_vs_tier_on_with_empty_sidecar() {
            let store = test_store();
            let svc_off = DefaultMemoryService::with_store(store.clone());

            svc_off
                .remember(
                    "claim".into(),
                    "First".into(),
                    "The first seeded claim about widgets.".into(),
                    String::new(),
                    -1.0,
                    vec![],
                    vec![],
                )
                .await;
            svc_off
                .remember(
                    "claim".into(),
                    "Second".into(),
                    "The second seeded claim about gadgets.".into(),
                    String::new(),
                    -1.0,
                    vec![],
                    vec![],
                )
                .await;
            svc_off
                .remember(
                    "episode".into(),
                    "Third".into(),
                    "An episode about the same widgets.".into(),
                    String::new(),
                    -1.0,
                    vec![],
                    vec![],
                )
                .await;

            let off_result = svc_off
                .recall("widgets".into(), String::new(), 0, false)
                .await;
            assert!(off_result.ok, "{}", off_result.message);
            assert!(!off_result.entries.is_empty(), "{:?}", off_result.entries);

            // Tier ON, but the sidecar is (and stays) completely empty — no
            // `seed_vector` call at all.
            let (_dir, path) = temp_sidecar_path();
            let embedding_store = EmbeddingStore::open(&path).expect("open empty sidecar");
            let svc_on = DefaultMemoryService::with_store_and_vector_tier(
                store.clone(),
                embedding_store,
                StubEmbedder::new("stub/v1"),
            );
            let on_result = svc_on
                .recall("widgets".into(), String::new(), 0, false)
                .await;
            assert!(on_result.ok, "{}", on_result.message);

            let off_ids: Vec<&str> = off_result.entries.iter().map(|e| e.id.as_str()).collect();
            let on_ids: Vec<&str> = on_result.entries.iter().map(|e| e.id.as_str()).collect();
            assert_eq!(
                off_ids, on_ids,
                "tier-on with an empty sidecar must not perturb order"
            );
            assert_eq!(off_result.entries.len(), on_result.entries.len());
        }

        // -- memory_status: coverage -----------------------------------------

        #[tokio::test]
        async fn memory_status_reports_vector_coverage_when_tier_live() {
            let (_dir, path) = temp_sidecar_path();
            let model = "stub/v1";
            let svc = svc_with_vectors(&path, StubEmbedder::new(model));

            let a = svc
                .remember(
                    "claim".into(),
                    "A".into(),
                    "First claim, entirely on its own topic.".into(),
                    String::new(),
                    -1.0,
                    vec![],
                    vec![],
                )
                .await;
            let b = svc
                .remember(
                    "claim".into(),
                    "B".into(),
                    "Second claim, a distinctly different topic.".into(),
                    String::new(),
                    -1.0,
                    vec![],
                    vec![],
                )
                .await;
            let c = svc
                .remember(
                    "claim".into(),
                    "C".into(),
                    "Third claim, yet another unrelated topic.".into(),
                    String::new(),
                    -1.0,
                    vec![],
                    vec![],
                )
                .await;
            assert!(a.ok && b.ok && c.ok, "{a:?} {b:?} {c:?}");

            // Only two of the three claims have been embedded.
            seed_vector(&path, &a.claim_id, model, vec![1.0, 0.0, 0.0]);
            seed_vector(&path, &b.claim_id, model, vec![0.0, 1.0, 0.0]);

            let status = svc.memory_status().await;
            assert!(status.ok, "{}", status.message);
            assert_eq!(
                status.vector_tier,
                format!("live (model {model}, 2/3 items embedded)")
            );
            assert!(
                (status.embedding_coverage - 2.0 / 3.0).abs() < 1e-9,
                "{}",
                status.embedding_coverage
            );
        }
    }
}
