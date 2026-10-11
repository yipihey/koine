//! Structural validation of a [`Scenario`], independent of any caller.
//!
//! `validate` checks what a spec alone can prove: capture names referenced
//! before they exist, an empty step list, and `wait.log` bounds/needles. It
//! cannot check that a `call` step names a real verb (this crate
//! has no inventory) — `impress-scenario-service`'s `validate` verb adds
//! that check with the inventory it links.

use std::collections::BTreeSet;

use crate::spec::{Requires, Scenario, Step};

/// One structural problem, with the step index it was found at (`None` for
/// a scenario-level problem).
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub step: Option<usize>,
    pub message: String,
}

/// Every structural problem found. Empty means the spec is well-formed —
/// not that every verb it calls exists.
pub fn validate(scenario: &Scenario) -> Vec<Problem> {
    let mut problems = Vec::new();

    if scenario.steps.is_empty() {
        problems.push(Problem {
            step: None,
            message: "a scenario needs at least one step".to_string(),
        });
    }

    let mut known: BTreeSet<String> = scenario
        .seed
        .iter()
        .filter_map(|s| s.r#as.clone())
        .collect();

    for (index, step) in scenario.steps.iter().chain(&scenario.teardown).enumerate() {
        let args = match step {
            Step::Call(call) => {
                check_adverbs(&call.args, call.adverbs.as_ref(), index, &mut problems);
                let mut capture_references = Vec::new();
                for (name, capture) in &call.capture {
                    let mut messages = Vec::new();
                    match capture {
                        crate::spec::CallCapture::Path(path) if path.trim().is_empty() => {
                            messages.push("capture path must be non-empty")
                        }
                        crate::spec::CallCapture::Argument(spec) => {
                            if spec.argument.trim().is_empty() {
                                messages.push("argument capture path must be non-empty");
                            }
                            if spec.argument.contains("{{") {
                                messages.push("argument capture path must be fixed");
                            }
                        }
                        crate::spec::CallCapture::SelectOne(spec) => {
                            let query = &spec.select_one;
                            if query.from.trim().is_empty() || query.path.trim().is_empty() {
                                messages.push("select_one paths must be non-empty");
                            }
                            if query.path.contains("{{") {
                                messages.push("select_one candidate path must be fixed");
                            }
                            if query.object_key_as.as_deref().is_some_and(|v| v != "u64") {
                                messages.push("select_one object_key_as only accepts `u64`");
                            }
                        }
                        crate::spec::CallCapture::FillArray(spec)
                            if spec.fill_array.length_of.trim().is_empty() =>
                        {
                            messages.push("fill_array length_of must be non-empty")
                        }
                        _ => {}
                    }
                    for message in messages {
                        problems.push(Problem {
                            step: Some(index),
                            message: format!("capture `{name}`: {message}"),
                        });
                    }
                    match capture {
                        crate::spec::CallCapture::Path(path) => {
                            capture_references.push(serde_json::json!(path));
                        }
                        crate::spec::CallCapture::Argument(_) => {}
                        crate::spec::CallCapture::SelectOne(spec) => {
                            let query = &spec.select_one;
                            capture_references.push(serde_json::json!(query.from));
                            match &query.predicate {
                                crate::spec::SelectPredicate::Equals(predicate) => {
                                    capture_references.push(predicate.equals.clone());
                                }
                                crate::spec::SelectPredicate::ArrayContains(predicate) => {
                                    capture_references.push(predicate.array_contains.clone());
                                }
                            }
                        }
                        crate::spec::CallCapture::FillArray(spec) => {
                            capture_references.push(serde_json::json!(spec.fill_array.length_of));
                        }
                    }
                }
                serde_json::json!({
                    "args": call.args,
                    "expect": call.expect,
                    "capture_references": capture_references
                })
            }
            Step::BestEffort(step) => {
                let call = &step.best_effort;
                check_adverbs(&call.args, call.adverbs.as_ref(), index, &mut problems);
                call.args.clone()
            }
            Step::Store(step) => {
                if step.store.schema_ref.trim().is_empty()
                    || !(1..=10_000).contains(&step.store.max_rows)
                {
                    problems.push(Problem {
                        step: Some(index),
                        message: "store requires a schema_ref and max_rows in 1..=10000".into(),
                    });
                }
                serde_json::to_value(&step.store).expect("store spec serializes")
            }
            Step::Event(step) => serde_json::to_value(&step.event).expect("event serializes"),
            Step::Gesture(step) => step.gesture.clone(),
            Step::Wait(step) => serde_json::to_value(&step.wait).expect("wait serializes"),
        };
        let adverbs = match step {
            Step::Call(call) => call.adverbs.clone(),
            Step::BestEffort(step) => step.best_effort.adverbs.clone(),
            _ => None,
        };
        for reference in referenced_captures(&args)
            .into_iter()
            .chain(adverbs.iter().flat_map(referenced_captures))
        {
            if !known.contains(&reference) {
                problems.push(Problem { step: Some(index), message: format!(
                    "`{{{{state.{reference}}}}}` is referenced before `{reference}` is captured or seeded"
                ) });
            }
        }
        if let Step::Wait(wait) = step {
            if let Some(path) = &wait.when_present {
                match capture_state_path_segments(path) {
                    Ok(segments) => {
                        if !known.contains(segments[0]) {
                            problems.push(Problem {
                                step: Some(index),
                                message: format!(
                                    "wait.when_present path `{path}` roots at capture `{}` that has not been set",
                                    segments[0]
                                ),
                            });
                        }
                    }
                    Err(message) => problems.push(Problem {
                        step: Some(index),
                        message: format!("wait.when_present path `{path}`: {message}"),
                    }),
                }
            }
            match &wait.wait {
                crate::spec::WaitBody::Log { log } => {
                    if !(1..=60_000).contains(&log.timeout_ms) {
                        problems.push(Problem {
                            step: Some(index),
                            message: "wait.log timeout_ms must be in 1..=60000".into(),
                        });
                    }
                    if log.contains.trim().is_empty()
                        || log
                            .also_contains
                            .iter()
                            .any(|needle| needle.trim().is_empty())
                    {
                        problems.push(Problem {
                            step: Some(index),
                            message: "wait.log requires non-empty message needles".into(),
                        });
                    }
                }
                crate::spec::WaitBody::LogCursor { log_cursor }
                    if log_cursor.capture.trim().is_empty() =>
                {
                    problems.push(Problem {
                        step: Some(index),
                        message: "wait.log_cursor requires a capture name".into(),
                    });
                }
                _ => {}
            }
        }
        match step {
            Step::Call(call) => known.extend(call.capture.keys().cloned()),
            Step::Store(step) => known.extend(step.capture.keys().cloned()),
            Step::Gesture(step) => known.extend(step.capture.keys().cloned()),
            Step::Wait(wait) => {
                if let crate::spec::WaitBody::LogCursor { log_cursor } = &wait.wait {
                    if !log_cursor.capture.trim().is_empty() {
                        known.insert(log_cursor.capture.clone());
                    }
                }
            }
            _ => {}
        }
    }

    problems
}

/// Parse the closed `$.capture.field.0` path accepted by wait presence guards.
/// Brackets, wildcards, filters and empty segments are intentionally unsupported.
pub(crate) fn capture_state_path_segments(path: &str) -> Result<Vec<&str>, &'static str> {
    let rest = path
        .strip_prefix("$.")
        .ok_or("must start with `$.<capture>`")?;
    let segments: Vec<&str> = rest.split('.').collect();
    if segments.is_empty()
        || segments.iter().any(|segment| {
            segment.is_empty()
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        })
    {
        return Err(
            "must contain non-empty dotted path segments using letters, digits, `_` or `-`",
        );
    }
    Ok(segments)
}

/// Every `state.<name>` reference's first segment, anywhere inside `value`
/// — a light scan (not `impress_surface::template::Template::parse`,
/// which would flag this as a public dependency this module does not need)
/// good enough to catch a typo before a run.
fn referenced_captures(value: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    scan(value, &mut out);
    out
}

fn scan(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => {
            let mut rest = s.as_str();
            while let Some(start) = rest.find("{{state.") {
                let after = &rest[start + "{{state.".len()..];
                if let Some(end) = after.find("}}") {
                    let path = &after[..end];
                    let name = path.split('.').next().unwrap_or(path);
                    if !name.is_empty() {
                        out.push(name.to_string());
                    }
                    rest = &after[end + 2..];
                } else {
                    break;
                }
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| scan(v, out)),
        serde_json::Value::Object(map) => map.values().for_each(|v| scan(v, out)),
        _ => {}
    }
}

/// Whether `scenario.requires` holds, given the runner's knowledge of what
/// app is up, what preset exists, and what kinds are present. Returns
/// `None` when the runner has no way to check requirements at all (this
/// pure crate) — always `None` here, since `Requires` is checked by the
/// caller, which owns the store/app it can inspect. Kept as a named
/// function so [`crate::interpret::run`] has one place to change if S2
/// gives this crate a way to check requirements itself.
pub fn unmet_requirement(_scenario: &Scenario) -> Option<String> {
    None
}

/// Read-only accessor kept for symmetry with `unmet_requirement`, exercised
/// by `impress-scenario-service` when it decides to skip before calling
/// `interpret::run` at all (`Requires` alone, no caller involved).
pub fn requires_ok(
    requires: &Requires,
    app_up: impl Fn(&str) -> bool,
    preset_exists: impl Fn(&str) -> bool,
    kind_present: impl Fn(&str) -> bool,
) -> Option<String> {
    if let Some(app) = &requires.app {
        if !app_up(app) {
            return Some(format!("app `{app}` is not running"));
        }
    }
    if let Some(preset) = &requires.preset {
        if !preset_exists(preset) {
            return Some(format!("preset `{preset}` is not shipped"));
        }
    }
    for kind in &requires.kinds_present {
        if !kind_present(kind) {
            return Some(format!("no `{kind}` row is present"));
        }
    }
    None
}

/// `adverbs` is an object of snake_case adverb name to value, and it can only
/// travel in an object `args` that does not already name the reserved key. The
/// vocabulary is the pipeline's, not checked here.
fn check_adverbs(
    args: &serde_json::Value,
    adverbs: Option<&serde_json::Value>,
    index: usize,
    problems: &mut Vec<Problem>,
) {
    let Some(adverbs) = adverbs else { return };
    let mut push = |message: String| {
        problems.push(Problem {
            step: Some(index),
            message,
        })
    };
    let Some(map) = adverbs.as_object() else {
        push("`adverbs` is an object of adverb name to value".into());
        return;
    };
    for key in map.keys() {
        let mut chars = key.chars();
        let ok = matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !ok {
            push(format!(
                "adverb name '{key}' is not a snake_case identifier ([a-z][a-z0-9_]*)"
            ));
        }
    }
    match args {
        serde_json::Value::Object(a) if a.contains_key("adverbs") => push(
            "`args` already has an `adverbs` key; put adverbs in the step's `adverbs` field".into(),
        ),
        // A defaulted (absent) `args` is null: it becomes an empty object.
        serde_json::Value::Object(_) | serde_json::Value::Null => {}
        _ => push("`adverbs` need an object `args`".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{CallCapture, CallStep, Scenario, SeedRecord, Step, Tier};
    use serde_json::json;

    fn base(steps: Vec<Step>) -> Scenario {
        Scenario {
            wire_version: 1,
            id: "t".to_string(),
            description: "d".to_string(),
            tier: Tier::A,
            requires: None,
            seed: Vec::new(),
            steps,
            teardown: Vec::new(),
            expect_effects: None,
        }
    }

    fn call(args: serde_json::Value) -> Step {
        Step::Call(CallStep {
            call: "x".to_string(),
            args,
            r#as: "agent:scenario".to_string(),
            expect: None,
            capture: Default::default(),
            adverbs: None,
        })
    }

    fn with_adverbs(mut step: Step, adverbs: serde_json::Value) -> Step {
        if let Step::Call(c) = &mut step {
            c.adverbs = Some(adverbs);
        }
        step
    }

    #[test]
    fn adverbs_in_an_args_object_validate_clean() {
        let s = base(vec![with_adverbs(
            call(json!({"a": 1})),
            json!({"tentatively": true}),
        )]);
        assert!(validate(&s).is_empty());
    }

    #[test]
    fn a_step_whose_args_already_has_an_adverbs_key_fails_validation() {
        let s = base(vec![with_adverbs(
            call(json!({"adverbs": {}})),
            json!({"tentatively": true}),
        )]);
        let problems = validate(&s);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].message.contains("already has an `adverbs` key"));
    }

    #[test]
    fn adverbs_need_an_object_args_an_object_value_and_snake_case_keys() {
        let non_object_args = validate(&base(vec![with_adverbs(call(json!([1])), json!({}))]));
        assert_eq!(non_object_args.len(), 1);
        let non_object = validate(&base(vec![with_adverbs(call(json!({})), json!(5))]));
        assert_eq!(non_object.len(), 1);
        let bad_key = validate(&base(vec![with_adverbs(
            call(json!({})),
            json!({"Bad-Key": true}),
        )]));
        assert_eq!(bad_key.len(), 1);
        assert!(bad_key[0].message.contains("Bad-Key"));
    }

    #[test]
    fn empty_steps_is_a_problem() {
        let problems = validate(&base(vec![]));
        assert!(problems
            .iter()
            .any(|p| p.message.contains("at least one step")));
    }

    #[test]
    fn a_capture_referenced_before_it_exists_is_a_problem() {
        let scenario = base(vec![call(json!({"name": "{{state.missing}}"}))]);
        let problems = validate(&scenario);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].message.contains("missing"));
    }

    #[test]
    fn a_seeded_capture_is_known() {
        let mut scenario = base(vec![call(json!({"name": "{{state.paper}}"}))]);
        scenario.seed = vec![SeedRecord {
            kind: "imbib/bibliography-entry".to_string(),
            payload: json!({}),
            r#as: Some("paper".to_string()),
        }];
        assert!(validate(&scenario).is_empty());
    }

    #[test]
    fn a_capture_from_an_earlier_step_is_known_to_a_later_one() {
        let mut first = call(json!({}));
        if let Step::Call(c) = &mut first {
            c.capture
                .insert("name".to_string(), CallCapture::Path("$.name".to_string()));
        }
        let second = call(json!({"name": "{{state.name}}"}));
        let scenario = base(vec![first, second]);
        assert!(validate(&scenario).is_empty());
    }

    #[test]
    fn malformed_capture_operations_are_rejected_structurally() {
        let scenario: Scenario = serde_json::from_value(json!({
            "wire_version": 1,
            "id": "malformed-capture",
            "description": "invalid bounded capture",
            "tier": "a",
            "steps": [{
                "call": "x",
                "capture": {
                    "bad_selector": {"select_one": {
                        "from": " ",
                        "path": "$.{{state.dynamic}}",
                        "predicate": {"equals": "detail"},
                        "object_key_as": "integer"
                    }},
                    "bad_array": {"fill_array": {"value": 1, "length_of": " "}},
                    "bad_argument": {"argument": " "},
                    "dynamic_argument": {"argument": "$.{{state.path}}"}
                }
            }]
        }))
        .unwrap();
        let problems = validate(&scenario);
        assert!(problems
            .iter()
            .any(|p| p.message.contains("select_one paths")));
        assert!(problems.iter().any(|p| p.message.contains("object_key_as")));
        assert!(problems
            .iter()
            .any(|p| p.message.contains("candidate path must be fixed")));
        assert!(problems
            .iter()
            .any(|p| p.message.contains("fill_array length_of")));
        assert!(problems.iter().any(|p| {
            p.message
                .contains("argument capture path must be non-empty")
        }));
        assert!(problems
            .iter()
            .any(|p| p.message.contains("argument capture path must be fixed")));
        assert!(serde_json::from_value::<crate::spec::CallCapture>(json!({
            "argument": "$.ids.0",
            "unexpected": true
        }))
        .is_err());
        assert!(serde_json::from_value::<CallCapture>(json!({
            "select_one": {"from": "$.x", "path": "$.y", "predicate": {"regex": ".*"}}
        }))
        .is_err());
        assert!(serde_json::from_value::<CallCapture>(json!({
            "select_one": {
                "from": "$.x",
                "path": "$.y",
                "predicate": {"equals": 1, "array_contains": 1}
            }
        }))
        .is_err());
    }

    #[test]
    fn fill_array_value_is_literal_not_a_capture_reference() {
        let first = call(json!({}));
        let mut first = first;
        if let Step::Call(call) = &mut first {
            call.capture
                .insert("items".into(), CallCapture::Path("$.items".into()));
        }
        let mut second = call(json!({}));
        if let Step::Call(call) = &mut second {
            call.capture.insert(
                "literal_values".into(),
                CallCapture::FillArray(crate::spec::FillArrayCapture {
                    fill_array: crate::spec::FillArraySpec {
                        value: json!("{{{{state.missing}}}}"),
                        length_of: "{{state.items}}".into(),
                    },
                }),
            );
        }
        assert!(validate(&base(vec![first, second])).is_empty());
    }

    #[test]
    fn log_cursor_and_gesture_captures_are_known_to_later_steps() {
        let scenario: Scenario = serde_json::from_value(json!({
            "wire_version": 1,
            "id": "cursor",
            "description": "captured cursors",
            "tier": "b",
            "steps": [
                {"wait": {"log_cursor": {"capture": "before"}}},
                {"gesture": {"verb": "split"}, "capture": {"tile": "$.focused"}},
                {"wait": {"log": {
                    "category": "layout",
                    "contains": "pane {{state.tile}}",
                    "also_contains": ["Console"],
                    "after": "{{state.before}}",
                    "timeout_ms": 3000
                }}}
            ]
        }))
        .unwrap();
        assert!(validate(&scenario).is_empty());
    }

    #[test]
    fn log_wait_requires_prior_cursor_and_a_bounded_nonempty_needle_set() {
        let scenario: Scenario = serde_json::from_value(json!({
            "wire_version": 1,
            "id": "bad-log-wait",
            "description": "invalid log wait",
            "tier": "b",
            "steps": [{"wait": {"log": {
                "category": "layout",
                "contains": "",
                "also_contains": [""],
                "after": "{{state.missing}}",
                "timeout_ms": 60001
            }}}]
        }))
        .unwrap();
        let problems = validate(&scenario);
        assert!(problems.iter().any(|p| p.message.contains("missing")));
        assert!(problems.iter().any(|p| p.message.contains("1..=60000")));
        assert!(problems
            .iter()
            .any(|p| p.message.contains("non-empty message needles")));
    }

    #[test]
    fn wait_presence_guard_requires_a_prior_capture_and_closed_path() {
        let first = call(json!({}));
        let wait = |path: &str| {
            Step::Wait(crate::spec::WaitStep {
                wait: crate::spec::WaitBody::Log {
                    log: crate::spec::LogWait {
                        category: "layout".into(),
                        contains: "detail".into(),
                        also_contains: vec![],
                        after: None,
                        timeout_ms: 100,
                    },
                },
                when_present: Some(path.into()),
            })
        };
        let mut captured = first;
        if let Step::Call(c) = &mut captured {
            c.capture.insert(
                "detail".into(),
                crate::spec::CallCapture::Path("$.detail".into()),
            );
        }
        assert!(validate(&base(vec![captured, wait("$.detail.tile")])).is_empty());
        for (path, expected) in [
            ("$.missing.tile", "has not been set"),
            ("$.detail..tile", "non-empty dotted path segments"),
            ("$.detail[*]", "non-empty dotted path segments"),
            ("detail.tile", "must start with"),
            ("$", "must start with"),
        ] {
            let scenario = base(vec![wait(path)]);
            assert!(
                validate(&scenario)
                    .iter()
                    .any(|problem| problem.message.contains(expected)),
                "{path}"
            );
        }
    }
}
