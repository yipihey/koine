//! The AI verbs (`impress-ai-service_*`), moved from impress-apps'
//! `impress-ai-service` with their names (docs/plan-koine-ai-tasks.md AI-D1).
//!
//! No second implementation of AI behaviour: each method delegates to
//! [`AiStore`] and [`AiRegistry`]. Built with no arguments by the inventory,
//! the service uses the process store and `<workspace>/ai/` (AI-D3:
//! `koine_store::process::{current, workspace}`), resolved on every call so a
//! host that installs its store after linking is the store the verbs see.
//! [`install`] replaces both, for a host that holds its own.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use crate::registry::ResolveTarget;
use crate::{
    AiPreferences, AiRegistry, AiStore, ConversationDraft, ConversationSnapshot, MessageDraft,
    ModelSummary, ProviderHealth, RunProvenance, TaskProgress, ToolPolicy,
};
use impress_service_core::async_trait;
#[allow(unused_imports)]
use impress_service_macros::impress_method;
use impress_service_macros::{impress_service, impress_service_impl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ModelsResult {
    /// The provider the rows belong to (the resolved default when the caller
    /// named none).
    pub provider: Option<String>,
    pub models: Vec<ModelSummary>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CredentialFieldRow {
    pub id: String,
    pub label: String,
    pub secret: bool,
    pub optional: bool,
    /// Whether a value is present — never the value itself.
    pub configured: bool,
    /// `env`, `memory` or `keychain` when configured.
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProviderRow {
    pub id: String,
    pub display_name: String,
    pub description: String,
    /// `local`, `cloud`, `aggregator` or `native`.
    pub category: String,
    /// `rust` or `foreign` (executed by the host GUI).
    pub host: String,
    pub ready: bool,
    /// `ready`, `empty`, `needs_credentials`, `needs_endpoint`, `unreachable`,
    /// `foreign` or `foreign_unavailable`.
    pub readiness: String,
    pub endpoint: Option<String>,
    pub default_endpoint: Option<String>,
    pub endpoint_editable: bool,
    pub can_auto_start: bool,
    pub registration_url: Option<String>,
    pub credential_fields: Vec<CredentialFieldRow>,
    pub static_model_count: u32,
    pub has_dynamic_catalogue: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProvidersResult {
    pub providers: Vec<ProviderRow>,
    /// The device selection, if one is pinned.
    pub selected_provider: Option<String>,
    pub selected_model: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AiPreferencesResult {
    pub preferences: Option<AiPreferences>,
    /// Where the device-local file lives (`<workspace>/ai/preferences.json`).
    pub path: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProviderHealthResult {
    pub provider: Option<String>,
    pub health: Option<ProviderHealth>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConversationsResult {
    /// Canonical graph rows (`impress_item::Item` carries no schema).
    #[schemars(with = "Vec<serde_json::Value>")]
    pub conversations: Vec<impress_item::Item>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConversationResult {
    pub conversation: Option<ConversationSnapshot>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConversationMutationResult {
    pub success: bool,
    pub conversation_id: Option<Uuid>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QueuedMessageResult {
    pub success: bool,
    pub conversation_id: Option<Uuid>,
    pub message_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskStatusResult {
    pub task: Option<TaskProgress>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProvenanceResult {
    pub provenance: Option<RunProvenance>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AiHealthResult {
    /// False when the daemon is unreachable (fields then default to zero).
    pub daemon_reachable: bool,
    pub db_bytes: u64,
    pub wal_bytes: u64,
    pub wal_budget_bytes: u64,
    pub freelist_pages: u64,
    /// Whether the daemon held the maintenance lease on its last cycle.
    pub lease_owner: bool,
    pub last_checkpoint_ms: Option<i64>,
    pub last_compaction_ms: Option<i64>,
    pub last_demotion_count: Option<u64>,
    pub last_vacuum_ms: Option<i64>,
    /// Operations minted in the trailing 24 h (churn-rate telemetry).
    pub ops_last_24h: Option<u64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PairingLinkResult {
    /// Single-use link against the loopback origin. When the Mac is fronted
    /// by an HTTPS proxy (tailscale serve), replace the origin and keep the
    /// fragment — the ticket rides in `#pair=` so it never enters logs.
    pub url_local: Option<String>,
    pub ticket: Option<String>,
    pub expires_in_secs: u32,
    pub error: Option<String>,
}

#[impress_service]
pub trait ImpressAiService: Send + Sync + 'static {
    /// List models for `provider` (or the device's resolved default provider
    /// when omitted): the static catalogue merged with live discovery,
    /// including load state, context limit, modalities and the helper flag.
    #[impress_method(safety = external, effects(reach = [provider]))]
    #[impress_example(
        name = "catalogue-after-provider-discovery",
        tier = "b",
        args = r#"{"provider":"ollama"}"#
    )]
    async fn list_models(&self, provider: Option<String>) -> ModelsResult;

    /// Every catalogued AI provider with this device's endpoint, readiness
    /// and credential status (which fields are set — never their values).
    #[impress_method(safety = external, effects(reach = [provider, fs]))]
    #[impress_example(name = "device-provider-readiness", tier = "b", args = r#"{}"#)]
    async fn list_providers(&self) -> ProvidersResult;

    /// The device-local AI preferences: selected provider/model, endpoint
    /// overrides, oMLX auto-start and per-task-category assignments.
    #[impress_method]
    #[impress_example(name = "scratch-device-preferences", args = r#"{}"#)]
    async fn ai_preferences(&self) -> AiPreferencesResult;

    /// Pin the device's provider and (optionally) model. Every app and daemon
    /// on the device follows it. Helper pseudo-models are rejected.
    #[impress_method(safety = mutating)]
    #[impress_example(
        name = "select-local-model",
        args = r#"{"provider":"ollama","model":"llama3.2"}"#
    )]
    async fn select_model(&self, provider: String, model: Option<String>) -> AiPreferencesResult;

    /// Override (or, with `None`, reset) a provider's endpoint — e.g. an
    /// oMLX host reached over Tailscale. Secrets never go here.
    #[impress_method(safety = mutating)]
    #[impress_example(
        name = "set-scratch-endpoint",
        args = r#"{"provider":"ollama","endpoint":"http://127.0.0.1:11435"}"#
    )]
    async fn set_provider_endpoint(
        &self,
        provider: String,
        endpoint: Option<String>,
    ) -> AiPreferencesResult;

    /// Passive reachability probe for `provider` (or the resolved default):
    /// never launches a host. For oMLX it reports version, loaded/total
    /// models and memory.
    #[impress_method(safety = external, effects(reach = [provider]))]
    #[impress_example(
        name = "local-provider-probe",
        tier = "b",
        args = r#"{"provider":"ollama"}"#
    )]
    async fn provider_health(&self, provider: Option<String>) -> ProviderHealthResult;

    /// List durable AI conversations from the shared Impress item graph.
    #[impress_method(effects(reads = ["conversation@1.0.0"]))]
    #[impress_example(
        name = "scratch-conversation-list",
        args = r#"{"include_archived":false}"#
    )]
    async fn list_conversations(&self, include_archived: bool) -> ConversationsResult;

    /// Read a conversation with its ordered messages and pending durable
    /// response tasks.
    #[impress_method(effects(reads = ["conversation@1.0.0", "chat-message", "task@1.0.0"]))]
    #[impress_example(
        name = "scratch-conversation-detail",
        args = r#"{"conversation_id":"65000000-0000-4000-8000-000000000002"}"#
    )]
    async fn get_conversation(&self, conversation_id: String) -> ConversationResult;

    /// Create a durable conversation. Enabled tools are stable capability ids
    /// such as `scix`, `impress-mcp`, and `web`.
    #[impress_method(safety = mutating, effects(reads = ["conversation@1.0.0"], writes = ["conversation@1.0.0"]))]
    #[impress_example(
        name = "start-research-conversation",
        args = r#"{"title":"G3 literature review","model":"llama3.2","provider":"ollama","system_prompt":null,"temperature":0.3,"max_tokens":512,"thinking":false,"web_access":false,"enabled_tools":["impress-mcp"]}"#,
        expect = r#"{"success":true}"#
    )]
    #[allow(clippy::too_many_arguments)]
    async fn create_conversation(
        &self,
        title: String,
        model: String,
        provider: Option<String>,
        system_prompt: Option<String>,
        temperature: f32,
        max_tokens: u32,
        thinking: bool,
        web_access: bool,
        enabled_tools: Vec<String>,
    ) -> ConversationMutationResult;

    /// Atomically append a user message and queue its offline-capable response
    /// task. Attachment ids must already identify content-blob items.
    #[impress_method(safety = mutating, effects(reads = ["conversation@1.0.0", "chat-message"], writes = ["conversation@1.0.0", "chat-message", "task@1.0.0"]))]
    #[impress_example(
        name = "queue-scratch-question",
        args = r#"{"conversation_id":"68000000-0000-4000-8000-000000000001","body":"Summarize the methods in this project.","attachment_ids":[]}"#,
        expect = r#"{"success":true}"#
    )]
    async fn queue_message(
        &self,
        conversation_id: String,
        body: String,
        attachment_ids: Vec<String>,
    ) -> QueuedMessageResult;

    /// Replace the conversation's enabled tool-capability policy.
    #[impress_method(safety = mutating, effects(reads = ["conversation@1.0.0"], writes = ["conversation@1.0.0"]))]
    #[impress_example(
        name = "allow-research-tools",
        args = r#"{"conversation_id":"68000000-0000-4000-8000-000000000002","enabled_tools":["impress-mcp","web"]}"#,
        expect = r#"{"success":true}"#
    )]
    async fn set_enabled_tools(
        &self,
        conversation_id: String,
        enabled_tools: Vec<String>,
    ) -> ConversationMutationResult;

    /// Read durable scheduler/run progress for a queued response task.
    #[impress_method(effects(reads = ["task@1.0.0", "agent-run@1.0.0", "chat-message"]))]
    #[impress_example(
        name = "pending-scratch-task",
        args = r#"{"task_id":"68000000-0000-4000-8000-000000000003"}"#,
        expect = r#"{"task":{"state":"pending"}}"#
    )]
    async fn task_status(&self, task_id: String) -> TaskStatusResult;

    /// Return the latest model run lineage for a response task: canonical
    /// inputs, tool invocations, and attributed outputs.
    #[impress_method(effects(reads = ["task@1.0.0", "agent-run@1.0.0", "tool-invocation@1.0.0", any("run lineage loads produced outputs of any record kind")]))]
    #[impress_example(
        name = "scratch-task-lineage",
        args = r#"{"task_id":"68000000-0000-4000-8000-000000000004"}"#
    )]
    async fn task_provenance(&self, task_id: String) -> ProvenanceResult;

    /// Return complete lineage for a specific agent-run item.
    #[impress_method(effects(reads = ["agent-run@1.0.0", "task@1.0.0", "tool-invocation@1.0.0", any("run lineage loads produced outputs of any record kind")]))]
    #[impress_example(
        name = "scratch-run-lineage",
        args = r#"{"run_id":"68000000-0000-4000-8000-000000000006"}"#
    )]
    async fn run_provenance(&self, run_id: String) -> ProvenanceResult;

    /// Store-hygiene health from the AI daemon: db/WAL/freelist sizes, the
    /// maintenance lease, last verb outcomes, and the trailing-24h op rate.
    /// `daemon_reachable: false` (never an error) when it isn't running.
    #[impress_method(safety = external, effects(reach = [network]))]
    #[impress_example(
        name = "isolated-daemon-health",
        tier = "b",
        args = r#"{}"#,
        expect = r#"{"daemon_reachable":true}"#
    )]
    async fn ai_health(&self) -> AiHealthResult;

    /// Mint a single-use browser pairing link for the AI daemon (15-minute
    /// expiry). Requires the local keychain bearer (`com.impress.ai-http`),
    /// so this works on the Mac that runs the daemon, not remotely.
    #[impress_method(safety = external, effects(reach = [network]))]
    #[impress_example(name = "isolated-daemon-pairing", tier = "b", args = r#"{}"#)]
    async fn mint_pairing_link(&self) -> PairingLinkResult;
}

#[derive(Clone)]
pub struct DefaultImpressAiService {
    ai: Arc<AiStore>,
    registry: Arc<AiRegistry>,
}

impl DefaultImpressAiService {
    pub fn with_components(ai: Arc<AiStore>, registry: Arc<AiRegistry>) -> Self {
        Self { ai, registry }
    }

    /// The process's deployment: its store, and the registry over
    /// `<workspace>/ai/preferences.json` (the file every app and daemon on
    /// this device reads).
    fn shared() -> Self {
        let (store, _fallback) = koine_store::process::acquire_or_stand_in();
        let ai = Arc::new(AiStore::from_port(store, "agent:impress-ai-service"));
        Self::with_components(ai, registry_for(workspace()))
    }

    fn preferences_result(&self, result: crate::Result<AiPreferences>) -> AiPreferencesResult {
        let path = self
            .registry
            .preferences()
            .path()
            .to_string_lossy()
            .into_owned();
        match result {
            Ok(preferences) => AiPreferencesResult {
                preferences: Some(preferences),
                path,
                error: None,
            },
            Err(error) => AiPreferencesResult {
                preferences: None,
                path,
                error: Some(error.to_string()),
            },
        }
    }
}

fn parse_id(value: &str, kind: &str) -> Result<Uuid, String> {
    Uuid::parse_str(value).map_err(|error| format!("invalid {kind} id '{value}': {error}"))
}

fn parse_ids(values: Vec<String>, kind: &str) -> Result<Vec<Uuid>, String> {
    values
        .into_iter()
        .map(|value| parse_id(&value, kind))
        .collect()
}

#[async_trait::async_trait]
impl ImpressAiService for DefaultImpressAiService {
    async fn list_models(&self, provider: Option<String>) -> ModelsResult {
        match self.registry.models(provider.as_deref()).await {
            Ok((provider, mut models)) => {
                // Helper pseudo-models (oMLX's MarkItDown) are never
                // selectable, so the agent surface does not list them; the
                // GUI keeps them behind its "show helper models" toggle.
                models.retain(|model| !model.is_helper);
                ModelsResult {
                    provider: Some(provider),
                    models,
                    error: None,
                }
            }
            Err(error) => ModelsResult {
                provider,
                models: vec![],
                error: Some(error.to_string()),
            },
        }
    }

    async fn list_providers(&self) -> ProvidersResult {
        let preferences = self.registry.preferences().load().ok();
        let selected = preferences.and_then(|preferences| preferences.selected);
        ProvidersResult {
            providers: self
                .registry
                .provider_states_probed()
                .await
                .into_iter()
                .map(|state| {
                    let descriptor = state.descriptor;
                    ProviderRow {
                        id: descriptor.id.into(),
                        display_name: descriptor.display_name.into(),
                        description: descriptor.description.into(),
                        category: descriptor.category.label().into(),
                        host: descriptor.host.label().into(),
                        ready: state.readiness.is_ready(),
                        readiness: state.readiness.label().into(),
                        endpoint: state.endpoint,
                        default_endpoint: descriptor.default_endpoint.map(str::to_string),
                        endpoint_editable: descriptor.endpoint_editable,
                        can_auto_start: descriptor.can_auto_start,
                        registration_url: descriptor.registration_url.map(str::to_string),
                        credential_fields: state
                            .credentials
                            .fields
                            .into_iter()
                            .map(|field| {
                                let label = descriptor
                                    .credential_fields
                                    .iter()
                                    .find(|declared| declared.id == field.field)
                                    .map(|declared| declared.label.to_string())
                                    .unwrap_or_default();
                                CredentialFieldRow {
                                    id: field.field,
                                    label,
                                    secret: field.secret,
                                    optional: field.optional,
                                    configured: field.configured,
                                    source: field.source.map(str::to_string),
                                }
                            })
                            .collect(),
                        static_model_count: descriptor.static_models.len() as u32,
                        has_dynamic_catalogue: descriptor.discovery
                            == crate::catalogue::Discovery::Api,
                    }
                })
                .collect(),
            selected_provider: selected.as_ref().map(|selected| selected.provider.clone()),
            selected_model: selected.and_then(|selected| selected.model),
            error: None,
        }
    }

    async fn ai_preferences(&self) -> AiPreferencesResult {
        self.preferences_result(self.registry.preferences().load().map_err(crate::Error::Io))
    }

    async fn select_model(&self, provider: String, model: Option<String>) -> AiPreferencesResult {
        self.preferences_result(self.registry.select_model(&provider, model))
    }

    async fn set_provider_endpoint(
        &self,
        provider: String,
        endpoint: Option<String>,
    ) -> AiPreferencesResult {
        self.preferences_result(self.registry.set_provider_endpoint(&provider, endpoint))
    }

    async fn provider_health(&self, provider: Option<String>) -> ProviderHealthResult {
        let provider = match provider {
            Some(provider) => provider,
            None => match self.registry.resolve(&ResolveTarget::default()) {
                Ok(resolved) => resolved.provider,
                Err(error) => {
                    return ProviderHealthResult {
                        provider: None,
                        health: None,
                        error: Some(error.to_string()),
                    }
                }
            },
        };
        let health = self.registry.health(&provider).await;
        ProviderHealthResult {
            provider: Some(provider),
            health: Some(health),
            error: None,
        }
    }

    async fn list_conversations(&self, include_archived: bool) -> ConversationsResult {
        match self.ai.list_conversations(include_archived) {
            Ok(conversations) => ConversationsResult {
                conversations,
                error: None,
            },
            Err(error) => ConversationsResult {
                conversations: vec![],
                error: Some(error.to_string()),
            },
        }
    }

    async fn get_conversation(&self, conversation_id: String) -> ConversationResult {
        let result = parse_id(&conversation_id, "conversation")
            .and_then(|id| self.ai.snapshot(id).map_err(|error| error.to_string()));
        match result {
            Ok(conversation) => ConversationResult {
                conversation: Some(conversation),
                error: None,
            },
            Err(error) => ConversationResult {
                conversation: None,
                error: Some(error),
            },
        }
    }

    async fn create_conversation(
        &self,
        title: String,
        model: String,
        provider: Option<String>,
        system_prompt: Option<String>,
        temperature: f32,
        max_tokens: u32,
        thinking: bool,
        web_access: bool,
        enabled_tools: Vec<String>,
    ) -> ConversationMutationResult {
        let defaults = ConversationDraft::default();
        // A conversation resolves its provider once, at creation: an
        // explicit id wins, else the device selection, else the catalogue's
        // first ready provider, else the historical default.
        let provider = provider.unwrap_or_else(|| {
            self.registry
                .resolve(&ResolveTarget {
                    provider: None,
                    model: Some(model.clone()).filter(|model| !model.trim().is_empty()),
                    category: None,
                })
                .map(|resolved| resolved.provider)
                .unwrap_or_else(|_| defaults.provider.clone())
        });
        let result = self.ai.create_conversation(ConversationDraft {
            title,
            summary: None,
            system_prompt: system_prompt.or(defaults.system_prompt),
            provider,
            model,
            temperature,
            max_tokens,
            thinking,
            web_access,
            tool_policy: ToolPolicy {
                enabled: enabled_tools,
            },
        });
        match result {
            Ok(id) => ConversationMutationResult {
                success: true,
                conversation_id: Some(id),
                error: None,
            },
            Err(error) => ConversationMutationResult {
                success: false,
                conversation_id: None,
                error: Some(error.to_string()),
            },
        }
    }

    async fn queue_message(
        &self,
        conversation_id: String,
        body: String,
        attachment_ids: Vec<String>,
    ) -> QueuedMessageResult {
        let result = parse_id(&conversation_id, "conversation").and_then(|conversation_id| {
            let attachment_ids = parse_ids(attachment_ids, "attachment")?;
            let mut draft = MessageDraft::user(body);
            draft.attachment_ids = attachment_ids;
            self.ai
                .queue_user_turn(conversation_id, draft)
                .map_err(|error| error.to_string())
        });
        match result {
            Ok(queued) => QueuedMessageResult {
                success: true,
                conversation_id: Some(queued.conversation_id),
                message_id: Some(queued.message_id),
                task_id: Some(queued.task_id),
                error: None,
            },
            Err(error) => QueuedMessageResult {
                success: false,
                conversation_id: None,
                message_id: None,
                task_id: None,
                error: Some(error),
            },
        }
    }

    async fn set_enabled_tools(
        &self,
        conversation_id: String,
        enabled_tools: Vec<String>,
    ) -> ConversationMutationResult {
        let result = parse_id(&conversation_id, "conversation").and_then(|id| {
            self.ai
                .set_tool_policy(
                    id,
                    ToolPolicy {
                        enabled: enabled_tools,
                    },
                )
                .map(|()| id)
                .map_err(|error| error.to_string())
        });
        match result {
            Ok(id) => ConversationMutationResult {
                success: true,
                conversation_id: Some(id),
                error: None,
            },
            Err(error) => ConversationMutationResult {
                success: false,
                conversation_id: None,
                error: Some(error),
            },
        }
    }

    async fn task_status(&self, task_id: String) -> TaskStatusResult {
        let result = parse_id(&task_id, "task")
            .and_then(|id| self.ai.task_progress(id).map_err(|error| error.to_string()));
        match result {
            Ok(task) => TaskStatusResult {
                task: Some(task),
                error: None,
            },
            Err(error) => TaskStatusResult {
                task: None,
                error: Some(error),
            },
        }
    }

    async fn task_provenance(&self, task_id: String) -> ProvenanceResult {
        let result = parse_id(&task_id, "task").and_then(|id| {
            self.ai
                .task_provenance(id)
                .map_err(|error| error.to_string())
        });
        match result {
            Ok(provenance) => ProvenanceResult {
                provenance,
                error: None,
            },
            Err(error) => ProvenanceResult {
                provenance: None,
                error: Some(error),
            },
        }
    }

    async fn run_provenance(&self, run_id: String) -> ProvenanceResult {
        let result = parse_id(&run_id, "run").and_then(|id| {
            self.ai
                .run_provenance(id)
                .map_err(|error| error.to_string())
        });
        match result {
            Ok(provenance) => ProvenanceResult {
                provenance: Some(provenance),
                error: None,
            },
            Err(error) => ProvenanceResult {
                provenance: None,
                error: Some(error),
            },
        }
    }

    async fn ai_health(&self) -> AiHealthResult {
        let unreachable = |error: Option<String>| AiHealthResult {
            daemon_reachable: false,
            db_bytes: 0,
            wal_bytes: 0,
            wal_budget_bytes: 0,
            freelist_pages: 0,
            lease_owner: false,
            last_checkpoint_ms: None,
            last_compaction_ms: None,
            last_demotion_count: None,
            last_vacuum_ms: None,
            ops_last_24h: None,
            error,
        };
        let response = match reqwest::Client::new()
            .get(format!("http://127.0.0.1:{AI_DAEMON_PORT}/api/health"))
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => response,
            Ok(response) => {
                return unreachable(Some(format!("daemon health HTTP {}", response.status())))
            }
            Err(_) => return unreachable(None),
        };
        let body: serde_json::Value = match response.json().await {
            Ok(body) => body,
            Err(error) => return unreachable(Some(format!("health body: {error}"))),
        };
        let maintenance = &body["maintenance"];
        AiHealthResult {
            daemon_reachable: true,
            db_bytes: body["db_bytes"].as_u64().unwrap_or(0),
            wal_bytes: body["wal_bytes"].as_u64().unwrap_or(0),
            wal_budget_bytes: body["wal_budget_bytes"].as_u64().unwrap_or(0),
            freelist_pages: body["freelist_pages"].as_u64().unwrap_or(0),
            lease_owner: maintenance["lease_owner"].as_bool().unwrap_or(false),
            last_checkpoint_ms: maintenance["last_checkpoint_ms"].as_i64(),
            last_compaction_ms: maintenance["last_compaction_ms"].as_i64(),
            last_demotion_count: maintenance["last_demotion_count"].as_u64(),
            last_vacuum_ms: maintenance["last_vacuum_ms"].as_i64(),
            ops_last_24h: body["ops_last_24h"].as_u64(),
            error: None,
        }
    }

    async fn mint_pairing_link(&self) -> PairingLinkResult {
        let fail = |error: String| PairingLinkResult {
            url_local: None,
            ticket: None,
            expires_in_secs: 0,
            error: Some(error),
        };
        let bearer = match ai_daemon_bearer().await {
            Ok(bearer) => bearer,
            Err(error) => return fail(error),
        };
        let response = match reqwest::Client::new()
            .post(format!(
                "http://127.0.0.1:{AI_DAEMON_PORT}/api/pairing-tickets"
            ))
            .bearer_auth(bearer)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => response,
            Ok(response) => return fail(format!("daemon returned HTTP {}", response.status())),
            Err(error) => return fail(format!("daemon unreachable: {error}")),
        };
        let body: serde_json::Value = match response.json().await {
            Ok(body) => body,
            Err(error) => return fail(format!("ticket body: {error}")),
        };
        let Some(ticket) = body["ticket"].as_str().map(str::to_owned) else {
            return fail("daemon response carried no ticket".into());
        };
        PairingLinkResult {
            url_local: Some(format!("http://127.0.0.1:{AI_DAEMON_PORT}/#pair={ticket}")),
            ticket: Some(ticket),
            expires_in_secs: 15 * 60,
            error: None,
        }
    }
}

/// The daemon's port (`SiblingApp.Services.impressAIPort` on the Swift side).
const AI_DAEMON_PORT: u16 = 8787;

/// The daemon bearer from the login keychain (`com.impress.ai-http`) — the
/// same item `run.sh` resolves at daemon launch. macOS-only by nature.
async fn ai_daemon_bearer() -> Result<String, String> {
    let output = tokio::process::Command::new("/usr/bin/security")
        .args(["find-generic-password", "-w", "-s", "com.impress.ai-http"])
        .output()
        .await
        .map_err(|error| format!("keychain lookup failed to run: {error}"))?;
    if !output.status.success() {
        return Err("keychain item com.impress.ai-http not found (is the daemon set up?)".into());
    }
    let bearer = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if bearer.len() < 24 {
        return Err("keychain bearer looks malformed".into());
    }
    Ok(bearer)
}

/// The workspace the AI files live in: the host's (AI-D3), else the current
/// directory.
fn workspace() -> PathBuf {
    koine_store::process::workspace().unwrap_or_else(|| PathBuf::from("."))
}

/// One registry per workspace, kept while the workspace stays the same.
fn registry_for(workspace: PathBuf) -> Arc<AiRegistry> {
    static REGISTRY: Mutex<Option<(PathBuf, Arc<AiRegistry>)>> = Mutex::new(None);
    let mut slot = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((dir, registry)) = slot.as_ref() {
        if *dir == workspace {
            return registry.clone();
        }
    }
    let registry = AiRegistry::for_daemon(workspace.clone());
    *slot = Some((workspace, registry.clone()));
    registry
}

static INSTALLED: RwLock<Option<DefaultImpressAiService>> = RwLock::new(None);

/// Answer the AI verbs from `ai` and `registry` instead of the process's
/// store and workspace (a host that holds its own; a test).
pub fn install(ai: Arc<AiStore>, registry: Arc<AiRegistry>) {
    *INSTALLED.write().unwrap_or_else(|e| e.into_inner()) =
        Some(DefaultImpressAiService::with_components(ai, registry));
}

fn service_instance() -> DefaultImpressAiService {
    if let Some(installed) = INSTALLED.read().unwrap_or_else(|e| e.into_inner()).clone() {
        return installed;
    }
    DefaultImpressAiService::shared()
}

impress_service_impl! {
    service = ImpressAiService,
    safety = read_only,
    since = "0.1.0",
    effects = {
        reads: [],
        writes: [],
        reach: [fs],
    },
    impl = DefaultImpressAiService,
    instance = || service_instance(),
    methods = [
        list_models(
            /// Provider ID to discover, or null for the device default.
            provider: Option<String>
        ) -> ModelsResult,
        list_providers() -> ProvidersResult,
        ai_preferences() -> AiPreferencesResult,
        select_model(
            /// Catalogue provider ID to select for this device.
            provider: String,
            /// Chat model ID to pin, or null to use its provider default.
            model: Option<String>
        ) -> AiPreferencesResult,
        set_provider_endpoint(
            /// Editable provider ID whose endpoint changes.
            provider: String,
            /// HTTP(S) endpoint URL, or null to reset to the catalogue default.
            endpoint: Option<String>
        ) -> AiPreferencesResult,
        provider_health(
            /// Provider ID to probe, or null for the device default.
            provider: Option<String>
        ) -> ProviderHealthResult,
        list_conversations(
            /// Whether archived conversations are included.
            include_archived: bool
        ) -> ConversationsResult,
        get_conversation(
            /// UUID of the durable conversation to read.
            conversation_id: String
        ) -> ConversationResult,
        create_conversation(
            /// Display title for the new conversation.
            title: String,
            /// Model ID to use for assistant responses.
            model: String,
            /// Provider ID, or null to resolve from device preferences.
            provider: Option<String>,
            /// Optional system instruction to start the conversation.
            system_prompt: Option<String>,
            /// Sampling temperature for future model responses.
            temperature: f32,
            /// Maximum response tokens.
            max_tokens: u32,
            /// Whether model thinking is requested.
            thinking: bool,
            /// Whether the `web` tool is enabled.
            web_access: bool,
            /// Stable capability IDs the assistant may call.
            enabled_tools: Vec<String>
        ) -> ConversationMutationResult,
        queue_message(
            /// UUID of the conversation that receives the user turn.
            conversation_id: String,
            /// User message text; private content is redacted from public tool logs.
            #[impress_private] body: String,
            /// Existing content-blob UUIDs to attach, or an empty list.
            attachment_ids: Vec<String>
        ) -> QueuedMessageResult,
        set_enabled_tools(
            /// UUID of the conversation whose tool policy changes.
            conversation_id: String,
            /// Stable capability IDs enabled for subsequent turns.
            enabled_tools: Vec<String>
        ) -> ConversationMutationResult,
        task_status(
            /// UUID of the durable response task to inspect.
            task_id: String
        ) -> TaskStatusResult,
        task_provenance(
            /// UUID of the response task whose latest model run is traced.
            task_id: String
        ) -> ProvenanceResult,
        run_provenance(
            /// UUID of the specific agent run to trace.
            run_id: String
        ) -> ProvenanceResult,
        ai_health() -> AiHealthResult,
        mint_pairing_link() -> PairingLinkResult,
    ],
}

#[cfg(test)]
mod tests {
    use super::*;
    use impress_item::item::ActorKind;
    use impress_service_core::{CliSubcommand, McpToolDescriptor};

    #[test]
    fn all_methods_reach_both_generated_inventories() {
        let mcp: Vec<&str> = McpToolDescriptor::iter().map(|item| item.name).collect();
        let cli: Vec<&str> = CliSubcommand::iter().map(|item| item.name).collect();
        for (mcp_name, cli_name) in [
            ("impress-ai-service_list-models", "list-models"),
            ("impress-ai-service_list-providers", "list-providers"),
            ("impress-ai-service_ai-preferences", "ai-preferences"),
            ("impress-ai-service_select-model", "select-model"),
            (
                "impress-ai-service_set-provider-endpoint",
                "set-provider-endpoint",
            ),
            ("impress-ai-service_provider-health", "provider-health"),
            (
                "impress-ai-service_list-conversations",
                "list-conversations",
            ),
            ("impress-ai-service_get-conversation", "get-conversation"),
            (
                "impress-ai-service_create-conversation",
                "create-conversation",
            ),
            ("impress-ai-service_queue-message", "queue-message"),
            ("impress-ai-service_set-enabled-tools", "set-enabled-tools"),
            ("impress-ai-service_task-status", "task-status"),
            ("impress-ai-service_task-provenance", "task-provenance"),
            ("impress-ai-service_run-provenance", "run-provenance"),
            ("impress-ai-service_ai-health", "ai-health"),
            ("impress-ai-service_mint-pairing-link", "mint-pairing-link"),
        ] {
            assert!(mcp.contains(&mcp_name), "missing {mcp_name}");
            assert!(cli.contains(&cli_name), "missing {cli_name}");
        }
    }

    #[tokio::test]
    async fn provider_and_preference_verbs_share_the_device_registry() {
        let directory = tempfile::tempdir().unwrap();
        let ai = Arc::new(
            AiStore::open_dir(
                &directory.path().join("impress.sqlite"),
                "test:verbs",
                ActorKind::Human,
            )
            .unwrap(),
        );
        let registry = AiRegistry::for_app(directory.path(), crate::InMemoryCredentials::new());
        let service = DefaultImpressAiService::with_components(ai, registry);

        let providers = service.list_providers().await;
        assert_eq!(providers.providers.len(), 8);
        assert_eq!(providers.providers[0].id, "omlx");
        assert!(providers.selected_provider.is_none());
        let anthropic = providers
            .providers
            .iter()
            .find(|row| row.id == "anthropic")
            .unwrap();
        assert_eq!(anthropic.readiness, "needs_credentials");
        assert!(!anthropic.credential_fields[0].configured);

        let rejected = service
            .select_model("omlx".into(), Some("MarkItDown".into()))
            .await;
        assert!(rejected
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("helper"));
        let selected = service
            .select_model("anthropic".into(), Some("claude-sonnet-5".into()))
            .await;
        assert!(selected.error.is_none(), "{:?}", selected.error);
        assert!(selected.path.ends_with("ai/preferences.json"));
        let preferences = service.ai_preferences().await.preferences.unwrap();
        assert_eq!(
            preferences.selected.as_ref().unwrap().model.as_deref(),
            Some("claude-sonnet-5")
        );
        let providers = service.list_providers().await;
        assert_eq!(providers.selected_provider.as_deref(), Some("anthropic"));

        let endpoint = service
            .set_provider_endpoint("omlx".into(), Some("http://laptop:8000/v1".into()))
            .await;
        assert_eq!(
            endpoint.preferences.unwrap().endpoints["omlx"],
            "http://laptop:8000/v1"
        );
        let bad = service
            .set_provider_endpoint("apple-on-device".into(), Some("http://x".into()))
            .await;
        assert!(bad.error.is_some());

        // Static catalogues answer without a network call.
        let models = service.list_models(Some("anthropic".into())).await;
        assert!(models.error.is_none());
        assert!(models
            .models
            .iter()
            .any(|model| model.id == "claude-opus-5"));
        assert_eq!(models.provider.as_deref(), Some("anthropic"));

        let health = service.provider_health(Some("anthropic".into())).await;
        assert_eq!(
            health.health.unwrap().state,
            crate::HealthState::NeedsCredentials {
                fields: vec!["apiKey".into()]
            }
        );
    }

    #[tokio::test]
    async fn queue_and_status_share_the_canonical_graph() {
        let directory = tempfile::tempdir().unwrap();
        let ai = Arc::new(
            AiStore::open_dir(
                &directory.path().join("impress.sqlite"),
                "test:service",
                ActorKind::Human,
            )
            .unwrap(),
        );
        let registry = AiRegistry::for_app(directory.path(), crate::InMemoryCredentials::new());
        let service = DefaultImpressAiService::with_components(ai, registry);
        let created = service
            .create_conversation(
                "Research".into(),
                "local-model".into(),
                None,
                None,
                0.2,
                2048,
                false,
                false,
                vec!["scix".into()],
            )
            .await;
        let conversation_id = created.conversation_id.unwrap();
        assert_eq!(
            service
                .get_conversation(conversation_id.to_string())
                .await
                .conversation
                .unwrap()
                .conversation
                .payload
                .get("provider")
                .and_then(|value| match value {
                    impress_item::item::Value::String(text) => Some(text.as_str()),
                    _ => None,
                }),
            Some("omlx"),
            "nothing configured: the historical default provider is recorded"
        );
        let queued = service
            .queue_message(conversation_id.to_string(), "Find papers".into(), vec![])
            .await;
        assert!(queued.success);
        let status = service
            .task_status(queued.task_id.unwrap().to_string())
            .await;
        assert_eq!(status.task.unwrap().state, "pending");
        assert_eq!(
            service
                .get_conversation(conversation_id.to_string())
                .await
                .conversation
                .unwrap()
                .messages
                .len(),
            1
        );
    }
}
