//! The directory store passes the same suite as the reference backend.
//!
//! Each case gets a fresh root under the system temp dir (left for the OS to
//! clean: the suite takes the store by value, so there is no hook to remove
//! the directory when a case ends). `reopen` hands the suite a fresh root and
//! an opener for it: every call opens that same directory again, so two
//! results alive at once are two `DirStore`s contending on one root's lock.

use std::path::{Path, PathBuf};

use impress_item::AttributedStore;
use koine_store_dir::DirStore;

fn fresh_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "koine-store-dir-conformance-{}",
        uuid::Uuid::new_v4()
    ))
}

fn open_at(root: &Path) -> DirStore {
    DirStore::open(root).expect("open the store")
}

koine_store_conformance::suite! {
    dir => || open_at(&fresh_root()),
    blobs => || open_at(&fresh_root()),
    reopen => || {
        let root = fresh_root();
        move || -> Box<dyn AttributedStore> { Box::new(open_at(&root)) }
    },
}
