//! The interpreter's plan: one loop over a [`Scenario`]'s steps, run against
//! an abstract [`Caller`] (SC-1's "one interpreter, two `Caller`s" — Tier A
//! and Tier B differ only in how a step reaches a verb, a surface or a
//! layout tree; this module knows none of that).
//!
//! This crate never runs a verb itself: `impress-scenario-service` supplies
//! the `Caller` (a pipeline call for Tier A, an HTTP client for Tier B), so
//! this crate stays pure (no `impress-core`, no `tokio` runtime dependency
//! beyond the trait's `async_trait` signature).

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::time::Instant;

use async_trait::async_trait;
use serde_json::Value;

use crate::spec::{
    CallCapture, CallStep, Check, EventBody, Expect, ExpectEffects, FieldExpect, Scenario,
    SelectPredicate, Step, StoreStep, WaitBody,
};
use crate::{template, validate};
use impress_service_core::report::{CapabilityResult, Tier};

const MAX_CAPTURE_ARRAY_ITEMS: usize = 10_000;
const MAX_CAPTURE_ARRAY_ENCODED_BYTES: usize = 1_048_576;

/// What a scenario step needs to do outside this crate. Every method
/// returns a plain `String` error — the interpreter's job is to say *which
/// step* failed, not to model every caller's error type.
#[async_trait]
pub trait Caller: Send {
    /// Run one verb call, as the identity `as_ident` names it
    /// (`"person"` or `"agent:<name>"`). Returns the verb's raw result
    /// value even on a refusal envelope (`{"ok": false, ...}`) — expectation
    /// checking, not this trait, decides whether that is a failure.
    async fn call(
        &mut self,
        verb: &str,
        args: Value,
        as_ident: &str,
    ) -> Result<CallOutcome, String>;

    /// Dispatch a human surface event. Tier A refuses with a clear message
    /// (no surface-hosting store per scenario in S1); Tier B posts to
    /// `/api/surface/{id}/dispatch`.
    async fn event(&mut self, event: &EventBody) -> Result<CallOutcome, String>;

    /// Run a human layout gesture — a verb aimed at the layout specifically.
    async fn gesture(&mut self, gesture: &Value) -> Result<CallOutcome, String>;

    /// Wait for a job to reach `state`, or a log line to appear, within
    /// `timeout_ms`. Returns an error naming what did not happen in time.
    async fn wait(&mut self, wait: &WaitBody) -> Result<(), String>;

    /// Capture the caller's current log timestamp. Tier B can use this to
    /// anchor a later `wait.log` before a mutation; other callers refuse it.
    async fn log_cursor(&mut self) -> Result<String, String> {
        Err("capturing a log cursor is not supported by this caller".to_string())
    }

    /// Insert one seed record before the first step runs (Tier A: a row in
    /// the scratch store; Tier B: unsupported today — seeding a live app's
    /// store is out of S1's scope, and `run` refuses naming that before any
    /// step executes).
    async fn seed(&mut self, kind: &str, payload: &Value) -> Result<Value, String>;

    /// Whether `kind` was written by any step run so far (the effects spy,
    /// SC-2). `Ok(false)` when the caller tracks no effects at all (Tier B
    /// without a spy) is indistinguishable from "not written" — a scenario
    /// with `expect_effects` on Tier B without spy support simply fails,
    /// which is correct: the assertion was never checked.
    fn wrote(&self, kind: &str) -> bool;
}

/// The result of one call/event/gesture, as far as expectation-checking
/// needs it.
#[derive(Debug, Clone, Default)]
pub struct CallOutcome {
    pub result: Value,
    /// The HTTP status, when the caller ran over the wire; `None` for Tier
    /// A, where there is no status to check.
    pub status: Option<u16>,
}

/// Run every step of `scenario` against `caller`, producing one
/// [`CapabilityResult`] the way every other Tier A/B catalogue already
/// does (SC-1: one report shape, not three).
pub async fn run(scenario: &Scenario, caller: &mut dyn Caller) -> CapabilityResult {
    let started = Instant::now();
    let tier: Tier = scenario.tier.into();

    if let Some(reason) = validate::unmet_requirement(scenario) {
        return skipped(scenario, tier, started, reason);
    }

    let problems = validate::validate(scenario);
    if !problems.is_empty() {
        return failed(
            scenario,
            tier,
            started,
            format!("invalid scenario: {problems:?}"),
        );
    }
    let mut notes = Vec::new();
    let mut captures: BTreeMap<String, Value> = BTreeMap::new();

    for seed in &scenario.seed {
        match caller.seed(&seed.kind, &seed.payload).await {
            Ok(inserted) => {
                if let Some(name) = &seed.r#as {
                    captures.insert(name.clone(), inserted);
                }
            }
            Err(e) => {
                return failed(
                    scenario,
                    tier,
                    started,
                    format!("seed `{}`: {e}", seed.kind),
                );
            }
        }
    }

    for (index, step) in scenario.steps.iter().enumerate() {
        if let Err(detail) = run_step(step, index, &mut captures, caller, &mut notes).await {
            run_teardown(scenario, &mut captures, caller, &mut notes).await;
            let detail = if notes.is_empty() {
                detail
            } else {
                format!("{detail}; {}", notes.join("; "))
            };
            return failed(scenario, tier, started, detail);
        }
    }

    if !run_teardown(scenario, &mut captures, caller, &mut notes).await {
        return failed(scenario, tier, started, notes.join("; "));
    }

    if let Some(expect) = &scenario.expect_effects {
        if let Some(missing) = missing_effect(expect, caller) {
            return failed(
                scenario,
                tier,
                started,
                format!("expected effect on `{missing}` was not observed"),
            );
        }
    }

    let mut result = passed(scenario, tier, started);
    if !notes.is_empty() {
        result.detail.push_str(&format!("; {}", notes.join("; ")));
    }
    result
}

async fn run_teardown(
    scenario: &Scenario,
    captures: &mut BTreeMap<String, Value>,
    caller: &mut dyn Caller,
    notes: &mut Vec<String>,
) -> bool {
    let mut passed = true;
    for (index, step) in scenario.teardown.iter().enumerate() {
        if let Err(error) = run_step(step, index, captures, caller, notes).await {
            passed = false;
            notes.push(format!("teardown: {error}"));
        }
    }
    passed
}

fn missing_effect(expect: &ExpectEffects, caller: &dyn Caller) -> Option<String> {
    expect.writes.iter().find(|k| !caller.wrote(k)).cloned()
}

async fn run_step(
    step: &Step,
    index: usize,
    captures: &mut BTreeMap<String, Value>,
    caller: &mut dyn Caller,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    let captures_value = Value::Object(captures.clone().into_iter().collect());
    match step {
        Step::BestEffort(step) => {
            let call = &step.best_effort;
            let args = template::resolve(&call.args, &captures_value)
                .map_err(|e| format!("step {index} (best_effort `{}`): {e}", call.call))?;
            let failure = match caller.call(&call.call, args, &call.r#as).await {
                Err(error) => Some(error),
                Ok(outcome)
                    if outcome.status.is_some_and(|s| !(200..300).contains(&s))
                        || outcome.result.get("ok").and_then(Value::as_bool) == Some(false) =>
                {
                    Some(format!(
                        "status {:?}, result {}",
                        outcome.status, outcome.result
                    ))
                }
                Ok(_) => None,
            };
            if let Some(error) = failure {
                notes.push(format!(
                    "step {index} best_effort `{}` failed: {error}",
                    call.call
                ));
            }
            Ok(())
        }
        Step::Store(step) => run_store(step, index, &captures_value, captures, caller).await,
        Step::Call(call_step) => {
            run_call(call_step, index, &captures_value, captures, caller).await
        }
        Step::Event(event_step) => {
            let resolved = template::resolve(
                &serde_json::to_value(&event_step.event).map_err(|e| e.to_string())?,
                &captures_value,
            )
            .map_err(|e| format!("step {index} (event): {e}"))?;
            let event: EventBody = serde_json::from_value(resolved)
                .map_err(|e| format!("step {index} (event): {e}"))?;
            let outcome = caller
                .event(&event)
                .await
                .map_err(|e| format!("step {index} (event): {e}"))?;
            check_action_outcome(&outcome).map_err(|e| format!("step {index} (event): {e}"))
        }
        Step::Gesture(gesture_step) => {
            let resolved = template::resolve(&gesture_step.gesture, &captures_value)
                .map_err(|e| format!("step {index} (gesture): {e}"))?;
            let outcome = caller
                .gesture(&resolved)
                .await
                .map_err(|e| format!("step {index} (gesture): {e}"))?;
            check_action_outcome(&outcome).map_err(|e| format!("step {index} (gesture): {e}"))?;
            for (name, path) in &gesture_step.capture {
                let captured = json_path_get(&outcome.result, path).ok_or_else(|| {
                    format!(
                        "step {index} (gesture): capture `{name}` path `{path}` did not resolve"
                    )
                })?;
                captures.insert(name.clone(), captured.clone());
            }
            Ok(())
        }
        Step::Wait(wait_step) => {
            if let Some(path) = &wait_step.when_present {
                let segments = validate::capture_state_path_segments(path)
                    .map_err(|e| format!("step {index} (wait): when_present path `{path}`: {e}"))?;
                let mut value = &captures_value;
                for segment in segments {
                    value = match value {
                        Value::Object(map) => match map.get(segment) {
                            Some(value) => value,
                            None => return Ok(()),
                        },
                        Value::Array(items) => {
                            match segment.parse::<usize>().ok().and_then(|i| items.get(i)) {
                                Some(value) => value,
                                None => return Ok(()),
                            }
                        }
                        _ => return Ok(()),
                    };
                }
                if value.is_null() {
                    return Ok(());
                }
            }
            let resolved = template::resolve(
                &serde_json::to_value(&wait_step.wait).map_err(|e| e.to_string())?,
                &captures_value,
            )
            .map_err(|e| format!("step {index} (wait): {e}"))?;
            let wait: WaitBody = serde_json::from_value(resolved)
                .map_err(|e| format!("step {index} (wait): {e}"))?;
            match &wait {
                WaitBody::LogCursor { log_cursor } => {
                    if log_cursor.capture.trim().is_empty() {
                        return Err(format!(
                            "step {index} (wait): log cursor capture name is empty"
                        ));
                    }
                    let cursor = caller
                        .log_cursor()
                        .await
                        .map_err(|e| format!("step {index} (wait): {e}"))?;
                    captures.insert(log_cursor.capture.clone(), Value::String(cursor));
                    Ok(())
                }
                _ => caller
                    .wait(&wait)
                    .await
                    .map_err(|e| format!("step {index} (wait): {e}")),
            }
        }
    }
}

// An event or gesture has no `expect` field: unlike a call step, a refusal can
// never be an expected result. Check transport status and the service envelope
// before a gesture's result can be captured, so refusals cannot look successful.
fn check_action_outcome(outcome: &CallOutcome) -> Result<(), String> {
    if let Some(status) = outcome.status {
        if !(200..300).contains(&status) {
            return Err(format!("action returned HTTP {status}"));
        }
    }
    if outcome.result.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err("action did not return ok=true".to_string());
    }
    Ok(())
}

async fn run_call(
    call_step: &CallStep,
    index: usize,
    captures_value: &Value,
    captures: &mut BTreeMap<String, Value>,
    caller: &mut dyn Caller,
) -> Result<(), String> {
    let args = template::resolve(&call_step.args, captures_value)
        .map_err(|e| format!("step {index} (`{}`): {e}", call_step.call))?;

    // Argument captures are evaluated from the exact typed JSON that will be
    // sent. Resolve them before dispatch so a missing path cannot leave a
    // mutation applied without the capture a later step depends on.
    let argument_captures = call_step
        .capture
        .iter()
        .filter_map(|(name, operation)| match operation {
            CallCapture::Argument(spec) => Some((name, spec)),
            _ => None,
        })
        .map(|(name, spec)| {
            let captured = json_path_get(&args, &spec.argument)
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "step {index} (`{}`) argument capture `{name}` path `{}` did not resolve",
                        call_step.call, spec.argument
                    )
                })?;
            Ok((name.clone(), captured))
        })
        .collect::<Result<BTreeMap<_, _>, String>>()?;

    let outcome = caller
        .call(&call_step.call, args, &call_step.r#as)
        .await
        .map_err(|e| format!("step {index} (`{}`): {e}", call_step.call))?;

    if let Some(expect) = &call_step.expect {
        let resolved = template::resolve(
            &serde_json::to_value(expect).map_err(|e| e.to_string())?,
            captures_value,
        )
        .map_err(|e| format!("step {index} expectation: {e}"))?;
        let expect: Expect = serde_json::from_value(resolved)
            .map_err(|e| format!("step {index} expectation: {e}"))?;
        check_expect(&expect, &outcome)
            .map_err(|detail| format!("step {index} (`{}`): {detail}", call_step.call))?;
    }

    for (name, operation) in &call_step.capture {
        let captured = match operation {
            CallCapture::Argument(_) => argument_captures
                .get(name)
                .cloned()
                .expect("argument capture was extracted before dispatch"),
            _ => capture_call_result(operation, &outcome.result, captures_value).map_err(|e| {
                format!("step {index} (`{}`) capture `{name}`: {e}", call_step.call)
            })?,
        };
        captures.insert(name.clone(), captured);
    }
    Ok(())
}

fn capture_call_result(
    operation: &CallCapture,
    result: &Value,
    captures: &Value,
) -> Result<Value, String> {
    match operation {
        CallCapture::Path(path) => {
            let resolved = template::resolve(&Value::String(path.clone()), captures)?;
            let resolved = resolved
                .as_str()
                .ok_or("capture path must resolve to a string")?;
            json_path_get(result, resolved)
                .cloned()
                .ok_or_else(|| format!("path `{resolved}` did not resolve"))
        }
        CallCapture::Argument(_) => {
            Err("argument capture must read the resolved call arguments".to_string())
        }
        CallCapture::SelectOne(spec) => {
            let query = &spec.select_one;
            if query.from.trim().is_empty() || query.path.trim().is_empty() {
                return Err("select_one paths must be non-empty".to_string());
            }
            if query.path.contains("{{") {
                return Err("select_one candidate path must be fixed".to_string());
            }
            if query
                .object_key_as
                .as_deref()
                .is_some_and(|kind| kind != "u64")
            {
                return Err("select_one object_key_as only accepts `u64`".to_string());
            }
            let from = template::resolve(&Value::String(query.from.clone()), captures)?;
            let from = from
                .as_str()
                .ok_or("select_one from must resolve to a JSON path")?;
            let candidates = json_path_get(result, from)
                .ok_or_else(|| format!("select_one source `{from}` did not resolve"))?;
            let (contains, expected) = match &query.predicate {
                SelectPredicate::Equals(predicate) => {
                    (false, template::resolve(&predicate.equals, captures)?)
                }
                SelectPredicate::ArrayContains(predicate) => (
                    true,
                    template::resolve(&predicate.array_contains, captures)?,
                ),
            };
            let mut found = None;
            let mut inspect = |key: Value, candidate: &Value| -> Result<(), String> {
                let Some(field) = json_path_get(candidate, &query.path) else {
                    return Ok(());
                };
                let matches = if contains {
                    let items = field
                        .as_array()
                        .ok_or("select_one array_contains path is not an array")?;
                    if items.len() > MAX_CAPTURE_ARRAY_ITEMS {
                        return Err(format!(
                            "select_one predicate array exceeds {MAX_CAPTURE_ARRAY_ITEMS} items"
                        ));
                    }
                    items.contains(&expected)
                } else {
                    *field == expected
                };
                if matches {
                    if found.is_some() {
                        return Err("select_one matched more than one candidate".to_string());
                    }
                    let mut selection = serde_json::Map::new();
                    if query.object_key_as.as_deref() == Some("u64") {
                        let raw_key = key
                            .as_str()
                            .ok_or("select_one u64 key conversion requires an object")?;
                        let numeric_key = raw_key
                            .parse::<u64>()
                            .map_err(|_| "select_one object key is not a u64")?;
                        if raw_key != numeric_key.to_string() {
                            return Err(
                                "select_one object key is not a canonical decimal u64".to_string()
                            );
                        }
                        selection.insert("numeric_key".into(), Value::from(numeric_key));
                    }
                    selection.insert("key".into(), key);
                    selection.insert("value".into(), candidate.clone());
                    found = Some(Value::Object(selection));
                }
                Ok(())
            };
            match candidates {
                Value::Object(map) if map.len() <= MAX_CAPTURE_ARRAY_ITEMS => {
                    for (key, candidate) in map {
                        inspect(Value::String(key.clone()), candidate)?;
                    }
                }
                Value::Array(items) if items.len() <= MAX_CAPTURE_ARRAY_ITEMS => {
                    for (index, candidate) in items.iter().enumerate() {
                        inspect(Value::from(index), candidate)?;
                    }
                }
                Value::Object(_) | Value::Array(_) => {
                    return Err(format!(
                        "select_one source exceeds {MAX_CAPTURE_ARRAY_ITEMS} candidates"
                    ));
                }
                _ => return Err("select_one source must be an object or array".to_string()),
            }
            found.ok_or_else(|| "select_one matched no candidate".to_string())
        }
        CallCapture::FillArray(spec) => {
            let source =
                template::resolve(&Value::String(spec.fill_array.length_of.clone()), captures)?;
            let length = source
                .as_array()
                .ok_or_else(|| "fill_array length_of must resolve to a captured array".to_string())?
                .len();
            if length > MAX_CAPTURE_ARRAY_ITEMS {
                return Err(format!(
                    "fill_array length exceeds {MAX_CAPTURE_ARRAY_ITEMS}"
                ));
            }
            let value_bytes = serde_json::to_vec(&spec.fill_array.value)
                .map_err(|error| format!("fill_array value could not be encoded: {error}"))?
                .len();
            let output_bytes = if length == 0 {
                Some(2)
            } else {
                value_bytes
                    .checked_mul(length)
                    .and_then(|bytes| bytes.checked_add(length - 1))
                    .and_then(|bytes| bytes.checked_add(2))
            }
            .ok_or("fill_array encoded size overflowed")?;
            if output_bytes > MAX_CAPTURE_ARRAY_ENCODED_BYTES {
                return Err(format!(
                    "fill_array encoded output exceeds {MAX_CAPTURE_ARRAY_ENCODED_BYTES} bytes"
                ));
            }
            Ok(Value::Array(vec![spec.fill_array.value.clone(); length]))
        }
    }
}

// A bounded scan through existing verbs keeps store IO and identity handling
// in the caller. Neither Tier A nor Tier B gets a second query implementation.
async fn run_store(
    step: &StoreStep,
    index: usize,
    captures_value: &Value,
    captures: &mut BTreeMap<String, Value>,
    caller: &mut dyn Caller,
) -> Result<(), String> {
    let raw = template::resolve(
        &serde_json::to_value(&step.store).map_err(|e| e.to_string())?,
        captures_value,
    )?;
    let query: crate::spec::StorePredicate =
        serde_json::from_value(raw).map_err(|e| e.to_string())?;
    if query.schema_ref.trim().is_empty() || !(1..=10_000).contains(&query.max_rows) {
        return Err(format!(
            "step {index} (store): resolved schema_ref or max_rows is invalid"
        ));
    }
    let mut offset = 0;
    while offset < query.max_rows {
        let limit = (query.max_rows - offset).min(100);
        let page = caller
            .call(
                "store-query-service_list-items",
                serde_json::json!({
                    "schema_ref":query.schema_ref,"limit":limit,"offset":offset
                }),
                "agent:scenario",
            )
            .await?;
        check_action_outcome(&page).map_err(|e| format!("step {index} (store list): {e}"))?;
        let rows = page.result["items"]
            .as_array()
            .ok_or("store list omitted items")?;
        for row in rows.iter().take(limit) {
            let id = row["id"].as_str().ok_or("store item omitted id")?;
            let record = caller
                .call(
                    "store-query-service_get-item",
                    serde_json::json!({"id":id}),
                    "agent:scenario",
                )
                .await?;
            check_action_outcome(&record)
                .map_err(|e| format!("step {index} (store get {id}): {e}"))?;
            if record.result["truncated"] == true {
                return Err(format!(
                    "step {index} (store): payload for {id} is truncated; predicate cannot be checked"
                ));
            }
            let payload: Value = serde_json::from_str(
                record.result["payload"]
                    .as_str()
                    .ok_or("store item omitted payload")?,
            )
            .map_err(|e| format!("store payload for {id}: {e}"))?;
            let candidate = serde_json::json!({"item":record.result["item"], "payload":payload});
            if query
                .predicates
                .iter()
                .all(|field| check_field(field, &candidate).is_ok())
            {
                for (name, path) in &step.capture {
                    let value = json_path_get(&candidate, path).ok_or_else(|| {
                        format!(
                            "step {index} (store): capture `{name}` path `{path}` did not resolve"
                        )
                    })?;
                    captures.insert(name.clone(), value.clone());
                }
                return Ok(());
            }
        }
        offset += limit;
        if rows.len() < limit
            || page.result["total"]
                .as_u64()
                .is_some_and(|n| offset as u64 >= n)
        {
            return Err(format!(
                "step {index} (store): no `{}` item matched the predicate",
                query.schema_ref
            ));
        }
    }
    Err(format!(
        "step {index} (store): no match within max_rows={}",
        query.max_rows
    ))
}

fn check_expect(expect: &Expect, outcome: &CallOutcome) -> Result<(), String> {
    if let Some(ok) = expect.ok {
        let actual = outcome.result.get("ok").and_then(Value::as_bool);
        if actual != Some(ok) {
            return Err(format!(
                "expected ok={ok}, got {actual:?} (result: {})",
                outcome.result
            ));
        }
    }
    if let Some(code) = &expect.code {
        let actual = outcome.result.get("code").and_then(Value::as_str);
        if actual != Some(code.as_str()) {
            return Err(format!("expected code=\"{code}\", got {actual:?}"));
        }
    }
    if let Some(status) = expect.status {
        if outcome.status != Some(status) {
            return Err(format!(
                "expected status={status}, got {:?}",
                outcome.status
            ));
        }
    }
    for field in &expect.fields {
        check_field(field, &outcome.result)?;
    }
    Ok(())
}

fn check_field(field: &FieldExpect, result: &Value) -> Result<(), String> {
    let path = &field.path;
    let value = json_path_get(result, path);
    match &field.check {
        Check::Equals(expected) => {
            if value != Some(expected) {
                return Err(format!("{path}: expected {expected}, got {value:?}"));
            }
        }
        Check::NotEquals(expected) => {
            let Some(actual) = value else {
                return Err(format!(
                    "{path}: expected a value different from {expected}, got missing"
                ));
            };
            if actual == expected {
                return Err(format!(
                    "{path}: expected a value different from {expected}, got {actual}"
                ));
            }
        }
        Check::Contains(needle) => {
            let hay = value.and_then(Value::as_str).unwrap_or_default();
            if !hay.contains(needle.as_str()) {
                return Err(format!(
                    "{path}: expected to contain \"{needle}\", got \"{hay}\""
                ));
            }
        }
        Check::Gte(min) => {
            let n = value.and_then(Value::as_f64);
            if n.is_none_or(|n| n < *min) {
                return Err(format!("{path}: expected >= {min}, got {n:?}"));
            }
        }
        Check::Lte(max) => {
            let n = value.and_then(Value::as_f64);
            if n.is_none_or(|n| n > *max) {
                return Err(format!("{path}: expected <= {max}, got {n:?}"));
            }
        }
        Check::Gt(expected) => {
            let actual_number = value.and_then(Value::as_number).ok_or_else(|| {
                format!("{path}: expected a JSON number greater than {expected}, got {value:?}")
            })?;
            let expected_number = expected
                .as_number()
                .ok_or_else(|| format!("{path}: gt expects a JSON number, got {expected}"))?;
            if exact_number_cmp(actual_number, expected_number) != Some(Ordering::Greater) {
                return Err(format!("{path}: expected {value:?} > {expected}"));
            }
        }
        Check::Within { value: target, tol } => {
            let target = target
                .as_f64()
                .ok_or_else(|| format!("{path}: within expects a JSON number, got {target}"))?;
            let n = value.and_then(Value::as_f64);
            if n.is_none_or(|n| (n - target).abs() > *tol) {
                return Err(format!("{path}: expected {target} +/- {tol}, got {n:?}"));
            }
        }
        Check::Len(expected_len) => {
            let len = value.and_then(|v| match v {
                Value::Array(a) => Some(a.len()),
                Value::String(s) => Some(s.chars().count()),
                Value::Object(m) => Some(m.len()),
                _ => None,
            });
            if len != Some(*expected_len) {
                return Err(format!("{path}: expected len={expected_len}, got {len:?}"));
            }
        }
        Check::Present(want) => {
            let present = value.is_some();
            if present != *want {
                return Err(format!("{path}: expected present={want}, got {present}"));
            }
        }
        Check::Absent(want) => {
            let absent = value.is_none();
            if absent != *want {
                return Err(format!("{path}: expected absent={want}, got {absent}"));
            }
        }
    }
    Ok(())
}

/// Compare JSON numbers without routing integer values through `f64`.
/// In particular, distinct `u64`s above 2^53 must remain ordered distinctly.
fn exact_number_cmp(left: &serde_json::Number, right: &serde_json::Number) -> Option<Ordering> {
    fn integer(number: &serde_json::Number) -> Option<i128> {
        number
            .as_u64()
            .map(i128::from)
            .or_else(|| number.as_i64().map(i128::from))
    }

    fn integer_float_cmp(integer: i128, float: f64) -> Option<Ordering> {
        if !float.is_finite() {
            return None;
        }
        let upper_bound = 2_f64.powi(127);
        if float >= upper_bound {
            return Some(Ordering::Less);
        }
        if float < -upper_bound {
            return Some(Ordering::Greater);
        }
        if float == -upper_bound {
            return Some(integer.cmp(&i128::MIN));
        }

        let truncated = float.trunc() as i128;
        Some(match integer.cmp(&truncated) {
            Ordering::Equal if float.fract() > 0.0 => Ordering::Less,
            Ordering::Equal if float.fract() < 0.0 => Ordering::Greater,
            ordering => ordering,
        })
    }

    match (integer(left), integer(right)) {
        (Some(left), Some(right)) => Some(left.cmp(&right)),
        (Some(left), None) => integer_float_cmp(left, right.as_f64()?),
        (None, Some(right)) => Some(integer_float_cmp(right, left.as_f64()?)?.reverse()),
        (None, None) => left.as_f64()?.partial_cmp(&right.as_f64()?),
    }
}

/// A minimal `$.a.b.0` walk, matching the plan's spelling for `capture` and
/// `fields[].path`. `$` alone is the whole value.
fn json_path_get<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let path = path.strip_prefix('$').unwrap_or(path);
    let mut cur = value;
    for segment in path.split('.').filter(|s| !s.is_empty()) {
        cur = match cur {
            Value::Object(map) => map.get(segment)?,
            Value::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

fn passed(scenario: &Scenario, tier: Tier, started: Instant) -> CapabilityResult {
    CapabilityResult {
        id: scenario.id.clone(),
        description: scenario.description.clone(),
        tier,
        pass: true,
        detail: "every step ran and every expectation held".to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        skipped: false,
    }
}

fn failed(scenario: &Scenario, tier: Tier, started: Instant, detail: String) -> CapabilityResult {
    CapabilityResult {
        id: scenario.id.clone(),
        description: scenario.description.clone(),
        tier,
        pass: false,
        detail,
        duration_ms: started.elapsed().as_millis() as u64,
        skipped: false,
    }
}

fn skipped(scenario: &Scenario, tier: Tier, started: Instant, reason: String) -> CapabilityResult {
    CapabilityResult {
        id: scenario.id.clone(),
        description: scenario.description.clone(),
        tier,
        pass: false,
        detail: format!("skipped: {reason}"),
        duration_ms: started.elapsed().as_millis() as u64,
        skipped: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn action_outcome_requires_successful_status_and_envelope() {
        for outcome in [
            CallOutcome {
                result: json!({"ok": true}),
                status: Some(404),
            },
            CallOutcome {
                result: json!({"ok": false}),
                status: Some(200),
            },
            CallOutcome {
                result: json!({"message": "missing envelope"}),
                status: Some(200),
            },
        ] {
            assert!(check_action_outcome(&outcome).is_err(), "{outcome:?}");
        }
        assert!(check_action_outcome(&CallOutcome {
            result: json!({"ok": true}),
            status: Some(200),
        })
        .is_ok());
    }

    #[test]
    fn select_one_captures_unique_object_key_numeric_key_and_value() {
        let operation: CallCapture = serde_json::from_value(json!({
            "select_one": {
                "from": "$.tiles",
                "path": "$.container.linear.children",
                "predicate": {"array_contains": "{{state.tile}}"},
                "object_key_as": "u64"
            }
        }))
        .unwrap();
        let result = json!({"tiles": {
            "3": {"pane": {}},
            "9": {"container": {"linear": {"children": [4, 7]}}}
        }});
        let captures = json!({"tile": 7});
        assert_eq!(
            capture_call_result(&operation, &result, &captures).unwrap(),
            json!({
                "key": "9",
                "numeric_key": 9,
                "value": {"container": {"linear": {"children": [4, 7]}}}
            })
        );
    }

    #[test]
    fn select_one_resolves_prior_capture_in_source_and_returns_array_index() {
        let operation: CallCapture = serde_json::from_value(json!({
            "select_one": {
                "from": "$.tiles.{{state.container_id}}.channels",
                "path": "$.source",
                "predicate": {"equals": "selection"}
            }
        }))
        .unwrap();
        let result = json!({"tiles": {"8": {"channels": [
            {"source": "other"}, {"source": "selection", "number": 3}
        ]}}});
        assert_eq!(
            capture_call_result(&operation, &result, &json!({"container_id": 8})).unwrap(),
            json!({"key": 1, "value": {"source": "selection", "number": 3}})
        );
    }

    #[test]
    fn select_one_u64_conversion_rejects_noncanonical_and_overflowing_keys() {
        let operation: CallCapture = serde_json::from_value(json!({
            "select_one": {
                "from": "$.tiles",
                "path": "$.children",
                "predicate": {"array_contains": 7},
                "object_key_as": "u64"
            }
        }))
        .unwrap();
        for key in ["+9", "09", "18446744073709551616"] {
            let mut tiles = serde_json::Map::new();
            tiles.insert(key.to_string(), json!({"children": [7]}));
            let result = json!({"tiles": tiles});
            assert!(capture_call_result(&operation, &result, &json!({}))
                .unwrap_err()
                .contains("object key"));
        }
    }

    #[test]
    fn select_one_refuses_zero_multiple_non_collection_and_oversized_inputs() {
        let operation: CallCapture = serde_json::from_value(json!({
            "select_one": {
                "from": "$.items",
                "path": "$.name",
                "predicate": {"equals": "wanted"}
            }
        }))
        .unwrap();
        for (result, expected) in [
            (
                json!({"items": [{"name": "other"}]}),
                "matched no candidate",
            ),
            (
                json!({"items": [{"name": "wanted"}, {"name": "wanted"}]}),
                "more than one",
            ),
            (
                json!({"items": "not a collection"}),
                "must be an object or array",
            ),
            (
                json!({"items": (0..=10_000).map(|_| json!({})).collect::<Vec<_>>()}),
                "exceeds 10000",
            ),
        ] {
            assert!(capture_call_result(&operation, &result, &json!({}))
                .unwrap_err()
                .contains(expected));
        }
        let contains: CallCapture = serde_json::from_value(json!({
            "select_one": {
                "from": "$.items",
                "path": "$.values",
                "predicate": {"array_contains": 1}
            }
        }))
        .unwrap();
        for (result, expected) in [
            (json!({"items": [{"values": "wrong type"}]}), "not an array"),
            (
                json!({"items": [{"values": (0..=10_000).collect::<Vec<_>>()}]}),
                "predicate array exceeds 10000",
            ),
        ] {
            assert!(capture_call_result(&contains, &result, &json!({}))
                .unwrap_err()
                .contains(expected));
        }
    }

    #[test]
    fn fill_array_is_bounded_and_uses_only_a_prior_captured_array() {
        let operation: CallCapture = serde_json::from_value(json!({
            "fill_array": {"value": 1.0, "length_of": "{{state.parent.value.children}}"}
        }))
        .unwrap();
        assert_eq!(
            capture_call_result(
                &operation,
                &json!({}),
                &json!({"parent": {"value": {"children": [1, 2, 3]}}})
            )
            .unwrap(),
            json!([1.0, 1.0, 1.0])
        );
        for captures in [
            json!({"parent": {"value": {"children": "not array"}}}),
            json!({"parent": {"value": {"children": (0..=10_000).collect::<Vec<_>>()}}}),
        ] {
            let error = capture_call_result(&operation, &json!({}), &captures).unwrap_err();
            assert!(error.contains("array") || error.contains("exceeds 10000"));
        }

        let literal: CallCapture = serde_json::from_value(json!({
            "fill_array": {
                "value": "{{state.missing}}",
                "length_of": "{{state.parent.value.children}}"
            }
        }))
        .unwrap();
        assert_eq!(
            capture_call_result(
                &literal,
                &json!({}),
                &json!({"parent": {"value": {"children": [1]}}})
            )
            .unwrap(),
            json!(["{{state.missing}}"])
        );

        let amplification: CallCapture = serde_json::from_value(json!({
            "fill_array": {
                "value": "x".repeat(128),
                "length_of": "{{state.items}}"
            }
        }))
        .unwrap();
        let items = (0..MAX_CAPTURE_ARRAY_ITEMS).collect::<Vec<_>>();
        assert!(
            capture_call_result(&amplification, &json!({}), &json!({"items": items}))
                .unwrap_err()
                .contains("encoded output exceeds")
        );
    }

    #[tokio::test]
    async fn call_capture_operators_feed_typed_values_to_later_calls() {
        struct Fixture {
            resize: Option<Value>,
        }

        #[async_trait::async_trait]
        impl Caller for Fixture {
            async fn call(
                &mut self,
                verb: &str,
                args: Value,
                _as_ident: &str,
            ) -> Result<CallOutcome, String> {
                if verb == "layout-service_resize" {
                    self.resize = Some(args);
                    return Ok(CallOutcome {
                        result: json!({"ok": true}),
                        status: None,
                    });
                }
                Ok(CallOutcome {
                    result: json!({
                        "focused": 7,
                        "layout": {"tiles": {
                            "8": {"container": {"linear": {"children": [4, 7]}}}
                        }}
                    }),
                    status: None,
                })
            }

            async fn event(&mut self, _event: &EventBody) -> Result<CallOutcome, String> {
                Err("unused".into())
            }

            async fn gesture(&mut self, _gesture: &Value) -> Result<CallOutcome, String> {
                Err("unused".into())
            }

            async fn wait(&mut self, _wait: &WaitBody) -> Result<(), String> {
                Err("unused".into())
            }

            async fn seed(&mut self, _kind: &str, _payload: &Value) -> Result<Value, String> {
                Err("unused".into())
            }

            fn wrote(&self, _kind: &str) -> bool {
                false
            }
        }

        let scenario: Scenario = serde_json::from_value(json!({
            "wire_version": 1,
            "id": "capture-operators",
            "description": "typed capture values flow to later calls",
            "tier": "b",
            "steps": [
                {"call": "layout-service_get-pane", "capture": {"tile": "$.focused"}},
                {"call": "layout-service_get-layout", "capture": {
                    "parent": {"select_one": {
                        "from": "$.layout.tiles",
                        "path": "$.container.linear.children",
                        "predicate": {"array_contains": "{{state.tile}}"},
                        "object_key_as": "u64"
                    }}
                }},
                {"call": "layout-service_get-layout", "capture": {
                    "shares": {"fill_array": {
                        "value": 1.0,
                        "length_of": "{{state.parent.value.container.linear.children}}"
                    }}
                }},
                {"call": "layout-service_resize", "args": {
                    "container": "{{state.parent.numeric_key}}",
                    "shares": "{{state.shares}}"
                }}
            ]
        }))
        .unwrap();
        let mut caller = Fixture { resize: None };
        let result = run(&scenario, &mut caller).await;
        assert!(result.pass, "{}", result.detail);
        assert_eq!(
            caller.resize,
            Some(json!({"container": 8, "shares": [1.0, 1.0]}))
        );
    }

    #[tokio::test]
    async fn guarded_wait_skips_missing_or_null_capture_before_template_resolution() {
        struct Fixture {
            detail: Value,
            waits: usize,
        }
        #[async_trait::async_trait]
        impl Caller for Fixture {
            async fn call(
                &mut self,
                _verb: &str,
                _args: Value,
                _as_ident: &str,
            ) -> Result<CallOutcome, String> {
                Ok(CallOutcome {
                    result: json!({"detail": self.detail}),
                    status: None,
                })
            }
            async fn event(&mut self, _event: &EventBody) -> Result<CallOutcome, String> {
                Err("unused".into())
            }
            async fn gesture(&mut self, _gesture: &Value) -> Result<CallOutcome, String> {
                Err("unused".into())
            }
            async fn wait(&mut self, _wait: &WaitBody) -> Result<(), String> {
                self.waits += 1;
                Err("wait failed".into())
            }
            async fn seed(&mut self, _kind: &str, _payload: &Value) -> Result<Value, String> {
                Err("unused".into())
            }
            fn wrote(&self, _kind: &str) -> bool {
                false
            }
        }
        let scenario: Scenario = serde_json::from_value(json!({
            "wire_version": 1,
            "id": "guarded-wait",
            "description": "optional detail wait",
            "tier": "b",
            "steps": [
                {"call": "read-detail", "capture": {"detail": "$.detail"}},
                {"wait": {"log": {"category": "layout", "contains": "pane {{state.detail.tile}}", "timeout_ms": 100}}, "when_present": "$.detail.tile"}
            ]
        })).unwrap();
        for detail in [Value::Null, json!({})] {
            let mut caller = Fixture { detail, waits: 0 };
            let result = run(&scenario, &mut caller).await;
            assert!(result.pass, "{}", result.detail);
            assert_eq!(caller.waits, 0);
        }
        let mut caller = Fixture {
            detail: json!({"tile": 9}),
            waits: 0,
        };
        let result = run(&scenario, &mut caller).await;
        assert!(!result.pass);
        assert!(result.detail.contains("wait failed"), "{}", result.detail);
        assert_eq!(caller.waits, 1);
    }
}
