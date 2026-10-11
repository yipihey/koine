//! What a compiled pane query depends on, and whether a given store
//! mutation can have changed its result (ADR-0031 D9).
//!
//! D9's prerequisite sentence: *"The prerequisite Rust work is
//! incremental query results from the event bus, so a pane re-runs only
//! when a mutation touches its query; without that, every mutation
//! re-runs every pane and the startup render-loop invariant is re-tripped
//! at scale."* That invariant is the 60–90 s startup delay every impress
//! background service carries: a `.storeDidMutate` fan-out during the
//! first seconds of launch compounds into a perpetual render loop. A
//! twenty-pane window that re-queries on every mutation is the same
//! failure with a different trigger.
//!
//! # The key is not just the schema ref
//!
//! [`CompiledQuery::schema_refs`] is the documented invalidation key and
//! it is right for the ordinary case: a mutation on
//! `imbib/bibliography-entry` cannot change a pane that queries
//! `manuscript`. But two of the algebra's five scopes, and its relation
//! walk, depend on things a schema ref does not name:
//!
//! | dependency | changed by | the mutation's schema ref is |
//! |---|---|---|
//! | collection membership | `Contains` edge add / remove | the **collection**'s |
//! | subtree membership | ditto, plus the folder tree moving | the collection's |
//! | `Scope::Parent` | an item being re-parented | the moved item's |
//! | relation walk | a `Cites` edge add / remove | the **citing** item's |
//!
//! A publication list scoped to a collection has `schema_refs =
//! ["imbib/bibliography-entry"]`; filing a paper into that collection
//! targets the COLLECTION row (`collection_ops` writes membership as a
//! `Contains` edge from the collection to the member), so the mutation
//! carries `imbib/collection` and the schema-ref test says "cannot affect
//! this pane". It affects it completely. [`Invalidation`] is the rest of
//! the key.
//!
//! # Conservative by construction
//!
//! Every rule below errs toward `true`. A pane that re-runs when it did
//! not have to costs one query; a pane that fails to re-run shows the
//! user rows that are no longer true and gives no sign of it — the same
//! silent-wrongness class as a misspelled schema ref. So an
//! undeterminable schema ref is treated as matching (this is also what
//! `SqliteItemStore::emit` does with a schemaless event), and a mutation
//! on an id the query names literally invalidates whatever its schema.

use crate::compile::CompiledQuery;
use impress_item::{EdgeType, ItemId, MutationKind, StoreMutation};
use serde::{Deserialize, Serialize};

/// Everything a compiled query depends on.
///
/// Built by [`crate::compile`] in lockstep with the predicates it
/// describes and carried on [`CompiledQuery::invalidation`], so the
/// dependency set can never disagree with the query it belongs to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invalidation {
    /// Every schema ref the query can return, from the kind manifest.
    /// A create / update / delete on one of these can change the result.
    #[serde(default)]
    pub schema_refs: Vec<String>,
    /// `(edge type, item)` pairs the result is computed over: the
    /// `Contains` edge of a collection or subtree scope, and the walked
    /// edge of a relation walk. An edge of this type touching this item
    /// — at either end — can change the result.
    #[serde(default)]
    pub edges: Vec<(EdgeType, ItemId)>,
    /// The parents a `Scope::Parent` query selects under. An item moving
    /// into or out of one of these changes the result.
    #[serde(default)]
    pub parents: Vec<ItemId>,
    /// Every item the query names literally: the `Scope::Item` id and
    /// every fixed or bound id in any other position (the collection, the
    /// parent, the relation anchor). Mutating one of these can change
    /// the result whatever its schema — the compiled `ItemQuery` sets
    /// `include_references: true`, so the pane's rows carry their edges
    /// and their parent, not only their payload.
    #[serde(default)]
    pub items: Vec<ItemId>,
    /// The query's result is confined to the ids in [`Self::items`]:
    /// `Scope::Item`, whose whole population is one named row.
    ///
    /// Under it the schema-ref rule does not apply. A detail pane over
    /// one paper carries `imbib/bibliography-entry` in `schema_refs`
    /// because that is the kind it returns, but it can never show a
    /// second row, so every OTHER publication's edits are noise — and
    /// the detail pane is the one pane every window has, re-rendering
    /// the most expensive view in the suite. Rules 2 to 5 still apply,
    /// so the pane's own row, and the collection / parent / relation
    /// anchor it is computed from, still wake it.
    ///
    /// It is set for an UNBOUND `Scope::Item` too — the detail pane
    /// before the first selection, which compiles to "matches nothing"
    /// and stays empty until its binding changes, and a binding change
    /// is a recompile rather than an invalidation.
    #[serde(default)]
    pub narrowed: bool,
    /// The query reads the full-text index, so any content change on one
    /// of `schema_refs` can change the result.
    ///
    /// Today this is subsumed by the schema-ref rule, because
    /// [`MutationKind::Updated`] does not carry the field list and every
    /// update on a matching schema ref already invalidates. It is
    /// recorded anyway because it is the discriminator the *opposite*
    /// refinement needs: once an update says which fields moved, a pane
    /// with `text == false` can skip payload-only churn, and a pane with
    /// `text == true` cannot.
    #[serde(default)]
    pub text: bool,
}

impl Invalidation {
    /// Record a dependency on edges of `edge` touching `item`.
    pub(crate) fn depend_on_edge(&mut self, edge: EdgeType, item: ItemId) {
        let pair = (edge, item);
        if !self.edges.contains(&pair) {
            self.edges.push(pair);
        }
    }

    /// Record a dependency on `Scope::Parent` membership under `parent`.
    pub(crate) fn depend_on_parent(&mut self, parent: ItemId) {
        if !self.parents.contains(&parent) {
            self.parents.push(parent);
        }
    }

    /// Record a dependency on the row `item` itself.
    pub(crate) fn depend_on_item(&mut self, item: ItemId) {
        if !self.items.contains(&item) {
            self.items.push(item);
        }
    }

    /// Whether `m` can have changed this query's result.
    ///
    /// The rules, in order, each of them a *sufficient* condition:
    ///
    /// 1. a create / update / delete whose schema ref is in
    ///    [`Self::schema_refs`] — the ordinary case, and the one rule
    ///    [`Self::narrowed`] switches off;
    /// 2. any mutation of an id in [`Self::items`], whatever its schema —
    ///    the detail pane's own row, and the collection / parent /
    ///    relation anchor the query is computed from;
    /// 3. a parent change whose old or new parent is in
    ///    [`Self::parents`] — an item moving into or out of the pane;
    /// 4. a reference add / remove whose edge type AND one endpoint match
    ///    an [`Self::edges`] entry — membership and relation walks;
    /// 5. an update to a row that is an [`Self::edges`] anchor — renaming
    ///    a collection does not change its membership, but a folder move
    ///    changes what a subtree scope resolves to, and the compiler
    ///    cannot tell those apart from here;
    /// 6. an undeterminable schema ref on a create / update / delete —
    ///    conservative, and the same stance the event bus already takes.
    ///
    /// Anything else is `false`. In particular a mutation on kind A never
    /// re-runs a pane scoped to kind B, which is the property L4 exists
    /// to establish.
    pub fn is_affected_by(&self, m: &StoreMutation) -> bool {
        // Rule 2 — an id the query names literally, whatever its schema.
        if self.items.contains(&m.item_id) {
            return true;
        }

        match &m.kind {
            MutationKind::Created | MutationKind::Updated | MutationKind::Deleted => {
                // Rule 5 — the anchor row of an edge dependency.
                if self.edges.iter().any(|(_, anchor)| *anchor == m.item_id) {
                    return true;
                }
                // Rules 1 and 6 — unless the query is narrowed to the
                // ids rule 2 just checked, in which case no third row
                // of this kind exists to matter.
                if self.narrowed {
                    return false;
                }
                match &m.schema_ref {
                    None => true,
                    Some(s) => self.schema_refs.iter().any(|r| r == s),
                }
            }
            // Rule 3. A re-parent is NOT also a rule-1 match: an item
            // moving between two libraries neither of which this pane
            // selects cannot change what this pane shows, even though the
            // moved item is of the pane's kind.
            MutationKind::ParentChanged { old, new } => self
                .parents
                .iter()
                .any(|p| Some(*p) == *old || Some(*p) == *new),
            // Rule 4. Both endpoints are checked because a walk may be
            // `Outgoing` (the anchor is the edge's source) or `Incoming`
            // (the anchor is its target), and collection membership is an
            // edge FROM the collection.
            MutationKind::ReferenceAdded {
                edge,
                source,
                target,
            }
            | MutationKind::ReferenceRemoved {
                edge,
                source,
                target,
            } => self
                .edges
                .iter()
                .any(|(e, anchor)| e == edge && (anchor == source || anchor == target)),
        }
    }
}

/// The function spelling of [`CompiledQuery::invalidation`].
///
/// The field is authoritative — it is computed inside `compile`, where
/// the scopes are already resolved and cannot be re-read differently.
/// This exists so a call site that reads as "get the invalidation for
/// this query" can say so.
pub fn invalidation_for(compiled: &CompiledQuery) -> &Invalidation {
    &compiled.invalidation
}

/// The per-pane registry L3's `layout-service` and L5's FFI hold: which
/// subscribers depend on what, and which of them a mutation wakes.
///
/// Keyed by whatever the caller uses to name a pane — `TileId` in
/// `impress-layout`, a `String` by default.
///
/// # Why a linear scan
///
/// A schema-ref index would only narrow rule 1. Rules 2 to 5 are keyed
/// on item ids and edge types, so a mutation would still have to visit
/// every subscriber that names any item — which, for a window of
/// sidebar, list and detail panes, is all of them. A window holds tens
/// of panes, not thousands, and the scan is a few comparisons per pane.
/// Insertion order is preserved so that the affected set is
/// deterministic, which is what makes the tests readable.
#[derive(Debug, Clone)]
pub struct QuerySubscriptions<K = String> {
    entries: Vec<(K, Invalidation)>,
}

impl<K> Default for QuerySubscriptions<K> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl<K: Clone + PartialEq> QuerySubscriptions<K> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or replace) the dependency set for `key`. Returns the
    /// previous one, if the pane was already subscribed — a pane whose
    /// query is retyped re-registers rather than accumulating.
    pub fn insert(&mut self, key: K, invalidation: Invalidation) -> Option<Invalidation> {
        match self.entries.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => Some(std::mem::replace(&mut slot.1, invalidation)),
            None => {
                self.entries.push((key, invalidation));
                None
            }
        }
    }

    /// Register a pane from its compiled query.
    pub fn insert_compiled(&mut self, key: K, compiled: &CompiledQuery) -> Option<Invalidation> {
        self.insert(key, compiled.invalidation.clone())
    }

    /// Drop a pane's subscription (the pane was closed).
    pub fn remove(&mut self, key: &K) -> Option<Invalidation> {
        let idx = self.entries.iter().position(|(k, _)| k == key)?;
        Some(self.entries.remove(idx).1)
    }

    pub fn get(&self, key: &K) -> Option<&Invalidation> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, i)| i)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.entries.iter().map(|(k, _)| k)
    }

    /// The subscribers `m` wakes, in registration order.
    pub fn affected_by(&self, m: &StoreMutation) -> Vec<K> {
        self.entries
            .iter()
            .filter(|(_, inv)| inv.is_affected_by(m))
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// [`Self::affected_by`] over a batch, deduplicated: a batch of
    /// operations wakes each pane once, which is the point — coalescing
    /// here is what keeps a 500-row triage sweep from being 500 re-runs
    /// of every pane.
    pub fn affected_by_all<'a>(
        &self,
        mutations: impl IntoIterator<Item = &'a StoreMutation>,
    ) -> Vec<K> {
        let mut out: Vec<K> = Vec::new();
        let mutations: Vec<&StoreMutation> = mutations.into_iter().collect();
        for (key, inv) in &self.entries {
            if mutations.iter().any(|m| inv.is_affected_by(m)) {
                out.push(key.clone());
            }
        }
        out
    }
}

#[cfg(test)]
mod invalidation_tests {
    //! Table-driven, because the interesting content of L4 is a truth table
    //! and the only way to read a truth table is as one.

    use super::*;
    use crate::compile::{compile, compile_with, CompiledQuery};
    use crate::test_support::builtin_manifest;
    use impress_item::{ItemId, MutationKind, StoreMutation};
    use impress_pane_query::{
        Bindings, Direction, ItemRef, PaneQuery, ParamDecl, RecordKindId, RelationWalk, Scope,
    };
    use uuid::Uuid;

    const PUBLICATION: &str = "imbib/bibliography-entry";
    const MANUSCRIPT: &str = "manuscript";
    const COLLECTION: &str = "imbib/collection";

    fn compiled(query: &PaneQuery) -> CompiledQuery {
        compile(query, &[], &Bindings::new(), &builtin_manifest()).expect("compiles")
    }

    fn inv(query: &PaneQuery) -> Invalidation {
        compiled(query).invalidation
    }

    fn mutation(id: ItemId, schema: &str, kind: MutationKind) -> StoreMutation {
        StoreMutation::new(id, Some(schema.to_string()), kind)
    }

    fn kinds(k: &[&str]) -> Vec<RecordKindId> {
        k.iter().map(|s| (*s).to_string()).collect()
    }

    // ── What compile records ─────────────────────────────────────────────

    #[test]
    fn a_kind_scoped_pane_depends_on_its_schema_refs_and_nothing_else() {
        let i = inv(&PaneQuery {
            kinds: kinds(&["publication"]),
            ..Default::default()
        });
        assert_eq!(i.schema_refs, vec![PUBLICATION.to_string()]);
        assert!(i.edges.is_empty());
        assert!(i.parents.is_empty());
        assert!(i.items.is_empty());
        assert!(!i.text);
        assert!(!i.narrowed, "a kind-scoped list is not narrowed to ids");
    }

    #[test]
    fn only_an_item_scope_is_narrowed() {
        let id = Uuid::new_v4();
        let scoped = |scope: Scope| {
            inv(&PaneQuery {
                kinds: kinds(&["publication"]),
                scope,
                ..Default::default()
            })
        };
        assert!(
            scoped(Scope::Item {
                id: ItemRef::Id { id }
            })
            .narrowed
        );
        // A collection names a fixed id too, but its population is that
        // collection's members — unbounded, so rule 1 has to keep applying.
        for scope in [
            Scope::All,
            Scope::Collection {
                id: ItemRef::Id { id },
            },
            Scope::CollectionSubtree {
                id: ItemRef::Id { id },
            },
            Scope::Parent {
                id: ItemRef::Id { id },
            },
        ] {
            assert!(!scoped(scope.clone()).narrowed, "{scope:?}");
        }
    }

    #[test]
    fn a_collection_pane_depends_on_the_contains_edge_and_on_the_collection_row() {
        let c = Uuid::new_v4();
        let i = inv(&PaneQuery {
            kinds: kinds(&["publication"]),
            scope: Scope::Collection {
                id: ItemRef::Id { id: c },
            },
            ..Default::default()
        });
        assert_eq!(i.edges, vec![(EdgeType::Contains, c)]);
        assert_eq!(i.items, vec![c]);
    }

    #[test]
    fn a_subtree_pane_depends_on_every_collection_the_resolver_named() {
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let grandchild = Uuid::new_v4();
        let resolver = |r: ItemId| {
            assert_eq!(r, root);
            vec![root, child, grandchild]
        };
        let c = compile_with(
            &PaneQuery {
                kinds: kinds(&["publication"]),
                scope: Scope::CollectionSubtree {
                    id: ItemRef::Id { id: root },
                },
                ..Default::default()
            },
            &[],
            &Bindings::new(),
            &builtin_manifest(),
            &resolver,
        )
        .expect("compiles");
        assert_eq!(
            c.invalidation.edges,
            vec![
                (EdgeType::Contains, root),
                (EdgeType::Contains, child),
                (EdgeType::Contains, grandchild),
            ]
        );
    }

    #[test]
    fn a_parent_pane_depends_on_the_parent() {
        let lib = Uuid::new_v4();
        let i = inv(&PaneQuery {
            kinds: kinds(&["publication"]),
            scope: Scope::Parent {
                id: ItemRef::Id { id: lib },
            },
            ..Default::default()
        });
        assert_eq!(i.parents, vec![lib]);
        assert_eq!(i.items, vec![lib]);
    }

    #[test]
    fn a_relation_walk_depends_on_its_edge_type_in_both_directions() {
        let manuscript = Uuid::new_v4();
        for direction in [Direction::Outgoing, Direction::Incoming] {
            let i = inv(&PaneQuery {
                kinds: kinds(&["publication"]),
                relation: Some(RelationWalk {
                    edge: EdgeType::Cites.into(),
                    from: ItemRef::Id { id: manuscript },
                    direction,
                }),
                ..Default::default()
            });
            assert_eq!(
                i.edges,
                vec![(EdgeType::Cites, manuscript)],
                "{direction:?}"
            );
        }
    }

    #[test]
    fn a_text_pane_records_that_it_reads_the_index() {
        assert!(
            inv(&PaneQuery {
                kinds: kinds(&["publication"]),
                text: Some("dark matter".into()),
                ..Default::default()
            })
            .text
        );
        // Whitespace is not a search term, and the compiler drops it from
        // the predicates — the dependency has to agree.
        assert!(
            !inv(&PaneQuery {
                kinds: kinds(&["publication"]),
                text: Some("   ".into()),
                ..Default::default()
            })
            .text
        );
    }

    #[test]
    fn an_unbound_parameter_records_no_dependency() {
        // The pane compiles to "matches nothing" (ADR-0031 D3), and nothing
        // is exactly what can change it: there is no collection yet.
        let i = inv(&PaneQuery {
            kinds: kinds(&["publication"]),
            scope: Scope::Collection {
                id: ItemRef::Param {
                    name: "collection".into(),
                },
            },
            ..Default::default()
        });
        assert!(i.edges.is_empty());
        assert!(i.items.is_empty());
    }

    #[test]
    fn a_bound_parameter_records_the_id_it_resolved_to() {
        let c = Uuid::new_v4();
        let compiled = compile(
            &PaneQuery {
                kinds: kinds(&["publication"]),
                scope: Scope::Collection {
                    id: ItemRef::Param {
                        name: "collection".into(),
                    },
                },
                ..Default::default()
            },
            &[ParamDecl {
                name: "collection".into(),
                kind: "collection".into(),
                required: false,
            }],
            &Bindings::new().with("collection", c),
            &builtin_manifest(),
        )
        .expect("compiles");
        assert_eq!(compiled.invalidation.edges, vec![(EdgeType::Contains, c)]);
        assert_eq!(invalidation_for(&compiled).items, vec![c]);
    }

    // ── The truth table ──────────────────────────────────────────────────

    struct Case {
        name: &'static str,
        invalidation: Invalidation,
        mutation: StoreMutation,
        affected: bool,
    }

    #[test]
    fn is_affected_by_truth_table() {
        let collection = Uuid::new_v4();
        let other_collection = Uuid::new_v4();
        let library = Uuid::new_v4();
        let other_library = Uuid::new_v4();
        let manuscript = Uuid::new_v4();
        let paper = Uuid::new_v4();
        let unrelated = Uuid::new_v4();

        let publications = || PaneQuery {
            kinds: kinds(&["publication"]),
            ..Default::default()
        };
        let in_collection = || PaneQuery {
            kinds: kinds(&["publication"]),
            scope: Scope::Collection {
                id: ItemRef::Id { id: collection },
            },
            ..Default::default()
        };
        let in_library = || PaneQuery {
            kinds: kinds(&["publication"]),
            scope: Scope::Parent {
                id: ItemRef::Id { id: library },
            },
            ..Default::default()
        };
        let one_paper = || PaneQuery {
            kinds: kinds(&["publication"]),
            scope: Scope::Item {
                id: ItemRef::Id { id: paper },
            },
            ..Default::default()
        };
        let cited_by = || PaneQuery {
            kinds: kinds(&["publication"]),
            relation: Some(RelationWalk {
                edge: EdgeType::Cites.into(),
                from: ItemRef::Id { id: manuscript },
                direction: Direction::Outgoing,
            }),
            ..Default::default()
        };
        let manuscripts = || PaneQuery {
            kinds: kinds(&["manuscript"]),
            ..Default::default()
        };

        let cases = vec![
            // ── Rule 1: the schema ref ───────────────────────────────────
            Case {
                name: "a new publication invalidates the publication list",
                invalidation: inv(&publications()),
                mutation: mutation(unrelated, PUBLICATION, MutationKind::Created),
                affected: true,
            },
            Case {
                name: "an updated publication invalidates the publication list",
                invalidation: inv(&publications()),
                mutation: mutation(unrelated, PUBLICATION, MutationKind::Updated),
                affected: true,
            },
            Case {
                name: "a deleted publication invalidates the publication list",
                invalidation: inv(&publications()),
                mutation: mutation(unrelated, PUBLICATION, MutationKind::Deleted),
                affected: true,
            },
            // THE negative case the plan names (L4's row of the WP table):
            // "a mutation on kind A does not re-run a pane scoped to kind B".
            Case {
                name: "a manuscript mutation does NOT invalidate a publication pane",
                invalidation: inv(&publications()),
                mutation: mutation(unrelated, MANUSCRIPT, MutationKind::Created),
                affected: false,
            },
            Case {
                name: "a publication mutation does NOT invalidate a manuscript pane",
                invalidation: inv(&manuscripts()),
                mutation: mutation(unrelated, PUBLICATION, MutationKind::Updated),
                affected: false,
            },
            Case {
                name: "an operation-journal row does NOT invalidate a publication pane",
                invalidation: inv(&publications()),
                mutation: mutation(unrelated, "core/operation", MutationKind::Created),
                affected: false,
            },
            // ── Rule 2: an id the query names ────────────────────────────
            Case {
                name: "the detail pane's own row, updated",
                invalidation: inv(&one_paper()),
                mutation: mutation(paper, PUBLICATION, MutationKind::Updated),
                affected: true,
            },
            Case {
                name: "the detail pane's own row, deleted",
                invalidation: inv(&one_paper()),
                mutation: mutation(paper, PUBLICATION, MutationKind::Deleted),
                affected: true,
            },
            // The `narrowed` refinement. An `Item`-scope pane carries its
            // kind's schema refs because that is what it returns, but it can
            // never show a second row, so rule 1 is switched off for it —
            // and the detail pane is the one pane every window has, hosting
            // the most expensive view in the suite.
            Case {
                name: "another publication does NOT invalidate a narrowed pane",
                invalidation: inv(&one_paper()),
                mutation: mutation(unrelated, PUBLICATION, MutationKind::Updated),
                affected: false,
            },
            Case {
                name: "a narrowed pane still wakes for an edge at its own row",
                invalidation: inv(&one_paper()),
                mutation: mutation(
                    paper,
                    PUBLICATION,
                    MutationKind::ReferenceAdded {
                        edge: EdgeType::Cites,
                        source: paper,
                        target: unrelated,
                    },
                ),
                affected: true,
            },
            // An unbound detail pane is narrowed to the EMPTY set: it
            // compiles to "matches nothing" and stays empty until its
            // binding changes, and a binding change is a recompile rather
            // than an invalidation.
            Case {
                name: "a narrowed pane with no binding yet wakes for nothing",
                invalidation: inv(&PaneQuery {
                    kinds: kinds(&["publication"]),
                    scope: Scope::Item {
                        id: ItemRef::Param {
                            name: "item".into(),
                        },
                    },
                    ..Default::default()
                }),
                mutation: mutation(unrelated, PUBLICATION, MutationKind::Created),
                affected: false,
            },
            // ── Rule 5: the anchor row, whatever its schema ──────────────
            Case {
                name: "the scoped collection row itself, updated (a move may \
                       change what the subtree resolves to)",
                invalidation: inv(&in_collection()),
                mutation: mutation(collection, COLLECTION, MutationKind::Updated),
                affected: true,
            },
            Case {
                name: "a DIFFERENT collection row does NOT invalidate the pane",
                invalidation: inv(&in_collection()),
                mutation: mutation(other_collection, COLLECTION, MutationKind::Updated),
                affected: false,
            },
            // ── Rule 4: edges ────────────────────────────────────────────
            Case {
                name: "filing a paper into the scoped collection",
                invalidation: inv(&in_collection()),
                mutation: mutation(
                    collection,
                    COLLECTION,
                    MutationKind::ReferenceAdded {
                        edge: EdgeType::Contains,
                        source: collection,
                        target: unrelated,
                    },
                ),
                affected: true,
            },
            Case {
                name: "unfiling a paper from the scoped collection",
                invalidation: inv(&in_collection()),
                mutation: mutation(
                    collection,
                    COLLECTION,
                    MutationKind::ReferenceRemoved {
                        edge: EdgeType::Contains,
                        source: collection,
                        target: unrelated,
                    },
                ),
                affected: true,
            },
            Case {
                name: "filing into ANOTHER collection does not invalidate",
                invalidation: inv(&in_collection()),
                mutation: mutation(
                    other_collection,
                    COLLECTION,
                    MutationKind::ReferenceAdded {
                        edge: EdgeType::Contains,
                        source: other_collection,
                        target: unrelated,
                    },
                ),
                affected: false,
            },
            Case {
                name: "the RIGHT anchor with the WRONG edge type does not invalidate",
                invalidation: inv(&in_collection()),
                mutation: mutation(
                    collection,
                    COLLECTION,
                    MutationKind::ReferenceAdded {
                        edge: EdgeType::RelatesTo,
                        source: unrelated,
                        target: unrelated,
                    },
                ),
                affected: true, // rule 5: the anchor row is `collection`
            },
            Case {
                name: "the right edge type at the anchor's TARGET end invalidates",
                invalidation: inv(&cited_by()),
                mutation: mutation(
                    unrelated,
                    MANUSCRIPT,
                    MutationKind::ReferenceAdded {
                        edge: EdgeType::Cites,
                        source: unrelated,
                        target: manuscript,
                    },
                ),
                affected: true,
            },
            Case {
                name: "a Cites edge between two strangers does not invalidate the walk",
                invalidation: inv(&cited_by()),
                mutation: mutation(
                    unrelated,
                    MANUSCRIPT,
                    MutationKind::ReferenceAdded {
                        edge: EdgeType::Cites,
                        source: unrelated,
                        target: other_collection,
                    },
                ),
                affected: false,
            },
            Case {
                name: "a Contains edge does not invalidate a Cites walk",
                invalidation: inv(&cited_by()),
                mutation: mutation(
                    manuscript,
                    MANUSCRIPT,
                    MutationKind::ReferenceAdded {
                        edge: EdgeType::Contains,
                        source: manuscript,
                        target: unrelated,
                    },
                ),
                affected: true, // rule 5 again: `manuscript` is the anchor row
            },
            // ── Rule 3: parents ──────────────────────────────────────────
            Case {
                name: "a paper moved INTO the scoped library",
                invalidation: inv(&in_library()),
                mutation: mutation(
                    unrelated,
                    PUBLICATION,
                    MutationKind::ParentChanged {
                        old: Some(other_library),
                        new: Some(library),
                    },
                ),
                affected: true,
            },
            Case {
                name: "a paper moved OUT of the scoped library",
                invalidation: inv(&in_library()),
                mutation: mutation(
                    unrelated,
                    PUBLICATION,
                    MutationKind::ParentChanged {
                        old: Some(library),
                        new: None,
                    },
                ),
                affected: true,
            },
            Case {
                name: "a move between two libraries this pane does not select — \
                       and of the pane's OWN kind, which rule 1 would have \
                       caught if a re-parent were an ordinary update",
                invalidation: inv(&in_library()),
                mutation: mutation(
                    unrelated,
                    PUBLICATION,
                    MutationKind::ParentChanged {
                        old: Some(other_library),
                        new: Some(other_collection),
                    },
                ),
                affected: false,
            },
            Case {
                name: "a re-parent does not invalidate an unscoped kind pane",
                invalidation: inv(&publications()),
                mutation: mutation(
                    unrelated,
                    PUBLICATION,
                    MutationKind::ParentChanged {
                        old: None,
                        new: Some(library),
                    },
                ),
                affected: false,
            },
            // ── Rule 6: undeterminable schema ────────────────────────────
            Case {
                name: "a schemaless delete is conservatively taken as affecting",
                invalidation: inv(&publications()),
                mutation: StoreMutation::new(unrelated, None, MutationKind::Deleted),
                affected: true,
            },
        ];

        for case in cases {
            assert_eq!(
                case.invalidation.is_affected_by(&case.mutation),
                case.affected,
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn a_multi_ref_kind_matches_every_one_of_its_refs() {
        // `message` spans email and chat; a pane over it must wake for both.
        let i = inv(&PaneQuery {
            kinds: kinds(&["message"]),
            ..Default::default()
        });
        for r in ["email-message", "chat-message"] {
            assert!(
                i.is_affected_by(&mutation(Uuid::new_v4(), r, MutationKind::Created)),
                "{r}"
            );
        }
        assert!(!i.is_affected_by(&mutation(
            Uuid::new_v4(),
            PUBLICATION,
            MutationKind::Created
        )));
    }

    #[test]
    fn an_unkinded_pane_wakes_for_every_kind_in_the_manifest() {
        let i = inv(&PaneQuery::default());
        for r in [PUBLICATION, MANUSCRIPT, COLLECTION, "task@1.0.0"] {
            assert!(
                i.is_affected_by(&mutation(Uuid::new_v4(), r, MutationKind::Created)),
                "{r}"
            );
        }
        // Still not the journal: `core/operation` is in no kind's manifest
        // entry, and an operation row per mutation would be the render loop
        // D9 warns about, exactly.
        assert!(!i.is_affected_by(&mutation(
            Uuid::new_v4(),
            "core/operation",
            MutationKind::Created
        )));
    }

    #[test]
    fn invalidation_serde_round_trip() {
        let i = inv(&PaneQuery {
            kinds: kinds(&["publication"]),
            scope: Scope::Collection {
                id: ItemRef::Id { id: Uuid::new_v4() },
            },
            text: Some("dark matter".into()),
            ..Default::default()
        });
        let json = serde_json::to_string(&i).expect("serialize");
        let back: Invalidation = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(i, back);
    }

    // ── The registry ─────────────────────────────────────────────────────

    #[test]
    fn subscriptions_return_only_the_affected_panes() {
        let collection = Uuid::new_v4();
        let paper = Uuid::new_v4();

        let mut subs: QuerySubscriptions<&'static str> = QuerySubscriptions::new();
        subs.insert(
            "sidebar",
            inv(&PaneQuery {
                kinds: kinds(&["collection"]),
                ..Default::default()
            }),
        );
        subs.insert(
            "list",
            inv(&PaneQuery {
                kinds: kinds(&["publication"]),
                scope: Scope::Collection {
                    id: ItemRef::Id { id: collection },
                },
                ..Default::default()
            }),
        );
        subs.insert(
            "detail",
            inv(&PaneQuery {
                kinds: kinds(&["publication"]),
                scope: Scope::Item {
                    id: ItemRef::Id { id: paper },
                },
                ..Default::default()
            }),
        );
        subs.insert(
            "manuscripts",
            inv(&PaneQuery {
                kinds: kinds(&["manuscript"]),
                ..Default::default()
            }),
        );
        assert_eq!(subs.len(), 4);

        // Filing a paper into the open collection: the sidebar (the
        // collection row is of kind `collection`) and the list wake; the
        // detail pane and the manuscript pane do not.
        let filed = mutation(
            collection,
            COLLECTION,
            MutationKind::ReferenceAdded {
                edge: EdgeType::Contains,
                source: collection,
                target: paper,
            },
        );
        assert_eq!(subs.affected_by(&filed), vec!["list"]);

        // Editing the selected paper: the list (kind match) and the detail
        // pane (its own row), not the sidebar, not the manuscripts.
        let edited = mutation(paper, PUBLICATION, MutationKind::Updated);
        assert_eq!(subs.affected_by(&edited), vec!["list", "detail"]);

        // A manuscript save wakes only the manuscript pane.
        let saved = mutation(Uuid::new_v4(), MANUSCRIPT, MutationKind::Updated);
        assert_eq!(subs.affected_by(&saved), vec!["manuscripts"]);

        // A batch wakes each pane once, in registration order.
        assert_eq!(
            subs.affected_by_all([&filed, &edited, &saved]),
            vec!["list", "detail", "manuscripts"]
        );
    }

    #[test]
    fn re_registering_a_pane_replaces_its_dependency_set() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut subs: QuerySubscriptions<&'static str> = QuerySubscriptions::new();
        let first = inv(&PaneQuery {
            kinds: kinds(&["publication"]),
            scope: Scope::Item {
                id: ItemRef::Id { id: a },
            },
            ..Default::default()
        });
        assert!(subs.insert("detail", first.clone()).is_none());
        let previous = subs.insert_compiled(
            "detail",
            &compiled(&PaneQuery {
                kinds: kinds(&["publication"]),
                scope: Scope::Item {
                    id: ItemRef::Id { id: b },
                },
                ..Default::default()
            }),
        );
        assert_eq!(previous, Some(first));
        assert_eq!(subs.len(), 1, "a retyped pane does not accumulate");
        assert_eq!(subs.get(&"detail").expect("registered").items, vec![b]);
        assert_eq!(subs.keys().copied().collect::<Vec<_>>(), vec!["detail"]);

        // A closed pane stops waking.
        assert!(subs.remove(&"detail").is_some());
        assert!(subs.is_empty());
        assert!(subs
            .affected_by(&mutation(b, PUBLICATION, MutationKind::Updated))
            .is_empty());
    }
}
