//! A store that tells the call it runs in what it read and wrote
//! (docs/plan-koine-notebook.md N-D3).
//!
//! A backend does not know it is inside a verb call. [`RecordingStore`] wraps
//! one, delegates every method to it unchanged, and after each successful
//! operation tells the pipeline's call context what happened:
//!
//! | Method | Noted as |
//! |---|---|
//! | `get` (found), `query`, `neighbors` | a read of each returned id |
//! | `insert`, `insert_batch` | a creation (`note_mutation(id, true, false)`) |
//! | `delete` | a deletion |
//! | `update`, `apply_operation*` | an in-place change (`note_update`) |
//!
//! Outside a call every note is a no-op, so the wrapper costs one task-local
//! lookup per operation. `count`, `subscribe`, `change_token` and
//! `logical_clock_of` read no record and note nothing.

use std::sync::mpsc::Receiver;
use std::sync::Arc;

use impress_item::{
    ArrivalCursor, AttributedStore, EdgeType, FieldMutation, GuardedBatch, GuardedWrite, Item,
    ItemEvent, ItemId, ItemQuery, ItemStore, OperationSpec, StoreCapabilities, StoreError, TextHit,
};
use impress_service_core::pipeline::context;

/// `inner`, reporting its reads and writes to the current call.
pub struct RecordingStore {
    inner: Arc<dyn AttributedStore>,
}

impl RecordingStore {
    pub fn new(inner: Arc<dyn AttributedStore>) -> Self {
        Self { inner }
    }

    /// The wrapped store.
    pub fn inner(&self) -> &Arc<dyn AttributedStore> {
        &self.inner
    }
}

fn read_all(items: &[Item]) {
    for item in items {
        context::note_read(item.id.to_string());
    }
}

impl ItemStore for RecordingStore {
    fn capabilities(&self) -> StoreCapabilities {
        self.inner.capabilities()
    }

    fn insert(&self, item: Item) -> Result<ItemId, StoreError> {
        let id = self.inner.insert(item)?;
        context::note_mutation(id.to_string(), true, false);
        Ok(id)
    }

    fn insert_batch(&self, items: Vec<Item>) -> Result<Vec<ItemId>, StoreError> {
        let ids = self.inner.insert_batch(items)?;
        for id in &ids {
            context::note_mutation(id.to_string(), true, false);
        }
        Ok(ids)
    }

    fn get(&self, id: ItemId) -> Result<Option<Item>, StoreError> {
        let item = self.inner.get(id)?;
        if item.is_some() {
            context::note_read(id.to_string());
        }
        Ok(item)
    }

    fn update(&self, id: ItemId, mutations: Vec<FieldMutation>) -> Result<(), StoreError> {
        self.inner.update(id, mutations)?;
        context::note_update(id.to_string());
        Ok(())
    }

    fn delete(&self, id: ItemId) -> Result<(), StoreError> {
        self.inner.delete(id)?;
        context::note_mutation(id.to_string(), false, true);
        Ok(())
    }

    fn query(&self, q: &ItemQuery) -> Result<Vec<Item>, StoreError> {
        let items = self.inner.query(q)?;
        read_all(&items);
        Ok(items)
    }

    fn count(&self, q: &ItemQuery) -> Result<usize, StoreError> {
        self.inner.count(q)
    }

    fn neighbors(
        &self,
        id: ItemId,
        edge_types: &[EdgeType],
        depth: u32,
    ) -> Result<Vec<Item>, StoreError> {
        let items = self.inner.neighbors(id, edge_types, depth)?;
        read_all(&items);
        Ok(items)
    }

    fn subscribe(&self, q: ItemQuery) -> Result<Receiver<ItemEvent>, StoreError> {
        self.inner.subscribe(q)
    }

    fn change_token(&self) -> Result<u64, StoreError> {
        self.inner.change_token()
    }

    fn search_text(
        &self,
        schema: Option<&str>,
        text: &str,
        limit: usize,
    ) -> Result<Vec<TextHit>, StoreError> {
        let hits = self.inner.search_text(schema, text, limit)?;
        for hit in &hits {
            context::note_read(hit.id.to_string());
        }
        Ok(hits)
    }

    fn arrivals_after(
        &self,
        cursor: ArrivalCursor,
        schema: Option<&str>,
        limit: usize,
    ) -> Result<(Vec<Item>, ArrivalCursor), StoreError> {
        let (items, next) = self.inner.arrivals_after(cursor, schema, limit)?;
        read_all(&items);
        Ok((items, next))
    }
}

impl AttributedStore for RecordingStore {
    fn apply_operation(&self, spec: OperationSpec) -> Result<ItemId, StoreError> {
        let target = spec.target_id;
        let id = self.inner.apply_operation(spec)?;
        context::note_update(target.to_string());
        Ok(id)
    }

    fn apply_operation_if_clock(
        &self,
        spec: OperationSpec,
        expected_clock: u64,
    ) -> Result<GuardedWrite, StoreError> {
        let target = spec.target_id;
        let written = self.inner.apply_operation_if_clock(spec, expected_clock)?;
        if matches!(written, GuardedWrite::Applied { .. }) {
            context::note_update(target.to_string());
        }
        Ok(written)
    }

    fn apply_operations_if_clock(
        &self,
        specs: Vec<OperationSpec>,
        guard: ItemId,
        expected_clock: u64,
    ) -> Result<GuardedBatch, StoreError> {
        let targets: Vec<ItemId> = specs.iter().map(|spec| spec.target_id).collect();
        let written = self
            .inner
            .apply_operations_if_clock(specs, guard, expected_clock)?;
        if matches!(written, GuardedBatch::Applied { .. }) {
            for target in targets {
                context::note_update(target.to_string());
            }
        }
        Ok(written)
    }

    fn logical_clock_of(&self, id: ItemId) -> Result<Option<u64>, StoreError> {
        self.inner.logical_clock_of(id)
    }

    fn apply_operation_batch(&self, specs: Vec<OperationSpec>) -> Result<Vec<ItemId>, StoreError> {
        let targets: Vec<ItemId> = specs.iter().map(|spec| spec.target_id).collect();
        let written = self.inner.apply_operation_batch(specs)?;
        for target in targets {
            context::note_update(target.to_string());
        }
        Ok(written)
    }

    fn operations_for(
        &self,
        target: ItemId,
        limit: Option<usize>,
    ) -> Result<Vec<Item>, StoreError> {
        self.inner.operations_for(target, limit)
    }

    fn metadata_get(&self, key: &str) -> Result<Option<String>, StoreError> {
        self.inner.metadata_get(key)
    }

    fn metadata_set(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.inner.metadata_set(key, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use impress_service_core::pipeline::context::{sync_scope, CallContext};
    use impress_service_core::pipeline::CallerIdentity;
    use koine_store_memory::MemoryStore;

    fn item(schema: &str) -> Item {
        serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::new_v4(), "schema": schema, "payload": {"title": "t"},
            "created": "2026-01-01T00:00:00Z", "modified": "2026-01-01T00:00:00Z",
            "author": "t", "author_kind": "Human", "logical_clock": 0,
            "origin": null, "canonical_id": null, "tags": [], "flag": null,
            "is_read": false, "is_starred": false, "priority": "normal",
            "visibility": "private", "message_type": null, "produced_by": null,
            "version": null, "batch_id": null, "references": [], "parent": null
        }))
        .unwrap()
    }

    fn call() -> Arc<CallContext> {
        Arc::new(CallContext {
            call_id: "c".into(),
            trace_id: "t".into(),
            parent_call: None,
            caller: CallerIdentity::Person,
            verb: "x-service_y".into(),
            store_override: None,
            mutation_ids: Default::default(),
            adverbs: Default::default(),
            inverse: Default::default(),
            notes: Default::default(),
        })
    }

    #[test]
    fn reads_and_writes_inside_a_call_are_noted_on_it() {
        let store = RecordingStore::new(Arc::new(MemoryStore::new()));
        let context = call();
        let (kept, gone, changed) = sync_scope(context.clone(), || {
            let kept = store.insert(item("paper")).unwrap();
            let gone = store.insert(item("paper")).unwrap();
            store.delete(gone).unwrap();
            store
                .update(
                    kept,
                    vec![FieldMutation::SetPayload(
                        "title".into(),
                        impress_item::Value::String("u".into()),
                    )],
                )
                .unwrap();
            let other = store.insert(item("note")).unwrap();
            assert!(store.get(other).unwrap().is_some());
            let all = store
                .query(&ItemQuery {
                    schema: Some(impress_item::SchemaRef::from_stored("paper".into())),
                    ..Default::default()
                })
                .unwrap();
            assert_eq!(all.len(), 1);
            (kept, gone, other)
        });
        let (inserted, deleted) = context.mutation_ids.snapshot();
        assert!(inserted.contains(&kept.to_string()));
        assert!(inserted.contains(&changed.to_string()));
        assert_eq!(deleted, vec![gone.to_string()]);
        let (reads, _) = context.notes.reads();
        let mut want = vec![kept.to_string(), changed.to_string()];
        want.sort();
        assert_eq!(reads, want);
        let (updated, _) = context.notes.updates();
        assert_eq!(updated, vec![kept.to_string()]);
    }

    #[test]
    fn outside_a_call_it_is_the_store_it_wraps() {
        let store = RecordingStore::new(Arc::new(MemoryStore::new()));
        let id = store.insert(item("paper")).unwrap();
        assert!(store.get(id).unwrap().is_some());
        assert_eq!(store.count(&ItemQuery::default()).unwrap(), 1);
        assert!(!store.capabilities().durable);
    }
}
