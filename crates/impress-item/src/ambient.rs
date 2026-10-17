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
