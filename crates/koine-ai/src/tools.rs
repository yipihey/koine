//! Model-visible tool adapter boundary.
//!
//! Implementations project an existing capability authority (normally the
//! generated `McpToolDescriptor` inventory) into stable policy buckets. The AI
//! core never redefines service schemas or handlers.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde_json::Value;

use crate::{Error, Result, ToolDefinition};

#[async_trait]
pub trait ToolAdapter: Send + Sync {
    /// Definitions grouped by the stable ids persisted in `enabled_tools`.
    fn catalog(&self) -> BTreeMap<String, Vec<ToolDefinition>>;

    /// A non-secret implementation id recorded in tool provenance.
    fn provider_id(&self, tool_name: &str) -> String;

    async fn call(&self, tool_name: &str, arguments: Value) -> Result<Value>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PendingToolCall {
    pub index: u32,
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Default)]
pub(crate) struct ToolCallAccumulator {
    calls: BTreeMap<u32, PendingToolCall>,
}

impl ToolCallAccumulator {
    pub fn push(
        &mut self,
        index: u32,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    ) {
        let call = self.calls.entry(index).or_insert_with(|| PendingToolCall {
            index,
            ..Default::default()
        });
        if let Some(id) = id {
            call.id = id;
        }
        if let Some(name) = name {
            call.name.push_str(&name);
        }
        call.arguments.push_str(&arguments);
    }

    pub fn finish(self) -> Result<Vec<PendingToolCall>> {
        self.calls
            .into_values()
            .map(|call| {
                if call.name.is_empty() {
                    return Err(Error::Invalid(format!(
                        "tool call {} did not include a function name",
                        call.index
                    )));
                }
                if call.id.is_empty() {
                    return Err(Error::Invalid(format!(
                        "tool call {} did not include an id",
                        call.index
                    )));
                }
                let arguments = if call.arguments.trim().is_empty() {
                    "{}"
                } else {
                    &call.arguments
                };
                let value: Value = serde_json::from_str(arguments).map_err(|error| {
                    Error::Invalid(format!(
                        "{} returned invalid tool arguments: {error}",
                        call.name
                    ))
                })?;
                if !value.is_object() {
                    return Err(Error::Invalid(format!(
                        "{} tool arguments must be an object",
                        call.name
                    )));
                }
                Ok(call)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_fragmented_calls_in_index_order() {
        let mut calls = ToolCallAccumulator::default();
        calls.push(1, Some("b".into()), Some("second".into()), "{}".into());
        calls.push(
            0,
            Some("a".into()),
            Some("first".into()),
            "{\"query\":".into(),
        );
        calls.push(0, None, None, "\"stars\"}".into());
        let calls = calls.finish().unwrap();
        assert_eq!(calls[0].name, "first");
        assert_eq!(calls[0].arguments, r#"{"query":"stars"}"#);
        assert_eq!(calls[1].name, "second");
    }

    #[test]
    fn rejects_non_object_arguments() {
        let mut calls = ToolCallAccumulator::default();
        calls.push(0, Some("a".into()), Some("bad".into()), "[]".into());
        assert!(calls.finish().is_err());
    }
}
