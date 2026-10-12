//! ADR-0033 surface verbs, store records and runtime (`docs/plan-agent-surfaces.md`
//! work package S4).
//!
//! # What this crate adds to `impress-surface`
//!
//! `crates/impress-surface` is pure: a [`impress_surface::SurfaceSpec`] is a
//! value, `plan`/`resolve`/`reduce` are value → value functions, and none of
//! it touches a store or calls a verb. This crate is the other half:
//!
//! 1. **Persistence** ([`store`]) — a surface's spec is an
//!    `impress/ui/surface@1.0.0` row; its working state and the events it
//!    emits are `impress/ui/surface-state@1.0.0` /
//!    `impress/ui/surface-event@1.0.0` rows, written through the store's
//!    operation path exactly as the layout tree's rows are (ADR-0033 D5).
//! 2. **Execution** ([`runtime`]) — an [`runtime::Executor`] that runs a
//!    source's verb through the linked `#[impress_service]` inventory
//!    (ADR-0033 D4) or a query through the store, and a
//!    [`runtime::SurfaceRuntime`] that holds one `(surface, host)`
//!    instance's state/params/source-cache between calls, in a
//!    [`runtime::SessionRegistry`] shaped like the layout tree's own.
//! 3. **Reach** ([`service`]) — one `#[impress_method]` per verb, so MCP,
//!    the CLI and impel's agent loop get the whole five-verb loop
//!    (`surface_schema` → author → `surface_validate` → `surface_create` →
//!    `surface_show` → `surface_wait` → react) together.
//!
//! # The cut, in one line
//!
//! Nothing here decides anything about what a spec MEANS. Every verb either
//! reads/writes a store row unchanged, or hands a value to
//! `impress-surface`'s pure functions and stores what comes back. When
//! something about the VOCABULARY is wrong, it is wrong in
//! `crates/impress-surface`; when something about persistence, execution or
//! reach is wrong, it is wrong here.

use std::future::Future;
use std::time::Instant;

pub mod dto;
/// The `/api/surface/*` route table, shared by the Swift FFI and a headless host.
pub mod http;
/// The self-test report types: one copy, in `impress-service-core` (RS-S21).
pub use impress_service_core::report;
pub mod rename;
pub mod runtime;
pub mod selftest;
pub mod service;
pub mod store;
pub mod tier_a;
pub mod tier_b;

pub use report::{CapabilityResult, SelfTestReport, Tier};
pub use runtime::{
    verb_exists, DefaultExecutor, Executor, PaneHandle, SessionRegistry, SurfaceRuntime, VerbHost,
};
pub use selftest::{DefaultSurfaceSelftestService, SurfaceSelftestService};
pub use service::{call_verb_on, DefaultImpressSurfaceService, ImpressSurfaceService};
pub use store::{EventRow, SurfaceRow, SurfaceStore};

/// What went wrong: a stable `code` and a sentence (see
/// `impress_service_core::refusal`). A bare `String` error converts into an
/// `invalid-argument` refusal through `?`, so argument parsers stay plain;
/// store, lookup and conflict failures are built with their own code.
pub type Result<T> = std::result::Result<T, impress_service_core::Refusal>;

/// Run one capability check, timing it and packaging the outcome. Copied
/// from `impress-layout-service`'s function of the same name/shape (see
/// `report.rs`'s module docs for why these two small report types are a copy
/// rather than a shared dependency).
pub async fn check<F, Fut>(id: &str, description: &str, tier: Tier, body: F) -> CapabilityResult
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = std::result::Result<String, String>>,
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

/// Run the Tier A catalogue: every S4 verb, headless, over a private
/// in-memory store.
pub async fn run_tier_a() -> SelfTestReport {
    SelfTestReport::from_results(tier_a::run().await)
}

/// Run the Tier B catalogue against the app `tier_b::configured_base_url`
/// names (see that module).
pub async fn run_tier_b() -> SelfTestReport {
    SelfTestReport::from_results(tier_b::run().await)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalogue is the proof the verbs work, so it runs as an ordinary
    /// `cargo test` — the same discipline `impress-layout-service` uses.
    #[tokio::test]
    async fn every_tier_a_capability_passes() {
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
    }
}

#[cfg(test)]
mod inventory_tests {
    use impress_service_core::McpToolDescriptor;

    /// Every S4 verb must reach the MCP inventory — a missing
    /// `#[impress_method]` is otherwise invisible until an agent cannot find
    /// the tool (the same test `impress-layout-service` runs on its own
    /// verb list).
    const EXPECTED: [&str; 15] = [
        "impress-surface-service_surface-schema",
        "impress-surface-service_surface-validate",
        "impress-surface-service_surface-create",
        "impress-surface-service_surface-update",
        "impress-surface-service_surface-get",
        "impress-surface-service_surface-list",
        "impress-surface-service_surface-delete",
        "impress-surface-service_surface-show",
        "impress-surface-service_surface-render",
        "impress-surface-service_surface-state-get",
        "impress-surface-service_surface-state-set",
        "impress-surface-service_surface-dispatch",
        "impress-surface-service_surface-events",
        "impress-surface-service_surface-wait",
        "impress-surface-service_surface-examples",
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

    /// Every tool description must be a real sentence, not the macro's
    /// "Invoke Service.method" fallback.
    #[test]
    fn every_verb_describes_itself_to_the_model() {
        for descriptor in McpToolDescriptor::iter() {
            if !descriptor.name.starts_with("impress-surface-service_") {
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

/// Install the suite's built-in kind manifest as the process's, once: the
/// stand-in for the host that does it in production
/// (`impress_store_service::register_process_ports`).
#[cfg(test)]
pub(crate) fn install_test_manifest() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        impress_pane_query::process_manifest::install(std::sync::Arc::new(
            impress_pane_query_compile::test_support::builtin_manifest(),
        ));
    });
}
