//! `impress.memory.embed` — one bounded window of embedding backfill per task
//! (ADR-0028 D7).
//!
//! The task item carries its own window: a keyset cursor in, the cursor it
//! reached out. That is D8's rule — the watermark lives in the task chain, not
//! in a side table — and it is what lets [`crate::executors::plan_memory_tasks`] compute
//! the next window from nothing but the last completed task.
//!
//! # Why a keyset cursor and not `OFFSET`
//!
//! `idx_items_schema_created` is `(schema_ref, created)`. A
//! `(created, id) > (cursor_created, cursor_id)` predicate rides that index and
//! reads only the rows it returns; `LIMIT n OFFSET k` re-walks every skipped
//! row on every pass, so a backfill over a large library would get
//! quadratically slower exactly as it made progress. The `id` half of the
//! cursor is what makes it correct rather than merely fast: `created` is a
//! millisecond stamp and a bulk import writes hundreds of rows inside one
//! millisecond, so a cursor on `created` alone either re-embeds them forever or
//! skips them entirely, depending on which comparison you pick.
//!
//! # Why the executor probes the sidecar before embedding
//!
//! The window a spawned task scans overlaps the boundary millisecond of the
//! last completed one (see `plan_embed`'s rewind), and nothing in the items
//! store marks a row as embedded — the vectors live in the sidecar. Without
//! the probe, already-embedded rows re-enter the LIMIT-capped window, and a
//! same-millisecond cluster of at least `batch_limit` rows regenerates the
//! identical window forever: every run reports a full batch embedded while the
//! rows behind the cluster — and everything written after them — never get a
//! vector. One batched sidecar read per page turns those rows into skips, so
//! only unembedded work consumes the window and the cursor's forward progress
//! is structural rather than probabilistic.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use impress_item::schema::refs::CONTENT_CHUNK as CONTENT_CHUNK_SCHEMA;
use impress_item::{AttributedStore, ItemStore};
use impress_item::{Item, ItemId, Value};
use koine_embeddings::{EmbeddingStore, StoredVector};
use koine_tasks::{AgentRunRecord, ExecutionOutcome, TaskError, TaskExecutor, TaskStoreApi};
use sha2::{Digest, Sha256};

use crate::executors::{
    vector_id, EmbedderProvider, FastEmbedProvider, SOURCE_TYPE_CHUNK, SOURCE_TYPE_MEMORY,
};

/// Dispatch key: the `task_kind` payload field the scheduler matches on.
pub const KIND_EMBED: &str = "impress.memory.embed";

/// `agent_id` stamped on this executor's `agent-run@1.0.0` provenance rows.
pub const EMBED_AGENT_ID: &str = "impel-memory/embed";

/// Env var overriding the sidecar path for the whole process. A task's
/// `sidecar_path` payload field still wins over it — the task is the more
/// specific instruction, and the tests rely on that ordering.
pub const EMBEDDINGS_PATH_ENV: &str = "IMPRESS_EMBEDDINGS_PATH";

/// Rows embedded per task when the payload does not say.
pub const DEFAULT_BATCH_LIMIT: i64 = 256;

/// Ceiling on `batch_limit`, however large the payload asks for. One task is
/// meant to be a bounded unit of work the scheduler can retry cheaply; a
/// 100 000-row "batch" is a long-running job wearing a task's clothes.
pub const MAX_BATCH_LIMIT: i64 = 4096;

/// Ceiling on candidate rows one run may FETCH across pages, however many of
/// them turn out to need no embedding. The page loop in `execute` walks past
/// already-embedded (and textless) rows so a same-millisecond cluster larger
/// than `batch_limit` completes within one run instead of regenerating the
/// same window forever; this cap keeps that walk a bounded unit of work. It
/// cannot reintroduce the livelock it exists alongside: the only fetched
/// stretch that fails to advance the recorded cursor is the rewound
/// boundary-millisecond overlap, and 65 536 rows written inside one or two
/// milliseconds is beyond what the store can physically absorb.
pub const MAX_SCAN_ROWS: i64 = 65_536;

/// The schemas this backfill covers, in the order the ADR names them.
///
/// Consts, never literals: the store matches `schema_ref` by exact equality, so
/// a second spelling here is a silently-empty scan forever (root CLAUDE.md,
/// "Definition of done — schema refs").
pub fn embeddable_schemas() -> [&'static str; 4] {
    [
        impress_item::schema::names::CONTENT_CHUNK,
        impress_item::schema::names::MEMORY_CLAIM,
        impress_item::schema::names::MEMORY_EPISODE,
        impress_item::schema::names::MEMORY_INSTRUCTION,
    ]
}

/// Embeddable rows strictly after `(cursor_created_ms, cursor_id)` in
/// `(created, id)` keyset order, at most `limit`: one keyset query per
/// embeddable schema over the store port, merged.
pub(crate) fn candidates_after(
    store: &dyn AttributedStore,
    cursor_created_ms: i64,
    cursor_id: &str,
    limit: i64,
) -> Result<Vec<Item>, impress_item::StoreError> {
    use impress_item::{ItemQuery, Predicate, SortDescriptor};
    let limit = limit.max(0) as usize;
    let after = Predicate::Or(vec![
        Predicate::Gt("created".into(), Value::Int(cursor_created_ms)),
        Predicate::And(vec![
            Predicate::Eq("created".into(), Value::Int(cursor_created_ms)),
            Predicate::Gt("id".into(), Value::String(cursor_id.to_string())),
        ]),
    ]);
    let mut rows = Vec::new();
    for schema in embeddable_schemas() {
        let Ok(schema) = schema.parse::<impress_item::SchemaRef>() else {
            continue;
        };
        rows.extend(store.query(&ItemQuery {
            schema: Some(schema),
            predicates: vec![after.clone()],
            sort: vec![
                SortDescriptor {
                    field: "created".into(),
                    ascending: true,
                },
                SortDescriptor {
                    field: "id".into(),
                    ascending: true,
                },
            ],
            limit: Some(limit),
            include_tags: false,
            include_references: false,
            ..Default::default()
        })?);
    }
    rows.sort_by(|a, b| {
        a.created
            .timestamp_millis()
            .cmp(&b.created.timestamp_millis())
            .then_with(|| a.id.to_string().cmp(&b.id.to_string()))
    });
    rows.truncate(limit);
    Ok(rows)
}

/// The default sidecar location: the same file `impress-mcp` opens.
///
/// `impress_mcp::default_embeddings_path()` is `dirs::data_dir()/imbib/…` and
/// is private to that binary, so this replicates it rather than importing it —
/// with the env override in front, so a daemon can be pointed elsewhere without
/// a rebuild.
pub fn default_sidecar_path() -> Option<String> {
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

/// One window of embedding work.
pub struct EmbedBackfillExecutor {
    store: Arc<dyn AttributedStore>,
    provider: Arc<dyn EmbedderProvider>,
    /// Used when the task payload names no `sidecar_path`. `None` means "ask
    /// [`default_sidecar_path`] at execution time", which is what lets the env
    /// override be read late rather than frozen at construction.
    sidecar_path: Option<String>,
    actor: String,
}

impl EmbedBackfillExecutor {
    /// Production wiring: fastembed behind the seam, default sidecar.
    pub fn new(store: Arc<dyn AttributedStore>) -> Self {
        Self::with_provider(store, Arc::new(FastEmbedProvider::new()))
    }

    /// Inject an embedder provider — the test seam, and the hook for a future
    /// on-device or remote embedder.
    pub fn with_provider(
        store: Arc<dyn AttributedStore>,
        provider: Arc<dyn EmbedderProvider>,
    ) -> Self {
        Self {
            store,
            provider,
            sidecar_path: None,
            actor: EMBED_AGENT_ID.to_string(),
        }
    }

    /// Pin the sidecar path for tasks that do not name one.
    pub fn with_sidecar_path(mut self, path: impl Into<String>) -> Self {
        self.sidecar_path = Some(path.into());
        self
    }

    /// Where this task's vectors go: task payload, then constructor, then env
    /// / platform default.
    fn sidecar_for(&self, task: &Item) -> Result<String, TaskError> {
        if let Some(path) = payload_string(task, "sidecar_path") {
            if !path.trim().is_empty() {
                return Ok(path);
            }
        }
        if let Some(path) = &self.sidecar_path {
            return Ok(path.clone());
        }
        default_sidecar_path().ok_or_else(|| {
            // No home/data directory and no override: a machine misconfiguration
            // a retry cannot fix.
            TaskError::Permanent(
                "no embeddings sidecar path: set the task's sidecar_path or \
                 IMPRESS_EMBEDDINGS_PATH"
                    .into(),
            )
        })
    }

    /// Candidate rows strictly after the cursor, in keyset order.
    ///
    /// Returns `(id, created_ms, schema_ref)`; the payload is fetched per row
    /// afterwards. Splitting it that way keeps this query on the covering index
    /// instead of dragging every payload blob through the scan just to discover
    /// which rows have text.
    fn candidates(
        &self,
        cursor_created_ms: i64,
        cursor_id: &str,
        limit: i64,
    ) -> Result<Vec<(ItemId, i64, String)>, TaskError> {
        let rows = candidates_after(self.store.as_ref(), cursor_created_ms, cursor_id, limit)
            .map_err(|e| TaskError::Retryable(format!("scan candidates: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|item| {
                (
                    item.id,
                    item.created.timestamp_millis(),
                    item.schema.to_string(),
                )
            })
            .collect())
    }

    /// Write one payload field on the task item itself — the cursor advance.
    /// See [`crate::executors::set_payload`] for why an executor may write payload.
    fn set_payload(
        &self,
        task_id: ItemId,
        field: &str,
        value: Value,
        store: &dyn TaskStoreApi,
    ) -> Result<(), TaskError> {
        crate::executors::set_payload(store, task_id, field, value, &self.actor)
    }
}

#[async_trait]
impl TaskExecutor for EmbedBackfillExecutor {
    fn task_kind(&self) -> &str {
        KIND_EMBED
    }

    async fn execute(
        &self,
        task: &Item,
        store: &dyn TaskStoreApi,
    ) -> Result<ExecutionOutcome, TaskError> {
        let started = Instant::now();
        let cursor_created_ms = payload_i64(task, "cursor_created_ms").unwrap_or(0);
        let cursor_id = payload_string(task, "cursor_id").unwrap_or_default();
        let batch_limit = payload_i64(task, "batch_limit")
            .unwrap_or(DEFAULT_BATCH_LIMIT)
            .clamp(1, MAX_BATCH_LIMIT);
        let requested_model = payload_string(task, "model")
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| koine_embeddings::semantic::FASTEMBED_MODEL_ID.to_string());
        let sidecar_path = self.sidecar_for(task)?;

        let mut page = self.candidates(cursor_created_ms, &cursor_id, batch_limit)?;

        // Hoisted before the model load and the sidecar open: an empty window
        // must not pay a ~100MB model initialization — or even a file open —
        // to discover it has nothing to do, and on a caught-up store that is
        // the common case, once per poll interval, forever.
        if page.is_empty() {
            self.set_payload(
                task.id,
                "cursor_end_created_ms",
                Value::Int(cursor_created_ms),
                store,
            )?;
            self.set_payload(
                task.id,
                "cursor_end_id",
                Value::String(cursor_id.clone()),
                store,
            )?;
            self.set_payload(task.id, "embedded_count", Value::Int(0), store)?;
            store.record_agent_run(
                task.id,
                AgentRunRecord {
                    agent_id: self.actor.clone(),
                    model: requested_model,
                    prompt_hash: prompt_hash(cursor_created_ms, &cursor_id, batch_limit),
                    result_summary: Some("0 embedded (window empty)".into()),
                    token_count: None,
                    duration_ms: Some(started.elapsed().as_millis() as i64),
                    executor_kind: Some(koine_tasks::EXECUTOR_MODEL.into()),
                },
            )?;
            return Ok(ExecutionOutcome::Complete);
        }

        // At least one candidate exists, so the sidecar is needed either way
        // now: for the already-embedded probe, and for the save if anything
        // survives it.
        let sidecar = EmbeddingStore::open(&sidecar_path)
            .map_err(|e| TaskError::Retryable(format!("open sidecar {sidecar_path}: {e}")))?;

        // The cursor advances to the last row FETCHED, not the last row
        // embedded. A page of rows with no work in it is progress: leaving the
        // cursor behind those rows would re-examine them on every future pass
        // and the backfill would never reach the rows after them.
        let (mut last_id, mut last_created, _) = page
            .last()
            .cloned()
            .expect("non-empty checked directly above");

        // Walk pages until one yields real work (or the candidates run out).
        // The window's rewound start re-covers the last task's boundary
        // millisecond, so leading rows routinely carry a vector already;
        // skipping them by sidecar probe — never re-embedding them — is what
        // lets a same-millisecond cluster larger than `batch_limit` finish
        // within one run instead of regenerating the identical window forever.
        // The walk is bounded in pages, not spawner passes: one run either
        // embeds a batch or moves the cursor up to MAX_SCAN_ROWS forward.
        let max_pages = (MAX_SCAN_ROWS / batch_limit).max(1);
        let mut pages = 1i64;
        let mut fetched = 0usize; // candidate rows fetched, across all pages
        let mut already = 0usize; // skipped: the sidecar has the vector
        let mut texts: Vec<String> = Vec::new();
        let mut targets: Vec<(ItemId, &'static str)> = Vec::new();
        loop {
            fetched += page.len();
            if let Some((id, created, _)) = page.last() {
                last_id = *id;
                last_created = *created;
            }

            // ONE batched probe per page: which of these rows already hold a
            // vector under the (source_type, model) this run would stamp?
            // `requested_model` is deliberately the probe's model — it is what
            // the spawner wrote into the task (the IMPRESS_MEMORY_EMBED_MODEL
            // override included) and what the save below stamps. A provider
            // that substituted a different model would make the probe miss and
            // the rows re-embed under the substitute — a redundant write,
            // never a wrong skip.
            let ids: Vec<String> = page.iter().map(|(id, _, _)| id.to_string()).collect();
            let embedded_already: HashSet<(String, String)> = sidecar
                .embedded_sources_for_model(&ids, &requested_model)
                .map_err(|e| TaskError::Retryable(format!("probe sidecar {sidecar_path}: {e}")))?
                .into_iter()
                .collect();

            for (id, _, schema) in &page {
                let source_type = source_type_for(schema);
                if embedded_already.contains(&(id.to_string(), source_type.to_string())) {
                    // No ONNX, no save: the vector is there. This is both the
                    // forward-progress guarantee and the quiescence one — a
                    // boundary row re-fetched by the rewound window costs one
                    // set lookup, not a re-embedding.
                    already += 1;
                    continue;
                }
                let Some(item) = ItemStore::get(self.store.as_ref(), *id)
                    .map_err(|e| TaskError::Retryable(format!("load candidate {id}: {e}")))?
                else {
                    // Deleted between the scan and now. Not an error; the
                    // cursor moves past it either way.
                    continue;
                };
                let (text, source_type) = match embeddable_text(&item, schema) {
                    Some(pair) => pair,
                    // A data condition, not a failure: a chunk with no text
                    // and a memory row with an empty title+body have nothing
                    // to embed.
                    None => continue,
                };
                texts.push(text);
                targets.push((*id, source_type));
            }

            // One batch of real work per run: found some, go do it.
            if !texts.is_empty() {
                break;
            }
            // A short page is the end of the candidates; a full one may have
            // more behind it — keep walking, within the page budget.
            if (page.len() as i64) < batch_limit || pages >= max_pages {
                break;
            }
            page = self.candidates(last_created, &last_id.to_string(), batch_limit)?;
            if page.is_empty() {
                break;
            }
            pages += 1;
        }

        let mut embedded = 0usize;
        let model_id = if texts.is_empty() {
            requested_model.clone()
        } else {
            let embedder = self
                .provider
                .embedder_for(&requested_model)
                // Transient by default: an unreachable model host or a failed
                // first-run download is exactly what retry is for.
                .map_err(|e| TaskError::Retryable(format!("embedding model unavailable: {e}")))?;
            let vectors = embedder
                .embed_batch(&texts)
                .map_err(|e| TaskError::Retryable(format!("embedding failed: {e}")))?;
            if vectors.len() != texts.len() {
                return Err(TaskError::Retryable(format!(
                    "embedder returned {} vectors for {} texts",
                    vectors.len(),
                    texts.len()
                )));
            }
            let model_id = embedder.model_id().to_string();
            let created_at = chrono::Utc::now().to_rfc3339();
            let stored: Vec<StoredVector> = targets
                .iter()
                .zip(vectors)
                .map(|((id, source_type), vector)| {
                    let source_id = id.to_string();
                    StoredVector {
                        id: vector_id(&source_id, &model_id).to_string(),
                        source_id,
                        source_type: (*source_type).to_string(),
                        vector,
                        model: model_id.clone(),
                        created_at: created_at.clone(),
                    }
                })
                .collect();

            embedded = sidecar
                .save_vectors(&stored)
                .map_err(|e| TaskError::Retryable(format!("save vectors: {e}")))?;
            model_id
        };

        self.set_payload(
            task.id,
            "cursor_end_created_ms",
            Value::Int(last_created),
            store,
        )?;
        self.set_payload(
            task.id,
            "cursor_end_id",
            Value::String(last_id.to_string()),
            store,
        )?;
        self.set_payload(
            task.id,
            "embedded_count",
            Value::Int(embedded as i64),
            store,
        )?;

        store.record_agent_run(
            task.id,
            AgentRunRecord {
                agent_id: self.actor.clone(),
                model: model_id,
                prompt_hash: prompt_hash(cursor_created_ms, &cursor_id, batch_limit),
                result_summary: Some(format!(
                    "{embedded} embedded, {already} already embedded, of {fetched} candidate(s)"
                )),
                token_count: None,
                duration_ms: Some(started.elapsed().as_millis() as i64),
                executor_kind: Some(koine_tasks::EXECUTOR_MODEL.into()),
            },
        )?;
        Ok(ExecutionOutcome::Complete)
    }

    fn max_retries(&self) -> u32 {
        3
    }

    /// `TaskError::Store` is a store access failure — under WAL with three apps
    /// and a sync engine on one file, overwhelmingly `SQLITE_BUSY`. The trait's
    /// default retries `Retryable` only, so without this override a moment of
    /// write contention would fail a backfill window permanently and the cursor
    /// would never advance past it.
    fn is_retryable(&self, error: &TaskError) -> bool {
        matches!(error, TaskError::Retryable(_) | TaskError::Store(_))
    }
}

/// The sidecar `source_type` this executor stamps for a row of `schema`.
///
/// A function of the schema alone — deliberately answerable before the item
/// is loaded, because the already-embedded probe in `execute` matches on it
/// per candidate row, ahead of any payload fetch.
fn source_type_for(schema: &str) -> &'static str {
    if schema == CONTENT_CHUNK_SCHEMA.as_str() {
        SOURCE_TYPE_CHUNK
    } else {
        SOURCE_TYPE_MEMORY
    }
}

/// The text to embed for one item, with the sidecar `source_type` it belongs
/// under. `None` when there is nothing to embed.
fn embeddable_text(item: &Item, schema: &str) -> Option<(String, &'static str)> {
    let source_type = source_type_for(schema);
    if source_type == SOURCE_TYPE_CHUNK {
        // `body` is the FTS-indexed copy the source service writes
        // (`indexed_text`); `data.text` is the structured record. Either alone
        // would miss rows written by the other convention.
        let text = payload_string(item, "body")
            .filter(|t| !t.trim().is_empty())
            .or_else(|| chunk_data_text(item))?;
        return Some((text, source_type));
    }
    let title = payload_string(item, "title").unwrap_or_default();
    let body = payload_string(item, "body").unwrap_or_default();
    let text = format!("{title}\n{body}");
    if text.trim().is_empty() {
        return None;
    }
    Some((text, source_type))
}

/// `payload.data.text` of a content chunk.
fn chunk_data_text(item: &Item) -> Option<String> {
    match item.payload.get("data") {
        Some(Value::Object(fields)) => match fields.get("text") {
            Some(Value::String(text)) if !text.trim().is_empty() => Some(text.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Reproducibility stamp for one window. Not a model prompt — there is no
/// prompt — but `agent-run@1.0.0` reserves the field for "what input produced
/// this run", and the window bounds are exactly that.
fn prompt_hash(cursor_created_ms: i64, cursor_id: &str, batch_limit: i64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("embed:{cursor_created_ms}:{cursor_id}:{batch_limit}").as_bytes());
    format!("{:x}", hasher.finalize())
}

fn payload_string(item: &Item, field: &str) -> Option<String> {
    match item.payload.get(field) {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn payload_i64(item: &Item, field: &str) -> Option<i64> {
    match item.payload.get(field) {
        Some(Value::Int(i)) => Some(*i),
        Some(Value::Float(f)) => Some(*f as i64),
        _ => None,
    }
}

/// Payload keys a spawner writes onto an `impress.memory.embed` task.
pub(crate) fn embed_task_payload(
    cursor_created_ms: i64,
    cursor_id: &str,
    batch_limit: i64,
    model: &str,
    sidecar_path: Option<&str>,
) -> BTreeMap<String, Value> {
    let mut payload = BTreeMap::new();
    payload.insert(
        "title".into(),
        Value::String("Backfill memory embeddings".into()),
    );
    payload.insert("task_kind".into(), Value::String(KIND_EMBED.into()));
    payload.insert(
        "state".into(),
        Value::String(koine_tasks::TaskState::Pending.as_str().into()),
    );
    payload.insert("source_app".into(), Value::String("impel-memory".into()));
    payload.insert("cursor_created_ms".into(), Value::Int(cursor_created_ms));
    payload.insert("cursor_id".into(), Value::String(cursor_id.to_string()));
    payload.insert("batch_limit".into(), Value::Int(batch_limit));
    payload.insert("model".into(), Value::String(model.to_string()));
    if let Some(path) = sidecar_path {
        payload.insert("sidecar_path".into(), Value::String(path.to_string()));
    }
    payload
}
