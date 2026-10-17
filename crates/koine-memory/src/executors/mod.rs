//! ADR-0028 D7 — the memory kernel's two task-kernel executors, and the
//! spawner that keeps them fed.
//!
//! Both are ordinary [`koine_tasks::TaskExecutor`]s, so everything the ADR-0005
//! §6 loop already provides (readiness gating, acquire/retry/escalate, the
//! operation-journal retry ledger) applies unchanged, and neither executor
//! touches task state — that is the scheduler's exclusive job.
//!
//! * [`EmbedBackfillExecutor`] (`impress.memory.embed`) — one bounded window of
//!   embedding work per task, over memory items and `content-chunk@1.0.0`,
//!   written into the `impress-embeddings` sidecar.
//! * [`MemoryConsolidationExecutor`] (`impress.memory.consolidate`) — one
//!   bounded time window of terminal `agent-run@1.0.0` items per task, distilled
//!   into `memory/episode@1.0.0` rows through the D6 dedup gate. Optionally,
//!   also distilled into `memory/claim@1.0.0` rows by an LLM
//!   ([`claim_distill`], ADR-0028 P6) — off unless a [`ClaimDistiller`] is
//!   wired in, in which case a transport failure degrades to the
//!   deterministic-only result rather than retrying the task.
//! * [`plan_memory_tasks`] — called once per taskd pass; spawns at most one task
//!   of each kind, deduped on open state and cooled off after failure.
//!
//! # Why both executors hold an `Arc<dyn AttributedStore>`
//!
//! [`koine_tasks::TaskStoreApi`] is deliberately narrow: it is the *kernel*
//! surface (get/create/apply/add_edge/record_agent_run/transition), not a query
//! surface. These executors need `query` and
//! [`crate::ops`], none of which are on it, so they take the same
//! concrete store the scheduler runs against — exactly what
//! `koine_ai::AiStore` does for the inference executors in the same daemon.
//! Kernel writes still go through the `&dyn TaskStoreApi` handed to `execute`,
//! so provenance and attribution stay on the one path.
//!
//! # Idempotency is the executor's job, not the scheduler's
//!
//! `Scheduler::run_once`'s resume pass re-executes any task left `running` by a
//! crash. Both executors are therefore replay-safe by construction rather than
//! by bookkeeping: the embed executor's vector ids are a pure function of
//! `(source_id, model)` so a re-run is an UPSERT over the same rows, and the
//! consolidation executor's memory rows carry a `deterministic_key` derived
//! from the source run id so a re-run resolves to the same UUIDv5 and returns
//! the existing row untouched.

use std::sync::Mutex;

use impress_item::{ActorKind, ItemId, Value};
use impress_item::{OperationIntent, OperationSpec, OperationType, RetentionTier};
use koine_tasks::{TaskError, TaskStoreApi};
use uuid::Uuid;

pub mod claim_distill;
pub mod consolidate;
pub mod embed;
pub mod spawn;

pub use claim_distill::{ClaimDistiller, LlmDistiller};
pub use consolidate::{MemoryConsolidationExecutor, CONSOLIDATE_AGENT_ID, KIND_CONSOLIDATE};
pub use embed::{EmbedBackfillExecutor, EMBED_AGENT_ID, KIND_EMBED};
pub use spawn::{plan_memory_tasks, MemoryPlanConfig};

/// Fixed namespace for the sidecar ids the embed executor mints.
///
/// This is `UUIDv5(NAMESPACE_URL, "impress-memory-vector")`, computed once and
/// hardcoded as a `const` (the v5 constructor is not `const fn`) — the same
/// arrangement `crate::ops::MEMORY_NAMESPACE` uses, and pinned
/// against its own derivation by `namespace_is_the_documented_v5_uuid`.
///
/// **Never change it.** Vector ids are `UUIDv5(this, "{source_id}:{model}")`,
/// and `EmbeddingStore::save_vectors` is an `INSERT OR REPLACE` keyed by id.
/// Together those two facts are the *whole* of the executor's crash-replay
/// story: a batch killed halfway re-runs and writes the same ids over the same
/// rows, so there is no duplicate-detection pass to get wrong and no
/// "did I already do this one?" state to keep. Re-keying the namespace would
/// silently turn every replay into a duplicate insert instead.
pub const VECTOR_ID_NAMESPACE: Uuid = Uuid::from_u128(0x6d5e_6ec2_578c_5e4b_b6b6_c1ac_2321_9ba7);

/// The sidecar `source_type` for a `content-chunk@1.0.0` vector.
///
/// Deliberately the spelling `impress-mcp`'s chunk index already filters on
/// (`select_chunk_vectors` → `load_vectors_by_type_and_model("chunk", model)`),
/// so these land in the same population rather than a second, invisible one.
///
/// Note what that does *not* buy on its own: the MCP index also needs a sidecar
/// `chunks` row whose id equals the vector's `source_id` to map it back to a
/// publication, and our `source_id` is the `content-chunk@1.0.0` **item id**
/// from the shared store. Where the two ids differ the index skips the row —
/// the vector is still correct and still readable by
/// `load_vectors_by_type_and_model`, which is what the ADR-0028 D5 vector tier
/// reads. Nothing is written wrong; the chunk-index join simply is not this
/// executor's to complete.
pub const SOURCE_TYPE_CHUNK: &str = "chunk";

/// The sidecar `source_type` for a `memory/*` vector. A new population — no
/// reader filters on it yet; the vector tier of recall (ADR-0028 D5) is what
/// will.
pub const SOURCE_TYPE_MEMORY: &str = "memory-item";

/// The deterministic sidecar id for one `(source_id, model)` pair.
///
/// A pure function on purpose: a caller that wants to know whether a row has
/// been embedded can compute the id without asking the sidecar, and a test can
/// assert the replay property without a model.
pub fn vector_id(source_id: &str, model: &str) -> Uuid {
    Uuid::new_v5(
        &VECTOR_ID_NAMESPACE,
        format!("{source_id}:{model}").as_bytes(),
    )
}

/// Write one payload field on an existing item, through the kernel.
///
/// Legal, and worth being explicit about: only a *task's* `state` is
/// scheduler-exclusive (ADR-0015 D1). Every other payload field is ordinary
/// mutable item data, and going through `apply` means the write lands in the
/// operation journal with this executor's attribution, like every other kernel
/// write.
///
/// Shared by both executors: the embed cursor, the consolidation truncation
/// cursor and the consolidation finalization marker are one operation with
/// three different fields.
pub(crate) fn set_payload(
    store: &dyn TaskStoreApi,
    target_id: ItemId,
    field: &str,
    value: Value,
    actor: &str,
) -> Result<(), TaskError> {
    store.apply(OperationSpec {
        target_id,
        op_type: OperationType::SetPayload(field.into(), value),
        intent: OperationIntent::Routine,
        reason: None,
        batch_id: None,
        author: actor.to_string(),
        author_kind: ActorKind::Agent,
        // Compactable: a cursor or a completion marker is bookkeeping, not
        // research record. The next task's cursor supersedes it, and the chain
        // of done tasks is what D8 reads.
        retention: RetentionTier::Compactable,
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The embedder seam
// ---------------------------------------------------------------------------

/// Turns text into vectors.
///
/// The seam exists for one reason: `SemanticSearch::new()` downloads a ~100MB
/// ONNX model on first use, which has no place in a unit test run. With the
/// executor written against this trait, the mechanics it actually owns —
/// candidate selection, the keyset cursor, the vector ids, the sidecar write,
/// the cursor advance — are testable headlessly with a deterministic stub,
/// while production keeps the real model behind [`FastEmbedder`].
pub trait TextEmbedder: Send + Sync {
    /// The id stamped into `StoredVector.model`, and the id model-aware reads
    /// filter by (ADR-0028 D4). It must be what the *embedder* says, never what
    /// the caller asked for, or a fallback to a different model would write
    /// vectors under a label that does not describe them.
    fn model_id(&self) -> &str;

    /// Embed `texts`, returning one vector per input **in the same order**.
    /// A short or reordered result is a contract violation, and the executor
    /// treats a length mismatch as retryable rather than mis-assigning vectors.
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String>;
}

/// Obtains a [`TextEmbedder`] for a model id named in a task payload.
///
/// Separate from the trait because construction is the expensive, fallible,
/// network-touching half: the executor asks for an embedder once per task and
/// turns a failure into [`koine_tasks::TaskError::Retryable`] (the model host
/// being unreachable is transient), whereas embedding itself is pure compute.
pub trait EmbedderProvider: Send + Sync {
    fn embedder_for(&self, model: &str) -> Result<std::sync::Arc<dyn TextEmbedder>, String>;
}

/// The production provider: fastembed via `koine_embeddings::SemanticSearch`.
///
/// Caches the one embedder it has built, so a daemon running a task every poll
/// interval loads the model once rather than per task. Only the default model
/// is served; a task naming a different one is an error rather than a silent
/// substitution, because the model id is what every subsequent read filters by.
#[derive(Default)]
pub struct FastEmbedProvider {
    cached: Mutex<Option<std::sync::Arc<dyn TextEmbedder>>>,
}

impl FastEmbedProvider {
    pub fn new() -> Self {
        Self::default()
    }
}

impl EmbedderProvider for FastEmbedProvider {
    fn embedder_for(&self, model: &str) -> Result<std::sync::Arc<dyn TextEmbedder>, String> {
        if model != koine_embeddings::semantic::FASTEMBED_MODEL_ID {
            return Err(format!(
                "unsupported embedding model {model:?} (this provider serves only {:?})",
                koine_embeddings::semantic::FASTEMBED_MODEL_ID
            ));
        }
        let mut cached = self
            .cached
            .lock()
            .map_err(|_| "embedder cache poisoned".to_string())?;
        if let Some(existing) = cached.as_ref() {
            return Ok(existing.clone());
        }
        // Honors IMPRESS_FASTEMBED_CACHE (koine_embeddings::semantic docs),
        // which is how an offline or CI host points at a pre-populated cache
        // instead of attempting the first-run download.
        let search = koine_embeddings::SemanticSearch::new()
            .map_err(|e| format!("initialize fastembed: {e}"))?;
        let embedder: std::sync::Arc<dyn TextEmbedder> =
            std::sync::Arc::new(FastEmbedder::new(search));
        *cached = Some(embedder.clone());
        Ok(embedder)
    }
}

/// [`TextEmbedder`] over `koine_embeddings::SemanticSearch`.
///
/// **Embeds one text per call, in a loop.** `SemanticSearch` does expose a
/// batched path (`embed_publications`), but it builds its own text out of a
/// `(id, title, authors, abstract)` tuple and truncates the abstract at 1000
/// characters — reaching it with `(id, body, [], None)` would produce the right
/// string *today* by an incidental property of that assembly, and would
/// silently embed different text the moment the assembly changes. Embedding the
/// wrong text fails silently forever (the vectors look fine; recall is just
/// subtly wrong), so this pays the per-call overhead instead. A genuine
/// arbitrary-text batch method on `SemanticSearch` would slot in here without
/// the executor noticing.
pub struct FastEmbedder {
    search: koine_embeddings::SemanticSearch,
    model_id: String,
}

impl FastEmbedder {
    pub fn new(search: koine_embeddings::SemanticSearch) -> Self {
        let model_id = search.model_id().to_string();
        Self { search, model_id }
    }
}

impl TextEmbedder for FastEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let mut out = Vec::with_capacity(texts.len());
        for text in texts {
            out.push(
                self.search
                    .embed_text(text)
                    .map_err(|e| format!("embed: {e}"))?,
            );
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The namespace const is a hardcoded literal because `new_v5` is not
    /// `const fn`. This is the derivation it claims to be.
    #[test]
    fn namespace_is_the_documented_v5_uuid() {
        assert_eq!(
            VECTOR_ID_NAMESPACE,
            Uuid::new_v5(&Uuid::NAMESPACE_URL, b"impress-memory-vector")
        );
    }

    #[test]
    fn vector_ids_are_stable_and_model_scoped() {
        let a = vector_id("11111111-1111-4111-8111-111111111111", "fastembed/X");
        let b = vector_id("11111111-1111-4111-8111-111111111111", "fastembed/X");
        let c = vector_id("11111111-1111-4111-8111-111111111111", "fastembed/Y");
        assert_eq!(a, b, "replay must land on the same sidecar row");
        assert_ne!(a, c, "a different model is a different vector");
    }
}
