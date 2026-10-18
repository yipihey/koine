//! The host of regex-lab.
//!
//! Linking `regex-lab-service` is what puts its verbs in the process-wide
//! inventory; everything the host serves (`POST /api/verb/<name>`,
//! `GET /api/verb/<name>/surface`, `/api/surface/*`, `/ui/`, `/mcp`) is a call
//! into that inventory. The app's own pages come in as its package
//! (`package.rs`, in this crate's library so the phone app links the same
//! one). The flags are `koine_host::cli`'s; run it with `--help`.

fn main() -> std::process::ExitCode {
    // Each keeps its crate's verbs through the link.
    koine_host::ensure_linked();
    regex_lab_service::ensure_linked();
    koine_host::cli::main_configured(
        "regex-lab",
        |config| config.packages.push(regex_lab_host::package::package()),
        |_, _, _| {},
    )
}
