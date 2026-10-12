//! `koine-store-conformance`: the suite every koine store backend passes.
//!
//! The port is `impress_item`'s `ItemStore` and `AttributedStore`; a backend
//! is a store when it passes this crate. Each case is a function of the store
//! under test that asserts one piece of the semantics `docs/plan-koine-store.md`
//! D2 writes down, taken from `SqliteItemStore` (the oracle, in impress-apps),
//! whose own tests are where most of these assertions were first made.
//!
//! # Use
//!
//! In the backend's `tests/conformance.rs`:
//!
//! ```ignore
//! koine_store_conformance::suite! {
//!     memory => || koine_store_memory::MemoryStore::new(),
//!     blobs => || koine_store_memory::MemoryStore::new(),
//! }
//! ```
//!
//! That expands to `mod memory { #[test] fn insert_get_round_trip() { .. } .. }`:
//! one `#[test]` per case, named after the case, each calling the case with a
//! **fresh** store from the expression. (A module, not a `memory_` prefix,
//! because `macro_rules!` cannot join identifiers.) The arms:
//!
//! | arm | meaning |
//! |-----|---------|
//! | `<name> => <make>` | required. `<make>` is a closure `\|\| store` where `store: impl AttributedStore`. Runs [`CASES`]. |
//! | `blobs => <make>` | the backend implements `BlobStore`; `<make>` builds one value that is both an `AttributedStore` and a `BlobStore`. Runs [`BLOB_CASES`]. Required when the store declares `blobs: true`, forbidden when it does not. |
//! | `reopen => <factory>` | the backend is `durable` or `shared`. `<factory>` is a closure `\|\| open` that makes a **fresh location** and returns `open`, a closure `\|\| -> Box<dyn AttributedStore>` that opens that same location each time it is called (two results alive at once are two handles to one store). Runs [`REOPEN_CASES`]. Required when the store declares `durable` or `shared`. |
//!
//! The arms go in that order. A host (a CLI, a CI report) can list the cases
//! without running them: [`CASES`], [`BLOB_CASES`], [`REOPEN_CASES`],
//! [`case_names`].
//!
//! # Skips are loud
//!
//! A case that needs a capability the store does not declare (`subscribe`,
//! `durable`, `shared`, `cross_process_events`) prints `SKIPPED <case>: <why>` and returns. It never
//! passes without saying so. Run `cargo test -- --nocapture` to see them.
//!
//! # What the suite does not pin
//!
//! * **Schemaless queries and operation rows.** Whether a query with no schema
//!   also returns operation rows is not asserted; every case that could be
//!   affected names a schema. What *is* asserted is that operation rows are
//!   readable by `get`, and queryable by schema `core/operation` and by
//!   `op_target_id`.
//! * **Order among equal sort keys**, and the order of an unsorted result.
//! * **Duplicates in `neighbors`**: results are compared as sets.
//! * **`ItemEvent::Updated`**: the oracle never sends it from a store write.
//! * **Crash atomicity** (`atomic_batch`): the suite checks in-process
//!   atomicity only; a backend proves the rest in its own tests.
//! * **Column affinity**: a text parameter against an integer column.
//!
//! Cases documented *(beyond D5)* in [`blobs`] pin decisions the plan leaves
//! implicit.

pub mod blobs;
pub mod crud;
pub mod durable;
pub mod events;
pub mod fixture;
pub mod graph;
pub mod ops;
pub mod port;
pub mod query;

pub use blobs::*;
pub use crud::*;
pub use durable::*;
pub use events::*;
pub use graph::*;
pub use ops::*;
pub use port::*;
pub use query::*;

use impress_item::{AttributedStore, BlobStore};

/// The one list of cases. The tables ([`CASES`] and the other two) and the
/// [`suite!`] macro are both generated from it, so they cannot disagree.
///
/// `$callback` is a macro invoked as `$callback! { <args> ; core: [..],
/// blobs: [..], reopen: [..] }`.
#[doc(hidden)]
#[macro_export]
macro_rules! __cases {
    ($callback:path; $($args:tt)*) => {
        $callback! { $($args)* ;
            core: [
                // crud
                an_empty_store_is_empty,
                insert_get_round_trip,
                get_missing_is_none,
                insert_duplicate_is_already_exists,
                insert_stamps_a_zero_clock_and_keeps_a_nonzero_one,
                insert_keeps_a_given_origin_and_stamps_a_missing_one,
                insert_collapses_repeated_tags,
                insert_keeps_references_once_per_target_and_edge,
                insert_batch_inserts_every_item,
                insert_batch_is_atomic,
                insert_batch_items_may_refer_to_earlier_items,
                update_applies_every_field_mutation,
                update_bumps_clock_and_modified,
                update_with_no_change_changes_nothing,
                update_missing_is_not_found,
                update_records_operations_with_a_shared_batch,
                delete_removes_the_row,
                delete_cascades,
                parent_can_be_set_cleared_and_must_exist,
                references_add_remove_and_must_resolve,
                tags_are_a_set,
                priority_and_visibility_are_set_by_operations,
                concurrent_inserts_all_land,
                // query
                query_schema_is_exact_equality,
                query_eq_and_neq,
                query_ordering_comparisons,
                query_contains,
                query_in,
                query_has_tag,
                query_flags_read_and_starred,
                query_has_parent,
                query_references_in_both_directions,
                query_logical_combinators,
                query_missing_fields_are_unknown_not_false,
                query_sort_orders,
                query_limit_and_offset,
                query_offset_without_limit_is_ignored,
                count_matches_query,
                count_ignores_limit_and_offset,
                query_include_flags_omit_tags_and_references,
                query_rows_are_the_rows_get_returns,
                // neighbors
                neighbors_walk_both_directions_to_a_depth,
                neighbors_follow_only_the_named_edge_types,
                neighbors_never_return_the_start_item_even_in_a_cycle,
                neighbors_return_whole_items,
                neighbors_forget_deleted_items,
                // events
                subscribe_hears_created_applied_and_deleted,
                subscribe_hears_each_item_of_a_batch,
                subscribe_hears_guarded_writes_that_applied,
                subscribe_filters_by_schema_prefix,
                subscribers_are_independent_and_dropped_ones_are_forgotten,
                failed_writes_announce_nothing,
                // operations
                apply_operation_materializes_every_operation_type,
                apply_operation_records_author_intent_reason_and_batch,
                operation_payload_records_the_previous_value,
                apply_operation_on_a_missing_row_is_not_found,
                apply_operation_refuses_dangling_references_and_parents,
                // guarded writes
                guarded_write_applies_only_on_the_clock_it_was_read_at,
                guarded_write_that_fails_writes_nothing,
                guarded_batch_applies_every_operation_under_one_batch,
                guarded_batch_is_all_or_nothing,
                guarded_batch_refuses_an_empty_batch,
                guarded_writes_lose_no_update_under_contention,
                // the clock
                logical_clock_of_tracks_the_row,
                clocks_increase_strictly_across_inserts_and_operations,
                clocks_pack_wall_milliseconds,
                a_row_inserted_with_a_clock_keeps_it_until_written,
                // the port additions the host runtimes take (plan-koine-host H-D1)
                change_token_moves_on_every_write,
                operations_for_lists_a_rows_history_oldest_first_with_limit,
                metadata_round_trips_and_overwrites,
                metadata_missing_is_none,
                // capabilities
                capabilities_are_coherent,
            ],
            blobs: [
                blob_put_get_round_trip,
                blob_put_replaces,
                blob_delete,
                blob_list_is_sorted_and_per_item,
                blobs_do_not_change_the_item,
                blob_put_for_a_missing_item_is_not_found,
                blob_names_must_be_plain_file_names,
                deleting_an_item_deletes_its_blobs,
            ],
            reopen: [
                reopen_keeps_items_operations_and_deletions,
                reopen_keeps_the_clock_monotonic,
                two_handles_share_one_store,
                cross_process_write_reaches_another_handle,
            ],
        }
    };
}

macro_rules! tables {
    (; core: [$($core:ident),* $(,)?], blobs: [$($blob:ident),* $(,)?], reopen: [$($reopen:ident),* $(,)?] $(,)?) => {
        /// The cases of every backend: a function of a fresh store.
        pub const CASES: &[(&str, fn(&dyn AttributedStore))] =
            &[$((stringify!($core), $core as fn(&dyn AttributedStore))),*];

        /// The cases of a backend that holds blobs: the item store, and the
        /// blob store (which may be the same object).
        pub const BLOB_CASES: &[(&str, fn(&dyn AttributedStore, &dyn BlobStore))] =
            &[$((stringify!($blob), $blob as fn(&dyn AttributedStore, &dyn BlobStore))),*];

        /// The cases of a backend that outlives a process or is opened twice:
        /// a fresh store (for its capabilities) and an [`Open`](durable::Open).
        pub const REOPEN_CASES: &[(&str, fn(&dyn AttributedStore, durable::Open<'_>))] =
            &[$((stringify!($reopen), $reopen as fn(&dyn AttributedStore, durable::Open<'_>))),*];
    };
}

__cases!(tables;);

/// Every case name, in the order the tables list them.
pub fn case_names() -> Vec<&'static str> {
    CASES
        .iter()
        .map(|c| c.0)
        .chain(BLOB_CASES.iter().map(|c| c.0))
        .chain(REOPEN_CASES.iter().map(|c| c.0))
        .collect()
}

/// The capabilities of a store, for the macro to read without importing the
/// trait into the caller's module.
#[doc(hidden)]
pub fn capabilities_of(store: &dyn AttributedStore) -> impress_item::StoreCapabilities {
    store.capabilities()
}

// -- the callbacks `suite!` hands the list to -------------------------------

#[doc(hidden)]
#[macro_export]
macro_rules! __suite_core {
    ($make:expr ; core: [$($core:ident),* $(,)?], blobs: [$($blob:ident),* $(,)?], reopen: [$($reopen:ident),* $(,)?] $(,)?) => {
        $(
            #[test]
            fn $core() {
                let store = ($make)();
                $crate::$core(&store);
            }
        )*
    };
}

/// The backend has no blobs arm: it must not claim blobs.
#[doc(hidden)]
#[macro_export]
macro_rules! __suite_without_blobs {
    ($make:expr ; core: [$($core:ident),* $(,)?], blobs: [$($blob:ident),* $(,)?], reopen: [$($reopen:ident),* $(,)?] $(,)?) => {
        #[test]
        fn blobs_are_not_declared_without_a_blobs_arm() {
            let store = ($make)();
            assert!(
                !$crate::capabilities_of(&store).blobs,
                "the store declares blobs: true; add a `blobs => ..` arm to suite! so BlobStore is checked"
            );
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __suite_blobs {
    ($make:expr, $make_blob:expr ; core: [$($core:ident),* $(,)?], blobs: [$($blob:ident),* $(,)?], reopen: [$($reopen:ident),* $(,)?] $(,)?) => {
        #[test]
        fn blobs_are_declared_with_a_blobs_arm() {
            let store = ($make_blob)();
            assert!(
                $crate::capabilities_of(&store).blobs,
                "a `blobs` arm was given but the store declares blobs: false"
            );
        }
        $(
            #[test]
            fn $blob() {
                let store = ($make_blob)();
                $crate::$blob(&store, &store);
            }
        )*
    };
}

/// No reopen arm: the cases run with `None`, so a store that declares
/// `durable` or `shared` fails and one that does not skips, loudly.
#[doc(hidden)]
#[macro_export]
macro_rules! __suite_without_reopen {
    ($make:expr ; core: [$($core:ident),* $(,)?], blobs: [$($blob:ident),* $(,)?], reopen: [$($reopen:ident),* $(,)?] $(,)?) => {
        $(
            #[test]
            fn $reopen() {
                let probe = ($make)();
                $crate::$reopen(&probe, None);
            }
        )*
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __suite_reopen {
    ($make:expr, $factory:expr ; core: [$($core:ident),* $(,)?], blobs: [$($blob:ident),* $(,)?], reopen: [$($reopen:ident),* $(,)?] $(,)?) => {
        $(
            #[test]
            fn $reopen() {
                let probe = ($make)();
                let open = ($factory)();
                $crate::$reopen(&probe, Some(&open));
            }
        )*
    };
}

/// Instantiate the suite for a backend. See the [crate docs](crate).
#[macro_export]
macro_rules! suite {
    ($name:ident => $make:expr $(,)?) => {
        // The caller writes `|| Store::new()`; clippy would have it write
        // `Store::new`, which reads worse in a table of arms.
        #[allow(clippy::redundant_closure)]
        mod $name {
            #[allow(unused_imports)]
            use super::*;
            $crate::__cases!($crate::__suite_core; $make);
            $crate::__cases!($crate::__suite_without_blobs; $make);
            $crate::__cases!($crate::__suite_without_reopen; $make);
        }
    };
    ($name:ident => $make:expr, blobs => $make_blob:expr $(,)?) => {
        // The caller writes `|| Store::new()`; clippy would have it write
        // `Store::new`, which reads worse in a table of arms.
        #[allow(clippy::redundant_closure)]
        mod $name {
            #[allow(unused_imports)]
            use super::*;
            $crate::__cases!($crate::__suite_core; $make);
            $crate::__cases!($crate::__suite_blobs; $make, $make_blob);
            $crate::__cases!($crate::__suite_without_reopen; $make);
        }
    };
    ($name:ident => $make:expr, reopen => $factory:expr $(,)?) => {
        // The caller writes `|| Store::new()`; clippy would have it write
        // `Store::new`, which reads worse in a table of arms.
        #[allow(clippy::redundant_closure)]
        mod $name {
            #[allow(unused_imports)]
            use super::*;
            $crate::__cases!($crate::__suite_core; $make);
            $crate::__cases!($crate::__suite_without_blobs; $make);
            $crate::__cases!($crate::__suite_reopen; $make, $factory);
        }
    };
    ($name:ident => $make:expr, blobs => $make_blob:expr, reopen => $factory:expr $(,)?) => {
        // The caller writes `|| Store::new()`; clippy would have it write
        // `Store::new`, which reads worse in a table of arms.
        #[allow(clippy::redundant_closure)]
        mod $name {
            #[allow(unused_imports)]
            use super::*;
            $crate::__cases!($crate::__suite_core; $make);
            $crate::__cases!($crate::__suite_blobs; $make, $make_blob);
            $crate::__cases!($crate::__suite_reopen; $make, $factory);
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_names_are_unique_and_the_tables_are_not_empty() {
        let names = case_names();
        let unique: std::collections::BTreeSet<_> = names.iter().collect();
        assert_eq!(
            unique.len(),
            names.len(),
            "a case is listed twice: {names:?}"
        );
        assert!(CASES.len() > 50, "{} core cases", CASES.len());
        assert!(!BLOB_CASES.is_empty() && !REOPEN_CASES.is_empty());
    }
}
