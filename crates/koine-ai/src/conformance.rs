//! The shared tool-loop fixture (docs/plan-koine-local-models.md, "What pins
//! this"): one request, one tool adapter and one set of property checks that
//! every backend (scripted, llama.cpp, Apple) is run through. Not API.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agent_loop::LoopOutcome;
use crate::types::{
    ChatRequest, ModelContentPart, ModelMessage, ResponseFormat, Role, ToolDefinition,
};
use crate::{Error, Result, ToolAdapter};

/// `weather_lookup {city, unit}` and the parameterless `deny_me`.
pub fn tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "weather_lookup".into(),
            description: "Current temperature for a city.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "city": { "type": "string" },
                    "unit": { "type": "string", "enum": ["c", "f"] }
                },
                "required": ["city", "unit"]
            }),
        },
        ToolDefinition {
            name: "deny_me".into(),
            description: "A tool the policy always refuses.".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
        },
    ]
}

/// The final answer: `{city, temperature, summary}`, all required.
pub fn answer_schema() -> ResponseFormat {
    ResponseFormat::JsonSchema {
        name: "answer".into(),
        schema: json!({
            "type": "object",
            "properties": {
                "city": { "type": "string" },
                "temperature": { "type": "integer" },
                "summary": { "type": "string" }
            },
            "required": ["city", "temperature", "summary"],
            "additionalProperties": false
        }),
        strict: true,
    }
}

/// The weather question, with both tools and the answer schema.
pub fn request(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.into(),
        messages: vec![
            ModelMessage::text(
                Role::System,
                "Use the weather_lookup tool for weather questions.",
            ),
            ModelMessage::text(Role::User, "What is the temperature in Paris in celsius?"),
        ],
        tools: tools(),
        response_format: Some(answer_schema()),
        ..Default::default()
    }
}

/// The adapter behind the fixture tools; records `weather_lookup` arguments.
#[derive(Default)]
pub struct FixtureTools {
    pub weather_calls: Mutex<Vec<Value>>,
}

#[async_trait]
impl ToolAdapter for FixtureTools {
    fn catalog(&self) -> BTreeMap<String, Vec<ToolDefinition>> {
        BTreeMap::from([("conformance".to_string(), tools())])
    }

    fn provider_id(&self, _tool_name: &str) -> String {
        "conformance".into()
    }

    async fn call(&self, tool_name: &str, arguments: Value) -> Result<Value> {
        match tool_name {
            "weather_lookup" => {
                self.weather_calls.lock().unwrap().push(arguments.clone());
                Ok(json!({
                    "city": arguments["city"],
                    "temperature": 18,
                    "unit": arguments["unit"],
                }))
            }
            // The shape a pipeline refusal has through `InventoryTools`.
            "deny_me" => Ok(json!({
                "ok": false,
                "code": "denied",
                "message": "policy refused deny_me",
            })),
            other => Err(Error::Invalid(format!("unknown tool {other}"))),
        }
    }
}

fn text_of(message: &ModelMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|part| match part {
            ModelContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Properties of a finished weather turn, not wording.
pub fn check_weather_turn(
    outcome: &LoopOutcome,
    tools: &FixtureTools,
) -> std::result::Result<(), String> {
    let calls = tools.weather_calls.lock().unwrap();
    if !calls.iter().any(|arguments| {
        arguments["city"]
            .as_str()
            .is_some_and(|city| city.to_lowercase().contains("paris"))
            && arguments["unit"].as_str() == Some("c")
    }) {
        return Err(format!(
            "weather_lookup was not called for Paris in c; calls: {calls:?}"
        ));
    }

    let call_ids: Vec<&str> = outcome
        .messages
        .iter()
        .filter(|message| message.role == Role::Assistant)
        .flat_map(|message| message.tool_calls.iter().map(|call| call.id.as_str()))
        .collect();
    let answered = outcome.messages.iter().any(|message| {
        message.role == Role::Tool
            && message
                .tool_call_id
                .as_deref()
                .is_some_and(|id| call_ids.contains(&id))
            && text_of(message).contains("18")
    });
    if !answered {
        return Err("no tool message with the result 18 answers an assistant tool call".into());
    }

    let answer: Value = serde_json::from_str(outcome.content.trim())
        .map_err(|error| format!("content is not JSON ({error}): {:?}", outcome.content))?;
    if !answer["city"].is_string() {
        return Err(format!("answer.city is not a string: {answer}"));
    }
    if answer["temperature"].as_i64() != Some(18) {
        return Err(format!(
            "answer.temperature is not the integer 18: {answer}"
        ));
    }
    if !answer["summary"].is_string() {
        return Err(format!("answer.summary is not a string: {answer}"));
    }
    Ok(())
}
