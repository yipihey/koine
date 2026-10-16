//! The tool-loop conformance fixture over the real on-device model. Runs only
//! with `KOINE_TEST_APPLE_FM=1` on macOS with Apple Intelligence:
//!
//! `KOINE_TEST_APPLE_FM=1 cargo test -p koine-ai-apple -- --nocapture --test-threads=1`

use std::time::{Duration, Instant};

use koine_ai::agent_loop::{self, AdapterToolRunner, NoObserver};
use koine_ai::conformance::{self, FixtureTools};
use koine_ai::{ChatRequest, ModelContentPart, ModelMessage, ResponseFormat, Role, ToolDefinition};
use koine_ai_apple::AppleFoundationModels;

fn enabled() -> bool {
    if cfg!(target_os = "macos") && std::env::var("KOINE_TEST_APPLE_FM").as_deref() == Ok("1") {
        assert!(
            koine_ai_apple::available(),
            "KOINE_TEST_APPLE_FM=1 but Apple Intelligence is unavailable"
        );
        return true;
    }
    eprintln!("skipped: set KOINE_TEST_APPLE_FM=1 on a Mac with Apple Intelligence");
    false
}

fn transcript(messages: &[ModelMessage]) -> String {
    messages
        .iter()
        .map(|m| {
            let text: String = m
                .content
                .iter()
                .filter_map(|p| match p {
                    ModelContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            format!("{:?} {:?} calls={:?}: {text}", m.role, m.name, m.tool_calls)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn the_weather_turn_calls_the_tool_and_answers_in_the_schema() {
    if !enabled() {
        return;
    }
    let fixture = FixtureTools::default();
    let started = Instant::now();
    let outcome = agent_loop::run(
        &AppleFoundationModels::new(),
        conformance::request("apple-on-device"),
        Some(&AdapterToolRunner {
            adapter: &fixture,
            observer: &NoObserver,
        }),
        &NoObserver,
        4,
    )
    .await
    .unwrap_or_else(|e| panic!("turn failed: {e:?}"));
    eprintln!(
        "weather turn: {:?}\ncontent: {}\n{}",
        started.elapsed(),
        outcome.content,
        transcript(&outcome.messages)
    );
    conformance::check_weather_turn(&outcome, &fixture).unwrap();
}

fn deny_request(rounds_note: &str) -> ChatRequest {
    let deny: ToolDefinition = conformance::tools()
        .into_iter()
        .find(|t| t.name == "deny_me")
        .unwrap();
    ChatRequest {
        model: "apple-on-device".into(),
        messages: vec![
            ModelMessage::text(Role::System, "You must use tools when asked to."),
            ModelMessage::text(
                Role::User,
                format!("Call the deny_me tool, then tell me what it said. {rounds_note}"),
            ),
        ],
        tools: vec![deny],
        response_format: None,
        ..Default::default()
    }
}

#[tokio::test]
async fn a_refused_tool_reaches_the_model_as_a_result() {
    if !enabled() {
        return;
    }
    let fixture = FixtureTools::default();
    let started = Instant::now();
    let outcome = agent_loop::run(
        &AppleFoundationModels::new(),
        deny_request(""),
        Some(&AdapterToolRunner {
            adapter: &fixture,
            observer: &NoObserver,
        }),
        &NoObserver,
        4,
    )
    .await
    .unwrap_or_else(|e| panic!("turn failed: {e:?}"));
    eprintln!(
        "deny turn: {:?}\n{}\nANSWER: {}",
        started.elapsed(),
        transcript(&outcome.messages),
        outcome.content
    );
    let called = outcome
        .messages
        .iter()
        .any(|m| m.role == Role::Tool && m.name.as_deref() == Some("deny_me"));
    assert!(
        called,
        "the model did not call deny_me; transcript above, answer: {}",
        outcome.content
    );
    assert!(!outcome.content.trim().is_empty());
}

#[tokio::test]
async fn at_the_limit_a_call_gets_tool_limit_and_the_turn_still_answers() {
    if !enabled() {
        return;
    }
    let fixture = FixtureTools::default();
    let outcome = agent_loop::run(
        &AppleFoundationModels::new(),
        deny_request(""),
        Some(&AdapterToolRunner {
            adapter: &fixture,
            observer: &NoObserver,
        }),
        &NoObserver,
        0,
    )
    .await
    .unwrap_or_else(|e| panic!("turn failed: {e:?}"));
    eprintln!(
        "limit turn:\n{}\nANSWER: {}",
        transcript(&outcome.messages),
        outcome.content
    );
    let results: Vec<String> = outcome
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .flat_map(|m| m.content.iter())
        .filter_map(|p| match p {
            ModelContentPart::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        results.iter().all(|r| r.contains("tool_limit")),
        "a call ran past the limit: {results:?}"
    );
    if results.is_empty() {
        eprintln!("note: the model made no call at all with a limit of 0");
    }
    assert!(!outcome.content.trim().is_empty(), "no answer");
}

#[tokio::test]
async fn a_plain_stream_without_tools_returns_text() {
    if !enabled() {
        return;
    }
    use koine_ai::InferenceProvider;
    let request = ChatRequest {
        model: "apple-on-device".into(),
        messages: vec![ModelMessage::text(
            Role::User,
            "Suggest a title of at most five words for a chat about the weather in Paris.",
        )],
        ..Default::default()
    };
    let completion = AppleFoundationModels::new()
        .complete(request)
        .await
        .unwrap();
    eprintln!("stream: {:?}", completion.content);
    assert!(!completion.content.trim().is_empty());
}

#[tokio::test]
async fn dropping_a_turn_midway_is_safe_and_the_next_turn_works() {
    if !enabled() {
        return;
    }
    let provider = AppleFoundationModels::new();
    for _ in 0..3 {
        let fixture = FixtureTools::default();
        let runner = AdapterToolRunner {
            adapter: &fixture,
            observer: &NoObserver,
        };
        let cut = tokio::time::timeout(
            Duration::from_millis(50),
            agent_loop::run(
                &provider,
                conformance::request("apple-on-device"),
                Some(&runner),
                &NoObserver,
                4,
            ),
        )
        .await;
        assert!(cut.is_err(), "the turn finished inside 50 ms");
    }
    // Let any cancelled turns deliver their last event.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let fixture = FixtureTools::default();
    let outcome = agent_loop::run(
        &provider,
        conformance::request("apple-on-device"),
        Some(&AdapterToolRunner {
            adapter: &fixture,
            observer: &NoObserver,
        }),
        &NoObserver,
        4,
    )
    .await
    .unwrap_or_else(|e| panic!("turn after cancellation failed: {e:?}"));
    conformance::check_weather_turn(&outcome, &fixture).unwrap();
}

#[test]
fn the_schema_format_is_still_a_json_schema() {
    // Guards the fixture this file leans on.
    assert!(matches!(
        conformance::answer_schema(),
        ResponseFormat::JsonSchema { .. }
    ));
}
