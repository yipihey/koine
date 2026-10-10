//! The Tier A example runner (plan-auto-gui-and-self-docs.md, work package
//! G3; § Examples as tests).
//!
//! An example is written once, beside the method
//! (`#[impress_example(name, args, expect)]`), and read three ways: the
//! reference page prints it, a generated form's `state` is prefilled from
//! it, and *this module* runs it against whatever store the calling process
//! has installed and checks the result against `expect`, when the example
//! states one.
//!
//! This module owns none of the store setup — it has no dependency on
//! `impress-core` or any `*-service` crate, only on
//! [`crate::VerbDescriptor`] and [`crate::Example`], so it stays a leaf every
//! service crate can share. The caller (today, `impress-capabilities`'s
//! `tests/tier_a.rs`) installs a scratch store the way
//! `crates/impress-capabilities/tests/effects.rs` already does, then calls
//! [`run_all`]. P2's per-call store override will let this module open its
//! own scratch store per example instead of depending on whatever the
//! process installed once — until then, every Tier A example in one process
//! shares one store, so an example must not assume a *clean* store, only
//! that the store exists (queries return `[]`, not an error).
//!
//! A verb is Tier A-eligible when its declared `effects.reach` never leaves
//! the process (`Reach::is_external`) — the same predicate
//! `crates/impress-capabilities/tests/effects.rs`'s `is_headless` uses,
//! duplicated here rather than shared because that test crate depends on
//! this one, not the other way around. A verb outside that set (`needs_app`,
//! network, a device, a subprocess) keeps its exception in the effects
//! table with a `tier = b`/`needs app` reason; this runner never touches it.

use std::future::Future;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::descriptor::{Example, ExampleTier, Reach, VerbDescriptor};
use crate::descriptor_handle::{ExampleView, VerbHandle};
use crate::pipeline::CallerIdentity;

/// A verb is Tier A-eligible when nothing it declares leaves the process.
pub fn is_tier_a(v: &VerbDescriptor) -> bool {
    !v.effects.reach.iter().any(Reach::is_external)
}

/// How one example's run went.
#[derive(Debug)]
pub enum Outcome {
    /// The handler returned and, when the example named an `expect`, the
    /// result matched it.
    Passed { result: Value },
    /// The handler returned an error, timed out, or the result did not
    /// match `expect`. The message is what a test should print.
    Failed(String),
}

impl Outcome {
    pub fn is_pass(&self) -> bool {
        matches!(self, Outcome::Passed { .. })
    }
}

/// One example's outcome, identified by the verb and example name.
#[derive(Debug)]
pub struct ExampleResult {
    pub verb: &'static str,
    pub example: &'static str,
    pub outcome: Outcome,
}

/// The same outcome with owned names for a runtime provider descriptor.
#[derive(Debug)]
pub struct ProviderExampleResult {
    pub verb: String,
    pub example: String,
    pub outcome: Outcome,
}

/// How long a single example may run before this runner calls it a failure.
/// Generous: this is a correctness check, not the `budget_ms` performance
/// gate below.
const EXAMPLE_TIMEOUT: Duration = Duration::from_secs(30);

/// The multiplier this runner applies to a verb's declared `budget_ms`
/// before failing an example on it (D-P2). Chosen over a re-run: the
/// self-hosted runners are oversubscribed and shared
/// (`docs/self-hosted-runners.md`, impress-mac-3/-4 capped off) — a re-run
/// on the same loaded box would likely be slow again, costing wall-clock
/// twice for the same false positive, where a slack factor pays the cost
/// once and still catches a *real* regression (one that blows the budget by
/// more than the slack, not just crosses it under load). `3` is generous
/// enough that a verb whose budget was measured on a quiet machine does not
/// flake under a loaded CI runner, while a genuine several-times-over
/// regression still fails.
const BUDGET_SLACK_FACTOR: u32 = 3;

/// Run the A examples of one Tier A-eligible verb, checking `expect` where
/// given. B examples and verbs outside Tier A are never invoked here.
pub async fn run_verb(v: &'static VerbDescriptor) -> Vec<ExampleResult> {
    if !is_tier_a(v) {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(v.examples.len());
    let handle = VerbHandle::Linked(v);
    for ex in v.examples.iter().filter(|ex| ex.tier == ExampleTier::A) {
        let outcome = run_one(
            &handle,
            ExampleView {
                name: ex.name,
                args: ex.args,
                expect: ex.expect,
                tier: ex.tier,
            },
            CallerIdentity::system("tier-a"),
        )
        .await;
        out.push(ExampleResult {
            verb: v.name,
            example: ex.name,
            outcome,
        });
    }
    out
}

/// Execute one linked A example with its fixture-prepared arguments. The
/// isolated host installs its store before calling and supplies full output
/// schema validation; setup time is excluded from the example's budget.
/// A B example or externally reaching verb is refused without invocation.
pub async fn run_linked_example_with_args<V>(
    verb: &'static VerbDescriptor,
    example: &Example,
    args: Value,
    caller: CallerIdentity,
    validate_output: V,
) -> Outcome
where
    V: Fn(&Value, &Value) -> Result<(), String>,
{
    if example.tier != ExampleTier::A || !is_tier_a(verb) {
        return Outcome::Failed(format!(
            "`{}` example `{}` is not eligible for Tier A",
            verb.name, example.name
        ));
    }
    let handle = VerbHandle::Linked(verb);
    let schema = (verb.output_schema)();
    let call = crate::pipeline::invoke_handle(handle, crate::pipeline::Call::new(caller, args));
    check_invocation(
        verb.name,
        example.name,
        example.expect,
        verb.budget_ms,
        call,
        Some((&schema, &validate_output)),
    )
    .await
}

/// Run one explicitly selected Tier B example after its host prepared owned
/// fixture arguments. No default app or provider is chosen by this function.
pub async fn run_linked_tier_b_example_with_args<I, Fut, E, V>(
    verb: &'static VerbDescriptor,
    example: &Example,
    args: Value,
    caller: CallerIdentity,
    invoke: I,
    validate_output: V,
) -> Outcome
where
    I: FnOnce(Value, CallerIdentity) -> Fut,
    Fut: Future<Output = Result<Value, E>>,
    E: std::fmt::Display,
    V: Fn(&Value, &Value) -> Result<(), String>,
{
    if example.tier != ExampleTier::B {
        return Outcome::Failed(format!(
            "`{}` example `{}` is not Tier B",
            verb.name, example.name
        ));
    }
    let schema = (verb.output_schema)();
    check_invocation(
        verb.name,
        example.name,
        example.expect,
        verb.budget_ms,
        invoke(args, caller),
        Some((&schema, &validate_output)),
    )
    .await
}

/// Explicit linked Tier B entry. An isolated host supplies an invoker (for
/// example its owned HTTP app) and schema validator. Merely enumerating the
/// inventory cannot run this path. The caller identity is repeated for each
/// example and must be established by that host, not read from example args.
pub async fn run_linked_tier_b_examples<I, Fut, E, V>(
    verb: &'static VerbDescriptor,
    caller: CallerIdentity,
    mut invoke: I,
    validate_output: V,
) -> Vec<ExampleResult>
where
    I: FnMut(Value, CallerIdentity) -> Fut,
    Fut: Future<Output = Result<Value, E>>,
    E: std::fmt::Display,
    V: Fn(&Value, &Value) -> Result<(), String>,
{
    let mut out = Vec::new();
    for example in verb.examples.iter().filter(|ex| ex.tier == ExampleTier::B) {
        let outcome = run_linked_tier_b_example_with_args(
            verb,
            example,
            example.args_value(),
            caller.clone(),
            &mut invoke,
            &validate_output,
        )
        .await;
        out.push(ExampleResult {
            verb: verb.name,
            example: example.name,
            outcome,
        });
    }
    out
}

/// Explicit Tier B test entry for a registered provider. The caller must
/// install the provider transport and a scratch host store first. Enumerating
/// descriptors never calls this function; providers remain ineligible for
/// [`run_all`] and [`run_verb`]. System identity avoids interactive review
/// during this deliberate test run, while the ordinary pipeline still applies
/// strict arguments, liveness, tracing, auditing, and refusal handling.
pub async fn run_provider_examples(
    verb: &VerbHandle,
) -> Result<Vec<ProviderExampleResult>, &'static str> {
    if !matches!(verb, VerbHandle::Provider(_)) {
        return Err("Tier B provider example runner requires a provider handle");
    }
    let mut out = Vec::new();
    for example in verb.examples() {
        let outcome = run_one(
            verb,
            example,
            CallerIdentity::system("tier-b-provider-example"),
        )
        .await;
        out.push(ProviderExampleResult {
            verb: verb.name().to_owned(),
            example: example.name.to_owned(),
            outcome,
        });
    }
    Ok(out)
}

async fn run_one(v: &VerbHandle, ex: ExampleView<'_>, caller: CallerIdentity) -> Outcome {
    let args = ex.args_value();
    let call = crate::pipeline::invoke_handle(v.clone(), crate::pipeline::Call::new(caller, args));
    check_invocation(v.name(), ex.name, ex.expect, v.budget_ms(), call, None).await
}

type OutputCheck<'a> = dyn Fn(&Value, &Value) -> Result<(), String> + 'a;

async fn check_invocation<F, E>(
    name: &str,
    example: &str,
    expect: Option<&str>,
    budget_ms: Option<u64>,
    call: F,
    output_check: Option<(&Value, &OutputCheck<'_>)>,
) -> Outcome
where
    F: Future<Output = Result<Value, E>>,
    E: std::fmt::Display,
{
    let started = Instant::now();
    let outcome = match tokio::time::timeout(EXAMPLE_TIMEOUT, call).await {
        Err(_) => Outcome::Failed(format!(
            "`{}` example `{}` did not finish in {:?}",
            name, example, EXAMPLE_TIMEOUT
        )),
        Ok(Err(e)) => Outcome::Failed(format!("`{name}` example `{example}` failed: {e}")),
        Ok(Ok(result)) => {
            if let Some((schema, validate)) = output_check {
                // A structured refusal is the pipeline envelope, not the
                // method's declared successful output shape.
                if result.get("ok").and_then(Value::as_bool) != Some(false)
                    && validate(schema, &result).is_err()
                {
                    return Outcome::Failed(format!(
                        "`{name}` example `{example}` returned a value outside its output schema"
                    ));
                }
            }
            check_result(name, example, expect, result)
        }
    };
    // D-P2: a Tier A example whose verb declares `budget_ms` fails when it
    // blows that budget by more than `BUDGET_SLACK_FACTOR` — checked after
    // (never instead of) correctness, so a failing example is reported for
    // failing, not for running long while broken.
    if outcome.is_pass() {
        if let Some(budget_ms) = budget_ms {
            let elapsed_us = started.elapsed().as_micros() as u64;
            let ceiling_ms = budget_ms.saturating_mul(u64::from(BUDGET_SLACK_FACTOR));
            if elapsed_us > ceiling_ms.saturating_mul(1_000) {
                return Outcome::Failed(format!(
                    "`{}` example `{}` took {elapsed_us}us, over its {budget_ms}ms budget \
                     (even with {BUDGET_SLACK_FACTOR}x CI slack, ceiling {ceiling_ms}ms)",
                    name, example
                ));
            }
        }
    }
    outcome
}

fn check_result(name: &str, example: &str, expect: Option<&str>, result: Value) -> Outcome {
    // A documented negative example can intentionally assert a refusal.
    // An unexpected refusal must never pass merely because no expect was given.
    let expected_refusal = expect
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .is_some_and(|expected| expected.get("ok").and_then(Value::as_bool) == Some(false));
    if result.get("ok").and_then(Value::as_bool) == Some(false) && !expected_refusal {
        return Outcome::Failed(format!(
            "`{name}` example `{example}` refused: {}",
            result
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        ));
    }
    let Some(expect) = expect else {
        return Outcome::Passed { result };
    };
    match serde_json::from_str::<Value>(expect) {
        Err(e) => Outcome::Failed(format!(
            "`{name}` example `{example}` has an `expect` that is not JSON: {e}"
        )),
        Ok(expected) if shape_matches(&expected, &result) => Outcome::Passed { result },
        Ok(expected) => Outcome::Failed(format!(
            "`{name}` example `{example}`: expected shape {expected}, got {result}"
        )),
    }
}

/// Whether `actual` matches the shape `expected` describes. This is a
/// **subset** match, not equality: every key `expected` names must be
/// present in `actual` with an equal (recursively matching) value, but
/// `actual` may carry other keys `expected` does not mention (a
/// `revision`, a timestamp, a generated id). An array in `expected` must
/// match `actual`'s array elementwise and in length — an example that
/// wants "at least N" should assert on a scalar (`"count": 1`) instead.
/// Anything else (numbers, strings, bools, null) is compared for equality.
fn shape_matches(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::Object(e), Value::Object(a)) => e
            .iter()
            .all(|(k, ev)| a.get(k).is_some_and(|av| shape_matches(ev, av))),
        (Value::Array(e), Value::Array(a)) => {
            e.len() == a.len() && e.iter().zip(a).all(|(ev, av)| shape_matches(ev, av))
        }
        (e, a) => e == a,
    }
}

/// Run every example of every Tier A-eligible verb in `verbs`. A verb with
/// zero examples contributes nothing (its absence is a separate check —
/// `docs/verb-coverage.md`/the effects exception table, not this runner's).
pub async fn run_all(
    verbs: impl IntoIterator<Item = &'static VerbDescriptor>,
) -> Vec<ExampleResult> {
    let mut out = Vec::new();
    for v in verbs {
        out.extend(run_verb(v).await);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_matches_is_a_subset_check() {
        let expected = serde_json::json!({"ok": true, "n": 3});
        let actual = serde_json::json!({"ok": true, "n": 3, "revision": 7});
        assert!(shape_matches(&expected, &actual));

        let mismatched = serde_json::json!({"ok": false, "n": 3});
        assert!(!shape_matches(&mismatched, &actual));

        let missing_key = serde_json::json!({"ok": true, "missing": 1});
        assert!(!shape_matches(&missing_key, &actual));
    }

    #[test]
    fn shape_matches_arrays_elementwise() {
        let expected = serde_json::json!({"x": [1, 2, 3]});
        assert!(shape_matches(
            &expected,
            &serde_json::json!({"x": [1, 2, 3], "extra": true})
        ));
        assert!(!shape_matches(&expected, &serde_json::json!({"x": [1, 2]})));
    }

    use crate::descriptor::{Effects, Example, Safety, SafetyClass, Source};
    use crate::provider::{ProviderExample, ProviderStatus, ProviderVerb};
    use crate::ServiceFuture;
    use std::sync::Arc;

    fn schema() -> Value {
        serde_json::json!({})
    }

    fn instant_ok(_: Value) -> ServiceFuture {
        Box::pin(async { Ok(serde_json::json!({"ok": true})) })
    }

    fn slow_ok(_: Value) -> ServiceFuture {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(60)).await;
            Ok(serde_json::json!({"ok": true}))
        })
    }

    fn echo_args(args: Value) -> ServiceFuture {
        Box::pin(async move { Ok(args) })
    }

    fn budgeted_verb(
        name: &'static str,
        budget_ms: Option<u64>,
        handler: fn(Value) -> ServiceFuture,
    ) -> VerbDescriptor {
        VerbDescriptor {
            name,
            service: "tier-a-budget-test",
            method: "x",
            description: "d",
            input_schema: schema,
            output_schema: schema,
            safety: Safety {
                class: SafetyClass::ReadOnly,
                idempotent: true,
            },
            effects: Effects::NONE,
            since: "0.1.0",
            deprecated: None,
            aliases: &[],
            examples: &[Example {
                name: "default",
                args: "{}",
                expect: None,
                tier: ExampleTier::A,
            }],
            strict: false,
            budget_ms,
            replay_full: false,
            source: Source::Linked,
            handler,
        }
    }

    fn tiered_verb() -> &'static VerbDescriptor {
        static EXAMPLES: [Example; 2] = [
            Example {
                name: "headless",
                args: "{}",
                expect: Some(r#"{"ok":true}"#),
                tier: ExampleTier::A,
            },
            Example {
                name: "hosted",
                args: r#"{"label":"b"}"#,
                expect: Some(r#"{"ok":true,"label":"b"}"#),
                tier: ExampleTier::B,
            },
        ];
        static VERB: std::sync::OnceLock<VerbDescriptor> = std::sync::OnceLock::new();
        VERB.get_or_init(|| {
            let mut verb = budgeted_verb("tier-example-test_tiered", Some(1_000), instant_ok);
            verb.examples = &EXAMPLES;
            verb
        })
    }

    #[tokio::test]
    async fn tier_a_never_executes_tier_b_and_linked_b_uses_host_invoker() {
        let verb = tiered_verb();
        let headless = run_verb(verb).await;
        assert_eq!(headless.len(), 1);
        assert_eq!(headless[0].example, "headless");
        assert!(headless[0].outcome.is_pass());

        let invoked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls = invoked.clone();
        let hosted = run_linked_tier_b_examples(
            verb,
            CallerIdentity::App("fixture".into()),
            move |args, caller| {
                calls.lock().unwrap().push((args.clone(), caller));
                async move { Ok::<_, String>(serde_json::json!({"ok":true,"label":args["label"]})) }
            },
            |schema, value| {
                (schema.is_object() && value.is_object())
                    .then_some(())
                    .ok_or_else(|| "output is not an object".into())
            },
        )
        .await;
        assert_eq!(hosted.len(), 1);
        assert_eq!(hosted[0].example, "hosted");
        assert!(hosted[0].outcome.is_pass());
        assert_eq!(
            *invoked.lock().unwrap(),
            vec![(
                serde_json::json!({"label":"b"}),
                CallerIdentity::App("fixture".into())
            )]
        );
    }

    #[tokio::test]
    async fn supplied_output_validator_and_a_tier_a_guard_are_enforced() {
        let verb = tiered_verb();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        let refused = run_linked_tier_b_examples(
            verb,
            CallerIdentity::system("isolated-host"),
            move |_args, _caller| {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { Ok::<_, String>(serde_json::json!({"ok":true,"label":"b"})) }
            },
            |_schema, _value| Err("secret value from schema engine".into()),
        )
        .await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(matches!(&refused[0].outcome, Outcome::Failed(message)
            if message.contains("outside its output schema") && !message.contains("secret")));

        let not_run = run_linked_example_with_args(
            verb,
            &verb.examples[1],
            serde_json::json!({}),
            CallerIdentity::system("tier-a"),
            |_schema, _value| Ok(()),
        )
        .await;
        assert!(matches!(not_run, Outcome::Failed(message) if message.contains("not eligible")));

        let headless = run_linked_example_with_args(
            verb,
            &verb.examples[0],
            serde_json::json!({}),
            CallerIdentity::system("tier-a"),
            |_schema, value| {
                value
                    .is_object()
                    .then_some(())
                    .ok_or_else(|| "not object".into())
            },
        )
        .await;
        assert!(headless.is_pass());
    }

    #[tokio::test]
    async fn tier_a_fixture_arguments_replace_declared_example_arguments() {
        static EXAMPLES: [Example; 1] = [Example {
            name: "fixture",
            args: r#"{"ok":true,"fixture":0}"#,
            expect: Some(r#"{"ok":true,"fixture":42}"#),
            tier: ExampleTier::A,
        }];
        static VERB: std::sync::OnceLock<VerbDescriptor> = std::sync::OnceLock::new();
        let verb = VERB.get_or_init(|| {
            let mut verb = budgeted_verb("tier-example-test_fixture", None, echo_args);
            verb.examples = &EXAMPLES;
            verb
        });
        let result = run_linked_example_with_args(
            verb,
            &EXAMPLES[0],
            serde_json::json!({"ok":true,"fixture":42}),
            CallerIdentity::system("fixture-host"),
            |_schema, actual| {
                (actual["fixture"] == 42)
                    .then_some(())
                    .ok_or_else(|| "wrong fixture".into())
            },
        )
        .await;
        assert!(result.is_pass());
    }

    #[tokio::test]
    async fn an_example_over_budget_even_with_slack_fails() {
        static VERB: std::sync::OnceLock<VerbDescriptor> = std::sync::OnceLock::new();
        let verb = VERB.get_or_init(|| budgeted_verb("tier-a-budget-test_slow", Some(1), slow_ok));
        let results = run_verb(verb).await;
        assert_eq!(results.len(), 1);
        assert!(
            !results[0].outcome.is_pass(),
            "a call several times over a 1ms budget must fail, not just log"
        );
        if let Outcome::Failed(message) = &results[0].outcome {
            assert!(message.contains("budget"), "{message}");
        }
    }

    #[tokio::test]
    async fn an_example_inside_budget_passes() {
        static VERB: std::sync::OnceLock<VerbDescriptor> = std::sync::OnceLock::new();
        let verb =
            VERB.get_or_init(|| budgeted_verb("tier-a-budget-test_fast", Some(1_000), instant_ok));
        let results = run_verb(verb).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].outcome.is_pass());
    }

    #[tokio::test]
    async fn no_budget_declared_means_no_ceiling() {
        static VERB: std::sync::OnceLock<VerbDescriptor> = std::sync::OnceLock::new();
        let verb =
            VERB.get_or_init(|| budgeted_verb("tier-a-budget-test_unbudgeted", None, slow_ok));
        let results = run_verb(verb).await;
        assert_eq!(results.len(), 1);
        assert!(
            results[0].outcome.is_pass(),
            "no budget_ms means no ceiling, however long the call took"
        );
    }

    #[test]
    fn shared_result_checker_matches_expect_and_rejects_refusals() {
        assert!(check_result(
            "fixture-service_echo",
            "one",
            Some(r#"{"echo":"hello"}"#),
            serde_json::json!({"echo":"hello","extra":1}),
        )
        .is_pass());
        let refused = check_result(
            "fixture-service_echo",
            "one",
            None,
            serde_json::json!({"ok":false,"code":"host-unavailable","message":"down"}),
        );
        assert!(
            matches!(refused, Outcome::Failed(message) if message.contains("host-unavailable"))
        );
        let refusal = serde_json::json!({"ok":false,"code":"forbidden","message":"person only"});
        assert!(check_result(
            "provider-service_set-trusted",
            "non-person-refused",
            Some(r#"{"ok":false,"code":"forbidden"}"#),
            refusal.clone()
        )
        .is_pass());
        assert!(!check_result(
            "provider-service_set-trusted",
            "wrong-refusal",
            Some(r#"{"ok":false,"code":"invalid-argument"}"#),
            refusal.clone()
        )
        .is_pass());
        assert!(!check_result(
            "provider-service_set-trusted",
            "success-expected",
            Some(r#"{"ok":true}"#),
            refusal
        )
        .is_pass());
    }

    #[tokio::test]
    async fn provider_examples_are_explicit_tier_b_and_never_tier_a() {
        static LINKED: std::sync::OnceLock<VerbDescriptor> = std::sync::OnceLock::new();
        let linked =
            LINKED.get_or_init(|| budgeted_verb("tier-a-budget-test_linked", None, instant_ok));
        assert!(run_provider_examples(&VerbHandle::Linked(linked))
            .await
            .is_err());

        let safety = Safety {
            class: SafetyClass::ReadOnly,
            idempotent: true,
        };
        let provider = VerbHandle::Provider(Arc::new(ProviderVerb {
            name: "fixture-service_echo".into(),
            service: "fixture-service".into(),
            method: "echo".into(),
            description: "Echo text".into(),
            input_schema: serde_json::json!({"type":"object","properties":{},"additionalProperties":false}),
            output_schema: serde_json::json!({"type":"object"}),
            declared_safety: safety,
            effective_safety: safety,
            since: "0.1.0".into(),
            examples: vec![ProviderExample {
                name: "one".into(),
                args: "{}".into(),
                expect: Some(r#"{"echo":"hello"}"#.into()),
            }],
            provider_id: "fixture".into(),
            status: ProviderStatus::Unavailable,
            deprecated_since: None,
            generation: 1,
        }));
        let results = run_provider_examples(&provider).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].verb, "fixture-service_echo");
        assert_eq!(results[0].example, "one");
        assert!(
            matches!(&results[0].outcome, Outcome::Failed(message) if message.contains("host-unavailable"))
        );
    }
}
