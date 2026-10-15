use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::mpsc::Receiver;

use crate::event::ItemEvent;
use crate::item::{ActorKind, FlagState, Item, ItemId, Priority, Value, Visibility};
use crate::operation::OperationSpec;
use crate::query::{ItemQuery, Predicate, SortDescriptor};
use crate::reference::{EdgeType, TypedReference};
use crate::schema::{refs::CORE_OPERATION, SchemaRef};

/// The schema of the rows [`AttributedStore::metadata_get`] and
/// [`AttributedStore::metadata_set`] keep by default: one item per key,
/// payload `{ "key": <key>, "value": <value> }`, authored by `system:local`.
///
/// SQLite keeps the same pairs in its `store_metadata` table instead and
/// overrides the two methods, so this spelling is a default's, not a wire
/// format.
pub const STORE_METADATA_SCHEMA: &str = "core/store-metadata";

/// The author of the rows the metadata defaults write.
const METADATA_AUTHOR: &str = "system:local";

/// Mutation to apply to an item's fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FieldMutation {
    SetPayload(String, Value),
    RemovePayload(String),
    SetRead(bool),
    SetStarred(bool),
    SetFlag(Option<FlagState>),
    AddTag(String),
    RemoveTag(String),
    AddReference(TypedReference),
    RemoveReference(ItemId, EdgeType),
    SetParent(Option<ItemId>),
}

/// What a backend can and cannot do, as it declares at open
/// (`docs/plan-koine-store.md` D1). A host reads this and refuses or
/// degrades explicitly: nothing silently slows or silently forgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreCapabilities {
    /// Predicates and sorts are served by an index; a query's cost does not
    /// grow with the number of items of other kinds.
    pub indexed_query: bool,
    /// `insert_batch` and `apply_operations_if_clock` are all-or-nothing
    /// even if the process dies mid-way.
    pub atomic_batch: bool,
    /// `subscribe` delivers events for writes made by this process.
    pub subscribe: bool,
    /// `subscribe` also delivers events for writes made by other processes.
    pub cross_process_events: bool,
    /// Records survive the process.
    pub durable: bool,
    /// Two processes may open the same store at once and each sees the
    /// other's committed writes.
    pub shared: bool,
    /// The store holds blobs beside items ([`BlobStore`]).
    pub blobs: bool,
}

/// Bytes kept beside an item: a PDF next to its record, a figure next to
/// its plot (`docs/plan-koine-store.md` D5). A backend whose
/// [`StoreCapabilities::blobs`] is false does not implement this.
pub trait BlobStore: Send + Sync {
    /// Store `bytes` as `name` under `item`, replacing a blob of that name.
    fn put_blob(&self, item: ItemId, name: &str, bytes: &[u8]) -> Result<(), StoreError>;
    /// The blob, or `None` when `item` has no blob of that name.
    fn get_blob(&self, item: ItemId, name: &str) -> Result<Option<Vec<u8>>, StoreError>;
    /// Remove one blob. A blob that is already gone is not an error.
    fn delete_blob(&self, item: ItemId, name: &str) -> Result<(), StoreError>;
    /// The names of `item`'s blobs, sorted.
    fn list_blobs(&self, item: ItemId) -> Result<Vec<String>, StoreError>;
}

/// The trait that all storage backends implement.
pub trait ItemStore: Send + Sync {
    /// What this backend can do (`docs/plan-koine-store.md` D1).
    fn capabilities(&self) -> StoreCapabilities;

    /// Insert a new item. Returns the item's ID.
    fn insert(&self, item: Item) -> Result<ItemId, StoreError>;

    /// Insert multiple items atomically.
    fn insert_batch(&self, items: Vec<Item>) -> Result<Vec<ItemId>, StoreError>;

    /// Get an item by ID.
    fn get(&self, id: ItemId) -> Result<Option<Item>, StoreError>;

    /// Apply mutations to an existing item.
    fn update(&self, id: ItemId, mutations: Vec<FieldMutation>) -> Result<(), StoreError>;

    /// Delete an item by ID.
    fn delete(&self, id: ItemId) -> Result<(), StoreError>;

    /// Query items matching predicates, sorted and paginated.
    fn query(&self, q: &ItemQuery) -> Result<Vec<Item>, StoreError>;

    /// Count items matching a query without fetching them.
    fn count(&self, q: &ItemQuery) -> Result<usize, StoreError>;

    /// Get all items reachable from the given item via the specified edge types,
    /// up to the given depth. An empty edge filter traverses all edge types.
    fn neighbors(
        &self,
        id: ItemId,
        edge_types: &[EdgeType],
        depth: u32,
    ) -> Result<Vec<Item>, StoreError>;

    /// Subscribe to changes matching a query. Returns a channel of events.
    fn subscribe(&self, q: ItemQuery) -> Result<Receiver<ItemEvent>, StoreError>;

    /// A value that changes whenever a write committed through any handle
    /// this backend can see: SQLite's `PRAGMA data_version` (plus its own
    /// writes), the directory store's `clock` file, the memory store's clock.
    ///
    /// A reader that remembers the last value knows whether to look again
    /// without re-querying anything; a host polls it to learn that another
    /// process wrote. Rules: monotonic within one handle (never goes back),
    /// *not* comparable across backends or across two stores of one backend,
    /// and the number itself means nothing. The only supported questions are
    /// "is it the value I saw" and "is it larger". Cheap: no I/O beyond one
    /// read of the backend's own marker.
    ///
    /// No default: a backend that cannot answer cannot tell a host whether
    /// anything changed, and a guessed constant would say "never".
    fn change_token(&self) -> Result<u64, StoreError>;
}

/// What [`AttributedStore::apply_operation_if_clock`] did.
///
/// The clock check and the write are one transaction. `Moved` means another
/// writer committed first and this call wrote nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardedWrite {
    /// The clock matched; the operation is written. `clock` is the row's
    /// new `logical_clock` — the revision the writer now holds.
    Applied { operation_id: ItemId, clock: u64 },
    /// The row moved (or is gone, `None`) since the caller read it. Nothing
    /// was written.
    Moved { clock: Option<u64> },
}

/// What [`AttributedStore::apply_operations_if_clock`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardedBatch {
    /// The guard row's clock matched; every operation is written, in order.
    /// `clock` is the guard row's new `logical_clock`.
    Applied {
        operation_ids: Vec<ItemId>,
        clock: u64,
    },
    /// The guard row moved (or is gone, `None`) since the caller read it.
    /// Nothing was written.
    Moved { clock: Option<u64> },
}

/// The store port koine's layout and surface records speak.
///
/// [`ItemStore`] is the item CRUD. This adds the attributed operation and the
/// compare-and-swap the live layout row and a surface's spec revision require.
/// `SqliteItemStore` is the implementation in this crate. Layout and surface
/// persistence are generic over this trait, so a second backend can host those
/// rows without those services naming sqlite.
///
/// The item, query and operation types live in this crate. `SqliteItemStore`
/// in `impress-core` is the implementation. A second backend hosts the same
/// rows by implementing this trait (`docs/plan-koine-separation.md`, K1b).
pub trait AttributedStore: ItemStore {
    /// Apply one attributed operation and materialize it on the target.
    fn apply_operation(&self, spec: OperationSpec) -> Result<ItemId, StoreError>;

    /// Apply `spec` only if the target row's `logical_clock` is still
    /// `expected_clock`.
    fn apply_operation_if_clock(
        &self,
        spec: OperationSpec,
        expected_clock: u64,
    ) -> Result<GuardedWrite, StoreError>;

    /// Apply every spec, sharing one batch, only if `guard`'s `logical_clock`
    /// is still `expected_clock`. An empty batch is refused.
    fn apply_operations_if_clock(
        &self,
        specs: Vec<OperationSpec>,
        guard: ItemId,
        expected_clock: u64,
    ) -> Result<GuardedBatch, StoreError>;

    /// The row's `logical_clock`, or `None` when the row does not exist.
    fn logical_clock_of(&self, id: ItemId) -> Result<Option<u64>, StoreError>;

    /// Apply several operations as one batch: all of them or none
    /// (docs/plan-koine-ai-tasks.md Q-D4). Answers the operation ids, in
    /// order. An empty batch writes nothing.
    ///
    /// The default handles one target as a clock-guarded batch
    /// ([`apply_operations_if_clock`](Self::apply_operations_if_clock)),
    /// reading the clock again when another writer moved the row first.
    /// Specs naming several targets are applied in order, one at a time:
    /// a backend that can hold them in one transaction overrides this.
    /// SQLite does.
    fn apply_operation_batch(&self, specs: Vec<OperationSpec>) -> Result<Vec<ItemId>, StoreError> {
        let Some(first) = specs.first().map(|spec| spec.target_id) else {
            return Ok(Vec::new());
        };
        if specs.iter().any(|spec| spec.target_id != first) {
            return specs
                .into_iter()
                .map(|spec| self.apply_operation(spec))
                .collect();
        }
        const ATTEMPTS: usize = 8;
        for _ in 0..ATTEMPTS {
            let clock = self
                .logical_clock_of(first)?
                .ok_or(StoreError::NotFound(first))?;
            match self.apply_operations_if_clock(specs.clone(), first, clock)? {
                GuardedBatch::Applied { operation_ids, .. } => return Ok(operation_ids),
                GuardedBatch::Moved { clock: None } => return Err(StoreError::NotFound(first)),
                GuardedBatch::Moved { .. } => continue,
            }
        }
        Err(StoreError::Storage(format!(
            "apply_operation_batch: {first} moved under {ATTEMPTS} attempts"
        )))
    }

    /// The operation rows (schema `core/operation`) that targeted `target`,
    /// oldest first: ordered by `logical_clock` ascending, as the oracle's
    /// `SqliteItemStore::operations_for` does, so `limit` keeps the *first*
    /// `limit` operations. `None` returns them all. An item with no history
    /// (or that does not exist) answers an empty list.
    ///
    /// The default is a query over `op_target_id`; SQLite overrides it with
    /// its indexed column (and breaks ties on `created`, then `id`).
    fn operations_for(
        &self,
        target: ItemId,
        limit: Option<usize>,
    ) -> Result<Vec<Item>, StoreError> {
        self.query(&ItemQuery {
            schema: Some(CORE_OPERATION),
            predicates: vec![Predicate::Eq(
                "op_target_id".into(),
                Value::String(target.to_string()),
            )],
            sort: vec![SortDescriptor {
                field: "logical_clock".into(),
                ascending: true,
            }],
            limit,
            ..Default::default()
        })
    }

    /// One durable string per key for the store itself (a migration marker,
    /// an applied rename-table version): the value last written by
    /// [`metadata_set`](Self::metadata_set), or `None` for a key never set.
    ///
    /// The default keeps one item per key, schema [`STORE_METADATA_SCHEMA`],
    /// payload `{ "key", "value" }`. SQLite overrides it with its
    /// `store_metadata` table.
    fn metadata_get(&self, key: &str) -> Result<Option<String>, StoreError> {
        let hits = self.query(&metadata_query(key))?;
        Ok(hits
            .into_iter()
            .next()
            .and_then(|item| match item.payload.get("value") {
                Some(Value::String(v)) => Some(v.clone()),
                _ => None,
            }))
    }

    /// Write one metadata value, replacing the key's previous one.
    ///
    /// The default updates the key's item or inserts it. Two handles setting
    /// a key that has never been set at the same instant may each insert a
    /// row; reads then answer the older one. (SQLite's `INSERT OR REPLACE`
    /// has no such window and overrides this.)
    fn metadata_set(&self, key: &str, value: &str) -> Result<(), StoreError> {
        match self.query(&metadata_query(key))?.into_iter().next() {
            Some(existing) => self.update(
                existing.id,
                vec![FieldMutation::SetPayload(
                    "value".into(),
                    Value::String(value.to_owned()),
                )],
            ),
            None => {
                let now = chrono::Utc::now();
                self.insert(Item {
                    id: ItemId::new_v4(),
                    schema: SchemaRef::from_stored(STORE_METADATA_SCHEMA.to_owned()),
                    payload: BTreeMap::from([
                        ("key".to_owned(), Value::String(key.to_owned())),
                        ("value".to_owned(), Value::String(value.to_owned())),
                    ]),
                    created: now,
                    modified: now,
                    author: METADATA_AUTHOR.into(),
                    author_kind: ActorKind::System,
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
                })
                .map(|_| ())
            }
        }
    }
}

/// The rows holding `key`, oldest first.
fn metadata_query(key: &str) -> ItemQuery {
    ItemQuery {
        schema: Some(SchemaRef::from_stored(STORE_METADATA_SCHEMA.to_owned())),
        predicates: vec![Predicate::Eq("key".into(), Value::String(key.to_owned()))],
        sort: vec![
            SortDescriptor {
                field: "created".into(),
                ascending: true,
            },
            SortDescriptor {
                field: "id".into(),
                ascending: true,
            },
        ],
        limit: Some(1),
        include_tags: false,
        include_references: false,
        ..Default::default()
    }
}

/// Errors from the item store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("Item not found: {0}")]
    NotFound(ItemId),

    #[error("Item already exists: {0}")]
    AlreadyExists(ItemId),

    #[error("Schema not found: {0}")]
    SchemaNotFound(SchemaRef),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Storage error: {0}")]
    Storage(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_mutation_serde_round_trip() {
        let mutations = vec![
            FieldMutation::SetPayload("title".into(), Value::String("New Title".into())),
            FieldMutation::RemovePayload("old_field".into()),
            FieldMutation::SetRead(true),
            FieldMutation::SetStarred(false),
            FieldMutation::SetFlag(Some(FlagState {
                color: "amber".into(),
                style: Some("dashed".into()),
                length: None,
            })),
            FieldMutation::AddTag("methods/sims".into()),
            FieldMutation::RemoveTag("deprecated".into()),
            FieldMutation::SetParent(None),
        ];
        for m in &mutations {
            let json = serde_json::to_string(m).unwrap();
            let back: FieldMutation = serde_json::from_str(&json).unwrap();
            assert_eq!(*m, back);
        }
    }

    #[test]
    fn store_error_display() {
        let err = StoreError::NotFound(uuid::Uuid::nil());
        assert!(err.to_string().contains("not found"));

        let err = StoreError::Validation("missing required field: title".into());
        assert!(err.to_string().contains("title"));
    }
}
