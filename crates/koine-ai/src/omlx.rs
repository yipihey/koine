//! oMLX compatibility surface.
//!
//! oMLX speaks the OpenAI wire format, so the real client is
//! [`OpenAiCompatibleClient`] with [`Dialect::Omlx`]. This wrapper keeps the
//! constructor and method names the daemons, the service and the FFI bridge
//! were written against.

use crate::catalogue::Dialect;
pub use crate::catalogue::OMLX_DEFAULT_URL as DEFAULT_URL;
pub use crate::provider::EventStream;
use crate::provider::{InferenceProvider, ProviderHealth};
use crate::providers::OpenAiCompatibleClient;
use crate::secret::Secret;
use crate::types::{ChatRequest, ModelSummary};
use crate::Result;

/// OpenAI-compatible client tuned for oMLX's discovery and streaming surface.
#[derive(Clone)]
pub struct OmlxClient {
    inner: OpenAiCompatibleClient,
}

impl OmlxClient {
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Result<Self> {
        Self::with_endpoint_id(base_url, api_key, "omlx")
    }

    pub fn with_endpoint_id(
        base_url: impl Into<String>,
        api_key: Option<String>,
        endpoint_id: impl Into<String>,
    ) -> Result<Self> {
        let api_key = api_key
            .filter(|key| !key.trim().is_empty())
            .map(Secret::new);
        Ok(Self {
            inner: OpenAiCompatibleClient::new(Dialect::Omlx, base_url, api_key, endpoint_id)?,
        })
    }

    pub fn endpoint_id(&self) -> &str {
        self.inner.endpoint_id()
    }

    /// Canonical endpoint root used for requests and provenance. Accepting a
    /// configured OpenAI-compatible `/v1` URL here avoids the easy-to-miss
    /// `/v1/v1/models` failure when device settings are shared with clients
    /// that expect the versioned base URL.
    pub fn base_url(&self) -> &str {
        self.inner.origin()
    }

    pub async fn models(&self) -> Result<Vec<ModelSummary>> {
        self.inner.models().await
    }

    pub async fn health(&self) -> Result<ProviderHealth> {
        self.inner.health().await
    }

    pub async fn stream(&self, request: ChatRequest) -> Result<EventStream> {
        self.inner.stream(request).await
    }
}

#[async_trait::async_trait]
impl InferenceProvider for OmlxClient {
    fn provider_id(&self) -> &str {
        self.inner.provider_id()
    }

    fn endpoint_id(&self) -> &str {
        self.inner.endpoint_id()
    }

    async fn models(&self) -> Result<Vec<ModelSummary>> {
        self.inner.models().await
    }

    async fn health(&self) -> Result<ProviderHealth> {
        self.inner.health().await
    }

    async fn stream(&self, request: ChatRequest) -> Result<EventStream> {
        self.inner.stream(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_endpoint_roots_and_openai_versioned_urls() {
        let root = OmlxClient::new("http://127.0.0.1:8000/", None).unwrap();
        assert_eq!(root.base_url(), "http://127.0.0.1:8000");
        assert_eq!(root.provider_id(), "omlx");
        assert_eq!(root.endpoint_id(), "omlx");

        let versioned = OmlxClient::new(" http://laptop.tailnet.ts.net:8000/v1/ ", None).unwrap();
        assert_eq!(versioned.base_url(), "http://laptop.tailnet.ts.net:8000");
        assert!(OmlxClient::new("", None).is_err());
    }

    /// Talks to the oMLX server on this machine. Run with
    /// `cargo test -p impress-ai -- --ignored live_omlx` when it is up.
    #[tokio::test]
    #[ignore]
    async fn live_omlx_discovery_and_health() {
        let client = OmlxClient::new(DEFAULT_URL, None).unwrap();
        let health = client.health().await.unwrap();
        eprintln!("health: {health:?}");
        assert!(health.state.is_ready(), "{}", health.detail);
        let models = client.models().await.unwrap();
        for model in &models {
            eprintln!(
                "{:<48} {:<28} kind={:?} loaded={} ctx={:?} out={:?} default={} helper={}",
                model.id,
                model.display_name(),
                model.kind,
                model.loaded,
                model.max_context_window,
                model.max_output_tokens,
                model.is_default,
                model.is_helper
            );
        }
        assert!(models.iter().any(|model| !model.is_helper));
        assert!(models
            .iter()
            .filter(|model| model.id == "MarkItDown")
            .all(|model| model.is_helper));
    }

    #[tokio::test]
    async fn discovers_loaded_models_from_a_versioned_configured_endpoint() {
        let mut server = mockito::Server::new_async().await;
        let listed = server
            .mock("GET", "/v1/models")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"data":[{"id":"text-model","modalities":["text"]},{"id":"vision-model","modalities":["text","image"]}]}"#,
            )
            .create_async()
            .await;
        let status = server
            .mock("GET", "/v1/models/status")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"models":[{"id":"vision-model","loaded":true,"max_context_window":32768}]}"#,
            )
            .create_async()
            .await;

        let client = OmlxClient::new(format!("{}/v1", server.url()), None).unwrap();
        let models = client.models().await.unwrap();

        listed.assert_async().await;
        status.assert_async().await;
        assert_eq!(models[0].id, "vision-model");
        assert!(models[0].loaded);
        assert_eq!(models[0].max_context_window, Some(32_768));
        assert_eq!(models[0].modalities, ["text", "image"]);
        assert_eq!(models[1].id, "text-model");
    }
}
