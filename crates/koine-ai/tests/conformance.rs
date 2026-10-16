//! The tool-loop conformance fixture over scripted providers in each loop
//! mode, and over a live OpenAI-compatible server when one is configured.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use koine_ai::agent_loop::{self, AdapterToolRunner, NoObserver};
use koine_ai::catalogue::{Capabilities, Dialect, LLAMA_CPP_ID};
use koine_ai::conformance::{self, FixtureTools};
use koine_ai::{
    ChatRequest, EventStream, InferenceProvider, LoopError, LoopObserver, LoopOutcome,
    ModelContentPart, ModelMessage, ModelSummary, ModelToolCall, OpenAiCompatibleClient,
    ResponseFormat, Result, Role, StreamEvent, ToolCall, ToolLoop, ToolOutcome, ToolRunner,
};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

struct Scripted {
    rounds: Mutex<VecDeque<Vec<StreamEvent>>>,
    requests: Mutex<Vec<ChatRequest>>,
    structured_with_tools: bool,
}

impl Scripted {
    fn new(rounds: Vec<Vec<StreamEvent>>, structured_with_tools: bool) -> Self {
        Self {
            rounds: Mutex::new(rounds.into()),
            requests: Mutex::new(vec![]),
            structured_with_tools,
        }
    }
}

#[async_trait]
impl InferenceProvider for Scripted {
    fn provider_id(&self) -> &str {
        "scripted"
    }

    fn endpoint_id(&self) -> &str {
        "conformance"
    }

    async fn models(&self) -> Result<Vec<ModelSummary>> {
        Ok(vec![])
    }

    async fn capabilities(&self, _model: &str) -> Capabilities {
        Capabilities {
            structured_with_tools: self.structured_with_tools,
            ..Capabilities::default()
        }
    }

    async fn stream(&self, request: ChatRequest) -> Result<EventStream> {
        self.requests.lock().unwrap().push(request);
        let events = self.rounds.lock().unwrap().pop_front().unwrap();
        let (sender, receiver) = mpsc::channel(events.len().max(1));
        for event in events {
            sender.try_send(Ok(event)).unwrap();
        }
        Ok(ReceiverStream::new(receiver))
    }
}

fn call_round(name: &str, arguments: Value) -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolCallDelta {
            index: 0,
            id: Some(format!("call-{name}")),
            name: Some(name.into()),
            arguments: arguments.to_string(),
        },
        StreamEvent::Done {
            finish_reason: Some("tool_calls".into()),
        },
    ]
}

fn text_round(text: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::Token { text: text.into() },
        StreamEvent::Done {
            finish_reason: Some("stop".into()),
        },
    ]
}

const ANSWER: &str = r#"{"city":"Paris","temperature":18,"summary":"Mild."}"#;

fn paris_call() -> Vec<StreamEvent> {
    call_round("weather_lookup", json!({ "city": "Paris", "unit": "c" }))
}

fn tool_text(message: &ModelMessage) -> String {
    match &message.content[0] {
        ModelContentPart::Text { text } => text.clone(),
        other => panic!("unexpected part {other:?}"),
    }
}

#[tokio::test]
async fn caller_loop_holds_the_schema_for_a_last_round() {
    let provider = Scripted::new(
        vec![
            paris_call(),
            text_round("It is 18°C in Paris."),
            text_round(ANSWER),
        ],
        false,
    );
    let tools = FixtureTools::default();
    let runner = AdapterToolRunner {
        adapter: &tools,
        observer: &NoObserver,
    };
    let outcome = agent_loop::run(
        &provider,
        conformance::request("m"),
        Some(&runner),
        &NoObserver,
        4,
    )
    .await
    .unwrap();
    conformance::check_weather_turn(&outcome, &tools).unwrap();

    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].response_format.is_none());
    assert!(requests[1].response_format.is_none());
    assert!(!requests[0].tools.is_empty() && !requests[1].tools.is_empty());
    assert!(requests[2].tools.is_empty());
    assert!(matches!(
        requests[2].response_format,
        Some(ResponseFormat::JsonSchema { .. })
    ));
    let last = requests[2].messages.last().unwrap();
    assert_eq!(last.role, Role::User);
    let previous = &requests[2].messages[requests[2].messages.len() - 2];
    assert_eq!(previous.role, Role::Assistant);
    assert_eq!(tool_text(previous), "It is 18°C in Paris.");
}

#[tokio::test]
async fn caller_loop_sends_schema_and_tools_together_when_supported() {
    let provider = Scripted::new(vec![paris_call(), text_round(ANSWER)], true);
    let tools = FixtureTools::default();
    let runner = AdapterToolRunner {
        adapter: &tools,
        observer: &NoObserver,
    };
    let outcome = agent_loop::run(
        &provider,
        conformance::request("m"),
        Some(&runner),
        &NoObserver,
        4,
    )
    .await
    .unwrap();
    conformance::check_weather_turn(&outcome, &tools).unwrap();
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].response_format.is_some());
    assert!(!requests[0].tools.is_empty());
}

#[tokio::test]
async fn caller_loop_tells_the_model_a_refusal_is_ok_false() {
    let provider = Scripted::new(
        vec![
            call_round("deny_me", json!({})),
            text_round("It was refused."),
            text_round(ANSWER),
        ],
        false,
    );
    let tools = FixtureTools::default();
    let runner = AdapterToolRunner {
        adapter: &tools,
        observer: &NoObserver,
    };
    agent_loop::run(
        &provider,
        conformance::request("m"),
        Some(&runner),
        &NoObserver,
        4,
    )
    .await
    .unwrap();
    let requests = provider.requests.lock().unwrap();
    let tool_message = requests[1]
        .messages
        .iter()
        .find(|message| message.role == Role::Tool)
        .unwrap();
    let seen: Value = serde_json::from_str(&tool_text(tool_message)).unwrap();
    assert_eq!(seen["ok"], json!(false));
    assert_eq!(seen["code"], json!("denied"));
}

/// A runtime that runs its own loop: calls the runner twice for Paris.
struct SelfLooping {
    outcomes: Mutex<Vec<ToolOutcome>>,
}

#[async_trait]
impl InferenceProvider for SelfLooping {
    fn provider_id(&self) -> &str {
        "self-looping"
    }

    fn endpoint_id(&self) -> &str {
        "conformance"
    }

    async fn models(&self) -> Result<Vec<ModelSummary>> {
        Ok(vec![])
    }

    async fn stream(&self, _request: ChatRequest) -> Result<EventStream> {
        panic!("a provider-run turn never streams rounds");
    }

    fn tool_loop(&self) -> ToolLoop {
        ToolLoop::Provider
    }

    async fn run_turn(
        &self,
        mut request: ChatRequest,
        tools: &dyn ToolRunner,
        _observer: &dyn LoopObserver,
    ) -> std::result::Result<LoopOutcome, LoopError> {
        assert!(
            request.response_format.is_some(),
            "schema left to the provider"
        );
        for n in 0..2 {
            let arguments = json!({ "city": "Paris", "unit": "c" });
            let id = format!("call-{n}");
            let outcome = tools
                .run(ToolCall {
                    id: id.clone(),
                    name: "weather_lookup".into(),
                    arguments: arguments.clone(),
                    raw: arguments.to_string(),
                })
                .await
                .map_err(LoopError::Tool)?;
            request.messages.push(ModelMessage {
                role: Role::Assistant,
                content: vec![],
                name: None,
                tool_call_id: None,
                tool_calls: vec![ModelToolCall {
                    id: id.clone(),
                    name: "weather_lookup".into(),
                    arguments: arguments.to_string(),
                }],
            });
            request.messages.push(ModelMessage {
                role: Role::Tool,
                content: vec![ModelContentPart::Text {
                    text: outcome.model_text(),
                }],
                name: Some("weather_lookup".into()),
                tool_call_id: Some(id),
                tool_calls: vec![],
            });
            self.outcomes.lock().unwrap().push(outcome);
        }
        Ok(LoopOutcome {
            content: ANSWER.into(),
            messages: request.messages,
            ..Default::default()
        })
    }
}

#[tokio::test]
async fn provider_loop_runs_the_turn_and_the_limit_holds() {
    let provider = SelfLooping {
        outcomes: Mutex::new(vec![]),
    };
    let tools = FixtureTools::default();
    let runner = AdapterToolRunner {
        adapter: &tools,
        observer: &NoObserver,
    };
    let outcome = agent_loop::run(
        &provider,
        conformance::request("m"),
        Some(&runner),
        &NoObserver,
        1,
    )
    .await
    .unwrap();
    conformance::check_weather_turn(&outcome, &tools).unwrap();
    let outcomes = provider.outcomes.lock().unwrap();
    assert!(matches!(outcomes[0], ToolOutcome::Ok { .. }));
    assert!(matches!(
        &outcomes[1],
        ToolOutcome::Refused { code, .. } if code == "tool_limit"
    ));
    assert_eq!(tools.weather_calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn provider_loop_without_a_runner_refuses_with_no_tools() {
    let provider = SelfLooping {
        outcomes: Mutex::new(vec![]),
    };
    agent_loop::run(&provider, conformance::request("m"), None, &NoObserver, 4)
        .await
        .unwrap();
    let outcomes = provider.outcomes.lock().unwrap();
    assert!(outcomes
        .iter()
        .all(|o| matches!(o, ToolOutcome::Refused { code, .. } if code == "no_tools")));
}

#[tokio::test]
async fn live_openai_compatible_server() {
    let Ok(url) = std::env::var("KOINE_TEST_OPENAI_COMPAT_URL") else {
        eprintln!("KOINE_TEST_OPENAI_COMPAT_URL not set; skipping the live conformance run");
        return;
    };
    let model = std::env::var("KOINE_TEST_MODEL").expect("KOINE_TEST_MODEL");
    let client = OpenAiCompatibleClient::new(Dialect::Generic, url, None, "conformance").unwrap();
    let tools = FixtureTools::default();
    let runner = AdapterToolRunner {
        adapter: &tools,
        observer: &NoObserver,
    };
    let outcome = agent_loop::run(
        &client,
        conformance::request(&model),
        Some(&runner),
        &NoObserver,
        4,
    )
    .await
    .unwrap_or_else(|error| panic!("loop failed: {error:?}"));
    if let Err(message) = conformance::check_weather_turn(&outcome, &tools) {
        panic!(
            "{message}\ncontent: {:?}\ntranscript: {:#?}",
            outcome.content, outcome.messages
        );
    }
    eprintln!("live conformance passed; content: {}", outcome.content);
}

/// llama-server, built the way the registry builds the `llama-cpp` entry.
/// The model is `KOINE_TEST_MODEL` or the first one `/v1/models` lists.
#[tokio::test]
async fn live_llama_cpp() {
    let Ok(url) = std::env::var("KOINE_TEST_LLAMACPP_URL") else {
        eprintln!("KOINE_TEST_LLAMACPP_URL not set; skipping the live llama.cpp run");
        return;
    };
    let client = OpenAiCompatibleClient::new(Dialect::LlamaCpp, url, None, "conformance")
        .unwrap()
        .with_provider_id(LLAMA_CPP_ID);
    let health = client.health().await.unwrap();
    eprintln!("health: {:?} {}", health.state, health.detail);
    let model = match std::env::var("KOINE_TEST_MODEL") {
        Ok(model) => model,
        Err(_) => client.models().await.unwrap()[0].id.clone(),
    };
    let tools = FixtureTools::default();
    let runner = AdapterToolRunner {
        adapter: &tools,
        observer: &NoObserver,
    };

    let outcome = agent_loop::run(
        &client,
        conformance::request(&model),
        Some(&runner),
        &NoObserver,
        4,
    )
    .await
    .unwrap_or_else(|error| panic!("loop failed: {error:?}"));
    if let Err(message) = conformance::check_weather_turn(&outcome, &tools) {
        panic!(
            "{message}\ncontent: {:?}\ntranscript: {:#?}",
            outcome.content, outcome.messages
        );
    }
    eprintln!(
        "live llama.cpp weather turn passed; content: {}",
        outcome.content
    );

    // The refused tool: the model must see the refusal and still answer.
    let denied_tools = FixtureTools::default();
    let denied_runner = AdapterToolRunner {
        adapter: &denied_tools,
        observer: &NoObserver,
    };
    let mut request = conformance::request(&model);
    request.response_format = None;
    request.messages = vec![
        ModelMessage::text(Role::System, "Use tools when asked."),
        ModelMessage::text(
            Role::User,
            "Call the deny_me tool, then tell me in one sentence what happened.",
        ),
    ];
    let outcome = agent_loop::run(&client, request, Some(&denied_runner), &NoObserver, 4)
        .await
        .unwrap_or_else(|error| panic!("deny loop failed: {error:?}"));
    let saw_refusal = outcome.messages.iter().any(|message| {
        message.role == Role::Tool
            && message.content.iter().any(
                |part| matches!(part, ModelContentPart::Text { text } if text.contains("denied")),
            )
    });
    assert!(
        saw_refusal && !outcome.content.trim().is_empty(),
        "content: {:?}\ntranscript: {:#?}",
        outcome.content,
        outcome.messages
    );
    eprintln!(
        "live llama.cpp deny_me passed; content: {}",
        outcome.content
    );
}
