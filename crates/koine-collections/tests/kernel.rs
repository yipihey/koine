//! The collection kernel over the port's backends: every case runs against a
//! `MemoryStore` and a `DirStore` (docs/plan-koine-kit-moves.md KM-D2).
//!
//! The cases are the kernel's original SQLite tests, unchanged in what they
//! assert. The app bindings below are copies of the ones impress-apps defines
//! (`imbib/collection`, `manuscript-collection`, `figure-collection`): they
//! are the shapes a kernel must serve, not part of the kernel.

use std::collections::BTreeMap;

use impress_item::{
    ActorKind, AttributedStore, FieldMutation, ItemId, SchemaRef, StoreError, Value,
};
use koine_collections::ops::*;
use koine_store_dir::DirStore;
use koine_store_memory::MemoryStore;
use uuid::Uuid;

static IMBIB_COLLECTION_SCHEMA: SchemaRef = impress_item::schema::refs::IMBIB_COLLECTION;
static MANUSCRIPT_COLLECTION_SCHEMA: SchemaRef = impress_item::schema::refs::MANUSCRIPT_COLLECTION;
static FIGURE_COLLECTION_SCHEMA: SchemaRef = impress_item::schema::refs::FIGURE_COLLECTION;

/// imbib publication collections (`imbib/collection`): payload `parent_id`
/// tree, `Contains` membership, envelope parent = owning library.
///
/// The one binding with all three optional axes populated: it is per-library
/// (`container_field`), it has smart rows (`smart_field`), and its tree parent
/// is a payload ref while its envelope is the library.
pub const IMBIB_COLLECTION: CollectionSchemaBinding = CollectionSchemaBinding {
    schema_ref: &IMBIB_COLLECTION_SCHEMA,
    parent_field: ParentField::Payload("parent_id"),
    membership: Membership::ContainsEdge,
    kind_scope_field: None,
    envelope_parent: EnvelopeParent::OwningLibrary,
    unified_kind_scope: Some("publication"),
    container_field: Some(ContainerField::Envelope),
    smart_field: Some("is_smart"),
};

/// imprint manuscript folders (`manuscript-collection@1.0.0`, stored bare as
/// `manuscript-collection`): payload `parent_collection_ref` tree.
pub const MANUSCRIPT_COLLECTION: CollectionSchemaBinding = CollectionSchemaBinding {
    schema_ref: &MANUSCRIPT_COLLECTION_SCHEMA,
    parent_field: ParentField::Payload("parent_collection_ref"),
    membership: Membership::ContainsEdge,
    kind_scope_field: None,
    envelope_parent: EnvelopeParent::OwningLibrary,
    unified_kind_scope: Some("manuscript"),
    // No container axis: imprint's folders are global, not per-library. Every
    // container-taking verb therefore behaves exactly as it did before C2.
    container_field: None,
    // `manuscript-collection` declares `is_smart` (imbib-core `schemas.rs`), and
    // the sidebar already renders the badged glyph off it. Reporting it costs
    // nothing and no verb branches on it.
    smart_field: Some("is_smart"),
};

/// implore figure folders (`figure-collection@1.0.0`). The odd one out: the
/// schema has no parent field at all, so both nesting and membership run
/// through the envelope parent.
pub const FIGURE_COLLECTION: CollectionSchemaBinding = CollectionSchemaBinding {
    schema_ref: &FIGURE_COLLECTION_SCHEMA,
    parent_field: ParentField::Envelope,
    membership: Membership::EnvelopeParent,
    kind_scope_field: None,
    envelope_parent: EnvelopeParent::TreeParent,
    unified_kind_scope: Some("figure"),
    // The envelope is already spoken for — it IS the tree parent — so a figure
    // folder could not carry a container even if implore wanted one.
    container_field: None,
    // `figure-collection` has no such field (`FigureCollectionPayload` is
    // name + sort_order), so rows report `is_smart: false`.
    smart_field: None,
};

/// Every binding the kernel fronts, in stable order.
pub const ALL_BINDINGS: [CollectionSchemaBinding; 4] = [
    IMBIB_COLLECTION,
    MANUSCRIPT_COLLECTION,
    FIGURE_COLLECTION,
    GENERIC_COLLECTION,
];

fn parse_id(raw: &str) -> Result<ItemId, impl std::fmt::Debug> {
    raw.parse::<ItemId>()
}

mod cases {
    use super::*;

    /// A non-collection item of an arbitrary schema, for membership tests.
    fn make_item(store: &dyn AttributedStore, schema: &str, title: &str) -> String {
        let mut payload: BTreeMap<String, Value> = BTreeMap::new();
        payload.insert("title".into(), Value::String(title.into()));
        let item = new_item(store, &SchemaRef::from_stored(schema.to_owned()), payload);
        let id = store.insert(item).expect("insert item");
        id.to_string()
    }

    fn names(rows: &[CollectionRow]) -> Vec<&str> {
        rows.iter().map(|r| r.name.as_str()).collect()
    }

    fn row(rows: &[CollectionRow], id: &str) -> CollectionRow {
        rows.iter()
            .find(|r| r.id == id)
            .cloned()
            .unwrap_or_else(|| panic!("row {id} not listed"))
    }

    // ─── Cycle check ─────────────────────────────────────────────────────

    pub fn reparent_rejects_self_parent(store: &dyn AttributedStore) {
        let a = create(store, &GENERIC_COLLECTION, "A", None, None, None).unwrap();
        let err = reparent(store, &GENERIC_COLLECTION, &a.id, Some(&a.id));
        assert!(
            matches!(err, Err(StoreError::Validation(ref m)) if m.contains("own parent")),
            "self-parent must be rejected, got {err:?}"
        );
    }

    pub fn reparent_rejects_direct_child(store: &dyn AttributedStore) {
        let parent = create(store, &GENERIC_COLLECTION, "Parent", None, None, None).unwrap();
        let child = create(
            store,
            &GENERIC_COLLECTION,
            "Child",
            Some(&parent.id),
            None,
            None,
        )
        .unwrap();

        let err = reparent(store, &GENERIC_COLLECTION, &parent.id, Some(&child.id));
        assert!(
            matches!(err, Err(StoreError::Validation(ref m)) if m.contains("cycle")),
            "moving a parent under its own child must be rejected, got {err:?}"
        );
        // The tree is untouched by the rejected move.
        let rows = list_tree(store, &GENERIC_COLLECTION).unwrap();
        assert_eq!(row(&rows, &parent.id).parent_id, None);
        assert_eq!(row(&rows, &child.id).parent_id, Some(parent.id.clone()));
    }

    pub fn reparent_rejects_deep_descendant(store: &dyn AttributedStore) {
        let a = create(store, &GENERIC_COLLECTION, "A", None, None, None).unwrap();
        let b = create(store, &GENERIC_COLLECTION, "B", Some(&a.id), None, None).unwrap();
        let c = create(store, &GENERIC_COLLECTION, "C", Some(&b.id), None, None).unwrap();
        let d = create(store, &GENERIC_COLLECTION, "D", Some(&c.id), None, None).unwrap();

        let err = reparent(store, &GENERIC_COLLECTION, &a.id, Some(&d.id));
        assert!(
            matches!(err, Err(StoreError::Validation(ref m)) if m.contains("cycle")),
            "moving a root under its grandchild's child must be rejected, got {err:?}"
        );
        // A sibling move that is NOT a cycle still works.
        let e = create(store, &GENERIC_COLLECTION, "E", None, None, None).unwrap();
        let moved = reparent(store, &GENERIC_COLLECTION, &e.id, Some(&d.id)).unwrap();
        assert_eq!(moved.row.parent_id, Some(d.id));
    }

    pub fn reparent_cycle_check_walks_envelope_chain_for_figures(store: &dyn AttributedStore) {
        let parent = create(store, &FIGURE_COLLECTION, "Plots", None, None, None).unwrap();
        let child = create(
            store,
            &FIGURE_COLLECTION,
            "Panels",
            Some(&parent.id),
            None,
            None,
        )
        .unwrap();
        assert_eq!(child.parent_id, Some(parent.id.clone()));

        let err = reparent(store, &FIGURE_COLLECTION, &parent.id, Some(&child.id));
        assert!(
            matches!(err, Err(StoreError::Validation(ref m)) if m.contains("cycle")),
            "figure folders nest via the envelope parent; cycles must still be rejected"
        );
    }

    // ─── Round trips ─────────────────────────────────────────────────────

    pub fn generic_binding_round_trip(store: &dyn AttributedStore) {
        let root = create(store, &GENERIC_COLLECTION, "Root", None, Some("any"), None).unwrap();
        let child = create(
            store,
            &GENERIC_COLLECTION,
            "Child",
            Some(&root.id),
            Some("publication"),
            None,
        )
        .unwrap();

        assert_eq!(root.kind_scope.as_deref(), Some("any"));
        assert_eq!(child.kind_scope.as_deref(), Some("publication"));
        assert_eq!(child.parent_id, Some(root.id.clone()));
        assert_eq!(root.sort_order, 0);

        let renamed = rename(store, &GENERIC_COLLECTION, &child.id, "Renamed").unwrap();
        assert_eq!(renamed.row.name, "Renamed");
        assert_eq!(renamed.prior.name(), Some("Child"));

        let reordered = reorder(store, &GENERIC_COLLECTION, &child.id, 7).unwrap();
        assert_eq!(reordered.row.sort_order, 7);
        assert_eq!(reordered.prior.sort_order(), Some(0));

        let unparented = reparent(store, &GENERIC_COLLECTION, &child.id, None).unwrap();
        assert_eq!(unparented.row.parent_id, None);
        assert_eq!(
            unparented.prior.parent_id(),
            Some(Some(root.id.as_str())),
            "the prior parent is what undo moves it back to"
        );
        let reparented = reparent(store, &GENERIC_COLLECTION, &child.id, Some(&root.id)).unwrap();
        assert_eq!(reparented.row.parent_id, Some(root.id.clone()));
        assert_eq!(
            reparented.prior.parent_id(),
            Some(None),
            "'was a root' is distinguishable from 'no prior value'"
        );

        let rows = list_tree(store, &GENERIC_COLLECTION).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            names(&rows),
            vec!["Root", "Renamed"],
            "sorted by sort_order"
        );

        delete(store, &GENERIC_COLLECTION, &child.id).unwrap();
        let rows = list_tree(store, &GENERIC_COLLECTION).unwrap();
        assert_eq!(names(&rows), vec!["Root"]);
        assert!(matches!(
            delete(store, &GENERIC_COLLECTION, &child.id),
            Err(StoreError::NotFound(_))
        ));
    }

    pub fn manuscript_binding_round_trip(store: &dyn AttributedStore) {
        let workspace =
            create(store, &MANUSCRIPT_COLLECTION, "Workspace", None, None, None).unwrap();
        let folder = create(
            store,
            &MANUSCRIPT_COLLECTION,
            "Drafts",
            Some(&workspace.id),
            None,
            None,
        )
        .unwrap();

        // The tree edge is the payload ref this schema already uses.
        let item = store.get(parse_id(&folder.id).unwrap()).unwrap().unwrap();
        assert_eq!(
            item.payload.get("parent_collection_ref"),
            Some(&Value::String(workspace.id.clone()))
        );
        assert_eq!(item.parent, None, "envelope parent is not the tree edge");
        assert!(folder.kind_scope.is_none());

        rename(store, &MANUSCRIPT_COLLECTION, &folder.id, "Submitted").unwrap();
        reorder(store, &MANUSCRIPT_COLLECTION, &folder.id, 3).unwrap();
        let rows = list_tree(store, &MANUSCRIPT_COLLECTION).unwrap();
        let updated = row(&rows, &folder.id);
        assert_eq!(updated.name, "Submitted");
        assert_eq!(updated.sort_order, 3);
        assert_eq!(updated.parent_id, Some(workspace.id.clone()));

        let ms = make_item(store, "manuscript", "A Paper");
        assert_eq!(
            add_members(
                store,
                &MANUSCRIPT_COLLECTION,
                &folder.id,
                std::slice::from_ref(&ms)
            )
            .unwrap(),
            vec![ms.clone()]
        );
        assert_eq!(
            member_counts(
                store,
                &MANUSCRIPT_COLLECTION,
                std::slice::from_ref(&folder.id)
            )
            .unwrap(),
            vec![1]
        );

        delete(store, &MANUSCRIPT_COLLECTION, &folder.id).unwrap();
        assert_eq!(list_tree(store, &MANUSCRIPT_COLLECTION).unwrap().len(), 1);
        assert!(
            store.get(parse_id(&ms).unwrap()).unwrap().is_some(),
            "deleting a collection must not delete its members"
        );
    }

    pub fn create_inherits_owning_library_from_parent_collection(store: &dyn AttributedStore) {
        let library = make_item(store, "imbib/library", "Main");
        let library_id = parse_id(&library).unwrap();

        let root = create(store, &IMBIB_COLLECTION, "Reading", None, None, None).unwrap();
        store
            .update(
                parse_id(&root.id).unwrap(),
                vec![FieldMutation::SetParent(Some(library_id))],
            )
            .unwrap();

        let child = create(
            store,
            &IMBIB_COLLECTION,
            "Queue",
            Some(&root.id),
            None,
            None,
        )
        .unwrap();
        let item = store.get(parse_id(&child.id).unwrap()).unwrap().unwrap();
        assert_eq!(
            item.parent,
            Some(library_id),
            "envelope parent stays the owning library"
        );
        assert_eq!(
            child.parent_id,
            Some(root.id),
            "tree parent is the payload ref, never the library"
        );
    }

    // ─── Membership ──────────────────────────────────────────────────────

    pub fn mixed_kind_membership_over_any_scope(store: &dyn AttributedStore) {
        let mixed = create(
            store,
            &GENERIC_COLLECTION,
            "Grant renewal",
            None,
            Some("any"),
            None,
        )
        .unwrap();
        let other = create(store, &GENERIC_COLLECTION, "Empty", None, None, None).unwrap();

        let manuscript = make_item(store, "manuscript", "Draft II");
        let figure = make_item(store, "figure", "Rotation curve");

        let added = add_members(
            store,
            &GENERIC_COLLECTION,
            &mixed.id,
            &[manuscript.clone(), figure.clone()],
        )
        .unwrap();
        assert_eq!(added, vec![manuscript.clone(), figure.clone()]);

        assert_eq!(
            member_counts(
                store,
                &GENERIC_COLLECTION,
                &[mixed.id.clone(), other.id.clone()]
            )
            .unwrap(),
            vec![2, 0],
            "counts are aligned with the requested ids"
        );

        let members = list_members(store, &GENERIC_COLLECTION, &mixed.id).unwrap();
        let mut schemas: Vec<&str> = members.iter().map(|i| i.schema.as_str()).collect();
        schemas.sort_unstable();
        assert_eq!(
            schemas,
            vec!["figure", "manuscript"],
            "one collection, two record kinds"
        );

        // Idempotent add, then partial removal.
        assert!(
            add_members(
                store,
                &GENERIC_COLLECTION,
                &mixed.id,
                std::slice::from_ref(&figure)
            )
            .unwrap()
            .is_empty(),
            "an already-filed member reports no change, so undo removes nothing"
        );
        assert_eq!(
            member_counts(store, &GENERIC_COLLECTION, std::slice::from_ref(&mixed.id)).unwrap(),
            vec![2],
            "re-adding an existing member must not duplicate it"
        );

        // Removing a non-member is likewise a no-op the caller can see.
        let stranger = make_item(store, "figure", "Never filed");
        assert!(
            remove_members(store, &GENERIC_COLLECTION, &mixed.id, &[stranger])
                .unwrap()
                .is_empty(),
            "removing a non-member reports no change"
        );

        assert_eq!(
            remove_members(
                store,
                &GENERIC_COLLECTION,
                &mixed.id,
                std::slice::from_ref(&figure)
            )
            .unwrap(),
            vec![figure]
        );
        let members = list_members(store, &GENERIC_COLLECTION, &mixed.id).unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].id.to_string(), manuscript);
    }

    pub fn figure_membership_uses_envelope_parent(store: &dyn AttributedStore) {
        let folder = create(store, &FIGURE_COLLECTION, "Paper figures", None, None, None).unwrap();
        let nested = create(
            store,
            &FIGURE_COLLECTION,
            "Supplement",
            Some(&folder.id),
            None,
            None,
        )
        .unwrap();
        let figure = make_item(store, "figure", "Panel A");

        add_members(
            store,
            &FIGURE_COLLECTION,
            &folder.id,
            std::slice::from_ref(&figure),
        )
        .unwrap();
        let filed = store.get(parse_id(&figure).unwrap()).unwrap().unwrap();
        assert_eq!(filed.parent, Some(parse_id(&folder.id).unwrap()));
        assert_eq!(
            member_counts(store, &FIGURE_COLLECTION, std::slice::from_ref(&folder.id)).unwrap(),
            vec![1],
            "the nested folder is a tree node, not a member"
        );
        assert!(nested.parent_id.is_some());

        // Removing a figure filed elsewhere is a no-op, not an unfile.
        assert!(remove_members(
            store,
            &FIGURE_COLLECTION,
            &nested.id,
            std::slice::from_ref(&figure)
        )
        .unwrap()
        .is_empty());
        assert_eq!(
            remove_members(
                store,
                &FIGURE_COLLECTION,
                &folder.id,
                std::slice::from_ref(&figure)
            )
            .unwrap(),
            vec![figure.clone()]
        );
        let unfiled = store.get(parse_id(&figure).unwrap()).unwrap().unwrap();
        assert_eq!(unfiled.parent, None);
    }

    // ─── Binding hygiene ─────────────────────────────────────────────────

    pub fn bindings_reject_items_of_another_schema(store: &dyn AttributedStore) {
        let generic = create(store, &GENERIC_COLLECTION, "Mixed", None, None, None).unwrap();
        let err = rename(store, &MANUSCRIPT_COLLECTION, &generic.id, "Nope");
        assert!(
            matches!(err, Err(StoreError::Validation(ref m)) if m.contains("manuscript-collection")),
            "a binding must not mutate another schema's collections, got {err:?}"
        );
        assert!(list_tree(store, &MANUSCRIPT_COLLECTION).unwrap().is_empty());
    }

    pub fn ids_are_normalized_to_lowercase(store: &dyn AttributedStore) {
        let root = create(store, &GENERIC_COLLECTION, "Root", None, None, None).unwrap();
        let upper = root.id.to_uppercase();

        let child = create(
            store,
            &GENERIC_COLLECTION,
            "Child",
            Some(&upper),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            child.parent_id,
            Some(root.id.clone()),
            "an uppercase parent id comes back lowercase"
        );
        let item = store.get(parse_id(&child.id).unwrap()).unwrap().unwrap();
        assert_eq!(
            item.payload.get("parent_id"),
            Some(&Value::String(root.id.clone())),
            "payload refs are stored lowercase (string-equality matching)"
        );

        // A ref that was written uppercase by a legacy caller still resolves.
        store
            .update(
                parse_id(&child.id).unwrap(),
                vec![FieldMutation::SetPayload(
                    "parent_id".into(),
                    Value::String(upper),
                )],
            )
            .unwrap();
        let rows = list_tree(store, &GENERIC_COLLECTION).unwrap();
        assert_eq!(row(&rows, &child.id).parent_id, Some(root.id));
    }

    pub fn create_rejects_unknown_parent(store: &dyn AttributedStore) {
        let missing = Uuid::new_v4().to_string();
        assert!(matches!(
            create(
                store,
                &GENERIC_COLLECTION,
                "Orphan",
                Some(&missing),
                None,
                None
            ),
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            create(
                store,
                &GENERIC_COLLECTION,
                "Orphan",
                Some("not-a-uuid"),
                None,
                None
            ),
            Err(StoreError::Validation(_))
        ));
    }

    // ─── sort_order at create time ───────────────────────────────────────

    pub fn create_writes_the_requested_sort_order(store: &dyn AttributedStore) {
        let first = create(store, &FIGURE_COLLECTION, "First", None, None, None).unwrap();
        assert_eq!(first.sort_order, 0, "None keeps the historical default");

        // What implore's figure folders want: append to the end, in one call
        // instead of create-then-reorder.
        let second = create(store, &FIGURE_COLLECTION, "Second", None, None, Some(1)).unwrap();
        let third = create(store, &FIGURE_COLLECTION, "Third", None, None, Some(2)).unwrap();
        assert_eq!(second.sort_order, 1);
        assert_eq!(third.sort_order, 2);
        assert_eq!(
            names(&list_tree(store, &FIGURE_COLLECTION).unwrap()),
            vec!["First", "Second", "Third"],
            "list_tree orders by the sort_order create wrote"
        );

        // Negative and beyond-the-end values are the caller's business.
        let pinned = create(store, &FIGURE_COLLECTION, "Pinned", None, None, Some(-1)).unwrap();
        assert_eq!(pinned.sort_order, -1);
        assert_eq!(
            names(&list_tree(store, &FIGURE_COLLECTION).unwrap())[0],
            "Pinned"
        );
    }

    // ─── Undoable delete ─────────────────────────────────────────────────

    /// `list_tree` sorted by id, so a before/after comparison does not depend
    /// on how equal `sort_order`s happen to tie-break.
    fn tree_by_id(
        store: &dyn AttributedStore,
        binding: &CollectionSchemaBinding,
    ) -> Vec<CollectionRow> {
        let mut rows = list_tree(store, binding).unwrap();
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        rows
    }

    fn member_ids(
        store: &dyn AttributedStore,
        binding: &CollectionSchemaBinding,
        collection: &str,
    ) -> Vec<String> {
        let mut ids: Vec<String> = list_members(store, binding, collection)
            .unwrap()
            .iter()
            .map(|i| i.id.to_string())
            .collect();
        ids.sort();
        ids
    }

    pub fn delete_restore_round_trips_a_contains_binding(store: &dyn AttributedStore) {
        let library = make_item(store, "imbib/library", "Main");
        let library_id = parse_id(&library).unwrap();

        let workspace =
            create(store, &MANUSCRIPT_COLLECTION, "Workspace", None, None, None).unwrap();
        store
            .update(
                parse_id(&workspace.id).unwrap(),
                vec![FieldMutation::SetParent(Some(library_id))],
            )
            .unwrap();
        let folder = create(
            store,
            &MANUSCRIPT_COLLECTION,
            "Drafts",
            Some(&workspace.id),
            None,
            Some(3),
        )
        .unwrap();
        let nested = create(
            store,
            &MANUSCRIPT_COLLECTION,
            "Submitted",
            Some(&folder.id),
            None,
            None,
        )
        .unwrap();
        let a = make_item(store, "manuscript", "Paper A");
        let b = make_item(store, "manuscript", "Paper B");
        add_members(
            store,
            &MANUSCRIPT_COLLECTION,
            &folder.id,
            &[a.clone(), b.clone()],
        )
        .unwrap();

        let before_tree = tree_by_id(store, &MANUSCRIPT_COLLECTION);
        let before_members = member_ids(store, &MANUSCRIPT_COLLECTION, &folder.id);

        let snapshot = delete(store, &MANUSCRIPT_COLLECTION, &folder.id).unwrap();
        // Structurally the create-time row; `member_count` truthfully reports
        // the two members added since (it is a live read, not part of the
        // structural identity the round trip must preserve).
        assert_eq!(
            snapshot.row,
            CollectionRow {
                member_count: 2,
                ..folder.clone()
            }
        );
        assert_eq!(
            snapshot.envelope_parent_id,
            Some(library_id.to_string()),
            "the owning library must survive the round trip"
        );
        let mut snapshot_members = snapshot.member_ids.clone();
        snapshot_members.sort();
        assert_eq!(snapshot_members, before_members);
        assert_eq!(snapshot.child_collection_ids, vec![nested.id.clone()]);
        assert_eq!(list_tree(store, &MANUSCRIPT_COLLECTION).unwrap().len(), 2);

        let restored = restore(store, &MANUSCRIPT_COLLECTION, &snapshot).unwrap();
        assert_eq!(
            restored,
            CollectionRow {
                member_count: 2,
                ..folder.clone()
            },
            "restored under its ORIGINAL id, with its two members re-filed"
        );
        assert_eq!(
            tree_by_id(store, &MANUSCRIPT_COLLECTION),
            before_tree,
            "an undone delete leaves the tree byte-identical"
        );
        assert_eq!(
            member_ids(store, &MANUSCRIPT_COLLECTION, &folder.id),
            before_members
        );
        let item = store.get(parse_id(&folder.id).unwrap()).unwrap().unwrap();
        assert_eq!(item.parent, Some(library_id));
        assert_eq!(
            member_counts(
                store,
                &MANUSCRIPT_COLLECTION,
                std::slice::from_ref(&folder.id)
            )
            .unwrap(),
            vec![2]
        );

        // Restoring twice is a double-undo, not a repair.
        assert!(matches!(
            restore(store, &MANUSCRIPT_COLLECTION, &snapshot),
            Err(StoreError::Validation(ref m)) if m.contains("already exists")
        ));
    }

    pub fn delete_restore_round_trips_the_envelope_binding(store: &dyn AttributedStore) {
        let folder = create(store, &FIGURE_COLLECTION, "Paper figures", None, None, None).unwrap();
        let nested = create(
            store,
            &FIGURE_COLLECTION,
            "Supplement",
            Some(&folder.id),
            None,
            Some(2),
        )
        .unwrap();
        let panel = make_item(store, "figure", "Panel A");
        let curve = make_item(store, "figure", "Rotation curve");
        add_members(
            store,
            &FIGURE_COLLECTION,
            &folder.id,
            &[panel.clone(), curve.clone()],
        )
        .unwrap();

        let before_tree = tree_by_id(store, &FIGURE_COLLECTION);
        let before_members = member_ids(store, &FIGURE_COLLECTION, &folder.id);

        let snapshot = delete(store, &FIGURE_COLLECTION, &folder.id).unwrap();
        let mut snapshot_members = snapshot.member_ids.clone();
        snapshot_members.sort();
        assert_eq!(
            snapshot_members, before_members,
            "the unfiled figures are the snapshot's members"
        );
        assert_eq!(snapshot.child_collection_ids, vec![nested.id.clone()]);

        // The FK really did unfile everything: figures and the sub-folder.
        assert_eq!(
            store
                .get(parse_id(&panel).unwrap())
                .unwrap()
                .unwrap()
                .parent,
            None
        );
        assert_eq!(
            row(&list_tree(store, &FIGURE_COLLECTION).unwrap(), &nested.id).parent_id,
            None
        );

        let restored = restore(store, &FIGURE_COLLECTION, &snapshot).unwrap();
        // Restore re-files both figures, so the truthful count is 2; every
        // structural field must match the create-time row exactly.
        assert_eq!(
            restored,
            CollectionRow {
                member_count: 2,
                ..folder.clone()
            }
        );
        assert_eq!(
            tree_by_id(store, &FIGURE_COLLECTION),
            before_tree,
            "the sub-folder is re-attached to the restored parent"
        );
        assert_eq!(
            member_ids(store, &FIGURE_COLLECTION, &folder.id),
            before_members,
            "the figures are re-filed, and the sub-folder is still not a member"
        );
        assert_eq!(
            member_counts(store, &FIGURE_COLLECTION, std::slice::from_ref(&folder.id)).unwrap(),
            vec![2]
        );
    }

    pub fn restore_skips_members_that_are_gone(store: &dyn AttributedStore) {
        let folder = create(store, &GENERIC_COLLECTION, "Reading", None, None, None).unwrap();
        let kept = make_item(store, "manuscript", "Kept");
        let doomed = make_item(store, "manuscript", "Doomed");
        add_members(
            store,
            &GENERIC_COLLECTION,
            &folder.id,
            &[kept.clone(), doomed.clone()],
        )
        .unwrap();

        let snapshot = delete(store, &GENERIC_COLLECTION, &folder.id).unwrap();
        store.delete(doomed.parse::<ItemId>().unwrap()).unwrap();

        // Undo must still work when the world moved on underneath it.
        restore(store, &GENERIC_COLLECTION, &snapshot).unwrap();
        assert_eq!(
            member_ids(store, &GENERIC_COLLECTION, &folder.id),
            vec![kept]
        );
    }

    // ─── The D9 gate ─────────────────────────────────────────────────────

    /// **ADR-0022 D9 gate: the store half of "impress is proven, not shipped".**
    ///
    /// The impress app does not exist, and will not for a while. What has to
    /// exist NOW is the property it is built on: one collection holding records
    /// of several kinds at once, surviving the whole verb set. Before the
    /// kernel, this was impossible by construction — four schema-specific
    /// hierarchies, each of which could only hold its own kind (D1's opening
    /// premise). This test is the assertion that it is possible, and that no
    /// verb quietly forgets a kind on the way through.
    ///
    /// Three schemas on purpose (`AppShellConfiguration.impress` binds all of
    /// them to sections): a publication, a manuscript and a figure — three
    /// different apps' record kinds in one folder. Two of them are kinds whose
    /// OWN collection binding stores membership differently
    /// (`figure-collection` files members through the envelope parent), so a
    /// generic collection holding a figure is exactly the case a per-kind
    /// implementation could not express.
    pub fn impress_gate_mixed_kind_collection_round_trip(store: &dyn AttributedStore) {
        let shelf = create(
            store,
            &GENERIC_COLLECTION,
            "Cluster paper",
            None,
            Some(KIND_SCOPE_ANY),
            None,
        )
        .unwrap();
        let sibling = create(store, &GENERIC_COLLECTION, "Unrelated", None, None, None).unwrap();
        assert_eq!(shelf.kind_scope.as_deref(), Some("any"));

        // One member per record kind, each with the schema its own app writes.
        let publication = make_item(store, "imbib/bibliography-entry", "Bardeen et al. 1986");
        let manuscript = make_item(store, "manuscript", "Draft II");
        let figure = make_item(store, "figure", "Rotation curve");

        let added = add_members(
            store,
            &GENERIC_COLLECTION,
            &shelf.id,
            &[publication.clone(), manuscript.clone(), figure.clone()],
        )
        .unwrap();
        assert_eq!(
            added,
            vec![publication.clone(), manuscript.clone(), figure.clone()],
            "every kind is filed; none is skipped for being the wrong kind"
        );

        // ─ list_members returns ALL THREE kinds ─
        let members = list_members(store, &GENERIC_COLLECTION, &shelf.id).unwrap();
        let mut schemas: Vec<&str> = members.iter().map(|i| i.schema.as_str()).collect();
        schemas.sort_unstable();
        assert_eq!(
            schemas,
            vec!["figure", "imbib/bibliography-entry", "manuscript"],
            "a kind_scope=any collection is heterogeneous by construction"
        );

        // ─ member_counts, aligned with the ids asked for ─
        assert_eq!(
            member_counts(
                store,
                &GENERIC_COLLECTION,
                &[shelf.id.clone(), sibling.id.clone()]
            )
            .unwrap(),
            vec![3, 0],
            "counts must not depend on the members' kinds"
        );

        // ─ rename round trip, through the prior value undo would use ─
        let renamed = rename(
            store,
            &GENERIC_COLLECTION,
            &shelf.id,
            "Cluster paper (final)",
        )
        .unwrap();
        assert_eq!(renamed.row.name, "Cluster paper (final)");
        assert_eq!(renamed.prior.name(), Some("Cluster paper"));
        let undone = rename(
            store,
            &GENERIC_COLLECTION,
            &shelf.id,
            renamed.prior.name().unwrap(),
        )
        .unwrap();
        assert_eq!(
            undone.row,
            CollectionRow {
                member_count: 3,
                ..shelf.clone()
            },
            "an undone rename restores the row exactly (count reflects the \
             three members added after creation — a live read, not identity)"
        );

        // ─ reparent round trip: the mixed collection is an ordinary tree node ─
        let project = create(store, &GENERIC_COLLECTION, "2026", None, None, None).unwrap();
        let moved = reparent(store, &GENERIC_COLLECTION, &shelf.id, Some(&project.id)).unwrap();
        assert_eq!(moved.row.parent_id, Some(project.id.clone()));
        assert_eq!(
            moved.prior.parent_id(),
            Some(None),
            "'was a root' is what undo needs, and is not 'no prior value'"
        );
        assert_eq!(
            member_ids(store, &GENERIC_COLLECTION, &shelf.id).len(),
            3,
            "moving a collection must not disturb its membership"
        );
        let back = reparent(store, &GENERIC_COLLECTION, &shelf.id, None).unwrap();
        assert_eq!(
            back.row,
            CollectionRow {
                member_count: 3,
                ..shelf.clone()
            },
            "the tree is exactly where it started (count is a live read)"
        );

        // ─ delete → restore brings the MIXED membership back intact ─
        let before_members = member_ids(store, &GENERIC_COLLECTION, &shelf.id);
        let before_tree = tree_by_id(store, &GENERIC_COLLECTION);

        let snapshot = delete(store, &GENERIC_COLLECTION, &shelf.id).unwrap();
        assert_eq!(
            snapshot.row,
            CollectionRow {
                member_count: 3,
                ..shelf.clone()
            }
        );
        assert_eq!(
            snapshot.row.kind_scope.as_deref(),
            Some("any"),
            "the scope is part of the snapshot, or the restored collection stops being mixed"
        );
        let mut snapshot_members = snapshot.member_ids.clone();
        snapshot_members.sort();
        assert_eq!(snapshot_members, before_members);
        // Members outlive the collection — only the membership went away.
        for member in [&publication, &manuscript, &figure] {
            assert!(
                store.get(parse_id(member).unwrap()).unwrap().is_some(),
                "deleting a collection must never delete a member ({member})"
            );
        }

        let restored = restore(store, &GENERIC_COLLECTION, &snapshot).unwrap();
        assert_eq!(
            restored,
            CollectionRow {
                member_count: 3,
                ..shelf.clone()
            },
            "restored under its ORIGINAL id, scope included, members re-filed"
        );
        assert_eq!(
            tree_by_id(store, &GENERIC_COLLECTION),
            before_tree,
            "an undone delete leaves the tree byte-identical"
        );
        assert_eq!(
            member_ids(store, &GENERIC_COLLECTION, &shelf.id),
            before_members,
            "all three kinds are re-filed"
        );

        let members = list_members(store, &GENERIC_COLLECTION, &shelf.id).unwrap();
        let mut schemas: Vec<&str> = members.iter().map(|i| i.schema.as_str()).collect();
        schemas.sort_unstable();
        assert_eq!(
            schemas,
            vec!["figure", "imbib/bibliography-entry", "manuscript"],
            "the restored collection is still mixed, not merely non-empty"
        );
        assert_eq!(
            member_counts(store, &GENERIC_COLLECTION, std::slice::from_ref(&shelf.id)).unwrap(),
            vec![3]
        );
    }

    /// The G7 dual-mode switch must be INERT until somebody flips it. Every
    /// other test in this module runs against an unmarked store and would
    /// notice a behaviour change; this one states the invariant out loud, and
    /// pins the exact resolution an unmarked store produces.
    pub fn an_unmarked_store_resolves_every_binding_to_its_legacy_self(
        store: &dyn AttributedStore,
    ) {
        assert!(!is_migrated_on(store).unwrap());
        for binding in ALL_BINDINGS {
            let resolved = resolve(store, &binding).unwrap();
            assert!(!resolved.unified, "{}", binding.schema_ref);
            assert_eq!(resolved.schema_ref, binding.schema_ref);
            assert_eq!(resolved.parent_field, binding.parent_field);
            assert_eq!(resolved.membership, binding.membership);
            assert_eq!(resolved.kind_scope_field, binding.kind_scope_field);
            assert_eq!(
                resolved.kind_scope, None,
                "{}: no scope filter is applied before the flip",
                binding.schema_ref
            );
            assert!(
                !(resolved.envelope_parent == EnvelopeParent::TreeParent
                    && matches!(resolved.parent_field, ParentField::Payload(_))),
                "{}: no extra envelope write before the flip",
                binding.schema_ref
            );
            assert!(
                resolved.scope_predicates().is_empty(),
                "{}: the query is literally the pre-G7 query",
                binding.schema_ref
            );
        }
    }

    /// Setting the marker is all it takes for a legacy binding to resolve to
    /// the generic schema, scoped to its kind (the rewrite itself is the
    /// application's, SQLite's in impress).
    pub fn the_marker_resolves_legacy_bindings_to_the_generic_schema(store: &dyn AttributedStore) {
        assert!(!is_migrated_on(store).unwrap());
        store.metadata_set(MARKER_KEY, MARKER_VALUE).unwrap();
        assert!(is_migrated_on(store).unwrap());
        let resolved = resolve(store, &FIGURE_COLLECTION).unwrap();
        assert!(resolved.unified);
        assert_eq!(resolved.schema_ref, GENERIC_COLLECTION.schema_ref);
        assert_eq!(resolved.kind_scope, Some("figure"));
        assert_eq!(resolved.parent_field, ParentField::Payload("parent_id"));
        // The generic binding is the destination: unchanged.
        let generic = resolve(store, &GENERIC_COLLECTION).unwrap();
        assert_eq!(generic.kind_scope, None);
        // And a kernel-created folder is a generic row carrying its scope.
        let made = create(store, &FIGURE_COLLECTION, "Plots", None, None, None).unwrap();
        let item = store.get(parse_id(&made.id).unwrap()).unwrap().unwrap();
        assert_eq!(item.schema.as_str(), "collection");
        assert_eq!(
            item.payload.get("kind_scope"),
            Some(&Value::String("figure".into()))
        );
        assert_eq!(list_tree(store, &FIGURE_COLLECTION).unwrap().len(), 1);
    }

    pub fn author_matches_store_default(store: &dyn AttributedStore) {
        let root = create(store, &GENERIC_COLLECTION, "Root", None, None, None).unwrap();
        let item = store.get(parse_id(&root.id).unwrap()).unwrap().unwrap();
        assert_eq!(item.author, store.default_author().0);
        assert!(matches!(
            item.author_kind,
            ActorKind::System | ActorKind::Human | ActorKind::Agent
        ));
    }

    // ─── The optional axes (ADR-0022 C2) ─────────────────────────────────
    //
    // Every test below pairs the binding that DECLARES the axis with one that
    // declines it, because the contract is not just "the axis works" — it is
    // "the axis is invisible to a binding that has none". The manuscript and
    // figure bindings are the oracle for that half (wave-3's imprint parity
    // tests run through these same seams).

    /// A stand-in owning container: any item will do, the kernel only ever
    /// writes its id to the envelope.
    fn make_container(store: &dyn AttributedStore, name: &str) -> String {
        make_item(store, "imbib/library", name)
    }

    pub fn create_in_files_a_root_collection_under_its_container(store: &dyn AttributedStore) {
        let library = make_container(store, "Main");
        let row = create_in(
            store,
            &IMBIB_COLLECTION,
            "Reading",
            None,
            None,
            None,
            Some(&library),
        )
        .unwrap();

        assert_eq!(row.container_id.as_deref(), Some(library.as_str()));
        assert_eq!(
            row.parent_id, None,
            "the container is NOT the tree parent — that conflation is c902a22f"
        );
        let item = store.get(parse_id(&row.id).unwrap()).unwrap().unwrap();
        assert_eq!(item.parent.map(|p| p.to_string()), Some(library));
    }

    pub fn create_in_inherits_the_container_when_none_is_named(store: &dyn AttributedStore) {
        let library = make_container(store, "Main");
        let parent = create_in(
            store,
            &IMBIB_COLLECTION,
            "Parent",
            None,
            None,
            None,
            Some(&library),
        )
        .unwrap();
        // `create` is `create_in(.., None)`: the historical inherit-from-parent
        // rule, unchanged.
        let child = create(
            store,
            &IMBIB_COLLECTION,
            "Child",
            Some(&parent.id),
            None,
            None,
        )
        .unwrap();
        assert_eq!(child.container_id.as_deref(), Some(library.as_str()));
        assert_eq!(child.parent_id.as_deref(), Some(parent.id.as_str()));
    }

    pub fn create_in_is_ignored_by_a_binding_with_no_container_axis(store: &dyn AttributedStore) {
        let library = make_container(store, "Main");
        for binding in [MANUSCRIPT_COLLECTION, GENERIC_COLLECTION] {
            let row =
                create_in(store, &binding, "Folder", None, None, None, Some(&library)).unwrap();
            assert_eq!(
                row.container_id, None,
                "{}: reports no container",
                binding.schema_ref
            );
            let item = store.get(parse_id(&row.id).unwrap()).unwrap().unwrap();
            assert_eq!(
                item.parent, None,
                "{}: and writes no envelope parent — behaviour is pre-C2 verbatim",
                binding.schema_ref
            );
        }
    }

    pub fn list_tree_in_scopes_to_one_container_and_list_tree_still_sees_all(
        store: &dyn AttributedStore,
    ) {
        let main = make_container(store, "Main");
        let other = make_container(store, "Other");
        create_in(
            store,
            &IMBIB_COLLECTION,
            "A",
            None,
            None,
            Some(0),
            Some(&main),
        )
        .unwrap();
        create_in(
            store,
            &IMBIB_COLLECTION,
            "B",
            None,
            None,
            Some(1),
            Some(&main),
        )
        .unwrap();
        create_in(
            store,
            &IMBIB_COLLECTION,
            "Z",
            None,
            None,
            Some(0),
            Some(&other),
        )
        .unwrap();

        assert_eq!(
            names(&list_tree_in(store, &IMBIB_COLLECTION, Some(&main)).unwrap()),
            vec!["A", "B"]
        );
        assert_eq!(
            names(&list_tree_in(store, &IMBIB_COLLECTION, Some(&other)).unwrap()),
            vec!["Z"]
        );
        // Unscoped reads are untouched, and `list_tree` IS the unscoped read.
        assert_eq!(
            list_tree(store, &IMBIB_COLLECTION).unwrap().len(),
            3,
            "list_tree is container-blind, as it always was"
        );
        assert_eq!(
            list_tree_in(store, &IMBIB_COLLECTION, None).unwrap().len(),
            3
        );
    }

    pub fn list_tree_in_ignores_the_container_for_a_binding_without_one(
        store: &dyn AttributedStore,
    ) {
        let library = make_container(store, "Main");
        create(store, &MANUSCRIPT_COLLECTION, "Drafts", None, None, None).unwrap();
        assert_eq!(
            names(&list_tree_in(store, &MANUSCRIPT_COLLECTION, Some(&library)).unwrap()),
            vec!["Drafts"],
            "a manuscript folder is not hidden by a container it never had"
        );
    }

    pub fn reparent_in_moves_tree_and_container_together_and_inverts_exactly(
        store: &dyn AttributedStore,
    ) {
        let main = make_container(store, "Main");
        let other = make_container(store, "Other");
        let source = create_in(
            store,
            &IMBIB_COLLECTION,
            "Src",
            None,
            None,
            None,
            Some(&main),
        )
        .unwrap();
        let target = create_in(
            store,
            &IMBIB_COLLECTION,
            "Dst",
            None,
            None,
            None,
            Some(&other),
        )
        .unwrap();

        let moved = reparent_in(
            store,
            &IMBIB_COLLECTION,
            &source.id,
            Some(&target.id),
            Some(&other),
        )
        .unwrap();
        assert_eq!(moved.row.parent_id.as_deref(), Some(target.id.as_str()));
        assert_eq!(moved.row.container_id.as_deref(), Some(other.as_str()));
        assert_eq!(
            moved.prior,
            CollectionPrior::ParentInContainer {
                parent: None,
                container: Some(main.clone()),
            }
        );

        // The documented inverse, verbatim.
        let back = reparent_in(
            store,
            &IMBIB_COLLECTION,
            &source.id,
            moved.prior.parent_id().unwrap(),
            moved.prior.container_id().unwrap(),
        )
        .unwrap();
        assert_eq!(back.row.parent_id, None);
        assert_eq!(back.row.container_id.as_deref(), Some(main.as_str()));
    }

    pub fn a_same_container_reparent_leaves_the_container_alone(store: &dyn AttributedStore) {
        let main = make_container(store, "Main");
        let a = create_in(store, &IMBIB_COLLECTION, "A", None, None, None, Some(&main)).unwrap();
        let b = create_in(store, &IMBIB_COLLECTION, "B", None, None, None, Some(&main)).unwrap();

        // `reparent` is `reparent_in(.., None)`: one write, and a prior that
        // says "container untouched" so the inverse cannot start writing one.
        let moved = reparent(store, &IMBIB_COLLECTION, &b.id, Some(&a.id)).unwrap();
        assert_eq!(moved.prior, CollectionPrior::Parent(None));
        assert_eq!(moved.prior.container_id(), None);
        assert_eq!(moved.row.container_id.as_deref(), Some(main.as_str()));
    }

    pub fn reparent_in_is_ignored_by_a_binding_with_no_container_axis(store: &dyn AttributedStore) {
        let library = make_container(store, "Main");
        let parent = create(store, &MANUSCRIPT_COLLECTION, "P", None, None, None).unwrap();
        let child = create(store, &MANUSCRIPT_COLLECTION, "C", None, None, None).unwrap();
        let moved = reparent_in(
            store,
            &MANUSCRIPT_COLLECTION,
            &child.id,
            Some(&parent.id),
            Some(&library),
        )
        .unwrap();
        assert_eq!(
            moved.prior,
            CollectionPrior::Parent(None),
            "no container axis ⇒ the plain prior, so imprint's undo is unchanged"
        );
        let item = store.get(parse_id(&child.id).unwrap()).unwrap().unwrap();
        assert_eq!(item.parent, None, "and no envelope write happened at all");
    }

    pub fn is_smart_is_reported_per_row_and_only_where_the_schema_has_the_field(
        store: &dyn AttributedStore,
    ) {
        let library = make_container(store, "Main");
        let manual = create_in(
            store,
            &IMBIB_COLLECTION,
            "Manual",
            None,
            None,
            None,
            Some(&library),
        )
        .unwrap();
        assert!(!manual.is_smart, "the kernel never writes the flag");

        // Written the way imbib-core's `create_collection(is_smart: true)` does.
        store
            .update(
                parse_id(&manual.id).unwrap(),
                vec![FieldMutation::SetPayload(
                    "is_smart".into(),
                    Value::Bool(true),
                )],
            )
            .unwrap();
        let listed = row(&list_tree(store, &IMBIB_COLLECTION).unwrap(), &manual.id);
        assert!(listed.is_smart);

        // A binding whose schema has no such field reports false even if a
        // stray payload key exists — the axis is the BINDING's declaration.
        let folder = create(store, &FIGURE_COLLECTION, "Figures", None, None, None).unwrap();
        store
            .update(
                parse_id(&folder.id).unwrap(),
                vec![FieldMutation::SetPayload(
                    "is_smart".into(),
                    Value::Bool(true),
                )],
            )
            .unwrap();
        assert!(
            !row(&list_tree(store, &FIGURE_COLLECTION).unwrap(), &folder.id).is_smart,
            "figure-collection declares no smart_field"
        );
    }

    pub fn restore_puts_a_smart_collection_back_smart(store: &dyn AttributedStore) {
        let library = make_container(store, "Main");
        let smart = create_in(
            store,
            &IMBIB_COLLECTION,
            "Recent",
            None,
            None,
            None,
            Some(&library),
        )
        .unwrap();
        store
            .update(
                parse_id(&smart.id).unwrap(),
                vec![FieldMutation::SetPayload(
                    "is_smart".into(),
                    Value::Bool(true),
                )],
            )
            .unwrap();

        let snapshot = delete(store, &IMBIB_COLLECTION, &smart.id).unwrap();
        assert!(snapshot.row.is_smart, "the snapshot carries the flag");
        let restored = restore(store, &IMBIB_COLLECTION, &snapshot).unwrap();
        assert!(
            restored.is_smart,
            "undoing the delete of a smart collection must not demote it"
        );
        assert_eq!(restored.container_id.as_deref(), Some(library.as_str()));
    }
}

macro_rules! on_each_backend {
    ($($name:ident),* $(,)?) => {
        mod memory {
            use super::*;
            $(
                #[test]
                fn $name() {
                    let store = MemoryStore::new();
                    cases::$name(&store);
                }
            )*
        }
        mod dir {
            use super::*;
            $(
                #[test]
                fn $name() {
                    let tmp = tempfile::tempdir().unwrap();
                    let store = DirStore::open(tmp.path()).unwrap();
                    cases::$name(&store);
                }
            )*
        }
    };
}

on_each_backend!(
    reparent_rejects_self_parent,
    reparent_rejects_direct_child,
    reparent_rejects_deep_descendant,
    reparent_cycle_check_walks_envelope_chain_for_figures,
    generic_binding_round_trip,
    manuscript_binding_round_trip,
    create_inherits_owning_library_from_parent_collection,
    mixed_kind_membership_over_any_scope,
    figure_membership_uses_envelope_parent,
    bindings_reject_items_of_another_schema,
    ids_are_normalized_to_lowercase,
    create_rejects_unknown_parent,
    create_writes_the_requested_sort_order,
    delete_restore_round_trips_a_contains_binding,
    delete_restore_round_trips_the_envelope_binding,
    restore_skips_members_that_are_gone,
    impress_gate_mixed_kind_collection_round_trip,
    an_unmarked_store_resolves_every_binding_to_its_legacy_self,
    author_matches_store_default,
    the_marker_resolves_legacy_bindings_to_the_generic_schema,
    create_in_files_a_root_collection_under_its_container,
    create_in_inherits_the_container_when_none_is_named,
    create_in_is_ignored_by_a_binding_with_no_container_axis,
    list_tree_in_scopes_to_one_container_and_list_tree_still_sees_all,
    list_tree_in_ignores_the_container_for_a_binding_without_one,
    reparent_in_moves_tree_and_container_together_and_inverts_exactly,
    a_same_container_reparent_leaves_the_container_alone,
    reparent_in_is_ignored_by_a_binding_with_no_container_axis,
    is_smart_is_reported_per_row_and_only_where_the_schema_has_the_field,
    restore_puts_a_smart_collection_back_smart,
);
