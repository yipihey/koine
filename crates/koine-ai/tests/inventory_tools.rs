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

mod flat {
    use super::*;
    use koine_ai::schema::Profile;

    fn link() {
        koine_agent::ensure_linked();
        koine_ai::ensure_linked();
        koine_store::process::install(Arc::new(koine_store_memory::MemoryStore::new()));
    }

    fn names(tools: &InventoryTools) -> Vec<String> {
        tools.catalog()[POLICY_ID]
            .iter()
            .map(|t| t.name.clone())
            .collect()
    }

    #[test]
    fn the_catalog_lists_the_allowed_linked_verbs_with_schemas_and_annotations() {
        link();
        let tools = InventoryTools::flat(
            "flat-test",
            ["agent-service_ask", "no-such-service_verb"],
            None,
        );
        let catalog = tools.catalog();
        let offered = &catalog[POLICY_ID];
        assert_eq!(offered.len(), 1, "{offered:?}");
        let ask = &offered[0];
        assert_eq!(ask.name, "agent-service_ask");
        assert!(!ask.description.is_empty());
        assert!(ask.input_schema["properties"]["prompt"].is_object());
        assert!(ask.output_schema.is_some());
        let annotations = ask.annotations.as_ref().unwrap();
        assert!(annotations["readOnlyHint"].is_boolean());
        assert!(annotations["destructiveHint"].is_boolean());
        assert!(!names(&tools).contains(&CAPABILITIES_TOOL.to_string()));
    }

    #[test]
    fn a_namespace_prefix_expands_to_every_verb_in_it() {
        link();
        let tools = InventoryTools::flat("flat-test", ["agent-service_"], None);
        let listed = names(&tools);
        assert!(
            listed.contains(&"agent-service_ask".to_string()),
            "{listed:?}"
        );
        assert!(listed.iter().all(|n| n.starts_with("agent-service_")));
        assert!(listed.windows(2).all(|w| w[0] < w[1]), "sorted, no dupes");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_call_passes_its_arguments_straight_through() {
        link();
        let tools = InventoryTools::flat("flat-test", ["agent-service_ask"], None);
        let asked = tools
            .call(
                "agent-service_ask",
                json!({ "prompt": "flat", "with_context": false }),
            )
            .await
            .unwrap();
        assert_eq!(asked["ok"], true, "{asked}");
        assert_eq!(asked["request"]["created_by"], "agent:flat-test");

        let refused = tools
            .call("agent-service_ask", json!({ "prompt": "  " }))
            .await
            .unwrap();
        assert_eq!(refused["ok"], false, "{refused}");

        let not_offered = tools
            .call("agent-service_list", json!({}))
            .await
            .unwrap_err();
        assert!(
            not_offered.to_string().contains("not offered"),
            "{not_offered}"
        );
        assert!(tools.call("agent-service_ask", json!([1])).await.is_err());
    }

    #[test]
    fn a_profile_lowers_every_schema_and_skips_what_it_cannot_express() {
        link();
        let tools = InventoryTools::flat(
            "flat-test",
            ["agent-service_", "task-service_"],
            Some(Profile::AppleGuided),
        );
        let catalog = tools.catalog();
        let offered = &catalog[POLICY_ID];
        assert!(!offered.is_empty());
        for tool in offered {
            assert_eq!(
                tool.input_schema["additionalProperties"],
                json!(false),
                "{}",
                tool.name
            );
        }
        let skipped = tools.skipped();
        // `task-service_enqueue` takes any verb's arguments, so Apple can't
        // express it; it must be linked here for this check to mean anything.
        assert!(koine_tasks_linked(), "task-service_enqueue is not linked");
        assert!(skipped.iter().any(|(v, _)| v == "task-service_enqueue"));
        assert!(!offered.iter().any(|t| t.name == "task-service_enqueue"));
        assert!(tools
            .call_blocking_not_offered("task-service_enqueue")
            .contains("not offered"));
        for (verb, unsupported) in &skipped {
            assert!(!offered.iter().any(|t| &t.name == verb));
            assert!(!unsupported.reason.is_empty());
        }
    }

    fn koine_tasks_linked() -> bool {
        impress_service_core::call::descriptors().any(|h| h.name() == "task-service_enqueue")
    }

    trait NotOffered {
        fn call_blocking_not_offered(&self, name: &str) -> String;
    }
    impl NotOffered for InventoryTools {
        fn call_blocking_not_offered(&self, name: &str) -> String {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            runtime
                .block_on(self.call(name, json!({})))
                .unwrap_err()
                .to_string()
        }
    }
}
