//! The store-generic reads: grouped search across every kind, the edge walk
//! in both directions, and get/browse of any item (ADR-0022 D5/D6/D8;
//! docs/plan-koine-kit-moves.md KM9).
//!
//! * [`ops`]: the kernels over the store port.
//! * [`service`] (feature `service`, default): `StoreQueryService`, the four `store-query-service_*`
//!   verbs. Its default deployment reads the process store
//!   (`koine_store::process`).

pub mod ops;
#[cfg(feature = "service")]
pub mod service;

#[cfg(feature = "service")]
pub use service::{DefaultStoreQueryService, StoreQueryService};

#[cfg(feature = "service")]
/// Keep the verbs' inventory registrations in a host that names nothing
/// else of this crate.
pub fn ensure_linked() {
    std::hint::black_box(DefaultStoreQueryService::new as *const ());
}
