//! Pure S3 call-log-to-scenario matching. The service fetches and orders call
//! rows; this module never opens a store or invokes a verb.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use impress_scenario::spec::{
    CallStep, EventBody, EventStep, Expect, Requires, Scenario, Step, Tier,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const SURFACE_DISPATCH_VERB: &str = "impress-surface-service_surface-dispatch";

/// One chronologically ordered call, after the service has read its audit row.
/// `result_ids` contains only privacy-safe scalar IDs and their paths in the
/// raw handler result; it is not an arbitrary copy of the result body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordedCall {
    pub call_id: String,
    pub parent_call: Option<String>,
    pub verb: String,
    pub args: Value,
    pub args_replayable: bool,
    pub result_ids: BTreeMap<String, Value>,
    pub ok: bool,
    pub code: Option<String>,
    /// `person` or `agent:<name>`; other identities cannot be represented
    /// faithfully by a scenario call step.
    pub caller: String,
    /// App names from the verb descriptor's declared `reach`.
    pub app_reach: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedCall {
    pub call_id: String,
    pub verb: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeneratedScenario {
    pub scenario: Scenario,
    pub skipped: Vec<SkippedCall>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordError {
    NoReplayableCalls {
        skipped: Vec<SkippedCall>,
    },
    AmbiguousApps {
        apps: Vec<String>,
    },
    UncapturableDependency {
        consumer_call_id: String,
        producer_call_id: String,
    },
}

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoReplayableCalls { .. } => write!(f, "no replayable calls remain"),
            Self::AmbiguousApps { apps } => write!(
                f,
                "recorded calls reach multiple apps ({}); requires.app cannot be inferred",
                apps.join(", ")
            ),
            Self::UncapturableDependency {
                consumer_call_id,
                producer_call_id,
            } => write!(
                f,
                "call {consumer_call_id} uses an ID from omitted call {producer_call_id}; replay cannot capture that dependency"
            ),
        }
    }
}

impl std::error::Error for RecordError {}

struct ProducedId {
    value: Value,
    path: String,
    call_id: String,
    /// `None` when the producer was omitted (including a child replayed by
    /// its parent). A later consumer must not embed its old literal ID.
    step_index: Option<usize>,
}

/// Build one Tier B scenario from calls supplied oldest first. No store I/O,
/// descriptor lookup or guessed result assertions occur here.
pub fn generate_scenario(
    id: String,
    description: String,
    calls: Vec<RecordedCall>,
) -> Result<GeneratedScenario, RecordError> {
    let parent_by_id: BTreeMap<&str, Option<&str>> = calls
        .iter()
        .map(|call| (call.call_id.as_str(), call.parent_call.as_deref()))
        .collect();
    let mut retained_ids = BTreeSet::new();
    let mut produced = Vec::<ProducedId>::new();
    let mut steps = Vec::<Step>::new();
    let mut skipped = Vec::new();
    let mut apps = BTreeSet::new();

    for call in &calls {
        let skip_reason = if !call.args_replayable {
            Some("arguments were not recorded losslessly")
        } else if !call.args.is_object() {
            Some("recorded arguments are not a complete object")
        } else if impress_scenario::template::escape_literals(&call.args).is_err() {
            Some("literal arguments cannot be escaped without changing their value")
        } else if !canonical_verb(&call.verb) {
            Some("verb name is not canonical")
        } else if caller_as(&call.caller).is_none() {
            Some("caller identity cannot be replayed without impersonation")
        } else if retained_ancestor(call, &retained_ids, &parent_by_id) {
            Some("child call is replayed by a retained ancestor")
        } else {
            None
        };

        let step_index = if let Some(reason) = skip_reason {
            skipped.push(SkippedCall {
                call_id: call.call_id.clone(),
                verb: call.verb.clone(),
                reason: reason.into(),
            });
            // A suppressed child can still declare an app the retained parent
            // reaches indirectly; preserve that Tier B requirement.
            if reason == "child call is replayed by a retained ancestor" {
                apps.extend(call.app_reach.iter().cloned());
            }
            None
        } else {
            let mut args = call.args.clone();
            bind_prior_ids(&mut args, &call.call_id, &produced, &mut steps)?;
            let index = steps.len();
            let caller = caller_as(&call.caller).expect("checked above");
            let step = faithful_event(call, &args, caller)
                .map(Step::Event)
                .unwrap_or_else(|| {
                    Step::Call(CallStep {
                        call: call.verb.clone(),
                        args,
                        r#as: caller.into(),
                        expect: Some(Expect {
                            ok: Some(call.ok),
                            code: call.code.clone(),
                            status: None,
                            fields: Vec::new(),
                        }),
                        capture: BTreeMap::new(),
                        adverbs: None,
                    })
                });
            steps.push(step);
            retained_ids.insert(call.call_id.as_str());
            apps.extend(call.app_reach.iter().cloned());
            Some(index)
        };

        for (path, value) in &call.result_ids {
            if scalar_id(value) {
                produced.push(ProducedId {
                    value: value.clone(),
                    path: path.clone(),
                    call_id: call.call_id.clone(),
                    step_index,
                });
            }
        }
    }

    if steps.is_empty() {
        return Err(RecordError::NoReplayableCalls { skipped });
    }
    if apps.len() > 1 {
        return Err(RecordError::AmbiguousApps {
            apps: apps.into_iter().collect(),
        });
    }
    let requires = apps.into_iter().next().map(|app| Requires {
        app: Some(app),
        ..Requires::default()
    });
    Ok(GeneratedScenario {
        scenario: Scenario {
            wire_version: 1,
            id,
            description,
            tier: Tier::B,
            requires,
            seed: Vec::new(),
            steps,
            teardown: Vec::new(),
            expect_effects: None,
        },
        skipped,
    })
}

fn canonical_verb(name: &str) -> bool {
    let Some((service, method)) = name.split_once('_') else {
        return false;
    };
    fn kebab(s: &str) -> bool {
        !s.is_empty()
            && s.split('-').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            })
    }
    service.ends_with("-service") && kebab(service) && kebab(method)
}

fn caller_as(caller: &str) -> Option<&str> {
    if matches!(caller, "person" | "human") {
        Some("person")
    } else if caller
        .strip_prefix("agent:")
        .is_some_and(|name| !name.is_empty())
    {
        Some(caller)
    } else {
        None
    }
}

fn retained_ancestor<'a>(
    call: &'a RecordedCall,
    retained_ids: &BTreeSet<&str>,
    parent_by_id: &BTreeMap<&str, Option<&'a str>>,
) -> bool {
    let mut current = call.parent_call.as_deref();
    let mut visited = BTreeSet::new();
    while let Some(parent) = current {
        if !visited.insert(parent) {
            break;
        }
        if retained_ids.contains(parent) {
            return true;
        }
        current = parent_by_id.get(parent).copied().flatten();
    }
    false
}

fn scalar_id(value: &Value) -> bool {
    matches!(value, Value::String(_) | Value::Number(_))
}

fn valid_result_path(path: &str) -> bool {
    path == "$"
        || path.strip_prefix("$.").is_some_and(|rest| {
            !rest.is_empty()
                && rest.split('.').all(|part| {
                    !part.is_empty()
                        && part.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
                        })
                })
        })
}

fn bind_prior_ids(
    args: &mut Value,
    consumer_call_id: &str,
    produced: &[ProducedId],
    steps: &mut [Step],
) -> Result<(), RecordError> {
    match args {
        Value::Array(items) => {
            for item in items {
                bind_prior_ids(item, consumer_call_id, produced, steps)?;
            }
        }
        Value::Object(map) => {
            // Values only: JSON object keys are never rewritten.
            for item in map.values_mut() {
                bind_prior_ids(item, consumer_call_id, produced, steps)?;
            }
        }
        value if scalar_id(value) => {
            // The first prior producer wins, including omitted producers: a
            // later duplicate is not evidence that the omitted earlier call
            // can be reconstructed from it.
            if let Some(producer) = produced.iter().find(|prior| prior.value == *value) {
                let Some(index) = producer.step_index else {
                    return Err(RecordError::UncapturableDependency {
                        consumer_call_id: consumer_call_id.into(),
                        producer_call_id: producer.call_id.clone(),
                    });
                };
                if !valid_result_path(&producer.path) {
                    return Err(RecordError::UncapturableDependency {
                        consumer_call_id: consumer_call_id.into(),
                        producer_call_id: producer.call_id.clone(),
                    });
                }
                let Step::Call(origin) = &mut steps[index] else {
                    return Err(RecordError::UncapturableDependency {
                        consumer_call_id: consumer_call_id.into(),
                        producer_call_id: producer.call_id.clone(),
                    });
                };
                let capture_name = origin
                    .capture
                    .iter()
                    .find_map(|(name, path)| {
                        matches!(path, impress_scenario::spec::CallCapture::Path(path) if path == &producer.path)
                            .then(|| name.clone())
                    })
                    .unwrap_or_else(|| {
                        let name = format!("capture_{index}_{}", origin.capture.len());
                        origin.capture.insert(
                            name.clone(),
                            impress_scenario::spec::CallCapture::Path(producer.path.clone()),
                        );
                        name
                    });
                *value = Value::String(format!("{{{{state.{capture_name}}}}}"));
            } else {
                *value = impress_scenario::template::escape_literals(value)
                    .expect("argument literals were checked before binding IDs");
            }
        }
        _ => {}
    }
    Ok(())
}

/// Event steps have no caller, refusal expectation or captures. Convert only
/// the exact successful human dispatch shape those fields can represent.
fn faithful_event(call: &RecordedCall, args: &Value, caller: &str) -> Option<EventStep> {
    if call.verb != SURFACE_DISPATCH_VERB
        || caller != "person"
        || !call.ok
        || call.code.is_some()
        || !call.result_ids.is_empty()
    {
        return None;
    }
    let fields = args.as_object()?;
    if fields
        .keys()
        .any(|key| !matches!(key.as_str(), "id" | "event" | "host" | "params"))
        || !fields.contains_key("id")
        || !fields.contains_key("event")
        || fields.get("host").is_some_and(|value| !value.is_null())
        || fields.get("params").is_some_and(|value| !value.is_null())
    {
        return None;
    }
    let surface = fields.get("id")?.as_str()?.to_string();
    let event = fields.get("event")?.as_object()?;
    if event.len() > 3
        || event
            .keys()
            .any(|key| !matches!(key.as_str(), "widget" | "kind" | "value"))
    {
        return None;
    }
    Some(EventStep {
        event: EventBody {
            surface,
            widget: event.get("widget")?.as_str()?.to_string(),
            kind: event.get("kind")?.as_str()?.to_string(),
            value: event.get("value").cloned().unwrap_or(Value::Null),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(id: &str, verb: &str, args: Value) -> RecordedCall {
        RecordedCall {
            call_id: id.into(),
            parent_call: None,
            verb: verb.into(),
            args,
            args_replayable: true,
            result_ids: BTreeMap::new(),
            ok: true,
            code: None,
            caller: "person".into(),
            app_reach: BTreeSet::new(),
        }
    }

    fn generate(calls: Vec<RecordedCall>) -> GeneratedScenario {
        generate_scenario("recorded.test".into(), "Recorded test".into(), calls).unwrap()
    }

    fn as_call(step: &Step) -> &CallStep {
        match step {
            Step::Call(call) => call,
            other => panic!("expected call step, got {other:?}"),
        }
    }

    #[test]
    fn a_schema_identified_root_uuid_can_be_captured() {
        let mut origin = call("one", "example-service_create", json!({}));
        origin.result_ids.insert("$".into(), json!("paper-id"));
        let later = call("two", "example-service_update", json!({"id":"paper-id"}));
        let generated = generate(vec![origin, later]);
        assert_eq!(
            as_call(&generated.scenario.steps[0]).capture["capture_0_0"],
            impress_scenario::spec::CallCapture::Path("$".into())
        );
        assert_eq!(
            as_call(&generated.scenario.steps[1]).args["id"],
            "{{state.capture_0_0}}"
        );
    }

    #[test]
    fn nested_scalar_id_values_bind_to_one_reusable_capture_without_rewriting_keys_or_substrings() {
        let mut origin = call(
            "one",
            "layout-service_save-layout",
            json!({"name": "layout"}),
        );
        origin
            .result_ids
            .insert("$.result.0.id".into(), json!("id-42"));
        let second = call(
            "two",
            "layout-service_apply-layout",
            json!({"nested": [{"id-42": "id-42", "text": "before-id-42-after"}]}),
        );
        let third = call(
            "three",
            "layout-service_delete-layout",
            json!({"id": "id-42"}),
        );
        let generated = generate(vec![origin, second, third]);
        let first = as_call(&generated.scenario.steps[0]);
        assert_eq!(first.capture.len(), 1);
        assert_eq!(
            first.capture["capture_0_0"],
            impress_scenario::spec::CallCapture::Path("$.result.0.id".into())
        );
        let expected = "{{state.capture_0_0}}";
        let second = as_call(&generated.scenario.steps[1]);
        assert_eq!(second.args["nested"][0]["id-42"], json!(expected));
        assert_eq!(
            second.args["nested"][0]["text"],
            json!("before-id-42-after")
        );
        assert!(second.args["nested"][0].get("id-42").is_some());
        assert_eq!(
            as_call(&generated.scenario.steps[2]).args["id"],
            json!(expected)
        );
    }

    #[test]
    fn duplicate_id_uses_earliest_prior_producer_not_latest_or_future_one() {
        let before = call(
            "before",
            "layout-service_apply-layout",
            json!({"id": "same"}),
        );
        let mut first = call("first", "layout-service_save-layout", json!({}));
        first.result_ids.insert("$.first".into(), json!("same"));
        let mut second = call("second", "layout-service_save-layout", json!({}));
        second.result_ids.insert("$.second".into(), json!("same"));
        let after = call(
            "after",
            "layout-service_delete-layout",
            json!({"id": "same"}),
        );
        let generated = generate(vec![before, first, second, after]);
        assert_eq!(
            as_call(&generated.scenario.steps[0]).args["id"],
            json!("same")
        );
        assert_eq!(
            as_call(&generated.scenario.steps[1]).capture["capture_1_0"],
            impress_scenario::spec::CallCapture::Path("$.first".into())
        );
        assert!(as_call(&generated.scenario.steps[2]).capture.is_empty());
        assert_eq!(
            as_call(&generated.scenario.steps[3]).args["id"],
            json!("{{state.capture_1_0}}")
        );
    }

    #[test]
    fn nonlossless_system_and_noncanonical_calls_are_reported_as_skipped() {
        let mut reduced = call("reduced", "layout-service_save-layout", json!({"id": "x"}));
        reduced.args_replayable = false;
        let mut system = call("system", "layout-service_save-layout", json!({}));
        system.caller = "system:background".into();
        let alias = call("alias", "save_layout", json!({}));
        let safe = call("safe", "layout-service_save-layout", json!({}));
        let generated = generate(vec![reduced, system, alias, safe]);
        assert_eq!(generated.scenario.steps.len(), 1);
        assert_eq!(generated.skipped.len(), 3);
        assert!(generated.skipped[0].reason.contains("losslessly"));
        assert!(generated.skipped[1].reason.contains("impersonation"));
        assert!(generated.skipped[2].reason.contains("canonical"));
    }

    #[test]
    fn recorded_literals_survive_replay_without_becoming_captures() {
        let inputs = [
            json!({"nested": ["{{state.paper}}"]}),
            json!({"name": "prefix-{{uuid}}"}),
            json!({"name": "{{not-a-template}}"}),
            json!({"name": "{{!state.already_escaped}}"}),
        ];
        let calls = inputs
            .iter()
            .enumerate()
            .map(|(i, args)| {
                call(
                    &format!("literal-{i}"),
                    "layout-service_save-layout",
                    args.clone(),
                )
            })
            .collect();
        let generated = generate(calls);
        assert_eq!(generated.scenario.steps.len(), inputs.len());
        assert!(generated.skipped.is_empty());
        for (step, input) in generated.scenario.steps.iter().zip(inputs) {
            let args = &as_call(step).args;
            assert_eq!(
                impress_scenario::template::resolve(args, &Value::Null).unwrap(),
                input
            );
        }
    }

    #[test]
    fn only_faithful_successful_person_dispatch_becomes_an_event() {
        assert!(impress_surface_service::verb_exists(SURFACE_DISPATCH_VERB));
        assert!(impress_service_core::VerbDescriptor::find(SURFACE_DISPATCH_VERB).is_some());
        let args =
            json!({"id": "surface-1", "event": {"widget": "bins", "kind": "change", "value": 17}});
        let success = call("success", SURFACE_DISPATCH_VERB, args.clone());
        let with_null_optionals = call(
            "with-null-optionals",
            SURFACE_DISPATCH_VERB,
            json!({"id": "surface-1", "event": {"widget": "bins", "kind": "change", "value": 17}, "host": null, "params": null}),
        );
        let mut failed = call("failed", SURFACE_DISPATCH_VERB, args.clone());
        failed.ok = false;
        failed.code = Some("conflict".into());
        let mut with_result = call("with-result", SURFACE_DISPATCH_VERB, args.clone());
        with_result
            .result_ids
            .insert("$.id".into(), json!("produced"));
        let with_host = call(
            "with-host",
            SURFACE_DISPATCH_VERB,
            json!({"id": "surface-1", "event": {"widget": "bins", "kind": "change", "value": 17}, "host": "detail"}),
        );
        let mut agent = call("agent", SURFACE_DISPATCH_VERB, args);
        agent.caller = "agent:reader".into();
        let generated = generate(vec![
            success,
            with_null_optionals,
            failed,
            with_result,
            with_host,
            agent,
        ]);
        match &generated.scenario.steps[0] {
            Step::Event(event) => {
                assert_eq!(event.event.surface, "surface-1");
                assert_eq!(event.event.widget, "bins");
                assert_eq!(event.event.kind, "change");
                assert_eq!(event.event.value, json!(17));
            }
            other => panic!("expected event, got {other:?}"),
        }
        assert!(matches!(generated.scenario.steps[1], Step::Event(_)));
        let refusal = as_call(&generated.scenario.steps[2]);
        assert_eq!(refusal.expect.as_ref().unwrap().ok, Some(false));
        assert_eq!(
            refusal.expect.as_ref().unwrap().code.as_deref(),
            Some("conflict")
        );
        for step in &generated.scenario.steps[3..] {
            assert!(matches!(step, Step::Call(_)));
        }
    }

    #[test]
    fn retained_parent_suppresses_children_but_skipped_parent_leaves_descendant() {
        let parent = call("parent", "layout-service_apply-layout", json!({}));
        let mut child = call("child", "layout-service_save-layout", json!({}));
        child.parent_call = Some("parent".into());
        let mut grandchild = call("grandchild", "layout-service_save-layout", json!({}));
        grandchild.parent_call = Some("child".into());
        let generated = generate(vec![parent, child, grandchild]);
        assert_eq!(generated.scenario.steps.len(), 1);
        assert_eq!(generated.skipped.len(), 2);
        assert!(generated
            .skipped
            .iter()
            .all(|entry| entry.reason.contains("ancestor")));

        let mut skipped_parent = call("parent", "layout-service_apply-layout", json!({}));
        skipped_parent.args_replayable = false;
        let mut child = call("child", "layout-service_save-layout", json!({}));
        child.parent_call = Some("parent".into());
        let generated = generate(vec![skipped_parent, child]);
        assert_eq!(generated.scenario.steps.len(), 1);
        assert_eq!(
            as_call(&generated.scenario.steps[0]).call,
            "layout-service_save-layout"
        );
        assert_eq!(generated.skipped.len(), 1);
    }

    #[test]
    fn dependency_on_suppressed_child_is_refused_instead_of_embedding_a_stale_id() {
        let parent = call("parent", "layout-service_apply-layout", json!({}));
        let mut child = call("child", "layout-service_save-layout", json!({}));
        child.parent_call = Some("parent".into());
        child.result_ids.insert("$.id".into(), json!("child-id"));
        let consumer = call(
            "consumer",
            "layout-service_delete-layout",
            json!({"id": "child-id"}),
        );
        let error =
            generate_scenario("x".into(), "x".into(), vec![parent, child, consumer]).unwrap_err();
        assert_eq!(
            error,
            RecordError::UncapturableDependency {
                consumer_call_id: "consumer".into(),
                producer_call_id: "child".into(),
            }
        );
    }

    #[test]
    fn failed_call_keeps_outcome_and_agent_identity() {
        let mut failed = call("failed", "layout-service_save-layout", json!({}));
        failed.ok = false;
        failed.code = Some("conflict".into());
        failed.caller = "agent:reviewer".into();
        let generated = generate(vec![failed]);
        let step = as_call(&generated.scenario.steps[0]);
        assert_eq!(step.r#as, "agent:reviewer");
        assert_eq!(step.expect.as_ref().unwrap().ok, Some(false));
        assert_eq!(
            step.expect.as_ref().unwrap().code.as_deref(),
            Some("conflict")
        );
        assert!(step.expect.as_ref().unwrap().fields.is_empty());
    }

    #[test]
    fn app_reach_is_inferred_only_when_unambiguous() {
        let mut imbib = call("imbib", "imbib-library-service_list-libraries", json!({}));
        imbib.app_reach.insert("imbib".into());
        let generated = generate(vec![imbib.clone()]);
        assert_eq!(generated.scenario.tier, Tier::B);
        assert_eq!(
            generated.scenario.requires.unwrap().app.as_deref(),
            Some("imbib")
        );
        let mut imprint = call(
            "imprint",
            "imprint-project-service_list-projects",
            json!({}),
        );
        imprint.app_reach.insert("imprint".into());
        let error = generate_scenario("x".into(), "x".into(), vec![imbib, imprint]).unwrap_err();
        assert_eq!(
            error,
            RecordError::AmbiguousApps {
                apps: vec!["imbib".into(), "imprint".into()]
            }
        );
    }
}
