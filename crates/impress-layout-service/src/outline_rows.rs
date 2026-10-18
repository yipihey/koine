//! The outline sidebar's rows, as a function of the store (plan L8 leaf 4b,
//! SB-D1).
//!
//! [`outline_rows`] answers the sidebar as a flat list a `tree` nests: each
//! row has an `id` (its [`node_key`], SB-D2), a `parent` key, a label, a
//! count and the [`OutlineNode`] it selects. It mirrors what the legacy
//! Swift builders (`ImbibSidebarViewModel`) draw, but it is read from the
//! store the layout service already holds, so an agent and a surface see the
//! same rows the human does.
//!
//! # A row's count is the count of the query the row selects
//!
//! For every row whose [`outline_target`] is a query, `count` is
//! `store.count` of that query compiled through the manifest, so the badge is
//! the list's row count by construction. A legacy or inert row has no count,
//! except an artifact-type row, which is counted by its schema ref (the
//! algebra filters by kind, never by ref — see [`outline_target`]). A count
//! that fails leaves the row in place with no count and a `layout` warning.
//!
//! # Where the facts come from
//!
//! * Section names and display names: `SidebarSectionType`
//!   (`apps/imbib/PublicationManagerCore/.../Files/SidebarSectionOrderStore.swift`),
//!   copied into [`SECTIONS`], which is also the order the sections appear in.
//! * Search forms: `SearchFormType`
//!   (`.../Files/SearchFormStore.swift`), copied into [`SEARCH_FORMS`].
//! * Artifact type names: `ArtifactType.pluralDisplayName`
//!   (`.../Domain/ResearchArtifact.swift`), copied into [`ARTIFACT_PLURALS`].
//! * Library, collection and smart-search payloads: imbib-core's
//!   `unified/schemas.rs` and `shaped_queries.rs`. A collection's tree parent
//!   is the payload `parent_id`; its envelope `parent` is the OWNING library.
//!   A smart search's library is likewise its envelope `parent`.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use impress_item::item::Item;
use impress_item::query::ItemQuery;
use impress_item::schema::{refs, SchemaRef};
use impress_item::store::AttributedStore;
use impress_item::Value;
use impress_pane_query::{Bindings, KindManifest, PaneQuery};
use impress_pane_query_compile::{compile_with, SubtreeResolver};
use impress_service_core::Refusal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::outline::{
    node_key, outline_sections, outline_target, OutlineNode, OutlineTarget, RecordScope,
};
use crate::presets::section_bindings;

/// One row of the outline sidebar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OutlineRow {
    /// The row's [`node_key`].
    pub id: String,
    /// The parent row's key; `None` for a section row.
    pub parent: Option<String>,
    pub label: String,
    /// The count of the query the row selects (artifact types: of the ref).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
    /// What selecting the row means.
    pub node: OutlineNode,
    /// The row's [`outline_target`] is `Legacy` or `Inert`: selecting it hosts
    /// a legacy route (or navigates nowhere) instead of retargeting the list.
    pub legacy: bool,
    /// One word per row shape, for a renderer to style by: `section`,
    /// `library`, `collection`, `smart-search`, `flag`, `artifact`,
    /// `search-form`, `recent`.
    pub kind: String,
}

/// Every `SidebarSectionType` case name with its display name, in the order
/// the sidebar draws the sections by default (the Swift enum's case order).
///
/// Source: `SidebarSectionType` and its `displayName`.
const SECTIONS: &[(&str, &str)] = &[
    ("inbox", "Inbox"),
    ("libraries", "Libraries"),
    ("sharedWithMe", "Shared With Me"),
    ("scixLibraries", "SciX Libraries"),
    ("search", "Search"),
    ("exploration", "Exploration"),
    ("flagged", "Flagged"),
    ("tags", "Tags"),
    ("citedInManuscripts", "Cited in Manuscripts"),
    ("artifacts", "Artifacts"),
    ("manuscripts", "Manuscripts"),
    ("figures", "Figures"),
    ("mail", "Mail"),
    ("agents", "Agents"),
    ("reviewQueue", "Review Queue"),
    ("dismissed", "Dismissed"),
];

/// `SearchFormType`'s raw value and display name, in case order.
///
/// Source: `SearchFormType` in `SearchFormStore.swift`.
const SEARCH_FORMS: &[(&str, &str)] = &[
    ("nl-search", "Smart Search (AI)"),
    ("ads-modern", "SciX Search"),
    ("ads-classic", "ADS Classic"),
    ("ads-paper", "SciX Paper"),
    ("ads-vague-memory", "Vague Memory Search"),
    ("arxiv-advanced", "arXiv Advanced"),
    ("arxiv-feed", "arXiv Feed"),
    ("arxiv-group-feed", "Group arXiv Feed"),
    ("openalex", "OpenAlex"),
];

/// `ArtifactType.pluralDisplayName` by the last segment of its schema ref.
///
/// Source: `ArtifactType` in `ResearchArtifact.swift`.
const ARTIFACT_PLURALS: &[(&str, &str)] = &[
    ("presentation", "Presentations"),
    ("poster", "Posters"),
    ("dataset", "Datasets"),
    ("webpage", "Web Pages"),
    ("note", "Notes"),
    ("media", "Media"),
    ("code", "Code"),
    ("general", "General"),
];

/// The flag colours after "Any Flag", with their labels.
const FLAG_COLORS: &[(&str, &str)] = &[
    ("red", "Red"),
    ("amber", "Amber"),
    ("blue", "Blue"),
    ("gray", "Gray"),
];

/// The binding the host passes for the exploration library.
pub const EXPLORATION_LIBRARY_BINDING: &str = "exploration_library";
/// The binding the host passes for the Dismissed library.
pub const DISMISSED_LIBRARY_BINDING: &str = "dismissed_library";

/// The section nodes `app_id`'s outline shows, in order: the named sections
/// in the sidebar's default order, then the deferred ones.
fn ordered_sections(app_id: &str) -> Vec<String> {
    let position = |name: &str| {
        SECTIONS
            .iter()
            .position(|(case, _)| *case == name)
            .unwrap_or(SECTIONS.len())
    };
    let (mut deferred, mut named): (Vec<_>, Vec<_>) = outline_sections(app_id)
        .into_iter()
        .partition(|section| section.legacy);
    named.sort_by_key(|section| position(&section.section));
    deferred.sort_by_key(|section| position(&section.section));
    named
        .into_iter()
        .chain(deferred)
        .map(|section| section.section)
        .collect()
}

/// The section rows' keys in order — what seeds a tree's expanded state.
///
/// Leaves `section:dismissed` out: that row has no children to expand and
/// [`outline_rows`] only draws it when something is dismissed, so every key
/// returned here is a row [`outline_rows`] answers.
pub fn section_keys(app_id: &str) -> Vec<String> {
    ordered_sections(app_id)
        .into_iter()
        .filter(|section| section != "dismissed")
        .map(|section| node_key(&OutlineNode::Section { section }))
        .collect()
}

// ---------------------------------------------------------------------------
// Reading the store
// ---------------------------------------------------------------------------

fn text(item: &Item, key: &str) -> Option<String> {
    match item.payload.get(key) {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn flag(item: &Item, key: &str) -> bool {
    matches!(item.payload.get(key), Some(Value::Bool(true)))
}

fn whole(item: &Item, key: &str) -> i64 {
    match item.payload.get(key) {
        Some(Value::Int(n)) => *n,
        Some(Value::Float(f)) => *f as i64,
        _ => 0,
    }
}

fn name_of(item: &Item) -> String {
    text(item, "name").unwrap_or_default()
}

/// Order by `sort_order`, then name (case-insensitive), then id.
fn by_order(a: (i64, &str, Uuid), b: (i64, &str, Uuid)) -> Ordering {
    a.0.cmp(&b.0)
        .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
        .then_with(|| a.2.cmp(&b.2))
}

struct Library {
    id: Uuid,
    name: String,
    is_inbox: bool,
}

struct Collection {
    id: Uuid,
    name: String,
    library: Uuid,
    /// The tree parent (payload `parent_id`).
    parent: Option<Uuid>,
    sort_order: i64,
    is_smart: bool,
}

struct Feed {
    id: Uuid,
    name: String,
    library: Option<Uuid>,
    feeds_to_inbox: bool,
    auto_refresh: bool,
    sort_order: i64,
}

/// What the rows are made of, read once.
struct Data {
    libraries: Vec<Library>,
    collections: Vec<Collection>,
    feeds: Vec<Feed>,
}

fn schema_query(schema: &str) -> ItemQuery {
    ItemQuery {
        schema: Some(SchemaRef::from_stored(schema.to_string())),
        include_tags: false,
        include_references: false,
        ..ItemQuery::default()
    }
}

fn rows_of(
    store: &dyn AttributedStore,
    schemas: &[String],
    what: &str,
) -> Result<Vec<Item>, Refusal> {
    let mut out = Vec::new();
    for schema in schemas {
        out.extend(
            store
                .query(&schema_query(schema))
                .map_err(|e| Refusal::store(format!("read the {what} rows ({schema}): {e}")))?,
        );
    }
    Ok(out)
}

fn read_data(store: &dyn AttributedStore, manifest: &KindManifest) -> Result<Data, Refusal> {
    let refs_of = |kind: &str| -> Vec<String> {
        manifest
            .schema_refs(kind)
            .map(<[String]>::to_vec)
            .unwrap_or_default()
    };
    let libraries = rows_of(store, &refs_of("library"), "library")?
        .iter()
        .map(|item| Library {
            id: item.id,
            name: name_of(item),
            is_inbox: flag(item, "is_inbox"),
        })
        .collect();
    // A collection belongs to the library that is its envelope parent; one
    // with no parent belongs to nothing the sidebar draws.
    let collections = rows_of(store, &refs_of("collection"), "collection")?
        .iter()
        .filter_map(|item| {
            Some(Collection {
                id: item.id,
                name: name_of(item),
                library: item.parent?,
                parent: text(item, "parent_id").and_then(|s| Uuid::parse_str(&s).ok()),
                sort_order: whole(item, "sort_order"),
                is_smart: flag(item, "is_smart"),
            })
        })
        .collect();
    let feeds = rows_of(
        store,
        &[refs::IMBIB_SMART_SEARCH.as_str().to_string()],
        "smart search",
    )?
    .iter()
    .map(|item| Feed {
        id: item.id,
        name: name_of(item),
        library: item
            .parent
            .or_else(|| text(item, "library_id").and_then(|s| Uuid::parse_str(&s).ok())),
        feeds_to_inbox: flag(item, "feeds_to_inbox"),
        auto_refresh: flag(item, "auto_refresh_enabled"),
        sort_order: whole(item, "sort_order"),
    })
    .collect();
    Ok(Data {
        libraries,
        collections,
        feeds,
    })
}

// ---------------------------------------------------------------------------
// Building the rows
// ---------------------------------------------------------------------------

/// How a row gets its count.
enum Count {
    /// Count the query the row's target is, if it is one.
    Target,
    /// This count (an artifact type, or a row that has none).
    Fixed(Option<u64>),
}

struct Builder<'a> {
    app_id: &'a str,
    bindings: &'a BTreeMap<String, Uuid>,
    /// `bindings` with `library` = the Inbox library, for the Inbox row.
    inbox_bindings: Option<BTreeMap<String, Uuid>>,
    store: &'a dyn AttributedStore,
    manifest: &'a KindManifest,
    resolver: &'a dyn SubtreeResolver,
    rows: Vec<OutlineRow>,
}

/// A child of a library, an Inbox or an Exploration row.
enum Entry<'d> {
    Feed(&'d Feed),
    Collection(&'d Collection),
}

impl Builder<'_> {
    fn target(&self, node: &OutlineNode) -> OutlineTarget {
        let bindings = match (node, &self.inbox_bindings) {
            (OutlineNode::Section { section }, Some(inbox)) if section == "inbox" => inbox,
            _ => self.bindings,
        };
        outline_target(self.app_id, node, bindings)
    }

    fn count_query(&self, key: &str, query: &PaneQuery) -> Option<u64> {
        let compiled = compile_with(query, &[], &Bindings::new(), self.manifest, self.resolver)
            .map_err(|e| tracing::warn!(target: "layout", "outline row {key}: its query does not compile, so it has no count: {e}"))
            .ok()?;
        self.store
            .count(&compiled.item_query)
            .map(|n| n as u64)
            .map_err(
                |e| tracing::warn!(target: "layout", "outline row {key}: the count failed: {e}"),
            )
            .ok()
    }

    fn count_ref(&self, key: &str, schema: &str) -> Option<u64> {
        self.store
            .count(&schema_query(schema))
            .map(|n| n as u64)
            .map_err(|e| tracing::warn!(target: "layout", "outline row {key}: the count of {schema} failed: {e}"))
            .ok()
    }

    /// Append a row; returns its key.
    fn add(
        &mut self,
        parent: Option<&str>,
        node: OutlineNode,
        label: impl Into<String>,
        kind: &str,
        count: Count,
    ) -> String {
        let id = node_key(&node);
        let target = self.target(&node);
        let legacy = !matches!(target, OutlineTarget::Query { .. });
        let count = match count {
            Count::Fixed(count) => count,
            Count::Target => match &target {
                OutlineTarget::Query { query } => self.count_query(&id, query),
                _ => None,
            },
        };
        self.rows.push(OutlineRow {
            id: id.clone(),
            parent: parent.map(str::to_string),
            label: label.into(),
            count,
            node,
            legacy,
            kind: kind.to_string(),
        });
        id
    }

    fn feed(&mut self, parent: &str, feed: &Feed, section: &str) {
        self.add(
            Some(parent),
            OutlineNode::SmartSearch {
                id: feed.id,
                section: section.to_string(),
            },
            feed.name.clone(),
            "smart-search",
            Count::Fixed(None),
        );
    }

    /// Emit `entries` under `parent`, each collection followed by its own
    /// sub-collections (`children`, by payload `parent_id`).
    fn entries(
        &mut self,
        parent: &str,
        section: &str,
        entries: &[Entry<'_>],
        children: &HashMap<Uuid, Vec<&Collection>>,
        seen: &mut HashSet<Uuid>,
    ) {
        for entry in entries {
            match entry {
                Entry::Feed(feed) => self.feed(parent, feed, section),
                Entry::Collection(collection) => {
                    if !seen.insert(collection.id) {
                        continue; // a parent_id cycle: each row once
                    }
                    let key = self.add(
                        Some(parent),
                        OutlineNode::Collection { id: collection.id },
                        collection.name.clone(),
                        "collection",
                        Count::Target,
                    );
                    if let Some(kids) = children.get(&collection.id) {
                        let kids: Vec<Entry<'_>> =
                            kids.iter().map(|c| Entry::Collection(c)).collect();
                        self.entries(&key, section, &kids, children, seen);
                    }
                }
            }
        }
    }
}

/// The root collections of one library, and every collection's children, each
/// level sorted by `sort_order` then name.
fn collection_tree<'d>(
    collections: &[&'d Collection],
) -> (Vec<&'d Collection>, HashMap<Uuid, Vec<&'d Collection>>) {
    let ids: HashSet<Uuid> = collections.iter().map(|c| c.id).collect();
    let order = |a: &&Collection, b: &&Collection| {
        by_order((a.sort_order, &a.name, a.id), (b.sort_order, &b.name, b.id))
    };
    let mut roots = Vec::new();
    let mut children: HashMap<Uuid, Vec<&Collection>> = HashMap::new();
    for collection in collections {
        match collection.parent {
            // A parent_id that names no collection of this library is a root
            // (and a collection that names itself is one too).
            Some(parent) if parent != collection.id && ids.contains(&parent) => {
                children.entry(parent).or_default().push(collection)
            }
            _ => roots.push(*collection),
        }
    }
    roots.sort_by(order);
    for kids in children.values_mut() {
        kids.sort_by(order);
    }
    (roots, children)
}

fn sorted_feeds(mut feeds: Vec<&Feed>) -> Vec<&Feed> {
    feeds.sort_by(|a, b| by_order((a.sort_order, &a.name, a.id), (b.sort_order, &b.name, b.id)));
    feeds
}

/// "Title Case" for an artifact type nobody gave a plural.
fn title_case(segment: &str) -> String {
    let mut chars = segment.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The sidebar's rows for `app_id`, read from `store`.
///
/// * `bindings` are the ids only the host knows: `dismissed_library` and
///   `exploration_library` (the Inbox library is the one whose payload says
///   `is_inbox`). They are the map `outline_target` takes.
/// * An unknown app answers no rows.
///
/// Rows come depth-first, a parent before its children, so a flat consumer
/// can append in order.
pub fn outline_rows(
    app_id: &str,
    bindings: &BTreeMap<String, Uuid>,
    store: &dyn AttributedStore,
    manifest: &KindManifest,
    resolver: &dyn SubtreeResolver,
) -> Result<Vec<OutlineRow>, Refusal> {
    let sections = ordered_sections(app_id);
    if sections.is_empty() {
        return Ok(Vec::new());
    }
    let data = read_data(store, manifest)?;

    let mut inbox: Vec<&Library> = data.libraries.iter().filter(|l| l.is_inbox).collect();
    inbox.sort_by_key(|l| (l.name.to_lowercase(), l.id));
    let inbox = inbox.first().map(|l| l.id);
    let exploration = bindings.get(EXPLORATION_LIBRARY_BINDING).copied();
    let dismissed = bindings.get(DISMISSED_LIBRARY_BINDING).copied();

    let mut builder = Builder {
        app_id: app_id.trim(),
        bindings,
        inbox_bindings: inbox.map(|id| {
            let mut with_inbox = bindings.clone();
            with_inbox.insert("library".to_string(), id);
            with_inbox
        }),
        store,
        manifest,
        resolver,
        rows: Vec::new(),
    };

    let collections_of = |library: Uuid, skip_smart: bool| -> Vec<&Collection> {
        data.collections
            .iter()
            .filter(|c| c.library == library && !(skip_smart && c.is_smart))
            .collect()
    };

    for section in &sections {
        let node = OutlineNode::Section {
            section: section.clone(),
        };
        let label = SECTIONS
            .iter()
            .find(|(case, _)| case == section)
            .map_or(section.as_str(), |(_, name)| *name)
            .to_string();

        // Dismissed shows only when something is in it, as the Swift does.
        if section == "dismissed" {
            let OutlineTarget::Query { query } = builder.target(&node) else {
                continue;
            };
            let count = builder.count_query(&node_key(&node), &query);
            if count == Some(0) {
                continue;
            }
            builder.add(None, node, label, "section", Count::Fixed(count));
            continue;
        }

        let key = builder.add(None, node, label, "section", Count::Target);
        let mut seen = HashSet::new();
        match section.as_str() {
            "inbox" => {
                builder.add(
                    Some(&key),
                    OutlineNode::Recent,
                    "Recent",
                    "recent",
                    Count::Fixed(None),
                );
                let feeds = sorted_feeds(data.feeds.iter().filter(|f| f.feeds_to_inbox).collect());
                for feed in feeds {
                    builder.feed(&key, feed, "inbox");
                }
                if let Some(inbox) = inbox {
                    let (roots, children) = collection_tree(&collections_of(inbox, true));
                    let roots: Vec<Entry<'_>> = roots.into_iter().map(Entry::Collection).collect();
                    builder.entries(&key, "inbox", &roots, &children, &mut seen);
                }
            }
            "libraries" => {
                let mut libraries: Vec<&Library> = data
                    .libraries
                    .iter()
                    .filter(|l| !l.is_inbox && Some(l.id) != exploration && Some(l.id) != dismissed)
                    .collect();
                libraries.sort_by_key(|l| (l.name.to_lowercase(), l.id));
                for library in libraries {
                    let library_key = builder.add(
                        Some(&key),
                        OutlineNode::Library { id: library.id },
                        library.name.clone(),
                        "library",
                        Count::Target,
                    );
                    let feeds = sorted_feeds(
                        data.feeds
                            .iter()
                            .filter(|f| {
                                f.auto_refresh && !f.feeds_to_inbox && f.library == Some(library.id)
                            })
                            .collect(),
                    );
                    let (roots, children) = collection_tree(&collections_of(library.id, false));
                    let entries: Vec<Entry<'_>> = feeds
                        .into_iter()
                        .map(Entry::Feed)
                        .chain(roots.into_iter().map(Entry::Collection))
                        .collect();
                    builder.entries(&library_key, "libraries", &entries, &children, &mut seen);
                }
            }
            "search" => {
                for (form, name) in SEARCH_FORMS {
                    builder.add(
                        Some(&key),
                        OutlineNode::SearchForm {
                            form: (*form).to_string(),
                        },
                        *name,
                        "search-form",
                        Count::Fixed(None),
                    );
                }
            }
            "exploration" => {
                if let Some(exploration) = exploration {
                    let (roots, children) = collection_tree(&collections_of(exploration, true));
                    // The searches and the collections interleave by
                    // sort_order then name.
                    let mut entries: Vec<(i64, &str, Uuid, Entry<'_>)> =
                        data.feeds
                            .iter()
                            .filter(|f| f.library == Some(exploration))
                            .map(|f| (f.sort_order, f.name.as_str(), f.id, Entry::Feed(f)))
                            .chain(roots.into_iter().map(|c| {
                                (c.sort_order, c.name.as_str(), c.id, Entry::Collection(c))
                            }))
                            .collect();
                    entries.sort_by(|a, b| by_order((a.0, a.1, a.2), (b.0, b.1, b.2)));
                    let entries: Vec<Entry<'_>> = entries.into_iter().map(|e| e.3).collect();
                    builder.entries(&key, "exploration", &entries, &children, &mut seen);
                }
            }
            "flagged" => {
                let kind = section_bindings(app_id)
                    .get("flagged")
                    .copied()
                    .unwrap_or("publication");
                let flagged = |color: Option<&str>, label: &str, builder: &mut Builder<'_>| {
                    builder.add(
                        Some(&key),
                        OutlineNode::Record {
                            kind: kind.to_string(),
                            scope: RecordScope::Flagged {
                                color: color.map(str::to_string),
                            },
                        },
                        label,
                        "flag",
                        Count::Target,
                    );
                };
                flagged(None, "Any Flag", &mut builder);
                for (color, label) in FLAG_COLORS {
                    flagged(Some(color), label, &mut builder);
                }
            }
            "artifacts" => {
                // "All Artifacts" is the artifact kind's own row rather than
                // the section node again: a key names one row, and selecting
                // either is the same query (`q::artifacts`).
                builder.add(
                    Some(&key),
                    OutlineNode::Record {
                        kind: "artifact".to_string(),
                        scope: RecordScope::All,
                    },
                    "All Artifacts",
                    "artifact",
                    Count::Target,
                );
                let mut types: Vec<(String, String, u64)> = Vec::new();
                for schema in manifest.schema_refs("artifact").unwrap_or_default() {
                    let node_id = format!("artifact-type:{schema}");
                    let Some(count) = builder.count_ref(&node_id, schema).filter(|n| *n > 0) else {
                        continue;
                    };
                    let segment = schema
                        .rsplit('/')
                        .next()
                        .unwrap_or(schema)
                        .split('@')
                        .next()
                        .unwrap_or(schema);
                    let label = ARTIFACT_PLURALS
                        .iter()
                        .find(|(s, _)| *s == segment)
                        .map_or_else(|| title_case(segment), |(_, plural)| (*plural).to_string());
                    types.push((schema.clone(), label, count));
                }
                types.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
                for (schema, label, count) in types {
                    builder.add(
                        Some(&key),
                        OutlineNode::ArtifactType {
                            artifact_type: schema,
                        },
                        label,
                        "artifact",
                        Count::Fixed(Some(count)),
                    );
                }
            }
            // Cited in Manuscripts, the other record-kind sections and the
            // four deferred ones are the section row alone.
            _ => {}
        }
    }
    Ok(builder.rows)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use impress_item::item::{ActorKind, FlagState, ItemId, Priority, Visibility};
    use impress_item::reference::{EdgeType, TypedReference};
    use impress_item::ItemStore;
    use impress_pane_query_compile::test_support::builtin_manifest;
    use impress_pane_query_compile::RootOnly;
    use koine_store_memory::MemoryStore;

    const PUBLICATION: &str = "imbib/bibliography-entry";
    const LIBRARY: &str = "imbib/library";
    const COLLECTION: &str = "imbib/collection";
    const SMART_SEARCH: &str = "imbib/smart-search";

    fn item(schema: &str, payload: Vec<(&str, Value)>, parent: Option<ItemId>) -> Item {
        let now = chrono::Utc::now();
        Item {
            id: Uuid::new_v4(),
            schema: SchemaRef::from_stored(schema.to_owned()),
            payload: payload
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
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
            references: vec![],
            parent,
            extra: Default::default(),
        }
    }

    fn name(n: &str) -> (&'static str, Value) {
        ("name", Value::String(n.into()))
    }

    fn put(store: &MemoryStore, item: Item) -> ItemId {
        store.insert(item).expect("insert")
    }

    fn library(store: &MemoryStore, n: &str, is_inbox: bool) -> ItemId {
        put(
            store,
            item(
                LIBRARY,
                vec![name(n), ("is_inbox", Value::Bool(is_inbox))],
                None,
            ),
        )
    }

    fn paper(store: &MemoryStore, title: &str, library: ItemId) -> ItemId {
        put(
            store,
            item(
                PUBLICATION,
                vec![("title", Value::String(title.into()))],
                Some(library),
            ),
        )
    }

    /// A collection of `library`, filing `members` the way
    /// `collection_ops` does: a `Contains` edge FROM the collection TO each
    /// member (`Scope::Collection` compiles to `ReferencedBy(Contains, c)`).
    fn collection(
        store: &MemoryStore,
        n: &str,
        library: ItemId,
        parent: Option<ItemId>,
        sort_order: i64,
        members: &[ItemId],
    ) -> ItemId {
        let mut payload = vec![name(n), ("sort_order", Value::Int(sort_order))];
        if let Some(parent) = parent {
            payload.push(("parent_id", Value::String(parent.to_string())));
        }
        let mut row = item(COLLECTION, payload, Some(library));
        row.references = members
            .iter()
            .map(|m| TypedReference {
                target: *m,
                edge_type: EdgeType::Contains,
                metadata: None,
            })
            .collect();
        put(store, row)
    }

    fn feed(store: &MemoryStore, n: &str, library: ItemId, to_inbox: bool, auto: bool) -> ItemId {
        put(
            store,
            item(
                SMART_SEARCH,
                vec![
                    name(n),
                    ("feeds_to_inbox", Value::Bool(to_inbox)),
                    ("auto_refresh_enabled", Value::Bool(auto)),
                ],
                Some(library),
            ),
        )
    }

    struct Seed {
        store: MemoryStore,
        inbox: ItemId,
        alpha: ItemId,
        bravo: ItemId,
        beta: ItemId,
        child: ItemId,
        zeta: ItemId,
        triage: ItemId,
        library_feed: ItemId,
        inbox_feed: ItemId,
    }

    fn seed() -> Seed {
        let store = MemoryStore::new();
        let inbox = library(&store, "Inbox", true);
        let alpha = library(&store, "Alpha", false);
        let bravo = library(&store, "bravo", false);

        let inbox_papers = [paper(&store, "i1", inbox), paper(&store, "i2", inbox)];
        let triage = collection(&store, "Triage", inbox, None, 0, &inbox_papers[..1]);
        // A smart collection under the Inbox is not drawn.
        let mut smart = item(
            COLLECTION,
            vec![name("Smart"), ("is_smart", Value::Bool(true))],
            Some(inbox),
        );
        smart.payload.insert("sort_order".into(), Value::Int(0));
        put(&store, smart);

        let a = [
            paper(&store, "a1", alpha),
            paper(&store, "a2", alpha),
            paper(&store, "a3", alpha),
        ];
        let child = collection(&store, "Child", alpha, None, 0, &a[2..3]);
        // Filed before its parent exists in the store: nesting is by
        // payload parent_id, not insertion order.
        let beta = collection(&store, "Beta", alpha, None, 0, &a[..2]);
        let mut child_row = store.get(child).unwrap().unwrap();
        child_row
            .payload
            .insert("parent_id".into(), Value::String(beta.to_string()));
        store.delete(child).unwrap();
        put(&store, child_row);
        let zeta = collection(&store, "Zeta", alpha, None, 1, &[]);

        let b = paper(&store, "b1", bravo);
        let mut flagged = store.get(b).unwrap().unwrap();
        flagged.flag = Some(FlagState {
            color: "red".into(),
            style: None,
            length: None,
        });
        store.delete(b).unwrap();
        put(&store, flagged);

        let library_feed = feed(&store, "Alpha feed", alpha, false, true);
        // Neither of these is a library child: one feeds the Inbox, one does
        // not refresh.
        let inbox_feed = feed(&store, "Daily arXiv", alpha, true, true);
        feed(&store, "Dormant", alpha, false, false);

        Seed {
            store,
            inbox,
            alpha,
            bravo,
            beta,
            child,
            zeta,
            triage,
            library_feed,
            inbox_feed,
        }
    }

    fn rows(app: &str, seed: &Seed, bindings: &BTreeMap<String, Uuid>) -> Vec<OutlineRow> {
        outline_rows(app, bindings, &seed.store, &builtin_manifest(), &RootOnly).expect("rows")
    }

    fn find<'a>(rows: &'a [OutlineRow], key: &str) -> &'a OutlineRow {
        rows.iter().find(|r| r.id == key).unwrap_or_else(|| {
            panic!(
                "no row {key}: {:?}",
                rows.iter().map(|r| &r.id).collect::<Vec<_>>()
            )
        })
    }

    fn children<'a>(rows: &'a [OutlineRow], key: &str) -> Vec<&'a str> {
        rows.iter()
            .filter(|r| r.parent.as_deref() == Some(key))
            .map(|r| r.id.as_str())
            .collect()
    }

    fn key(node: OutlineNode) -> String {
        node_key(&node)
    }

    fn lib_key(id: ItemId) -> String {
        key(OutlineNode::Library { id })
    }

    fn coll_key(id: ItemId) -> String {
        key(OutlineNode::Collection { id })
    }

    fn section_key(s: &str) -> String {
        key(OutlineNode::Section { section: s.into() })
    }

    /// `store.count` of the compiled `outline_target` query of a row.
    fn expected_count(app: &str, row: &OutlineRow, seed: &Seed, b: &BTreeMap<String, Uuid>) -> u64 {
        let mut b = b.clone();
        if row.id == section_key("inbox") {
            b.insert("library".into(), seed.inbox);
        }
        let OutlineTarget::Query { query } = outline_target(app, &row.node, &b) else {
            panic!("{} is not a query row", row.id);
        };
        let compiled = compile_with(
            &query,
            &[],
            &Bindings::new(),
            &builtin_manifest(),
            &RootOnly,
        )
        .expect("compile");
        seed.store.count(&compiled.item_query).expect("count") as u64
    }

    #[test]
    fn sections_come_in_order_with_the_deferred_ones_last() {
        let seed = seed();
        let rows = rows("imbib", &seed, &BTreeMap::new());
        let sections: Vec<&str> = rows
            .iter()
            .filter(|r| r.parent.is_none())
            .map(|r| r.id.as_str())
            .collect();
        let expect: Vec<String> = [
            "inbox",
            "libraries",
            "search",
            "exploration",
            "flagged",
            "citedInManuscripts",
            "artifacts",
            // dismissed: nothing bound, so no row
            "sharedWithMe",
            "scixLibraries",
            "tags",
            "reviewQueue",
        ]
        .iter()
        .map(|s| section_key(s))
        .collect();
        assert_eq!(sections, expect);
        assert!(rows
            .iter()
            .filter(|r| r.parent.is_none())
            .all(|r| r.kind == "section"));
        // The four deferred sections are legacy rows with no children.
        for s in ["sharedWithMe", "scixLibraries", "tags", "reviewQueue"] {
            let row = find(&rows, &section_key(s));
            assert!(row.legacy, "{s}");
            assert_eq!(row.count, None, "{s}");
            assert!(children(&rows, &row.id).is_empty(), "{s}");
        }
        let cited = find(&rows, &section_key("citedInManuscripts"));
        assert!(cited.legacy && children(&rows, &cited.id).is_empty());
        assert_eq!(find(&rows, &section_key("libraries")).label, "Libraries");
        assert_eq!(
            find(&rows, &section_key("citedInManuscripts")).label,
            "Cited in Manuscripts"
        );
    }

    #[test]
    fn section_keys_match_the_section_rows() {
        let seed = seed();
        let rows = rows("imbib", &seed, &BTreeMap::new());
        let in_rows: Vec<String> = rows
            .iter()
            .filter(|r| r.parent.is_none())
            .map(|r| r.id.clone())
            .collect();
        assert_eq!(section_keys("imbib"), in_rows);
        assert!(section_keys("unknown-app").is_empty());
    }

    #[test]
    fn libraries_nest_their_collections_by_parent_id_and_sort_order() {
        let seed = seed();
        let rows = rows("imbib", &seed, &BTreeMap::new());
        let libraries = section_key("libraries");

        // Sorted by name, case-insensitively; the Inbox library is not one.
        assert_eq!(
            children(&rows, &libraries),
            vec![lib_key(seed.alpha), lib_key(seed.bravo)]
        );
        let alpha = find(&rows, &lib_key(seed.alpha));
        assert_eq!(alpha.parent.as_deref(), Some(libraries.as_str()));
        assert_eq!(alpha.kind, "library");
        assert_eq!(alpha.label, "Alpha");
        assert!(!alpha.legacy);

        // The library feed first (legacy, no count), then the root
        // collections by sort_order then name; the Inbox feed and the
        // dormant one are not children.
        assert_eq!(
            children(&rows, &alpha.id),
            vec![
                key(OutlineNode::SmartSearch {
                    id: seed.library_feed,
                    section: "libraries".into()
                }),
                coll_key(seed.beta),
                coll_key(seed.zeta),
            ]
        );
        let library_feed = find(
            &rows,
            &key(OutlineNode::SmartSearch {
                id: seed.library_feed,
                section: "libraries".into(),
            }),
        );
        assert!(library_feed.legacy);
        assert_eq!(library_feed.count, None);
        assert_eq!(library_feed.kind, "smart-search");

        // Nesting.
        let child = find(&rows, &coll_key(seed.child));
        assert_eq!(child.parent.as_deref(), Some(coll_key(seed.beta).as_str()));
        assert_eq!(child.kind, "collection");
        assert_eq!(
            find(&rows, &coll_key(seed.beta)).parent.as_deref(),
            Some(alpha.id.as_str())
        );

        // Counts.
        let none = BTreeMap::new();
        assert_eq!(alpha.count, Some(3), "three papers are in Alpha");
        assert_eq!(find(&rows, &lib_key(seed.bravo)).count, Some(1));
        assert_eq!(find(&rows, &coll_key(seed.beta)).count, Some(2));
        assert_eq!(find(&rows, &coll_key(seed.child)).count, Some(1));
        assert_eq!(find(&rows, &coll_key(seed.zeta)).count, Some(0));
        for row in rows.iter().filter(|r| r.count.is_some() && !r.legacy) {
            assert_eq!(
                row.count,
                Some(expected_count("imbib", row, &seed, &none)),
                "{} should count its own query",
                row.id
            );
        }
    }

    #[test]
    fn the_inbox_row_has_recent_first_then_feeds_then_collections() {
        let seed = seed();
        let rows = rows("imbib", &seed, &BTreeMap::new());
        let inbox = section_key("inbox");
        assert_eq!(
            children(&rows, &inbox),
            vec![
                "recent".to_string(),
                key(OutlineNode::SmartSearch {
                    id: seed.inbox_feed,
                    section: "inbox".into()
                }),
                coll_key(seed.triage),
            ]
        );
        let recent = find(&rows, "recent");
        assert_eq!(recent.label, "Recent");
        assert!(recent.legacy);
        assert_eq!(recent.kind, "recent");
        // The smart collection is not drawn; the Inbox row counts the Inbox.
        assert_eq!(rows.iter().filter(|r| r.label == "Smart").count(), 0);
        assert_eq!(find(&rows, &inbox).count, Some(2));
        assert_eq!(find(&rows, &coll_key(seed.triage)).count, Some(1));
        assert_eq!(
            find(&rows, &coll_key(seed.triage)).parent.as_deref(),
            Some(inbox.as_str())
        );
    }

    #[test]
    fn flags_are_any_then_the_four_colours_with_their_counts() {
        let seed = seed();
        let rows = rows("imbib", &seed, &BTreeMap::new());
        let flagged = section_key("flagged");
        let kids = children(&rows, &flagged);
        assert_eq!(
            kids,
            vec![
                "record:publication:flagged",
                "record:publication:flagged:red",
                "record:publication:flagged:amber",
                "record:publication:flagged:blue",
                "record:publication:flagged:gray",
            ]
        );
        let counts: Vec<Option<u64>> = kids.iter().map(|k| find(&rows, k).count).collect();
        assert_eq!(counts, vec![Some(1), Some(1), Some(0), Some(0), Some(0)]);
        assert_eq!(find(&rows, kids[0]).label, "Any Flag");
        assert_eq!(find(&rows, kids[1]).label, "Red");
        assert!(rows
            .iter()
            .filter(|r| r.parent.as_deref() == Some(flagged.as_str()))
            .all(|r| r.kind == "flag" && !r.legacy));
    }

    #[test]
    fn search_forms_are_legacy_rows_under_search() {
        let seed = seed();
        let rows = rows("imbib", &seed, &BTreeMap::new());
        let search = section_key("search");
        let forms = children(&rows, &search);
        assert_eq!(forms.len(), SEARCH_FORMS.len());
        assert_eq!(forms[0], "search-form:nl-search");
        let first = find(&rows, forms[0]);
        assert_eq!(first.label, "Smart Search (AI)");
        assert!(first.legacy);
        assert_eq!(first.count, None);
    }

    #[test]
    fn artifacts_list_all_then_the_types_that_have_rows() {
        let seed = seed();
        for (schema, n) in [
            ("impress/artifact/code", 2),
            ("impress/artifact/dataset", 1),
        ] {
            for i in 0..n {
                put(
                    &seed.store,
                    item(
                        schema,
                        vec![("title", Value::String(format!("{schema} {i}")))],
                        None,
                    ),
                );
            }
        }
        let rows = rows("imbib", &seed, &BTreeMap::new());
        let artifacts = section_key("artifacts");
        assert_eq!(
            children(&rows, &artifacts),
            vec![
                "record:artifact:all",
                "artifact-type:impress/artifact/code",
                "artifact-type:impress/artifact/dataset",
            ]
        );
        assert_eq!(find(&rows, "record:artifact:all").label, "All Artifacts");
        assert_eq!(find(&rows, "record:artifact:all").count, Some(3));
        let code = find(&rows, "artifact-type:impress/artifact/code");
        assert_eq!((code.label.as_str(), code.count), ("Code", Some(2)));
        assert!(code.legacy, "selecting a type hosts a legacy route");
        let dataset = find(&rows, "artifact-type:impress/artifact/dataset");
        assert_eq!(
            (dataset.label.as_str(), dataset.count),
            ("Datasets", Some(1))
        );
    }

    #[test]
    fn exploration_and_dismissed_follow_their_bindings() {
        let seed = seed();
        let exploration = library(&seed.store, "Exploration", false);
        let dismissed = library(&seed.store, "Dismissed", false);
        paper(&seed.store, "d1", dismissed);
        let search = feed(&seed.store, "Dark matter", exploration, false, false);
        let col = collection(&seed.store, "Kept", exploration, None, 0, &[]);
        let later = collection(&seed.store, "Zzz", exploration, None, 5, &[]);
        let earlier_feed = feed(&seed.store, "Aaa", exploration, false, false);
        // Give the searches an order against the collections.
        for (id, order) in [(search, 3), (earlier_feed, 1)] {
            let mut row = seed.store.get(id).unwrap().unwrap();
            row.payload.insert("sort_order".into(), Value::Int(order));
            seed.store.delete(id).unwrap();
            seed.store.insert(row).unwrap();
        }

        let bindings = BTreeMap::from([
            ("exploration_library".to_string(), exploration),
            ("dismissed_library".to_string(), dismissed),
        ]);
        let rows = rows("imbib", &seed, &bindings);
        let sections: Vec<&str> = rows
            .iter()
            .filter(|r| r.parent.is_none())
            .map(|r| r.id.as_str())
            .collect();
        let mut with_dismissed = section_keys("imbib");
        with_dismissed.insert(
            with_dismissed.len() - 4, // before the four deferred sections
            section_key("dismissed"),
        );
        assert_eq!(
            with_dismissed
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            sections,
            "with something dismissed the row joins the others"
        );
        let dismissed_row = find(&rows, &section_key("dismissed"));
        assert_eq!(dismissed_row.count, Some(1));
        assert!(!dismissed_row.legacy);

        // Neither bound library is listed among the libraries.
        let libraries = children(&rows, &section_key("libraries"));
        assert!(!libraries.contains(&lib_key(exploration).as_str()));
        assert!(!libraries.contains(&lib_key(dismissed).as_str()));

        // Interleaved by sort_order then name: collections (0: Kept), the
        // order-1 search, the order-3 search, the order-5 collection.
        let smart = |id| {
            key(OutlineNode::SmartSearch {
                id,
                section: "exploration".into(),
            })
        };
        assert_eq!(
            children(&rows, &section_key("exploration")),
            vec![
                coll_key(col),
                smart(earlier_feed),
                smart(search),
                coll_key(later)
            ]
        );
    }

    #[test]
    fn every_id_is_unique_and_every_parent_comes_first() {
        let seed = seed();
        let rows = rows("impress", &seed, &BTreeMap::new());
        let mut seen = HashSet::new();
        for row in &rows {
            if let Some(parent) = &row.parent {
                assert!(seen.contains(parent), "{} precedes its parent", row.id);
            }
            assert!(seen.insert(row.id.clone()), "duplicate id {}", row.id);
            assert_eq!(
                parse_back(&row.id),
                row.node,
                "{} is not its node's key",
                row.id
            );
        }
        // impress shows every other app's sections too.
        assert!(seen.contains(&section_key("manuscripts")));
        assert!(seen.contains(&section_key("agents")));
    }

    fn parse_back(key: &str) -> OutlineNode {
        crate::outline::parse_node_key(key).expect("a row's id parses")
    }

    #[test]
    fn rows_serialize_without_an_absent_count() {
        let seed = seed();
        let rows = rows("imbib", &seed, &BTreeMap::new());
        let recent = serde_json::to_value(find(&rows, "recent")).unwrap();
        assert!(recent.get("count").is_none());
        assert_eq!(recent["node"]["node"], "recent");
        assert_eq!(recent["parent"], section_key("inbox"));
    }

    #[test]
    fn an_unknown_app_has_no_rows() {
        let seed = seed();
        assert!(rows("unknown-app", &seed, &BTreeMap::new()).is_empty());
    }

    #[test]
    fn a_store_with_no_library_rows_still_draws_the_sections() {
        let store = MemoryStore::new();
        let rows = outline_rows(
            "imbib",
            &BTreeMap::new(),
            &store,
            &builtin_manifest(),
            &RootOnly,
        )
        .expect("rows");
        let inbox = find(&rows, &section_key("inbox"));
        // No Inbox library, so the section's `$library` is unbound: legacy.
        assert!(inbox.legacy);
        assert_eq!(inbox.count, None);
        assert_eq!(children(&rows, &inbox.id), vec!["recent"]);
    }
}
