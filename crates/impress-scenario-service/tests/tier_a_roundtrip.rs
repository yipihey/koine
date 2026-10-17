//! S1's proof, Tier A half: a scenario spec, validated, run against a
//! scratch store through the pipeline (H-P2-3), reported the shared way.

use impress_scenario_service::{DefaultImpressScenarioService, ImpressScenarioService};
use serde_json::json;

fn a_layout_scenario() -> serde_json::Value {
    json!({
        "wire_version": 1,
        "id": "test.layout.list_layouts",
        "description": "list-layouts answers ok for a fresh app id",
        "tier": "a",
        "steps": [
            {
                "call": "layout-service_list-layouts",
                "args": {"app_id": "scenario-test"},
                "as": "agent:scenario",
                "expect": {"ok": true}
            }
        ]
    })
}

#[tokio::test]
async fn a_scenario_validates_and_runs_tier_a() {
    let service = DefaultImpressScenarioService::new();
    let spec = a_layout_scenario();

    let validated = service
        .scenario_validate(impress_scenario_service::dto::SpecArg(spec.clone()))
        .await;
    assert!(validated.ok, "{validated:?}");

    let created = service
        .scenario_create(impress_scenario_service::dto::SpecArg(spec), None)
        .await;
    assert!(created.ok, "{created:?}");
    assert_eq!(created.scenario_id, "test.layout.list_layouts");

    let run = service
        .scenario_run(
            "test.layout.list_layouts".to_string(),
            Some("a".to_string()),
            None,
        )
        .await;
    assert!(run.ok, "{run:?}");
    assert_eq!(run.passed, 1);
    assert_eq!(run.failed, 0);
}

#[tokio::test]
async fn an_unknown_verb_fails_validation() {
    let service = DefaultImpressScenarioService::new();
    let mut spec = a_layout_scenario();
    spec["steps"][0]["call"] = json!("no-such-service_no-such-verb");
    let validated = service
        .scenario_validate(impress_scenario_service::dto::SpecArg(spec))
        .await;
    assert!(!validated.ok);
    assert!(validated
        .problems
        .iter()
        .any(|p| p.message.contains("no such verb")));
}
