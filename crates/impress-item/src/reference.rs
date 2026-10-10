use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::item::{ItemId, Value};

/// A typed, directed edge between two items in the graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypedReference {
    pub target: ItemId,
    pub edge_type: EdgeType,
    pub metadata: Option<BTreeMap<String, Value>>,
}

/// Edge type taxonomy.
///
/// Common types are enum variants; domain-specific types use `Custom`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum EdgeType {
    // Academic
    Cites,
    References,

    // Communication
    InResponseTo,
    Discusses,

    // Containment & Attachment
    Contains,
    Attaches,

    // Provenance
    ProducedBy,
    DerivedFrom,
    Supersedes,

    // Annotation
    Annotates,

    // Visualization
    Visualizes,

    // General
    RelatesTo,

    // Workflow
    DependsOn,

    // Operations
    OperatesOn,

    /// Part-of relationship (e.g., section is part of manuscript)
    IsPartOf,
    /// Version relationship
    HasVersion,
    /// Mentions relationship (loosely references)
    Mentions,
    /// Triggered by (e.g., task triggered by email)
    TriggeredBy,
    /// Export relationship
    Exports,

    // Extensible
    Custom(String),
}

// ─────────────────────────────────────────────────────────────────────────
// impress-pane-query boundary (ADR-0033 D7)
// ─────────────────────────────────────────────────────────────────────────
//
// `impress_pane_query::EdgeType` is a real, distinct enum copied
// variant-for-variant and serde-for-serde from this one, because the algebra
// crate must not depend on impress-core's domain modules. `RelationWalk`
// (the algebra's) carries the pane-query crate's `EdgeType`; `StoreMutation`
// and every `Predicate` here carry this one. The compiler in
// `crate::pane_query` converts at that one seam with `.into()`.

impl From<impress_pane_query::EdgeType> for EdgeType {
    fn from(e: impress_pane_query::EdgeType) -> Self {
        match e {
            impress_pane_query::EdgeType::Cites => EdgeType::Cites,
            impress_pane_query::EdgeType::References => EdgeType::References,
            impress_pane_query::EdgeType::InResponseTo => EdgeType::InResponseTo,
            impress_pane_query::EdgeType::Discusses => EdgeType::Discusses,
            impress_pane_query::EdgeType::Contains => EdgeType::Contains,
            impress_pane_query::EdgeType::Attaches => EdgeType::Attaches,
            impress_pane_query::EdgeType::ProducedBy => EdgeType::ProducedBy,
            impress_pane_query::EdgeType::DerivedFrom => EdgeType::DerivedFrom,
            impress_pane_query::EdgeType::Supersedes => EdgeType::Supersedes,
            impress_pane_query::EdgeType::Annotates => EdgeType::Annotates,
            impress_pane_query::EdgeType::Visualizes => EdgeType::Visualizes,
            impress_pane_query::EdgeType::RelatesTo => EdgeType::RelatesTo,
            impress_pane_query::EdgeType::DependsOn => EdgeType::DependsOn,
            impress_pane_query::EdgeType::OperatesOn => EdgeType::OperatesOn,
            impress_pane_query::EdgeType::IsPartOf => EdgeType::IsPartOf,
            impress_pane_query::EdgeType::HasVersion => EdgeType::HasVersion,
            impress_pane_query::EdgeType::Mentions => EdgeType::Mentions,
            impress_pane_query::EdgeType::TriggeredBy => EdgeType::TriggeredBy,
            impress_pane_query::EdgeType::Exports => EdgeType::Exports,
            impress_pane_query::EdgeType::Custom(s) => EdgeType::Custom(s),
        }
    }
}

impl From<EdgeType> for impress_pane_query::EdgeType {
    fn from(e: EdgeType) -> Self {
        match e {
            EdgeType::Cites => impress_pane_query::EdgeType::Cites,
            EdgeType::References => impress_pane_query::EdgeType::References,
            EdgeType::InResponseTo => impress_pane_query::EdgeType::InResponseTo,
            EdgeType::Discusses => impress_pane_query::EdgeType::Discusses,
            EdgeType::Contains => impress_pane_query::EdgeType::Contains,
            EdgeType::Attaches => impress_pane_query::EdgeType::Attaches,
            EdgeType::ProducedBy => impress_pane_query::EdgeType::ProducedBy,
            EdgeType::DerivedFrom => impress_pane_query::EdgeType::DerivedFrom,
            EdgeType::Supersedes => impress_pane_query::EdgeType::Supersedes,
            EdgeType::Annotates => impress_pane_query::EdgeType::Annotates,
            EdgeType::Visualizes => impress_pane_query::EdgeType::Visualizes,
            EdgeType::RelatesTo => impress_pane_query::EdgeType::RelatesTo,
            EdgeType::DependsOn => impress_pane_query::EdgeType::DependsOn,
            EdgeType::OperatesOn => impress_pane_query::EdgeType::OperatesOn,
            EdgeType::IsPartOf => impress_pane_query::EdgeType::IsPartOf,
            EdgeType::HasVersion => impress_pane_query::EdgeType::HasVersion,
            EdgeType::Mentions => impress_pane_query::EdgeType::Mentions,
            EdgeType::TriggeredBy => impress_pane_query::EdgeType::TriggeredBy,
            EdgeType::Exports => impress_pane_query::EdgeType::Exports,
            EdgeType::Custom(s) => impress_pane_query::EdgeType::Custom(s),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn typed_reference_serde_round_trip() {
        let refs = vec![
            TypedReference {
                target: Uuid::new_v4(),
                edge_type: EdgeType::Cites,
                metadata: None,
            },
            TypedReference {
                target: Uuid::new_v4(),
                edge_type: EdgeType::Custom("co-authored-with".into()),
                metadata: Some({
                    let mut m = BTreeMap::new();
                    m.insert("role".into(), Value::String("corresponding".into()));
                    m
                }),
            },
        ];
        for r in &refs {
            let json = serde_json::to_string(r).unwrap();
            let back: TypedReference = serde_json::from_str(&json).unwrap();
            assert_eq!(*r, back);
        }
    }

    #[test]
    fn edge_type_serde_variants() {
        let variants = vec![
            EdgeType::Cites,
            EdgeType::References,
            EdgeType::InResponseTo,
            EdgeType::Discusses,
            EdgeType::Contains,
            EdgeType::Attaches,
            EdgeType::ProducedBy,
            EdgeType::DerivedFrom,
            EdgeType::Supersedes,
            EdgeType::Annotates,
            EdgeType::Visualizes,
            EdgeType::RelatesTo,
            EdgeType::DependsOn,
            EdgeType::OperatesOn,
            EdgeType::IsPartOf,
            EdgeType::HasVersion,
            EdgeType::Mentions,
            EdgeType::TriggeredBy,
            EdgeType::Exports,
            EdgeType::Custom("my-edge".into()),
        ];
        for v in &variants {
            let json = serde_json::to_string(v).unwrap();
            let back: EdgeType = serde_json::from_str(&json).unwrap();
            assert_eq!(*v, back);
        }
    }

    /// Every variant round-trips through the [`impress_pane_query::EdgeType`]
    /// boundary and lands back on itself (ADR-0033 D7).
    #[test]
    fn edge_type_round_trips_through_the_pane_query_boundary() {
        let variants = vec![
            EdgeType::Cites,
            EdgeType::References,
            EdgeType::InResponseTo,
            EdgeType::Discusses,
            EdgeType::Contains,
            EdgeType::Attaches,
            EdgeType::ProducedBy,
            EdgeType::DerivedFrom,
            EdgeType::Supersedes,
            EdgeType::Annotates,
            EdgeType::Visualizes,
            EdgeType::RelatesTo,
            EdgeType::DependsOn,
            EdgeType::OperatesOn,
            EdgeType::IsPartOf,
            EdgeType::HasVersion,
            EdgeType::Mentions,
            EdgeType::TriggeredBy,
            EdgeType::Exports,
            EdgeType::Custom("my-edge".into()),
        ];
        for v in &variants {
            let algebra: impress_pane_query::EdgeType = v.clone().into();
            let back: EdgeType = algebra.into();
            assert_eq!(*v, back, "{v:?}");
        }
    }

    /// The two `EdgeType`s must serialize identically: `RelationWalk.edge`
    /// (the algebra's) and a `StoreMutation`'s edge (this crate's) describe
    /// the same wire shape wherever a surface or the FFI hands one to the
    /// other side.
    #[test]
    fn edge_type_serde_matches_impress_core() {
        let variants = vec![
            EdgeType::Cites,
            EdgeType::Contains,
            EdgeType::Custom("mirrors".into()),
        ];
        for v in &variants {
            let algebra: impress_pane_query::EdgeType = v.clone().into();
            assert_eq!(
                serde_json::to_string(v).unwrap(),
                serde_json::to_string(&algebra).unwrap(),
                "{v:?}"
            );
        }
    }
}
