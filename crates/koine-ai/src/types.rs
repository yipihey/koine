use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

/// Content already resolved for an inference endpoint.
///
/// Durable messages never store data URLs/base64. They point to
/// `content-blob@1.0.0` items; `AiStore::prepare_request` resolves those bytes
/// immediately before a model call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
    InputAudio { input_audio: InputAudio },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageUrl {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InputAudio {
    pub data: String,
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelMessage {
    pub role: Role,
    pub content: Vec<ModelContentPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ModelToolCall>,
}

impl ModelMessage {
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![ModelContentPart::Text { text: text.into() }],
            name: None,
            tool_call_id: None,
            tool_calls: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolPolicy {
    /// Stable capability ids such as `scix`, `impress-mcp`, and `web`.
    pub enabled: Vec<String>,
}

impl ToolPolicy {
    pub fn normalized(mut self) -> Self {
        self.enabled.retain(|tool| !tool.trim().is_empty());
        self.enabled.sort();
        self.enabled.dedup();
        self
    }

    pub fn allows(&self, capability: &str) -> bool {
        self.enabled.iter().any(|value| value == capability)
    }
}

/// Structured-output request. `JsonSchema` becomes OpenAI-style
/// `response_format` or Anthropic `output_config.format`, depending on the
/// backend; `JsonObject` is the weaker "any JSON" mode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    Text,
    JsonObject,
    JsonSchema {
        name: String,
        schema: serde_json::Value,
        #[serde(default)]
        strict: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ModelMessage>,
    /// `None` leaves sampling to the provider default. Some newer cloud
    /// models reject an explicit temperature outright, so the backend decides
    /// per model whether a value is sent at all.
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default)]
    pub thinking: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    #[serde(default)]
    pub tool_policy: ToolPolicy,
}

impl Default for ChatRequest {
    fn default() -> Self {
        Self {
            model: String::new(),
            messages: vec![],
            temperature: None,
            max_tokens: default_max_tokens(),
            thinking: false,
            top_p: None,
            stop: vec![],
            response_format: None,
            tools: vec![],
            tool_policy: ToolPolicy::default(),
        }
    }
}

impl ChatRequest {
    pub fn validate(&self) -> crate::Result<()> {
        if self.model.trim().is_empty() || self.model.len() > 300 {
            return Err(crate::Error::Invalid("choose a valid model".into()));
        }
        if self.messages.is_empty() || self.messages.len() > 200 {
            return Err(crate::Error::Invalid(
                "provide between 1 and 200 messages".into(),
            ));
        }
        if let Some(temperature) = self.temperature {
            if !(0.0..=2.0).contains(&temperature) {
                return Err(crate::Error::Invalid(
                    "temperature must be between 0 and 2".into(),
                ));
            }
        }
        if let Some(top_p) = self.top_p {
            if !(0.0..=1.0).contains(&top_p) {
                return Err(crate::Error::Invalid(
                    "top_p must be between 0 and 1".into(),
                ));
            }
        }
        if !(1..=131_072).contains(&self.max_tokens) {
            return Err(crate::Error::Invalid(
                "max tokens must be between 1 and 131072".into(),
            ));
        }
        if self.stop.len() > 8 {
            return Err(crate::Error::Invalid(
                "a request may name at most 8 stop sequences".into(),
            ));
        }
        if self.tools.len() > 128 {
            return Err(crate::Error::Invalid(
                "a request may expose at most 128 tools".into(),
            ));
        }
        Ok(())
    }

    /// The system instructions, if the transcript carries any. Backends that
    /// take the system prompt out of band (Anthropic) use this and skip the
    /// system rows when encoding messages.
    pub fn system_text(&self) -> Option<String> {
        let text = self
            .messages
            .iter()
            .filter(|message| message.role == Role::System)
            .flat_map(|message| message.content.iter())
            .filter_map(|part| match part {
                ModelContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        (!text.trim().is_empty()).then_some(text)
    }
}

fn default_max_tokens() -> u32 {
    2048
}

/// Coarse model family, mostly meaningful for local hosts that advertise it
/// (oMLX `model_type`). Cloud catalogues report `Llm`/`Vlm` from their own
/// capability tables.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ModelKind {
    Llm,
    Vlm,
    Embedding,
    Helper,
    #[default]
    Unknown,
}

/// Where a model row came from: the static catalogue compiled into the crate,
/// a live discovery call, or both (discovery merged over the catalogue row).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ModelSource {
    Catalogue,
    #[default]
    Discovered,
    Both,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelSummary {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default)]
    pub kind: ModelKind,
    #[serde(default)]
    pub loaded: bool,
    #[serde(default)]
    pub is_loading: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default)]
    pub modalities: Vec<String>,
    /// The provider's own default (oMLX `/health.default_model`, catalogue
    /// `is_default`), used when the caller names no model.
    #[serde(default)]
    pub is_default: bool,
    /// Helper pseudo-models (oMLX `MarkItDown`) are listed for transparency
    /// but never selectable as a chat model.
    #[serde(default)]
    pub is_helper: bool,
    #[serde(default)]
    pub vision: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<bool>,
    #[serde(default)]
    pub source: ModelSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_repo: Option<String>,
}

impl ModelSummary {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            ..Default::default()
        }
    }

    pub fn display_name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.id)
    }

    pub fn supports_vision(&self) -> bool {
        self.vision || self.modalities.iter().any(|modality| modality == "image")
    }
}

/// Normalise a backend usage object to the OpenAI key set so run provenance
/// stays uniform across providers: `prompt_tokens`, `completion_tokens`,
/// `total_tokens`. Unknown keys are preserved.
pub fn normalize_usage(value: serde_json::Value) -> serde_json::Value {
    let serde_json::Value::Object(mut object) = value else {
        return value;
    };
    let read = |object: &serde_json::Map<String, serde_json::Value>, key: &str| {
        object.get(key).and_then(serde_json::Value::as_u64)
    };
    let prompt = read(&object, "prompt_tokens").or_else(|| read(&object, "input_tokens"));
    let completion = read(&object, "completion_tokens").or_else(|| read(&object, "output_tokens"));
    if let Some(prompt) = prompt {
        object.insert("prompt_tokens".into(), prompt.into());
    }
    if let Some(completion) = completion {
        object.insert("completion_tokens".into(), completion.into());
    }
    if let (Some(prompt), Some(completion)) = (prompt, completion) {
        object
            .entry("total_tokens")
            .or_insert_with(|| (prompt + completion).into());
    }
    serde_json::Value::Object(object)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    Token {
        text: String,
    },
    Reasoning {
        text: String,
    },
    ToolCallDelta {
        index: u32,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    },
    Usage {
        usage: serde_json::Value,
    },
    Error {
        error: String,
    },
    Done {
        finish_reason: Option<String>,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CompletionRecord {
    pub content: String,
    pub reasoning: String,
    pub usage: Option<serde_json::Value>,
    pub finish_reason: Option<String>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolInvocationRecord {
    pub tool: String,
    pub provider: String,
    pub arguments: BTreeMap<String, impress_item::Value>,
    pub result: Option<BTreeMap<String, impress_item::Value>>,
    pub result_summary: Option<String>,
    pub error: Option<String>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredAttachment {
    pub item_id: Uuid,
    pub mime_type: String,
    pub sha256: String,
    pub file_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_policy_is_stable_and_deduplicated() {
        let policy = ToolPolicy {
            enabled: vec!["scix".into(), "web".into(), "scix".into(), "".into()],
        }
        .normalized();
        assert_eq!(policy.enabled, ["scix", "web"]);
        assert!(policy.allows("scix"));
        assert!(!policy.allows("impress-mcp"));
    }

    #[test]
    fn chat_request_decodes_legacy_payloads_without_the_new_fields() {
        let legacy = serde_json::json!({
            "model": "m",
            "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hi" }] }],
            "temperature": 0.2,
            "max_tokens": 64
        });
        let request: ChatRequest = serde_json::from_value(legacy).unwrap();
        assert_eq!(request.temperature, Some(0.2));
        assert_eq!(request.top_p, None);
        assert!(request.stop.is_empty());
        assert_eq!(request.response_format, None);
        request.validate().unwrap();

        let bare: ChatRequest = serde_json::from_value(serde_json::json!({
            "model": "m",
            "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hi" }] }]
        }))
        .unwrap();
        assert_eq!(bare.temperature, None);
        assert_eq!(bare.max_tokens, 2048);
    }

    #[test]
    fn chat_request_validates_the_new_sampling_fields() {
        let mut request = ChatRequest {
            model: "m".into(),
            messages: vec![ModelMessage::text(Role::User, "hi")],
            ..Default::default()
        };
        request.validate().unwrap();
        request.top_p = Some(1.5);
        assert!(request.validate().is_err());
        request.top_p = Some(0.9);
        request.stop = vec!["x".into(); 9];
        assert!(request.validate().is_err());
    }

    #[test]
    fn system_text_joins_only_system_rows() {
        let request = ChatRequest {
            model: "m".into(),
            messages: vec![
                ModelMessage::text(Role::System, "Be terse."),
                ModelMessage::text(Role::User, "hi"),
                ModelMessage::text(Role::System, "Cite sources."),
            ],
            ..Default::default()
        };
        assert_eq!(
            request.system_text().as_deref(),
            Some("Be terse.\n\nCite sources.")
        );
        let none = ChatRequest {
            model: "m".into(),
            messages: vec![ModelMessage::text(Role::User, "hi")],
            ..Default::default()
        };
        assert_eq!(none.system_text(), None);
    }

    #[test]
    fn model_summary_decodes_legacy_rows_and_normalises_usage() {
        let legacy: ModelSummary = serde_json::from_value(serde_json::json!({
            "id": "old-model",
            "loaded": true,
            "max_context_window": 4096,
            "modalities": ["text", "image"]
        }))
        .unwrap();
        assert_eq!(legacy.kind, ModelKind::Unknown);
        assert!(!legacy.is_helper);
        assert!(legacy.supports_vision());
        assert_eq!(legacy.display_name(), "old-model");

        let usage = normalize_usage(serde_json::json!({ "input_tokens": 4, "output_tokens": 6 }));
        assert_eq!(usage["prompt_tokens"], 4);
        assert_eq!(usage["completion_tokens"], 6);
        assert_eq!(usage["total_tokens"], 10);
        let passthrough = normalize_usage(serde_json::json!({ "prompt_tokens": 1 }));
        assert_eq!(passthrough["prompt_tokens"], 1);
        assert!(passthrough.get("total_tokens").is_none());
    }

    #[test]
    fn multimodal_message_serializes_as_openai_content_parts() {
        let message = ModelMessage {
            role: Role::User,
            content: vec![
                ModelContentPart::Text {
                    text: "inspect".into(),
                },
                ModelContentPart::ImageUrl {
                    image_url: ImageUrl {
                        url: "data:image/png;base64,AAAA".into(),
                        detail: Some("high".into()),
                    },
                },
            ],
            name: None,
            tool_call_id: None,
            tool_calls: vec![],
        };
        let json = serde_json::to_value(message).unwrap();
        assert_eq!(json["content"][1]["type"], "image_url");
    }
}
