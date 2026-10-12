//! The ADR-0031 D8 verbs, as an `#[impress_service]` trait over a store-backed
//! layout tree (work package L3 of `docs/plan-layout-tree.md`).
//!
//! # What this crate adds to `impress-layout`
//!
//! `crates/impress-layout` is pure: a [`Layout`](impress_layout::Layout) is a
//! value, every gesture is a [`Verb`](impress_layout::Verb), and applying one
//! returns a patch that reverts exactly. It has no I/O by design, which is
//! what makes the whole model testable with no app running.
//!
//! This crate is the other half — the three things a pure tree cannot do:
//!
//! 1. **Persistence.** The live arrangement of every `(app_id, device)` scope
//!    is an `impress/ui/layout@1.0.0` row ([`store`]), written through the
//!    store's operation path so it is attributed, intent-bearing and tiered:
//!    `Ephemeral` for the gestures, `Durable` for the commits (ADR-0019 D2/D6,
//!    ADR-0031 D7).
//! 2. **Continuity.** The undo rings are in-memory per scope ([`session`]),
//!    because a ring is a record of *this* sitting and restoring one from a
//!    file would let ⌘Z revert a gesture made on another device last week.
//! 3. **Reach.** One `#[impress_method]` per verb ([`service`]), so MCP, the
//!    CLI and impel's agent loop get split / move / retype / re-link / save /
//!    recall together — invariant 6 of ADR-0031: no Swift-only layout
//!    operation.
//!
//! # The cut, in one line
//!
//! Nothing here decides anything about the tree. Every verb builds an
//! `impress_layout::Verb`, hands it to the pure applier, and stores what comes
//! back. When something about *layout* is wrong, it is wrong in
//! `impress-layout`; when something about *durability or attribution* is
//! wrong, it is wrong here.
//!
//! # Reading the results
//!
//! Every verb answers with `focused` and `affected_panes` — the focused leaf
//! and the panes to redraw. That pair is the renderer's whole input, and it is
//! why a Swift host can walk this tree without holding layout state of its own
//! (ADR-0019 D3).

use std::future::Future;
use std::time::Instant;

pub mod authorship;
pub mod device;
pub mod dto;
pub mod outline;
pub mod presets;
pub mod rename;
/// The self-test report types: one copy, in `impress-service-core` (RS-S21).
pub use impress_service_core::report;
/// S2b: the shared Tier B `impress_scenario::Caller`, reused by
/// `impress-surface-service` and `impress-scenario-service`. See its module
/// docs for why it lives here rather than in `impress-scenario` itself.
pub mod scenario_caller;
pub mod selftest;
pub mod service;
pub mod session;
pub mod store;
pub mod tier_a;
pub mod tier_b;

pub use device::{current_device, resolve_device};
pub use dto::{
    ChannelResult, CompiledQueryDto, LayoutListResult, LayoutResult, LayoutVerbResult,
    MaterializeFirstDto, PaneRefDto, PaneResult, PatchSummary, PresetDto, PresetListResult,
    PresetResult, ReferenceResult, SavedLayoutDto,
};
pub use outline::{
    initial_selection_applies, outline_cleared_verbs, outline_sections, outline_target,
    outline_verbs, OutlineNode, OutlinePanes, OutlineSection, OutlineTarget, RecordScope,
};
pub use presets::{
    is_superseded_list_query, named_queries, ordinal_targets, preset_id, previous_revision,
    section_bindings, shipped_list_queries, shipped_preset, shipped_presets, shipped_presets_for,
    OrdinalTarget, PresetRow, PresetStore, ShippedPreset, StoredPreset, HIDDEN_SHARE,
    MATERIALIZE_FIRST,
};
pub use report::{CapabilityResult, SelfTestReport, Tier};
pub use selftest::{DefaultLayoutSelftestService, LayoutSelftestService};
pub use service::{CompiledPane, DefaultLayoutService, LayoutService};
pub use session::{AppliedVerb, LayoutSession, SessionRegistry, Stack, UndoTarget};
pub use store::{
    actor_from, author_for, cold_start_layout, default_list_query, LayoutRow, LayoutStore,
    LiveLoad, LiveWrite,
};

/// What went wrong, as a sentence the service turns into `ok: false` +
/// `message`.
pub type Result<T> = std::result::Result<T, String>;

/// Run one capability check, timing it and packaging the outcome.
///
/// The check returns `Ok(detail)` on success or `Err(reason)` on failure;
/// either becomes the human-readable evidence in the report.
pub async fn check<F, Fut>(id: &str, description: &str, tier: Tier, body: F) -> CapabilityResult
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<String>>,
{
    let start = Instant::now();
    let outcome = body().await;
    let duration_ms = start.elapsed().as_millis() as u64;
    let (pass, detail) = match outcome {
        Ok(detail) => (true, detail),
        Err(reason) => (false, reason),
    };
    CapabilityResult {
        id: id.to_string(),
        description: description.to_string(),
        tier,
        pass,
        detail,
        duration_ms,
        skipped: false,
    }
}

/// A capability that could not run. A skip is not a pass: `pass` is false,
/// `skipped` says why, and the report is not `ok` (review RL-L18).
pub fn skipped(id: &str, description: &str, tier: Tier, reason: &str) -> CapabilityResult {
    CapabilityResult {
        id: id.to_string(),
        description: description.to_string(),
        tier,
        pass: false,
        detail: reason.to_string(),
        duration_ms: 0,
        skipped: true,
    }
}

/// Run the Tier A catalogue: every D8 verb, headless, over private in-memory
/// stores.
pub async fn run_tier_a() -> SelfTestReport {
    SelfTestReport::from_results(tier_a::run().await)
}

/// Run the Tier B catalogue against the running impress app. When nothing is
/// listening, every capability is skipped — reported as skipped, not failed,
/// and the report is not `ok`.
pub async fn run_tier_b() -> SelfTestReport {
    SelfTestReport::from_results(tier_b::run(&tier_b::configured_base_url()).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalogue is the proof that the verbs work, so the catalogue itself
    /// runs as an ordinary `cargo test`. One assertion, and the failure
    /// message names every capability that broke.
    #[tokio::test]
    async fn every_tier_a_capability_passes() {
        // The catalogue reads the process's kind manifest (the host installs
        // it); the suite's, as a test table.
        impress_pane_query::process_manifest::install(std::sync::Arc::new(
            impress_pane_query_compile::test_support::builtin_manifest(),
        ));
        let report = run_tier_a().await;
        assert!(
            report.ok(),
            "{}\nfailures: {:#?}",
            report.summary(),
            report
                .results
                .iter()
                .filter(|r| !r.pass && !r.skipped)
                .map(|r| format!("{}: {}", r.id, r.detail))
                .collect::<Vec<_>>()
        );
        assert!(
            report.total >= 30,
            "the catalogue must cover every D8 verb; it has only {} capabilities",
            report.total
        );
    }
}

#[cfg(test)]
mod inventory_tests {
    use impress_service_core::{CliSubcommand, McpToolDescriptor};

    /// Every D8 verb must reach the MCP inventory — that is the entire point
    /// of the crate, and a missing `#[impress_method]` is otherwise invisible
    /// until an agent cannot find the tool.
    ///
    /// The list is written out rather than derived: a test that asks the
    /// inventory what it contains and then asserts it contains that proves
    /// nothing. This is the closed vocabulary of ADR-0031 D8, spelled once.
    const EXPECTED: [&str; 35] = [
        // arrangement
        "layout-service_split",
        "layout-service_move-tile",
        "layout-service_close",
        "layout-service_swap",
        "layout-service_resize",
        "layout-service_set-container-kind",
        "layout-service_maximize",
        "layout-service_restore",
        "layout-service_detach",
        // content
        "layout-service_set-pane",
        "layout-service_set-query",
        "layout-service_set-view-kind",
        "layout-service_bind-param",
        "layout-service_set-channel",
        "layout-service_set-default-channel",
        "layout-service_set-role",
        // focus / selection
        "layout-service_focus",
        "layout-service_focus-direction",
        "layout-service_select",
        "layout-service_set-window-geometry",
        // persistence
        "layout-service_commit",
        "layout-service_save-layout",
        "layout-service_apply-layout",
        "layout-service_undo",
        "layout-service_redo",
        // read
        "layout-service_get-layout",
        "layout-service_get-pane",
        "layout-service_get-channel",
        "layout-service_resolve-reference",
        "layout-service_list-layouts",
        // presets (L7)
        "layout-service_list-presets",
        "layout-service_apply-preset",
        "layout-service_save-preset",
        "layout-service_reset-preset",
        // the catalogue
        "layout-selftest-service_run-selftest",
    ];

    #[test]
    fn every_verb_reaches_the_mcp_inventory() {
        let names: Vec<&str> = McpToolDescriptor::iter().map(|d| d.name).collect();
        for expected in EXPECTED {
            assert!(
                names.contains(&expected),
                "MCP inventory is missing {expected}; have: {names:?}"
            );
        }
    }

    /// And the same methods must be reachable from the shell, because
    /// "headless" is the point.
    #[test]
    fn every_verb_reaches_the_cli_inventory() {
        let names: Vec<&str> = CliSubcommand::iter().map(|c| c.name).collect();
        for expected in EXPECTED {
            let subcommand = expected
                .split_once('_')
                .map(|(_, verb)| verb)
                .unwrap_or(expected);
            assert!(
                names.contains(&subcommand),
                "CLI inventory is missing {subcommand}; have: {names:?}"
            );
        }
    }

    /// The clap tree the CLI builds from this inventory must actually
    /// construct — every argument schema turned into a flag, no duplicate
    /// subcommand names, no malformed arg.
    ///
    /// `impress-cli` itself cannot be built on every machine (it pulls an ONNX
    /// runtime whose binary is fetched at build time), so proving the CLI half
    /// of the codegen here is the difference between "registered" and
    /// "reachable".
    #[test]
    fn the_cli_tree_builds_with_every_verb_in_it() {
        let app = impress_service_core::cli::build_cli_from_inventory("impress");
        let names: Vec<String> = app
            .get_subcommands()
            .map(|c| c.get_name().to_string())
            .collect();
        for expected in EXPECTED {
            let subcommand = expected
                .split_once('_')
                .map(|(_, verb)| verb)
                .unwrap_or(expected);
            assert!(
                names.contains(&subcommand.to_string()),
                "the clap tree is missing `{subcommand}`; have: {names:?}"
            );
        }
        // clap's own consistency checks: duplicate subcommand or argument
        // names, conflicting shorts, impossible requirements.
        app.debug_assert();
    }

    /// Every tool description must be a real sentence. The pipeline falls back
    /// to "Invoke Service.method" when a doc comment is missing, and 119 of
    /// 133 tools once shipped in exactly that state.
    #[test]
    fn every_verb_describes_itself_to_the_model() {
        for descriptor in McpToolDescriptor::iter() {
            if !descriptor.name.starts_with("layout-") {
                continue;
            }
            assert!(
                !descriptor.description.starts_with("Invoke "),
                "{} has no description of its own",
                descriptor.name
            );
        }
    }
}
