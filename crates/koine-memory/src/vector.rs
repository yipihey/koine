//! The memory service's vector tier (ADR-0028 D6): opt-in, lazy,
//! process-wide; compiled only with the `embeddings` feature.

use koine_embeddings::EmbeddingStore;
#[cfg(feature = "embedder")]
use koine_embeddings::SemanticSearch;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Vector tier (ADR-0028 D6) — opt-in, lazy, process-wide.
// ---------------------------------------------------------------------------
//
// This section is everything the vector tier needs, self-contained: the
// embedder seam, the lazily-built process singleton, the env gate, and the
// read-only coverage counter `memory_status` uses instead of it. Nothing
// outside this section reaches `koine_embeddings::SemanticSearch`,
// `EmbeddingStore`, or the two env vars directly — `remember`/`recall` go
// through `DefaultMemoryService::vector_tier`, and `memory_status` goes
// through `DefaultMemoryService::vector_status`.

/// Env var that overrides the vector tier's presence gate, in either
/// direction. `1`/`true`/`on` forces it on, `0`/`false`/`off` forces it off;
/// unset (or anything else) leaves the decision to [`memory_vectors_present`]
/// — see the module docs.
pub(crate) const VECTOR_TIER_ENV: &str = "IMPRESS_MEMORY_VECTORS";

/// Env var overriding the embeddings sidecar path for this process. Mirrors
/// `impel_memory::embed::EMBEDDINGS_PATH_ENV` and the default
/// `impress_mcp::default_embeddings_path()` resolves to
/// (`dirs::data_dir()/imbib/embeddings.sqlite`) — REPLICATED here rather
/// than imported, the same way `impel-memory` replicates rather than
/// imports `impress-mcp`'s (private, and the wrong dependency direction:
/// `impress-mcp` already depends on this crate) copy. Three copies of one
/// six-line function is accepted duplication, not a design; a shared
/// `impress-embeddings`-side helper would be the right fix (see this PR's
/// report for the pointer).
pub(crate) const EMBEDDINGS_PATH_ENV: &str = "IMPRESS_EMBEDDINGS_PATH";

/// The sidecar `source_type` a memory-item vector is written under. Mirrors
/// `impel_memory::SOURCE_TYPE_MEMORY` (duplicated for the same reason as
/// [`EMBEDDINGS_PATH_ENV`] — this crate does not depend on `impel-memory`).
pub(crate) const MEMORY_ITEM_SOURCE_TYPE: &str = "memory-item";

/// Cosine similarity at or above which the vector tier's half of `remember`'s
/// dedup gate confirms an existing memory instead of inserting a new one —
/// the semantic-tier counterpart to `memory_ops::GATE_CONFIRM_THRESHOLD`
/// (0.85 token overlap).
///
/// Deliberately HIGHER than the FTS threshold. `remember` bodies are
/// typically a sentence or two, and short texts embed into a tighter region
/// of a sentence-embedding model's vector space than long ones do — two
/// genuinely UNRELATED short claims routinely cosine well above 0.85 purely
/// from sharing sentence structure and common words, so reusing the FTS
/// number here would over-merge distinct memories. 0.92 is a conservative
/// choice for the same asymmetric-cost reason `GATE_CONFIRM_THRESHOLD` is
/// high rather than moderate: confirming when it should have inserted loses
/// a distinct memory outright, while inserting when it should have confirmed
/// only costs one redundant row a later consolidation pass can merge.
pub(crate) const VECTOR_CONFIRM_THRESHOLD: f32 = 0.92;

/// Turns text into a vector. The seam that keeps the vector tier testable
/// without ever constructing a real `SemanticSearch` — first use downloads a
/// ~100MB ONNX model, which has no place in a unit test. Implemented for the
/// real embedder below; tests implement it with a deterministic stub.
/// `impel_memory::TextEmbedder` is the identical seam on the WRITE side of
/// this same sidecar, for the identical reason.
pub(crate) trait Embedder: Send + Sync {
    /// Embed `text`. `Err` is always treated as "this one call degrades",
    /// never as "turn the tier off" — see `DefaultMemoryService::vector_tier`'s
    /// callers.
    fn embed(&self, text: &str) -> Result<Vec<f32>, String>;

    /// The id stamped into / filtered by `StoredVector.model` (ADR-0028 D4).
    /// Must be what THIS embedder actually is, never a caller's request.
    fn model_id(&self) -> &str;
}

#[cfg(feature = "embedder")]
impl Embedder for SemanticSearch {
    fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        self.embed_text(text).map_err(|e| e.to_string())
    }

    fn model_id(&self) -> &str {
        SemanticSearch::model_id(self)
    }
}

/// The live vector tier: an opened sidecar plus a ready embedder. Built at
/// most once per process by [`process_vector_tier`] — or, in tests,
/// constructed directly against a temp sidecar and a stub embedder, via
/// [`DefaultMemoryService::with_store_and_vector_tier`], bypassing the env
/// gate and the process-wide singleton entirely so tests never race each
/// other over shared process state.
pub(crate) struct VectorTierInner {
    pub(crate) embedding_store: EmbeddingStore,
    pub(crate) embedder: Box<dyn Embedder>,
}

/// The result of the one, at-most-once attempt to build [`VectorTierInner`]
/// for this process.
pub(crate) enum TierState {
    Live(VectorTierInner),
    /// Carries the reason so `memory_status` can report
    /// `"unavailable: <reason>"` — the plain `Option` `impress-mcp::tools::ToolContext`
    /// caches for its own degraded mode is not enough here because, unlike
    /// that context, this service ALSO ANSWERS A STATUS QUERY about the
    /// failure rather than only degrading tool calls.
    Failed(String),
}

/// What [`VECTOR_TIER_ENV`] says, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VectorTierGate {
    /// Forced on: build the tier even before a single vector exists.
    On,
    /// Forced off: never build it, however many vectors exist.
    Off,
    /// Unset — decide from whether there are vectors to read.
    ByPresence,
}

/// Read the override. Cheap and stateless: the env var is read fresh every
/// call, because the only guarantee required is "never touch [`VECTOR_TIER`]
/// when this says off", not "cache the answer".
pub(crate) fn vector_tier_gate() -> VectorTierGate {
    match std::env::var(VECTOR_TIER_ENV) {
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "on" => VectorTierGate::On,
            "0" | "false" | "off" => VectorTierGate::Off,
            // An unrecognised value is not a vote. Guessing which way a typo
            // meant is worse than falling back to the automatic answer.
            _ => VectorTierGate::ByPresence,
        },
        Err(_) => VectorTierGate::ByPresence,
    }
}

/// Whether the sidecar already holds `memory-item` vectors for the model this
/// process would embed with.
///
/// One `EXISTS` against a small SQLite file: no model load, no vector blobs
/// read — which is what makes it usable as the gate rather than only as a
/// status reading. A machine that has never run the embed sweep answers
/// `false` and pays nothing beyond the file open.
pub(crate) fn memory_vectors_present() -> bool {
    // A unit test must never consult the developer's real sidecar: the
    // default path is `dirs::data_dir()/imbib/embeddings.sqlite`, and a
    // machine that has run the embed sweep would otherwise make this crate's
    // no-override tests load a 100MB ONNX model. The explicit
    // `IMPRESS_MEMORY_VECTORS=1` override still reaches the forced-on branch
    // under test, which is how the forced path is exercised.
    if cfg!(test) {
        return false;
    }
    let Some(path) = resolve_embeddings_path() else {
        return false;
    };
    let Ok(store) = EmbeddingStore::open(&path) else {
        return false;
    };
    store
        .has_vectors_for_source_and_model(
            MEMORY_ITEM_SOURCE_TYPE,
            koine_embeddings::FASTEMBED_MODEL_ID,
        )
        .unwrap_or(false)
}

/// Resolve the sidecar path: [`EMBEDDINGS_PATH_ENV`], then the platform
/// default. `None` only when the platform has no data directory AND the env
/// var is unset — a machine misconfiguration [`try_init_vector_tier`] turns
/// into a `Failed` state like any other.
pub(crate) fn resolve_embeddings_path() -> Option<String> {
    if let Ok(path) = std::env::var(EMBEDDINGS_PATH_ENV) {
        if !path.trim().is_empty() {
            return Some(path);
        }
    }
    dirs::data_dir().map(|dir| {
        dir.join("imbib/embeddings.sqlite")
            .to_string_lossy()
            .into_owned()
    })
}

/// Open the sidecar and load the embedding model. The fallible half of
/// building a [`VectorTierInner`], separated from [`init_vector_tier`] so the
/// `stderr` line lives in exactly one place regardless of which step failed.
#[cfg(feature = "embedder")]
pub(crate) fn try_init_vector_tier() -> Result<VectorTierInner, String> {
    let path = resolve_embeddings_path().ok_or_else(|| {
        "no data directory available and IMPRESS_EMBEDDINGS_PATH is unset".to_string()
    })?;
    let embedding_store =
        EmbeddingStore::open(&path).map_err(|e| format!("open sidecar {path}: {e}"))?;
    let embedder = SemanticSearch::new().map_err(|e| format!("initialize embedding model: {e}"))?;
    Ok(VectorTierInner {
        embedding_store,
        embedder: Box::new(embedder),
    })
}

/// Feature-off twin: binaries built without `vector-embedder` (the FFI
/// consumers — impel-tools must never link the ONNX runtime) get the same
/// degraded path a missing model produces at runtime.
#[cfg(not(feature = "embedder"))]
pub(crate) fn try_init_vector_tier() -> Result<VectorTierInner, String> {
    Err("built without the embedder feature (FTS tier only)".to_string())
}

/// [`try_init_vector_tier`], with the one-time `stderr` line on failure. Only
/// ever invoked by [`VECTOR_TIER`]'s `get_or_init` — i.e. AT MOST ONCE per
/// process — so this printing here (rather than at every call site that sees
/// a cached `Failed`) is what keeps it to one line for the process lifetime,
/// imitating `impress-mcp::tools::ToolContext::semantic`'s degraded-mode
/// discipline.
pub(crate) fn init_vector_tier() -> TierState {
    match try_init_vector_tier() {
        Ok(inner) => TierState::Live(inner),
        Err(e) => {
            eprintln!("koine-memory: vector tier unavailable: {e}");
            TierState::Failed(e)
        }
    }
}

/// Process-wide, at-most-once vector tier state. Reached ONLY through
/// [`process_vector_tier`] (which gates on the env var first) and peeked
/// (never initialized) by `memory_status` via a bare `.get()` — see the
/// module docs' cost rule.
pub(crate) static VECTOR_TIER: OnceLock<TierState> = OnceLock::new();

/// The live tier for `remember`/`recall`'s production path, or `None` when
/// it is off, still uninitialized-and-this-call-does-not-need-it-yet (it
/// does; calling this always resolves it), or unavailable. Building it here,
/// lazily, on the first call that needs it is what satisfies the cost rule:
/// nothing pays for a sidecar open or a model load unless the env var is set
/// AND a memory verb actually runs.
pub(crate) fn process_vector_tier() -> Option<&'static VectorTierInner> {
    // Already resolved: the gate question was settled when it mattered, and
    // re-asking would cost a sidecar open on every memory verb forever.
    if let Some(state) = VECTOR_TIER.get() {
        return match state {
            TierState::Live(inner) => Some(inner),
            TierState::Failed(_) => None,
        };
    }
    match vector_tier_gate() {
        VectorTierGate::Off => return None,
        VectorTierGate::On => {}
        // Nothing embedded yet — the model load would buy an empty search.
        // Not cached: the sweep may write the first vectors at any point in
        // this process's life, and a latch here is how the tier stayed dark.
        VectorTierGate::ByPresence if !memory_vectors_present() => return None,
        VectorTierGate::ByPresence => {}
    }
    match VECTOR_TIER.get_or_init(init_vector_tier) {
        TierState::Live(inner) => Some(inner),
        TierState::Failed(_) => None,
    }
}

/// `N / M`, or `0.0` when there is nothing to divide by — never a division
/// producing NaN or infinity.
pub(crate) fn coverage_ratio(embedded: usize, total: u32) -> f64 {
    if total == 0 {
        0.0
    } else {
        embedded as f64 / total as f64
    }
}

/// Vector coverage WITHOUT constructing a [`SemanticSearch`] — opens the
/// sidecar (cheap: no model, no HNSW build) and counts memory-item vectors
/// for `model`. This is the whole reason `memory_status` does not simply
/// call [`process_vector_tier`]: that path may need to load the embedding
/// model, and a status query must never pay for that just to answer.
///
/// `0.0` when the sidecar cannot be opened (including "does not exist yet")
/// — an absent sidecar means zero vectors, not an error worth surfacing here
/// (the `vector_tier` status string already says `"initializing lazily"` or
/// `"unavailable: ..."` in the cases where that distinction matters).
pub(crate) fn readonly_coverage(model: &str, total_items: u32) -> f64 {
    let Some(path) = resolve_embeddings_path() else {
        return 0.0;
    };
    let Ok(store) = EmbeddingStore::open(&path) else {
        return 0.0;
    };
    let embedded = store
        .load_vectors_by_type_and_model(MEMORY_ITEM_SOURCE_TYPE, model)
        .map(|v| v.len())
        .unwrap_or(0);
    coverage_ratio(embedded, total_items)
}
