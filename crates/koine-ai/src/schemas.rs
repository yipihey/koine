//! The record kinds the AI kernel writes, spelled once (schema-refs.json is
//! the authority; these are its constants under the names this crate has
//! always used). The field declarations a store validates against stay with
//! the store that registers them (impress-core `schemas/ai.rs`).

use impress_item::SchemaRef;

/// A durable research conversation.
pub const CONVERSATION_SCHEMA: SchemaRef = impress_item::schema::refs::CONVERSATION;
/// Bytes a message attaches (an image, audio), held in the blob store.
pub const CONTENT_BLOB_SCHEMA: SchemaRef = impress_item::schema::refs::CONTENT_BLOB;
/// One tool call made during a model run.
pub const TOOL_INVOCATION_SCHEMA: SchemaRef = impress_item::schema::refs::TOOL_INVOCATION;
/// One receipt per imported legacy conversation.
pub const AI_IMPORT_LEDGER_SCHEMA: SchemaRef = impress_item::schema::refs::AI_IMPORT_LEDGER;
/// A message in a conversation.
pub const CHAT_MESSAGE_SCHEMA: SchemaRef = impress_item::schema::refs::CHAT_MESSAGE;
/// The task a queued turn is (see `koine_tasks`).
pub const TASK_SCHEMA: SchemaRef = koine_tasks::TASK_SCHEMA;
/// One model or executor invocation (see `koine_tasks`).
pub const AGENT_RUN_SCHEMA: SchemaRef = koine_tasks::AGENT_RUN_SCHEMA;
