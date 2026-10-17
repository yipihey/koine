//! Suite-scoped memory: the kernel, the `memory-service_*` verbs and the task
//! executors that keep it fed (ADR-0028; docs/plan-koine-kit-moves.md KM5).
//!
//! * [`ops`] is the kernel — write, gate, rank and recall — over the store
//!   port (`impress_item::AttributedStore`), so it runs on the memory,
//!   directory and SQLite backends alike. Full-text retrieval is the port's
//!   `search_text`; SQLite answers it from FTS.
//! * [`service`] (feature `service`, on by default) is the `#[impress_service]`
//!   `MemoryService`. Its default deployment reads the process store
//!   (`koine_store::process`).
//! * Feature `embeddings` adds the vector tier (the `koine-embeddings`
//!   sidecar); `embedder` adds the local model that writes and queries it.
//!   Without them recall is text search alone.
//! * [`executors`] (feature `executors`) are the two task-kernel executors —
//!   embedding backfill and consolidation — and the spawner that keeps them
//!   fed.

#[cfg(feature = "executors")]
pub mod executors;
pub mod ops;
#[cfg(feature = "service")]
pub mod service;
#[cfg(feature = "embeddings")]
mod vector;

#[cfg(feature = "service")]
pub use service::{DefaultMemoryService, MemoryService};

/// Keep the memory verbs' inventory registrations in a host that names no
/// other item of this crate (as `koine_tasks::ensure_linked` does).
#[cfg(feature = "service")]
pub fn ensure_linked() {
    std::hint::black_box(std::ptr::addr_of!(
        service::__IMPRESS_VERB_MemoryService_recall
    ));
}
