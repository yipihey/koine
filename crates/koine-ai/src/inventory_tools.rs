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
//!
//! [`InventoryTools::flat`] is the other shape (LM-D4): one tool per allowed
//! verb, named by the verb, with the verb's own input schema (lowered for a
//! [`Profile`] when one is given), its output schema and its MCP
//! annotations. The arguments object is the verb's arguments, with no
//! `action`/`args` wrapper, and there is no `koine_capabilities` tool.

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use impress_service_core::pipeline::{self, Call, PipelineError};
use impress_service_core::{call, ProviderStatus, VerbHandle};
use serde_json::{json, Map, Value};

use crate::schema::{self, Profile, Unsupported};
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
    flat: Option<Flat>,
}

/// The flat mode's allowlist and schema profile.
#[derive(Debug, Clone)]
struct Flat {
    allow: Vec<String>,
    profile: Option<Profile>,
}

impl Flat {
    fn allows(&self, verb: &str) -> bool {
        self.allow.iter().any(|entry| {
            entry == verb || (entry.ends_with('_') && verb.starts_with(entry.as_str()))
        })
    }
}

impl InventoryTools {
    /// Calls run as `agent:<agent>`.
    pub fn new(agent: impl Into<String>) -> Self {
        Self {
            agent: agent.into(),
            excluded: Vec::new(),
            flat: None,
        }
    }

    /// One tool per allowed verb, each with the verb's own input schema
    /// lowered for `profile` (the raw schema when `None`).
    ///
    /// An entry of `verbs` is a full verb name (`task-service_list`) or a
    /// namespace prefix ending in `_` (`task-service_`), which means every
    /// verb in that namespace. A verb that is not linked, is unavailable or
    /// deprecated, is in an [excluded](Self::excluding) namespace, or whose
    /// schema the profile cannot express is left out of the catalog;
    /// [`skipped`](Self::skipped) reports the last kind. The catalog is
    /// computed on each [`catalog`](ToolAdapter::catalog) call, since the
    /// inventory can change.
    pub fn flat<I, S>(agent: impl Into<String>, verbs: I, profile: Option<Profile>) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            flat: Some(Flat {
                allow: verbs.into_iter().map(Into::into).collect(),
                profile,
            }),
            ..Self::new(agent)
        }
    }

    /// Flat mode: the allowed verbs whose schemas the profile cannot
    /// express, with why. Empty in grouped mode.
    pub fn skipped(&self) -> Vec<(String, Unsupported)> {
        self.flat_entries().1
    }

    /// Allowed, available verbs by name, split into offered definitions and
    /// the ones the profile refused.
    fn flat_entries(&self) -> (Vec<ToolDefinition>, Vec<(String, Unsupported)>) {
        let Some(flat) = &self.flat else {
            return (Vec::new(), Vec::new());
        };
        let mut handles: Vec<VerbHandle> = self
            .available()
            .filter(|handle| flat.allows(handle.name()))
            .collect();
        handles.sort_by(|a, b| a.name().cmp(b.name()));
        handles.dedup_by(|a, b| a.name() == b.name());
        let (mut offered, mut skipped) = (Vec::new(), Vec::new());
        for handle in handles {
            let input = match flat.profile {
                None => handle.input_schema().into_owned(),
                Some(profile) => {
                    let title = handle.name().replace(['-', '.'], "_");
                    match schema::lower(&handle.input_schema(), &title, profile) {
                        Ok(lowered) => lowered,
                        Err(unsupported) => {
                            skipped.push((handle.name().to_string(), unsupported));
                            continue;
                        }
                    }
                }
            };
            let mut tool = ToolDefinition::new(handle.name(), handle.description(), input);
            let output = handle.output_schema().into_owned();
            let empty = output.is_null() || output.as_object().is_some_and(|o| o.is_empty());
            if !empty {
                tool.output_schema = Some(output);
            }
            tool.annotations = Some(handle.mcp_annotations());
            offered.push(tool);
        }
        (offered, skipped)
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
        if self.flat.is_some() {
            return self.flat_entries().0;
        }
        let groups = self.groups();
        let mut tools = vec![ToolDefinition::new(
            CAPABILITIES_TOOL,
            "List the tools (one per service) and how many actions each has.",
            json!({ "type": "object", "properties": {} }),
        )];
        for (ns, actions) in groups {
            tools.push(ToolDefinition::new(
                ns.clone(),
                format!(
                    "The {ns} verbs. Pass `describe: true` with an action for its argument schema."
                ),
                json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": actions },
                        "args": { "type": "object" },
                        "describe": { "type": "boolean" }
                    },
                    "required": ["action"]
                }),
            ));
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

impl InventoryTools {
    /// Flat mode: the tool name is the verb, the arguments are the verb's.
    async fn call_flat(&self, tool_name: &str, arguments: Value) -> Result<Value> {
        let not_offered = || Error::Invalid(format!("{tool_name}: not offered"));
        let offered = self
            .flat_entries()
            .0
            .iter()
            .any(|tool| tool.name == tool_name);
        let handle = self
            .available()
            .find(|handle| handle.name() == tool_name)
            .filter(|_| offered)
            .ok_or_else(not_offered)?;
        let args = match arguments {
            Value::Object(values) => Value::Object(values),
            Value::Null => Value::Object(Map::new()),
            _ => {
                return Err(Error::Invalid(format!(
                    "{tool_name}: arguments must be an object"
                )))
            }
        };
        pipeline::invoke_handle(handle, Call::agent(self.agent.clone(), args))
            .await
            .map_err(|error| match error {
                PipelineError::Handler(e) => Error::Invalid(format!("{tool_name} failed: {e}")),
                unavailable => Error::Invalid(unavailable.to_string()),
            })
    }
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
        if self.flat.is_some() {
            return self.call_flat(tool_name, arguments).await;
        }
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
