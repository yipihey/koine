//! Unit tests: every trait method, the D4 layout, the journal, sharing and
//! the lock. The cross-backend semantics are the conformance suite's job
//! (`koine-store-conformance`); these pin what is specific to a directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use impress_item::*;
use uuid::Uuid;

use crate::*;

/// A scratch root under the system temp dir, removed on drop.
struct TempRoot(PathBuf);

impl TempRoot {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("koine-store-dir-test-{}", Uuid::new_v4()));
        TempRoot(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn open() -> (TempRoot, DirStore) {
    let root = TempRoot::new();
    let store = DirStore::open(root.path()).unwrap();
    (root, store)
}

fn schema(name: &str) -> SchemaRef {
    SchemaRef::from_stored(name.to_owned())
}

fn make_item(schema_ref: &str, title: &str) -> Item {
    let mut payload = BTreeMap::new();
    payload.insert("title".into(), Value::String(title.into()));
    Item {
        id: Uuid::new_v4(),
        schema: schema(schema_ref),
        payload,
        created: Utc::now(),
        modified: Utc::now(),
        author: "test@example.com".into(),
        author_kind: ActorKind::Human,
        logical_clock: 0,
        origin: None,
        canonical_id: None,
        tags: vec![],
        flag: None,
        is_read: false,
        is_starred: false,
        priority: Priority::Normal,
        visibility: Visibility::Private,
        message_type: None,
        produced_by: None,
        version: None,
        batch_id: None,
        references: vec![],
        parent: None,
    }
}

fn spec(target: ItemId, op: OperationType) -> OperationSpec {
    OperationSpec {
        target_id: target,
        op_type: op,
        intent: OperationIntent::Editorial,
        reason: Some("test".into()),
        batch_id: None,
        author: "tester".into(),
        author_kind: ActorKind::Agent,
        retention: RetentionTier::Durable,
    }
}

fn cites(target: ItemId) -> TypedReference {
    TypedReference {
        target,
        edge_type: EdgeType::Cites,
        metadata: None,
    }
}

fn q(schema_ref: Option<&str>, predicates: Vec<Predicate>) -> ItemQuery {
    ItemQuery {
        schema: schema_ref.map(schema),
        predicates,
        ..Default::default()
    }
}

fn ids(items: &[Item]) -> Vec<ItemId> {
    items.iter().map(|i| i.id).collect()
}

fn files_under(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                stack.push(entry.path());
            } else {
                out.push(
                    entry
                        .path()
                        .strip_prefix(dir)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    out.sort();
    out
}

// ── the port ────────────────────────────────────────────────────────────────

#[test]
fn capabilities_are_the_plan_d4() {
    let (_root, store) = open();
    assert_eq!(
        store.capabilities(),
        StoreCapabilities {
            indexed_query: false,
            atomic_batch: true,
            subscribe: true,
            cross_process_events: false,
            durable: true,
            shared: true,
            blobs: true,
        }
    );
}

#[test]
fn insert_get_round_trip_and_duplicate() {
    let (_root, store) = open();
    let mut item = make_item("bibliography-entry", "Dark Matter");
    item.tags = vec!["b".into(), "a".into(), "a".into()];
    item.flag = Some(FlagState {
        color: "red".into(),
        style: Some("dashed".into()),
        length: None,
    });
    item.priority = Priority::High;
    item.references = vec![];
    let id = store.insert(item.clone()).unwrap();
    assert_eq!(id, item.id);

    let got = store.get(id).unwrap().unwrap();
    assert_eq!(got.payload, item.payload);
    assert_eq!(got.schema, item.schema);
    assert_eq!(got.tags, vec!["a", "b"], "tags are a sorted set");
    assert_eq!(got.flag, item.flag);
    assert_eq!(got.priority, Priority::High);
    assert_eq!(
        got.created.timestamp_millis(),
        item.created.timestamp_millis()
    );
    assert_ne!(got.logical_clock, 0, "a zero clock is stamped");
    assert_eq!(got.origin.as_deref(), Some(store.store_id()));

    assert!(matches!(
        store.insert(item).unwrap_err(),
        StoreError::AlreadyExists(i) if i == id
    ));
    assert!(store.get(Uuid::new_v4()).unwrap().is_none());
}

#[test]
fn a_nonzero_clock_and_origin_are_kept_verbatim() {
    let (_root, store) = open();
    let mut item = make_item("test", "Synced");
    item.logical_clock = 42;
    item.origin = Some("elsewhere".into());
    let id = store.insert(item).unwrap();
    let got = store.get(id).unwrap().unwrap();
    assert_eq!(got.logical_clock, 42);
    assert_eq!(got.origin.as_deref(), Some("elsewhere"));
}

#[test]
fn update_materializes_every_field_mutation() {
    let (_root, store) = open();
    let target = store.insert(make_item("test", "Target")).unwrap();
    let other = store.insert(make_item("test", "Other")).unwrap();
    let before = store.logical_clock_of(target).unwrap().unwrap();

    store
        .update(
            target,
            vec![
                FieldMutation::SetPayload("n".into(), Value::Int(3)),
                FieldMutation::SetPayload("title".into(), Value::String("New".into())),
                FieldMutation::SetRead(true),
                FieldMutation::SetStarred(true),
                FieldMutation::SetFlag(Some(FlagState {
                    color: "amber".into(),
                    style: None,
                    length: None,
                })),
                FieldMutation::AddTag("methods/sims".into()),
                FieldMutation::AddTag("methods/sims".into()),
                FieldMutation::AddReference(cites(other)),
                FieldMutation::AddReference(cites(other)),
                FieldMutation::SetParent(Some(other)),
            ],
        )
        .unwrap();
    let t = store.get(target).unwrap().unwrap();
    assert_eq!(t.payload.get("n"), Some(&Value::Int(3)));
    assert_eq!(t.payload.get("title"), Some(&Value::String("New".into())));
    assert!(t.is_read && t.is_starred);
    assert_eq!(t.flag.as_ref().unwrap().color, "amber");
    assert_eq!(t.tags, vec!["methods/sims"]);
    assert_eq!(t.references.len(), 1);
    assert_eq!(t.parent, Some(other));
    assert!(t.logical_clock > before);

    store
        .update(
            target,
            vec![
                FieldMutation::RemovePayload("n".into()),
                FieldMutation::SetFlag(None),
                FieldMutation::RemoveTag("methods/sims".into()),
                FieldMutation::RemoveReference(other, EdgeType::Cites),
                FieldMutation::SetParent(None),
            ],
        )
        .unwrap();
    let t = store.get(target).unwrap().unwrap();
    assert!(!t.payload.contains_key("n"));
    assert!(t.flag.is_none() && t.tags.is_empty() && t.references.is_empty());
    assert!(t.parent.is_none());

    // The operations were recorded: 8 distinct changes, then 5.
    let ops = store
        .query(&q(
            Some("core/operation"),
            vec![Predicate::Eq(
                "op_target_id".into(),
                Value::String(target.to_string()),
            )],
        ))
        .unwrap();
    assert_eq!(
        ops.len(),
        10 + 5,
        "a repeated tag or edge still mints its operation"
    );
}

#[test]
fn update_errors_and_noops() {
    let (_root, store) = open();
    let err = store
        .update(Uuid::new_v4(), vec![FieldMutation::SetRead(true)])
        .unwrap_err();
    assert!(matches!(err, StoreError::NotFound(_)));
    // The oracle never looks the row up for an empty mutation list.
    store.update(Uuid::new_v4(), vec![]).unwrap();

    let mut item = make_item("test", "Target");
    item.payload.insert("k".into(), Value::Int(1));
    let id = store.insert(item).unwrap();
    let clock = store.logical_clock_of(id).unwrap().unwrap();
    store
        .update(
            id,
            vec![
                FieldMutation::SetPayload("k".into(), Value::Int(1)),
                FieldMutation::RemovePayload("absent".into()),
                FieldMutation::SetRead(false),
                FieldMutation::SetStarred(false),
            ],
        )
        .unwrap();
    assert_eq!(
        store.logical_clock_of(id).unwrap(),
        Some(clock),
        "no clock bump"
    );
    assert_eq!(store.count(&q(Some("core/operation"), vec![])).unwrap(), 0);

    // A reference to nothing is refused, and nothing is written.
    let err = store
        .update(
            id,
            vec![
                FieldMutation::AddTag("t".into()),
                FieldMutation::AddReference(cites(Uuid::new_v4())),
            ],
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::Validation(_)));
    assert!(store.get(id).unwrap().unwrap().tags.is_empty());
    assert_eq!(store.count(&q(Some("core/operation"), vec![])).unwrap(), 0);
}

#[test]
fn update_uses_one_batch_id_for_several_mutations() {
    let (_root, store) = open();
    let id = store.insert(make_item("test", "T")).unwrap();
    store
        .update(
            id,
            vec![
                FieldMutation::SetRead(true),
                FieldMutation::SetStarred(true),
            ],
        )
        .unwrap();
    let ops = store.query(&q(Some("core/operation"), vec![])).unwrap();
    assert_eq!(ops.len(), 2);
    assert!(ops[0].batch_id.is_some() && ops[0].batch_id == ops[1].batch_id);
    assert_eq!(ops[0].author, "system:local");
    assert_eq!(ops[0].author_kind, ActorKind::System);

    store
        .update(id, vec![FieldMutation::SetRead(false)])
        .unwrap();
    let ops = store.query(&q(Some("core/operation"), vec![])).unwrap();
    assert_eq!(ops.iter().filter(|o| o.batch_id.is_none()).count(), 1);
}

#[test]
fn delete_cascades_like_the_foreign_keys() {
    let (root, store) = open();
    let parent = store.insert(make_item("test", "Parent")).unwrap();
    let mut child = make_item("test", "Child");
    child.parent = Some(parent);
    let child = store.insert(child).unwrap();
    let mut citer = make_item("test", "Citer");
    citer.references = vec![cites(parent)];
    let citer = store.insert(citer).unwrap();
    store
        .update(parent, vec![FieldMutation::SetRead(true)])
        .unwrap();
    store.put_blob(parent, "a.pdf", b"x").unwrap();
    assert_eq!(store.count(&q(Some("core/operation"), vec![])).unwrap(), 1);

    store.delete(parent).unwrap();
    assert!(store.get(parent).unwrap().is_none());
    assert!(store.get(child).unwrap().unwrap().parent.is_none());
    assert!(store.get(citer).unwrap().unwrap().references.is_empty());
    assert_eq!(store.count(&q(Some("core/operation"), vec![])).unwrap(), 0);
    assert!(store.list_blobs(parent).unwrap().is_empty());
    assert!(!root.path().join("blobs").join(parent.to_string()).exists());

    assert!(matches!(
        store.delete(parent).unwrap_err(),
        StoreError::NotFound(_)
    ));
}

#[test]
fn insert_batch_is_all_or_nothing() {
    let (_root, store) = open();
    let existing = make_item("test", "Existing");
    store.insert(existing.clone()).unwrap();

    let items: Vec<Item> = (0..5)
        .map(|i| make_item("test", &format!("n{i}")))
        .collect();
    let mut with_duplicate = items.clone();
    with_duplicate.push(existing);
    assert!(matches!(
        store.insert_batch(with_duplicate).unwrap_err(),
        StoreError::AlreadyExists(_)
    ));
    assert_eq!(
        store.count(&ItemQuery::default()).unwrap(),
        1,
        "nothing landed"
    );

    // A dangling parent fails the whole batch; one that points at an earlier
    // member of the same batch is fine.
    let mut dangling = make_item("test", "dangling");
    dangling.parent = Some(Uuid::new_v4());
    let mut bad = items.clone();
    bad.push(dangling);
    assert!(store.insert_batch(bad).is_err());
    assert_eq!(store.count(&ItemQuery::default()).unwrap(), 1);

    let mut later = make_item("test", "later");
    later.parent = Some(items[0].id);
    let mut good = items.clone();
    good.push(later);
    let got = store.insert_batch(good).unwrap();
    assert_eq!(got.len(), 6);
    assert_eq!(store.count(&ItemQuery::default()).unwrap(), 7);
    assert!(store.insert_batch(vec![]).unwrap().is_empty());
}

#[test]
fn query_filters_sorts_and_pages() {
    let (_root, store) = open();
    let mut made = Vec::new();
    for (i, title) in ["delta", "alpha", "charlie", "bravo"].iter().enumerate() {
        let mut item = make_item("paper", title);
        item.payload.insert("n".into(), Value::Int(i as i64));
        item.is_read = i % 2 == 0;
        item.tags = vec![format!("t/{i}")];
        made.push(store.insert(item).unwrap());
    }
    store.insert(make_item("note", "other kind")).unwrap();

    // schema is exact equality; None is every kind
    assert_eq!(store.query(&q(Some("paper"), vec![])).unwrap().len(), 4);
    assert_eq!(store.query(&q(Some("pap"), vec![])).unwrap().len(), 0);
    assert_eq!(store.query(&q(None, vec![])).unwrap().len(), 5);
    assert_eq!(
        store
            .count(&q(Some("paper"), vec![Predicate::IsRead(true)]))
            .unwrap(),
        2
    );

    // insertion order when unsorted
    let unsorted = store.query(&q(Some("paper"), vec![])).unwrap();
    assert_eq!(ids(&unsorted), made);

    let sorted = |field: &str, ascending: bool| {
        store
            .query(&ItemQuery {
                schema: Some(schema("paper")),
                sort: vec![SortDescriptor {
                    field: field.into(),
                    ascending,
                }],
                ..Default::default()
            })
            .unwrap()
    };
    let titles: Vec<_> = sorted("payload.title", true)
        .iter()
        .map(|i| i.payload["title"].clone())
        .collect();
    assert_eq!(
        titles,
        ["alpha", "bravo", "charlie", "delta"].map(|s| Value::String(s.into()))
    );
    assert_eq!(
        ids(&sorted("payload.n", false)),
        made.iter().rev().copied().collect::<Vec<_>>()
    );

    let page = store
        .query(&ItemQuery {
            schema: Some(schema("paper")),
            sort: vec![SortDescriptor {
                field: "payload.n".into(),
                ascending: true,
            }],
            limit: Some(2),
            offset: Some(1),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(ids(&page), made[1..3]);
    assert_eq!(
        store
            .count(&ItemQuery {
                schema: Some(schema("paper")),
                limit: Some(1),
                ..Default::default()
            })
            .unwrap(),
        4,
        "count ignores paging"
    );

    // predicates: comparisons, contains, in, tag, logic
    let hit = |p: Predicate| store.query(&q(Some("paper"), vec![p])).unwrap().len();
    assert_eq!(hit(Predicate::Gte("payload.n".into(), Value::Int(2))), 2);
    assert_eq!(hit(Predicate::Lt("payload.n".into(), Value::Int(1))), 1);
    assert_eq!(
        hit(Predicate::Contains("payload.title".into(), "BRAV".into())),
        1,
        "full-text: a token prefix, any case"
    );
    assert_eq!(
        hit(Predicate::Contains("payload.title".into(), "rav".into())),
        0,
        "full-text: not a substring"
    );
    assert_eq!(
        hit(Predicate::Contains("payload.n".into(), "2".into())),
        1,
        "other fields: a substring of the value read as text"
    );
    assert_eq!(
        hit(Predicate::In(
            "payload.n".into(),
            vec![Value::Int(0), Value::Int(3)]
        )),
        2
    );
    assert_eq!(hit(Predicate::HasTag("t".into())), 4);
    assert_eq!(hit(Predicate::HasTag("t/2".into())), 1);
    assert_eq!(
        hit(Predicate::And(vec![
            Predicate::IsRead(true),
            Predicate::Not(Box::new(Predicate::Eq("payload.n".into(), Value::Int(0)))),
        ])),
        1
    );
    assert_eq!(
        hit(Predicate::Neq("payload.absent".into(), Value::Int(0))),
        0
    );
    assert_eq!(hit(Predicate::HasParent(made[0])), 0);

    // include_* trim the rows, not the matching
    let trimmed = store
        .query(&ItemQuery {
            schema: Some(schema("paper")),
            predicates: vec![Predicate::HasTag("t/0".into())],
            include_tags: false,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(trimmed.len(), 1);
    assert!(trimmed[0].tags.is_empty());
}

#[test]
fn query_graph_predicates_see_both_directions() {
    let (_root, store) = open();
    let a = store.insert(make_item("test", "A")).unwrap();
    let b = store.insert(make_item("test", "B")).unwrap();
    let c = store.insert(make_item("test", "C")).unwrap();
    store
        .update(a, vec![FieldMutation::AddReference(cites(b))])
        .unwrap();
    store
        .update(c, vec![FieldMutation::AddReference(cites(b))])
        .unwrap();
    let run = |p| ids(&store.query(&q(None, vec![p])).unwrap());
    let mut citers = run(Predicate::HasReference(EdgeType::Cites, b));
    citers.sort();
    let mut expect = vec![a, c];
    expect.sort();
    assert_eq!(citers, expect);
    assert_eq!(run(Predicate::ReferencedBy(EdgeType::Cites, a)), vec![b]);
    assert!(run(Predicate::ReferencedBy(EdgeType::Mentions, a)).is_empty());
}

#[test]
fn neighbors_walk_both_directions_breadth_first() {
    let (_root, store) = open();
    let ids4: Vec<ItemId> = ["A", "B", "C", "D"]
        .iter()
        .map(|t| store.insert(make_item("test", t)).unwrap())
        .collect();
    for (source, target, edge) in [
        (ids4[0], ids4[1], EdgeType::Cites),
        (ids4[2], ids4[0], EdgeType::Custom("fixture-link".into())),
        (ids4[1], ids4[3], EdgeType::Cites),
        (ids4[3], ids4[1], EdgeType::Cites),
    ] {
        store
            .update(
                source,
                vec![FieldMutation::AddReference(TypedReference {
                    target,
                    edge_type: edge,
                    metadata: None,
                })],
            )
            .unwrap();
    }
    let n = |edges: &[EdgeType], depth| {
        let got = store.neighbors(ids4[0], edges, depth).unwrap();
        let mut set: Vec<_> = ids(&got);
        let len = set.len();
        set.sort();
        set.dedup();
        assert_eq!(set.len(), len, "no item twice");
        assert!(!set.contains(&ids4[0]), "never the start");
        set
    };
    let sorted = |mut v: Vec<ItemId>| {
        v.sort();
        v
    };
    assert!(n(&[], 0).is_empty());
    assert_eq!(n(&[], 1), sorted(vec![ids4[1], ids4[2]]));
    assert_eq!(n(&[], 2), sorted(vec![ids4[1], ids4[2], ids4[3]]));
    assert_eq!(n(&[EdgeType::Cites], 1), vec![ids4[1]]);
    assert_eq!(n(&[EdgeType::Cites], 2), sorted(vec![ids4[1], ids4[3]]));
    assert!(n(&[EdgeType::Custom("absent".into())], 3).is_empty());
    // breadth first: the nearer ring comes first
    let order = ids(&store.neighbors(ids4[0], &[], 2).unwrap());
    assert_eq!(order[2], ids4[3]);
    assert!(store.neighbors(Uuid::new_v4(), &[], 2).unwrap().is_empty());
}

#[test]
fn events_are_filtered_by_schema_prefix_and_dead_receivers_pruned() {
    let (_root, store) = open();
    let tasks = store
        .subscribe(ItemQuery {
            schema: Some(schema("task@")),
            ..Default::default()
        })
        .unwrap();
    let everything = store.subscribe(ItemQuery::default()).unwrap();
    let dead = store.subscribe(ItemQuery::default()).unwrap();
    drop(dead);

    let note = store.insert(make_item("note@1.0.0", "N")).unwrap();
    let task = store.insert(make_item("task@1.0.0", "T")).unwrap();
    match tasks.try_recv().unwrap() {
        ItemEvent::Created(i) => assert_eq!(i.id, task),
        other => panic!("{other:?}"),
    }
    assert!(tasks.try_recv().is_err());
    assert!(matches!(
        everything.try_recv().unwrap(),
        ItemEvent::Created(_)
    ));
    assert!(matches!(
        everything.try_recv().unwrap(),
        ItemEvent::Created(_)
    ));
    assert_eq!(
        store.subscribers.lock().unwrap().len(),
        2,
        "the dead one was pruned"
    );

    store
        .update(note, vec![FieldMutation::SetRead(true)])
        .unwrap();
    assert!(
        tasks.try_recv().is_err(),
        "a note's operation does not reach task@"
    );
    store
        .update(task, vec![FieldMutation::SetRead(true)])
        .unwrap();
    match tasks.try_recv().unwrap() {
        ItemEvent::OperationApplied { target_id, .. } => assert_eq!(target_id, task),
        other => panic!("{other:?}"),
    }
    store.delete(task).unwrap();
    assert!(matches!(tasks.try_recv().unwrap(), ItemEvent::Deleted(i) if i == task));

    // a batch announces every item after it commits
    let before = everything.try_iter().count();
    assert!(before >= 3);
    store
        .insert_batch(vec![make_item("test", "a"), make_item("test", "b")])
        .unwrap();
    assert_eq!(everything.try_iter().count(), 2);
}

#[test]
fn apply_operation_records_an_operation_row_and_materializes() {
    let (root, store) = open();
    let id = store.insert(make_item("test", "Target")).unwrap();
    let rx = store.subscribe(ItemQuery::default()).unwrap();
    let before = store.logical_clock_of(id).unwrap().unwrap();

    let op_id = store
        .apply_operation(spec(id, OperationType::AddTag("methods/sims".into())))
        .unwrap();
    let op = store.get(op_id).unwrap().unwrap();
    assert_eq!(op.schema.as_str(), "core/operation");
    assert_eq!(
        op.payload.get("op_type"),
        Some(&Value::String("add_tag".into()))
    );
    assert_eq!(
        op.payload.get("intent"),
        Some(&Value::String("editorial".into()))
    );
    assert_eq!(
        op.payload.get("reason"),
        Some(&Value::String("test".into()))
    );
    assert_eq!(op.author, "tester");
    assert_eq!(op.author_kind, ActorKind::Agent);
    assert!(op.logical_clock > before);
    assert!(root
        .path()
        .join("operations")
        .join(format!("{op_id}.json"))
        .is_file());
    let target = store.get(id).unwrap().unwrap();
    assert_eq!(target.tags, vec!["methods/sims"]);
    assert_eq!(
        target.logical_clock, op.logical_clock,
        "op and target share a clock"
    );
    match rx.try_recv().unwrap() {
        ItemEvent::OperationApplied {
            operation_id,
            target_id,
        } => assert_eq!((operation_id, target_id), (op_id, id)),
        other => panic!("{other:?}"),
    }

    // `prev` is captured for undo; a missing target is refused
    store
        .apply_operation(spec(
            id,
            OperationType::SetPayload("n".into(), Value::Int(1)),
        ))
        .unwrap();
    let op2 = store
        .apply_operation(spec(
            id,
            OperationType::SetPayload("n".into(), Value::Int(2)),
        ))
        .unwrap();
    assert_eq!(
        store.get(op2).unwrap().unwrap().payload.get("prev"),
        Some(&Value::Int(1))
    );
    assert!(matches!(
        store
            .apply_operation(spec(Uuid::new_v4(), OperationType::SetRead(true)))
            .unwrap_err(),
        StoreError::NotFound(_)
    ));

    // routine + compactable follows the per-kind policy
    let routine = OperationSpec {
        intent: OperationIntent::Routine,
        retention: RetentionTier::Compactable,
        ..spec(id, OperationType::SetRead(true))
    };
    let routine_id = store.apply_operation(routine).unwrap();
    let (path, is_op) = store.locate(routine_id).unwrap().unwrap();
    assert!(is_op);
    let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(doc["retention"], "compactable");
    assert_eq!(doc["op_target_id"], id.to_string());
}

#[test]
fn a_guarded_write_applies_only_on_the_clock_it_was_read_at() {
    let (_root, store) = open();
    let id = store.insert(make_item("test", "Target")).unwrap();
    let set = |v: &str| {
        spec(
            id,
            OperationType::SetPayload("title".into(), Value::String(v.into())),
        )
    };

    let read = store.logical_clock_of(id).unwrap().unwrap();
    let GuardedWrite::Applied {
        clock,
        operation_id,
    } = store.apply_operation_if_clock(set("mine"), read).unwrap()
    else {
        panic!("a matching clock writes");
    };
    assert!(clock > read);
    assert_eq!(store.logical_clock_of(id).unwrap(), Some(clock));
    assert!(store.get(operation_id).unwrap().is_some());

    store.apply_operation(set("theirs")).unwrap();
    let stale = store.apply_operation_if_clock(set("stale"), clock).unwrap();
    assert!(matches!(stale, GuardedWrite::Moved { clock: Some(now) } if now != clock));
    assert_eq!(
        store.get(id).unwrap().unwrap().payload.get("title"),
        Some(&Value::String("theirs".into())),
        "the refused write left the row alone"
    );
    assert_eq!(
        store
            .apply_operation_if_clock(
                OperationSpec {
                    target_id: Uuid::new_v4(),
                    ..set("nobody")
                },
                clock
            )
            .unwrap(),
        GuardedWrite::Moved { clock: None }
    );
}

#[test]
fn a_guarded_batch_is_all_or_nothing() {
    let (_root, store) = open();
    let id = store.insert(make_item("test", "Target")).unwrap();
    let set = |f: &str, v: Value| spec(id, OperationType::SetPayload(f.into(), v));
    let batch = |t: &str, n: i64| {
        vec![
            set("title", Value::String(t.into())),
            set("n", Value::Int(n)),
        ]
    };
    let read = store.logical_clock_of(id).unwrap().unwrap();
    let GuardedBatch::Applied {
        operation_ids,
        clock,
    } = store
        .apply_operations_if_clock(batch("first", 2), id, read)
        .unwrap()
    else {
        panic!("a matching clock writes");
    };
    assert_eq!(operation_ids.len(), 2);
    assert_eq!(store.logical_clock_of(id).unwrap(), Some(clock));
    let ops: Vec<Item> = operation_ids
        .iter()
        .map(|o| store.get(*o).unwrap().unwrap())
        .collect();
    assert!(ops[0].batch_id.is_some() && ops[0].batch_id == ops[1].batch_id);
    assert!(ops[0].logical_clock < ops[1].logical_clock);

    assert_eq!(
        store
            .apply_operations_if_clock(batch("second", 2), id, read)
            .unwrap(),
        GuardedBatch::Moved { clock: Some(clock) }
    );

    let broken = vec![
        set("title", Value::String("half".into())),
        spec(
            Uuid::new_v4(),
            OperationType::SetPayload("n".into(), Value::Int(3)),
        ),
    ];
    assert!(matches!(
        store.apply_operations_if_clock(broken, id, clock),
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(store.logical_clock_of(id).unwrap(), Some(clock));
    assert_eq!(
        store.get(id).unwrap().unwrap().payload.get("title"),
        Some(&Value::String("first".into()))
    );
    assert_eq!(store.count(&q(Some("core/operation"), vec![])).unwrap(), 2);

    assert!(store.apply_operations_if_clock(vec![], id, clock).is_err());
    assert_eq!(
        store
            .apply_operations_if_clock(batch("x", 9), Uuid::new_v4(), clock)
            .unwrap(),
        GuardedBatch::Moved { clock: None }
    );
}

#[test]
fn the_write_policy_can_refuse_an_operation_before_anything_is_written() {
    struct NoPayloadOnLocked;
    impl WritePolicy for NoPayloadOnLocked {
        fn check(&self, target: &Item, op: &OperationType) -> Result<(), StoreError> {
            if target.schema.as_str() == "locked"
                && matches!(
                    op,
                    OperationType::SetPayload(..)
                        | OperationType::RemovePayload(_)
                        | OperationType::PatchPayload(_)
                )
            {
                return Err(StoreError::Validation(format!(
                    "{} is immutable",
                    target.id
                )));
            }
            Ok(())
        }
    }
    let root = TempRoot::new();
    let store = DirStore::open(root.path())
        .unwrap()
        .with_policy(Arc::new(NoPayloadOnLocked));
    let locked = store.insert(make_item("locked", "L")).unwrap();
    let free = store.insert(make_item("free", "F")).unwrap();
    let err = store
        .update(
            locked,
            vec![FieldMutation::SetPayload("a".into(), Value::Int(1))],
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::Validation(_)));
    assert!(!store
        .get(locked)
        .unwrap()
        .unwrap()
        .payload
        .contains_key("a"));
    assert_eq!(store.count(&q(Some("core/operation"), vec![])).unwrap(), 0);
    // envelope operations stay allowed, and other kinds are untouched
    store
        .update(locked, vec![FieldMutation::AddTag("t".into())])
        .unwrap();
    store
        .update(
            free,
            vec![FieldMutation::SetPayload("a".into(), Value::Int(1))],
        )
        .unwrap();
}

// ── the layout (D4) ─────────────────────────────────────────────────────────

#[test]
fn the_layout_on_disk_is_d4() {
    let (root, store) = open();
    let r = root.path();
    for dir in ["items", "operations", "blobs", "journal"] {
        assert!(r.join(dir).is_dir(), "{dir}/");
    }
    assert!(r.join("lock").is_file());
    let meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(r.join("koine-store.json")).unwrap()).unwrap();
    assert_eq!(meta["format"], 1);
    assert_eq!(meta["store_id"], store.store_id());
    assert!(Uuid::parse_str(meta["store_id"].as_str().unwrap()).is_ok());
    assert!(meta["created"]
        .as_str()
        .unwrap()
        .parse::<chrono::DateTime<Utc>>()
        .is_ok());

    let a = store.insert(make_item("imbib/library", "A")).unwrap();
    let b = store.insert(make_item("task@1.0.0", "B")).unwrap();
    let op = store
        .apply_operation(spec(a, OperationType::SetRead(true)))
        .unwrap();
    store.put_blob(a, "paper.pdf", b"%PDF").unwrap();

    assert!(r.join(format!("items/imbib/library/{a}.json")).is_file());
    assert!(r.join(format!("items/task@1.0.0/{b}.json")).is_file());
    assert!(r.join(format!("operations/{op}.json")).is_file());
    assert!(r.join(format!("blobs/{a}/paper.pdf")).is_file());
    let clock = std::fs::read_to_string(r.join("clock")).unwrap();
    let parts: Vec<&str> = clock.trim_end().split(' ').collect();
    assert_eq!(parts.len(), 2);
    assert!(clock.ends_with('\n'));
    assert!(parts.iter().all(|p| p.parse::<u64>().is_ok()));
    assert_eq!(
        std::fs::read_dir(r.join("journal")).unwrap().count(),
        0,
        "no journal is left behind"
    );
    // a record is the item, legible
    let doc: serde_json::Value = serde_json::from_slice(
        &std::fs::read(r.join(format!("items/imbib/library/{a}.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(doc["schema"], "imbib/library");
    assert_eq!(doc["payload"]["title"], "A");
    assert_eq!(doc["is_read"], true);
    // no temporaries survive
    assert!(
        files_under(r).iter().all(|f| !f.contains(".tmp-")),
        "{:?}",
        files_under(r)
    );
}

/// Prints the tree of a used root; `cargo test -- --nocapture tree` shows it.
#[test]
fn tree() {
    let (root, store) = open();
    let a = store.insert(make_item("imbib/library", "A")).unwrap();
    store.insert(make_item("task@1.0.0", "B")).unwrap();
    store
        .update(a, vec![FieldMutation::AddTag("x".into())])
        .unwrap();
    store.put_blob(a, "paper.pdf", b"%PDF").unwrap();
    let files = files_under(root.path());
    println!("{}", files.join("\n"));
    assert_eq!(files.len(), 7);
}

#[test]
fn schema_refs_with_slash_and_at_land_in_the_right_folders() {
    let (root, store) = open();
    let nested = store
        .insert(make_item("impress/ui/layout@1.0.0", "L"))
        .unwrap();
    let flat = store.insert(make_item("manuscript", "M")).unwrap();
    let parent_and_child = [
        store.insert(make_item("imbib", "shallow")).unwrap(),
        store.insert(make_item("imbib/library", "deep")).unwrap(),
    ];
    let r = root.path().join("items");
    assert!(r
        .join(format!("impress/ui/layout@1.0.0/{nested}.json"))
        .is_file());
    assert!(r.join(format!("manuscript/{flat}.json")).is_file());
    // a kind named like another kind's folder is still its own kind
    assert!(r
        .join(format!("imbib/{}.json", parent_and_child[0]))
        .is_file());
    assert!(r
        .join(format!("imbib/library/{}.json", parent_and_child[1]))
        .is_file());
    assert_eq!(
        ids(&store.query(&q(Some("imbib"), vec![])).unwrap()),
        vec![parent_and_child[0]]
    );
    assert_eq!(
        ids(&store.query(&q(Some("imbib/library"), vec![])).unwrap()),
        vec![parent_and_child[1]]
    );
    assert_eq!(store.query(&q(None, vec![])).unwrap().len(), 4);
    // and `get` finds them wherever they are
    assert!(store.get(nested).unwrap().is_some());
    assert!(store.get(parent_and_child[1]).unwrap().is_some());

    // a ref that cannot be a path is refused rather than escaping the root
    for bad in ["", "a//b", "../x", "a/../b", "a/./b", "/abs"] {
        let err = store.insert(make_item(bad, "x")).unwrap_err();
        assert!(matches!(err, StoreError::Validation(_)), "{bad:?}: {err:?}");
    }
    assert_eq!(store.query(&q(Some("../x"), vec![])).unwrap().len(), 0);
}

#[test]
fn reopen_sees_the_data_and_keeps_the_identity() {
    let root = TempRoot::new();
    let (id, op, store_id, clock) = {
        let store = DirStore::open(root.path()).unwrap();
        let id = store.insert(make_item("paper", "Persisted")).unwrap();
        let op = store
            .apply_operation(spec(id, OperationType::AddTag("t".into())))
            .unwrap();
        store.put_blob(id, "b", b"bytes").unwrap();
        (
            id,
            op,
            store.store_id().to_owned(),
            store.clock_file().unwrap(),
        )
    };
    let store = DirStore::open(root.path()).unwrap();
    assert_eq!(store.store_id(), store_id);
    assert_eq!(store.get(id).unwrap().unwrap().tags, vec!["t"]);
    assert!(store.get(op).unwrap().is_some());
    assert_eq!(
        store.get_blob(id, "b").unwrap().as_deref(),
        Some(&b"bytes"[..])
    );
    assert_eq!(store.clock_file().unwrap(), clock);
    // the clock carries on past what was written
    let next = store.insert(make_item("paper", "Next")).unwrap();
    assert!(store.get(next).unwrap().unwrap().logical_clock > clock);
}

#[test]
fn a_root_of_a_newer_format_is_refused() {
    let root = TempRoot::new();
    std::fs::create_dir_all(root.path()).unwrap();
    std::fs::write(
        root.path().join("koine-store.json"),
        r#"{"format": 2, "store_id": "x", "created": "2026-01-01T00:00:00Z"}"#,
    )
    .unwrap();
    let err = DirStore::open(root.path()).unwrap_err();
    assert!(err.to_string().contains("format 2"), "{err}");
}

// ── the clock ───────────────────────────────────────────────────────────────

#[test]
fn clock_file_advances_and_clocks_never_repeat() {
    let (root, store) = open();
    assert_eq!(
        store.clock_file().unwrap(),
        0,
        "a fresh store has not ticked"
    );
    let a = store.insert(make_item("test", "A")).unwrap();
    let c1 = store.clock_file().unwrap();
    assert_eq!(c1, store.get(a).unwrap().unwrap().logical_clock);
    assert!(c1 > 0);

    let mut seen = std::collections::HashSet::from([c1]);
    let mut last = c1;
    for i in 0..200 {
        let id = store.insert(make_item("test", &format!("{i}"))).unwrap();
        let clock = store.get(id).unwrap().unwrap().logical_clock;
        assert!(clock > last, "monotonic");
        assert!(seen.insert(clock), "never twice");
        last = clock;
    }
    assert_eq!(store.clock_file().unwrap(), last);
    let text = std::fs::read_to_string(root.path().join("clock")).unwrap();
    let (wall, counter) = text.trim().split_once(' ').unwrap();
    assert_eq!(
        (wall.parse::<u64>().unwrap() << 16) | counter.parse::<u64>().unwrap(),
        last
    );

    // a clock observed from a replica moves ours past it
    let remote = last + (1 << 20);
    store.observe_clock(remote).unwrap();
    let b = store.insert(make_item("test", "after")).unwrap();
    assert!(store.get(b).unwrap().unwrap().logical_clock > remote);
}

#[test]
fn a_clock_file_that_is_not_a_clock_is_an_error_not_zero() {
    let (root, store) = open();
    std::fs::write(root.path().join("clock"), "garbage").unwrap();
    assert!(store.clock_file().is_err());
    assert!(store.insert(make_item("test", "x")).is_err());
}

// ── the journal (D4.2) ──────────────────────────────────────────────────────

fn batch_of(n: usize) -> Vec<Item> {
    (0..n)
        .map(|i| make_item("paper", &format!("batch {i}")))
        .collect()
}

#[test]
fn a_batch_killed_after_the_journal_is_completed_by_open() {
    let root = TempRoot::new();
    let items = batch_of(4);
    {
        let store = DirStore::open(root.path()).unwrap();
        let mut tx = store.begin().unwrap();
        tx.preload_ids().unwrap();
        for item in items.clone() {
            tx.insert(item).unwrap();
        }
        let (plan, _events) = tx.plan().unwrap();
        plan.write(root.path()).unwrap();
        // killed here: the journal is on disk, no document is.
        drop(tx);
        assert_eq!(store.count(&ItemQuery::default()).unwrap(), 0);
        assert_eq!(
            std::fs::read_dir(root.path().join("journal"))
                .unwrap()
                .count(),
            1
        );
    }
    let store = DirStore::open(root.path()).unwrap();
    assert_eq!(store.count(&ItemQuery::default()).unwrap(), 4);
    for item in &items {
        assert!(store.get(item.id).unwrap().is_some());
    }
    assert_eq!(
        std::fs::read_dir(root.path().join("journal"))
            .unwrap()
            .count(),
        0,
        "the journal is removed once replayed"
    );
    // the clock covers what the replayed documents carry
    let top = store
        .query(&q(None, vec![]))
        .unwrap()
        .iter()
        .map(|i| i.logical_clock)
        .max()
        .unwrap();
    assert!(store.clock_file().unwrap() >= top);
}

#[test]
fn a_batch_killed_part_way_through_the_apply_is_completed_by_open() {
    let root = TempRoot::new();
    let items = batch_of(5);
    {
        let store = DirStore::open(root.path()).unwrap();
        let mut tx = store.begin().unwrap();
        tx.preload_ids().unwrap();
        for item in items.clone() {
            tx.insert(item).unwrap();
        }
        let (plan, _) = tx.plan().unwrap();
        plan.write(root.path()).unwrap();
        // two documents land, then the process dies
        Journal::apply_write(root.path(), &plan.writes[0]).unwrap();
        Journal::apply_write(root.path(), &plan.writes[1]).unwrap();
        drop(tx);
        assert_eq!(
            store.count(&ItemQuery::default()).unwrap(),
            2,
            "half applied"
        );
    }
    let store = DirStore::open(root.path()).unwrap();
    assert_eq!(store.count(&ItemQuery::default()).unwrap(), 5);
}

#[test]
fn replaying_twice_is_the_same_result() {
    let root = TempRoot::new();
    let store = DirStore::open(root.path()).unwrap();
    let mut tx = store.begin().unwrap();
    tx.insert(make_item("paper", "once")).unwrap();
    tx.insert(make_item("paper", "twice")).unwrap();
    let (plan, _) = tx.plan().unwrap();
    plan.write(root.path()).unwrap();
    plan.apply(root.path()).unwrap();
    plan.apply(root.path()).unwrap();
    drop(tx);
    store.replay_journals().unwrap();
    store.replay_journals().unwrap();
    assert_eq!(store.count(&ItemQuery::default()).unwrap(), 2);
}

#[test]
fn a_guarded_batch_killed_after_the_journal_is_completed_by_open() {
    let root = TempRoot::new();
    let (id, read) = {
        let store = DirStore::open(root.path()).unwrap();
        let id = store.insert(make_item("paper", "T")).unwrap();
        let read = store.logical_clock_of(id).unwrap().unwrap();
        let mut tx = store.begin().unwrap();
        let shared = Uuid::new_v4().to_string();
        for (f, v) in [("a", 1), ("b", 2)] {
            let mut s = spec(id, OperationType::SetPayload(f.into(), Value::Int(v)));
            s.batch_id = Some(shared.clone());
            tx.apply_op(&s).unwrap();
        }
        let (plan, _) = tx.plan().unwrap();
        assert_eq!(plan.writes.len(), 3, "the target and two operations");
        plan.write(root.path()).unwrap();
        drop(tx);
        let untouched = store.get(id).unwrap().unwrap();
        assert!(!untouched.payload.contains_key("a"), "nothing applied yet");
        (id, read)
    };
    let store = DirStore::open(root.path()).unwrap();
    let target = store.get(id).unwrap().unwrap();
    assert_eq!(target.payload.get("a"), Some(&Value::Int(1)));
    assert_eq!(target.payload.get("b"), Some(&Value::Int(2)));
    assert!(target.logical_clock > read);
    assert_eq!(store.count(&q(Some("core/operation"), vec![])).unwrap(), 2);
}

#[test]
fn the_next_writer_completes_a_journal_a_dead_one_left() {
    let root = TempRoot::new();
    let store = DirStore::open(root.path()).unwrap();
    let items = batch_of(3);
    let mut tx = store.begin().unwrap();
    for item in items.clone() {
        tx.insert(item).unwrap();
    }
    let (plan, _) = tx.plan().unwrap();
    plan.write(root.path()).unwrap();
    drop(tx);
    // no reopen: the next mutating call on any handle finishes it first
    store.insert(make_item("paper", "after")).unwrap();
    assert_eq!(store.count(&ItemQuery::default()).unwrap(), 4);
}

#[test]
fn a_journal_cannot_reach_outside_the_store() {
    let root = TempRoot::new();
    let store = DirStore::open(root.path()).unwrap();
    for bad in [
        "../escape.json",
        "items/../../escape.json",
        "/etc/x",
        "blobs/x",
        "items//x",
    ] {
        let journal = Journal {
            batch: Uuid::new_v4().to_string(),
            created: Utc::now(),
            ids: vec![],
            writes: vec![JournalWrite {
                path: bad.into(),
                doc: serde_json::json!({}),
            }],
            removes: vec![],
        };
        journal.write(root.path()).unwrap();
        assert!(store.replay_journals().is_err(), "{bad}");
        journal.finish(root.path()).unwrap();
    }
}

// ── sharing and the lock (D4.1) ─────────────────────────────────────────────

#[test]
fn two_handles_on_one_root_see_each_others_committed_writes() {
    let root = TempRoot::new();
    let a = DirStore::open(root.path()).unwrap();
    let b = DirStore::open(root.path()).unwrap();
    assert_eq!(a.store_id(), b.store_id());

    let ia = a.insert(make_item("paper", "from a")).unwrap();
    assert!(b.get(ia).unwrap().is_some());
    b.update(ia, vec![FieldMutation::SetStarred(true)]).unwrap();
    assert!(a.get(ia).unwrap().unwrap().is_starred);
    let ib = b.insert(make_item("paper", "from b")).unwrap();
    assert_eq!(a.count(&q(Some("paper"), vec![])).unwrap(), 2);
    assert!(matches!(
        a.insert(b.get(ib).unwrap().unwrap()).unwrap_err(),
        StoreError::AlreadyExists(_)
    ));

    // the clock file is the cross-process "something changed" probe
    let seen = a.clock_file().unwrap();
    b.insert(make_item("paper", "again")).unwrap();
    assert!(a.clock_file().unwrap() > seen);

    // and the two never hand out the same clock
    let mut clocks = std::collections::HashSet::new();
    for i in 0..50 {
        let writer = if i % 2 == 0 { &a } else { &b };
        let id = writer.insert(make_item("paper", &format!("{i}"))).unwrap();
        assert!(clocks.insert(a.get(id).unwrap().unwrap().logical_clock));
    }
}

#[test]
fn without_the_watcher_events_are_in_process_only() {
    let root = TempRoot::new();
    let a = DirStore::open(root.path()).unwrap();
    let b = DirStore::open(root.path()).unwrap();
    let rx = a.subscribe(ItemQuery::default()).unwrap();
    b.insert(make_item("paper", "elsewhere")).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        rx.try_recv().is_err(),
        "no watcher: cross_process_events is false"
    );
    assert!(!a.capabilities().cross_process_events);
    a.insert(make_item("paper", "here")).unwrap();
    assert!(rx.try_recv().is_ok());
}

#[test]
fn a_held_lock_blocks_other_lockers_and_writers() {
    let root = TempRoot::new();
    let a = DirStore::open(root.path()).unwrap();
    let b = Arc::new(DirStore::open(root.path()).unwrap());

    let guard = a.lock().unwrap();
    let err = FileLock::try_exclusive(root.path().join("lock")).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);

    let done = Arc::new(AtomicBool::new(false));
    let writer = {
        let (b, done) = (b.clone(), done.clone());
        std::thread::spawn(move || {
            let id = b.insert(make_item("paper", "blocked")).unwrap();
            done.store(true, Ordering::SeqCst);
            id
        })
    };
    std::thread::sleep(Duration::from_millis(250));
    assert!(
        !done.load(Ordering::SeqCst),
        "the writer waits for the lock"
    );
    // a reader does not
    assert_eq!(b.count(&ItemQuery::default()).unwrap(), 0);

    drop(guard);
    let id = writer.join().unwrap();
    assert!(done.load(Ordering::SeqCst));
    assert!(a.get(id).unwrap().is_some());
    FileLock::try_exclusive(root.path().join("lock")).unwrap();
}

#[test]
fn concurrent_writers_do_not_lose_updates() {
    let root = TempRoot::new();
    let store = Arc::new(DirStore::open(root.path()).unwrap());
    let id = store.insert(make_item("paper", "counter")).unwrap();
    let handles: Vec<_> = (0..4)
        .map(|t| {
            let root = root.path().to_path_buf();
            std::thread::spawn(move || {
                let mine = DirStore::open(root).unwrap();
                for i in 0..10 {
                    mine.update(
                        id,
                        vec![FieldMutation::SetPayload(
                            format!("k{t}-{i}"),
                            Value::Int(1),
                        )],
                    )
                    .unwrap();
                }
            })
        })
        .collect();
    handles.into_iter().for_each(|h| h.join().unwrap());
    assert_eq!(store.get(id).unwrap().unwrap().payload.len(), 1 + 40);
    assert_eq!(store.count(&q(Some("core/operation"), vec![])).unwrap(), 40);
}

// ── conflicts (D4.4) ────────────────────────────────────────────────────────

#[test]
fn a_conflict_copy_with_a_higher_clock_is_reported_and_left_alone() {
    let root = TempRoot::new();
    let (id, own, copy_path) = {
        let store = DirStore::open(root.path()).unwrap();
        let id = store.insert(make_item("paper", "mine")).unwrap();
        let (path, _) = store.locate(id).unwrap().unwrap();
        let own = store.logical_clock_of(id).unwrap().unwrap();
        // what a sync tool leaves beside a file it could not reconcile
        let mut doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        doc["logical_clock"] = (own + 1000).into();
        doc["payload"]["title"] = "theirs".into();
        let copy = path.with_file_name(format!("{id}.conflict-1.json"));
        std::fs::write(&copy, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
        // and one that is merely stale is not a conflict
        doc["logical_clock"] = 1.into();
        std::fs::write(
            path.with_file_name(format!("{id}.conflict-2.json")),
            serde_json::to_vec(&doc).unwrap(),
        )
        .unwrap();
        (id, own, copy)
    };
    let store = DirStore::open(root.path()).unwrap();
    let conflicts = store.conflicts();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].id, id);
    assert_eq!(conflicts[0].file_clock, Some(own));
    assert_eq!(conflicts[0].copy_clock, own + 1000);
    assert_eq!(conflicts[0].copy, copy_path);

    // never merged: the file keeps its value and the copy is not a record
    assert_eq!(
        store.get(id).unwrap().unwrap().payload.get("title"),
        Some(&Value::String("mine".into()))
    );
    assert_eq!(store.count(&ItemQuery::default()).unwrap(), 1);
    assert!(copy_path.is_file());

    // a store with none reports none, and a rescan sees what arrives later
    let (_r2, clean) = open();
    assert!(clean.conflicts().is_empty());
    std::fs::remove_file(&copy_path).unwrap();
    assert!(store.conflicts().len() == 1 && store.rescan_conflicts().unwrap().is_empty());
}

// ── blobs (D5) ──────────────────────────────────────────────────────────────

#[test]
fn blobs_round_trip_list_and_delete() {
    let (root, store) = open();
    let id = store.insert(make_item("paper", "P")).unwrap();
    assert!(store.list_blobs(id).unwrap().is_empty());
    assert!(store.get_blob(id, "paper.pdf").unwrap().is_none());

    store.put_blob(id, "paper.pdf", b"one").unwrap();
    store.put_blob(id, "figure.png", &[0, 1, 2, 255]).unwrap();
    assert_eq!(store.get_blob(id, "paper.pdf").unwrap().unwrap(), b"one");
    assert_eq!(
        store.get_blob(id, "figure.png").unwrap().unwrap(),
        vec![0, 1, 2, 255]
    );
    assert_eq!(
        store.list_blobs(id).unwrap(),
        vec!["figure.png", "paper.pdf"]
    );

    store.put_blob(id, "paper.pdf", b"replaced").unwrap();
    assert_eq!(
        store.get_blob(id, "paper.pdf").unwrap().unwrap(),
        b"replaced"
    );
    assert_eq!(
        files_under(&root.path().join("blobs")).len(),
        2,
        "no temporaries"
    );

    store.delete_blob(id, "paper.pdf").unwrap();
    store.delete_blob(id, "paper.pdf").unwrap();
    assert!(store.get_blob(id, "paper.pdf").unwrap().is_none());
    assert_eq!(store.list_blobs(id).unwrap(), vec!["figure.png"]);
    store.delete_blob(id, "figure.png").unwrap();
    assert!(store.list_blobs(id).unwrap().is_empty());

    // blobs are per item
    let other = store.insert(make_item("paper", "Q")).unwrap();
    store.put_blob(other, "x", b"y").unwrap();
    assert!(store.list_blobs(id).unwrap().is_empty());
    assert_eq!(store.list_blobs(other).unwrap(), vec!["x"]);

    // a blob belongs to an item: none for an id nobody inserted
    let ghost = Uuid::new_v4();
    assert!(matches!(
        store.put_blob(ghost, "x", b"y"),
        Err(StoreError::NotFound(g)) if g == ghost
    ));
    assert!(files_under(&root.path().join("blobs"))
        .iter()
        .all(|f| !f.contains(&ghost.to_string())));
}

#[test]
fn blob_names_are_validated() {
    let (root, store) = open();
    let id = store.insert(make_item("paper", "P")).unwrap();
    let temporary = format!(".x.tmp-{}", Uuid::new_v4().simple());
    for bad in [
        "", ".", "a/b", "../x", "..", "a\\b", "x\0y", "/abs", &temporary,
    ] {
        for result in [
            store.put_blob(id, bad, b"x"),
            store.delete_blob(id, bad),
            store.get_blob(id, bad).map(|_| ()),
        ] {
            assert!(
                matches!(result, Err(StoreError::Validation(_))),
                "{bad:?} must be refused"
            );
        }
    }
    assert!(files_under(root.path())
        .iter()
        .all(|f| !f.starts_with("blobs")));
    // dots inside a name, and a leading one, are fine and are listed; only a
    // half-written temporary is skipped
    store.put_blob(id, "v1.2.pdf", b"x").unwrap();
    store.put_blob(id, ".hidden", b"x").unwrap();
    store.put_blob(id, "a..b", b"x").unwrap();
    std::fs::write(
        root.path()
            .join("blobs")
            .join(id.to_string())
            .join(&temporary),
        b"half",
    )
    .unwrap();
    assert_eq!(
        store.list_blobs(id).unwrap(),
        vec![".hidden", "a..b", "v1.2.pdf"]
    );
}

// ── the watcher (F-D4) ──────────────────────────────────────────────────────

#[cfg(feature = "watch")]
mod watching {
    use super::*;

    const WAIT: Duration = Duration::from_secs(2);
    const QUIET: Duration = Duration::from_millis(300);

    /// Two handles on one root; the second watches.
    fn watched_pair() -> (TempRoot, DirStore, DirStore) {
        let root = TempRoot::new();
        let a = DirStore::open(root.path()).unwrap();
        let b = DirStore::open(root.path()).unwrap();
        b.watch().unwrap();
        assert!(b.wait_until_watching(Duration::from_secs(5)));
        (root, a, b)
    }

    /// Everything heard from the first event (which must come within [`WAIT`])
    /// until [`QUIET`] of silence.
    fn settle(rx: &Receiver<ItemEvent>) -> Vec<ItemEvent> {
        let mut seen = vec![rx
            .recv_timeout(WAIT)
            .unwrap_or_else(|_| panic!("no event within {WAIT:?}"))];
        while let Ok(event) = rx.recv_timeout(QUIET) {
            seen.push(event);
        }
        seen
    }

    fn silence(rx: &Receiver<ItemEvent>) {
        if let Ok(event) = rx.recv_timeout(QUIET) {
            panic!("expected silence, heard {event:?}");
        }
    }

    #[test]
    fn another_handles_writes_reach_the_watching_one() {
        let (_root, a, b) = watched_pair();
        assert!(b.capabilities().cross_process_events);
        assert!(!a.capabilities().cross_process_events);
        let rx = b.subscribe(ItemQuery::default()).unwrap();

        // insert, into a schema folder that does not exist yet
        let id = a.insert(make_item("paper", "first")).unwrap();
        match settle(&rx).as_slice() {
            [ItemEvent::Created(created)] => {
                assert_eq!(created.id, id);
                assert_eq!(
                    created.payload.get("title"),
                    Some(&Value::String("first".into()))
                );
            }
            other => panic!("{other:?}"),
        }

        // update: a payload change and a tag
        a.update(
            id,
            vec![
                FieldMutation::SetPayload("title".into(), Value::String("second".into())),
                FieldMutation::AddTag("t".into()),
            ],
        )
        .unwrap();
        let events = settle(&rx);
        let ops: Vec<ItemId> = a
            .operations_for(id, None)
            .unwrap()
            .iter()
            .map(|o| o.id)
            .collect();
        assert_eq!(ops.len(), 2);
        let mut heard_ops = Vec::new();
        let mut updates = Vec::new();
        for event in events {
            match event {
                ItemEvent::OperationApplied {
                    operation_id,
                    target_id,
                } => {
                    assert_eq!(target_id, id);
                    heard_ops.push(operation_id);
                }
                ItemEvent::Updated { id: got, mutations } => updates.push((got, mutations)),
                other => panic!("{other:?}"),
            }
        }
        heard_ops.sort();
        let mut ops_sorted = ops.clone();
        ops_sorted.sort();
        assert_eq!(heard_ops, ops_sorted, "both operations are announced");
        assert_eq!(
            updates,
            vec![(
                id,
                vec![
                    FieldMutation::SetPayload("title".into(), Value::String("second".into())),
                    FieldMutation::AddTag("t".into()),
                ]
            )]
        );

        // an operation applied directly
        let op = a
            .apply_operation(spec(id, OperationType::SetStarred(true)))
            .unwrap();
        let events = settle(&rx);
        assert!(events.contains(&ItemEvent::OperationApplied {
            operation_id: op,
            target_id: id
        }));
        assert!(events.contains(&ItemEvent::Updated {
            id,
            mutations: vec![FieldMutation::SetStarred(true)]
        }));

        // a delete (the item's operations go quietly)
        a.delete(id).unwrap();
        assert_eq!(settle(&rx), vec![ItemEvent::Deleted(id)]);
    }

    #[test]
    fn the_schema_prefix_filters_what_the_watcher_announces() {
        let (_root, a, b) = watched_pair();
        let papers = b.subscribe(q(Some("paper"), vec![])).unwrap();
        let notes = b.subscribe(q(Some("note"), vec![])).unwrap();
        let id = a.insert(make_item("note", "n")).unwrap();
        assert!(matches!(settle(&notes).as_slice(), [ItemEvent::Created(c)] if c.id == id));
        silence(&papers);
    }

    #[test]
    fn what_existed_before_the_watcher_is_not_announced_but_changes_to_it_are() {
        let root = TempRoot::new();
        let a = DirStore::open(root.path()).unwrap();
        let id = a.insert(make_item("paper", "old")).unwrap();
        let b = DirStore::open(root.path()).unwrap();
        let rx = b.subscribe(ItemQuery::default()).unwrap();
        b.watch().unwrap();
        b.watch().unwrap(); // idempotent
        assert!(b.wait_until_watching(Duration::from_secs(5)));
        silence(&rx);

        a.update(id, vec![FieldMutation::SetRead(true)]).unwrap();
        let events = settle(&rx);
        assert!(events.contains(&ItemEvent::Updated {
            id,
            mutations: vec![FieldMutation::SetRead(true)]
        }));
        assert!(!events.iter().any(|e| matches!(e, ItemEvent::Created(_))));
    }

    #[test]
    fn a_watching_handle_does_not_echo_its_own_writes() {
        let root = TempRoot::new();
        let a = DirStore::open(root.path()).unwrap();
        a.watch().unwrap();
        assert!(a.wait_until_watching(Duration::from_secs(5)));
        let rx = a.subscribe(ItemQuery::default()).unwrap();

        let id = a.insert(make_item("paper", "mine")).unwrap();
        assert!(matches!(rx.try_recv(), Ok(ItemEvent::Created(c)) if c.id == id));
        silence(&rx);

        a.update(id, vec![FieldMutation::AddTag("t".into())])
            .unwrap();
        assert!(matches!(
            rx.try_recv(),
            Ok(ItemEvent::OperationApplied { target_id, .. }) if target_id == id
        ));
        silence(&rx);

        a.insert_batch(vec![make_item("paper", "x"), make_item("note", "y")])
            .unwrap();
        assert_eq!(drain_now(&rx).len(), 2);
        silence(&rx);

        a.delete(id).unwrap();
        assert_eq!(rx.try_recv().unwrap(), ItemEvent::Deleted(id));
        silence(&rx);

        // and a later write by someone else is still heard exactly once
        let other = DirStore::open(root.path()).unwrap();
        let theirs = other.insert(make_item("paper", "theirs")).unwrap();
        assert!(matches!(settle(&rx).as_slice(), [ItemEvent::Created(c)] if c.id == theirs));
    }

    fn drain_now(rx: &Receiver<ItemEvent>) -> Vec<ItemEvent> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    #[test]
    fn dropping_the_store_ends_the_watcher() {
        let (_root, _a, b) = watched_pair();
        let watching = b.watching.clone();
        assert!(watching.load(Ordering::SeqCst));
        let started = std::time::Instant::now();
        drop(b);
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "drop does not wait"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while watching.load(Ordering::SeqCst) {
            assert!(std::time::Instant::now() < deadline, "the thread ended");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_rescan_reconciles_the_cache_with_the_disk() {
        let (root, a, _b) = watched_pair();
        let kept = a.insert(make_item("paper", "kept")).unwrap();
        let gone = a.insert(make_item("paper", "gone")).unwrap();
        let rx = a.subscribe(ItemQuery::default()).unwrap();
        // a worker with no OS events at all: only a reconcile can tell
        let subscribers = a.subscribers.clone();
        let mut worker =
            crate::watch::testing::worker(root.path(), subscribers, Arc::new(OwnWrites::default()));
        let other = DirStore::open(root.path()).unwrap();
        let fresh = other.insert(make_item("note", "fresh")).unwrap();
        other.delete(gone).unwrap();
        other
            .update(kept, vec![FieldMutation::AddTag("x".into())])
            .unwrap();
        drain_now(&rx);
        worker.reconcile();
        let events = drain_now(&rx);
        assert!(events.contains(&ItemEvent::Deleted(gone)), "{events:?}");
        assert!(events
            .iter()
            .any(|e| matches!(e, ItemEvent::Created(c) if c.id == fresh)));
        assert!(events.contains(&ItemEvent::Updated {
            id: kept,
            mutations: vec![FieldMutation::AddTag("x".into())]
        }));
        // and a second reconcile has nothing to say
        worker.reconcile();
        assert!(drain_now(&rx).is_empty());
    }
}

// ── merge policies (F-D5) ───────────────────────────────────────────────────

mod merging {
    use super::*;

    /// A root with one item and conflict copies beside it, written by hand as a
    /// sync tool would. Returns the root's guard, the item's id and its file
    /// clock; `edit` shapes the copy (its clock is the file's plus 1000).
    struct Fixture {
        root: TempRoot,
        id: ItemId,
        file_clock: u64,
        copy: PathBuf,
        stale: PathBuf,
        other_item: ItemId,
    }

    fn fixture(edit: impl FnOnce(&mut serde_json::Value)) -> Fixture {
        let root = TempRoot::new();
        let store = DirStore::open(root.path()).unwrap();
        let other_item = store.insert(make_item("paper", "cited")).unwrap();
        let mut item = make_item("paper", "mine");
        item.tags = vec!["a".into()];
        item.payload.insert("only_file".into(), Value::Int(1));
        item.payload
            .insert("both".into(), Value::String("file".into()));
        let id = store.insert(item).unwrap();
        let (path, _) = store.locate(id).unwrap().unwrap();
        let file_clock = store.logical_clock_of(id).unwrap().unwrap();
        let mut doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        doc["logical_clock"] = (file_clock + 1000).into();
        doc["payload"]["title"] = "theirs".into();
        doc["payload"]["both"] = "copy".into();
        doc["payload"]["only_copy"] = 2.into();
        doc["tags"] = serde_json::json!(["b"]);
        doc["is_starred"] = true.into();
        edit(&mut doc);
        let copy = path.with_file_name(format!("{id}.conflict-1.json"));
        std::fs::write(&copy, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
        doc["logical_clock"] = 1.into();
        let stale = path.with_file_name(format!("{id}.conflict-2.json"));
        std::fs::write(&stale, serde_json::to_vec(&doc).unwrap()).unwrap();
        Fixture {
            root,
            id,
            file_clock,
            copy,
            stale,
            other_item,
        }
    }

    fn title(store: &DirStore, id: ItemId) -> Value {
        store
            .get(id)
            .unwrap()
            .unwrap()
            .payload
            .get("title")
            .unwrap()
            .clone()
    }

    fn merge_operation(store: &DirStore, id: ItemId) -> Item {
        let ops = store.operations_for(id, None).unwrap();
        ops.into_iter()
            .find(|o| {
                o.payload.get("op_type") == Some(&Value::String("custom:merge-conflict".into()))
            })
            .expect("a merge-conflict operation")
    }

    /// Open under `Report` (the conflict is reported), subscribe, then set the
    /// policy: `with_merge_policy` resolves what is pending.
    fn resolve_with(f: &Fixture, policy: MergePolicy) -> (DirStore, Receiver<ItemEvent>) {
        let store = DirStore::open(f.root.path()).unwrap();
        assert_eq!(store.conflicts().len(), 1, "reported first");
        let rx = store.subscribe(ItemQuery::default()).unwrap();
        (store.with_merge_policy(policy), rx)
    }

    #[test]
    fn union_merges_tags_payload_references_and_takes_the_rest_from_the_higher_clock() {
        let mut file = make_item("paper", "f");
        file.logical_clock = 10;
        file.tags = vec!["a".into(), "both".into()];
        file.payload
            .insert("title".into(), Value::String("file".into()));
        file.payload.insert("only_file".into(), Value::Int(1));
        file.payload.insert("disputed".into(), Value::Int(1));
        file.references = vec![cites(Uuid::from_u128(1))];
        file.parent = Some(Uuid::from_u128(7));
        file.is_read = true;
        let mut copy = file.clone();
        copy.logical_clock = 20;
        copy.tags = vec!["both".into(), "b".into()];
        copy.payload.insert("disputed".into(), Value::Int(2));
        copy.payload.insert("only_copy".into(), Value::Int(3));
        copy.payload.remove("only_file");
        copy.references = vec![cites(Uuid::from_u128(2))];
        copy.parent = Some(Uuid::from_u128(8));
        copy.is_read = false;
        copy.is_starred = true;
        copy.modified = file.modified + chrono::Duration::seconds(5);

        let merged = union_merge(&file, &copy);
        assert_eq!(merged.tags, vec!["a", "b", "both"]);
        assert_eq!(
            merged.payload.get("disputed"),
            Some(&Value::Int(2)),
            "the copy is higher"
        );
        assert_eq!(merged.payload.get("only_file"), Some(&Value::Int(1)));
        assert_eq!(merged.payload.get("only_copy"), Some(&Value::Int(3)));
        assert_eq!(merged.parent, Some(Uuid::from_u128(8)));
        assert!(!merged.is_read && merged.is_starred);
        let targets: Vec<_> = merged.references.iter().map(|r| r.target).collect();
        assert_eq!(targets, vec![Uuid::from_u128(1), Uuid::from_u128(2)]);
        assert_eq!(merged.modified, copy.modified);
        assert_eq!(merged.logical_clock, 20);

        // the other way round, the file is the higher side
        let mut file_higher = file.clone();
        file_higher.logical_clock = 30;
        let merged = union_merge(&file_higher, &copy);
        assert_eq!(merged.payload.get("disputed"), Some(&Value::Int(1)));
        assert_eq!(merged.parent, Some(Uuid::from_u128(7)));
        assert_eq!(merged.logical_clock, 30);
    }

    #[test]
    fn report_leaves_both_files_and_a_stale_copy_alone() {
        let f = fixture(|_| {});
        let store = DirStore::open(f.root.path()).unwrap();
        assert_eq!(store.merge_policy().name(), "report");
        assert!(store.resolve_conflicts().unwrap().is_empty());
        assert_eq!(store.conflicts().len(), 1);
        assert!(f.copy.is_file() && f.stale.is_file());
        assert_eq!(title(&store, f.id), Value::String("mine".into()));
        assert!(!f.root.path().join("conflicts").exists());
    }

    #[test]
    fn take_copy_makes_the_copy_the_file_and_files_the_copy_and_the_stale_one() {
        let f = fixture(|_| {});
        let (store, rx) = resolve_with(&f, MergePolicy::TakeCopy);

        let item = store.get(f.id).unwrap().unwrap();
        assert_eq!(
            item.payload.get("title"),
            Some(&Value::String("theirs".into()))
        );
        assert_eq!(item.tags, vec!["b"]);
        assert!(item.is_starred);
        assert!(
            item.logical_clock > f.file_clock + 1000,
            "a fresh clock after both"
        );
        assert!(store.conflicts().is_empty());

        // both copies are under conflicts/, by their own names
        let filed = f.root.path().join("conflicts");
        assert!(filed.join(format!("{}.conflict-1.json", f.id)).is_file());
        assert!(filed.join(format!("{}.conflict-2.json", f.id)).is_file());
        assert!(!f.copy.exists() && !f.stale.exists());

        // the operation is on the item, with its custom type and data
        let op = merge_operation(&store, f.id);
        match op.payload.get("op_data") {
            Some(Value::Object(data)) => {
                assert_eq!(data.get("policy"), Some(&Value::String("take-copy".into())));
                assert_eq!(
                    data.get("file_clock"),
                    Some(&Value::Int(f.file_clock as i64))
                );
                assert_eq!(
                    data.get("copy_clock"),
                    Some(&Value::Int(f.file_clock as i64 + 1000))
                );
                assert_eq!(
                    data.get("copy"),
                    Some(&Value::String(format!(
                        "items/paper/{}.conflict-1.json",
                        f.id
                    )))
                );
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(op.author, "system:local");
        assert_eq!(
            store
                .operations_for(f.id, None)
                .unwrap()
                .iter()
                .filter(|o| o.id == op.id)
                .count(),
            1
        );

        // exactly one event: the mutations that took the file to the winner
        match drain_events(&rx).as_slice() {
            [ItemEvent::Updated { id, mutations }] => {
                assert_eq!(*id, f.id);
                assert!(mutations.contains(&FieldMutation::SetPayload(
                    "title".into(),
                    Value::String("theirs".into())
                )));
                assert!(mutations.contains(&FieldMutation::AddTag("b".into())));
                assert!(mutations.contains(&FieldMutation::RemoveTag("a".into())));
                assert!(mutations.contains(&FieldMutation::SetStarred(true)));
                assert!(mutations.contains(&FieldMutation::SetPayload(
                    "only_copy".into(),
                    Value::Int(2)
                )));
            }
            other => panic!("{other:?}"),
        }

        // a reopen sees the merged item and a clock above both
        let clock = item.logical_clock;
        drop(store);
        let again = DirStore::open(f.root.path()).unwrap();
        assert_eq!(again.logical_clock_of(f.id).unwrap(), Some(clock));
        assert!(again.conflicts().is_empty());
        let fresh = again.insert(make_item("paper", "after")).unwrap();
        assert!(again.logical_clock_of(fresh).unwrap().unwrap() > clock);
    }

    fn drain_events(rx: &Receiver<ItemEvent>) -> Vec<ItemEvent> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    #[test]
    fn keep_file_keeps_the_content_and_files_the_copies() {
        let f = fixture(|_| {});
        let (store, rx) = resolve_with(&f, MergePolicy::KeepFile);
        let item = store.get(f.id).unwrap().unwrap();
        assert_eq!(
            item.payload.get("title"),
            Some(&Value::String("mine".into()))
        );
        assert_eq!(item.tags, vec!["a"]);
        assert!(
            item.logical_clock > f.file_clock + 1000,
            "behind-the-copy no more"
        );
        assert!(store.conflicts().is_empty());
        assert!(f
            .root
            .path()
            .join("conflicts")
            .join(format!("{}.conflict-1.json", f.id))
            .is_file());
        assert!(merge_operation(&store, f.id)
            .payload
            .contains_key("op_data"));
        assert!(matches!(
            drain_events(&rx).as_slice(),
            [ItemEvent::Updated { mutations, .. }] if mutations.is_empty()
        ));
    }

    #[test]
    fn union_resolves_a_synced_root() {
        let f = fixture(|doc| {
            doc["references"] = serde_json::json!([]);
        });
        let (store, _rx) = resolve_with(&f, MergePolicy::Union);
        let item = store.get(f.id).unwrap().unwrap();
        assert_eq!(item.tags, vec!["a", "b"]);
        // the copy is the higher clock: it wins the disputed keys
        assert_eq!(
            item.payload.get("title"),
            Some(&Value::String("theirs".into()))
        );
        assert_eq!(
            item.payload.get("both"),
            Some(&Value::String("copy".into()))
        );
        assert_eq!(item.payload.get("only_file"), Some(&Value::Int(1)));
        assert_eq!(item.payload.get("only_copy"), Some(&Value::Int(2)));
        assert!(item.is_starred);
        assert!(store.conflicts().is_empty());
        assert!(merge_operation(&store, f.id)
            .payload
            .contains_key("op_data"));
        let _ = f.other_item;
    }

    #[test]
    fn a_custom_merge_decides() {
        struct Shout;
        fn text(v: &Value) -> String {
            match v {
                Value::String(s) => s.clone(),
                other => format!("{other:?}"),
            }
        }
        impl Merge for Shout {
            fn merge(&self, file: &Item, copy: &Item) -> Item {
                let mut out = file.clone();
                out.payload.insert(
                    "title".into(),
                    Value::String(format!(
                        "{}+{}",
                        text(&file.payload["title"]),
                        text(&copy.payload["title"])
                    )),
                );
                // the store keeps the id and the file's schema whatever this says
                out.id = Uuid::new_v4();
                out
            }
        }
        let f = fixture(|_| {});
        let (store, _rx) = resolve_with(&f, MergePolicy::Custom(Arc::new(Shout)));
        assert_eq!(title(&store, f.id), Value::String("mine+theirs".into()));
        assert_eq!(store.count(&q(Some("paper"), vec![])).unwrap(), 2);
        assert_eq!(format!("{:?}", store.merge_policy()), "Custom(..)");
    }

    #[test]
    fn open_with_a_policy_resolves_at_open_and_resolve_conflicts_lists_them() {
        let f = fixture(|_| {});
        let store = DirStore::open_with(f.root.path(), MergePolicy::TakeCopy).unwrap();
        assert!(store.conflicts().is_empty());
        assert_eq!(title(&store, f.id), Value::String("theirs".into()));

        // a second conflict, found by an explicit call
        let (path, _) = store.locate(f.id).unwrap().unwrap();
        let mut doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let file_clock = doc["logical_clock"].as_u64().unwrap();
        doc["logical_clock"] = (file_clock + 5).into();
        doc["payload"]["title"] = "later".into();
        let copy = path.with_file_name(format!("{}.conflict-3.json", f.id));
        std::fs::write(&copy, serde_json::to_vec(&doc).unwrap()).unwrap();
        assert_eq!(store.rescan_conflicts().unwrap().len(), 1);
        let resolved = store.resolve_conflicts().unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].id, f.id);
        assert_eq!(resolved[0].policy, "take-copy");
        assert_eq!(resolved[0].file_clock, Some(file_clock));
        assert_eq!(resolved[0].copy_clock, file_clock + 5);
        assert_eq!(
            resolved[0].moved_copy,
            f.root
                .path()
                .join("conflicts")
                .join(format!("{}.conflict-3.json", f.id))
        );
        assert!(resolved[0].moved_copy.is_file());
        assert_eq!(title(&store, f.id), Value::String("later".into()));
        assert!(store.conflicts().is_empty());
        assert_eq!(
            store
                .operations_for(f.id, None)
                .unwrap()
                .iter()
                .filter(|o| o.payload.get("op_type")
                    == Some(&Value::String("custom:merge-conflict".into())))
                .count(),
            2
        );
    }

    #[test]
    fn a_missing_file_takes_the_copy_unless_the_policy_is_keep_file() {
        for (policy, kept) in [
            (MergePolicy::TakeCopy, true),
            (MergePolicy::Union, true),
            (MergePolicy::KeepFile, false),
        ] {
            let root = TempRoot::new();
            let id = {
                let store = DirStore::open(root.path()).unwrap();
                let id = store.insert(make_item("paper", "lost")).unwrap();
                let (path, _) = store.locate(id).unwrap().unwrap();
                let copy = path.with_file_name(format!("{id}.conflict-1.json"));
                std::fs::rename(&path, &copy).unwrap();
                id
            };
            let store = DirStore::open(root.path()).unwrap();
            assert_eq!(store.conflicts().len(), 1);
            assert_eq!(store.conflicts()[0].file_clock, None);
            let rx = store.subscribe(ItemQuery::default()).unwrap();
            let store = store.with_merge_policy(policy);
            assert_eq!(store.get(id).unwrap().is_some(), kept);
            assert!(store.conflicts().is_empty());
            let filed = root
                .path()
                .join("conflicts")
                .join(format!("{id}.conflict-1.json"));
            assert!(filed.is_file(), "the copy is always filed");
            if kept {
                assert!(matches!(rx.try_recv(), Ok(ItemEvent::Created(c)) if c.id == id));
                // no operation: there was no file to record it on... the item
                // has none until something is done to it
                assert!(store.operations_for(id, None).unwrap().is_empty());
            } else {
                assert!(rx.try_recv().is_err());
            }
        }
    }

    #[test]
    fn a_stale_copy_is_filed_under_take_copy() {
        let root = TempRoot::new();
        let (id, stale) = {
            let store = DirStore::open(root.path()).unwrap();
            let id = store.insert(make_item("paper", "mine")).unwrap();
            let (path, _) = store.locate(id).unwrap().unwrap();
            let mut doc: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            doc["logical_clock"] = 1.into();
            let stale = path.with_file_name(format!("{id}.conflict-1.json"));
            std::fs::write(&stale, serde_json::to_vec(&doc).unwrap()).unwrap();
            (id, stale)
        };
        // Report: untouched
        let report = DirStore::open(root.path()).unwrap();
        assert!(report.conflicts().is_empty() && stale.is_file());
        drop(report);
        // TakeCopy: filed, no operation, no event
        let store = DirStore::open(root.path()).unwrap();
        let rx = store.subscribe(ItemQuery::default()).unwrap();
        let store = store.with_merge_policy(MergePolicy::TakeCopy);
        assert!(!stale.exists());
        assert!(root
            .path()
            .join("conflicts")
            .join(format!("{id}.conflict-1.json"))
            .is_file());
        assert!(store.operations_for(id, None).unwrap().is_empty());
        assert!(rx.try_recv().is_err());
        assert_eq!(title(&store, id), Value::String("mine".into()));
    }

    #[test]
    fn a_conflict_copy_of_an_operation_is_left_reported() {
        let root = TempRoot::new();
        let op = {
            let store = DirStore::open(root.path()).unwrap();
            let id = store.insert(make_item("paper", "p")).unwrap();
            let op = store
                .apply_operation(spec(id, OperationType::SetRead(true)))
                .unwrap();
            let path = root.path().join(op_rel(op));
            let mut doc: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            doc["logical_clock"] = u64::MAX.into();
            std::fs::write(
                path.with_file_name(format!("{op}.conflict-1.json")),
                serde_json::to_vec(&doc).unwrap(),
            )
            .unwrap();
            op
        };
        let store = DirStore::open_with(root.path(), MergePolicy::Union).unwrap();
        assert_eq!(store.conflicts().len(), 1);
        assert_eq!(store.conflicts()[0].id, op);
    }
}
