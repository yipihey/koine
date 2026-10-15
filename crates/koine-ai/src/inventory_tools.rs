//! The model's tools are the linked inventory (docs/plan-koine-ai-tasks.md
//! AI-D4).
//!
//! [`InventoryTools`] offers every linked verb, grouped by service
//! namespace (`task-service`, `agent-service`, …): one tool per namespace
//! whose `action` is the verb, `args` its arguments, and `describe: true`
//! answers the verb's own input schema instead of calling it. A local model
//! keeps its context for the work, not for hundreds of flat schemas; a
//! namespace tool lists its actions, `koine_capabilities` lists the
//! namespaces.
//!
//! A call goes through the pipeline as the agent this adapter names, so the
//! call log, the notebook and a review policy see it like any agent's call.
//! A refusal is the verb's own answer (`ok: false`); an unreachable verb is
//! an error the model reads. Nothing is hidden or rewritten. The policy id
//! a conversation enables these with is [`POLICY_ID`].

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use impress_service_core::pipeline::{self, Call, PipelineError};
use impress_service_core::{call, ProviderStatus, VerbHandle};
use serde_json::{json, Map, Value};

use crate::{Error, Result, ToolAdapter, ToolDefinition};

/// The tool-policy id a conversation lists to get these tools.
pub const POLICY_ID: &str = "koine-verbs";

/// The overview tool.
pub const CAPABILITIES_TOOL: &str = "koine_capabilities";

/// The linked verbs as model tools.
#[derive(Debug, Clone)]
pub struct InventoryTools {
    agent: String,
    excluded: Vec<String>,
}

impl InventoryTools {
    /// Calls run as `agent:<agent>`.
    pub fn new(agent: impl Into<String>) -> Self {
        Self {
            agent: agent.into(),
            excluded: Vec::new(),
        }
    }

    /// Leave out every verb whose namespace is one of `namespaces`.
    pub fn excluding<I, S>(mut self, namespaces: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.excluded.extend(namespaces.into_iter().map(Into::into));
        self
    }

    fn available(&self) -> impl Iterator<Item = VerbHandle> + '_ {
        call::descriptors().filter(move |handle| {
            handle.provider_status() != Some(ProviderStatus::Unavailable)
                && handle.deprecation_notice().is_none()
                && namespace_of(handle.name())
                    .is_some_and(|ns| !self.excluded.iter().any(|ex| ex == ns))
        })
    }

    /// Namespace → its verbs' actions, sorted.
    fn groups(&self) -> BTreeMap<String, BTreeSet<String>> {
        let mut groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for handle in self.available() {
            if let (Some(ns), Some(action)) =
                (namespace_of(handle.name()), action_of(handle.name()))
            {
                groups
                    .entry(ns.to_string())
                    .or_default()
                    .insert(action.to_string());
            }
        }
        groups
    }

    fn resolve(&self, namespace: &str, action: &str) -> Option<VerbHandle> {
        self.available().find(|handle| {
            namespace_of(handle.name()) == Some(namespace)
                && action_of(handle.name()) == Some(action)
        })
    }

    fn capabilities(&self) -> Value {
        let namespaces: Vec<Value> = self
            .groups()
            .into_iter()
            .map(|(ns, actions)| json!({ "tool": ns, "actions": actions.len() }))
            .collect();
        json!({ "namespaces": namespaces })
    }

    fn definitions(&self) -> Vec<ToolDefinition> {
        let groups = self.groups();
        let mut tools = vec![ToolDefinition {
            name: CAPABILITIES_TOOL.into(),
            description: "List the tools (one per service) and how many actions each has.".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
        }];
        for (ns, actions) in groups {
            tools.push(ToolDefinition {
                description: format!(
                    "The {ns} verbs. Pass `describe: true` with an action for its argument schema."
                ),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": actions },
                        "args": { "type": "object" },
                        "describe": { "type": "boolean" }
                    },
                    "required": ["action"]
                }),
                name: ns,
            });
        }
        tools
    }
}

impl Default for InventoryTools {
    fn default() -> Self {
        Self::new("koine-ai")
    }
}

/// `task-service` of `task-service_enqueue`.
pub fn namespace_of(verb: &str) -> Option<&str> {
    verb.split_once('_')
        .map(|(ns, _)| ns)
        .filter(|ns| !ns.is_empty())
}

/// `enqueue` of `task-service_enqueue`.
pub fn action_of(verb: &str) -> Option<&str> {
    verb.split_once('_')
        .map(|(_, action)| action)
        .filter(|action| !action.is_empty())
}

#[async_trait]
impl ToolAdapter for InventoryTools {
    fn catalog(&self) -> BTreeMap<String, Vec<ToolDefinition>> {
        BTreeMap::from([(POLICY_ID.to_string(), self.definitions())])
    }

    fn provider_id(&self, _tool_name: &str) -> String {
        "koine-inventory".into()
    }

    async fn call(&self, tool_name: &str, arguments: Value) -> Result<Value> {
        if tool_name == CAPABILITIES_TOOL {
            return Ok(self.capabilities());
        }
        let action = arguments
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Invalid(format!("{tool_name}: missing action")))?;
        let handle = self.resolve(tool_name, action).ok_or_else(|| {
            Error::Invalid(format!(
                "{tool_name}: unknown or unavailable action {action}"
            ))
        })?;
        if arguments.get("describe").and_then(Value::as_bool) == Some(true) {
            return Ok(json!({
                "tool": handle.name(),
                "action": action,
                "description": handle.description(),
                "inputSchema": handle.input_schema()
            }));
        }
        let args = match arguments.get("args") {
            Some(Value::Object(values)) => Value::Object(values.clone()),
            Some(Value::Null) | None => Value::Object(Map::new()),
            Some(_) => {
                return Err(Error::Invalid(format!(
                    "{tool_name}: args must be an object"
                )))
            }
        };
        let name = handle.name().to_string();
        pipeline::invoke_handle(handle, Call::agent(self.agent.clone(), args))
            .await
            .map_err(|error| match error {
                PipelineError::Handler(e) => Error::Invalid(format!("{name} failed: {e}")),
                unavailable => Error::Invalid(unavailable.to_string()),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verb_name_splits_into_its_tool_and_action() {
        assert_eq!(namespace_of("task-service_enqueue"), Some("task-service"));
        assert_eq!(action_of("task-service_enqueue"), Some("enqueue"));
        assert_eq!(namespace_of("nounderscore"), None);
        assert_eq!(action_of("trailing_"), None);
    }
}
