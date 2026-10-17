//! What a verb actually read and wrote, over any backend of the store port
//! (ADR-0036 D1; docs/plan-koine-kit-moves.md KM-D14).
//!
//! A declaration on a `VerbDescriptor` (`effects = { reads, writes, reach }`)
//! is only as true as whoever wrote it. [`SpyStore`] wraps a store and, while
//! a window is open ([`SpyStore::start`] … [`SpyStore::stop`]), records the
//! record kinds every call touched, so a test can run a verb against it and
//! assert `observed ⊆ declared`. Outside a window it forwards and records
//! nothing. Every method of the port is forwarded, the ones with defaults
//! included, so the wrapped store's own answers (an index, a fast path) are
//! the ones the caller gets.
//!
//! What is recorded, and from where (the rules impress-core's SQLite hook
//! has followed since ADR-0036):
//! - **reads**: `get` (the row's kind), `query`/`count`/`arrivals_after`
//!   (the query's schema when it names one, else the kinds of the rows
//!   returned, else [`ANY`]), `neighbors` (the rows' kinds), `search_text`
//!   (its schema, else [`ANY`]), `references_to` (the kinds of the sources
//!   it returned), `operations_for` (`core/operation`);
//! - **writes**: `insert`/`insert_batch` (the row's kind; the id in
//!   `inserted`), `update`/`delete`/`apply_operation*` (the target row's
//!   kind, read through the wrapped store so the spy's own look is not
//!   charged as a read; [`ANY`] when the row is gone; a delete's id in
//!   `deleted`).

use std::collections::BTreeSet;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use impress_item::{
    ActorKind, ArrivalCursor, AttributedStore, EdgeType, FieldMutation, GuardedBatch, GuardedWrite,
    Item, ItemEvent, ItemId, ItemQuery, ItemStore, OperationSpec, StoreCapabilities, StoreError,
    TextHit,
};

/// The kind recorded when the store touched rows whose kind it could not
/// name: a schemaless `count`, an empty schemaless `query`, a write whose row
/// was already gone. A declaration covers it only with `any(…)`.
pub const ANY: &str = "*";

/// The kind of an operation row.
const OPERATION: &str = "core/operation";

/// What one recording window saw.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Observed {
    pub reads: BTreeSet<String>,
    pub writes: BTreeSet<String>,
    pub inserted: Vec<ItemId>,
    pub deleted: Vec<ItemId>,
}

/// A store that records what it is asked, around `S`.
pub struct SpyStore<S: ?Sized> {
    inner: Arc<S>,
    window: Mutex<Option<Observed>>,
}

impl<S: AttributedStore + ?Sized> SpyStore<S> {
    pub fn new(inner: Arc<S>) -> Self {
        Self {
            inner,
            window: Mutex::new(None),
        }
    }

    /// The wrapped store.
    pub fn inner(&self) -> &Arc<S> {
        &self.inner
    }

    /// Open a window. One already open is replaced (its contents dropped), so
    /// a test that forgot to stop cannot leak into the next.
    pub fn start(&self) {
        *self.lock() = Some(Observed::default());
    }

    /// Close the window and answer what it saw; empty when none was open.
    pub fn stop(&self) -> Observed {
        self.lock().take().unwrap_or_default()
    }

    /// Whether a window is open.
    pub fn is_recording(&self) -> bool {
        self.lock().is_some()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Observed>> {
        self.window.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn with(&self, f: impl FnOnce(&mut Observed)) {
        if let Some(observed) = self.lock().as_mut() {
            f(observed);
        }
    }

    fn read(&self, kind: &str) {
        self.with(|o| {
            o.reads.insert(kind.to_string());
        });
    }

    fn read_rows<'a>(&self, schema: Option<String>, rows: impl Iterator<Item = &'a Item>) {
        self.with(|o| {
            if let Some(kind) = schema {
                o.reads.insert(kind);
                return;
            }
            let mut any = false;
            for row in rows {
                any = true;
                o.reads.insert(row.schema.to_string());
            }
            if !any {
                o.reads.insert(ANY.to_string());
            }
        });
    }

    fn write(&self, kind: String) {
        self.with(|o| {
            o.writes.insert(kind);
        });
    }

    /// The target row's kind, looked up through the wrapped store (not a read
    /// the caller made).
    fn kind_of(&self, id: ItemId) -> String {
        if !self.is_recording() {
            return String::new();
        }
        match self.inner.get(id) {
            Ok(Some(item)) => item.schema.to_string(),
            _ => ANY.to_string(),
        }
    }
}

impl<S: AttributedStore + ?Sized> ItemStore for SpyStore<S> {
    /// The wrapped store's, except blobs: the spy does not carry them.
    fn capabilities(&self) -> StoreCapabilities {
        StoreCapabilities {
            blobs: false,
            ..self.inner.capabilities()
        }
    }

    fn default_author(&self) -> (String, ActorKind) {
        self.inner.default_author()
    }

    fn insert(&self, item: Item) -> Result<ItemId, StoreError> {
        let kind = item.schema.to_string();
        let id = self.inner.insert(item)?;
        self.with(|o| {
            o.writes.insert(kind);
            o.inserted.push(id);
        });
        Ok(id)
    }

    fn insert_batch(&self, items: Vec<Item>) -> Result<Vec<ItemId>, StoreError> {
        let kinds: Vec<String> = items.iter().map(|i| i.schema.to_string()).collect();
        let ids = self.inner.insert_batch(items)?;
        self.with(|o| {
            o.writes.extend(kinds);
            o.inserted.extend(ids.iter().copied());
        });
        Ok(ids)
    }

    fn get(&self, id: ItemId) -> Result<Option<Item>, StoreError> {
        let row = self.inner.get(id)?;
        if let Some(item) = &row {
            self.read(item.schema.as_str());
        }
        Ok(row)
    }

    fn update(&self, id: ItemId, mutations: Vec<FieldMutation>) -> Result<(), StoreError> {
        let kind = self.kind_of(id);
        self.inner.update(id, mutations)?;
        self.write(kind);
        Ok(())
    }

    fn delete(&self, id: ItemId) -> Result<(), StoreError> {
        let kind = self.kind_of(id);
        self.inner.delete(id)?;
        self.with(|o| {
            o.writes.insert(kind);
            o.deleted.push(id);
        });
        Ok(())
    }

    fn query(&self, q: &ItemQuery) -> Result<Vec<Item>, StoreError> {
        let rows = self.inner.query(q)?;
        self.read_rows(q.schema.as_ref().map(|s| s.to_string()), rows.iter());
        Ok(rows)
    }

    fn count(&self, q: &ItemQuery) -> Result<usize, StoreError> {
        let n = self.inner.count(q)?;
        self.read(&q.schema.as_ref().map_or(ANY.to_string(), |s| s.to_string()));
        Ok(n)
    }

    fn neighbors(
        &self,
        id: ItemId,
        edge_types: &[EdgeType],
        depth: u32,
    ) -> Result<Vec<Item>, StoreError> {
        let rows = self.inner.neighbors(id, edge_types, depth)?;
        self.read_rows(None, rows.iter());
        Ok(rows)
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
        self.read(schema.unwrap_or(ANY));
        Ok(hits)
    }

    fn references_to(
        &self,
        target: ItemId,
        edge_types: &[EdgeType],
    ) -> Result<Vec<(ItemId, EdgeType)>, StoreError> {
        let found = self.inner.references_to(target, edge_types)?;
        if self.is_recording() {
            for (source, _) in &found {
                let kind = self.kind_of(*source);
                self.read(&kind);
            }
        }
        Ok(found)
    }

    fn arrivals_after(
        &self,
        cursor: ArrivalCursor,
        schema: Option<&str>,
        limit: usize,
    ) -> Result<(Vec<Item>, ArrivalCursor), StoreError> {
        let (rows, next) = self.inner.arrivals_after(cursor, schema, limit)?;
        self.read_rows(schema.map(str::to_string), rows.iter());
        Ok((rows, next))
    }
}

impl<S: AttributedStore + ?Sized> AttributedStore for SpyStore<S> {
    fn apply_operation(&self, spec: OperationSpec) -> Result<ItemId, StoreError> {
        let kind = self.kind_of(spec.target_id);
        let id = self.inner.apply_operation(spec)?;
        self.write(kind);
        Ok(id)
    }

    fn apply_operation_if_clock(
        &self,
        spec: OperationSpec,
        expected_clock: u64,
    ) -> Result<GuardedWrite, StoreError> {
        let kind = self.kind_of(spec.target_id);
        let outcome = self.inner.apply_operation_if_clock(spec, expected_clock)?;
        if matches!(outcome, GuardedWrite::Applied { .. }) {
            self.write(kind);
        }
        Ok(outcome)
    }

    fn apply_operations_if_clock(
        &self,
        specs: Vec<OperationSpec>,
        guard: ItemId,
        expected_clock: u64,
    ) -> Result<GuardedBatch, StoreError> {
        let kinds: BTreeSet<String> = specs.iter().map(|s| self.kind_of(s.target_id)).collect();
        let outcome = self
            .inner
            .apply_operations_if_clock(specs, guard, expected_clock)?;
        if matches!(outcome, GuardedBatch::Applied { .. }) {
            self.with(|o| o.writes.extend(kinds));
        }
        Ok(outcome)
    }

    fn logical_clock_of(&self, id: ItemId) -> Result<Option<u64>, StoreError> {
        self.inner.logical_clock_of(id)
    }

    fn apply_operation_batch(&self, specs: Vec<OperationSpec>) -> Result<Vec<ItemId>, StoreError> {
        let kinds: BTreeSet<String> = specs.iter().map(|s| self.kind_of(s.target_id)).collect();
        let ids = self.inner.apply_operation_batch(specs)?;
        self.with(|o| o.writes.extend(kinds));
        Ok(ids)
    }

    fn operations_for(
        &self,
        target: ItemId,
        limit: Option<usize>,
    ) -> Result<Vec<Item>, StoreError> {
        let rows = self.inner.operations_for(target, limit)?;
        self.read(OPERATION);
        Ok(rows)
    }

    fn metadata_get(&self, key: &str) -> Result<Option<String>, StoreError> {
        self.inner.metadata_get(key)
    }

    fn metadata_set(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.inner.metadata_set(key, value)
    }
}
