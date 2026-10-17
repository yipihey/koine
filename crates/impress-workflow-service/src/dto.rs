//! Wire types the workflow verbs take and return.
//!
//! # The wire (version 1)
//!
//! Every result is snake_case and carries `"wire_version": 1`
//! (`impress_service_core::wire::WIRE_VERSION`); every refusal is `ok: false`
//! with a `code` and a `message`. Every argument is strict
//! (`strict_args = true` on the service): an unknown field is refused
//! `invalid-argument` naming it.

use impress_service_core::pipeline::policy::REVIEW_PENDING;
use impress_service_core::refusal::codes;
use impress_service_core::wire::{wire_version, WIRE_VERSION};
use impress_service_core::Refusal;
use impress_workflow::{Problem, WorkflowSpec};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A workflow spec as an argument: JSON, read by the verb rather than by
/// the argument parser, so a structural mistake comes back as a located
/// problem (`workflow-service_validate`) or an `invalid-spec` refusal
/// listing every problem, never a bare parse failure — the same choice
/// `impress-surface-service`'s `SpecArg` makes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkflowSpecArg(pub Value);

impl schemars::JsonSchema for WorkflowSpecArg {
    fn schema_name() -> String {
        "WorkflowSpec".to_string()
    }

    fn is_referenceable() -> bool {
        false
    }

    fn json_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        WorkflowSpec::json_schema(gen)
    }
}

impl WorkflowSpecArg {
    /// Parses the argument, refusing with `invalid-argument` naming the
    /// first structural problem (a missing field, a wrong type) rather
    /// than a bare serde error.
    pub fn parse(&self) -> Result<WorkflowSpec, Refusal> {
        serde_json::from_value(self.0.clone()).map_err(|e| {
            Refusal::new(
                codes::INVALID_ARGUMENT,
                format!("`spec` does not match the WorkflowSpec shape: {e}"),
            )
        })
    }
}

/// The code a spec with error-severity problems is refused with — the same
/// spelling `impress-surface-service` uses for the same situation.
pub const INVALID_SPEC: &str = "invalid-spec";

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkflowValidateResult {
    pub ok: bool,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub problems: Vec<Problem>,
    #[serde(default = "wire_version")]
    pub wire_version: u32,
}

impl WorkflowValidateResult {
    pub fn of(problems: Vec<Problem>) -> Self {
        let errors = problems.iter().filter(|p| p.is_error()).count();
        let warnings = problems.len() - errors;
        let ok = errors == 0;
        Self {
            ok,
            message: format!("{errors} error(s), {warnings} warning(s)"),
            code: (!ok).then(|| INVALID_SPEC.to_string()),
            problems,
            wire_version: WIRE_VERSION,
        }
    }
}

/// One stored workflow, in full — `workflow-service_get`, and what `create`
/// echoes back.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkflowResult {
    pub ok: bool,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<WorkflowSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    #[serde(default)]
    pub problems: Vec<Problem>,
    #[serde(default = "wire_version")]
    pub wire_version: u32,
}

impl WorkflowResult {
    pub fn refused(refusal: Refusal) -> Self {
        Self {
            ok: false,
            message: refusal.message,
            code: Some(refusal.code),
            id: None,
            spec: None,
            created: None,
            modified: None,
            problems: Vec::new(),
            wire_version: WIRE_VERSION,
        }
    }

    /// A spec with error-severity problems, refused with every problem.
    pub fn invalid_spec(problems: Vec<Problem>) -> Self {
        let errors = problems.iter().filter(|p| p.is_error()).count();
        let first = problems
            .iter()
            .find(|p| p.is_error())
            .map(|p| {
                format!(
                    "{}: {}",
                    if p.path.is_empty() { "/" } else { &p.path },
                    p.message
                )
            })
            .unwrap_or_default();
        let mut refused = Self::refused(Refusal::new(
            INVALID_SPEC,
            format!("the spec has {errors} error(s), nothing was stored — first: {first}"),
        ));
        let mut problems = problems;
        problems.sort_by_key(|p| !p.is_error());
        refused.problems = problems;
        refused
    }

    /// The `review-pending` refusal `workflow-service_enable` gives an
    /// agent caller (D-R6): the workflow stays `proposed`, unchanged.
    pub fn review_pending(name: &str) -> Self {
        Self::refused(Refusal::new(
            REVIEW_PENDING,
            format!(
                "'{name}' is proposed and needs a person's review before it can be enabled \
                 (D-R6); nothing changed"
            ),
        ))
    }
}

/// One row as `workflow-service_list` shows it — no `spec` (an agent lists
/// to pick an id; `get` has the whole document).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkflowSummaryDto {
    pub id: String,
    pub name: String,
    pub state: String,
    pub trigger: String,
    pub created: String,
    pub modified: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkflowListResult {
    pub ok: bool,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub workflows: Vec<WorkflowSummaryDto>,
    #[serde(default = "wire_version")]
    pub wire_version: u32,
}

/// One call a dry run would make, with the calling verb's own declared
/// safety and effects when the verb is in the linked inventory (`None` for
/// a name the inventory does not know, which `workflow-service_validate`
/// already reports as an error).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WouldCallDto {
    pub verb: String,
    pub args: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety: Option<String>,
    #[serde(default)]
    pub effects_writes: Vec<String>,
    /// The adverbs the step declares (`docs/grammar.md` § Adverbs), when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adverbs: Option<Value>,
}

impl WouldCallDto {
    pub fn new(verb: String, args: Value) -> Self {
        let descriptor = impress_service_core::call::find(&verb);
        Self {
            verb,
            args,
            safety: descriptor
                .as_ref()
                .map(|d| d.safety().class.as_str().to_string()),
            effects_writes: descriptor
                .as_ref()
                .map(|d| d.effects().writes.iter().map(|k| k.describe()).collect())
                .unwrap_or_default(),
            adverbs: None,
        }
    }

    /// This entry with the step's adverbs recorded.
    pub fn with_adverbs(mut self, adverbs: Option<Value>) -> Self {
        self.adverbs = adverbs;
        self
    }
}

/// `workflow-service_dry-run`'s answer: `plan → resolve → reduce` with
/// every `call` effect returned, never executed (plan § Workflows,
/// "Dry run and validation") — the same table a review surface shows.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkflowDryRunResult {
    pub ok: bool,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub would_call: Vec<WouldCallDto>,
    /// Every non-`call` effect the run would also produce (`emit`, `set`
    /// folds into state directly and is not listed here).
    #[serde(default)]
    pub other_effects: Vec<Value>,
    #[serde(default = "wire_version")]
    pub wire_version: u32,
}
