//! The trigger engine (plan-self-reflective-layer.md § Workflows, W2): turns
//! `(enabled workflows, clock, signals since the last tick)` into which
//! workflows fire right now. Pure — no store I/O, no sleeping; a host (W2's
//! two: `impel-taskd`'s fourth spawn rule and the app's FFI tick) resolves
//! the store scans into [`Signal`]s and calls [`tick`] with them.
//!
//! **The 90-second rule has one owner.** [`EngineCursors::started_at_ms`] is
//! stamped once, when a host creates its engine, and no [`tick`] call
//! produces a run before `started_at_ms + start_delay_ms` — a Swift service
//! no longer has to remember this (`no_run_before_start_delay` below).
//!
//! **Store/job/call/message triggers are host-resolved signals**, not a
//! kind this crate scans for: [`Signal`] already names the workflow it
//! matched (the host did that match, since it alone can query the store).
//! **Schedule triggers are computed here**, from the clock and a persisted
//! per-workflow `last_schedule_run_ms`, because nothing external need be
//! watched.
//!
//! **Debounce** (`store`/`message` triggers) is per workflow, not per
//! signal: two signals in the same tick collapse to at most one run, and a
//! signal inside the previous run's debounce window is dropped
//! (`store_trigger_fires_once_per_debounce_window` below).

use std::collections::BTreeMap;

use serde_json::Value;

use crate::spec::{Trigger, WorkflowSpec, WorkflowState};

/// A source of "now", injected so a test drives the engine without a real
/// sleep (plan § Workflows' Tier A list: "no run before `start_delay`").
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}

/// The real clock — wall time in milliseconds.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}

/// A signal a host resolved from its own scan (a store row matching a
/// workflow's declared `kinds`/`ops`, a `task@1.0.0` row reaching the
/// declared `state`, or a `core/verb-call@1.0.0` row naming the declared
/// verb) — already attributed to the one workflow it fired for.
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    pub workflow_id: String,
    pub at_ms: i64,
    /// The trigger's own payload — what the fired workflow's `plan` call
    /// wraps as `{"widget": "trigger", "kind": "submit", "value": …}`.
    pub trigger_value: Value,
}

/// One workflow that fired: its id and the trigger's payload, ready for
/// [`crate::plan::plan`].
#[derive(Debug, Clone, PartialEq)]
pub struct DueRun {
    pub workflow_id: String,
    pub trigger_value: Value,
}

/// Per-workflow state the engine must remember across ticks.
#[derive(Debug, Clone, Copy, Default)]
struct WorkflowCursor {
    last_schedule_run_ms: Option<i64>,
    last_fire_ms: Option<i64>,
}

/// The engine's whole persisted state: when the host started (the
/// `start_delay` anchor) and every workflow's per-kind cursor. A host
/// persists this across restarts however it likes (impel-taskd's
/// `ScanCursors` precedent) — `EngineCursors` itself does no I/O.
#[derive(Debug, Clone)]
pub struct EngineCursors {
    pub started_at_ms: i64,
    per_workflow: BTreeMap<String, WorkflowCursor>,
}

impl EngineCursors {
    /// A fresh engine, anchored at `started_at_ms` — the instant the host
    /// process (or app launch) began. Every `tick` before
    /// `started_at_ms + start_delay_ms` returns nothing, regardless of what
    /// signals are handed in.
    pub fn new(started_at_ms: i64) -> Self {
        Self {
            started_at_ms,
            per_workflow: BTreeMap::new(),
        }
    }
}

/// Parses a plan-vocabulary duration (`"90s"`, `"24h"`, `"5m"`) to
/// milliseconds. Unitless or unparseable is `None` — the caller treats an
/// unparseable `every` as "never due" rather than guessing.
pub fn parse_duration_ms(text: &str) -> Option<i64> {
    let text = text.trim();
    let (number, unit) = text.split_at(text.find(|c: char| !c.is_ascii_digit())?);
    let n: i64 = number.parse().ok()?;
    let multiplier = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return None,
    };
    Some(n * multiplier)
}

/// The tick: which enabled workflows fire right now. `workflows` is every
/// `state: enabled` row (a `proposed`/`disabled`/`broken` row is never
/// passed in — the host filters before calling, and this function has no
/// way to run one that is not enabled). `signals` is every host-resolved
/// store/job/call/message match since the last tick, in any order.
pub fn tick(
    workflows: &[(String, WorkflowSpec)],
    now_ms: i64,
    start_delay_ms: i64,
    cursors: &mut EngineCursors,
    signals: &[Signal],
) -> Vec<DueRun> {
    let mut due = Vec::new();
    if now_ms < cursors.started_at_ms.saturating_add(start_delay_ms) {
        return due;
    }
    for (id, workflow) in workflows {
        if workflow.state != WorkflowState::Enabled {
            continue;
        }
        match &workflow.trigger {
            Trigger::Schedule { every, .. } => {
                let Some(every_ms) = parse_duration_ms(every) else {
                    continue;
                };
                let cursor = cursors.per_workflow.entry(id.clone()).or_default();
                let ready_at = cursor
                    .last_schedule_run_ms
                    .map(|t| t + every_ms)
                    .unwrap_or(cursors.started_at_ms + start_delay_ms);
                if now_ms >= ready_at {
                    cursor.last_schedule_run_ms = Some(now_ms);
                    due.push(DueRun {
                        workflow_id: id.clone(),
                        trigger_value: serde_json::json!({"tick_at_ms": now_ms}),
                    });
                }
            }
            Trigger::Store { .. } | Trigger::Message { .. } => {
                let debounce = match &workflow.trigger {
                    Trigger::Store { debounce_ms, .. } => debounce_ms.unwrap_or(0) as i64,
                    _ => 0,
                };
                let mut matched: Vec<&Signal> =
                    signals.iter().filter(|s| &s.workflow_id == id).collect();
                matched.sort_by_key(|s| s.at_ms);
                let cursor = cursors.per_workflow.entry(id.clone()).or_default();
                for signal in matched {
                    let ready = cursor
                        .last_fire_ms
                        .map(|last| signal.at_ms - last >= debounce)
                        .unwrap_or(true);
                    if ready {
                        cursor.last_fire_ms = Some(signal.at_ms);
                        due.push(DueRun {
                            workflow_id: id.clone(),
                            trigger_value: signal.trigger_value.clone(),
                        });
                    }
                }
            }
            Trigger::Job { .. } | Trigger::Call { .. } => {
                // Every matched signal fires — a finished job or a completed
                // call is a discrete event, not a debounced stream.
                for signal in signals.iter().filter(|s| &s.workflow_id == id) {
                    due.push(DueRun {
                        workflow_id: id.clone(),
                        trigger_value: signal.trigger_value.clone(),
                    });
                }
            }
            Trigger::Manual {} => {
                for signal in signals.iter().filter(|s| &s.workflow_id == id) {
                    due.push(DueRun {
                        workflow_id: id.clone(),
                        trigger_value: signal.trigger_value.clone(),
                    });
                }
            }
        }
    }
    due
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::spec::{Author, Guards, Review};

    fn workflow(trigger: Trigger) -> WorkflowSpec {
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
            steps: vec![],
            review: Review::default(),
        }
    }

    /// The proof: no run before `start_delay`, whatever signals arrive.
    #[test]
    fn no_run_before_start_delay() {
        let wf = workflow(Trigger::Manual {});
        let mut cursors = EngineCursors::new(0);
        let signal = Signal {
            workflow_id: "wf-1".into(),
            at_ms: 10_000,
            trigger_value: serde_json::json!({}),
        };
        let due = tick(
            &[("wf-1".into(), wf)],
            10_000, // 10s < 90s start_delay
            90_000,
            &mut cursors,
            std::slice::from_ref(&signal),
        );
        assert!(due.is_empty(), "must not run before start_delay: {due:?}");

        let due = tick(
            &[("wf-1".into(), workflow(Trigger::Manual {}))],
            90_001,
            90_000,
            &mut cursors,
            &[signal],
        );
        assert_eq!(
            due.len(),
            1,
            "the same signal fires once start_delay has passed"
        );
    }

    /// A `store` trigger fires once per debounce window even when several
    /// signals land in it.
    #[test]
    fn store_trigger_fires_once_per_debounce_window() {
        let wf = workflow(Trigger::Store {
            kinds: vec!["imbib/bibliography-entry".into()],
            ops: vec!["insert".into()],
            debounce_ms: Some(5_000),
        });
        let mut cursors = EngineCursors::new(0);
        let signals = vec![
            Signal {
                workflow_id: "wf-1".into(),
                at_ms: 100_000,
                trigger_value: serde_json::json!({"n": 1}),
            },
            Signal {
                workflow_id: "wf-1".into(),
                at_ms: 101_000, // 1s later — inside the 5s window
                trigger_value: serde_json::json!({"n": 2}),
            },
        ];
        let due = tick(&[("wf-1".into(), wf)], 101_000, 0, &mut cursors, &signals);
        assert_eq!(
            due.len(),
            1,
            "two signals inside one debounce window collapse to one run"
        );
        assert_eq!(
            due[0].trigger_value["n"], 1,
            "the first signal in the window is the one that fires"
        );

        // A signal outside the window fires again.
        let wf2 = workflow(Trigger::Store {
            kinds: vec!["imbib/bibliography-entry".into()],
            ops: vec!["insert".into()],
            debounce_ms: Some(5_000),
        });
        let later = Signal {
            workflow_id: "wf-1".into(),
            at_ms: 200_000,
            trigger_value: serde_json::json!({"n": 3}),
        };
        let due = tick(
            &[("wf-1".into(), wf2)],
            200_000,
            0,
            &mut cursors,
            std::slice::from_ref(&later),
        );
        assert_eq!(
            due.len(),
            1,
            "outside the debounce window, the next signal fires"
        );
    }

    /// A `job` trigger fires on `done` — every matched signal is a run,
    /// with no debounce (a finished job is a discrete event).
    #[test]
    fn job_trigger_fires_on_done() {
        let wf = workflow(Trigger::Job {
            verb: "imprint-project-service_project-build".into(),
            state: "done".into(),
        });
        let mut cursors = EngineCursors::new(0);
        let signal = Signal {
            workflow_id: "wf-1".into(),
            at_ms: 1_000,
            trigger_value: serde_json::json!({"job_id": "j-1"}),
        };
        let due = tick(
            &[("wf-1".into(), wf)],
            1_000,
            0,
            &mut cursors,
            std::slice::from_ref(&signal),
        );
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].trigger_value["job_id"], "j-1");
    }

    /// A `disabled` or `proposed` workflow never fires, whatever signals or
    /// schedule ticks arrive — `state: enabled` is the only state this
    /// function is ever handed, but the check stays as a second line of
    /// defense against a host that forgot to filter.
    #[test]
    fn a_workflow_not_enabled_never_fires() {
        let mut wf = workflow(Trigger::Manual {});
        wf.state = WorkflowState::Disabled;
        let mut cursors = EngineCursors::new(0);
        let signal = Signal {
            workflow_id: "wf-1".into(),
            at_ms: 1_000,
            trigger_value: serde_json::json!({}),
        };
        let due = tick(&[("wf-1".into(), wf)], 1_000, 0, &mut cursors, &[signal]);
        assert!(due.is_empty());
    }

    /// A `schedule` trigger becomes due `every` after the last run (or
    /// after `start_delay`, for the first run), never before.
    #[test]
    fn schedule_trigger_runs_every_interval_after_start_delay() {
        let wf = workflow(Trigger::Schedule {
            every: "60s".into(),
            at: None,
        });
        let mut cursors = EngineCursors::new(0);
        // Before start_delay: nothing.
        let due = tick(
            &[("wf-1".into(), wf.clone())],
            89_999,
            90_000,
            &mut cursors,
            &[],
        );
        assert!(due.is_empty());
        // Exactly at start_delay: first run.
        let due = tick(
            &[("wf-1".into(), wf.clone())],
            90_000,
            90_000,
            &mut cursors,
            &[],
        );
        assert_eq!(due.len(), 1);
        // 30s later: not yet due (interval is 60s).
        let due = tick(
            &[("wf-1".into(), wf.clone())],
            120_000,
            90_000,
            &mut cursors,
            &[],
        );
        assert!(due.is_empty());
        // 60s after the first run: due again.
        let due = tick(&[("wf-1".into(), wf)], 150_000, 90_000, &mut cursors, &[]);
        assert_eq!(due.len(), 1);
    }
}
