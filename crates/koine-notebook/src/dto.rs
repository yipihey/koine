//! What the notebook verbs answer.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunRow {
    /// The run's record id.
    pub id: String,
    pub name: String,
    /// `open` or `closed`.
    pub status: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    /// Whether read-only calls are recorded while it is open.
    pub record_reads: bool,
    /// The machine it ran on.
    pub host: String,
    /// How many call records name it.
    pub calls: u64,
}

/// `runs`: every run, newest first.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunsResult {
    pub ok: bool,
    pub runs: Vec<RunRow>,
    /// The open run's id, if one is open.
    pub open: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `run-start` / `run-stop`: the run opened or closed.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// One call record on the timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TimelineRow {
    pub call_id: String,
    pub started_at: String,
    pub verb: String,
    /// `human`, `agent:<name>`, `system:<name>`, ….
    pub caller: String,
    pub ok: bool,
    pub code: Option<String>,
    /// Wall-clock time of the call.
    pub duration_ms: f64,
    /// Time not spent in the calls it made.
    pub self_ms: f64,
    /// Calls made inside it, nested ones included.
    pub steps: u64,
    /// Records it created, changed or deleted.
    pub writes: u64,
    /// Records it read.
    pub reads: u64,
    pub run: Option<String>,
    /// Whether it ran inside another logged call.
    pub nested: bool,
}

/// `timeline`: records, newest first.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TimelineResult {
    pub ok: bool,
    pub rows: Vec<TimelineRow>,
    /// Records that matched before `limit`.
    pub total: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// One step of an entry, flattened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StepRow {
    /// Position in depth-first order, 0 for the call itself.
    pub index: u64,
    pub depth: u64,
    /// The verb, indented by depth.
    pub label: String,
    pub verb: String,
    pub call_id: String,
    pub total_ms: f64,
    pub self_ms: f64,
    pub ok: bool,
    pub code: Option<String>,
}

/// A note on a call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NoteRow {
    pub id: String,
    pub text: String,
    pub author: String,
    pub at: String,
}

/// `entry`: one record in full.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EntryResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// The headline facts, for a key-value list.
    #[serde(default)]
    pub summary: BTreeMap<String, String>,
    /// The stored record as written (the pipeline's payload plus host and
    /// build).
    #[serde(default)]
    pub record: Value,
    /// The call and every step under it, depth first.
    #[serde(default)]
    pub steps: Vec<StepRow>,
    /// The five steps with the most self time.
    #[serde(default)]
    pub slowest: Vec<StepRow>,
    /// Folded stacks, `a;b;c <self µs>`.
    #[serde(default)]
    pub folded: Vec<String>,
    /// A `plot-spec@1.0.0` bar chart of self time per step.
    #[serde(default)]
    pub plot: Value,
    #[serde(default)]
    pub notes: Vec<NoteRow>,
    /// Whether `replay` can run it again.
    #[serde(default)]
    pub replayable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `annotate`: the note written.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AnnotateResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<NoteRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// One step verb's timings in two sets of records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CompareRow {
    pub step: String,
    pub a_count: u64,
    pub a_mean_ms: f64,
    pub a_p50_ms: f64,
    pub a_p95_ms: f64,
    pub a_self_mean_ms: f64,
    pub b_count: u64,
    pub b_mean_ms: f64,
    pub b_p50_ms: f64,
    pub b_p95_ms: f64,
    pub b_self_mean_ms: f64,
    /// `b_p50 / a_p50`, when both have the step.
    pub ratio_p50: Option<f64>,
}

/// `compare`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CompareResult {
    pub ok: bool,
    pub verb: String,
    /// What set `a` is: a run id, or `all`.
    pub a: String,
    pub b: String,
    pub rows: Vec<CompareRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `replay`: what the replayed call answered.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReplayResult {
    pub ok: bool,
    /// The call replayed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_of: Option<String>,
    /// The new call's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// The new call's answer.
    #[serde(default)]
    pub answer: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `export`: a run as Markdown.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExportResult {
    pub ok: bool,
    #[serde(default)]
    pub markdown: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `prune`: how many records went.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PruneResult {
    pub ok: bool,
    pub deleted: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
