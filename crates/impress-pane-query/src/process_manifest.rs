//! The process-wide kind manifest.
//!
//! A [`KindManifest`] says which schema refs a record kind spans. The kinds
//! are the host's domain data, so the library crates that compile pane
//! queries (the layout and surface runtimes) never name one: the host
//! [`install`]s its manifest once, at the place it installs its store, and
//! every service built with no explicit manifest answers [`current`].
//!
//! A host that installs nothing gets an empty manifest, and one
//! `tracing::warn` the first time that happens: every kind-scoped query then
//! refuses `unknown kind`, which is the honest answer, and the line says why.
//!
//! ```
//! use impress_pane_query::{process_manifest, KindManifest};
//! use std::sync::Arc;
//!
//! let mut manifest = KindManifest::default();
//! manifest.kinds.insert("note".into(), vec!["note".into()]);
//! process_manifest::install(Arc::new(manifest));
//! assert!(process_manifest::current().schema_refs("note").is_some());
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use crate::KindManifest;

static INSTALLED: RwLock<Option<Arc<KindManifest>>> = RwLock::new(None);
static WARNED: AtomicBool = AtomicBool::new(false);

/// Install the process's manifest. A later install replaces the earlier one,
/// so a test can substitute its own.
pub fn install(manifest: Arc<KindManifest>) {
    *INSTALLED.write().unwrap_or_else(|e| e.into_inner()) = Some(manifest);
}

/// The installed manifest, or an empty one (with one warning, the first
/// time) when the host installed none.
pub fn current() -> Arc<KindManifest> {
    if let Some(manifest) = INSTALLED.read().unwrap_or_else(|e| e.into_inner()).clone() {
        return manifest;
    }
    if !WARNED.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            target: "pane-query",
            "no kind manifest was installed (impress_pane_query::process_manifest::install); \
             every kind-scoped query will refuse as an unknown kind"
        );
    }
    Arc::new(KindManifest::default())
}

/// Whether the host installed a manifest.
pub fn is_installed() -> bool {
    INSTALLED
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    // One case, in order: the registry is process-wide, so the "nothing
    // installed" half has to run before the "installed" half.
    #[test]
    fn a_host_that_installs_nothing_gets_an_empty_manifest_and_one_that_does_gets_its_own() {
        assert!(!is_installed());
        assert!(current().kinds.is_empty());
        assert!(current().kinds.is_empty(), "asked twice, warned once");

        let mut manifest = KindManifest::default();
        manifest.kinds.insert("note".into(), vec!["note@1".into()]);
        install(Arc::new(manifest));
        assert!(is_installed());
        assert_eq!(
            current().schema_refs("note"),
            Some(&["note@1".to_string()][..])
        );
    }
}
