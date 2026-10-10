//! The pane query algebra (ADR-0031 D2), standalone (ADR-0033 D7).
//!
//! A pane shows the result of a [`PaneQuery`]: a small, closed algebra over
//! a shared store. It is deliberately not a language. Kinds, a scope,
//! filters, a text term, one relation walk and a sort — and any position
//! that names an item may instead name a **parameter** (`$name`) that is
//! filled at resolution time from a channel, a fixed item, or the view
//! kind's default (ADR-0031 D3).
//!
//! # What lives here, and what does not
//!
//! This crate holds the algebra's *types* only: no I/O, no store, no
//! compiler, no domain knowledge of which schema refs back which kind.
//! `impress_core::pane_query` compiles a [`PaneQuery`] onto its store in
//! exactly one place ([`compile`], carried by `impress-core`) from the
//! built-in [`KindManifest`] entries (`builtin_manifest()`, domain data that
//! belongs with the store it describes) — and re-exports every type below,
//! so every existing `impress_core::pane_query::X` path keeps resolving. A
//! kit crate that wants to build, inspect or route a query —
//! `impress-layout` today, and eventually a standalone host — depends on
//! this crate alone, never on `impress-core`'s domain modules (ADR-0033 D7).
//!
//! What the algebra cannot express is **materialized first**: explorations,
//! online search results, smart searches, joins and aggregations become
//! items or collections in the store, and a pane queries those. Growing the
//! algebra is an ADR amendment.
//!
//! # Empty is not an error, and an error is not empty
//!
//! Two failure modes are deliberately kept apart. A query that is WRONG —
//! an unknown kind, a required parameter with no value, a parameter declared
//! as the wrong kind — compiles to a typed [`PaneQueryError`] the pane
//! renders as a reason. A query that is merely UNFILLED — an optional
//! parameter with no value yet, which is every detail pane before the first
//! selection — compiles to a query that matches nothing, because a dropped
//! predicate would show the user every row in the store and call it a
//! result.
//!
//! # `ItemId` and `EdgeType`
//!
//! Both are the two `impress-core` types the algebra names, defined here so
//! this crate never depends on `impress-core`'s domain modules:
//!
//! - [`ItemId`] is a bare `Uuid` alias — `impress_core::item::ItemId` is
//!   *also* a bare `Uuid` alias, not a newtype, so the two are literally the
//!   same type at the compiler level and no conversion boundary exists (or
//!   is possible: `impl From<Uuid> for Uuid` would conflict with the
//!   standard library's reflexive impl). A `PaneQuery` built here and one
//!   built against `impress_core::item::ItemId` carry interchangeable ids.
//! - [`EdgeType`] is a real, distinct enum — copied variant-for-variant and
//!   serde-for-serde from `impress_core::reference::EdgeType` — because this
//!   crate must not depend on `impress-core::reference`. `impress-core`
//!   converts at the compiler boundary (`impl From` in its `reference.rs`),
//!   since its own `EdgeType` is what a [`crate::event::StoreMutation`] (a
//!   type this crate does not and must not know about) carries natively.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Globally unique item identifier. Identical to (and interchangeable with)
/// `impress_core::item::ItemId` — see the module docs.
pub type ItemId = uuid::Uuid;

/// Edge type taxonomy, copied variant-for-variant and serde-for-serde from
/// `impress_core::reference::EdgeType` (ADR-0033 D7). `impress-core`
/// converts at the compiler boundary; see the module docs.
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

/// A record-kind identifier as the chassis spells it: `publication`,
/// `manuscript`, `figure`, `message`, `task`, `agent-run`, `artifact`,
/// `collection`. The manifest maps each to its canonical schema refs.
pub type RecordKindId = String;

/// A parameter name inside a query (`item`, `manuscript`, `colormap`, …).
pub type ParamName = String;

/// A position that names an item: either a literal id or a parameter to be
/// filled at resolution time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "ref", rename_all = "kebab-case")]
pub enum ItemRef {
    /// A literal item id.
    Id {
        #[cfg_attr(feature = "schema", schemars(with = "String"))]
        id: ItemId,
    },
    /// A parameter, filled from [`Bindings`] when the query is compiled.
    Param { name: ParamName },
}

/// What population the query draws from.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "scope", rename_all = "kebab-case")]
pub enum Scope {
    /// Every item of the requested kinds.
    #[default]
    All,
    /// Members of one collection (a `Contains` edge from the collection).
    Collection { id: ItemRef },
    /// Members of a collection and of every collection beneath it.
    CollectionSubtree { id: ItemRef },
    /// Items whose envelope parent is this library / account / folder, and,
    /// for a container the manifest lists in
    /// [`KindManifest::contains_members`] (a library), the targets of its
    /// `Contains` edges too.
    Parent { id: ItemRef },
    /// Exactly one item. The detail pane's scope.
    Item { id: ItemRef },
}

/// A filter over the envelope fields every kind carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "filter", rename_all = "kebab-case")]
pub enum Filter {
    /// Carries a flag; `color` narrows to one colour.
    Flag {
        color: Option<String>,
    },
    Starred {
        starred: bool,
    },
    Read {
        read: bool,
    },
    /// Payload `status` equals this value (`dismissed`, `archived`, `draft`, …).
    Status {
        status: String,
    },
    /// Carries this tag path (or a descendant of it).
    Tag {
        path: String,
    },
    /// A payload or envelope field within a date range (ISO-8601 strings).
    DateRange {
        field: String,
        from: Option<String>,
        to: Option<String>,
    },
}

/// One relation walk: the items reachable from `from` over `edge`, in the
/// stated direction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RelationWalk {
    pub edge: EdgeType,
    pub from: ItemRef,
    /// `Outgoing`: items `from` points at (papers a manuscript cites).
    /// `Incoming`: items pointing at `from` (manuscripts citing a paper).
    pub direction: Direction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum Direction {
    Outgoing,
    Incoming,
}

/// Sort key. `field` is an envelope or `payload.` path, as in
/// `impress_core::query::SortDescriptor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SortKey {
    pub field: String,
    #[serde(default)]
    pub descending: bool,
}

/// The query a pane shows. See the module docs.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PaneQuery {
    /// Record kinds to include. Empty means every kind the manifest knows.
    #[serde(default)]
    pub kinds: Vec<RecordKindId>,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub filters: Vec<Filter>,
    /// Full-text term over `items_fts`.
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub relation: Option<RelationWalk>,
    #[serde(default)]
    pub sort: Vec<SortKey>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// A parameter declaration on a pane: what kind of item fills it and whether
/// the view kind can render without it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ParamDecl {
    pub name: ParamName,
    pub kind: RecordKindId,
    #[serde(default)]
    pub required: bool,
}

/// Resolved parameter values at compile time: name → item id. A parameter the
/// query names but the bindings lack is unbound; whether that is an error
/// depends on its [`ParamDecl::required`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Bindings {
    #[cfg_attr(feature = "schema", schemars(with = "BTreeMap<String, String>"))]
    pub values: BTreeMap<ParamName, ItemId>,
}

impl Bindings {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, name: impl Into<ParamName>, id: ItemId) -> Self {
        self.values.insert(name.into(), id);
        self
    }

    pub fn get(&self, name: &str) -> Option<ItemId> {
        self.values.get(name).copied()
    }
}

/// The one place record kinds are mapped to the schema refs the store matches
/// by exact equality.
///
/// This struct is generic: it carries no chassis data of its own. The
/// chassis's built-in kinds are domain data and live with the store that
/// backs them, as `impress_core::pane_query::builtin_manifest()` (ADR-0033
/// D7) — a host that is not the impress suite builds its own manifest the
/// same way, from its own schema refs.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct KindManifest {
    /// kind id → canonical schema refs (a kind may span several, e.g.
    /// `message` = `email-message` + `chat-message`).
    pub kinds: BTreeMap<RecordKindId, Vec<String>>,
    /// Container kind → the member kinds it holds by a `Contains` edge as
    /// well as by the envelope parent.
    ///
    /// A [`Scope::Parent`] whose parent is such a container, over only those
    /// member kinds, selects the parent's children OR the targets of its
    /// `Contains` edges. Everything else under a Parent scope is the envelope
    /// children alone. Empty by default: a host that files by parent only
    /// declares nothing here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub contains_members: BTreeMap<RecordKindId, Vec<RecordKindId>>,
}

impl KindManifest {
    pub fn schema_refs(&self, kind: &str) -> Option<&[String]> {
        self.kinds.get(kind).map(Vec::as_slice)
    }

    /// Does a [`Scope::Parent`] over `members` also take the parent's
    /// `Contains` targets (see [`Self::contains_members`])?
    ///
    /// `parent_kind` is the kind the parent was declared as, when a
    /// parameter declaration says so. A literal id carries no kind; its
    /// parent is then the one container whose `Contains` members cover every
    /// queried kind, and when no container, or more than one, does, the
    /// scope stays parent-only.
    pub fn parent_includes_contains(
        &self,
        parent_kind: Option<&str>,
        members: &[RecordKindId],
    ) -> bool {
        if members.is_empty() {
            return false;
        }
        let covers = |held: &Vec<RecordKindId>| members.iter().all(|m| held.contains(m));
        match parent_kind {
            Some(kind) => self.contains_members.get(kind).is_some_and(covers),
            None => {
                self.contains_members
                    .values()
                    .filter(|held| covers(held))
                    .count()
                    == 1
            }
        }
    }

    pub fn kind_for_schema_ref(&self, schema_ref: &str) -> Option<&str> {
        self.kinds
            .iter()
            .find(|(_, refs)| refs.iter().any(|r| r == schema_ref))
            .map(|(k, _)| k.as_str())
    }
}

/// Why a query did not compile. Every variant is a *typed* failure: the pane
/// renders its empty state with the reason, never an empty list that looks
/// like "no data yet".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "error", rename_all = "kebab-case")]
pub enum PaneQueryError {
    #[error("unknown record kind '{kind}'")]
    UnknownKind { kind: RecordKindId },
    #[error("parameter '{name}' is required but unbound")]
    UnboundParam { name: ParamName },
    #[error("parameter '{name}' is bound to a {actual} but the query needs a {expected}")]
    ParamKindMismatch {
        name: ParamName,
        expected: RecordKindId,
        actual: RecordKindId,
    },
    #[error("query has no kinds and the manifest is empty")]
    NoKinds,
    /// A [`Filter::DateRange`] bound on an envelope timestamp column
    /// (`created` / `modified`, stored as epoch milliseconds) that is not
    /// ISO-8601. Typed rather than passed through, because SQLite compares a
    /// text bound against an integer column by TYPE ORDER — text always sorts
    /// above integers — so a malformed bound would silently match every row
    /// on `Gte` and no row on `Lte`.
    #[error("date range bound '{value}' on field '{field}' is not ISO-8601")]
    InvalidDate { field: String, value: String },
}

/// The names of every parameter the query mentions, in first-use order.
pub fn params_in(query: &PaneQuery) -> Vec<ParamName> {
    let mut out: Vec<ParamName> = Vec::new();
    let mut push = |r: &ItemRef| {
        if let ItemRef::Param { name } = r {
            if !out.contains(name) {
                out.push(name.clone());
            }
        }
    };
    match &query.scope {
        Scope::All => {}
        Scope::Collection { id }
        | Scope::CollectionSubtree { id }
        | Scope::Parent { id }
        | Scope::Item { id } => push(id),
    }
    if let Some(walk) = &query.relation {
        push(&walk.from);
    }
    out
}

#[cfg(test)]
mod contract_tests {
    use super::*;
    use uuid::Uuid;

    fn shelf_manifest() -> KindManifest {
        let kinds = ["shelf", "book", "note", "box"]
            .into_iter()
            .map(|k| (k.to_string(), vec![format!("x/{k}")]))
            .collect();
        KindManifest {
            kinds,
            contains_members: [("shelf".to_string(), vec!["book".to_string()])]
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn a_parent_takes_contains_members_only_for_a_listed_container() {
        let m = shelf_manifest();
        let k = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // Declared kind decides.
        assert!(m.parent_includes_contains(Some("shelf"), &k(&["book"])));
        assert!(!m.parent_includes_contains(Some("box"), &k(&["book"])));
        assert!(!m.parent_includes_contains(Some("shelf"), &k(&["note"])));
        assert!(!m.parent_includes_contains(Some("shelf"), &k(&["book", "note"])));
        // No declaration: inferred from the members.
        assert!(m.parent_includes_contains(None, &k(&["book"])));
        assert!(!m.parent_includes_contains(None, &k(&["note"])));
        assert!(!m.parent_includes_contains(None, &[]));
        // A manifest that declares nothing is parent-only everywhere.
        let bare = KindManifest {
            contains_members: BTreeMap::new(),
            ..m
        };
        assert!(!bare.parent_includes_contains(None, &k(&["book"])));
    }

    #[test]
    fn a_manifest_without_contains_members_still_decodes() {
        let m: KindManifest = serde_json::from_str(r#"{"kinds":{"book":["x/book"]}}"#).unwrap();
        assert!(m.contains_members.is_empty());
        let json = serde_json::to_string(&shelf_manifest()).unwrap();
        assert_eq!(
            serde_json::from_str::<KindManifest>(&json).unwrap(),
            shelf_manifest()
        );
    }

    #[test]
    fn params_are_listed_in_first_use_order_without_duplicates() {
        let q = PaneQuery {
            scope: Scope::Collection {
                id: ItemRef::Param {
                    name: "collection".into(),
                },
            },
            relation: Some(RelationWalk {
                edge: EdgeType::Cites,
                from: ItemRef::Param {
                    name: "manuscript".into(),
                },
                direction: Direction::Outgoing,
            }),
            ..Default::default()
        };
        assert_eq!(params_in(&q), vec!["collection", "manuscript"]);
    }

    #[test]
    fn query_serde_round_trip() {
        let q = PaneQuery {
            kinds: vec!["publication".into()],
            scope: Scope::Item {
                id: ItemRef::Id { id: Uuid::nil() },
            },
            filters: vec![
                Filter::Flag { color: None },
                Filter::Starred { starred: true },
            ],
            text: Some("dark matter".into()),
            relation: None,
            sort: vec![SortKey {
                field: "modified".into(),
                descending: true,
            }],
            limit: Some(50),
        };
        let json = serde_json::to_string(&q).unwrap();
        let back: PaneQuery = serde_json::from_str(&json).unwrap();
        assert_eq!(q, back);
    }

    /// This crate's `EdgeType` must serialize exactly like
    /// `impress_core::reference::EdgeType` (no `rename_all`, so a bare
    /// variant is its literal Rust name and `Custom` is a one-key object) —
    /// the two are compared byte-for-byte in `impress-core`'s
    /// `edge_type_serde_matches_impress_core` test.
    #[test]
    fn edge_type_serde_shape() {
        assert_eq!(
            serde_json::to_string(&EdgeType::Cites).unwrap(),
            "\"Cites\""
        );
        assert_eq!(
            serde_json::to_string(&EdgeType::Custom("mirrors".into())).unwrap(),
            "{\"Custom\":\"mirrors\"}"
        );
    }
}
