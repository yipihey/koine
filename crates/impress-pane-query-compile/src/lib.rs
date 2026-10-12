//! `impress-pane-query-compile`: the pane-query compiler and its invalidation
//! rules, over the item protocol (`docs/plan-koine-host.md` H-D2).
//!
//! * [`compile`] / [`compile_with`] turn a [`PaneQuery`] (the algebra, in
//!   `impress-pane-query`) into an [`ImpressItemQuery`](impress_item::ItemQuery)
//!   against a [`KindManifest`] the *caller* supplies: a bespoke app passes
//!   its own, impress passes `impress_core::pane_query::builtin_manifest()`.
//! * [`invalidation`] says whether a store mutation can have changed a
//!   compiled query's result, so a pane re-runs only when it must.
//!
//! This is a separate crate rather than a module of `impress-pane-query`
//! because `impress-item` depends on `impress-pane-query` (its `EdgeType`
//! converts from the algebra's), so the algebra crate cannot depend back on
//! the item protocol the compiler targets.
//!
//! [`PaneQuery`]: impress_pane_query::PaneQuery
//! [`KindManifest`]: impress_pane_query::KindManifest

mod compile;
pub mod invalidation;

pub use compile::{compile, compile_with, CompiledQuery, RootOnly, SubtreeResolver};

#[cfg(test)]
mod store_tests;
/// A manifest shaped like the suite's `builtin_manifest()`, for tests.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
