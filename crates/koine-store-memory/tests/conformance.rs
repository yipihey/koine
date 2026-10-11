//! The memory store is the reference backend: it passes the whole suite.
//!
//! Run `cargo test -p koine-store-memory -- --nocapture` to see which cases
//! were skipped (a memory store is not durable and is not shared, so the
//! reopen cases say so and return).

koine_store_conformance::suite! {
    memory => || koine_store_memory::MemoryStore::new(),
    blobs => || koine_store_memory::MemoryStore::new(),
}
