//! P8 runtime providers: the registry's persistence over the store port
//! (descriptors, the person's trust decision, credential hashes beside
//! hash-addressed token files) and the `provider-service_*` verbs, with which
//! a person reviews providers and trusts or distrusts them
//! (docs/plan-koine-kit-moves.md KM12).
//!
//! * [`persistence`]: [`persistence::StoreProviderPersistence`] and the
//!   registry builders a host calls with its open store and workspace.
//! * [`service`]: `ProviderService`, over the installed registry.

pub mod persistence;
pub mod service;

pub use persistence::{build_registry, install_for_store, StoreProviderPersistence};
pub use service::{DefaultProviderService, ProviderService};

/// Keep the verbs' inventory registrations in a host that names nothing
/// else of this crate.
pub fn ensure_linked() {
    std::hint::black_box(DefaultProviderService::new as *const ());
}
