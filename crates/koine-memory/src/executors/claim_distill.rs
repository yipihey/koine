//! The optional claim-distillation tier of `impress.memory.consolidate`
//! (ADR-0028 D7 follow-up, P6).
//!
//! [`crate::executors::consolidate`] is deterministic end to end: one structural
//! `memory/episode@1.0.0` per terminal agent-run, no model involved. This
//! module is the opt-in second pass over the SAME window — one LLM call,
//! built from facts the episode pass already resolved, proposing up to
//! [`MAX_CLAIMS_PER_WINDOW`] durable `memory/claim@1.0.0` rows: things worth
//! remembering across sessions, as opposed to a record of what happened.
//!
//! # Why a trait
//!
//! [`ClaimDistiller`] is the same seam `impel_enrichment::classify_llm` and
//! `impel_throughline::llm_drafter` already use: production code talks to
//! [`LlmDistiller`], tests talk to a stub that never touches the network.
//! `MemoryConsolidationExecutor` (in [`crate::executors::consolidate`]) holds an
//! `Option<Arc<dyn ClaimDistiller>>` — `None` is the default and reproduces
//! v1's deterministic-only behavior exactly; a caller opts in the same way
//! `impel-taskd` already wires the classifier and the throughline drafter,
//! by constructing [`LlmDistiller::from_env`] and handing it to the executor
//! when `Some`.
//!
//! # Degrade, never retry-loop
//!
//! [`ClaimDistiller::distill`] returns `Result<String, String>` rather than
//! panicking or blocking the task: an `Err` means the model host is
//! unreachable, and the caller's job (`MemoryConsolidationExecutor::run_claim_tier`)
//! is to note that in the provenance run's summary and complete with the
//! deterministic results the window already has — never to retry a task
//! whose only failing part was optional to begin with.
//!
//! # Parsing is tolerant by design
//!
//! The registry has no structured-output guarantee across providers (the
//! same limitation `classify_llm` documents), so [`parse_reply`] salvage-parses: strict parse
//! first, then the first `[...]` block in the reply (tolerating surrounding
//! prose), then each array element independently — a title or body missing
//! or mistyped on one proposed claim drops only that claim, never the rest
//! of the response.

use std::sync::Arc;

use async_trait::async_trait;
use koine_ai::blocking::complete_text_sync;
use koine_ai::{AiRegistry, ResolveTarget};

/// Claims proposed per window, before gating. High enough that a rich window
/// still gets its best few ideas across; low enough that one distillation
/// pass cannot flood recall with restatements. Enforced twice: the prompt
/// asks for it, and [`parse_reply`] truncates to it regardless of what the
/// model actually returned.
pub const MAX_CLAIMS_PER_WINDOW: usize = 5;

/// One agent-run's facts as the distillation prompt is allowed to see them —
/// a narrow, store-free view. `MemoryConsolidationExecutor` builds these from
/// the SAME `linked_task` resolution the deterministic episode pass already
/// pays for, so enabling this tier adds no new store scan.
#[derive(Debug, Clone, PartialEq)]
pub struct RunFact {
    /// The `agent-run@1.0.0` item id, lowercase-hyphenated — what
    /// `about_run_ids` in the model's reply must cite to be honored.
    pub run_id: String,
    pub agent_id: Option<String>,
    pub model: Option<String>,
    pub task_kind: Option<String>,
    pub task_title: Option<String>,
    pub result_summary: Option<String>,
    pub token_count: Option<i64>,
}

/// One claim the model proposed: parsed and validated, but not yet gated
/// against existing memory.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedClaim {
    pub title: String,
    pub body: String,
    pub claim_type: Option<String>,
    /// Clamped to `[0, 1]` on parse — never trust a model's arithmetic.
    pub confidence: Option<f64>,
    /// Run ids the model believes this claim is evidenced by, verbatim from
    /// the reply. The caller filters this to runs actually in the window;
    /// see `MemoryConsolidationExecutor::claim_draft`.
    pub about_run_ids: Vec<String>,
}

/// Produces claim proposals for one consolidation window.
///
/// Implementations must be pure with respect to their input (same prompt in
/// ⇒ same reply out) so the provenance run's `prompt_hash` reproducibility
/// story (ADR-0005 §5) holds as meaningfully as the provider allows —
/// [`LlmDistiller`] pins temperature to 0.0 for exactly this reason.
#[async_trait]
pub trait ClaimDistiller: Send + Sync {
    /// Identifier recorded in the provenance run's `model` field — but ONLY
    /// when a call actually completed. See the module docs on degrading
    /// rather than retrying: a transport failure leaves the run's `model`
    /// at the deterministic tier's own stamp.
    fn model_id(&self) -> &str;

    /// Complete `prompt`, returning the raw reply text. `Err` signals a
    /// transport failure (the model host is down, a network error, a
    /// non-2xx, …); it must never be used to mean "the model said no
    /// claims" — that is `Ok("[]".to_string())`.
    async fn distill(&self, prompt: &str) -> Result<String, String>;
}

/// Build the one prompt for a window, from facts the deterministic episode
/// pass already resolved. Nothing here reads a store.
pub fn build_prompt(facts: &[RunFact]) -> String {
    let mut prompt = String::from(
        "You are the memory-distillation step of an autonomous research-agent \
         suite. Below are the agent runs completed in one consolidation \
         window. Propose up to 5 DURABLE claims worth remembering across \
         sessions: facts about the world, the user's preferences, methods \
         that worked, decisions that were made, or results that held up. Do \
         not restate what a run did — that is already recorded separately as \
         an episode. Only propose a claim when the runs reveal something \
         that will still matter days from now.\n\n\
         Respond with ONLY a JSON array, no prose, of the form:\n\
         [{\"title\": \"...\", \"body\": \"...\", \"claim_type\": \
         \"fact|preference|method|decision|result\", \"confidence\": 0.0, \
         \"about_run_ids\": [\"...\"]}]\n\
         Return at most 5 entries. If nothing durable was learned, respond \
         with [].\n\nRuns:\n",
    );
    for fact in facts {
        prompt.push_str("- ");
        prompt.push_str(&fact.run_id);
        if let Some(agent_id) = &fact.agent_id {
            prompt.push_str(&format!(" | agent: {agent_id}"));
        }
        if let Some(model) = &fact.model {
            prompt.push_str(&format!(" | model: {model}"));
        }
        if let Some(task_kind) = &fact.task_kind {
            prompt.push_str(&format!(" | task_kind: {task_kind}"));
        }
        if let Some(task_title) = &fact.task_title {
            prompt.push_str(&format!(" | task: {task_title}"));
        }
        if let Some(tokens) = fact.token_count {
            prompt.push_str(&format!(" | tokens: {tokens}"));
        }
        if let Some(summary) = &fact.result_summary {
            prompt.push_str(&format!(" | result: {summary}"));
        }
        prompt.push('\n');
    }
    prompt
}

/// Salvage-parse the model's reply into claims. Strict parse first, then the
/// first `[...]` block in the reply (tolerating surrounding prose), then
/// each array element independently — a malformed entry is dropped, not
/// fatal to the rest. Capped at [`MAX_CLAIMS_PER_WINDOW`] regardless of what
/// the model returned, so an instruction the model ignored cannot flood a
/// window.
pub fn parse_reply(reply: &str) -> Vec<ParsedClaim> {
    let attempt = |s: &str| -> Option<Vec<serde_json::Value>> { serde_json::from_str(s).ok() };
    let values = attempt(reply.trim())
        .or_else(|| {
            let start = reply.find('[')?;
            let end = reply.rfind(']')?;
            if end <= start {
                return None;
            }
            attempt(&reply[start..=end])
        })
        .unwrap_or_default();

    values
        .iter()
        .filter_map(parse_one)
        .take(MAX_CLAIMS_PER_WINDOW)
        .collect()
}

/// One array element ⇒ one claim, or `None` when it is missing either of the
/// two fields a memory row cannot exist without — mirrors
/// `memory_ops::insert_memory_item`'s own title/body validation, so a claim
/// that would be refused there is dropped here instead of surfacing as a
/// store error mid-window.
fn parse_one(value: &serde_json::Value) -> Option<ParsedClaim> {
    let title = value.get("title")?.as_str()?.trim().to_string();
    let body = value.get("body")?.as_str()?.trim().to_string();
    if title.is_empty() || body.is_empty() {
        return None;
    }
    let claim_type = value
        .get("claim_type")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let confidence = value
        .get("confidence")
        .and_then(|v| v.as_f64())
        .map(|f| f.clamp(0.0, 1.0));
    let about_run_ids = value
        .get("about_run_ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|id| id.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Some(ParsedClaim {
        title,
        body,
        claim_type,
        confidence,
        about_run_ids,
    })
}

/// LLM-backed [`ClaimDistiller`] over the Rust AI registry (ADR-0029).
/// Configuration mirrors `impel_enrichment::classify_llm::LlmClassifier`
/// exactly — the same `AiRegistry::daemon_target` rule over the
/// `agent.memory` task category (or the one-release `IMPEL_LLM_*` override),
/// the same temperature-0.0 pin.
pub struct LlmDistiller {
    registry: Arc<AiRegistry>,
    target: ResolveTarget,
    model_id: String,
}

/// The task category whose primary model runs this tier.
pub const CATEGORY: &str = "agent.memory";

impl LlmDistiller {
    pub fn new(registry: Arc<AiRegistry>, target: ResolveTarget) -> Self {
        let model_id = registry.describe_target(&target);
        Self {
            registry,
            target,
            model_id,
        }
    }

    /// Build from the registry's daemon target for [`CATEGORY`]; `None`
    /// when unconfigured — a caller then leaves the tier disabled, exactly
    /// like the classifier and the throughline drafter fall back to their
    /// deterministic tiers.
    pub fn from_registry(registry: Arc<AiRegistry>) -> Option<Self> {
        let target = registry.daemon_target(CATEGORY)?;
        Some(Self::new(registry, target))
    }
}

#[async_trait]
impl ClaimDistiller for LlmDistiller {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    async fn distill(&self, prompt: &str) -> Result<String, String> {
        let registry = Arc::clone(&self.registry);
        let target = self.target.clone();
        let prompt = prompt.to_string();
        // The registry call blocks on its own runtime; hop off the async
        // worker — same arrangement as classify_llm and llm_drafter.
        tokio::task::spawn_blocking(move || {
            complete_text_sync(&registry, &target, None, &prompt, 1024, Some(0.0))
        })
        .await
        .map_err(|e| format!("distillation task panicked: {e}"))?
        .map(|(_, text)| text)
        .map_err(|e| format!("{e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(run_id: &str) -> RunFact {
        RunFact {
            run_id: run_id.to_string(),
            agent_id: None,
            model: None,
            task_kind: None,
            task_title: None,
            result_summary: None,
            token_count: None,
        }
    }

    #[test]
    fn prompt_includes_every_run_id_and_the_five_cap_instruction() {
        let facts = vec![fact("run-a"), fact("run-b")];
        let prompt = build_prompt(&facts);
        assert!(prompt.contains("run-a"));
        assert!(prompt.contains("run-b"));
        assert!(prompt.contains("at most 5"));
        assert!(
            prompt.contains("[]"),
            "must instruct the empty-array escape hatch"
        );
    }

    #[test]
    fn prompt_carries_the_resolved_task_title_not_a_second_lookup() {
        let facts = vec![RunFact {
            run_id: "r1".into(),
            agent_id: Some("impel/keyword-tag".into()),
            model: Some("heuristic-v1".into()),
            task_kind: Some("keyword-tag".into()),
            task_title: Some("Tag new arrivals".into()),
            result_summary: Some("3 tag proposal(s)".into()),
            token_count: Some(42),
        }];
        let prompt = build_prompt(&facts);
        assert!(prompt.contains("impel/keyword-tag"));
        assert!(prompt.contains("Tag new arrivals"));
        assert!(prompt.contains("3 tag proposal(s)"));
        assert!(prompt.contains("tokens: 42"));
    }

    #[test]
    fn parses_clean_json() {
        let out = parse_reply(
            r#"[{"title":"Flux units","body":"The 2018 catalogue's flux column is in mJy.","claim_type":"fact","confidence":0.9,"about_run_ids":["r1"]}]"#,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Flux units");
        assert_eq!(out[0].claim_type.as_deref(), Some("fact"));
        assert_eq!(out[0].confidence, Some(0.9));
        assert_eq!(out[0].about_run_ids, vec!["r1".to_string()]);
    }

    #[test]
    fn salvages_json_from_surrounding_prose_and_drops_one_bad_entry() {
        let reply = "Sure! Here are the claims I found:\n\
             [{\"title\":\"A\",\"body\":\"Uses Rust for the core.\",\"claim_type\":\"fact\",\"confidence\":0.8,\"about_run_ids\":[]},\
             {\"nope\":true},\
             {\"title\":\"B\",\"body\":\"Prefers Typst over LaTeX.\",\"claim_type\":\"preference\",\"confidence\":0.7,\"about_run_ids\":[]}]\n\
             Hope that helps!";
        let out = parse_reply(reply);
        assert_eq!(
            out.len(),
            2,
            "the malformed middle entry must be dropped, not fatal: {out:?}"
        );
        assert_eq!(out[0].title, "A");
        assert_eq!(out[1].title, "B");
    }

    #[test]
    fn empty_array_is_zero_claims() {
        assert!(parse_reply("[]").is_empty());
        assert!(parse_reply("  []  ").is_empty());
        assert!(parse_reply("No durable claims this window. []").is_empty());
    }

    #[test]
    fn garbage_is_empty() {
        assert!(parse_reply("not json at all").is_empty());
        assert!(parse_reply("[unterminated").is_empty());
        assert!(parse_reply("").is_empty());
    }

    #[test]
    fn confidence_is_clamped_and_optional_fields_default() {
        let out = parse_reply(
            r#"[{"title":"T","body":"B","confidence":5.0},{"title":"T2","body":"B2","confidence":-1.0}]"#,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].confidence, Some(1.0));
        assert_eq!(out[1].confidence, Some(0.0));
        assert_eq!(out[0].claim_type, None);
        assert!(out[0].about_run_ids.is_empty());
    }

    #[test]
    fn missing_title_or_body_drops_the_entry() {
        let out = parse_reply(
            r#"[{"title":"","body":"has body"},{"title":"has title"},{"body":"no title at all"}]"#,
        );
        assert!(
            out.is_empty(),
            "empty title, missing body, and missing title must all drop: {out:?}"
        );
    }

    #[test]
    fn caps_at_max_claims_per_window_even_if_the_model_ignores_the_instruction() {
        let items: Vec<String> = (0..8)
            .map(|i| format!(r#"{{"title":"T{i}","body":"B{i}"}}"#))
            .collect();
        let reply = format!("[{}]", items.join(","));
        let out = parse_reply(&reply);
        assert_eq!(out.len(), MAX_CLAIMS_PER_WINDOW);
    }
}
