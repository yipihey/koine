//! `ImpressWorkflowService` — the W1 verbs
//! (`validate`/`create`/`get`/`list`/`dry-run`/`enable`/`disable`) over
//! `impress-workflow`'s pure spec, validator and planner.
//!
//! # Review (D-R6)
//!
//! `create` from an agent caller always stores `state: proposed`, whatever
//! the spec itself said — the pipeline's own `CallerIdentity`, never an
//! argument, decides this (ADR-0034 D3's rule that a caller identity is a
//! transport fact). `enable` from an agent is refused `review-pending`
//! (`impress_service_core::pipeline::policy::REVIEW_PENDING`) and leaves
//! the row untouched; a person's `enable` (or the app acting for them)
//! runs it. `disable` carries no such restriction — an agent may always
//! turn a workflow off.

use std::sync::Arc;

use impress_item::{ActorKind, AttributedStore};
use impress_service_core::async_trait;
use impress_service_core::pipeline;
use impress_service_core::refusal::codes;
use impress_service_core::Refusal;
use impress_service_macros::{impress_service, impress_service_impl};
use impress_workflow::{Trigger, WorkflowSpec, WorkflowState};
use serde_json::Value;

#[allow(unused_imports)]
use impress_service_macros::impress_method;

use crate::dto::{
    WorkflowDryRunResult, WorkflowListResult, WorkflowResult, WorkflowSpecArg, WorkflowSummaryDto,
    WorkflowValidateResult, WouldCallDto,
};
use crate::store::{self, WorkflowRow};

/// Workflows: store, validate and dry-run a reviewable action sequence
/// (plan-self-reflective-layer.md § Workflows, W1). Running a workflow on
/// its own trigger is W2's planner (`impel-taskd`, the app's FFI tick); this
/// service is the verbs a person or an agent calls directly.
#[impress_service]
pub trait ImpressWorkflowService: Send + Sync + 'static {
    /// Every problem with a workflow, by JSON pointer and severity: the
    /// surface validator's own structural checks over the synthetic spec
    /// `impress-workflow` builds, plus a `publish`/`open` step (refused — a
    /// workflow has no pane) and a `schedule` trigger under 60s. `ok` is
    /// false (`invalid-spec`) when any problem is an error.
    #[impress_method]
    #[impress_example(
        name = "manual spec",
        args = r#"{"spec": {"wire_version": 1, "name": "example", "state": "proposed", "author": {"kind": "agent", "name": "example"}, "trigger": {"manual": {}}, "steps": []}}"#
    )]
    async fn workflow_validate(&self, spec: WorkflowSpecArg) -> WorkflowValidateResult;

    /// Store a spec as a new `impress/workflow@1.0.0` row, after validating
    /// it exactly as `workflow_validate` does — a spec with an error is
    /// refused (`invalid-spec`) and nothing is stored. **An agent caller's
    /// row is always `state: proposed`** (D-R6), whatever `spec.state`
    /// said; a person's row keeps the state the spec named.
    #[impress_method(safety = mutating, effects(reads = ["impress/workflow@1.0.0"], writes = ["impress/workflow@1.0.0"]))]
    #[impress_example(
        name = "manual macro",
        args = r#"{"spec": {"wire_version": 1, "name": "example", "state": "proposed", "author": {"kind": "agent", "name": "example"}, "trigger": {"manual": {}}, "steps": []}}"#
    )]
    async fn workflow_create(&self, spec: WorkflowSpecArg) -> WorkflowResult;

    /// One workflow row, spec included.
    #[impress_method(effects(reads = ["impress/workflow@1.0.0"]))]
    #[impress_example(
        name = "g3-read-stored-workflow",
        args = r#"{"id":"64000000-0000-4000-8000-000000000001"}"#,
        expect = r#"{"ok":true,"spec":{"name":"G3 reviewable workflow"}}"#
    )]
    async fn workflow_get(&self, id: String) -> WorkflowResult;

    /// Every stored workflow, oldest first, without their specs (see
    /// `workflow_get` for the full document).
    #[impress_method(effects(reads = ["impress/workflow@1.0.0"]))]
    #[impress_example(name = "default", args = r#"{}"#)]
    async fn workflow_list(&self) -> WorkflowListResult;

    /// `plan → resolve → reduce` with `event` (or `{}` when absent) as the
    /// trigger's own payload, and every `call` step returned as
    /// `would_call` rather than executed — the same table a review surface
    /// shows (plan § Workflows, "Dry run and validation").
    #[impress_method(effects(reads = ["impress/workflow@1.0.0"]))]
    #[impress_example(
        name = "g3-plan-without-executing",
        args = r#"{"id":"64000000-0000-4000-8000-000000000002","event":{"flag":"red"}}"#,
        expect = r#"{"ok":true,"would_call":[{"verb":"triage-service_set-flag","args":{"id":"64000000-0000-4000-8000-000000000011","flag":"red"}}]}"#
    )]
    async fn workflow_dry_run(&self, id: String, event: Option<Value>) -> WorkflowDryRunResult;

    /// Turns a workflow on. From an agent caller on a `proposed` workflow,
    /// this is a review (D-R6, ADR-0034 D3): refused `review-pending`, the
    /// row left `proposed`. A person (or the app acting for them) enables
    /// it outright.
    #[impress_method(safety = mutating, effects(reads = ["impress/workflow@1.0.0"], writes = ["impress/workflow@1.0.0"]))]
    #[impress_example(
        name = "g3-enable-reviewed-workflow",
        args = r#"{"id":"64000000-0000-4000-8000-000000000003"}"#,
        expect = r#"{"ok":true,"spec":{"state":"enabled"}}"#
    )]
    async fn workflow_enable(&self, id: String) -> WorkflowResult;

    /// Turns a workflow off. No review restriction — an agent may always
    /// disable a workflow it or a person enabled.
    #[impress_method(safety = mutating, effects(reads = ["impress/workflow@1.0.0"], writes = ["impress/workflow@1.0.0"]))]
    #[impress_example(
        name = "g3-disable-running-workflow",
        args = r#"{"id":"64000000-0000-4000-8000-000000000004"}"#,
        expect = r#"{"ok":true,"spec":{"state":"disabled"}}"#
    )]
    async fn workflow_disable(&self, id: String) -> WorkflowResult;
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct DefaultWorkflowService {
    store: Option<Arc<dyn AttributedStore>>,
}

impl DefaultWorkflowService {
    pub fn new() -> Self {
        Self { store: None }
    }

    pub fn with_store(store: Arc<dyn AttributedStore>) -> Self {
        Self { store: Some(store) }
    }

    /// The injected store, else the call's or the process's
    /// (`koine_store::process`), else a stand-in.
    fn store(&self) -> Arc<dyn AttributedStore> {
        self.store
            .clone()
            .unwrap_or_else(|| koine_store::process::acquire_or_stand_in().0)
    }
}

/// The caller this call is running under, as the pipeline established it —
/// never an argument (ADR-0034 D3). `ActorKind::System` (no live context,
/// e.g. a unit test with no pipeline around it) is treated as a person for
/// D-R6's purposes: only an *agent* caller is restricted.
fn caller_is_agent() -> bool {
    pipeline::context::current()
        .map(|c| c.caller.kind() == "agent")
        .unwrap_or(false)
}

fn caller_actor_kind() -> ActorKind {
    match pipeline::context::current() {
        Some(c) if c.caller.kind() == "agent" => ActorKind::Agent,
        Some(c) if c.caller.kind() == "human" => ActorKind::Human,
        _ => ActorKind::System,
    }
}

fn row_to_result(row: &WorkflowRow) -> WorkflowResult {
    WorkflowResult {
        ok: true,
        message: format!("workflow '{}' ({})", row.spec.name, row.id),
        code: None,
        id: Some(row.id.to_string()),
        spec: Some(row.spec.clone()),
        created: Some(row.created.to_rfc3339()),
        modified: Some(row.modified.to_rfc3339()),
        problems: Vec::new(),
        wire_version: impress_service_core::wire::WIRE_VERSION,
    }
}

fn trigger_label(trigger: &Trigger) -> &'static str {
    match trigger {
        Trigger::Schedule { .. } => "schedule",
        Trigger::Store { .. } => "store",
        Trigger::Job { .. } => "job",
        Trigger::Message { .. } => "message",
        Trigger::Call { .. } => "call",
        Trigger::Manual {} => "manual",
    }
}

fn parse_id(id: &str) -> Result<uuid::Uuid, Refusal> {
    id.parse()
        .map_err(|_| Refusal::new(codes::INVALID_ARGUMENT, format!("'{id}' is not an item id")))
}

#[async_trait::async_trait]
impl ImpressWorkflowService for DefaultWorkflowService {
    async fn workflow_validate(&self, spec: WorkflowSpecArg) -> WorkflowValidateResult {
        let workflow = match spec.parse() {
            Ok(w) => w,
            Err(refusal) => {
                return WorkflowValidateResult::of(vec![impress_workflow::Problem::error(
                    "",
                    refusal.message,
                )])
            }
        };
        WorkflowValidateResult::of(impress_workflow::validate(&workflow))
    }

    async fn workflow_create(&self, spec: WorkflowSpecArg) -> WorkflowResult {
        let mut workflow: WorkflowSpec = match spec.parse() {
            Ok(w) => w,
            Err(refusal) => return WorkflowResult::refused(refusal),
        };

        // D-R6: an agent's workflow is always stored `proposed`, whatever
        // the spec itself asked for.
        if caller_is_agent() {
            workflow.state = WorkflowState::Proposed;
            workflow.review.required = true;
        }

        let problems = impress_workflow::validate(&workflow);
        if problems.iter().any(|p| p.is_error()) {
            return WorkflowResult::invalid_spec(problems);
        }

        let store = self.store();
        match store::insert(store.as_ref(), &workflow, caller_actor_kind()) {
            Ok(row) => {
                let mut result = row_to_result(&row);
                result.message = format!(
                    "Stored workflow '{}' ({}), {}.",
                    row.spec.name,
                    row.id,
                    row.spec.state.as_str()
                );
                result.problems = problems;
                result
            }
            Err(e) => WorkflowResult::refused(Refusal::new(codes::STORE_ERROR, e.to_string())),
        }
    }

    async fn workflow_get(&self, id: String) -> WorkflowResult {
        let item_id = match parse_id(&id) {
            Ok(id) => id,
            Err(refusal) => return WorkflowResult::refused(refusal),
        };
        match store::get(self.store().as_ref(), item_id) {
            Ok(Some(row)) => row_to_result(&row),
            Ok(None) => WorkflowResult::refused(Refusal::new(
                codes::NOT_FOUND,
                format!("no workflow '{id}'"),
            )),
            Err(e) => WorkflowResult::refused(Refusal::new(codes::STORE_ERROR, e)),
        }
    }

    async fn workflow_list(&self) -> WorkflowListResult {
        match store::list(self.store().as_ref()) {
            Ok(rows) => {
                let workflows: Vec<WorkflowSummaryDto> = rows
                    .iter()
                    .map(|row| WorkflowSummaryDto {
                        id: row.id.to_string(),
                        name: row.spec.name.clone(),
                        state: row.spec.state.as_str().to_string(),
                        trigger: trigger_label(&row.spec.trigger).to_string(),
                        created: row.created.to_rfc3339(),
                        modified: row.modified.to_rfc3339(),
                    })
                    .collect();
                WorkflowListResult {
                    ok: true,
                    message: format!("{} workflow(s).", workflows.len()),
                    code: None,
                    workflows,
                    wire_version: impress_service_core::wire::WIRE_VERSION,
                }
            }
            Err(e) => WorkflowListResult {
                ok: false,
                message: e,
                code: Some(codes::STORE_ERROR.to_string()),
                workflows: Vec::new(),
                wire_version: impress_service_core::wire::WIRE_VERSION,
            },
        }
    }

    async fn workflow_dry_run(&self, id: String, event: Option<Value>) -> WorkflowDryRunResult {
        let item_id = match parse_id(&id) {
            Ok(id) => id,
            Err(refusal) => {
                return WorkflowDryRunResult {
                    ok: false,
                    message: refusal.message,
                    code: Some(refusal.code),
                    would_call: Vec::new(),
                    other_effects: Vec::new(),
                    wire_version: impress_service_core::wire::WIRE_VERSION,
                }
            }
        };
        let row = match store::get(self.store().as_ref(), item_id) {
            Ok(Some(row)) => row,
            Ok(None) => {
                return WorkflowDryRunResult {
                    ok: false,
                    message: format!("no workflow '{id}'"),
                    code: Some(codes::NOT_FOUND.to_string()),
                    would_call: Vec::new(),
                    other_effects: Vec::new(),
                    wire_version: impress_service_core::wire::WIRE_VERSION,
                }
            }
            Err(e) => {
                return WorkflowDryRunResult {
                    ok: false,
                    message: e,
                    code: Some(codes::STORE_ERROR.to_string()),
                    would_call: Vec::new(),
                    other_effects: Vec::new(),
                    wire_version: impress_service_core::wire::WIRE_VERSION,
                }
            }
        };

        let trigger_value = event.unwrap_or_else(|| Value::Object(Default::default()));
        match impress_workflow::plan(
            &row.spec,
            trigger_value,
            &Value::Object(Default::default()),
            &Value::Object(Default::default()),
            &Value::Object(Default::default()),
        ) {
            Ok((_, effects)) => {
                let mut would_call = Vec::new();
                let mut other_effects = Vec::new();
                for effect in effects {
                    match effect {
                        impress_workflow::Effect::Call {
                            verb,
                            args,
                            adverbs,
                            ..
                        } => {
                            would_call.push(WouldCallDto::new(verb, args).with_adverbs(adverbs));
                        }
                        other => {
                            other_effects.push(serde_json::to_value(&other).unwrap_or(Value::Null));
                        }
                    }
                }
                WorkflowDryRunResult {
                    ok: true,
                    message: format!("{} would-call effect(s).", would_call.len()),
                    code: None,
                    would_call,
                    other_effects,
                    wire_version: impress_service_core::wire::WIRE_VERSION,
                }
            }
            Err(e) => WorkflowDryRunResult {
                ok: false,
                message: e.to_string(),
                code: Some(codes::VERB_FAILED.to_string()),
                would_call: Vec::new(),
                other_effects: Vec::new(),
                wire_version: impress_service_core::wire::WIRE_VERSION,
            },
        }
    }

    async fn workflow_enable(&self, id: String) -> WorkflowResult {
        let item_id = match parse_id(&id) {
            Ok(id) => id,
            Err(refusal) => return WorkflowResult::refused(refusal),
        };
        let store = self.store();
        let row = match store::get(store.as_ref(), item_id) {
            Ok(Some(row)) => row,
            Ok(None) => {
                return WorkflowResult::refused(Refusal::new(
                    codes::NOT_FOUND,
                    format!("no workflow '{id}'"),
                ))
            }
            Err(e) => return WorkflowResult::refused(Refusal::new(codes::STORE_ERROR, e)),
        };

        if caller_is_agent() {
            // D-R6: an agent's `enable` on ANY workflow is a review, not an
            // action — never mind whether this particular row is already
            // `proposed`; the person's confirm is what re-enters as
            // `Person` and actually flips the state.
            return WorkflowResult::review_pending(&row.spec.name);
        }

        match store::set_state(store.as_ref(), item_id, WorkflowState::Enabled.as_str()) {
            Ok(()) => {
                let mut spec = row.spec.clone();
                spec.state = WorkflowState::Enabled;
                WorkflowResult {
                    ok: true,
                    message: format!("workflow '{}' enabled.", spec.name),
                    code: None,
                    id: Some(row.id.to_string()),
                    spec: Some(spec),
                    created: Some(row.created.to_rfc3339()),
                    modified: Some(chrono::Utc::now().to_rfc3339()),
                    problems: Vec::new(),
                    wire_version: impress_service_core::wire::WIRE_VERSION,
                }
            }
            Err(e) => WorkflowResult::refused(Refusal::new(codes::STORE_ERROR, e)),
        }
    }

    async fn workflow_disable(&self, id: String) -> WorkflowResult {
        let item_id = match parse_id(&id) {
            Ok(id) => id,
            Err(refusal) => return WorkflowResult::refused(refusal),
        };
        let store = self.store();
        let row = match store::get(store.as_ref(), item_id) {
            Ok(Some(row)) => row,
            Ok(None) => {
                return WorkflowResult::refused(Refusal::new(
                    codes::NOT_FOUND,
                    format!("no workflow '{id}'"),
                ))
            }
            Err(e) => return WorkflowResult::refused(Refusal::new(codes::STORE_ERROR, e)),
        };
        match store::set_state(store.as_ref(), item_id, WorkflowState::Disabled.as_str()) {
            Ok(()) => {
                let mut spec = row.spec.clone();
                spec.state = WorkflowState::Disabled;
                WorkflowResult {
                    ok: true,
                    message: format!("workflow '{}' disabled.", spec.name),
                    code: None,
                    id: Some(row.id.to_string()),
                    spec: Some(spec),
                    created: Some(row.created.to_rfc3339()),
                    modified: Some(chrono::Utc::now().to_rfc3339()),
                    problems: Vec::new(),
                    wire_version: impress_service_core::wire::WIRE_VERSION,
                }
            }
            Err(e) => WorkflowResult::refused(Refusal::new(codes::STORE_ERROR, e)),
        }
    }
}

impress_service_impl! {
    service = ImpressWorkflowService,
    safety = read_only,
    since = "0.1.0",
    effects = {
        reads: [],
        writes: [],
        reach: [],
    },
    impl = DefaultWorkflowService,
    instance = DefaultWorkflowService::new,
    strict_args = true,
    methods = [
        workflow_validate(
            /// Complete workflow document to validate without storing or executing it.
            spec: WorkflowSpecArg
        ) -> WorkflowValidateResult,
        workflow_create(
            /// Complete workflow document; agent callers always create a proposed workflow.
            spec: WorkflowSpecArg
        ) -> WorkflowResult,
        workflow_get(
            /// UUID of the stored workflow row returned by workflow-create or workflow-list.
            id: String
        ) -> WorkflowResult,
        workflow_list() -> WorkflowListResult,
        workflow_dry_run(
            /// UUID of the stored workflow to plan without running its steps.
            id: String,
            /// Trigger payload used to resolve event templates; defaults to an empty object.
            event: Option<Value>
        ) -> WorkflowDryRunResult,
        workflow_enable(
            /// UUID of the stored workflow row returned by workflow-create or workflow-list.
            id: String
        ) -> WorkflowResult,
        workflow_disable(
            /// UUID of the stored workflow row returned by workflow-create or workflow-list.
            id: String
        ) -> WorkflowResult,
    ],
}
