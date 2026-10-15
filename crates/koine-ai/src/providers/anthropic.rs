//! Anthropic Messages API client.
//!
//! The wire format differs from the OpenAI family in every direction — the
//! system prompt is out of band, tool results travel as user content
//! blocks, and streaming is a typed event sequence — so it gets its own
//! encoder and parser rather than a dialect.

use std::sync::{Arc, Mutex};

use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde_json::{json, Value};

use super::{http_client, status_error, transport_error};
use crate::catalogue::{self, ANTHROPIC_DEFAULT_URL, ANTHROPIC_ID};
use crate::provider::{EventStream, InferenceProvider, ProviderHealth};
use crate::secret::Secret;
use crate::sse::{spawn_sse_stream, SseFrame, SseOutcome};
use crate::types::{
    normalize_usage, ChatRequest, ModelContentPart, ModelKind, ModelMessage, ModelSummary,
    ResponseFormat, Role, StreamEvent, ToolDefinition,
};
use crate::{Error, Result};

pub const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Clone)]
pub struct AnthropicClient {
    client: reqwest::Client,
    origin: String,
    api_key: Secret,
    endpoint_id: String,
}

impl AnthropicClient {
    pub fn new(
        origin: Option<&str>,
        api_key: Secret,
        endpoint_id: impl Into<String>,
    ) -> Result<Self> {
        if api_key.is_empty() {
            return Err(Error::NotConfigured {
                provider: ANTHROPIC_ID.into(),
                message: "no API key configured".into(),
            });
        }
        let origin = origin
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(ANTHROPIC_DEFAULT_URL)
            .trim_end_matches('/');
        let origin = origin
            .strip_suffix("/v1")
            .unwrap_or(origin)
            .trim_end_matches('/')
            .to_string();
        let parsed = url::Url::parse(&origin).map_err(|error| {
            Error::Invalid(format!(
                "anthropic endpoint must be an http(s) URL: {error}"
            ))
        })?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(Error::Invalid(
                "anthropic endpoint must be an http(s) URL".into(),
            ));
        }
        Ok(Self {
            client: http_client()?,
            origin,
            api_key,
            endpoint_id: endpoint_id.into(),
        })
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn endpoint_id(&self) -> &str {
        &self.endpoint_id
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.origin))
            .header("x-api-key", self.api_key.expose())
            .header("anthropic-version", ANTHROPIC_VERSION)
    }

    pub async fn models(&self) -> Result<Vec<ModelSummary>> {
        let response = self
            .request(reqwest::Method::GET, "/v1/models?limit=100")
            .header(ACCEPT, "application/json")
            .send()
            .await
            .map_err(|error| transport_error(ANTHROPIC_ID, error))?;
        if !response.status().is_success() {
            return Err(status_error(ANTHROPIC_ID, response).await);
        }
        let body: Value = response.json().await.map_err(|error| Error::Provider {
            provider: ANTHROPIC_ID.into(),
            status: None,
            message: format!("invalid JSON from /v1/models: {error}"),
        })?;
        Ok(parse_models(&body))
    }

    pub async fn stream(&self, request: ChatRequest) -> Result<EventStream> {
        request.validate()?;
        let payload = build_payload(&request)?;
        let response = self
            .request(reqwest::Method::POST, "/v1/messages")
            .header(ACCEPT, "text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|error| transport_error(ANTHROPIC_ID, error))?;
        if !response.status().is_success() {
            return Err(status_error(ANTHROPIC_ID, response).await);
        }
        let state = Arc::new(Mutex::new(StreamState::default()));
        Ok(spawn_sse_stream(
            response,
            |message| Error::Unreachable {
                provider: ANTHROPIC_ID.into(),
                message,
            },
            move |frame| parse_frame(&state, frame),
        ))
    }
}

#[async_trait::async_trait]
impl InferenceProvider for AnthropicClient {
    fn provider_id(&self) -> &str {
        ANTHROPIC_ID
    }

    fn endpoint_id(&self) -> &str {
        &self.endpoint_id
    }

    async fn models(&self) -> Result<Vec<ModelSummary>> {
        self.models().await
    }

    async fn health(&self) -> Result<ProviderHealth> {
        let mut health = ProviderHealth::from_models(self.models().await?);
        health.endpoint = Some(self.origin.clone());
        Ok(health)
    }

    async fn stream(&self, request: ChatRequest) -> Result<EventStream> {
        self.stream(request).await
    }
}

fn parse_models(body: &Value) -> Vec<ModelSummary> {
    body.get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let id = row.get("id")?.as_str()?;
            let mut model = ModelSummary::new(id);
            model.display_name = row
                .get("display_name")
                .and_then(Value::as_str)
                .map(str::to_string);
            model.kind = ModelKind::Vlm;
            model.vision = true;
            model.modalities = vec!["text".into(), "image".into()];
            model.max_context_window = row.get("max_input_tokens").and_then(Value::as_u64);
            model.max_output_tokens = row.get("max_tokens").and_then(Value::as_u64);
            model.tools = Some(true);
            model.thinking = Some(true);
            model.json_schema = Some(true);
            Some(model)
        })
        .collect()
}

/// Whether the model takes the adaptive thinking configuration (Claude 4.6
/// and later) rather than an explicit token budget.
fn uses_adaptive_thinking(model: &str) -> bool {
    [
        "opus-5",
        "sonnet-5",
        "haiku-5",
        "fable",
        "opus-4-8",
        "opus-4-7",
        "sonnet-4-6",
    ]
    .iter()
    .any(|marker| model.contains(marker))
}

fn thinking_config(model: &str, max_tokens: u32) -> Option<Value> {
    if uses_adaptive_thinking(model) {
        return Some(json!({ "type": "adaptive", "display": "summarized" }));
    }
    // A budget needs room to think and room to answer.
    if max_tokens < 2048 {
        return None;
    }
    let budget = (max_tokens / 2).clamp(1024, max_tokens - 1);
    Some(json!({ "type": "enabled", "budget_tokens": budget }))
}

fn build_payload(request: &ChatRequest) -> Result<Value> {
    let descriptor = catalogue::descriptor(ANTHROPIC_ID);
    let sends_sampling = descriptor
        .map(|descriptor| descriptor.sends_sampling_params(&request.model))
        .unwrap_or(true);
    let mut payload = json!({
        "model": request.model,
        "max_tokens": request.max_tokens,
        "stream": true,
        "messages": encode_messages(&request.messages)?,
    });
    if let Some(system) = request.system_text() {
        payload["system"] = json!([{ "type": "text", "text": system }]);
    }
    if sends_sampling {
        // Newer models reject temperature and top_p together; temperature
        // wins when both were requested.
        if let Some(temperature) = request.temperature {
            payload["temperature"] = super::sampling_value(temperature);
        } else if let Some(top_p) = request.top_p {
            payload["top_p"] = super::sampling_value(top_p);
        }
    }
    if !request.stop.is_empty() {
        payload["stop_sequences"] = json!(request.stop);
    }
    if !request.tools.is_empty() {
        payload["tools"] = Value::Array(request.tools.iter().map(tool_json).collect());
    }
    if let Some(ResponseFormat::JsonSchema { schema, .. }) = &request.response_format {
        payload["output_config"] = json!({ "format": { "type": "json_schema", "schema": schema } });
    }
    if request.thinking {
        if let Some(thinking) = thinking_config(&request.model, request.max_tokens) {
            payload["thinking"] = thinking;
        }
    }
    Ok(payload)
}

fn tool_json(tool: &ToolDefinition) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": tool.input_schema,
    })
}

/// Encode the transcript in Anthropic's shape: system rows are omitted (they
/// travel as the `system` parameter), tool rows become `tool_result` blocks
/// inside a user message, and adjacent same-role messages are merged because
/// the API requires strict alternation.
fn encode_messages(messages: &[ModelMessage]) -> Result<Vec<Value>> {
    let mut encoded: Vec<(String, Vec<Value>)> = Vec::new();
    for message in messages {
        let (role, blocks) = match message.role {
            Role::System => continue,
            Role::User => ("user", user_blocks(message)?),
            Role::Assistant => ("assistant", assistant_blocks(message)?),
            Role::Tool => {
                let tool_use_id = message.tool_call_id.clone().ok_or_else(|| {
                    Error::Invalid("tool result message has no tool_call_id".into())
                })?;
                let content = message
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        ModelContentPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                (
                    "user",
                    vec![
                        json!({ "type": "tool_result", "tool_use_id": tool_use_id, "content": content }),
                    ],
                )
            }
        };
        if blocks.is_empty() {
            continue;
        }
        match encoded.last_mut() {
            Some((last_role, last_blocks)) if last_role == role => last_blocks.extend(blocks),
            _ => encoded.push((role.to_string(), blocks)),
        }
    }
    Ok(encoded
        .into_iter()
        .map(|(role, content)| json!({ "role": role, "content": content }))
        .collect())
}

fn user_blocks(message: &ModelMessage) -> Result<Vec<Value>> {
    message
        .content
        .iter()
        .map(|part| match part {
            ModelContentPart::Text { text } => Ok(json!({ "type": "text", "text": text })),
            ModelContentPart::ImageUrl { image_url } => image_block(&image_url.url),
            ModelContentPart::InputAudio { .. } => Err(Error::UnsupportedContent(
                "Anthropic messages do not accept audio input".into(),
            )),
        })
        .collect()
}

fn assistant_blocks(message: &ModelMessage) -> Result<Vec<Value>> {
    let mut blocks = Vec::new();
    for part in &message.content {
        if let ModelContentPart::Text { text } = part {
            if !text.is_empty() {
                blocks.push(json!({ "type": "text", "text": text }));
            }
        }
    }
    for call in &message.tool_calls {
        let input: Value = if call.arguments.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&call.arguments).map_err(|error| {
                Error::Invalid(format!("{} has invalid tool arguments: {error}", call.name))
            })?
        };
        blocks
            .push(json!({ "type": "tool_use", "id": call.id, "name": call.name, "input": input }));
    }
    Ok(blocks)
}

fn image_block(url: &str) -> Result<Value> {
    if let Some(rest) = url.strip_prefix("data:") {
        let (media_type, data) = rest.split_once(";base64,").ok_or_else(|| {
            Error::UnsupportedContent("image data URLs must be base64 encoded".into())
        })?;
        return Ok(json!({
            "type": "image",
            "source": { "type": "base64", "media_type": media_type, "data": data },
        }));
    }
    Ok(json!({ "type": "image", "source": { "type": "url", "url": url } }))
}

#[derive(Default)]
struct StreamState {
    input_tokens: Option<u64>,
}

fn map_stop_reason(reason: &str) -> String {
    match reason {
        "end_turn" | "stop_sequence" | "pause_turn" => "stop".into(),
        "max_tokens" => "length".into(),
        "tool_use" => "tool_calls".into(),
        "refusal" => "content_filter".into(),
        other => other.to_string(),
    }
}

fn parse_frame(state: &Mutex<StreamState>, frame: &SseFrame) -> Result<SseOutcome> {
    let data: Value = serde_json::from_str(&frame.data).map_err(|error| Error::Provider {
        provider: ANTHROPIC_ID.into(),
        status: None,
        message: format!("invalid stream event: {error}"),
    })?;
    let event = frame
        .event
        .as_deref()
        .or_else(|| data.get("type").and_then(Value::as_str))
        .unwrap_or_default();
    let mut events = Vec::new();
    match event {
        "message_start" => {
            let input = data
                .pointer("/message/usage/input_tokens")
                .and_then(Value::as_u64);
            state.lock().unwrap().input_tokens = input;
        }
        "content_block_start" => {
            let block = &data["content_block"];
            if block["type"] == "tool_use" {
                events.push(StreamEvent::ToolCallDelta {
                    index: data["index"].as_u64().unwrap_or(0) as u32,
                    id: block["id"].as_str().map(str::to_string),
                    name: block["name"].as_str().map(str::to_string),
                    arguments: String::new(),
                });
            }
        }
        "content_block_delta" => {
            let delta = &data["delta"];
            match delta["type"].as_str().unwrap_or_default() {
                "text_delta" => {
                    if let Some(text) = delta["text"].as_str() {
                        events.push(StreamEvent::Token { text: text.into() });
                    }
                }
                "thinking_delta" => {
                    if let Some(text) = delta["thinking"].as_str() {
                        events.push(StreamEvent::Reasoning { text: text.into() });
                    }
                }
                "input_json_delta" => events.push(StreamEvent::ToolCallDelta {
                    index: data["index"].as_u64().unwrap_or(0) as u32,
                    id: None,
                    name: None,
                    arguments: delta["partial_json"].as_str().unwrap_or_default().into(),
                }),
                _ => {}
            }
        }
        "message_delta" => {
            if let Some(reason) = data.pointer("/delta/stop_reason").and_then(Value::as_str) {
                events.push(StreamEvent::Done {
                    finish_reason: Some(map_stop_reason(reason)),
                });
            }
            if let Some(output) = data.pointer("/usage/output_tokens").and_then(Value::as_u64) {
                let input = state.lock().unwrap().input_tokens;
                let mut usage = json!({ "output_tokens": output });
                if let Some(input) = input {
                    usage["input_tokens"] = json!(input);
                }
                events.push(StreamEvent::Usage {
                    usage: normalize_usage(usage),
                });
            }
        }
        "message_stop" => return Ok(SseOutcome::terminal(events)),
        "error" => {
            let message = data
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("stream error")
                .to_string();
            events.push(StreamEvent::Error { error: message });
            return Ok(SseOutcome::terminal(events));
        }
        _ => {}
    }
    Ok(SseOutcome::events(events))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ImageUrl, ModelToolCall};
    use futures_util::StreamExt;
    use mockito::Matcher;

    fn request(model: &str) -> ChatRequest {
        ChatRequest {
            model: model.into(),
            messages: vec![
                ModelMessage::text(Role::System, "Be terse."),
                ModelMessage::text(Role::User, "Find papers"),
                ModelMessage {
                    role: Role::Assistant,
                    content: vec![],
                    name: None,
                    tool_call_id: None,
                    tool_calls: vec![ModelToolCall {
                        id: "toolu_1".into(),
                        name: "scix".into(),
                        arguments: r#"{"q":"stars"}"#.into(),
                    }],
                },
                ModelMessage {
                    role: Role::Tool,
                    content: vec![ModelContentPart::Text {
                        text: "3 papers".into(),
                    }],
                    name: Some("scix".into()),
                    tool_call_id: Some("toolu_1".into()),
                    tool_calls: vec![],
                },
                ModelMessage {
                    role: Role::User,
                    content: vec![
                        ModelContentPart::Text {
                            text: "and this image".into(),
                        },
                        ModelContentPart::ImageUrl {
                            image_url: ImageUrl {
                                url: "data:image/png;base64,AAAA".into(),
                                detail: None,
                            },
                        },
                    ],
                    name: None,
                    tool_call_id: None,
                    tool_calls: vec![],
                },
            ],
            temperature: Some(0.5),
            top_p: Some(0.9),
            max_tokens: 4096,
            thinking: true,
            tools: vec![ToolDefinition {
                name: "scix".into(),
                description: "Search".into(),
                input_schema: json!({ "type": "object" }),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn payload_uses_the_messages_api_shape() {
        let payload = build_payload(&request("claude-opus-5")).unwrap();
        assert_eq!(payload["system"][0]["text"], "Be terse.");
        assert!(
            payload.get("temperature").is_none(),
            "opus 5 rejects sampling params"
        );
        assert_eq!(payload["thinking"]["type"], "adaptive");
        assert_eq!(payload["tools"][0]["input_schema"]["type"], "object");
        let messages = payload["messages"].as_array().unwrap();
        assert_eq!(
            messages.len(),
            3,
            "system dropped; tool result merged into the user turn"
        );
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"][0]["type"], "tool_use");
        assert_eq!(messages[1]["content"][0]["input"]["q"], "stars");
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "toolu_1");
        assert_eq!(messages[2]["content"][1]["text"], "and this image");
        assert_eq!(
            messages[2]["content"][2]["source"]["media_type"],
            "image/png"
        );
    }

    #[test]
    fn legacy_models_get_a_budget_and_sampling() {
        let payload = build_payload(&request("claude-haiku-4-5-20251001")).unwrap();
        assert_eq!(payload["temperature"], 0.5);
        assert!(payload.get("top_p").is_none(), "never both");
        assert_eq!(payload["thinking"]["type"], "enabled");
        assert_eq!(payload["thinking"]["budget_tokens"], 2048);
        let mut small = request("claude-haiku-4-5-20251001");
        small.max_tokens = 512;
        assert!(build_payload(&small).unwrap().get("thinking").is_none());
        let mut structured = request("claude-sonnet-5");
        structured.response_format = Some(ResponseFormat::JsonSchema {
            name: "x".into(),
            schema: json!({ "type": "object" }),
            strict: true,
        });
        assert_eq!(
            build_payload(&structured).unwrap()["output_config"]["format"]["type"],
            "json_schema"
        );
    }

    #[test]
    fn stream_events_map_onto_the_shared_vocabulary() {
        let state = Mutex::new(StreamState::default());
        let frame = |event: &str, data: &str| SseFrame {
            event: Some(event.into()),
            data: data.into(),
        };
        assert!(parse_frame(
            &state,
            &frame(
                "message_start",
                r#"{"type":"message_start","message":{"usage":{"input_tokens":12}}}"#
            )
        )
        .unwrap()
        .events
        .is_empty());
        let start = parse_frame(&state, &frame("content_block_start", r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"scix"}}"#)).unwrap();
        assert!(
            matches!(&start.events[0], StreamEvent::ToolCallDelta { index: 1, id: Some(id), name: Some(name), .. } if id == "toolu_1" && name == "scix")
        );
        let text = parse_frame(&state, &frame("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#)).unwrap();
        assert!(matches!(&text.events[0], StreamEvent::Token { text } if text == "Hi"));
        let thinking = parse_frame(
            &state,
            &frame(
                "content_block_delta",
                r#"{"delta":{"type":"thinking_delta","thinking":"hmm"}}"#,
            ),
        )
        .unwrap();
        assert!(matches!(&thinking.events[0], StreamEvent::Reasoning { text } if text == "hmm"));
        let json_delta = parse_frame(
            &state,
            &frame(
                "content_block_delta",
                r#"{"index":1,"delta":{"type":"input_json_delta","partial_json":"{\"q\":"}}"#,
            ),
        )
        .unwrap();
        assert!(
            matches!(&json_delta.events[0], StreamEvent::ToolCallDelta { index: 1, id: None, arguments, .. } if arguments == "{\"q\":")
        );
        let delta = parse_frame(
            &state,
            &frame(
                "message_delta",
                r#"{"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":30}}"#,
            ),
        )
        .unwrap();
        assert!(
            matches!(&delta.events[0], StreamEvent::Done { finish_reason: Some(reason) } if reason == "tool_calls")
        );
        assert!(
            matches!(&delta.events[1], StreamEvent::Usage { usage } if usage["prompt_tokens"] == 12 && usage["completion_tokens"] == 30 && usage["total_tokens"] == 42)
        );
        assert!(
            parse_frame(&state, &frame("message_stop", r#"{"type":"message_stop"}"#))
                .unwrap()
                .terminal
        );
        let error = parse_frame(
            &state,
            &frame(
                "error",
                r#"{"type":"error","error":{"message":"overloaded"}}"#,
            ),
        )
        .unwrap();
        assert!(error.terminal);
        assert!(matches!(&error.events[0], StreamEvent::Error { error } if error == "overloaded"));
    }

    #[test]
    fn constructor_validates_key_and_origin() {
        assert!(matches!(
            AnthropicClient::new(None, Secret::new(""), "x"),
            Err(Error::NotConfigured { .. })
        ));
        let client =
            AnthropicClient::new(Some("https://proxy.example/v1/"), Secret::new("k"), "x").unwrap();
        assert_eq!(client.origin(), "https://proxy.example");
        let default = AnthropicClient::new(None, Secret::new("k"), "x").unwrap();
        assert_eq!(default.origin(), ANTHROPIC_DEFAULT_URL);
    }

    #[tokio::test]
    async fn streams_a_messages_response_end_to_end() {
        let mut server = mockito::Server::new_async().await;
        let messages = server
            .mock("POST", "/v1/messages")
            .match_header("x-api-key", "k")
            .match_header("anthropic-version", ANTHROPIC_VERSION)
            .match_body(Matcher::PartialJson(json!({ "model": "claude-sonnet-5", "stream": true })))
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(concat!(
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5}}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
                "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
            ))
            .create_async()
            .await;
        let models = server
            .mock("GET", "/v1/models?limit=100")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":[{"id":"claude-sonnet-5","display_name":"Claude Sonnet 5"}]}"#)
            .create_async()
            .await;
        let client = AnthropicClient::new(Some(&server.url()), Secret::new("k"), "test").unwrap();
        let mut stream = client
            .stream(ChatRequest {
                model: "claude-sonnet-5".into(),
                messages: vec![ModelMessage::text(Role::User, "hi")],
                ..Default::default()
            })
            .await
            .unwrap();
        let mut events = vec![];
        while let Some(event) = stream.next().await {
            events.push(event.unwrap());
        }
        messages.assert_async().await;
        assert!(matches!(&events[0], StreamEvent::Token { text } if text == "Hello"));
        assert!(
            matches!(&events[1], StreamEvent::Done { finish_reason: Some(reason) } if reason == "stop")
        );
        assert!(matches!(&events[2], StreamEvent::Usage { usage } if usage["total_tokens"] == 6));
        assert_eq!(
            events.len(),
            3,
            "message_stop is terminal and adds no synthetic Done"
        );

        let listed = client.models().await.unwrap();
        models.assert_async().await;
        assert_eq!(listed[0].display_name.as_deref(), Some("Claude Sonnet 5"));
        assert!(listed[0].vision);
    }
}
