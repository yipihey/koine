use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_stream::wrappers::ReceiverStream;

use crate::catalogue::{self, ProviderDescriptor};
use crate::tools::{PendingToolCall, ToolCallAccumulator};
use crate::types::{ChatRequest, ModelSummary, StreamEvent};
use crate::{Error, Result};

/// Token/tool/usage events for one model call, in arrival order.
pub type EventStream = ReceiverStream<Result<StreamEvent>>;

/// Transport-neutral inference boundary used by the durable task executor,
/// the HTTP adapter, the generated service verbs and the UniFFI bridge.
///
/// oMLX was the first implementation; the OpenAI-compatible dialects and the
/// Anthropic client are the others. The conversation graph, provenance, tool
/// loop and scheduler depend only on this port, so a host can install
/// another backend without creating a second execution path or weakening
/// the recorded run identity.
#[async_trait]
pub trait InferenceProvider: Send + Sync {
    /// Stable provider family recorded on every agent run (for example
    /// `omlx`).
    fn provider_id(&self) -> &str;

    /// Stable endpoint identity within that family (for example
    /// `local-omlx` or `lab-server`).
    fn endpoint_id(&self) -> &str;

    /// The catalogue entry for this provider, when it is a catalogued one.
    fn descriptor(&self) -> Option<&'static ProviderDescriptor> {
        catalogue::descriptor(self.provider_id())
    }

    async fn models(&self) -> Result<Vec<ModelSummary>>;

    /// Passive reachability probe. The default derives it from `models()`;
    /// backends with a cheaper or richer endpoint override it.
    async fn health(&self) -> Result<ProviderHealth> {
        Ok(ProviderHealth::from_models(self.models().await?))
    }

    async fn stream(&self, request: ChatRequest) -> Result<EventStream>;

    /// Non-streaming completion, folded from `stream()` so every backend
    /// shares one accumulation rule for text, reasoning, tool calls and
    /// usage.
    async fn complete(&self, request: ChatRequest) -> Result<Completion> {
        collect_stream(self.stream(request).await?).await
    }
}

/// One finished model call.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Completion {
    pub content: String,
    pub reasoning: String,
    pub tool_calls: Vec<PendingToolCall>,
    pub usage: Option<Value>,
    pub finish_reason: Option<String>,
}

/// Fold a stream into a [`Completion`]. A `StreamEvent::Error` becomes an
/// `Error::Provider` so callers see one failure path.
pub async fn collect_stream(mut stream: EventStream) -> Result<Completion> {
    let mut completion = Completion::default();
    let mut calls = ToolCallAccumulator::default();
    while let Some(event) = stream.next().await {
        match event? {
            StreamEvent::Token { text } => completion.content.push_str(&text),
            StreamEvent::Reasoning { text } => completion.reasoning.push_str(&text),
            StreamEvent::ToolCallDelta {
                index,
                id,
                name,
                arguments,
            } => calls.push(index, id, name, arguments),
            StreamEvent::Usage { usage } => completion.usage = Some(usage),
            StreamEvent::Done { finish_reason } => {
                if finish_reason.is_some() {
                    completion.finish_reason = finish_reason;
                }
            }
            StreamEvent::Error { error } => {
                return Err(Error::Provider {
                    provider: "stream".into(),
                    status: None,
                    message: error,
                })
            }
        }
    }
    completion.tool_calls = calls.finish()?;
    Ok(completion)
}

/// Reachability of a provider as observed from this device right now. It is
/// never persisted: two devices can legitimately disagree at the same moment.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HealthState {
    Ready,
    /// Reachable but advertising no usable models.
    Empty,
    NeedsCredentials {
        fields: Vec<String>,
    },
    NeedsEndpoint,
    Unreachable,
    /// Executed by the host GUI; Rust can only report whether the host said
    /// it is available.
    Foreign {
        available: bool,
    },
}

impl HealthState {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready | Self::Foreign { available: true })
    }

    /// Stable lowercase label for FFI records and service DTOs.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Empty => "empty",
            Self::NeedsCredentials { .. } => "needs_credentials",
            Self::NeedsEndpoint => "needs_endpoint",
            Self::Unreachable => "unreachable",
            Self::Foreign { available: true } => "foreign",
            Self::Foreign { available: false } => "foreign_unavailable",
        }
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderHealth {
    pub state: HealthState,
    pub checked_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default)]
    pub model_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loaded_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_in_use_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_ceiling_bytes: Option<u64>,
    pub detail: String,
}

impl ProviderHealth {
    pub fn now_ms() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }

    pub fn new(state: HealthState, detail: impl Into<String>) -> Self {
        Self {
            state,
            checked_at_ms: Self::now_ms(),
            endpoint: None,
            default_model: None,
            model_count: 0,
            loaded_count: None,
            server_version: None,
            memory_in_use_bytes: None,
            memory_ceiling_bytes: None,
            detail: detail.into(),
        }
    }

    pub fn unreachable(detail: impl Into<String>) -> Self {
        Self::new(HealthState::Unreachable, detail)
    }

    /// Health derived from a model listing: helper pseudo-models do not count
    /// as usable, so a host that only advertises them reports `Empty`.
    pub fn from_models(models: Vec<ModelSummary>) -> Self {
        let usable = models.iter().filter(|model| !model.is_helper).count() as u32;
        let loaded = models
            .iter()
            .filter(|model| !model.is_helper && model.loaded)
            .count() as u32;
        let default_model = models
            .iter()
            .find(|model| model.is_default && !model.is_helper)
            .map(|model| model.id.clone());
        let state = if usable == 0 {
            HealthState::Empty
        } else {
            HealthState::Ready
        };
        let detail = if usable == 0 {
            "Host reachable; no models reported".to_string()
        } else {
            let noun = if usable == 1 { "model" } else { "models" };
            format!("Host reachable; {usable} {noun} available, {loaded} loaded")
        };
        Self {
            model_count: usable,
            loaded_count: Some(loaded),
            default_model,
            ..Self::new(state, detail)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn scripted(events: Vec<StreamEvent>) -> EventStream {
        let (sender, receiver) = mpsc::channel(events.len().max(1));
        for event in events {
            sender.try_send(Ok(event)).unwrap();
        }
        ReceiverStream::new(receiver)
    }

    #[tokio::test]
    async fn collect_stream_folds_text_reasoning_tools_and_usage() {
        let completion = collect_stream(scripted(vec![
            StreamEvent::Reasoning {
                text: "think".into(),
            },
            StreamEvent::Token { text: "Hel".into() },
            StreamEvent::Token { text: "lo".into() },
            StreamEvent::ToolCallDelta {
                index: 0,
                id: Some("call-1".into()),
                name: Some("scix".into()),
                arguments: r#"{"q":"#.into(),
            },
            StreamEvent::ToolCallDelta {
                index: 0,
                id: None,
                name: None,
                arguments: r#""stars"}"#.into(),
            },
            StreamEvent::Usage {
                usage: serde_json::json!({ "prompt_tokens": 2, "completion_tokens": 3 }),
            },
            StreamEvent::Done {
                finish_reason: Some("tool_calls".into()),
            },
        ]))
        .await
        .unwrap();
        assert_eq!(completion.content, "Hello");
        assert_eq!(completion.reasoning, "think");
        assert_eq!(completion.tool_calls.len(), 1);
        assert_eq!(completion.tool_calls[0].arguments, r#"{"q":"stars"}"#);
        assert_eq!(completion.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(completion.usage.unwrap()["prompt_tokens"], 2);
    }

    #[tokio::test]
    async fn collect_stream_surfaces_stream_errors() {
        let error = collect_stream(scripted(vec![StreamEvent::Error {
            error: "context length exceeded".into(),
        }]))
        .await
        .unwrap_err();
        assert!(error.to_string().contains("context length exceeded"));
    }

    #[test]
    fn health_from_models_ignores_helpers_and_reports_the_default() {
        let helper = ModelSummary {
            is_helper: true,
            ..ModelSummary::new("MarkItDown")
        };
        assert_eq!(
            ProviderHealth::from_models(vec![helper.clone()]).state,
            HealthState::Empty
        );
        let default = ModelSummary {
            is_default: true,
            loaded: true,
            ..ModelSummary::new("llama")
        };
        let health = ProviderHealth::from_models(vec![helper, default, ModelSummary::new("qwen")]);
        assert_eq!(health.state, HealthState::Ready);
        assert_eq!(health.model_count, 2);
        assert_eq!(health.loaded_count, Some(1));
        assert_eq!(health.default_model.as_deref(), Some("llama"));
        assert_eq!(
            HealthState::Foreign { available: false }.label(),
            "foreign_unavailable"
        );
    }
}
