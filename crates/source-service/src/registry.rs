//! The adapters a host links (S-D9): a process-wide registry, one entry per
//! source id. `bib-source-service_explain` is its inventory. A host registers
//! each adapter package once at start (`source_crossref::register()`); a
//! test builds its own [`Registry`] and hands it to the service.

use std::sync::{Arc, RwLock};

use crate::adapter::Adapter;

/// A set of adapters by source id.
#[derive(Clone, Default)]
pub struct Registry {
    adapters: Arc<RwLock<Vec<Arc<dyn Adapter>>>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `adapter`; a second adapter with the same source id replaces the
    /// first (a test pointing the same source at a mock server).
    pub fn register(&self, adapter: Arc<dyn Adapter>) {
        let id = adapter.explain().source;
        let mut list = self.adapters.write().unwrap_or_else(|e| e.into_inner());
        list.retain(|a| a.explain().source != id);
        list.push(adapter);
    }

    /// Every adapter, in registration order.
    pub fn adapters(&self) -> Vec<Arc<dyn Adapter>> {
        self.adapters
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The adapter for source `id`.
    pub fn get(&self, id: &str) -> Option<Arc<dyn Adapter>> {
        self.adapters()
            .into_iter()
            .find(|a| a.explain().source == id)
    }

    /// The source ids, in registration order.
    pub fn sources(&self) -> Vec<String> {
        self.adapters().iter().map(|a| a.explain().source).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.adapters
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }
}

static GLOBAL: std::sync::OnceLock<Registry> = std::sync::OnceLock::new();

/// The process-wide registry the linked verbs read.
pub fn global() -> &'static Registry {
    GLOBAL.get_or_init(Registry::new)
}

/// Register `adapter` process-wide. Idempotent by source id.
pub fn register(adapter: Arc<dyn Adapter>) {
    global().register(adapter);
}
