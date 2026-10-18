//! Builders and readers the cases share.
//!
//! Public so a backend's own tests can build the same rows the suite does.

use std::collections::BTreeSet;
use std::sync::mpsc::Receiver;

use chrono::{DateTime, TimeZone, Utc};
use impress_item::schema::refs::CORE_OPERATION;
use impress_item::{
    ActorKind, EdgeType, Item, ItemEvent, ItemId, ItemQuery, ItemStore, OperationIntent,
    OperationSpec, OperationType, Predicate, Priority, RetentionTier, SchemaRef, SortDescriptor,
    TypedReference, Value, Visibility,
};

/// An arbitrary schema name. Opaque names are legitimate rows (a newer writer,
/// an old backup), and the suite's kinds are not the manifest's.
pub fn schema(name: &str) -> SchemaRef {
    SchemaRef::from_stored(name.to_owned())
}

/// A day in 2020, to millisecond precision (the precision the oracle stores).
/// Fixtures use old dates so that "this write moved `modified`" is observable.
pub fn day(d: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2020, 1, d, 12, 0, 0)
        .single()
        .expect("a valid date")
}

/// A string payload value.
pub fn text(s: &str) -> Value {
    Value::String(s.to_owned())
}

/// A fresh item of `schema` titled `title`: dated 2020-01-01, clock zero (so
/// the store stamps it), authored by a human, otherwise empty.
pub fn item(schema_name: &str, title: &str) -> Item {
    let mut payload = std::collections::BTreeMap::new();
    payload.insert("title".into(), text(title));
    payload.insert("author_text".into(), text("Test Author"));
    Item {
        id: uuid::Uuid::new_v4(),
        schema: schema(schema_name),
        payload,
        created: day(1),
        modified: day(1),
        author: "test@example.com".into(),
        author_kind: ActorKind::Human,
        logical_clock: 0,
        origin: None,
        canonical_id: None,
        tags: Vec::new(),
        flag: None,
        is_read: false,
        is_starred: false,
        priority: Priority::Normal,
        visibility: Visibility::Private,
        message_type: None,
        produced_by: None,
        version: None,
        batch_id: None,
        references: Vec::new(),
        parent: None,
        extra: Default::default(),
    }
}

/// Insert a fresh item and return its id.
pub fn insert(store: &dyn ItemStore, schema_name: &str, title: &str) -> ItemId {
    store
        .insert(item(schema_name, title))
        .expect("insert a fresh item")
}

/// An operation by `test-user`, routine, durable, no reason, no batch.
pub fn spec(target: ItemId, op: OperationType) -> OperationSpec {
    OperationSpec {
        target_id: target,
        op_type: op,
        intent: OperationIntent::Routine,
        reason: None,
        batch_id: None,
        author: "test-user".into(),
        author_kind: ActorKind::Human,
        retention: RetentionTier::Durable,
    }
}

/// A reference without metadata.
pub fn reference(target: ItemId, edge: EdgeType) -> TypedReference {
    TypedReference {
        target,
        edge_type: edge,
        metadata: None,
    }
}

/// Every row of `schema`, no predicates.
pub fn all_of(schema_name: &str) -> ItemQuery {
    ItemQuery {
        schema: Some(schema(schema_name)),
        ..Default::default()
    }
}

/// Rows of `schema` satisfying `predicates`.
pub fn where_(schema_name: &str, predicates: Vec<Predicate>) -> ItemQuery {
    ItemQuery {
        predicates,
        ..all_of(schema_name)
    }
}

/// Rows of `schema`, sorted.
pub fn sorted(schema_name: &str, sort: &[(&str, bool)]) -> ItemQuery {
    ItemQuery {
        sort: sort
            .iter()
            .map(|(field, ascending)| SortDescriptor {
                field: (*field).to_owned(),
                ascending: *ascending,
            })
            .collect(),
        ..all_of(schema_name)
    }
}

/// The titles of `items`, in order.
pub fn titles(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .map(|i| match i.payload.get("title") {
            Some(Value::String(s)) => s.clone(),
            other => panic!("item {} has no string title: {other:?}", i.id),
        })
        .collect()
}

/// The titles of `items`, as a set (order is not under test).
pub fn title_set(items: &[Item]) -> BTreeSet<String> {
    titles(items).into_iter().collect()
}

/// A set of titles, from literals.
pub fn set(titles: &[&str]) -> BTreeSet<String> {
    titles.iter().map(|t| (*t).to_owned()).collect()
}

/// The ids of `items`, as a set.
pub fn id_set(items: &[Item]) -> BTreeSet<ItemId> {
    items.iter().map(|i| i.id).collect()
}

/// The operations applied to `target`, oldest first. Operation rows are items
/// of schema `core/operation` whose `op_target_id` is the item they changed;
/// this is how the port exposes the history.
pub fn operations_of(store: &dyn ItemStore, target: ItemId) -> Vec<Item> {
    let q = ItemQuery {
        schema: Some(CORE_OPERATION),
        predicates: vec![Predicate::Eq(
            "op_target_id".into(),
            Value::String(target.to_string()),
        )],
        sort: vec![SortDescriptor {
            field: "logical_clock".into(),
            ascending: true,
        }],
        ..Default::default()
    };
    store.query(&q).expect("query operations")
}

/// The `op_type` string of an operation row.
pub fn op_type_of(op: &Item) -> String {
    match op.payload.get("op_type") {
        Some(Value::String(s)) => s.clone(),
        other => panic!("operation {} has no op_type: {other:?}", op.id),
    }
}

/// Everything waiting on a subscription, in order.
pub fn drain(rx: &Receiver<ItemEvent>) -> Vec<ItemEvent> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

/// Wall-clock milliseconds since the epoch.
pub fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_millis() as u64
}

/// Announce that a case did not run, and why. A skipped case says so; it never
/// passes silently. (Visible with `cargo test -- --nocapture`.)
pub fn skip(case: &str, why: &str) {
    eprintln!("SKIPPED {case}: {why}");
}
