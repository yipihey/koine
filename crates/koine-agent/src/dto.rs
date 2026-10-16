//! What the agent-desk verbs answer.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `skill`: this app's skill.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SkillResult {
    pub ok: bool,
    /// The skill's name (its directory under an agent's skills).
    pub name: String,
    /// The `SKILL.md` text.
    pub markdown: String,
}

/// One selection on a channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SelectionRow {
    /// The channel number.
    pub channel: String,
    /// The record kind selected.
    pub kind: String,
    /// The selected record ids.
    pub ids: Vec<String>,
}

/// A verb that acts on the selection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VerbRow {
    pub name: String,
    /// `read-only`, `mutating`, `destructive` or `external`.
    pub safety: String,
    /// The first sentence of its description.
    pub description: String,
}

/// `context`: what the person is pointing at.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ContextResult {
    pub ok: bool,
    pub app_id: String,
    /// The focused pane's tile id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_pane: Option<u64>,
    /// The focused pane: its view kind, role, query and the surface it shows.
    #[serde(default)]
    pub pane: Value,
    /// What every channel carries.
    #[serde(default)]
    pub selection: Vec<SelectionRow>,
    /// Verbs that act on the selected kinds: those that return them, and the
    /// verbs of the same services that take `id` or `ids`.
    #[serde(default)]
    pub verbs: Vec<VerbRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// One request on the desk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RequestRow {
    pub id: String,
    /// `ask`, `diagnose` or `make-surface`.
    pub kind: String,
    pub prompt: String,
    /// `open`, `claimed`, `answered` or `declined`.
    pub status: String,
    /// Who filed it.
    pub created_by: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_by: Option<String>,
    /// The answer, or the reason it was declined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// A surface the answer made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_id: Option<String>,
    /// The call a `diagnose` request is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// How many times an agent claimed it and gave it back
    /// (`agent-service_release`), so the next agent can tell a request that
    /// keeps failing from a fresh one.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub releases: u64,
    /// Why it was last given back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_reason: Option<String>,
    /// The task category the person asked for (`agent.frontier` is the
    /// strongest model); the desk routes the request when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// The category the desk's triage chose for a request with none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routed_category: Option<String>,
    /// The model that triaged it, as `provider/model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triage_model: Option<String>,
    /// The JSON Schema (an object schema) the answer must follow; the desk
    /// agent passes it to the model as the turn's response format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<Value>,
    /// The answer parsed, when the request had a `response_schema` and the
    /// answer is JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_json: Option<Value>,
    /// What the agent needs to act: the pointing context, a call record, the
    /// kinds and verbs to build with.
    #[serde(default)]
    pub context: Value,
}

/// A verb that answers one request.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct RequestResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<RequestRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `requests`: the desk, newest first (without each request's context).
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct RequestsResult {
    pub ok: bool,
    pub rows: Vec<RequestRow>,
    /// How many are open.
    pub open: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `show`: where the answer's surface went.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ShowResult {
    pub ok: bool,
    #[serde(default)]
    pub shown: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}
