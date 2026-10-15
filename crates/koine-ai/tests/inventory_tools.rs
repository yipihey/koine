//! The model's tools are the linked inventory (docs/plan-koine-ai-tasks.md
//! AI-D4): one tool per service namespace, `describe` answers the verb's own
//! schema, and a call runs through the pipeline as the adapter's agent.
#![cfg(feature = "service")]

use std::sync::Arc;

use koine_ai::inventory_tools::{CAPABILITIES_TOOL, POLICY_ID};
use koine_ai::{InventoryTools, ToolAdapter};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn a_tool_call_is_a_verb_call_as_the_agent() {
    koine_agent::ensure_linked();
    koine_ai::ensure_linked();
    koine_store::process::install(Arc::new(koine_store_memory::MemoryStore::new()));
    let tools = InventoryTools::new("inventory-test").excluding(["impress-ai-service"]);

    let catalog = tools.catalog();
    let offered = &catalog[POLICY_ID];
    let names: Vec<&str> = offered.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names[0], CAPABILITIES_TOOL);
    assert!(names.contains(&"agent-service"), "{names:?}");
    assert!(
        !names.contains(&"impress-ai-service"),
        "an excluded namespace is not offered"
    );
    let desk = offered.iter().find(|t| t.name == "agent-service").unwrap();
    let actions = desk.input_schema["properties"]["action"]["enum"]
        .as_array()
        .unwrap();
    assert!(actions.contains(&json!("ask")));

    let described = tools
        .call(
            "agent-service",
            json!({ "action": "ask", "describe": true }),
        )
        .await
        .unwrap();
    assert_eq!(described["tool"], "agent-service_ask");
    assert!(described["inputSchema"]["properties"]["prompt"].is_object());

    let asked = tools
        .call(
            "agent-service",
            json!({ "action": "ask", "args": { "prompt": "from the model", "with_context": false } }),
        )
        .await
        .unwrap();
    assert_eq!(asked["ok"], true, "{asked}");
    assert_eq!(asked["request"]["created_by"], "agent:inventory-test");

    // A refusal is the verb's own answer, not an error.
    let refused = tools
        .call(
            "agent-service",
            json!({ "action": "ask", "args": { "prompt": "  " } }),
        )
        .await
        .unwrap();
    assert_eq!(refused["ok"], false, "{refused}");

    assert!(tools
        .call("agent-service", json!({ "action": "no-such-action" }))
        .await
        .is_err());
    assert!(tools
        .call("agent-service", json!({ "action": "ask", "args": [1] }))
        .await
        .is_err());
    let overview = tools.call(CAPABILITIES_TOOL, json!({})).await.unwrap();
    assert!(overview["namespaces"]
        .as_array()
        .unwrap()
        .iter()
        .any(|ns| ns["tool"] == "agent-service"));
}
