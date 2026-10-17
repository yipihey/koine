//! Cases for `neighbors`: the walk over references.
//!
//! Results are compared as sets. A node reachable by two paths may come back
//! twice from a backend that does not de-duplicate a level, and the port does
//! not promise otherwise; what it does promise is *which* items, and that the
//! start item is never among them.

use std::collections::BTreeSet;

use impress_item::{AttributedStore, EdgeType, FieldMutation, ItemId};

use crate::fixture::*;

struct Graph {
    a: ItemId,
    b: ItemId,
    c: ItemId,
    d: ItemId,
    e: ItemId,
}

fn link(store: &dyn AttributedStore, from: ItemId, to: ItemId, edge: EdgeType) {
    store
        .update(from, vec![FieldMutation::AddReference(reference(to, edge))])
        .unwrap();
}

/// ```text
/// D --fixture-link--> A --Cites--> B --Cites--> C
///                                  ^
///                     E --Cites----+
/// ```
fn graph(store: &dyn AttributedStore) -> Graph {
    let [a, b, c, d, e] = ["A", "B", "C", "D", "E"].map(|t| insert(store, "test", t));
    link(store, a, b, EdgeType::Cites);
    link(store, b, c, EdgeType::Cites);
    link(store, d, a, EdgeType::Custom("fixture-link".into()));
    link(store, e, b, EdgeType::Cites);
    Graph { a, b, c, d, e }
}

fn walk(
    store: &dyn AttributedStore,
    from: ItemId,
    edges: &[EdgeType],
    depth: u32,
) -> BTreeSet<ItemId> {
    let found = store.neighbors(from, edges, depth).unwrap();
    assert!(
        found.iter().all(|i| i.id != from),
        "the start item is never its own neighbour"
    );
    id_set(&found)
}

/// Breadth-first over references in both directions, to a depth, along the
/// named edge types (none named means all of them, custom ones included).
pub fn neighbors_walk_both_directions_to_a_depth(store: &dyn AttributedStore) {
    let g = graph(store);
    let ids = |xs: &[ItemId]| xs.iter().copied().collect::<BTreeSet<_>>();

    assert!(
        walk(store, g.a, &[], 0).is_empty(),
        "depth 0 reaches nothing"
    );
    // From A: out to B, in from D.
    assert_eq!(walk(store, g.a, &[], 1), ids(&[g.b, g.d]));
    // One step further: B leads to C (out) and E (in); D leads nowhere new.
    assert_eq!(walk(store, g.a, &[], 2), ids(&[g.b, g.d, g.c, g.e]));
    assert_eq!(
        walk(store, g.a, &[], 9),
        ids(&[g.b, g.d, g.c, g.e]),
        "a deep walk stops at the edge of the graph"
    );
    // The walk is symmetric: B's neighbours include the sources that cite it.
    assert_eq!(walk(store, g.b, &[], 1), ids(&[g.a, g.c, g.e]));
    assert_eq!(walk(store, g.c, &[], 1), ids(&[g.b]));
}

/// The edge filter keeps the walk on the named edge types.
pub fn neighbors_follow_only_the_named_edge_types(store: &dyn AttributedStore) {
    let g = graph(store);
    let ids = |xs: &[ItemId]| xs.iter().copied().collect::<BTreeSet<_>>();
    let cites = [EdgeType::Cites];
    let custom = [EdgeType::Custom("fixture-link".into())];

    assert_eq!(walk(store, g.a, &cites, 1), ids(&[g.b]));
    assert_eq!(walk(store, g.a, &cites, 2), ids(&[g.b, g.c, g.e]));
    assert_eq!(walk(store, g.a, &custom, 1), ids(&[g.d]));
    assert_eq!(
        walk(store, g.a, &custom, 3),
        ids(&[g.d]),
        "nothing else is linked that way"
    );
    assert!(walk(store, g.a, &[EdgeType::Custom("absent".into())], 2).is_empty());
    assert!(walk(store, g.a, &[EdgeType::Mentions], 2).is_empty());
    // Several edge types at once.
    assert_eq!(
        walk(
            store,
            g.a,
            &[EdgeType::Cites, EdgeType::Custom("fixture-link".into())],
            1
        ),
        ids(&[g.b, g.d])
    );
}

/// A cycle does not bring the start item back, and the walk terminates.
pub fn neighbors_never_return_the_start_item_even_in_a_cycle(store: &dyn AttributedStore) {
    let g = graph(store);
    link(store, g.c, g.a, EdgeType::Cites); // A -> B -> C -> A
    let found = walk(store, g.a, &[EdgeType::Cites], 5);
    assert_eq!(found, [g.b, g.c, g.e].into_iter().collect());
    // Self-reference.
    link(store, g.a, g.a, EdgeType::RelatesTo);
    assert!(walk(store, g.a, &[EdgeType::RelatesTo], 3).is_empty());
}

/// The items come back whole, and a start that is not stored has no neighbours.
pub fn neighbors_return_whole_items(store: &dyn AttributedStore) {
    let g = graph(store);
    store
        .update(
            g.b,
            vec![
                FieldMutation::AddTag("t".into()),
                FieldMutation::SetRead(true),
            ],
        )
        .unwrap();
    let found = store.neighbors(g.a, &[EdgeType::Cites], 1).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, g.b);
    assert_eq!(found[0].tags, vec!["t".to_owned()]);
    assert!(found[0].is_read);
    assert_eq!(found[0].references.len(), 1);

    assert!(store
        .neighbors(uuid::Uuid::new_v4(), &[], 3)
        .unwrap()
        .is_empty());
}

/// A deleted item is no longer anyone's neighbour.
pub fn neighbors_forget_deleted_items(store: &dyn AttributedStore) {
    let g = graph(store);
    store.delete(g.b).unwrap();
    assert!(walk(store, g.a, &[EdgeType::Cites], 3).is_empty());
    assert_eq!(walk(store, g.a, &[], 3), [g.d].into_iter().collect());
    assert!(walk(store, g.c, &[], 3).is_empty());
}

// ------------------------------------------------- references_to (KM-D8)

/// Every source of an edge into an item, whatever kind or edge type, each
/// edge once, ordered by source then edge type.
pub fn references_to_finds_every_source_of_an_edge_into_an_item(store: &dyn AttributedStore) {
    let g = graph(store);
    // B is cited by A and by E.
    let mut want = vec![(g.a, EdgeType::Cites), (g.e, EdgeType::Cites)];
    want.sort_by_key(|x| x.0);
    assert_eq!(store.references_to(g.b, &[]).unwrap(), want);
    // A is the target of D's custom edge only; C of B's citation only.
    assert_eq!(
        store.references_to(g.a, &[]).unwrap(),
        vec![(g.d, EdgeType::Custom("fixture-link".into()))]
    );
    assert_eq!(
        store.references_to(g.c, &[]).unwrap(),
        vec![(g.b, EdgeType::Cites)]
    );
    // Nothing points at D or E.
    assert!(store.references_to(g.d, &[]).unwrap().is_empty());
    assert!(store.references_to(g.e, &[]).unwrap().is_empty());
}

/// Naming edge types keeps only edges of those types.
pub fn references_to_filters_by_edge_type(store: &dyn AttributedStore) {
    let g = graph(store);
    link(store, g.c, g.a, EdgeType::Supersedes);
    let custom = EdgeType::Custom("fixture-link".into());
    assert_eq!(
        store.references_to(g.a, &[EdgeType::Supersedes]).unwrap(),
        vec![(g.c, EdgeType::Supersedes)]
    );
    assert_eq!(
        store
            .references_to(g.a, std::slice::from_ref(&custom))
            .unwrap(),
        vec![(g.d, custom.clone())]
    );
    assert_eq!(
        store
            .references_to(g.a, &[EdgeType::Supersedes, custom])
            .unwrap()
            .len(),
        2
    );
    assert!(store
        .references_to(g.a, &[EdgeType::Cites])
        .unwrap()
        .is_empty());
}

/// A deleted source takes its edges with it; removing an edge removes it.
pub fn references_to_forgets_deleted_sources_and_removed_edges(store: &dyn AttributedStore) {
    let g = graph(store);
    store.delete(g.e).unwrap();
    assert_eq!(
        store.references_to(g.b, &[]).unwrap(),
        vec![(g.a, EdgeType::Cites)]
    );
    store
        .update(
            g.a,
            vec![FieldMutation::RemoveReference(g.b, EdgeType::Cites)],
        )
        .unwrap();
    assert!(store.references_to(g.b, &[]).unwrap().is_empty());
}
