//! The spy is transparent: the whole store conformance suite passes through
//! a `SpyStore` around the memory store, with a window open the whole time.

use std::sync::Arc;

use koine_store_memory::MemoryStore;
use koine_store_spy::SpyStore;

fn spied() -> SpyStore<MemoryStore> {
    let spy = SpyStore::new(Arc::new(MemoryStore::new()));
    spy.start();
    spy
}

koine_store_conformance::suite!(spy => || spied());
