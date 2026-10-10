use serde::{Deserialize, Serialize};

use crate::item::{Item, ItemId};
use crate::reference::EdgeType;
use crate::store::FieldMutation;

/// Events emitted by the item store when items change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ItemEvent {
    Created(Box<Item>),
    Updated {
        id: ItemId,
        mutations: Vec<FieldMutation>,
    },
    Deleted(ItemId),
    OperationApplied {
        operation_id: ItemId,
        target_id: ItemId,
    },
}

// ─────────────────────────────────────────────────────────────────────────────
// Incremental invalidation (ADR-0031 D9, work package L4)
// ─────────────────────────────────────────────────────────────────────────────

/// A store mutation, described in the terms a query-addressed pane needs in
/// order to decide whether to re-run (ADR-0031 D9).
///
/// [`ItemEvent`] is the Swift-facing bus and stays exactly as it is: Swift's
/// `StoreMirrorKernel` subscribes with a schema-ref prefix and consumes
/// `Created` / `Updated` / `Deleted` / `OperationApplied`. But
/// `OperationApplied` carries only an operation id and a target id, so a
/// subscriber cannot tell a tag edit from a collection membership change
/// without reading the operation item back out of the store — and *which*
/// edge moved is exactly what decides whether a pane scoped to a collection
/// has to re-query.
///
/// `StoreMutation` is that missing description, published on its own channel
/// (`SqliteItemStore::subscribe_mutations`, in `impress-core`) beside the
/// existing bus rather than in place of it.
///
/// It is matched against a pane-query `Invalidation`, which is derived from
/// the compiled pane query, by `Invalidation::is_affected_by`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreMutation {
    /// The item that changed. For a reference mutation this is the edge's
    /// SOURCE — the row the operation targeted.
    pub item_id: ItemId,
    /// The schema ref of `item_id`, when it could be determined.
    ///
    /// `None` means undeterminable (the row was already gone, or the read
    /// failed), and invalidation treats it as *possibly affecting* — the same
    /// stance `SqliteItemStore::emit` takes when it
    /// delivers a schemaless event to every subscriber regardless of filter.
    /// Over-running a pane costs a query; under-running it shows the user
    /// stale rows and calls them the truth.
    pub schema_ref: Option<String>,
    pub kind: MutationKind,
}

/// What kind of change [`StoreMutation`] describes.
///
/// The three graph-shaped variants exist because a pane's result can change
/// without the pane's own rows changing at all: adding a `Contains` edge to a
/// collection changes that collection's members, and re-parenting an item
/// changes every `Scope::Parent` pane on both the old and the new parent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum MutationKind {
    Created,
    /// Any materialized change to the row that is not a parent or reference
    /// move: payload, tags, flags, read/starred state, priority, visibility.
    Updated,
    Deleted,
    ParentChanged {
        old: Option<ItemId>,
        new: Option<ItemId>,
    },
    ReferenceAdded {
        edge: EdgeType,
        source: ItemId,
        target: ItemId,
    },
    ReferenceRemoved {
        edge: EdgeType,
        source: ItemId,
        target: ItemId,
    },
}

impl StoreMutation {
    /// A mutation on `item_id` of `schema_ref`.
    pub fn new(item_id: ItemId, schema_ref: Option<String>, kind: MutationKind) -> Self {
        Self {
            item_id,
            schema_ref,
            kind,
        }
    }

    /// The edge this mutation moved, if any: `(edge type, source, target)`.
    pub fn edge(&self) -> Option<(&EdgeType, ItemId, ItemId)> {
        match &self.kind {
            MutationKind::ReferenceAdded {
                edge,
                source,
                target,
            }
            | MutationKind::ReferenceRemoved {
                edge,
                source,
                target,
            } => Some((edge, *source, *target)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn event_serde_round_trip() {
        let events = vec![
            ItemEvent::Deleted(Uuid::new_v4()),
            ItemEvent::Updated {
                id: Uuid::new_v4(),
                mutations: vec![FieldMutation::SetRead(true)],
            },
            ItemEvent::OperationApplied {
                operation_id: Uuid::new_v4(),
                target_id: Uuid::new_v4(),
            },
        ];
        for e in &events {
            let json = serde_json::to_string(e).unwrap();
            let back: ItemEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(*e, back);
        }
    }

    #[test]
    fn store_mutation_serde_round_trip() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mutations = vec![
            StoreMutation::new(a, Some("manuscript".into()), MutationKind::Created),
            StoreMutation::new(a, Some("manuscript".into()), MutationKind::Updated),
            StoreMutation::new(a, None, MutationKind::Deleted),
            StoreMutation::new(
                a,
                Some("imbib/bibliography-entry".into()),
                MutationKind::ParentChanged {
                    old: None,
                    new: Some(b),
                },
            ),
            StoreMutation::new(
                a,
                Some("imbib/collection".into()),
                MutationKind::ReferenceAdded {
                    edge: EdgeType::Contains,
                    source: a,
                    target: b,
                },
            ),
            StoreMutation::new(
                a,
                Some("imbib/collection".into()),
                MutationKind::ReferenceRemoved {
                    edge: EdgeType::Custom("mirrors".into()),
                    source: a,
                    target: b,
                },
            ),
        ];
        for m in &mutations {
            let json = serde_json::to_string(m).unwrap();
            let back: StoreMutation = serde_json::from_str(&json).unwrap();
            assert_eq!(*m, back);
        }
    }

    #[test]
    fn edge_is_reported_for_reference_mutations_only() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let added = StoreMutation::new(
            a,
            None,
            MutationKind::ReferenceAdded {
                edge: EdgeType::Cites,
                source: a,
                target: b,
            },
        );
        assert_eq!(added.edge(), Some((&EdgeType::Cites, a, b)));
        assert!(StoreMutation::new(a, None, MutationKind::Updated)
            .edge()
            .is_none());
        assert!(StoreMutation::new(
            a,
            None,
            MutationKind::ParentChanged {
                old: None,
                new: Some(b),
            }
        )
        .edge()
        .is_none());
    }
}
