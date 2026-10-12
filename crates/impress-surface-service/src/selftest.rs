//! `SurfaceSelftestService` — the capability catalogue through the codegen
//! pipeline, so `run_selftest` shows up as the MCP tool
//! `surface-selftest-service_run-selftest` and as a CLI subcommand with no
//! hand-written glue — mirroring `impress-layout-service::LayoutSelftestService`
//! exactly (including the tool-name precedent: neither trait's kebab name
//! carries an `impress-` prefix, unlike `ImpressSurfaceService` itself, whose
//! prefix `docs/agent-surfaces.md` fixes explicitly).
//!
//! Confirming a surface capability works must not require clicking a GUI —
//! the same rule `imprint-selftest` and `impress-layout-service`'s own
//! catalogue already keep.

use std::sync::Arc;

use impress_service_core::async_trait;
#[allow(unused_imports)]
use impress_service_macros::impress_method;
use impress_service_macros::{impress_service, impress_service_impl};

use crate::report::SelfTestReport;

/// Run the surface capability self-tests and return a structured report.
#[impress_service]
pub trait SurfaceSelftestService: Send + Sync + 'static {
    /// Run the surface capability self-tests and return the report.
    ///
    /// `tier` is `"a"` (or `""`: pure Rust over a private in-memory store —
    /// every verb plus the create → show → dispatch → render → emit →
    /// events loop), `"b"` (a running app's `/api/surface/*`: every route
    /// answers its verb's result with `wire_version`, strict arguments, an
    /// invalid spec refused at create — at `IMPRESS_SURFACE_SELFTEST_BASE_URL`,
    /// else `IMPRESS_LAYOUT_SELFTEST_BASE_URL`, else impress's port; skipped,
    /// and not ok, when no app answers; its one scratch surface is deleted),
    /// or `"all"` (both). Any other value is refused — a report with one
    /// failed `tier` entry — rather than quietly running Tier A (review
    /// AC-F25). Tier A touches no store but its own; Tier B only its scratch
    /// surface.
    #[impress_method]
    #[impress_example(
        name = "owned-headless-catalogue",
        tier = "b",
        args = r#"{"tier":"a"}"#,
        expect = r#"{"ok":true,"failed":0,"skipped":0}"#
    )]
    async fn run_selftest(&self, tier: String) -> SelfTestReport;
}

/// Default implementation. Stateless.
#[derive(Default, Clone, Copy)]
pub struct DefaultSurfaceSelftestService;

#[async_trait::async_trait]
impl SurfaceSelftestService for DefaultSurfaceSelftestService {
    async fn run_selftest(&self, tier: String) -> SelfTestReport {
        match tier.trim().to_ascii_lowercase().as_str() {
            "b" => crate::run_tier_b().await,
            "a" | "" => crate::run_tier_a().await,
            "all" => {
                let mut report = crate::run_tier_a().await;
                report.results.extend(crate::run_tier_b().await.results);
                SelfTestReport::from_results(report.results)
            }
            other => SelfTestReport::from_results(vec![crate::CapabilityResult {
                id: "tier".to_string(),
                description: "choose a tier".to_string(),
                tier: crate::report::Tier::A,
                pass: false,
                detail: format!("unknown tier '{other}': use a, b or all"),
                duration_ms: 0,
                skipped: false,
            }]),
        }
    }
}

fn selftest_instance() -> Arc<dyn SurfaceSelftestService> {
    Arc::new(DefaultSurfaceSelftestService)
}

impress_service_impl! {
    service = SurfaceSelftestService,
    safety = external,
    since = "0.1.0",
    effects = {
        reads: [any("runs the catalogue's capabilities")],
        writes: [any("runs the catalogue's capabilities")],
        reach: [app("impress")],
    },
    impl = DefaultSurfaceSelftestService,
    instance = || selftest_instance(),
    methods = [
        /// Run the surface capability self-tests (`tier` = a | b | all).
        run_selftest(
            /// Catalogue tier: a for owned headless checks, b for the configured live app, or all for both.
            tier: String
        ) -> SelfTestReport,
    ],
}
