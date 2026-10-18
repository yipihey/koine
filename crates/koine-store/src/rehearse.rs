//! How a host rehearses dry runs (docs/plan-koine-trust.md TR-D3): a
//! `tentatively` call's handler runs against a [`OverlayStore`] laid over the
//! store the call would have used, and the answer says what it would have
//! changed. [`install`] registers it with the pipeline.

use std::any::Any;
use std::sync::Arc;

use impress_service_core::pipeline::adverbs::{
    install_rehearsal_provider, RehearsalProvider, RehearsalStore,
};
use koine_store_overlay::OverlayStore;
use serde_json::Value;

struct Overlay(Arc<OverlayStore>);

impl RehearsalStore for Overlay {
    fn erased(&self) -> Arc<dyn Any + Send + Sync> {
        self.0.clone()
    }

    fn diff(&self) -> Value {
        self.0.diff()
    }
}

/// Lays an overlay over the call's own store, else the process store.
pub struct OverlayRehearsal;

impl RehearsalProvider for OverlayRehearsal {
    fn begin(
        &self,
        call_store: Option<Arc<dyn Any + Send + Sync>>,
    ) -> Option<Arc<dyn RehearsalStore>> {
        let base = match call_store {
            Some(erased) => crate::process::widen(erased)?,
            None => crate::process::current()?,
        };
        Some(Arc::new(Overlay(Arc::new(OverlayStore::new(base)))))
    }
}

/// Rehearse this process's dry runs.
pub fn install() {
    install_rehearsal_provider(Arc::new(OverlayRehearsal));
}
