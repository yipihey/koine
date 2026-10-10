//! `validate(&WorkflowSpec) -> Vec<Problem>` (plan § Workflows,
//! `workflow-service_validate`). Runs the surface validator's own checks
//! over a synthetic spec ([`crate::plan::to_surface_spec`]) plus the
//! workflow-specific findings the plan names: an unknown verb (checked by
//! the caller, which has the inventory — see [`UnknownVerb`] below, left as
//! a hook this crate cannot fill on its own since it is pure and the
//! inventory lives in a service crate), a `publish`/`open` step (refused —
//! a workflow has no pane), and a `schedule` trigger under 60 seconds.
//!
//! The one check the plan calls out that this crate genuinely cannot make
//! ("a step calling a verb whose `effects.writes` intersect a kind the
//! trigger listens to without `debounce_ms`") needs the verb inventory's
//! `effects` declarations, which live in `impress-capabilities` — outside
//! the kit line this crate must not cross (`pure` tier). `validate_with`
//! below takes a small callback so `impress-workflow-service` can supply
//! that inventory without this crate depending on it.

use impress_surface::spec::Action;
pub use impress_surface::validate::{Problem, Severity};

use crate::plan::to_surface_spec;
use crate::spec::{Trigger, WorkflowSpec};

/// The shortest a `schedule` trigger's `every` may be (plan § Workflows,
/// "a `schedule` under 60 s").
pub const MIN_SCHEDULE_SECONDS: u64 = 60;

/// What `validate_with`'s effects callback answers for one verb: the kinds
/// it reads and writes, or `None` when the verb is not in the inventory at
/// all (an "unknown verb" finding).
pub struct VerbEffects {
    pub writes: Vec<String>,
}

/// Every problem with a workflow, structural (reusing
/// `impress_surface::validate::validate` over the synthetic spec) plus the
/// workflow-only findings this crate can check without an inventory.
pub fn validate(workflow: &WorkflowSpec) -> Vec<Problem> {
    validate_with(workflow, |_verb| None)
}

/// Like [`validate`], but `verb_effects` may answer the feedback-loop check
/// (a step's verb writing a kind the trigger listens to, with no
/// `debounce_ms`) and the unknown-verb check, given each step's verb name.
/// Returning `None` means "cannot say" and skips both for that verb, which
/// is what the pure [`validate`] does for every verb.
pub fn validate_with(
    workflow: &WorkflowSpec,
    verb_effects: impl Fn(&str) -> Option<VerbEffects>,
) -> Vec<Problem> {
    let surface = to_surface_spec(workflow, serde_json::json!({}));
    let mut problems = impress_surface::validate::validate(&surface);

    if workflow.steps.is_empty() {
        problems.push(Problem::warning(
            "/steps",
            "a workflow with no steps never does anything",
        ));
    }

    for (i, step) in workflow.steps.iter().enumerate() {
        match step {
            Action::Publish { .. } => problems.push(Problem::error(
                format!("/steps/{i}"),
                "a workflow has no pane; `publish` is refused",
            )),
            Action::Open { .. } => problems.push(Problem::error(
                format!("/steps/{i}"),
                "a workflow has no pane; `open` is refused",
            )),
            Action::Call { verb, .. } => {
                if let Some(effects) = verb_effects(verb) {
                    let (trigger_kinds, debounced): (Vec<&str>, bool) = match &workflow.trigger {
                        Trigger::Store {
                            kinds, debounce_ms, ..
                        } => (
                            kinds.iter().map(String::as_str).collect(),
                            debounce_ms.is_some(),
                        ),
                        Trigger::Message { kind, .. } => (vec![kind.as_str()], false),
                        _ => (vec![], false),
                    };
                    let overlaps = effects
                        .writes
                        .iter()
                        .any(|w| trigger_kinds.contains(&w.as_str()));
                    if overlaps && !debounced {
                        problems.push(Problem::error(
                            format!("/steps/{i}"),
                            format!(
                                "`{verb}` writes a kind this workflow's trigger listens \
                                 to, with no `debounce_ms` — a feedback loop"
                            ),
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    if let Trigger::Schedule { every, .. } = &workflow.trigger {
        match parse_duration_seconds(every) {
            Some(secs) if secs < MIN_SCHEDULE_SECONDS => {
                problems.push(Problem::error(
                    "/trigger/schedule/every",
                    format!(
                        "a schedule under {MIN_SCHEDULE_SECONDS}s ('{every}' = {secs}s) is refused"
                    ),
                ));
            }
            None => {
                problems.push(Problem::error(
                    "/trigger/schedule/every",
                    format!(
                        "'{every}' is not a duration this build understands (e.g. '24h', '90s')"
                    ),
                ));
            }
            _ => {}
        }
    }

    if workflow.review.required
        && !workflow.author.is_agent()
        && workflow.state == crate::spec::WorkflowState::Proposed
    {
        // Not an error: a person may propose a workflow for review too. Kept
        // as the one place this rule is spelled out, so a future finding
        // has somewhere to attach.
    }

    problems
}

/// Parses `"90s"`, `"24h"`, `"1d"` into whole seconds. `None` for anything
/// else (unit-less, unknown unit, non-numeric).
fn parse_duration_seconds(s: &str) -> Option<u64> {
    let s = s.trim();
    let (digits, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
    let n: u64 = digits.parse().ok()?;
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => return None,
    };
    Some(n * mult)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::spec::{Author, Guards, Review, WorkflowState};

    fn base(trigger: Trigger, steps: Vec<Action>) -> WorkflowSpec {
        WorkflowSpec {
            wire_version: 1,
            name: "fixture".into(),
            description: String::new(),
            state: WorkflowState::Enabled,
            author: Author {
                kind: "person".into(),
                name: None,
            },
            trigger,
            guards: Guards::default(),
            params: vec![],
            sources: BTreeMap::new(),
            steps,
            review: Review::default(),
        }
    }

    /// Fixture: a schedule under 60s is refused.
    #[test]
    fn schedule_under_60s_is_refused() {
        let wf = base(
            Trigger::Schedule {
                every: "30s".into(),
                at: None,
            },
            vec![],
        );
        let problems = validate(&wf);
        assert!(problems
            .iter()
            .any(|p| p.severity == Severity::Error && p.path == "/trigger/schedule/every"));
    }

    /// Fixture: a schedule at or above 60s passes that check.
    #[test]
    fn schedule_at_60s_is_accepted() {
        let wf = base(
            Trigger::Schedule {
                every: "60s".into(),
                at: None,
            },
            vec![Action::Set {
                path: "state.x".into(),
                value: serde_json::json!(1),
            }],
        );
        let problems = validate(&wf);
        assert!(!problems.iter().any(|p| p.path == "/trigger/schedule/every"));
    }

    /// Fixture: an unparseable duration is refused.
    #[test]
    fn unparseable_duration_is_refused() {
        let wf = base(
            Trigger::Schedule {
                every: "often".into(),
                at: None,
            },
            vec![],
        );
        let problems = validate(&wf);
        assert!(problems
            .iter()
            .any(|p| p.severity == Severity::Error && p.path == "/trigger/schedule/every"));
    }

    /// Fixture: `publish` in steps is refused — a workflow has no pane.
    #[test]
    fn publish_step_is_refused() {
        let wf = base(Trigger::Manual {}, vec![Action::Publish { ids: None }]);
        let problems = validate(&wf);
        assert!(problems.iter().any(|p| p.severity == Severity::Error
            && p.path == "/steps/0"
            && p.message.contains("publish")));
    }

    /// Fixture: `open` in steps is refused — a workflow has no pane.
    #[test]
    fn open_step_is_refused() {
        let wf = base(
            Trigger::Manual {},
            vec![Action::Open {
                query: serde_json::json!({}),
                view_kind: "list".into(),
                target: None,
            }],
        );
        let problems = validate(&wf);
        assert!(problems.iter().any(|p| p.severity == Severity::Error
            && p.path == "/steps/0"
            && p.message.contains("open")));
    }

    /// Fixture: no steps at all is a warning, not an error.
    #[test]
    fn empty_steps_is_a_warning() {
        let wf = base(Trigger::Manual {}, vec![]);
        let problems = validate(&wf);
        let p = problems.iter().find(|p| p.path == "/steps").unwrap();
        assert_eq!(p.severity, Severity::Warning);
    }

    /// Fixture: a step's verb writing the trigger's own kind with no
    /// `debounce_ms` is a feedback loop, refused by `validate_with`.
    #[test]
    fn feedback_loop_is_refused_when_effects_are_known() {
        let wf = base(
            Trigger::Store {
                kinds: vec!["imbib/bibliography-entry".into()],
                ops: vec!["insert".into()],
                debounce_ms: None,
            },
            vec![Action::Call {
                verb: "imbib-library-service_touch".into(),
                args: serde_json::json!({}),
                into: None,
                each: None,
            }],
        );
        let problems = validate_with(&wf, |_| {
            Some(VerbEffects {
                writes: vec!["imbib/bibliography-entry".into()],
            })
        });
        assert!(problems
            .iter()
            .any(|p| p.severity == Severity::Error && p.message.contains("feedback loop")));
    }

    /// Fixture: the same shape, but the trigger is debounced — no loop finding.
    #[test]
    fn debounced_store_trigger_is_not_a_feedback_loop() {
        let wf = base(
            Trigger::Store {
                kinds: vec!["imbib/bibliography-entry".into()],
                ops: vec!["insert".into()],
                debounce_ms: Some(5000),
            },
            vec![Action::Call {
                verb: "imbib-library-service_touch".into(),
                args: serde_json::json!({}),
                into: None,
                each: None,
            }],
        );
        let problems = validate_with(&wf, |_| {
            Some(VerbEffects {
                writes: vec!["imbib/bibliography-entry".into()],
            })
        });
        assert!(!problems.iter().any(|p| p.message.contains("feedback loop")));
    }
}
