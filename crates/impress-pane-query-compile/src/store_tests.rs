//! The compiled query run against real rows, on the memory store.
//!
//! Ported from `impress-core`'s `store_tests` (which ran them on
//! `SqliteItemStore` and stays there for the tests that need the SQLite-only
//! kernels: `collection_ops`, `triage_ops`, FTS). Collections here are plain
//! rows with `Contains` edges, which is all `collection_ops` writes.
//!
//! The shape of each test is the shape of a real sidebar node or detail pane
//! (ADR-0031 D2): compile a [`PaneQuery`], run `item_query` through the port,
//! compare ids.

use std::collections::BTreeMap;

use impress_item::{
    ActorKind, AttributedStore, EdgeType, FieldMutation, Item, ItemId, ItemQuery, ItemStore,
    MutationKind, OperationIntent, OperationSpec, OperationType, Predicate, Priority,
    RetentionTier, SchemaRef, StoreMutation, TypedReference, Value, Visibility,
};
use impress_pane_query::{
    Bindings, Direction, Filter, ItemRef, PaneQuery, ParamDecl, RelationWalk, Scope, SortKey,
};
use koine_store_memory::MemoryStore;

use crate::test_support::builtin_manifest;
use crate::{compile, compile_with, CompiledQuery};

const PUBLICATION: &str = "imbib/bibliography-entry";
const COLLECTION: &str = "imbib/collection";
const LIBRARY: &str = "imbib/library";

fn open() -> MemoryStore {
    MemoryStore::new()
}

fn row(schema: &str, title: &str, refs: Vec<TypedReference>) -> Item {
    let now = chrono::Utc::now();
    Item {
        id: uuid::Uuid::new_v4(),
        schema: SchemaRef::from_stored(schema.to_owned()),
        payload: BTreeMap::from([("title".to_owned(), Value::String(title.to_owned()))]),
        created: now,
        modified: now,
        author: "test@example.com".into(),
        author_kind: ActorKind::Human,
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
        references: refs,
        parent: None,
        extra: Default::default(),
    }
}

fn insert(store: &MemoryStore, schema: &str, title: &str, refs: Vec<TypedReference>) -> ItemId {
    store.insert(row(schema, title, refs)).expect("insert")
}

fn publication(store: &MemoryStore, title: &str) -> ItemId {
    insert(store, PUBLICATION, title, vec![])
}

fn edge(target: ItemId, edge_type: EdgeType) -> TypedReference {
    TypedReference {
        target,
        edge_type,
        metadata: None,
    }
}

fn contains(members: &[ItemId]) -> Vec<TypedReference> {
    members
        .iter()
        .map(|m| edge(*m, EdgeType::Contains))
        .collect()
}

fn sorted(mut ids: Vec<ItemId>) -> Vec<ItemId> {
    ids.sort();
    ids
}

fn run(store: &MemoryStore, compiled: &CompiledQuery) -> Vec<ItemId> {
    sorted(
        store
            .query(&compiled.item_query)
            .expect("query runs")
            .into_iter()
            .map(|i| i.id)
            .collect(),
    )
}

fn compiled(query: &PaneQuery) -> CompiledQuery {
    compile(query, &[], &Bindings::new(), &builtin_manifest()).expect("compiles")
}

fn of_kind(kind: &str) -> PaneQuery {
    PaneQuery {
        kinds: vec![kind.into()],
        ..Default::default()
    }
}

/// The direction claim, proved against rows rather than asserted: a
/// collection holds its members by a `Contains` edge FROM the collection, and
/// the compiler reads it back.
#[test]
fn collection_scope_returns_the_members_the_collection_holds() {
    let store = open();
    let a = publication(&store, "Member A");
    let b = publication(&store, "Member B");
    let outsider = publication(&store, "Not a member");
    let collection = insert(&store, COLLECTION, "Reading", contains(&[a, b]));

    let c = compiled(&PaneQuery {
        scope: Scope::Collection {
            id: ItemRef::Id { id: collection },
        },
        ..of_kind("publication")
    });
    assert_eq!(run(&store, &c), sorted(vec![a, b]));
    assert!(!run(&store, &c).contains(&outsider));
}

#[test]
fn collection_subtree_scope_reaches_descendant_collections() {
    let store = open();
    let top_member = publication(&store, "Filed at the top");
    let deep_member = publication(&store, "Filed two levels down");
    let elsewhere = publication(&store, "Filed nowhere");

    let top = insert(&store, COLLECTION, "Top", contains(&[top_member]));
    let mut mid_row = row(COLLECTION, "Mid", vec![]);
    mid_row.parent = Some(top);
    let mid = store.insert(mid_row).expect("mid");
    let mut leaf_row = row(COLLECTION, "Leaf", contains(&[deep_member]));
    leaf_row.parent = Some(mid);
    store.insert(leaf_row).expect("leaf");

    let query = PaneQuery {
        scope: Scope::CollectionSubtree {
            id: ItemRef::Id { id: top },
        },
        ..of_kind("publication")
    };

    // Without a resolver, a subtree is the folder's own members only.
    let shallow = compiled(&query);
    assert_eq!(run(&store, &shallow), vec![top_member]);
    assert!(!run(&store, &shallow).contains(&elsewhere));

    // With one that walks the real tree (here: the envelope parent of the
    // collection rows), the descendants come too.
    let walk = |start: ItemId| {
        let mut out = vec![start];
        let mut i = 0;
        while i < out.len() {
            let children = store
                .query(&ItemQuery {
                    schema: Some(SchemaRef::from_stored(COLLECTION.to_owned())),
                    predicates: vec![Predicate::HasParent(out[i])],
                    ..Default::default()
                })
                .expect("children");
            for child in children {
                if !out.contains(&child.id) {
                    out.push(child.id);
                }
            }
            i += 1;
        }
        out
    };
    let deep =
        compile_with(&query, &[], &Bindings::new(), &builtin_manifest(), &walk).expect("compiles");
    assert_eq!(run(&store, &deep), sorted(vec![top_member, deep_member]));
}

/// Both directions of one real `Cites` edge. Getting these backwards is
/// invisible in a unit test and shows up as an empty "cited in" pane.
#[test]
fn a_cites_edge_resolves_in_both_directions() {
    let store = open();
    let paper = publication(&store, "The cited paper");
    let _unrelated = publication(&store, "An uncited paper");
    let manuscript = insert(
        &store,
        "manuscript",
        "The citing manuscript",
        vec![edge(paper, EdgeType::Cites)],
    );

    // Papers this manuscript cites.
    let outgoing = compiled(&PaneQuery {
        relation: Some(RelationWalk {
            edge: EdgeType::Cites.into(),
            from: ItemRef::Id { id: manuscript },
            direction: Direction::Outgoing,
        }),
        ..of_kind("publication")
    });
    assert_eq!(run(&store, &outgoing), vec![paper]);

    // Manuscripts citing this paper.
    let incoming = compiled(&PaneQuery {
        relation: Some(RelationWalk {
            edge: EdgeType::Cites.into(),
            from: ItemRef::Id { id: paper },
            direction: Direction::Incoming,
        }),
        ..of_kind("manuscript")
    });
    assert_eq!(run(&store, &incoming), vec![manuscript]);
}

/// A kind that spans two refs must find BOTH: this is the `In` path, and
/// dropping it would silently return every kind in the store.
#[test]
fn a_multi_ref_kind_finds_every_one_of_its_refs_and_nothing_else() {
    let store = open();
    let email = insert(&store, "email-message", "An email", vec![]);
    let chat = insert(&store, "chat-message", "A chat", vec![]);
    let _paper = publication(&store, "Not a message");

    let c = compiled(&of_kind("message"));
    assert_eq!(run(&store, &c), sorted(vec![email, chat]));
}

#[test]
fn item_scope_returns_exactly_that_row() {
    let store = open();
    let a = publication(&store, "A");
    let _b = publication(&store, "B");
    let c = compiled(&PaneQuery {
        scope: Scope::Item {
            id: ItemRef::Id { id: a },
        },
        ..of_kind("publication")
    });
    assert_eq!(c.single_item, Some(a));
    assert_eq!(run(&store, &c), vec![a]);
}

#[test]
fn parent_scope_returns_the_library_the_rows_are_filed_in() {
    let store = open();
    let library = insert(&store, LIBRARY, "Main", vec![]);
    let other = insert(&store, LIBRARY, "Other", vec![]);
    let mine = publication(&store, "Filed here");
    let theirs = publication(&store, "Filed there");
    store
        .update(mine, vec![FieldMutation::SetParent(Some(library))])
        .expect("file");
    store
        .update(theirs, vec![FieldMutation::SetParent(Some(other))])
        .expect("file");

    let c = compiled(&PaneQuery {
        scope: Scope::Parent {
            id: ItemRef::Id { id: library },
        },
        ..of_kind("publication")
    });
    assert_eq!(run(&store, &c), vec![mine]);
}

/// A library holds a paper by parent or by a `Contains` edge from the
/// library (a paper parented to Save that is also in the Inbox). The library
/// pane lists both; a paper merely parented elsewhere is not in it.
#[test]
fn a_library_pane_lists_its_contains_linked_papers_too() {
    let store = open();
    let parented = publication(&store, "Parented here");
    let linked = publication(&store, "Parented elsewhere, linked here");
    let elsewhere = publication(&store, "Parented elsewhere only");
    let library = insert(&store, LIBRARY, "Inbox", contains(&[linked]));
    let save = insert(&store, LIBRARY, "Save", vec![]);
    for (paper, parent) in [(parented, library), (linked, save), (elsewhere, save)] {
        store
            .update(paper, vec![FieldMutation::SetParent(Some(parent))])
            .expect("file");
    }

    let c = compiled(&PaneQuery {
        scope: Scope::Parent {
            id: ItemRef::Id { id: library },
        },
        ..of_kind("publication")
    });
    assert_eq!(run(&store, &c), sorted(vec![parented, linked]));
}

/// A figure folder files by the envelope parent alone, so a `Contains` edge
/// from the folder does not put a figure in it.
#[test]
fn a_figure_folder_pane_is_still_its_envelope_children() {
    let store = open();
    let filed = insert(&store, "figure", "Filed", vec![]);
    let linked = insert(&store, "figure", "Only linked", vec![]);
    let folder = insert(&store, "figure-collection", "Folder", contains(&[linked]));
    store
        .update(filed, vec![FieldMutation::SetParent(Some(folder))])
        .expect("file");

    let c = compiled(&PaneQuery {
        scope: Scope::Parent {
            id: ItemRef::Id { id: folder },
        },
        ..of_kind("figure")
    });
    assert_eq!(c.item_query.predicates, vec![Predicate::HasParent(folder)]);
    assert_eq!(run(&store, &c), vec![filed]);
}

#[test]
fn a_status_filter_finds_the_dismissed_rows() {
    let store = open();
    let live = insert(&store, "manuscript", "Live", vec![]);
    let dead = insert(&store, "manuscript", "Dismissed", vec![]);
    store
        .update(
            dead,
            vec![FieldMutation::SetPayload(
                "status".into(),
                Value::String("dismissed".into()),
            )],
        )
        .expect("set status");

    let c = compiled(&PaneQuery {
        filters: vec![Filter::Status {
            status: "dismissed".into(),
        }],
        ..of_kind("manuscript")
    });
    assert_eq!(run(&store, &c), vec![dead]);
    assert!(!run(&store, &c).contains(&live));
}

#[test]
fn flag_starred_read_and_tag_filters_reach_the_envelope() {
    let store = open();
    let plain = publication(&store, "Plain");
    let starred = publication(&store, "Starred");
    let read = publication(&store, "Read");
    let tagged = publication(&store, "Tagged");
    store
        .update(starred, vec![FieldMutation::SetStarred(true)])
        .unwrap();
    store
        .update(read, vec![FieldMutation::SetRead(true)])
        .unwrap();
    store
        .update(tagged, vec![FieldMutation::AddTag("methods/sims".into())])
        .unwrap();

    let with = |filter: Filter| {
        run(
            &store,
            &compiled(&PaneQuery {
                filters: vec![filter],
                ..of_kind("publication")
            }),
        )
    };
    assert_eq!(with(Filter::Starred { starred: true }), vec![starred]);
    assert_eq!(with(Filter::Read { read: true }), vec![read]);
    assert_eq!(
        with(Filter::Tag {
            path: "methods/sims".into()
        }),
        vec![tagged]
    );
    let unread = with(Filter::Read { read: false });
    assert!(unread.contains(&plain) && !unread.contains(&read));
}

#[test]
fn a_text_term_matches_titles() {
    let store = open();
    let hit = publication(&store, "Scaling relations in dark matter haloes");
    let _miss = publication(&store, "Stellar populations of dwarf galaxies");

    let c = compiled(&PaneQuery {
        text: Some("dark matter".into()),
        ..of_kind("publication")
    });
    assert_eq!(run(&store, &c), vec![hit]);
}

/// The bug this whole compiler exists to prevent, run end to end: an unbound
/// optional parameter must return NOTHING, not everything.
#[test]
fn an_unbound_optional_param_returns_no_rows_rather_than_all_of_them() {
    let store = open();
    let _a = publication(&store, "A");
    let _b = publication(&store, "B");

    let c = compile(
        &PaneQuery {
            scope: Scope::Item {
                id: ItemRef::Param {
                    name: "item".into(),
                },
            },
            ..of_kind("publication")
        },
        &[ParamDecl {
            name: "item".into(),
            kind: "publication".into(),
            required: false,
        }],
        &Bindings::new(),
        &builtin_manifest(),
    )
    .expect("compiles");
    assert!(run(&store, &c).is_empty());
}

#[test]
fn sort_and_limit_reach_the_store() {
    let store = open();
    let _a = publication(&store, "First");
    let _b = publication(&store, "Second");
    let _c = publication(&store, "Third");

    let c = compiled(&PaneQuery {
        sort: vec![SortKey {
            field: "payload.title".into(),
            descending: false,
        }],
        limit: Some(2),
        ..of_kind("publication")
    });
    let titles: Vec<String> = store
        .query(&c.item_query)
        .expect("query")
        .into_iter()
        .filter_map(|i| match i.payload.get("title") {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(titles, vec!["First".to_string(), "Second".to_string()]);
}

/// The compiler targets a store it has never heard of, so a query over
/// no kind the store holds is empty, not an error and not everything.
#[test]
fn a_kind_the_store_holds_no_rows_of_is_empty() {
    let store = open();
    let _paper = publication(&store, "A paper");
    assert!(run(&store, &compiled(&of_kind("task"))).is_empty());
}

/// Invalidation end to end: the mutation a store write produces wakes the
/// pane that shows its effect, and re-running the pane shows it. Filing a
/// paper into a collection mutates the COLLECTION row, whose schema ref is
/// outside the pane's own `schema_refs`.
#[test]
fn filing_a_paper_into_a_collection_wakes_the_collection_pane_and_shows_it() {
    let store = open();
    let member = publication(&store, "Already in");
    let newcomer = publication(&store, "Filed later");
    let collection = insert(&store, COLLECTION, "Reading", contains(&[member]));

    let pane = compiled(&PaneQuery {
        scope: Scope::Collection {
            id: ItemRef::Id { id: collection },
        },
        ..of_kind("publication")
    });
    assert_eq!(run(&store, &pane), vec![member]);

    // The write, as the port performs it.
    store
        .apply_operation(OperationSpec {
            target_id: collection,
            op_type: OperationType::AddReference(edge(newcomer, EdgeType::Contains)),
            intent: OperationIntent::Routine,
            reason: None,
            batch_id: None,
            author: "test".into(),
            author_kind: ActorKind::Human,
            retention: RetentionTier::Durable,
        })
        .expect("file the paper");

    // The mutation a host derives from it.
    let filed = StoreMutation::new(
        collection,
        Some(COLLECTION.to_owned()),
        MutationKind::ReferenceAdded {
            edge: EdgeType::Contains,
            source: collection,
            target: newcomer,
        },
    );
    assert!(pane.invalidation.is_affected_by(&filed));
    assert_eq!(run(&store, &pane), sorted(vec![member, newcomer]));

    // A write to an unrelated kind wakes nothing.
    let note = StoreMutation::new(
        uuid::Uuid::new_v4(),
        Some("manuscript".to_owned()),
        MutationKind::Updated,
    );
    assert!(!pane.invalidation.is_affected_by(&note));
}
