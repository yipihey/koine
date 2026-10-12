//! The directory store passes the same suite as the reference backend.
//!
//! Each case gets a fresh root under the system temp dir (left for the OS to
//! clean: the suite takes the store by value, so there is no hook to remove
//! the directory when a case ends). `reopen` hands the suite a fresh root and
//! an opener for it: every call opens that same directory again, so two
//! results alive at once are two `DirStore`s contending on one root's lock.
//! With the `watch` feature each one also watches, so the second hears what
//! the first writes (`cross_process_write_reaches_another_handle`).

use std::path::{Path, PathBuf};

use impress_item::AttributedStore;
use koine_store_dir::DirStore;

fn fresh_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "koine-store-dir-conformance-{}",
        uuid::Uuid::new_v4()
    ))
}

/// Every handle watches, and is waited for: the suite then runs against the
/// store as a host opens it (`dir:<path>`), and the cross-process case has
/// something to hear with. (`--no-default-features` has no watcher; the case
/// then skips.)
fn open_at(root: &Path) -> DirStore {
    let store = DirStore::open(root).expect("open the store");
    #[cfg(feature = "watch")]
    {
        store.watch().expect("start the watcher");
        assert!(
            store.wait_until_watching(std::time::Duration::from_secs(10)),
            "the watcher made its first scan"
        );
    }
    store
}

koine_store_conformance::suite! {
    dir => || open_at(&fresh_root()),
    blobs => || open_at(&fresh_root()),
    reopen => || {
        let root = fresh_root();
        move || -> Box<dyn AttributedStore> { Box::new(open_at(&root)) }
    },
}
