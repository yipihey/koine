//! Apple Foundation Models as a native `koine-ai` provider (LM-D5).
//!
//! Apple's `LanguageModelSession` runs the tool loop itself, so the provider
//! reports `ToolLoop::Provider` and Koine's tools are called back from Swift.
//! The Swift bridge (`swift/`) is built by `build.rs` and linked statically;
//! on other platforms this crate is a stub whose [`install`] does nothing and
//! whose [`available`] is false. The request and event JSON is built and
//! parsed in [`wire`], which is portable and tested everywhere.

use std::sync::Arc;

use async_trait::async_trait;
use koine_ai::catalogue::{self, Capabilities, APPLE_ON_DEVICE_ID};
use koine_ai::{
    AiRegistry, ChatRequest, Error, EventStream, HealthState, InferenceProvider, LoopError,
    LoopObserver, LoopOutcome, ModelSummary, NoObserver, ProviderHealth, Result, StreamEvent,
    ToolLoop, ToolRunner,
};
use tokio_stream::wrappers::ReceiverStream;

pub mod wire;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;

    pub fn availability() -> wire::Availability {
        wire::Availability::unavailable("Apple Foundation Models needs macOS")
    }

    pub async fn run(
        _request: &ChatRequest,
        _tools: Option<&dyn ToolRunner>,
        _observer: &dyn LoopObserver,
    ) -> std::result::Result<LoopOutcome, LoopError> {
        Err(LoopError::Transport(Error::Unreachable {
            provider: APPLE_ON_DEVICE_ID.into(),
            message: "Apple Foundation Models needs macOS".into(),
        }))
    }
}

/// Whether the on-device model can run turns right now.
pub fn available() -> bool {
    platform::availability().available
}

/// Register the provider with `registry` when the model is available, and
/// tell the registry whether it is. Returns the availability.
pub fn install(registry: &AiRegistry) -> bool {
    let ready = available();
    if ready {
        registry.register_native(APPLE_ON_DEVICE_ID, Arc::new(AppleFoundationModels::new()));
    }
    registry.set_foreign_available(APPLE_ON_DEVICE_ID, ready);
    ready
}

#[derive(Debug, Clone, Default)]
pub struct AppleFoundationModels;

impl AppleFoundationModels {
    pub fn new() -> Self {
        Self
    }
}

fn into_error(error: LoopError) -> Error {
    match error {
        LoopError::Transport(error) | LoopError::Invalid(error) | LoopError::Tool(error) => error,
        LoopError::Stream(message) | LoopError::Exhausted(message) => Error::Provider {
            provider: APPLE_ON_DEVICE_ID.into(),
            status: None,
            message,
        },
    }
}

#[async_trait]
impl InferenceProvider for AppleFoundationModels {
    fn provider_id(&self) -> &str {
        APPLE_ON_DEVICE_ID
    }

    fn endpoint_id(&self) -> &str {
        "system"
    }

    async fn models(&self) -> Result<Vec<ModelSummary>> {
        let statics = catalogue::descriptor(APPLE_ON_DEVICE_ID)
            .and_then(|d| d.static_models.first().copied());
        Ok(vec![ModelSummary {
            display_name: statics.map(|m| m.display_name.to_string()),
            max_context_window: statics.and_then(|m| m.context_window),
            max_output_tokens: statics.and_then(|m| m.max_output_tokens),
            is_default: true,
            loaded: true,
            tools: Some(true),
            json_schema: Some(true),
            ..ModelSummary::new(APPLE_ON_DEVICE_ID)
        }])
    }

    async fn health(&self) -> Result<ProviderHealth> {
        let availability = platform::availability();
        if !availability.available {
            let reason = availability.reason.unwrap_or_else(|| "unavailable".into());
            return Ok(ProviderHealth::unreachable(reason));
        }
        Ok(ProviderHealth {
            model_count: 1,
            loaded_count: Some(1),
            default_model: Some(APPLE_ON_DEVICE_ID.into()),
            ..ProviderHealth::new(HealthState::Ready, "On-device model available")
        })
    }

    fn tool_loop(&self) -> ToolLoop {
        ToolLoop::Provider
    }

    async fn capabilities(&self, _model: &str) -> Capabilities {
        let availability = platform::availability();
        if !availability.available {
            return Capabilities::default();
        }
        Capabilities {
            streaming: true,
            tools: availability.capabilities.tool_calling,
            vision: availability.capabilities.vision,
            json_schema: availability.capabilities.guided_generation,
            thinking: availability.capabilities.reasoning,
            embeddings: false,
            system_prompt: true,
            structured_with_tools: true,
        }
    }

    /// A tool-free turn run to completion, then replayed as one token and a
    /// `Done`: simple, and enough for title suggestions. The loop's
    /// `previews` use `run_turn`, which streams.
    async fn stream(&self, mut request: ChatRequest) -> Result<EventStream> {
        request.tools.clear();
        let outcome = platform::run(&request, None, &NoObserver)
            .await
            .map_err(into_error)?;
        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        let _ = sender.try_send(Ok(StreamEvent::Token {
            text: outcome.content,
        }));
        let _ = sender.try_send(Ok(StreamEvent::Done {
            finish_reason: outcome.finish_reason,
        }));
        Ok(ReceiverStream::new(receiver))
    }

    async fn run_turn(
        &self,
        request: ChatRequest,
        tools: &dyn ToolRunner,
        observer: &dyn LoopObserver,
    ) -> std::result::Result<LoopOutcome, LoopError> {
        platform::run(&request, Some(tools), observer).await
    }
}
