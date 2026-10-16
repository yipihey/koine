//! A queued turn with a response schema, through `AiStore` and
//! `AiTaskExecutor` over the real on-device model. Runs only with
//! `KOINE_TEST_APPLE_FM=1` on macOS with Apple Intelligence.

use std::sync::Arc;

use koine_ai::conformance;
use koine_ai_apple::AppleFoundationModels;

#[tokio::test]
async fn a_queued_turn_with_a_schema_is_answered_in_json() {
    if !(cfg!(target_os = "macos") && std::env::var("KOINE_TEST_APPLE_FM").as_deref() == Ok("1")) {
        eprintln!("skipped: set KOINE_TEST_APPLE_FM=1 on a Mac with Apple Intelligence");
        return;
    }
    assert!(koine_ai_apple::available());
    let started = std::time::Instant::now();
    let (answer, calls) = conformance::run_structured_task(
        Arc::new(AppleFoundationModels::new()),
        "apple-on-device",
        true,
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    eprintln!(
        "apple structured task: {:?}, {calls} call(s), {answer}",
        started.elapsed()
    );
    assert!(calls >= 1, "the tool was never called");
    assert!(
        answer["city"].is_string() && answer["summary"].is_string(),
        "{answer}"
    );
    assert_eq!(answer["temperature"].as_i64(), Some(18), "{answer}");
}
