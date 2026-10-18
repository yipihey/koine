//! `HistoryService` — the self-reflective layer's read (and controlled
//! write) side over the call log (L2, plan-self-reflective-layer § Call
//! log).
//!
//! L1 (this crate's call-log sink, [`crate::sink`]) writes
//! one `core/verb-call@1.0.0` row per non-read-only call, whose id is the
//! `batch_id` every operation the call wrote carries. This service is the
//! agent- and human-facing surface over that join:
//!
//! * [`HistoryService::calls`] — a filtered page of the log ("what
//!   happened").
//! * [`HistoryService::why`] — operations joined to their calls, plus calls
//!   recording direct inserts/deletes that have no operation row ("why did
//!   this change").
//! * [`HistoryService::trace`] — every call sharing a trace id, as a tree
//!   built from `parent_call`.
//! * [`HistoryService::replay`] — re-runs recorded calls through the
//!   pipeline, refusing any whose arguments were not recorded in full
//!   (`not-replayable`, never guessed at).
//! * [`HistoryService::save_macro`] — turns a list of call ids into a
//!   stored, reviewable `impress/workflow@1.0.0` document (§ Workflows'
//!   `manual` trigger; W1 builds the runtime that executes one, this only
//!   writes the document, `state: "proposed"`).
//! * [`HistoryService::propose_workflows`] — the n-gram miner (W4, plan §
//!   Workflows, "the loop's last arc"): finds sequences of consecutive
//!   mutating verb calls by one caller that repeat at least `min_repeats`
//!   times and writes each as a `proposed` `impress/workflow@1.0.0`
//!   document with a `manual` trigger and `review.required: true`
//!   (D-R6) — nothing runs from a proposal until a person enables it.
//! * [`HistoryService::health`] — the audit sink's backlog, plus the log's
//!   own row count and oldest row.
//!
//! No domain logic beyond the join and the privacy-recorded-in-full check;
//! like every other service in this crate it converts arguments, reads the
//! store, and turns errors into `ok: false` plus a message.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use chrono::{SecondsFormat, Utc};
use impress_item::schema::refs::{
    CORE_VERB_CALL as VERB_CALL_SCHEMA, IMPRESS_WORKFLOW as CORE_WORKFLOW_SCHEMA,
};
use impress_item::{
    ActorKind, AttributedStore, Item, ItemId, ItemQuery, Predicate, Priority, SortDescriptor,
    Value as ItemValue, Visibility,
};
use impress_service_core::async_trait;
use impress_service_core::call;
use impress_service_core::descriptor::SafetyClass;
use impress_service_core::pipeline::{self, Call};
use impress_service_core::refusal::codes;
use impress_service_macros::{impress_service, impress_service_impl};
use impress_workflow::spec::{
    Action, Author, Guards, Review, Trigger, WorkflowSpec, WorkflowState,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[allow(unused_imports)]
use impress_service_macros::impress_method;

/// The canonical spelling of a saved macro's kind (schema-refs.json;
/// `impress/workflow@1.0.0`, registered by W1). Re-exported
/// under this crate's own name because every call site here already spells
/// it `WORKFLOW_SCHEMA` and existed before W1 registered the canonical
/// definition in the schema-ref manifest.
pub const WORKFLOW_SCHEMA: impress_item::SchemaRef = CORE_WORKFLOW_SCHEMA;

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// One call, as the history verbs report it.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CallSummary {
    /// The call id — the row's own id and every op's `batch_id`.
    pub call_id: String,
    pub verb: String,
    pub since: String,
    /// `{"kind": "agent"|"human"|"system"|"app"|"provider", "name": "…"?}`.
    pub caller: Value,
    pub trace_id: String,
    pub parent_call: Option<String>,
    /// The surface this call came from, when the call context carried one.
    pub surface: Option<String>,
    pub args: Value,
    pub ok: bool,
    pub code: Option<String>,
    pub message_len: u64,
    /// RFC 3339.
    pub started_at: String,
    pub duration_ms: u64,
    /// True once CL-6 compaction has reduced this row's `args`.
    pub compacted: bool,
}

/// An operation on an item, or an affected-ID call with no operation row.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WhyEntry {
    /// The operation row's id; `None` for a direct insert/delete recorded
    /// only in the call log. No operation id is invented for those writes.
    pub operation_id: Option<String>,
    /// RFC 3339: the operation's timestamp, or the call's start time.
    pub applied_at: String,
    /// The call id (every operation written by that call has it as `batch_id`).
    pub batch_id: String,
    /// The call that wrote this operation, when its row still exists. `None`
    /// only for a call the audit sink dropped (never silent — see
    /// [`HistoryService::health`]).
    pub call: Option<CallSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WhyResult {
    pub ok: bool,
    pub id: String,
    pub message: String,
    /// Newest first.
    pub entries: Vec<WhyEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CallsResult {
    pub ok: bool,
    pub message: String,
    /// Newest first.
    pub calls: Vec<CallSummary>,
}

/// One trace, as a tree: every call under it, nested by `parent_call`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TraceNode {
    pub call: CallSummary,
    pub children: Vec<TraceNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TraceResult {
    pub ok: bool,
    pub trace_id: String,
    pub message: String,
    /// The calls under this trace with no parent inside it.
    pub roots: Vec<TraceNode>,
}

/// What `replay` did (or would do) with one recorded call.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReplayOutcome {
    pub call_id: String,
    pub verb: Option<String>,
    pub ok: bool,
    pub code: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReplayResult {
    pub ok: bool,
    pub dry_run: bool,
    pub message: String,
    pub results: Vec<ReplayOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SaveMacroResult {
    pub ok: bool,
    /// The new `impress/workflow@1.0.0` document's id.
    pub id: String,
    pub message: String,
}

/// One workflow the miner proposed.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProposedWorkflow {
    /// The new `impress/workflow@1.0.0` document's id.
    pub id: String,
    pub name: String,
    /// The verb sequence this proposal was mined from, in step order.
    pub verbs: Vec<String>,
    /// How many times this sequence repeated in the log.
    pub repeats: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProposeWorkflowsResult {
    pub ok: bool,
    pub message: String,
    pub proposed: Vec<ProposedWorkflow>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HistoryHealth {
    pub ok: bool,
    pub message: String,
    /// Rows the audit sink's writer thread has written.
    pub written: u64,
    /// Rows dropped for a full channel.
    pub dropped: u64,
    /// Rows the store refused.
    pub failed: u64,
    /// Calls that found no sink installed at all.
    pub no_sink: u64,
    pub channel_capacity: u64,
    /// `core/verb-call@1.0.0` rows currently in the store.
    pub rows: usize,
    /// The oldest row's `started_at`, when there is one.
    pub oldest: Option<String>,
    /// Writes whose payload did not match their kind's registered schema
    /// (plan-koine-trust TR-D12, warn mode: counted, not refused).
    #[serde(default)]
    pub kind_mismatches: u64,
    /// The latest such write: kind, item and what did not match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_kind_mismatch: Option<String>,
}

// ---------------------------------------------------------------------------
// Service trait
// ---------------------------------------------------------------------------

/// The self-reflective layer's history verbs: what happened, why an item
/// changed, every call under a trace, replaying a recorded call, and saving
/// a list of calls as a reviewable macro.
#[impress_service]
pub trait HistoryService: Send + Sync + 'static {
    /// A filtered page of the call log — "what happened". Every filter is
    /// optional; `limit` of 0 means the default (50), clamped above 500.
    #[impress_method]
    #[impress_example(name = "default", args = r#"{"limit": 5}"#)]
    async fn calls(
        &self,
        since: Option<String>,
        until: Option<String>,
        verb: Option<String>,
        caller: Option<String>,
        trace_id: Option<String>,
        limit: i64,
    ) -> CallsResult;

    /// Why an item is the way it is: operations and direct insert/delete
    /// calls naming its id, newest first. A direct write has no operation
    /// row, so its entry has `operation_id: null` and `batch_id` is its call
    /// id. Each entry carries the caller and surface when recorded.
    #[impress_method]
    #[impress_example(
        name = "default",
        args = r#"{"id": "00000000-0000-0000-0000-000000000000"}"#
    )]
    async fn why(&self, id: String) -> WhyResult;

    /// Every call, as a tree built from `parent_call`, for one trace id —
    /// a surface click, the verb it ran, and any verb that ran nested
    /// inside it.
    #[impress_method]
    #[impress_example(name = "default", args = r#"{"trace_id": "t"}"#)]
    async fn trace(&self, trace_id: String) -> TraceResult;

    /// Re-invoke recorded calls through the pipeline, as
    /// `Agent("replay:<original caller>")`. Only a call whose arguments were
    /// recorded in full (never privacy-reduced, never compacted) can be
    /// replayed; every other one is refused `not-replayable`, by name,
    /// without running anything. `dry_run` lists what would run instead of
    /// running it.
    #[impress_method(
        safety = mutating,
        effects(
            reads = ["core/verb-call@1.0.0"],
            writes = [any("re-invokes an arbitrary recorded verb, whose own writes are its own")]
        )
    )]
    #[impress_example(
        name = "missing-call",
        args = r#"{"call_ids": ["00000000-0000-0000-0000-000000000000"], "dry_run": true}"#,
        expect = r#"{"ok":false,"results":[{"code":"not-found"}]}"#
    )]
    async fn replay(&self, call_ids: Vec<String>, dry_run: bool) -> ReplayResult;

    /// Turn a list of recorded call ids into a stored, reviewable
    /// `impress/workflow@1.0.0` document (`trigger: manual`, `state:
    /// "proposed"`) — a macro a person can review and enable once W1's
    /// runtime exists to run it.
    #[impress_method(
        safety = mutating,
        effects(reads = ["core/verb-call@1.0.0"], writes = ["impress/workflow@1.0.0"])
    )]
    #[impress_example(
        name = "missing-call",
        args = r#"{"call_ids": ["00000000-0000-0000-0000-000000000000"], "name": "my-macro"}"#,
        expect = r#"{"ok":false}"#
    )]
    async fn save_macro(&self, call_ids: Vec<String>, name: String) -> SaveMacroResult;

    /// Mines the call log for sequences of consecutive mutating verb calls
    /// by one caller that repeat at least `min_repeats` times (default 3),
    /// and writes each as a `proposed` `impress/workflow@1.0.0` document
    /// with a `manual` trigger and `review.required: true` (D-R6): an agent
    /// may create a proposal, but nothing runs from it without a person's
    /// review. "Consecutive" is in the caller's own call stream — other
    /// callers' calls interleaved in the log do not break a sequence. An
    /// argument whose value is the same across every repeat stays literal
    /// in the proposed step; one that differs becomes
    /// `"{{event.value.<name>}}"`, filled from the payload a person passes
    /// when they run the workflow's `manual` trigger. `max_len` bounds how
    /// long a mined sequence may be (default 6, capped at 20); the miner
    /// tries the longest lengths first so a shorter sequence is not
    /// reported as a sub-pattern of one already proposed.
    #[impress_method(
        safety = mutating,
        effects(
            reads = ["core/verb-call@1.0.0"],
            writes = ["impress/workflow@1.0.0"]
        )
    )]
    #[impress_example(
        name = "missing-max-len",
        args = r#"{"min_repeats": 3}"#,
        expect = r#"{"ok":false,"code":"invalid-argument"}"#
    )]
    async fn propose_workflows(
        &self,
        since: Option<String>,
        min_repeats: i64,
        max_len: i64,
    ) -> ProposeWorkflowsResult;

    /// The call log's own backlog: the audit sink's `written`/`dropped`/
    /// `failed`/`no_sink` counters and channel bound, plus how many
    /// `core/verb-call@1.0.0` rows exist and the oldest one's timestamp.
    #[impress_method]
    #[impress_example(name = "default", args = r#"{}"#)]
    async fn health(&self) -> HistoryHealth;
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

/// Store-backed `HistoryService`.
#[derive(Clone, Default)]
pub struct DefaultHistoryService {
    /// An injected store, held as the port and as its concrete type so a
    /// replay runs on it (`Call::store`); `None` reads the process store.
    store: Option<koine_store::process::CallStore>,
}

impl DefaultHistoryService {
    pub fn new() -> Self {
        Self { store: None }
    }

    pub fn with_store<T: AttributedStore + 'static>(store: Arc<T>) -> Self {
        Self {
            store: Some(koine_store::process::CallStore::of(store)),
        }
    }

    fn store(&self) -> Arc<dyn AttributedStore> {
        match &self.store {
            Some(scoped) => scoped.port().clone(),
            None => koine_store::process::acquire_or_stand_in().0,
        }
    }
}

fn payload_json(item: &Item) -> Value {
    serde_json::to_value(&item.payload).unwrap_or(Value::Object(Default::default()))
}

/// Whether a JSON value carries any of the audit layer's reduced shapes
/// (`summarize_field`/`hashed` in `impress_service_core::pipeline::audit`):
/// `{"len", "sha256_8"}`, `{"len", "first"}` or `{"len", "keys"}`, at any
/// depth. A value that never carries one was never privacy-reduced.
fn is_reduced(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
            keys.sort_unstable();
            let shape = |wanted: &[&str]| {
                let mut wanted = wanted.to_vec();
                wanted.sort_unstable();
                keys == wanted
            };
            if shape(&["len", "sha256_8"]) || shape(&["len", "first"]) || shape(&["len", "keys"]) {
                return true;
            }
            map.values().any(is_reduced)
        }
        Value::Array(items) => items.iter().any(is_reduced),
        _ => false,
    }
}

/// Whether the recorded call's arguments are still the full, original
/// arguments: not compacted (CL-6) and never reduced by the privacy filter.
/// Small scalars and id fields pass the filter unchanged (§ Call log's
/// table), so this is exact for them; a verb whose args carry only such
/// fields is always replayable, and one that carries a long string, a body
/// or an object never is.
fn args_are_full(payload: &Value) -> bool {
    if payload.get("compacted").and_then(Value::as_bool) == Some(true) {
        return false;
    }
    // New records explicitly distinguish a lossless summary from a reduced
    // one. In particular, a private null is not permission to replay it.
    if let Some(replayable) = payload.get("args_replayable").and_then(Value::as_bool) {
        return replayable && payload.get("args").is_some();
    }
    // Older rows predate the marker; preserve their existing conservative
    // reduced-shape check rather than reclassifying historical records.
    match payload.get("args") {
        Some(args) => !is_reduced(args),
        None => false,
    }
}

fn call_summary(item: &Item) -> CallSummary {
    let p = payload_json(item);
    CallSummary {
        call_id: item.id.to_string(),
        verb: p
            .get("verb")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        since: p
            .get("since")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        caller: p.get("caller").cloned().unwrap_or(Value::Null),
        trace_id: p
            .get("trace_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        parent_call: p
            .get("parent_call")
            .and_then(Value::as_str)
            .map(str::to_string),
        surface: p.get("surface").and_then(Value::as_str).map(str::to_string),
        args: p.get("args").cloned().unwrap_or(Value::Null),
        ok: p.get("ok").and_then(Value::as_bool).unwrap_or(true),
        code: p.get("code").and_then(Value::as_str).map(str::to_string),
        message_len: p.get("message_len").and_then(Value::as_u64).unwrap_or(0),
        started_at: p
            .get("started_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        duration_ms: p.get("duration_ms").and_then(Value::as_u64).unwrap_or(0),
        compacted: p.get("compacted").and_then(Value::as_bool).unwrap_or(false),
    }
}

fn call_row(store: &dyn AttributedStore, call_id: &str) -> Option<Item> {
    let uuid = uuid::Uuid::parse_str(call_id).ok()?;
    store.get(uuid).ok().flatten()
}

fn error_calls(message: String) -> CallsResult {
    CallsResult {
        ok: false,
        message,
        calls: vec![],
    }
}

// ---------------------------------------------------------------------------
// The n-gram miner (W4)
// ---------------------------------------------------------------------------

const DEFAULT_MIN_REPEATS: usize = 3;
const DEFAULT_MAX_LEN: usize = 6;

/// Whether a verb's declared safety class is `mutating` (not `read_only`,
/// not `destructive`, not `external`) — the class the miner proposes
/// sequences of. An unlinked verb (should not happen for a recorded call,
/// but the log outlives a build that removed one) is never mutating.
fn is_mutating(verb: &str) -> bool {
    call::find(verb).is_some_and(|d| d.safety().class == SafetyClass::Mutating)
}

/// A stable grouping key for one call's caller: prefer the name (an agent
/// or a surface names itself), falling back to the kind alone.
fn caller_key(caller: &Value) -> String {
    let kind = caller.get("kind").and_then(Value::as_str).unwrap_or("?");
    match caller.get("name").and_then(Value::as_str) {
        Some(name) => format!("{kind}:{name}"),
        None => kind.to_string(),
    }
}

/// One repeated sequence the miner found: `verbs[i]` is the verb of every
/// `matches[*][i]`; `matches` holds one clone of the matched calls per
/// repeat, in the order they occurred.
struct MinedGroup {
    verbs: Vec<String>,
    matches: Vec<Vec<CallSummary>>,
}

/// Deliberately excludes recorded argument values from operational logs.
#[derive(Debug)]
enum ProposalWriteError {
    ArgumentsNotObject,
    ArgumentKeysDiffer,
    StepDecode,
    Validation,
    Serialization,
    StoreInsert,
}

/// Finds sequences of consecutive calls in `seq` (already filtered to one
/// caller's mutating, successful calls, oldest first) that repeat, as
/// non-overlapping blocks, at least `min_repeats` times — trying the
/// longest length first (`max_len` down to 2) so a proposal is not also
/// reported again as a shorter sub-pattern of itself. A block already
/// consumed by a proposal is not considered again at a shorter length.
fn mine_ngrams(seq: &[CallSummary], min_repeats: usize, max_len: usize) -> Vec<MinedGroup> {
    let n = seq.len();
    let mut consumed = vec![false; n];
    let mut groups = Vec::new();
    let longest = max_len.min(n);
    for len in (2..=longest).rev() {
        if n / len < min_repeats {
            continue;
        }
        // Index each currently available window once. The old scan walked
        // the rest of the caller's history for every starting position,
        // making varied histories quadratic even when nothing repeated.
        let mut occurrences: BTreeMap<Vec<&str>, Vec<usize>> = BTreeMap::new();
        for start in 0..=n - len {
            if !consumed[start..start + len].contains(&true) {
                let signature = seq[start..start + len]
                    .iter()
                    .map(|call| call.verb.as_str())
                    .collect();
                occurrences.entry(signature).or_default().push(start);
            }
        }

        let mut processed: BTreeSet<Vec<&str>> = BTreeSet::new();
        for i in 0..=n - len {
            if consumed[i..i + len].contains(&true) {
                continue;
            }
            let signature: Vec<&str> = seq[i..i + len].iter().map(|c| c.verb.as_str()).collect();
            if !processed.insert(signature.clone()) {
                continue;
            }
            let mut match_starts = Vec::new();
            let mut next_allowed = i;
            for &start in &occurrences[&signature] {
                if start < next_allowed || consumed[start..start + len].contains(&true) {
                    continue;
                }
                match_starts.push(start);
                next_allowed = start + len;
            }
            if match_starts.len() >= min_repeats {
                for &start in &match_starts {
                    consumed[start..start + len].fill(true);
                }
                let verbs = signature.iter().map(|s| s.to_string()).collect();
                let matches = match_starts
                    .iter()
                    .map(|&start| seq[start..start + len].to_vec())
                    .collect();
                groups.push(MinedGroup { verbs, matches });
            }
        }
    }
    groups
}

/// Builds one step per column of a mined group: an argument that is
/// identical (by JSON value) across every repeat stays literal; one that
/// differs becomes `"{{event.value.<name>}}"`, named `step<i>_<key>`.
fn steps_from_group(group: &MinedGroup) -> Vec<Value> {
    group
        .verbs
        .iter()
        .enumerate()
        .map(|(step_idx, verb)| {
            let first_args = group.matches[0][step_idx].args.clone();
            let mut args = match &first_args {
                Value::Object(map) => map.clone(),
                _ => Default::default(),
            };
            if let Value::Object(first_map) = &first_args {
                for key in first_map.keys() {
                    let varies = group
                        .matches
                        .iter()
                        .any(|m| m[step_idx].args.get(key) != first_map.get(key));
                    if varies {
                        args.insert(
                            key.clone(),
                            Value::String(format!("{{{{event.value.step{step_idx}_{key}}}}}")),
                        );
                    }
                }
            }
            json!({ "call": { "verb": verb, "args": Value::Object(args) } })
        })
        .collect()
}

/// Validates and writes one mined group as a `proposed` workflow document.
/// Refuses (without writing) any proposal that would not pass
/// `impress_workflow::validate` — a mined sequence is a heuristic, and the
/// validator is the one place "well-formed" is decided.
fn write_proposal(
    store: &dyn AttributedStore,
    group: &MinedGroup,
) -> Result<ProposedWorkflow, ProposalWriteError> {
    // The call log can omit or privacy-reduce arguments. Even among full
    // records, a changed key set has no safe scalar value to substitute for
    // the missing key, so do not propose a step that silently drops it.
    for step_idx in 0..group.verbs.len() {
        let Some(first) = group.matches[0][step_idx].args.as_object() else {
            return Err(ProposalWriteError::ArgumentsNotObject);
        };
        if group.matches.iter().any(|repeat| {
            repeat[step_idx]
                .args
                .as_object()
                .is_none_or(|args| args.keys().ne(first.keys()))
        }) {
            return Err(ProposalWriteError::ArgumentKeysDiffer);
        }
    }
    let steps: Vec<Action> = steps_from_group(group)
        .into_iter()
        .map(|s| serde_json::from_value(s).map_err(|_| ProposalWriteError::StepDecode))
        .collect::<Result<_, _>>()?;
    let name = format!(
        "proposed.{}.{}",
        group.verbs.join("-then-"),
        &uuid::Uuid::new_v4().to_string()[..8]
    );
    let spec = WorkflowSpec {
        wire_version: 1,
        name: name.clone(),
        description: format!(
            "Mined from {} repeat(s) of: {}.",
            group.matches.len(),
            group.verbs.join(" -> ")
        ),
        state: WorkflowState::Proposed,
        author: Author {
            kind: "agent".into(),
            name: Some("history-service".into()),
        },
        trigger: Trigger::Manual {},
        guards: Guards::default(),
        params: vec![],
        sources: BTreeMap::new(),
        steps,
        review: Review { required: true },
    };
    let problems = impress_workflow::validate::validate(&spec);
    if problems
        .iter()
        .any(|p| p.severity == impress_workflow::validate::Severity::Error)
    {
        return Err(ProposalWriteError::Validation);
    }

    let doc = serde_json::to_value(&spec).map_err(|_| ProposalWriteError::Serialization)?;
    let mut payload: BTreeMap<String, ItemValue> = BTreeMap::new();
    if let Value::Object(fields) = doc {
        for (k, v) in fields {
            payload.insert(k, serde_json_to_item_value(&v));
        }
    }
    let now = Utc::now();
    let id = uuid::Uuid::new_v4();
    let outcome = store.insert(Item {
        id,
        schema: WORKFLOW_SCHEMA,
        payload,
        created: now,
        modified: now,
        author: "history-service".into(),
        author_kind: ActorKind::Agent,
        logical_clock: 0,
        origin: None,
        canonical_id: None,
        tags: vec![],
        flag: None,
        is_read: false,
        is_starred: false,
        priority: Priority::None,
        visibility: Visibility::Private,
        message_type: None,
        produced_by: None,
        version: None,
        batch_id: None,
        references: vec![],
        parent: None,
        extra: Default::default(),
    });
    let id = outcome.map_err(|_| ProposalWriteError::StoreInsert)?;
    Ok(ProposedWorkflow {
        id: id.to_string(),
        name,
        verbs: group.verbs.clone(),
        repeats: group.matches.len() as u64,
    })
}

#[async_trait::async_trait]
impl HistoryService for DefaultHistoryService {
    async fn calls(
        &self,
        since: Option<String>,
        until: Option<String>,
        verb: Option<String>,
        caller: Option<String>,
        trace_id: Option<String>,
        limit: i64,
    ) -> CallsResult {
        let store = self.store();
        let query = ItemQuery {
            schema: Some(VERB_CALL_SCHEMA),
            sort: vec![SortDescriptor {
                field: "created".into(),
                ascending: false,
            }],
            ..Default::default()
        };
        let items = match store.query(&query) {
            Ok(items) => items,
            Err(e) => return error_calls(e.to_string()),
        };
        let cap = if limit <= 0 {
            50usize
        } else {
            (limit as usize).min(500)
        };
        let mut summaries: Vec<CallSummary> = items.iter().map(call_summary).collect();
        summaries.retain(|c| {
            since.as_deref().is_none_or(|s| c.started_at.as_str() >= s)
                && until.as_deref().is_none_or(|u| c.started_at.as_str() <= u)
                && verb.as_deref().is_none_or(|v| c.verb == v)
                && trace_id.as_deref().is_none_or(|t| c.trace_id == t)
                && caller.as_deref().is_none_or(|want| {
                    c.caller
                        .get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|name| name == want)
                        || c.caller.get("kind").and_then(Value::as_str) == Some(want)
                })
        });
        summaries.truncate(cap);
        CallsResult {
            ok: true,
            message: format!("{} call(s).", summaries.len()),
            calls: summaries,
        }
    }

    async fn why(&self, id: String) -> WhyResult {
        let target: ItemId = match id.parse() {
            Ok(uuid) => uuid,
            Err(e) => {
                return WhyResult {
                    ok: false,
                    id,
                    message: format!("not a UUID: {e}"),
                    entries: vec![],
                }
            }
        };
        let store = self.store();
        let ops = match store.operations_for(target, None) {
            Ok(ops) => ops,
            Err(e) => {
                return WhyResult {
                    ok: false,
                    id,
                    message: e.to_string(),
                    entries: vec![],
                }
            }
        };
        let mut entries: Vec<WhyEntry> = ops
            .iter()
            .filter_map(|op| {
                let batch_id = op.batch_id.clone()?;
                let call = call_row(&*store, &batch_id).as_ref().map(call_summary);
                Some(WhyEntry {
                    operation_id: Some(op.id.to_string()),
                    applied_at: op.created.to_rfc3339_opts(SecondsFormat::Millis, true),
                    batch_id,
                    call,
                })
            })
            .collect();
        entries.reverse(); // For equal timestamps, keep operations_for's newest-first order.
                           // Direct store inserts/deletes deliberately mint no operation row.
                           // Their affected IDs live on the call row. A call that also wrote an
                           // operation for this item is already represented above; show it once.
        let operation_calls: std::collections::HashSet<String> =
            entries.iter().map(|entry| entry.batch_id.clone()).collect();
        let call_rows = match store.query(&ItemQuery {
            schema: Some(VERB_CALL_SCHEMA),
            include_tags: false,
            include_references: false,
            assume_schema_rare: true,
            ..Default::default()
        }) {
            Ok(rows) => rows,
            Err(e) => {
                return WhyResult {
                    ok: false,
                    id,
                    message: e.to_string(),
                    entries: vec![],
                }
            }
        };
        let canonical_id = target.to_string();
        for row in &call_rows {
            let call_id = row.id.to_string();
            if operation_calls.contains(call_id.as_str()) {
                continue;
            }
            let payload = payload_json(row);
            let affected = ["inserted_ids", "deleted_ids"].iter().any(|field| {
                payload
                    .get(*field)
                    .and_then(Value::as_array)
                    .is_some_and(|ids| {
                        ids.iter()
                            .any(|value| value.as_str() == Some(canonical_id.as_str()))
                    })
            });
            if !affected {
                continue;
            }
            let call = call_summary(row);
            let applied_at = chrono::DateTime::parse_from_rfc3339(&call.started_at)
                .map(|time| time.with_timezone(&Utc))
                .unwrap_or(row.created)
                .to_rfc3339_opts(SecondsFormat::Millis, true);
            entries.push(WhyEntry {
                operation_id: None,
                applied_at,
                batch_id: call_id,
                call: Some(call),
            });
        }
        entries.sort_by(|a, b| b.applied_at.cmp(&a.applied_at));
        WhyResult {
            ok: true,
            id,
            message: format!("{} change(s) affected this item.", entries.len()),
            entries,
        }
    }

    async fn trace(&self, trace_id: String) -> TraceResult {
        let store = self.store();
        let query = ItemQuery {
            schema: Some(VERB_CALL_SCHEMA),
            predicates: vec![Predicate::Eq(
                "trace_id".into(),
                serde_json_to_item_value(&Value::String(trace_id.clone())),
            )],
            sort: vec![SortDescriptor {
                field: "created".into(),
                ascending: true,
            }],
            ..Default::default()
        };
        let items = match store.query(&query) {
            Ok(items) => items,
            Err(e) => {
                return TraceResult {
                    ok: false,
                    trace_id,
                    message: e.to_string(),
                    roots: vec![],
                }
            }
        };
        let summaries: Vec<CallSummary> = items.iter().map(call_summary).collect();
        let by_id: BTreeMap<String, CallSummary> = summaries
            .iter()
            .map(|c| (c.call_id.clone(), c.clone()))
            .collect();
        let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for c in &summaries {
            if let Some(parent) = &c.parent_call {
                if by_id.contains_key(parent) {
                    children
                        .entry(parent.clone())
                        .or_default()
                        .push(c.call_id.clone());
                }
            }
        }
        fn build(
            id: &str,
            by_id: &BTreeMap<String, CallSummary>,
            children: &BTreeMap<String, Vec<String>>,
        ) -> TraceNode {
            let call = by_id.get(id).cloned().expect("id came from by_id");
            let kids = children
                .get(id)
                .into_iter()
                .flatten()
                .map(|kid| build(kid, by_id, children))
                .collect();
            TraceNode {
                call,
                children: kids,
            }
        }
        let roots: Vec<TraceNode> = summaries
            .iter()
            .filter(|c| {
                c.parent_call
                    .as_deref()
                    .is_none_or(|p| !by_id.contains_key(p))
            })
            .map(|c| build(&c.call_id, &by_id, &children))
            .collect();
        TraceResult {
            ok: true,
            trace_id,
            message: format!("{} call(s) under this trace.", summaries.len()),
            roots,
        }
    }

    async fn replay(&self, call_ids: Vec<String>, dry_run: bool) -> ReplayResult {
        let store = self.store();
        let mut results = Vec::with_capacity(call_ids.len());
        for call_id in &call_ids {
            let Some(item) = call_row(&*store, call_id) else {
                results.push(ReplayOutcome {
                    call_id: call_id.clone(),
                    verb: None,
                    ok: false,
                    code: Some(codes::NOT_FOUND.to_string()),
                    message: format!("no call row for {call_id}"),
                });
                continue;
            };
            let payload = payload_json(&item);
            let verb_name = payload
                .get("verb")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if !args_are_full(&payload) {
                results.push(ReplayOutcome {
                    call_id: call_id.clone(),
                    verb: Some(verb_name.clone()),
                    ok: false,
                    code: Some(codes::NOT_REPLAYABLE.to_string()),
                    message: format!(
                        "{call_id} ({verb_name}) was not recorded in full — privacy-reduced or compacted — and cannot be replayed"
                    ),
                });
                continue;
            }
            let Some(descriptor) = call::find(&verb_name) else {
                results.push(ReplayOutcome {
                    call_id: call_id.clone(),
                    verb: Some(verb_name.clone()),
                    ok: false,
                    code: Some(codes::NOT_FOUND.to_string()),
                    message: format!("verb {verb_name} is not linked in this process"),
                });
                continue;
            };
            let args = payload.get("args").cloned().unwrap_or(json!({}));
            if dry_run {
                results.push(ReplayOutcome {
                    call_id: call_id.clone(),
                    verb: Some(verb_name.clone()),
                    ok: true,
                    code: None,
                    message: format!("would call {verb_name} with {args}"),
                });
                continue;
            }
            let original_caller = payload
                .get("caller")
                .and_then(|c| c.get("name"))
                .and_then(Value::as_str)
                .unwrap_or(&item.author);
            let mut call = Call::agent(format!("replay:{original_caller}"), args);
            call.store = self.store.as_ref().map(|s| s.erased());
            match pipeline::invoke_handle(descriptor, call).await {
                Ok(answer) => {
                    let ok = answer.get("ok").and_then(Value::as_bool).unwrap_or(true);
                    results.push(ReplayOutcome {
                        call_id: call_id.clone(),
                        verb: Some(verb_name),
                        ok,
                        code: answer
                            .get("code")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        message: answer
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("replayed")
                            .to_string(),
                    });
                }
                Err(e) => {
                    results.push(ReplayOutcome {
                        call_id: call_id.clone(),
                        verb: Some(verb_name),
                        ok: false,
                        code: Some(codes::VERB_FAILED.to_string()),
                        message: e.to_string(),
                    });
                }
            }
        }
        let ok = results.iter().all(|r| r.ok);
        ReplayResult {
            ok,
            dry_run,
            message: format!(
                "{}/{} call(s) {}.",
                results.iter().filter(|r| r.ok).count(),
                results.len(),
                if dry_run { "would replay" } else { "replayed" }
            ),
            results,
        }
    }

    async fn save_macro(&self, call_ids: Vec<String>, name: String) -> SaveMacroResult {
        let store = self.store();
        let mut steps = Vec::with_capacity(call_ids.len());
        for call_id in &call_ids {
            let Some(item) = call_row(&*store, call_id) else {
                return SaveMacroResult {
                    ok: false,
                    id: String::new(),
                    message: format!("no call row for {call_id}; nothing saved"),
                };
            };
            let payload = payload_json(&item);
            let verb = payload
                .get("verb")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let args = payload.get("args").cloned().unwrap_or(json!({}));
            steps.push(json!({ "call": { "verb": verb, "args": args } }));
        }
        let author = pipeline::context::current()
            .map(|c| c.caller.to_json())
            .unwrap_or_else(|| json!({"kind": "agent", "name": "history-service"}));
        let now = Utc::now();
        let mut payload: BTreeMap<String, ItemValue> = BTreeMap::new();
        let doc = json!({
            "wire_version": 1,
            "name": name,
            "description": format!("Macro of {} recorded call(s).", steps.len()),
            "state": "proposed",
            "author": author,
            "trigger": { "manual": {} },
            "guards": {},
            "params": [],
            "sources": {},
            "steps": steps,
            "review": { "required": true },
        });
        if let Value::Object(fields) = doc {
            for (k, v) in fields {
                payload.insert(k, serde_json_to_item_value(&v));
            }
        }
        let id = uuid::Uuid::new_v4();
        let outcome = store.insert(Item {
            id,
            schema: WORKFLOW_SCHEMA,
            payload,
            created: now,
            modified: now,
            author: "history-service".into(),
            author_kind: ActorKind::Agent,
            logical_clock: 0,
            origin: None,
            canonical_id: None,
            tags: vec![],
            flag: None,
            is_read: false,
            is_starred: false,
            priority: Priority::None,
            visibility: Visibility::Private,
            message_type: None,
            produced_by: None,
            version: None,
            batch_id: None,
            references: vec![],
            parent: None,
            extra: Default::default(),
        });
        match outcome {
            Ok(id) => SaveMacroResult {
                ok: true,
                id: id.to_string(),
                message: format!(
                    "Saved macro '{name}' with {} step(s), proposed.",
                    call_ids.len()
                ),
            },
            Err(e) => SaveMacroResult {
                ok: false,
                id: String::new(),
                message: e.to_string(),
            },
        }
    }

    async fn propose_workflows(
        &self,
        since: Option<String>,
        min_repeats: i64,
        max_len: i64,
    ) -> ProposeWorkflowsResult {
        tracing::info!(
            target: "verb",
            "propose_workflows requested: since={} min_repeats={} max_len={}",
            since.as_deref().unwrap_or("<all>"),
            min_repeats,
            max_len
        );
        let store = self.store();
        let min_repeats = if min_repeats <= 0 {
            DEFAULT_MIN_REPEATS
        } else {
            min_repeats as usize
        };
        let max_len = if max_len <= 0 {
            DEFAULT_MAX_LEN
        } else {
            (max_len as usize).min(20)
        };

        let query = ItemQuery {
            schema: Some(VERB_CALL_SCHEMA),
            sort: vec![SortDescriptor {
                field: "created".into(),
                ascending: true,
            }],
            ..Default::default()
        };
        let items = match store.query(&query) {
            Ok(items) => items,
            Err(e) => {
                tracing::warn!(target: "verb", "propose_workflows call-log query failed: {e}");
                return ProposeWorkflowsResult {
                    ok: false,
                    message: e.to_string(),
                    proposed: vec![],
                };
            }
        };
        let mut summaries: Vec<CallSummary> = items.iter().map(call_summary).collect();
        if let Some(since) = &since {
            summaries.retain(|c| c.started_at.as_str() >= since.as_str());
        }
        // Only mutating calls that succeeded are candidate steps: a
        // read-only call proposes nothing to run, and a failed call is not
        // a sequence worth repeating.
        summaries.retain(|c| {
            c.ok && is_mutating(&c.verb)
                && !c.compacted
                && c.args.is_object()
                && !is_reduced(&c.args)
        });

        // Group by caller, preserving each caller's own relative order —
        // "consecutive" means in that caller's stream, not the whole log.
        let mut by_caller: BTreeMap<String, Vec<CallSummary>> = BTreeMap::new();
        for c in summaries {
            by_caller.entry(caller_key(&c.caller)).or_default().push(c);
        }

        let mut proposed = Vec::new();
        let mut skipped = 0;
        for seq in by_caller.into_values() {
            for group in mine_ngrams(&seq, min_repeats, max_len) {
                match write_proposal(&*store, &group) {
                    Ok(pw) => {
                        tracing::info!(
                            target: "verb",
                            "propose_workflows saved id={} steps={} repeats={}",
                            pw.id,
                            pw.verbs.len(),
                            pw.repeats
                        );
                        proposed.push(pw);
                    }
                    Err(error) => {
                        skipped += 1;
                        tracing::warn!(
                            target: "verb",
                            "propose_workflows skipped group: {error:?} (verbs={}, repeats={})",
                            group.verbs.join(" -> "),
                            group.matches.len()
                        );
                    }
                }
            }
        }
        tracing::info!(
            target: "verb",
            "propose_workflows returning {} proposal(s), {skipped} skipped",
            proposed.len()
        );
        ProposeWorkflowsResult {
            ok: true,
            message: format!("{} workflow(s) proposed.", proposed.len()),
            proposed,
        }
    }

    async fn health(&self) -> HistoryHealth {
        let store = self.store();
        let sink_health = crate::health();
        let rows = store
            .count(&ItemQuery {
                schema: Some(VERB_CALL_SCHEMA),
                ..Default::default()
            })
            .unwrap_or(0);
        let oldest = store
            .query(&ItemQuery {
                schema: Some(VERB_CALL_SCHEMA),
                sort: vec![SortDescriptor {
                    field: "created".into(),
                    ascending: true,
                }],
                limit: Some(1),
                ..Default::default()
            })
            .ok()
            .and_then(|items| items.into_iter().next())
            .map(|item| call_summary(&item).started_at);
        HistoryHealth {
            ok: true,
            message: format!("{rows} call row(s) recorded."),
            written: sink_health["written"].as_u64().unwrap_or(0),
            dropped: sink_health["dropped"].as_u64().unwrap_or(0),
            failed: sink_health["failed"].as_u64().unwrap_or(0),
            no_sink: sink_health["no_sink"].as_u64().unwrap_or(0),
            channel_capacity: sink_health["channel_capacity"].as_u64().unwrap_or(0),
            rows,
            oldest,
            kind_mismatches: impress_item::kind::validation_failures().0,
            last_kind_mismatch: impress_item::kind::validation_failures().1,
        }
    }
}

/// `serde_json::Value` → `impress_item::Value`, for a predicate or a
/// hand-built payload (both otherwise want the crate's own value type).
fn serde_json_to_item_value(value: &Value) -> ItemValue {
    serde_json::from_value(value.clone()).unwrap_or(ItemValue::Null)
}

impress_service_impl! {
    service = HistoryService,
    safety = read_only,
    since = "0.1.0",
    effects = {
        reads: [any("history reads any kind's operations and every call row")],
        writes: [],
        reach: [],
    },
    impl = DefaultHistoryService,
    instance = DefaultHistoryService::new,
    strict_args = true,
    methods = [
        calls(
            /// RFC 3339 lower bound on `started_at`, inclusive.
            since: Option<String>,
            /// RFC 3339 upper bound on `started_at`, inclusive.
            until: Option<String>,
            /// Exact verb name (`imbib-tags-service_add-tag`).
            verb: Option<String>,
            /// Matches the caller's `name` exactly, or its `kind`
            /// ("human", "agent", "system").
            caller: Option<String>,
            /// Optional exact trace identifier used to select related calls.
            trace_id: Option<String>,
            /// Page size; 0 for the default (50), clamped above 500.
            limit: i64,
        ) -> CallsResult,
        why(
            /// The item's id, a lowercase UUID string, of any record kind.
            id: String,
        ) -> WhyResult,
        trace(
            /// The trace id every nested call under one entry point shares.
            trace_id: String,
        ) -> TraceResult,
        replay(
            /// Call ids to re-invoke, as `core/verb-call@1.0.0` row ids.
            call_ids: Vec<String>,
            /// List what would run, and why a call cannot be, without
            /// running anything.
            dry_run: bool,
        ) -> ReplayResult,
        save_macro(
            /// The call ids to turn into a workflow's steps, in order.
            call_ids: Vec<String>,
            /// The saved workflow's `name`.
            name: String,
        ) -> SaveMacroResult,
        propose_workflows(
            /// RFC 3339 lower bound on `started_at`, inclusive; unbounded
            /// when absent.
            since: Option<String>,
            /// A repeated sequence must occur at least this many times.
            /// 0 or absent means the default (3).
            min_repeats: i64,
            /// The longest sequence length the miner considers. 0 or absent
            /// means the default (6), clamped above 20.
            max_len: i64,
        ) -> ProposeWorkflowsResult,
        health() -> HistoryHealth,
    ],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_replay_privacy_marker_overrides_the_legacy_shape_guess() {
        assert!(!args_are_full(
            &json!({"args": {"token": null}, "args_replayable": false})
        ));
        assert!(args_are_full(
            &json!({"args": {"spec": {"len": 2, "keys": ["a", "b"]}}, "args_replayable": true})
        ));
        assert!(!args_are_full(
            &json!({"args": {}, "args_replayable": true, "compacted": true})
        ));
    }
    use impress_item::FieldMutation;
    use impress_item::ItemStore;
    use impress_item::Value as CoreValue;
    use koine_store_memory::MemoryStore;

    /// A fresh store, with the call log's sink installed once per process:
    /// a call that runs on a scoped memory store is recorded there.
    fn test_store() -> Arc<MemoryStore> {
        // The triage verbs the tests drive must be in the inventory.
        std::hint::black_box(std::ptr::addr_of!(
            koine_triage::service::__IMPRESS_VERB_TriageService_set_starred
        ));
        static SINK: std::sync::Once = std::sync::Once::new();
        SINK.call_once(|| {
            crate::install(
                Arc::new(MemoryStore::new()),
                crate::sink::SinkConfig {
                    keep: 0,
                    ..crate::sink::SinkConfig::new("test", "test", "0")
                },
            );
        });
        Arc::new(MemoryStore::new())
    }

    fn flush() {
        impress_service_core::pipeline::audit::flush(std::time::Duration::from_secs(5))
            .expect("audit flush");
    }

    /// Write one `core/verb-call@1.0.0` row as the sink would.
    fn record_verb_call(
        store: &dyn AttributedStore,
        call_id: &str,
        caller: &impress_service_core::pipeline::CallerIdentity,
        payload: serde_json::Map<String, Value>,
    ) -> Result<ItemId, impress_item::StoreError> {
        let fields: BTreeMap<String, ItemValue> =
            serde_json::from_value(Value::Object(payload)).expect("payload");
        let now = Utc::now();
        store.insert(Item {
            id: call_id.parse().expect("call id"),
            schema: VERB_CALL_SCHEMA,
            payload: fields,
            created: now,
            modified: now,
            author: caller.author(),
            author_kind: crate::records::actor_kind(caller),
            logical_clock: 0,
            origin: None,
            canonical_id: None,
            tags: vec![],
            flag: None,
            is_read: false,
            is_starred: false,
            priority: Priority::None,
            visibility: Visibility::Private,
            message_type: None,
            produced_by: None,
            version: None,
            batch_id: Some(call_id.to_string()),
            references: vec![],
            parent: None,
            extra: Default::default(),
        })
    }

    fn make_item(store: &dyn AttributedStore, schema: &str) -> String {
        let now = Utc::now();
        let mut payload = BTreeMap::new();
        payload.insert("title".to_string(), ItemValue::String("Untitled".into()));
        store
            .insert(Item {
                id: uuid::Uuid::new_v4(),
                schema: serde_json::from_value(Value::String(schema.into())).expect("schema"),
                payload,
                created: now,
                modified: now,
                author: "test".into(),
                author_kind: ActorKind::System,
                logical_clock: 0,
                origin: None,
                canonical_id: None,
                tags: vec![],
                flag: None,
                is_read: false,
                is_starred: false,
                priority: Priority::None,
                visibility: Visibility::Private,
                message_type: None,
                produced_by: None,
                version: None,
                batch_id: None,
                references: vec![],
                parent: None,
                extra: Default::default(),
            })
            .expect("insert item")
            .to_string()
    }
    use impress_service_core::pipeline::Call as PipeCall;
    use impress_service_core::VerbDescriptor;

    fn item(store: &dyn AttributedStore) -> uuid::Uuid {
        make_item(store, "manuscript").parse().unwrap()
    }

    fn paper(store: &dyn AttributedStore) -> uuid::Uuid {
        let now = Utc::now();
        let id = uuid::Uuid::new_v4();
        store
            .insert(Item {
                id,
                schema: impress_item::schema::refs::IMBIB_BIBLIOGRAPHY_ENTRY,
                payload: Default::default(),
                created: now,
                modified: now,
                author: "test".into(),
                author_kind: ActorKind::Human,
                logical_clock: 0,
                origin: None,
                canonical_id: None,
                tags: vec![],
                flag: None,
                is_read: false,
                is_starred: false,
                priority: Priority::None,
                visibility: Visibility::Private,
                message_type: None,
                produced_by: None,
                version: None,
                batch_id: None,
                references: vec![],
                parent: None,
                extra: Default::default(),
            })
            .expect("insert paper");
        id
    }

    fn affected_call(
        store: &dyn AttributedStore,
        id: uuid::Uuid,
        field: &str,
        started_at: chrono::DateTime<Utc>,
        caller: impress_service_core::pipeline::CallerIdentity,
    ) -> String {
        let call_id = uuid::Uuid::new_v4().to_string();
        let mut payload = serde_json::json!({
            "verb": "imbib-library-service_retention-cleanup",
            "caller": caller.to_json(),
            "started_at": started_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        })
        .as_object()
        .unwrap()
        .clone();
        payload.insert(field.into(), json!([id.to_string()]));
        record_verb_call(store, &call_id, &caller, payload).expect("record call");
        call_id
    }

    fn set_starred(store: Arc<MemoryStore>, id: uuid::Uuid, caller: &str) -> Value {
        let verb = VerbDescriptor::find("triage-service_set-starred").expect("linked");
        impress_service_core::runtime::block_on(pipeline::invoke_on(
            store,
            verb,
            PipeCall::agent(caller, json!({"id": id.to_string(), "starred": true})),
        ))
        .expect("verb ran")
    }

    /// The Tier A proof: an item created and starred through the real
    /// pipeline, then `why` names the call, its caller and the verb.
    #[test]
    fn why_names_the_call_its_caller_and_the_verb() {
        let store = test_store();
        let id = item(&*store);
        let answer = set_starred(store.clone(), id, "test-agent");
        assert_eq!(answer["ok"], true, "{answer}");
        flush();

        let svc = DefaultHistoryService::with_store(store);
        let result = impress_service_core::runtime::block_on(svc.why(id.to_string()));
        assert!(result.ok, "{}", result.message);
        assert_eq!(result.entries.len(), 1, "one operation wrote this item");
        let call = result.entries[0]
            .call
            .as_ref()
            .expect("the call row was written before we flushed");
        assert_eq!(call.verb, "triage-service_set-starred");
        assert_eq!(call.caller, json!({"kind": "agent", "name": "test-agent"}));
        assert!(!result.entries[0].applied_at.is_empty());
        assert!(result.entries[0].operation_id.is_some());
    }

    #[test]
    fn why_shows_workflow_caller_for_a_deleted_paper_without_an_operation() {
        let store = test_store();
        let id = paper(&*store);
        let now = Utc::now();
        let inserted_call = affected_call(
            &*store,
            id,
            "inserted_ids",
            now - chrono::Duration::hours(2),
            impress_service_core::pipeline::CallerIdentity::agent("importer"),
        );
        store.delete(id).expect("direct delete");
        let deleted_call = affected_call(
            &*store,
            id,
            "deleted_ids",
            now - chrono::Duration::hours(1),
            impress_service_core::pipeline::CallerIdentity::system("imbib.retention-cleanup"),
        );
        assert!(store.operations_for(id, None).unwrap().is_empty());

        let result = impress_service_core::runtime::block_on(
            DefaultHistoryService::with_store(store).why(id.to_string()),
        );
        assert!(result.ok, "{}", result.message);
        assert_eq!(result.entries.len(), 2);
        assert_eq!(result.entries[0].batch_id, deleted_call);
        assert_eq!(result.entries[1].batch_id, inserted_call);
        assert!(result
            .entries
            .iter()
            .all(|entry| entry.operation_id.is_none()));
        assert_eq!(
            result.entries[0].call.as_ref().unwrap().caller,
            json!({"kind": "system", "name": "imbib.retention-cleanup"})
        );
    }

    #[test]
    fn why_does_not_duplicate_an_operation_linked_call_that_names_the_item() {
        let store = test_store();
        let id = item(&*store);
        set_starred(store.clone(), id, "test-agent");
        flush();
        let call_id = store.operations_for(id, None).unwrap()[0]
            .batch_id
            .clone()
            .unwrap();
        store
            .update(
                call_id.parse().unwrap(),
                vec![FieldMutation::SetPayload(
                    "deleted_ids".into(),
                    CoreValue::Array(vec![CoreValue::String(id.to_string())]),
                )],
            )
            .unwrap();

        let result = impress_service_core::runtime::block_on(
            DefaultHistoryService::with_store(store).why(id.to_string()),
        );
        assert!(result.ok, "{}", result.message);
        assert_eq!(result.entries.len(), 1);
        assert!(result.entries[0].operation_id.is_some());
        assert_eq!(result.entries[0].batch_id, call_id);
    }

    #[test]
    fn trace_nests_a_nested_call_under_its_parent() {
        let store = test_store();
        let id_a = item(&*store);
        let id_b = item(&*store);
        let verb = VerbDescriptor::find("triage-service_set-starred").expect("linked");
        let trace_id = uuid::Uuid::new_v4().to_string();
        let call_a = PipeCall::agent("test", json!({"id": id_a.to_string(), "starred": true}))
            .with_trace(trace_id.clone());
        let call_b = PipeCall::agent("test", json!({"id": id_b.to_string(), "starred": true}))
            .with_trace(trace_id.clone());
        impress_service_core::runtime::block_on(pipeline::invoke_on(store.clone(), verb, call_a))
            .unwrap();
        impress_service_core::runtime::block_on(pipeline::invoke_on(store.clone(), verb, call_b))
            .unwrap();
        flush();

        let svc = DefaultHistoryService::with_store(store);
        let result = impress_service_core::runtime::block_on(svc.trace(trace_id.clone()));
        assert!(result.ok, "{}", result.message);
        assert_eq!(result.roots.len(), 2, "both calls are roots, no parent");
        assert!(result.roots.iter().all(|r| r.call.trace_id == trace_id));
    }

    #[test]
    fn replay_refuses_a_call_whose_args_were_privacy_reduced() {
        let store = test_store();
        let id = item(&*store);
        // A long string argument is reduced to {len, sha256_8} by the audit
        // layer's own privacy rule, so the recorded call is not full.
        let long = "x".repeat(200);
        let verb = VerbDescriptor::find("triage-service_add-tag").expect("linked");
        let answer = impress_service_core::runtime::block_on(pipeline::invoke_on(
            store.clone(),
            verb,
            PipeCall::agent("test", json!({"id": id.to_string(), "tag": long})),
        ))
        .expect("verb ran");
        assert_eq!(answer["ok"], true, "{answer}");
        flush();

        let calls = impress_service_core::runtime::block_on(
            DefaultHistoryService::with_store(store.clone()).calls(
                None,
                None,
                Some("triage-service_add-tag".into()),
                None,
                None,
                0,
            ),
        );
        let call_id = calls
            .calls
            .first()
            .expect("one call recorded")
            .call_id
            .clone();

        let svc = DefaultHistoryService::with_store(store);
        let result = impress_service_core::runtime::block_on(svc.replay(vec![call_id], false));
        assert!(!result.ok);
        assert_eq!(
            result.results[0].code.as_deref(),
            Some(codes::NOT_REPLAYABLE)
        );
    }

    #[test]
    fn replay_dry_run_lists_a_full_call_without_running_it() {
        let store = test_store();
        let id = item(&*store);
        set_starred(store.clone(), id, "test");
        flush();

        let calls = impress_service_core::runtime::block_on(
            DefaultHistoryService::with_store(store.clone()).calls(None, None, None, None, None, 0),
        );
        let call_id = calls
            .calls
            .first()
            .expect("one call recorded")
            .call_id
            .clone();

        let svc = DefaultHistoryService::with_store(store);
        let result = impress_service_core::runtime::block_on(svc.replay(vec![call_id], true));
        assert!(result.ok, "{result:?}");
        assert!(result.dry_run);
        assert!(result.results[0].message.contains("would call"));
    }

    #[test]
    fn runtime_provider_is_resolved_for_mutation_mining_and_replay_preview() {
        use impress_service_core::provider::{RegistrationRequest, Registry, SchemaValidator};
        use impress_service_core::registry_runtime;

        struct RestoreRegistry(Arc<Registry>);
        impl Drop for RestoreRegistry {
            fn drop(&mut self) {
                registry_runtime::install(self.0.clone());
            }
        }
        let _restore = RestoreRegistry(registry_runtime::current());

        struct AcceptSchema;
        impl SchemaValidator for AcceptSchema {
            fn validate(&self, _: &Value) -> Result<(), String> {
                Ok(())
            }
            fn validate_instance(&self, _: &Value, _: &Value) -> Result<(), String> {
                Ok(())
            }
        }

        let registry = Arc::new(Registry::new().with_validator(Arc::new(AcceptSchema)));
        let request: RegistrationRequest = serde_json::from_value(json!({
            "provider":{"id":"history-provider-fixture","language":"Python","version":"1.0.0","endpoint":"http://127.0.0.1:1"},
            "verbs":[{"name":"history-provider-fixture-service_change","description":"Change a value",
                "input_schema":{"type":"object","properties":{"value":{"type":"string","description":"Value"}},"required":["value"],"additionalProperties":false},
                "output_schema":{"type":"object","properties":{"ok":{"type":"boolean"}}},
                "safety":{"class":"mutating"},"since":"1.0.0",
                "examples":[{"name":"change","args":{"value":"hello"}}]}]
        }))
        .unwrap();
        registry.register(request).unwrap();
        registry
            .set_trusted("history-provider-fixture", true)
            .unwrap();
        registry_runtime::install(registry);
        let name = "history-provider-fixture-service_change";
        assert!(is_mutating(name));

        let store = test_store();
        let call_id = uuid::Uuid::new_v4().to_string();
        let payload = json!({
            "verb":name,"args":{"value":"hello"},"args_replayable":true,
            "caller":{"kind":"agent","name":"fixture"},"ok":true
        })
        .as_object()
        .unwrap()
        .clone();
        record_verb_call(
            &*store,
            &call_id,
            &pipeline::CallerIdentity::agent("fixture"),
            payload,
        )
        .unwrap();
        let result = impress_service_core::runtime::block_on(
            DefaultHistoryService::with_store(store).replay(vec![call_id], true),
        );
        assert!(result.ok, "{result:?}");
        assert!(result.results[0].message.contains(name));
    }

    #[test]
    fn save_macro_writes_a_proposed_workflow_document() {
        let store = test_store();
        let id = item(&*store);
        set_starred(store.clone(), id, "test");
        flush();

        let calls = impress_service_core::runtime::block_on(
            DefaultHistoryService::with_store(store.clone()).calls(None, None, None, None, None, 0),
        );
        let call_id = calls
            .calls
            .first()
            .expect("one call recorded")
            .call_id
            .clone();

        let svc = DefaultHistoryService::with_store(store.clone());
        let result = impress_service_core::runtime::block_on(
            svc.save_macro(vec![call_id], "my-macro".into()),
        );
        assert!(result.ok, "{}", result.message);
        let row = store
            .get(uuid::Uuid::parse_str(&result.id).unwrap())
            .unwrap()
            .expect("workflow row exists");
        assert_eq!(row.schema, WORKFLOW_SCHEMA);
        assert_eq!(
            row.payload.get("state"),
            Some(&CoreValue::String("proposed".into()))
        );
        let steps = row.payload.get("steps").expect("steps field");
        assert!(matches!(steps, CoreValue::Array(v) if v.len() == 1));
    }

    #[test]
    fn health_reports_the_sinks_counters_and_a_row_count() {
        let store = test_store();
        let id = item(&*store);
        set_starred(store.clone(), id, "test");
        flush();

        let svc = DefaultHistoryService::with_store(store);
        let health = impress_service_core::runtime::block_on(svc.health());
        assert!(health.ok);
        assert!(health.rows >= 1);
        assert!(health.channel_capacity > 0);
    }

    fn add_tag(store: Arc<MemoryStore>, id: uuid::Uuid, tag: &str, caller: &str) -> Value {
        let verb = VerbDescriptor::find("triage-service_add-tag").expect("linked");
        impress_service_core::runtime::block_on(pipeline::invoke_on(
            store,
            verb,
            PipeCall::agent(caller, json!({"id": id.to_string(), "tag": tag})),
        ))
        .expect("verb ran")
    }

    /// W4's proof (plan § W4): a recorded session of three identical triage
    /// sequences — star then tag, three times over — yields exactly one
    /// `proposed` workflow, with a `manual` trigger, `review.required:
    /// true`, and two steps.
    #[test]
    fn three_identical_triage_sequences_propose_one_workflow() {
        let store = test_store();
        for _ in 0..3 {
            let id = item(&*store);
            set_starred(store.clone(), id, "triage-agent");
            add_tag(store.clone(), id, "reading/queue", "triage-agent");
        }
        flush();

        let svc = DefaultHistoryService::with_store(store.clone());
        let result = impress_service_core::runtime::block_on(svc.propose_workflows(None, 0, 0));
        assert!(result.ok, "{}", result.message);
        assert_eq!(result.proposed.len(), 1, "{:?}", result.proposed);
        let proposal = &result.proposed[0];
        assert_eq!(proposal.repeats, 3);
        assert_eq!(
            proposal.verbs,
            vec![
                "triage-service_set-starred".to_string(),
                "triage-service_add-tag".to_string(),
            ]
        );

        let row = store
            .get(uuid::Uuid::parse_str(&proposal.id).unwrap())
            .unwrap()
            .expect("workflow row exists");
        assert_eq!(row.schema, WORKFLOW_SCHEMA);
        assert_eq!(
            row.payload.get("state"),
            Some(&CoreValue::String("proposed".into()))
        );
        assert_eq!(
            row.payload.get("trigger"),
            Some(&serde_json_to_item_value(&json!({"manual": {}})))
        );
        let review = row.payload.get("review").expect("review field");
        assert_eq!(
            review,
            &serde_json_to_item_value(&json!({"required": true}))
        );
        let steps = row.payload.get("steps").expect("steps field");
        assert!(matches!(steps, CoreValue::Array(v) if v.len() == 2));

        // W2's engine ignores anything not `state: enabled` (`trigger.rs`'s
        // `tick` doc: "a proposed/disabled/broken row is never passed in").
        // A freshly mined proposal's own state proves the point directly —
        // `a_proposed_workflow_never_runs_through_the_engine` below covers
        // the engine side of that filter.
        assert_eq!(
            impress_workflow::spec::WorkflowState::Proposed.as_str(),
            "proposed"
        );
        assert_ne!(
            row.payload.get("state"),
            Some(&serde_json_to_item_value(&json!("enabled")))
        );
    }

    /// W4's proof, the engine half (the engine is koine's
    /// `impress-workflow-service`, KM4): a workflow mined by
    /// `history-service_propose-workflows` is written `state: proposed`
    /// (never `enabled`), and `run_once`'s own row filter
    /// (`r.spec.state == WorkflowState::Enabled`) means the engine never
    /// runs it, however far past `start_delay` the clock is.
    #[test]
    fn a_proposed_workflow_never_runs_through_the_engine() {
        use impress_service_core::pipeline::{self, Call as PipeCall};
        use impress_service_core::VerbDescriptor;
        use impress_workflow_service::{CallStore, WorkflowEngine};

        let store = test_store();

        // Three identical two-step triage sessions by one caller — W4's
        // miner input (see `three_identical_triage_sequences_...` above for the
        // mining half).
        let star = VerbDescriptor::find("triage-service_set-starred").expect("linked");
        let tag = VerbDescriptor::find("triage-service_add-tag").expect("linked");
        for _ in 0..3 {
            let id = uuid::Uuid::new_v4();
            let mut payload = std::collections::BTreeMap::new();
            payload.insert(
                "title".to_string(),
                impress_item::Value::String("paper".into()),
            );
            store
                .insert(impress_item::Item {
                    id,
                    schema: impress_item::schema::refs::IMBIB_BIBLIOGRAPHY_ENTRY,
                    payload,
                    created: chrono::Utc::now(),
                    modified: chrono::Utc::now(),
                    author: "test".into(),
                    author_kind: impress_item::ActorKind::Agent,
                    logical_clock: 0,
                    origin: None,
                    canonical_id: None,
                    tags: vec![],
                    flag: None,
                    is_read: false,
                    is_starred: false,
                    priority: impress_item::Priority::None,
                    visibility: impress_item::Visibility::Private,
                    message_type: None,
                    produced_by: None,
                    version: None,
                    batch_id: None,
                    references: vec![],
                    parent: None,
                    extra: Default::default(),
                })
                .expect("insert paper");
            impress_service_core::runtime::block_on(pipeline::invoke_on(
                store.clone(),
                star,
                PipeCall::agent(
                    "triage-agent",
                    serde_json::json!({"id": id.to_string(), "starred": true}),
                ),
            ))
            .expect("star ran");
            impress_service_core::runtime::block_on(pipeline::invoke_on(
                store.clone(),
                tag,
                PipeCall::agent(
                    "triage-agent",
                    serde_json::json!({"id": id.to_string(), "tag": "reading/queue"}),
                ),
            ))
            .expect("tag ran");
        }
        flush();

        let svc = DefaultHistoryService::with_store(store.clone());
        let proposal = impress_service_core::runtime::block_on(svc.propose_workflows(None, 0, 0));
        assert_eq!(proposal.proposed.len(), 1, "{:?}", proposal.proposed);

        let workspace = tempfile::tempdir().unwrap();
        let mut engine = WorkflowEngine::new(0, 90_000);
        // Well past `start_delay`, and the proposed row's `manual` trigger
        // has no autonomous fire path anyway — the filter this asserts is
        // `run_once`'s row-state filter, which runs before any trigger is
        // even looked at.
        let outcomes = engine.run_once(&CallStore::of(store.clone()), 1_000_000, workspace.path());
        assert!(
            outcomes.is_empty(),
            "a proposed workflow must never run: {outcomes:?}"
        );
    }

    /// The negative half of the same proof: three *different* two-step
    /// sequences by the same caller never repeat, so nothing is proposed.
    #[test]
    fn three_different_sequences_propose_nothing() {
        let store = test_store();
        let a = item(&*store);
        let b = item(&*store);
        let c = item(&*store);
        set_starred(store.clone(), a, "triage-agent");
        add_tag(store.clone(), a, "x", "triage-agent");
        add_tag(store.clone(), b, "y", "triage-agent");
        set_starred(store.clone(), b, "triage-agent");
        set_starred(store.clone(), c, "triage-agent");
        set_starred(store.clone(), c, "triage-agent");
        flush();

        let svc = DefaultHistoryService::with_store(store);
        let result = impress_service_core::runtime::block_on(svc.propose_workflows(None, 0, 0));
        assert!(result.ok, "{}", result.message);
        assert!(result.proposed.is_empty(), "{:?}", result.proposed);
    }

    #[test]
    fn privacy_reduced_arguments_are_not_embedded_in_a_proposal() {
        let store = test_store();
        let long_tag = "private-context-".repeat(12);
        for _ in 0..3 {
            let id = item(&*store);
            set_starred(store.clone(), id, "triage-agent");
            add_tag(store.clone(), id, &long_tag, "triage-agent");
        }
        flush();

        let svc = DefaultHistoryService::with_store(store);
        let result = impress_service_core::runtime::block_on(svc.propose_workflows(None, 0, 0));
        assert!(result.ok, "{}", result.message);
        assert!(result.proposed.is_empty(), "{:?}", result.proposed);
    }

    fn mined_call(index: usize, verb: &str) -> CallSummary {
        CallSummary {
            call_id: index.to_string(),
            verb: verb.into(),
            since: String::new(),
            caller: json!({"kind": "agent", "name": "miner-test"}),
            trace_id: String::new(),
            parent_call: None,
            surface: None,
            args: json!({}),
            ok: true,
            code: None,
            message_len: 0,
            started_at: String::new(),
            duration_ms: 0,
            compacted: false,
        }
    }

    /// The original position-by-position scan, kept only as an oracle for
    /// overlap and minimum-repeat behavior on small deterministic fixtures.
    fn reference_mine_ngrams(
        seq: &[CallSummary],
        min_repeats: usize,
        max_len: usize,
    ) -> Vec<MinedGroup> {
        let n = seq.len();
        let mut consumed = vec![false; n];
        let mut groups = Vec::new();
        for len in (2..=max_len.min(n)).rev() {
            if n / len < min_repeats {
                continue;
            }
            let mut i = 0;
            while i + len <= n {
                if consumed[i..i + len].contains(&true) {
                    i += 1;
                    continue;
                }
                let signature: Vec<&str> = seq[i..i + len]
                    .iter()
                    .map(|call| call.verb.as_str())
                    .collect();
                let mut starts = vec![i];
                let mut j = i + len;
                while j + len <= n {
                    if consumed[j..j + len].contains(&true) {
                        j += 1;
                        continue;
                    }
                    let candidate: Vec<&str> = seq[j..j + len]
                        .iter()
                        .map(|call| call.verb.as_str())
                        .collect();
                    if candidate == signature {
                        starts.push(j);
                        j += len;
                    } else {
                        j += 1;
                    }
                }
                if starts.len() >= min_repeats {
                    for &start in &starts {
                        consumed[start..start + len].fill(true);
                    }
                    groups.push(MinedGroup {
                        verbs: signature.iter().map(|verb| verb.to_string()).collect(),
                        matches: starts
                            .iter()
                            .map(|&start| seq[start..start + len].to_vec())
                            .collect(),
                    });
                    i = starts[0] + len;
                } else {
                    i += 1;
                }
            }
        }
        groups
    }

    fn group_evidence(groups: Vec<MinedGroup>) -> Vec<(Vec<String>, Vec<Vec<String>>)> {
        groups
            .into_iter()
            .map(|group| {
                (
                    group.verbs,
                    group
                        .matches
                        .into_iter()
                        .map(|calls| calls.into_iter().map(|call| call.call_id).collect())
                        .collect(),
                )
            })
            .collect()
    }

    #[test]
    fn indexed_miner_preserves_overlap_and_minimum_repeat_behavior() {
        let fixtures: &[(&[&str], usize, usize)] = &[
            (&["a", "a", "a", "a", "a", "a"], 2, 3),
            (&["a", "b", "a", "b", "c", "d", "c", "d"], 2, 3),
            (&["a", "b", "x", "a", "b", "a", "b", "x", "a", "b"], 2, 3),
            (
                &["a", "b", "c", "a", "b", "c", "d", "e", "d", "e", "a", "b"],
                2,
                3,
            ),
            (&["a", "b", "a", "b"], 3, 2),
        ];
        for &(verbs, min_repeats, max_len) in fixtures {
            let calls: Vec<_> = verbs
                .iter()
                .enumerate()
                .map(|(index, verb)| mined_call(index, verb))
                .collect();
            assert_eq!(
                group_evidence(mine_ngrams(&calls, min_repeats, max_len)),
                group_evidence(reference_mine_ngrams(&calls, min_repeats, max_len)),
                "fixture: {verbs:?}"
            );
        }
    }

    #[test]
    fn indexed_miner_handles_ten_thousand_varied_calls_without_proposals() {
        let calls: Vec<_> = (0..10_000)
            .map(|index| mined_call(index, &format!("unique-verb-{index}")))
            .collect();
        assert!(mine_ngrams(&calls, 3, 6).is_empty());
    }
}
