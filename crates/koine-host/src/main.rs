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
    koine_host::cli::main("koine-demo")
}
