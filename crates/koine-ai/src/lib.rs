//! Provenance-first AI over the item graph (docs/plan-koine-ai-tasks.md).
//!
//! Every model host — oMLX, Ollama, any OpenAI-compatible server, Anthropic,
//! OpenAI, Google, OpenRouter — is a client behind [`InferenceProvider`],
//! declared once in [`CATALOGUE`]; the device-wide choice is
//! `<workspace>/ai/preferences.json` ([`preferences`]); conversations,
//! queued turns, runs and tool calls are items in whatever store the host
//! has ([`AiStore`], over the store port). Moved from impress-apps'
//! `impress-ai`, which re-exports it (AI-D1).
//!
//! Features: `executor` (the task executors and the desk agent, over
//! `koine-tasks`), `service` (the `impress-ai-service_*` verbs and the
//! model's tools over the linked inventory), `schema` (`JsonSchema` on the
//! DTOs those verbs answer), `legacy-sqlite` (an error variant for a legacy
//! SQLite import).

pub mod agent_loop;
pub mod blob;
pub mod blocking;
pub mod catalogue;
pub mod categories;
#[doc(hidden)]
pub mod conformance;
pub mod credentials;
#[cfg(feature = "executor")]
pub mod desk;
pub mod error;
#[cfg(feature = "executor")]
pub mod executor;
/// The advisory flock, now the pure `impress-fs-lock` crate (ADR-0036 D-R8);
/// re-exported so every `crate::fs_lock::` path still resolves.
pub use impress_fs_lock as fs_lock;
#[cfg(feature = "service")]
pub mod inventory_tools;
pub mod omlx;
pub mod omlx_control;
pub mod preferences;
pub mod presentation;
pub mod provider;
pub mod providers;
pub mod registry;
pub mod research;
pub mod schemas;
pub mod secret;
#[cfg(feature = "service")]
pub mod service;
pub mod sse;
pub mod store;
pub mod tools;
pub mod types;
pub mod worker;

pub use agent_loop::{
    AdapterToolRunner, LoopError, LoopObserver, LoopOutcome, NoObserver, ToolCall, ToolOutcome,
    ToolRunner,
};
pub use blob::{BlobDescriptor, BlobStore, FileBlobStore};
pub use catalogue::{ProviderDescriptor, CATALOGUE};
pub use categories::{TaskCategory, TASK_CATEGORIES};
pub use credentials::{
    CredentialSource, CredentialStatus, EnvCredentials, InMemoryCredentials,
    KeychainCliCredentials, LayeredCredentials,
};
#[cfg(feature = "executor")]
pub use desk::DeskBridge;
pub use error::{Error, Result};
#[cfg(feature = "executor")]
pub use executor::{AiTaskExecutor, AiTitleTaskExecutor, OmlxTaskExecutor};
#[cfg(feature = "service")]
pub use inventory_tools::InventoryTools;
pub use omlx::{EventStream, OmlxClient};
pub use preferences::{
    AiPreferences, CategoryAssignment, LegacyImportReport, LegacyPreferences, ModelRef,
    PreferencesStore, PREFERENCES_VERSION,
};
pub use presentation::{
    tool_options_for_policy, AiAttachmentRow, AiConversationRow, AiConversationView, AiMessageRow,
    AiTaskRow, AiToolInvocationRow, AiToolOption, AiWebSourceRow,
};
pub use provider::{
    collect_stream, Completion, HealthState, InferenceProvider, ProviderHealth, ToolLoop,
};
pub use providers::{AnthropicClient, OpenAiCompatibleClient};
pub use registry::{AiRegistry, ProviderState, ResolutionOrigin, ResolveTarget, ResolvedTarget};
pub use research::{ResearchContext, ResearchContextProvider, ResearchSource, WebResearchProvider};
pub use secret::Secret;
pub use store::{
    AiStore, ConversationDraft, ConversationSnapshot, MessageDraft, PreparedTurn, QueuedTurn,
    RunProvenance, TaskProgress, INFERENCE_TASK_KIND, TITLE_SUGGESTION_TASK_KIND,
};
pub use tools::{PendingToolCall, ToolAdapter};
pub use types::*;
pub use worker::{
    read_worker_status, worker_runtime_directory, write_worker_status, write_worker_status_fast,
    WorkerLease, WorkerLifecycleState, WorkerStatusSnapshot, WORKER_HEARTBEAT_INTERVAL_SECS,
    WORKER_PROTOCOL_VERSION, WORKER_STALE_AFTER_SECS,
};

/// Keep the AI verbs in a binary whose code names nothing else here.
#[cfg(feature = "service")]
pub fn ensure_linked() {
    std::hint::black_box(service::DefaultImpressAiService::with_components as *const ());
}
