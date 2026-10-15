//! Cases for attributed operations, the guarded writes, and the clock.

use std::collections::{BTreeMap, BTreeSet};

use impress_item::schema::refs::CORE_OPERATION;
use impress_item::{
    ActorKind, AttributedStore, EdgeType, FlagState, GuardedBatch, GuardedWrite, ItemId,
    OperationIntent, OperationSpec, OperationType, Priority, RetentionTier, StoreError,
    TypedReference, Value, Visibility,
};

use crate::fixture::*;

// ---------------------------------------------------------------------------
// apply_operation
// ---------------------------------------------------------------------------

/// Every `OperationType` materializes its change on the target and records an
/// operation row: an item of schema `core/operation` that names its target,
/// its kind and its author, carrying the clock the target was stamped with.
pub fn apply_operation_materializes_every_operation_type(store: &dyn AttributedStore) {
    let folder = insert(store, "folder", "Folder");
    let other = insert(store, "test", "Other");
    let id = insert(store, "test", "Target");
    let mut applied = 0usize;
    let mut last_clock = store.logical_clock_of(id).unwrap().unwrap();
    let amber = FlagState {
        color: "amber".into(),
        style: Some("dashed".into()),
        length: Some("short".into()),
    };
    let meta = BTreeMap::from([("role".to_owned(), text("corresponding"))]);

    // (operation, its op_type name, a check on the target afterwards)
    type Check = Box<dyn Fn(&impress_item::Item)>;
    let steps: Vec<(OperationType, &str, Check)> = vec![
        (
            OperationType::AddTag("methods/sims".into()),
            "add_tag",
            Box::new(|t| assert!(t.tags.contains(&"methods/sims".to_owned()))),
        ),
        (
            OperationType::RemoveTag("methods/sims".into()),
            "remove_tag",
            Box::new(|t| assert!(t.tags.is_empty())),
        ),
        (
            OperationType::SetFlag(Some(amber.clone())),
            "set_flag",
            Box::new(move |t| assert_eq!(t.flag, Some(amber.clone()))),
        ),
        (
            OperationType::SetFlag(None),
            "set_flag",
            Box::new(|t| assert_eq!(t.flag, None)),
        ),
        (
            OperationType::SetRead(true),
            "set_read",
            Box::new(|t| assert!(t.is_read)),
        ),
        (
            OperationType::SetStarred(true),
            "set_starred",
            Box::new(|t| assert!(t.is_starred)),
        ),
        (
            OperationType::SetPriority(Priority::Urgent),
            "set_priority",
            Box::new(|t| assert_eq!(t.priority, Priority::Urgent)),
        ),
        (
            OperationType::SetVisibility(Visibility::Public),
            "set_visibility",
            Box::new(|t| assert_eq!(t.visibility, Visibility::Public)),
        ),
        (
            OperationType::SetPayload("k".into(), Value::Int(5)),
            "set_payload",
            Box::new(|t| assert_eq!(t.payload.get("k"), Some(&Value::Int(5)))),
        ),
        (
            OperationType::PatchPayload(BTreeMap::from([
                ("a".to_owned(), Value::Int(1)),
                ("b".to_owned(), text("x")),
            ])),
            "patch_payload",
            Box::new(|t| {
                assert_eq!(t.payload.get("a"), Some(&Value::Int(1)));
                assert_eq!(t.payload.get("b"), Some(&text("x")));
                assert_eq!(
                    t.payload.get("k"),
                    Some(&Value::Int(5)),
                    "a patch keeps the other keys"
                );
                assert_eq!(t.payload.get("title"), Some(&text("Target")));
            }),
        ),
        (
            OperationType::RemovePayload("k".into()),
            "remove_payload",
            Box::new(|t| assert!(!t.payload.contains_key("k"))),
        ),
        (
            OperationType::AddReference(TypedReference {
                target: other,
                edge_type: EdgeType::Cites,
                metadata: Some(meta.clone()),
            }),
            "add_reference",
            Box::new(move |t| {
                assert_eq!(t.references.len(), 1);
                assert_eq!(t.references[0].target, other);
                assert_eq!(t.references[0].edge_type, EdgeType::Cites);
                assert_eq!(t.references[0].metadata, Some(meta.clone()));
            }),
        ),
        (
            OperationType::RemoveReference(other, EdgeType::Cites),
            "remove_reference",
            Box::new(|t| assert!(t.references.is_empty())),
        ),
        (
            OperationType::SetParent(Some(folder)),
            "set_parent",
            Box::new(move |t| assert_eq!(t.parent, Some(folder))),
        ),
        (
            OperationType::SetParent(None),
            "set_parent",
            Box::new(|t| assert_eq!(t.parent, None)),
        ),
        (
            OperationType::Custom("note".into(), Value::Int(1)),
            "custom:note",
            Box::new(|t| assert_eq!(t.payload.get("title"), Some(&text("Target")))),
        ),
    ];

    for (op, name, check) in steps {
        let op_id = store
            .apply_operation(spec(id, op.clone()))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        applied += 1;

        let target = store.get(id).unwrap().unwrap();
        check(&target);
        assert!(
            target.logical_clock > last_clock,
            "{name}: the clock moves forward"
        );
        last_clock = target.logical_clock;

        let row = store
            .get(op_id)
            .unwrap()
            .unwrap_or_else(|| panic!("{name}: the operation row is readable by id"));
        assert_eq!(row.schema, CORE_OPERATION, "{name}");
        assert_eq!(op_type_of(&row), name);
        assert_eq!(
            row.payload.get("target_id"),
            Some(&text(&id.to_string())),
            "{name}: the row names its target"
        );
        assert_eq!(row.author, "test-user", "{name}");
        assert_eq!(row.author_kind, ActorKind::Human, "{name}");
        assert_eq!(
            row.logical_clock, target.logical_clock,
            "{name}: the operation and its target share one clock"
        );
        assert_eq!(operations_of(store, id).len(), applied, "{name}");
    }
    let ids: Vec<ItemId> = operations_of(store, id).iter().map(|o| o.id).collect();
    assert_eq!(ids.len(), applied);
    assert_eq!(
        ids.iter().collect::<BTreeSet<_>>().len(),
        applied,
        "every operation has its own id"
    );
}

/// The operation row records who, why, how important and in which batch.
pub fn apply_operation_records_author_intent_reason_and_batch(store: &dyn AttributedStore) {
    let id = insert(store, "test", "Target");
    let op = store
        .apply_operation(OperationSpec {
            target_id: id,
            op_type: OperationType::SetRead(true),
            intent: OperationIntent::Escalation,
            reason: Some("needs review".into()),
            batch_id: Some("batch-7".into()),
            author: "agent:reviewer".into(),
            author_kind: ActorKind::Agent,
            retention: RetentionTier::Durable,
        })
        .unwrap();
    let row = store.get(op).unwrap().unwrap();
    assert_eq!(row.author, "agent:reviewer");
    assert_eq!(row.author_kind, ActorKind::Agent);
    assert_eq!(row.batch_id.as_deref(), Some("batch-7"));
    assert_eq!(row.payload.get("intent"), Some(&text("escalation")));
    assert_eq!(row.payload.get("reason"), Some(&text("needs review")));
    assert_eq!(row.payload.get("op_data"), Some(&Value::Bool(true)));

    // Without a reason there is no reason key.
    let plain = store
        .apply_operation(spec(id, OperationType::SetStarred(true)))
        .unwrap();
    let row = store.get(plain).unwrap().unwrap();
    assert!(!row.payload.contains_key("reason"));
    assert_eq!(row.payload.get("intent"), Some(&text("routine")));
}

/// The operation keeps what it overwrote, so it can be undone.
pub fn operation_payload_records_the_previous_value(store: &dyn AttributedStore) {
    let mut it = item("test", "Target");
    it.payload.insert("doi".into(), text("10.1/old"));
    let id = store.insert(it).unwrap();
    let prev = |op: ItemId| store.get(op).unwrap().unwrap().payload.get("prev").cloned();

    let op = store
        .apply_operation(spec(id, OperationType::SetRead(true)))
        .unwrap();
    assert_eq!(prev(op), Some(Value::Bool(false)));
    let op = store
        .apply_operation(spec(id, OperationType::SetRead(false)))
        .unwrap();
    assert_eq!(prev(op), Some(Value::Bool(true)));

    let op = store
        .apply_operation(spec(
            id,
            OperationType::SetPayload("doi".into(), text("10.1/new")),
        ))
        .unwrap();
    assert_eq!(prev(op), Some(text("10.1/old")));
    let op = store
        .apply_operation(spec(
            id,
            OperationType::SetPayload("fresh".into(), Value::Int(1)),
        ))
        .unwrap();
    assert_eq!(
        prev(op),
        Some(Value::Null),
        "a field that was absent was null"
    );

    let folder = insert(store, "folder", "Folder");
    let op = store
        .apply_operation(spec(id, OperationType::SetParent(Some(folder))))
        .unwrap();
    assert_eq!(prev(op), Some(Value::Null));
    let op = store
        .apply_operation(spec(id, OperationType::SetParent(None)))
        .unwrap();
    assert_eq!(prev(op), Some(text(&folder.to_string())));

    // A tag has an inverse that needs no memory.
    let op = store
        .apply_operation(spec(id, OperationType::AddTag("t".into())))
        .unwrap();
    assert_eq!(prev(op), None);
}

/// An operation on a row that does not exist is `NotFound` and writes nothing.
pub fn apply_operation_on_a_missing_row_is_not_found(store: &dyn AttributedStore) {
    let ghost = uuid::Uuid::new_v4();
    let err = store
        .apply_operation(spec(ghost, OperationType::SetRead(true)))
        .expect_err("a missing target");
    assert!(
        matches!(err, StoreError::NotFound(id) if id == ghost),
        "{err:?}"
    );
    assert!(operations_of(store, ghost).is_empty());
    assert_eq!(store.logical_clock_of(ghost).unwrap(), None);
}

/// A reference or parent that names a row that is not stored is refused, and
/// the target is left as it was.
pub fn apply_operation_refuses_dangling_references_and_parents(store: &dyn AttributedStore) {
    let id = insert(store, "test", "Target");
    let nowhere = uuid::Uuid::new_v4();
    assert!(store
        .apply_operation(spec(
            id,
            OperationType::AddReference(reference(nowhere, EdgeType::Cites))
        ))
        .is_err());
    assert!(store
        .apply_operation(spec(id, OperationType::SetParent(Some(nowhere))))
        .is_err());
    let got = store.get(id).unwrap().unwrap();
    assert!(got.references.is_empty());
    assert_eq!(got.parent, None);
}

// ---------------------------------------------------------------------------
// guarded writes
// ---------------------------------------------------------------------------

fn set(target: ItemId, field: &str, value: Value) -> OperationSpec {
    OperationSpec {
        retention: RetentionTier::Ephemeral,
        ..spec(target, OperationType::SetPayload(field.into(), value))
    }
}

/// A guarded write applies only on the clock it was read at. `Applied` hands
/// back the row's new clock; after someone else writes, the same guard is
/// `Moved` with the clock that beat it, and nothing is written.
pub fn guarded_write_applies_only_on_the_clock_it_was_read_at(store: &dyn AttributedStore) {
    let id = insert(store, "test", "Target");
    let read = store
        .logical_clock_of(id)
        .unwrap()
        .expect("the row has a clock");

    let first = store
        .apply_operation_if_clock(set(id, "title", text("mine")), read)
        .unwrap();
    let GuardedWrite::Applied {
        operation_id,
        clock,
    } = first
    else {
        panic!("a matching clock writes: {first:?}");
    };
    assert!(clock > read, "the write moves the clock");
    assert_eq!(store.logical_clock_of(id).unwrap(), Some(clock));
    assert_eq!(store.get(id).unwrap().unwrap().logical_clock, clock);
    assert_eq!(
        store.get(operation_id).unwrap().unwrap().schema,
        CORE_OPERATION
    );
    assert_eq!(
        store.get(id).unwrap().unwrap().payload.get("title"),
        Some(&text("mine"))
    );

    // Someone else writes; a writer still holding `clock` is refused.
    store
        .apply_operation(set(id, "title", text("theirs")))
        .unwrap();
    let theirs = store.logical_clock_of(id).unwrap().unwrap();
    assert_ne!(theirs, clock);
    let ops_before = operations_of(store, id).len();
    let stale = store
        .apply_operation_if_clock(set(id, "title", text("stale")), clock)
        .unwrap();
    assert_eq!(
        stale,
        GuardedWrite::Moved {
            clock: Some(theirs)
        }
    );
    assert_eq!(
        store.get(id).unwrap().unwrap().payload.get("title"),
        Some(&text("theirs")),
        "the refused write left the row alone"
    );
    assert_eq!(
        operations_of(store, id).len(),
        ops_before,
        "and recorded nothing"
    );
    assert_eq!(store.logical_clock_of(id).unwrap(), Some(theirs));

    // A row that is gone is `Moved`, with no clock.
    assert_eq!(
        store
            .apply_operation_if_clock(set(uuid::Uuid::new_v4(), "x", Value::Null), clock)
            .unwrap(),
        GuardedWrite::Moved { clock: None }
    );
}

/// A guarded write whose operation is refused (here, a dangling reference)
/// is an error, and the clock does not move.
pub fn guarded_write_that_fails_writes_nothing(store: &dyn AttributedStore) {
    let id = insert(store, "test", "Target");
    let read = store.logical_clock_of(id).unwrap().unwrap();
    let bad = spec(
        id,
        OperationType::AddReference(reference(uuid::Uuid::new_v4(), EdgeType::Cites)),
    );
    assert!(store.apply_operation_if_clock(bad, read).is_err());
    assert_eq!(store.logical_clock_of(id).unwrap(), Some(read));
    assert!(operations_of(store, id).is_empty());
}

/// Two fields that must move together land as one guarded batch: one batch id
/// on every operation, the guard row's new clock returned, and a second
/// writer that read the row before the first committed is refused with
/// nothing written.
pub fn guarded_batch_applies_every_operation_under_one_batch(store: &dyn AttributedStore) {
    let id = insert(store, "test", "Target");
    let batch =
        |title: &str, n: i64| vec![set(id, "title", text(title)), set(id, "n", Value::Int(n))];
    // Two writers read the same clock.
    let read_first = store.logical_clock_of(id).unwrap().unwrap();
    let read_second = store.logical_clock_of(id).unwrap().unwrap();
    assert_eq!(read_first, read_second);

    let won = store
        .apply_operations_if_clock(batch("first", 2), id, read_first)
        .unwrap();
    let GuardedBatch::Applied {
        operation_ids,
        clock,
    } = won
    else {
        panic!("a matching clock writes: {won:?}");
    };
    assert_eq!(operation_ids.len(), 2);
    assert!(clock > read_first);
    assert_eq!(store.logical_clock_of(id).unwrap(), Some(clock));
    let ops: Vec<_> = operation_ids
        .iter()
        .map(|op| store.get(*op).unwrap().expect("each operation is a row"))
        .collect();
    assert!(
        ops[0].batch_id.is_some() && ops[0].batch_id == ops[1].batch_id,
        "one batch id on all of them"
    );
    let row = store.get(id).unwrap().unwrap();
    assert_eq!(row.payload.get("title"), Some(&text("first")));
    assert_eq!(row.payload.get("n"), Some(&Value::Int(2)));

    // The loser read before the winner committed.
    let lost = store
        .apply_operations_if_clock(batch("second", 3), id, read_second)
        .unwrap();
    assert_eq!(lost, GuardedBatch::Moved { clock: Some(clock) });
    let row = store.get(id).unwrap().unwrap();
    assert_eq!(row.payload.get("title"), Some(&text("first")));
    assert_eq!(row.payload.get("n"), Some(&Value::Int(2)));
    assert_eq!(
        operations_of(store, id).len(),
        2,
        "the refused batch recorded nothing"
    );

    // A gone guard is `Moved` with no clock.
    assert_eq!(
        store
            .apply_operations_if_clock(batch("x", 9), uuid::Uuid::new_v4(), clock)
            .unwrap(),
        GuardedBatch::Moved { clock: None }
    );
}

/// A guarded batch that fails part-way writes none of it.
pub fn guarded_batch_is_all_or_nothing(store: &dyn AttributedStore) {
    let id = insert(store, "test", "Target");
    let clock = store.logical_clock_of(id).unwrap().unwrap();
    let broken = vec![
        set(id, "title", text("half")),
        // The second operation names a row that does not exist.
        set(uuid::Uuid::new_v4(), "n", Value::Int(3)),
    ];
    assert!(matches!(
        store.apply_operations_if_clock(broken, id, clock),
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(
        store.logical_clock_of(id).unwrap(),
        Some(clock),
        "the clock did not move"
    );
    let row = store.get(id).unwrap().unwrap();
    assert_eq!(
        row.payload.get("title"),
        Some(&text("Target")),
        "the first operation was rolled back"
    );
    assert!(operations_of(store, id).is_empty());

    // A batch may touch other rows; the returned clock is the guard's.
    let other = insert(store, "test", "Other");
    let other_clock = store.logical_clock_of(other).unwrap().unwrap();
    let both = vec![set(id, "a", Value::Int(1)), set(other, "b", Value::Int(2))];
    let GuardedBatch::Applied {
        operation_ids,
        clock: guard_clock,
    } = store.apply_operations_if_clock(both, id, clock).unwrap()
    else {
        panic!("the guard matched");
    };
    assert_eq!(operation_ids.len(), 2);
    assert_eq!(store.logical_clock_of(id).unwrap(), Some(guard_clock));
    assert!(store.logical_clock_of(other).unwrap().unwrap() > other_clock);
    assert_eq!(
        store.get(other).unwrap().unwrap().payload.get("b"),
        Some(&Value::Int(2))
    );
}

/// `apply_operation_batch` on one row writes every operation under one batch
/// id, in order, and a batch that fails part-way writes none of it. An empty
/// batch writes nothing. Several rows are each written.
pub fn operation_batch_is_one_batch_and_all_or_nothing(store: &dyn AttributedStore) {
    let id = insert(store, "test", "Target");
    let ids = store
        .apply_operation_batch(vec![
            set(id, "title", text("one")),
            set(id, "n", Value::Int(1)),
        ])
        .unwrap();
    assert_eq!(ids.len(), 2);
    let ops: Vec<_> = ids
        .iter()
        .map(|op| store.get(*op).unwrap().expect("each operation is a row"))
        .collect();
    assert!(
        ops[0].batch_id.is_some() && ops[0].batch_id == ops[1].batch_id,
        "one batch id on all of them"
    );
    let row = store.get(id).unwrap().unwrap();
    assert_eq!(row.payload.get("title"), Some(&text("one")));
    assert_eq!(row.payload.get("n"), Some(&Value::Int(1)));

    let clock = store.logical_clock_of(id).unwrap();
    let missing = uuid::Uuid::new_v4();
    assert!(store
        .apply_operation_batch(vec![set(missing, "title", text("x"))])
        .is_err());
    assert_eq!(store.logical_clock_of(id).unwrap(), clock);
    assert!(store.apply_operation_batch(Vec::new()).unwrap().is_empty());

    let other = insert(store, "test", "Other");
    let both = store
        .apply_operation_batch(vec![
            set(id, "a", Value::Int(1)),
            set(other, "b", Value::Int(2)),
        ])
        .unwrap();
    assert_eq!(both.len(), 2);
    assert_eq!(
        store.get(other).unwrap().unwrap().payload.get("b"),
        Some(&Value::Int(2))
    );
}

/// An empty guarded batch is refused: it would report a write that never
/// happened.
pub fn guarded_batch_refuses_an_empty_batch(store: &dyn AttributedStore) {
    let id = insert(store, "test", "Target");
    let clock = store.logical_clock_of(id).unwrap().unwrap();
    assert!(store
        .apply_operations_if_clock(Vec::new(), id, clock)
        .is_err());
    assert!(store
        .apply_operations_if_clock(Vec::new(), uuid::Uuid::new_v4(), clock)
        .is_err());
    assert_eq!(store.logical_clock_of(id).unwrap(), Some(clock));
}

/// Writers racing on one row through compare-and-swap lose no update: each
/// increments a counter by reading the row and writing it back guarded, and
/// the counter ends at exactly the number of increments.
pub fn guarded_writes_lose_no_update_under_contention(store: &dyn AttributedStore) {
    const THREADS: usize = 6;
    const EACH: usize = 15;
    let mut it = item("test", "Counter");
    it.payload.insert("n".into(), Value::Int(0));
    let id = store.insert(it).unwrap();

    std::thread::scope(|scope| {
        for _ in 0..THREADS {
            scope.spawn(|| {
                for _ in 0..EACH {
                    loop {
                        let row = store.get(id).unwrap().unwrap();
                        let Some(Value::Int(n)) = row.payload.get("n") else {
                            panic!("the counter is an integer");
                        };
                        let write = set(id, "n", Value::Int(n + 1));
                        match store
                            .apply_operation_if_clock(write, row.logical_clock)
                            .unwrap()
                        {
                            GuardedWrite::Applied { .. } => break,
                            GuardedWrite::Moved { .. } => continue,
                        }
                    }
                }
            });
        }
    });
    assert_eq!(
        store.get(id).unwrap().unwrap().payload.get("n"),
        Some(&Value::Int((THREADS * EACH) as i64))
    );
    assert_eq!(operations_of(store, id).len(), THREADS * EACH);
}

// ---------------------------------------------------------------------------
// the clock
// ---------------------------------------------------------------------------

/// `logical_clock_of` is `None` for a row that is not there and the row's own
/// clock otherwise, at once after every write.
pub fn logical_clock_of_tracks_the_row(store: &dyn AttributedStore) {
    assert_eq!(store.logical_clock_of(uuid::Uuid::new_v4()).unwrap(), None);
    let id = insert(store, "test", "Target");
    let after_insert = store.logical_clock_of(id).unwrap().unwrap();
    assert_eq!(after_insert, store.get(id).unwrap().unwrap().logical_clock);
    store
        .update(id, vec![impress_item::FieldMutation::SetRead(true)])
        .unwrap();
    let after_update = store.logical_clock_of(id).unwrap().unwrap();
    assert!(after_update > after_insert);
    assert_eq!(after_update, store.get(id).unwrap().unwrap().logical_clock);
    store.delete(id).unwrap();
    assert_eq!(store.logical_clock_of(id).unwrap(), None);
}

/// The clock is store-wide and monotonic: every insert and every operation
/// gets a clock greater than all before it, so the clocks of a run of mixed
/// writes are strictly increasing.
pub fn clocks_increase_strictly_across_inserts_and_operations(store: &dyn AttributedStore) {
    let mut seen = Vec::new();
    for round in 0..60 {
        let id = insert(store, "test", &format!("Item {round}"));
        seen.push(store.logical_clock_of(id).unwrap().unwrap());
        let op = store
            .apply_operation(spec(
                id,
                OperationType::SetPayload("r".into(), Value::Int(round)),
            ))
            .unwrap();
        seen.push(store.get(op).unwrap().unwrap().logical_clock);
        assert_eq!(
            store.logical_clock_of(id).unwrap().unwrap(),
            *seen.last().unwrap(),
            "the target takes the operation's clock"
        );
    }
    for pair in seen.windows(2) {
        assert!(pair[0] < pair[1], "{} then {}", pair[0], pair[1]);
    }
    assert_eq!(
        seen.iter().collect::<BTreeSet<_>>().len(),
        seen.len(),
        "never the same clock twice"
    );
}

/// A clock is `(wall_ms << 16) | counter`: its high bits are the wall clock in
/// milliseconds since the epoch, so clocks from two stores sort together.
pub fn clocks_pack_wall_milliseconds(store: &dyn AttributedStore) {
    let before = wall_ms();
    let id = insert(store, "test", "Clocked");
    store
        .apply_operation(spec(id, OperationType::SetRead(true)))
        .unwrap();
    let after = wall_ms();
    let clock = store.logical_clock_of(id).unwrap().unwrap();
    let wall = clock >> 16;
    // A little slack either way: the wall clock is read inside the store.
    assert!(
        wall + 2_000 >= before && wall <= after + 2_000,
        "clock {clock} unpacks to {wall} ms; the test ran between {before} and {after}"
    );
    // A burst of writes inside one millisecond still gets distinct clocks (the
    // low bits count).
    let burst: BTreeSet<u64> = (0..300)
        .map(|i| {
            let row = insert(store, "test", &format!("Burst {i}"));
            store.logical_clock_of(row).unwrap().unwrap()
        })
        .collect();
    assert_eq!(burst.len(), 300);
}

/// The clock is the writer's to set: a replica applying remote rows keeps
/// their clocks, until a local write restamps the row.
pub fn a_row_inserted_with_a_clock_keeps_it_until_written(store: &dyn AttributedStore) {
    let mut remote = item("test", "Remote");
    remote.logical_clock = (wall_ms() << 16) | 41;
    let id = store.insert(remote.clone()).unwrap();
    assert_eq!(
        store.logical_clock_of(id).unwrap(),
        Some(remote.logical_clock)
    );
    store
        .apply_operation(spec(id, OperationType::SetRead(true)))
        .unwrap();
    assert_ne!(
        store.logical_clock_of(id).unwrap(),
        Some(remote.logical_clock),
        "a local write restamps the row"
    );
}
