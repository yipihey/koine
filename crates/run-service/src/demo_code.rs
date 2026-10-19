//! The one built-in code, registered for proof: `demo-code`
//! (`src/bin/demo-code.rs`), found beside this process's own binary or
//! named by `KOINE_DEMO_CODE`. Its schema is what the binary emits on
//! `--schema`, read at registration, never a copy kept in Rust;
//! `fixtures/demo-code.schema.json` is the pinned snapshot a test holds the
//! binary to.

use std::path::PathBuf;
use std::sync::Arc;

use crate::codes::{self, CodeProvider, DirVia, Launch, ParamsVia};

/// The code's name, as its `run.json` spells it.
pub const NAME: &str = "demo-code";

/// Where the binary is: `KOINE_DEMO_CODE`, else `demo-code` beside the
/// current executable (a workspace build puts every binary in one
/// `target/<profile>/`), else `None`.
pub fn locate() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("KOINE_DEMO_CODE").filter(|p| !p.is_empty()) {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [dir.join(NAME), dir.parent()?.join(NAME)]
        .into_iter()
        .find(|candidate| candidate.is_file())
}

/// The launch template of the demo code: `demo-code --dir <run> --params
/// <file> --problem <json> [--from <ckpt>]`.
pub fn launch(binary: &std::path::Path) -> Launch {
    Launch {
        argv: vec![binary.to_string_lossy().into_owned()],
        params: ParamsVia::File,
        params_flag: "--params".into(),
        dir: DirVia::Flag,
        dir_name: "--dir".into(),
        problem_flag: Some("--problem".into()),
        resume_flag: Some("--from".into()),
        env: Default::default(),
    }
}

/// Register the demo code from `binary`.
pub fn register_at(binary: &std::path::Path) -> Result<Arc<CodeProvider>, String> {
    codes::register_from_binary(
        NAME,
        "0.1.0",
        launch(binary),
        Some(serde_json::json!({"name": "demo", "version": "1"})),
    )
}

/// Register the demo code if its binary can be found; `None` (and a log
/// line) when it cannot. Idempotent.
pub fn register() -> Option<Arc<CodeProvider>> {
    if let Some(existing) = codes::get(NAME) {
        return Some(existing);
    }
    let Some(binary) = locate() else {
        tracing::info!(
            "demo-code is not beside this binary and KOINE_DEMO_CODE is unset; not registered"
        );
        return None;
    };
    match register_at(&binary) {
        Ok(code) => Some(code),
        Err(e) => {
            tracing::warn!("demo-code at {} did not register: {e}", binary.display());
            None
        }
    }
}
