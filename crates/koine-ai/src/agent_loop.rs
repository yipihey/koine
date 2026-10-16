//! The model <-> tool loop, store-agnostic (docs/plan-koine-local-models.md,
//! LM-D1, LM-D2).
//!
//! [`run`] streams a round, hands any tool calls to a [`ToolRunner`] in
//! order, appends the results and asks again, until the model answers
//! without a tool or the round limit forces a tools-disabled final round.
//! A caller that keeps a record listens through a [`LoopObserver`].

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use futures_util::StreamExt;
use impress_item::item::Value as CoreValue;
use serde_json::{json, Value};

use crate::provider::ToolLoop;
use crate::tools::{PendingToolCall, ToolCallAccumulator};
use crate::types::{
    ChatRequest, ModelContentPart, ModelMessage, ModelToolCall, ResponseFormat, Role, StreamEvent,
    ToolInvocationRecord,
};
use crate::{Error, InferenceProvider, ToolAdapter};

/// One tool call the model made, arguments already validated as a JSON object.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    /// The arguments exactly as the model streamed them.
    pub raw: String,
}

/// What a tool call came to, as the model will see it.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutcome {
    Ok { content: Value },
    Refused { code: String, message: String },
    Failed { message: String },
}

impl ToolOutcome {
    /// The text sent back to the model.
    pub fn model_text(&self) -> String {
        match self {
            Self::Ok { content } => content.to_string(),
            Self::Failed { message } => json!({ "error": message }).to_string(),
            Self::Refused { code, message } => {
                json!({ "ok": false, "error": { "code": code, "message": message } }).to_string()
            }
        }
    }
}

#[async_trait]
pub trait ToolRunner: Send + Sync {
    /// `Err` is an infrastructure failure that aborts the loop (recording
    /// failed); [`ToolOutcome::Failed`] is a tool failure the model is told.
    async fn run(&self, call: ToolCall) -> crate::Result<ToolOutcome>;
}

/// Hooks for a caller that keeps a record. The defaults do nothing.
pub trait LoopObserver: Send + Sync {
    /// After each text token of a round, with that round's text so far; only
    /// when the request carried no tools (the executor's preview rule).
    fn on_text(&self, _round_text: &str) -> crate::Result<()> {
        Ok(())
    }

    /// A finished tool call.
    fn on_tool_invocation(&self, _record: ToolInvocationRecord) -> crate::Result<()> {
        Ok(())
    }
}

/// An observer that ignores everything.
pub struct NoObserver;

impl LoopObserver for NoObserver {}

/// The portable runner over a [`ToolAdapter`]: calls it, times it, reports a
/// [`ToolInvocationRecord`] to the observer and answers the model.
pub struct AdapterToolRunner<'a> {
    pub adapter: &'a dyn ToolAdapter,
    pub observer: &'a dyn LoopObserver,
}

#[async_trait]
impl ToolRunner for AdapterToolRunner<'_> {
    async fn run(&self, call: ToolCall) -> crate::Result<ToolOutcome> {
        let started = Instant::now();
        let result = self.adapter.call(&call.name, call.arguments.clone()).await;
        let duration_ms = Some(started.elapsed().as_millis().try_into().unwrap_or(u64::MAX));
        let (outcome, result, error) = match result {
            Ok(value) => (
                ToolOutcome::Ok {
                    content: value.clone(),
                },
                Some(json_object_to_core(value)),
                None,
            ),
            Err(error) => {
                let message = error.to_string();
                (
                    ToolOutcome::Failed {
                        message: message.clone(),
                    },
                    None,
                    Some(message),
                )
            }
        };
        self.observer.on_tool_invocation(ToolInvocationRecord {
            tool: call.name.clone(),
            provider: self.adapter.provider_id(&call.name),
            arguments: json_object_to_core(call.arguments),
            result,
            result_summary: Some(outcome.model_text().chars().take(500).collect()),
            error,
            duration_ms,
        })?;
        Ok(outcome)
    }
}

#[derive(Debug, Clone, Default)]
pub struct LoopOutcome {
    /// The final round's text.
    pub content: String,
    /// Each round's reasoning, joined with a blank line.
    pub reasoning: String,
    pub usage: Option<Value>,
    pub finish_reason: Option<String>,
    /// The transcript sent to the last round.
    pub messages: Vec<ModelMessage>,
}

#[derive(Debug)]
pub enum LoopError {
    /// `stream()` failed or a stream item was an error.
    Transport(Error),
    /// The provider reported an error event.
    Stream(String),
    /// A tool after the round limit, no runner installed, or no terminal response.
    Exhausted(String),
    /// Tool-call accumulation or argument parsing failed.
    Invalid(Error),
    /// The runner or the observer failed.
    Tool(Error),
}

/// Passes the first `limit` calls through; later ones are refused without
/// reaching the inner runner. Wraps the runner of a provider-run turn.
struct LimitedRunner<'a> {
    inner: Option<&'a dyn ToolRunner>,
    limit: u32,
    used: AtomicU32,
}

#[async_trait]
impl ToolRunner for LimitedRunner<'_> {
    async fn run(&self, call: ToolCall) -> crate::Result<ToolOutcome> {
        let Some(inner) = self.inner else {
            return Ok(ToolOutcome::Refused {
                code: "no_tools".into(),
                message: "No tools are available for this turn.".into(),
            });
        };
        if self.used.fetch_add(1, Ordering::SeqCst) >= self.limit {
            return Ok(ToolOutcome::Refused {
                code: "tool_limit".into(),
                message: "The tool-use limit has been reached. Answer now from the evidence \
                          already collected."
                    .into(),
            });
        }
        inner.run(call).await
    }
}

/// One streamed round: its text and tool calls. Reasoning, usage and the
/// finish reason accumulate into `outcome`.
async fn stream_round(
    provider: &dyn InferenceProvider,
    request: &ChatRequest,
    previews: bool,
    observer: &dyn LoopObserver,
    outcome: &mut LoopOutcome,
) -> std::result::Result<(String, Vec<PendingToolCall>), LoopError> {
    let mut stream = provider
        .stream(request.clone())
        .await
        .map_err(LoopError::Transport)?;
    let mut round_content = String::new();
    let mut round_reasoning = String::new();
    let mut calls = ToolCallAccumulator::default();
    while let Some(event) = stream.next().await {
        match event.map_err(LoopError::Transport)? {
            StreamEvent::Token { text } => {
                round_content.push_str(&text);
                if previews {
                    observer.on_text(&round_content).map_err(LoopError::Tool)?;
                }
            }
            StreamEvent::Reasoning { text } => round_reasoning.push_str(&text),
            StreamEvent::Usage { usage } => outcome.usage = Some(usage),
            StreamEvent::Done { finish_reason } => {
                if finish_reason.is_some() {
                    outcome.finish_reason = finish_reason;
                }
            }
            StreamEvent::ToolCallDelta {
                index,
                id,
                name,
                arguments,
            } => calls.push(index, id, name, arguments),
            StreamEvent::Error { error } => return Err(LoopError::Stream(error)),
        }
    }
    if !round_reasoning.is_empty() {
        if !outcome.reasoning.is_empty() {
            outcome.reasoning.push_str("\n\n");
        }
        outcome.reasoning.push_str(&round_reasoning);
    }
    let pending = calls.finish().map_err(LoopError::Invalid)?;
    Ok((round_content, pending))
}

/// Run the loop. `max_tool_rounds` tool-using rounds are allowed, then one
/// tools-disabled round makes the model answer from what it has. A
/// [`ToolLoop::Provider`] runtime runs its own turn through `run_turn`, with
/// the same limit applied to the runner. When the provider can't combine
/// tools with a response schema, the schema is held back for a final
/// tools-free round (LM-D7).
pub async fn run(
    provider: &dyn InferenceProvider,
    request: ChatRequest,
    tools: Option<&dyn ToolRunner>,
    observer: &dyn LoopObserver,
    max_tool_rounds: u32,
) -> std::result::Result<LoopOutcome, LoopError> {
    let mut request = request;
    if provider.tool_loop() == ToolLoop::Provider {
        let limited = LimitedRunner {
            inner: tools,
            limit: max_tool_rounds,
            used: AtomicU32::new(0),
        };
        return provider.run_turn(request, &limited, observer).await;
    }

    let mut outcome = LoopOutcome::default();
    let previews_allowed = request.tools.is_empty();
    // LM-D7: the schema rides only on rounds that carry no tools.
    let mut held_schema = None;
    if matches!(
        request.response_format,
        Some(ResponseFormat::JsonSchema { .. })
    ) && !request.tools.is_empty()
        && !provider
            .capabilities(&request.model)
            .await
            .structured_with_tools
    {
        held_schema = request.response_format.take();
    }

    for round in 0..=max_tool_rounds {
        if held_schema.is_some() && request.tools.is_empty() {
            request.response_format = held_schema.clone();
        }
        let carried_tools = !request.tools.is_empty();
        let (round_content, pending) =
            stream_round(provider, &request, previews_allowed, observer, &mut outcome).await?;
        if pending.is_empty() {
            if carried_tools && held_schema.is_some() {
                if !round_content.is_empty() {
                    request
                        .messages
                        .push(ModelMessage::text(Role::Assistant, round_content));
                }
                request.messages.push(ModelMessage::text(
                    Role::User,
                    "Give that answer as JSON matching the response schema.",
                ));
                request.tools.clear();
                request.response_format = held_schema.take();
                let (content, pending) =
                    stream_round(provider, &request, previews_allowed, observer, &mut outcome)
                        .await?;
                if !pending.is_empty() {
                    return Err(LoopError::Exhausted(
                        "model requested a tool in the schema round".into(),
                    ));
                }
                outcome.content = content;
            } else {
                outcome.content = round_content;
            }
            outcome.messages = request.messages;
            return Ok(outcome);
        }
        if round == max_tool_rounds {
            return Err(LoopError::Exhausted(format!(
                "model requested a tool after the {max_tool_rounds}-round tool limit"
            )));
        }
        let Some(runner) = tools else {
            return Err(LoopError::Exhausted(
                "model requested a tool, but no tool execution adapter is installed".into(),
            ));
        };

        request.messages.push(ModelMessage {
            role: Role::Assistant,
            content: if round_content.is_empty() {
                vec![]
            } else {
                vec![ModelContentPart::Text {
                    text: round_content,
                }]
            },
            name: None,
            tool_call_id: None,
            tool_calls: pending
                .iter()
                .map(|call| ModelToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                })
                .collect(),
        });

        for call in pending {
            let arguments: Value = serde_json::from_str(&call.arguments).map_err(|error| {
                LoopError::Invalid(Error::Invalid(format!(
                    "{} returned invalid tool arguments: {error}",
                    call.name
                )))
            })?;
            let name = call.name.clone();
            let id = call.id.clone();
            let result = runner
                .run(ToolCall {
                    id: call.id,
                    name: call.name,
                    arguments,
                    raw: call.arguments,
                })
                .await
                .map_err(LoopError::Tool)?;
            request.messages.push(ModelMessage {
                role: Role::Tool,
                content: vec![ModelContentPart::Text {
                    text: result.model_text(),
                }],
                name: Some(name),
                tool_call_id: Some(id),
                tool_calls: vec![],
            });
        }

        if round + 1 == max_tool_rounds {
            request.tools.clear();
            // LM-D7: some llama.cpp chat templates reject a system message
            // after the first; move this to the user role with package 5.
            request.messages.push(ModelMessage::text(
                Role::System,
                "The tool-use limit has been reached. Answer the user now using the evidence \
                 already collected. Do not request more tools. Clearly state any remaining \
                 uncertainty.",
            ));
        }
    }

    Err(LoopError::Exhausted(
        "model did not produce a terminal response".into(),
    ))
}

pub(crate) fn json_object_to_core(value: Value) -> BTreeMap<String, CoreValue> {
    match value {
        Value::Object(values) => values
            .into_iter()
            .map(|(key, value)| (key, json_to_core(value)))
            .collect(),
        other => BTreeMap::from([("value".into(), json_to_core(other))]),
    }
}

pub(crate) fn json_to_core(value: Value) -> CoreValue {
    match value {
        Value::Null => CoreValue::Null,
        Value::Bool(value) => CoreValue::Bool(value),
        Value::Number(value) => value
            .as_i64()
            .map(CoreValue::Int)
            .or_else(|| value.as_f64().map(CoreValue::Float))
            .unwrap_or(CoreValue::Null),
        Value::String(value) => CoreValue::String(value),
        Value::Array(values) => CoreValue::Array(values.into_iter().map(json_to_core).collect()),
        Value::Object(values) => CoreValue::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, json_to_core(value)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use crate::{EventStream, ModelSummary, Result as AiResult, ToolDefinition};
    use tokio::sync::mpsc;
    use tokio_stream::wrappers::ReceiverStream;

    struct ScriptedProvider {
        rounds: Mutex<VecDeque<Vec<StreamEvent>>>,
        requests: Mutex<Vec<ChatRequest>>,
    }

    impl ScriptedProvider {
        fn new(rounds: Vec<Vec<StreamEvent>>) -> Self {
            Self {
                rounds: Mutex::new(rounds.into()),
                requests: Mutex::new(vec![]),
            }
        }
    }

    #[async_trait]
    impl InferenceProvider for ScriptedProvider {
        fn provider_id(&self) -> &str {
            "scripted"
        }

        fn endpoint_id(&self) -> &str {
            "agent-loop-test"
        }

        async fn models(&self) -> AiResult<Vec<ModelSummary>> {
            Ok(vec![])
        }

        async fn stream(&self, request: ChatRequest) -> AiResult<EventStream> {
            self.requests.lock().unwrap().push(request);
            let events = self.rounds.lock().unwrap().pop_front().unwrap();
            let (sender, receiver) = mpsc::channel(events.len().max(1));
            for event in events {
                sender.try_send(Ok(event)).unwrap();
            }
            drop(sender);
            Ok(ReceiverStream::new(receiver))
        }
    }

    fn tool_round(arguments: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ToolCallDelta {
                index: 0,
                id: Some("call-1".into()),
                name: Some("lookup".into()),
                arguments: arguments.into(),
            },
            StreamEvent::Done {
                finish_reason: Some("tool_calls".into()),
            },
        ]
    }

    fn answer_round(text: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::Token { text: text.into() },
            StreamEvent::Done {
                finish_reason: Some("stop".into()),
            },
        ]
    }

    fn request(with_tool: bool) -> ChatRequest {
        let tools = if with_tool {
            vec![ToolDefinition {
                name: "lookup".into(),
                description: "Look something up".into(),
                input_schema: json!({ "type": "object" }),
            }]
        } else {
            vec![]
        };
        serde_json::from_value(json!({
            "model": "m",
            "messages": [],
            "tools": tools,
        }))
        .unwrap()
    }

    fn message_text(message: &ModelMessage) -> &str {
        match &message.content[0] {
            ModelContentPart::Text { text } => text,
            other => panic!("unexpected part {other:?}"),
        }
    }

    struct CapturingRunner {
        calls: Mutex<Vec<ToolCall>>,
    }

    #[async_trait]
    impl ToolRunner for CapturingRunner {
        async fn run(&self, call: ToolCall) -> AiResult<ToolOutcome> {
            self.calls.lock().unwrap().push(call);
            Ok(ToolOutcome::Ok {
                content: json!({ "hits": 2 }),
            })
        }
    }

    #[tokio::test]
    async fn one_tool_round_then_final_answer() {
        let provider = ScriptedProvider::new(vec![
            tool_round(r#"{"query":"stars"}"#),
            answer_round("two hits"),
        ]);
        let runner = CapturingRunner {
            calls: Mutex::new(vec![]),
        };
        let outcome = run(&provider, request(true), Some(&runner), &NoObserver, 4)
            .await
            .unwrap();
        assert_eq!(outcome.content, "two hits");
        assert_eq!(outcome.finish_reason.as_deref(), Some("stop"));

        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, json!({ "query": "stars" }));
        assert_eq!(calls[0].raw, r#"{"query":"stars"}"#);

        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let messages = &requests[1].messages;
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::Assistant);
        assert_eq!(messages[0].tool_calls[0].id, "call-1");
        assert_eq!(messages[1].role, Role::Tool);
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(messages[1].name.as_deref(), Some("lookup"));
        assert_eq!(message_text(&messages[1]), r#"{"hits":2}"#);
        assert_eq!(outcome.messages.len(), 2);
    }

    struct FailingAdapter;

    #[async_trait]
    impl ToolAdapter for FailingAdapter {
        fn catalog(&self) -> BTreeMap<String, Vec<ToolDefinition>> {
            BTreeMap::new()
        }

        fn provider_id(&self, _tool_name: &str) -> String {
            "failing".into()
        }

        async fn call(&self, _tool_name: &str, _arguments: Value) -> AiResult<Value> {
            Err(Error::Invalid("boom".into()))
        }
    }

    #[derive(Default)]
    struct RecordingObserver {
        records: Mutex<Vec<ToolInvocationRecord>>,
    }

    impl LoopObserver for RecordingObserver {
        fn on_tool_invocation(&self, record: ToolInvocationRecord) -> AiResult<()> {
            self.records.lock().unwrap().push(record);
            Ok(())
        }
    }

    #[tokio::test]
    async fn adapter_error_is_fed_back_and_recorded() {
        let provider =
            ScriptedProvider::new(vec![tool_round(r#"{"q":1}"#), answer_round("recovered")]);
        let observer = RecordingObserver::default();
        let runner = AdapterToolRunner {
            adapter: &FailingAdapter,
            observer: &observer,
        };
        let outcome = run(&provider, request(true), Some(&runner), &observer, 4)
            .await
            .unwrap();
        assert_eq!(outcome.content, "recovered");

        let requests = provider.requests.lock().unwrap();
        let tool_message = &requests[1].messages[1];
        let fed_back: Value = serde_json::from_str(message_text(tool_message)).unwrap();
        assert!(fed_back["error"].as_str().unwrap().contains("boom"));

        let records = observer.records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].tool, "lookup");
        assert_eq!(records[0].provider, "failing");
        assert!(records[0].error.as_deref().unwrap().contains("boom"));
        assert!(records[0].result.is_none());
        assert!(records[0].duration_ms.is_some());
    }

    #[tokio::test]
    async fn tool_call_without_runner_is_exhausted() {
        let provider = ScriptedProvider::new(vec![tool_round("{}")]);
        let error = run(&provider, request(true), None, &NoObserver, 4)
            .await
            .unwrap_err();
        match error {
            LoopError::Exhausted(message) => assert_eq!(
                message,
                "model requested a tool, but no tool execution adapter is installed"
            ),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn last_round_has_no_tools_and_a_limit_message() {
        let provider = ScriptedProvider::new(vec![tool_round("{}"), answer_round("done")]);
        let runner = CapturingRunner {
            calls: Mutex::new(vec![]),
        };
        let outcome = run(&provider, request(true), Some(&runner), &NoObserver, 1)
            .await
            .unwrap();
        assert_eq!(outcome.content, "done");
        let requests = provider.requests.lock().unwrap();
        assert!(!requests[0].tools.is_empty());
        assert!(requests[1].tools.is_empty());
        let last = requests[1].messages.last().unwrap();
        assert_eq!(last.role, Role::System);
        assert!(message_text(last).starts_with("The tool-use limit has been reached."));
    }

    #[tokio::test]
    async fn empty_arguments_run_as_an_empty_object() {
        let provider = ScriptedProvider::new(vec![tool_round(""), answer_round("ok")]);
        let runner = CapturingRunner {
            calls: Mutex::new(vec![]),
        };
        run(&provider, request(true), Some(&runner), &NoObserver, 4)
            .await
            .unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].arguments, json!({}));
        assert_eq!(calls[0].raw, "{}");
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests[1].messages[0].tool_calls[0].arguments, "{}");
    }

    #[test]
    fn model_text_renders_each_outcome() {
        assert_eq!(
            ToolOutcome::Ok {
                content: json!({ "a": 1 })
            }
            .model_text(),
            r#"{"a":1}"#
        );
        assert_eq!(
            ToolOutcome::Failed {
                message: "nope".into()
            }
            .model_text(),
            r#"{"error":"nope"}"#
        );
        let refused = ToolOutcome::Refused {
            code: "denied".into(),
            message: "not allowed".into(),
        }
        .model_text();
        let parsed: Value = serde_json::from_str(&refused).unwrap();
        assert_eq!(
            parsed,
            json!({ "ok": false, "error": { "code": "denied", "message": "not allowed" } })
        );
    }
}
