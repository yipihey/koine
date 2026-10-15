//! One client for every host that speaks the OpenAI chat-completions wire
//! format: oMLX, OpenAI, OpenRouter, Google's compatibility endpoint,
//! Ollama's `/v1` surface and any generic `/v1` server. A [`Dialect`]
//! decides the parts that differ — paths, headers, which sampling and
//! thinking parameters a host accepts, and how discovery is shaped.

use std::sync::Arc;

use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use super::{http_client, status_error, transport_error};
use crate::catalogue::{self, Dialect, ProviderDescriptor};
use crate::provider::{EventStream, HealthState, InferenceProvider, ProviderHealth};
use crate::secret::Secret;
use crate::sse::{spawn_sse_stream, SseFrame, SseOutcome};
use crate::types::{
    normalize_usage, ChatRequest, ModelKind, ModelMessage, ModelSummary, ResponseFormat,
    StreamEvent, ToolDefinition,
};
use crate::{Error, Result};

impl Dialect {
    /// The catalogue id this dialect serves by default.
    pub fn provider_id(self) -> &'static str {
        match self {
            Self::Omlx => catalogue::OMLX_ID,
            Self::Generic => catalogue::OPENAI_COMPATIBLE_ID,
            Self::OpenAi => catalogue::OPENAI_ID,
            Self::OpenRouter => catalogue::OPENROUTER_ID,
            Self::Google => catalogue::GOOGLE_ID,
            Self::Ollama => catalogue::OLLAMA_ID,
        }
    }

    fn api_root(self) -> &'static str {
        match self {
            Self::OpenRouter => "/api/v1",
            Self::Google => "/v1beta/openai",
            _ => "/v1",
        }
    }

    fn chat_path(self) -> String {
        format!("{}/chat/completions", self.api_root())
    }

    fn models_path(self) -> String {
        match self {
            Self::Ollama => "/api/tags".into(),
            _ => format!("{}/models", self.api_root()),
        }
    }

    /// Suffixes users paste from client docs that must not be doubled.
    fn stripped_suffixes(self) -> &'static [&'static str] {
        match self {
            Self::OpenRouter => &["/api/v1", "/api"],
            Self::Google => &["/v1beta/openai", "/v1beta"],
            _ => &["/v1"],
        }
    }

    fn max_tokens_key(self) -> &'static str {
        match self {
            Self::OpenAi => "max_completion_tokens",
            _ => "max_tokens",
        }
    }

    fn include_usage(self) -> bool {
        !matches!(self, Self::Google)
    }

    fn default_headers(self) -> Vec<(&'static str, String)> {
        match self {
            Self::OpenRouter => vec![
                ("HTTP-Referer", "https://impress.app".into()),
                ("X-Title", "Impress".into()),
            ],
            _ => vec![],
        }
    }
}

/// An OpenAI-compatible chat client bound to one host and one dialect.
#[derive(Clone)]
pub struct OpenAiCompatibleClient {
    client: reqwest::Client,
    dialect: Dialect,
    origin: String,
    api_key: Option<Secret>,
    provider_id: &'static str,
    endpoint_id: String,
    headers: Vec<(&'static str, String)>,
}

impl OpenAiCompatibleClient {
    pub fn new(
        dialect: Dialect,
        origin: impl Into<String>,
        api_key: Option<Secret>,
        endpoint_id: impl Into<String>,
    ) -> Result<Self> {
        let origin = normalized_origin(dialect, origin.into())?;
        Ok(Self {
            client: http_client()?,
            dialect,
            origin,
            api_key: api_key.filter(|key| !key.is_empty()),
            provider_id: dialect.provider_id(),
            endpoint_id: endpoint_id.into(),
            headers: dialect.default_headers(),
        })
    }

    /// Serve a different catalogue id than the dialect's default (a generic
    /// server registered under its own name).
    pub fn with_provider_id(mut self, provider_id: &'static str) -> Self {
        self.provider_id = provider_id;
        self
    }

    pub fn dialect(&self) -> Dialect {
        self.dialect
    }

    /// Canonical endpoint root used for requests and provenance, with any
    /// pasted `/v1`-style suffix removed so paths are never doubled.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn endpoint_id(&self) -> &str {
        &self.endpoint_id
    }

    fn descriptor(&self) -> Option<&'static ProviderDescriptor> {
        catalogue::descriptor(self.provider_id)
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.origin));
        if let Some(key) = &self.api_key {
            request = request.header(AUTHORIZATION, format!("Bearer {}", key.expose()));
        }
        for (name, value) in &self.headers {
            request = request.header(*name, value);
        }
        request
    }

    async fn get_json(&self, path: &str) -> Result<Value> {
        let response = self
            .request(reqwest::Method::GET, path)
            .header(ACCEPT, "application/json")
            .send()
            .await
            .map_err(|error| transport_error(self.provider_id, error))?;
        if !response.status().is_success() {
            return Err(status_error(self.provider_id, response).await);
        }
        response
            .json::<Value>()
            .await
            .map_err(|error| Error::Provider {
                provider: self.provider_id.into(),
                status: None,
                message: format!("invalid JSON from {path}: {error}"),
            })
    }

    async fn optional_json(&self, path: &str) -> Option<Value> {
        match self
            .request(reqwest::Method::GET, path)
            .header(ACCEPT, "application/json")
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => response.json::<Value>().await.ok(),
            _ => None,
        }
    }

    pub async fn models(&self) -> Result<Vec<ModelSummary>> {
        let listed = self.get_json(&self.dialect.models_path()).await?;
        let mut models = match self.dialect {
            Dialect::Omlx => {
                let status = self.optional_json("/v1/models/status").await;
                let health = self.optional_json("/health").await;
                parse_omlx_models(&listed, status.as_ref(), health.as_ref())
            }
            Dialect::Ollama => parse_ollama_models(&listed),
            Dialect::OpenRouter => parse_openrouter_models(&listed),
            Dialect::Google => parse_generic_models(&listed, Some("models/")),
            Dialect::OpenAi | Dialect::Generic => parse_generic_models(&listed, None),
        };
        models.sort_by(|left, right| {
            (!left.loaded).cmp(&(!right.loaded)).then_with(|| {
                left.display_name()
                    .to_lowercase()
                    .cmp(&right.display_name().to_lowercase())
            })
        });
        Ok(models)
    }

    pub async fn health(&self) -> Result<ProviderHealth> {
        if self.dialect != Dialect::Omlx {
            let mut health = ProviderHealth::from_models(self.models().await?);
            health.endpoint = Some(self.origin.clone());
            return Ok(health);
        }
        let body = match self.get_json("/health").await {
            Ok(body) => body,
            Err(error @ Error::Unreachable { .. }) => {
                let mut health = ProviderHealth::unreachable(error.to_string());
                health.endpoint = Some(self.origin.clone());
                return Ok(health);
            }
            Err(error) => return Err(error),
        };
        let pool = &body["engine_pool"];
        let model_count = pool["model_count"].as_u64().unwrap_or(0) as u32;
        let loaded_count = pool["loaded_count"].as_u64().map(|count| count as u32);
        let status = self.optional_json("/api/status").await;
        let server_version = status
            .as_ref()
            .and_then(|status| status["version"].as_str())
            .map(str::to_string);
        let state = if model_count == 0 {
            HealthState::Empty
        } else {
            HealthState::Ready
        };
        let detail = match (&server_version, loaded_count) {
            (Some(version), Some(loaded)) => {
                format!("oMLX {version} · {loaded} of {model_count} loaded")
            }
            (None, Some(loaded)) => format!("oMLX · {loaded} of {model_count} loaded"),
            _ => format!("oMLX · {model_count} models"),
        };
        Ok(ProviderHealth {
            endpoint: Some(self.origin.clone()),
            default_model: body["default_model"].as_str().map(str::to_string),
            model_count,
            loaded_count,
            server_version,
            memory_in_use_bytes: pool["current_model_memory"].as_u64(),
            memory_ceiling_bytes: pool["final_ceiling"].as_u64(),
            ..ProviderHealth::new(state, detail)
        })
    }

    pub async fn stream(&self, request: ChatRequest) -> Result<EventStream> {
        request.validate()?;
        let payload = self.build_payload(&request);
        let response = self
            .request(reqwest::Method::POST, &self.dialect.chat_path())
            .header(ACCEPT, "text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|error| transport_error(self.provider_id, error))?;
        if !response.status().is_success() {
            return Err(status_error(self.provider_id, response).await);
        }
        let provider: Arc<str> = Arc::from(self.provider_id);
        let provider_for_frames = provider.clone();
        Ok(spawn_sse_stream(
            response,
            move |message| Error::Unreachable {
                provider: provider.to_string(),
                message,
            },
            move |frame| parse_frame(&provider_for_frames, frame),
        ))
    }

    fn build_payload(&self, request: &ChatRequest) -> Value {
        let sends_sampling = self
            .descriptor()
            .map(|descriptor| descriptor.sends_sampling_params(&request.model))
            .unwrap_or(true);
        let mut payload = json!({
            "model": request.model,
            "messages": request.messages.iter().map(message_json).collect::<Vec<_>>(),
            "stream": true,
        });
        payload[self.dialect.max_tokens_key()] = json!(request.max_tokens);
        if sends_sampling {
            if let Some(temperature) = request.temperature {
                payload["temperature"] = super::sampling_value(temperature);
            }
            if let Some(top_p) = request.top_p {
                payload["top_p"] = super::sampling_value(top_p);
            }
        }
        if !request.stop.is_empty() {
            payload["stop"] = json!(request.stop);
        }
        if self.dialect.include_usage() {
            payload["stream_options"] = json!({ "include_usage": true });
        }
        if !request.tools.is_empty() {
            payload["tools"] = Value::Array(request.tools.iter().map(tool_json).collect());
            payload["tool_choice"] = Value::String("auto".into());
        }
        if let Some(format) = &request.response_format {
            if let Some(value) = response_format_json(format) {
                payload["response_format"] = value;
            }
        }
        apply_thinking(self.dialect, request, sends_sampling, &mut payload);
        payload
    }
}

#[async_trait::async_trait]
impl InferenceProvider for OpenAiCompatibleClient {
    fn provider_id(&self) -> &str {
        self.provider_id
    }

    fn endpoint_id(&self) -> &str {
        &self.endpoint_id
    }

    async fn models(&self) -> Result<Vec<ModelSummary>> {
        self.models().await
    }

    async fn health(&self) -> Result<ProviderHealth> {
        self.health().await
    }

    async fn stream(&self, request: ChatRequest) -> Result<EventStream> {
        self.stream(request).await
    }
}

fn normalized_origin(dialect: Dialect, value: String) -> Result<String> {
    let mut trimmed = value.trim().trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        return Err(Error::NotConfigured {
            provider: dialect.provider_id().into(),
            message: "no endpoint configured".into(),
        });
    }
    for suffix in dialect.stripped_suffixes() {
        if let Some(stripped) = trimmed.strip_suffix(suffix) {
            trimmed = stripped.trim_end_matches('/').to_string();
            break;
        }
    }
    let parsed = url::Url::parse(&trimmed).map_err(|error| {
        Error::Invalid(format!(
            "{} endpoint must be an http(s) URL: {error}",
            dialect.provider_id()
        ))
    })?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(Error::Invalid(format!(
            "{} endpoint must be an http(s) URL",
            dialect.provider_id()
        )));
    }
    Ok(trimmed)
}

fn apply_thinking(
    dialect: Dialect,
    request: &ChatRequest,
    sends_sampling: bool,
    payload: &mut Value,
) {
    match dialect {
        Dialect::Omlx => {
            payload["chat_template_kwargs"] = json!({ "enable_thinking": request.thinking });
        }
        Dialect::Generic => {
            if request.thinking {
                payload["chat_template_kwargs"] = json!({ "enable_thinking": true });
            }
        }
        // Reasoning models expose the effort knob; chat models reject it.
        Dialect::OpenAi => {
            if request.thinking && !sends_sampling {
                payload["reasoning_effort"] = json!("medium");
            }
        }
        Dialect::OpenRouter => {
            if request.thinking {
                payload["reasoning"] = json!({ "enabled": true });
            }
        }
        Dialect::Google => {
            if request.thinking {
                payload["reasoning_effort"] = json!("medium");
                payload["extra_body"] =
                    json!({ "google": { "thinking_config": { "include_thoughts": true } } });
            }
        }
        Dialect::Ollama => {}
    }
}

fn response_format_json(format: &ResponseFormat) -> Option<Value> {
    match format {
        ResponseFormat::Text => None,
        ResponseFormat::JsonObject => Some(json!({ "type": "json_object" })),
        ResponseFormat::JsonSchema {
            name,
            schema,
            strict,
        } => Some(json!({
            "type": "json_schema",
            "json_schema": { "name": name, "schema": schema, "strict": strict },
        })),
    }
}

pub(crate) fn message_json(message: &ModelMessage) -> Value {
    let content = match message.content.as_slice() {
        [crate::types::ModelContentPart::Text { text }] => Value::String(text.clone()),
        _ => serde_json::to_value(&message.content).unwrap_or(Value::Array(vec![])),
    };
    let mut value = json!({ "role": message.role, "content": content });
    if let Some(name) = &message.name {
        value["name"] = Value::String(name.clone());
    }
    if let Some(tool_call_id) = &message.tool_call_id {
        value["tool_call_id"] = Value::String(tool_call_id.clone());
    }
    if !message.tool_calls.is_empty() {
        value["tool_calls"] = Value::Array(
            message
                .tool_calls
                .iter()
                .map(|call| {
                    json!({
                        "id": call.id,
                        "type": "function",
                        "function": { "name": call.name, "arguments": call.arguments },
                    })
                })
                .collect(),
        );
    }
    value
}

pub(crate) fn tool_json(tool: &ToolDefinition) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.input_schema,
        }
    })
}

fn content_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| match part {
                Value::String(text) => Some(text.as_str()),
                Value::Object(object) => object.get("text").and_then(Value::as_str),
                _ => None,
            })
            .collect(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn parse_frame(provider: &str, frame: &SseFrame) -> Result<SseOutcome> {
    if frame.data.trim() == "[DONE]" {
        return Ok(SseOutcome::terminal(vec![]));
    }
    Ok(SseOutcome::events(parse_chunk(provider, &frame.data)?))
}

pub(crate) fn parse_chunk(provider: &str, data: &str) -> Result<Vec<StreamEvent>> {
    let value: Value = serde_json::from_str(data).map_err(|error| Error::Provider {
        provider: provider.into(),
        status: None,
        message: format!("invalid stream event: {error}"),
    })?;
    let mut events = Vec::new();
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| error.to_string());
        events.push(StreamEvent::Error { error: message });
        return Ok(events);
    }
    if let Some(choice) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
    {
        let delta = choice.get("delta").unwrap_or(choice);
        let reasoning = ["reasoning_content", "reasoning", "thinking"]
            .into_iter()
            .map(|key| content_text(delta.get(key)))
            .find(|text| !text.is_empty())
            .unwrap_or_default();
        if !reasoning.is_empty() {
            events.push(StreamEvent::Reasoning { text: reasoning });
        }
        let text = content_text(delta.get("content"));
        if !text.is_empty() {
            events.push(StreamEvent::Token { text });
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let function = call.get("function").unwrap_or(&Value::Null);
                events.push(StreamEvent::ToolCallDelta {
                    index: call.get("index").and_then(Value::as_u64).unwrap_or(0) as u32,
                    id: call.get("id").and_then(Value::as_str).map(str::to_string),
                    name: function
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    arguments: function
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                });
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            events.push(StreamEvent::Done {
                finish_reason: Some(reason.into()),
            });
        }
    }
    if let Some(usage) = value.get("usage").filter(|usage| usage.is_object()) {
        events.push(StreamEvent::Usage {
            usage: normalize_usage(usage.clone()),
        });
    }
    Ok(events)
}

fn listed_rows(listed: &Value) -> impl Iterator<Item = &Value> {
    listed
        .get("data")
        .or_else(|| listed.get("models"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn parse_generic_models(listed: &Value, strip_prefix: Option<&str>) -> Vec<ModelSummary> {
    listed_rows(listed)
        .filter_map(|row| {
            let raw = row.get("id")?.as_str()?;
            let id = strip_prefix
                .and_then(|prefix| raw.strip_prefix(prefix))
                .unwrap_or(raw);
            if id.is_empty() {
                return None;
            }
            let mut model = ModelSummary::new(id);
            model.max_context_window = ["context_length", "max_model_len", "context_window"]
                .into_iter()
                .find_map(|key| row.get(key).and_then(Value::as_u64));
            Some(model)
        })
        .collect()
}

fn parse_openrouter_models(listed: &Value) -> Vec<ModelSummary> {
    listed_rows(listed)
        .filter_map(|row| {
            let id = row.get("id")?.as_str()?;
            let supported: Vec<&str> = row
                .get("supported_parameters")
                .and_then(Value::as_array)
                .map(|values| values.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let vision = row
                .pointer("/architecture/input_modalities")
                .and_then(Value::as_array)
                .is_some_and(|modalities| modalities.iter().any(|value| value == "image"));
            let mut model = ModelSummary::new(id);
            model.display_name = row.get("name").and_then(Value::as_str).map(str::to_string);
            model.kind = if vision {
                ModelKind::Vlm
            } else {
                ModelKind::Llm
            };
            model.max_context_window = row.get("context_length").and_then(Value::as_u64);
            model.max_output_tokens = row
                .pointer("/top_provider/max_completion_tokens")
                .and_then(Value::as_u64);
            model.modalities = if vision {
                vec!["text".into(), "image".into()]
            } else {
                vec!["text".into()]
            };
            model.vision = vision;
            if !supported.is_empty() {
                model.tools = Some(supported.contains(&"tools"));
                model.json_schema = Some(
                    supported.contains(&"response_format")
                        || supported.contains(&"structured_outputs"),
                );
                model.thinking = Some(
                    supported.contains(&"reasoning") || supported.contains(&"include_reasoning"),
                );
            }
            Some(model)
        })
        .collect()
}

fn parse_ollama_models(listed: &Value) -> Vec<ModelSummary> {
    listed_rows(listed)
        .filter_map(|row| {
            let name = row.get("name").or_else(|| row.get("model"))?.as_str()?;
            let families: Vec<String> = row
                .pointer("/details/families")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(|value| value.to_lowercase())
                        .collect()
                })
                .unwrap_or_default();
            let lowered = name.to_lowercase();
            let vision = families
                .iter()
                .any(|family| matches!(family.as_str(), "clip" | "mllama" | "qwen2vl"))
                || lowered.contains("llava")
                || lowered.contains("vision");
            let mut model = ModelSummary::new(name);
            model.display_name = Some(catalogue::friendly_model_name(name));
            model.kind = if vision {
                ModelKind::Vlm
            } else {
                ModelKind::Llm
            };
            model.loaded = false;
            model.modalities = if vision {
                vec!["text".into(), "image".into()]
            } else {
                vec!["text".into()]
            };
            model.vision = vision;
            Some(model)
        })
        .collect()
}

fn parse_omlx_models(
    listed: &Value,
    status: Option<&Value>,
    health: Option<&Value>,
) -> Vec<ModelSummary> {
    let default_model = health
        .and_then(|health| health.get("default_model"))
        .and_then(Value::as_str);
    let status_rows: Vec<&Value> = status
        .and_then(|status| status.get("models"))
        .and_then(Value::as_array)
        .map(|rows| rows.iter().collect())
        .unwrap_or_default();
    listed_rows(listed)
        .filter_map(|row| {
            let id = row.get("id")?.as_str()?;
            let status = status_rows
                .iter()
                .find(|candidate| candidate.get("id").and_then(Value::as_str) == Some(id))
                .copied();
            let field = |key: &str| status.and_then(|status| status.get(key));
            if field("is_hidden").and_then(Value::as_bool).unwrap_or(false) {
                return None;
            }
            let model_type = field("model_type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_lowercase();
            let is_helper = field("is_helper").and_then(Value::as_bool).unwrap_or(false)
                || model_type == "markitdown"
                || id == "MarkItDown";
            let kind = match model_type.as_str() {
                "llm" => ModelKind::Llm,
                "vlm" => ModelKind::Vlm,
                "embedding" | "embeddings" => ModelKind::Embedding,
                _ if is_helper => ModelKind::Helper,
                _ => ModelKind::Unknown,
            };
            let vision = kind == ModelKind::Vlm
                || row
                    .get("modalities")
                    .and_then(Value::as_array)
                    .is_some_and(|values| values.iter().any(|value| value == "image"));
            let mut model = ModelSummary::new(id);
            model.display_name = Some(catalogue::friendly_model_name(id));
            model.kind = kind;
            model.loaded = field("loaded")
                .and_then(Value::as_bool)
                .or_else(|| row.get("loaded").and_then(Value::as_bool))
                .unwrap_or(false);
            model.is_loading = field("is_loading")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            model.max_context_window = field("max_context_window")
                .and_then(Value::as_u64)
                .or_else(|| field("model_context_length").and_then(Value::as_u64))
                .or_else(|| row.get("max_model_len").and_then(Value::as_u64))
                .or_else(|| row.get("max_context_window").and_then(Value::as_u64));
            model.max_output_tokens = field("max_tokens").and_then(Value::as_u64);
            model.modalities = if vision {
                vec!["text".into(), "image".into()]
            } else {
                vec!["text".into()]
            };
            model.is_default = default_model == Some(id) && !is_helper;
            model.is_helper = is_helper;
            model.vision = vision;
            if !is_helper {
                model.tools = Some(true);
                model.json_schema = Some(true);
            }
            model.source_repo = field("source_repo_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            Some(model)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ImageUrl, ModelContentPart, Role};
    use futures_util::StreamExt;
    use mockito::Matcher;

    fn client(dialect: Dialect, origin: &str) -> OpenAiCompatibleClient {
        OpenAiCompatibleClient::new(dialect, origin, Some(Secret::new("token")), "test").unwrap()
    }

    #[test]
    fn origins_are_normalised_per_dialect() {
        assert_eq!(
            client(Dialect::Omlx, "http://127.0.0.1:8000/v1/").origin(),
            "http://127.0.0.1:8000"
        );
        assert_eq!(
            client(Dialect::OpenRouter, "https://openrouter.ai/api/v1").origin(),
            "https://openrouter.ai"
        );
        assert_eq!(
            client(
                Dialect::Google,
                "https://generativelanguage.googleapis.com/v1beta/openai/"
            )
            .origin(),
            "https://generativelanguage.googleapis.com"
        );
        assert_eq!(
            client(Dialect::Generic, " http://host:1234/v1 ").origin(),
            "http://host:1234"
        );
        assert!(matches!(
            OpenAiCompatibleClient::new(Dialect::Generic, "", None, "x"),
            Err(Error::NotConfigured { .. })
        ));
        assert!(OpenAiCompatibleClient::new(Dialect::Generic, "ftp://x", None, "x").is_err());
    }

    #[test]
    fn keeps_text_only_messages_backward_compatible() {
        let value = message_json(&ModelMessage::text(Role::User, "hello"));
        assert_eq!(value["content"], "hello");
    }

    #[test]
    fn emits_openai_multimodal_content_arrays() {
        let value = message_json(&ModelMessage {
            role: Role::User,
            content: vec![
                ModelContentPart::Text {
                    text: "look".into(),
                },
                ModelContentPart::ImageUrl {
                    image_url: ImageUrl {
                        url: "data:image/png;base64,AA==".into(),
                        detail: None,
                    },
                },
            ],
            name: None,
            tool_call_id: None,
            tool_calls: vec![],
        });
        assert!(value["content"].is_array());
        assert_eq!(value["content"][1]["type"], "image_url");
    }

    #[test]
    fn parses_reasoning_tools_usage_and_finish_reason() {
        let events = parse_chunk(
            "omlx",
            r#"{"choices":[{"delta":{"reasoning_content":"why","content":"answer","tool_calls":[{"index":0,"id":"call_1","function":{"name":"scix_search","arguments":"{\"q\":\"stars\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":4,"completion_tokens":2}}"#,
        )
        .unwrap();
        assert!(matches!(&events[0], StreamEvent::Reasoning { text } if text == "why"));
        assert!(events.iter().any(|event| matches!(event, StreamEvent::ToolCallDelta { name: Some(name), .. } if name == "scix_search")));
        assert!(events.iter().any(
            |event| matches!(event, StreamEvent::Usage { usage } if usage["total_tokens"] == 6)
        ));
        assert!(events.iter().any(|event| matches!(event, StreamEvent::Done { finish_reason: Some(reason) } if reason == "tool_calls")));
    }

    #[test]
    fn accepts_common_thinking_delta_spellings_and_error_frames() {
        for key in ["reasoning_content", "reasoning", "thinking"] {
            let chunk = format!(r#"{{"choices":[{{"delta":{{"{key}":"working"}}}}]}}"#);
            let events = parse_chunk("x", &chunk).unwrap();
            assert!(matches!(&events[0], StreamEvent::Reasoning { text } if text == "working"));
        }
        let events = parse_chunk("x", r#"{"error":{"message":"context too long"}}"#).unwrap();
        assert!(matches!(&events[0], StreamEvent::Error { error } if error == "context too long"));
    }

    #[test]
    fn payload_shape_follows_the_dialect() {
        let request = ChatRequest {
            model: "o3-mini".into(),
            messages: vec![ModelMessage::text(Role::User, "hi")],
            temperature: Some(0.3),
            top_p: Some(0.9),
            stop: vec!["END".into()],
            thinking: true,
            response_format: Some(ResponseFormat::JsonSchema {
                name: "answer".into(),
                schema: json!({ "type": "object" }),
                strict: true,
            }),
            ..Default::default()
        };
        let openai = client(Dialect::OpenAi, "https://api.openai.com").build_payload(&request);
        assert_eq!(openai["max_completion_tokens"], 2048);
        assert!(openai.get("max_tokens").is_none());
        assert!(
            openai.get("temperature").is_none(),
            "o3-mini rejects sampling params"
        );
        assert_eq!(openai["reasoning_effort"], "medium");
        assert_eq!(openai["stop"][0], "END");
        assert_eq!(openai["response_format"]["json_schema"]["name"], "answer");
        assert_eq!(openai["stream_options"]["include_usage"], true);

        let chat = ChatRequest {
            model: "gpt-4o".into(),
            ..request.clone()
        };
        let openai_chat = client(Dialect::OpenAi, "https://api.openai.com").build_payload(&chat);
        assert_eq!(openai_chat["temperature"], 0.3);
        assert!(openai_chat.get("reasoning_effort").is_none());

        let omlx = client(Dialect::Omlx, "http://127.0.0.1:8000").build_payload(&request);
        assert_eq!(omlx["max_tokens"], 2048);
        assert_eq!(omlx["chat_template_kwargs"]["enable_thinking"], true);
        assert_eq!(omlx["temperature"], 0.3);

        let google = client(Dialect::Google, "https://generativelanguage.googleapis.com")
            .build_payload(&request);
        assert!(google.get("stream_options").is_none());
        assert_eq!(
            google["extra_body"]["google"]["thinking_config"]["include_thoughts"],
            true
        );

        let router = client(Dialect::OpenRouter, "https://openrouter.ai").build_payload(&request);
        assert_eq!(router["reasoning"]["enabled"], true);

        let ollama = client(Dialect::Ollama, "http://localhost:11434").build_payload(&request);
        assert!(ollama.get("reasoning").is_none());
        assert!(ollama.get("chat_template_kwargs").is_none());
    }

    #[test]
    fn omlx_discovery_merges_status_and_health() {
        let listed = json!({ "data": [
            { "id": "mlx-community--Qwen3.5-4B-4bit", "max_model_len": 262144 },
            { "id": "mlx-community--Hidden-1B", "max_model_len": 4096 },
            { "id": "MarkItDown", "max_model_len": null }
        ]});
        let status = json!({ "models": [
            { "id": "mlx-community--Qwen3.5-4B-4bit", "loaded": true, "model_type": "vlm", "is_helper": false, "is_hidden": false, "max_context_window": 131072, "max_tokens": 4096, "source_repo_id": "mlx-community/Qwen3.5-4B-4bit" },
            { "id": "mlx-community--Hidden-1B", "loaded": false, "model_type": "llm", "is_hidden": true },
            { "id": "MarkItDown", "loaded": true, "model_type": "markitdown" }
        ]});
        let health = json!({ "default_model": "mlx-community--Qwen3.5-4B-4bit" });
        let models = parse_omlx_models(&listed, Some(&status), Some(&health));
        assert_eq!(models.len(), 2, "hidden models are dropped, helpers kept");
        let qwen = &models[0];
        assert_eq!(qwen.display_name.as_deref(), Some("Qwen3.5 4B 4bit"));
        assert_eq!(qwen.kind, ModelKind::Vlm);
        assert!(qwen.vision && qwen.loaded && qwen.is_default);
        assert_eq!(qwen.max_context_window, Some(131_072));
        assert_eq!(qwen.max_output_tokens, Some(4_096));
        assert_eq!(
            qwen.source_repo.as_deref(),
            Some("mlx-community/Qwen3.5-4B-4bit")
        );
        let helper = &models[1];
        assert!(helper.is_helper);
        assert_eq!(helper.kind, ModelKind::Helper);
        assert!(!helper.is_default);
    }

    #[test]
    fn openrouter_google_and_ollama_rows_parse_their_own_shapes() {
        let router = parse_openrouter_models(&json!({ "data": [{
            "id": "anthropic/claude-sonnet-4", "name": "Claude Sonnet 4", "context_length": 200000,
            "architecture": { "input_modalities": ["text", "image"] },
            "top_provider": { "max_completion_tokens": 64000 },
            "supported_parameters": ["tools", "response_format", "reasoning"]
        }]}));
        assert_eq!(router[0].display_name.as_deref(), Some("Claude Sonnet 4"));
        assert!(router[0].vision);
        assert_eq!(router[0].tools, Some(true));
        assert_eq!(router[0].json_schema, Some(true));
        assert_eq!(router[0].thinking, Some(true));
        assert_eq!(router[0].max_output_tokens, Some(64_000));

        let google = parse_generic_models(
            &json!({ "data": [{ "id": "models/gemini-2.0-flash" }] }),
            Some("models/"),
        );
        assert_eq!(google[0].id, "gemini-2.0-flash");

        let ollama = parse_ollama_models(&json!({ "models": [
            { "name": "llama3.2:latest", "details": { "families": ["llama"] } },
            { "name": "llava:13b", "details": { "families": ["llama", "clip"] } }
        ]}));
        assert_eq!(ollama[0].display_name.as_deref(), Some("llama3.2"));
        assert!(!ollama[0].vision);
        assert!(ollama[1].vision);
    }

    #[tokio::test]
    async fn discovers_loaded_models_from_a_versioned_configured_endpoint() {
        let mut server = mockito::Server::new_async().await;
        let listed = server
            .mock("GET", "/v1/models")
            .match_header("authorization", "Bearer token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":[{"id":"text-model"},{"id":"vision-model"}]}"#)
            .create_async()
            .await;
        let status = server
            .mock("GET", "/v1/models/status")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"models":[{"id":"vision-model","loaded":true,"model_type":"vlm","max_context_window":32768}]}"#)
            .create_async()
            .await;
        let client = client(Dialect::Omlx, &format!("{}/v1", server.url()));
        let models = client.models().await.unwrap();
        listed.assert_async().await;
        status.assert_async().await;
        assert_eq!(models[0].id, "vision-model");
        assert!(models[0].loaded);
        assert!(models[0].vision);
        assert_eq!(models[0].max_context_window, Some(32_768));
        assert_eq!(models[1].id, "text-model");
    }

    #[tokio::test]
    async fn omlx_health_reads_health_and_status_endpoints() {
        let mut server = mockito::Server::new_async().await;
        let _health = server
            .mock("GET", "/health")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":"healthy","default_model":"llama","engine_pool":{"model_count":9,"loaded_count":2,"final_ceiling":100,"current_model_memory":40}}"#)
            .create_async()
            .await;
        let _status = server
            .mock("GET", "/api/status")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"version":"0.6.4"}"#)
            .create_async()
            .await;
        let health = client(Dialect::Omlx, &server.url()).health().await.unwrap();
        assert_eq!(health.state, HealthState::Ready);
        assert_eq!(health.model_count, 9);
        assert_eq!(health.loaded_count, Some(2));
        assert_eq!(health.server_version.as_deref(), Some("0.6.4"));
        assert_eq!(health.default_model.as_deref(), Some("llama"));
        assert_eq!(health.memory_ceiling_bytes, Some(100));
        assert_eq!(health.detail, "oMLX 0.6.4 · 2 of 9 loaded");

        let down = OpenAiCompatibleClient::new(Dialect::Omlx, "http://127.0.0.1:1", None, "x")
            .unwrap()
            .health()
            .await
            .unwrap();
        assert_eq!(down.state, HealthState::Unreachable);
    }

    #[tokio::test]
    async fn streams_tokens_tool_calls_and_usage_and_sends_dialect_headers() {
        let mut server = mockito::Server::new_async().await;
        let chat = server
            .mock("POST", "/api/v1/chat/completions")
            .match_header("HTTP-Referer", "https://impress.app")
            .match_body(Matcher::PartialJson(json!({ "model": "openai/gpt-4o", "stream": true, "max_tokens": 64 })))
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\n",
                "data: [DONE]\n\n"
            ))
            .create_async()
            .await;
        let client = client(Dialect::OpenRouter, &server.url());
        let mut stream = client
            .stream(ChatRequest {
                model: "openai/gpt-4o".into(),
                messages: vec![ModelMessage::text(Role::User, "hi")],
                max_tokens: 64,
                ..Default::default()
            })
            .await
            .unwrap();
        let mut events = vec![];
        while let Some(event) = stream.next().await {
            events.push(event.unwrap());
        }
        chat.assert_async().await;
        let text: String = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::Token { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Hello");
        assert!(events.iter().any(
            |event| matches!(event, StreamEvent::Usage { usage } if usage["total_tokens"] == 5)
        ));
        // OpenAI-style hosts send the usage chunk after the finish_reason
        // chunk, so Done is not the last event; the terminal [DONE] frame
        // adds no synthetic Done of its own.
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, StreamEvent::Done { finish_reason: Some(reason) } if reason == "stop"))
                .count(),
            1
        );
        assert!(matches!(events.last(), Some(StreamEvent::Usage { .. })));
    }

    #[tokio::test]
    async fn maps_http_failures_to_the_shared_error_vocabulary() {
        let mut server = mockito::Server::new_async().await;
        let _unauthorised = server
            .mock("POST", "/v1/chat/completions")
            .with_status(401)
            .with_body(r#"{"error":{"message":"Incorrect API key"}}"#)
            .create_async()
            .await;
        let _limited = server
            .mock("GET", "/v1/models")
            .with_status(429)
            .with_header("retry-after", "7")
            .with_body("slow down")
            .create_async()
            .await;
        let client = client(Dialect::OpenAi, &server.url());
        let request = ChatRequest {
            model: "gpt-4o".into(),
            messages: vec![ModelMessage::text(Role::User, "hi")],
            ..Default::default()
        };
        match client.stream(request).await {
            Err(Error::Unauthorized { provider, message }) => {
                assert_eq!(provider, "openai");
                assert_eq!(message, "Incorrect API key");
            }
            other => panic!("expected Unauthorized, got {other:?}"),
        }
        match client.models().await {
            Err(Error::RateLimited {
                retry_after_secs, ..
            }) => assert_eq!(retry_after_secs, Some(7)),
            other => panic!("expected RateLimited, got {other:?}"),
        }
        let unreachable =
            OpenAiCompatibleClient::new(Dialect::Generic, "http://127.0.0.1:1", None, "x")
                .unwrap()
                .models()
                .await;
        assert!(matches!(unreachable, Err(Error::Unreachable { .. })));
    }
}
