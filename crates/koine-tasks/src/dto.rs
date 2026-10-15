//! What the task verbs answer.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One task as the task verbs show it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskRow {
    pub id: String,
    pub title: String,
    /// The executor dispatch key (`koine.verb` for a verb task).
    pub kind: String,
    /// pending, running, done, failed, cancelled.
    pub state: String,
    /// A verb task's verb (and a job's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verb: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
    /// The verb's answer, once it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub attempts: i64,
    /// Who filed it (`human`, `agent:mcp`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filed_by: Option<String>,
    /// The scheduler that runs (or ran) it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_to: Option<String>,
    /// Not before this time (ms since the epoch): the retry backoff, or the
    /// time it was filed to wait for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_attempt_at: Option<i64>,
    pub cancel_requested: bool,
    /// The tasks it waits for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// The task this one retries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<String>,
    pub created: String,
    pub modified: String,
}

/// A verb that answers one task.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct TaskResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `list`: tasks, newest first.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct TasksResult {
    pub ok: bool,
    #[serde(default)]
    pub rows: Vec<TaskRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
