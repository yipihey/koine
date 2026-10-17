//! `ImpressScenarioService` — `validate`, `run`, `list`, `get`, `create`
//! (docs/plan-self-reflective-layer.md S1, § Scenarios).
//!
//! Modelled on `impress-surface-service/src/service.rs`: every argument is
//! strict, every result carries `wire_version`, a spec is validated
//! wherever it is stored.

use std::sync::Arc;

use impress_item::{ActorKind, AttributedStore};
use impress_service_core::wire::WIRE_VERSION;
use impress_service_core::Refusal;
use impress_service_macros::{impress_service, impress_service_impl};

#[allow(unused_imports)]
use impress_service_macros::impress_method;

use crate::dto::{
    RecordedCallOmission, ScenarioListResult, ScenarioRecordResult, ScenarioResult,
    ScenarioRunResult, ScenarioSummaryDto, ScenarioValidateResult, SpecArg,
};
use crate::store::ScenarioStore;
use crate::tier_a::TierACaller;
use crate::TierBCaller;

/// Every scenario verb an agent needs (S1's row): validate a spec, store
/// one, list/get what is stored, and run it — Tier A on a scratch store,
/// Tier B against a running app.
#[impress_service]
pub trait ImpressScenarioService: Send + Sync + 'static {
    /// Record one caller's trace or bounded time window as a stored Tier B
    /// scenario. Only lossless audit arguments are replayed. Earlier output
    /// IDs reused by later steps become captures; omitted calls are reported.
    /// This stores a document for review and editing; it executes no steps.
    #[impress_method(safety = mutating, effects(reads = ["core/verb-call@1.0.0", "impress/scenario@1.0.0"], writes = ["impress/scenario@1.0.0"]))]
    #[impress_example(
        name = "recorded-trace",
        args = r#"{"trace_id":"scenario-record-example"}"#,
        expect = r#"{"ok":true,"selected":1,"skipped":[]}"#
    )]
    async fn scenario_record(
        &self,
        trace_id: Option<String>,
        since: Option<String>,
        until: Option<String>,
        r#as: Option<String>,
    ) -> ScenarioRecordResult;

    /// Every structural problem with a scenario spec (`impress-scenario::validate`).
    /// `ok` is false when any problem was found; this crate cannot check
    /// that a `call` step names a real verb without the inventory, so it
    /// adds that check on top of `impress-scenario`'s own structural pass.
    #[impress_method]
    #[impress_example(
        name = "default",
        args = r#"{"spec": {"wire_version": 1, "id": "example.noop", "description": "a scenario with one no-op step", "tier": "a", "steps": [{"call": "impress-scenario-service_scenario-list", "args": {}, "as": "agent:scenario"}]}}"#
    )]
    async fn scenario_validate(&self, spec: SpecArg) -> ScenarioValidateResult;

    /// Store a spec as a new `impress/scenario@1.0.0` row, after validating
    /// it exactly as `scenario_validate` does. Refused `invalid-spec` with
    /// every problem when any is found; nothing is stored.
    #[impress_method(safety = mutating, effects(reads = ["impress/scenario@1.0.0"], writes = ["impress/scenario@1.0.0"]))]
    #[impress_example(
        name = "default",
        args = r#"{"spec": {"wire_version": 1, "id": "example.noop", "description": "a scenario with one no-op step", "tier": "a", "steps": [{"call": "impress-scenario-service_scenario-list", "args": {}, "as": "agent:scenario"}]}}"#
    )]
    async fn scenario_create(&self, spec: SpecArg, tags: Option<Vec<String>>) -> ScenarioResult;

    /// One stored scenario, by its row id OR its stable `scenario_id`
    /// (`layout.saved_round_trip`) — whichever `id` names.
    #[impress_method(effects(reads = ["impress/scenario@1.0.0"]))]
    #[impress_example(name = "default", args = r#"{"id": "example.noop"}"#)]
    async fn scenario_get(&self, id: String) -> ScenarioResult;

    /// Every stored scenario, oldest first, without their specs (see
    /// `scenario_get` for the full document).
    #[impress_method(effects(reads = ["impress/scenario@1.0.0"]))]
    #[impress_example(name = "default", args = r#"{}"#)]
    async fn scenario_list(&self) -> ScenarioListResult;

    /// Run one stored scenario by its `scenario_id`, Tier A on a fresh
    /// scratch store or Tier B against `base_url` (default: this device's
    /// own app, resolved the way the layout Tier B catalogue does).
    /// `external`: a Tier B run leaves the process over loopback HTTP, and
    /// a scenario's steps may call any verb, including a mutating or
    /// destructive one — the same reasoning that makes `workflow-service_run`
    /// (and every other "run arbitrary steps" verb) an external safety class
    /// rather than trying to infer a tighter one from what happens to run.
    #[impress_method(safety = external, effects(reads = ["impress/scenario@1.0.0"], writes = [any("a scenario's steps may call any verb, including a mutating one")], reach = [network]))]
    #[impress_example(
        name = "run-owned-noop",
        tier = "b",
        args = r#"{"scenario_id":"example.noop","tier":"a"}"#,
        expect = r#"{"ok":true,"total":1,"passed":1,"failed":0,"skipped":0}"#
    )]
    async fn scenario_run(
        &self,
        scenario_id: String,
        tier: Option<String>,
        base_url: Option<String>,
    ) -> ScenarioRunResult;
}

#[derive(Clone, Default)]
pub struct DefaultImpressScenarioService {
    store: Option<Arc<dyn AttributedStore>>,
}

impl DefaultImpressScenarioService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_store(store: Arc<dyn AttributedStore>) -> Self {
        Self { store: Some(store) }
    }

    /// The injected store, else the call's or the process's
    /// (`koine_store::process`), else a stand-in.
    fn store_arc(&self) -> Arc<dyn AttributedStore> {
        self.store
            .clone()
            .unwrap_or_else(|| koine_store::process::acquire_or_stand_in().0)
    }

    fn scenarios(&self) -> ScenarioStore {
        ScenarioStore::new(self.store_arc())
    }

    /// Everything `impress-scenario::validate` finds, plus — when the
    /// inventory this process links has it — every `call` step naming a
    /// verb that does not exist. Provider reach must use Tier B.
    fn problems_of(
        &self,
        raw: &serde_json::Value,
    ) -> (
        Option<impress_scenario::Scenario>,
        Vec<impress_scenario::Problem>,
    ) {
        let parsed: Result<impress_scenario::Scenario, _> = serde_json::from_value(raw.clone());
        let Ok(scenario) = parsed else {
            return (
                None,
                vec![impress_scenario::Problem {
                    step: None,
                    message: format!(
                        "not a well-formed scenario: {}",
                        parsed.err().map(|e| e.to_string()).unwrap_or_default()
                    ),
                }],
            );
        };
        let mut problems = impress_scenario::validate(&scenario);
        for (index, step) in scenario.steps.iter().chain(&scenario.teardown).enumerate() {
            let called = match step {
                impress_scenario::Step::Call(call) => Some(call.call.as_str()),
                impress_scenario::Step::BestEffort(step) => Some(step.best_effort.call.as_str()),
                _ => None,
            };
            if let Some(name) = called {
                match impress_service_core::call::find(name) {
                    None => problems.push(impress_scenario::Problem {
                        step: Some(index),
                        message: format!("no such verb: {name}"),
                    }),
                    Some(descriptor)
                        if scenario.tier == impress_scenario::Tier::A
                            && descriptor
                                .effects()
                                .reach
                                .contains(&impress_service_core::descriptor::Reach::Provider) =>
                    {
                        problems.push(impress_scenario::Problem {
                            step: Some(index),
                            message: format!("provider verb {name} requires Tier B"),
                        });
                    }
                    Some(_) => {}
                }
            }
        }
        (Some(scenario), problems)
    }
}

#[async_trait::async_trait]
impl ImpressScenarioService for DefaultImpressScenarioService {
    async fn scenario_record(
        &self,
        trace_id: Option<String>,
        since: Option<String>,
        until: Option<String>,
        r#as: Option<String>,
    ) -> ScenarioRecordResult {
        let selection = match crate::record_store::Selection::new(
            trace_id.as_deref(),
            since.as_deref(),
            until.as_deref(),
            r#as.as_deref(),
        ) {
            Ok(selection) => selection,
            Err(error) => return ScenarioRecordResult::refused(error),
        };
        // Do not log the selected arguments or arbitrary caller-supplied text.
        tracing::info!(target: "verb", "Recording scenario from {}", if trace_id.is_some() { "trace" } else { "time window" });
        let store = self.store_arc();
        let calls = match selection.read(store.as_ref()) {
            Ok(calls) => calls,
            Err(error) => return ScenarioRecordResult::refused(error),
        };
        let selected = calls.len();
        let generated = match crate::record::generate_scenario(
            format!("recorded.{}", uuid::Uuid::new_v4()),
            format!("Recorded session of {selected} calls; review expectations before sharing."),
            calls,
        ) {
            Ok(generated) => generated,
            Err(crate::record::RecordError::NoReplayableCalls { skipped }) => {
                return ScenarioRecordResult {
                    scenario: ScenarioResult::refused(Refusal::invalid_argument(
                        "no replayable calls remain",
                    )),
                    selected,
                    skipped: skipped
                        .into_iter()
                        .map(RecordedCallOmission::from)
                        .collect(),
                };
            }
            Err(error) => {
                return ScenarioRecordResult::refused(Refusal::invalid_argument(error.to_string()))
            }
        };
        let raw = match serde_json::to_value(&generated.scenario) {
            Ok(raw) => raw,
            Err(error) => {
                return ScenarioRecordResult::refused(Refusal::internal(error.to_string()))
            }
        };
        let (_, problems) = self.problems_of(&raw);
        if !problems.is_empty() {
            return ScenarioRecordResult::refused(Refusal::invalid_argument(format!(
                "recorded scenario is not replayable: {}",
                problems
                    .into_iter()
                    .map(|problem| problem.message)
                    .collect::<Vec<_>>()
                    .join("; ")
            )));
        }
        let skipped: Vec<_> = generated
            .skipped
            .into_iter()
            .map(RecordedCallOmission::from)
            .collect();
        let scenarios = ScenarioStore::new(store);
        let row =
            match scenarios.create(&generated.scenario, &["recorded".into()], ActorKind::Agent) {
                Ok(row) => row,
                Err(error) => return ScenarioRecordResult::refused(error),
            };
        tracing::info!(target: "verb", scenario_id = %row.spec.id, selected, steps = row.spec.steps.len(), skipped = skipped.len(), "Saved recorded scenario");
        // Read back the stored document, so the returned preview is what a
        // later get/run sees, not just the pre-save in-memory draft.
        let row = match scenarios.get(row.id) {
            Ok(Some(row)) => row,
            Ok(None) => {
                return ScenarioRecordResult::refused(Refusal::store(
                    "recorded scenario disappeared after save",
                ))
            }
            Err(error) => return ScenarioRecordResult::refused(error),
        };
        tracing::info!(target: "verb", scenario_id = %row.spec.id, steps = row.spec.steps.len(), "Display recorded scenario read back from store");
        ScenarioRecordResult {
            scenario: ScenarioResult::from_row(&row),
            selected,
            skipped,
        }
    }

    async fn scenario_validate(&self, spec: SpecArg) -> ScenarioValidateResult {
        ScenarioValidateResult::of(self.problems_of(&spec.0).1)
    }

    async fn scenario_create(&self, spec: SpecArg, tags: Option<Vec<String>>) -> ScenarioResult {
        let (parsed, problems) = self.problems_of(&spec.0);
        let Some(parsed) = parsed.filter(|_| problems.is_empty()) else {
            return ScenarioResult {
                ok: false,
                ..ScenarioResult::refused(Refusal::invalid_argument(format!(
                    "invalid scenario: {}",
                    problems
                        .into_iter()
                        .map(|p| p.message)
                        .collect::<Vec<_>>()
                        .join("; ")
                )))
            };
        };
        let tags = tags.unwrap_or_default();
        match self.scenarios().create(&parsed, &tags, ActorKind::Agent) {
            Ok(row) => ScenarioResult::from_row(&row),
            Err(e) => ScenarioResult::refused(e),
        }
    }

    async fn scenario_get(&self, id: String) -> ScenarioResult {
        let scenarios = self.scenarios();
        let found = if let Ok(item_id) = id.parse() {
            scenarios.get(item_id).ok().flatten()
        } else {
            None
        };
        let found = found.or_else(|| scenarios.get_by_scenario_id(&id).ok().flatten());
        match found {
            Some(row) => ScenarioResult::from_row(&row),
            None => ScenarioResult::refused(Refusal::not_found(format!("no scenario `{id}`"))),
        }
    }

    async fn scenario_list(&self) -> ScenarioListResult {
        match self.scenarios().list() {
            Ok(rows) => ScenarioListResult {
                wire_version: WIRE_VERSION,
                ok: true,
                scenarios: rows
                    .iter()
                    .map(|r| ScenarioSummaryDto {
                        id: r.id.to_string(),
                        scenario_id: r.spec.id.clone(),
                        description: r.spec.description.clone(),
                        tier: match r.spec.tier {
                            impress_scenario::Tier::A => "a".to_string(),
                            impress_scenario::Tier::B => "b".to_string(),
                        },
                        tags: r.tags.clone(),
                    })
                    .collect(),
                refusal: Default::default(),
            },
            Err(e) => ScenarioListResult {
                wire_version: WIRE_VERSION,
                ok: false,
                scenarios: Vec::new(),
                refusal: e.into(),
            },
        }
    }

    async fn scenario_run(
        &self,
        scenario_id: String,
        tier: Option<String>,
        base_url: Option<String>,
    ) -> ScenarioRunResult {
        let row = match self.scenarios().get_by_scenario_id(&scenario_id) {
            Ok(Some(row)) => row,
            Ok(None) => {
                return ScenarioRunResult::refused(Refusal::not_found(format!(
                    "no scenario `{scenario_id}`"
                )))
            }
            Err(e) => return ScenarioRunResult::refused(e),
        };
        let requested_tier = tier.as_deref().unwrap_or(match row.spec.tier {
            impress_scenario::Tier::A => "a",
            impress_scenario::Tier::B => "b",
        });
        let result = if requested_tier == "b" {
            let base = base_url.unwrap_or_else(|| "http://127.0.0.1:23125".to_string());
            let mut caller = TierBCaller::new(&base);
            impress_scenario::run(&row.spec, &mut caller).await
        } else {
            match TierACaller::open() {
                Ok(mut caller) => impress_scenario::run(&row.spec, &mut caller).await,
                Err(e) => {
                    return ScenarioRunResult::refused(Refusal::internal(e));
                }
            }
        };
        let report = impress_service_core::report::SelfTestReport::from_results(vec![result]);
        report.into()
    }
}

fn impress_scenario_service_instance() -> Arc<dyn ImpressScenarioService> {
    Arc::new(DefaultImpressScenarioService::new())
}

impress_service_impl! {
    service = ImpressScenarioService,
    safety = read_only,
    since = "0.1.0",
    effects = {
        reads: [],
        writes: [],
        reach: [],
    },
    impl = DefaultImpressScenarioService,
    instance = || impress_scenario_service_instance(),
    strict_args = true,
    methods = [
        scenario_record(
            /// The exact recorded trace ID. Mutually exclusive with since/until.
            trace_id: Option<String>,
            /// Inclusive RFC 3339 start of a window; requires until and as.
            since: Option<String>,
            /// Inclusive RFC 3339 end of a window; requires since and as.
            until: Option<String>,
            /// One exact recorded caller: person, agent:<name>, app:<name>,
            /// system:<name>, or provider:<name>. A trace may omit this only
            /// when all matching rows have the same caller. This selects
            /// history; it does not change the invoking caller's authority.
            r#as: Option<String>
        ) -> ScenarioRecordResult,
        scenario_validate(
            /// The scenario spec, as JSON.
            spec: SpecArg
        ) -> ScenarioValidateResult,
        scenario_create(
            /// The scenario spec, as JSON. Validated first; refused with
            /// every problem if any is found.
            spec: SpecArg,
            /// Free-text labels.
            tags: Option<Vec<String>>
        ) -> ScenarioResult,
        scenario_get(
            /// The row id, or the scenario's own stable `scenario_id`.
            id: String
        ) -> ScenarioResult,
        scenario_list() -> ScenarioListResult,
        scenario_run(
            /// The scenario's stable `scenario_id`.
            scenario_id: String,
            /// `"a"` or `"b"`; the scenario's own declared tier when absent.
            tier: Option<String>,
            /// The app's loopback base URL for a Tier B run (this device's
            /// impress by default).
            base_url: Option<String>
        ) -> ScenarioRunResult,
    ],
}

#[cfg(test)]
mod provider_reader_tests {
    use super::*;
    use impress_scenario::Caller;
    use impress_service_core::provider::{RegistrationRequest, Registry, SchemaValidator};
    use impress_service_core::registry_runtime;
    use serde_json::{json, Value};

    struct AcceptSchema;
    impl SchemaValidator for AcceptSchema {
        fn validate(&self, _: &Value) -> Result<(), String> {
            Ok(())
        }
        fn validate_instance(&self, _: &Value, _: &Value) -> Result<(), String> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn provider_calls_validate_as_tier_b_and_never_run_in_tier_a() {
        struct RestoreRegistry(Arc<Registry>);
        impl Drop for RestoreRegistry {
            fn drop(&mut self) {
                registry_runtime::install(self.0.clone());
            }
        }
        let _restore = RestoreRegistry(registry_runtime::current());
        let registry = Arc::new(Registry::new().with_validator(Arc::new(AcceptSchema)));
        let request: RegistrationRequest = serde_json::from_value(json!({
            "provider":{"id":"scenario-provider-fixture","language":"Python","version":"1.0.0","endpoint":"http://127.0.0.1:1"},
            "verbs":[{"name":"scenario-provider-fixture-service_echo","description":"Echo text",
                "input_schema":{"type":"object","properties":{"text":{"type":"string","description":"Text"}},"required":["text"],"additionalProperties":false},
                "output_schema":{"type":"object","properties":{"echo":{"type":"string"}}},
                "safety":{"class":"read_only"},"since":"1.0.0",
                "examples":[{"name":"echo","args":{"text":"hello"}}]}]
        }))
        .unwrap();
        registry.register(request).unwrap();
        registry_runtime::install(registry);
        let name = "scenario-provider-fixture-service_echo";
        let spec = |tier| {
            json!({
                "wire_version":1,"id":"provider.example","description":"Provider call",
                "tier":tier,"steps":[{"call":name,"args":{"text":"hello"},"as":"agent:scenario"}]
            })
        };
        let service = DefaultImpressScenarioService::new();
        let (_, a_problems) = service.problems_of(&spec("a"));
        assert!(a_problems
            .iter()
            .any(|p| p.message.contains("requires Tier B")));
        let (_, b_problems) = service.problems_of(&spec("b"));
        assert!(b_problems.is_empty(), "{b_problems:?}");
        let mut caller = TierACaller::open().unwrap();
        let error = caller
            .call(name, json!({"text":"hello"}), "agent:scenario")
            .await
            .unwrap_err();
        assert!(error.contains("requires Tier B"), "{error}");
    }
}
