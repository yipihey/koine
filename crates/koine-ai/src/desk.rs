//! The desk agent: a model answers what a person asks on the agent desk
//! (docs/plan-koine-research-os.md G-D, docs/plan-koine-ai-tasks.md AI-D5).
//!
//! A person files a request from any app (⌃⌥⌘A, the web page's ask tab, a
//! surface's button): a `koine/agent-request@1.0.0` item in the store, made
//! by `agent-service_ask`, `_diagnose` or `_make-surface`. This bridge is the
//! agent that picks it up, with the host's model and tools:
//!
//! 1. take each open request (`agent-service_next`) and claim it
//!    (`agent-service_claim`) as `agent:<agent>` ([`DEFAULT_AGENT`] unless
//!    the host names another; impress names `impress-ai`);
//! 2. open a conversation for it, tagged `agent-desk:<request id>` in its
//!    summary, with the host's tool policy (the linked inventory,
//!    `koine-verbs`, unless the host names another; impress names
//!    `impress-mcp`), and queue the request (its prompt, kind and context) as
//!    the user turn; the host's `AiTaskExecutor` runs it like any other turn,
//!    tool rounds included;
//! 3. when the turn has an answer, answer the request with it
//!    (`agent-service_answer`, naming a surface the answer names as
//!    `surface:<uuid>`), or decline it with the run's error.
//!
//! A bridge built over the registry ([`DeskBridge::with_registry`]) routes
//! each request on its own (docs/plan-koine-local-models.md LM-D11): the
//! request's `category` picks the model through the registry's assignments;
//! a request with none is triaged by the `agent.route` model when one is
//! assigned. A bridge built with [`DeskBridge::new`] has one fixed
//! provider and model and does neither.
//!
//! Every step is a verb call or an `AiStore` write, so the call log and the
//! desk show what happened. The tag makes a restart pick up where it left
//! off: a claimed request whose conversation exists is only waited on.

use std::sync::Arc;

use impress_item::item::{Item, ItemId, Value as CoreValue};
use impress_service_core::call;
use impress_service_core::pipeline::CallerIdentity;
use serde_json::{json, Value};

use crate::agent_loop::{self, NoObserver};
use crate::categories;
use crate::registry::{AiRegistry, ResolveTarget};

use crate::{AiStore, ConversationDraft, MessageDraft, ToolPolicy};

/// Who the bridge is on the desk and in the call log, unless the host
/// names another.
pub const DEFAULT_AGENT: &str = "koine-ai";
/// The conversation summary that ties a conversation to its request.
pub const TAG: &str = "agent-desk:";
/// The system prompt a desk conversation starts with, unless the host names
/// another.
pub const DEFAULT_SYSTEM_PROMPT: &str = "You are the agent on this app's agent desk. Be precise, \
     use the app's tools rather than guessing, and say what you did.";
/// Context JSON longer than this is cut, so one request cannot fill the
/// model's window on its own.
pub const MAX_CONTEXT_CHARS: usize = 24_000;

/// What one pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Pass {
    pub started: usize,
    pub answered: usize,
    pub declined: usize,
    /// Given back to the desk after a failure that may pass.
    pub released: usize,
}

/// How many times a request is given back after a failure that may pass
/// before the bridge declines it instead. A request that keeps failing is
/// not this agent's to keep retrying for ever.
pub const MAX_RELEASES: u64 = 3;

/// The category a triaged request falls back to when nothing else places it.
pub const DEFAULT_CATEGORY: &str = "agent";
/// The category whose model triages a request that names none.
pub const ROUTE_CATEGORY: &str = "agent.route";
/// How long triage may take before the request goes to the default model.
pub const TRIAGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Where a request's model comes from.
enum Target {
    /// One provider and model for every request.
    Fixed { provider: String, model: String },
    /// Each request resolved on its own against the device registry.
    Registry(Arc<AiRegistry>),
}

pub struct DeskBridge {
    ai: Arc<AiStore>,
    target: Target,
    agent: String,
    tools: Vec<String>,
    system_prompt: String,
}

async fn verb(caller: &CallerIdentity, name: &str, args: Value) -> Result<Value, String> {
    let answer = call::call_async_as(name, caller.clone(), args)
        .await
        .map_err(|e| format!("{name}: {e}"))?;
    if answer["ok"] == json!(false) {
        return Err(format!(
            "{name}: {} ({})",
            answer["message"].as_str().unwrap_or("refused"),
            answer["code"].as_str().unwrap_or("?")
        ));
    }
    Ok(answer)
}

fn text(item: &Item, field: &str) -> Option<String> {
    match item.payload.get(field) {
        Some(CoreValue::String(s)) => Some(s.clone()),
        _ => None,
    }
}

/// The user turn for a request: what the person asked, what they were
/// looking at, and how to answer.
pub fn turn_for(request: &Value) -> String {
    let kind = request["kind"].as_str().unwrap_or("ask");
    let prompt = request["prompt"].as_str().unwrap_or("");
    let mut context = serde_json::to_string_pretty(&request["context"]).unwrap_or_default();
    if context.chars().count() > MAX_CONTEXT_CHARS {
        context = context.chars().take(MAX_CONTEXT_CHARS).collect();
        context.push_str("\n… (context cut)");
    }
    let how = match kind {
        "make-surface" => {
            "Build the surface the person describes: write the spec, check it with \
             impress-surface-service_surface-validate, store it with \
             impress-surface-service_surface-create, then answer with one sentence saying \
             what it shows and the line `surface:<id>` with the id you created."
        }
        "diagnose" => {
            "Explain what happened in this call and, if it failed, what would fix it. \
             The record, its steps and the verb's schema are in the context."
        }
        _ => {
            "Answer the person. Use the app's tools to look at the records they point at; \
             if you make a surface for the answer, add the line `surface:<id>`."
        }
    };
    format!(
        "A person asked on the agent desk ({kind} request).\n\n{prompt}\n\n{how}\n\n\
         What they were pointing at (JSON):\n```json\n{context}\n```"
    )
}

/// The surface an answer names as `surface:<uuid>`, if any.
pub fn surface_in(answer: &str) -> Option<String> {
    answer.split_whitespace().find_map(|word| {
        let id = word
            .strip_prefix("surface:")?
            .trim_matches(|c: char| !c.is_ascii_hexdigit() && c != '-');
        id.parse::<uuid::Uuid>().ok().map(|u| u.to_string())
    })
}

impl DeskBridge {
    /// The bridge over `ai`, asking `model` at `provider`, as
    /// `agent:koine-ai` with the linked inventory as its tools. The model is
    /// fixed: no request is routed.
    pub fn new(ai: Arc<AiStore>, provider: String, model: String) -> Self {
        Self::over(ai, Target::Fixed { provider, model })
    }

    /// The bridge over `ai` that resolves each request's model from
    /// `registry` (LM-D11): the request's category, else the `agent.route`
    /// triage, else the device default.
    pub fn with_registry(ai: Arc<AiStore>, registry: Arc<AiRegistry>) -> Self {
        Self::over(ai, Target::Registry(registry))
    }

    fn over(ai: Arc<AiStore>, target: Target) -> Self {
        Self {
            ai,
            target,
            agent: DEFAULT_AGENT.into(),
            tools: vec![crate::inventory_tools::POLICY_ID.into()],
            system_prompt: DEFAULT_SYSTEM_PROMPT.into(),
        }
    }

    /// Claim and answer as `agent:<agent>`.
    pub fn as_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = agent.into();
        self
    }

    /// The tool-policy ids a desk conversation enables.
    pub fn with_tools<I, S>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tools = tools.into_iter().map(Into::into).collect();
        self
    }

    /// The system prompt a desk conversation starts with.
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    /// Who the bridge is on the desk: `agent:<agent>`.
    pub fn caller(&self) -> CallerIdentity {
        CallerIdentity::agent(self.agent.clone())
    }

    /// The conversation tagged for `request`, if one was opened.
    pub fn conversation_for(&self, request: &str) -> Result<Option<Item>, String> {
        let tag = format!("{TAG}{request}");
        Ok(self
            .ai
            .list_conversations(true)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|c| text(c, "summary").as_deref() == Some(tag.as_str())))
    }

    /// Open a conversation for `request` and queue its turn. Safe to call
    /// again for the same request: a conversation already tagged for it is
    /// reused, and its turn queued only if it has none, so a retry after a
    /// failure between the two writes never opens a second conversation.
    async fn start(&self, request: &Value) -> crate::Result<ItemId> {
        let id = request["id"].as_str().unwrap_or_default();
        let tag = format!("{TAG}{id}");
        let existing = self
            .ai
            .list_conversations(true)?
            .into_iter()
            .find(|c| text(c, "summary").as_deref() == Some(tag.as_str()));
        let conversation = match existing {
            Some(conversation) => {
                if !self.ai.snapshot(conversation.id)?.messages.is_empty() {
                    return Ok(conversation.id);
                }
                conversation.id
            }
            None => {
                let (provider, model) = self.model_for(request).await?;
                let prompt = request["prompt"].as_str().unwrap_or("Agent desk request");
                let title: String = prompt.chars().take(60).collect();
                self.ai.create_conversation(ConversationDraft {
                    title: if title.trim().is_empty() {
                        "Agent desk request".into()
                    } else {
                        title
                    },
                    summary: Some(tag),
                    system_prompt: Some(self.system_prompt.clone()),
                    provider,
                    model,
                    tool_policy: ToolPolicy {
                        enabled: self.tools.clone(),
                    },
                    ..ConversationDraft::default()
                })?
            }
        };
        let mut turn = MessageDraft::user(turn_for(request));
        turn.from = format!("agent-desk:{id}");
        // A request that asked for a structured answer rides on the turn.
        // Triage never sees it: it has its own schema.
        let format =
            request["response_schema"]
                .is_object()
                .then(|| crate::ResponseFormat::JsonSchema {
                    name: "answer".into(),
                    schema: request["response_schema"].clone(),
                    strict: true,
                });
        self.ai
            .queue_user_turn_with_format(conversation, turn, format)?;
        Ok(conversation)
    }

    /// The provider and model that answer `request`. A fixed bridge has
    /// one. Over a registry: the request's category (refused when unknown),
    /// else the triage's choice, else no category; the registry resolves it
    /// (assignment, then the device selection, then the first ready).
    async fn model_for(&self, request: &Value) -> crate::Result<(String, String)> {
        let registry = match &self.target {
            Target::Fixed { provider, model } => return Ok((provider.clone(), model.clone())),
            Target::Registry(registry) => registry,
        };
        let mut category = request["category"]
            .as_str()
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_string);
        match &category {
            Some(id) => self.check_category(id)?,
            None => {
                if let Some(routed) = self.triage(registry, request).await {
                    self.record_triage(request, &routed.category, &routed.model);
                    category = Some(routed.category);
                }
            }
        }
        let target = ResolveTarget {
            category,
            ..ResolveTarget::default()
        };
        // Network-free when the preferences name the model; discovery only
        // when they leave it open.
        let resolved = match registry.resolve(&target) {
            Ok(resolved) => resolved,
            Err(_) => registry.resolve_with_discovery(&target).await?,
        };
        Ok((resolved.provider, resolved.model))
    }

    /// A category the desk knows: a built-in or an installed one.
    fn check_category(&self, id: &str) -> crate::Result<()> {
        let known = categories::category(id).is_some()
            || categories::installed_in(self.ai.shared_store().as_ref())
                .iter()
                .any(|category| category.id == id);
        if known {
            Ok(())
        } else {
            Err(crate::Error::Invalid(format!(
                "unknown task category '{id}'; ask without a category, or with a category \
                 impress-ai-service_ai-preferences knows"
            )))
        }
    }

    /// The ids triage may answer: every category with an enabled primary
    /// assignment except `agent.route` itself, plus `agent`.
    fn routable(registry: &AiRegistry) -> Vec<String> {
        let mut ids: Vec<String> = registry
            .task_categories()
            .into_iter()
            .filter(|(id, assignment)| {
                id != ROUTE_CATEGORY && assignment.enabled && assignment.primary.is_some()
            })
            .map(|(id, _)| id)
            .collect();
        if !ids.iter().any(|id| id == DEFAULT_CATEGORY) {
            ids.push(DEFAULT_CATEGORY.into());
        }
        ids.sort();
        ids
    }

    /// One structured call on the `agent.route` model: which category fits
    /// the request. `None` when nothing is assigned to `agent.route`, and
    /// when the call fails or answers outside the enum (logged): the
    /// request then goes to the default model.
    async fn triage(&self, registry: &AiRegistry, request: &Value) -> Option<Routed> {
        let assigned = registry.task_categories();
        let route = assigned.get(ROUTE_CATEGORY)?;
        if !route.enabled || route.primary.is_none() {
            return None;
        }
        let id = request["id"].as_str().unwrap_or_default();
        let ids = Self::routable(registry);
        let attempt = async {
            let resolved = registry.resolve(&ResolveTarget::category(ROUTE_CATEGORY))?;
            let provider = registry.provider(&resolved.provider)?;
            let chat = categories::triage_request(
                &resolved.model,
                request["prompt"].as_str().unwrap_or(""),
                &ids,
            );
            let outcome = agent_loop::run(provider.as_ref(), chat, None, &NoObserver, 1)
                .await
                .map_err(|e| crate::Error::Invalid(format!("{e:?}")))?;
            let category = categories::parse_triage(&outcome.content, &ids).ok_or_else(|| {
                crate::Error::Invalid(format!(
                    "answered {:?}, which is none of {ids:?}",
                    outcome.content.chars().take(120).collect::<String>()
                ))
            })?;
            Ok::<_, crate::Error>(Routed {
                category,
                model: format!("{}/{}", resolved.provider, resolved.model),
            })
        };
        match tokio::time::timeout(TRIAGE_TIMEOUT, attempt).await {
            Ok(Ok(routed)) => {
                tracing::info!(target: "koine-ai",
                    "desk: request {id} routed to {} by {}", routed.category, routed.model);
                Some(routed)
            }
            Ok(Err(error)) => {
                tracing::warn!(target: "koine-ai",
                    "desk: triage of request {id} failed, using the default model: {error}");
                None
            }
            Err(_) => {
                tracing::warn!(target: "koine-ai",
                    "desk: triage of request {id} timed out, using the default model");
                None
            }
        }
    }

    /// Put the triage on the request's row, where `agent-service_requests`
    /// shows it. A failure is logged: the request is answered either way.
    fn record_triage(&self, request: &Value, category: &str, model: &str) {
        use impress_item::{FieldMutation, ItemStore};
        let Some(id) = request["id"]
            .as_str()
            .and_then(|id| id.parse::<ItemId>().ok())
        else {
            return;
        };
        let set = |field: &str, value: &str| {
            FieldMutation::SetPayload(field.into(), CoreValue::String(value.into()))
        };
        if let Err(error) = ItemStore::update(
            self.ai.shared_store().as_ref(),
            id,
            vec![set("routed_category", category), set("triage_model", model)],
        ) {
            tracing::warn!(target: "koine-ai",
                "desk: could not record the triage on request {id}: {error}");
        }
    }

    /// After `start` failed for `request`: give it back to the desk when the
    /// failure may pass and it has not come back too often
    /// ([`MAX_RELEASES`]), else decline it with the error. Answers whether it
    /// was released.
    async fn after_failed_start(
        &self,
        request: &Value,
        error: &crate::Error,
        done: &mut Pass,
    ) -> Result<bool, String> {
        let caller = self.caller();
        let id = request["id"].as_str().unwrap_or_default();
        let releases = request["releases"].as_u64().unwrap_or(0);
        if error.is_retryable() && releases < MAX_RELEASES {
            verb(
                &caller,
                "agent-service_release",
                json!({ "id": id, "reason": format!("the desk agent could not start yet: {error}") }),
            )
            .await?;
            done.released += 1;
            Ok(true)
        } else {
            verb(
                &caller,
                "agent-service_decline",
                json!({ "id": id, "reason": format!("the desk agent could not start: {error}") }),
            )
            .await?;
            done.declined += 1;
            Ok(false)
        }
    }

    /// Whether `conversation` has no message yet: `start` opened it but did
    /// not get as far as queueing the request's turn.
    fn turnless(&self, conversation: ItemId) -> bool {
        self.ai
            .snapshot(conversation)
            .is_ok_and(|snapshot| snapshot.messages.is_empty())
    }

    /// The answer the conversation has, once its turn finished: `Ok(Some)`
    /// with the assistant's text, `Err` with the run's error, `Ok(None)`
    /// while it is still running.
    fn outcome(&self, conversation: ItemId) -> Result<Option<String>, String> {
        let snapshot = self.ai.snapshot(conversation).map_err(|e| e.to_string())?;
        if !snapshot.pending_tasks.is_empty() {
            return Ok(None);
        }
        let reply = snapshot
            .messages
            .iter()
            .rev()
            .find(|m| text(m, "role").as_deref() == Some("assistant"));
        match reply {
            Some(message) => Ok(text(message, "body")),
            None => {
                // No pending task and no reply: the run failed.
                Err("the model run ended without an answer; see the conversation".into())
            }
        }
    }

    /// One pass: start every open request, and answer every one of ours
    /// whose turn has finished.
    pub async fn pass(&self) -> Result<Pass, String> {
        let caller = self.caller();
        let verb = |name: &'static str, args: Value| {
            let caller = caller.clone();
            async move { verb(&caller, name, args).await }
        };
        let mut done = Pass::default();
        loop {
            let next = verb("agent-service_next", json!({ "timeout_ms": 0 })).await?;
            let Some(id) = next["request"]["id"].as_str().map(str::to_string) else {
                break;
            };
            let claimed = verb("agent-service_claim", json!({ "id": id })).await?;
            match self.start(&claimed["request"]).await {
                Ok(_) => done.started += 1,
                Err(error) => {
                    // A request given back is the oldest open one again, so
                    // this pass would take it straight back: stop here and
                    // let the next pass, after the poll interval, try again.
                    if self
                        .after_failed_start(&claimed["request"], &error, &mut done)
                        .await?
                    {
                        break;
                    }
                }
            }
        }
        let ours = verb("agent-service_requests", json!({ "status": "claimed" })).await?;
        let mine = caller.author();
        for request in ours["rows"].as_array().cloned().unwrap_or_default() {
            if request["claimed_by"].as_str() != Some(mine.as_str()) {
                continue;
            }
            let id = request["id"].as_str().unwrap_or_default().to_string();
            let started = match self.conversation_for(&id)? {
                // A conversation with its turn: wait for it below.
                Some(c) if !self.turnless(c.id) => Some(c.id),
                // Claimed before a restart and never started, or started
                // only as far as the conversation: (re)start it now.
                _ => None,
            };
            let Some(conversation) = started else {
                let full = verb("agent-service_claim", json!({ "id": id })).await;
                let request = full
                    .map(|f| f["request"].clone())
                    .unwrap_or(request.clone());
                match self.start(&request).await {
                    Ok(_) => done.started += 1,
                    Err(error) => {
                        self.after_failed_start(&request, &error, &mut done).await?;
                    }
                }
                continue;
            };
            match self.outcome(conversation) {
                Ok(None) => {}
                Ok(Some(answer)) => {
                    let mut args = json!({ "id": id, "text": answer });
                    if let Some(surface) = surface_in(&answer) {
                        args["surface_id"] = json!(surface);
                    }
                    verb("agent-service_answer", args).await?;
                    done.answered += 1;
                }
                Err(error) => {
                    verb(
                        "agent-service_decline",
                        json!({ "id": id, "reason": error }),
                    )
                    .await?;
                    done.declined += 1;
                }
            }
        }
        Ok(done)
    }
}

/// What triage chose: a category id and the `provider/model` that chose it.
struct Routed {
    category: String,
    model: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The desk is the process-wide store, so the tests that use it take
    /// turns.
    static TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn fresh_desk() -> Arc<AiStore> {
        koine_agent::ensure_linked();
        let ai = Arc::new(AiStore::from_store(
            Arc::new(koine_store_memory::MemoryStore::new()),
            "desk-test",
        ));
        koine_store::process::install(ai.shared_store());
        ai
    }

    async fn ask_and_claim(bridge: &DeskBridge, prompt: &str) -> Value {
        let asked = call::call_async("agent-service_ask", json!({ "prompt": prompt }))
            .await
            .unwrap();
        let id = asked["request"]["id"].as_str().unwrap().to_string();
        verb(&bridge.caller(), "agent-service_claim", json!({ "id": id }))
            .await
            .unwrap()["request"]
            .clone()
    }

    /// A start that failed because the store was busy gives the request back
    /// for another pass; one that cannot work, or that has come back
    /// MAX_RELEASES times, is declined. On 2026-10-15 a busy store declined
    /// a person's question outright.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_start_is_released_while_it_may_pass_and_declined_after() {
        let _turn = TURN.lock().await;
        let ai = fresh_desk();
        let bridge = DeskBridge::new(ai, "omlx".into(), "test-model".into()).as_agent("desk-test");
        let busy = crate::Error::Store("update hlc_last_wall_ms: database is locked".into());

        let request = ask_and_claim(&bridge, "Which notes are drafts?").await;
        let id = request["id"].as_str().unwrap().to_string();
        let mut done = Pass::default();
        assert!(bridge
            .after_failed_start(&request, &busy, &mut done)
            .await
            .unwrap());
        assert_eq!(done.released, 1);
        let open = call::call_async("agent-service_requests", json!({"status": "open"}))
            .await
            .unwrap();
        assert_eq!(open["rows"][0]["id"], id, "{open}");
        assert_eq!(open["rows"][0]["releases"], 1);

        // Back for the last time it may be: declined, not released again.
        let mut again = verb(&bridge.caller(), "agent-service_claim", json!({ "id": id }))
            .await
            .unwrap()["request"]
            .clone();
        again["releases"] = json!(MAX_RELEASES);
        let mut done = Pass::default();
        assert!(!bridge
            .after_failed_start(&again, &busy, &mut done)
            .await
            .unwrap());
        assert_eq!(done.declined, 1);

        // A failure that cannot pass is declined the first time.
        let request = ask_and_claim(&bridge, "And this?").await;
        let mut done = Pass::default();
        let invalid = crate::Error::Invalid("no such provider".into());
        assert!(!bridge
            .after_failed_start(&request, &invalid, &mut done)
            .await
            .unwrap());
        let declined = call::call_async("agent-service_requests", json!({"status": "declined"}))
            .await
            .unwrap();
        assert_eq!(declined["rows"].as_array().unwrap().len(), 2, "{declined}");
    }

    /// Starting twice, or after a start that opened the conversation but
    /// failed before queueing the turn, keeps one conversation with one turn.
    #[tokio::test(flavor = "multi_thread")]
    async fn starting_a_request_again_reuses_its_conversation() {
        let _turn = TURN.lock().await;
        let ai = fresh_desk();
        let bridge =
            DeskBridge::new(ai.clone(), "omlx".into(), "test-model".into()).as_agent("desk-test");
        let request = ask_and_claim(&bridge, "Summarise the open paper").await;
        let id = request["id"].as_str().unwrap();

        // What a failure between the two writes leaves: the tagged
        // conversation and no turn.
        let half = ai
            .create_conversation(ConversationDraft {
                title: "Summarise the open paper".into(),
                summary: Some(format!("{TAG}{id}")),
                provider: "omlx".into(),
                model: "test-model".into(),
                ..ConversationDraft::default()
            })
            .unwrap();
        assert!(bridge.turnless(half));

        assert_eq!(bridge.start(&request).await.unwrap(), half);
        assert_eq!(bridge.start(&request).await.unwrap(), half);
        let tagged = ai
            .list_conversations(true)
            .unwrap()
            .into_iter()
            .filter(|c| text(c, "summary").as_deref() == Some(format!("{TAG}{id}").as_str()))
            .count();
        assert_eq!(tagged, 1);
        assert_eq!(ai.snapshot(half).unwrap().messages.len(), 1);
    }

    #[test]
    fn a_turn_carries_the_prompt_the_kind_and_the_context() {
        let turn = turn_for(&json!({
            "kind": "make-surface",
            "prompt": "A table of my notes",
            "context": {"kind": "build", "kinds": ["note"]}
        }));
        assert!(turn.contains("A table of my notes"));
        assert!(turn.contains("impress-surface-service_surface-create"));
        assert!(turn.contains("\"kinds\""));
        let long = turn_for(&json!({"prompt": "p", "context": {"x": "y".repeat(40_000)}}));
        assert!(long.contains("(context cut)"));
    }

    /// The whole bridge over a real `AiStore`: a person asks, a pass starts a
    /// tagged conversation and waits while its turn runs, and once the turn
    /// has a reply the next pass answers the request with it (and the
    /// surface it names). The executor is simulated by completing the task
    /// and appending the reply, which is what it writes.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_request_becomes_a_conversation_and_its_reply_the_answer() {
        use crate::Role;
        let _turn = TURN.lock().await;
        let ai = fresh_desk();

        let asked = call::call_async(
            "agent-service_ask",
            json!({"prompt": "Which notes cite each other?", "app_id": "imbib"}),
        )
        .await
        .unwrap();
        let id = asked["request"]["id"].as_str().unwrap().to_string();

        let bridge = DeskBridge::new(ai.clone(), "omlx".into(), "test-model".into())
            .as_agent("desk-test-agent");
        let first = bridge.pass().await.unwrap();
        assert_eq!(first.started, 1);
        let claimed = call::call_async("agent-service_requests", json!({"status": "claimed"}))
            .await
            .unwrap();
        assert_eq!(claimed["rows"][0]["claimed_by"], "agent:desk-test-agent");

        let conversation = bridge.conversation_for(&id).unwrap().expect("tagged");
        assert!(matches!(
            conversation.payload.get("enabled_tools"),
            Some(CoreValue::Array(tools)) if tools.contains(&CoreValue::String("koine-verbs".into()))
        ));
        let snapshot = ai.snapshot(conversation.id).unwrap();
        assert_eq!(snapshot.pending_tasks.len(), 1);
        let body = text(&snapshot.messages[0], "body").unwrap();
        assert!(body.contains("Which notes cite each other?") && body.contains("imbib"));

        // Still running: the next pass waits.
        assert_eq!(bridge.pass().await.unwrap(), Pass::default());

        let task = snapshot.pending_tasks[0].id;
        impress_item::ItemStore::update(
            ai.shared_store().as_ref(),
            task,
            vec![impress_item::FieldMutation::SetPayload(
                "state".into(),
                impress_item::Value::String("complete".into()),
            )],
        )
        .unwrap();
        let surface = "5a000000-0000-4000-8000-000000000003";
        ai.append_message(
            conversation.id,
            MessageDraft {
                role: Role::Assistant,
                body: format!("Two of them do.\nsurface:{surface}"),
                from: "model".into(),
                attachment_ids: vec![],
                in_response_to: None,
            },
        )
        .unwrap();
        assert_eq!(bridge.pass().await.unwrap().answered, 1);
        let answered = call::call_async("agent-service_requests", json!({"status": "answered"}))
            .await
            .unwrap();
        assert_eq!(answered["rows"][0]["id"], id);
        assert_eq!(answered["rows"][0]["surface_id"], surface);
    }

    #[test]
    fn an_answer_names_its_surface_as_surface_colon_id() {
        let id = "5a000000-0000-4000-8000-000000000003";
        assert_eq!(
            surface_in(&format!("It shows your notes.\nsurface:{id}.")).as_deref(),
            Some(id)
        );
        assert_eq!(surface_in("no surface here"), None);
        assert_eq!(surface_in("surface:not-a-uuid"), None);
    }

    // Routing (LM-D11).

    use crate::credentials::InMemoryCredentials;
    use crate::preferences::{CategoryAssignment, ModelRef};
    use crate::ResponseFormat;
    use crate::{
        ChatRequest, EventStream, InferenceProvider, ModelSummary, Result as AiResult, StreamEvent,
    };
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// A provider that answers triage from a script and keeps what it was
    /// asked.
    struct Triage {
        answers: Mutex<VecDeque<Result<String, String>>>,
        asked: Mutex<Vec<ChatRequest>>,
    }

    impl Triage {
        fn new(answers: Vec<Result<&str, &str>>) -> Arc<Self> {
            Arc::new(Self {
                answers: Mutex::new(
                    answers
                        .into_iter()
                        .map(|a| a.map(str::to_string).map_err(str::to_string))
                        .collect(),
                ),
                asked: Mutex::new(vec![]),
            })
        }
    }

    #[async_trait]
    impl InferenceProvider for Triage {
        fn provider_id(&self) -> &str {
            "apple-on-device"
        }
        fn endpoint_id(&self) -> &str {
            "triage-test"
        }
        async fn models(&self) -> AiResult<Vec<ModelSummary>> {
            Ok(vec![])
        }
        async fn stream(&self, request: ChatRequest) -> AiResult<EventStream> {
            self.asked.lock().unwrap().push(request);
            let answer = self
                .answers
                .lock()
                .unwrap()
                .pop_front()
                .expect("triage asked more often than scripted")
                .map_err(crate::Error::Invalid)?;
            let (sender, receiver) = tokio::sync::mpsc::channel(2);
            sender
                .try_send(Ok(StreamEvent::Token { text: answer }))
                .unwrap();
            sender
                .try_send(Ok(StreamEvent::Done {
                    finish_reason: Some("stop".into()),
                }))
                .unwrap();
            Ok(tokio_stream::wrappers::ReceiverStream::new(receiver))
        }
    }

    fn assign(registry: &AiRegistry, category: &str, provider: &str, model: &str) {
        registry
            .set_task_category(
                category,
                CategoryAssignment {
                    primary: Some(ModelRef {
                        provider: provider.into(),
                        model: Some(model.into()),
                        display_name: None,
                    }),
                    ..Default::default()
                },
            )
            .unwrap();
    }

    /// A registry whose `agent.route` is the scripted `triage`, `agent` the
    /// small omlx model, `agent.frontier` the big one, and whose device
    /// selection is `selected-model`.
    fn routed(triage: &Arc<Triage>, with_route: bool) -> (tempfile::TempDir, Arc<AiRegistry>) {
        let directory = tempfile::tempdir().unwrap();
        let registry = AiRegistry::for_app(directory.path(), InMemoryCredentials::new());
        registry.register_native("apple-on-device", triage.clone());
        if with_route {
            assign(&registry, "agent.route", "apple-on-device", "triage");
        }
        assign(&registry, "agent", "omlx", "small");
        assign(&registry, "agent.frontier", "omlx", "big");
        registry
            .select_model("ollama", Some("selected-model".into()))
            .unwrap();
        (directory, registry)
    }

    /// The provider/model a request's conversation was opened with, and its
    /// row on the desk.
    async fn routed_to(
        bridge: &DeskBridge,
        prompt: &str,
        category: Option<&str>,
    ) -> (String, Value) {
        let mut args = json!({ "prompt": prompt, "with_context": false });
        if let Some(category) = category {
            args["category"] = json!(category);
        }
        let asked = call::call_async("agent-service_ask", args).await.unwrap();
        let id = asked["request"]["id"].as_str().unwrap().to_string();
        bridge.pass().await.unwrap();
        let listed = call::call_async("agent-service_requests", json!({}))
            .await
            .unwrap();
        let row = listed["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == json!(id))
            .cloned()
            .unwrap();
        let used = match bridge.conversation_for(&id).unwrap() {
            Some(c) => format!(
                "{}/{}",
                text(&c, "provider").unwrap(),
                text(&c, "model").unwrap()
            ),
            None => "none".into(),
        };
        (used, row)
    }

    /// A request that names its category goes to that category's model, with
    /// no triage; a request that names none is triaged on the `agent.route`
    /// model, which sees a schema whose enum is the assigned categories
    /// (not `agent.route`), and the choice is on the row.
    #[tokio::test(flavor = "multi_thread")]
    async fn each_request_goes_to_the_model_of_its_category() {
        let _turn = TURN.lock().await;
        let ai = fresh_desk();
        let triage = Triage::new(vec![Ok(r#"{"category":"agent.frontier"}"#)]);
        let (_dir, registry) = routed(&triage, true);
        let bridge = DeskBridge::with_registry(ai, registry).as_agent("desk-test");

        let (used, row) = routed_to(&bridge, "Review this proof", Some("agent.frontier")).await;
        assert_eq!(used, "omlx/big");
        assert_eq!(row["category"], "agent.frontier");
        assert!(row.get("routed_category").is_none(), "{row}");
        assert!(
            triage.asked.lock().unwrap().is_empty(),
            "a named category is not triaged"
        );

        let (used, row) = routed_to(&bridge, "Review this proof for errors", None).await;
        assert_eq!(used, "omlx/big");
        assert_eq!(row["routed_category"], "agent.frontier", "{row}");
        assert_eq!(row["triage_model"], "apple-on-device/triage");
        assert!(row.get("category").is_none());
        let asked = triage.asked.lock().unwrap();
        assert_eq!(asked.len(), 1);
        let Some(ResponseFormat::JsonSchema { schema, .. }) = &asked[0].response_format else {
            panic!("triage asks for a schema: {:?}", asked[0].response_format);
        };
        assert_eq!(
            schema["properties"]["category"]["enum"],
            json!(["agent", "agent.frontier"])
        );
        assert_eq!(schema["required"], json!(["category"]));
        assert!(asked[0].tools.is_empty());
        assert!(format!("{:?}", asked[0].messages).contains("Review this proof for errors"));
    }

    /// A triage that fails, or answers outside the enum, leaves the request
    /// on the default model and the row without a routing; with no
    /// `agent.route` assignment there is no triage at all.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_triage_falls_back_to_the_default_model() {
        let _turn = TURN.lock().await;
        let ai = fresh_desk();
        let triage = Triage::new(vec![
            Err("the on-device model is busy"),
            Ok(r#"{"category":"agent.route"}"#),
            Ok("not json"),
        ]);
        let (_dir, registry) = routed(&triage, true);
        let bridge = DeskBridge::with_registry(ai, registry).as_agent("desk-test");
        for _ in 0..3 {
            let (used, row) = routed_to(&bridge, "What is the capital of France?", None).await;
            assert_eq!(used, "ollama/selected-model");
            assert!(row.get("routed_category").is_none(), "{row}");
            assert_eq!(row["status"], "claimed", "{row}");
        }
        assert_eq!(triage.asked.lock().unwrap().len(), 3);

        let untriaged = Triage::new(vec![]);
        let (_dir, registry) = routed(&untriaged, false);
        let bridge = DeskBridge::with_registry(fresh_desk(), registry).as_agent("desk-test");
        let (used, _) = routed_to(&bridge, "Hello", None).await;
        assert_eq!(used, "ollama/selected-model");
        assert!(untriaged.asked.lock().unwrap().is_empty());
    }

    /// An unknown category is declined with a message that says so; a fixed
    /// bridge ignores the category.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_category_is_declined_and_a_fixed_bridge_ignores_it() {
        let _turn = TURN.lock().await;
        let ai = fresh_desk();
        let triage = Triage::new(vec![]);
        let (_dir, registry) = routed(&triage, true);
        let bridge = DeskBridge::with_registry(ai.clone(), registry).as_agent("desk-test");
        let (used, row) = routed_to(&bridge, "x", Some("no.such")).await;
        assert_eq!(used, "none");
        assert_eq!(row["status"], "declined", "{row}");
        assert!(
            row["answer"]
                .as_str()
                .unwrap()
                .contains("unknown task category 'no.such'"),
            "{row}"
        );

        let fixed = DeskBridge::new(ai, "omlx".into(), "fixed-model".into()).as_agent("desk-test");
        let (used, row) = routed_to(&fixed, "y", Some("agent.frontier")).await;
        assert_eq!(used, "omlx/fixed-model");
        assert_eq!(row["category"], "agent.frontier");
    }

    /// A request with a `response_schema`: the conversation's turn carries
    /// it, the model's JSON answer becomes the request's answer and
    /// `answer_json`; a request without one gets no `answer_json`.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_response_schema_rides_the_turn_and_the_json_answer_is_parsed() {
        use koine_tasks::TaskExecutor;
        let _turn = TURN.lock().await;
        let ai = fresh_desk();
        let schema = json!({
            "type": "object",
            "properties": {"city": {"type": "string"}, "temperature": {"type": "integer"}},
            "required": ["city", "temperature"]
        });
        let asked = call::call_async(
            "agent-service_ask",
            json!({"prompt": "Weather in Paris?", "with_context": false, "response_schema": schema}),
        )
        .await
        .unwrap();
        assert_eq!(asked["request"]["response_schema"], schema, "{asked}");
        let id = asked["request"]["id"].as_str().unwrap().to_string();
        let plain = call::call_async(
            "agent-service_ask",
            json!({"prompt": "No schema", "with_context": false}),
        )
        .await
        .unwrap();
        assert!(plain["request"].get("response_schema").is_none());

        let bridge =
            DeskBridge::new(ai.clone(), "omlx".into(), "test-model".into()).as_agent("desk-test");
        assert_eq!(bridge.pass().await.unwrap().started, 2);
        let conversation = bridge.conversation_for(&id).unwrap().expect("tagged");
        let snapshot = ai.snapshot(conversation.id).unwrap();
        let provider = Triage::new(vec![Ok(r#"{"city":"Paris","temperature":18}"#)]);
        let scratch = tempfile::tempdir().unwrap();
        let blobs = Arc::new(crate::FileBlobStore::open(scratch.path().join("blobs")).unwrap());
        let executor = crate::AiTaskExecutor::with_provider(ai.clone(), provider.clone(), blobs);
        let task = ai
            .shared_store()
            .get(snapshot.pending_tasks[0].id)
            .unwrap()
            .unwrap();
        executor
            .execute(&task, &koine_tasks::ItemTaskStore::new(ai.shared_store()))
            .await
            .unwrap();
        // The scheduler marks the task done; here that is by hand.
        impress_item::ItemStore::update(
            ai.shared_store().as_ref(),
            task.id,
            vec![impress_item::FieldMutation::SetPayload(
                "state".into(),
                impress_item::Value::String("complete".into()),
            )],
        )
        .unwrap();
        match &provider.asked.lock().unwrap()[0].response_format {
            Some(ResponseFormat::JsonSchema {
                name,
                schema: sent,
                strict,
            }) => {
                assert_eq!(name, "answer");
                assert_eq!(sent, &schema);
                assert!(strict);
            }
            other => panic!("the turn did not carry the schema: {other:?}"),
        }

        assert_eq!(bridge.pass().await.unwrap().answered, 1);
        let answered = call::call_async("agent-service_requests", json!({"status": "answered"}))
            .await
            .unwrap();
        let row = &answered["rows"][0];
        assert_eq!(row["id"], id.as_str());
        assert_eq!(row["answer"], r#"{"city":"Paris","temperature":18}"#);
        assert_eq!(
            row["answer_json"],
            json!({"city": "Paris", "temperature": 18})
        );
    }
}
