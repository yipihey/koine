//! The outline sidebar as decisions (plan wave 6, W3 — L8 leaf 4).
//!
//! The navigator pane of every preset is an `outline` pane. What it DRAWS is
//! the chassis sidebar — the same rows, the same counts, the same context
//! menus — because the Swift host renders the sidebar the chassis already
//! has rather than a second one. What it DOES when a row is selected is this
//! module: a row becomes an [`OutlineNode`], [`outline_target`] says which
//! pane query (or which legacy route) that node is, and [`outline_verbs`]
//! says which D8 verbs retarget the `list` pane to it. Swift applies the verbs
//! and decides nothing (the plan's "Rust decides with a test; Swift maps").
//!
//! # Three answers, never a guess
//!
//! * [`OutlineTarget::Query`] — the node is a value in the algebra. The list
//!   pane's query becomes it by `set-query` (or `set-pane` when the pane is
//!   not a `list` right now), and the detail pane follows the channel as it
//!   always did.
//! * [`OutlineTarget::Legacy`] — the node is NOT a value in the algebra: one
//!   of [`crate::MATERIALIZE_FIRST`]'s five sections, or a route whose query
//!   would lie (a task's lifecycle is `payload.state`, which `Filter::Status`
//!   does not read). The list pane becomes a `legacy` pane scoped to THAT
//!   route — `view_state` names the section and the node, so the host renders
//!   one section's route and never the whole chassis. The reason travels with
//!   it, because this is the materialization work that remains and it is
//!   recorded where it happens rather than hidden.
//! * [`OutlineTarget::Inert`] — the node navigates nowhere in this app. No
//!   verb is sent, and the reason is logged.
//!
//! # Which sections an app has
//!
//! [`outline_sections`] is the app's section table: its named queries (the
//! `SidebarSectionType` cases of `presets::named_queries`) plus the
//! [`crate::MATERIALIZE_FIRST`] sections the app shows. The Swift outline
//! shows exactly these sections; a section the chassis would show and this
//! table does not name is dropped and logged, never drawn.

use std::collections::BTreeMap;

use impress_layout::preset::{detail_query, DETAIL_PARAM};
use impress_layout::{PaneRef, PaneSpec, Role, Verb, ViewKindId};
use impress_pane_query::{Filter, ItemRef, PaneQuery, Scope, SortKey};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::presets::{
    is_superseded_list_query, named_queries, q, shipped_list_queries, MATERIALIZE_FIRST,
};

// ---------------------------------------------------------------------------
// The node — what a selected sidebar row IS, in data
// ---------------------------------------------------------------------------

/// A selected outline row, as the Swift host describes it.
///
/// One variant per SHAPE of row, not per `ImbibTab` case: the three
/// collection tabs (library, inbox, exploration) are one `collection`, and
/// every record-kind row (figures, mail, agents, manuscripts) is one `record`
/// with a [`RecordScope`], exactly as the chassis' own `RecordRoute`
/// collapsed fourteen cases into one. Section names are the
/// `SidebarSectionType` CASE names (`manuscripts`, not its raw value
/// `journal`), which is how [`named_queries`] spells them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "node", rename_all = "kebab-case")]
pub enum OutlineNode {
    /// A section row (the Inbox header, or a leaf that IS its section:
    /// All Artifacts, Dismissed, Cited in Manuscripts, the Review Queue).
    Section { section: String },
    /// A library row.
    Library { id: Uuid },
    /// A collection row — in a library, in the Inbox, or in Exploration.
    Collection { id: Uuid },
    /// A smart search: an inbox feed, a library feed, an exploration search.
    SmartSearch { id: Uuid, section: String },
    /// A library shared with the user.
    SharedLibrary { id: Uuid },
    /// A SciX / ADS library.
    ScixLibrary { id: Uuid },
    /// One of the online search forms (`SearchFormType`'s raw value).
    SearchForm { form: String },
    /// The feed-creation / feed-editing forms. `feed` names the feed being
    /// edited (Edit Feed…), `library` the library a new feed is for (a
    /// library's Add Feed…); neither is the generic "pick a search form"
    /// route. Both optional and omitted when absent, so `{"node":
    /// "feed-form"}` is still the generic form (review PH-M1: the ids were
    /// dropped, and Edit Feed… opened feed creation).
    FeedForm {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        feed: Option<Uuid>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        library: Option<Uuid>,
    },
    /// Recently viewed or hand-added papers.
    Recent,
    /// One artifact type (`ArtifactType`'s raw value).
    ArtifactType {
        #[serde(rename = "type")]
        artifact_type: String,
    },
    /// An app-owned whole-pane surface.
    CustomSurface { id: String },
    /// A record kind's row: which kind (the manifest's short id, `figure`,
    /// `task`, `message`, …) and which subset of it.
    Record { kind: String, scope: RecordScope },
    /// One record, full-pane — the deep-link shape.
    RecordDetail { kind: String, id: String },
    /// A non-record route a shell declares (imprint's Submissions inbox).
    Auxiliary { route: String },
}

/// The subset of a record kind a `record` row names — the chassis'
/// `RecordSidebarScope`, minus the `section` case (a section is a
/// [`OutlineNode::Section`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "kebab-case")]
pub enum RecordScope {
    /// Every record of the kind.
    All,
    /// One value of the kind's lifecycle field.
    Status { status: String },
    /// One folder of the kind's collection binding.
    Folder { id: Uuid },
    /// Flagged records; `None` = any colour.
    Flagged {
        #[serde(default)]
        color: Option<String>,
    },
    /// Records carrying a tag path (a Tags row, a watched folder's
    /// provenance tag).
    Tag { path: String },
    /// A subset only the host can name (`RecordSidebarScope.host`).
    Host { key: String },
}

// ---------------------------------------------------------------------------
// The target — what the node means
// ---------------------------------------------------------------------------

/// What an outline row selects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "target", rename_all = "kebab-case")]
pub enum OutlineTarget {
    /// The row is this query. The list pane shows it.
    Query { query: PaneQuery },
    /// The row is not a value in the algebra: host its route in a `legacy`
    /// pane scoped to `section`. `reason` is the materialization work left.
    Legacy {
        #[serde(default)]
        section: Option<String>,
        reason: String,
    },
    /// The row navigates nowhere here.
    Inert { reason: String },
}

/// The `view_state` key a scoped legacy pane carries its section under —
/// `impress_layout::view_state::SECTION`, the one spelling (review PH-M7).
pub const LEGACY_SECTION_KEY: &str = impress_layout::view_state::SECTION;
/// The `view_state` key a scoped legacy pane carries its node under.
pub const LEGACY_NODE_KEY: &str = impress_layout::view_state::NODE;
/// The `view_state` key a scoped legacy pane carries its reason under.
pub const LEGACY_REASON_KEY: &str = impress_layout::view_state::REASON;

/// The five [`MATERIALIZE_FIRST`] sections by their `SidebarSectionType` case
/// name — the table's first word, which is how it spells them.
pub fn deferred_section_names() -> Vec<&'static str> {
    MATERIALIZE_FIRST
        .iter()
        .map(|(name, _)| name.split_whitespace().next().unwrap_or(name))
        .collect()
}

/// The [`MATERIALIZE_FIRST`] reason for a section, by case name.
fn deferred_reason(section: &str) -> Option<&'static str> {
    MATERIALIZE_FIRST
        .iter()
        .find(|(name, _)| name.split_whitespace().next() == Some(section))
        .map(|(_, reason)| *reason)
}

/// The legacy target for one of the five [`MATERIALIZE_FIRST`] sections.
fn deferred(section: &str) -> OutlineTarget {
    OutlineTarget::Legacy {
        section: Some(section.to_string()),
        reason: deferred_reason(section)
            .unwrap_or("not a value in the pane-query algebra")
            .to_string(),
    }
}

/// A route that is not one of the five but whose query would lie or does not
/// exist: host it, and say why.
fn legacy(section: Option<&str>, reason: &str) -> OutlineTarget {
    OutlineTarget::Legacy {
        section: section.map(str::to_string),
        reason: reason.to_string(),
    }
}

fn newest_first() -> Vec<SortKey> {
    vec![SortKey {
        field: "created".into(),
        descending: true,
    }]
}

fn publications(scope: Scope) -> PaneQuery {
    PaneQuery {
        kinds: vec!["publication".into()],
        scope,
        sort: newest_first(),
        ..PaneQuery::default()
    }
}

/// The section a record kind's rows live in, for a legacy route's scope.
fn section_of_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "publication" => Some("libraries"),
        "manuscript" => Some("manuscripts"),
        "figure" => Some("figures"),
        "message" => Some("mail"),
        "task" | "agent-run" => Some("agents"),
        _ => None,
    }
}

/// The query of "every record of `kind`", in that kind's section order.
///
/// The per-kind sorts are the named queries' own (`q::figures`, `q::mail`,
/// `q::agents`, `q::manuscripts`), so a row and the section it sits under
/// list the same records in the same order.
fn all_of_kind(kind: &str) -> PaneQuery {
    match kind {
        "figure" => q::figures(),
        "message" => q::mail(),
        "task" => q::agents(),
        "manuscript" => q::manuscripts(),
        "artifact" => q::artifacts(),
        other => PaneQuery {
            kinds: vec![other.to_string()],
            sort: newest_first(),
            ..PaneQuery::default()
        },
    }
}

/// How a folder of `kind` holds its members, per `collection_ops::Membership`.
///
/// Transcribed, because a kit-adjacent crate may not reach `impress-core`'s
/// domain modules (ADR-0033 D7): figure folders file their members through
/// the envelope parent (`Membership::EnvelopeParent`), mail folders are the
/// messages' envelope parent (IMAP mailboxes, `countItems parentId`),
/// manuscript folders are a subtree of `Contains` edges (the L0 sidebar
/// table's "folder and everything under it"), and every other binding is a
/// `Contains` edge (`Membership::ContainsEdge`). `folder_scopes_follow_the_
/// membership_table` pins it.
fn folder_scope(kind: &str, id: Uuid) -> Scope {
    let id = ItemRef::Id { id };
    match kind {
        "figure" | "message" => Scope::Parent { id },
        "manuscript" => Scope::CollectionSubtree { id },
        _ => Scope::Collection { id },
    }
}

/// Replace every `$param` a caller bound with its id, leaving the rest.
fn substitute(query: &mut PaneQuery, bindings: &BTreeMap<String, Uuid>) {
    fn fill(item: &mut ItemRef, bindings: &BTreeMap<String, Uuid>) {
        if let ItemRef::Param { name } = item {
            if let Some(id) = bindings.get(name) {
                *item = ItemRef::Id { id: *id };
            }
        }
    }
    match &mut query.scope {
        Scope::All => {}
        Scope::Collection { id }
        | Scope::CollectionSubtree { id }
        | Scope::Parent { id }
        | Scope::Item { id } => fill(id, bindings),
    }
    if let Some(walk) = &mut query.relation {
        fill(&mut walk.from, bindings);
    }
}

/// The parameters a query still names after substitution.
fn unbound_params(query: &PaneQuery) -> Vec<String> {
    let mut out = Vec::new();
    let scope_ref = match &query.scope {
        Scope::All => None,
        Scope::Collection { id }
        | Scope::CollectionSubtree { id }
        | Scope::Parent { id }
        | Scope::Item { id } => Some(id),
    };
    for item in scope_ref
        .into_iter()
        .chain(query.relation.as_ref().map(|w| &w.from))
    {
        if let ItemRef::Param { name } = item {
            out.push(name.clone());
        }
    }
    out
}

/// What `node` selects in `app_id`.
///
/// `bindings` are the ids the host knows for the parameters a section query
/// names (`library` = the Inbox library for the Inbox row,
/// `dismissed_library` for Dismissed). A section whose query still names a
/// parameter nobody bound is hosted as a legacy route instead: an unbound
/// parameter compiles to "matches nothing" (ADR-0031 D3), and an empty list
/// under a row that has papers is exactly the silent lie the schema-ref rule
/// exists to prevent. Cited in Manuscripts is the case in point: the named
/// query is "cited by `$manuscript`", the row means "cited by ANY
/// manuscript".
pub fn outline_target(
    app_id: &str,
    node: &OutlineNode,
    bindings: &BTreeMap<String, Uuid>,
) -> OutlineTarget {
    match node {
        OutlineNode::Section { section } => {
            if deferred_section_names().contains(&section.as_str()) {
                return deferred(section);
            }
            let named = named_queries(app_id);
            let Some(mut query) = named.get(section.as_str()).cloned() else {
                return OutlineTarget::Inert {
                    reason: format!("{app_id} has no section '{section}'"),
                };
            };
            substitute(&mut query, bindings);
            let unbound = unbound_params(&query);
            if unbound.is_empty() {
                OutlineTarget::Query { query }
            } else {
                legacy(
                    Some(section),
                    &format!(
                        "the section's query needs ${} and the row binds no such item",
                        unbound.join(", $")
                    ),
                )
            }
        }
        OutlineNode::Library { id } => OutlineTarget::Query {
            query: publications(Scope::Parent {
                id: ItemRef::Id { id: *id },
            }),
        },
        OutlineNode::Collection { id } => OutlineTarget::Query {
            query: publications(Scope::Collection {
                id: ItemRef::Id { id: *id },
            }),
        },
        OutlineNode::SmartSearch { section, .. } => legacy(
            Some(section),
            "a smart search is a stored search, not stored results: until its hits are \
             materialized (a feed files them into the Inbox, an exploration into a \
             collection) there is no relation for a pane to query",
        ),
        OutlineNode::SharedLibrary { .. } => deferred("sharedWithMe"),
        OutlineNode::ScixLibrary { .. } => deferred("scixLibraries"),
        OutlineNode::SearchForm { .. } | OutlineNode::FeedForm { .. } => deferred("search"),
        OutlineNode::Recent => legacy(
            Some("inbox"),
            "'recent' is a view history (viewed or added by hand), which is not a field \
             or an edge the algebra reads",
        ),
        OutlineNode::ArtifactType { .. } => legacy(
            Some("artifacts"),
            "an artifact TYPE is one schema ref inside the `artifact` kind, and the algebra \
             filters by kind, never by schema ref",
        ),
        OutlineNode::CustomSurface { .. } => legacy(
            None,
            "an app-owned surface is a view, not a query over records",
        ),
        OutlineNode::RecordDetail { kind, .. } => legacy(
            section_of_kind(kind),
            "a deep link to one record renders full-pane with no list",
        ),
        OutlineNode::Auxiliary { .. } => legacy(
            Some("manuscripts"),
            "an auxiliary route is a host-declared view, not a query over records",
        ),
        OutlineNode::Record { kind, scope } => record_target(kind, scope),
    }
}

fn record_target(kind: &str, scope: &RecordScope) -> OutlineTarget {
    match scope {
        RecordScope::All => OutlineTarget::Query {
            query: all_of_kind(kind),
        },
        RecordScope::Status { status } => {
            if kind == "task" {
                // `Filter::Status` compiles to `payload.status`; a task keeps
                // its lifecycle in `payload.state` (AgentTaskPayload's key,
                // with several spellings per state). A query here would list
                // zero tasks under a row whose badge says forty.
                return legacy(
                    section_of_kind(kind),
                    "a task's lifecycle is `payload.state` (several spellings per state), and \
                     `Filter::Status` reads `payload.status`",
                );
            }
            let mut query = all_of_kind(kind);
            query.filters.push(Filter::Status {
                status: status.clone(),
            });
            OutlineTarget::Query { query }
        }
        RecordScope::Folder { id } => {
            let mut query = all_of_kind(kind);
            query.scope = folder_scope(kind, *id);
            OutlineTarget::Query { query }
        }
        RecordScope::Flagged { color } => {
            let mut query = all_of_kind(kind);
            query.filters.push(Filter::Flag {
                color: color.clone(),
            });
            OutlineTarget::Query { query }
        }
        RecordScope::Tag { path } => {
            let mut query = all_of_kind(kind);
            query.filters.push(Filter::Tag { path: path.clone() });
            OutlineTarget::Query { query }
        }
        RecordScope::Host { key } => legacy(
            section_of_kind(kind),
            &format!("'{key}' is a subset only the host can name (RecordSidebarScope.host)"),
        ),
    }
}

// ---------------------------------------------------------------------------
// The verbs — what selecting the row does to the tree
// ---------------------------------------------------------------------------

/// The panes a row retargets, as they are now. Both optional: a layout the
/// user rearranged may have no pane with the `detail` role, and a
/// `list`-less layout gets no verbs at all.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutlinePanes {
    pub list: Option<PaneSpec>,
    pub detail: Option<PaneSpec>,
}

/// The verbs that make the tree show `target`, and nothing else.
///
/// * **Query** — `set-query` on the pane with the `list` role, or `set-pane`
///   when that pane is not a `list` right now (a legacy route was up), so a
///   query row always lands in a list. When the detail pane's `item`
///   parameter names a different kind from the new list's — a figure folder
///   selected in impress, whose detail pane was reading publications — the
///   detail pane is re-pointed too (`detail_query` of the new list, the
///   parameter's kind updated), because a channel carries one value PER KIND
///   (D3) and a publication parameter would never hear a figure selection.
///   A library or collection row also publishes itself on the navigator's
///   channel (`select`), which is what every pane bound to `$library` has
///   always followed.
/// * **Legacy** — `set-pane` on the `list` pane: view kind `legacy`, its
///   `view_state` naming the section, the node and the reason. Query,
///   params, channel and role are kept, so returning to a query row is one
///   `set-pane` back. The detail pane's selection is cleared with it (an
///   empty `select` of its kind): the hosted route brings its own list and
///   detail, and the tree's `info` pane beside it would otherwise keep
///   showing the paper chosen from the list the route replaced (W5).
/// * **Inert** — nothing.
///
/// A verb that would change nothing is not emitted: selecting the row the
/// list already shows is a no-op, not an undo entry.
pub fn outline_verbs(
    node: &OutlineNode,
    target: &OutlineTarget,
    panes: &OutlinePanes,
) -> Vec<Verb> {
    let Some(list) = panes.list.as_ref() else {
        return Vec::new();
    };
    let list_ref = PaneRef::role(Role::LIST);
    let mut verbs = Vec::new();
    match target {
        OutlineTarget::Query { query } => {
            match node {
                OutlineNode::Library { id } => verbs.push(Verb::Select {
                    target: PaneRef::role(Role::NAVIGATOR),
                    kind: "library".into(),
                    ids: vec![*id],
                }),
                OutlineNode::Collection { id } => verbs.push(Verb::Select {
                    target: PaneRef::role(Role::NAVIGATOR),
                    kind: "collection".into(),
                    ids: vec![*id],
                }),
                _ => {}
            }
            if list.view_kind != ViewKindId::LIST {
                let mut spec = list.clone();
                spec.query = query.clone();
                spec.view_kind = ViewKindId::LIST;
                spec.view_state = serde_json::Value::Null;
                verbs.push(Verb::SetPane {
                    target: list_ref,
                    spec,
                });
            } else if list.query != *query {
                verbs.push(Verb::SetQuery {
                    target: list_ref,
                    query: query.clone(),
                });
            }
            if let (Some(detail), Some(kind)) = (panes.detail.as_ref(), query.kinds.first()) {
                let current = detail.param(DETAIL_PARAM).map(|b| b.decl.kind.as_str());
                if current.is_some() && current != Some(kind.as_str()) {
                    let mut spec = detail.clone();
                    spec.query = detail_query(query);
                    for binding in spec.params.iter_mut() {
                        if binding.decl.name == DETAIL_PARAM {
                            binding.decl.kind = kind.clone();
                        }
                    }
                    verbs.push(Verb::SetPane {
                        target: PaneRef::role(Role::DETAIL),
                        spec,
                    });
                }
            }
        }
        OutlineTarget::Legacy { section, reason } => {
            let view_state = serde_json::json!({
                LEGACY_SECTION_KEY: section,
                LEGACY_NODE_KEY: node,
                LEGACY_REASON_KEY: reason,
            });
            if list.view_kind != ViewKindId::LEGACY || list.view_state != view_state {
                let mut spec = list.clone();
                spec.view_kind = ViewKindId::LEGACY;
                spec.view_state = view_state;
                verbs.push(Verb::SetPane {
                    target: list_ref,
                    spec,
                });
                verbs.extend(clear_detail(panes));
            }
        }
        OutlineTarget::Inert { .. } => {}
    }
    verbs
}

/// "Nothing of the detail pane's kind is selected", published where the list
/// publishes its selection — so the detail pane renders its empty state
/// (`Channels::publish`: an empty selection is a real value). `None` when
/// there is no detail pane or it binds no `item`.
fn clear_detail(panes: &OutlinePanes) -> Option<Verb> {
    let detail = panes.detail.as_ref()?;
    let binding = detail.param(DETAIL_PARAM)?;
    Some(Verb::Select {
        target: PaneRef::role(Role::LIST),
        kind: binding.decl.kind.clone(),
        ids: Vec::new(),
    })
}

/// The verbs for "the selected row is gone" — its library or collection was
/// deleted while it was the outline's selection (W5).
///
/// The chassis' own sidebar does NOT fall back to the parent: deleting the
/// selected collection sets its selection to nil
/// (`ImbibSidebarViewModel.deleteFolder` / `deleteCollection`), deleting the
/// selected library leaves a selection that resolves to nothing, and in both
/// cases the content column shows "No Selection" — no list, no detail. The
/// tree matches that: the list pane keeps its (now empty) query and so lists
/// nothing, the navigator's channel stops carrying the dead library or
/// collection (every pane bound to `$library` unbinds rather than holding
/// it), and the detail pane stops showing the paper chosen from the deleted
/// row's list. No row is selected for the user, exactly as the chassis
/// selects none.
pub fn outline_cleared_verbs(node: &OutlineNode, panes: &OutlinePanes) -> Vec<Verb> {
    if panes.list.is_none() {
        return Vec::new();
    }
    let mut verbs = Vec::new();
    let navigator_kind = match node {
        OutlineNode::Library { .. } => Some("library"),
        OutlineNode::Collection { .. } => Some("collection"),
        _ => None,
    };
    if let Some(kind) = navigator_kind {
        verbs.push(Verb::Select {
            target: PaneRef::role(Role::NAVIGATOR),
            kind: kind.into(),
            ids: Vec::new(),
        });
    }
    verbs.extend(clear_detail(panes));
    verbs
}

/// Should the outline's FIRST selection — the one the chassis makes by
/// itself at launch (its default section) — retarget the list?
///
/// Only when the list still shows the preset's own list query. A layout the
/// user left on a collection, restored at launch, is the user's; a default
/// selection the user never made must not overwrite it. A list still on the
/// preset query is the cold-start case, where the outline's highlighted row
/// and the list should agree.
///
/// "The preset's own list query" is any revision of it the table has shipped
/// ([`shipped_list_queries`]): a layout stored before the Inbox stopped
/// filtering to unread (W5) still holds revision 1, and that list is still
/// the preset's, not a place the user chose — and so is revision 1 with its
/// `$library` already bound, which is what the outline's Inbox row itself
/// wrote before W5 ([`is_superseded_list_query`]).
pub fn initial_selection_applies(app_id: &str, panes: &OutlinePanes) -> bool {
    let Some(list) = panes.list.as_ref() else {
        return false;
    };
    list.view_kind == ViewKindId::LIST
        && (shipped_list_queries(app_id).contains(&list.query)
            || is_superseded_list_query(&list.query))
}

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

/// One section of an app's outline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutlineSection {
    /// The `SidebarSectionType` case name.
    pub section: String,
    /// `true` when the section is one of the five [`MATERIALIZE_FIRST`]
    /// sections and its rows open a legacy pane.
    pub legacy: bool,
}

/// The [`MATERIALIZE_FIRST`] sections each app shows (its `visibleSections`
/// minus its named queries). `search` is named (the local search) and its
/// forms are rows, so it is not here.
const APP_DEFERRED_SECTIONS: &[(&str, &[&str])] = &[
    (
        "imbib",
        &["sharedWithMe", "scixLibraries", "tags", "reviewQueue"],
    ),
    ("imprint", &["tags"]),
    ("implore", &["tags"]),
    ("impel", &["tags"]),
    ("impart", &["tags"]),
    (
        "impress",
        &["sharedWithMe", "scixLibraries", "tags", "reviewQueue"],
    ),
];

/// Every section `app_id`'s outline shows: its named section queries, then
/// the [`MATERIALIZE_FIRST`] sections it shows. Empty for an unknown app.
///
/// Order is not a claim here — the sidebar's order is the user's (persisted
/// section order), so the host orders and this table filters.
pub fn outline_sections(app_id: &str) -> Vec<OutlineSection> {
    let app_id = app_id.trim();
    let named = named_queries(app_id);
    let mut out: Vec<OutlineSection> = named
        .keys()
        .filter(|k| !matches!(k.as_str(), "navigator" | "list" | "detail"))
        .map(|k| OutlineSection {
            section: k.clone(),
            legacy: false,
        })
        .collect();
    if let Some((_, deferred)) = APP_DEFERRED_SECTIONS.iter().find(|(app, _)| *app == app_id) {
        for section in *deferred {
            out.push(OutlineSection {
                section: (*section).to_string(),
                legacy: true,
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use impress_layout::{ChannelId, ParamBinding, ParamSource};

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn list_spec(query: PaneQuery) -> PaneSpec {
        PaneSpec::new(query, ViewKindId::LIST)
            .with_role(Role::LIST)
            .with_param(ParamBinding::optional(
                "library",
                "library",
                ParamSource::Channel {
                    channel: ChannelId::ONE,
                },
            ))
    }

    fn detail_spec(kind: &str) -> PaneSpec {
        let list = PaneQuery {
            kinds: vec![kind.into()],
            ..PaneQuery::default()
        };
        PaneSpec::new(detail_query(&list), ViewKindId::INFO)
            .with_role(Role::DETAIL)
            .with_param(ParamBinding::optional(
                DETAIL_PARAM,
                kind,
                ParamSource::Channel {
                    channel: ChannelId::ONE,
                },
            ))
    }

    fn impress_panes() -> OutlinePanes {
        let named = named_queries("impress");
        OutlinePanes {
            list: Some(list_spec(named["list"].clone())),
            detail: Some(detail_spec("publication")),
        }
    }

    #[test]
    fn a_collection_row_is_its_members_and_retargets_the_list() {
        let node = OutlineNode::Collection { id: id(7) };
        let target = outline_target("impress", &node, &BTreeMap::new());
        let OutlineTarget::Query { query } = &target else {
            panic!("a collection is a value in the algebra: {target:?}");
        };
        assert_eq!(query.kinds, vec!["publication".to_string()]);
        assert_eq!(
            query.scope,
            Scope::Collection {
                id: ItemRef::Id { id: id(7) }
            }
        );

        let verbs = outline_verbs(&node, &target, &impress_panes());
        assert_eq!(
            verbs.len(),
            2,
            "select on the channel, then set-query: {verbs:?}"
        );
        assert!(matches!(
            &verbs[0],
            Verb::Select { target: PaneRef::Role { role }, kind, ids }
                if *role == Role::NAVIGATOR && kind == "collection" && ids == &vec![id(7)]
        ));
        assert!(matches!(
            &verbs[1],
            Verb::SetQuery { target: PaneRef::Role { role }, query: q } if *role == Role::LIST && q == query
        ));
    }

    #[test]
    fn a_library_row_is_its_papers_and_publishes_the_library() {
        let node = OutlineNode::Library { id: id(3) };
        let target = outline_target("imbib", &node, &BTreeMap::new());
        let OutlineTarget::Query { query } = &target else {
            panic!("{target:?}")
        };
        assert_eq!(
            query.scope,
            Scope::Parent {
                id: ItemRef::Id { id: id(3) }
            }
        );
        let verbs = outline_verbs(&node, &target, &impress_panes());
        assert!(matches!(&verbs[0], Verb::Select { kind, .. } if kind == "library"));
    }

    #[test]
    fn the_five_materialize_first_sections_open_a_scoped_legacy_pane() {
        for section in deferred_section_names() {
            let node = OutlineNode::Section {
                section: section.to_string(),
            };
            let target = outline_target("impress", &node, &BTreeMap::new());
            let OutlineTarget::Legacy {
                section: Some(s),
                reason,
            } = &target
            else {
                panic!("{section} must be legacy: {target:?}");
            };
            assert_eq!(s, section);
            assert!(!reason.is_empty(), "the reason is the recorded work");

            let verbs = outline_verbs(&node, &target, &impress_panes());
            assert_eq!(
                verbs.len(),
                2,
                "set-pane, then the detail cleared: {verbs:?}"
            );
            let Verb::SetPane { spec, .. } = &verbs[0] else {
                panic!("{verbs:?}")
            };
            assert!(
                matches!(
                    &verbs[1],
                    Verb::Select { target: PaneRef::Role { role }, kind, ids }
                        if *role == Role::LIST && kind == "publication" && ids.is_empty()
                ),
                "the info pane must not keep the last paper beside a hosted route: {verbs:?}"
            );
            assert_eq!(spec.view_kind, ViewKindId::LEGACY);
            assert_eq!(spec.role, Some(Role::LIST), "the role stays on the pane");
            assert_eq!(spec.view_state[LEGACY_SECTION_KEY], section);
            assert_eq!(
                spec.view_state[LEGACY_NODE_KEY]["node"], "section",
                "the node rides along so the host renders that route"
            );
        }
    }

    #[test]
    fn rows_of_the_five_sections_are_legacy_too() {
        let cases = [
            (OutlineNode::ScixLibrary { id: id(1) }, "scixLibraries"),
            (OutlineNode::SharedLibrary { id: id(1) }, "sharedWithMe"),
            (
                OutlineNode::SearchForm {
                    form: "adsModern".into(),
                },
                "search",
            ),
            (
                OutlineNode::FeedForm {
                    feed: None,
                    library: None,
                },
                "search",
            ),
            (
                OutlineNode::FeedForm {
                    feed: Some(Uuid::nil()),
                    library: None,
                },
                "search",
            ),
        ];
        for (node, section) in cases {
            let target = outline_target("impress", &node, &BTreeMap::new());
            assert!(
                matches!(&target, OutlineTarget::Legacy { section: Some(s), .. } if s == section),
                "{node:?} → {target:?}"
            );
        }
    }

    #[test]
    fn a_tag_row_is_a_query_even_though_the_tag_outline_is_not() {
        // MATERIALIZE_FIRST says it: the tag OUTLINE is not a query, a
        // selected tag's contents are.
        let node = OutlineNode::Record {
            kind: "publication".into(),
            scope: RecordScope::Tag {
                path: "methods/nbody".into(),
            },
        };
        let target = outline_target("impress", &node, &BTreeMap::new());
        let OutlineTarget::Query { query } = target else {
            panic!()
        };
        assert_eq!(
            query.filters,
            vec![Filter::Tag {
                path: "methods/nbody".into()
            }]
        );
    }

    #[test]
    fn the_inbox_row_binds_the_inbox_library_it_was_given() {
        let node = OutlineNode::Section {
            section: "inbox".into(),
        };
        let bindings = BTreeMap::from([("library".to_string(), id(42))]);
        let OutlineTarget::Query { query } = outline_target("impress", &node, &bindings) else {
            panic!()
        };
        assert_eq!(
            query.scope,
            Scope::Parent {
                id: ItemRef::Id { id: id(42) }
            }
        );
        // Read AND unread (W5 parity with the chassis' Inbox list; the
        // badge, not the list, is unread-only).
        assert!(query.filters.is_empty(), "{:?}", query.filters);
    }

    #[test]
    fn a_section_whose_parameter_nobody_bound_is_hosted_not_emptied() {
        // "Cited in Manuscripts" means cited by ANY manuscript; its query is
        // cited by `$manuscript`. Unbound, it would be an empty list under a
        // row with papers in it.
        let node = OutlineNode::Section {
            section: "citedInManuscripts".into(),
        };
        let target = outline_target("impress", &node, &BTreeMap::new());
        assert!(
            matches!(&target, OutlineTarget::Legacy { section: Some(s), reason }
                if s == "citedInManuscripts" && reason.contains("$manuscript")),
            "{target:?}"
        );
    }

    #[test]
    fn task_states_are_legacy_because_the_status_filter_would_lie() {
        let node = OutlineNode::Record {
            kind: "task".into(),
            scope: RecordScope::Status {
                status: "running".into(),
            },
        };
        assert!(matches!(
            outline_target("impel", &node, &BTreeMap::new()),
            OutlineTarget::Legacy { section: Some(s), .. } if s == "agents"
        ));
        // A manuscript's status IS `payload.status`.
        let node = OutlineNode::Record {
            kind: "manuscript".into(),
            scope: RecordScope::Status {
                status: "submitted".into(),
            },
        };
        assert!(matches!(
            outline_target("imprint", &node, &BTreeMap::new()),
            OutlineTarget::Query { .. }
        ));
    }

    #[test]
    fn folder_scopes_follow_the_membership_table() {
        let folder = |kind: &str| {
            let node = OutlineNode::Record {
                kind: kind.into(),
                scope: RecordScope::Folder { id: id(9) },
            };
            match outline_target("impress", &node, &BTreeMap::new()) {
                OutlineTarget::Query { query } => query.scope,
                other => panic!("{other:?}"),
            }
        };
        let r = ItemRef::Id { id: id(9) };
        assert_eq!(folder("figure"), Scope::Parent { id: r.clone() });
        assert_eq!(folder("message"), Scope::Parent { id: r.clone() });
        assert_eq!(
            folder("manuscript"),
            Scope::CollectionSubtree { id: r.clone() }
        );
        assert_eq!(folder("publication"), Scope::Collection { id: r });
    }

    #[test]
    fn a_kind_change_repoints_the_detail_pane_so_the_channel_reaches_it() {
        let node = OutlineNode::Record {
            kind: "figure".into(),
            scope: RecordScope::All,
        };
        let target = outline_target("impress", &node, &BTreeMap::new());
        let verbs = outline_verbs(&node, &target, &impress_panes());
        let detail = verbs
            .iter()
            .find_map(|v| match v {
                Verb::SetPane {
                    target: PaneRef::Role { role },
                    spec,
                } if *role == Role::DETAIL => Some(spec),
                _ => None,
            })
            .expect("a publication detail pane never hears a figure selection");
        assert_eq!(detail.query.kinds, vec!["figure".to_string()]);
        assert_eq!(detail.param(DETAIL_PARAM).unwrap().decl.kind, "figure");
        assert_eq!(detail.view_kind, ViewKindId::INFO, "the view kind is kept");

        // Same kind: the detail pane is left alone.
        let node = OutlineNode::Collection { id: id(1) };
        let target = outline_target("impress", &node, &BTreeMap::new());
        let verbs = outline_verbs(&node, &target, &impress_panes());
        assert!(!verbs
            .iter()
            .any(|v| matches!(v, Verb::SetPane { target: PaneRef::Role { role }, .. } if *role == Role::DETAIL)));
    }

    #[test]
    fn leaving_a_legacy_route_restores_a_list_and_reselecting_is_a_no_op() {
        let legacy_node = OutlineNode::Section {
            section: "reviewQueue".into(),
        };
        let legacy_target = outline_target("impress", &legacy_node, &BTreeMap::new());
        let mut panes = impress_panes();
        let Some(spec) = outline_verbs(&legacy_node, &legacy_target, &panes)
            .into_iter()
            .find_map(|verb| match verb {
                Verb::SetPane { spec, .. } => Some(spec),
                _ => None,
            })
        else {
            panic!("a legacy row sets the list pane")
        };
        panes.list = Some(spec);

        // The same legacy row again: nothing to do.
        assert!(outline_verbs(&legacy_node, &legacy_target, &panes).is_empty());

        // A query row from a legacy pane: set-pane back to a list.
        let node = OutlineNode::Collection { id: id(5) };
        let target = outline_target("impress", &node, &BTreeMap::new());
        let verbs = outline_verbs(&node, &target, &panes);
        let Some(Verb::SetPane { spec, .. }) = verbs.last() else {
            panic!("{verbs:?}")
        };
        assert_eq!(spec.view_kind, ViewKindId::LIST);
        assert!(spec.view_state.is_null());
        assert_eq!(
            spec.params,
            panes.list.as_ref().unwrap().params,
            "params survive"
        );

        // And the list already on that query: only the channel select.
        panes.list = Some(spec.clone());
        let verbs = outline_verbs(&node, &target, &panes);
        assert_eq!(verbs.len(), 1);
        assert!(matches!(verbs[0], Verb::Select { .. }));
    }

    #[test]
    fn no_list_pane_means_no_verbs() {
        let node = OutlineNode::Collection { id: id(1) };
        let target = outline_target("impress", &node, &BTreeMap::new());
        assert!(outline_verbs(&node, &target, &OutlinePanes::default()).is_empty());
    }

    #[test]
    fn the_launch_selection_only_applies_to_an_untouched_list() {
        let panes = impress_panes();
        assert!(initial_selection_applies("impress", &panes));
        let mut moved = panes.clone();
        moved.list.as_mut().unwrap().query = publications(Scope::Collection {
            id: ItemRef::Id { id: id(1) },
        });
        assert!(!initial_selection_applies("impress", &moved));
    }

    #[test]
    fn every_verb_round_trips_its_wire_form() {
        let node = OutlineNode::Section {
            section: "tags".into(),
        };
        let target = outline_target("impress", &node, &BTreeMap::new());
        for verb in outline_verbs(&node, &target, &impress_panes()) {
            let json = serde_json::to_string(&verb).unwrap();
            let back: Verb = serde_json::from_str(&json).unwrap();
            assert_eq!(back, verb);
        }
        // And the node's own wire form, which Swift writes.
        let json = r#"{"node":"record","kind":"figure","scope":{"scope":"folder","id":"00000000-0000-0000-0000-000000000009"}}"#;
        let parsed: OutlineNode = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed,
            OutlineNode::Record {
                kind: "figure".into(),
                scope: RecordScope::Folder { id: id(9) }
            }
        );
    }

    /// Review PH-M1: Edit Feed… and a library's Add Feed… carry the feed and
    /// the library, through the legacy pane's `view_state` and back, and the
    /// bare form is still the generic one.
    #[test]
    fn a_feed_form_carries_its_feed_or_library_through_the_legacy_pane() {
        for (json, feed, library) in [
            (r#"{"node":"feed-form"}"#.to_string(), None, None),
            (
                format!(r#"{{"node":"feed-form","feed":"{}"}}"#, id(4)),
                Some(id(4)),
                None,
            ),
            (
                format!(r#"{{"node":"feed-form","library":"{}"}}"#, id(5)),
                None,
                Some(id(5)),
            ),
        ] {
            let node: OutlineNode = serde_json::from_str(&json).unwrap();
            assert_eq!(node, OutlineNode::FeedForm { feed, library });
            assert_eq!(serde_json::to_string(&node).unwrap().replace(' ', ""), json);
            let target = outline_target("imbib", &node, &BTreeMap::new());
            let verbs = outline_verbs(&node, &target, &impress_panes());
            let spec = verbs
                .iter()
                .find_map(|verb| match verb {
                    Verb::SetPane { spec, .. } => Some(spec),
                    _ => None,
                })
                .expect("a legacy pane");
            let back: OutlineNode =
                serde_json::from_value(spec.view_state[LEGACY_NODE_KEY].clone()).unwrap();
            assert_eq!(back, node, "the pane gets the node, ids and all");
        }
    }

    #[test]
    fn every_app_section_is_a_query_or_one_of_the_five() {
        let deferred = deferred_section_names();
        for app in ["imbib", "imprint", "implore", "impel", "impart", "impress"] {
            let sections = outline_sections(app);
            assert!(!sections.is_empty(), "{app} has sections");
            for s in sections {
                let node = OutlineNode::Section {
                    section: s.section.clone(),
                };
                let target = outline_target(app, &node, &BTreeMap::new());
                if s.legacy {
                    assert!(
                        deferred.contains(&s.section.as_str()),
                        "{app}/{}",
                        s.section
                    );
                    assert!(matches!(target, OutlineTarget::Legacy { .. }));
                } else {
                    assert!(
                        !matches!(target, OutlineTarget::Inert { .. }),
                        "{app}/{} names a query the table cannot find",
                        s.section
                    );
                }
            }
        }
        assert!(outline_sections("nope").is_empty());
    }

    // ---------------------------------------------------------------- W5

    #[test]
    fn a_hosted_route_empties_the_detail_pane_of_whatever_kind_it_reads() {
        let node = OutlineNode::Section {
            section: "reviewQueue".into(),
        };
        let target = outline_target("impress", &node, &BTreeMap::new());
        let mut panes = impress_panes();
        panes.detail = Some(detail_spec("figure"));
        let verbs = outline_verbs(&node, &target, &panes);
        assert!(
            verbs.iter().any(|v| matches!(
                v,
                Verb::Select { kind, ids, .. } if kind == "figure" && ids.is_empty()
            )),
            "the clear is of the detail pane's own kind: {verbs:?}"
        );

        // No detail pane, nothing to clear — and still the set-pane.
        panes.detail = None;
        let verbs = outline_verbs(&node, &target, &panes);
        assert_eq!(verbs.len(), 1);
        assert!(matches!(verbs[0], Verb::SetPane { .. }));
    }

    #[test]
    fn a_deleted_selection_selects_nothing_as_the_chassis_does() {
        let panes = impress_panes();
        for (node, kind) in [
            (OutlineNode::Library { id: id(4) }, "library"),
            (OutlineNode::Collection { id: id(5) }, "collection"),
        ] {
            let verbs = outline_cleared_verbs(&node, &panes);
            assert_eq!(verbs.len(), 2, "{verbs:?}");
            assert!(matches!(
                &verbs[0],
                Verb::Select { target: PaneRef::Role { role }, kind: k, ids }
                    if *role == Role::NAVIGATOR && k == kind && ids.is_empty()
            ));
            assert!(matches!(
                &verbs[1],
                Verb::Select { target: PaneRef::Role { role }, kind: k, ids }
                    if *role == Role::LIST && k == "publication" && ids.is_empty()
            ));
            assert!(
                !verbs
                    .iter()
                    .any(|v| matches!(v, Verb::SetQuery { .. } | Verb::SetPane { .. })),
                "no fallback to a parent or a section: the chassis selects none"
            );
        }

        // A record folder publishes nothing on the navigator's channel; only
        // the detail is emptied.
        let folder = OutlineNode::Record {
            kind: "figure".into(),
            scope: RecordScope::Folder { id: id(6) },
        };
        assert_eq!(outline_cleared_verbs(&folder, &panes).len(), 1);

        assert!(outline_cleared_verbs(
            &OutlineNode::Library { id: id(4) },
            &OutlinePanes::default()
        )
        .is_empty());
    }

    #[test]
    fn a_list_still_on_revision_1_of_the_inbox_is_the_presets_list() {
        let mut panes = impress_panes();
        panes.list.as_mut().unwrap().query = q::inbox_revision_1();
        assert!(
            initial_selection_applies("impress", &panes),
            "a layout stored before W5 still holds the unread Inbox; the launch \
             selection must retarget it to the current one"
        );

        // The outline's own pre-W5 Inbox click: the same query, library bound.
        panes.list.as_mut().unwrap().query = PaneQuery {
            scope: Scope::Parent {
                id: ItemRef::Id { id: id(9) },
            },
            ..q::inbox_revision_1()
        };
        assert!(initial_selection_applies("impress", &panes));
        let node = OutlineNode::Section {
            section: "inbox".into(),
        };
        let bindings = BTreeMap::from([("library".to_string(), id(9))]);
        let target = outline_target("impress", &node, &bindings);
        let verbs = outline_verbs(&node, &target, &panes);
        assert!(
            verbs
                .iter()
                .any(|v| matches!(v, Verb::SetQuery { query, .. } if query.filters.is_empty())),
            "the launch selection rewrites it to read-and-unread: {verbs:?}"
        );
    }
}
