//! The batch an operation is written under when its writer named none: the
//! id of the verb call it runs inside, so `operations_for(item) → batch_id →
//! call` answers "why did this change" on every backend
//! (docs/plan-koine-kit-moves.md KM10). The pipeline owns the call; it
//! registers how to read the current one ([`set_ambient_batch`],
//! `koine_store::process` does it), and a store asks [`ambient_batch`] when it
//! builds an operation row. Nothing registered, or no call running: `None`.

use std::sync::OnceLock;

static SOURCE: OnceLock<fn() -> Option<String>> = OnceLock::new();

/// Register where the current call's id comes from. The first registration
/// wins; later ones are ignored (there is one pipeline per process).
pub fn set_ambient_batch(source: fn() -> Option<String>) {
    let _ = SOURCE.set(source);
}

/// The current call's id, when a source is registered and a call is running.
pub fn ambient_batch() -> Option<String> {
    SOURCE.get().and_then(|source| source())
}

// ---------------------------------------------------------------------------
// The dry-run write gate (plan-koine-trust TR-D3)
// ---------------------------------------------------------------------------

use std::cell::Cell;

/// What a store answers a write with while the call it runs in is a dry run:
/// `StoreError::Unsupported` with this prefix.
pub const DRY_RUN_WRITE: &str = "dry run: this call may not write";

static DRY_RUN: OnceLock<fn() -> bool> = OnceLock::new();
static ON_REFUSAL: OnceLock<fn()> = OnceLock::new();

thread_local! {
    static EXEMPT: Cell<u32> = const { Cell::new(0) };
}

/// Register how to tell that the current call is a dry run (`tentatively`),
/// and what to do when a store refuses a write because of it. The pipeline
/// registers both (`koine_store::process` does it); the first registration
/// wins.
pub fn set_ambient_dry_run(is_dry_run: fn() -> bool, on_refusal: fn()) {
    let _ = DRY_RUN.set(is_dry_run);
    let _ = ON_REFUSAL.set(on_refusal);
}

/// Run `f` with the gate open on this thread: the dry run's own overlay
/// store writes through it.
pub fn with_dry_run_exempt<R>(f: impl FnOnce() -> R) -> R {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            EXEMPT.with(|e| e.set(e.get().saturating_sub(1)));
        }
    }
    EXEMPT.with(|e| e.set(e.get() + 1));
    let _restore = Restore;
    f()
}

/// Whether a store must refuse a write now: the current call is a dry run
/// and the write is not the dry run's own overlay.
pub fn dry_run_refuses_write() -> bool {
    EXEMPT.with(|e| e.get()) == 0 && DRY_RUN.get().is_some_and(|is| is())
}

/// The refusal a store answers a write with during a dry run, or `Ok` when it
/// may write. Every backend calls this first in each write method.
pub fn check_dry_run_write() -> Result<(), crate::StoreError> {
    if dry_run_refuses_write() {
        if let Some(note) = ON_REFUSAL.get() {
            note();
        }
        return Err(crate::StoreError::Unsupported(DRY_RUN_WRITE.into()));
    }
    Ok(())
}

/// Whether `error` is the dry-run refusal.
pub fn is_dry_run_refusal(error: &crate::StoreError) -> bool {
    matches!(error, crate::StoreError::Unsupported(m) if m.starts_with(DRY_RUN_WRITE))
}

#[cfg(test)]
mod dry_run_tests {
    use super::*;

    #[test]
    fn exemption_nests_and_restores() {
        EXEMPT.with(|e| assert_eq!(e.get(), 0));
        with_dry_run_exempt(|| {
            with_dry_run_exempt(|| EXEMPT.with(|e| assert_eq!(e.get(), 2)));
            EXEMPT.with(|e| assert_eq!(e.get(), 1));
        });
        EXEMPT.with(|e| assert_eq!(e.get(), 0));
        assert!(is_dry_run_refusal(&crate::StoreError::Unsupported(
            DRY_RUN_WRITE.into()
        )));
    }
}
