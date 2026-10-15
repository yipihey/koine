use std::any::Any;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use crate::schemas::{
    AGENT_RUN_SCHEMA, CONTENT_BLOB_SCHEMA, CONVERSATION_SCHEMA, TASK_SCHEMA, TOOL_INVOCATION_SCHEMA,
};
use base64::Engine;
use chrono::Utc;
use impress_item::item::{ActorKind, Item, ItemId, Priority, Value, Visibility};
use impress_item::query::{ItemQuery, Predicate, SortDescriptor};
use impress_item::reference::{EdgeType, TypedReference};
use impress_item::store::{AttributedStore, FieldMutation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::blob::{BlobDescriptor, BlobStore};
use crate::research::ResearchContext;
use crate::types::{
    ChatRequest, CompletionRecord, ImageUrl, InputAudio, ModelContentPart, ModelMessage, Role,
    StoredAttachment, ToolDefinition, ToolInvocationRecord, ToolPolicy,
};
use crate::{Error, Result};

pub const INFERENCE_TASK_KIND: &str = "impress.ai.respond";
pub const TITLE_SUGGESTION_TASK_KIND: &str = "impress.ai.suggest-title";
use crate::schemas::CHAT_MESSAGE_SCHEMA;

#[derive(Debug, Clone)]
pub struct ConversationDraft {
    pub title: String,
    pub summary: Option<String>,
    pub system_prompt: Option<String>,
    pub provider: String,
    pub model: String,
    pub temperature: f32,
    pub max_tokens: u32,
    pub thinking: bool,
    pub web_access: bool,
    pub tool_policy: ToolPolicy,
}

impl Default for ConversationDraft {
    fn default() -> Self {
        Self {
            title: "New chat".into(),
            summary: None,
            system_prompt: Some("Be precise, thoughtful, and honest about uncertainty.".into()),
            provider: "omlx".into(),
            model: String::new(),
            temperature: 0.2,
            max_tokens: 2048,
            thinking: false,
            web_access: false,
            tool_policy: ToolPolicy::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MessageDraft {
    pub role: Role,
    pub body: String,
    pub from: String,
    pub attachment_ids: Vec<ItemId>,
    pub in_response_to: Option<ItemId>,
}

impl MessageDraft {
    pub fn user(body: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            body: body.into(),
            from: "user:local".into(),
            attachment_ids: vec![],
            in_response_to: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedTurn {
    pub conversation_id: ItemId,
    pub message_id: ItemId,
    pub task_id: ItemId,
}

// The items are canonical graph rows (`impress_item::Item` has no schema);
// a schema reader sees them as free-form JSON.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationSnapshot {
    #[cfg_attr(feature = "schema", schemars(with = "serde_json::Value"))]
    pub conversation: Item,
    #[cfg_attr(feature = "schema", schemars(with = "Vec<serde_json::Value>"))]
    pub messages: Vec<Item>,
    #[cfg_attr(feature = "schema", schemars(with = "Vec<serde_json::Value>"))]
    pub pending_tasks: Vec<Item>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskProgress {
    pub task_id: ItemId,
    pub state: String,
    pub run_id: Option<ItemId>,
    pub response_message_id: Option<ItemId>,
    pub preview_text: Option<String>,
    pub preview_complete: bool,
    pub error: Option<String>,
}

/// Complete durable lineage for one model run. The DTO intentionally carries
/// canonical graph items rather than flattened copies so every caller sees
/// the same ids, references, timestamps, and actor attribution.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunProvenance {
    #[cfg_attr(feature = "schema", schemars(with = "serde_json::Value"))]
    pub run: Item,
    #[cfg_attr(feature = "schema", schemars(with = "serde_json::Value"))]
    pub task: Item,
    #[cfg_attr(feature = "schema", schemars(with = "Vec<serde_json::Value>"))]
    pub inputs: Vec<Item>,
    #[cfg_attr(feature = "schema", schemars(with = "Vec<serde_json::Value>"))]
    pub tool_invocations: Vec<Item>,
    #[cfg_attr(feature = "schema", schemars(with = "Vec<serde_json::Value>"))]
    pub outputs: Vec<Item>,
}

#[derive(Debug, Clone)]
pub struct PreparedTurn {
    pub conversation_id: ItemId,
    pub triggering_message_id: ItemId,
    pub input_message_ids: Vec<ItemId>,
    pub context_item_ids: Vec<ItemId>,
    /// The provider the conversation was created with, when it recorded one.
    /// Executors resolve the actual client from it; `None` means "the
    /// device default".
    pub provider: Option<String>,
    pub request: ChatRequest,
}

/// Shared-store conversation kernel. It owns no second database and no UI
/// state; every durable write is an Impress item or attributed operation.
///
/// It runs over the store port (docs/plan-koine-ai-tasks.md AI-D2): any
/// [`AttributedStore`]. [`AiStore::from_store`] remembers the concrete store
/// it was given, so [`AiStore::backing`] hands a caller that needs the typed
/// handle (a SQL-only rule, a backend's own maintenance) exactly that.
pub struct AiStore {
    store: Arc<dyn AttributedStore>,
    backing: Option<Arc<dyn Any + Send + Sync>>,
    actor: String,
}

impl AiStore {
    /// The kernel over `store`, writing as `actor`.
    pub fn from_store<S: AttributedStore + 'static>(
        store: Arc<S>,
        actor: impl Into<String>,
    ) -> Self {
        let backing: Arc<dyn Any + Send + Sync> = store.clone();
        Self {
            store,
            backing: Some(backing),
            actor: actor.into(),
        }
    }

    /// The kernel over a store known only by the port (the process store).
    pub fn from_port(store: Arc<dyn AttributedStore>, actor: impl Into<String>) -> Self {
        Self {
            store,
            backing: None,
            actor: actor.into(),
        }
    }

    /// The kernel over a directory store at `path` (created when absent): a
    /// durable history with no database, for a host that has no store of
    /// its own.
    pub fn open_dir(path: &Path, actor: impl Into<String>, _actor_kind: ActorKind) -> Result<Self> {
        let store = koine_store_dir::DirStore::open(path)?;
        Ok(Self::from_store(Arc::new(store), actor))
    }

    /// The store, as the port.
    pub fn shared_store(&self) -> Arc<dyn AttributedStore> {
        self.store.clone()
    }

    /// The concrete store [`Self::from_store`] was given, when it is an `S`.
    pub fn backing<S: Any + Send + Sync>(&self) -> Option<Arc<S>> {
        self.backing
            .clone()
            .and_then(|backing| backing.downcast::<S>().ok())
    }

    /// The actor this kernel writes as.
    pub fn actor(&self) -> &str {
        &self.actor
    }

    pub fn create_conversation(&self, draft: ConversationDraft) -> Result<ItemId> {
        validate_conversation(&draft)?;
        let title = normalized_conversation_title(&draft.title)?;
        let now = Utc::now();
        let mut policy = draft.tool_policy;
        if draft.web_access {
            policy.enabled.push("web".into());
        }
        let policy = policy.normalized();
        let mut payload = BTreeMap::new();
        payload.insert("title".into(), Value::String(title));
        payload.insert("state".into(), Value::String("active".into()));
        insert_optional_string(&mut payload, "summary", draft.summary);
        insert_optional_string(&mut payload, "system_prompt", draft.system_prompt);
        payload.insert("provider".into(), Value::String(draft.provider));
        payload.insert("model".into(), Value::String(draft.model));
        payload.insert("temperature".into(), Value::Float(draft.temperature as f64));
        payload.insert("max_tokens".into(), Value::Int(i64::from(draft.max_tokens)));
        payload.insert("thinking".into(), Value::Bool(draft.thinking));
        // `enabled_tools` is the canonical policy. Keep the historical boolean
        // synchronized for older LocalModels/HTTP clients that still read it.
        payload.insert("web_access".into(), Value::Bool(policy.allows("web")));
        payload.insert(
            "enabled_tools".into(),
            Value::Array(policy.enabled.into_iter().map(Value::String).collect()),
        );
        payload.insert("last_activity_at".into(), Value::String(now.to_rfc3339()));
        let item = self.item(CONVERSATION_SCHEMA, payload, None, vec![], ActorKind::Human);
        Ok(self.store.insert(item)?)
    }

    pub fn list_conversations(&self, include_archived: bool) -> Result<Vec<Item>> {
        let mut predicates = Vec::new();
        if !include_archived {
            predicates.push(Predicate::Neq(
                "payload.state".into(),
                Value::String("archived".into()),
            ));
        }
        Ok(self.store.query(&ItemQuery {
            schema: Some(CONVERSATION_SCHEMA),
            predicates,
            sort: vec![SortDescriptor {
                field: "payload.last_activity_at".into(),
                ascending: false,
            }],
            ..Default::default()
        })?)
    }

    pub fn snapshot(&self, conversation_id: ItemId) -> Result<ConversationSnapshot> {
        let conversation = self.require_item(conversation_id, CONVERSATION_SCHEMA)?;
        let messages = self.messages(conversation_id)?;
        let pending_tasks = self.store.query(&ItemQuery {
            schema: Some(TASK_SCHEMA),
            predicates: vec![
                Predicate::HasParent(conversation_id),
                Predicate::In(
                    "payload.state".into(),
                    vec![
                        Value::String("pending".into()),
                        Value::String("running".into()),
                    ],
                ),
            ],
            sort: vec![SortDescriptor {
                field: "created".into(),
                ascending: true,
            }],
            ..Default::default()
        })?;
        Ok(ConversationSnapshot {
            conversation,
            messages,
            pending_tasks,
        })
    }

    pub fn task_progress(&self, task_id: ItemId) -> Result<TaskProgress> {
        let task = self.require_item(task_id, TASK_SCHEMA)?;
        let mut runs = self.store.query(&ItemQuery {
            schema: Some(AGENT_RUN_SCHEMA),
            predicates: vec![Predicate::HasReference(EdgeType::OperatesOn, task_id)],
            sort: vec![SortDescriptor {
                field: "created".into(),
                ascending: false,
            }],
            limit: Some(1),
            ..Default::default()
        })?;
        let run = runs.pop();
        let response_message_id = if let Some(run) = &run {
            self.store
                .query(&ItemQuery {
                    schema: Some(CHAT_MESSAGE_SCHEMA),
                    predicates: vec![Predicate::Eq(
                        "produced_by".into(),
                        Value::String(run.id.to_string()),
                    )],
                    limit: Some(1),
                    ..Default::default()
                })?
                .into_iter()
                .next()
                .map(|message| message.id)
        } else {
            None
        };
        Ok(TaskProgress {
            task_id,
            state: payload_string(&task, "state").unwrap_or_else(|| "unknown".into()),
            run_id: run.as_ref().map(|run| run.id),
            response_message_id,
            preview_text: run
                .as_ref()
                .and_then(|run| payload_string(run, "preview_text")),
            preview_complete: run
                .as_ref()
                .and_then(|run| payload_bool(run, "preview_complete"))
                .unwrap_or(false),
            error: payload_string(&task, "error")
                .or_else(|| run.as_ref().and_then(|run| payload_string(run, "error"))),
        })
    }

    /// Resolve the latest run for a task and return its complete graph
    /// lineage. A pending task legitimately has no provenance yet.
    pub fn task_provenance(&self, task_id: ItemId) -> Result<Option<RunProvenance>> {
        self.require_item(task_id, TASK_SCHEMA)?;
        let run = self
            .store
            .query(&ItemQuery {
                schema: Some(AGENT_RUN_SCHEMA),
                predicates: vec![Predicate::HasReference(EdgeType::OperatesOn, task_id)],
                sort: vec![SortDescriptor {
                    field: "created".into(),
                    ascending: false,
                }],
                limit: Some(1),
                ..Default::default()
            })?
            .into_iter()
            .next();
        run.map(|run| self.run_provenance(run.id)).transpose()
    }

    /// Return the inputs, tool calls, and outputs attributed to an agent run.
    pub fn run_provenance(&self, run_id: ItemId) -> Result<RunProvenance> {
        let run = self.require_item(run_id, AGENT_RUN_SCHEMA)?;
        let task_id = run
            .references
            .iter()
            .find(|reference| reference.edge_type == EdgeType::OperatesOn)
            .map(|reference| reference.target)
            .ok_or_else(|| Error::Store(format!("agent run {run_id} has no task reference")))?;
        let task = self.require_item(task_id, TASK_SCHEMA)?;

        let mut inputs = Vec::new();
        for reference in run
            .references
            .iter()
            .filter(|reference| reference.edge_type == EdgeType::DerivedFrom)
        {
            if let Some(item) = self.store.get(reference.target)? {
                inputs.push(item);
            }
        }

        let produced = self
            .store
            .query(&ItemQuery {
                predicates: vec![Predicate::Eq(
                    "produced_by".into(),
                    Value::String(run_id.to_string()),
                )],
                sort: vec![SortDescriptor {
                    field: "created".into(),
                    ascending: true,
                }],
                ..Default::default()
            })?
            .into_iter();
        let (tool_invocations, outputs): (Vec<_>, Vec<_>) =
            produced.partition(|item| item.schema == TOOL_INVOCATION_SCHEMA);

        Ok(RunProvenance {
            run,
            task,
            inputs,
            tool_invocations,
            outputs,
        })
    }

    pub fn set_tool_policy(&self, conversation_id: ItemId, policy: ToolPolicy) -> Result<()> {
        self.require_item(conversation_id, CONVERSATION_SCHEMA)?;
        let policy = policy.normalized();
        let web_access = policy.allows("web");
        let value = Value::Array(policy.enabled.into_iter().map(Value::String).collect());
        self.store.update(
            conversation_id,
            vec![
                FieldMutation::SetPayload("enabled_tools".into(), value),
                FieldMutation::SetPayload("web_access".into(), Value::Bool(web_access)),
            ],
        )?;
        Ok(())
    }

    /// Change the default model for future turns in a conversation. Existing
    /// agent-run records retain the exact model they used, so this preference
    /// update does not rewrite execution provenance.
    pub fn set_conversation_model(&self, conversation_id: ItemId, model: String) -> Result<()> {
        self.require_item(conversation_id, CONVERSATION_SCHEMA)?;
        let model = model.trim();
        if model.is_empty() {
            return Err(Error::Invalid("choose a valid conversation model".into()));
        }
        self.store.update(
            conversation_id,
            vec![FieldMutation::SetPayload(
                "model".into(),
                Value::String(model.into()),
            )],
        )?;
        Ok(())
    }

    /// Rename a conversation without rewriting any messages or model-run
    /// provenance. The shared store operation remains the auditable mutation.
    pub fn set_conversation_title(&self, conversation_id: ItemId, title: String) -> Result<()> {
        self.require_item(conversation_id, CONVERSATION_SCHEMA)?;
        let title = normalized_conversation_title(&title)?;
        self.store.update(
            conversation_id,
            vec![FieldMutation::SetPayload(
                "title".into(),
                Value::String(title),
            )],
        )?;
        self.touch_conversation(conversation_id)?;
        Ok(())
    }

    /// Queue one provenance-bearing local-model title suggestion. Capturing
    /// the current title gives completion optimistic concurrency semantics: a
    /// human rename made while the model is working always wins.
    pub fn queue_title_suggestion(&self, conversation_id: ItemId) -> Result<ItemId> {
        self.queue_title_suggestion_as(conversation_id, ActorKind::Human)
    }

    /// Automatically queue a title only while a conversation still has a
    /// known placeholder. Existing human or model titles are never replaced.
    pub fn queue_title_suggestion_if_placeholder(
        &self,
        conversation_id: ItemId,
    ) -> Result<Option<ItemId>> {
        let conversation = self.require_item(conversation_id, CONVERSATION_SCHEMA)?;
        let title = payload_string(&conversation, "title").unwrap_or_default();
        if !is_placeholder_conversation_title(&title) {
            return Ok(None);
        }
        self.queue_title_suggestion_as(conversation_id, ActorKind::Agent)
            .map(Some)
    }

    fn queue_title_suggestion_as(
        &self,
        conversation_id: ItemId,
        actor_kind: ActorKind,
    ) -> Result<ItemId> {
        let conversation = self.require_item(conversation_id, CONVERSATION_SCHEMA)?;
        if !self.messages(conversation_id)?.iter().any(|message| {
            payload_string(message, "body").is_some_and(|body| !body.trim().is_empty())
        }) {
            return Err(Error::Invalid(
                "send a message before asking the model to name this conversation".into(),
            ));
        }
        if let Some(task) = self
            .store
            .query(&ItemQuery {
                schema: Some(TASK_SCHEMA),
                predicates: vec![
                    Predicate::HasParent(conversation_id),
                    Predicate::Eq(
                        "payload.task_kind".into(),
                        Value::String(TITLE_SUGGESTION_TASK_KIND.into()),
                    ),
                    Predicate::In(
                        "payload.state".into(),
                        vec![
                            Value::String("pending".into()),
                            Value::String("running".into()),
                        ],
                    ),
                ],
                limit: Some(1),
                ..Default::default()
            })?
            .into_iter()
            .next()
        {
            return Ok(task.id);
        }

        let expected_title = payload_string(&conversation, "title").unwrap_or_default();
        let mut payload = BTreeMap::new();
        payload.insert(
            "title".into(),
            Value::String("Suggest conversation title".into()),
        );
        payload.insert(
            "task_kind".into(),
            Value::String(TITLE_SUGGESTION_TASK_KIND.into()),
        );
        payload.insert("state".into(), Value::String("pending".into()));
        payload.insert("source_app".into(), Value::String("impart".into()));
        payload.insert(
            "output_schema".into(),
            Value::String(CONVERSATION_SCHEMA.into()),
        );
        payload.insert("expected_title".into(), Value::String(expected_title));
        let task = self.item(
            TASK_SCHEMA,
            payload,
            Some(conversation_id),
            vec![TypedReference {
                target: conversation_id,
                edge_type: EdgeType::OperatesOn,
                metadata: None,
            }],
            actor_kind,
        );
        let task_id = task.id;
        self.store.insert(task)?;
        Ok(task_id)
    }

    /// Append a user message and a dispatchable model task in one transaction.
    /// The task is what survives an offline iPhone: CloudKit can deliver it to
    /// the model host later without the phone holding an HTTP connection open.
    pub fn queue_user_turn(
        &self,
        conversation_id: ItemId,
        mut draft: MessageDraft,
    ) -> Result<QueuedTurn> {
        self.require_item(conversation_id, CONVERSATION_SCHEMA)?;
        if draft.role != Role::User {
            return Err(Error::Invalid("queued turns must have role=user".into()));
        }
        if draft.body.trim().is_empty() && draft.attachment_ids.is_empty() {
            return Err(Error::Invalid(
                "a user turn needs text or at least one attachment".into(),
            ));
        }
        self.validate_attachments(&draft.attachment_ids)?;
        draft.attachment_ids.sort();
        draft.attachment_ids.dedup();
        let sequence = self.messages(conversation_id)?.len() as i64;
        let message_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();
        let message = self.message_item(
            message_id,
            conversation_id,
            &draft,
            sequence,
            "queued",
            None,
            None,
        );
        let mut task_payload = BTreeMap::new();
        task_payload.insert("title".into(), Value::String("Generate response".into()));
        task_payload.insert(
            "task_kind".into(),
            Value::String(INFERENCE_TASK_KIND.into()),
        );
        task_payload.insert("state".into(), Value::String("pending".into()));
        task_payload.insert("source_app".into(), Value::String("impart".into()));
        task_payload.insert(
            "output_schema".into(),
            Value::String(CHAT_MESSAGE_SCHEMA.into()),
        );
        let mut task = self.item(
            TASK_SCHEMA,
            task_payload,
            Some(conversation_id),
            vec![TypedReference {
                target: message_id,
                edge_type: EdgeType::OperatesOn,
                metadata: None,
            }],
            ActorKind::Human,
        );
        task.id = task_id;
        self.store.insert_batch(vec![message, task])?;
        self.touch_conversation(conversation_id)?;
        Ok(QueuedTurn {
            conversation_id,
            message_id,
            task_id,
        })
    }

    pub fn append_message(&self, conversation_id: ItemId, draft: MessageDraft) -> Result<ItemId> {
        self.require_item(conversation_id, CONVERSATION_SCHEMA)?;
        self.validate_attachments(&draft.attachment_ids)?;
        let sequence = self.messages(conversation_id)?.len() as i64;
        let id = Uuid::new_v4();
        let item = self.message_item(
            id,
            conversation_id,
            &draft,
            sequence,
            "complete",
            None,
            None,
        );
        self.store.insert(item)?;
        self.touch_conversation(conversation_id)?;
        Ok(id)
    }

    pub fn ingest_blob(
        &self,
        blob_store: &dyn BlobStore,
        bytes: &[u8],
        mime_type: impl Into<String>,
        file_name: Option<String>,
    ) -> Result<StoredAttachment> {
        self.ingest_blob_as(blob_store, bytes, mime_type, file_name, ActorKind::Human)
    }

    fn ingest_blob_as(
        &self,
        blob_store: &dyn BlobStore,
        bytes: &[u8],
        mime_type: impl Into<String>,
        file_name: Option<String>,
        actor_kind: ActorKind,
    ) -> Result<StoredAttachment> {
        if bytes.is_empty() {
            return Err(Error::Invalid("attachment is empty".into()));
        }
        let mime_type = mime_type.into();
        if mime_type.trim().is_empty() {
            return Err(Error::Invalid("attachment MIME type is required".into()));
        }
        let descriptor = blob_store.put(bytes)?;
        let mut payload = BTreeMap::new();
        payload.insert("sha256".into(), Value::String(descriptor.sha256.clone()));
        payload.insert("mime_type".into(), Value::String(mime_type.clone()));
        payload.insert(
            "byte_length".into(),
            Value::Int(
                descriptor
                    .byte_length
                    .try_into()
                    .map_err(|_| Error::Invalid("attachment is too large to represent".into()))?,
            ),
        );
        payload.insert(
            "storage_kind".into(),
            Value::String(descriptor.storage_kind),
        );
        payload.insert("locator".into(), Value::String(descriptor.locator));
        insert_optional_string(&mut payload, "file_name", file_name.clone());
        let item = self.item(CONTENT_BLOB_SCHEMA, payload, None, vec![], actor_kind);
        let item_id = item.id;
        self.store.insert(item)?;
        Ok(StoredAttachment {
            item_id,
            mime_type,
            sha256: descriptor.sha256,
            file_name,
        })
    }

    /// Persist live context before inference and attach only a marked prompt
    /// projection to the endpoint request. Source content remains independently
    /// addressable through a web-page artifact and hash-verified text blob.
    pub fn attach_research_context(
        &self,
        prepared: &mut PreparedTurn,
        blob_store: &dyn BlobStore,
        context: &ResearchContext,
    ) -> Result<Vec<ItemId>> {
        let mut artifact_ids = Vec::new();
        for source in &context.sources {
            let source_hash = format!("{:x}", Sha256::digest(source.content.as_bytes()));
            if let Some(artifact_id) = self.research_artifact(
                prepared.conversation_id,
                prepared.triggering_message_id,
                &source.url,
                &source_hash,
            )? {
                artifact_ids.push(artifact_id);
                continue;
            }
            let blob = self.ingest_blob_as(
                blob_store,
                source.content.as_bytes(),
                "text/plain; charset=utf-8",
                None,
                ActorKind::Agent,
            )?;
            let mut payload = BTreeMap::new();
            payload.insert("title".into(), Value::String(source.title.clone()));
            payload.insert("source_url".into(), Value::String(source.url.clone()));
            payload.insert(
                "capture_context".into(),
                Value::String(format!(
                    "live AI research context captured {}",
                    Utc::now().to_rfc3339()
                )),
            );
            let artifact = self.item(
                impress_item::schema::refs::IMPRESS_ARTIFACT_WEBPAGE,
                payload,
                Some(prepared.conversation_id),
                vec![
                    TypedReference {
                        target: blob.item_id,
                        edge_type: EdgeType::Attaches,
                        metadata: None,
                    },
                    TypedReference {
                        target: prepared.triggering_message_id,
                        edge_type: EdgeType::RelatesTo,
                        metadata: None,
                    },
                ],
                ActorKind::Agent,
            );
            artifact_ids.push(self.store.insert(artifact)?);
        }
        if let Some(block) = context.prompt_block() {
            if let Some(message) = prepared
                .request
                .messages
                .iter_mut()
                .rfind(|message| message.role == Role::User)
            {
                message.content.push(ModelContentPart::Text { text: block });
            }
        }
        prepared
            .context_item_ids
            .extend(artifact_ids.iter().copied());
        Ok(artifact_ids)
    }

    pub fn prepare_request(
        &self,
        task_id: ItemId,
        blob_store: &dyn BlobStore,
        tool_catalog: &BTreeMap<String, Vec<ToolDefinition>>,
    ) -> Result<PreparedTurn> {
        let task = self.require_item(task_id, TASK_SCHEMA)?;
        if payload_string(&task, "task_kind").as_deref() != Some(INFERENCE_TASK_KIND) {
            return Err(Error::Invalid(format!(
                "task {task_id} is not an Impress AI response task"
            )));
        }
        let triggering_message_id = task
            .references
            .iter()
            .find(|reference| reference.edge_type == EdgeType::OperatesOn)
            .map(|reference| reference.target)
            .ok_or_else(|| Error::Invalid("inference task has no triggering message".into()))?;
        let triggering = self.require_item(triggering_message_id, CHAT_MESSAGE_SCHEMA)?;
        let conversation_id = triggering.parent.ok_or_else(|| {
            Error::Invalid("triggering message has no conversation parent".into())
        })?;
        let conversation = self.require_item(conversation_id, CONVERSATION_SCHEMA)?;
        let stored_messages = self.messages(conversation_id)?;
        let mut messages = Vec::new();
        if let Some(system) = payload_string(&conversation, "system_prompt") {
            if !system.is_empty() {
                messages.push(ModelMessage::text(Role::System, system));
            }
        }
        let mut input_message_ids = Vec::new();
        for item in stored_messages {
            // Do not feed partial/failed future responses back into the model.
            if item.created > triggering.created {
                continue;
            }
            let role = parse_role(payload_string(&item, "role").as_deref())?;
            let mut content = Vec::new();
            if let Some(body) = payload_string(&item, "body") {
                if !body.is_empty() {
                    content.push(ModelContentPart::Text { text: body });
                }
            }
            for reference in &item.references {
                if reference.edge_type != EdgeType::Attaches {
                    continue;
                }
                let blob = self.require_item(reference.target, CONTENT_BLOB_SCHEMA)?;
                content.extend(resolve_blob(&blob, blob_store)?);
            }
            if content.is_empty() {
                continue;
            }
            input_message_ids.push(item.id);
            messages.push(ModelMessage {
                role,
                content,
                name: None,
                tool_call_id: None,
                tool_calls: vec![],
            });
        }
        let mut enabled_tools = payload_strings(&conversation, "enabled_tools");
        if payload_bool(&conversation, "web_access").unwrap_or(false)
            && !enabled_tools.iter().any(|tool| tool == "web")
        {
            enabled_tools.push("web".into());
        }
        let tool_policy = ToolPolicy {
            enabled: enabled_tools,
        }
        .normalized();
        let tools = tool_policy
            .enabled
            .iter()
            .filter_map(|capability| tool_catalog.get(capability))
            .flatten()
            .cloned()
            .collect();
        let model = payload_string(&conversation, "model").unwrap_or_default();
        let request = ChatRequest {
            model,
            messages,
            temperature: Some(payload_f64(&conversation, "temperature").unwrap_or(0.2) as f32),
            max_tokens: payload_i64(&conversation, "max_tokens")
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(2048),
            thinking: payload_bool(&conversation, "thinking").unwrap_or(false),
            tools,
            tool_policy,
            ..Default::default()
        };
        request.validate()?;
        Ok(PreparedTurn {
            conversation_id,
            triggering_message_id,
            input_message_ids,
            context_item_ids: vec![],
            provider: payload_string(&conversation, "provider"),
            request,
        })
    }

    /// Build the small, tool-free request used by the title worker. The
    /// transcript is bounded, while the run's DerivedFrom edges retain the
    /// exact canonical message inputs used to build it.
    pub fn prepare_title_request(&self, task_id: ItemId) -> Result<PreparedTurn> {
        let task = self.require_item(task_id, TASK_SCHEMA)?;
        if payload_string(&task, "task_kind").as_deref() != Some(TITLE_SUGGESTION_TASK_KIND) {
            return Err(Error::Invalid(format!(
                "task {task_id} is not an Impress AI title-suggestion task"
            )));
        }
        let conversation_id = task.parent.ok_or_else(|| {
            Error::Invalid("title-suggestion task has no conversation parent".into())
        })?;
        let conversation = self.require_item(conversation_id, CONVERSATION_SCHEMA)?;
        let stored_messages = self.messages(conversation_id)?;
        let selected = stored_messages
            .iter()
            .rev()
            .filter(|message| {
                payload_string(message, "body").is_some_and(|body| !body.trim().is_empty())
            })
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>();
        let triggering_message_id = selected.last().map(|message| message.id).ok_or_else(|| {
            Error::Invalid("a title suggestion needs at least one message".into())
        })?;
        let input_message_ids = selected
            .iter()
            .map(|message| message.id)
            .collect::<Vec<_>>();
        let transcript = selected
            .iter()
            .map(|message| {
                let role = payload_string(message, "role").unwrap_or_else(|| "message".into());
                let body = payload_string(message, "body").unwrap_or_default();
                format!("{role}: {body}")
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let transcript: String = transcript.chars().take(6_000).collect();
        let request = ChatRequest {
            model: payload_string(&conversation, "model").unwrap_or_default(),
            messages: vec![
                ModelMessage::text(
                    Role::System,
                    "Name this research chat with a concise, specific title. Return only the title: 3 to 8 words, no quotation marks, no label, and no trailing punctuation. Preserve important technical terms.",
                ),
                ModelMessage::text(Role::User, transcript),
            ],
            temperature: Some(0.1),
            max_tokens: 48,
            thinking: false,
            tools: vec![],
            tool_policy: ToolPolicy::default(),
            ..Default::default()
        };
        request.validate()?;
        Ok(PreparedTurn {
            conversation_id,
            triggering_message_id,
            input_message_ids,
            context_item_ids: vec![],
            provider: payload_string(&conversation, "provider"),
            request,
        })
    }

    pub fn record_run_start(
        &self,
        task_id: ItemId,
        prepared: &PreparedTurn,
        provider: &str,
        endpoint: &str,
    ) -> Result<ItemId> {
        self.require_item(task_id, TASK_SCHEMA)?;
        let request_json = serde_json::to_vec(&prepared.request)?;
        let request_hash = format!("{:x}", Sha256::digest(&request_json));
        if let Some(run_id) = self.running_run(task_id, &request_hash)? {
            return Ok(run_id);
        }
        let prompt_hash = prepared
            .request
            .messages
            .first()
            .filter(|message| message.role == Role::System)
            .map(|message| serde_json::to_vec(message).unwrap_or_default())
            .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
            .unwrap_or_else(|| format!("{:x}", Sha256::digest([])));
        let mut payload = BTreeMap::new();
        payload.insert("agent_id".into(), Value::String("impart-counsel".into()));
        payload.insert(
            "model".into(),
            Value::String(prepared.request.model.clone()),
        );
        payload.insert("prompt_hash".into(), Value::String(prompt_hash));
        payload.insert("provider".into(), Value::String(provider.into()));
        payload.insert("endpoint".into(), Value::String(endpoint.into()));
        payload.insert("request_hash".into(), Value::String(request_hash));
        payload.insert("status".into(), Value::String("running".into()));
        payload.insert("started_at".into(), Value::String(Utc::now().to_rfc3339()));
        payload.insert(
            "enabled_tools".into(),
            Value::Array(
                prepared
                    .request
                    .tool_policy
                    .enabled
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
        // Honesty about degradation: `enabled_tools` records what the
        // POLICY asked for; this records what the model was actually shown
        // after intersecting with the live catalog. A run that answered
        // tool-less because its app was closed is now visibly degraded in
        // provenance instead of affirmatively looking tool-enabled.
        payload.insert(
            "available_tools".into(),
            Value::Array(
                prepared
                    .request
                    .tools
                    .iter()
                    .map(|tool| Value::String(tool.name.clone()))
                    .collect(),
            ),
        );
        payload.insert(
            "parameters".into(),
            Value::Object(BTreeMap::from([
                (
                    "temperature".into(),
                    prepared
                        .request
                        .temperature
                        .map(|temperature| Value::Float(temperature as f64))
                        .unwrap_or(Value::Null),
                ),
                (
                    "max_tokens".into(),
                    Value::Int(i64::from(prepared.request.max_tokens)),
                ),
                ("thinking".into(), Value::Bool(prepared.request.thinking)),
            ])),
        );
        let mut references = vec![TypedReference {
            target: task_id,
            edge_type: EdgeType::OperatesOn,
            metadata: None,
        }];
        references.extend(prepared.input_message_ids.iter().map(|id| TypedReference {
            target: *id,
            edge_type: EdgeType::DerivedFrom,
            metadata: None,
        }));
        references.extend(prepared.context_item_ids.iter().map(|id| TypedReference {
            target: *id,
            edge_type: EdgeType::DerivedFrom,
            metadata: None,
        }));
        let item = self.item(
            AGENT_RUN_SCHEMA,
            payload,
            Some(prepared.conversation_id),
            references,
            ActorKind::Agent,
        );
        let run_id = self.store.insert(item)?;
        self.store.update(
            task_id,
            vec![FieldMutation::AddReference(TypedReference {
                target: run_id,
                edge_type: EdgeType::ProducedBy,
                metadata: None,
            })],
        )?;
        Ok(run_id)
    }

    /// Crash/retry idempotency guard for the scheduler. A completed run means
    /// its assistant message was committed before the run status operation;
    /// re-executing the still-running task would create a duplicate response.
    pub fn has_completed_run(&self, task_id: ItemId) -> Result<bool> {
        Ok(self.store.count(&ItemQuery {
            schema: Some(AGENT_RUN_SCHEMA),
            predicates: vec![
                Predicate::HasReference(EdgeType::OperatesOn, task_id),
                Predicate::Eq("payload.status".into(), Value::String("completed".into())),
            ],
            limit: Some(1),
            ..Default::default()
        })? > 0)
    }

    /// Publish one bounded, display-only response preview on the canonical
    /// run. The final assistant message remains the authoritative output.
    pub fn record_run_preview(
        &self,
        run_id: ItemId,
        text: &str,
        paragraph_complete: bool,
    ) -> Result<()> {
        self.require_item(run_id, AGENT_RUN_SCHEMA)?;
        let preview = text.trim();
        if preview.is_empty() {
            return Err(Error::Invalid("response preview cannot be empty".into()));
        }
        let preview: String = preview.chars().take(4_096).collect();
        self.store.update(
            run_id,
            vec![
                FieldMutation::SetPayload("preview_text".into(), Value::String(preview)),
                FieldMutation::SetPayload(
                    "preview_complete".into(),
                    Value::Bool(paragraph_complete),
                ),
                FieldMutation::SetPayload(
                    "preview_updated_at".into(),
                    Value::String(Utc::now().to_rfc3339()),
                ),
            ],
        )?;
        Ok(())
    }

    pub fn record_completion(
        &self,
        prepared: &PreparedTurn,
        run_id: ItemId,
        completion: CompletionRecord,
    ) -> Result<ItemId> {
        self.require_item(run_id, AGENT_RUN_SCHEMA)?;
        let triggering = self.require_item(prepared.triggering_message_id, CHAT_MESSAGE_SCHEMA)?;
        let existing_response = self
            .messages(prepared.conversation_id)?
            .into_iter()
            .find(|message| message.produced_by == Some(run_id));
        let id = if let Some(message) = existing_response {
            message.id
        } else {
            let sequence = self.messages(prepared.conversation_id)?.len() as i64;
            let draft = MessageDraft {
                role: Role::Assistant,
                body: completion.content.clone(),
                from: "agent:impart-counsel".into(),
                attachment_ids: vec![],
                in_response_to: Some(triggering.id),
            };
            let id = Uuid::new_v4();
            let message = self.message_item(
                id,
                prepared.conversation_id,
                &draft,
                sequence,
                "complete",
                Some(run_id),
                Some((&completion.reasoning, &prepared.request.model)),
            );
            self.store.insert(message)?;
            id
        };

        let mut updates = vec![
            FieldMutation::SetPayload("status".into(), Value::String("completed".into())),
            FieldMutation::SetPayload("finished_at".into(), Value::String(Utc::now().to_rfc3339())),
        ];
        let result_summary: String = completion.content.chars().take(500).collect();
        if !result_summary.is_empty() {
            updates.push(FieldMutation::SetPayload(
                "result_summary".into(),
                Value::String(result_summary),
            ));
        }
        if let Some(duration_ms) = completion.duration_ms {
            updates.push(FieldMutation::SetPayload(
                "duration_ms".into(),
                Value::Int(i64::try_from(duration_ms).unwrap_or(i64::MAX)),
            ));
        }
        if let Some(reason) = completion.finish_reason {
            updates.push(FieldMutation::SetPayload(
                "finish_reason".into(),
                Value::String(reason),
            ));
        }
        if let Some(usage) = completion.usage {
            let input_tokens = json_token_count(&usage, &["prompt_tokens", "input_tokens"]);
            let output_tokens = json_token_count(&usage, &["completion_tokens", "output_tokens"]);
            if let Some(count) = input_tokens {
                updates.push(FieldMutation::SetPayload(
                    "input_token_count".into(),
                    Value::Int(count),
                ));
            }
            if let Some(count) = output_tokens {
                updates.push(FieldMutation::SetPayload(
                    "output_token_count".into(),
                    Value::Int(count),
                ));
            }
            if let Some(count) = input_tokens
                .zip(output_tokens)
                .map(|(input, output)| input + output)
            {
                updates.push(FieldMutation::SetPayload(
                    "token_count".into(),
                    Value::Int(count),
                ));
            }
            updates.push(FieldMutation::SetPayload(
                "usage".into(),
                json_to_value(usage),
            ));
        }
        self.store.update(run_id, updates)?;
        self.store.update(
            prepared.triggering_message_id,
            vec![FieldMutation::SetPayload(
                "status".into(),
                Value::String("complete".into()),
            )],
        )?;
        self.touch_conversation(prepared.conversation_id)?;
        Ok(id)
    }

    /// Commit a model-generated title and complete its distinct agent run.
    /// If the title changed after the task was queued, provenance is retained
    /// but the stale suggestion is deliberately not applied.
    pub fn record_title_completion(
        &self,
        task_id: ItemId,
        prepared: &PreparedTurn,
        run_id: ItemId,
        title: String,
        completion: CompletionRecord,
    ) -> Result<bool> {
        let task = self.require_item(task_id, TASK_SCHEMA)?;
        self.require_item(run_id, AGENT_RUN_SCHEMA)?;
        let title = normalized_conversation_title(&title)?;
        let conversation = self.require_item(prepared.conversation_id, CONVERSATION_SCHEMA)?;
        let expected_title = payload_string(&task, "expected_title").unwrap_or_default();
        let current_title = payload_string(&conversation, "title").unwrap_or_default();
        let applied = current_title == expected_title;
        if applied {
            self.store.update(
                prepared.conversation_id,
                vec![FieldMutation::SetPayload(
                    "title".into(),
                    Value::String(title.clone()),
                )],
            )?;
            self.touch_conversation(prepared.conversation_id)?;
        }

        let mut updates = vec![
            FieldMutation::SetPayload("status".into(), Value::String("completed".into())),
            FieldMutation::SetPayload("finished_at".into(), Value::String(Utc::now().to_rfc3339())),
            FieldMutation::SetPayload("result_summary".into(), Value::String(title)),
            FieldMutation::SetPayload("result_applied".into(), Value::Bool(applied)),
        ];
        if let Some(duration_ms) = completion.duration_ms {
            updates.push(FieldMutation::SetPayload(
                "duration_ms".into(),
                Value::Int(i64::try_from(duration_ms).unwrap_or(i64::MAX)),
            ));
        }
        if let Some(reason) = completion.finish_reason {
            updates.push(FieldMutation::SetPayload(
                "finish_reason".into(),
                Value::String(reason),
            ));
        }
        if let Some(usage) = completion.usage {
            let input_tokens = json_token_count(&usage, &["prompt_tokens", "input_tokens"]);
            let output_tokens = json_token_count(&usage, &["completion_tokens", "output_tokens"]);
            if let Some(count) = input_tokens {
                updates.push(FieldMutation::SetPayload(
                    "input_token_count".into(),
                    Value::Int(count),
                ));
            }
            if let Some(count) = output_tokens {
                updates.push(FieldMutation::SetPayload(
                    "output_token_count".into(),
                    Value::Int(count),
                ));
            }
            if let Some(count) = input_tokens.zip(output_tokens).map(|(a, b)| a + b) {
                updates.push(FieldMutation::SetPayload(
                    "token_count".into(),
                    Value::Int(count),
                ));
            }
            updates.push(FieldMutation::SetPayload(
                "usage".into(),
                json_to_value(usage),
            ));
        }
        self.store.update(run_id, updates)?;
        Ok(applied)
    }

    pub fn record_run_failure(&self, run_id: ItemId, error: &str) -> Result<()> {
        self.require_item(run_id, AGENT_RUN_SCHEMA)?;
        self.store.update(
            run_id,
            vec![
                FieldMutation::SetPayload("status".into(), Value::String("failed".into())),
                FieldMutation::SetPayload("error".into(), Value::String(error.into())),
                FieldMutation::SetPayload(
                    "finished_at".into(),
                    Value::String(Utc::now().to_rfc3339()),
                ),
            ],
        )?;
        Ok(())
    }

    pub fn record_tool_invocation(
        &self,
        run_id: ItemId,
        task_id: ItemId,
        invocation: ToolInvocationRecord,
    ) -> Result<ItemId> {
        self.require_item(run_id, AGENT_RUN_SCHEMA)?;
        let mut payload = BTreeMap::new();
        payload.insert("tool".into(), Value::String(invocation.tool));
        payload.insert("provider".into(), Value::String(invocation.provider));
        payload.insert(
            "state".into(),
            Value::String(if invocation.error.is_some() {
                "failed".into()
            } else {
                "completed".into()
            }),
        );
        payload.insert("arguments".into(), Value::Object(invocation.arguments));
        if let Some(result) = invocation.result {
            payload.insert("result".into(), Value::Object(result));
        }
        insert_optional_string(&mut payload, "result_summary", invocation.result_summary);
        insert_optional_string(&mut payload, "error", invocation.error);
        let now = Utc::now().to_rfc3339();
        payload.insert("started_at".into(), Value::String(now.clone()));
        payload.insert("finished_at".into(), Value::String(now));
        if let Some(duration_ms) = invocation.duration_ms {
            payload.insert(
                "duration_ms".into(),
                Value::Int(i64::try_from(duration_ms).unwrap_or(i64::MAX)),
            );
        }
        let tool_name = payload_string_from_map(&payload, "tool").unwrap_or_default();
        let mut item = self.item(
            TOOL_INVOCATION_SCHEMA,
            payload,
            Some(run_id),
            vec![
                TypedReference {
                    target: task_id,
                    edge_type: EdgeType::OperatesOn,
                    metadata: None,
                },
                TypedReference {
                    target: run_id,
                    edge_type: EdgeType::ProducedBy,
                    metadata: None,
                },
            ],
            ActorKind::Agent,
        );
        item.produced_by = Some(run_id);
        let invocation_id = self.store.insert(item)?;
        let run = self.require_item(run_id, AGENT_RUN_SCHEMA)?;
        let mut tool_calls = payload_strings(&run, "tool_calls");
        tool_calls.push(tool_name);
        self.store.update(
            run_id,
            vec![FieldMutation::SetPayload(
                "tool_calls".into(),
                Value::Array(tool_calls.into_iter().map(Value::String).collect()),
            )],
        )?;
        Ok(invocation_id)
    }

    fn messages(&self, conversation_id: ItemId) -> Result<Vec<Item>> {
        Ok(self.store.query(&ItemQuery {
            schema: Some(CHAT_MESSAGE_SCHEMA),
            predicates: vec![Predicate::HasParent(conversation_id)],
            sort: vec![SortDescriptor {
                field: "payload.sequence".into(),
                ascending: true,
            }],
            ..Default::default()
        })?)
    }

    fn touch_conversation(&self, conversation_id: ItemId) -> Result<()> {
        self.store.update(
            conversation_id,
            vec![FieldMutation::SetPayload(
                "last_activity_at".into(),
                Value::String(Utc::now().to_rfc3339()),
            )],
        )?;
        Ok(())
    }

    fn validate_attachments(&self, ids: &[ItemId]) -> Result<()> {
        for id in ids {
            self.require_item(*id, CONTENT_BLOB_SCHEMA)?;
        }
        Ok(())
    }

    fn running_run(&self, task_id: ItemId, request_hash: &str) -> Result<Option<ItemId>> {
        Ok(self
            .store
            .query(&ItemQuery {
                schema: Some(AGENT_RUN_SCHEMA),
                predicates: vec![
                    Predicate::HasReference(EdgeType::OperatesOn, task_id),
                    Predicate::Eq("payload.status".into(), Value::String("running".into())),
                    Predicate::Eq(
                        "payload.request_hash".into(),
                        Value::String(request_hash.into()),
                    ),
                ],
                limit: Some(1),
                ..Default::default()
            })?
            .into_iter()
            .next()
            .map(|run| run.id))
    }

    fn research_artifact(
        &self,
        conversation_id: ItemId,
        triggering_message_id: ItemId,
        source_url: &str,
        source_hash: &str,
    ) -> Result<Option<ItemId>> {
        let candidates = self.store.query(&ItemQuery {
            schema: Some(impress_item::schema::refs::IMPRESS_ARTIFACT_WEBPAGE),
            predicates: vec![
                Predicate::HasParent(conversation_id),
                Predicate::Eq(
                    "payload.source_url".into(),
                    Value::String(source_url.into()),
                ),
            ],
            ..Default::default()
        })?;
        for artifact in candidates {
            let same_turn = artifact.references.iter().any(|reference| {
                reference.edge_type == EdgeType::RelatesTo
                    && reference.target == triggering_message_id
            });
            if !same_turn {
                continue;
            }
            for reference in artifact
                .references
                .iter()
                .filter(|reference| reference.edge_type == EdgeType::Attaches)
            {
                let Some(blob) = self.store.get(reference.target)? else {
                    continue;
                };
                if payload_string(&blob, "sha256").as_deref() == Some(source_hash) {
                    return Ok(Some(artifact.id));
                }
            }
        }
        Ok(None)
    }

    fn require_item(&self, id: ItemId, schema: impress_item::SchemaRef) -> Result<Item> {
        let item = self
            .store
            .get(id)?
            .ok_or_else(|| Error::Invalid(format!("item {id} does not exist")))?;
        if item.schema != schema {
            return Err(Error::Invalid(format!(
                "item {id} has schema '{}', expected '{schema}'",
                item.schema
            )));
        }
        Ok(item)
    }

    fn item(
        &self,
        schema: impress_item::SchemaRef,
        payload: BTreeMap<String, Value>,
        parent: Option<ItemId>,
        references: Vec<TypedReference>,
        author_kind: ActorKind,
    ) -> Item {
        let now = Utc::now();
        Item {
            id: Uuid::new_v4(),
            schema,
            payload,
            created: now,
            modified: now,
            author: self.actor.clone(),
            author_kind,
            logical_clock: 0,
            origin: None,
            canonical_id: None,
            tags: vec![],
            flag: None,
            is_read: false,
            is_starred: false,
            priority: Priority::Normal,
            visibility: Visibility::Private,
            message_type: Some("discussion".into()),
            produced_by: None,
            version: Some("1.0.0".into()),
            batch_id: None,
            references,
            parent,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn message_item(
        &self,
        id: ItemId,
        conversation_id: ItemId,
        draft: &MessageDraft,
        sequence: i64,
        status: &str,
        produced_by: Option<ItemId>,
        assistant_metadata: Option<(&str, &str)>,
    ) -> Item {
        let mut payload = BTreeMap::new();
        payload.insert("body".into(), Value::String(draft.body.clone()));
        payload.insert("format".into(), Value::String("markdown".into()));
        payload.insert("from".into(), Value::String(draft.from.clone()));
        payload.insert("role".into(), Value::String(draft.role.as_str().into()));
        payload.insert("status".into(), Value::String(status.into()));
        payload.insert("sequence".into(), Value::Int(sequence));
        if let Some((reasoning, model)) = assistant_metadata {
            if !reasoning.is_empty() {
                payload.insert("reasoning".into(), Value::String(reasoning.into()));
            }
            payload.insert("model".into(), Value::String(model.into()));
        }
        let mut references: Vec<TypedReference> = draft
            .attachment_ids
            .iter()
            .map(|target| TypedReference {
                target: *target,
                edge_type: EdgeType::Attaches,
                metadata: None,
            })
            .collect();
        if let Some(target) = draft.in_response_to {
            references.push(TypedReference {
                target,
                edge_type: EdgeType::InResponseTo,
                metadata: None,
            });
        }
        let mut item = self.item(
            CHAT_MESSAGE_SCHEMA,
            payload,
            Some(conversation_id),
            references,
            match draft.role {
                Role::User => ActorKind::Human,
                Role::System => ActorKind::System,
                Role::Assistant | Role::Tool => ActorKind::Agent,
            },
        );
        item.id = id;
        item.produced_by = produced_by;
        item
    }
}

fn validate_conversation(draft: &ConversationDraft) -> Result<()> {
    normalized_conversation_title(&draft.title)?;
    if draft.provider.trim().is_empty() {
        return Err(Error::Invalid(
            "conversation provider cannot be empty".into(),
        ));
    }
    if draft.model.trim().is_empty() || draft.model.len() > 300 {
        return Err(Error::Invalid("choose a valid conversation model".into()));
    }
    if !(0.0..=2.0).contains(&draft.temperature) {
        return Err(Error::Invalid("temperature must be between 0 and 2".into()));
    }
    if !(1..=131_072).contains(&draft.max_tokens) {
        return Err(Error::Invalid(
            "max tokens must be between 1 and 131072".into(),
        ));
    }
    Ok(())
}

pub fn is_placeholder_conversation_title(title: &str) -> bool {
    matches!(
        title.trim().to_ascii_lowercase().as_str(),
        "new chat" | "new conversation" | "untitled conversation"
    )
}

fn normalized_conversation_title(title: &str) -> Result<String> {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        return Err(Error::Invalid("conversation title cannot be empty".into()));
    }
    if title.chars().count() > 120 {
        return Err(Error::Invalid(
            "conversation title cannot exceed 120 characters".into(),
        ));
    }
    Ok(title)
}

fn insert_optional_string(payload: &mut BTreeMap<String, Value>, key: &str, value: Option<String>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        payload.insert(key.into(), Value::String(value));
    }
}

fn payload_string(item: &Item, key: &str) -> Option<String> {
    match item.payload.get(key) {
        Some(Value::String(value)) => Some(value.clone()),
        _ => None,
    }
}

fn payload_string_from_map(payload: &BTreeMap<String, Value>, key: &str) -> Option<String> {
    match payload.get(key) {
        Some(Value::String(value)) => Some(value.clone()),
        _ => None,
    }
}

fn payload_strings(item: &Item, key: &str) -> Vec<String> {
    match item.payload.get(key) {
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(|value| match value {
                Value::String(value) => Some(value.clone()),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

fn payload_i64(item: &Item, key: &str) -> Option<i64> {
    match item.payload.get(key) {
        Some(Value::Int(value)) => Some(*value),
        _ => None,
    }
}

fn payload_f64(item: &Item, key: &str) -> Option<f64> {
    match item.payload.get(key) {
        Some(Value::Float(value)) => Some(*value),
        Some(Value::Int(value)) => Some(*value as f64),
        _ => None,
    }
}

fn payload_bool(item: &Item, key: &str) -> Option<bool> {
    match item.payload.get(key) {
        Some(Value::Bool(value)) => Some(*value),
        _ => None,
    }
}

fn parse_role(role: Option<&str>) -> Result<Role> {
    match role.unwrap_or("user") {
        "system" => Ok(Role::System),
        "user" => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        "tool" => Ok(Role::Tool),
        other => Err(Error::Invalid(format!("unknown message role '{other}'"))),
    }
}

fn blob_descriptor(item: &Item) -> Result<BlobDescriptor> {
    Ok(BlobDescriptor {
        sha256: payload_string(item, "sha256")
            .ok_or_else(|| Error::Store("content blob has no SHA-256".into()))?,
        byte_length: payload_i64(item, "byte_length")
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| Error::Store("content blob has invalid byte length".into()))?,
        storage_kind: payload_string(item, "storage_kind")
            .ok_or_else(|| Error::Store("content blob has no storage kind".into()))?,
        locator: payload_string(item, "locator").unwrap_or_default(),
    })
}

fn resolve_blob(item: &Item, blob_store: &dyn BlobStore) -> Result<Vec<ModelContentPart>> {
    let descriptor = blob_descriptor(item)?;
    let bytes = blob_store.read(&descriptor)?;
    let mime_type = payload_string(item, "mime_type")
        .ok_or_else(|| Error::Store("content blob has no MIME type".into()))?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    if mime_type.starts_with("image/") {
        return Ok(vec![ModelContentPart::ImageUrl {
            image_url: ImageUrl {
                url: format!("data:{mime_type};base64,{encoded}"),
                detail: None,
            },
        }]);
    }
    if mime_type.starts_with("audio/") {
        let format = mime_type
            .split_once('/')
            .map(|(_, subtype)| subtype)
            .unwrap_or("wav")
            .trim_start_matches("x-")
            .to_string();
        return Ok(vec![ModelContentPart::InputAudio {
            input_audio: InputAudio {
                data: encoded,
                format,
            },
        }]);
    }
    if mime_type.starts_with("text/") || mime_type == "application/json" {
        let text = String::from_utf8(bytes)
            .map_err(|_| Error::UnsupportedContent(format!("{mime_type} is not UTF-8")))?;
        return Ok(vec![ModelContentPart::Text { text }]);
    }
    Err(Error::UnsupportedContent(format!(
        "oMLX request encoding for '{mime_type}' is not implemented"
    )))
}

fn json_to_value(value: serde_json::Value) -> Value {
    match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(value) => Value::Bool(value),
        serde_json::Value::Number(value) => value
            .as_i64()
            .map(Value::Int)
            .or_else(|| value.as_f64().map(Value::Float))
            .unwrap_or(Value::Null),
        serde_json::Value::String(value) => Value::String(value),
        serde_json::Value::Array(values) => {
            Value::Array(values.into_iter().map(json_to_value).collect())
        }
        serde_json::Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, json_to_value(value)))
                .collect(),
        ),
    }
}

fn json_token_count(usage: &serde_json::Value, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|key| usage.get(key).and_then(serde_json::Value::as_i64))
        .filter(|count| *count >= 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FileBlobStore;

    fn harness() -> (tempfile::TempDir, AiStore, FileBlobStore) {
        let directory = tempfile::tempdir().unwrap();
        let store = AiStore::open_dir(
            &directory.path().join("impress.sqlite"),
            "test:user",
            ActorKind::Human,
        )
        .unwrap();
        let blobs = FileBlobStore::open(directory.path().join("blobs")).unwrap();
        (directory, store, blobs)
    }

    #[test]
    fn queues_an_offline_turn_as_synced_graph_items() {
        let (_directory, store, _blobs) = harness();
        let conversation = store
            .create_conversation(ConversationDraft {
                model: "local-model".into(),
                tool_policy: ToolPolicy {
                    enabled: vec!["impress-mcp".into(), "scix".into()],
                },
                ..Default::default()
            })
            .unwrap();
        let turn = store
            .queue_user_turn(conversation, MessageDraft::user("Find the relevant papers"))
            .unwrap();
        let snapshot = store.snapshot(conversation).unwrap();
        assert_eq!(snapshot.messages.len(), 1);
        assert_eq!(snapshot.pending_tasks.len(), 1);
        assert_eq!(snapshot.pending_tasks[0].id, turn.task_id);
        assert_eq!(snapshot.pending_tasks[0].parent, Some(conversation));
        assert!(snapshot.pending_tasks[0]
            .references
            .iter()
            .any(|reference| reference.target == turn.message_id
                && reference.edge_type == EdgeType::OperatesOn));
    }

    #[test]
    fn resolves_image_attachments_only_at_request_time() {
        let (_directory, store, blobs) = harness();
        let attachment = store
            .ingest_blob(&blobs, b"png bytes", "image/png", Some("plot.png".into()))
            .unwrap();
        let conversation = store
            .create_conversation(ConversationDraft {
                model: "vision-model".into(),
                ..Default::default()
            })
            .unwrap();
        let mut message = MessageDraft::user("Interpret this figure");
        message.attachment_ids.push(attachment.item_id);
        let turn = store.queue_user_turn(conversation, message).unwrap();
        let prepared = store
            .prepare_request(turn.task_id, &blobs, &BTreeMap::new())
            .unwrap();
        let content = &prepared.request.messages.last().unwrap().content;
        assert!(matches!(content[1], ModelContentPart::ImageUrl { .. }));
        let blob_item = store
            .shared_store()
            .get(attachment.item_id)
            .unwrap()
            .unwrap();
        assert!(!blob_item.payload.contains_key("data"));
        assert!(!blob_item.payload.contains_key("base64"));
    }

    #[test]
    fn changing_conversation_model_only_updates_future_preference() {
        let (_directory, store, _blobs) = harness();
        let conversation = store
            .create_conversation(ConversationDraft {
                model: "first-model".into(),
                ..Default::default()
            })
            .unwrap();

        store
            .set_conversation_model(conversation, " second-model ".into())
            .unwrap();

        let snapshot = store.snapshot(conversation).unwrap();
        assert_eq!(
            snapshot.conversation.payload.get("model"),
            Some(&Value::String("second-model".into()))
        );
        assert!(store
            .set_conversation_model(conversation, "  ".into())
            .is_err());
    }

    #[test]
    fn title_suggestions_are_durable_deduplicated_and_never_clobber_a_rename() {
        let (_directory, store, _blobs) = harness();
        let conversation = store
            .create_conversation(ConversationDraft {
                model: "local-model".into(),
                ..Default::default()
            })
            .unwrap();
        store
            .append_message(
                conversation,
                MessageDraft::user("Explain galaxy rotation curves"),
            )
            .unwrap();

        let task_id = store.queue_title_suggestion(conversation).unwrap();
        assert_eq!(
            store.conversation_rows(false).unwrap()[0].pending_task_count,
            0
        );
        assert!(store
            .conversation_view(conversation)
            .unwrap()
            .tasks
            .is_empty());
        assert_eq!(
            store
                .queue_title_suggestion_if_placeholder(conversation)
                .unwrap(),
            Some(task_id)
        );
        let prepared = store.prepare_title_request(task_id).unwrap();
        assert_eq!(prepared.request.model, "local-model");
        assert!(prepared.request.tools.is_empty());
        let run_id = store
            .record_run_start(task_id, &prepared, "omlx", "local-omlx")
            .unwrap();

        store
            .set_conversation_title(conversation, "  Human   title  ".into())
            .unwrap();
        let applied = store
            .record_title_completion(
                task_id,
                &prepared,
                run_id,
                "Galaxy Rotation Curve Physics".into(),
                CompletionRecord::default(),
            )
            .unwrap();
        assert!(!applied);
        let snapshot = store.snapshot(conversation).unwrap();
        assert_eq!(
            snapshot.conversation.payload.get("title"),
            Some(&Value::String("Human title".into()))
        );
        assert!(store.has_completed_run(task_id).unwrap());
        assert_eq!(
            store
                .shared_store()
                .get(run_id)
                .unwrap()
                .unwrap()
                .payload
                .get("result_applied"),
            Some(&Value::Bool(false))
        );
    }

    #[test]
    fn preserves_capture_metadata_when_blob_bytes_are_deduplicated() {
        let (_directory, store, blobs) = harness();
        let first = store
            .ingest_blob(&blobs, b"same bytes", "image/png", Some("raw.png".into()))
            .unwrap();
        let second = store
            .ingest_blob(
                &blobs,
                b"same bytes",
                "image/png",
                Some("annotated.png".into()),
            )
            .unwrap();

        assert_eq!(first.sha256, second.sha256);
        assert_ne!(first.item_id, second.item_id);
        assert_eq!(first.file_name.as_deref(), Some("raw.png"));
        assert_eq!(second.file_name.as_deref(), Some("annotated.png"));
    }

    #[test]
    fn research_capture_is_idempotent_for_the_same_turn_and_content() {
        let (_directory, store, blobs) = harness();
        let conversation = store
            .create_conversation(ConversationDraft {
                model: "qwen".into(),
                web_access: true,
                ..Default::default()
            })
            .unwrap();
        let turn = store
            .queue_user_turn(conversation, MessageDraft::user("latest result"))
            .unwrap();
        let context = ResearchContext {
            sources: vec![crate::ResearchSource {
                url: "https://example.org/result".into(),
                title: "Result".into(),
                content: "A captured result".into(),
            }],
            errors: vec![],
        };
        let mut first = store
            .prepare_request(turn.task_id, &blobs, &BTreeMap::new())
            .unwrap();
        assert!(first.request.tool_policy.allows("web"));
        let first_ids = store
            .attach_research_context(&mut first, &blobs, &context)
            .unwrap();
        let mut retry = store
            .prepare_request(turn.task_id, &blobs, &BTreeMap::new())
            .unwrap();
        let retry_ids = store
            .attach_research_context(&mut retry, &blobs, &context)
            .unwrap();
        assert_eq!(first_ids, retry_ids);
        assert_eq!(
            store
                .shared_store()
                .count(&ItemQuery {
                    schema: Some(impress_item::schema::refs::IMPRESS_ARTIFACT_WEBPAGE),
                    predicates: vec![Predicate::HasParent(conversation)],
                    ..Default::default()
                })
                .unwrap(),
            1
        );
    }

    #[test]
    fn records_model_and_tool_provenance() {
        let (_directory, store, blobs) = harness();
        let conversation = store
            .create_conversation(ConversationDraft {
                model: "qwen".into(),
                tool_policy: ToolPolicy {
                    enabled: vec!["scix".into()],
                },
                ..Default::default()
            })
            .unwrap();
        let turn = store
            .queue_user_turn(conversation, MessageDraft::user("Search SciX"))
            .unwrap();
        let prepared = store
            .prepare_request(turn.task_id, &blobs, &BTreeMap::new())
            .unwrap();
        let run = store
            .record_run_start(turn.task_id, &prepared, "omlx", "laptop")
            .unwrap();
        assert_eq!(
            store
                .record_run_start(turn.task_id, &prepared, "omlx", "laptop")
                .unwrap(),
            run
        );
        store
            .record_run_preview(run, "The first paragraph is available.", true)
            .unwrap();
        let progress = store.task_progress(turn.task_id).unwrap();
        assert_eq!(
            progress.preview_text.as_deref(),
            Some("The first paragraph is available.")
        );
        assert!(progress.preview_complete);
        let tool = store
            .record_tool_invocation(
                run,
                turn.task_id,
                ToolInvocationRecord {
                    tool: "scix.search".into(),
                    provider: "scix".into(),
                    arguments: BTreeMap::from([("query".into(), Value::String("stars".into()))]),
                    result: None,
                    result_summary: Some("3 papers".into()),
                    error: None,
                    duration_ms: Some(12),
                },
            )
            .unwrap();
        let completion = CompletionRecord {
            content: "Three papers are relevant.".into(),
            reasoning: "Compared abstracts.".into(),
            usage: Some(serde_json::json!({
                "prompt_tokens": 10,
                "completion_tokens": 4
            })),
            finish_reason: Some("stop".into()),
            duration_ms: Some(125),
        };
        let assistant = store
            .record_completion(&prepared, run, completion.clone())
            .unwrap();
        assert_eq!(
            store.record_completion(&prepared, run, completion).unwrap(),
            assistant
        );
        assert_eq!(store.snapshot(conversation).unwrap().messages.len(), 2);
        let shared = store.shared_store();
        assert_eq!(shared.get(tool).unwrap().unwrap().produced_by, Some(run));
        assert_eq!(
            shared.get(assistant).unwrap().unwrap().produced_by,
            Some(run)
        );
        let run_item = shared.get(run).unwrap().unwrap();
        assert_eq!(
            payload_string(&run_item, "status").as_deref(),
            Some("completed")
        );
        assert_eq!(payload_i64(&run_item, "input_token_count"), Some(10));
        assert_eq!(payload_i64(&run_item, "output_token_count"), Some(4));
        assert_eq!(payload_i64(&run_item, "token_count"), Some(14));
        assert_eq!(payload_i64(&run_item, "duration_ms"), Some(125));
        assert!(run_item
            .references
            .iter()
            .any(|reference| reference.edge_type == EdgeType::DerivedFrom));
        assert!(store.has_completed_run(turn.task_id).unwrap());
        let provenance = store.run_provenance(run).unwrap();
        assert_eq!(provenance.task.id, turn.task_id);
        assert_eq!(provenance.inputs.len(), 1);
        assert_eq!(provenance.tool_invocations.len(), 1);
        assert_eq!(provenance.outputs.len(), 1);
        assert_eq!(
            store.task_provenance(turn.task_id).unwrap().unwrap().run.id,
            run
        );
    }
}
