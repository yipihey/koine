//! The worked example from `docs/plan-agent-surfaces.md` ("The vocabulary
//! (normative)"), loaded from the committed JSON file rather than built by hand
//! in Rust — so what ships as documentation (the `.surface.json`, which S8's
//! `surface_examples` verb and S9's demo capability both point agents at) is the
//! exact bytes this crate's own tests exercise, never a second, driftable copy.

use crate::spec::SurfaceSpec;

const SIGNAL_EXPLORER_JSON: &str = include_str!("../examples/signal-explorer.surface.json");
const PAPER_TRIAGE_JSON: &str = include_str!("../examples/paper-triage.surface.json");

/// The "Signal explorer" surface from the plan, parsed. Panics if the committed
/// JSON does not parse — which would mean the file and this crate's own types
/// have drifted, a build-time bug worth a hard failure rather than an `Option`
/// every caller has to handle.
pub fn example_signal_explorer() -> SurfaceSpec {
    serde_json::from_str(SIGNAL_EXPLORER_JSON)
        .unwrap_or_else(|e| panic!("examples/signal-explorer.surface.json failed to parse: {e}"))
}

/// The "Paper triage" surface (wave 5 V3, extended in V5): a `query` source
/// over the user's own unread papers (`publication` /
/// `imbib/bibliography-entry`), a table of title/year/first-author, and a row
/// of buttons that star, flag or tag every SELECTED row through the kit
/// `triage-service_*` verbs (`crates/impress-store-service/src/triage_service.rs`)
/// — present in the app, the CLI and MCP alike, never a second definition of
/// triage for surfaces. A table `select` event carries an array of ids
/// (uniform across every widget); each verb still takes exactly one `id`.
/// The buttons bridge that gap with `each: "state.selected"` and
/// `{{item}}` (V5: `docs/plan-agent-surfaces.md`'s vocabulary, `reduce.rs`)
/// rather than a second table shape or a list-taking verb — see
/// `docs/agent-surfaces.md`'s second worked example for the loop an agent
/// runs with it. Parses and panics the same way [`example_signal_explorer`]
/// does.
pub fn example_paper_triage() -> SurfaceSpec {
    serde_json::from_str(PAPER_TRIAGE_JSON)
        .unwrap_or_else(|e| panic!("examples/paper-triage.surface.json failed to parse: {e}"))
}

const JOB_MONITOR_JSON: &str = include_str!("../examples/job-monitor.surface.json");

/// The "Job monitor" surface (ADR-0034 D6, plan P4): a `status` widget bound
/// to `impel-service_job-status`, a `log` of `impel-service_job-events`, and
/// a Cancel button that calls `impel-service_job-cancel` and `emit`s — so an
/// agent that started a long-running verb can show the person its progress
/// in a pane and learn through `surface_wait` when they stopped it. The job
/// is the surface's one `param`, a `task@1.0.0` row: the handle every
/// long-running verb answers with. Parses and panics the same way
/// [`example_signal_explorer`] does.
pub fn example_job_monitor() -> SurfaceSpec {
    serde_json::from_str(JOB_MONITOR_JSON)
        .unwrap_or_else(|e| panic!("examples/job-monitor.surface.json failed to parse: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::validate;

    /// The job monitor binds the `status` widget to a job and its cancel
    /// button to `job_cancel` (ADR-0034 D6); it validates clean and names
    /// exactly the three `impel-service_job-*` verbs.
    #[test]
    fn job_monitor_parses_and_validates_clean() {
        let spec = example_job_monitor();
        assert_eq!(spec.name, "Job monitor");
        assert_eq!(spec.params.len(), 1);
        assert_eq!(spec.params[0].kind, "task@1.0.0");
        let problems = validate(&spec);
        assert!(problems.is_empty(), "unexpected problems: {problems:?}");
        let text = serde_json::to_string(&spec).unwrap();
        for verb in [
            "impel-service_job-status",
            "impel-service_job-events",
            "impel-service_job-cancel",
        ] {
            assert!(text.contains(verb), "the monitor must use {verb}");
        }
    }

    #[test]
    fn the_worked_example_parses_and_validates_clean() {
        let spec = example_signal_explorer();
        assert_eq!(spec.surface, "1.0");
        assert_eq!(spec.name, "Signal explorer");
        let problems = validate(&spec);
        assert!(problems.is_empty(), "unexpected problems: {problems:?}");
    }

    /// The `on_select` handler sets `state.selected` straight from
    /// `{{event.value}}` — an array of ids, the uniform shape every `select`
    /// event carries (V5). Each button's `on_click` then fans out over it:
    /// `each: "state.selected"` runs its `call`/`emit` once per selected id,
    /// with `{{item}}` bound to that one id for the duration
    /// (`template.rs`'s numeric path segments — `{{state.selected.0}}` — are
    /// how a spec would single out just one, if it only ever wanted one).
    /// `triage-service_*` keeps its own one-id signature throughout; `each`
    /// is what lets a uniform multi-select event drive it without either
    /// side bending its shape — see `reduce.rs`'s module docs and this
    /// crate's (V5) report.
    #[test]
    fn paper_triage_parses_and_validates_clean() {
        let spec = example_paper_triage();
        assert_eq!(spec.surface, "1.0");
        assert_eq!(spec.name, "Paper triage");
        let problems = validate(&spec);
        assert!(problems.is_empty(), "unexpected problems: {problems:?}");
    }
}
