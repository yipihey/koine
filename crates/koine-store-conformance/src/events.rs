//! Cases for `subscribe`: what is delivered, to whom, and what is not.

use std::sync::mpsc::TryRecvError;

use impress_item::{AttributedStore, EdgeType, FieldMutation, ItemEvent, ItemQuery, OperationType};

use crate::fixture::*;

/// A subscription that asks only for a kind: the schema is a *prefix*, so
/// `task@` hears every version of a task.
fn hear(prefix: &str) -> ItemQuery {
    all_of(prefix)
}

/// A subscriber hears what this process writes: `Created` for an insert,
/// `OperationApplied` for every change (`update` and `apply_operation` alike),
/// `Deleted` for a delete, in the order the writes happened, and nothing else.
///
/// (`ItemEvent::Updated` exists in the vocabulary but the oracle never sends
/// it from a store write, so no backend is asked to; a subscriber must not
/// depend on it.)
pub fn subscribe_hears_created_applied_and_deleted(store: &dyn AttributedStore) {
    if !store.capabilities().subscribe {
        return skip(
            "subscribe_hears_created_applied_and_deleted",
            "the store declares subscribe: false",
        );
    }
    let rx = store.subscribe(ItemQuery::default()).expect("subscribe");

    let id = insert(store, "test", "Event Test");
    match drain(&rx).as_slice() {
        [ItemEvent::Created(created)] => {
            assert_eq!(created.id, id);
            assert_eq!(created.schema, schema("test"));
        }
        other => panic!("an insert is one Created, got {other:?}"),
    }

    store
        .update(id, vec![FieldMutation::SetRead(true)])
        .unwrap();
    match drain(&rx).as_slice() {
        [ItemEvent::OperationApplied {
            target_id,
            operation_id,
        }] => {
            assert_eq!(*target_id, id);
            assert_eq!(operations_of(store, id)[0].id, *operation_id);
        }
        other => panic!("an update is one OperationApplied per change, got {other:?}"),
    }

    let op = store
        .apply_operation(spec(id, OperationType::AddTag("t".into())))
        .unwrap();
    assert_eq!(
        drain(&rx),
        vec![ItemEvent::OperationApplied {
            operation_id: op,
            target_id: id
        }]
    );

    // Two changes in one update are two events, in order.
    store
        .update(
            id,
            vec![
                FieldMutation::SetStarred(true),
                FieldMutation::AddTag("u".into()),
            ],
        )
        .unwrap();
    let events = drain(&rx);
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(events
        .iter()
        .all(|e| matches!(e, ItemEvent::OperationApplied { target_id, .. } if *target_id == id)));

    store.delete(id).unwrap();
    assert_eq!(drain(&rx), vec![ItemEvent::Deleted(id)]);
    assert!(
        matches!(rx.try_recv(), Err(TryRecvError::Empty)),
        "nothing else"
    );
}

/// A batch announces each item it inserted, in order.
pub fn subscribe_hears_each_item_of_a_batch(store: &dyn AttributedStore) {
    if !store.capabilities().subscribe {
        return skip(
            "subscribe_hears_each_item_of_a_batch",
            "the store declares subscribe: false",
        );
    }
    let rx = store.subscribe(ItemQuery::default()).unwrap();
    let items: Vec<_> = (0..4).map(|i| item("test", &format!("Item {i}"))).collect();
    let expected: Vec<_> = items.iter().map(|i| i.id).collect();
    store.insert_batch(items).unwrap();
    let got: Vec<_> = drain(&rx)
        .into_iter()
        .map(|e| match e {
            ItemEvent::Created(item) => item.id,
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(got, expected);
}

/// Guarded writes announce the operations they applied, and only those.
pub fn subscribe_hears_guarded_writes_that_applied(store: &dyn AttributedStore) {
    if !store.capabilities().subscribe {
        return skip(
            "subscribe_hears_guarded_writes_that_applied",
            "the store declares subscribe: false",
        );
    }
    let id = insert(store, "test", "Target");
    let rx = store.subscribe(ItemQuery::default()).unwrap();
    let clock = store.logical_clock_of(id).unwrap().unwrap();

    let impress_item::GuardedWrite::Applied {
        operation_id,
        clock: after,
    } = store
        .apply_operation_if_clock(spec(id, OperationType::SetRead(true)), clock)
        .unwrap()
    else {
        panic!("the clock matched");
    };
    assert_eq!(
        drain(&rx),
        vec![ItemEvent::OperationApplied {
            operation_id,
            target_id: id
        }]
    );

    // A refused guard announces nothing.
    store
        .apply_operation_if_clock(spec(id, OperationType::SetStarred(true)), clock)
        .unwrap();
    assert!(drain(&rx).is_empty());

    // A batch announces each of its operations.
    let impress_item::GuardedBatch::Applied { operation_ids, .. } = store
        .apply_operations_if_clock(
            vec![
                spec(id, OperationType::SetPayload("a".into(), text("1"))),
                spec(id, OperationType::SetPayload("b".into(), text("2"))),
            ],
            id,
            after,
        )
        .unwrap()
    else {
        panic!("the clock matched");
    };
    let announced: Vec<_> = drain(&rx)
        .into_iter()
        .map(|e| match e {
            ItemEvent::OperationApplied {
                operation_id,
                target_id,
            } => {
                assert_eq!(target_id, id);
                operation_id
            }
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(announced, operation_ids);
}

/// The subscription's schema is a prefix: a subscriber for `task@` hears the
/// events about `task@1.0.0` rows, including operations on them and their
/// deletion, and none about other kinds. One with no schema hears everything.
pub fn subscribe_filters_by_schema_prefix(store: &dyn AttributedStore) {
    if !store.capabilities().subscribe {
        return skip(
            "subscribe_filters_by_schema_prefix",
            "the store declares subscribe: false",
        );
    }
    let tasks = store.subscribe(hear("task@")).unwrap();
    let exact = store.subscribe(hear("task@1.0.0")).unwrap();
    let other_version = store.subscribe(hear("task@2")).unwrap();
    let everything = store.subscribe(ItemQuery::default()).unwrap();

    let note = insert(store, "note@1.0.0", "A Note");
    let task = insert(store, "task@1.0.0", "A Task");

    for rx in [&tasks, &exact] {
        match drain(rx).as_slice() {
            [ItemEvent::Created(c)] => assert_eq!(c.id, task),
            other => panic!("only the task, got {other:?}"),
        }
    }
    assert!(
        drain(&other_version).is_empty(),
        "task@2 is not a prefix of task@1.0.0"
    );
    assert_eq!(drain(&everything).len(), 2);

    // An operation on the note does not reach the task subscribers: the
    // filter is the TARGET's kind, not "anything".
    store
        .update(note, vec![FieldMutation::SetRead(true)])
        .unwrap();
    assert!(
        drain(&tasks).is_empty(),
        "a note operation leaked through the task@ filter"
    );
    assert!(drain(&exact).is_empty());
    assert_eq!(drain(&everything).len(), 1);

    // An operation on the task does.
    store
        .update(task, vec![FieldMutation::SetRead(true)])
        .unwrap();
    for rx in [&tasks, &exact] {
        match drain(rx).as_slice() {
            [ItemEvent::OperationApplied { target_id, .. }] => assert_eq!(*target_id, task),
            other => panic!("expected the task's operation, got {other:?}"),
        }
    }

    // A delete is announced to the kind the row WAS.
    store.delete(note).unwrap();
    assert!(drain(&tasks).is_empty());
    store.delete(task).unwrap();
    assert_eq!(drain(&tasks), vec![ItemEvent::Deleted(task)]);
    assert_eq!(drain(&exact), vec![ItemEvent::Deleted(task)]);
    assert_eq!(
        drain(&everything).len(),
        3,
        "the task's operation and both deletes"
    );
}

/// Subscribers are independent, any number of them; one that is dropped
/// neither blocks writes nor starves the others.
pub fn subscribers_are_independent_and_dropped_ones_are_forgotten(store: &dyn AttributedStore) {
    if !store.capabilities().subscribe {
        return skip(
            "subscribers_are_independent_and_dropped_ones_are_forgotten",
            "the store declares subscribe: false",
        );
    }
    let first = store.subscribe(ItemQuery::default()).unwrap();
    let second = store.subscribe(ItemQuery::default()).unwrap();
    let dead = store.subscribe(ItemQuery::default()).unwrap();
    drop(dead);

    insert(store, "test", "Both hear this");
    assert!(matches!(drain(&first).as_slice(), [ItemEvent::Created(_)]));
    assert!(matches!(drain(&second).as_slice(), [ItemEvent::Created(_)]));
    // Later writes still work and still reach the survivors.
    insert(store, "test", "And this");
    assert_eq!(drain(&first).len(), 1);
    assert_eq!(drain(&second).len(), 1);
}

/// A write that fails announces nothing.
pub fn failed_writes_announce_nothing(store: &dyn AttributedStore) {
    if !store.capabilities().subscribe {
        return skip(
            "failed_writes_announce_nothing",
            "the store declares subscribe: false",
        );
    }
    let existing = item("test", "Existing");
    let existing_id = existing.id;
    store.insert(existing.clone()).unwrap();
    let rx = store.subscribe(ItemQuery::default()).unwrap();

    assert!(store.insert(existing.clone()).is_err(), "duplicate insert");
    let mut clash = item("test", "Clash");
    clash.id = existing_id;
    assert!(store
        .insert_batch(vec![item("test", "Fresh"), clash])
        .is_err());
    assert!(store
        .update(uuid::Uuid::new_v4(), vec![FieldMutation::SetRead(true)])
        .is_err());
    assert!(store.delete(uuid::Uuid::new_v4()).is_err());
    assert!(store
        .update(
            existing_id,
            vec![FieldMutation::AddReference(reference(
                uuid::Uuid::new_v4(),
                EdgeType::Cites
            ))],
        )
        .is_err());
    assert!(
        drain(&rx).is_empty(),
        "no event for a write that did not happen"
    );
}
