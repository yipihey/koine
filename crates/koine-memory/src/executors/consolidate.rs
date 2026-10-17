//! `impress.memory.consolidate` — terminal agent-runs into durable episodes
//! (ADR-0028 D7), plus an optional LLM claim-distillation tier (P6).
//!
//! The **deterministic tier** is v1 and never changes shape: no model is
//! involved. Each terminal `agent-run@1.0.0` in the window becomes one
//! structural `memory/episode@1.0.0` — what ran, under which model, how it
//! ended — and every draft goes through the D6 dedup gate, which is what
//! makes the executor safe to replay: consolidating an overlapping window
//! *confirms* the existing episodes instead of duplicating them.
//!
//! The **claim-distillation tier** ([`crate::executors::claim_distill`]) is optional and
//! additive: when a [`crate::executors::claim_distill::ClaimDistiller`] is wired in via
//! [`MemoryConsolidationExecutor::with_claim_distiller`], one extra LLM call
//! per window proposes up to five durable `memory/claim@1.0.0` rows from the
//! SAME facts the episode pass already resolved. `None` (the default)
//! reproduces the deterministic-only v1 behavior byte for byte — no prompt is
//! built, no call is made. See [`MemoryConsolidationExecutor::run_claim_tier`].
//!
//! # Which agent-runs count as terminal
//!
//! The two writers of `agent-run@1.0.0` disagree about `payload.status`, and
//! the disagreement is load-bearing:
//!
//! * `koine_ai::AiStore` writes inference runs with an explicit lifecycle:
//!   `"running"` → `"completed"` / `"failed"`.
//! * `koine_tasks::TaskStoreApi::record_agent_run` — the kernel path every
//!   executor in this daemon uses, including both of ours — writes **no status
//!   field at all**. Its rows are terminal by construction: the run item is
//!   created once, after the work, and never updated.
//!
//! So the filter is subtractive: exclude `"running"` and `"failed"`, and treat
//! an ABSENT status as terminal. Requiring `status == "completed"` would look
//! more careful and would silently skip every kernel-written run — which is to
//! say, everything this daemon produces.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

use crate::ops::{
    self as memory_ops, GateOutcome, MemoryDraft, MemoryKind, GATE_CONFIRM_THRESHOLD,
};
use async_trait::async_trait;
use impress_item::EdgeType;
use impress_item::{ActorKind, Item, ItemId, Value};
use impress_item::{AttributedStore, ItemStore};
use impress_item::{ItemQuery, Predicate, SortDescriptor};
use koine_tasks::{
    AgentRunRecord, ExecutionOutcome, TaskError, TaskExecutor, TaskStoreApi, AGENT_RUN_SCHEMA,
};
use sha2::{Digest, Sha256};

use crate::executors::claim_distill::{self, ClaimDistiller};

/// Dispatch key: the `task_kind` payload field the scheduler matches on.
pub const KIND_CONSOLIDATE: &str = "impress.memory.consolidate";

/// `agent_id` stamped on this executor's `agent-run@1.0.0` provenance rows —
/// and the marker that keeps consolidation from consolidating itself. Each
/// pass writes one run; without this filter, the next window would distil an
/// episode about it, whose own run the window after would distil again, forever.
pub const CONSOLIDATE_AGENT_ID: &str = "impel-memory/consolidate";

/// Envelope author on every episode this executor writes.
pub const CONSOLIDATE_AUTHOR: &str = "impel-memory";

/// `model` stamped on the provenance run when the claim tier did not run —
/// no [`ClaimDistiller`] configured, an empty window, or a transport
/// failure. The tier is the model here: naming it makes "was this window
/// distilled by the deterministic tier alone, or did the LLM tier also run?"
/// answerable from the run alone. When the LLM tier DOES complete a call,
/// the run's `model` is overwritten with the distiller's own
/// [`ClaimDistiller::model_id`] instead — see
/// [`MemoryConsolidationExecutor::run_claim_tier`].
pub const DETERMINISTIC_MODEL: &str = "deterministic-v1";

/// Source runs examined per task.
///
/// A cap rather than a page: one task is a bounded unit of work, and a window
/// that overflows it hands the tail to a follow-up window instead of silently
/// dropping it (see [`TRUNCATED_FIELD`]). The window arithmetic in
/// [`crate::executors::plan_memory_tasks`] (24 h) keeps normal operation far below this.
pub const MAX_SOURCE_RUNS: usize = 512;

/// Task payload flag: this window hit [`MAX_SOURCE_RUNS`] and did NOT reach its
/// own `window_end_ms`.
///
/// Without it the chain in [`crate::executors::spawn::plan_memory_tasks`] starts the next
/// window at this task's `window_end_ms`, and every run past the cap is skipped
/// forever — silently, because the task still completed. Read together with
/// [`CONSUMED_THROUGH_FIELD`].
pub const TRUNCATED_FIELD: &str = "window_truncated";

/// Task payload cursor: `modified` of the last run this window actually
/// consumed, in ms. Where a truncated window's follow-up resumes.
///
/// Inclusive on purpose — the follow-up re-scans that millisecond and re-derives
/// the same `deterministic_key` for anything already distilled, so the overlap
/// costs a gate probe rather than a duplicate episode. Skipping it would be the
/// unrecoverable direction.
pub const CONSUMED_THROUGH_FIELD: &str = "consumed_through_ms";

/// Agent-run payload flag written **after** every episode, claim and edge of the
/// window has landed.
///
/// This is the idempotency marker, and its position is the whole point. The run
/// item itself cannot be it: the run must exist before the memory rows, because
/// each row cites it as `agent_run_ref` — so a crash between "run written" and
/// "episodes written" used to leave a marker with nothing behind it, and the
/// scheduler's resume pass would see the run, return `Complete`, and lose the
/// window permanently. A run without this flag means "a previous attempt died
/// mid-write"; `execute` then re-uses that run rather than minting a second one.
pub const RUN_FINALIZED_FIELD: &str = "consolidation_finalized";

/// `source_kind` payload value this v1 understands.
pub const SOURCE_KIND_AGENT_RUNS: &str = "agent-runs";

/// One window of consolidation.
pub struct MemoryConsolidationExecutor {
    store: Arc<dyn AttributedStore>,
    actor: String,
    gate_threshold: f32,
    /// The optional claim-distillation tier (ADR-0028 P6). `None` — the
    /// constructor default, and what every deterministic-only test in this
    /// crate builds — reproduces v1's behavior exactly: no prompt is built,
    /// no call is made, the provenance run's `model` stays
    /// [`DETERMINISTIC_MODEL`].
    claim_distiller: Option<Arc<dyn ClaimDistiller>>,
}

impl MemoryConsolidationExecutor {
    pub fn new(store: Arc<dyn AttributedStore>) -> Self {
        Self {
            store,
            actor: CONSOLIDATE_AGENT_ID.to_string(),
            gate_threshold: GATE_CONFIRM_THRESHOLD,
            claim_distiller: None,
        }
    }

    /// Override the D6 dedup threshold. Tests pin it; production takes the
    /// kernel default, which is where the decision belongs.
    pub fn with_gate_threshold(mut self, threshold: f32) -> Self {
        self.gate_threshold = threshold;
        self
    }

    /// Enable (or explicitly disable) the optional LLM claim-distillation
    /// tier. `None` is the default already set by [`Self::new`]; production
    /// wiring mirrors `impel_enrichment::LlmClassifier::from_env` and
    /// `impel_throughline::LlmDrafter::from_env`: build a
    /// [`claim_distill::LlmDistiller`] from `IMPEL_LLM_*` and pass it here
    /// (as `Some(Arc::new(..))`) when present, matching the "LLM tier when
    /// configured, deterministic tier otherwise" convention every other
    /// executor in this daemon follows.
    pub fn with_claim_distiller(mut self, distiller: Option<Arc<dyn ClaimDistiller>>) -> Self {
        self.claim_distiller = distiller;
        self
    }

    /// The provenance run this task already produced, if any, and whether that
    /// run is [`RUN_FINALIZED_FIELD`]-complete.
    ///
    /// `record_agent_run` writes the edge in BOTH directions (run —ProducedBy→
    /// task, and task —ProducedBy→ run), so either side answers the question.
    /// Both are checked because the two are separate writes: a crash between
    /// them leaves exactly one, and a replay that consulted only the missing
    /// side would redo the whole window.
    fn existing_run(&self, task: &Item) -> Result<Option<(ItemId, bool)>, TaskError> {
        for reference in &task.references {
            if reference.edge_type != EdgeType::ProducedBy {
                continue;
            }
            match ItemStore::get(self.store.as_ref(), reference.target) {
                Ok(Some(item)) if item.schema == AGENT_RUN_SCHEMA => {
                    return Ok(Some((item.id, is_finalized(&item))))
                }
                Ok(_) => {}
                Err(e) => return Err(TaskError::Retryable(format!("load run reference: {e}"))),
            }
        }
        let runs = ItemStore::query(
            self.store.as_ref(),
            &ItemQuery {
                schema: Some(AGENT_RUN_SCHEMA),
                predicates: vec![Predicate::HasReference(EdgeType::ProducedBy, task.id)],
                limit: Some(1),
                include_tags: false,
                include_references: false,
                ..Default::default()
            },
        )
        .map_err(|e| TaskError::Retryable(format!("query existing run: {e}")))?;
        Ok(runs.first().map(|item| (item.id, is_finalized(item))))
    }

    /// Terminal agent-runs modified within `[start, end)`, oldest first.
    fn source_runs(&self, start_ms: i64, end_ms: i64) -> Result<Vec<Item>, TaskError> {
        let runs = ItemStore::query(
            self.store.as_ref(),
            &ItemQuery {
                schema: Some(AGENT_RUN_SCHEMA),
                predicates: vec![
                    Predicate::Gte("modified".into(), Value::Int(start_ms)),
                    Predicate::Lt("modified".into(), Value::Int(end_ms)),
                ],
                sort: vec![SortDescriptor {
                    field: "modified".into(),
                    ascending: true,
                }],
                // +1 so a full page is distinguishable from a window that
                // exactly filled the cap — the summary can then say it
                // truncated rather than guessing.
                limit: Some(MAX_SOURCE_RUNS + 1),
                include_tags: false,
                ..Default::default()
            },
        )
        .map_err(|e| TaskError::Retryable(format!("scan agent runs: {e}")))?;
        // Post-filter in Rust rather than SQL: `status` is absent on the rows
        // that matter most (see the module docs), and "absent OR not in (...)"
        // is clearer here than in a json_extract predicate.
        Ok(runs.into_iter().filter(is_consolidatable).collect())
    }
}

#[async_trait]
impl TaskExecutor for MemoryConsolidationExecutor {
    fn task_kind(&self) -> &str {
        KIND_CONSOLIDATE
    }

    async fn execute(
        &self,
        task: &Item,
        store: &dyn TaskStoreApi,
    ) -> Result<ExecutionOutcome, TaskError> {
        let started = Instant::now();

        // Idempotency FIRST, before any scan or write: the scheduler's resume
        // pass re-executes a task a crash left `running`, and a window already
        // distilled must not be distilled twice. Only a FINALIZED run counts as
        // "already distilled" — see [`RUN_FINALIZED_FIELD`].
        let prior_run = self.existing_run(task)?;
        if matches!(prior_run, Some((_, true))) {
            return Ok(ExecutionOutcome::Complete);
        }

        let source_kind = payload_string(task, "source_kind")
            .unwrap_or_else(|| SOURCE_KIND_AGENT_RUNS.to_string());
        if source_kind != SOURCE_KIND_AGENT_RUNS {
            // A structural absence: this build has no handler for that source.
            // Retrying cannot grow one.
            return Err(TaskError::Permanent(format!(
                "unsupported source_kind {source_kind:?} (v1 consolidates {SOURCE_KIND_AGENT_RUNS:?})"
            )));
        }
        let window_start = payload_i64(task, "window_start_ms")
            .ok_or_else(|| TaskError::Permanent("task has no window_start_ms".into()))?;
        let window_end = payload_i64(task, "window_end_ms")
            .ok_or_else(|| TaskError::Permanent("task has no window_end_ms".into()))?;

        let mut runs = self.source_runs(window_start, window_end)?;
        let truncated = runs.len() > MAX_SOURCE_RUNS;
        runs.truncate(MAX_SOURCE_RUNS);

        // ── Distil, then gate. Neither step writes. ────────────────────────
        // `gate_fts` is a pure read, so every decision can be made before the
        // provenance run exists — which is what lets the run's `result_summary`
        // carry the real insert/confirm counts instead of a plan. `task` is
        // resolved once per run here (not inside `episode_draft`) so the
        // optional claim tier below can reuse it via `run_fact` without a
        // second store read.
        let mut drafts: Vec<(MemoryDraft, GateOutcome)> = Vec::with_capacity(runs.len());
        let mut prompt_facts: Vec<claim_distill::RunFact> = Vec::with_capacity(runs.len());
        for run in &runs {
            // Not `task` — the outer `task: &Item` (the consolidate task
            // itself) stays in scope for `record_agent_run` below; this is
            // the run's OWN parent task, resolved once and reused by both
            // `episode_draft` and `run_fact`.
            let parent_task = self.linked_task(run);
            let draft = self.episode_draft(run, parent_task.as_ref());
            prompt_facts.push(self.run_fact(run, parent_task.as_ref()));
            let outcome = memory_ops::gate_fts(self.store.as_ref(), &draft, self.gate_threshold)
                .map_err(|e| TaskError::Retryable(format!("dedup gate: {e}")))?;
            drafts.push((draft, outcome));
        }
        let to_insert = drafts
            .iter()
            .filter(|(_, outcome)| matches!(outcome, GateOutcome::Insert))
            .count();
        let to_confirm = drafts.len() - to_insert;

        // Where a truncated window actually stopped. `runs` is already capped,
        // so this is the last run consumed — the follow-up window's inclusive
        // start. `None` only when the cap is 0, which it never is.
        let consumed_through_ms = truncated
            .then(|| runs.last().map(|run| run.modified.timestamp_millis()))
            .flatten();

        let mut summary = format!(
            "{} episode(s) from {} run(s) ({to_insert} inserted, {to_confirm} confirmed)",
            drafts.len(),
            runs.len()
        );
        if let Some(through) = consumed_through_ms {
            summary.push_str(&format!(
                "; window truncated at {MAX_SOURCE_RUNS} runs — follow-up window resumes at {through}"
            ));
        }

        // ── Optional claim-distillation tier (ADR-0028 P6) ─────────────────
        // Same "distil, then gate, neither writes" discipline as episodes:
        // the LLM call and its gating both happen before `record_agent_run`,
        // so the provenance run's summary and `model` carry the real outcome,
        // and a failure here has written nothing yet to roll back.
        let claim_tier = self
            .run_claim_tier(&runs, &prompt_facts, window_start, window_end)
            .await?;
        if let Some(suffix) = &claim_tier.summary_suffix {
            summary.push_str(suffix);
        }
        // `model` and `executor_kind` are read together by every provenance
        // surface: a completed claim-tier call means an inference produced part
        // of this window, and `deterministic-v1` means nothing did.
        let executor_kind = match &claim_tier.model {
            Some(_) => koine_tasks::EXECUTOR_MODEL,
            None => koine_tasks::EXECUTOR_DETERMINISTIC,
        };
        let model = claim_tier
            .model
            .clone()
            .unwrap_or_else(|| DETERMINISTIC_MODEL.to_string());

        let duration_ms = started.elapsed().as_millis() as i64;
        let run_id = match prior_run {
            // A previous attempt wrote its provenance run and died before the
            // memory rows landed. Re-use that run instead of minting a second
            // one — a replay must not leave a trail of half-finished runs, and
            // the rows below re-derive the same deterministic ids either way.
            // Its summary/model/duration describe the attempt that failed, so
            // they are overwritten with this pass's.
            Some((run_id, _)) => {
                self.set_payload(store, run_id, "result_summary", Value::String(summary))?;
                self.set_payload(store, run_id, "model", Value::String(model))?;
                self.set_payload(
                    store,
                    run_id,
                    "executor_kind",
                    Value::String(executor_kind.into()),
                )?;
                self.set_payload(store, run_id, "duration_ms", Value::Int(duration_ms))?;
                run_id
            }
            None => store.record_agent_run(
                task.id,
                AgentRunRecord {
                    agent_id: self.actor.clone(),
                    model,
                    prompt_hash: window_hash(window_start, window_end, &runs),
                    result_summary: Some(summary),
                    token_count: None,
                    duration_ms: Some(duration_ms),
                    executor_kind: Some(executor_kind.into()),
                },
            )?,
        };

        for (draft, outcome) in drafts {
            self.apply_gate_outcome(draft, outcome, run_id, "episode")?;
        }
        for (draft, outcome) in claim_tier.drafts {
            self.apply_gate_outcome(draft, outcome, run_id, "claim")?;
        }

        for run in &runs {
            store.add_edge(run_id, run.id, EdgeType::DerivedFrom, &self.actor)?;
        }

        // Hand the tail to a follow-up window before declaring this one done,
        // so a crash here leaves the task un-finalized and the whole pass
        // replayable rather than the tail lost.
        if let Some(through) = consumed_through_ms {
            self.set_payload(store, task.id, CONSUMED_THROUGH_FIELD, Value::Int(through))?;
            self.set_payload(store, task.id, TRUNCATED_FIELD, Value::Bool(true))?;
        }

        // The marker, LAST. Everything this window promised is now in the store.
        self.set_payload(store, run_id, RUN_FINALIZED_FIELD, Value::Bool(true))?;

        Ok(ExecutionOutcome::Complete)
    }

    fn max_retries(&self) -> u32 {
        2
    }

    /// Same reasoning as the embed executor: `TaskError::Store` is almost
    /// always `SQLITE_BUSY` on a file four processes share, and the trait
    /// default would turn a moment of contention into a permanently skipped
    /// window that no later pass revisits.
    fn is_retryable(&self, error: &TaskError) -> bool {
        matches!(error, TaskError::Retryable(_) | TaskError::Store(_))
    }
}

impl MemoryConsolidationExecutor {
    /// One structural episode from one agent-run. No model, no interpretation —
    /// only fields the run actually carries.
    ///
    /// `task` is resolved by the caller (`execute`'s window loop), not here —
    /// so the same graph walk can be reused for the optional claim tier's
    /// prompt facts (`run_fact`) without a second store read per run.
    fn episode_draft(&self, run: &Item, task: Option<&Item>) -> MemoryDraft {
        let agent_id = payload_string(run, "agent_id").filter(|s| !s.trim().is_empty());
        let model = payload_string(run, "model").filter(|s| !s.trim().is_empty());
        let task_kind = task
            .and_then(|t| payload_string(t, "task_kind"))
            .filter(|s| !s.trim().is_empty());
        let task_title = task
            .and_then(|t| payload_string(t, "title"))
            .filter(|s| !s.trim().is_empty());

        let subject = agent_id
            .clone()
            .or_else(|| task_title.clone())
            .unwrap_or_else(|| "unattributed run".to_string());
        let title = match &model {
            Some(model) => format!("Episode: {subject} ({model})"),
            None => format!("Episode: {subject}"),
        };

        let status = payload_string(run, "status");
        let outcome = match status.as_deref() {
            Some("completed") => "completed",
            _ => "terminal",
        };

        let mut lines: Vec<String> = Vec::new();
        if let Some(kind) = &task_kind {
            lines.push(format!("Task kind: {kind}."));
        }
        if let Some(title) = &task_title {
            lines.push(format!("Task: {title}."));
        }
        if let Some(agent) = &agent_id {
            lines.push(format!("Agent: {agent}."));
        }
        if let Some(model) = &model {
            lines.push(format!("Model: {model}."));
        }
        if let Some(summary) =
            payload_string(run, "result_summary").filter(|s| !s.trim().is_empty())
        {
            lines.push(format!("Result: {summary}."));
        }
        if let Some(tokens) = payload_i64(run, "token_count") {
            lines.push(format!("Tokens: {tokens}."));
        }
        if let Some(duration) = payload_i64(run, "duration_ms") {
            lines.push(format!("Duration: {duration} ms."));
        }
        if let Some(finish) = payload_string(run, "finish_reason").filter(|s| !s.trim().is_empty())
        {
            lines.push(format!("Finish reason: {finish}."));
        }
        lines.push(format!("Outcome: {outcome}."));
        // `insert_memory_item` rejects an empty body, and `lines` is never
        // empty (the outcome line is unconditional) — so this cannot produce a
        // draft the kernel will refuse.
        let body = lines.join(" ");

        let mut extra: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        extra.insert(
            "outcome".into(),
            serde_json::Value::String(outcome.to_string()),
        );
        if let Some(kind) = task_kind {
            extra.insert("task_kind".into(), serde_json::Value::String(kind));
        }
        extra.insert(
            "approach".into(),
            serde_json::Value::String(DETERMINISTIC_MODEL.to_string()),
        );

        MemoryDraft {
            kind: MemoryKind::Episode,
            title,
            body,
            claim_type: None,
            confidence: None,
            subject_refs: Vec::new(),
            evidence_refs: vec![run.id.to_string()],
            agent_id,
            // Filled in by `execute` once the provenance run exists.
            agent_run_ref: None,
            author: CONSOLIDATE_AUTHOR.into(),
            author_kind: ActorKind::Agent,
            // The idempotency seam: a replay of this window derives the same
            // UUIDv5 and `insert_memory_item` returns the existing row without
            // touching it. The `v1` suffix is the distillation recipe's version
            // — when the recipe changes, the key changes and the new episode
            // supersedes rather than silently rewriting history.
            deterministic_key: Some(format!("consolidate:{}:{}", run.id, "v1")),
            extra,
        }
    }

    /// The task an agent-run was produced for, best-effort.
    ///
    /// Best-effort by design: `record_agent_run` writes the edge, but a run
    /// mirrored in from another writer may not, and a missing task must cost a
    /// field on the episode rather than the episode itself.
    fn linked_task(&self, run: &Item) -> Option<Item> {
        for reference in &run.references {
            if reference.edge_type != EdgeType::ProducedBy {
                continue;
            }
            if let Ok(Some(item)) = ItemStore::get(self.store.as_ref(), reference.target) {
                if item.schema == koine_tasks::TASK_SCHEMA {
                    return Some(item);
                }
            }
        }
        None
    }

    /// The subset of an agent-run's facts the claim-distillation prompt is
    /// allowed to see. Reuses the SAME `task` resolution the episode draft
    /// for this run already paid for (see the call site in `execute`), so
    /// enabling the claim tier costs no extra store read.
    fn run_fact(&self, run: &Item, task: Option<&Item>) -> claim_distill::RunFact {
        claim_distill::RunFact {
            run_id: run.id.to_string(),
            agent_id: payload_string(run, "agent_id").filter(|s| !s.trim().is_empty()),
            model: payload_string(run, "model").filter(|s| !s.trim().is_empty()),
            task_kind: task
                .and_then(|t| payload_string(t, "task_kind"))
                .filter(|s| !s.trim().is_empty()),
            task_title: task
                .and_then(|t| payload_string(t, "title"))
                .filter(|s| !s.trim().is_empty()),
            result_summary: payload_string(run, "result_summary").filter(|s| !s.trim().is_empty()),
            token_count: payload_i64(run, "token_count"),
        }
    }

    /// The optional second pass over a window: one LLM call proposing durable
    /// claims, gated through the same D6 defence episodes use. Never writes —
    /// same contract as the episode pass in `execute` — and never turns an
    /// unreachable model host into a task retry: the deterministic episode
    /// pass has already succeeded by the time this runs, so a dead LLM host
    /// degrades this window to "no claims this time", not to "redo
    /// everything".
    async fn run_claim_tier(
        &self,
        runs: &[Item],
        facts: &[claim_distill::RunFact],
        window_start: i64,
        window_end: i64,
    ) -> Result<ClaimTierOutcome, TaskError> {
        let Some(distiller) = self.claim_distiller.as_ref() else {
            return Ok(ClaimTierOutcome::default());
        };
        // Nothing to distil, and a call would be a wasted round trip — same
        // reasoning as the embed executor's "an empty window never asks for
        // an embedder".
        if runs.is_empty() {
            return Ok(ClaimTierOutcome::default());
        }

        let prompt = claim_distill::build_prompt(facts);
        let reply = match distiller.distill(&prompt).await {
            Ok(text) => text,
            Err(err) => {
                return Ok(ClaimTierOutcome {
                    drafts: Vec::new(),
                    model: None,
                    summary_suffix: Some(format!("; llm: \"unavailable\" ({err})")),
                });
            }
        };

        // What a claim's `about_run_ids` may cite: only runs this window
        // actually consumed, lowercased to match `memory_ops`'s own ref
        // normalization.
        let window_ids: BTreeSet<String> = runs
            .iter()
            .map(|r| r.id.to_string().to_lowercase())
            .collect();

        let mut drafts: Vec<(MemoryDraft, GateOutcome)> = Vec::new();
        for claim in claim_distill::parse_reply(&reply)
            .into_iter()
            .take(claim_distill::MAX_CLAIMS_PER_WINDOW)
        {
            let draft = self.claim_draft(&claim, &window_ids, window_start, window_end);
            let outcome = memory_ops::gate_fts(self.store.as_ref(), &draft, self.gate_threshold)
                .map_err(|e| TaskError::Retryable(format!("dedup gate (claim): {e}")))?;
            drafts.push((draft, outcome));
        }

        let inserted = drafts
            .iter()
            .filter(|(_, outcome)| matches!(outcome, GateOutcome::Insert))
            .count();
        let confirmed = drafts.len() - inserted;
        let summary_suffix = Some(format!(
            "; {} claim(s) distilled ({inserted} inserted, {confirmed} confirmed) via {}",
            drafts.len(),
            distiller.model_id()
        ));
        Ok(ClaimTierOutcome {
            drafts,
            model: Some(distiller.model_id().to_string()),
            summary_suffix,
        })
    }

    /// One claim draft from one parsed proposal. `window_ids` restricts
    /// `evidence_refs` to runs actually consumed by this window — a model
    /// may cite a run id that does not exist or belongs to a different
    /// window, and that must cost the citation, not the claim.
    fn claim_draft(
        &self,
        claim: &claim_distill::ParsedClaim,
        window_ids: &BTreeSet<String>,
        window_start: i64,
        window_end: i64,
    ) -> MemoryDraft {
        let evidence_refs: Vec<String> = claim
            .about_run_ids
            .iter()
            .map(|id| id.trim().to_lowercase())
            .filter(|id| window_ids.contains(id))
            .collect();

        MemoryDraft {
            kind: MemoryKind::Claim,
            title: claim.title.clone(),
            body: claim.body.clone(),
            claim_type: claim.claim_type.clone(),
            confidence: claim.confidence,
            subject_refs: Vec::new(),
            evidence_refs,
            agent_id: None,
            // Filled in by `execute` once the provenance run exists — same
            // two-step as episodes.
            agent_run_ref: None,
            author: CONSOLIDATE_AUTHOR.into(),
            author_kind: ActorKind::Agent,
            // Window-scoped, NOT run-scoped like an episode's key: the same
            // claim re-derived in a DIFFERENT window is expected to come back
            // with slightly different model wording, so only an EXACT replay
            // of THIS window collapses to the same id here. Catching "the
            // same claim, worded differently, from a different window" is
            // `gate_fts`'s job, not this key's — see
            // `a_duplicate_claim_across_two_windows_confirms_not_duplicates`.
            deterministic_key: Some(format!(
                "consolidate-claim:{window_start}:{window_end}:{}",
                claim_text_hash(&claim.title, &claim.body)
            )),
            extra: BTreeMap::new(),
        }
    }

    /// Write one payload field on an item this executor owns — the truncation
    /// cursor on the task, the finalization marker on the run. See
    /// [`crate::executors::set_payload`] for why an executor may write payload at all.
    fn set_payload(
        &self,
        store: &dyn TaskStoreApi,
        target_id: ItemId,
        field: &str,
        value: Value,
    ) -> Result<(), TaskError> {
        crate::executors::set_payload(store, target_id, field, value, &self.actor)
    }

    /// Write one gated draft (episode or claim): insert on [`GateOutcome::Insert`]
    /// (stamping this consolidation's own provenance run as its producer),
    /// confirm the existing row on [`GateOutcome::Confirm`]. `kind_label` only
    /// shapes the error message, so a store failure is diagnosable without a
    /// debugger.
    fn apply_gate_outcome(
        &self,
        mut draft: MemoryDraft,
        outcome: GateOutcome,
        run_id: ItemId,
        kind_label: &str,
    ) -> Result<(), TaskError> {
        match outcome {
            GateOutcome::Insert => {
                draft.agent_run_ref = Some(run_id.to_string());
                memory_ops::insert_memory_item(self.store.as_ref(), &draft)
                    .map_err(|e| TaskError::Retryable(format!("insert {kind_label}: {e}")))?;
            }
            GateOutcome::Confirm(existing) => {
                memory_ops::confirm(
                    self.store.as_ref(),
                    existing,
                    CONSOLIDATE_AUTHOR,
                    ActorKind::Agent,
                )
                .map_err(|e| TaskError::Retryable(format!("confirm {kind_label}: {e}")))?;
            }
        }
        Ok(())
    }
}

/// Outcome of attempting the optional claim tier for one window.
#[derive(Default)]
struct ClaimTierOutcome {
    /// Claim drafts already gated (Insert/Confirm decided) — same shape the
    /// episode pass produces, ready to write once the provenance run exists.
    drafts: Vec<(MemoryDraft, GateOutcome)>,
    /// The real provider/model string, but ONLY when a call actually
    /// completed. `None` (the default) leaves the provenance run's `model`
    /// at [`DETERMINISTIC_MODEL`] — no distiller configured, an empty
    /// window, and a transport failure all count as "the LLM tier did not
    /// run" for this field.
    model: Option<String>,
    /// Appended verbatim to the provenance run's `result_summary`.
    summary_suffix: Option<String>,
}

/// Stable id material for a claim's `deterministic_key`: sha256 over the
/// title and body, so the id changes if and only if the text does.
fn claim_text_hash(title: &str, body: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(title.trim().as_bytes());
    hasher.update([0u8]);
    hasher.update(body.trim().as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Whether a provenance run carries [`RUN_FINALIZED_FIELD`].
///
/// Absent counts as NOT finalized, which is what makes this safe to add to a
/// store full of runs written before the flag existed: the worst an old run
/// costs is one replayed window, and every write in that replay is idempotent.
fn is_finalized(run: &Item) -> bool {
    matches!(
        run.payload.get(RUN_FINALIZED_FIELD),
        Some(Value::Bool(true))
    )
}

/// Whether a run is terminal, and not one of ours. See the module docs.
fn is_consolidatable(run: &Item) -> bool {
    if matches!(run.payload.get("status"), Some(Value::String(s)) if s == "running" || s == "failed")
    {
        return false;
    }
    !matches!(run.payload.get("agent_id"),
              Some(Value::String(agent)) if agent == CONSOLIDATE_AGENT_ID)
}

/// Reproducibility stamp: the window bounds plus the exact source runs consumed.
/// Two runs of the same window over the same data hash the same; a window that
/// gained a row does not.
fn window_hash(start_ms: i64, end_ms: i64, runs: &[Item]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("consolidate:{start_ms}:{end_ms}").as_bytes());
    for run in runs {
        hasher.update(b":");
        hasher.update(run.id.to_string().as_bytes());
    }
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

/// Payload keys a spawner writes onto an `impress.memory.consolidate` task.
pub(crate) fn consolidate_task_payload(
    window_start_ms: i64,
    window_end_ms: i64,
) -> BTreeMap<String, Value> {
    let mut payload = BTreeMap::new();
    payload.insert(
        "title".into(),
        Value::String("Consolidate agent runs into episodes".into()),
    );
    payload.insert("task_kind".into(), Value::String(KIND_CONSOLIDATE.into()));
    payload.insert(
        "state".into(),
        Value::String(koine_tasks::TaskState::Pending.as_str().into()),
    );
    payload.insert("source_app".into(), Value::String("impel-memory".into()));
    payload.insert("window_start_ms".into(), Value::Int(window_start_ms));
    payload.insert("window_end_ms".into(), Value::Int(window_end_ms));
    payload.insert(
        "source_kind".into(),
        Value::String(SOURCE_KIND_AGENT_RUNS.into()),
    );
    payload
}
