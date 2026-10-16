//! Desk triage over the real on-device model. Runs only with
//! `KOINE_TEST_APPLE_FM=1` on macOS with Apple Intelligence:
//!
//! `KOINE_TEST_APPLE_FM=1 cargo test -p koine-ai-apple --test triage -- --nocapture`
//!
//! The model's choice is printed, not asserted: it must be one of the enum.

use std::time::Instant;

use koine_ai::agent_loop::{self, NoObserver};
use koine_ai::categories::{parse_triage, triage_request};
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

#[tokio::test]
async fn the_on_device_model_triages_a_request_into_the_enum() {
    if !enabled() {
        return;
    }
    let ids: Vec<String> = [
        "agent",
        "agent.frontier",
        "research.summarize",
        "writing.rewrite",
    ]
    .iter()
    .map(|id| id.to_string())
    .collect();
    for prompt in [
        "What is the capital of France?",
        "Summarise the abstract of this paper in two sentences.",
        "Review this proof for errors: every bounded sequence of reals converges, since it is bounded.",
    ] {
        let started = Instant::now();
        let outcome = agent_loop::run(
            &AppleFoundationModels::new(),
            triage_request("apple-on-device", prompt, &ids),
            None,
            &NoObserver,
            1,
        )
        .await
        .expect("triage ran");
        let chosen = parse_triage(&outcome.content, &ids);
        eprintln!(
            "triage {:?} -> {} (raw {:?}) in {:?}",
            prompt,
            chosen.as_deref().unwrap_or("NONE"),
            outcome.content,
            started.elapsed()
        );
        assert!(chosen.is_some(), "not one of {ids:?}: {:?}", outcome.content);
    }
}
