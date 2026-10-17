//! Collections over any item: the kernel and the `collection-service_*` verbs
//! (ADR-0022 D1/D2/D5; docs/plan-koine-kit-moves.md KM4).
//!
//! * [`ops`] is the kernel, written against the store port
//!   (`impress_item::AttributedStore`), so it runs over the memory, directory
//!   and SQLite backends alike. It carries the generic `collection@1.0.0`
//!   binding; an app's binding (`imbib/collection`, `manuscript-collection`,
//!   `figure-collection`) is a [`ops::CollectionSchemaBinding`] value the app
//!   defines and registers. impress-core re-exports the kernel as
//!   `collection_ops`.
//! * [`service`] (feature `service`) is the `#[impress_service]`
//!   `CollectionService`. Its default deployment reads the process store
//!   (`koine_store::process`).
//! * [`package`] (feature `package`) is the capability's content as the
//!   built-in package `builtin:collections` (KM-D5).

pub mod ops;
#[cfg(feature = "service")]
pub mod service;

#[cfg(feature = "service")]
pub use service::{CollectionService, DefaultCollectionService};

#[cfg(feature = "package")]
mod pkg;
#[cfg(feature = "package")]
pub use pkg::{package, PACKAGE, SKILL, VERBS};
