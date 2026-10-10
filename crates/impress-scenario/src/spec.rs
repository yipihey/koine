//! The scenario spec (docs/plan-self-reflective-layer.md § Scenarios,
//! `impress/scenario@1.0.0`) — serde + `JsonSchema` data, no behaviour.
//!
//! A scenario is a starting state (`seed`), a sequence of `steps` (a verb
//! call, a surface event, a layout gesture, or a wait), and expectations
//! evaluated against each step's result. Nothing here computes: like a
//! surface spec (ADR-0033 D3), there are no expressions, conditionals or
//! loops — [`crate::interpret`] is the only place behaviour lives, and it
//! runs each step through an abstract [`crate::interpret::Caller`] so this
//! crate never depends on the pipeline or a store.

use serde::{Deserialize, Serialize};

/// The wire shape of one `impress/scenario@1.0.0` document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Scenario {
    pub wire_version: u32,
    /// Stable across catalogues (SC-1: the three catalogues keep their
    /// ids), e.g. `layout.saved_round_trip`.
    pub id: String,
    pub description: String,
    pub tier: Tier,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires: Option<Requires>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seed: Vec<SeedRecord>,
    pub steps: Vec<Step>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub teardown: Vec<Step>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_effects: Option<ExpectEffects>,
}

/// Which layer the scenario exercises — spelled the way
/// `impress_service_core::report::Tier` already is, and interconvertible
/// with it (`From` impls below) so a Tier A/B runner shares one report
/// shape instead of a fourth copy (SC-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    A,
    B,
}

impl From<Tier> for impress_service_core::report::Tier {
    fn from(t: Tier) -> Self {
        match t {
            Tier::A => impress_service_core::report::Tier::A,
            Tier::B => impress_service_core::report::Tier::B,
        }
    }
}

/// What must hold for the scenario to be run at all. Unmet ⇒ skipped, never
/// failed (the plan's "skip, not fail, when unmet").
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Requires {
    /// The app a Tier B scenario needs answering on its loopback port
    /// (`imbib`, `imprint`, `impress`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// A named layout preset the scenario assumes is shipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    /// Record kinds (schema refs) the store must already carry at least one
    /// row of.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds_present: Vec<String>,
}

/// One row to insert into the scratch store before a Tier A run.
/// `as` names it for later `{{state.<as>.…}}` reference — a seed record
/// is captured under its own name the moment it is written, before any
/// step runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SeedRecord {
    pub kind: String,
    pub payload: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#as: Option<String>,
}

/// One step. The wire shape uses mutually exclusive keys
/// (`call`/`event`/`gesture`/`wait`/`store`/`best_effort`) — modeled as a flattened enum so a step
/// document reads exactly as the plan's example shows it, not wrapped in a
/// `{"kind": "call", …}` envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum Step {
    Call(CallStep),
    Event(EventStep),
    Gesture(GestureStep),
    Wait(WaitStep),
    Store(StoreStep),
    BestEffort(BestEffortStep),
}

/// A verb call — the common case, and the only step kind a Tier A run can
/// exercise on an arbitrary verb (an `event`/`gesture` needs a surface or a
/// layout tree already in play).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CallStep {
    pub call: String,
    #[serde(default)]
    pub args: serde_json::Value,
    /// The caller identity to run this step as: `"person"` or
    /// `"agent:<name>"`. Defaults to `"agent:scenario"` — a scenario is an
    /// agent-authored artifact unless a step says otherwise.
    #[serde(default = "default_as", rename = "as")]
    pub r#as: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<Expect>,
    /// Named captures out of this step's resolved arguments or result. A
    /// string is a result JSON path; closed object forms provide argument
    /// capture and the bounded `select_one`/`fill_array` operations. Later
    /// steps reference captures as `{{state.name}}`.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub capture: std::collections::BTreeMap<String, CallCapture>,
}

/// A closed capture operation on one call's resolved arguments or result. A
/// string keeps the original result JSON-path spelling; tagged forms capture
/// one argument, select one unique result member, or build a bounded constant
/// array from an earlier capture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum CallCapture {
    Path(String),
    Argument(ArgumentCapture),
    SelectOne(SelectOneCapture),
    FillArray(FillArrayCapture),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ArgumentCapture {
    /// Fixed JSON path into the arguments after template and UUID resolution.
    pub argument: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SelectOneCapture {
    pub select_one: SelectOneQuery,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SelectOneQuery {
    /// Fixed JSON path to an object or array in this call result.
    pub from: String,
    /// Fixed path relative to each candidate value.
    pub path: String,
    pub predicate: SelectPredicate,
    /// Convert a decimal object key to a JSON u64 as `numeric_key`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_key_as: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum SelectPredicate {
    Equals(EqualsPredicate),
    ArrayContains(ArrayContainsPredicate),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct EqualsPredicate {
    pub equals: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ArrayContainsPredicate {
    pub array_contains: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FillArrayCapture {
    pub fill_array: FillArraySpec,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FillArraySpec {
    /// JSON value repeated in the result array.
    pub value: serde_json::Value,
    /// Whole capture reference ending at an array, e.g. `{{state.parent.value.children}}`.
    pub length_of: String,
}

/// Select the first stored item satisfying every predicate. Reads use the
/// existing list-items/get-item verbs through the caller; the interpreter
/// gains no store dependency. Captures see `{item: envelope, payload: object}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct StoreStep {
    pub store: StorePredicate,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub capture: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct StorePredicate {
    pub schema_ref: String,
    #[serde(default, rename = "where")]
    pub predicates: Vec<FieldExpect>,
    /// Bound work even if the store changes during paging. 1..=10,000.
    #[serde(default = "default_max_rows")]
    pub max_rows: usize,
}

fn default_max_rows() -> usize {
    100
}

/// An optional operation. Operational failures are reported in the scenario's
/// detail and execution continues. No assertions or captures are accepted;
/// missing template captures remain authoring errors and fail the scenario.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BestEffortStep {
    pub best_effort: BestEffortCall,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BestEffortCall {
    pub call: String,
    #[serde(default)]
    pub args: serde_json::Value,
    #[serde(default = "default_as", rename = "as")]
    pub r#as: String,
}

fn default_as() -> String {
    "agent:scenario".to_string()
}

/// A human surface event — `POST /api/surface/{id}/dispatch` in Tier B; not
/// runnable in Tier A until a surface-hosting store exists per scenario
/// (out of S1's scope; validated but refused at run time with a clear
/// message rather than silently skipped).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EventStep {
    pub event: EventBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EventBody {
    pub surface: String,
    pub widget: String,
    pub kind: String,
    #[serde(default)]
    pub value: serde_json::Value,
}

/// A human layout verb — `POST /api/layout/verb` in Tier B, or the layout
/// service's own verb dispatch in Tier A (a `gesture` is a `call` to a
/// layout verb under the hood; kept as its own step kind because it always
/// targets the layout, not an arbitrary verb).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GestureStep {
    pub gesture: serde_json::Value,
    /// Capture fields from a gesture result for later steps or cleanup.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub capture: std::collections::BTreeMap<String, String>,
}

/// Wait for a job to reach a state, or for a log line to appear.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WaitStep {
    pub wait: WaitBody,
    /// Run this wait only when a prior capture-state path exists and is not null.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_present: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum WaitBody {
    Job {
        job: String,
        state: String,
        timeout_ms: u64,
    },
    Log {
        log: LogWait,
    },
    /// Capture a Tier B log timestamp immediately before a later mutation.
    /// Its value is available to `LogWait::after` as a normal capture.
    LogCursor {
        log_cursor: LogCursorCapture,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LogCursorCapture {
    pub capture: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LogWait {
    pub category: String,
    pub contains: String,
    /// Additional required substrings in the same message. All needles are
    /// matched case-insensitively.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_contains: Vec<String>,
    /// Only consider lines newer than this captured cursor, when supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    pub timeout_ms: u64,
}

/// A closed set of expectations (ADR-0033 D3: no expressions), evaluated on
/// a `call` step's result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Expect {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The Tier B HTTP status, when the step ran over the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldExpect>,
}

/// One assertion against a JSON path (`$.foo.bar`, the same dotted-path
/// walk `impress_surface::template` uses, rooted at the step's result
/// rather than a template context).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FieldExpect {
    pub path: String,
    #[serde(flatten)]
    pub check: Check,
}

/// The closed set of checks a `fields` entry may make.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Check {
    Equals(serde_json::Value),
    NotEquals(serde_json::Value),
    Contains(String),
    Gt(serde_json::Value),
    Gte(f64),
    Lte(f64),
    Within { value: serde_json::Value, tol: f64 },
    Len(usize),
    Present(bool),
    Absent(bool),
}

/// The spy's whole-scenario check: every kind named here must have been
/// written by some step (SC-2's effects spy).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ExpectEffects {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writes: Vec<String>,
}
