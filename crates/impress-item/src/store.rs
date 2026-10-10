use serde::{Deserialize, Serialize};
use std::sync::mpsc::Receiver;

use crate::event::ItemEvent;
use crate::item::{FlagState, Item, ItemId, Value};
use crate::operation::OperationSpec;
use crate::query::ItemQuery;
use crate::reference::{EdgeType, TypedReference};
use crate::schema::SchemaRef;

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

/// The trait that all storage backends implement.
pub trait ItemStore: Send + Sync {
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
