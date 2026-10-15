//! The static provider catalogue.
//!
//! Every provider the suite can talk to is declared here once: identity,
//! how it is executed, which credential fields it needs, its default
//! endpoint, its capabilities and a static model table used before (or
//! without) live discovery. The catalogue order is also the resolution
//! order used when no provider is selected — local hosts first.
//!
//! Secrets are declared as *fields*, never values. Endpoints are defaults;
//! per-device overrides live in the preferences file.

use serde::{Deserialize, Serialize};

use crate::types::{ModelKind, ModelSource, ModelSummary};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderCategory {
    Local,
    Cloud,
    Aggregator,
    /// Executed by the host platform (Apple on-device models).
    Native,
}

impl ProviderCategory {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Cloud => "cloud",
            Self::Aggregator => "aggregator",
            Self::Native => "native",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionHost {
    /// The Rust runtime owns the wire protocol.
    Rust,
    /// The GUI layer executes requests (a platform framework); Rust lists,
    /// selects and records the provider but cannot call it.
    Foreign,
}

impl ExecutionHost {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Foreign => "foreign",
        }
    }
}

/// Which OpenAI-compatible flavour a host speaks. The wire format is shared;
/// the dialect decides paths, headers, thinking spelling, discovery shape and
/// which sampling parameters a model accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dialect {
    Omlx,
    Generic,
    OpenAi,
    OpenRouter,
    Google,
    Ollama,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    OpenAiCompatible(Dialect),
    Anthropic,
    Foreign,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredentialField {
    pub id: &'static str,
    pub label: &'static str,
    pub secret: bool,
    pub optional: bool,
    pub placeholder: Option<&'static str>,
}

const API_KEY: CredentialField = CredentialField {
    id: "apiKey",
    label: "API Key",
    secret: true,
    optional: false,
    placeholder: None,
};

const OPTIONAL_BEARER: CredentialField = CredentialField {
    id: "apiKey",
    label: "Bearer token",
    secret: true,
    optional: true,
    placeholder: Some("Optional unless server authentication is enabled"),
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub streaming: bool,
    pub tools: bool,
    pub vision: bool,
    pub json_schema: bool,
    pub thinking: bool,
    pub embeddings: bool,
    pub system_prompt: bool,
}

impl Capabilities {
    pub const CHAT: Self = Self {
        streaming: true,
        tools: false,
        vision: false,
        json_schema: false,
        thinking: false,
        embeddings: false,
        system_prompt: true,
    };

    pub const FULL: Self = Self {
        streaming: true,
        tools: true,
        vision: true,
        json_schema: true,
        thinking: false,
        embeddings: false,
        system_prompt: true,
    };

    pub const FULL_THINKING: Self = Self {
        thinking: true,
        ..Self::FULL
    };

    pub const LOCAL_SERVER: Self = Self {
        streaming: true,
        tools: true,
        vision: true,
        json_schema: true,
        thinking: true,
        embeddings: false,
        system_prompt: true,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaticModel {
    pub id: &'static str,
    pub display_name: &'static str,
    pub description: &'static str,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub is_default: bool,
    pub capabilities: Capabilities,
    /// Whether the model accepts `temperature`/`top_p`. Reasoning models
    /// (OpenAI o-series, the newest Claude generations) reject them.
    pub sampling_params: bool,
}

impl StaticModel {
    pub fn summary(&self) -> ModelSummary {
        ModelSummary {
            id: self.id.into(),
            display_name: Some(self.display_name.into()),
            kind: if self.capabilities.vision {
                ModelKind::Vlm
            } else {
                ModelKind::Llm
            },
            max_context_window: self.context_window,
            max_output_tokens: self.max_output_tokens,
            modalities: if self.capabilities.vision {
                vec!["text".into(), "image".into()]
            } else {
                vec!["text".into()]
            },
            is_default: self.is_default,
            vision: self.capabilities.vision,
            tools: Some(self.capabilities.tools),
            thinking: Some(self.capabilities.thinking),
            json_schema: Some(self.capabilities.json_schema),
            source: ModelSource::Catalogue,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discovery {
    /// The static table is the whole inventory.
    None,
    /// The host advertises its models; discovery merges over the table.
    Api,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderDescriptor {
    pub id: &'static str,
    pub display_name: &'static str,
    pub description: &'static str,
    pub category: ProviderCategory,
    pub host: ExecutionHost,
    pub transport: Transport,
    /// Secret fields only; endpoints are preferences, not credentials.
    pub credential_fields: &'static [CredentialField],
    pub default_endpoint: Option<&'static str>,
    pub endpoint_editable: bool,
    pub registration_url: Option<&'static str>,
    pub capabilities: Capabilities,
    pub static_models: &'static [StaticModel],
    pub discovery: Discovery,
    /// The suite may start this host itself (oMLX.app on macOS).
    pub can_auto_start: bool,
    /// SF Symbol name the GUI uses for the provider row.
    pub icon: &'static str,
}

impl ProviderDescriptor {
    pub fn static_summaries(&self) -> Vec<ModelSummary> {
        self.static_models
            .iter()
            .map(StaticModel::summary)
            .collect()
    }

    pub fn static_model(&self, id: &str) -> Option<&'static StaticModel> {
        self.static_models.iter().find(|model| model.id == id)
    }

    /// Whether `temperature`/`top_p` may be sent for `model`. Unknown models
    /// default to yes; the catalogue only records the exceptions.
    pub fn sends_sampling_params(&self, model: &str) -> bool {
        self.static_model(model)
            .map(|model| model.sampling_params)
            .unwrap_or(true)
    }

    pub fn required_credential_fields(&self) -> impl Iterator<Item = &'static CredentialField> {
        self.credential_fields
            .iter()
            .filter(|field| !field.optional)
    }
}

pub const OMLX_ID: &str = "omlx";
pub const OLLAMA_ID: &str = "ollama";
pub const OPENAI_COMPATIBLE_ID: &str = "openai-compatible";
pub const APPLE_ON_DEVICE_ID: &str = "apple-on-device";
pub const ANTHROPIC_ID: &str = "anthropic";
pub const OPENAI_ID: &str = "openai";
pub const GOOGLE_ID: &str = "google";
pub const OPENROUTER_ID: &str = "openrouter";

pub const OMLX_DEFAULT_URL: &str = "http://127.0.0.1:8000";
pub const OMLX_BUNDLE_ID: &str = "app.omlx";
pub const OMLX_PORT: u16 = 8000;
pub const OLLAMA_DEFAULT_URL: &str = "http://localhost:11434";
pub const ANTHROPIC_DEFAULT_URL: &str = "https://api.anthropic.com";
pub const OPENAI_DEFAULT_URL: &str = "https://api.openai.com";
pub const GOOGLE_DEFAULT_URL: &str = "https://generativelanguage.googleapis.com";
pub const OPENROUTER_DEFAULT_URL: &str = "https://openrouter.ai";

const OLLAMA_MODELS: &[StaticModel] = &[
    StaticModel {
        id: "llama3.2",
        display_name: "Llama 3.2",
        description: "Meta's latest open model",
        context_window: Some(128_000),
        max_output_tokens: None,
        is_default: true,
        capabilities: Capabilities::CHAT,
        sampling_params: true,
    },
    StaticModel {
        id: "qwen2.5",
        display_name: "Qwen 2.5",
        description: "Alibaba's multilingual model",
        context_window: Some(128_000),
        max_output_tokens: None,
        is_default: false,
        capabilities: Capabilities::CHAT,
        sampling_params: true,
    },
    StaticModel {
        id: "mistral",
        display_name: "Mistral",
        description: "Efficient open model",
        context_window: Some(32_000),
        max_output_tokens: None,
        is_default: false,
        capabilities: Capabilities::CHAT,
        sampling_params: true,
    },
    StaticModel {
        id: "codellama",
        display_name: "Code Llama",
        description: "Specialized for code generation",
        context_window: Some(100_000),
        max_output_tokens: None,
        is_default: false,
        capabilities: Capabilities::CHAT,
        sampling_params: true,
    },
];

const APPLE_MODELS: &[StaticModel] = &[StaticModel {
    id: APPLE_ON_DEVICE_ID,
    display_name: "Apple Intelligence",
    description: "On-device Apple foundation model",
    context_window: Some(8_000),
    max_output_tokens: Some(4_000),
    is_default: true,
    capabilities: Capabilities::CHAT,
    sampling_params: true,
}];

const CLAUDE_CURRENT: Capabilities = Capabilities::FULL_THINKING;

const ANTHROPIC_MODELS: &[StaticModel] = &[
    StaticModel {
        id: "claude-opus-5",
        display_name: "Claude Opus 5",
        description: "Most intelligent model for agents and coding",
        context_window: Some(200_000),
        max_output_tokens: Some(128_000),
        is_default: true,
        capabilities: CLAUDE_CURRENT,
        sampling_params: false,
    },
    StaticModel {
        id: "claude-fable-5-1",
        display_name: "Claude Fable 5.1",
        description: "Mythos-class model with additional safety measures",
        context_window: Some(200_000),
        max_output_tokens: Some(128_000),
        is_default: false,
        capabilities: CLAUDE_CURRENT,
        sampling_params: false,
    },
    StaticModel {
        id: "claude-sonnet-5",
        display_name: "Claude Sonnet 5",
        description: "Best combination of speed and intelligence",
        context_window: Some(200_000),
        max_output_tokens: Some(64_000),
        is_default: false,
        capabilities: CLAUDE_CURRENT,
        sampling_params: false,
    },
    StaticModel {
        id: "claude-opus-4-8",
        display_name: "Claude Opus 4.8",
        description: "Previous flagship for agents and coding",
        context_window: Some(200_000),
        max_output_tokens: Some(128_000),
        is_default: false,
        capabilities: CLAUDE_CURRENT,
        sampling_params: false,
    },
    StaticModel {
        id: "claude-sonnet-4-6",
        display_name: "Claude Sonnet 4.6",
        description: "Previous balanced model",
        context_window: Some(200_000),
        max_output_tokens: Some(64_000),
        is_default: false,
        capabilities: CLAUDE_CURRENT,
        sampling_params: true,
    },
    StaticModel {
        id: "claude-haiku-4-5-20251001",
        display_name: "Claude Haiku 4.5",
        description: "Fastest model with near-frontier intelligence",
        context_window: Some(200_000),
        max_output_tokens: Some(64_000),
        is_default: false,
        capabilities: CLAUDE_CURRENT,
        sampling_params: true,
    },
    StaticModel {
        id: "claude-opus-4-5-20251101",
        display_name: "Claude Opus 4.5",
        description: "Legacy flagship model",
        context_window: Some(200_000),
        max_output_tokens: Some(64_000),
        is_default: false,
        capabilities: CLAUDE_CURRENT,
        sampling_params: true,
    },
    StaticModel {
        id: "claude-sonnet-4-5-20250929",
        display_name: "Claude Sonnet 4.5",
        description: "Legacy balanced model",
        context_window: Some(200_000),
        max_output_tokens: Some(64_000),
        is_default: false,
        capabilities: Capabilities::FULL,
        sampling_params: true,
    },
];

const OPENAI_REASONING: Capabilities = Capabilities {
    streaming: true,
    tools: true,
    vision: true,
    json_schema: true,
    thinking: true,
    embeddings: false,
    system_prompt: true,
};

const OPENAI_MODELS: &[StaticModel] = &[
    StaticModel {
        id: "gpt-4o",
        display_name: "GPT-4o",
        description: "Most capable multimodal model",
        context_window: Some(128_000),
        max_output_tokens: Some(16_384),
        is_default: true,
        capabilities: Capabilities::FULL,
        sampling_params: true,
    },
    StaticModel {
        id: "gpt-4o-mini",
        display_name: "GPT-4o Mini",
        description: "Fast and affordable multimodal model",
        context_window: Some(128_000),
        max_output_tokens: Some(16_384),
        is_default: false,
        capabilities: Capabilities::FULL,
        sampling_params: true,
    },
    StaticModel {
        id: "o1",
        display_name: "o1",
        description: "Advanced reasoning model",
        context_window: Some(200_000),
        max_output_tokens: Some(100_000),
        is_default: false,
        capabilities: OPENAI_REASONING,
        sampling_params: false,
    },
    StaticModel {
        id: "o1-mini",
        display_name: "o1 Mini",
        description: "Fast reasoning model",
        context_window: Some(128_000),
        max_output_tokens: Some(65_536),
        is_default: false,
        capabilities: Capabilities {
            tools: false,
            vision: false,
            ..OPENAI_REASONING
        },
        sampling_params: false,
    },
    StaticModel {
        id: "o3-mini",
        display_name: "o3 Mini",
        description: "Latest compact reasoning model",
        context_window: Some(200_000),
        max_output_tokens: Some(100_000),
        is_default: false,
        capabilities: OPENAI_REASONING,
        sampling_params: false,
    },
];

const GOOGLE_MODELS: &[StaticModel] = &[
    StaticModel {
        id: "gemini-2.0-flash",
        display_name: "Gemini 2.0 Flash",
        description: "Fast and versatile multimodal model",
        context_window: Some(1_000_000),
        max_output_tokens: Some(8_192),
        is_default: true,
        capabilities: Capabilities::FULL,
        sampling_params: true,
    },
    StaticModel {
        id: "gemini-2.0-flash-thinking-exp",
        display_name: "Gemini 2.0 Flash Thinking",
        description: "Enhanced reasoning capabilities",
        context_window: Some(1_000_000),
        max_output_tokens: Some(64_000),
        is_default: false,
        capabilities: Capabilities::FULL_THINKING,
        sampling_params: true,
    },
    StaticModel {
        id: "gemini-1.5-pro",
        display_name: "Gemini 1.5 Pro",
        description: "Best for complex reasoning tasks",
        context_window: Some(2_000_000),
        max_output_tokens: Some(8_192),
        is_default: false,
        capabilities: Capabilities::FULL,
        sampling_params: true,
    },
    StaticModel {
        id: "gemini-1.5-flash",
        display_name: "Gemini 1.5 Flash",
        description: "Fast, efficient for high-volume tasks",
        context_window: Some(1_000_000),
        max_output_tokens: Some(8_192),
        is_default: false,
        capabilities: Capabilities::FULL,
        sampling_params: true,
    },
];

const OPENROUTER_MODELS: &[StaticModel] = &[
    StaticModel {
        id: "anthropic/claude-sonnet-4",
        display_name: "Claude Sonnet 4",
        description: "Anthropic's balanced model",
        context_window: Some(200_000),
        max_output_tokens: Some(64_000),
        is_default: true,
        capabilities: Capabilities::FULL,
        sampling_params: true,
    },
    StaticModel {
        id: "openai/gpt-4o",
        display_name: "GPT-4o",
        description: "OpenAI's multimodal flagship",
        context_window: Some(128_000),
        max_output_tokens: Some(16_384),
        is_default: false,
        capabilities: Capabilities::FULL,
        sampling_params: true,
    },
    StaticModel {
        id: "google/gemini-2.0-flash-001",
        display_name: "Gemini 2.0 Flash",
        description: "Google's fast multimodal model",
        context_window: Some(1_000_000),
        max_output_tokens: Some(8_192),
        is_default: false,
        capabilities: Capabilities::FULL,
        sampling_params: true,
    },
    StaticModel {
        id: "meta-llama/llama-3.3-70b-instruct",
        display_name: "Llama 3.3 70B",
        description: "Meta's open-weight model",
        context_window: Some(128_000),
        max_output_tokens: Some(8_192),
        is_default: false,
        capabilities: Capabilities::CHAT,
        sampling_params: true,
    },
    StaticModel {
        id: "deepseek/deepseek-r1",
        display_name: "DeepSeek R1",
        description: "Advanced reasoning model",
        context_window: Some(128_000),
        max_output_tokens: Some(8_192),
        is_default: false,
        capabilities: Capabilities {
            thinking: true,
            ..Capabilities::CHAT
        },
        sampling_params: true,
    },
];

/// Fixed order: this is also the fallback resolution order — local hosts,
/// then the on-device model, then cloud providers.
pub const CATALOGUE: [ProviderDescriptor; 8] = [
    ProviderDescriptor {
        id: OMLX_ID,
        display_name: "oMLX — local models on this Mac",
        description: "MLX models served by oMLX; discovered live, never leave this Mac",
        category: ProviderCategory::Local,
        host: ExecutionHost::Rust,
        transport: Transport::OpenAiCompatible(Dialect::Omlx),
        credential_fields: &[OPTIONAL_BEARER],
        default_endpoint: Some(OMLX_DEFAULT_URL),
        endpoint_editable: true,
        registration_url: None,
        capabilities: Capabilities::LOCAL_SERVER,
        static_models: &[],
        discovery: Discovery::Api,
        can_auto_start: true,
        icon: "cpu",
    },
    ProviderDescriptor {
        id: OLLAMA_ID,
        display_name: "Ollama",
        description: "Local AI models via Ollama",
        category: ProviderCategory::Local,
        host: ExecutionHost::Rust,
        transport: Transport::OpenAiCompatible(Dialect::Ollama),
        credential_fields: &[],
        default_endpoint: Some(OLLAMA_DEFAULT_URL),
        endpoint_editable: true,
        registration_url: Some("https://ollama.ai"),
        capabilities: Capabilities {
            tools: true,
            vision: true,
            json_schema: true,
            ..Capabilities::CHAT
        },
        static_models: OLLAMA_MODELS,
        discovery: Discovery::Api,
        can_auto_start: false,
        icon: "shippingbox",
    },
    ProviderDescriptor {
        id: OPENAI_COMPATIBLE_ID,
        display_name: "Other OpenAI-compatible server",
        description:
            "vLLM, llama.cpp, LM Studio, Groq, Mistral, DeepSeek, xAI or any other /v1 endpoint",
        category: ProviderCategory::Local,
        host: ExecutionHost::Rust,
        transport: Transport::OpenAiCompatible(Dialect::Generic),
        credential_fields: &[OPTIONAL_BEARER],
        default_endpoint: None,
        endpoint_editable: true,
        registration_url: None,
        capabilities: Capabilities::LOCAL_SERVER,
        static_models: &[],
        discovery: Discovery::Api,
        can_auto_start: false,
        icon: "server.rack",
    },
    ProviderDescriptor {
        id: APPLE_ON_DEVICE_ID,
        display_name: "Apple Intelligence (on-device)",
        description: "Private, on-device model — no API key, works offline.",
        category: ProviderCategory::Native,
        host: ExecutionHost::Foreign,
        transport: Transport::Foreign,
        credential_fields: &[],
        default_endpoint: None,
        endpoint_editable: false,
        registration_url: None,
        capabilities: Capabilities::CHAT,
        static_models: APPLE_MODELS,
        discovery: Discovery::None,
        can_auto_start: false,
        icon: "apple.logo",
    },
    ProviderDescriptor {
        id: ANTHROPIC_ID,
        display_name: "Claude (Anthropic)",
        description: "Claude AI models from Anthropic",
        category: ProviderCategory::Cloud,
        host: ExecutionHost::Rust,
        transport: Transport::Anthropic,
        credential_fields: &[API_KEY],
        default_endpoint: Some(ANTHROPIC_DEFAULT_URL),
        endpoint_editable: true,
        registration_url: Some("https://console.anthropic.com/account/keys"),
        capabilities: Capabilities::FULL_THINKING,
        static_models: ANTHROPIC_MODELS,
        discovery: Discovery::Api,
        can_auto_start: false,
        icon: "brain.head.profile",
    },
    ProviderDescriptor {
        id: OPENAI_ID,
        display_name: "OpenAI",
        description: "GPT models from OpenAI",
        category: ProviderCategory::Cloud,
        host: ExecutionHost::Rust,
        transport: Transport::OpenAiCompatible(Dialect::OpenAi),
        credential_fields: &[API_KEY],
        default_endpoint: Some(OPENAI_DEFAULT_URL),
        endpoint_editable: true,
        registration_url: Some("https://platform.openai.com/api-keys"),
        capabilities: Capabilities::FULL_THINKING,
        static_models: OPENAI_MODELS,
        discovery: Discovery::Api,
        can_auto_start: false,
        icon: "sparkle",
    },
    ProviderDescriptor {
        id: GOOGLE_ID,
        display_name: "Google AI",
        description: "Gemini models from Google",
        category: ProviderCategory::Cloud,
        host: ExecutionHost::Rust,
        transport: Transport::OpenAiCompatible(Dialect::Google),
        credential_fields: &[API_KEY],
        default_endpoint: Some(GOOGLE_DEFAULT_URL),
        endpoint_editable: true,
        registration_url: Some("https://aistudio.google.com/apikey"),
        capabilities: Capabilities::FULL_THINKING,
        static_models: GOOGLE_MODELS,
        discovery: Discovery::Api,
        can_auto_start: false,
        icon: "sparkles",
    },
    ProviderDescriptor {
        id: OPENROUTER_ID,
        display_name: "OpenRouter",
        description: "Access multiple AI providers through one API",
        category: ProviderCategory::Aggregator,
        host: ExecutionHost::Rust,
        transport: Transport::OpenAiCompatible(Dialect::OpenRouter),
        credential_fields: &[API_KEY],
        default_endpoint: Some(OPENROUTER_DEFAULT_URL),
        endpoint_editable: true,
        registration_url: Some("https://openrouter.ai/keys"),
        capabilities: Capabilities::FULL_THINKING,
        static_models: OPENROUTER_MODELS,
        discovery: Discovery::Api,
        can_auto_start: false,
        icon: "arrow.triangle.branch",
    },
];

pub fn descriptor(id: &str) -> Option<&'static ProviderDescriptor> {
    CATALOGUE.iter().find(|descriptor| descriptor.id == id)
}

/// Human-readable name for ids that carry a repository prefix or a tag.
/// `mlx-community--Qwen3.5-4B-4bit` → `Qwen3.5 4B 4bit`; `llama3.2:latest`
/// → `llama3.2`; anything else is returned unchanged so curated catalogue
/// names are never mangled.
pub fn friendly_model_name(id: &str) -> String {
    if let Some((_, repo)) = id.split_once("--") {
        let name = repo.replace(['-', '_'], " ");
        let collapsed = name.split_whitespace().collect::<Vec<_>>().join(" ");
        if !collapsed.is_empty() {
            return collapsed;
        }
    }
    id.strip_suffix(":latest").unwrap_or(id).to_string()
}

/// The organisation half of a repository-style id, if any.
pub fn model_organisation(id: &str) -> Option<&str> {
    id.split_once("--")
        .map(|(organisation, _)| organisation)
        .filter(|organisation| !organisation.is_empty())
}

/// Whether `endpoint` is oMLX's conventional loopback address. Only this
/// endpoint is ever auto-started; a custom runtime on another port is never
/// claimed by the suite.
pub fn is_managed_omlx_endpoint(endpoint: &str) -> bool {
    let Ok(url) = url::Url::parse(endpoint.trim()) else {
        return false;
    };
    url.scheme() == "http"
        && url.port_or_known_default() == Some(OMLX_PORT)
        && matches!(
            url.host_str().map(|host| host.trim_matches(['[', ']'])),
            Some("127.0.0.1") | Some("localhost") | Some("::1")
        )
}

/// Merge live discovery over the static table. Discovered rows win on
/// everything they report; catalogue-only rows are kept so a cloud host that
/// omits a model from `/v1/models` still offers it; the union is marked by
/// [`ModelSource`].
pub fn merge_models(
    catalogue: &[ModelSummary],
    discovered: Vec<ModelSummary>,
) -> Vec<ModelSummary> {
    let mut merged: Vec<ModelSummary> = Vec::with_capacity(catalogue.len() + discovered.len());
    let mut seen = std::collections::BTreeSet::new();
    for mut model in discovered {
        seen.insert(model.id.clone());
        if let Some(known) = catalogue.iter().find(|known| known.id == model.id) {
            model.source = ModelSource::Both;
            model.display_name = model
                .display_name
                .take()
                .or_else(|| known.display_name.clone());
            model.max_context_window = model.max_context_window.or(known.max_context_window);
            model.max_output_tokens = model.max_output_tokens.or(known.max_output_tokens);
            model.is_default |= known.is_default;
            model.vision |= known.vision;
            model.tools = model.tools.or(known.tools);
            model.thinking = model.thinking.or(known.thinking);
            model.json_schema = model.json_schema.or(known.json_schema);
            if model.kind == ModelKind::Unknown {
                model.kind = known.kind;
            }
            if model.modalities.is_empty() {
                model.modalities = known.modalities.clone();
            }
        }
        merged.push(model);
    }
    for known in catalogue {
        if !seen.contains(&known.id) {
            merged.push(known.clone());
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_ids_are_unique_and_in_resolution_order() {
        let ids: Vec<_> = CATALOGUE.iter().map(|descriptor| descriptor.id).collect();
        assert_eq!(
            ids,
            [
                "omlx",
                "ollama",
                "openai-compatible",
                "apple-on-device",
                "anthropic",
                "openai",
                "google",
                "openrouter"
            ]
        );
        let unique: std::collections::BTreeSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
    }

    #[test]
    fn every_cloud_provider_needs_a_secret_and_has_one_default() {
        for descriptor in CATALOGUE.iter() {
            if descriptor.category == ProviderCategory::Cloud
                || descriptor.category == ProviderCategory::Aggregator
            {
                assert!(
                    descriptor.required_credential_fields().count() == 1,
                    "{} must require exactly one secret",
                    descriptor.id
                );
                assert!(descriptor.default_endpoint.is_some(), "{}", descriptor.id);
            }
            if !descriptor.static_models.is_empty() {
                assert_eq!(
                    descriptor
                        .static_models
                        .iter()
                        .filter(|model| model.is_default)
                        .count(),
                    1,
                    "{} must have exactly one default model",
                    descriptor.id
                );
            }
            for field in descriptor.credential_fields {
                assert!(
                    field.secret,
                    "{}: endpoints are preferences, not credentials",
                    descriptor.id
                );
            }
        }
        assert_eq!(
            descriptor("apple-on-device").unwrap().host,
            ExecutionHost::Foreign
        );
        assert!(descriptor("nope").is_none());
    }

    #[test]
    fn sampling_params_follow_the_model_table() {
        let anthropic = descriptor(ANTHROPIC_ID).unwrap();
        assert!(!anthropic.sends_sampling_params("claude-opus-5"));
        assert!(anthropic.sends_sampling_params("claude-haiku-4-5-20251001"));
        assert!(anthropic.sends_sampling_params("some-unknown-model"));
        let openai = descriptor(OPENAI_ID).unwrap();
        assert!(!openai.sends_sampling_params("o3-mini"));
        assert!(openai.sends_sampling_params("gpt-4o"));
    }

    #[test]
    fn friendly_names_only_rewrite_repository_ids() {
        assert_eq!(
            friendly_model_name("mlx-community--Qwen3.5-4B-4bit"),
            "Qwen3.5 4B 4bit"
        );
        assert_eq!(
            friendly_model_name("mlx-community--NVIDIA-Nemotron-3.5-Lightning-30B-A3B-4bit"),
            "NVIDIA Nemotron 3.5 Lightning 30B A3B 4bit"
        );
        assert_eq!(friendly_model_name("llama3.2:latest"), "llama3.2");
        assert_eq!(friendly_model_name("gpt-4o"), "gpt-4o");
        assert_eq!(
            friendly_model_name("anthropic/claude-sonnet-4"),
            "anthropic/claude-sonnet-4"
        );
        assert_eq!(
            model_organisation("mlx-community--x"),
            Some("mlx-community")
        );
        assert_eq!(model_organisation("gpt-4o"), None);
    }

    #[test]
    fn managed_omlx_endpoint_is_loopback_8000_only() {
        assert!(is_managed_omlx_endpoint("http://127.0.0.1:8000"));
        assert!(is_managed_omlx_endpoint("http://localhost:8000/v1/"));
        assert!(is_managed_omlx_endpoint("http://[::1]:8000"));
        assert!(!is_managed_omlx_endpoint("http://127.0.0.1:1234/v1"));
        assert!(!is_managed_omlx_endpoint("https://127.0.0.1:8000"));
        assert!(!is_managed_omlx_endpoint(
            "http://laptop.tailnet.ts.net:8000"
        ));
        assert!(!is_managed_omlx_endpoint("not a url"));
    }

    #[test]
    fn merge_prefers_discovery_and_keeps_catalogue_only_rows() {
        let catalogue = descriptor(OPENAI_ID).unwrap().static_summaries();
        let discovered = vec![
            ModelSummary {
                loaded: true,
                ..ModelSummary::new("gpt-4o")
            },
            ModelSummary::new("gpt-5-preview"),
        ];
        let merged = merge_models(&catalogue, discovered);
        let gpt4o = merged.iter().find(|model| model.id == "gpt-4o").unwrap();
        assert_eq!(gpt4o.source, ModelSource::Both);
        assert!(gpt4o.is_default, "catalogue default survives the merge");
        assert_eq!(gpt4o.display_name.as_deref(), Some("GPT-4o"));
        assert_eq!(gpt4o.max_context_window, Some(128_000));
        assert!(gpt4o.loaded);
        assert!(merged.iter().any(|model| model.id == "gpt-5-preview"));
        assert!(merged
            .iter()
            .any(|model| model.id == "o3-mini" && model.source == ModelSource::Catalogue));
    }
}
