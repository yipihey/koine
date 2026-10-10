//! Reachability: ONE rule for which verbs need a running app, and one probe
//! interface (plan-verb-pipeline PL-2).
//!
//! Before P2 there were three rules: impress-mcp gated four namespaces on a
//! probe made once at startup; impel-tools gated *every* `imbib-*` and
//! `imprint-*` namespace on a re-probing backend table (inside an app, a verb
//! must reach the owning app rather than write the shared store behind its
//! back); impress-ai-tools gated the four namespaces on a table refreshed
//! every 300 s. The rule is now written once, here, with the one thing that
//! legitimately differs between processes as a parameter:
//!
//! * [`APP_GATED`] namespaces need their app, always — their default
//!   implementations refuse, and a method returning `Vec<T>` can only refuse
//!   by answering an empty list, which reads as "you have none";
//! * every other namespace an app *owns* (by prefix) needs the app only when
//!   the process says the shared store is not a fallback for it
//!   ([`Config::store_fallback`] = `false`, which is impel-tools inside an
//!   app);
//! * a namespace no app owns runs against the shared store and is always
//!   reachable.
//!
//! The *probe* stays with the process that owns the shared app transport:
//! each entry process installs one [`Probe`] that answers "is this app up?" — at
//! startup, re-probing, or on a cadence. With nothing
//! installed nothing is gated, which is what the CLI, the FFI and the surface
//! runtime did before P2 (their verbs' default implementations refuse by
//! themselves).

use std::sync::{Arc, RwLock};

/// Namespaces that only work while their app is running, and the app each
/// needs.
pub const APP_GATED: &[(&str, &str)] = &[
    ("imbib-app-service", "imbib"),
    ("imprint-app-service", "imprint"),
    ("implore-service", "implore"),
    ("impart-service", "impart"),
];

/// The namespace prefix each app owns.
pub const APP_OWNED_PREFIXES: &[(&str, &str)] = &[
    ("imbib-", "imbib"),
    ("imprint-", "imprint"),
    ("implore-", "implore"),
    ("impart-", "impart"),
];

/// Answers whether an app is reachable right now. Installed once per process
/// by the entry path that owns the HTTP clients.
pub trait Probe: Send + Sync {
    fn is_up(&self, app: &str) -> bool;
}

impl<F: Fn(&str) -> bool + Send + Sync> Probe for F {
    fn is_up(&self, app: &str) -> bool {
        self(app)
    }
}

/// The process's reachability configuration.
#[derive(Clone)]
pub struct Config {
    pub probe: Arc<dyn Probe>,
    /// Whether a verb in an app-owned namespace that is *not* app-gated may
    /// run against the shared store while its app is down. `true` for the
    /// MCP server, the CLI and the daemons; `false` inside an app (impel),
    /// where the shared store must not be written behind the running app.
    pub store_fallback: bool,
    /// Disable the gate for introspection (`IMPRESS_MCP_LIST_ALL=1`): a
    /// capability audit must not depend on which apps happened to be open.
    pub list_all: bool,
}

static CONFIG: RwLock<Option<Config>> = RwLock::new(None);

/// Install (or replace) the process's probe and its store-fallback rule.
pub fn install(config: Config) {
    if let Ok(mut slot) = CONFIG.write() {
        *slot = Some(config);
    }
}

fn config() -> Option<Config> {
    CONFIG.read().ok().and_then(|c| c.clone())
}

/// The namespace of a qualified verb name.
pub fn namespace_of(name: &str) -> Option<&str> {
    name.split_once('_').map(|(ns, _)| ns)
}

/// The app that owns a namespace by prefix, if any.
pub fn owner_of(name: &str) -> Option<&'static str> {
    let namespace = namespace_of(name)?;
    APP_OWNED_PREFIXES
        .iter()
        .find(|(prefix, _)| namespace.starts_with(prefix))
        .map(|(_, app)| *app)
}

/// The app a verb needs before it can run at all (the [`APP_GATED`] table).
pub fn gated_app(name: &str) -> Option<&'static str> {
    let namespace = namespace_of(name)?;
    APP_GATED
        .iter()
        .find(|(ns, _)| *ns == namespace)
        .map(|(_, app)| *app)
}

/// The app this verb needs under the installed configuration: its gated app,
/// or its owner when the process does not fall back to the store.
pub fn required_app(name: &str) -> Option<&'static str> {
    if let Some(app) = gated_app(name) {
        return Some(app);
    }
    match config() {
        Some(config) if !config.store_fallback => owner_of(name),
        _ => None,
    }
}

/// Whether this verb should be advertised and dispatched right now.
pub fn is_available(name: &str) -> bool {
    unavailable_app(name).is_none()
}

/// The app that is down and keeps this verb from running, if one is.
pub fn unavailable_app(name: &str) -> Option<&'static str> {
    let config = config()?;
    if config.list_all {
        return None;
    }
    let app = required_app(name)?;
    (!config.probe.is_up(app)).then_some(app)
}

/// Why a verb was withheld — the text impress-mcp has always answered.
pub fn unavailable_reason(name: &str) -> Option<String> {
    let app = unavailable_app(name)?;
    Some(reason_text(app, name))
}

/// Describe an app availability refusal consistently across entry paths.
pub fn reason_text(app: &str, name: &str) -> String {
    format!(
        "{app} is not running, so {name} is unavailable. This capability \
         lives in the app rather than the shared store. Open {app} and try again."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // The configuration is process-global; these tests take turns.
    static LOCK: Mutex<()> = Mutex::new(());

    fn probe(up: &'static [&'static str]) -> Arc<dyn Probe> {
        Arc::new(move |app: &str| up.contains(&app))
    }

    fn clear() {
        if let Ok(mut slot) = CONFIG.write() {
            *slot = None;
        }
    }

    #[test]
    fn the_tables_classify_names() {
        assert_eq!(gated_app("imbib-app-service_search-sources"), Some("imbib"));
        assert_eq!(gated_app("imbib-library-service_list-libraries"), None);
        assert_eq!(
            owner_of("imbib-library-service_list-libraries"),
            Some("imbib")
        );
        assert_eq!(owner_of("collection-service_tree"), None);
        assert_eq!(gated_app("impart-service_add-message"), Some("impart"));
        assert_eq!(gated_app("search_papers"), None);
    }

    #[test]
    fn with_nothing_installed_nothing_is_gated() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        assert!(is_available("implore-service_status"));
        assert!(unavailable_reason("implore-service_status").is_none());
    }

    #[test]
    fn the_store_fallback_flag_is_the_one_difference_between_processes() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        install(Config {
            probe: probe(&[]),
            store_fallback: true,
            list_all: false,
        });
        // impress-mcp / the daemons: gated namespaces withheld, owned ones run
        // against the store.
        assert!(!is_available("implore-service_status"));
        assert!(is_available("imbib-library-service_list-libraries"));
        assert!(is_available("collection-service_create"));
        assert!(unavailable_reason("implore-service_status")
            .unwrap()
            .contains("implore is not running"));

        install(Config {
            probe: probe(&["imprint"]),
            store_fallback: false,
            list_all: false,
        });
        // impel-tools inside an app: every owned namespace needs its app.
        assert!(!is_available("imbib-library-service_list-libraries"));
        assert!(is_available("imprint-manuscript-service_list-sections"));
        assert!(is_available("collection-service_create"));

        install(Config {
            probe: probe(&[]),
            store_fallback: true,
            list_all: true,
        });
        assert!(is_available("implore-service_status"));
        clear();
    }
}
