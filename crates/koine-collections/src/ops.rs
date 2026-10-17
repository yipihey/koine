//! The collection kernel (ADR-0022 D1/D2).
//!
//! One implementation of the collection verb set — `list_tree`,
//! `collections_containing`, `create`, `rename`, `reparent`, `reorder`,
//! `delete`, `add_members`, `remove_members`, `member_counts` — parameterized by a
//! [`CollectionSchemaBinding`] so it can front the schemas that already exist
//! (`imbib/collection`, `manuscript-collection`, `figure-collection`) as well
//! as the generic `collection@1.0.0` schema. D2 unifies the API first; the
//! data migration onto one schema is a separate work package.
//!
//! This lives in impress-core — next to `manuscript_ops` — so both FFI
//! surfaces (`impress-store-ffi::SharedStore` and `imbib-core::ImbibStore`)
//! and the future `#[impress_service]` trait share one implementation
//! instead of drifting. The reparent cycle check in particular used to live
//! only in Swift; it is Rust's now.
//!
//! Every UUID that crosses this API is normalized to lowercase: ids are
//! parsed into `Uuid` on the way in and rendered with `to_string()` on the
//! way out. Payload parent refs are matched by string equality downstream
//! (the imbib "manuscript UUID strings crossing the FFI must be lowercase"
//! invariant), so this layer is where the normalization happens.
//!
//! # Undo contract (ADR-0022 G2)
//!
//! Every mutating verb returns what an undo stack needs to invert it, so a
//! caller can register an exact inverse without re-reading the store first
//! (which races, and which is what the Swift adapters used to do by hand):
//!
//! | verb             | returns                   | undo by calling                                   |
//! |------------------|---------------------------|---------------------------------------------------|
//! | `create`         | [`CollectionRow`]         | `delete(row.id)`                                  |
//! | `create_in`      | [`CollectionRow`]         | `delete(row.id)`                                  |
//! | `create_in_with_payload` | [`CollectionRow`] | `delete(row.id)`                                  |
//! | `rename`         | [`CollectionMutation`]    | `rename(id, prior.name())`                        |
//! | `reorder`        | [`CollectionMutation`]    | `reorder(id, prior.sort_order())`                 |
//! | `reparent`       | [`CollectionMutation`]    | `reparent(id, prior.parent_id())` (`None` = root) |
//! | `reparent_in`    | [`CollectionMutation`]    | `reparent_in(id, prior.parent_id(), prior.container_id())` |
//! | `delete`         | [`DeletedCollection`]     | `restore(snapshot)`                               |
//! | `restore`        | [`CollectionRow`]         | `delete(row.id)`                                  |
//! | `add_members`    | changed ids               | `remove_members(collection, changed)`             |
//! | `remove_members` | changed ids               | `add_members(collection, changed)`                |
//!
//! The membership verbs return only the ids they actually changed — never the
//! ids the caller asked for — so the inverse cannot re-file an item that was
//! already a member (or unfile one that never was).
//!
//! # The optional axes (ADR-0022 C2)
//!
//! Three axes are `Option`al on [`CollectionSchemaBinding`] because exactly one
//! shipped binding needs each, and a binding that declines one behaves precisely
//! as it did before the axis existed:
//!
//! | axis | declared by | what it buys | `None` means |
//! |---|---|---|---|
//! | [`ContainerField`] (`container_field`) | `IMBIB_COLLECTION` | per-container creation ([`create_in`]), per-container listing ([`list_tree_in`]), atomic cross-container reparent ([`reparent_in`]), and a `container_id` on every row | collections are global; every container argument is ignored |
//! | `smart_field` | `IMBIB_COLLECTION`, `MANUSCRIPT_COLLECTION`, `GENERIC_COLLECTION` | [`CollectionRow::is_smart`], the per-row read-only predicate a sidebar gates Rename / New Subcollection on | rows report `is_smart: false` |
//! | `kind_scope_field` (pre-existing) | `GENERIC_COLLECTION` | per-row record-kind scope | the binding's scope is a constant it already knows |
//!
//! The container axis is the one axis the WP G7 flip provably cannot disturb:
//! the migration rewrites `schema_ref` and the payload and never touches the
//! envelope, so a container read or write is byte-identical on both sides.
//! That is what makes [`list_tree_in`] a drop-in for imbib-core's
//! `list_collections(library_id)`, which is blind after the flip.
//!
//! **Not an axis: "ensure container membership".** The wave-3 routing survey
//! listed `ImbibStore::add_to_collection` as also ensuring the publication is a
//! member of the collection's library, which would have made membership a
//! two-write verb for one binding. It does not — the export is thirty lines of
//! `AddReference(Contains)` and nothing else (`imbib-core/unified/store_api.rs`),
//! and the claim came from a Swift call-site COMMENT, not from the code. So
//! [`add_members`] is already faithful for publication collections and no hook
//! was added: a seam nothing needs is a seam that rots.
//!
//! # Dual mode (ADR-0022 WP G7)
//!
//! Once `collection_migration` (impress-core) has converged the store's data onto
//! `collection@1.0.0`, the three legacy bindings no longer have rows of their
//! own schema to find. Every verb therefore starts by RESOLVING its binding
//! against the store's `collections.unified` marker
//! ([`CollectionSchemaBinding::resolved`]):
//!
//! | binding | marker off → query | marker on → query | membership (both) |
//! |---|---|---|---|
//! | `IMBIB_COLLECTION` | `schema_ref = imbib/collection`, tree = payload `parent_id` | `schema_ref = collection` + `kind_scope = publication`, tree = payload `parent_id` | `Contains` edge |
//! | `MANUSCRIPT_COLLECTION` | `schema_ref = manuscript-collection`, tree = payload `parent_collection_ref` | `schema_ref = collection` + `kind_scope = manuscript`, tree = payload `parent_id` | `Contains` edge |
//! | `FIGURE_COLLECTION` | `schema_ref = figure-collection`, tree = envelope `item.parent` | `schema_ref = collection` + `kind_scope = figure`, tree = payload `parent_id` | envelope `item.parent` |
//! | `GENERIC_COLLECTION` | `schema_ref = collection`, tree = payload `parent_id` | unchanged (it is the destination) | `Contains` edge |
//!
//! Two invariants make the flip invisible to callers:
//!
//! - **Membership never moves.** A migrated figure folder still collects its
//!   figures through `item.parent`, because the migration did not change ids
//!   and did not touch the envelope. `Contains` edges likewise survive as-is.
//! - **`CollectionRow.kind_scope` stays `None` for the legacy bindings.** Their
//!   scope is a constant the binding already knows; surfacing it post-flip
//!   would change the row shape every FFI caller sees for no information gain.
//!   Only `GENERIC_COLLECTION`, whose scope is genuinely per-row, reports it.
//!
//! With the marker off, every path below is byte-identical to its pre-G7 self.

use std::collections::BTreeMap;

use chrono::Utc;
use uuid::Uuid;

use impress_item::{
    AttributedStore, EdgeType, FieldMutation, Item, ItemId, ItemQuery, Predicate, Priority,
    SchemaRef, SortDescriptor, StoreError, TypedReference, Value, Visibility,
};

/// `kind_scope` value for a collection that accepts every record kind.
pub const KIND_SCOPE_ANY: &str = "any";

/// The store-metadata key whose value [`MARKER_VALUE`] says the store's
/// collections are `collection@1.0.0` (ADR-0022 WP G7).
pub const MARKER_KEY: &str = "collections.unified";

/// The only value [`MARKER_KEY`] is ever written with.
pub const MARKER_VALUE: &str = "1";

/// Is this store's collection data converged on `collection@1.0.0`?
///
/// One metadata read through the port, cheap enough that every verb asks it
/// once per operation rather than caching it, which is what keeps a migrate or
/// rollback visible immediately to every live store handle.
pub fn is_migrated_on(store: &dyn AttributedStore) -> Result<bool, StoreError> {
    Ok(store
        .metadata_get(MARKER_KEY)?
        .is_some_and(|v| v == MARKER_VALUE))
}

/// Hard ceiling on parent-chain walks. A tree deeper than this is a data bug
/// (or a pre-existing cycle written before the check moved into Rust), and
/// either way must not spin forever.
const MAX_TREE_DEPTH: usize = 256;

/// Where a binding keeps the collection *tree* parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentField {
    /// A payload string field holding the parent collection's UUID.
    ///
    /// The tree parent is NEVER the envelope `item.parent`: for imbib and
    /// imprint collections that field is the owning LIBRARY, which is what
    /// `list_collections` filters on. c902a22f returned it as the tree parent
    /// and flattened the whole sidebar (see the invariant in
    /// `apps/imbib/CLAUDE.md`).
    Payload(&'static str),
    /// The envelope `item.parent` chain. implore's figure folders have no
    /// payload parent field — folders nest and figures are filed through
    /// `set_parent` (`ImploreStoreAdapter`, `FigureStoreReader`).
    Envelope,
}

/// How a binding records membership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Membership {
    /// A `Contains` edge from the collection to the member (imbib, imprint,
    /// generic). Deleting the collection drops the edges by FK cascade.
    ContainsEdge,
    /// The member's envelope `item.parent` points at the collection (figure
    /// folders). Deleting the collection unfiles its members (FK SET NULL).
    EnvelopeParent,
}

/// Where a binding records a collection's OWNING CONTAINER (ADR-0022 C2).
///
/// The container is what a collection belongs **to**; the tree parent
/// ([`ParentField`]) is what it nests **under**. imbib is the binding that
/// forced this to be declared: a publication collection carries
/// (collectionID, libraryID), drop acceptance requires the SAME library, a
/// cross-library reparent is TWO writes, and creation is per-library — none of
/// which the kernel could say before.
///
/// A binding with `None` has no container axis at all: manuscript folders and
/// figure folders are global, and every container-taking verb below degrades to
/// its historical, container-free behaviour for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerField {
    /// The envelope `item.parent`. For imbib publication collections that field
    /// IS the owning library — which is precisely what `list_collections`
    /// filters on (`HasParent`) and what the c902a22f postmortem is about: the
    /// envelope is the CONTAINER, never the tree parent.
    ///
    /// Migration-invariant on purpose. WP G7 rewrites `schema_ref` and the
    /// payload and does not touch the envelope, so container reads and writes
    /// are byte-identical on both sides of the `collections.unified` flip.
    Envelope,
}

/// What a binding writes to a collection row's envelope `item.parent`.
///
/// Separate from [`ParentField`] because the two answer different questions:
/// `ParentField` says where the TREE parent is READ from, this says what the
/// envelope is FOR. They coincide only for figure folders — and after WP G7's
/// migration even those read their tree parent from the payload while their
/// envelope keeps pointing at the folder above, which is what keeps
/// envelope-filed figure membership working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeParent {
    /// The OWNING LIBRARY. A new row inherits its parent collection's envelope
    /// parent, so it stays visible to the legacy `HasParent` library listings.
    OwningLibrary,
    /// The TREE PARENT itself (figure folders). The kernel keeps the envelope
    /// in step with the tree parent whichever field the tree parent is read
    /// from.
    TreeParent,
}

// Borrow stable manifest values so the small binding descriptors remain Copy.
static GENERIC_COLLECTION_SCHEMA: SchemaRef = impress_item::schema::refs::COLLECTION;

/// Descriptor binding the kernel to one concrete collection schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectionSchemaBinding {
    /// Exact `schema_ref` stored on collection items (queries match it with
    /// string equality, so this is the stored form, not the display form).
    pub schema_ref: &'static SchemaRef,
    pub parent_field: ParentField,
    pub membership: Membership,
    /// Payload field naming the record kind the collection organises, for
    /// schemas that have one. Only `collection@1.0.0` does today.
    pub kind_scope_field: Option<&'static str>,
    /// What this binding's rows carry on the envelope `item.parent`.
    pub envelope_parent: EnvelopeParent,
    /// The payload `kind_scope` this binding's rows carry once the store has
    /// converged on `collection@1.0.0` (ADR-0022 WP G7). `None` for the
    /// generic binding, which is unscoped: it is the destination, and it sees
    /// every collection row whatever its scope.
    pub unified_kind_scope: Option<&'static str>,
    /// Where this binding records the OWNING CONTAINER, or `None` for a binding
    /// whose collections are global (manuscript folders, figure folders).
    pub container_field: Option<ContainerField>,
    /// Payload bool field marking a row as a SMART (query-defined) collection,
    /// for schemas that have one. Read-only here: the kernel never writes it and
    /// has no predicate language (ADR-0022 risk register). It exists so the
    /// per-row read-only predicate a sidebar needs — imbib gates Rename and New
    /// Subcollection on it, leaving Delete — is sourced from the payload instead
    /// of from a second Swift read of a legacy row shape.
    pub smart_field: Option<&'static str>,
}

/// The generic `collection@1.0.0` kernel schema (ADR-0022 D1).
pub const GENERIC_COLLECTION: CollectionSchemaBinding = CollectionSchemaBinding {
    schema_ref: &GENERIC_COLLECTION_SCHEMA,
    parent_field: ParentField::Payload("parent_id"),
    membership: Membership::ContainsEdge,
    kind_scope_field: Some("kind_scope"),
    envelope_parent: EnvelopeParent::OwningLibrary,
    unified_kind_scope: None,
    // The generic binding is the DESTINATION and must see every collection row
    // whatever container it sits in, exactly as it is unscoped by `kind_scope`.
    // A container-scoped generic read is the caller's filter, not the binding's.
    container_field: None,
    // `collection@1.0.0` schema'd `is_smart` from the start (D1), inert.
    smart_field: Some("is_smart"),
};

/// A binding after the store's `collections.unified` marker has been consulted
/// — the shape every verb below actually works against.
///
/// Built by [`CollectionSchemaBinding::resolved`]; obtained from a live store
/// by [`resolve`]. Marker OFF resolves to the static binding verbatim, which is
/// why the pre-migration behaviour is byte-identical to its pre-G7 self.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedBinding {
    /// The `schema_ref` to query and to write.
    pub schema_ref: &'static SchemaRef,
    /// Where the tree parent is read from and written to.
    pub parent_field: ParentField,
    /// Unchanged by the flip, always.
    pub membership: Membership,
    /// The payload field whose value [`CollectionRow::kind_scope`] reports.
    /// `Some` only for the generic binding, whose scope is genuinely per-row.
    pub kind_scope_field: Option<&'static str>,
    /// The payload `kind_scope` this binding is PINNED to: a filter on every
    /// read, a forced value on every write. `None` = unscoped (marker off, or
    /// the generic binding).
    pub kind_scope: Option<&'static str>,
    /// What to write to the envelope `item.parent`.
    pub envelope_parent: EnvelopeParent,
    /// Unchanged by the flip, always: WP G7 rewrites the schema and the payload
    /// and leaves the envelope alone, so the container axis is the one axis that
    /// is provably identical on both sides.
    pub container_field: Option<ContainerField>,
    /// Unchanged by the flip: `collection@1.0.0` spells its smart flag
    /// `is_smart` too, which is the name every legacy binding that has one uses.
    pub smart_field: Option<&'static str>,
    /// The marker's state when this was resolved. Reported for diagnostics;
    /// no verb branches on it directly.
    pub unified: bool,
}

impl ResolvedBinding {
    /// Does `item` belong to this binding — right schema, and (once converged)
    /// right `kind_scope`?
    ///
    /// The public half of the kernel's internal membership test, for callers
    /// that own a schema GUARD rather than a query: imbib-core's
    /// `rename_collection` keeps writing through
    /// `SqliteItemStore::update_with_undo` — its operation-log rows are what
    /// the history panel reads, which the kernel's `CollectionMutation` is not
    /// — so it cannot delegate to [`rename`]. But its
    /// `item.schema == impress_item::schema::refs::IMBIB_COLLECTION` guard is exactly what turns into an
    /// unconditional `NotFound` at the flip. Resolving the binding once and
    /// asking it this question is the whole fix, and it does not fork
    /// [`resolve`].
    pub fn matches(&self, item: &Item) -> bool {
        is_bound(self, item)
    }

    /// The predicates that narrow a query on [`Self::schema_ref`] to this
    /// binding's rows. Empty with the marker off — the query the store sees is
    /// then literally the pre-G7 query — and one `kind_scope` equality once
    /// converged.
    ///
    /// Exposed for the same reason as [`Self::matches`]: a caller outside this
    /// module that must build its own `ItemQuery` can be marker-correct with
    /// `b.schema_ref` plus this, instead of re-deriving the resolution table.
    pub fn scope_predicates(&self) -> Vec<Predicate> {
        scope_predicates(self)
    }
}

impl CollectionSchemaBinding {
    /// Resolve this binding for a store whose convergence marker is `unified`.
    ///
    /// Pure, so the resolution table in the module docs is testable without a
    /// database.
    pub fn resolved(&self, unified: bool) -> ResolvedBinding {
        match (unified, self.unified_kind_scope) {
            // Converged: the legacy schema has no rows of its own left. Query
            // the generic schema, scoped to this binding's kind, through the
            // canonical payload tree field. Membership is untouched.
            (true, Some(kind_scope)) => ResolvedBinding {
                schema_ref: &GENERIC_COLLECTION_SCHEMA,
                parent_field: ParentField::Payload("parent_id"),
                membership: self.membership,
                kind_scope_field: self.kind_scope_field,
                kind_scope: Some(kind_scope),
                envelope_parent: self.envelope_parent,
                container_field: self.container_field,
                smart_field: self.smart_field,
                unified: true,
            },
            _ => ResolvedBinding {
                schema_ref: self.schema_ref,
                parent_field: self.parent_field,
                membership: self.membership,
                kind_scope_field: self.kind_scope_field,
                kind_scope: None,
                envelope_parent: self.envelope_parent,
                container_field: self.container_field,
                smart_field: self.smart_field,
                unified,
            },
        }
    }
}

/// Resolve a binding against a live store's `collections.unified` marker.
///
/// One indexed `store_metadata` read; every verb does this once, at the top.
pub fn resolve(
    store: &dyn AttributedStore,
    binding: &CollectionSchemaBinding,
) -> Result<ResolvedBinding, StoreError> {
    resolve_on(store, binding)
}

/// [`resolve`] over any backend of the store port — the marker is the port's
/// `metadata_get`, so the layout runtime's collection-subtree read
/// (`impress-layout-service`) works on a store it only knows as
/// `dyn AttributedStore` (docs/plan-koine-host.md H-D1).
pub fn resolve_on(
    store: &dyn AttributedStore,
    binding: &CollectionSchemaBinding,
) -> Result<ResolvedBinding, StoreError> {
    Ok(binding.resolved(is_migrated_on(store)?))
}

/// One flat collection row. Callers assemble the tree from `parent_id`
/// (`None` = root) — the kernel deliberately returns a flat list so sidebars
/// can group, filter and sort it their own way.
#[derive(Debug, Clone, PartialEq)]
pub struct CollectionRow {
    /// Lowercase UUID string.
    pub id: String,
    pub name: String,
    /// Lowercase UUID string of the tree parent, or `None` for a root.
    pub parent_id: Option<String>,
    pub sort_order: i64,
    /// Record-kind scope, for bindings whose schema carries one.
    pub kind_scope: Option<String>,
    /// Lowercase UUID string of the OWNING CONTAINER (imbib: the library), or
    /// `None` for a binding with no container axis. Never the tree parent —
    /// that is `parent_id`, and conflating the two is the c902a22f regression.
    pub container_id: Option<String>,
    /// Is this a smart (query-defined) collection? `false` for every binding
    /// whose schema has no such field. The kernel has no predicate language, so
    /// this is a REPORTED fact a caller may gate its own verbs on — which is
    /// what imbib's sidebar does (smart rows offer Delete only).
    pub is_smart: bool,
    /// How many members the collection holds — [`member_counts`]'s number,
    /// carried on the row so a tree read is a drop-in for imbib-core's
    /// `list_collections` (whose `publication_count` is exactly the outgoing
    /// `Contains`-edge count, unfiltered by member schema). For an
    /// envelope-membership binding this is the filed-children count instead.
    pub member_count: i64,
}

/// The value the mutated field held BEFORE a single-field structural verb ran.
///
/// One variant per verb rather than three optional fields, because
/// `reparent`'s prior value is legitimately `None` (the collection was a root)
/// and "absent" must not be confusable with "was a root".
#[derive(Debug, Clone, PartialEq)]
pub enum CollectionPrior {
    /// Prior `name`, from [`rename`].
    Name(String),
    /// Prior `sort_order`, from [`reorder`].
    SortOrder(i64),
    /// Prior tree parent, from [`reparent`]. `None` = it was a root.
    Parent(Option<String>),
    /// Prior tree parent AND prior owning container, from [`reparent_in`] when
    /// the move crossed containers. A separate variant rather than a wider
    /// `Parent`, so every existing caller's `Parent` match keeps compiling and
    /// keeps meaning exactly what it meant: a same-container move.
    ParentInContainer {
        /// The tree parent before the move. `None` = it was a root.
        parent: Option<String>,
        /// The owning container before the move. `None` = it had none.
        container: Option<String>,
    },
}

impl CollectionPrior {
    /// The prior name, if this came from [`rename`].
    pub fn name(&self) -> Option<&str> {
        match self {
            CollectionPrior::Name(name) => Some(name),
            _ => None,
        }
    }

    /// The prior sort order, if this came from [`reorder`].
    pub fn sort_order(&self) -> Option<i64> {
        match self {
            CollectionPrior::SortOrder(order) => Some(*order),
            _ => None,
        }
    }

    /// The prior tree parent, if this came from [`reparent`] or [`reparent_in`].
    /// The outer `Option` is "wrong variant", the inner one is "was a root".
    pub fn parent_id(&self) -> Option<Option<&str>> {
        match self {
            CollectionPrior::Parent(parent) => Some(parent.as_deref()),
            CollectionPrior::ParentInContainer { parent, .. } => Some(parent.as_deref()),
            _ => None,
        }
    }

    /// The prior owning container, if this came from a [`reparent_in`] that
    /// crossed containers. `None` for every other variant — including
    /// [`CollectionPrior::Parent`], whose whole point is that the container did
    /// not move and must not be rewritten by the inverse.
    pub fn container_id(&self) -> Option<Option<&str>> {
        match self {
            CollectionPrior::ParentInContainer { container, .. } => Some(container.as_deref()),
            _ => None,
        }
    }
}

/// The result of a single-field structural verb: the row as it now stands,
/// plus the prior value needed to undo it. See the module-level undo contract.
#[derive(Debug, Clone, PartialEq)]
pub struct CollectionMutation {
    /// The collection AFTER the change.
    pub row: CollectionRow,
    /// The value the changed field held BEFORE.
    pub prior: CollectionPrior,
}

/// Everything needed to put a deleted collection back exactly as it was —
/// same id, same place in the tree, same members, same children.
///
/// Produced by [`delete`], consumed by [`restore`]. Deliberately structured
/// (rather than an opaque serialized item) so the Swift undo stack can hold it
/// in a value type and read it in the debugger.
#[derive(Debug, Clone, PartialEq)]
pub struct DeletedCollection {
    /// The collection as it was, including its original id.
    pub row: CollectionRow,
    /// The envelope `item.parent` the row carried — the OWNING LIBRARY for
    /// payload-tree bindings, and the same thing as `row.parent_id` for
    /// envelope bindings. Restored verbatim so the row reappears in the
    /// legacy `HasParent` library listings.
    pub envelope_parent_id: Option<String>,
    /// Members whose membership the delete dropped: `Contains` targets (FK
    /// CASCADE) or envelope-filed child items that were unfiled (FK SET NULL).
    /// Never includes sub-collections — those are tree nodes.
    pub member_ids: Vec<String>,
    /// Direct child collections of this binding that were left dangling: for
    /// envelope bindings their `item.parent` was cleared by the FK, for
    /// payload bindings their parent ref now points at a row that is gone.
    /// [`restore`] re-attaches all of them.
    pub child_collection_ids: Vec<String>,
}

// ─── Reads ───────────────────────────────────────────────────────────────────

/// All collections of the bound schema, ordered by `sort_order`.
pub fn list_tree(
    store: &dyn AttributedStore,
    binding: &CollectionSchemaBinding,
) -> Result<Vec<CollectionRow>, StoreError> {
    list_tree_on(store, binding)
}

/// [`list_tree`] over any backend of the store port (see [`resolve_on`]).
pub fn list_tree_on(
    store: &dyn AttributedStore,
    binding: &CollectionSchemaBinding,
) -> Result<Vec<CollectionRow>, StoreError> {
    let b = resolve_on(store, binding)?;
    list_tree_resolved(store, &b, None)
}

/// The collections of one OWNING CONTAINER, ordered by `sort_order`.
///
/// `container` is ignored by a binding with no container axis, which then
/// answers exactly as [`list_tree`] does — so a caller may pass the container it
/// has without asking whether the binding cares.
///
/// This is the migration-safe replacement for imbib-core's
/// `list_collections(library_id)`: that export hard-codes
/// `schema_ref = "imbib/collection"` and returns nothing once WP G7 has run,
/// while this resolves the binding against the `collections.unified` marker and
/// filters on the envelope, which the migration does not touch.
pub fn list_tree_in(
    store: &dyn AttributedStore,
    binding: &CollectionSchemaBinding,
    container: Option<&str>,
) -> Result<Vec<CollectionRow>, StoreError> {
    let b = resolve(store, binding)?;
    let container = container.map(parse_id).transpose()?;
    list_tree_resolved(store, &b, container)
}

fn list_tree_resolved(
    store: &dyn AttributedStore,
    b: &ResolvedBinding,
    container: Option<ItemId>,
) -> Result<Vec<CollectionRow>, StoreError> {
    let mut predicates = scope_predicates(b);
    if let (Some(ContainerField::Envelope), Some(container)) = (b.container_field, container) {
        predicates.push(Predicate::HasParent(container));
    }
    let q = ItemQuery {
        schema: Some(b.schema_ref.clone()),
        predicates,
        sort: vec![SortDescriptor {
            field: "payload.sort_order".into(),
            ascending: true,
        }],
        include_tags: false,
        // References are the ContainsEdge member count (`row_of`); collection
        // trees are small, so loading them here is cheaper than a count query
        // per row and keeps the read one round trip.
        include_references: true,
        ..Default::default()
    };
    store
        .query(&q)?
        .iter()
        .map(|item| row_of(store, b, item))
        .collect()
}

/// The items belonging to a collection, in creation order (newest first).
/// Heterogeneous by construction: a `kind_scope: "any"` collection returns
/// members of every schema.
pub fn list_members(
    store: &dyn AttributedStore,
    binding: &CollectionSchemaBinding,
    collection_id: &str,
) -> Result<Vec<Item>, StoreError> {
    let b = resolve(store, binding)?;
    let id = parse_id(collection_id)?;
    load_collection(store, &b, id)?;
    store.query(&member_query(&b, id))
}

/// The collections that CONTAIN `member` — the inverse of [`list_members`],
/// ordered by `sort_order` like every other tree read.
///
/// This is the migration-safe replacement for imbib-core's
/// `list_collections_for_publication`, which hard-codes
/// `schema_ref = "imbib/collection"` and returns nothing once WP G7 has run.
/// It is one verb rather than a per-kind query because the inverse-membership
/// question is not publication-specific: the binding is what makes "which
/// manuscript folders hold this manuscript" the same call with a different
/// argument, which is exactly how `get_manuscript_detail` was fixed — one line,
/// no second predicate.
///
/// Both membership mechanics are answered, so a caller need not know which one
/// its binding uses: a `Contains` binding runs the reverse-edge predicate, and
/// an envelope binding (figure folders) reports the member's own `item.parent`
/// when that parent is a collection of this binding — at most one row, which
/// is the truth for a filing-based membership.
///
/// A member id that does not exist yields an empty list rather than an error:
/// "no collections contain a thing that is not there" is the answer every
/// caller wants, and it matches [`member_counts`]'s tolerance of stale ids.
pub fn collections_containing(
    store: &dyn AttributedStore,
    binding: &CollectionSchemaBinding,
    member_id: &str,
) -> Result<Vec<CollectionRow>, StoreError> {
    let b = resolve(store, binding)?;
    let member = parse_id(member_id)?;
    containing_items(store, &b, member, true)?
        .iter()
        .map(|item| row_of(store, &b, item))
        .collect()
}

/// [`collections_containing`], projected to ids.
///
/// The same query, without the per-row member count — which means without
/// loading every containing collection's `Contains` edges. imbib-core's
/// `get_publication_detail` wants exactly this (it reports collection IDS on a
/// detail pane that opens on every selection change), and paying for a
/// thousand-row reference load per open to throw the count away would be a new
/// cost the blind query never had.
pub fn collections_containing_ids(
    store: &dyn AttributedStore,
    binding: &CollectionSchemaBinding,
    member_id: &str,
) -> Result<Vec<String>, StoreError> {
    let b = resolve(store, binding)?;
    let member = parse_id(member_id)?;
    Ok(containing_items(store, &b, member, false)?
        .iter()
        .map(|item| item.id.to_string())
        .collect())
}

/// Member count per collection, aligned with `collection_ids`. Unknown ids
/// count 0 rather than erroring, so a stale sidebar cannot break a refresh.
pub fn member_counts(
    store: &dyn AttributedStore,
    binding: &CollectionSchemaBinding,
    collection_ids: &[String],
) -> Result<Vec<u32>, StoreError> {
    let b = resolve(store, binding)?;
    let mut counts = Vec::with_capacity(collection_ids.len());
    for raw in collection_ids {
        let id = parse_id(raw)?;
        counts.push(match store.get(id)? {
            Some(item) if is_bound(&b, &item) => member_count_of(store, &b, &item)?,
            _ => 0,
        });
    }
    Ok(counts)
}

// ─── Structure ───────────────────────────────────────────────────────────────

/// Create a collection under `parent` (`None` = root).
///
/// `kind_scope` is written only for bindings whose schema has the field; the
/// generic binding defaults it to `"any"`. `sort_order` positions the new row
/// among its siblings: `None` keeps the historical behaviour (`0`, i.e. sort
/// by whatever the caller's tie-break is), `Some(n)` writes `n` — pass the
/// current sibling count to append to the end, which is what implore's figure
/// folders want and used to emulate with a second `reorder` call.
///
/// For payload-tree bindings the new item inherits the parent collection's
/// envelope parent — the owning library — so a kernel-created collection lands
/// in the same library its parent is filed under and stays visible to the
/// legacy `HasParent` listings.
///
/// Once the store has converged (WP G7) a legacy binding writes generic-schema
/// rows carrying its own `kind_scope`, and the caller's `kind_scope` argument
/// is ignored: a figure folder is a figure folder whatever an agent asks for.
///
/// **Undo:** `delete(row.id)`.
pub fn create(
    store: &dyn AttributedStore,
    binding: &CollectionSchemaBinding,
    name: &str,
    parent: Option<&str>,
    kind_scope: Option<&str>,
    sort_order: Option<i64>,
) -> Result<CollectionRow, StoreError> {
    create_in(store, binding, name, parent, kind_scope, sort_order, None)
}

/// Create a collection in an explicit OWNING CONTAINER (ADR-0022 C2).
///
/// `container` is what makes per-library creation expressible: imbib's "New
/// Collection" on a library row creates a ROOT collection whose owning library
/// is that row, and there is no parent collection to inherit the library from.
/// The historical inherit-from-parent rule is what `None` still means, so
/// [`create`] is this function with `None` and every pre-C2 caller is unchanged.
///
/// A binding with no container axis ignores `container` outright rather than
/// writing an envelope parent its tree semantics would then misread.
///
/// **Undo:** `delete(row.id)`.
#[allow(clippy::too_many_arguments)]
pub fn create_in(
    store: &dyn AttributedStore,
    binding: &CollectionSchemaBinding,
    name: &str,
    parent: Option<&str>,
    kind_scope: Option<&str>,
    sort_order: Option<i64>,
    container: Option<&str>,
) -> Result<CollectionRow, StoreError> {
    create_in_with_payload(
        store,
        binding,
        name,
        parent,
        kind_scope,
        sort_order,
        container,
        &[],
    )
}

/// [`create_in`] plus payload fields the kernel does not interpret.
///
/// The kernel owns the STRUCTURAL write shape — `schema_ref`, `name`,
/// `sort_order`, `kind_scope`, the tree parent, the envelope — because that
/// shape is what `collection_migration` (impress-core) has to be able to produce and
/// consume. An app layered on top may still have payload of its own on the same
/// row: imbib's `create_collection` takes `is_smart` and `smart_query`, and
/// dropping them at the flip would silently demote every smart collection a
/// user creates.
///
/// `extra` is written FIRST, so a structural field can never be shadowed by it:
/// a caller cannot smuggle a `parent_id` past the binding's tree semantics.
/// One insert, so a create is still one operation in the store's op log — the
/// alternative (create, then patch) would put a second entry in the undo
/// history panel for what the user did once.
///
/// **Undo:** `delete(row.id)`.
#[allow(clippy::too_many_arguments)]
pub fn create_in_with_payload(
    store: &dyn AttributedStore,
    binding: &CollectionSchemaBinding,
    name: &str,
    parent: Option<&str>,
    kind_scope: Option<&str>,
    sort_order: Option<i64>,
    container: Option<&str>,
    extra: &[(&str, Value)],
) -> Result<CollectionRow, StoreError> {
    let b = resolve(store, binding)?;
    let parent_id = parent.map(parse_id).transpose()?;
    let container_id = match b.container_field {
        Some(ContainerField::Envelope) => container.map(parse_id).transpose()?,
        None => None,
    };
    let parent_item = match parent_id {
        Some(pid) => Some(load_collection(store, &b, pid)?),
        None => None,
    };

    let mut payload: BTreeMap<String, Value> = BTreeMap::new();
    for (key, value) in extra {
        payload.insert((*key).to_string(), value.clone());
    }
    payload.insert("name".into(), Value::String(name.to_string()));
    payload.insert("sort_order".into(), Value::Int(sort_order.unwrap_or(0)));
    write_kind_scope(&b, &mut payload, kind_scope);
    if let (ParentField::Payload(field), Some(pid)) = (b.parent_field, parent_id) {
        payload.insert(field.into(), Value::String(pid.to_string()));
    }

    let mut item = new_item(store, b.schema_ref, payload);
    item.parent = match b.envelope_parent {
        // An EXPLICIT container wins over the inherit: a root collection has no
        // parent to inherit from, and a sub-collection created with a stated
        // library must land in that library rather than in its parent's.
        EnvelopeParent::OwningLibrary => {
            container_id.or_else(|| parent_item.as_ref().and_then(|p| p.parent))
        }
        // Figure folders keep nesting through the envelope even once their
        // tree parent also lives in the payload, so envelope-filed membership
        // (`item.parent` of a figure → this folder) keeps working unchanged.
        EnvelopeParent::TreeParent => parent_id,
    };

    store.insert(item.clone())?;
    row_of(store, &b, &item)
}

/// Rename a collection.
///
/// **Undo:** `rename(id, mutation.prior.name())`.
pub fn rename(
    store: &dyn AttributedStore,
    binding: &CollectionSchemaBinding,
    id: &str,
    name: &str,
) -> Result<CollectionMutation, StoreError> {
    let b = resolve(store, binding)?;
    let id = parse_id(id)?;
    let before = load_collection(store, &b, id)?;
    let prior = CollectionPrior::Name(string_field(&before, "name").unwrap_or_default());
    store.update(
        id,
        vec![FieldMutation::SetPayload(
            "name".into(),
            Value::String(name.to_string()),
        )],
    )?;
    Ok(CollectionMutation {
        row: reload_row(store, &b, id)?,
        prior,
    })
}

/// Move a collection under `new_parent` (`None` = make it a root).
///
/// Rejects self-parenting and any move that would put a collection under one
/// of its own descendants. This check used to live in the Swift sidebar view
/// model, where every caller had to remember it; it is the kernel's now.
///
/// **Undo:** `reparent(id, mutation.prior.parent_id())` — the prior value is
/// `Some(None)` when the collection was a root, which a bare `Option<String>`
/// could not express.
pub fn reparent(
    store: &dyn AttributedStore,
    binding: &CollectionSchemaBinding,
    id: &str,
    new_parent: Option<&str>,
) -> Result<CollectionMutation, StoreError> {
    reparent_in(store, binding, id, new_parent, None)
}

/// Move a collection under `new_parent` AND into `new_container` (ADR-0022 C2).
///
/// `new_container: None` means "leave the owning container alone", which is what
/// [`reparent`] passes and what a same-library move wants: the legacy Swift path
/// skipped its `reparentItem` write entirely when the library did not change, and
/// so does this — one write, not two.
///
/// `new_container: Some(c)` is the CROSS-container move imbib performs as two
/// writes today (payload `parent_id` plus `reparentItem`). Both land in one
/// `store.update`, which is also what makes the pair atomic and the inverse
/// exact.
///
/// A binding with no container axis ignores `new_container`.
///
/// **Undo:** `reparent_in(id, prior.parent_id(), prior.container_id())`. When
/// the container did not move the prior is a plain
/// [`CollectionPrior::Parent`] and `container_id()` is `None`, so the inverse
/// leaves the container alone — undoing a same-library move must not start
/// writing an envelope the forward move never touched.
pub fn reparent_in(
    store: &dyn AttributedStore,
    binding: &CollectionSchemaBinding,
    id: &str,
    new_parent: Option<&str>,
    new_container: Option<&str>,
) -> Result<CollectionMutation, StoreError> {
    let b = resolve(store, binding)?;
    let id = parse_id(id)?;
    let before = load_collection(store, &b, id)?;
    let new_container_id = match b.container_field {
        Some(ContainerField::Envelope) => new_container.map(parse_id).transpose()?,
        None => None,
    };
    let prior_parent = tree_parent(&b, &before).map(|p| p.to_string());
    let prior = match new_container_id {
        Some(_) => CollectionPrior::ParentInContainer {
            parent: prior_parent,
            container: container_of(&b, &before).map(|c| c.to_string()),
        },
        None => CollectionPrior::Parent(prior_parent),
    };
    let new_parent_id = new_parent.map(parse_id).transpose()?;

    if let Some(target) = new_parent_id {
        if target == id {
            return Err(StoreError::Validation(format!(
                "collection {id} cannot be its own parent"
            )));
        }
        let target_item = load_collection(store, &b, target)?;
        let mut cursor = tree_parent(&b, &target_item);
        let mut hops = 0usize;
        while let Some(ancestor) = cursor {
            if ancestor == id {
                return Err(StoreError::Validation(format!(
                    "reparenting {id} under {target} would create a cycle"
                )));
            }
            hops += 1;
            if hops > MAX_TREE_DEPTH {
                return Err(StoreError::Validation(format!(
                    "collection ancestry of {target} exceeds {MAX_TREE_DEPTH} levels"
                )));
            }
            cursor = match store.get(ancestor)? {
                Some(item) => tree_parent(&b, &item),
                None => None,
            };
        }
    }

    let mut mutations = vec![match (b.parent_field, new_parent_id) {
        (ParentField::Payload(field), Some(target)) => {
            FieldMutation::SetPayload(field.into(), Value::String(target.to_string()))
        }
        (ParentField::Payload(field), None) => FieldMutation::RemovePayload(field.into()),
        (ParentField::Envelope, target) => FieldMutation::SetParent(target),
    }];
    // Converged figure folders read their tree parent from the payload but
    // still nest on the envelope; the two must not drift apart, or a move
    // would leave the folder's figures filed under the OLD parent's subtree.
    if mirrors_tree_parent_onto_envelope(&b) {
        mutations.push(FieldMutation::SetParent(new_parent_id));
    }
    // The second half of a cross-container move. Mutually exclusive with the
    // mirror above by construction: a binding whose envelope IS the tree parent
    // has no container axis, so `new_container_id` is always `None` there.
    if let Some(container) = new_container_id {
        mutations.push(FieldMutation::SetParent(Some(container)));
    }
    store.update(id, mutations)?;
    Ok(CollectionMutation {
        row: reload_row(store, &b, id)?,
        prior,
    })
}

/// Set a collection's position among its siblings.
///
/// **Undo:** `reorder(id, mutation.prior.sort_order())`.
pub fn reorder(
    store: &dyn AttributedStore,
    binding: &CollectionSchemaBinding,
    id: &str,
    sort_order: i64,
) -> Result<CollectionMutation, StoreError> {
    let b = resolve(store, binding)?;
    let id = parse_id(id)?;
    let before = load_collection(store, &b, id)?;
    let prior = CollectionPrior::SortOrder(int_field(&before, "sort_order").unwrap_or(0));
    store.update(
        id,
        vec![FieldMutation::SetPayload(
            "sort_order".into(),
            Value::Int(sort_order),
        )],
    )?;
    Ok(CollectionMutation {
        row: reload_row(store, &b, id)?,
        prior,
    })
}

/// Delete a collection. Members are never deleted — only the membership goes
/// away: `Contains` edges vanish with the row (FK CASCADE) and envelope-filed
/// members are unfiled (FK SET NULL). Not idempotent: deleting a missing
/// collection returns `NotFound`, matching `delete_collection` in imbib-core.
///
/// Returns the snapshot the delete consumed. **Undo:** `restore(snapshot)`,
/// which puts the row back under its original id with its members and child
/// collections re-attached.
pub fn delete(
    store: &dyn AttributedStore,
    binding: &CollectionSchemaBinding,
    id: &str,
) -> Result<DeletedCollection, StoreError> {
    let b = resolve(store, binding)?;
    let id = parse_id(id)?;
    let item = load_collection(store, &b, id)?;
    let row = row_of(store, &b, &item)?;

    // Membership the delete is about to drop. `member_query` already excludes
    // sub-collections for envelope bindings; Contains edges never point at
    // sub-collections in the first place (nesting runs through the parent).
    let member_ids: Vec<String> = match b.membership {
        Membership::ContainsEdge => item
            .references
            .iter()
            .filter(|r| r.edge_type == EdgeType::Contains)
            .map(|r| r.target.to_string())
            .collect(),
        Membership::EnvelopeParent => store
            .query(&member_query(&b, id))?
            .iter()
            .map(|i| i.id.to_string())
            .collect(),
    };

    // Direct children, whose parent pointer the delete invalidates (cleared by
    // the FK for envelope bindings, left dangling for payload bindings).
    let parent_ref = row.id.as_str();
    let child_collection_ids: Vec<String> = list_tree_resolved(store, &b, None)?
        .into_iter()
        .filter(|r| r.parent_id.as_deref() == Some(parent_ref))
        .map(|r| r.id)
        .collect();

    store.delete(id)?;

    Ok(DeletedCollection {
        row,
        envelope_parent_id: item.parent.map(|p| p.to_string()),
        member_ids,
        child_collection_ids,
    })
}

/// Put a deleted collection back, exactly as [`delete`] found it.
///
/// Recreates the row under its ORIGINAL id (the store takes explicit ids —
/// items carry theirs), re-files its members and re-attaches the child
/// collections whose parent pointer the delete invalidated. Only the envelope
/// timestamps are new; nothing else about the row changes, so an undone delete
/// leaves `list_tree` and membership byte-identical.
///
/// Tolerant where undo has to be: member and child ids that no longer exist
/// (deleted in between) are skipped rather than failing the restore. Restoring
/// over a live id is an error — that is a double-undo, not a repair.
///
/// **Undo:** `delete(row.id)`.
pub fn restore(
    store: &dyn AttributedStore,
    binding: &CollectionSchemaBinding,
    snapshot: &DeletedCollection,
) -> Result<CollectionRow, StoreError> {
    let b = resolve(store, binding)?;
    let id = parse_id(&snapshot.row.id)?;
    if store.get(id)?.is_some() {
        return Err(StoreError::Validation(format!(
            "cannot restore collection {id}: an item with that id already exists"
        )));
    }

    let mut payload: BTreeMap<String, Value> = BTreeMap::new();
    payload.insert("name".into(), Value::String(snapshot.row.name.clone()));
    payload.insert("sort_order".into(), Value::Int(snapshot.row.sort_order));
    write_kind_scope(&b, &mut payload, snapshot.row.kind_scope.as_deref());
    // A restored SMART collection must come back smart. The flag was invisible
    // to this function before C2 gave `CollectionRow` an `is_smart`, so undoing
    // the delete of a smart publication collection silently demoted it to a
    // manual one — the row reappeared with the wrong glyph and the full menu.
    // Written only when the binding HAS the field, so a figure folder's payload
    // is byte-identical to what it was.
    if let Some(field) = b.smart_field {
        if snapshot.row.is_smart {
            payload.insert(field.into(), Value::Bool(true));
        }
    }
    if let (ParentField::Payload(field), Some(parent)) =
        (b.parent_field, snapshot.row.parent_id.as_deref())
    {
        payload.insert(field.into(), Value::String(parse_id(parent)?.to_string()));
    }

    let mut item = new_item(store, b.schema_ref, payload);
    item.id = id;
    item.parent = snapshot
        .envelope_parent_id
        .as_deref()
        .map(parse_id)
        .transpose()?;
    store.insert(item)?;

    // Re-file the members the delete unfiled / whose edges it cascaded away.
    for raw in &snapshot.member_ids {
        let member = parse_id(raw)?;
        if store.get(member)?.is_none() {
            continue;
        }
        match b.membership {
            Membership::ContainsEdge => store.update(
                id,
                vec![FieldMutation::AddReference(TypedReference {
                    target: member,
                    edge_type: EdgeType::Contains,
                    metadata: None,
                })],
            )?,
            Membership::EnvelopeParent => {
                store.update(member, vec![FieldMutation::SetParent(Some(id))])?
            }
        }
    }

    // Re-attach the children. For payload bindings their ref still names this
    // id and the write is a no-op; for envelope bindings the FK nulled it. A
    // converged figure folder needs BOTH: its payload ref survived the delete,
    // its envelope did not.
    for raw in &snapshot.child_collection_ids {
        let child = parse_id(raw)?;
        match store.get(child)? {
            Some(item) if is_bound(&b, &item) => {}
            _ => continue,
        }
        let mut mutations = vec![match b.parent_field {
            ParentField::Payload(field) => {
                FieldMutation::SetPayload(field.into(), Value::String(id.to_string()))
            }
            ParentField::Envelope => FieldMutation::SetParent(Some(id)),
        }];
        if mirrors_tree_parent_onto_envelope(&b) {
            mutations.push(FieldMutation::SetParent(Some(id)));
        }
        store.update(child, mutations)?;
    }

    reload_row(store, &b, id)
}

// ─── Membership ──────────────────────────────────────────────────────────────

/// Add items to a collection. Idempotent per member.
///
/// Returns the ids that ACTUALLY became members, in the order given —
/// re-adding an existing member reports nothing, so the caller can register an
/// inverse that removes only what this call added.
///
/// **Undo:** `remove_members(collection_id, changed)`.
pub fn add_members(
    store: &dyn AttributedStore,
    binding: &CollectionSchemaBinding,
    collection_id: &str,
    item_ids: &[String],
) -> Result<Vec<String>, StoreError> {
    let b = resolve(store, binding)?;
    let collection = parse_id(collection_id)?;
    let item = load_collection(store, &b, collection)?;
    let mut members = contains_members(&item);
    let mut changed = Vec::new();
    for raw in item_ids {
        let member = parse_id(raw)?;
        match b.membership {
            Membership::ContainsEdge => {
                if members.contains(&member) {
                    continue;
                }
                store.update(
                    collection,
                    vec![FieldMutation::AddReference(TypedReference {
                        target: member,
                        edge_type: EdgeType::Contains,
                        metadata: None,
                    })],
                )?;
                members.push(member);
            }
            Membership::EnvelopeParent => {
                let filed_here = store
                    .get(member)?
                    .map(|i| i.parent == Some(collection))
                    .unwrap_or(false);
                if filed_here {
                    continue;
                }
                store.update(member, vec![FieldMutation::SetParent(Some(collection))])?;
            }
        }
        changed.push(member.to_string());
    }
    Ok(changed)
}

/// Remove items from a collection without touching the items themselves.
///
/// Returns the ids that were ACTUALLY removed, in the order given: ids that
/// were not members are skipped, and envelope-filed items that live in a
/// different collection are left alone rather than unfiled.
///
/// **Undo:** `add_members(collection_id, changed)`.
pub fn remove_members(
    store: &dyn AttributedStore,
    binding: &CollectionSchemaBinding,
    collection_id: &str,
    item_ids: &[String],
) -> Result<Vec<String>, StoreError> {
    let b = resolve(store, binding)?;
    let collection = parse_id(collection_id)?;
    let item = load_collection(store, &b, collection)?;
    let members = contains_members(&item);
    let mut changed = Vec::new();
    for raw in item_ids {
        let member = parse_id(raw)?;
        match b.membership {
            Membership::ContainsEdge => {
                if !members.contains(&member) {
                    continue;
                }
                store.update(
                    collection,
                    vec![FieldMutation::RemoveReference(member, EdgeType::Contains)],
                )?;
            }
            Membership::EnvelopeParent => {
                let filed_here = store
                    .get(member)?
                    .map(|item| item.parent == Some(collection))
                    .unwrap_or(false);
                if !filed_here {
                    continue;
                }
                store.update(member, vec![FieldMutation::SetParent(None)])?;
            }
        }
        changed.push(member.to_string());
    }
    Ok(changed)
}

// ─── Internals ───────────────────────────────────────────────────────────────

fn parse_id(id: &str) -> Result<ItemId, StoreError> {
    Uuid::parse_str(id).map_err(|_| StoreError::Validation(format!("invalid UUID: {id}")))
}

/// The `kind_scope` predicate a converged legacy binding needs, and nothing at
/// all for an unscoped one. Empty pre-migration, so the query the store sees is
/// literally the pre-G7 query.
fn scope_predicates(b: &ResolvedBinding) -> Vec<Predicate> {
    match b.kind_scope {
        Some(scope) => vec![Predicate::Eq(
            "payload.kind_scope".into(),
            Value::String(scope.to_string()),
        )],
        None => vec![],
    }
}

/// Does this item belong to the resolved binding — right schema, and (once
/// converged) right `kind_scope`?
fn is_bound(b: &ResolvedBinding, item: &Item) -> bool {
    if item.schema != *b.schema_ref {
        return false;
    }
    match b.kind_scope {
        Some(scope) => string_field(item, "kind_scope").as_deref() == Some(scope),
        None => true,
    }
}

/// Must a tree-parent write be mirrored onto the envelope?
///
/// Only for a CONVERGED figure binding: its tree parent moved into the payload
/// while its membership stayed on the envelope, so both have to move together.
/// Pre-migration the single `SetParent` already IS the tree write, and mirroring
/// would emit it twice.
fn mirrors_tree_parent_onto_envelope(b: &ResolvedBinding) -> bool {
    b.envelope_parent == EnvelopeParent::TreeParent
        && matches!(b.parent_field, ParentField::Payload(_))
}

/// Write the `kind_scope` payload field.
///
/// A converged legacy binding FORCES its own scope (the caller does not get to
/// turn a figure folder into a publication folder). The generic binding takes
/// the caller's value, defaulting to `"any"`. An unconverged legacy binding has
/// no such field and writes nothing.
fn write_kind_scope(
    b: &ResolvedBinding,
    payload: &mut BTreeMap<String, Value>,
    requested: Option<&str>,
) {
    if let Some(scope) = b.kind_scope {
        payload.insert("kind_scope".into(), Value::String(scope.to_string()));
    } else if let Some(field) = b.kind_scope_field {
        payload.insert(
            field.into(),
            Value::String(requested.unwrap_or(KIND_SCOPE_ANY).to_string()),
        );
    }
}

/// Fetch an item and assert it is a collection of the bound schema (and, once
/// converged, of the binding's `kind_scope`).
fn load_collection(
    store: &dyn AttributedStore,
    b: &ResolvedBinding,
    id: ItemId,
) -> Result<Item, StoreError> {
    let item = store.get(id)?.ok_or(StoreError::NotFound(id))?;
    if !is_bound(b, &item) {
        return Err(StoreError::Validation(format!(
            "item {id} has schema '{}'{}, expected '{}'{}",
            item.schema,
            describe_scope(string_field(&item, "kind_scope").as_deref()),
            b.schema_ref,
            describe_scope(b.kind_scope),
        )));
    }
    Ok(item)
}

fn describe_scope(scope: Option<&str>) -> String {
    scope
        .map(|s| format!(" kind_scope '{s}'"))
        .unwrap_or_default()
}

/// The collection's current `Contains` targets. Empty for envelope bindings,
/// which record membership on the member instead.
fn contains_members(collection: &Item) -> Vec<ItemId> {
    collection
        .references
        .iter()
        .filter(|r| r.edge_type == EdgeType::Contains)
        .map(|r| r.target)
        .collect()
}

fn reload_row(
    store: &dyn AttributedStore,
    b: &ResolvedBinding,
    id: ItemId,
) -> Result<CollectionRow, StoreError> {
    let item = load_collection(store, b, id)?;
    row_of(store, b, &item)
}

fn row_of(
    store: &dyn AttributedStore,
    b: &ResolvedBinding,
    item: &Item,
) -> Result<CollectionRow, StoreError> {
    Ok(CollectionRow {
        id: item.id.to_string(),
        name: string_field(item, "name").unwrap_or_default(),
        parent_id: tree_parent(b, item).map(|p| p.to_string()),
        sort_order: int_field(item, "sort_order").unwrap_or(0),
        // Deliberately the STATIC field, not the resolved scope: a converged
        // legacy binding keeps reporting `None` so the row shape every FFI
        // caller sees does not change under them at the flip.
        kind_scope: b
            .kind_scope_field
            .and_then(|field| string_field(item, field)),
        container_id: container_of(b, item).map(|c| c.to_string()),
        is_smart: b
            .smart_field
            .and_then(|field| bool_field(item, field))
            .unwrap_or(false),
        member_count: i64::from(member_count_of(store, b, item)?),
    })
}

/// One collection's member count, from an item already in hand. The
/// `ContainsEdge` arm needs the item loaded WITH references (`store.get` does;
/// a query must say `include_references: true`) and costs no store round trip;
/// the envelope arm is a count query, which is what [`member_counts`] has
/// always done for it.
fn member_count_of(
    store: &dyn AttributedStore,
    b: &ResolvedBinding,
    item: &Item,
) -> Result<u32, StoreError> {
    Ok(match b.membership {
        // Sub-collections nest through the parent field, never through a
        // Contains edge, so every outgoing Contains edge is a member.
        Membership::ContainsEdge => item
            .references
            .iter()
            .filter(|r| r.edge_type == EdgeType::Contains)
            .count() as u32,
        Membership::EnvelopeParent => store.count(&member_query(b, item.id))? as u32,
    })
}

/// The collection's owning container, or `None` for a binding with no container
/// axis. Deliberately a separate reader from [`tree_parent`] even though both
/// can name the envelope: they answer different questions, and the one time they
/// were conflated (c902a22f) every collection's parent became its library and
/// the sidebar tree flattened.
fn container_of(b: &ResolvedBinding, item: &Item) -> Option<ItemId> {
    match b.container_field {
        Some(ContainerField::Envelope) => item.parent,
        None => None,
    }
}

/// The collection's tree parent — payload ref or envelope parent per the
/// binding. Payload refs are parsed rather than compared as raw strings, so a
/// legacy uppercase ref still resolves and comes back lowercase.
fn tree_parent(b: &ResolvedBinding, item: &Item) -> Option<ItemId> {
    match b.parent_field {
        ParentField::Payload(field) => string_field(item, field)
            .filter(|s| !s.is_empty())
            .and_then(|s| Uuid::parse_str(&s).ok()),
        ParentField::Envelope => item.parent,
    }
}

/// The bound collections that contain `member`. The single implementation
/// behind [`collections_containing`] and [`collections_containing_ids`], so the
/// inverse-membership predicate is written once.
///
/// `with_references` decides whether the rows come back carrying their
/// `Contains` edges: [`row_of`] needs them for `member_count`, the id
/// projection does not, and for a collection holding thousands of papers that
/// is the difference between one cheap query and a bulk reference load.
fn containing_items(
    store: &dyn AttributedStore,
    b: &ResolvedBinding,
    member: ItemId,
    with_references: bool,
) -> Result<Vec<Item>, StoreError> {
    match b.membership {
        Membership::ContainsEdge => {
            let mut predicates = scope_predicates(b);
            predicates.push(Predicate::HasReference(EdgeType::Contains, member));
            store.query(&ItemQuery {
                schema: Some(b.schema_ref.clone()),
                predicates,
                sort: vec![SortDescriptor {
                    field: "payload.sort_order".into(),
                    ascending: true,
                }],
                include_tags: false,
                include_references: with_references,
                ..Default::default()
            })
        }
        // Filing-based membership: the member names its own collection, so the
        // answer is its envelope parent — if that parent is a collection of
        // this binding at all, which is what excludes a figure filed directly
        // under a library.
        Membership::EnvelopeParent => {
            let parent = match store.get(member)? {
                Some(item) => item.parent,
                None => return Ok(vec![]),
            };
            Ok(match parent {
                Some(pid) => store
                    .get(pid)?
                    .filter(|item| is_bound(b, item))
                    .map(|item| vec![item])
                    .unwrap_or_default(),
                None => vec![],
            })
        }
    }
}

/// Query selecting a collection's members.
fn member_query(b: &ResolvedBinding, collection: ItemId) -> ItemQuery {
    let predicates = match b.membership {
        Membership::ContainsEdge => vec![Predicate::ReferencedBy(EdgeType::Contains, collection)],
        // Envelope children include nested sub-collections; those are tree
        // nodes, not members. Post-convergence the excluded schema is the
        // generic one, which still catches every sub-folder — and now also
        // any native generic collection that happens to be filed here.
        Membership::EnvelopeParent => vec![
            Predicate::HasParent(collection),
            Predicate::Neq("schema_ref".into(), Value::String(b.schema_ref.to_string())),
        ],
    };
    ItemQuery {
        predicates,
        sort: vec![SortDescriptor {
            field: "created".into(),
            ascending: false,
        }],
        ..Default::default()
    }
}

/// A blank row of `schema` carrying `payload`, authored as the store's default
/// author ([`ItemStore::default_author`]), ready to `insert`.
pub fn new_item(
    store: &dyn AttributedStore,
    schema: &SchemaRef,
    payload: BTreeMap<String, Value>,
) -> Item {
    let now = Utc::now();
    let (author, author_kind) = store.default_author();
    Item {
        id: Uuid::new_v4(),
        schema: schema.clone(),
        payload,
        created: now,
        modified: now,
        author,
        author_kind,
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
    }
}

fn string_field(item: &Item, field: &str) -> Option<String> {
    match item.payload.get(field) {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn int_field(item: &Item, field: &str) -> Option<i64> {
    match item.payload.get(field) {
        Some(Value::Int(i)) => Some(*i),
        _ => None,
    }
}

fn bool_field(item: &Item, field: &str) -> Option<bool> {
    match item.payload.get(field) {
        Some(Value::Bool(b)) => Some(*b),
        _ => None,
    }
}
