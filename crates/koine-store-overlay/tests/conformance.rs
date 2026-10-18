//! The overlay is a store: the whole conformance suite passes through an
//! `OverlayStore` over an empty memory store.

use std::sync::Arc;

use koine_store_memory::MemoryStore;
use koine_store_overlay::OverlayStore;

fn overlaid() -> OverlayStore {
    OverlayStore::new(Arc::new(MemoryStore::new()))
}

koine_store_conformance::suite!(overlay => || overlaid());
