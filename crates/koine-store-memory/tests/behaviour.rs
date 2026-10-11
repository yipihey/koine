//! What the memory store promises beyond the conformance suite: the policy
//! hook, the shape of its clock under a controlled wall clock, retention
//! steering, and the authorship of the operations `update` records.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use impress_item::{
    ActorKind, AttributedStore, FieldMutation, GuardedBatch, Item, ItemQuery, ItemStore,
    OperationIntent, OperationSpec, OperationType, Priority, RetentionTier, StoreError, Value,
    Visibility,
};
use koine_store_conformance::fixture::{item, operations_of, spec, text};
use koine_store_memory::{MemoryStore, WritePolicy};

/// Revisions are immutable: no operation may change their payload. Everything
/// else (tags, stars, references) is allowed. This is impress's rule for its
/// `manuscript-revision` kind, expressed as a policy a backend never learns.
struct ImmutableRevisions;

impl WritePolicy for ImmutableRevisions {
    fn check(&self, target: &Item, op: &OperationType) -> Result<(), StoreError> {
        let revision = target.schema.as_str() == "manuscript-revision";
        let payload = matches!(
            op,
            OperationType::SetPayload(..)
                | OperationType::RemovePayload(_)
                | OperationType::PatchPayload(_)
        );
        if revision && payload {
            return Err(StoreError::Validation(format!(
                "manuscript-revision items are immutable: payload-mutating operation on item {} rejected",
                target.id
            )));
        }
        Ok(())
    }
}

fn immutable_store() -> MemoryStore {
    MemoryStore::with_policy(Box::new(ImmutableRevisions))
}

#[test]
fn a_policy_refuses_payload_operations_on_its_kind_and_leaves_the_rest() {
    let store = immutable_store();
    let revision = store.insert(item("manuscript-revision", "v1")).unwrap();
    let regular = store.insert(item("bibliography-entry", "Paper")).unwrap();
    let clock = store.logical_clock_of(revision).unwrap();

    for op in [
        OperationType::SetPayload("title".into(), text("mutated")),
        OperationType::RemovePayload("title".into()),
        OperationType::PatchPayload([("title".to_owned(), text("mutated"))].into()),
    ] {
        let err = store.apply_operation(spec(revision, op)).unwrap_err();
        assert!(
            matches!(&err, StoreError::Validation(m) if m.contains("immutable")),
            "{err:?}"
        );
    }
    // update() goes through the same door.
    assert!(matches!(
        store.update(
            revision,
            vec![FieldMutation::SetPayload("n".into(), Value::Int(1))]
        ),
        Err(StoreError::Validation(_))
    ));
    // Refused means untouched: no payload change, no clock, no operation.
    let after = store.get(revision).unwrap().unwrap();
    assert_eq!(after.payload.get("title"), Some(&text("v1")));
    assert_eq!(store.logical_clock_of(revision).unwrap(), clock);
    assert!(operations_of(&store, revision).is_empty());

    // Envelope operations are fine on a revision, and payload ones on anything else.
    store
        .apply_operation(spec(revision, OperationType::AddTag("filed".into())))
        .unwrap();
    store
        .apply_operation(spec(revision, OperationType::SetStarred(true)))
        .unwrap();
    store
        .apply_operation(spec(
            regular,
            OperationType::SetPayload("title".into(), text("Updated")),
        ))
        .unwrap();
    assert!(store
        .get(revision)
        .unwrap()
        .unwrap()
        .tags
        .contains(&"filed".to_owned()));
}

#[test]
fn a_policy_applies_to_guarded_writes_and_rolls_back_a_guarded_batch() {
    let store = immutable_store();
    let revision = store.insert(item("manuscript-revision", "v1")).unwrap();
    let guard = store.insert(item("test", "Guard")).unwrap();
    let clock = store.logical_clock_of(guard).unwrap().unwrap();

    let refused = store.apply_operation_if_clock(
        spec(
            revision,
            OperationType::SetPayload("title".into(), text("x")),
        ),
        store.logical_clock_of(revision).unwrap().unwrap(),
    );
    assert!(matches!(refused, Err(StoreError::Validation(_))));

    // The first operation is fine, the second violates the policy: neither lands.
    let batch = vec![
        spec(guard, OperationType::SetPayload("a".into(), Value::Int(1))),
        spec(
            revision,
            OperationType::SetPayload("title".into(), text("x")),
        ),
    ];
    assert!(matches!(
        store.apply_operations_if_clock(batch, guard, clock),
        Err(StoreError::Validation(_))
    ));
    assert_eq!(store.logical_clock_of(guard).unwrap(), Some(clock));
    assert!(!store.get(guard).unwrap().unwrap().payload.contains_key("a"));
    assert!(operations_of(&store, guard).is_empty());
}

fn controlled_clock() -> (Arc<AtomicU64>, MemoryStore) {
    let wall = Arc::new(AtomicU64::new(1_000));
    let handle = Arc::clone(&wall);
    let store = MemoryStore::new().with_wall_clock(move || handle.load(Ordering::SeqCst));
    (wall, store)
}

fn clock_of(store: &MemoryStore, title: &str) -> u64 {
    let id = store.insert(item("test", title)).unwrap();
    store.logical_clock_of(id).unwrap().unwrap()
}

#[test]
fn the_clock_packs_wall_milliseconds_and_counts_within_one() {
    let (wall, store) = controlled_clock();
    assert_eq!(clock_of(&store, "a"), 1_000 << 16);
    // Same millisecond: the counter counts.
    assert_eq!(clock_of(&store, "b"), (1_000 << 16) | 1);
    assert_eq!(clock_of(&store, "c"), (1_000 << 16) | 2);
    // The wall clock moves: the counter resets.
    wall.store(1_001, Ordering::SeqCst);
    assert_eq!(clock_of(&store, "d"), 1_001 << 16);
    // The wall clock goes BACKWARDS (a clock adjustment): the clock does not.
    wall.store(900, Ordering::SeqCst);
    assert_eq!(clock_of(&store, "e"), (1_001 << 16) | 1);
}

#[test]
fn the_counter_never_wraps_into_a_clock_already_given() {
    let (wall, store) = controlled_clock();
    let first = clock_of(&store, "first");
    wall.store(1_000, Ordering::SeqCst);
    let mut last = first;
    // More than 65,535 writes inside one millisecond.
    let items: Vec<Item> = (0..70_000)
        .map(|i| item("burst", &format!("{i}")))
        .collect();
    let ids = store.insert_batch(items).unwrap();
    for id in ids {
        let clock = store.logical_clock_of(id).unwrap().unwrap();
        assert!(clock > last, "{clock} must pass {last}");
        last = clock;
    }
    assert!(
        last >> 16 > 1_000,
        "the wall component moved on rather than wrapping"
    );
}

#[test]
fn observing_a_remote_clock_raises_every_later_clock_and_ignores_older_ones() {
    let (_wall, store) = controlled_clock();
    let remote = (4_000_000_000_000u64 << 16) | 7;
    store.observe_clock(remote).unwrap();
    assert!(clock_of(&store, "after") > remote);
    // An older remote clock changes nothing.
    let before = clock_of(&store, "x");
    store.observe_clock(1_234).unwrap();
    assert!(clock_of(&store, "y") > before);
    // Same wall component, higher counter: the counter is taken.
    let (_wall, store) = controlled_clock();
    let first = clock_of(&store, "first");
    store.observe_clock(first + 500).unwrap();
    assert!(clock_of(&store, "second") > first + 500);
}

fn retention_of(
    store: &MemoryStore,
    schema: &str,
    intent: OperationIntent,
    tier: RetentionTier,
) -> RetentionTier {
    let id = store.insert(item(schema, "t")).unwrap();
    let op = store
        .apply_operation(OperationSpec {
            intent,
            retention: tier,
            ..spec(id, OperationType::SetRead(true))
        })
        .unwrap();
    store
        .retention_of(op)
        .unwrap()
        .expect("an operation row has a tier")
}

#[test]
fn routine_compactable_operations_are_steered_by_the_targets_kind() {
    let store = MemoryStore::new();
    let routine = OperationIntent::Routine;
    // Telemetry kinds are ephemeral; every other kind compacts.
    assert_eq!(
        retention_of(
            &store,
            "citation-usage",
            routine,
            RetentionTier::Compactable
        ),
        RetentionTier::Ephemeral
    );
    assert_eq!(
        retention_of(
            &store,
            "bibliography-entry",
            routine,
            RetentionTier::Compactable
        ),
        RetentionTier::Compactable
    );
    // Explicit tiers, and non-routine intents, are never overridden.
    assert_eq!(
        retention_of(&store, "citation-usage", routine, RetentionTier::Durable),
        RetentionTier::Durable
    );
    assert_eq!(
        retention_of(
            &store,
            "citation-usage",
            OperationIntent::Editorial,
            RetentionTier::Compactable
        ),
        RetentionTier::Compactable
    );
    assert_eq!(store.retention_of(uuid::Uuid::new_v4()).unwrap(), None);
    let plain = store.insert(item("test", "plain")).unwrap();
    assert_eq!(
        store.retention_of(plain).unwrap(),
        None,
        "an ordinary row has no tier"
    );
}

#[test]
fn update_operations_are_authored_by_the_stores_default_author() {
    let store = MemoryStore::new().with_author("human:tom", ActorKind::Human);
    let id = store.insert(item("test", "t")).unwrap();
    store
        .update(id, vec![FieldMutation::SetRead(true)])
        .unwrap();
    let ops = operations_of(&store, id);
    assert_eq!(ops[0].author, "human:tom");
    assert_eq!(ops[0].author_kind, ActorKind::Human);

    let default = MemoryStore::new();
    let id = default.insert(item("test", "t")).unwrap();
    default
        .update(id, vec![FieldMutation::SetRead(true)])
        .unwrap();
    let ops = operations_of(&default, id);
    assert_eq!(ops[0].author, "system:local");
    assert_eq!(ops[0].author_kind, ActorKind::System);
}

#[test]
fn rows_carry_this_stores_origin_unless_they_name_another() {
    let store = MemoryStore::new();
    let id = store.insert(item("test", "t")).unwrap();
    assert_eq!(
        store.get(id).unwrap().unwrap().origin.as_deref(),
        Some(store.origin())
    );
    let op = store
        .apply_operation(spec(id, OperationType::SetRead(true)))
        .unwrap();
    assert_eq!(
        store.get(op).unwrap().unwrap().origin.as_deref(),
        Some(store.origin())
    );
    assert_ne!(
        MemoryStore::new().origin(),
        store.origin(),
        "each store is its own origin"
    );
}

#[test]
fn operation_rows_are_ordinary_rows_with_the_oracles_shape() {
    let store = MemoryStore::new();
    let id = store.insert(item("test", "t")).unwrap();
    let op = store
        .apply_operation(spec(id, OperationType::SetPriority(Priority::High)))
        .unwrap();
    let row = store.get(op).unwrap().unwrap();
    assert_eq!(row.priority, Priority::Normal);
    assert_eq!(row.visibility, Visibility::Private);
    assert!(row.tags.is_empty() && row.references.is_empty());
    assert!(!row.is_read && !row.is_starred);
    assert_eq!(row.modified, row.created);
    assert_eq!(row.payload.get("op_data"), Some(&text("high")));
    // A schemaless query sees it, as the oracle's does.
    assert_eq!(store.count(&ItemQuery::default()).unwrap(), 2);
    assert_eq!(operations_of(&store, id).len(), 1);
}

#[test]
fn a_guarded_batch_names_one_batch_whatever_the_specs_said() {
    let store = MemoryStore::new();
    let id = store.insert(item("test", "t")).unwrap();
    let clock = store.logical_clock_of(id).unwrap().unwrap();
    let mut a = spec(id, OperationType::SetRead(true));
    a.batch_id = Some("mine".into());
    let b = spec(id, OperationType::SetStarred(true));
    let GuardedBatch::Applied { operation_ids, .. } = store
        .apply_operations_if_clock(vec![a, b], id, clock)
        .unwrap()
    else {
        panic!("the clock matched");
    };
    let batches: Vec<_> = operation_ids
        .iter()
        .map(|op| store.get(*op).unwrap().unwrap().batch_id)
        .collect();
    assert!(batches[0].is_some() && batches[0] == batches[1]);
    assert_ne!(batches[0].as_deref(), Some("mine"));
}

#[test]
fn a_store_is_shareable_across_threads() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<MemoryStore>();
    let store = Arc::new(MemoryStore::new());
    let writers: Vec<_> = (0..4)
        .map(|t| {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                for i in 0..50 {
                    store.insert(item("test", &format!("{t}-{i}"))).unwrap();
                }
            })
        })
        .collect();
    for w in writers {
        w.join().unwrap();
    }
    assert_eq!(store.count(&ItemQuery::default()).unwrap(), 200);
}
