//! `koine-host`: serve the linked verbs, generated surfaces, the layout, the
//! `koine-web` page and MCP from one process.
//!
//! ```text
//! koine-host --store dir:~/koine-demo/store --port 23130 --web-dir <public>
//! koine-host --store memory: --port 0 --check      # start, print, exit 0
//! ```
//!
//! It links `surface-demo-service` (`series`, `histogram` and `tick`) and
//! the two runtime crates; a bespoke app is this same `main` with its own
//! service crate linked instead (`scripts/new-app.sh`). The flags and the run
//! loop are `koine_host::cli`, shared with that scaffold.

use std::process::ExitCode;

fn main() -> ExitCode {
    // The runtimes' registrations, and the demo's, must survive the link.
    koine_host::ensure_linked();
    std::hint::black_box(surface_demo_service::DefaultSurfaceDemoService::new as *const ());
    // The demo host is the tour (docs/plan-koine-tour.md): once bound, seed
    // the tour's surfaces and notes if the store lacks them (replacing a
    // surface whose spec changed, A-D8), show three of them side by side in
    // the window's layout once (A-D4), and run the concierge on the host's
    // runtime until the host stops.
    let mut concierge = None;
    koine_host::cli::main_with("koine-demo", |cli, served, handle| {
        let report = handle.block_on(surface_demo_service::tour::seed_with_layout(
            &cli.config.app_id,
            Some(&cli.config.device),
        ));
        println!(
            "  tour: {} surfaces created, {} updated, {} existing, {} notes seeded, \
             {} tiles laid out",
            report.created.len(),
            report.updated.len(),
            report.existing.len(),
            report.notes_seeded,
            report.layout.len()
        );
        for error in &report.errors {
            eprintln!("  tour: {error}");
        }
        let _ = served;
        concierge = Some(surface_demo_service::concierge::spawn_on(
            handle,
            surface_demo_service::concierge::ConciergeConfig::new(cli.config.app_id.clone())
                .with_host(cli.config.device.clone()),
        ));
    })
}
