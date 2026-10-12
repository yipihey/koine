//! The store side of the layout tree: `impress/ui/layout@1.0.0` rows
//! (ADR-0019 D1/D2, ADR-0031 D4).
//!
//! # The two kinds of row
//!
//! * **The live arrangement** — `is_live: true`, `name` absent, `device` set.
//!   Exactly one per `(app_id, device)` scope. This is what the window is
//!   currently in, and what the chassis restores at launch.
//! * **A named layout** — `is_live: false`, `name` set, `device` absent.
//!   Portable-durable: it follows the user to every device, which is why it
//!   carries no device tag and why its window geometry must be filtered out
//!   by the projection before it is applied elsewhere (ADR-0019 D2).
//!
//! # Attribution and retention — how, exactly
//!
//! Every write after the first goes through
//! [`SqliteItemStore::apply_operation`], so each one is an attributed
//! operation item carrying `author`, `author_kind`, an [`OperationIntent`] and
//! a `reason` — the sentence describing the gesture. **Retention is a property
//! of the operation row, not of the item row** (ADR-0006; the `items` table
//! has no retention column, `OperationSpec::retention` is where the tier
//! lives), so:
//!
//! * [`LayoutStore::save_live`] writes at [`RetentionTier::Ephemeral`] with
//!   [`OperationIntent::Routine`]. A drag emits many of these; they are
//!   compacted by the watermark snapshot and excluded from sync, per ADR-0019
//!   D6. **Coalescing is the caller's job** — the GUI debounces the splitter
//!   and re-binding gestures and calls this once on release; an agent calling
//!   one verb per intention needs no debounce.
//! * [`LayoutStore::save_named`] writes at [`RetentionTier::Durable`] with
//!   [`OperationIntent::Editorial`]. That is the commit (ADR-0031 D7): one
//!   durable, attributed operation with intent, which undo never crosses.
//!
//! The **create** of a row is a plain `insert`, because an insert is not an
//! operation and there is nowhere to hang a tier on it. It is still attributed
//! — `author` and `author_kind` are on the envelope — and it happens once per
//! scope, so the churn ADR-0019 D6 is about is entirely in the updates, which
//! are tiered correctly.
//!
//! # Revisions: the live row is compare-and-swap
//!
//! A [`crate::LayoutSession`] caches the live tree, and several writers hold
//! one on the same row — the app, `impress-mcp`, `impress-cli`, a second
//! chassis app on the same file. The row's `logical_clock` is its revision:
//! every operation and insert stamps it (ADR-0007 Phase 3), including writes
//! from builds that have never heard of this. [`LayoutStore::load_live`]
//! returns it, and [`LayoutStore::save_live_if`] writes only if the row is
//! still at the revision the session read — one `BEGIN IMMEDIATE`
//! transaction (`SqliteItemStore::apply_operation_if_clock`), so a stale
//! session can never write its tree back over someone else's change
//! (review RL-L1).
//!
//! # Startup
//!
//! Writes at launch are expected: the host's first snapshot cold-starts the
//! live row, and a first touch gives session-bearing panes their sessions.
//! What must not happen in the first ~90 s (root CLAUDE.md, ADR-0019 D6) is
//! *invalidation delivery* waking SwiftUI while the window settles, and that
//! rule is enforced where the invalidations are delivered — the FFI feed's
//! grace period (`SharedLayout::set_startup_grace_secs`) — not here.
//!
//! [`OperationIntent`]: impress_item::operation::OperationIntent
//! [`RetentionTier::Ephemeral`]: impress_item::operation::RetentionTier::Ephemeral
//! [`OperationIntent::Routine`]: impress_item::operation::OperationIntent::Routine
//! [`RetentionTier::Durable`]: impress_item::operation::RetentionTier::Durable
//! [`OperationIntent::Editorial`]: impress_item::operation::OperationIntent::Editorial

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use impress_item::item::{ActorKind, Item, ItemId, Priority, Value, Visibility};
use impress_item::operation::{OperationSpec, OperationType};
use impress_item::query::ItemQuery;
use impress_item::schema::refs::IMPRESS_UI_LAYOUT as LAYOUT_SCHEMA_REF;
use impress_item::store::{AttributedStore, GuardedWrite};
use impress_layout::{preset, Layout, ViewKindId};
use impress_pane_query::PaneQuery;
use impress_service_core::Refusal;

pub use crate::authorship::actor_from;
use crate::authorship::{author_for_service, Ephemerality};

/// Payload field names. Spelled once, here, for the same reason the schema ref
/// is: a reader that spells a field differently from its writer reads `None`
/// forever and looks exactly like "nothing saved yet".
pub mod field {
    pub const NAME: &str = "name";
    pub const PURPOSE: &str = "purpose";
    pub const LAYOUT: &str = "layout";
    pub const DEVICE: &str = "device";
    pub const APP_ID: &str = "app_id";
    pub const IS_LIVE: &str = "is_live";
    /// Set on a live row that no longer decoded and was set aside
    /// ([`super::LayoutStore::load_live`]): why, in the decoder's words.
    pub const QUARANTINED_REASON: &str = "quarantined_reason";
}

/// The record kind the cold-start live layout lists: imbib's publications.
/// A preset argument rather than a constant in `impress-layout` because "an
/// app preset is what the app is" (ADR-0031 D10) and the tree crate must stay
/// free of any app's vocabulary.
pub const DEFAULT_LIST_KIND: &str = "publication";

/// What went wrong, as a sentence. The services turn this into `ok: false` +
/// `message`, which is the shape every other `#[impress_service]` result in
/// the suite has.
/// A store failure is a `store-error` [`Refusal`]; an argument the store
/// cannot use (a nameless named layout) is `invalid-argument`.
pub type Result<T> = std::result::Result<T, Refusal>;

/// What [`LayoutStore::load_live`] found.
#[derive(Debug, Clone)]
pub struct LiveLoad {
    pub item_id: ItemId,
    /// The row's `logical_clock` when it was read.
    pub revision: u64,
    pub layout: Layout,
    /// Set when the load did something the caller should report: a live row
    /// that did not decode was quarantined.
    pub note: Option<String>,
}

/// What [`LayoutStore::save_live_if`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveWrite {
    /// Written; the row is now at `revision`.
    Written { revision: u64 },
    /// The row had moved (or is gone, `None`); nothing was written.
    Moved { revision: Option<u64> },
}

/// One `impress/ui/layout@1.0.0` row, without its tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutRow {
    pub id: ItemId,
    pub name: Option<String>,
    pub purpose: Option<String>,
    pub app_id: Option<String>,
    pub device: Option<String>,
    pub is_live: bool,
    pub created: DateTime<Utc>,
    pub modified: DateTime<Utc>,
}

/// Store-backed access to the layout rows.
///
/// `S` defaults to [`SqliteItemStore`]. The methods below call
/// [`AttributedStore`], so a test or a future backend can host the same rows
/// without this service naming sqlite (`docs/plan-koine-separation.md`, K1).
pub struct LayoutStore<S: AttributedStore + ?Sized = dyn AttributedStore> {
    store: Arc<S>,
}

impl<S: AttributedStore + ?Sized> Clone for LayoutStore<S> {
    fn clone(&self) -> Self {
        Self {
            store: Arc::clone(&self.store),
        }
    }
}

impl LayoutStore<dyn AttributedStore> {
    /// Rows over any backend of the port. This is the only `new`, so
    /// `LayoutStore::new(sqlite_arc)` is the default `LayoutStore` and an
    /// `Arc<SqliteItemStore>` coerces at the call, the way
    /// `HashMap::new` fixes its hasher.
    pub fn new(store: Arc<dyn AttributedStore>) -> Self {
        Self { store }
    }
}

impl<S: AttributedStore + ?Sized> LayoutStore<S> {
    /// Rows over a store of one named type — for a caller that wants the
    /// concrete handle back from [`Self::store`] rather than the port.
    pub fn over(store: Arc<S>) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &Arc<S> {
        &self.store
    }

    // ------------------------------------------------------------ the live row

    /// The live arrangement for `(app_id, device)`, creating it on a cold
    /// start, with the row's revision.
    ///
    /// "Cold start" is the path that matters: with no row for this scope, the
    /// three-column preset is built (navigator | list | detail, the chassis as
    /// a value — ADR-0031 D10) and **persisted**, so the very first read
    /// leaves the store in the state every later read expects. A caller never
    /// has to ask whether a layout exists.
    ///
    /// A live row that no longer decodes (a hand edit, a sync merge, a future
    /// rename) is **quarantined, never deleted** (review RL-L17): it stops
    /// being live, is renamed "Unreadable layout <time>" so it shows in the
    /// saved-layouts list where the user can inspect or delete it, carries
    /// the decoder's error in `quarantined_reason`, and a fresh preset takes
    /// its place. Before, every snapshot, verb and read of the scope failed
    /// with "read layout tree: …" for good. [`LiveLoad::note`] says what
    /// happened, for the caller to report.
    pub fn load_live(&self, app_id: &str, device: &str, actor: ActorKind) -> Result<LiveLoad> {
        let mut note = None;
        if let Some(item) = self.live_item(app_id, device)? {
            match layout_of(&item) {
                Ok(layout) => {
                    return Ok(LiveLoad {
                        item_id: item.id,
                        revision: item.logical_clock,
                        layout,
                        note: None,
                    });
                }
                Err(error) => {
                    let name = self.quarantine(&item, &error.message, actor)?;
                    tracing::error!(
                        target: "layout",
                        "live layout row {} ({app_id}/{device}) does not decode: {error} — \
                         kept as the saved layout '{name}' and replaced by a fresh preset",
                        item.id
                    );
                    note = Some(format!(
                        "The live layout could not be read ({error}); it was kept as the saved \
                         layout '{name}' and a fresh arrangement was started."
                    ));
                }
            }
        }
        let layout = cold_start_layout();
        let id = self.insert_row(
            app_id,
            Some(device),
            None,
            None,
            true,
            &layout,
            actor,
            "cold start: the three-column preset",
        )?;
        let revision = self
            .store
            .logical_clock_of(id)
            .map_err(|e| Refusal::store(format!("read layout {id}: {e}")))?
            .ok_or_else(|| Refusal::store("the cold-started layout vanished"))?;
        tracing::info!(
            target: "layout",
            "{app_id}/{device}: no live layout row; cold-started one ({id}) as {actor:?}"
        );
        Ok(LiveLoad {
            item_id: id,
            revision,
            layout,
            note,
        })
    }

    /// Set an undecodable live row aside: not live any more, named so it
    /// lists among the saved layouts, and carrying the reason. Returns the
    /// name it was given.
    fn quarantine(&self, item: &Item, error: &str, actor: ActorKind) -> Result<String> {
        let name = format!(
            "Unreadable layout {}",
            Utc::now().format("%Y-%m-%d %H:%M:%S")
        );
        let intent = "set aside a live layout that no longer decodes";
        for (field, value) in [
            (field::IS_LIVE, Value::Bool(false)),
            (field::NAME, Value::String(name.clone())),
            (field::QUARANTINED_REASON, Value::String(error.to_string())),
        ] {
            self.patch_field(item.id, field, value, actor, intent, Ephemerality::Commit)?;
        }
        Ok(name)
    }

    /// Write the live tree only if the row is still at `revision` — see the
    /// module docs. `Moved` means another writer got there first and nothing
    /// was written; the caller reloads.
    pub fn save_live_if(
        &self,
        item_id: ItemId,
        revision: u64,
        layout: &Layout,
        actor: ActorKind,
        intent: &str,
    ) -> Result<LiveWrite> {
        let spec = OperationSpec {
            target_id: item_id,
            op_type: OperationType::SetPayload(field::LAYOUT.to_string(), layout_value(layout)?),
            intent: Ephemerality::Exploration.intent(),
            reason: Some(intent.to_string()),
            batch_id: None,
            author: author_for(actor),
            author_kind: actor,
            retention: Ephemerality::Exploration.retention(),
        };
        match self
            .store
            .apply_operation_if_clock(spec, revision)
            .map_err(|e| Refusal::store(format!("write layout: {e}")))?
        {
            GuardedWrite::Applied { clock, .. } => Ok(LiveWrite::Written { revision: clock }),
            GuardedWrite::Moved { clock } => Ok(LiveWrite::Moved { revision: clock }),
        }
    }

    /// The live row's current revision, `None` when it is gone. One indexed
    /// lookup — what a session checks before every use.
    pub fn revision_of(&self, item_id: ItemId) -> Result<Option<u64>> {
        self.store
            .logical_clock_of(item_id)
            .map_err(|e| Refusal::store(format!("read layout {item_id}: {e}")))
    }

    /// Persist the live arrangement. Ephemeral retention; see the module docs.
    ///
    /// The **one live row per scope** invariant is held here rather than
    /// asserted about: an existing row is patched in place and only its
    /// absence inserts, so calling this a hundred times during a drag leaves
    /// one row and a hundred compactable operations.
    pub fn save_live(
        &self,
        app_id: &str,
        device: &str,
        layout: &Layout,
        actor: ActorKind,
        intent: &str,
    ) -> Result<ItemId> {
        match self.live_row(app_id, device)? {
            Some((row, _)) => {
                self.patch_layout(row.id, layout, actor, intent, Ephemerality::Exploration)?;
                Ok(row.id)
            }
            None => self.insert_row(
                app_id,
                Some(device),
                None,
                None,
                true,
                layout,
                actor,
                intent,
            ),
        }
    }

    /// The live row and its tree, if this scope has one.
    pub fn live_row(&self, app_id: &str, device: &str) -> Result<Option<(LayoutRow, Layout)>> {
        match self.live_item(app_id, device)? {
            Some(item) => Ok(Some((row_of(&item), layout_of(&item)?))),
            None => Ok(None),
        }
    }

    /// The live row itself, undecoded.
    fn live_item(&self, app_id: &str, device: &str) -> Result<Option<Item>> {
        let mut live: Vec<Item> = self
            .rows(app_id)?
            .into_iter()
            .filter(|item| {
                is_live(item) && string_field(item, field::DEVICE).as_deref() == Some(device)
            })
            .collect();
        // Newest wins. Two rows claiming to be live for one scope is the bug
        // `is_live` exists to make visible (schemas/ui.rs); it cannot be
        // produced by this crate, but a sync merge or a hand-edit can, and
        // answering with the newest is the only reading that does not lose
        // the user's most recent arrangement.
        live.sort_by_key(|item| item.modified);
        Ok(live.pop())
    }

    // ----------------------------------------------------------- named layouts

    /// Every named layout of `app_id`, oldest first — the tail of the
    /// ⌃⌘1–9 union ([`crate::presets::ordinal_targets`]), whose head is the
    /// app's presets. A layout keeps its chord when one is renamed or
    /// re-saved, and gains one only by being saved after the others.
    ///
    /// Ordered by **creation**, then by name. Creation rather than
    /// modification because re-saving Triage must not move it to slot 9; the
    /// name tiebreak because `created` has millisecond resolution and two
    /// layouts saved in one millisecond would otherwise be ordered by a
    /// random UUID — an ordinal that changes between runs is worse than one
    /// that is alphabetical for a tie.
    pub fn list_named(&self, app_id: &str) -> Result<Vec<LayoutRow>> {
        let mut rows: Vec<LayoutRow> = self
            .rows(app_id)?
            .iter()
            .filter(|item| !is_live(item))
            .map(row_of)
            .collect();
        rows.sort_by(|a, b| {
            a.created
                .cmp(&b.created)
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(rows)
    }

    /// Every layout row of `app_id`, live and named alike. Diagnostics and
    /// the "one live row per scope" check. (L7 planned an ADR-0019 D5
    /// importer of imbib's `PaneLayoutState` layouts that would also read
    /// these; it was never built, and plan wave 6 W5 dropped it.)
    pub fn all_rows(&self, app_id: &str) -> Result<Vec<LayoutRow>> {
        Ok(self.rows(app_id)?.iter().map(row_of).collect())
    }

    // There is deliberately no `ordinal()` here any more. ⌃⌘`n` spans the
    // app's PRESETS and then its named layouts (ADR-0031 D10, L7), and that
    // union is spelled once, in [`crate::presets::ordinal_targets`]. A second
    // 1-based index over `list_named` alone would be a second answer to "what
    // does ⌃⌘2 recall?" — and two answers to one question is how a chord ends
    // up meaning different things in the menu and in the keymap.

    /// Save (or overwrite) a named layout. **Durable** — this is the commit.
    ///
    /// The tree is stored with its window geometry intact; filtering geometry
    /// out is the *applying* side's job (ADR-0019 D2), because the row has to
    /// be able to come home to the device that wrote it.
    pub fn save_named(
        &self,
        app_id: &str,
        name: &str,
        purpose: Option<&str>,
        layout: &Layout,
        actor: ActorKind,
        intent: &str,
    ) -> Result<LayoutRow> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Refusal::invalid_argument("a named layout needs a name"));
        }
        match self.named_row(app_id, name)? {
            Some((row, _)) => {
                self.patch_layout(row.id, layout, actor, intent, Ephemerality::Commit)?;
                if let Some(purpose) = purpose {
                    self.patch_field(
                        row.id,
                        field::PURPOSE,
                        Value::String(purpose.to_string()),
                        actor,
                        intent,
                        Ephemerality::Commit,
                    )?;
                }
                Ok(self.row(row.id)?.ok_or("saved layout vanished mid-save")?)
            }
            None => {
                let id = self.insert_row(
                    app_id,
                    None,
                    Some(name),
                    purpose,
                    false,
                    layout,
                    actor,
                    intent,
                )?;
                Ok(self.row(id)?.ok_or("saved layout vanished mid-save")?)
            }
        }
    }

    /// A named layout by name (case-insensitive) or by id.
    pub fn load_named(&self, app_id: &str, key: &str) -> Result<Option<(LayoutRow, Layout)>> {
        if let Ok(id) = key.trim().parse::<ItemId>() {
            if let Some(item) = self.item(id)? {
                if !is_live(&item) {
                    return Ok(Some((row_of(&item), layout_of(&item)?)));
                }
            }
        }
        self.named_row(app_id, key)
    }

    /// Delete a named layout. The live row is refused: it is not a saved thing
    /// the user can throw away, it is where they currently are.
    pub fn delete_named(&self, app_id: &str, key: &str) -> Result<bool> {
        let Some((row, _)) = self.load_named(app_id, key)? else {
            return Ok(false);
        };
        self.store
            .delete(row.id)
            .map_err(|e| Refusal::store(format!("delete layout: {e}")))?;
        Ok(true)
    }

    // ------------------------------------------------------------------ internals

    fn named_row(&self, app_id: &str, name: &str) -> Result<Option<(LayoutRow, Layout)>> {
        let wanted = name.trim().to_lowercase();
        for item in self.rows(app_id)? {
            if is_live(&item) {
                continue;
            }
            let matches = string_field(&item, field::NAME)
                .map(|n| n.trim().to_lowercase() == wanted)
                .unwrap_or(false);
            if matches {
                let layout = layout_of(&item)?;
                return Ok(Some((row_of(&item), layout)));
            }
        }
        Ok(None)
    }

    fn row(&self, id: ItemId) -> Result<Option<LayoutRow>> {
        Ok(self.item(id)?.map(|item| row_of(&item)))
    }

    fn item(&self, id: ItemId) -> Result<Option<Item>> {
        self.store
            .get(id)
            .map_err(|e| Refusal::store(format!("read layout {id}: {e}")))
            .map(|item| item.filter(|i| i.schema == LAYOUT_SCHEMA_REF))
    }

    /// Every layout row of `app_id`.
    ///
    /// The `app_id` / `is_live` / `device` filtering happens in Rust rather
    /// than in predicates on purpose: a workspace holds a handful of these
    /// rows, and matching a JSON boolean through `json_extract` is the kind of
    /// silently-empty comparison this repo has a lint about.
    fn rows(&self, app_id: &str) -> Result<Vec<Item>> {
        let query = ItemQuery {
            schema: Some(LAYOUT_SCHEMA_REF),
            include_tags: false,
            include_references: false,
            ..Default::default()
        };
        let app_id = app_id.trim();
        Ok(self
            .store
            .query(&query)
            .map_err(|e| Refusal::store(format!("read layouts: {e}")))?
            .into_iter()
            .filter(|item| match string_field(item, field::APP_ID) {
                Some(id) => id == app_id,
                // A row with no app is offered to every app: the five binaries
                // are a distribution decision, not a chassis one (D10).
                None => true,
            })
            .collect())
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_row(
        &self,
        app_id: &str,
        device: Option<&str>,
        name: Option<&str>,
        purpose: Option<&str>,
        is_live_row: bool,
        layout: &Layout,
        actor: ActorKind,
        intent: &str,
    ) -> Result<ItemId> {
        let mut payload: BTreeMap<String, Value> = BTreeMap::new();
        payload.insert(field::LAYOUT.into(), layout_value(layout)?);
        payload.insert(field::IS_LIVE.into(), Value::Bool(is_live_row));
        payload.insert(field::APP_ID.into(), Value::String(app_id.trim().into()));
        if let Some(device) = device {
            payload.insert(field::DEVICE.into(), Value::String(device.into()));
        }
        if let Some(name) = name {
            payload.insert(field::NAME.into(), Value::String(name.trim().into()));
        }
        if let Some(purpose) = purpose {
            payload.insert(field::PURPOSE.into(), Value::String(purpose.trim().into()));
        }

        let now = Utc::now();
        let item = Item {
            id: uuid::Uuid::new_v4(),
            schema: LAYOUT_SCHEMA_REF,
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
            // Private per ADR-0019 D2 for every scope in the table.
            visibility: Visibility::Private,
            message_type: None,
            produced_by: None,
            version: None,
            batch_id: None,
            references: vec![],
            parent: None,
        };
        // An insert is not an operation, so there is no tier to set on it; see
        // the module docs. `intent` is carried for the caller's log line.
        let _ = intent;
        self.store
            .insert(item)
            .map_err(|e| Refusal::store(format!("write layout: {e}")))
    }

    fn patch_layout(
        &self,
        id: ItemId,
        layout: &Layout,
        actor: ActorKind,
        intent: &str,
        kind: Ephemerality,
    ) -> Result<()> {
        let value = layout_value(layout)?;
        self.patch_field(id, field::LAYOUT, value, actor, intent, kind)
    }

    fn patch_field(
        &self,
        id: ItemId,
        field: &str,
        value: Value,
        actor: ActorKind,
        intent: &str,
        kind: Ephemerality,
    ) -> Result<()> {
        self.store
            .apply_operation(OperationSpec {
                target_id: id,
                op_type: OperationType::SetPayload(field.to_string(), value),
                intent: kind.intent(),
                reason: Some(intent.to_string()),
                batch_id: None,
                author: author_for(actor),
                author_kind: actor,
                retention: kind.retention(),
            })
            .map(|_| ())
            .map_err(|e| Refusal::store(format!("write layout: {e}")))
    }
}

/// The three-column chassis as a value — what a cold start creates.
pub fn cold_start_layout() -> Layout {
    preset::three_column(default_list_query(), ViewKindId::INFO)
}

/// The list query the cold-start preset shows.
pub fn default_list_query() -> PaneQuery {
    PaneQuery {
        kinds: vec![DEFAULT_LIST_KIND.to_string()],
        ..PaneQuery::default()
    }
}

/// The author string this service writes with an operation
/// (`human:layout-service`, or `IMPRESS_AUTHOR`); see
/// [`crate::authorship::author_for_service`].
pub fn author_for(actor: ActorKind) -> String {
    author_for_service(actor, "layout-service")
}

fn is_live(item: &Item) -> bool {
    // "Readers should still treat absent as false rather than erroring on a
    // hand-edited row" — schemas/ui.rs, `is_live`.
    matches!(item.payload.get(field::IS_LIVE), Some(Value::Bool(true)))
}

fn string_field(item: &Item, field: &str) -> Option<String> {
    match item.payload.get(field) {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn row_of(item: &Item) -> LayoutRow {
    LayoutRow {
        id: item.id,
        name: string_field(item, field::NAME),
        purpose: string_field(item, field::PURPOSE),
        app_id: string_field(item, field::APP_ID),
        device: string_field(item, field::DEVICE),
        is_live: is_live(item),
        created: item.created,
        modified: item.modified,
    }
}

/// The tree out of a row's `layout` field.
pub fn layout_of(item: &Item) -> Result<Layout> {
    let Some(value) = item.payload.get(field::LAYOUT) else {
        return Err(Refusal::store(format!(
            "layout row {} has no `layout` field",
            item.id
        )));
    };
    let json = serde_json::to_value(value)
        .map_err(|e| Refusal::store(format!("read layout tree: {e}")))?;
    serde_json::from_value(json).map_err(|e| Refusal::store(format!("read layout tree: {e}")))
}

/// `pub(crate)`: [`crate::rename`]'s pass reuses this instead of a second
/// encoder for the same tree, on both a layout row and a preset row (they
/// store their tree under the same `layout` field).
pub(crate) fn layout_value(layout: &Layout) -> Result<Value> {
    let json = serde_json::to_value(layout)
        .map_err(|e| Refusal::store(format!("encode layout tree: {e}")))?;
    serde_json::from_value(json).map_err(|e| Refusal::store(format!("encode layout tree: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use impress_item::item::ActorKind;
    use impress_item::store::AttributedStore;

    /// The layout rows are generic over [`AttributedStore`]. `SqliteItemStore`
    /// is one implementation; this is the call a second backend would make.
    #[test]
    fn layout_store_speaks_the_attributed_port() {
        fn host<S: AttributedStore>(store: Arc<S>) -> LayoutStore<S> {
            LayoutStore::over(store)
        }

        let store = Arc::new(koine_store_memory::MemoryStore::new());
        let layouts = host(store);
        let loaded = layouts
            .load_live("koine", "test-device", ActorKind::System)
            .expect("cold start");
        assert!(loaded.revision >= 1);
        assert!(loaded.note.is_none());
        let again = layouts
            .load_live("koine", "test-device", ActorKind::System)
            .expect("reload");
        assert_eq!(again.item_id, loaded.item_id);
    }
}
