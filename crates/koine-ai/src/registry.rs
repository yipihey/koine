//! The provider registry: catalogue + preferences + credentials → clients.
//!
//! Every consumer (UniFFI bridge, service verbs, HTTP adapter, task
//! executors) resolves "which provider and model" through
//! [`AiRegistry::resolve`], so the rule is written once and never silently
//! substitutes a provider the user did not choose.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::catalogue::{
    self, Dialect, Discovery, ExecutionHost, ProviderCategory, ProviderDescriptor, Transport,
    CATALOGUE,
};
use crate::categories;
use crate::credentials::{
    credential_status, CredentialSource, CredentialStatus, InMemoryCredentials, LayeredCredentials,
};
use crate::preferences::{
    is_helper_model, AiPreferences, CategoryAssignment, ModelRef, PreferencesStore,
};
use crate::provider::{Completion, EventStream, HealthState, InferenceProvider, ProviderHealth};
use crate::providers::{AnthropicClient, OpenAiCompatibleClient};
use crate::types::{ChatRequest, ModelSummary};
use crate::{Error, Result};

/// How long a local host's health probe counts as current for readiness.
const HEALTH_FRESH_FOR: Duration = Duration::from_secs(60);
const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// What a caller knows about the target of a request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolveTarget {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub category: Option<String>,
}

impl ResolveTarget {
    pub fn default_target() -> Self {
        Self::default()
    }

    pub fn provider(provider: impl Into<String>) -> Self {
        Self {
            provider: Some(provider.into()),
            ..Self::default()
        }
    }

    pub fn category(category: impl Into<String>) -> Self {
        Self {
            category: Some(category.into()),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionOrigin {
    Explicit,
    Category,
    Selected,
    FirstReady,
}

impl ResolutionOrigin {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Category => "category",
            Self::Selected => "selected",
            Self::FirstReady => "first_ready",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedTarget {
    pub provider: String,
    pub model: String,
    pub endpoint_id: String,
    pub origin: ResolutionOrigin,
}

/// One inference client per (provider, endpoint, credential fingerprint):
/// a changed endpoint or rotated key yields a fresh client, an unchanged
/// one is reused with its connection pool.
type ClientKey = (String, String, String);

pub struct AiRegistry {
    preferences: PreferencesStore,
    credentials: Arc<dyn CredentialSource>,
    memory: Option<Arc<InMemoryCredentials>>,
    clients: Mutex<HashMap<ClientKey, Arc<dyn InferenceProvider>>>,
    health_cache: Mutex<HashMap<String, (ProviderHealth, std::time::Instant)>>,
    models_cache: Mutex<HashMap<String, Vec<ModelSummary>>>,
    foreign_available: RwLock<HashMap<String, bool>>,
}

impl AiRegistry {
    pub fn open(workspace: impl AsRef<Path>, credentials: Arc<dyn CredentialSource>) -> Arc<Self> {
        Arc::new(Self {
            preferences: PreferencesStore::open(workspace),
            credentials,
            memory: None,
            clients: Mutex::new(HashMap::new()),
            health_cache: Mutex::new(HashMap::new()),
            models_cache: Mutex::new(HashMap::new()),
            foreign_available: RwLock::new(HashMap::new()),
        })
    }

    /// Daemons: environment, then the login keychain through `security`.
    pub fn for_daemon(workspace: impl AsRef<Path>) -> Arc<Self> {
        Self::open(workspace, Arc::new(LayeredCredentials::for_daemon()))
    }

    /// GUI processes: environment, then whatever the host pushed from its
    /// keychain into `memory`.
    pub fn for_app(workspace: impl AsRef<Path>, memory: Arc<InMemoryCredentials>) -> Arc<Self> {
        let mut registry = Self::open(
            workspace,
            Arc::new(LayeredCredentials::for_app(memory.clone())),
        );
        Arc::get_mut(&mut registry)
            .expect("freshly created registry is uniquely owned")
            .memory = Some(memory);
        registry
    }

    pub fn preferences(&self) -> &PreferencesStore {
        &self.preferences
    }

    pub fn memory_credentials(&self) -> Option<&Arc<InMemoryCredentials>> {
        self.memory.as_ref()
    }

    pub fn descriptors(&self) -> &'static [ProviderDescriptor] {
        &CATALOGUE
    }

    pub fn descriptor(&self, id: &str) -> Result<&'static ProviderDescriptor> {
        catalogue::descriptor(id)
            .ok_or_else(|| Error::Invalid(format!("unknown AI provider '{id}'")))
    }

    /// Effective endpoint: environment override, then the preferences file,
    /// then the catalogue default.
    pub fn endpoint_for(&self, id: &str) -> Option<String> {
        let variable = format!(
            "IMPRESS_{}_URL",
            id.chars()
                .map(|character| if character.is_ascii_alphanumeric() {
                    character.to_ascii_uppercase()
                } else {
                    '_'
                })
                .collect::<String>()
        );
        if let Ok(value) = std::env::var(variable) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
        if let Ok(preferences) = self.preferences.load() {
            if let Some(endpoint) = preferences.endpoints.get(id) {
                if !endpoint.trim().is_empty() {
                    return Some(endpoint.clone());
                }
            }
        }
        catalogue::descriptor(id)
            .and_then(|descriptor| descriptor.default_endpoint)
            .map(str::to_string)
    }

    pub fn credential_status(&self, id: &str) -> Result<CredentialStatus> {
        Ok(credential_status(
            self.descriptor(id)?,
            self.credentials.as_ref(),
        ))
    }

    pub fn set_foreign_available(&self, id: &str, available: bool) {
        self.foreign_available
            .write()
            .unwrap()
            .insert(id.to_string(), available);
    }

    /// Synchronous readiness from what is known locally: credentials,
    /// endpoint, the host's availability flag, and a recent health probe for
    /// local hosts. Never touches the network.
    pub fn readiness(&self, id: &str) -> HealthState {
        let Some(descriptor) = catalogue::descriptor(id) else {
            return HealthState::Unreachable;
        };
        if descriptor.host == ExecutionHost::Foreign {
            let available = self
                .foreign_available
                .read()
                .unwrap()
                .get(id)
                .copied()
                .unwrap_or(false);
            return HealthState::Foreign { available };
        }
        let missing = credential_status(descriptor, self.credentials.as_ref()).missing_required();
        if !missing.is_empty() {
            return HealthState::NeedsCredentials { fields: missing };
        }
        if self.endpoint_for(id).is_none() {
            return HealthState::NeedsEndpoint;
        }
        if descriptor.category == ProviderCategory::Local {
            return match self.cached_health(id) {
                Some(health) => health.state,
                None => HealthState::Unreachable,
            };
        }
        HealthState::Ready
    }

    fn cached_health(&self, id: &str) -> Option<ProviderHealth> {
        self.health_cache
            .lock()
            .unwrap()
            .get(id)
            .filter(|(_, at)| at.elapsed() < HEALTH_FRESH_FOR)
            .map(|(health, _)| health.clone())
    }

    fn endpoint_identity(descriptor: &ProviderDescriptor, endpoint: &str) -> String {
        if descriptor.id == catalogue::OMLX_ID && catalogue::is_managed_omlx_endpoint(endpoint) {
            return "local-omlx".into();
        }
        if descriptor
            .default_endpoint
            .is_some_and(|default| default.trim_end_matches('/') == endpoint.trim_end_matches('/'))
        {
            return format!("{}-default", descriptor.id);
        }
        url::Url::parse(endpoint)
            .ok()
            .and_then(|url| {
                url.host_str().map(|host| match url.port() {
                    Some(port) => format!("{host}:{port}"),
                    None => host.to_string(),
                })
            })
            .unwrap_or_else(|| format!("{}-custom", descriptor.id))
    }

    /// The client for `id`, built from the descriptor, the effective
    /// endpoint and the current credentials; cached until either changes.
    pub fn provider(&self, id: &str) -> Result<Arc<dyn InferenceProvider>> {
        let descriptor = self.descriptor(id)?;
        if descriptor.host == ExecutionHost::Foreign {
            return Err(Error::ForeignExecutor(id.into()));
        }
        let endpoint = self.endpoint_for(id).ok_or_else(|| Error::NotConfigured {
            provider: id.into(),
            message: "no endpoint configured".into(),
        })?;
        let api_key = self.credentials.get(id, "apiKey");
        let missing = credential_status(descriptor, self.credentials.as_ref()).missing_required();
        if !missing.is_empty() {
            return Err(Error::NotConfigured {
                provider: id.into(),
                message: format!("missing credentials: {}", missing.join(", ")),
            });
        }
        let key: ClientKey = (
            id.into(),
            endpoint.clone(),
            api_key
                .as_ref()
                .map(|secret| secret.fingerprint())
                .unwrap_or_default(),
        );
        if let Some(client) = self.clients.lock().unwrap().get(&key) {
            return Ok(client.clone());
        }
        let endpoint_id = Self::endpoint_identity(descriptor, &endpoint);
        let client: Arc<dyn InferenceProvider> = match descriptor.transport {
            Transport::OpenAiCompatible(dialect) => Arc::new(
                OpenAiCompatibleClient::new(dialect, endpoint, api_key, endpoint_id)?
                    .with_provider_id(descriptor.id),
            ),
            Transport::Anthropic => Arc::new(AnthropicClient::new(
                Some(&endpoint),
                api_key.ok_or_else(|| Error::NotConfigured {
                    provider: id.into(),
                    message: "no API key configured".into(),
                })?,
                endpoint_id,
            )?),
            Transport::Foreign => return Err(Error::ForeignExecutor(id.into())),
        };
        self.clients.lock().unwrap().insert(key, client.clone());
        Ok(client)
    }

    /// The one resolution rule. Synchronous and network-free.
    pub fn resolve(&self, target: &ResolveTarget) -> Result<ResolvedTarget> {
        let preferences = self.preferences.load().map_err(Error::Io)?;
        let category_assignment = target
            .category
            .as_deref()
            .and_then(|category| preferences.task_categories.get(category))
            .filter(|assignment| assignment.enabled)
            .and_then(|assignment| assignment.primary.clone());

        let (provider, origin) = if let Some(explicit) = target.provider.as_deref() {
            self.descriptor(explicit)?;
            (explicit.to_string(), ResolutionOrigin::Explicit)
        } else if let Some(assignment) = &category_assignment {
            self.descriptor(&assignment.provider)?;
            (assignment.provider.clone(), ResolutionOrigin::Category)
        } else if let Some(selected) = &preferences.selected {
            self.descriptor(&selected.provider)?;
            (selected.provider.clone(), ResolutionOrigin::Selected)
        } else {
            let first = CATALOGUE
                .iter()
                .find(|descriptor| self.readiness(descriptor.id).is_ready())
                .ok_or_else(|| Error::NotConfigured {
                    provider: "ai".into(),
                    message: "no AI provider is configured or reachable".into(),
                })?;
            (first.id.to_string(), ResolutionOrigin::FirstReady)
        };

        let same_provider = |reference: &ModelRef| reference.provider == provider;
        let model = target
            .model
            .clone()
            .filter(|model| !model.trim().is_empty())
            .or_else(|| {
                preferences
                    .selected
                    .as_ref()
                    .filter(|selected| same_provider(selected))
                    .and_then(|selected| selected.model.clone())
            })
            .or_else(|| {
                category_assignment
                    .as_ref()
                    .filter(|assignment| same_provider(assignment))
                    .and_then(|assignment| assignment.model.clone())
            })
            .or_else(|| self.default_model(&provider))
            .ok_or_else(|| Error::Invalid(format!("choose a model for {provider}")))?;

        let endpoint_id = catalogue::descriptor(&provider)
            .map(|descriptor| match self.endpoint_for(&provider) {
                Some(endpoint) => Self::endpoint_identity(descriptor, &endpoint),
                None => format!("{}-unconfigured", descriptor.id),
            })
            .unwrap_or_default();
        Ok(ResolvedTarget {
            provider,
            model,
            endpoint_id,
            origin,
        })
    }

    /// Like [`Self::resolve`], but discovers models first for a dynamic
    /// provider whose catalogue has no default, so the model fallback can
    /// use the host's own default.
    pub async fn resolve_with_discovery(&self, target: &ResolveTarget) -> Result<ResolvedTarget> {
        if target.provider.is_none() {
            // `first_ready` can only pick a local host it has heard from.
            self.ensure_local_health().await;
        }
        match self.resolve(target) {
            Ok(resolved) => Ok(resolved),
            Err(Error::Invalid(message)) if message.starts_with("choose a model") => {
                let provider = self
                    .resolve_provider_only(target)
                    .ok_or_else(|| Error::Invalid(message.clone()))?;
                let _ = self.models(Some(&provider)).await?;
                self.resolve(target)
            }
            Err(error) => Err(error),
        }
    }

    fn resolve_provider_only(&self, target: &ResolveTarget) -> Option<String> {
        if let Some(provider) = &target.provider {
            return Some(provider.clone());
        }
        let preferences = self.preferences.load().ok()?;
        target
            .category
            .as_deref()
            .and_then(|category| preferences.task_categories.get(category))
            .filter(|assignment| assignment.enabled)
            .and_then(|assignment| assignment.primary.as_ref())
            .map(|primary| primary.provider.clone())
            .or_else(|| preferences.selected.map(|selected| selected.provider))
            .or_else(|| self.first_ready_provider())
    }

    /// The `first_ready` step of the resolution rule: the first provider in
    /// catalogue order whose readiness is `Ready` (or a foreign provider the
    /// host reported available).
    fn first_ready_provider(&self) -> Option<String> {
        self.descriptors()
            .iter()
            .find(|descriptor| self.readiness(descriptor.id).is_ready())
            .map(|descriptor| descriptor.id.to_string())
    }

    /// Catalogue default, else the host's advertised default from the last
    /// discovery, else the first loaded model, else the first model.
    fn default_model(&self, provider: &str) -> Option<String> {
        let descriptor = catalogue::descriptor(provider)?;
        if let Some(model) = descriptor
            .static_models
            .iter()
            .find(|model| model.is_default)
        {
            return Some(model.id.to_string());
        }
        let cache = self.models_cache.lock().unwrap();
        let discovered = cache.get(provider)?;
        let usable = || discovered.iter().filter(|model| !model.is_helper);
        usable()
            .find(|model| model.is_default)
            .or_else(|| usable().find(|model| model.loaded))
            .or_else(|| usable().next())
            .map(|model| model.id.clone())
    }

    /// Models for `id` (or the resolved default provider): static catalogue
    /// merged with live discovery. Foreign providers report their static
    /// table.
    pub async fn models(&self, id: Option<&str>) -> Result<(String, Vec<ModelSummary>)> {
        if id.is_none() {
            self.ensure_local_health().await;
        }
        let provider = match id {
            Some(id) => {
                self.descriptor(id)?;
                id.to_string()
            }
            None => self
                .resolve_provider_only(&ResolveTarget::default())
                .or_else(|| {
                    CATALOGUE
                        .iter()
                        .find(|descriptor| self.readiness(descriptor.id).is_ready())
                        .map(|descriptor| descriptor.id.to_string())
                })
                .ok_or_else(|| Error::NotConfigured {
                    provider: "ai".into(),
                    message: "no AI provider is configured or reachable".into(),
                })?,
        };
        let descriptor = self.descriptor(&provider)?;
        let catalogue_models = descriptor.static_summaries();
        if descriptor.discovery == Discovery::None || descriptor.host == ExecutionHost::Foreign {
            return Ok((provider, catalogue_models));
        }
        // A provider that cannot be reached yet (no key, no endpoint, host
        // down) still has its static catalogue to offer; only a provider with
        // nothing static (oMLX) turns that into an error.
        let client = match self.provider(&provider) {
            Ok(client) => client,
            Err(_) if !catalogue_models.is_empty() => return Ok((provider, catalogue_models)),
            Err(error) => return Err(error),
        };
        let discovered = match client.models().await {
            Ok(discovered) => discovered,
            Err(_) if !catalogue_models.is_empty() => return Ok((provider, catalogue_models)),
            Err(error) => return Err(error),
        };
        let merged = catalogue::merge_models(&catalogue_models, discovered);
        self.models_cache
            .lock()
            .unwrap()
            .insert(provider.clone(), merged.clone());
        Ok((provider, merged))
    }

    /// Bounded health probe; the result feeds `readiness()` for local hosts.
    pub async fn health(&self, id: &str) -> ProviderHealth {
        let health = self.probe_health(id).await;
        self.health_cache
            .lock()
            .unwrap()
            .insert(id.to_string(), (health.clone(), std::time::Instant::now()));
        health
    }

    async fn probe_health(&self, id: &str) -> ProviderHealth {
        let Some(descriptor) = catalogue::descriptor(id) else {
            return ProviderHealth::unreachable(format!("unknown AI provider '{id}'"));
        };
        match self.readiness(id) {
            HealthState::Foreign { available } => {
                return ProviderHealth::new(
                    HealthState::Foreign { available },
                    if available {
                        "Executed by the host application"
                    } else {
                        "Not available on this device"
                    },
                )
            }
            HealthState::NeedsCredentials { fields } => {
                return ProviderHealth::new(
                    HealthState::NeedsCredentials {
                        fields: fields.clone(),
                    },
                    format!("Missing credentials: {}", fields.join(", ")),
                )
            }
            HealthState::NeedsEndpoint => {
                return ProviderHealth::new(HealthState::NeedsEndpoint, "No endpoint configured")
            }
            _ => {}
        }
        let client = match self.provider(id) {
            Ok(client) => client,
            Err(error) => return ProviderHealth::unreachable(error.to_string()),
        };
        if descriptor.category != ProviderCategory::Local {
            // Cloud hosts: never probe the network passively; credentials
            // present means ready, and a failing request will say otherwise.
            return ProviderHealth {
                endpoint: self.endpoint_for(id),
                ..ProviderHealth::new(HealthState::Ready, "Credentials configured")
            };
        }
        match tokio::time::timeout(HEALTH_PROBE_TIMEOUT, client.health()).await {
            Ok(Ok(mut health)) => {
                if health.endpoint.is_none() {
                    health.endpoint = self.endpoint_for(id);
                }
                health
            }
            Ok(Err(error)) => ProviderHealth {
                endpoint: self.endpoint_for(id),
                ..ProviderHealth::unreachable(error.to_string())
            },
            Err(_) => ProviderHealth {
                endpoint: self.endpoint_for(id),
                ..ProviderHealth::unreachable("health probe timed out")
            },
        }
    }

    pub async fn refresh_health(&self, ids: &[&str]) {
        for id in ids {
            let _ = self.health(id).await;
        }
    }

    /// Probe every Rust-hosted local provider whose health is unknown or
    /// stale, so a fresh process — the CLI, the MCP server, an app at launch
    /// — sees the same readiness a daemon with its refresh loop does. Local
    /// hosts are the only providers whose readiness needs the network;
    /// nothing is probed when the cache is fresh, when credentials or the
    /// endpoint are missing, or for cloud and foreign providers.
    pub async fn ensure_local_health(&self) {
        let stale: Vec<&'static str> = self
            .descriptors()
            .iter()
            .filter(|descriptor| {
                descriptor.category == ProviderCategory::Local
                    && descriptor.host == ExecutionHost::Rust
            })
            .filter(|descriptor| self.cached_health(descriptor.id).is_none())
            .filter(|descriptor| {
                !matches!(
                    self.readiness(descriptor.id),
                    HealthState::NeedsCredentials { .. } | HealthState::NeedsEndpoint
                )
            })
            .map(|descriptor| descriptor.id)
            .collect();
        for id in stale {
            let _ = self.health(id).await;
        }
    }

    /// [`Self::provider_states`] after [`Self::ensure_local_health`]: the
    /// readiness a settings pane or `list-providers` verb should report.
    pub async fn provider_states_probed(&self) -> Vec<ProviderState> {
        self.ensure_local_health().await;
        self.provider_states()
    }

    /// The target a background daemon tier (impel's classify, memory and
    /// throughline executors) should use for `category`, or `None` when the
    /// tier must stay on its deterministic implementation.
    ///
    /// `IMPEL_LLM_PROVIDER` + `IMPEL_LLM_MODEL` (kept for one release; the
    /// provider must be a catalogue id) win; otherwise the category
    /// assignment must be enabled with a primary. Background tiers
    /// deliberately do NOT inherit the interactive `selected` model: a cloud
    /// chat pick must never create daemon spend silently.
    pub fn daemon_target(&self, category: &str) -> Option<ResolveTarget> {
        if let (Ok(provider), Ok(model)) = (
            std::env::var("IMPEL_LLM_PROVIDER"),
            std::env::var("IMPEL_LLM_MODEL"),
        ) {
            if !provider.trim().is_empty() && !model.trim().is_empty() {
                if catalogue::descriptor(provider.trim()).is_some() {
                    return Some(ResolveTarget {
                        provider: Some(provider.trim().to_string()),
                        model: Some(model.trim().to_string()),
                        category: Some(category.to_string()),
                    });
                }
                eprintln!(
                    "impress-ai: IMPEL_LLM_PROVIDER={provider} is not a catalogue provider; ignoring the override"
                );
            }
        }
        let preferences = self.preferences.load().ok()?;
        let assignment = preferences.task_categories.get(category)?;
        if !assignment.enabled || assignment.primary.is_none() {
            return None;
        }
        Some(ResolveTarget::category(category))
    }

    /// `"omlx/mlx-community--Qwen3.5-4B-4bit"` for logs and `model_id()`
    /// labels; falls back to the target's own words when nothing resolves.
    pub fn describe_target(&self, target: &ResolveTarget) -> String {
        match self.resolve(target) {
            Ok(resolved) => format!("{}/{}", resolved.provider, resolved.model),
            Err(_) => target
                .provider
                .clone()
                .or_else(|| target.category.clone())
                .unwrap_or_else(|| "unresolved".into()),
        }
    }

    pub async fn stream(
        &self,
        target: &ResolveTarget,
        mut request: ChatRequest,
    ) -> Result<(ResolvedTarget, EventStream)> {
        let resolved = self.resolve_with_discovery(target).await?;
        request.model = resolved.model.clone();
        let client = self.provider(&resolved.provider)?;
        let stream = client.stream(request).await?;
        Ok((resolved, stream))
    }

    pub async fn complete(
        &self,
        target: &ResolveTarget,
        mut request: ChatRequest,
    ) -> Result<(ResolvedTarget, Completion)> {
        let resolved = self.resolve_with_discovery(target).await?;
        request.model = resolved.model.clone();
        let client = self.provider(&resolved.provider)?;
        let completion = client.complete(request).await?;
        Ok((resolved, completion))
    }

    // ---- preference mutations -------------------------------------------

    pub fn select_model(&self, provider: &str, model: Option<String>) -> Result<AiPreferences> {
        self.descriptor(provider)?;
        let model = model
            .map(|model| model.trim().to_string())
            .filter(|model| !model.is_empty());
        if let Some(model) = &model {
            if is_helper_model(model) {
                return Err(Error::Invalid(format!(
                    "{model} is a helper model and cannot be selected for chat"
                )));
            }
            let cache = self.models_cache.lock().unwrap();
            if let Some(known) = cache.get(provider) {
                if let Some(row) = known.iter().find(|row| &row.id == model) {
                    if row.is_helper {
                        return Err(Error::Invalid(format!(
                            "{model} is a helper model and cannot be selected for chat"
                        )));
                    }
                }
            }
        }
        let display_name = {
            let cache = self.models_cache.lock().unwrap();
            model.as_ref().and_then(|model| {
                cache
                    .get(provider)
                    .and_then(|known| known.iter().find(|row| &row.id == model))
                    .and_then(|row| row.display_name.clone())
            })
        };
        self.preferences
            .update(|preferences| {
                preferences.selected = Some(ModelRef {
                    provider: provider.to_string(),
                    model,
                    display_name,
                });
            })
            .map_err(Error::Io)
    }

    /// Add or remove a model from the set the suite may use.
    ///
    /// Selecting a model implicitly enables it — a device default that no
    /// picker would offer is a contradiction — and the last enabled model
    /// cannot be removed while it is the selection, because an empty set
    /// means "no restriction" and would silently widen the list instead of
    /// narrowing it.
    pub fn set_model_enabled(
        &self,
        provider: &str,
        model: &str,
        enabled: bool,
    ) -> Result<AiPreferences> {
        self.descriptor(provider)?;
        if model.trim().is_empty() {
            return Err(Error::Invalid("a model id is required".into()));
        }
        if enabled && is_helper_model(model) {
            return Err(Error::Invalid(format!(
                "{model} is a helper model and cannot be selected for chat"
            )));
        }
        self.preferences
            .update(|preferences| {
                preferences.enabled_models.retain(|entry| {
                    !(entry.provider == provider && entry.model.as_deref() == Some(model))
                });
                if enabled {
                    preferences.enabled_models.push(ModelRef {
                        provider: provider.to_string(),
                        model: Some(model.to_string()),
                        display_name: None,
                    });
                }
            })
            .map_err(Error::Io)
    }

    /// Replace the whole set at once.
    pub fn set_enabled_models(&self, models: Vec<ModelRef>) -> Result<AiPreferences> {
        for entry in &models {
            self.descriptor(&entry.provider)?;
            if let Some(model) = &entry.model {
                if is_helper_model(model) {
                    return Err(Error::Invalid(format!(
                        "{model} is a helper model and cannot be selected for chat"
                    )));
                }
            }
        }
        self.preferences
            .update(|preferences| preferences.enabled_models = models)
            .map_err(Error::Io)
    }

    /// Whether a model may be used. An empty set means every model may.
    pub fn is_model_enabled(&self, provider: &str, model: &str) -> bool {
        let Ok(preferences) = self.preferences.load() else {
            return true;
        };
        if preferences.enabled_models.is_empty() {
            return true;
        }
        preferences
            .enabled_models
            .iter()
            .any(|entry| entry.provider == provider && entry.model.as_deref() == Some(model))
    }

    pub fn clear_selection(&self) -> Result<AiPreferences> {
        self.preferences
            .update(|preferences| preferences.selected = None)
            .map_err(Error::Io)
    }

    pub fn set_provider_endpoint(
        &self,
        provider: &str,
        endpoint: Option<String>,
    ) -> Result<AiPreferences> {
        let descriptor = self.descriptor(provider)?;
        if !descriptor.endpoint_editable {
            return Err(Error::Invalid(format!(
                "{provider} has no configurable endpoint"
            )));
        }
        let endpoint = endpoint
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty());
        if let Some(endpoint) = &endpoint {
            let parsed = url::Url::parse(endpoint).map_err(|error| {
                Error::Invalid(format!("endpoint must be an http(s) URL: {error}"))
            })?;
            if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                return Err(Error::Invalid("endpoint must be an http(s) URL".into()));
            }
        }
        let preferences = self
            .preferences
            .update(|preferences| match &endpoint {
                Some(endpoint) => {
                    preferences
                        .endpoints
                        .insert(provider.to_string(), endpoint.clone());
                }
                None => {
                    preferences.endpoints.remove(provider);
                }
            })
            .map_err(Error::Io)?;
        self.clients
            .lock()
            .unwrap()
            .retain(|(id, _, _), _| id != provider);
        self.health_cache.lock().unwrap().remove(provider);
        self.models_cache.lock().unwrap().remove(provider);
        Ok(preferences)
    }

    pub fn set_auto_start_omlx(&self, enabled: bool) -> Result<AiPreferences> {
        self.preferences
            .update(|preferences| preferences.auto_start_omlx = enabled)
            .map_err(Error::Io)
    }

    pub fn set_task_category(
        &self,
        category: &str,
        assignment: CategoryAssignment,
    ) -> Result<AiPreferences> {
        let known = categories::category(category)
            .ok_or_else(|| Error::Invalid(format!("unknown task category '{category}'")))?;
        if known.is_root() {
            return Err(Error::Invalid(format!(
                "{category} is a category group; assign models to its leaf categories"
            )));
        }
        for reference in assignment
            .primary
            .iter()
            .chain(assignment.comparison.iter())
        {
            self.descriptor(&reference.provider)?;
            if reference.model.as_deref().is_some_and(is_helper_model) {
                return Err(Error::Invalid(format!(
                    "{} is a helper model and cannot be assigned",
                    reference.model.as_deref().unwrap_or_default()
                )));
            }
        }
        self.preferences
            .update(|preferences| {
                preferences
                    .task_categories
                    .insert(category.to_string(), assignment);
            })
            .map_err(Error::Io)
    }

    /// Credentials changed in the host: drop clients built with the old ones.
    pub fn invalidate_clients(&self, provider: &str) {
        self.clients
            .lock()
            .unwrap()
            .retain(|(id, _, _), _| id != provider);
    }

    /// Non-secret view of every provider for pickers and diagnostics.
    pub fn provider_states(&self) -> Vec<ProviderState> {
        CATALOGUE
            .iter()
            .map(|descriptor| ProviderState {
                descriptor,
                endpoint: self.endpoint_for(descriptor.id),
                readiness: self.readiness(descriptor.id),
                credentials: credential_status(descriptor, self.credentials.as_ref()),
                dialect: match descriptor.transport {
                    Transport::OpenAiCompatible(dialect) => Some(dialect),
                    _ => None,
                },
            })
            .collect()
    }

    pub fn task_categories(&self) -> BTreeMap<String, CategoryAssignment> {
        self.preferences
            .load()
            .map(|preferences| preferences.task_categories)
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone)]
pub struct ProviderState {
    pub descriptor: &'static ProviderDescriptor,
    pub endpoint: Option<String>,
    pub readiness: HealthState,
    pub credentials: CredentialStatus,
    pub dialect: Option<Dialect>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::InMemoryCredentials;
    use std::collections::HashMap;

    fn registry_with(memory: &Arc<InMemoryCredentials>) -> (tempfile::TempDir, Arc<AiRegistry>) {
        let directory = tempfile::tempdir().unwrap();
        let registry = AiRegistry::for_app(directory.path(), memory.clone());
        (directory, registry)
    }

    fn set_key(memory: &InMemoryCredentials, provider: &str, key: &str) {
        memory.set_all(
            provider,
            HashMap::from([("apiKey".to_string(), key.to_string())]),
        );
    }

    #[test]
    fn explicit_beats_category_beats_selected_beats_first_ready() {
        let memory = InMemoryCredentials::new();
        let (_directory, registry) = registry_with(&memory);

        // Nothing configured, nothing reachable: NotConfigured.
        assert!(matches!(
            registry.resolve(&ResolveTarget::default()),
            Err(Error::NotConfigured { .. })
        ));

        // A cloud key makes anthropic the first ready provider.
        set_key(&memory, "anthropic", "sk-ant");
        let resolved = registry.resolve(&ResolveTarget::default()).unwrap();
        assert_eq!(resolved.provider, "anthropic");
        assert_eq!(resolved.model, "claude-opus-5");
        assert_eq!(resolved.origin, ResolutionOrigin::FirstReady);
        assert_eq!(resolved.endpoint_id, "anthropic-default");

        // A selection wins even though it is not reachable right now.
        registry.select_model("omlx", None).unwrap();
        let resolved = registry.resolve(&ResolveTarget::default());
        assert!(
            matches!(resolved, Err(Error::Invalid(ref message)) if message.contains("choose a model"))
        );
        registry
            .select_model("omlx", Some("mlx-community--Qwen3.5-4B-4bit".into()))
            .unwrap();
        let resolved = registry.resolve(&ResolveTarget::default()).unwrap();
        assert_eq!(resolved.provider, "omlx");
        assert_eq!(resolved.origin, ResolutionOrigin::Selected);
        assert_eq!(resolved.endpoint_id, "local-omlx");

        // A category assignment beats the selection for that category only.
        registry
            .set_task_category(
                "research.rag",
                CategoryAssignment {
                    primary: Some(ModelRef::new("anthropic", Some("claude-sonnet-5".into()))),
                    ..Default::default()
                },
            )
            .unwrap();
        let resolved = registry
            .resolve(&ResolveTarget::category("research.rag"))
            .unwrap();
        assert_eq!(resolved.provider, "anthropic");
        assert_eq!(resolved.model, "claude-sonnet-5");
        assert_eq!(resolved.origin, ResolutionOrigin::Category);
        let resolved = registry
            .resolve(&ResolveTarget::category("writing.rewrite"))
            .unwrap();
        assert_eq!(resolved.provider, "omlx");

        // Explicit wins over everything and uses the catalogue default model.
        let resolved = registry
            .resolve(&ResolveTarget {
                provider: Some("anthropic".into()),
                model: None,
                category: Some("research.rag".into()),
            })
            .unwrap();
        assert_eq!(resolved.origin, ResolutionOrigin::Explicit);
        assert_eq!(
            resolved.model, "claude-sonnet-5",
            "category model applies when providers agree"
        );
        let resolved = registry
            .resolve(&ResolveTarget {
                provider: Some("openai".into()),
                model: Some("gpt-4o-mini".into()),
                category: None,
            })
            .unwrap();
        assert_eq!(resolved.model, "gpt-4o-mini");
        assert!(matches!(
            registry.resolve(&ResolveTarget::provider("nope")),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn readiness_reflects_credentials_endpoint_and_foreign_flags() {
        let memory = InMemoryCredentials::new();
        let (_directory, registry) = registry_with(&memory);
        assert_eq!(
            registry.readiness("openai"),
            HealthState::NeedsCredentials {
                fields: vec!["apiKey".into()]
            }
        );
        set_key(&memory, "openai", "sk-1");
        assert_eq!(registry.readiness("openai"), HealthState::Ready);
        assert_eq!(
            registry.readiness("openai-compatible"),
            HealthState::NeedsEndpoint
        );
        assert_eq!(
            registry.readiness("omlx"),
            HealthState::Unreachable,
            "no probe yet"
        );
        assert_eq!(
            registry.readiness("apple-on-device"),
            HealthState::Foreign { available: false }
        );
        registry.set_foreign_available("apple-on-device", true);
        assert!(registry.readiness("apple-on-device").is_ready());
        assert!(matches!(
            registry.provider("apple-on-device"),
            Err(Error::ForeignExecutor(_))
        ));
    }

    #[test]
    fn the_enabled_set_narrows_the_pickers_and_never_hides_everything() {
        let memory = InMemoryCredentials::new();
        let (_directory, registry) = registry_with(&memory);

        // An empty set is "no restriction", so a fresh device sees every
        // model rather than none.
        assert!(registry.is_model_enabled("omlx", "mlx-community--Qwen3.5-4B-4bit"));
        assert!(registry.is_model_enabled("anthropic", "claude-opus-5"));

        registry
            .set_model_enabled("omlx", "mlx-community--Qwen3.5-4B-4bit", true)
            .unwrap();
        assert!(registry.is_model_enabled("omlx", "mlx-community--Qwen3.5-4B-4bit"));
        assert!(
            !registry.is_model_enabled("anthropic", "claude-opus-5"),
            "once a set exists it is the whole list"
        );

        // Enabling twice does not duplicate the row.
        registry
            .set_model_enabled("omlx", "mlx-community--Qwen3.5-4B-4bit", true)
            .unwrap();
        let preferences = registry.preferences().load().unwrap();
        assert_eq!(preferences.enabled_models.len(), 1);

        // Helpers are refused here exactly as they are for the selection.
        assert!(registry
            .set_model_enabled("omlx", "MarkItDown", true)
            .is_err());
        assert!(registry.set_model_enabled("nonesuch", "m", true).is_err());

        registry
            .set_model_enabled("omlx", "mlx-community--Qwen3.5-4B-4bit", false)
            .unwrap();
        assert!(
            registry.is_model_enabled("anthropic", "claude-opus-5"),
            "emptying the set restores the unrestricted list"
        );
    }

    #[test]
    fn mutations_validate_and_invalidate_clients() {
        let memory = InMemoryCredentials::new();
        let (_directory, registry) = registry_with(&memory);
        assert!(registry
            .select_model("omlx", Some("MarkItDown".into()))
            .is_err());
        assert!(registry.select_model("nope", None).is_err());
        assert!(registry
            .set_provider_endpoint("apple-on-device", Some("http://x".into()))
            .is_err());
        assert!(registry
            .set_provider_endpoint("omlx", Some("ftp://x".into()))
            .is_err());
        assert!(registry
            .set_task_category("research", CategoryAssignment::default())
            .is_err());
        assert!(registry
            .set_task_category("nope.leaf", CategoryAssignment::default())
            .is_err());

        let preferences = registry
            .set_provider_endpoint("openai-compatible", Some("http://box:1234/v1/".into()))
            .unwrap();
        assert_eq!(
            preferences.endpoints["openai-compatible"],
            "http://box:1234/v1"
        );
        assert_eq!(
            registry.endpoint_for("openai-compatible").as_deref(),
            Some("http://box:1234/v1")
        );
        let first = registry.provider("openai-compatible").unwrap();
        assert!(Arc::ptr_eq(
            &first,
            &registry.provider("openai-compatible").unwrap()
        ));
        registry
            .set_provider_endpoint("openai-compatible", None)
            .unwrap();
        assert_eq!(
            registry.readiness("openai-compatible"),
            HealthState::NeedsEndpoint
        );

        set_key(&memory, "anthropic", "k1");
        let before = registry.provider("anthropic").unwrap();
        set_key(&memory, "anthropic", "k2");
        let after = registry.provider("anthropic").unwrap();
        assert!(
            !Arc::ptr_eq(&before, &after),
            "rotated key yields a fresh client"
        );
        assert_eq!(after.endpoint_id(), "anthropic-default");

        let states = registry.provider_states();
        assert_eq!(states.len(), CATALOGUE.len());
        assert!(states
            .iter()
            .all(|state| !format!("{state:?}").contains("k2")));
        let auto = registry.set_auto_start_omlx(false).unwrap();
        assert!(!auto.auto_start_omlx);
    }

    #[tokio::test]
    async fn a_fresh_process_learns_local_readiness_before_first_ready_resolution() {
        // A one-shot process (CLI, MCP, an app at launch) has no health
        // cache. `first_ready` must still find a running local host, and
        // `provider_states_probed` must report it ready — without a daemon's
        // refresh loop having run first.
        let memory = InMemoryCredentials::new();
        let (_directory, registry) = registry_with(&memory);
        let mut server = mockito::Server::new_async().await;
        let _listed = server
            .mock("GET", "/v1/models")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":[{"id":"mlx-community--Qwen3.5-4B-4bit"}]}"#)
            .create_async()
            .await;
        let _status = server
            .mock("GET", "/v1/models/status")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"models":[{"id":"mlx-community--Qwen3.5-4B-4bit","loaded":true,"model_type":"llm"}]}"#)
            .create_async()
            .await;
        let _health = server
            .mock("GET", "/health")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":"healthy","default_model":"mlx-community--Qwen3.5-4B-4bit","engine_pool":{"model_count":1,"loaded_count":1}}"#)
            .create_async()
            .await;
        registry
            .set_provider_endpoint("omlx", Some(server.url()))
            .unwrap();
        assert!(
            !registry.readiness("omlx").is_ready(),
            "no probe has run: the network-free rule still says unreachable"
        );

        let resolved = registry
            .resolve_with_discovery(&ResolveTarget::default())
            .await
            .expect("first_ready finds the running local host");
        assert_eq!(resolved.provider, "omlx");
        assert_eq!(resolved.origin.label(), "first_ready");
        assert_eq!(resolved.model, "mlx-community--Qwen3.5-4B-4bit");

        let states = registry.provider_states_probed().await;
        let omlx = states
            .iter()
            .find(|state| state.descriptor.id == "omlx")
            .unwrap();
        assert!(omlx.readiness.is_ready());
        assert!(registry.readiness("omlx").is_ready(), "the probe is cached");
    }

    #[tokio::test]
    async fn models_merge_catalogue_and_discovery_and_feed_the_default() {
        let memory = InMemoryCredentials::new();
        let (_directory, registry) = registry_with(&memory);
        let mut server = mockito::Server::new_async().await;
        let _listed = server
            .mock("GET", "/v1/models")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":[{"id":"mlx-community--Qwen3.5-4B-4bit"},{"id":"MarkItDown"}]}"#)
            .create_async()
            .await;
        let _status = server
            .mock("GET", "/v1/models/status")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"models":[{"id":"mlx-community--Qwen3.5-4B-4bit","loaded":true,"model_type":"vlm"},{"id":"MarkItDown","model_type":"markitdown"}]}"#)
            .create_async()
            .await;
        let _health = server
            .mock("GET", "/health")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":"healthy","default_model":"mlx-community--Qwen3.5-4B-4bit","engine_pool":{"model_count":1,"loaded_count":1}}"#)
            .create_async()
            .await;
        registry
            .set_provider_endpoint("omlx", Some(server.url()))
            .unwrap();

        let (provider, models) = registry.models(Some("omlx")).await.unwrap();
        assert_eq!(provider, "omlx");
        assert_eq!(models.len(), 2);
        assert!(models.iter().any(|model| model.is_helper));

        registry.select_model("omlx", None).unwrap();
        let resolved = registry
            .resolve_with_discovery(&ResolveTarget::default())
            .await
            .unwrap();
        assert_eq!(
            resolved.model, "mlx-community--Qwen3.5-4B-4bit",
            "host default, never the helper"
        );
        assert_ne!(
            resolved.endpoint_id, "local-omlx",
            "a custom endpoint gets its own identity"
        );

        let health = registry.health("omlx").await;
        assert_eq!(health.state, HealthState::Ready);
        assert!(
            registry.readiness("omlx").is_ready(),
            "the probe feeds readiness"
        );
        assert_eq!(
            registry.resolve(&ResolveTarget::default()).unwrap().origin,
            ResolutionOrigin::Selected
        );
    }
}
