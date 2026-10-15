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
//! Every step is a verb call or an `AiStore` write, so the call log and the
//! desk show what happened. The tag makes a restart pick up where it left
//! off: a claimed request whose conversation exists is only waited on.

use std::sync::Arc;

use impress_item::item::{Item, ItemId, Value as CoreValue};
use impress_service_core::call;
use impress_service_core::pipeline::CallerIdentity;
use serde_json::{json, Value};

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
}

pub struct DeskBridge {
    ai: Arc<AiStore>,
    provider: String,
    model: String,
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
    /// `agent:koine-ai` with the linked inventory as its tools.
    pub fn new(ai: Arc<AiStore>, provider: String, model: String) -> Self {
        Self {
            ai,
            provider,
            model,
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

    /// Open a conversation for `request` and queue its turn.
    fn start(&self, request: &Value) -> Result<ItemId, String> {
        let id = request["id"].as_str().unwrap_or_default();
        let prompt = request["prompt"].as_str().unwrap_or("Agent desk request");
        let title: String = prompt.chars().take(60).collect();
        let conversation = self
            .ai
            .create_conversation(ConversationDraft {
                title: if title.trim().is_empty() {
                    "Agent desk request".into()
                } else {
                    title
                },
                summary: Some(format!("{TAG}{id}")),
                system_prompt: Some(self.system_prompt.clone()),
                provider: self.provider.clone(),
                model: self.model.clone(),
                tool_policy: ToolPolicy {
                    enabled: self.tools.clone(),
                },
                ..ConversationDraft::default()
            })
            .map_err(|e| e.to_string())?;
        let mut turn = MessageDraft::user(turn_for(request));
        turn.from = format!("agent-desk:{id}");
        self.ai
            .queue_user_turn(conversation, turn)
            .map_err(|e| e.to_string())?;
        Ok(conversation)
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
            match self.start(&claimed["request"]) {
                Ok(_) => done.started += 1,
                Err(error) => {
                    let _ = verb(
                        "agent-service_decline",
                        json!({ "id": id, "reason": format!("the desk agent could not start: {error}") }),
                    )
                    .await;
                    done.declined += 1;
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
            let conversation = match self.conversation_for(&id)? {
                Some(c) => c.id,
                // Claimed before a restart and never started: start it now.
                None => {
                    let full = verb("agent-service_claim", json!({ "id": id })).await;
                    let request = full
                        .map(|f| f["request"].clone())
                        .unwrap_or(request.clone());
                    self.start(&request)?;
                    done.started += 1;
                    continue;
                }
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

#[cfg(test)]
mod tests {
    use super::*;

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
        koine_agent::ensure_linked();
        let ai = Arc::new(AiStore::from_store(
            Arc::new(koine_store_memory::MemoryStore::new()),
            "desk-test",
        ));
        koine_store::process::install(ai.shared_store());

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
}
