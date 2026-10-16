//! The JSON the Swift bridge speaks, built and parsed in portable Rust so it
//! is tested on every OS. Nothing here touches FFI.

use koine_ai::schema::{self, Profile};
use koine_ai::{
    ChatRequest, Error, LoopError, ModelContentPart, ModelMessage, ResponseFormat, Role,
};
use serde::Deserialize;
use serde_json::{json, Value};

/// The request JSON for `koine_fm_respond`, and what building it left out.
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltRequest {
    pub json: Value,
    /// How many tools the request carries.
    pub tools_offered: usize,
    /// Tools the Apple profile cannot express: `(name, reason)`.
    pub skipped: Vec<(String, String)>,
}

fn text_of(message: &ModelMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|part| match part {
            ModelContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One earlier message as a `(role, text)` pair Foundation Models can take.
/// A tool call or result from an earlier turn is flattened to text: the
/// transcript Apple keeps is its own, so ours is a story, not a record.
/// Tool results read `[tool <name>] <result>`; an assistant message that made
/// calls says `[tool call <name>] <arguments>` for each.
fn flatten(messages: &[ModelMessage]) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let mut push = |role: &'static str, text: String| {
        if text.trim().is_empty() {
            return;
        }
        match out.last_mut() {
            // Consecutive entries of one role are merged.
            Some((last, existing)) if *last == role => {
                existing.push('\n');
                existing.push_str(&text);
            }
            _ => out.push((role, text)),
        }
    };
    for (index, message) in messages.iter().enumerate() {
        match message.role {
            Role::System => {}
            Role::User => push("user", text_of(message)),
            Role::Assistant => {
                push("assistant", text_of(message));
                for call in &message.tool_calls {
                    push(
                        "assistant",
                        format!("[tool call {}] {}", call.name, call.arguments),
                    );
                }
            }
            Role::Tool => {
                let name = message.name.clone().or_else(|| {
                    messages[..index]
                        .iter()
                        .flat_map(|m| m.tool_calls.iter())
                        .find(|call| Some(&call.id) == message.tool_call_id.as_ref())
                        .map(|call| call.name.clone())
                });
                push(
                    "assistant",
                    format!("[tool {}] {}", name.unwrap_or_default(), text_of(message)),
                );
            }
        }
    }
    out
}

/// Build the request JSON from a Koine request. System messages become the
/// instructions; the last user message is the prompt; what comes before it is
/// history, and anything after it (a tool exchange already in the request) is
/// flattened onto the end of the prompt. Tool schemas and the response schema
/// are lowered with [`Profile::AppleGuided`]; a tool that cannot be is left
/// out and reported in `skipped`.
pub fn build_request(request: &ChatRequest) -> Result<BuiltRequest, Error> {
    let last_user = request
        .messages
        .iter()
        .rposition(|message| message.role == Role::User)
        .ok_or_else(|| Error::Invalid("apple-on-device needs a user message".into()))?;
    let mut history = flatten(&request.messages[..last_user]);
    let mut prompt = text_of(&request.messages[last_user]);
    for (role, text) in flatten(&request.messages[last_user + 1..]) {
        prompt.push_str(&format!("\n[{role}] {text}"));
    }

    let mut instructions = request.system_text();
    let mut schema_json = Value::Null;
    match &request.response_format {
        Some(ResponseFormat::JsonSchema { name, schema, .. }) => {
            schema_json = schema::lower(schema, name, Profile::AppleGuided)
                .map_err(|error| Error::Invalid(format!("response schema `{name}`: {error}")))?;
        }
        Some(ResponseFormat::JsonObject) => {
            let line = "Answer with a JSON object.";
            instructions = Some(match instructions {
                Some(existing) => format!("{existing}\n\n{line}"),
                None => line.to_string(),
            });
        }
        Some(ResponseFormat::Text) | None => {}
    }

    let mut tools = Vec::new();
    let mut skipped = Vec::new();
    for tool in &request.tools {
        match schema::lower(&tool.input_schema, &tool.name, Profile::AppleGuided) {
            Ok(parameters) => tools.push(json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": parameters,
            })),
            Err(error) => skipped.push((tool.name.clone(), error.to_string())),
        }
    }

    let history: Vec<Value> = history
        .drain(..)
        .map(|(role, text)| json!({ "role": role, "text": text }))
        .collect();
    let mut options = serde_json::Map::new();
    if let Some(temperature) = request.temperature {
        options.insert("temperature".into(), json!(f64::from(temperature)));
    }
    if request.max_tokens > 0 {
        options.insert("max_tokens".into(), json!(request.max_tokens));
    }
    Ok(BuiltRequest {
        tools_offered: tools.len(),
        json: json!({
            "instructions": instructions,
            "history": history,
            "prompt": prompt,
            "tools": tools,
            "schema": schema_json,
            "options": options,
        }),
        skipped,
    })
}

/// What `koine_fm_availability` returns.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Availability {
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub capabilities: FmCapabilities,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FmCapabilities {
    #[serde(default)]
    pub tool_calling: bool,
    #[serde(default)]
    pub guided_generation: bool,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub vision: bool,
}

impl Availability {
    pub fn parse(json: &str) -> Self {
        serde_json::from_str(json).unwrap_or_default()
    }

    pub fn unavailable(reason: &str) -> Self {
        Self {
            available: false,
            reason: Some(reason.to_string()),
            capabilities: FmCapabilities::default(),
        }
    }
}

/// One event of a turn, as the bridge sends it.
#[derive(Debug, Clone, PartialEq)]
pub enum FmEvent {
    Text(String),
    Done {
        content: String,
        usage: Option<Value>,
    },
    Error {
        kind: String,
        message: String,
    },
}

impl FmEvent {
    /// Whether this is the last event of the turn.
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::Text(_))
    }

    pub fn parse(json: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(json).map_err(|e| format!("{e}: {json}"))?;
        let text = |key: &str| value[key].as_str().unwrap_or_default().to_string();
        match value["type"].as_str() {
            Some("text") => Ok(Self::Text(text("text"))),
            Some("done") => Ok(Self::Done {
                content: text("content"),
                usage: value.get("usage").filter(|u| !u.is_null()).cloned(),
            }),
            Some("error") => Ok(Self::Error {
                kind: text("kind"),
                message: text("message"),
            }),
            other => Err(format!("unknown event type {other:?}: {json}")),
        }
    }
}

/// Whether `json` is a terminal event. Cheap and total: the FFI callback uses
/// it to decide when to release the channel, and an unparseable payload is
/// treated as terminal so the turn cannot hang.
pub fn is_terminal_json(json: &str) -> bool {
    FmEvent::parse(json)
        .map(|event| event.is_terminal())
        .unwrap_or(true)
}

/// A bridged error as the loop's error. A refusal is `Exhausted` with an
/// `apple-on-device refused:` prefix rather than a new `Error` variant: the
/// loop already treats `Exhausted` as final, and no caller branches on it.
pub fn map_error(provider: &str, kind: &str, message: &str) -> LoopError {
    match kind {
        "unavailable" => LoopError::Transport(Error::Unreachable {
            provider: provider.into(),
            message: message.into(),
        }),
        "rate_limited" => LoopError::Transport(Error::RateLimited {
            provider: provider.into(),
            retry_after_secs: None,
        }),
        "guardrail" | "refusal" => LoopError::Exhausted(format!("{provider} refused: {message}")),
        "context" => LoopError::Exhausted(format!("{provider} context window exceeded: {message}")),
        "cancelled" => LoopError::Exhausted(format!("{provider} turn was cancelled")),
        _ => LoopError::Transport(Error::Provider {
            provider: provider.into(),
            status: None,
            message: message.into(),
        }),
    }
}

/// `{id, name, arguments}` from a tool callback; `arguments` arrives as the
/// JSON text of an object. Empty or non-object arguments become `{}`.
pub fn parse_tool_call(json: &str) -> Result<(String, String, Value, String), String> {
    let value: Value = serde_json::from_str(json).map_err(|e| format!("{e}: {json}"))?;
    let id = value["id"].as_str().unwrap_or_default().to_string();
    let name = value["name"]
        .as_str()
        .ok_or_else(|| format!("tool call without a name: {json}"))?
        .to_string();
    let raw = match &value["arguments"] {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    let arguments = match serde_json::from_str::<Value>(&raw) {
        Ok(object @ Value::Object(_)) => object,
        _ => json!({}),
    };
    Ok((id, name, arguments, raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use koine_ai::conformance;
    use koine_ai::{ModelToolCall, ToolDefinition};

    #[test]
    fn the_weather_request_becomes_instructions_prompt_tools_and_schema() {
        let built = build_request(&conformance::request("apple-on-device")).unwrap();
        let json = &built.json;
        assert_eq!(
            json["instructions"],
            "Use the weather_lookup tool for weather questions."
        );
        assert_eq!(
            json["prompt"],
            "What is the temperature in Paris in celsius?"
        );
        assert_eq!(json["history"], json!([]));
        assert_eq!(built.tools_offered, 2);
        assert!(built.skipped.is_empty());
        let weather = &json["tools"][0];
        assert_eq!(weather["name"], "weather_lookup");
        assert_eq!(weather["parameters"]["additionalProperties"], false);
        assert_eq!(weather["parameters"]["title"], "weather_lookup");
        assert_eq!(json["schema"]["title"], "answer");
        assert_eq!(
            json["schema"]["required"],
            json!(["city", "summary", "temperature"])
        );
        assert_eq!(json["options"]["max_tokens"], 2048);
        assert!(json["options"].get("temperature").is_none());
    }

    #[test]
    fn earlier_turns_become_history_and_tool_exchanges_are_flattened() {
        let mut assistant = ModelMessage::text(Role::Assistant, "Looking.");
        assistant.tool_calls = vec![ModelToolCall {
            id: "c1".into(),
            name: "weather_lookup".into(),
            arguments: r#"{"city":"Paris"}"#.into(),
        }];
        let tool = ModelMessage {
            role: Role::Tool,
            content: vec![ModelContentPart::Text { text: "18".into() }],
            name: None,
            tool_call_id: Some("c1".into()),
            tool_calls: vec![],
        };
        let request = ChatRequest {
            model: "apple-on-device".into(),
            temperature: Some(0.5),
            messages: vec![
                ModelMessage::text(Role::System, "Be terse."),
                ModelMessage::text(Role::User, "Paris?"),
                assistant,
                tool,
                ModelMessage::text(Role::Assistant, "18 C."),
                ModelMessage::text(Role::User, "And Rome?"),
            ],
            response_format: Some(ResponseFormat::JsonObject),
            ..Default::default()
        };
        let json = build_request(&request).unwrap().json;
        assert_eq!(
            json["instructions"],
            "Be terse.\n\nAnswer with a JSON object."
        );
        assert_eq!(json["prompt"], "And Rome?");
        assert_eq!(json["schema"], Value::Null);
        assert_eq!(json["options"]["temperature"], 0.5);
        let history = json["history"].as_array().unwrap();
        assert_eq!(history.len(), 2, "assistant entries merge: {history:?}");
        assert_eq!(history[0], json!({"role": "user", "text": "Paris?"}));
        assert_eq!(history[1]["role"], "assistant");
        let text = history[1]["text"].as_str().unwrap();
        assert!(text.contains(r#"[tool call weather_lookup] {"city":"Paris"}"#));
        assert!(text.contains("[tool weather_lookup] 18"));
        assert!(text.ends_with("18 C."));
    }

    #[test]
    fn a_tool_apple_cannot_express_is_left_out_and_reported() {
        let mut request = conformance::request("apple-on-device");
        request.tools.push(ToolDefinition::new(
            "anything",
            "Takes any value.",
            json!({"type": "object", "properties": {"args": {"type": "object"}}, "required": ["args"]}),
        ));
        let built = build_request(&request).unwrap();
        assert_eq!(built.tools_offered, 2);
        assert_eq!(built.skipped.len(), 1);
        assert_eq!(built.skipped[0].0, "anything");
        assert!(built.skipped[0].1.contains("args"), "{:?}", built.skipped);
    }

    #[test]
    fn a_request_without_a_user_message_is_refused() {
        let request = ChatRequest {
            messages: vec![ModelMessage::text(Role::System, "x")],
            ..Default::default()
        };
        assert!(matches!(build_request(&request), Err(Error::Invalid(_))));
    }

    #[test]
    fn events_parse_and_terminals_are_recognised() {
        assert_eq!(
            FmEvent::parse(r#"{"type":"text","text":"hi"}"#).unwrap(),
            FmEvent::Text("hi".into())
        );
        let done = FmEvent::parse(r#"{"type":"done","content":"x","usage":{"n":1}}"#).unwrap();
        assert_eq!(
            done,
            FmEvent::Done {
                content: "x".into(),
                usage: Some(json!({"n": 1}))
            }
        );
        assert!(done.is_terminal());
        let error =
            FmEvent::parse(r#"{"type":"error","kind":"guardrail","message":"no"}"#).unwrap();
        assert!(error.is_terminal());
        assert!(!FmEvent::Text(String::new()).is_terminal());
        assert!(FmEvent::parse(r#"{"type":"what"}"#).is_err());
        assert!(is_terminal_json("not json"));
        assert!(!is_terminal_json(r#"{"type":"text","text":"a"}"#));
    }

    #[test]
    fn error_kinds_map_to_the_loops_errors() {
        use LoopError::*;
        assert!(matches!(
            map_error("p", "unavailable", "m"),
            Transport(Error::Unreachable { .. })
        ));
        assert!(matches!(
            map_error("p", "rate_limited", "m"),
            Transport(Error::RateLimited { .. })
        ));
        for kind in ["guardrail", "refusal"] {
            match map_error("p", kind, "unsafe") {
                Exhausted(text) => assert_eq!(text, "p refused: unsafe"),
                other => panic!("{other:?}"),
            }
        }
        assert!(matches!(map_error("p", "context", "m"), Exhausted(_)));
        assert!(matches!(map_error("p", "cancelled", "m"), Exhausted(_)));
        assert!(matches!(
            map_error("p", "other", "m"),
            Transport(Error::Provider { .. })
        ));
    }

    #[test]
    fn tool_calls_parse_with_empty_arguments_as_an_object() {
        let (id, name, args, raw) =
            parse_tool_call(r#"{"id":"u","name":"deny_me","arguments":"{}"}"#).unwrap();
        assert_eq!(
            (id.as_str(), name.as_str(), raw.as_str()),
            ("u", "deny_me", "{}")
        );
        assert_eq!(args, json!({}));
        let (_, _, args, _) = parse_tool_call(r#"{"id":"u","name":"t","arguments":""}"#).unwrap();
        assert_eq!(args, json!({}));
        let (_, _, args, _) =
            parse_tool_call(r#"{"id":"u","name":"t","arguments":"{\"city\":\"Paris\"}"}"#).unwrap();
        assert_eq!(args["city"], "Paris");
        assert!(parse_tool_call(r#"{"id":"u"}"#).is_err());
    }

    #[test]
    fn availability_parses_and_defaults_to_unavailable() {
        let a = Availability::parse(
            r#"{"available":true,"capabilities":{"toolCalling":true,"guidedGeneration":true,"reasoning":false,"vision":false}}"#,
        );
        assert!(a.available && a.capabilities.tool_calling && a.capabilities.guided_generation);
        let b = Availability::parse(r#"{"available":false,"reason":"not enabled"}"#);
        assert!(!b.available);
        assert_eq!(b.reason.as_deref(), Some("not enabled"));
        assert!(!Availability::parse("garbage").available);
    }
}
