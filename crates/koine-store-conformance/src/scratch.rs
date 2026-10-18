//! Scratch directories for stores a test hands away by value.
//!
//! The suite takes a store by value and has no hook to run when a case ends,
//! so a store opened on a fresh directory used to leave that directory
//! behind for the OS to clean: thousands per run under `$TMPDIR` (2,991 of
//! koine-store-dir's alone on 2026-10-18), more under cargo-nextest, which
//! runs each test in its own process. [`dir`] puts every one under
//! `$TMPDIR/koine-test-scratch/`, and the first call in a process deletes
//! what earlier runs left there — anything older than an hour, which no
//! test run here lasts.

use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{Duration, SystemTime};

/// How old a leftover must be before a later run deletes it.
const STALE: Duration = Duration::from_secs(60 * 60);

/// Where every scratch directory goes.
pub fn root() -> PathBuf {
    std::env::temp_dir().join("koine-test-scratch")
}

/// A new, empty directory for `tag`, deleted by a run an hour from now.
pub fn dir(tag: &str) -> PathBuf {
    static SWEEP: Once = Once::new();
    let root = root();
    SWEEP.call_once(|| sweep(&root, STALE));
    let dir = root.join(format!("{tag}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).expect("create a scratch directory");
    dir
}

/// Delete the entries of `root` last modified more than `age` ago.
fn sweep(root: &Path, age: Duration) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|elapsed| elapsed > age);
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scratch_dir_is_fresh_and_old_ones_go() {
        let a = dir("scratch-test");
        let b = dir("scratch-test");
        assert_ne!(a, b);
        assert!(a.is_dir() && std::fs::read_dir(&a).unwrap().next().is_none());
        // Nothing here is older than an hour; a zero age sweeps both.
        sweep(&root(), Duration::ZERO);
        assert!(!a.exists() && !b.exists());
    }
}
