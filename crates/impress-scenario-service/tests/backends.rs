//! The scenario verbs over every backend of the store port: a stored
//! scenario reads back by its stable id and lists, on the memory and the
//! directory store alike (docs/plan-koine-kit-moves.md KM-D2).

use std::sync::Arc;

use impress_item::AttributedStore;
use impress_scenario_service::dto::SpecArg;
use impress_scenario_service::{DefaultImpressScenarioService, ImpressScenarioService};
use serde_json::json;

fn spec() -> serde_json::Value {
    json!({
        "wire_version": 1,
        "id": "backends.noop",
        "description": "a stored scenario on any backend",
        "tier": "a",
        "steps": [
            {"call": "impress-scenario-service_scenario-list", "args": {}, "as": "agent:scenario"}
        ]
    })
}

async fn create_get_list(store: Arc<dyn AttributedStore>) {
    let service = DefaultImpressScenarioService::with_store(store);
    let created = service
        .scenario_create(SpecArg(spec()), Some(vec!["backends".into()]))
        .await;
    assert!(created.ok, "{created:?}");

    let by_scenario_id = service.scenario_get("backends.noop".into()).await;
    assert!(by_scenario_id.ok, "{by_scenario_id:?}");
    assert_eq!(by_scenario_id.id, created.id);
    assert_eq!(by_scenario_id.spec["id"], "backends.noop");

    let by_row_id = service.scenario_get(created.id.clone()).await;
    assert_eq!(by_row_id.scenario_id, "backends.noop");

    let listed = service.scenario_list().await;
    assert!(listed.ok);
    assert_eq!(listed.scenarios.len(), 1);
}

#[tokio::test]
async fn scenarios_store_and_read_back_on_the_memory_store() {
    create_get_list(Arc::new(koine_store_memory::MemoryStore::new())).await;
}

#[tokio::test]
async fn scenarios_store_and_read_back_on_the_directory_store() {
    let dir = tempfile::tempdir().unwrap();
    create_get_list(Arc::new(
        koine_store_dir::DirStore::open(dir.path()).unwrap(),
    ))
    .await;
}
