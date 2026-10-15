//! Display-ready projections for declarative native and web frontends.
//!
//! These rows are intentionally shaped in Rust, following imbib's
//! `BibliographyRow` boundary: clients render values and issue commands, but
//! do not interpret item payloads, reconstruct task state, or reconcile tool
//! policy themselves.

use std::collections::{BTreeMap, BTreeSet};

use crate::schemas::{
    CONTENT_BLOB_SCHEMA, CONVERSATION_SCHEMA, TASK_SCHEMA, TOOL_INVOCATION_SCHEMA,
};
use chrono::{DateTime, Utc};
use impress_item::item::{Item, ItemId, Value};
use impress_item::query::{ItemQuery, Predicate, SortDescriptor};
use impress_item::reference::EdgeType;
use impress_item::store::ItemStore;
use serde::{Deserialize, Serialize};

use crate::{AiStore, Error, Result, INFERENCE_TASK_KIND};

const CHAT_MESSAGE_SCHEMA: impress_item::SchemaRef = impress_item::schema::refs::CHAT_MESSAGE;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiToolOption {
    pub id: String,
    pub label: String,
    pub description: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiConversationRow {
    pub id: ItemId,
    pub title: String,
    pub summary: Option<String>,
    pub state: String,
    pub provider: String,
    pub model: String,
    pub created_at_ms: i64,
    pub modified_at_ms: i64,
    pub last_activity_at_ms: i64,
    pub message_count: u32,
    pub pending_task_count: u32,
    pub enabled_tools: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiAttachmentRow {
    pub id: ItemId,
    pub mime_type: String,
    pub file_name: Option<String>,
    pub byte_length: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiWebSourceRow {
    pub id: ItemId,
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiToolInvocationRow {
    pub id: ItemId,
    pub tool: String,
    pub provider: String,
    pub state: String,
    pub result_summary: Option<String>,
    pub error: Option<String>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiMessageRow {
    pub id: ItemId,
    pub role: String,
    pub body: String,
    pub format: String,
    pub sender: String,
    pub status: String,
    pub sequence: i64,
    pub created_at_ms: i64,
    pub reasoning: Option<String>,
    pub model: Option<String>,
    pub in_response_to_id: Option<ItemId>,
    pub produced_by_run_id: Option<ItemId>,
    pub attachments: Vec<AiAttachmentRow>,
    pub sources: Vec<AiWebSourceRow>,
    pub tool_invocations: Vec<AiToolInvocationRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiTaskRow {
    pub id: ItemId,
    pub title: String,
    pub state: String,
    pub created_at_ms: i64,
    pub run_id: Option<ItemId>,
    pub response_message_id: Option<ItemId>,
    pub preview_text: Option<String>,
    pub preview_complete: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiConversationView {
    pub conversation: AiConversationRow,
    pub messages: Vec<AiMessageRow>,
    pub tasks: Vec<AiTaskRow>,
    pub tool_options: Vec<AiToolOption>,
}

impl AiStore {
    /// List display-ready conversations without asking a frontend to decode
    /// graph payloads or run one query per row.
    pub fn conversation_rows(&self, include_archived: bool) -> Result<Vec<AiConversationRow>> {
        let conversations = self.list_conversations(include_archived)?;
        let store = self.shared_store();
        let mut message_counts = BTreeMap::<ItemId, u32>::new();
        for message in store.query(&ItemQuery {
            schema: Some(CHAT_MESSAGE_SCHEMA),
            ..Default::default()
        })? {
            if let Some(parent) = message.parent {
                saturating_increment(&mut message_counts, parent);
            }
        }
        let mut pending_task_counts = BTreeMap::<ItemId, u32>::new();
        for task in store.query(&ItemQuery {
            schema: Some(TASK_SCHEMA),
            predicates: vec![
                Predicate::Eq(
                    "payload.task_kind".into(),
                    Value::String(INFERENCE_TASK_KIND.into()),
                ),
                Predicate::In(
                    "payload.state".into(),
                    vec![
                        Value::String("pending".into()),
                        Value::String("running".into()),
                    ],
                ),
            ],
            ..Default::default()
        })? {
            if let Some(parent) = task.parent {
                saturating_increment(&mut pending_task_counts, parent);
            }
        }
        conversations
            .iter()
            .map(|item| {
                conversation_row(
                    item,
                    message_counts.get(&item.id).copied().unwrap_or(0),
                    pending_task_counts.get(&item.id).copied().unwrap_or(0),
                )
            })
            .collect()
    }

    /// Full declarative screen projection. The frontend renders this value and
    /// invokes core commands; it does not infer relationships or task state.
    pub fn conversation_view(&self, conversation_id: ItemId) -> Result<AiConversationView> {
        let snapshot = self.snapshot(conversation_id)?;
        let store = self.shared_store();
        let task_items = store.query(&ItemQuery {
            schema: Some(TASK_SCHEMA),
            predicates: vec![
                Predicate::HasParent(conversation_id),
                Predicate::Eq(
                    "payload.task_kind".into(),
                    Value::String(INFERENCE_TASK_KIND.into()),
                ),
            ],
            sort: vec![SortDescriptor {
                field: "created".into(),
                ascending: true,
            }],
            ..Default::default()
        })?;
        let pending_count = task_items
            .iter()
            .filter(|task| {
                matches!(
                    payload_string(task, "state").as_deref(),
                    Some("pending" | "running")
                )
            })
            .count()
            .try_into()
            .unwrap_or(u32::MAX);
        let message_count = snapshot.messages.len().try_into().unwrap_or(u32::MAX);
        let conversation = conversation_row(&snapshot.conversation, message_count, pending_count)?;
        let enabled = conversation
            .enabled_tools
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let messages = snapshot
            .messages
            .iter()
            .map(|message| message_row(store.as_ref(), message))
            .collect::<Result<Vec<_>>>()?;
        let tasks = task_items
            .iter()
            .map(|task| {
                let progress = self.task_progress(task.id)?;
                Ok(AiTaskRow {
                    id: task.id,
                    title: payload_string(task, "title")
                        .unwrap_or_else(|| "Generate response".into()),
                    state: progress.state,
                    created_at_ms: task.created.timestamp_millis(),
                    run_id: progress.run_id,
                    response_message_id: progress.response_message_id,
                    preview_text: progress.preview_text,
                    preview_complete: progress.preview_complete,
                    error: progress.error,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(AiConversationView {
            conversation,
            messages,
            tasks,
            tool_options: tool_options(&enabled),
        })
    }
}

/// Stable selectable capability catalog for a new-conversation form.
/// Unknown enabled ids remain visible so newer hosts round-trip through older
/// frontends without silently losing policy.
pub fn tool_options_for_policy(enabled_tools: &[String]) -> Vec<AiToolOption> {
    let enabled = enabled_tools.iter().cloned().collect::<BTreeSet<_>>();
    tool_options(&enabled)
}

fn conversation_row(
    item: &Item,
    message_count: u32,
    pending_task_count: u32,
) -> Result<AiConversationRow> {
    if item.schema != CONVERSATION_SCHEMA {
        return Err(Error::Invalid(format!(
            "item {} is not an AI conversation",
            item.id
        )));
    }
    let enabled_tools = normalized_strings(item, "enabled_tools");
    Ok(AiConversationRow {
        id: item.id,
        title: payload_string(item, "title").unwrap_or_else(|| "Untitled conversation".into()),
        summary: payload_string(item, "summary"),
        state: payload_string(item, "state").unwrap_or_else(|| "active".into()),
        provider: payload_string(item, "provider").unwrap_or_else(|| "omlx".into()),
        model: payload_string(item, "model").unwrap_or_default(),
        created_at_ms: item.created.timestamp_millis(),
        modified_at_ms: item.modified.timestamp_millis(),
        last_activity_at_ms: payload_string(item, "last_activity_at")
            .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
            .map(|value| value.with_timezone(&Utc).timestamp_millis())
            .unwrap_or_else(|| item.modified.timestamp_millis()),
        message_count,
        pending_task_count,
        enabled_tools,
    })
}

fn message_row(store: &dyn ItemStore, item: &Item) -> Result<AiMessageRow> {
    if item.schema != CHAT_MESSAGE_SCHEMA {
        return Err(Error::Invalid(format!(
            "item {} is not a chat message",
            item.id
        )));
    }
    let mut attachments = Vec::new();
    let mut in_response_to_id = None;
    for reference in &item.references {
        match reference.edge_type {
            EdgeType::Attaches => {
                let Some(blob) = store.get(reference.target)? else {
                    continue;
                };
                if blob.schema != CONTENT_BLOB_SCHEMA {
                    continue;
                }
                attachments.push(AiAttachmentRow {
                    id: blob.id,
                    mime_type: payload_string(&blob, "mime_type")
                        .unwrap_or_else(|| "application/octet-stream".into()),
                    file_name: payload_string(&blob, "file_name"),
                    byte_length: payload_i64(&blob, "byte_length")
                        .and_then(|value| u64::try_from(value).ok())
                        .unwrap_or(0),
                    sha256: payload_string(&blob, "sha256").unwrap_or_default(),
                });
            }
            EdgeType::InResponseTo => in_response_to_id = Some(reference.target),
            _ => {}
        }
    }
    attachments.sort_by_key(|attachment| attachment.id);
    let (sources, tool_invocations) = item
        .produced_by
        .map(|run_id| run_facets(store, run_id))
        .transpose()?
        .unwrap_or_default();
    Ok(AiMessageRow {
        id: item.id,
        role: payload_string(item, "role").unwrap_or_else(|| "assistant".into()),
        body: payload_string(item, "body").unwrap_or_default(),
        format: payload_string(item, "format").unwrap_or_else(|| "markdown".into()),
        sender: payload_string(item, "from").unwrap_or_default(),
        status: payload_string(item, "status").unwrap_or_else(|| "complete".into()),
        sequence: payload_i64(item, "sequence").unwrap_or_default(),
        created_at_ms: item.created.timestamp_millis(),
        reasoning: payload_string(item, "reasoning"),
        model: payload_string(item, "model"),
        in_response_to_id,
        produced_by_run_id: item.produced_by,
        attachments,
        sources,
        tool_invocations,
    })
}

fn run_facets(
    store: &dyn ItemStore,
    run_id: ItemId,
) -> Result<(Vec<AiWebSourceRow>, Vec<AiToolInvocationRow>)> {
    let mut sources = BTreeMap::<String, AiWebSourceRow>::new();
    if let Some(run) = store.get(run_id)? {
        for reference in run
            .references
            .iter()
            .filter(|reference| reference.edge_type == EdgeType::DerivedFrom)
        {
            let Some(source) = store.get(reference.target)? else {
                continue;
            };
            let Some(url) = payload_string(&source, "source_url") else {
                continue;
            };
            if !url.starts_with("https://") && !url.starts_with("http://") {
                continue;
            }
            sources
                .entry(url.clone())
                .or_insert_with(|| AiWebSourceRow {
                    id: source.id,
                    title: payload_string(&source, "title").unwrap_or_else(|| url.clone()),
                    url,
                });
        }
    }

    let tool_invocations = store
        .query(&ItemQuery {
            schema: Some(TOOL_INVOCATION_SCHEMA),
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
        .into_iter()
        .filter(|invocation| !is_empty_research_policy_check(invocation))
        .map(|invocation| AiToolInvocationRow {
            id: invocation.id,
            tool: payload_string(&invocation, "tool").unwrap_or_else(|| "unknown".into()),
            provider: payload_string(&invocation, "provider").unwrap_or_default(),
            state: payload_string(&invocation, "state").unwrap_or_else(|| "unknown".into()),
            result_summary: payload_string(&invocation, "result_summary"),
            error: payload_string(&invocation, "error"),
            duration_ms: payload_i64(&invocation, "duration_ms")
                .and_then(|value| u64::try_from(value).ok()),
        })
        .collect();

    Ok((sources.into_values().collect(), tool_invocations))
}

fn is_empty_research_policy_check(invocation: &Item) -> bool {
    payload_string(invocation, "tool").as_deref() == Some("web.research-context")
        && payload_string(invocation, "error").is_none()
        && payload_string(invocation, "result_summary").as_deref()
            == Some("0 source(s), 0 retrieval warning(s)")
}

fn tool_options(enabled: &BTreeSet<String>) -> Vec<AiToolOption> {
    let builtins = [
        (
            "scix",
            "SciX",
            "Search NASA ADS/SciX and work with scientific literature.",
        ),
        (
            "impress-mcp",
            "Impress",
            "Use generated capabilities across imbib, imprint, implore, impart, and the shared store.",
        ),
        (
            "web",
            "Web research",
            "Search and capture external web sources with durable provenance.",
        ),
    ];
    let mut options = builtins
        .into_iter()
        .map(|(id, label, description)| AiToolOption {
            id: id.into(),
            label: label.into(),
            description: description.into(),
            enabled: enabled.contains(id),
        })
        .collect::<Vec<_>>();
    let builtin_ids = builtins
        .iter()
        .map(|(id, _, _)| *id)
        .collect::<BTreeSet<_>>();
    options.extend(
        enabled
            .iter()
            .filter(|id| !builtin_ids.contains(id.as_str()))
            .map(|id| AiToolOption {
                id: id.clone(),
                label: id.clone(),
                description: "Additional capability supplied by the model host.".into(),
                enabled: true,
            }),
    );
    options
}

fn saturating_increment(counts: &mut BTreeMap<ItemId, u32>, id: ItemId) {
    let count = counts.entry(id).or_default();
    *count = count.saturating_add(1);
}

fn payload_string(item: &Item, key: &str) -> Option<String> {
    match item.payload.get(key) {
        Some(Value::String(value)) => Some(value.clone()),
        _ => None,
    }
}

fn payload_i64(item: &Item, key: &str) -> Option<i64> {
    match item.payload.get(key) {
        Some(Value::Int(value)) => Some(*value),
        _ => None,
    }
}

fn normalized_strings(item: &Item, key: &str) -> Vec<String> {
    let mut values = match item.payload.get(key) {
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(|value| match value {
                Value::String(value) if !value.trim().is_empty() => Some(value.clone()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        _ => vec![],
    };
    values.sort();
    values.dedup();
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CompletionRecord, ConversationDraft, FileBlobStore, MessageDraft, ResearchContext,
        ResearchSource, ToolInvocationRecord, ToolPolicy,
    };
    use impress_item::item::ActorKind;

    #[test]
    fn projection_owns_display_shape_counts_and_tool_policy() {
        let temp = tempfile::tempdir().unwrap();
        let store = AiStore::open_dir(
            &temp.path().join("impress.sqlite"),
            "user:test",
            ActorKind::Human,
        )
        .unwrap();
        let conversation_id = store
            .create_conversation(ConversationDraft {
                title: "Research".into(),
                model: "mlx-community/model".into(),
                web_access: true,
                tool_policy: ToolPolicy {
                    enabled: vec!["scix".into(), "scix".into(), "custom-tool".into()],
                },
                ..Default::default()
            })
            .unwrap();
        store
            .queue_user_turn(conversation_id, MessageDraft::user("Question"))
            .unwrap();

        let rows = store.conversation_rows(false).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message_count, 1);
        assert_eq!(rows[0].pending_task_count, 1);
        assert_eq!(rows[0].enabled_tools, ["custom-tool", "scix", "web"]);

        let view = store.conversation_view(conversation_id).unwrap();
        assert_eq!(view.messages[0].body, "Question");
        assert_eq!(view.tasks[0].state, "pending");
        assert!(view
            .tool_options
            .iter()
            .any(|tool| tool.id == "web" && tool.enabled));
        assert!(view
            .tool_options
            .iter()
            .any(|tool| tool.id == "custom-tool" && tool.enabled));

        store
            .set_tool_policy(
                conversation_id,
                ToolPolicy {
                    enabled: vec!["impress-mcp".into()],
                },
            )
            .unwrap();
        let blobs = FileBlobStore::open(temp.path().join("blobs")).unwrap();
        let task_id = view.tasks[0].id;
        let prepared = store
            .prepare_request(task_id, &blobs, &BTreeMap::new())
            .unwrap();
        assert_eq!(prepared.request.tool_policy.enabled, ["impress-mcp"]);
        let updated = store.conversation_view(conversation_id).unwrap();
        assert!(updated
            .tool_options
            .iter()
            .any(|tool| tool.id == "web" && !tool.enabled));
    }

    #[test]
    fn projection_emits_reasoning_sources_and_tool_provenance() {
        let temp = tempfile::tempdir().unwrap();
        let store = AiStore::open_dir(
            &temp.path().join("impress.sqlite"),
            "agent:test",
            ActorKind::Agent,
        )
        .unwrap();
        let blobs = FileBlobStore::open(temp.path().join("blobs")).unwrap();
        let conversation_id = store
            .create_conversation(ConversationDraft {
                model: "thinking-model".into(),
                ..Default::default()
            })
            .unwrap();
        let queued = store
            .queue_user_turn(conversation_id, MessageDraft::user("Question"))
            .unwrap();
        let mut prepared = store
            .prepare_request(queued.task_id, &blobs, &BTreeMap::new())
            .unwrap();
        store
            .attach_research_context(
                &mut prepared,
                &blobs,
                &ResearchContext {
                    sources: vec![ResearchSource {
                        url: "https://example.org/paper".into(),
                        title: "A useful paper".into(),
                        content: "Captured source text".into(),
                    }],
                    errors: vec![],
                },
            )
            .unwrap();
        let run_id = store
            .record_run_start(queued.task_id, &prepared, "omlx", "test")
            .unwrap();
        store
            .record_tool_invocation(
                run_id,
                queued.task_id,
                ToolInvocationRecord {
                    tool: "web.research-context".into(),
                    provider: "bounded-web".into(),
                    arguments: BTreeMap::new(),
                    result: None,
                    result_summary: Some("1 source".into()),
                    error: None,
                    duration_ms: Some(12),
                },
            )
            .unwrap();
        store
            .record_tool_invocation(
                run_id,
                queued.task_id,
                ToolInvocationRecord {
                    tool: "web.research-context".into(),
                    provider: "bounded-web".into(),
                    arguments: BTreeMap::new(),
                    result: Some(BTreeMap::from([("sources".into(), Value::Array(vec![]))])),
                    result_summary: Some("0 source(s), 0 retrieval warning(s)".into()),
                    error: None,
                    duration_ms: None,
                },
            )
            .unwrap();
        store
            .record_completion(
                &prepared,
                run_id,
                CompletionRecord {
                    content: "Answer with [evidence](https://example.org/paper).".into(),
                    reasoning: "Considered the captured evidence.".into(),
                    ..Default::default()
                },
            )
            .unwrap();

        let view = store.conversation_view(conversation_id).unwrap();
        let assistant = view
            .messages
            .iter()
            .find(|message| message.role == "assistant")
            .unwrap();
        assert_eq!(
            assistant.reasoning.as_deref(),
            Some("Considered the captured evidence.")
        );
        assert_eq!(assistant.sources.len(), 1);
        assert_eq!(assistant.sources[0].url, "https://example.org/paper");
        assert_eq!(assistant.tool_invocations.len(), 1);
        assert_eq!(assistant.tool_invocations[0].tool, "web.research-context");
        assert_eq!(assistant.tool_invocations[0].duration_ms, Some(12));
    }
}
