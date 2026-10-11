//! The compiler: the one place the algebra becomes an [`ItemQuery`]
//! (ADR-0031 D2).
//!
//! Moved here from `impress_core::pane_query` (`docs/plan-koine-host.md`
//! H-D2). It uses only the algebra (`impress-pane-query`) and the item
//! protocol (`impress-item`), so it runs over any store that speaks the port;
//! `impress-core` keeps `builtin_manifest()` (the chassis's own schema refs)
//! and re-exports this crate so `impress_core::pane_query::compile` keeps
//! resolving.
//!
//! Every schema ref the store is asked for comes from the [`KindManifest`]
//! the caller passes; no ref is spelled at a call site, so a misspelled ref is
//! a compile-time error of the algebra rather than a silently empty pane (the
//! ADR-0022 schema-ref invariant).
//!
//! # Empty is not an error, and an error is not empty
//!
//! Two failure modes are deliberately kept apart. A query that is WRONG --
//! an unknown kind, a required parameter with no value, a parameter declared
//! as the wrong kind -- returns a typed [`PaneQueryError`] the pane renders as
//! a reason. A query that is merely UNFILLED -- an optional parameter with no
//! value yet, which is every detail pane before the first selection --
//! compiles to a query that matches nothing (`In("id", [])`). Neither ever
//! degrades to "no predicate", because a dropped predicate shows the user
//! every row in the store and calls it a result.
//!
//! [`compile`] is the whole surface; [`compile_with`] is the same plus a
//! [`SubtreeResolver`], which [`Scope::CollectionSubtree`] needs because the
//! collection tree lives in the store and this compiler is pure.

use impress_item::{EdgeType, ItemId, ItemQuery};
use impress_pane_query::{
    Bindings, Direction, Filter, ItemRef, KindManifest, PaneQuery, PaneQueryError, ParamDecl,
    ParamName, RecordKindId, Scope,
};

use crate::invalidation::Invalidation;

/// Compiled form: the store query plus, when the query is a single-kind
/// `Item` scope, the id it resolved to (so a detail pane can short-circuit).
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledQuery {
    pub item_query: ItemQuery,
    /// Every schema ref the compiled query can touch. This is what incremental
    /// invalidation (L4) keys on: a mutation whose schema ref is not in this
    /// set cannot change the result.
    pub schema_refs: Vec<String>,
    /// The single item, when `scope` was `Item` and resolved.
    pub single_item: Option<ItemId>,
    /// Everything this compiled query depends on, as
    /// [`Invalidation::is_affected_by`] consumes it.
    ///
    /// `schema_refs` above is the documented invalidation key and remains
    /// true as far as it goes, but it is not the whole dependency: a relation
    /// walk and a collection scope also change when an EDGE is added or
    /// removed, and the mutation that moves that edge carries the schema ref
    /// of the *collection* (or of the citing manuscript), which is routinely
    /// outside the pane's own `schema_refs`. A publication list scoped to a
    /// collection would never notice a paper being filed into it. This field
    /// is the rest of the key: edges, parents, fixed ids and whether the pane
    /// reads the full-text index.
    pub invalidation: Invalidation,
}

/// Expands a collection id into that collection and every collection beneath
/// it, for [`Scope::CollectionSubtree`].
///
/// A trait rather than a store handle so the compiler stays pure and
/// available without any store: the caller reads the tree (with
/// `collection_ops::list_tree`, or from a sidebar model it already holds) and
/// hands the compiler the ids.
///
/// `root` itself may be included or left out — the compiler always queries it,
/// because a subtree contains its own root and a resolver that returns only
/// descendants would otherwise silently drop the folder's own members.
pub trait SubtreeResolver {
    fn subtree(&self, root: ItemId) -> Vec<ItemId>;
}

impl<F> SubtreeResolver for F
where
    F: Fn(ItemId) -> Vec<ItemId>,
{
    fn subtree(&self, root: ItemId) -> Vec<ItemId> {
        self(root)
    }
}

/// The resolver [`compile`] uses: no store, so a subtree degrades to the one
/// collection named. Documented rather than hidden — a pane compiled without a
/// resolver shows the folder's own members, not its descendants'.
pub struct RootOnly;

impl SubtreeResolver for RootOnly {
    fn subtree(&self, root: ItemId) -> Vec<ItemId> {
        vec![root]
    }
}

/// Compile a pane query against the manifest with the given bindings.
///
/// [`Scope::CollectionSubtree`] needs the collection tree, which this
/// signature has no way to read, so it compiles as [`Scope::Collection`]
/// (members of the named collection only). Use [`compile_with`] and supply a
/// [`SubtreeResolver`] when descendants matter.
pub fn compile(
    query: &PaneQuery,
    decls: &[ParamDecl],
    bindings: &Bindings,
    manifest: &KindManifest,
) -> Result<CompiledQuery, PaneQueryError> {
    compiler::compile(query, decls, bindings, manifest, &RootOnly)
}

/// [`compile`], with a resolver for [`Scope::CollectionSubtree`].
pub fn compile_with(
    query: &PaneQuery,
    decls: &[ParamDecl],
    bindings: &Bindings,
    manifest: &KindManifest,
    resolver: &dyn SubtreeResolver,
) -> Result<CompiledQuery, PaneQueryError> {
    compiler::compile(query, decls, bindings, manifest, resolver)
}
mod compiler {
    //! The one place the algebra becomes an [`ItemQuery`] (ADR-0031 D2).
    //!
    //! Every schema ref the store is asked for comes from the
    //! [`KindManifest`]; no ref is ever spelled at a call site.

    use super::*;
    use impress_item::{Predicate, SchemaRef, SortDescriptor, Value};

    /// The journal schema. Excluded from the `assume_schema_rare` planner hint
    /// because operation rows ARE most of the table — the hint would trade a
    /// fast sort-index walk for probe-and-sort-everything (see
    /// [`ItemQuery::assume_schema_rare`]).
    const OPERATION_SCHEMA_REF: SchemaRef = impress_item::schema::refs::CORE_OPERATION;

    /// The column FTS routes through. `sql_query` turns `Contains` on any
    /// `items_fts`-backed field into `items_fts MATCH ?`, which is unqualified
    /// — it searches every indexed column (title, author, abstract, note,
    /// body), which is what a pane's text box means. This is the same index
    /// `search_ops::search_all` reads; that kernel exists to BUCKET hits by
    /// kind, which a single-kind pane query does not need.
    const FTS_FIELD: &str = "title";

    /// Envelope timestamp columns, stored as epoch milliseconds.
    const TIMESTAMP_FIELDS: [&str; 2] = ["created", "modified"];

    /// A predicate that matches nothing. `sql_query` compiles an empty `In`
    /// to the constant `0`, so this is the algebra's FALSE.
    fn matches_nothing() -> Predicate {
        Predicate::In("id".to_string(), Vec::new())
    }

    /// What an [`ItemRef`] resolved to. `Unbound` is not an error: an optional
    /// parameter with no value renders an empty pane (ADR-0031 D3), never a
    /// pane showing everything.
    enum Resolved {
        Id(ItemId),
        Unbound,
    }

    fn resolve(
        r: &ItemRef,
        decls: &[ParamDecl],
        bindings: &Bindings,
    ) -> Result<Resolved, PaneQueryError> {
        match r {
            ItemRef::Id { id } => Ok(Resolved::Id(*id)),
            ItemRef::Param { name } => match bindings.get(name) {
                Some(id) => Ok(Resolved::Id(id)),
                None => {
                    // An UNDECLARED parameter is optional: ADR-0031 D3 makes
                    // required-ness a property the view kind declares, so a
                    // query naming a blank nobody declared degrades to the
                    // empty state rather than failing the pane.
                    let required = decls
                        .iter()
                        .find(|d| &d.name == name)
                        .map(|d| d.required)
                        .unwrap_or(false);
                    if required {
                        Err(PaneQueryError::UnboundParam { name: name.clone() })
                    } else {
                        Ok(Resolved::Unbound)
                    }
                }
            },
        }
    }

    /// Which end of a [`Filter::DateRange`] a bound is. It matters only for a
    /// DATE-ONLY bound: a user who types `2026-01-31` as the upper bound means
    /// "through the 31st", so the day is INCLUDED. Midnight on both ends would
    /// make a single-day range (`from == to`) match nothing at all.
    #[derive(Clone, Copy)]
    enum Bound {
        /// Lower bound: a date-only value is midnight UTC that morning.
        From,
        /// Upper bound: a date-only value is the last millisecond of that day.
        To,
    }

    /// ISO-8601 → epoch milliseconds, for the envelope timestamp columns.
    ///
    /// A value that names a TIME is taken literally at both ends; only a
    /// date-only value is widened by [`Bound`].
    fn iso_to_millis(field: &str, raw: &str, bound: Bound) -> Result<i64, PaneQueryError> {
        let invalid = || PaneQueryError::InvalidDate {
            field: field.to_string(),
            value: raw.to_string(),
        };
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) {
            return Ok(dt.timestamp_millis());
        }
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S") {
            return Ok(naive.and_utc().timestamp_millis());
        }
        if let Ok(date) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
            let time = match bound {
                Bound::From => date.and_hms_milli_opt(0, 0, 0, 0),
                Bound::To => date.and_hms_milli_opt(23, 59, 59, 999),
            };
            return Ok(time.ok_or_else(invalid)?.and_utc().timestamp_millis());
        }
        Err(invalid())
    }

    fn date_bound(field: &str, raw: &str, bound: Bound) -> Result<Value, PaneQueryError> {
        if TIMESTAMP_FIELDS.contains(&field) {
            Ok(Value::Int(iso_to_millis(field, raw, bound)?))
        } else {
            // A payload date field holds whatever its writer wrote — ISO-8601
            // strings, which compare lexicographically in the right order.
            Ok(Value::String(raw.to_string()))
        }
    }

    pub fn compile(
        query: &PaneQuery,
        decls: &[ParamDecl],
        bindings: &Bindings,
        manifest: &KindManifest,
        resolver: &dyn SubtreeResolver,
    ) -> Result<CompiledQuery, PaneQueryError> {
        // ── Kinds → schema refs ──────────────────────────────────────────
        let kinds: Vec<RecordKindId> = if query.kinds.is_empty() {
            manifest.kinds.keys().cloned().collect()
        } else {
            query.kinds.clone()
        };
        if kinds.is_empty() {
            return Err(PaneQueryError::NoKinds);
        }

        let mut schema_refs: Vec<String> = Vec::new();
        for kind in &kinds {
            let refs = manifest
                .schema_refs(kind)
                .ok_or_else(|| PaneQueryError::UnknownKind { kind: kind.clone() })?;
            for r in refs {
                if !schema_refs.contains(r) {
                    schema_refs.push(r.clone());
                }
            }
        }

        // ── Static parameter checks ──────────────────────────────────────
        //
        // A scope parameter DECLARED as the wrong kind can never agree with
        // the scope, bound or not, so this runs before the bindings are
        // consulted. Which kind is expected depends on the scope:
        //
        // | scope                          | the parameter names            | expected      |
        // |--------------------------------|--------------------------------|---------------|
        // | `Item`                         | a row of the queried kind      | the query's   |
        // | `Collection`/`CollectionSubtree` | the containing collection    | `collection`  |
        // | `Parent`                       | the owning library (or folder) | `library`     |
        //
        // `Item` is checked only when the query names ONE kind: a
        // heterogeneous pane has no single expectation to report. `Parent`
        // ALSO accepts `collection`, because a figure folder collects its
        // members through the envelope parent (`collection_ops`
        // `Membership::EnvelopeParent`) — the message names `library` because
        // that is the ordinary case.
        let scope_expectation: Option<(&ParamName, RecordKindId, &[&str])> = match &query.scope {
            Scope::Item {
                id: ItemRef::Param { name },
            } if kinds.len() == 1 => Some((name, kinds[0].clone(), &[])),
            Scope::Collection {
                id: ItemRef::Param { name },
            }
            | Scope::CollectionSubtree {
                id: ItemRef::Param { name },
            } => Some((name, "collection".to_string(), &[])),
            Scope::Parent {
                id: ItemRef::Param { name },
            } => Some((name, "library".to_string(), &["collection"])),
            _ => None,
        };
        if let Some((name, expected, also_accepted)) = scope_expectation {
            if let Some(decl) = decls.iter().find(|d| &d.name == name) {
                if decl.kind != expected && !also_accepted.contains(&decl.kind.as_str()) {
                    return Err(PaneQueryError::ParamKindMismatch {
                        name: name.clone(),
                        expected,
                        actual: decl.kind.clone(),
                    });
                }
            }
        }

        let mut predicates: Vec<Predicate> = Vec::new();
        let mut single_item: Option<ItemId> = None;
        let mut matches_no_rows = false;
        // The dependency set, accumulated in lockstep with the predicates so
        // that there is exactly ONE place a scope or a walk is interpreted.
        // A second pass over the `PaneQuery` would re-resolve every `ItemRef`
        // and could disagree with the predicates it is supposed to describe —
        // which is the drift mode this whole layer exists to remove.
        let mut inv = Invalidation::default();

        // ── Scope ────────────────────────────────────────────────────────
        match &query.scope {
            Scope::All => {}
            Scope::Collection { id } => match resolve(id, decls, bindings)? {
                // Membership is a `Contains` edge FROM the collection TO the
                // member (`collection_ops::member_query`), so members are the
                // edge's TARGETS: `ReferencedBy(Contains, collection)`.
                Resolved::Id(c) => {
                    predicates.push(Predicate::ReferencedBy(EdgeType::Contains, c));
                    inv.depend_on_edge(EdgeType::Contains, c);
                    inv.depend_on_item(c);
                }
                Resolved::Unbound => matches_no_rows = true,
            },
            Scope::CollectionSubtree { id } => match resolve(id, decls, bindings)? {
                Resolved::Id(root) => {
                    // The resolver's order is kept (a tree walk reads
                    // top-down), so this dedups by membership rather than by
                    // sorting — `Vec::dedup` would only catch neighbours.
                    let mut ids: Vec<ItemId> = vec![root];
                    for id in resolver.subtree(root) {
                        if !ids.contains(&id) {
                            ids.push(id);
                        }
                    }
                    for c in &ids {
                        inv.depend_on_edge(EdgeType::Contains, *c);
                        inv.depend_on_item(*c);
                    }
                    let mut edges: Vec<Predicate> = ids
                        .into_iter()
                        .map(|c| Predicate::ReferencedBy(EdgeType::Contains, c))
                        .collect();
                    if edges.len() == 1 {
                        predicates.push(edges.remove(0));
                    } else {
                        predicates.push(Predicate::Or(edges));
                    }
                }
                Resolved::Unbound => matches_no_rows = true,
            },
            Scope::Parent { id } => match resolve(id, decls, bindings)? {
                Resolved::Id(p) => {
                    // The parent's declared kind, when a declaration names
                    // one; a literal id leaves it to the manifest to infer.
                    let declared = match id {
                        ItemRef::Param { name } => decls
                            .iter()
                            .find(|d| &d.name == name)
                            .map(|d| d.kind.as_str()),
                        ItemRef::Id { .. } => None,
                    };
                    if manifest.parent_includes_contains(declared, &kinds) {
                        predicates.push(Predicate::Or(vec![
                            Predicate::HasParent(p),
                            Predicate::ReferencedBy(EdgeType::Contains, p),
                        ]));
                        inv.depend_on_edge(EdgeType::Contains, p);
                    } else {
                        predicates.push(Predicate::HasParent(p));
                    }
                    inv.depend_on_parent(p);
                    inv.depend_on_item(p);
                }
                Resolved::Unbound => matches_no_rows = true,
            },
            Scope::Item { id } => match resolve(id, decls, bindings)? {
                Resolved::Id(i) => {
                    inv.narrowed = true;
                    // `items.id` is the lowercase hyphenated UUID text the
                    // store writes with `Uuid::to_string()`.
                    predicates.push(Predicate::Eq("id".into(), Value::String(i.to_string())));
                    single_item = Some(i);
                    inv.depend_on_item(i);
                }
                Resolved::Unbound => {
                    // Narrowed to an empty set of ids: nothing can change it
                    // until the binding does, and that is a recompile.
                    inv.narrowed = true;
                    matches_no_rows = true;
                }
            },
        }

        // ── Filters ──────────────────────────────────────────────────────
        for filter in &query.filters {
            match filter {
                Filter::Flag { color } => predicates.push(Predicate::HasFlag(color.clone())),
                Filter::Starred { starred } => predicates.push(Predicate::IsStarred(*starred)),
                Filter::Read { read } => predicates.push(Predicate::IsRead(*read)),
                Filter::Status { status } => predicates.push(Predicate::Eq(
                    // `triage_ops::STATUS_FIELD` — a payload string on every
                    // kind, not an envelope column.
                    "payload.status".into(),
                    Value::String(status.clone()),
                )),
                Filter::Tag { path } => predicates.push(Predicate::HasTag(path.clone())),
                Filter::DateRange { field, from, to } => {
                    if let Some(from) = from {
                        predicates.push(Predicate::Gte(
                            field.clone(),
                            date_bound(field, from, Bound::From)?,
                        ));
                    }
                    if let Some(to) = to {
                        predicates.push(Predicate::Lte(
                            field.clone(),
                            date_bound(field, to, Bound::To)?,
                        ));
                    }
                }
            }
        }

        // ── Text ─────────────────────────────────────────────────────────
        if let Some(text) = query.text.as_deref() {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                predicates.push(Predicate::Contains(
                    FTS_FIELD.to_string(),
                    trimmed.to_string(),
                ));
                inv.text = true;
            }
        }

        // ── Relation walk ────────────────────────────────────────────────
        if let Some(walk) = &query.relation {
            match resolve(&walk.from, decls, bindings)? {
                Resolved::Id(from) => {
                    // `walk.edge` is the algebra's `impress_pane_query::EdgeType`
                    // (ADR-0033 D7); everything downstream — the predicate and
                    // the invalidation dependency — wants `impress_item`'s
                    // `EdgeType`, so the boundary conversion happens once, here.
                    let edge: EdgeType = walk.edge.clone().into();
                    inv.depend_on_edge(edge.clone(), from);
                    inv.depend_on_item(from);
                    predicates.push(match walk.direction {
                        // `ReferencedBy(e, s)` = `id IN (SELECT target_id … source_id = s)`
                        // — the items `from` points at (papers a manuscript cites).
                        Direction::Outgoing => Predicate::ReferencedBy(edge, from),
                        // `HasReference(e, t)` = `id IN (SELECT source_id … target_id = t)`
                        // — the items pointing at `from` (manuscripts citing a paper).
                        Direction::Incoming => Predicate::HasReference(edge, from),
                    });
                }
                Resolved::Unbound => matches_no_rows = true,
            }
        }

        if matches_no_rows {
            predicates.push(matches_nothing());
            single_item = None;
        }

        // ── Schema constraint ────────────────────────────────────────────
        // One ref goes on `ItemQuery::schema` (the indexed column comparison);
        // several become `In("schema_ref", …)`, because `schema` holds exactly
        // one and dropping the others would widen the pane silently.
        let schema = if schema_refs.len() == 1 {
            Some(SchemaRef::from_stored(schema_refs[0].clone()))
        } else {
            predicates.push(Predicate::In(
                "schema_ref".into(),
                schema_refs.iter().cloned().map(Value::String).collect(),
            ));
            None
        };

        let assume_schema_rare = schema
            .as_deref()
            .is_some_and(|s| s != OPERATION_SCHEMA_REF.as_str());

        let item_query = ItemQuery {
            schema,
            predicates,
            sort: query
                .sort
                .iter()
                .map(|s| SortDescriptor {
                    field: s.field.clone(),
                    ascending: !s.descending,
                })
                .collect(),
            limit: query.limit.map(|l| l as usize),
            offset: None,
            include_tags: true,
            include_references: true,
            assume_schema_rare,
        };

        inv.schema_refs = schema_refs.clone();

        Ok(CompiledQuery {
            item_query,
            schema_refs,
            single_item,
            invalidation: inv,
        })
    }
}

#[cfg(test)]
mod compiler_tests {
    use super::*;
    use crate::test_support::builtin_manifest;
    use impress_item::{Predicate, SortDescriptor, Value};
    use impress_pane_query::{RelationWalk, SortKey};
    use uuid::Uuid;

    fn manifest() -> KindManifest {
        builtin_manifest()
    }

    fn ok(query: &PaneQuery) -> CompiledQuery {
        compile(query, &[], &Bindings::new(), &manifest()).expect("compiles")
    }

    fn kinds(k: &[&str]) -> Vec<RecordKindId> {
        k.iter().map(|s| (*s).to_string()).collect()
    }

    // ── The sidebar, as queries ──────────────────────────────────────────
    //
    // ADR-0031 D2's claim is that every navigable node the chassis ships
    // today is a value in this algebra. This table is that claim, checked.

    struct Node {
        name: &'static str,
        query: PaneQuery,
        decls: Vec<ParamDecl>,
        bindings: Bindings,
        refs: Vec<&'static str>,
    }

    fn sidebar_nodes() -> Vec<Node> {
        let library = Uuid::new_v4();
        let dismissed_library = Uuid::new_v4();
        let collection = Uuid::new_v4();
        let folder = Uuid::new_v4();
        let manuscript = Uuid::new_v4();
        let paper = Uuid::new_v4();

        let manuscript_decl = ParamDecl {
            name: "manuscript".into(),
            kind: "manuscript".into(),
            required: false,
        };
        let paper_decl = ParamDecl {
            name: "paper".into(),
            kind: "publication".into(),
            required: false,
        };

        vec![
            Node {
                name: "Inbox — publications in a library",
                query: PaneQuery {
                    kinds: kinds(&["publication"]),
                    scope: Scope::Parent {
                        id: ItemRef::Id { id: library },
                    },
                    filters: vec![Filter::Read { read: false }],
                    sort: vec![SortKey {
                        field: "created".into(),
                        descending: true,
                    }],
                    ..Default::default()
                },
                decls: vec![],
                bindings: Bindings::new(),
                refs: vec!["imbib/bibliography-entry"],
            },
            Node {
                name: "Libraries — the library rows themselves",
                query: PaneQuery {
                    kinds: kinds(&["library"]),
                    ..Default::default()
                },
                decls: vec![],
                bindings: Bindings::new(),
                refs: vec!["imbib/library"],
            },
            Node {
                name: "A publication collection",
                query: PaneQuery {
                    kinds: kinds(&["publication"]),
                    scope: Scope::Collection {
                        id: ItemRef::Id { id: collection },
                    },
                    ..Default::default()
                },
                decls: vec![],
                bindings: Bindings::new(),
                refs: vec!["imbib/bibliography-entry"],
            },
            Node {
                name: "Collections — the collection rows themselves",
                query: PaneQuery {
                    kinds: kinds(&["collection"]),
                    scope: Scope::Parent {
                        id: ItemRef::Id { id: library },
                    },
                    ..Default::default()
                },
                decls: vec![],
                bindings: Bindings::new(),
                refs: vec![
                    "collection",
                    "imbib/collection",
                    "manuscript-collection",
                    "figure-collection",
                ],
            },
            Node {
                name: "Flagged publications",
                query: PaneQuery {
                    kinds: kinds(&["publication"]),
                    filters: vec![Filter::Flag { color: None }],
                    ..Default::default()
                },
                decls: vec![],
                bindings: Bindings::new(),
                refs: vec!["imbib/bibliography-entry"],
            },
            Node {
                name: "Flagged red messages",
                query: PaneQuery {
                    kinds: kinds(&["message"]),
                    filters: vec![Filter::Flag {
                        color: Some("red".into()),
                    }],
                    ..Default::default()
                },
                decls: vec![],
                bindings: Bindings::new(),
                refs: vec!["email-message", "chat-message"],
            },
            Node {
                name: "Starred — every kind",
                query: PaneQuery {
                    filters: vec![Filter::Starred { starred: true }],
                    ..Default::default()
                },
                decls: vec![],
                bindings: Bindings::new(),
                refs: vec![
                    "agent-run@1.0.0",
                    "impress/artifact/code",
                    "impress/artifact/dataset",
                    "impress/artifact/general",
                    "impress/artifact/media",
                    "impress/artifact/note",
                    "impress/artifact/poster",
                    "impress/artifact/presentation",
                    "impress/artifact/webpage",
                    "collection",
                    "imbib/collection",
                    "manuscript-collection",
                    "figure-collection",
                    "figure",
                    "imbib/library",
                    "manuscript",
                    "email-message",
                    "chat-message",
                    "imbib/bibliography-entry",
                    "impress/ui/surface@1.0.0",
                    "task@1.0.0",
                ],
            },
            Node {
                name: "Dismissed manuscripts (payload status)",
                query: PaneQuery {
                    kinds: kinds(&["manuscript"]),
                    filters: vec![Filter::Status {
                        status: "dismissed".into(),
                    }],
                    ..Default::default()
                },
                decls: vec![],
                bindings: Bindings::new(),
                refs: vec!["manuscript"],
            },
            Node {
                // imbib dismisses a paper by MOVING it to the Dismissed
                // library, never by writing `status` (triage_ops module docs).
                name: "Dismissed publications (the dismissed library)",
                query: PaneQuery {
                    kinds: kinds(&["publication"]),
                    scope: Scope::Parent {
                        id: ItemRef::Id {
                            id: dismissed_library,
                        },
                    },
                    ..Default::default()
                },
                decls: vec![],
                bindings: Bindings::new(),
                refs: vec!["imbib/bibliography-entry"],
            },
            Node {
                name: "Cited in this manuscript",
                query: PaneQuery {
                    kinds: kinds(&["publication"]),
                    relation: Some(RelationWalk {
                        edge: EdgeType::Cites.into(),
                        from: ItemRef::Param {
                            name: "manuscript".into(),
                        },
                        direction: Direction::Outgoing,
                    }),
                    ..Default::default()
                },
                decls: vec![manuscript_decl.clone()],
                bindings: Bindings::new().with("manuscript", manuscript),
                refs: vec!["imbib/bibliography-entry"],
            },
            Node {
                name: "Manuscripts citing this paper",
                query: PaneQuery {
                    kinds: kinds(&["manuscript"]),
                    relation: Some(RelationWalk {
                        edge: EdgeType::Cites.into(),
                        from: ItemRef::Param {
                            name: "paper".into(),
                        },
                        direction: Direction::Incoming,
                    }),
                    ..Default::default()
                },
                decls: vec![paper_decl],
                bindings: Bindings::new().with("paper", paper),
                refs: vec!["manuscript"],
            },
            Node {
                name: "Manuscripts folder and everything under it",
                query: PaneQuery {
                    kinds: kinds(&["manuscript"]),
                    scope: Scope::CollectionSubtree {
                        id: ItemRef::Id { id: folder },
                    },
                    ..Default::default()
                },
                decls: vec![],
                bindings: Bindings::new(),
                refs: vec!["manuscript"],
            },
            Node {
                name: "All messages",
                query: PaneQuery {
                    kinds: kinds(&["message"]),
                    ..Default::default()
                },
                decls: vec![],
                bindings: Bindings::new(),
                refs: vec!["email-message", "chat-message"],
            },
            Node {
                name: "All tasks",
                query: PaneQuery {
                    kinds: kinds(&["task"]),
                    sort: vec![SortKey {
                        field: "payload.title".into(),
                        descending: false,
                    }],
                    limit: Some(200),
                    ..Default::default()
                },
                decls: vec![],
                bindings: Bindings::new(),
                refs: vec!["task@1.0.0"],
            },
            Node {
                name: "Agent runs",
                query: PaneQuery {
                    kinds: kinds(&["agent-run"]),
                    ..Default::default()
                },
                decls: vec![],
                bindings: Bindings::new(),
                refs: vec!["agent-run@1.0.0"],
            },
            Node {
                name: "Figures",
                query: PaneQuery {
                    kinds: kinds(&["figure"]),
                    ..Default::default()
                },
                decls: vec![],
                bindings: Bindings::new(),
                refs: vec!["figure"],
            },
            Node {
                name: "Artifacts",
                query: PaneQuery {
                    kinds: kinds(&["artifact"]),
                    ..Default::default()
                },
                decls: vec![],
                bindings: Bindings::new(),
                refs: vec![
                    "impress/artifact/code",
                    "impress/artifact/dataset",
                    "impress/artifact/general",
                    "impress/artifact/media",
                    "impress/artifact/note",
                    "impress/artifact/poster",
                    "impress/artifact/presentation",
                    "impress/artifact/webpage",
                ],
            },
            Node {
                name: "Search — text over one kind",
                query: PaneQuery {
                    kinds: kinds(&["publication"]),
                    text: Some("dark matter".into()),
                    ..Default::default()
                },
                decls: vec![],
                bindings: Bindings::new(),
                refs: vec!["imbib/bibliography-entry"],
            },
            Node {
                name: "Detail pane — one publication",
                query: PaneQuery {
                    kinds: kinds(&["publication"]),
                    scope: Scope::Item {
                        id: ItemRef::Param {
                            name: "item".into(),
                        },
                    },
                    ..Default::default()
                },
                decls: vec![ParamDecl {
                    name: "item".into(),
                    kind: "publication".into(),
                    required: false,
                }],
                bindings: Bindings::new().with("item", paper),
                refs: vec!["imbib/bibliography-entry"],
            },
        ]
    }

    #[test]
    fn every_sidebar_node_compiles_to_the_expected_schema_refs() {
        for node in sidebar_nodes() {
            let compiled = compile(&node.query, &node.decls, &node.bindings, &manifest())
                .unwrap_or_else(|e| panic!("{}: {e}", node.name));
            assert_eq!(
                compiled.schema_refs, node.refs,
                "{}: schema refs",
                node.name
            );
            // One ref goes on `schema`; several must become an `In` — never
            // be dropped, which would silently widen the pane to every kind.
            if node.refs.len() == 1 {
                assert_eq!(
                    compiled.item_query.schema.as_deref(),
                    Some(node.refs[0]),
                    "{}: single ref belongs on ItemQuery::schema",
                    node.name
                );
                assert!(
                    compiled.item_query.assume_schema_rare,
                    "{}: a single non-operation schema takes the planner hint",
                    node.name
                );
            } else {
                assert!(
                    compiled.item_query.schema.is_none(),
                    "{}: multi-ref queries leave `schema` empty",
                    node.name
                );
                let expected: Vec<Value> = node
                    .refs
                    .iter()
                    .map(|r| Value::String((*r).to_string()))
                    .collect();
                assert!(
                    compiled
                        .item_query
                        .predicates
                        .contains(&Predicate::In("schema_ref".into(), expected)),
                    "{}: multi-ref queries need In(schema_ref, …)",
                    node.name
                );
                assert!(
                    !compiled.item_query.assume_schema_rare,
                    "{}: the planner hint only applies to a single schema",
                    node.name
                );
            }
            // Nothing in the table is an accidentally-empty query.
            assert!(
                !compiled
                    .item_query
                    .predicates
                    .contains(&Predicate::In("id".into(), vec![])),
                "{}: compiled to a matches-nothing query",
                node.name
            );
        }
    }

    #[test]
    fn scopes_compile_to_the_documented_predicates() {
        let id = Uuid::new_v4();

        // Membership is a Contains edge FROM the collection TO the member,
        // so members are the edge's targets.
        let c = ok(&PaneQuery {
            kinds: kinds(&["publication"]),
            scope: Scope::Collection {
                id: ItemRef::Id { id },
            },
            ..Default::default()
        });
        assert_eq!(
            c.item_query.predicates,
            vec![Predicate::ReferencedBy(EdgeType::Contains, id)]
        );

        let p = ok(&PaneQuery {
            kinds: kinds(&["publication"]),
            scope: Scope::Parent {
                id: ItemRef::Id { id },
            },
            ..Default::default()
        });
        // Publications under a parent are a library's papers, which the
        // manifest says a library also holds by a Contains edge
        // (`in_library_predicate`'s membership).
        assert_eq!(
            p.item_query.predicates,
            vec![Predicate::Or(vec![
                Predicate::HasParent(id),
                Predicate::ReferencedBy(EdgeType::Contains, id),
            ])]
        );

        let i = ok(&PaneQuery {
            kinds: kinds(&["publication"]),
            scope: Scope::Item {
                id: ItemRef::Id { id },
            },
            ..Default::default()
        });
        assert_eq!(
            i.item_query.predicates,
            vec![Predicate::Eq("id".into(), Value::String(id.to_string()))]
        );
        assert_eq!(i.single_item, Some(id));
    }

    /// A Parent scope takes the parent's `Contains` targets only when the
    /// parent is a container the manifest lists (a library) and the pane
    /// queries only its listed members (publications). Figure and mail
    /// folders stay envelope-parent only.
    #[test]
    fn a_parent_scope_takes_contains_edges_only_under_a_library() {
        let id = Uuid::new_v4();
        let widened = vec![Predicate::Or(vec![
            Predicate::HasParent(id),
            Predicate::ReferencedBy(EdgeType::Contains, id),
        ])];
        let parent_only = vec![Predicate::HasParent(id)];
        let query = |k: &[&str], r: ItemRef| PaneQuery {
            kinds: kinds(k),
            scope: Scope::Parent { id: r },
            ..Default::default()
        };
        let literal = ItemRef::Id { id };
        let param = ItemRef::Param {
            name: "parent".into(),
        };
        let declared = |kind: &str, q: &PaneQuery| {
            compile(
                q,
                &[ParamDecl {
                    name: "parent".into(),
                    kind: kind.into(),
                    required: true,
                }],
                &Bindings::new().with("parent", id),
                &manifest(),
            )
            .expect("compiles")
        };

        // A literal id: the parent's kind is inferred from the members.
        let library = ok(&query(&["publication"], literal.clone()));
        assert_eq!(library.item_query.predicates, widened);
        assert_eq!(library.invalidation.edges, vec![(EdgeType::Contains, id)]);
        assert_eq!(library.invalidation.parents, vec![id]);
        for folder in [
            &["figure"][..],
            &["message"],
            &["publication", "figure"],
            &[],
        ] {
            // A kind with several schema refs adds its `In` after the scope.
            let c = ok(&query(folder, literal.clone()));
            assert_eq!(c.item_query.predicates[..1], parent_only[..], "{folder:?}");
            assert!(c.invalidation.edges.is_empty(), "{folder:?}");
        }

        // A declared parameter: its kind decides.
        let q = query(&["publication"], param.clone());
        assert_eq!(declared("library", &q).item_query.predicates, widened);
        assert_eq!(
            declared("collection", &q).item_query.predicates,
            parent_only
        );
        let figures = query(&["figure"], param);
        assert_eq!(
            declared("library", &figures).item_query.predicates,
            parent_only
        );
        assert_eq!(
            declared("collection", &figures).item_query.predicates,
            parent_only
        );
    }

    #[test]
    fn a_subtree_without_a_resolver_is_just_the_collection() {
        let root = Uuid::new_v4();
        let c = ok(&PaneQuery {
            kinds: kinds(&["manuscript"]),
            scope: Scope::CollectionSubtree {
                id: ItemRef::Id { id: root },
            },
            ..Default::default()
        });
        assert_eq!(
            c.item_query.predicates,
            vec![Predicate::ReferencedBy(EdgeType::Contains, root)]
        );
    }

    #[test]
    fn a_subtree_with_a_resolver_ors_every_descendant() {
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let grandchild = Uuid::new_v4();
        let expanded = vec![root, child, grandchild];
        let resolver = move |_: ItemId| expanded.clone();
        let c = compile_with(
            &PaneQuery {
                kinds: kinds(&["manuscript"]),
                scope: Scope::CollectionSubtree {
                    id: ItemRef::Id { id: root },
                },
                ..Default::default()
            },
            &[],
            &Bindings::new(),
            &manifest(),
            &resolver,
        )
        .expect("compiles");
        assert_eq!(
            c.item_query.predicates,
            vec![Predicate::Or(vec![
                Predicate::ReferencedBy(EdgeType::Contains, root),
                Predicate::ReferencedBy(EdgeType::Contains, child),
                Predicate::ReferencedBy(EdgeType::Contains, grandchild),
            ])]
        );
    }

    /// A resolver that forgets to include its own root would otherwise drop
    /// the folder's own members — the commonest case.
    #[test]
    fn a_resolver_that_omits_the_root_still_gets_the_root() {
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let resolver = move |_: ItemId| vec![child];
        let c = compile_with(
            &PaneQuery {
                kinds: kinds(&["manuscript"]),
                scope: Scope::CollectionSubtree {
                    id: ItemRef::Id { id: root },
                },
                ..Default::default()
            },
            &[],
            &Bindings::new(),
            &manifest(),
            &resolver,
        )
        .expect("compiles");
        assert_eq!(
            c.item_query.predicates,
            vec![Predicate::Or(vec![
                Predicate::ReferencedBy(EdgeType::Contains, root),
                Predicate::ReferencedBy(EdgeType::Contains, child),
            ])]
        );
    }

    #[test]
    fn filters_compile_to_their_predicates() {
        let c = ok(&PaneQuery {
            kinds: kinds(&["manuscript"]),
            filters: vec![
                Filter::Flag {
                    color: Some("red".into()),
                },
                Filter::Starred { starred: true },
                Filter::Read { read: false },
                Filter::Status {
                    status: "dismissed".into(),
                },
                Filter::Tag {
                    path: "methods/sims".into(),
                },
            ],
            ..Default::default()
        });
        assert_eq!(
            c.item_query.predicates,
            vec![
                Predicate::HasFlag(Some("red".into())),
                Predicate::IsStarred(true),
                Predicate::IsRead(false),
                Predicate::Eq("payload.status".into(), Value::String("dismissed".into())),
                Predicate::HasTag("methods/sims".into()),
            ]
        );
    }

    #[test]
    fn a_date_range_on_an_envelope_column_compiles_to_epoch_millis() {
        let c = ok(&PaneQuery {
            kinds: kinds(&["manuscript"]),
            filters: vec![Filter::DateRange {
                field: "created".into(),
                from: Some("2026-01-01T00:00:00Z".into()),
                to: Some("2026-02-01".into()),
            }],
            ..Default::default()
        });
        assert_eq!(
            c.item_query.predicates,
            vec![
                Predicate::Gte("created".into(), Value::Int(1_767_225_600_000)),
                // Date-only UPPER bound = the last millisecond of that day.
                Predicate::Lte("created".into(), Value::Int(1_769_990_399_999)),
            ]
        );
    }

    /// `from == to` on a single day must match that day, not nothing — which
    /// is what midnight on both ends would have given.
    #[test]
    fn a_single_day_range_covers_that_whole_day() {
        let c = ok(&PaneQuery {
            kinds: kinds(&["manuscript"]),
            filters: vec![Filter::DateRange {
                field: "modified".into(),
                from: Some("2026-02-01".into()),
                to: Some("2026-02-01".into()),
            }],
            ..Default::default()
        });
        let (from, to) = match c.item_query.predicates.as_slice() {
            [Predicate::Gte(_, Value::Int(f)), Predicate::Lte(_, Value::Int(t))] => (*f, *t),
            other => panic!("unexpected predicates: {other:?}"),
        };
        assert_eq!(from, 1_769_904_000_000, "midnight UTC on the 1st");
        assert_eq!(to, 1_769_990_399_999, "23:59:59.999 UTC on the 1st");
        assert_eq!(to - from, 86_399_999, "one whole day, inclusive");
    }

    /// A bound that names a TIME is taken literally at both ends — only a
    /// date-only upper bound is widened.
    #[test]
    fn a_timed_upper_bound_is_not_rolled_to_end_of_day() {
        let c = ok(&PaneQuery {
            kinds: kinds(&["manuscript"]),
            filters: vec![Filter::DateRange {
                field: "created".into(),
                from: None,
                to: Some("2026-02-01T00:00:00Z".into()),
            }],
            ..Default::default()
        });
        assert_eq!(
            c.item_query.predicates,
            vec![Predicate::Lte(
                "created".into(),
                Value::Int(1_769_904_000_000)
            )]
        );
    }

    #[test]
    fn a_date_range_on_a_payload_field_stays_a_string() {
        let c = ok(&PaneQuery {
            kinds: kinds(&["manuscript"]),
            filters: vec![Filter::DateRange {
                field: "payload.due".into(),
                from: Some("2026-01-01".into()),
                to: None,
            }],
            ..Default::default()
        });
        assert_eq!(
            c.item_query.predicates,
            vec![Predicate::Gte(
                "payload.due".into(),
                Value::String("2026-01-01".into())
            )]
        );
    }

    #[test]
    fn a_malformed_envelope_date_is_a_typed_error_not_a_silent_match_all() {
        let err = compile(
            &PaneQuery {
                kinds: kinds(&["manuscript"]),
                filters: vec![Filter::DateRange {
                    field: "modified".into(),
                    from: Some("last tuesday".into()),
                    to: None,
                }],
                ..Default::default()
            },
            &[],
            &Bindings::new(),
            &manifest(),
        )
        .expect_err("malformed date");
        assert!(matches!(
            err,
            PaneQueryError::InvalidDate { ref field, ref value }
                if field == "modified" && value == "last tuesday"
        ));
    }

    #[test]
    fn text_routes_through_the_fts_predicate() {
        let c = ok(&PaneQuery {
            kinds: kinds(&["publication"]),
            text: Some("  dark matter  ".into()),
            ..Default::default()
        });
        assert_eq!(
            c.item_query.predicates,
            vec![Predicate::Contains("title".into(), "dark matter".into())]
        );
    }

    #[test]
    fn a_blank_text_term_adds_no_predicate() {
        let c = ok(&PaneQuery {
            kinds: kinds(&["publication"]),
            text: Some("   ".into()),
            ..Default::default()
        });
        assert!(c.item_query.predicates.is_empty());
    }

    #[test]
    fn relation_directions_pick_the_matching_edge_predicate() {
        let from = Uuid::new_v4();
        let outgoing = ok(&PaneQuery {
            kinds: kinds(&["publication"]),
            relation: Some(RelationWalk {
                edge: EdgeType::Cites.into(),
                from: ItemRef::Id { id: from },
                direction: Direction::Outgoing,
            }),
            ..Default::default()
        });
        assert_eq!(
            outgoing.item_query.predicates,
            vec![Predicate::ReferencedBy(EdgeType::Cites, from)]
        );

        let incoming = ok(&PaneQuery {
            kinds: kinds(&["manuscript"]),
            relation: Some(RelationWalk {
                edge: EdgeType::Cites.into(),
                from: ItemRef::Id { id: from },
                direction: Direction::Incoming,
            }),
            ..Default::default()
        });
        assert_eq!(
            incoming.item_query.predicates,
            vec![Predicate::HasReference(EdgeType::Cites, from)]
        );
    }

    #[test]
    fn sort_and_limit_carry_over() {
        let c = ok(&PaneQuery {
            kinds: kinds(&["publication"]),
            sort: vec![
                SortKey {
                    field: "created".into(),
                    descending: true,
                },
                SortKey {
                    field: "payload.title".into(),
                    descending: false,
                },
            ],
            limit: Some(50),
            ..Default::default()
        });
        assert_eq!(
            c.item_query.sort,
            vec![
                SortDescriptor {
                    field: "created".into(),
                    ascending: false,
                },
                SortDescriptor {
                    field: "payload.title".into(),
                    ascending: true,
                },
            ]
        );
        assert_eq!(c.item_query.limit, Some(50));
        assert!(c.item_query.include_tags && c.item_query.include_references);
    }

    // ── Typed failures ───────────────────────────────────────────────────

    #[test]
    fn an_unknown_kind_is_a_typed_error() {
        let err = compile(
            &PaneQuery {
                kinds: kinds(&["publiction"]),
                ..Default::default()
            },
            &[],
            &Bindings::new(),
            &manifest(),
        )
        .expect_err("unknown kind");
        assert_eq!(
            err,
            PaneQueryError::UnknownKind {
                kind: "publiction".into()
            }
        );
    }

    #[test]
    fn an_unbound_required_param_is_a_typed_error() {
        let err = compile(
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
                required: true,
            }],
            &Bindings::new(),
            &manifest(),
        )
        .expect_err("unbound required param");
        assert_eq!(
            err,
            PaneQueryError::UnboundParam {
                name: "collection".into()
            }
        );
    }

    #[test]
    fn a_param_declared_as_the_wrong_kind_is_a_typed_error() {
        let err = compile(
            &PaneQuery {
                kinds: kinds(&["publication"]),
                scope: Scope::Item {
                    id: ItemRef::Param {
                        name: "item".into(),
                    },
                },
                ..Default::default()
            },
            &[ParamDecl {
                name: "item".into(),
                kind: "manuscript".into(),
                required: false,
            }],
            &Bindings::new().with("item", Uuid::new_v4()),
            &manifest(),
        )
        .expect_err("kind mismatch");
        assert_eq!(
            err,
            PaneQueryError::ParamKindMismatch {
                name: "item".into(),
                expected: "publication".into(),
                actual: "manuscript".into(),
            }
        );
    }

    /// A collection scope's parameter names the COLLECTION, not a row inside
    /// it — a pane wired to a `publication` channel there would silently show
    /// the collections containing nothing.
    #[test]
    fn a_collection_param_declared_as_the_wrong_kind_is_a_typed_error() {
        for scope in [
            Scope::Collection {
                id: ItemRef::Param {
                    name: "folder".into(),
                },
            },
            Scope::CollectionSubtree {
                id: ItemRef::Param {
                    name: "folder".into(),
                },
            },
        ] {
            let err = compile(
                &PaneQuery {
                    kinds: kinds(&["publication"]),
                    scope,
                    ..Default::default()
                },
                &[ParamDecl {
                    name: "folder".into(),
                    kind: "publication".into(),
                    required: false,
                }],
                &Bindings::new().with("folder", Uuid::new_v4()),
                &manifest(),
            )
            .expect_err("kind mismatch");
            assert_eq!(
                err,
                PaneQueryError::ParamKindMismatch {
                    name: "folder".into(),
                    expected: "collection".into(),
                    actual: "publication".into(),
                }
            );
        }
    }

    #[test]
    fn a_collection_param_declared_as_a_collection_is_accepted() {
        let id = Uuid::new_v4();
        let c = compile(
            &PaneQuery {
                kinds: kinds(&["publication"]),
                scope: Scope::Collection {
                    id: ItemRef::Param {
                        name: "folder".into(),
                    },
                },
                ..Default::default()
            },
            &[ParamDecl {
                name: "folder".into(),
                kind: "collection".into(),
                required: true,
            }],
            &Bindings::new().with("folder", id),
            &manifest(),
        )
        .expect("compiles");
        assert_eq!(
            c.item_query.predicates,
            vec![Predicate::ReferencedBy(EdgeType::Contains, id)]
        );
    }

    #[test]
    fn a_parent_param_declared_as_neither_library_nor_collection_is_a_typed_error() {
        let err = compile(
            &PaneQuery {
                kinds: kinds(&["publication"]),
                scope: Scope::Parent {
                    id: ItemRef::Param {
                        name: "library".into(),
                    },
                },
                ..Default::default()
            },
            &[ParamDecl {
                name: "library".into(),
                kind: "manuscript".into(),
                required: false,
            }],
            &Bindings::new().with("library", Uuid::new_v4()),
            &manifest(),
        )
        .expect_err("kind mismatch");
        assert_eq!(
            err,
            PaneQueryError::ParamKindMismatch {
                name: "library".into(),
                expected: "library".into(),
                actual: "manuscript".into(),
            }
        );
    }

    /// A figure folder collects its members through the envelope parent
    /// (`collection_ops` `Membership::EnvelopeParent`), so a `collection`
    /// parameter on a `Parent` scope is legitimate.
    #[test]
    fn a_parent_param_may_name_a_collection_as_well_as_a_library() {
        for kind in ["library", "collection"] {
            let id = Uuid::new_v4();
            let c = compile(
                &PaneQuery {
                    kinds: kinds(&["figure"]),
                    scope: Scope::Parent {
                        id: ItemRef::Param {
                            name: "container".into(),
                        },
                    },
                    ..Default::default()
                },
                &[ParamDecl {
                    name: "container".into(),
                    kind: kind.into(),
                    required: false,
                }],
                &Bindings::new().with("container", id),
                &manifest(),
            )
            .unwrap_or_else(|e| panic!("{kind}: {e}"));
            assert_eq!(c.item_query.predicates, vec![Predicate::HasParent(id)]);
        }
    }

    /// The mismatch check needs ONE kind to disagree with; a heterogeneous
    /// pane has no single expectation to report.
    #[test]
    fn a_multi_kind_item_scope_does_not_check_the_param_kind() {
        let id = Uuid::new_v4();
        let c = compile(
            &PaneQuery {
                kinds: kinds(&["publication", "manuscript"]),
                scope: Scope::Item {
                    id: ItemRef::Param {
                        name: "item".into(),
                    },
                },
                ..Default::default()
            },
            &[ParamDecl {
                name: "item".into(),
                kind: "figure".into(),
                required: false,
            }],
            &Bindings::new().with("item", id),
            &manifest(),
        )
        .expect("compiles");
        assert_eq!(c.single_item, Some(id));
    }

    #[test]
    fn an_unbound_optional_param_compiles_to_an_empty_result() {
        let c = compile(
            &PaneQuery {
                kinds: kinds(&["publication"]),
                scope: Scope::Item {
                    id: ItemRef::Param {
                        name: "item".into(),
                    },
                },
                ..Default::default()
            },
            &[ParamDecl {
                name: "item".into(),
                kind: "publication".into(),
                required: false,
            }],
            &Bindings::new(),
            &manifest(),
        )
        .expect("compiles to an empty pane, not an error");
        assert_eq!(c.single_item, None);
        assert!(c
            .item_query
            .predicates
            .contains(&Predicate::In("id".into(), vec![])));
    }

    #[test]
    fn an_undeclared_param_is_optional() {
        let c = compile(
            &PaneQuery {
                kinds: kinds(&["publication"]),
                scope: Scope::Collection {
                    id: ItemRef::Param {
                        name: "nobody-declared-me".into(),
                    },
                },
                ..Default::default()
            },
            &[],
            &Bindings::new(),
            &manifest(),
        )
        .expect("compiles");
        assert!(c
            .item_query
            .predicates
            .contains(&Predicate::In("id".into(), vec![])));
    }

    #[test]
    fn an_empty_manifest_has_no_kinds_to_offer() {
        let err = compile(
            &PaneQuery::default(),
            &[],
            &Bindings::new(),
            &KindManifest::default(),
        )
        .expect_err("no kinds");
        assert_eq!(err, PaneQueryError::NoKinds);
    }

    #[test]
    fn errors_round_trip_through_serde() {
        for err in [
            PaneQueryError::UnknownKind {
                kind: "nope".into(),
            },
            PaneQueryError::UnboundParam { name: "x".into() },
            PaneQueryError::ParamKindMismatch {
                name: "x".into(),
                expected: "publication".into(),
                actual: "manuscript".into(),
            },
            PaneQueryError::NoKinds,
            PaneQueryError::InvalidDate {
                field: "created".into(),
                value: "soon".into(),
            },
        ] {
            let json = serde_json::to_string(&err).unwrap();
            assert_eq!(serde_json::from_str::<PaneQueryError>(&json).unwrap(), err);
        }
    }
}
