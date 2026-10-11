//! Cases for the three methods the host runtimes take from the port
//! (`docs/plan-koine-host.md` H-D1): `change_token`, `operations_for` and the
//! `metadata_get` / `metadata_set` pair.
//!
//! A backend may override the defaults (SQLite does, with its tables); these
//! cases are what keeps the override and the default the same function.

use impress_item::{
    AttributedStore, FieldMutation, ItemId, ItemQuery, Predicate, StoreError, Value,
};

use crate::fixture::*;

// ---------------------------------------------------------------------------
// change_token
// ---------------------------------------------------------------------------

fn token(store: &dyn AttributedStore) -> u64 {
    store.change_token().expect("read the change token")
}

/// Assert that `write` moved the token upward (not merely changed it:
/// monotonic within a handle), and return the new value.
fn moved_by(store: &dyn AttributedStore, what: &str, before: u64) -> u64 {
    let after = token(store);
    assert!(
        after > before,
        "{what} must move the change token upward: {before} -> {after}"
    );
    after
}

/// Insert, batch insert, update, an operation and delete each move the token;
/// reads do not. (A write that changes nothing is not asserted either way.)
pub fn change_token_moves_on_every_write(store: &dyn AttributedStore) {
    let mut seen = token(store);

    let id = insert(store, "test", "A");
    seen = moved_by(store, "insert", seen);

    store
        .insert_batch(vec![item("test", "B"), item("test", "C")])
        .unwrap();
    seen = moved_by(store, "insert_batch", seen);

    // A row that arrives with a clock of its own (sync, import) stamps
    // nothing, and is still a write.
    let mut stamped = item("test", "D");
    stamped.logical_clock = 1;
    store.insert(stamped).unwrap();
    seen = moved_by(store, "insert of a row with a clock", seen);

    store
        .update(id, vec![FieldMutation::SetStarred(true)])
        .unwrap();
    seen = moved_by(store, "update", seen);

    store
        .apply_operation(spec(id, impress_item::OperationType::SetRead(true)))
        .unwrap();
    seen = moved_by(store, "apply_operation", seen);

    // Reads of every kind leave it where it was.
    let _ = store.get(id).unwrap();
    let _ = store.get(uuid::Uuid::new_v4()).unwrap();
    let _ = store.query(&all_of("test")).unwrap();
    let _ = store.count(&all_of("test")).unwrap();
    let _ = store.neighbors(id, &[], 2).unwrap();
    let _ = store.logical_clock_of(id).unwrap();
    let _ = store.operations_for(id, None).unwrap();
    let _ = store.metadata_get("never-set").unwrap();
    assert_eq!(token(store), seen, "reads must not move the change token");

    store.delete(id).unwrap();
    let after_delete = moved_by(store, "delete", seen);

    // A write that fails is not a write the token has to announce, but it
    // must not take the token backwards either.
    assert!(matches!(
        store.delete(id),
        Err(StoreError::NotFound(missing)) if missing == id
    ));
    assert!(token(store) >= after_delete);
}

// ---------------------------------------------------------------------------
// operations_for
// ---------------------------------------------------------------------------

fn clocks(ops: &[impress_item::Item]) -> Vec<u64> {
    ops.iter().map(|o| o.logical_clock).collect()
}

/// `operations_for(target, limit)` is the target's `core/operation` rows,
/// ordered by `logical_clock` ascending (the oracle's order: oldest first, so
/// `limit` keeps the first `limit`), and only the target's.
///
/// The plan's H-D1 wrote "desc"; `SqliteItemStore::operations_for`, the
/// method this default must agree with, is ascending, and every caller reads
/// it that way, so the suite pins the oracle's order.
pub fn operations_for_lists_a_rows_history_oldest_first_with_limit(store: &dyn AttributedStore) {
    let id = insert(store, "test", "Target");
    let other = insert(store, "test", "Other");
    assert!(store.operations_for(id, None).unwrap().is_empty());

    for mutation in [
        FieldMutation::SetStarred(true),
        FieldMutation::SetRead(true),
        FieldMutation::AddTag("t".into()),
        FieldMutation::SetPayload("k".into(), Value::Int(1)),
    ] {
        store.update(id, vec![mutation]).unwrap();
    }
    store
        .update(other, vec![FieldMutation::SetStarred(true)])
        .unwrap();

    let all = store.operations_for(id, None).unwrap();
    assert_eq!(all.len(), 4);
    for op in &all {
        assert_eq!(op.schema.as_str(), "core/operation");
    }
    let kinds: Vec<String> = all.iter().map(op_type_of).collect();
    assert_eq!(
        kinds,
        ["set_starred", "set_read", "add_tag", "set_payload"],
        "oldest first"
    );
    let clocks_all = clocks(&all);
    assert!(
        clocks_all.windows(2).all(|w| w[0] < w[1]),
        "logical_clock strictly ascending: {clocks_all:?}"
    );

    // The same rows the fixture reads through a query: the default is that
    // query, and an override must agree.
    let by_query: Vec<ItemId> = operations_of(store, id).iter().map(|o| o.id).collect();
    assert_eq!(all.iter().map(|o| o.id).collect::<Vec<_>>(), by_query);

    // limit keeps the first `limit` of the same order.
    let two = store.operations_for(id, Some(2)).unwrap();
    assert_eq!(
        two.iter().map(|o| o.id).collect::<Vec<_>>(),
        all[..2].iter().map(|o| o.id).collect::<Vec<_>>()
    );
    assert_eq!(store.operations_for(id, Some(1)).unwrap().len(), 1);
    assert!(store.operations_for(id, Some(0)).unwrap().is_empty());
    assert_eq!(store.operations_for(id, Some(99)).unwrap().len(), 4);

    // Only the target's: the other item has its own single operation, and an
    // id that names no row has none.
    assert_eq!(store.operations_for(other, None).unwrap().len(), 1);
    assert!(store
        .operations_for(uuid::Uuid::new_v4(), None)
        .unwrap()
        .is_empty());

    // Operation rows are ordinary rows: the query side sees them too.
    let q = ItemQuery {
        predicates: vec![Predicate::Eq(
            "op_target_id".into(),
            Value::String(id.to_string()),
        )],
        ..all_of("core/operation")
    };
    assert_eq!(store.count(&q).unwrap(), 4);
}

// ---------------------------------------------------------------------------
// metadata
// ---------------------------------------------------------------------------

/// A value set is the value got; setting again replaces it; keys are
/// independent, exact (case and punctuation count), and any string is a value,
/// the empty one included.
pub fn metadata_round_trips_and_overwrites(store: &dyn AttributedStore) {
    store.metadata_set("origin_id", "abc").unwrap();
    assert_eq!(
        store.metadata_get("origin_id").unwrap().as_deref(),
        Some("abc")
    );

    store.metadata_set("origin_id", "def").unwrap();
    assert_eq!(
        store.metadata_get("origin_id").unwrap().as_deref(),
        Some("def"),
        "a second set replaces the first"
    );
    // Setting the value it already has is fine.
    store.metadata_set("origin_id", "def").unwrap();
    assert_eq!(
        store.metadata_get("origin_id").unwrap().as_deref(),
        Some("def")
    );

    store.metadata_set("rename/applied:v1", "").unwrap();
    store.metadata_set("Origin_Id", "case").unwrap();
    let long = "x".repeat(10_000);
    store.metadata_set("blob", &long).unwrap();
    assert_eq!(
        store.metadata_get("rename/applied:v1").unwrap().as_deref(),
        Some(""),
        "the empty string is a value, not absence"
    );
    assert_eq!(
        store.metadata_get("Origin_Id").unwrap().as_deref(),
        Some("case")
    );
    assert_eq!(store.metadata_get("blob").unwrap().as_deref(), Some(&*long));
    assert_eq!(
        store.metadata_get("origin_id").unwrap().as_deref(),
        Some("def"),
        "another key's write leaves this one alone"
    );

    // A key that is also JSON-looking text stays text.
    store.metadata_set("json", r#"{"a":[1,2]}"#).unwrap();
    assert_eq!(
        store.metadata_get("json").unwrap().as_deref(),
        Some(r#"{"a":[1,2]}"#)
    );
}

/// A key that was never set is `None` (not an error, not the empty string),
/// on a store with no metadata at all and on one with other keys.
pub fn metadata_missing_is_none(store: &dyn AttributedStore) {
    assert_eq!(store.metadata_get("nothing").unwrap(), None);
    store.metadata_set("something", "v").unwrap();
    assert_eq!(store.metadata_get("nothing").unwrap(), None);
    assert_eq!(store.metadata_get("").unwrap(), None);
    assert_eq!(store.metadata_get("Something").unwrap(), None);
    assert_eq!(store.metadata_get("something ").unwrap(), None);
}
