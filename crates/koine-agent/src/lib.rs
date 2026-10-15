//! `koine-agent`: the agent desk and the generated skill
//! (docs/plan-koine-research-os.md S-D, G-D).
//!
//! Koine runs no model. An agent connects over MCP or HTTP and learns the app
//! from its skill ([`skill::render`], served by `agent-service_skill`,
//! `GET /api/skill` and the MCP resource `koine://skill`). A person leaves it
//! work on the desk — `ask`, `diagnose` a logged call, `make-surface` — and
//! the agent picks it up with `next`, `claim`s it and `answer`s it.
//! [`AGENT_DESK_SURFACE`] is the desk the kit package ships.

pub mod context;
pub mod desk;
pub mod dto;
pub mod service;
pub mod skill;

use std::sync::RwLock;

pub use service::{AgentService, DefaultAgentService};
pub use skill::AppInfo;

/// The Agent desk surface: ask, make a surface from a description with
/// starter prompts, and the requests with their answers.
pub const AGENT_DESK_SURFACE: &str = include_str!("../surfaces/agent-desk.surface.json");

/// The surface's name, as its spec says.
pub const AGENT_DESK_SURFACE_NAME: &str = "Agent desk";

static APP: RwLock<Option<AppInfo>> = RwLock::new(None);

/// Say which app this process is (a host at start): the skill's name and
/// the layout the context reads.
pub fn set_app(app: AppInfo) {
    if let Ok(mut slot) = APP.write() {
        *slot = Some(app);
    }
}

/// The app this process is: what [`set_app`] said, else `koine-app` with
/// the kinds of the installed manifest.
pub fn app() -> AppInfo {
    let mut app = APP
        .read()
        .ok()
        .and_then(|slot| slot.clone())
        .unwrap_or_else(|| AppInfo {
            app_id: "koine-app".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kinds: Vec::new(),
        });
    if app.kinds.is_empty() && impress_pane_query::process_manifest::is_installed() {
        app.kinds = impress_pane_query::process_manifest::current()
            .kinds
            .keys()
            .map(|k| k.to_string())
            .collect();
    }
    app
}

/// Reference the service so the linker keeps its registrations.
pub fn ensure_linked() {
    std::hint::black_box(DefaultAgentService::new as *const ());
}
