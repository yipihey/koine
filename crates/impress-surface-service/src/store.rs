//! The store side of a surface: `impress/ui/surface@1.0.0`,
//! `impress/ui/surface-state@1.0.0` and `impress/ui/surface-event@1.0.0` rows
//! (ADR-0033 D1/D5).
//!
//! # Scope, mirroring `impress-layout-service/src/store.rs`
//!
//! * The **surface** row itself is `Private` + `Durable`, and syncs like a
//!   named layout: an agent-built surface is a durable artifact of the
//!   conversation, not scratch state (`impress-core/src/schemas/ui.rs`'s
//!   module docs).
//! * The **state** row is one per `(surface, host)`, `Private` +
//!   `Ephemeral`, device-scoped by `host` — the same shape the layout tree's
//!   *live* row uses for `device` (never synced, exists so a process in
//!   another chat can read back what the human changed).
//! * The **event** rows are append-only, `Private` + `Ephemeral`, pruned to
//!   the last 200 per `(surface, host)` (ADR-0033 D5) — the same privacy
//!   reasoning ADR-0031 D7 gives for exploration churn: nothing is recorded
//!   here that exists only to be analysed later.
//!
//! # Attribution
//!
//! Every write after the first goes through
//! [`SqliteItemStore::apply_operation`] exactly as the layout store's module
//! docs describe: an insert (the row's creation) is attributed on the
//! envelope but is not itself an operation, so there is no tier to set on
//! it; every later write picks [`RetentionTier::Durable`] +
//! [`OperationIntent::Editorial`] (the surface row's `spec`) or
//! [`RetentionTier::Ephemeral`] + [`OperationIntent::Routine`] (the state
//! row) explicitly. Event rows are never patched, only inserted and later
//! hard-deleted by pruning, so they carry no operation at all — the same
//! reasoning the layout store gives for why a row's *creation* is a plain
//! insert.
//!
//! [`RetentionTier::Durable`]: impress_item::operation::RetentionTier::Durable
//! [`OperationIntent::Editorial`]: impress_item::operation::OperationIntent::Editorial
//! [`RetentionTier::Ephemeral`]: impress_item::operation::RetentionTier::Ephemeral
//! [`OperationIntent::Routine`]: impress_item::operation::OperationIntent::Routine

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
// Aliased: this module's own public API speaks `serde_json::Value` (the
// shape every other module in this crate uses), and only converts to the
// store's own `impress_item::item::Value` at the point of writing a payload
// field — see `spec_json`/`parse_json` and the `ItemValue::String(...)`
// constructions below.
use impress_item::item::Value as ItemValue;
use impress_item::item::{ActorKind, Item, ItemId, Priority, Visibility};
use impress_item::operation::{OperationSpec, OperationType};
use impress_item::query::{ItemQuery, Predicate, SortDescriptor};
use impress_item::schema::refs::{
    IMPRESS_UI_SURFACE as SURFACE_SCHEMA_REF, IMPRESS_UI_SURFACE_EVENT as SURFACE_EVENT_SCHEMA_REF,
    IMPRESS_UI_SURFACE_STATE as SURFACE_STATE_SCHEMA_REF,
};
use impress_item::store::{AttributedStore, GuardedBatch, StoreError};
pub use impress_layout_service::authorship::actor_from;
use impress_layout_service::authorship::{author_for_service, Ephemerality};
use impress_service_core::Refusal;
use impress_surface::SurfaceSpec;
use serde_json::Value;

/// How many events a `(surface, host)` ring keeps. ADR-0033 D5.
pub const EVENT_RING_CAPACITY: usize = 200;

/// Payload field names, spelled once. Copy these, never retype the strings:
/// a reader that spells a field differently from its writer reads `None`
/// forever and looks exactly like "nothing saved yet" (root CLAUDE.md,
/// "Definition of done — schema refs").
pub mod field {
    pub mod surface {
        pub const NAME: &str = "name";
        pub const VERSION: &str = "version";
        /// The row's integer revision: 1 on create, +1 on every update
        /// (AC-F22). What `surface_update`'s `expected_revision` is compared
        /// with. Distinct from the spec's own `surface: "1.0"`, which is the
        /// VOCABULARY version, and from `version`, which nothing writes.
        pub const REVISION: &str = "revision";
        pub const SPEC: &str = "spec";
        pub const TAGS: &str = "tags";
    }
    pub mod state {
        pub const SURFACE: &str = "surface";
        pub const HOST: &str = "host";
        pub const STATE: &str = "state";
        pub const CURSOR: &str = "cursor";
    }
    pub mod event {
        pub const SURFACE: &str = "surface";
        pub const HOST: &str = "host";
        pub const SEQ: &str = "seq";
        pub const NAME: &str = "name";
        pub const PAYLOAD: &str = "payload";
        pub const AT: &str = "at";
    }
}

/// See [`crate::Result`]: a stable `code` and a sentence.
pub type Result<T> = crate::Result<T>;

/// One `impress/ui/surface@1.0.0` row, spec included.
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceRow {
    pub id: ItemId,
    pub name: String,
    pub version: Option<String>,
    /// See [`field::surface::REVISION`]. A row written before revisions
    /// existed reads as 1.
    pub revision: u64,
    pub spec: SurfaceSpec,
    /// The spec exactly as stored — what a cached runtime compares to decide
    /// whether the row moved under it (RS-S1): equal text is an unchanged
    /// spec whoever wrote it, so there is no stamp to race on.
    pub spec_text: String,
    pub tags: Vec<String>,
    pub created: DateTime<Utc>,
    pub modified: DateTime<Utc>,
}

/// One `impress/ui/surface-event@1.0.0` row.
#[derive(Debug, Clone, PartialEq)]
pub struct EventRow {
    pub surface: ItemId,
    pub host: String,
    pub seq: u64,
    pub name: String,
    pub payload: Value,
    pub at: DateTime<Utc>,
    /// Who caused the event: the human clicking in the pane, or an agent
    /// dispatching over MCP/HTTP — the row's own `author_kind` (review
    /// SK-K5, AC-F5).
    pub actor: ActorKind,
}

/// Store-backed access to a surface's three record kinds.
///
/// `S` defaults to [`SqliteItemStore`]. Writes go through [`AttributedStore`],
/// the same port [`impress_layout_service::store::LayoutStore`] uses.
pub struct SurfaceStore<S: AttributedStore + ?Sized = dyn AttributedStore> {
    store: Arc<S>,
}

impl<S: AttributedStore + ?Sized> Clone for SurfaceStore<S> {
    fn clone(&self) -> Self {
        Self {
            store: Arc::clone(&self.store),
        }
    }
}

impl SurfaceStore<dyn AttributedStore> {
    /// Rows over any backend of the port. This is the only `new`, so
    /// `SurfaceStore::new(sqlite_arc)` is the default `SurfaceStore` and an
    /// `Arc<MemoryStore>` coerces at the call, the way
    /// `HashMap::new` fixes its hasher.
    pub fn new(store: Arc<dyn AttributedStore>) -> Self {
        Self { store }
    }
}

impl<S: AttributedStore + ?Sized> SurfaceStore<S> {
    /// Rows over a store of one named type — for a caller that wants the
    /// concrete handle back from [`Self::store`] rather than the port.
    pub fn over(store: Arc<S>) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &Arc<S> {
        &self.store
    }

    // ------------------------------------------------------------- surfaces

    /// Create a surface row. `name` overrides `spec.name` on the ROW's own
    /// `name` field when given; the spec's own `name` is untouched either
    /// way (it is the document's own title, the row's `name` is how
    /// `surface_list` labels it — the same distinction a saved layout's row
    /// `name` makes from nothing inside the tree it stores).
    pub fn create(
        &self,
        spec: &SurfaceSpec,
        name: Option<&str>,
        tags: &[String],
        actor: ActorKind,
    ) -> Result<SurfaceRow> {
        let name = name
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or(spec.name.trim());
        if name.is_empty() {
            return Err(Refusal::invalid_argument(
                "a surface needs a name (its own `name` field, or an override)",
            ));
        }
        let mut payload: BTreeMap<String, ItemValue> = BTreeMap::new();
        payload.insert(field::surface::NAME.into(), ItemValue::String(name.into()));
        payload.insert(field::surface::REVISION.into(), ItemValue::Int(1));
        payload.insert(
            field::surface::SPEC.into(),
            ItemValue::String(spec_json(spec)?),
        );
        if !tags.is_empty() {
            payload.insert(
                field::surface::TAGS.into(),
                ItemValue::Array(tags.iter().cloned().map(ItemValue::String).collect()),
            );
        }
        let now = Utc::now();
        let item = Item {
            id: uuid::Uuid::new_v4(),
            schema: SURFACE_SCHEMA_REF,
            payload,
            created: now,
            modified: now,
            author: author_for(actor),
            author_kind: actor,
            logical_clock: 0,
            origin: None,
            canonical_id: None,
            tags: vec![],
            flag: None,
            is_read: false,
            is_starred: false,
            priority: Priority::None,
            visibility: Visibility::Private,
            message_type: None,
            produced_by: None,
            version: None,
            batch_id: None,
            references: vec![],
            parent: None,
        };
        let id = self
            .store
            .insert(item)
            .map_err(|e| Refusal::store(format!("write surface: {e}")))?;
        self.get(id)?
            .ok_or_else(|| Refusal::store("surface vanished immediately after creation"))
    }

    /// Replace a surface's spec and bump its revision, in one store
    /// transaction, so a reader never sees the new spec under the old
    /// revision. `Durable` + `Editorial`: this is a commit, like
    /// `save_named` on a layout.
    ///
    /// The row's `name` is KEPT unless `name` is given (RS-S25): it may be a
    /// create-time override, which re-deriving it from `spec.name` silently
    /// threw away.
    ///
    /// # Atomic across processes (AC-F22)
    ///
    /// `expected_revision` is optimistic concurrency: when given and the row
    /// has moved past it, nothing is written and the error starts with
    /// `conflict:`. The write is conditional on the row's `logical_clock` as
    /// this call read it (`SqliteItemStore::apply_operations_if_clock`, the
    /// primitive the layout live row's compare-and-swap uses): the clock
    /// check and the spec, revision and name writes are one `BEGIN
    /// IMMEDIATE` transaction, so two writers — two processes on one store
    /// file, or two threads in one — that both read revision N cannot both
    /// write N + 1. The loser's write finds the clock moved, re-reads, and is
    /// refused `conflict` if it named a revision, or bumps from the new one
    /// if it did not (an update with no `expected_revision` is last writer
    /// wins, but never loses a revision number). A write that moved the row
    /// without changing its revision (a tag) is not a conflict: the re-read
    /// sees the same revision and the update goes ahead.
    pub fn update(
        &self,
        id: ItemId,
        spec: &SurfaceSpec,
        name: Option<&str>,
        expected_revision: Option<u64>,
        actor: ActorKind,
    ) -> Result<SurfaceRow> {
        // Each retry means another writer committed to this row between our
        // read and our write, so the bound is how many writers can race one
        // update, not a timeout.
        const MAX_ATTEMPTS: usize = 64;
        for _ in 0..MAX_ATTEMPTS {
            let existing = self
                .row_item(id)?
                .ok_or_else(|| Refusal::not_found(format!("no surface {id}")))?;
            let current = revision_of(&existing);
            if let Some(expected) = expected_revision {
                if expected != current {
                    return Err(Refusal::conflict(format!(
                        "conflict: surface {id} is at revision {current}, not {expected} — \
                         someone else updated it; read it again (surface_get) and apply your \
                         change to that"
                    )));
                }
            }
            let ops = self.update_ops(id, spec, name, current + 1, actor)?;
            match self
                .store
                .apply_operations_if_clock(ops, id, existing.logical_clock)
                .map_err(|e| Refusal::store(format!("write surface: {e}")))?
            {
                // The row as THIS write left it, not a re-read: a writer that
                // lands right after us must not hand our caller its revision
                // as if it were ours — the caller's next `expected_revision`
                // would then overwrite that write unseen.
                GuardedBatch::Applied { .. } => {
                    let written = current + 1;
                    if let Some(row) = self.get(id)?.filter(|r| r.revision == written) {
                        return Ok(row);
                    }
                    let mut row = surface_row_of(&existing)?;
                    row.revision = written;
                    row.spec = spec.clone();
                    row.spec_text = spec_json(spec)?;
                    if let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) {
                        row.name = name.to_string();
                    }
                    row.modified = Utc::now();
                    return Ok(row);
                }
                GuardedBatch::Moved { clock: None } => {
                    return Err(Refusal::not_found(format!("no surface {id}")));
                }
                GuardedBatch::Moved { clock: Some(_) } => continue,
            }
        }
        Err(Refusal::internal(format!(
            "update surface {id}: {MAX_ATTEMPTS} other writers changed it first"
        )))
    }

    /// The operations of one update: the spec, the revision, and the name
    /// when one is given.
    fn update_ops(
        &self,
        id: ItemId,
        spec: &SurfaceSpec,
        name: Option<&str>,
        revision: u64,
        actor: ActorKind,
    ) -> Result<Vec<OperationSpec>> {
        let mut ops = vec![
            self.op(
                id,
                field::surface::SPEC,
                ItemValue::String(spec_json(spec)?),
                actor,
                "updated the spec",
                Ephemerality::Commit,
            ),
            self.op(
                id,
                field::surface::REVISION,
                ItemValue::Int(revision as i64),
                actor,
                "bumped the revision",
                Ephemerality::Commit,
            ),
        ];
        if let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) {
            ops.push(self.op(
                id,
                field::surface::NAME,
                ItemValue::String(name.to_string()),
                actor,
                "renamed",
                Ephemerality::Commit,
            ));
        }
        Ok(ops)
    }

    pub fn get(&self, id: ItemId) -> Result<Option<SurfaceRow>> {
        let Some(item) = self.row_item(id)? else {
            return Ok(None);
        };
        Ok(Some(surface_row_of(&item)?))
    }

    pub fn list(&self) -> Result<Vec<SurfaceRow>> {
        let query = ItemQuery {
            schema: Some(SURFACE_SCHEMA_REF),
            include_tags: false,
            include_references: false,
            ..Default::default()
        };
        let mut rows: Vec<SurfaceRow> = self
            .store
            .query(&query)
            .map_err(|e| Refusal::store(format!("read surfaces: {e}")))?
            .iter()
            .map(surface_row_of)
            .collect::<Result<Vec<_>>>()?;
        rows.sort_by_key(|r| r.created);
        Ok(rows)
    }

    /// Delete a surface AND every state/event row that belongs to it — an
    /// orphaned state/event row (whose `surface` names a row that no longer
    /// exists) is exactly the ambiguity `impress/ui/surface-state`'s module
    /// docs warn a required-but-dangling foreign key produces, so this
    /// crate never leaves one behind.
    pub fn delete(&self, id: ItemId) -> Result<bool> {
        if self.row_item(id)?.is_none() {
            return Ok(false);
        }
        for state in self.all_state_rows(id)? {
            self.store
                .delete(state.id)
                .map_err(|e| Refusal::store(format!("delete surface state: {e}")))?;
        }
        for event in self.all_event_items(id)? {
            self.store
                .delete(event.id)
                .map_err(|e| Refusal::store(format!("delete surface event: {e}")))?;
        }
        self.store
            .delete(id)
            .map_err(|e| Refusal::store(format!("delete surface: {e}")))?;
        Ok(true)
    }

    fn row_item(&self, id: ItemId) -> Result<Option<Item>> {
        self.store
            .get(id)
            .map_err(|e| Refusal::store(format!("read surface {id}: {e}")))
            .map(|item| item.filter(|i| i.schema == SURFACE_SCHEMA_REF))
    }

    fn op(
        &self,
        id: ItemId,
        field: &str,
        value: ItemValue,
        actor: ActorKind,
        reason: &str,
        kind: Ephemerality,
    ) -> OperationSpec {
        OperationSpec {
            target_id: id,
            op_type: OperationType::SetPayload(field.to_string(), value),
            intent: kind.intent(),
            reason: Some(reason.to_string()),
            batch_id: None,
            author: author_for(actor),
            author_kind: actor,
            retention: kind.retention(),
        }
    }

    fn patch(
        &self,
        id: ItemId,
        field: &str,
        value: ItemValue,
        actor: ActorKind,
        reason: &str,
        kind: Ephemerality,
    ) -> Result<()> {
        self.store
            .apply_operation(self.op(id, field, value, actor, reason, kind))
            .map(|_| ())
            .map_err(|e| Refusal::store(format!("write surface: {e}")))
    }

    // --------------------------------------------------------------- state

    /// The working state of one `(surface, host)` instance, as raw JSON —
    /// `None` when nothing has been dispatched to this instance yet, which
    /// means "run with the spec's own initial `state` block" (the same
    /// absent-means-cold-start reading the layout store gives its live row).
    pub fn get_state(&self, surface: ItemId, host: &str) -> Result<Option<Value>> {
        self.get_state_text(surface, host)?
            .map(|text| parse_json(&text))
            .transpose()
    }

    /// The state row's `state` field exactly as stored — what a cached
    /// runtime compares with the text it last loaded or wrote (RS-S1).
    pub fn get_state_text(&self, surface: ItemId, host: &str) -> Result<Option<String>> {
        Ok(self.get_state_entry(surface, host)?.map(|(text, _)| text))
    }

    /// The state row's text and its revision (the row's logical clock, which
    /// every write moves) — what a runtime compares, and what a render or
    /// dispatch reports as `state_revision` (review SK-K15).
    pub fn get_state_entry(&self, surface: ItemId, host: &str) -> Result<Option<(String, u64)>> {
        Ok(self.state_item(surface, host)?.and_then(|item| {
            string_field(&item, field::state::STATE).map(|text| (text, item.logical_clock))
        }))
    }

    /// The state row's revision after a write: read back, since the store
    /// stamps it.
    fn state_revision_of(&self, surface: ItemId, host: &str) -> Result<u64> {
        Ok(self
            .state_item(surface, host)?
            .map(|item| item.logical_clock)
            .unwrap_or_default())
    }

    /// Upsert the state row for `(surface, host)`. `Ephemeral` + `Routine`:
    /// this is exploration churn, the same tier `save_live` gives a layout's
    /// drag.
    pub fn set_state(
        &self,
        surface: ItemId,
        host: &str,
        state: &Value,
        actor: ActorKind,
    ) -> Result<u64> {
        let text = serde_json::to_string(state)
            .map_err(|e| Refusal::store(format!("encode surface state: {e}")))?;
        self.set_state_text(surface, host, text, actor)
    }

    /// [`Self::set_state`] with the JSON already encoded — the runtime
    /// encodes once to compare and write. The FIRST write of an instance's
    /// row uses a deterministic id ([`state_row_id`]), so two writers racing
    /// to create it cannot leave two rows for one `(surface, host)`: the
    /// loser's insert is refused and it patches the winner's row instead.
    pub fn set_state_text(
        &self,
        surface: ItemId,
        host: &str,
        text: String,
        actor: ActorKind,
    ) -> Result<u64> {
        self.write_state_text(surface, host, text, actor)?;
        self.state_revision_of(surface, host)
    }

    fn write_state_text(
        &self,
        surface: ItemId,
        host: &str,
        text: String,
        actor: ActorKind,
    ) -> Result<()> {
        if let Some(item) = self.state_item(surface, host)? {
            return self.patch(
                item.id,
                field::state::STATE,
                ItemValue::String(text),
                actor,
                "surface state changed",
                Ephemerality::Exploration,
            );
        }
        let mut payload: BTreeMap<String, ItemValue> = BTreeMap::new();
        payload.insert(
            field::state::SURFACE.into(),
            ItemValue::String(surface.to_string()),
        );
        payload.insert(
            field::state::HOST.into(),
            ItemValue::String(host.to_string()),
        );
        payload.insert(field::state::STATE.into(), ItemValue::String(text.clone()));
        match self.try_insert_ephemeral(
            state_row_id(surface, host),
            SURFACE_STATE_SCHEMA_REF,
            payload,
            actor,
        ) {
            Ok(_) => Ok(()),
            Err(StoreError::AlreadyExists(id)) => self.patch(
                id,
                field::state::STATE,
                ItemValue::String(text),
                actor,
                "surface state changed",
                Ephemerality::Exploration,
            ),
            Err(e) => Err(Refusal::store(format!(
                "write {SURFACE_STATE_SCHEMA_REF}: {e}"
            ))),
        }
    }

    fn state_item(&self, surface: ItemId, host: &str) -> Result<Option<Item>> {
        let mut query = rare_rows(SURFACE_STATE_SCHEMA_REF, surface, Some(host));
        query.limit = Some(1);
        Ok(self
            .store
            .query(&query)
            .map_err(|e| Refusal::store(format!("read surface state: {e}")))?
            .into_iter()
            .next())
    }

    fn all_state_rows(&self, surface: ItemId) -> Result<Vec<Item>> {
        self.store
            .query(&rare_rows(SURFACE_STATE_SCHEMA_REF, surface, None))
            .map_err(|e| Refusal::store(format!("read surface state: {e}")))
    }

    // --------------------------------------------------------------- events

    /// Append one event, assigning the next `seq` for `(surface, host)`, and
    /// prune the ring to [`EVENT_RING_CAPACITY`] afterward. Returns the
    /// assigned `seq`.
    ///
    /// # Why two writers cannot share a `seq` (AC-F2, RS-S23)
    ///
    /// The row's id is DERIVED from `(surface, host, seq)` ([`event_row_id`]),
    /// and the store's primary key refuses a second row with the same id —
    /// in this process or any other on the same file. A writer that read
    /// the same maximum as another therefore fails its insert, reads the
    /// maximum again and takes the next number. `seq` stays unique and
    /// gap-free, which is what lets a reader treat a jump in it as pruning.
    pub fn append_event(
        &self,
        surface: ItemId,
        host: &str,
        name: &str,
        payload: &Value,
        actor: ActorKind,
    ) -> Result<u64> {
        let payload_text = serde_json::to_string(payload)
            .map_err(|e| Refusal::store(format!("encode event payload: {e}")))?;
        // Each retry means another writer took the number in between, so
        // this bound is the number of writers that can race one append, not
        // a timeout. Hitting it is a bug worth reporting, not retrying.
        const MAX_ATTEMPTS: usize = 64;
        for _ in 0..MAX_ATTEMPTS {
            let seq = self.max_seq(surface, host)? + 1;
            let mut fields: BTreeMap<String, ItemValue> = BTreeMap::new();
            fields.insert(
                field::event::SURFACE.into(),
                ItemValue::String(surface.to_string()),
            );
            fields.insert(
                field::event::HOST.into(),
                ItemValue::String(host.to_string()),
            );
            fields.insert(field::event::SEQ.into(), ItemValue::Int(seq as i64));
            fields.insert(
                field::event::NAME.into(),
                ItemValue::String(name.to_string()),
            );
            fields.insert(
                field::event::PAYLOAD.into(),
                ItemValue::String(payload_text.clone()),
            );
            fields.insert(
                field::event::AT.into(),
                ItemValue::String(Utc::now().to_rfc3339()),
            );
            match self.try_insert_ephemeral(
                event_row_id(surface, host, seq),
                SURFACE_EVENT_SCHEMA_REF,
                fields,
                actor,
            ) {
                Ok(_) => {
                    self.prune(surface, host, seq)?;
                    return Ok(seq);
                }
                Err(StoreError::AlreadyExists(_)) => continue,
                Err(e) => {
                    return Err(Refusal::store(format!(
                        "write {SURFACE_EVENT_SCHEMA_REF}: {e}"
                    )))
                }
            }
        }
        Err(Refusal::internal(format!(
            "append event '{name}': {MAX_ATTEMPTS} writers took the next seq first"
        )))
    }

    /// Events for `(surface, host)` with `seq > after`, oldest first, capped
    /// at `limit` (0 means unbounded). ONE store read, filtered and ordered
    /// by the store (RS-S14) — the cursor a caller hands back comes from
    /// these rows, never from a second read (AC-F2).
    pub fn events_after(
        &self,
        surface: ItemId,
        host: &str,
        after: u64,
        limit: usize,
    ) -> Result<Vec<EventRow>> {
        let mut query = rare_rows(SURFACE_EVENT_SCHEMA_REF, surface, Some(host));
        query.predicates.push(Predicate::Gt(
            field::event::SEQ.into(),
            ItemValue::Int(after.min(i64::MAX as u64) as i64),
        ));
        query.sort = vec![SortDescriptor {
            field: format!("payload.{}", field::event::SEQ),
            ascending: true,
        }];
        if limit > 0 {
            query.limit = Some(limit);
        }
        let rows = self
            .store
            .query(&query)
            .map_err(|e| Refusal::store(format!("read surface events: {e}")))?
            .iter()
            .map(event_row_of)
            .collect::<Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The highest `seq` this `(surface, host)` has ever emitted, 0 if none.
    /// Used to allocate the next one; a reader's cursor comes from the rows
    /// it read instead (see [`Self::events_after`]).
    pub fn max_seq(&self, surface: ItemId, host: &str) -> Result<u64> {
        let mut query = rare_rows(SURFACE_EVENT_SCHEMA_REF, surface, Some(host));
        query.sort = vec![SortDescriptor {
            field: format!("payload.{}", field::event::SEQ),
            ascending: false,
        }];
        query.limit = Some(1);
        Ok(self
            .store
            .query(&query)
            .map_err(|e| Refusal::store(format!("read surface events: {e}")))?
            .first()
            .and_then(|i| int_field(i, field::event::SEQ))
            .unwrap_or(0))
    }

    fn all_event_items(&self, surface: ItemId) -> Result<Vec<Item>> {
        self.store
            .query(&rare_rows(SURFACE_EVENT_SCHEMA_REF, surface, None))
            .map_err(|e| Refusal::store(format!("read surface events: {e}")))
    }

    /// Drop every row of `(surface, host)` that fell out of the last
    /// [`EVENT_RING_CAPACITY`] once `newest` was written (ADR-0033 D5). `seq`
    /// is gap-free (see [`Self::append_event`]), so "outside the ring" is a
    /// bound on `seq`, not a count of rows. Hard deletes, not a tombstone:
    /// this ring exists only so a live agent can read back what just
    /// happened, not to be an auditable log.
    fn prune(&self, surface: ItemId, host: &str, newest: u64) -> Result<()> {
        let capacity = EVENT_RING_CAPACITY as u64;
        if newest <= capacity {
            return Ok(());
        }
        let mut query = rare_rows(SURFACE_EVENT_SCHEMA_REF, surface, Some(host));
        query.predicates.push(Predicate::Lte(
            field::event::SEQ.into(),
            ItemValue::Int((newest - capacity) as i64),
        ));
        let stale = self
            .store
            .query(&query)
            .map_err(|e| Refusal::store(format!("read surface events: {e}")))?;
        for item in stale {
            self.store
                .delete(item.id)
                .map_err(|e| Refusal::store(format!("prune surface event: {e}")))?;
        }
        Ok(())
    }

    // ------------------------------------------------------------ internals

    /// A plain insert at `Private` visibility, under a caller-chosen id —
    /// the creation path every state/event row shares. Not an operation
    /// (there is nothing to revert: it is the row coming into being),
    /// matching the reasoning the layout store's module docs give for
    /// `insert_row`. The id is deterministic for both kinds (see
    /// [`state_row_id`], [`event_row_id`]) and the store error comes back
    /// typed, so a caller can tell "someone else already wrote this row"
    /// ([`StoreError::AlreadyExists`]) from a failure.
    fn try_insert_ephemeral(
        &self,
        id: ItemId,
        schema: impress_item::SchemaRef,
        payload: BTreeMap<String, ItemValue>,
        actor: ActorKind,
    ) -> std::result::Result<ItemId, StoreError> {
        let now = Utc::now();
        let item = Item {
            id,
            schema,
            payload,
            created: now,
            modified: now,
            author: author_for(actor),
            author_kind: actor,
            logical_clock: 0,
            origin: None,
            canonical_id: None,
            tags: vec![],
            flag: None,
            is_read: false,
            is_starred: false,
            priority: Priority::None,
            visibility: Visibility::Private,
            message_type: None,
            produced_by: None,
            version: None,
            batch_id: None,
            references: vec![],
            parent: None,
        };
        self.store.insert(item)
    }
}

/// Namespace for the derived row ids below. Fixed forever: changing it would
/// let a new build allocate an id an older build already used.
const ROW_ID_NAMESPACE: uuid::Uuid =
    uuid::Uuid::from_u128(0x6a1f_33c2_8d0e_4b5e_9c71_2f0d_5e8a_3b17);

/// The one id an `(surface, host)` state row can be created under.
pub fn state_row_id(surface: ItemId, host: &str) -> ItemId {
    uuid::Uuid::new_v5(
        &ROW_ID_NAMESPACE,
        format!("surface-state|{surface}|{host}").as_bytes(),
    )
}

/// The one id event `seq` of `(surface, host)` can be stored under — what
/// makes `seq` unique across writers (see `SurfaceStore::append_event`).
pub fn event_row_id(surface: ItemId, host: &str, seq: u64) -> ItemId {
    uuid::Uuid::new_v5(
        &ROW_ID_NAMESPACE,
        format!("surface-event|{surface}|{host}|{seq}").as_bytes(),
    )
}

/// A read of one surface's state or event rows, filtered BY THE STORE on the
/// payload's `surface` (and `host`) fields, with the rare-kind planner hint
/// (RS-S14). These kinds are a few hundred rows in a table of millions of
/// operation rows — exactly the shape `ItemQuery::assume_schema_rare`
/// exists for; without it, and with the filter done in Rust, every read was a
/// scan of every such row on the device.
fn rare_rows(schema: impress_item::SchemaRef, surface: ItemId, host: Option<&str>) -> ItemQuery {
    let mut predicates = vec![Predicate::Eq(
        field::event::SURFACE.into(),
        ItemValue::String(surface.to_string()),
    )];
    if let Some(host) = host {
        predicates.push(Predicate::Eq(
            field::event::HOST.into(),
            ItemValue::String(host.to_string()),
        ));
    }
    ItemQuery {
        schema: Some(schema),
        predicates,
        include_tags: false,
        include_references: false,
        assume_schema_rare: true,
        ..Default::default()
    }
}

fn revision_of(item: &Item) -> u64 {
    int_field(item, field::surface::REVISION).unwrap_or(1)
}

/// The author string this service writes with an operation
/// (`agent:surface-service`, or `IMPRESS_AUTHOR`). `Ephemerality`,
/// `actor_from` and the rule itself are layout-service's
/// (`impress_layout_service::authorship`, review RS-S21).
pub fn author_for(actor: ActorKind) -> String {
    author_for_service(actor, "surface-service")
}

fn string_field(item: &Item, field: &str) -> Option<String> {
    match item.payload.get(field) {
        Some(ItemValue::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn int_field(item: &Item, field: &str) -> Option<u64> {
    match item.payload.get(field) {
        Some(ItemValue::Int(n)) if *n >= 0 => Some(*n as u64),
        _ => None,
    }
}

fn spec_json(spec: &SurfaceSpec) -> Result<String> {
    serde_json::to_string(spec).map_err(|e| Refusal::store(format!("encode surface spec: {e}")))
}

fn parse_json(text: &str) -> Result<Value> {
    let json: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| Refusal::store(format!("parse surface JSON: {e}")))?;
    serde_json::from_value(json).map_err(|e| Refusal::store(format!("parse surface JSON: {e}")))
}

fn surface_row_of(item: &Item) -> Result<SurfaceRow> {
    let name = string_field(item, field::surface::NAME).unwrap_or_default();
    let version = string_field(item, field::surface::VERSION);
    let spec_text = item
        .payload
        .get(field::surface::SPEC)
        .and_then(|v| match v {
            ItemValue::String(s) => Some(s.clone()),
            _ => None,
        })
        .ok_or_else(|| Refusal::store(format!("surface {} has no `spec` field", item.id)))?;
    let spec: SurfaceSpec = serde_json::from_str(&spec_text)
        .map_err(|e| Refusal::store(format!("surface {} spec: {e}", item.id)))?;
    let tags = match item.payload.get(field::surface::TAGS) {
        Some(ItemValue::Array(items)) => items
            .iter()
            .filter_map(|v| match v {
                ItemValue::String(s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    Ok(SurfaceRow {
        id: item.id,
        name,
        version,
        revision: revision_of(item),
        spec,
        spec_text,
        tags,
        created: item.created,
        modified: item.modified,
    })
}

fn event_row_of(item: &Item) -> Result<EventRow> {
    let surface_text = string_field(item, field::event::SURFACE)
        .ok_or_else(|| Refusal::store(format!("event {} has no `surface` field", item.id)))?;
    let surface: ItemId = surface_text
        .parse()
        .map_err(|e| Refusal::store(format!("event {} surface id: {e}", item.id)))?;
    let host = string_field(item, field::event::HOST).unwrap_or_default();
    let seq = int_field(item, field::event::SEQ).unwrap_or(0);
    let name = string_field(item, field::event::NAME).unwrap_or_default();
    let payload = match item.payload.get(field::event::PAYLOAD) {
        Some(ItemValue::String(s)) => parse_json(s)?,
        _ => Value::Null,
    };
    let at = string_field(item, field::event::AT)
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or(item.modified);
    Ok(EventRow {
        surface,
        host,
        seq,
        name,
        payload,
        at,
        actor: item.author_kind,
    })
}
