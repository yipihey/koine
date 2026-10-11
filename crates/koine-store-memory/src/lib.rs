//! `koine-store-memory`: the in-memory koine store.
//!
//! The reference backend of the conformance suite (`koine-store-conformance`),
//! the host's default when no store is named, and the test double every koine
//! crate uses (`docs/plan-koine-store.md` D3). It implements the whole port:
//! [`ItemStore`], [`AttributedStore`] and [`BlobStore`], with the semantics
//! D2 spells out from `SqliteItemStore`.
//!
//! ```
//! use impress_item::ItemStore;
//! let store = koine_store_memory::MemoryStore::new();
//! assert!(store.capabilities().blobs);
//! assert!(!store.capabilities().durable);
//! ```
//!
//! Two modules are public for other backends to reuse rather than redefine:
//! [`eval`] is what a query means (predicates, sorting, paging), and
//! [`policy`] is the hook for rules a *schema* imposes on writes (SQLite's
//! immutable manuscript revisions) that a backend must not learn by name.
//!
//! ## What is stored
//!
//! One map of rows. An operation is a row like any other: an item of schema
//! `core/operation` with the `op_target_id` the oracle gives it, so
//! `get(operation_id)` answers it and a query with no schema counts it
//! (filter by schema to see only your own kinds, as the oracle's own tests
//! do). Deleting an item deletes its operations, every reference to it, and
//! clears its children's parent. Rows come back in insertion order unless a
//! query sorts them.
//!
//! ## Where this store is stricter or kinder than the oracle
//!
//! * A reference or parent naming an item that is not stored is refused
//!   (`Validation`) before anything is written; the oracle's foreign keys
//!   refuse it too but after it has already recorded the operation.
//! * `update` is all-or-nothing across its mutations: every one is checked
//!   before the first is applied.
//! * The hybrid clock never answers the same value twice, even past 65,535
//!   operations in one millisecond, where the oracle's counter would wrap.
//! * `neighbors` never lists an item twice.

pub mod eval;
pub mod policy;
mod state;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use impress_item::{
    ActorKind, AttributedStore, BlobStore, EdgeType, FieldMutation, GuardedBatch, GuardedWrite,
    Item, ItemEvent, ItemId, ItemQuery, ItemStore, OperationIntent, OperationSpec, RetentionTier,
    StoreCapabilities, StoreError,
};
use uuid::Uuid;

pub use policy::{NoPolicy, WritePolicy};

use eval::{Graph, Row as EvalRow};
use state::{Env, State};

/// What this store declares at open (`docs/plan-koine-store.md` D1 and D3).
pub const CAPABILITIES: StoreCapabilities = StoreCapabilities {
    indexed_query: false,
    atomic_batch: true,
    subscribe: true,
    cross_process_events: false,
    durable: false,
    shared: false,
    blobs: true,
};

struct Subscriber {
    /// `None` hears every event; `Some(prefix)` those about a schema that
    /// starts with it.
    prefix: Option<String>,
    tx: Sender<ItemEvent>,
}

struct Inner {
    state: State,
    blobs: BTreeMap<(ItemId, String), Vec<u8>>,
    subscribers: Vec<Subscriber>,
}

impl Inner {
    /// Deliver `event` to every subscriber that wants a write to a row of
    /// `schema` (`None`, an unknown schema, goes to everyone), dropping
    /// subscribers whose receiver is gone.
    fn emit(&mut self, schema: Option<&str>, event: ItemEvent) {
        self.subscribers.retain(|s| {
            let wanted = match (&s.prefix, schema) {
                (Some(prefix), Some(schema)) => schema.starts_with(prefix.as_str()),
                _ => true,
            };
            !wanted || s.tx.send(event.clone()).is_ok()
        });
    }
}

/// The in-memory store. Cheap to create, `Send + Sync`, forgets everything
/// when dropped.
pub struct MemoryStore {
    inner: RwLock<Inner>,
    policy: Box<dyn WritePolicy>,
    /// Stamped on rows written without an origin (the oracle's store id).
    origin: String,
    /// Author of the operations `update` records.
    author: String,
    author_kind: ActorKind,
    wall_ms: Box<dyn Fn() -> u64 + Send + Sync>,
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryStore")
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

fn system_wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn poisoned<T>(_: T) -> StoreError {
    StoreError::Storage("memory store lock poisoned by a panicking writer".into())
}

/// Blob names are file names in the directory backend, so they are refused
/// here too: the two must accept the same names.
fn check_blob_name(name: &str) -> Result<(), StoreError> {
    let bad = name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']);
    if bad {
        Err(StoreError::Validation(format!(
            "blob name {name:?} must be a plain file name"
        )))
    } else {
        Ok(())
    }
}

impl MemoryStore {
    /// An empty store that accepts every operation.
    pub fn new() -> Self {
        Self::with_policy(Box::new(NoPolicy))
    }

    /// An empty store that asks `policy` before every operation.
    pub fn with_policy(policy: Box<dyn WritePolicy>) -> Self {
        MemoryStore {
            inner: RwLock::new(Inner {
                state: State::default(),
                blobs: BTreeMap::new(),
                subscribers: Vec::new(),
            }),
            policy,
            origin: format!("memory-{}", Uuid::new_v4()),
            author: "system:local".into(),
            author_kind: ActorKind::System,
            wall_ms: Box::new(system_wall_ms),
        }
    }

    /// Record `author` on the operations `update` writes (default
    /// `system:local`, a system actor, as the oracle's default config).
    pub fn with_author(mut self, author: impl Into<String>, kind: ActorKind) -> Self {
        self.author = author.into();
        self.author_kind = kind;
        self
    }

    /// Read the wall clock from `wall_ms` (epoch milliseconds) instead of the
    /// system clock: a test that needs the same millisecond twice, or the
    /// counter at its limit, supplies one.
    pub fn with_wall_clock(mut self, wall_ms: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        self.wall_ms = Box::new(wall_ms);
        self
    }

    /// This store's origin id, stamped on every row written without one.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Raise the clock so every later clock sorts after `remote` (the sync
    /// receive rule: component-wise maximum of `(wall_ms, counter)`). A clock
    /// that is behind this store's is a no-op.
    pub fn observe_clock(&self, remote: u64) -> Result<(), StoreError> {
        self.write()?.state.observe_clock(remote);
        Ok(())
    }

    /// The retention tier an operation row was recorded with, if `operation`
    /// is one. (Not part of the port: the tier steers compaction, which this
    /// store does not do.)
    pub fn retention_of(&self, operation: ItemId) -> Result<Option<RetentionTier>, StoreError> {
        Ok(self
            .read()?
            .state
            .rows
            .get(&operation)
            .and_then(|r| r.retention))
    }

    fn read(&self) -> Result<RwLockReadGuard<'_, Inner>, StoreError> {
        self.inner.read().map_err(poisoned)
    }

    fn write(&self) -> Result<RwLockWriteGuard<'_, Inner>, StoreError> {
        self.inner.write().map_err(poisoned)
    }

    fn env(&self) -> Env<'_> {
        Env {
            policy: self.policy.as_ref(),
            origin: &self.origin,
            wall_ms: &*self.wall_ms,
        }
    }
}

/// `ReferencedBy` asks whether another row holds an edge to this one.
struct StateGraph<'a>(&'a State);

impl Graph for StateGraph<'_> {
    fn has_edge(&self, source: ItemId, edge: &EdgeType, target: ItemId) -> bool {
        self.0.rows.get(&source).is_some_and(|row| {
            row.item
                .references
                .iter()
                .any(|r| r.target == target && &r.edge_type == edge)
        })
    }
}

/// Every row as the evaluator sees it, with its insertion number.
fn eval_rows(state: &State) -> impl Iterator<Item = (u64, EvalRow<'_>)> {
    state.rows.values().map(|row| {
        (
            row.seq,
            EvalRow {
                item: &row.item,
                op_target: row.op_target,
            },
        )
    })
}

impl ItemStore for MemoryStore {
    fn capabilities(&self) -> StoreCapabilities {
        CAPABILITIES
    }

    fn insert(&self, item: Item) -> Result<ItemId, StoreError> {
        let id = item.id;
        self.insert_batch(vec![item])?;
        Ok(id)
    }

    fn insert_batch(&self, items: Vec<Item>) -> Result<Vec<ItemId>, StoreError> {
        let mut guard = self.write()?;
        let inner = &mut *guard;
        // Validated as a whole first: after this nothing can fail, so a
        // batch containing a duplicate inserts nothing.
        inner.state.validate_new(&items)?;
        let env = self.env();
        let ids = items.iter().map(|i| i.id).collect();
        let before = inner.state.clock_now();
        let wrote = !items.is_empty();
        for item in items {
            let stored = inner.state.insert_validated(item, &env);
            let schema = stored.schema.as_str().to_owned();
            inner.emit(Some(&schema), ItemEvent::Created(Box::new(stored)));
        }
        // Rows that arrived with clocks of their own stamp nothing; the write
        // still has to move the change token.
        if wrote && inner.state.clock_now() == before {
            inner.state.next_clock(&env);
        }
        Ok(ids)
    }

    fn get(&self, id: ItemId) -> Result<Option<Item>, StoreError> {
        Ok(self.read()?.state.rows.get(&id).map(|r| r.item.clone()))
    }

    fn update(&self, id: ItemId, mutations: Vec<FieldMutation>) -> Result<(), StoreError> {
        let mut guard = self.write()?;
        let inner = &mut *guard;
        let env = self.env();

        // A write that changes nothing mints no operation, bumps no clock and
        // leaves `modified` alone. Judged against the row as it is now, for
        // payload, read and starred only (the oracle's `filter_noop_mutations`).
        let mutations: Vec<FieldMutation> = match inner.state.rows.get(&id) {
            None => mutations,
            Some(row) => mutations
                .into_iter()
                .filter(|m| match m {
                    FieldMutation::SetPayload(k, v) => row.item.payload.get(k) != Some(v),
                    FieldMutation::RemovePayload(k) => row.item.payload.contains_key(k),
                    FieldMutation::SetRead(v) => row.item.is_read != *v,
                    FieldMutation::SetStarred(v) => row.item.is_starred != *v,
                    _ => true,
                })
                .collect(),
        };
        let batch_id = (mutations.len() > 1).then(|| Uuid::new_v4().to_string());
        let specs: Vec<OperationSpec> = mutations
            .into_iter()
            .map(|m| OperationSpec {
                target_id: id,
                op_type: m.into(),
                intent: OperationIntent::Routine,
                reason: None,
                batch_id: batch_id.clone(),
                author: self.author.clone(),
                author_kind: self.author_kind,
                retention: RetentionTier::Compactable,
            })
            .collect();

        // All or nothing: every mutation is checked before the first applies.
        for spec in &specs {
            inner.state.preflight(spec, &env)?;
        }
        for spec in &specs {
            let applied = inner.state.apply_operation(spec, &env)?;
            inner.emit(
                Some(&applied.target_schema),
                ItemEvent::OperationApplied {
                    operation_id: applied.operation_id,
                    target_id: id,
                },
            );
        }
        Ok(())
    }

    fn delete(&self, id: ItemId) -> Result<(), StoreError> {
        let mut guard = self.write()?;
        let inner = &mut *guard;
        let schema = inner.state.delete(id)?;
        // A delete stamps no row, but it is a write: tick the clock so the
        // change token moves.
        inner.state.next_clock(&self.env());
        inner.blobs.retain(|(item, _), _| *item != id);
        inner.emit(Some(&schema), ItemEvent::Deleted(id));
        Ok(())
    }

    fn query(&self, q: &ItemQuery) -> Result<Vec<Item>, StoreError> {
        let guard = self.read()?;
        let graph = StateGraph(&guard.state);
        // Insertion order is the tie-break for the sort and the order of an
        // unsorted result.
        let mut ordered: Vec<(u64, EvalRow<'_>)> = eval_rows(&guard.state).collect();
        ordered.sort_by_key(|(seq, _)| *seq);
        let hits = eval::run_query(ordered.into_iter().map(|(_, row)| row), q, &graph);
        Ok(hits
            .into_iter()
            .map(|item| {
                let mut item = item.clone();
                if !q.include_tags {
                    item.tags.clear();
                }
                if !q.include_references {
                    item.references.clear();
                }
                item
            })
            .collect())
    }

    fn count(&self, q: &ItemQuery) -> Result<usize, StoreError> {
        let guard = self.read()?;
        let graph = StateGraph(&guard.state);
        Ok(eval::count_matching(
            eval_rows(&guard.state).map(|(_, row)| row),
            q,
            &graph,
        ))
    }

    fn neighbors(
        &self,
        id: ItemId,
        edge_types: &[EdgeType],
        depth: u32,
    ) -> Result<Vec<Item>, StoreError> {
        let guard = self.read()?;
        let rows = &guard.state.rows;
        let mut seen = BTreeSet::from([id]);
        let mut frontier = vec![id];
        let mut found = Vec::new();
        for _ in 0..depth {
            let mut next = Vec::new();
            for current in &frontier {
                // Edges out of `current`, then edges into it, both along the
                // filter (an empty filter follows every edge type).
                let out = rows
                    .get(current)
                    .into_iter()
                    .flat_map(|row| eval::follows(&row.item, edge_types).map(|r| r.target));
                let into = rows.values().filter_map(|row| {
                    eval::follows(&row.item, edge_types)
                        .any(|r| r.target == *current)
                        .then_some(row.item.id)
                });
                for other in out.chain(into) {
                    if rows.contains_key(&other) && seen.insert(other) {
                        next.push(other);
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            found.extend(next.iter().map(|n| rows[n].item.clone()));
            frontier = next;
        }
        Ok(found)
    }

    fn subscribe(&self, q: ItemQuery) -> Result<Receiver<ItemEvent>, StoreError> {
        let (tx, rx) = mpsc::channel();
        self.write()?.subscribers.push(Subscriber {
            prefix: q.schema.map(|s| s.as_str().to_owned()),
            tx,
        });
        Ok(rx)
    }

    /// The packed clock. Every committed write moves it: inserts and
    /// operations stamp it, and a delete (which stamps no row) and an insert
    /// of rows that brought their own clocks tick it. A read leaves it be.
    fn change_token(&self) -> Result<u64, StoreError> {
        Ok(self.read()?.state.clock_now())
    }
}

impl AttributedStore for MemoryStore {
    fn apply_operation(&self, spec: OperationSpec) -> Result<ItemId, StoreError> {
        let mut guard = self.write()?;
        let inner = &mut *guard;
        let applied = inner.state.apply_operation(&spec, &self.env())?;
        inner.emit(
            Some(&applied.target_schema),
            ItemEvent::OperationApplied {
                operation_id: applied.operation_id,
                target_id: spec.target_id,
            },
        );
        Ok(applied.operation_id)
    }

    fn apply_operation_if_clock(
        &self,
        spec: OperationSpec,
        expected_clock: u64,
    ) -> Result<GuardedWrite, StoreError> {
        let mut guard = self.write()?;
        let inner = &mut *guard;
        // The compare and the write are one step under the write lock.
        match inner.state.clock_of(spec.target_id) {
            Some(clock) if clock == expected_clock => {}
            other => return Ok(GuardedWrite::Moved { clock: other }),
        }
        let applied = inner.state.apply_operation(&spec, &self.env())?;
        inner.emit(
            Some(&applied.target_schema),
            ItemEvent::OperationApplied {
                operation_id: applied.operation_id,
                target_id: spec.target_id,
            },
        );
        Ok(GuardedWrite::Applied {
            operation_id: applied.operation_id,
            clock: applied.clock,
        })
    }

    fn apply_operations_if_clock(
        &self,
        specs: Vec<OperationSpec>,
        guard: ItemId,
        expected_clock: u64,
    ) -> Result<GuardedBatch, StoreError> {
        if specs.is_empty() {
            return Err(StoreError::Validation(
                "a guarded batch needs at least one operation".into(),
            ));
        }
        let mut lock = self.write()?;
        let inner = &mut *lock;
        match inner.state.clock_of(guard) {
            Some(clock) if clock == expected_clock => {}
            other => return Ok(GuardedBatch::Moved { clock: other }),
        }
        // Build the new state on a copy and swap it in only if every
        // operation applied: a failure part-way leaves nothing behind.
        let env = self.env();
        let mut work = inner.state.clone();
        let batch_id = Uuid::new_v4().to_string();
        let mut done = Vec::with_capacity(specs.len());
        for mut spec in specs {
            spec.batch_id = Some(batch_id.clone());
            let applied = work.apply_operation(&spec, &env)?;
            done.push((spec.target_id, applied));
        }
        let clock = work.clock_of(guard).ok_or(StoreError::NotFound(guard))?;
        inner.state = work;
        let mut operation_ids = Vec::with_capacity(done.len());
        for (target_id, applied) in done {
            operation_ids.push(applied.operation_id);
            inner.emit(
                Some(&applied.target_schema),
                ItemEvent::OperationApplied {
                    operation_id: applied.operation_id,
                    target_id,
                },
            );
        }
        Ok(GuardedBatch::Applied {
            operation_ids,
            clock,
        })
    }

    fn logical_clock_of(&self, id: ItemId) -> Result<Option<u64>, StoreError> {
        Ok(self.read()?.state.clock_of(id))
    }
}

impl BlobStore for MemoryStore {
    fn put_blob(&self, item: ItemId, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
        check_blob_name(name)?;
        let mut guard = self.write()?;
        if !guard.state.rows.contains_key(&item) {
            return Err(StoreError::NotFound(item));
        }
        guard.blobs.insert((item, name.to_owned()), bytes.to_vec());
        Ok(())
    }

    fn get_blob(&self, item: ItemId, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.read()?.blobs.get(&(item, name.to_owned())).cloned())
    }

    fn delete_blob(&self, item: ItemId, name: &str) -> Result<(), StoreError> {
        self.write()?.blobs.remove(&(item, name.to_owned()));
        Ok(())
    }

    fn list_blobs(&self, item: ItemId) -> Result<Vec<String>, StoreError> {
        Ok(self
            .read()?
            .blobs
            .range((item, String::new())..)
            .take_while(|((owner, _), _)| *owner == item)
            .map(|((_, name), _)| name.clone())
            .collect())
    }
}
