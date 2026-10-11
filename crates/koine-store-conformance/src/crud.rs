//! Cases for the item CRUD: insert, get, update, delete, batches.

use std::collections::{BTreeMap, BTreeSet};

use impress_item::{
    ActorKind, AttributedStore, EdgeType, FieldMutation, FlagState, ItemId, ItemQuery,
    OperationType, Priority, StoreError, TypedReference, Value, Visibility,
};

use crate::fixture::*;

fn ms(t: chrono::DateTime<chrono::Utc>) -> i64 {
    t.timestamp_millis()
}

fn sorted_tags(tags: &[String]) -> Vec<String> {
    let mut t = tags.to_vec();
    t.sort();
    t
}

/// An item written with every envelope field set comes back with every field.
pub fn insert_get_round_trip(store: &dyn AttributedStore) {
    let producer = insert(store, "test", "Producer");
    let mut it = item("test", "Round Trip");
    it.payload.insert("n".into(), Value::Int(i64::MAX));
    it.payload.insert("f".into(), Value::Float(2.75));
    it.payload.insert("b".into(), Value::Bool(true));
    it.payload.insert("nothing".into(), Value::Null);
    it.payload
        .insert("unicode".into(), text("naïve café — 数据"));
    it.payload.insert(
        "list".into(),
        Value::Array(vec![Value::Int(1), text("two"), Value::Null]),
    );
    it.payload.insert(
        "nested".into(),
        Value::Object(BTreeMap::from([(
            "inner".to_owned(),
            Value::Array(vec![Value::Bool(false)]),
        )])),
    );
    it.author = "agent:reviewer".into();
    it.author_kind = ActorKind::Agent;
    it.created = day(2);
    it.modified = day(3);
    it.canonical_id = Some("shared-123".into());
    it.tags = vec!["topics/cosmo".into(), "methods/sims".into()];
    it.flag = Some(FlagState {
        color: "amber".into(),
        style: Some("dashed".into()),
        length: Some("short".into()),
    });
    it.is_read = true;
    it.is_starred = true;
    it.priority = Priority::High;
    it.visibility = Visibility::Shared;
    it.message_type = Some("email".into());
    it.produced_by = Some(producer);
    it.version = Some("2.0".into());
    it.batch_id = Some("batch-1".into());

    let id = store.insert(it.clone()).expect("insert");
    assert_eq!(id, it.id, "insert returns the item's own id");
    let got = store.get(id).expect("get").expect("the item is there");

    assert_eq!(got.id, it.id);
    assert_eq!(got.schema, it.schema);
    assert_eq!(got.payload, it.payload);
    assert_eq!(got.author, it.author);
    assert_eq!(got.author_kind, it.author_kind);
    assert_eq!(ms(got.created), ms(it.created));
    assert_eq!(ms(got.modified), ms(it.modified));
    assert_eq!(got.canonical_id, it.canonical_id);
    assert_eq!(sorted_tags(&got.tags), sorted_tags(&it.tags));
    assert_eq!(got.flag, it.flag);
    assert!(got.is_read && got.is_starred);
    assert_eq!(got.priority, Priority::High);
    assert_eq!(got.visibility, Visibility::Shared);
    assert_eq!(got.message_type, it.message_type);
    assert_eq!(got.produced_by, it.produced_by);
    assert_eq!(got.version, it.version);
    assert_eq!(got.batch_id, it.batch_id);
    assert_eq!(got.parent, None);
    assert!(got.references.is_empty());
}

/// An id nobody inserted is `None`, not an error.
pub fn get_missing_is_none(store: &dyn AttributedStore) {
    assert!(store.get(uuid::Uuid::new_v4()).expect("get").is_none());
}

/// Inserting an id that exists is `AlreadyExists`, and the first row stays.
pub fn insert_duplicate_is_already_exists(store: &dyn AttributedStore) {
    let first = item("test", "First");
    let id = store.insert(first.clone()).expect("insert");
    let mut second = item("test", "Second");
    second.id = id;
    match store.insert(second) {
        Err(StoreError::AlreadyExists(dup)) => assert_eq!(dup, id),
        other => panic!("expected AlreadyExists, got {other:?}"),
    }
    let kept = store.get(id).unwrap().unwrap();
    assert_eq!(kept.payload.get("title"), Some(&text("First")));
    assert_eq!(store.count(&all_of("test")).unwrap(), 1);
}

/// A clock of zero means "not versioned yet": the store stamps it. A nonzero
/// clock is the writer's (a replica applying remote state) and is kept.
pub fn insert_stamps_a_zero_clock_and_keeps_a_nonzero_one(store: &dyn AttributedStore) {
    let stamped = insert(store, "test", "Stamped");
    let clock = store.get(stamped).unwrap().unwrap().logical_clock;
    assert!(clock > 0, "a zero clock is stamped");

    let mut remote = item("test", "Remote");
    remote.logical_clock = (4_000_000_000_000 << 16) | 7;
    let id = store.insert(remote.clone()).unwrap();
    assert_eq!(
        store.get(id).unwrap().unwrap().logical_clock,
        remote.logical_clock,
        "a nonzero clock is kept verbatim"
    );
    assert_eq!(
        store.logical_clock_of(id).unwrap(),
        Some(remote.logical_clock)
    );
}

/// An origin the writer names is kept; a missing one is stamped with the
/// store's own.
pub fn insert_keeps_a_given_origin_and_stamps_a_missing_one(store: &dyn AttributedStore) {
    let mut named = item("test", "Named");
    named.origin = Some("replica-7".into());
    let named = store.insert(named).unwrap();
    assert_eq!(
        store.get(named).unwrap().unwrap().origin.as_deref(),
        Some("replica-7")
    );

    let anon = insert(store, "test", "Anonymous");
    let origin = store.get(anon).unwrap().unwrap().origin;
    assert!(
        origin.as_deref().is_some_and(|o| !o.is_empty()),
        "a missing origin is stamped, got {origin:?}"
    );
}

/// Tags are a set: a repeated tag is held once.
pub fn insert_collapses_repeated_tags(store: &dyn AttributedStore) {
    let mut it = item("test", "Tagged");
    it.tags = vec!["a".into(), "b".into(), "a".into()];
    let id = store.insert(it).unwrap();
    assert_eq!(
        sorted_tags(&store.get(id).unwrap().unwrap().tags),
        vec!["a".to_owned(), "b".to_owned()]
    );
}

/// A reference given at insert is kept, with its metadata, once per
/// `(target, edge)`.
pub fn insert_keeps_references_once_per_target_and_edge(store: &dyn AttributedStore) {
    let target = insert(store, "test", "Target");
    let mut it = item("test", "Source");
    let meta = BTreeMap::from([("role".to_owned(), text("corresponding"))]);
    it.references = vec![
        TypedReference {
            target,
            edge_type: EdgeType::Cites,
            metadata: Some(meta.clone()),
        },
        reference(target, EdgeType::Cites),
        reference(target, EdgeType::Mentions),
    ];
    let id = store.insert(it).unwrap();
    let got = store.get(id).unwrap().unwrap();
    assert_eq!(got.references.len(), 2, "one per (target, edge): {got:?}");
    let cites = got
        .references
        .iter()
        .find(|r| r.edge_type == EdgeType::Cites)
        .expect("the Cites edge");
    assert_eq!(cites.target, target);
    assert_eq!(cites.metadata, Some(meta), "the first copy wins");
    assert!(got
        .references
        .iter()
        .any(|r| r.edge_type == EdgeType::Mentions));
}

/// A batch of fresh items inserts them all, in order, and reports their ids.
pub fn insert_batch_inserts_every_item(store: &dyn AttributedStore) {
    assert_eq!(
        store.insert_batch(Vec::new()).unwrap(),
        Vec::<ItemId>::new()
    );
    let items: Vec<_> = (0..10)
        .map(|i| item("test", &format!("Item {i}")))
        .collect();
    let expected: Vec<ItemId> = items.iter().map(|i| i.id).collect();
    assert_eq!(store.insert_batch(items).unwrap(), expected);
    assert_eq!(store.count(&all_of("test")).unwrap(), 10);
    let clocks: BTreeSet<u64> = expected
        .iter()
        .map(|id| store.logical_clock_of(*id).unwrap().unwrap())
        .collect();
    assert_eq!(clocks.len(), 10, "every item got its own clock");
}

/// A batch is all or nothing: one duplicate (against the store, or within the
/// batch) and none of the batch is inserted.
pub fn insert_batch_is_atomic(store: &dyn AttributedStore) {
    let existing = insert(store, "test", "Existing");

    let mut clash = item("test", "Clash");
    clash.id = existing;
    let fresh = item("test", "Fresh");
    let fresh_id = fresh.id;
    let err = store
        .insert_batch(vec![fresh, item("test", "Another"), clash])
        .expect_err("a batch with a duplicate id fails");
    assert!(
        matches!(err, StoreError::AlreadyExists(id) if id == existing),
        "{err:?}"
    );
    assert!(
        store.get(fresh_id).unwrap().is_none(),
        "nothing was inserted"
    );
    assert_eq!(store.count(&all_of("test")).unwrap(), 1);

    // The same id twice inside one batch.
    let twin = item("test", "Twin");
    let mut twin_again = item("test", "Twin again");
    twin_again.id = twin.id;
    let twin_id = twin.id;
    let err = store
        .insert_batch(vec![item("test", "Before"), twin, twin_again])
        .expect_err("a batch with an internal duplicate fails");
    assert!(matches!(err, StoreError::AlreadyExists(_)), "{err:?}");
    assert!(store.get(twin_id).unwrap().is_none());
    assert_eq!(store.count(&all_of("test")).unwrap(), 1);

    // A reference to a row that is nowhere also fails the whole batch.
    let mut dangling = item("test", "Dangling");
    dangling.references = vec![reference(uuid::Uuid::new_v4(), EdgeType::Cites)];
    let dangling_id = dangling.id;
    let good = item("test", "Good");
    let good_id = good.id;
    assert!(store.insert_batch(vec![good, dangling]).is_err());
    assert!(store.get(good_id).unwrap().is_none());
    assert!(store.get(dangling_id).unwrap().is_none());
}

/// Items of a batch may refer to items earlier in the same batch, so a batch
/// can be a small graph written in dependency order. (A reference to an item
/// *later* in the batch is not asserted either way: the oracle checks its
/// references as it goes and refuses it; a backend may be more lenient.)
pub fn insert_batch_items_may_refer_to_earlier_items(store: &dyn AttributedStore) {
    let b = item("test", "B");
    let mut a = item("test", "A");
    a.references = vec![reference(b.id, EdgeType::Cites)];
    a.parent = Some(b.id);
    let (a_id, b_id) = (a.id, b.id);
    store
        .insert_batch(vec![b, a])
        .expect("a batch that is closed under reference, in dependency order");
    let a = store.get(a_id).unwrap().unwrap();
    assert_eq!(a.references[0].target, b_id);
    assert_eq!(a.parent, Some(b_id));
}

/// `update` applies every kind of `FieldMutation`.
pub fn update_applies_every_field_mutation(store: &dyn AttributedStore) {
    let parent = insert(store, "folder", "Folder");
    let other = insert(store, "test", "Other");
    let id = insert(store, "test", "Target");
    let flag = FlagState {
        color: "amber".into(),
        style: Some("dashed".into()),
        length: Some("short".into()),
    };

    store
        .update(
            id,
            vec![
                FieldMutation::SetPayload("doi".into(), text("10.1234/test")),
                FieldMutation::SetRead(true),
                FieldMutation::SetStarred(true),
                FieldMutation::SetFlag(Some(flag.clone())),
                FieldMutation::AddTag("methods/sims".into()),
                FieldMutation::AddTag("topics/cosmo".into()),
                FieldMutation::AddReference(reference(other, EdgeType::Cites)),
                FieldMutation::SetParent(Some(parent)),
            ],
        )
        .expect("update");
    let got = store.get(id).unwrap().unwrap();
    assert_eq!(got.payload.get("doi"), Some(&text("10.1234/test")));
    assert_eq!(
        got.payload.get("title"),
        Some(&text("Target")),
        "other keys stay"
    );
    assert!(got.is_read && got.is_starred);
    assert_eq!(got.flag, Some(flag));
    assert_eq!(
        sorted_tags(&got.tags),
        vec!["methods/sims".to_owned(), "topics/cosmo".to_owned()]
    );
    assert_eq!(got.references.len(), 1);
    assert_eq!(got.references[0].target, other);
    assert_eq!(got.references[0].edge_type, EdgeType::Cites);
    assert_eq!(got.parent, Some(parent));

    store
        .update(
            id,
            vec![
                FieldMutation::RemovePayload("doi".into()),
                FieldMutation::SetRead(false),
                FieldMutation::SetStarred(false),
                FieldMutation::SetFlag(None),
                FieldMutation::RemoveTag("methods/sims".into()),
                FieldMutation::RemoveReference(other, EdgeType::Cites),
                FieldMutation::SetParent(None),
            ],
        )
        .expect("update");
    let got = store.get(id).unwrap().unwrap();
    assert!(!got.payload.contains_key("doi"));
    assert!(!got.is_read && !got.is_starred);
    assert_eq!(got.flag, None);
    assert_eq!(got.tags, vec!["topics/cosmo".to_owned()]);
    assert!(got.references.is_empty());
    assert_eq!(got.parent, None);
}

/// A write that changes something moves the row's clock forward and its
/// `modified` to now; `created` never moves.
pub fn update_bumps_clock_and_modified(store: &dyn AttributedStore) {
    let id = insert(store, "test", "Target");
    let before = store.get(id).unwrap().unwrap();
    store
        .update(
            id,
            vec![FieldMutation::SetPayload("n".into(), Value::Int(1))],
        )
        .unwrap();
    let after = store.get(id).unwrap().unwrap();
    assert!(
        after.logical_clock > before.logical_clock,
        "clock {} should pass {}",
        after.logical_clock,
        before.logical_clock
    );
    assert!(after.modified > before.modified, "modified moves to now");
    assert_eq!(ms(after.created), ms(before.created));
    assert_eq!(
        store.logical_clock_of(id).unwrap(),
        Some(after.logical_clock)
    );
}

/// A write that changes nothing mints no operation, bumps no clock and leaves
/// `modified` alone (a re-upsert of unchanged rows once minted 23 million
/// operations). One that does change something mints exactly one.
pub fn update_with_no_change_changes_nothing(store: &dyn AttributedStore) {
    let mut it = item("test", "Target");
    it.payload.insert("cite_key".into(), text("lazeyras18"));
    let id = store.insert(it).unwrap();
    let before = store.get(id).unwrap().unwrap();
    let ops_before = operations_of(store, id).len();

    store
        .update(
            id,
            vec![
                FieldMutation::SetPayload("cite_key".into(), text("lazeyras18")),
                FieldMutation::SetRead(false),
                FieldMutation::SetStarred(false),
                FieldMutation::RemovePayload("absent".into()),
            ],
        )
        .unwrap();
    let after = store.get(id).unwrap().unwrap();
    assert_eq!(
        operations_of(store, id).len(),
        ops_before,
        "no operation minted"
    );
    assert_eq!(after.logical_clock, before.logical_clock, "no clock bump");
    assert_eq!(after.modified, before.modified, "modified untouched");

    store
        .update(
            id,
            vec![FieldMutation::SetPayload("cite_key".into(), text("abel24"))],
        )
        .unwrap();
    assert_eq!(
        operations_of(store, id).len(),
        ops_before + 1,
        "a real change mints one"
    );
    assert_eq!(
        store.get(id).unwrap().unwrap().payload.get("cite_key"),
        Some(&text("abel24"))
    );
}

/// `update` of an id nobody inserted is `NotFound`.
pub fn update_missing_is_not_found(store: &dyn AttributedStore) {
    let err = store
        .update(uuid::Uuid::new_v4(), vec![FieldMutation::SetRead(true)])
        .expect_err("update of a missing row");
    assert!(matches!(err, StoreError::NotFound(_)), "{err:?}");
}

/// `update` records one operation per mutation, and mutations of one call
/// share a batch id (a single mutation needs none).
pub fn update_records_operations_with_a_shared_batch(store: &dyn AttributedStore) {
    let id = insert(store, "test", "Target");
    store
        .update(
            id,
            vec![
                FieldMutation::AddTag("test-tag".into()),
                FieldMutation::SetRead(true),
            ],
        )
        .unwrap();
    let ops = operations_of(store, id);
    assert_eq!(ops.len(), 2);
    assert!(
        ops[0].batch_id.is_some(),
        "a multi-mutation update is a batch"
    );
    assert_eq!(ops[0].batch_id, ops[1].batch_id);
    assert_eq!(op_type_of(&ops[0]), "add_tag");
    assert_eq!(op_type_of(&ops[1]), "set_read");
    for op in &ops {
        assert_eq!(op.payload.get("intent"), Some(&text("routine")));
    }
    let target = store.get(id).unwrap().unwrap();
    assert!(target.is_read && target.tags.contains(&"test-tag".to_owned()));

    store
        .update(id, vec![FieldMutation::SetStarred(true)])
        .unwrap();
    assert_eq!(operations_of(store, id).len(), 3);
}

/// Deleting removes the row; deleting it again, or one that never was, is
/// `NotFound`.
pub fn delete_removes_the_row(store: &dyn AttributedStore) {
    let id = insert(store, "test", "Doomed");
    store.delete(id).expect("delete");
    assert!(store.get(id).unwrap().is_none());
    assert_eq!(store.count(&all_of("test")).unwrap(), 0);
    assert!(matches!(store.delete(id), Err(StoreError::NotFound(_))));
    assert!(matches!(
        store.delete(uuid::Uuid::new_v4()),
        Err(StoreError::NotFound(_))
    ));
}

/// Deleting an item takes with it what hangs on it: its operations, every
/// reference to it, and its children's parent pointer. Rows that merely
/// pointed at it stay.
pub fn delete_cascades(store: &dyn AttributedStore) {
    let doomed = insert(store, "test", "Doomed");
    let mut child = item("test", "Child");
    child.parent = Some(doomed);
    let child = store.insert(child).unwrap();
    let citer = insert(store, "test", "Citer");
    let bystander = insert(store, "test", "Bystander");
    store
        .update(
            citer,
            vec![
                FieldMutation::AddReference(reference(doomed, EdgeType::Cites)),
                FieldMutation::AddReference(reference(bystander, EdgeType::Cites)),
            ],
        )
        .unwrap();
    store
        .update(
            doomed,
            vec![
                FieldMutation::AddTag("t".into()),
                FieldMutation::SetRead(true),
            ],
        )
        .unwrap();
    let op_ids: Vec<_> = operations_of(store, doomed).iter().map(|o| o.id).collect();
    assert_eq!(op_ids.len(), 2);

    store.delete(doomed).unwrap();

    assert!(store.get(doomed).unwrap().is_none());
    for op in op_ids {
        assert!(
            store.get(op).unwrap().is_none(),
            "the operation went with its target"
        );
    }
    assert!(operations_of(store, doomed).is_empty());
    assert_eq!(
        store.get(child).unwrap().unwrap().parent,
        None,
        "child is orphaned, not deleted"
    );
    let citer = store.get(citer).unwrap().unwrap();
    assert_eq!(
        citer.references.len(),
        1,
        "the edge to the deleted row is gone"
    );
    assert_eq!(citer.references[0].target, bystander);
    assert!(store.get(bystander).unwrap().is_some());
}

/// Setting a parent and clearing it; a parent that is not stored is refused.
pub fn parent_can_be_set_cleared_and_must_exist(store: &dyn AttributedStore) {
    let folder = insert(store, "folder", "Folder");
    let id = insert(store, "test", "Child");
    store
        .update(id, vec![FieldMutation::SetParent(Some(folder))])
        .unwrap();
    assert_eq!(store.get(id).unwrap().unwrap().parent, Some(folder));
    store
        .update(id, vec![FieldMutation::SetParent(None)])
        .unwrap();
    assert_eq!(store.get(id).unwrap().unwrap().parent, None);

    let nowhere = uuid::Uuid::new_v4();
    assert!(store
        .update(id, vec![FieldMutation::SetParent(Some(nowhere))])
        .is_err());
    assert_eq!(store.get(id).unwrap().unwrap().parent, None);

    let mut orphan = item("test", "Orphan");
    orphan.parent = Some(nowhere);
    let orphan_id = orphan.id;
    assert!(store.insert_batch(vec![orphan]).is_err());
    assert!(store.get(orphan_id).unwrap().is_none());
}

/// A reference holds once per `(target, edge)`; another edge type to the same
/// target is another reference; removing names both; a target that is not
/// stored is refused.
pub fn references_add_remove_and_must_resolve(store: &dyn AttributedStore) {
    let a = insert(store, "test", "A");
    let b = insert(store, "test", "B");
    let meta = BTreeMap::from([("page".to_owned(), Value::Int(4))]);

    store
        .update(
            a,
            vec![FieldMutation::AddReference(TypedReference {
                target: b,
                edge_type: EdgeType::Cites,
                metadata: Some(meta.clone()),
            })],
        )
        .unwrap();
    let got = store.get(a).unwrap().unwrap();
    assert_eq!(got.references.len(), 1);
    assert_eq!(got.references[0].metadata, Some(meta));

    // The same (target, edge) again changes nothing.
    store
        .update(
            a,
            vec![FieldMutation::AddReference(reference(b, EdgeType::Cites))],
        )
        .unwrap();
    assert_eq!(store.get(a).unwrap().unwrap().references.len(), 1);

    // Another edge type to the same target is its own reference.
    store
        .update(
            a,
            vec![FieldMutation::AddReference(reference(
                b,
                EdgeType::Custom("fixture-link".into()),
            ))],
        )
        .unwrap();
    assert_eq!(store.get(a).unwrap().unwrap().references.len(), 2);

    // Removing one edge leaves the other.
    store
        .update(a, vec![FieldMutation::RemoveReference(b, EdgeType::Cites)])
        .unwrap();
    let got = store.get(a).unwrap().unwrap();
    assert_eq!(got.references.len(), 1);
    assert_eq!(
        got.references[0].edge_type,
        EdgeType::Custom("fixture-link".into())
    );

    // Removing an edge that is not there is not an error.
    store
        .update(a, vec![FieldMutation::RemoveReference(b, EdgeType::Cites)])
        .unwrap();

    // A target that is not stored cannot be referenced.
    let nowhere = uuid::Uuid::new_v4();
    assert!(store
        .update(
            a,
            vec![FieldMutation::AddReference(reference(
                nowhere,
                EdgeType::Cites
            ))]
        )
        .is_err());
    assert_eq!(store.get(a).unwrap().unwrap().references.len(), 1);
}

/// Tags are added once and removed by name.
pub fn tags_are_a_set(store: &dyn AttributedStore) {
    let id = insert(store, "test", "Tagged");
    for tag in ["methods/sims", "topics/cosmo", "methods/sims"] {
        store
            .update(id, vec![FieldMutation::AddTag(tag.into())])
            .unwrap();
    }
    let got = store.get(id).unwrap().unwrap();
    assert_eq!(got.tags.len(), 2, "{:?}", got.tags);
    store
        .update(id, vec![FieldMutation::RemoveTag("methods/sims".into())])
        .unwrap();
    assert_eq!(
        store.get(id).unwrap().unwrap().tags,
        vec!["topics/cosmo".to_owned()]
    );
    store
        .update(id, vec![FieldMutation::RemoveTag("never-there".into())])
        .unwrap();
}

/// Priority and visibility are set by operations, not by `FieldMutation`.
pub fn priority_and_visibility_are_set_by_operations(store: &dyn AttributedStore) {
    let id = insert(store, "test", "Priority");
    store
        .apply_operation(spec(id, OperationType::SetPriority(Priority::Urgent)))
        .unwrap();
    store
        .apply_operation(spec(id, OperationType::SetVisibility(Visibility::Public)))
        .unwrap();
    let got = store.get(id).unwrap().unwrap();
    assert_eq!(got.priority, Priority::Urgent);
    assert_eq!(got.visibility, Visibility::Public);
    assert_eq!(operations_of(store, id).len(), 2);
}

/// Many threads inserting at once: every row lands, none twice.
pub fn concurrent_inserts_all_land(store: &dyn AttributedStore) {
    const THREADS: usize = 8;
    const EACH: usize = 25;
    std::thread::scope(|scope| {
        for t in 0..THREADS {
            scope.spawn(move || {
                for i in 0..EACH {
                    insert(store, "test", &format!("t{t}-{i}"));
                }
            });
        }
    });
    assert_eq!(store.count(&all_of("test")).unwrap(), THREADS * EACH);
    let clocks: BTreeSet<u64> = store
        .query(&all_of("test"))
        .unwrap()
        .iter()
        .map(|i| i.logical_clock)
        .collect();
    assert_eq!(clocks.len(), THREADS * EACH, "no two rows share a clock");
}

/// The default query is every kind (the suite's baseline for "nothing else
/// is in the store"). Kept as a case so a backend that invents rows fails.
pub fn an_empty_store_is_empty(store: &dyn AttributedStore) {
    assert_eq!(store.count(&ItemQuery::default()).unwrap(), 0);
    assert!(store.query(&ItemQuery::default()).unwrap().is_empty());
}
