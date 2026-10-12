//! The shipped presets, as a data table and as store rows (ADR-0031 D10,
//! work package L7 of `docs/plan-layout-tree.md`).
//!
//! > An app preset is a set of named queries, a default tree, and a role
//! > assignment. imbib is "the publication queries with the triage layout";
//! > imprint is "the manuscript queries with the writing layout".
//!
//! That sentence is this module. [`shipped_presets`] is the table — one
//! [`ShippedPreset`] per `(app, purpose)` — and [`PresetStore`] is the half
//! that puts it in the store, because **presets are records the user can
//! edit**, not Rust constants. The table is only the *shipped* revision: it
//! seeds a row that is absent and is otherwise never written over, which is
//! what makes [`PresetStore::ensure_shipped`] safe to call on every launch
//! and what makes `reset_preset` a verb rather than the default behaviour.
//!
//! # What the table reproduces
//!
//! Today's chassis, per app, is `AppShellConfiguration.<app>`: a set of
//! visible sections, a default section, and a default detail tab. Each app's
//! **default** preset is that configuration as a tree —
//!
//! * the **navigator** pane is the outline over the navigable kinds
//!   (`collection` + `library`), which is the sidebar as a pane like any
//!   other (D1);
//! * the **list** pane is the app's `defaultSection` query;
//! * the **detail** pane is `item(id = $item)` rendered by the app's
//!   `defaultDetailTab` — the `DetailTab` raw values (`info`, `source`,
//!   `pdf`, `notes`, `bibtex`) are the view-kind spellings, so the Swift
//!   `ViewKindRegistry` resolves the tab it already has;
//!
//! shares 1 : 2 : 3, roles `navigator` / `list` / `detail`, focus on the
//! list (the pane the triage grammar acts on).
//!
//! and [`named_queries`] carries **every section** of that app's
//! `visibleSections` as a [`PaneQuery`], so the preset says what the app can
//! show, not merely what it opens on. The sections that are not values in the
//! algebra are named in [`MATERIALIZE_FIRST`], with the reason, per ADR-0031
//! D2 — a section that is missing from both lists is a bug, and
//! `every_visible_section_is_accounted_for` is the test that says so.
//!
//! # Hiding a pane
//!
//! `PaneLayoutState`'s three Booleans (`sidebarVisible`, `listPaneVisible`,
//! `detailPaneVisible`) become shares, and "hidden" is [`HIDDEN_SHARE`] —
//! **not** `0.0`, which the tree refuses and `normalize` rewrites to a full
//! column. [`impress_layout::shares`] is where that constant lives and why;
//! Triage and Reading below are the presets that use it, and a hidden pane
//! keeps its query, its role and its session, so ⌘0 / ⌥⌘0 / ⌃⌘S are a
//! `resize` back rather than a close and a re-split.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use impress_item::item::{ActorKind, Item, ItemId, Priority, Value, Visibility};
use impress_item::operation::{OperationIntent, OperationSpec, OperationType, RetentionTier};
use impress_item::query::ItemQuery;
use impress_item::reference::{EdgeType, TypedReference};
use impress_item::schema::refs::IMPRESS_UI_PRESET as PRESET_SCHEMA_REF;
use impress_item::store::{AttributedStore, GuardedWrite};
use impress_layout::preset::{detail_query, navigator_query, DETAIL_PARAM};
use impress_layout::{
    ChannelId, Container, Layout, LinearDir, PaneSpec, ParamBinding, ParamSource, Role, TileId,
    ViewKindId,
};
use impress_pane_query::{Direction, Filter, ItemRef, PaneQuery, RelationWalk, Scope, SortKey};
use std::sync::Arc;
use uuid::Uuid;

use crate::store::{author_for, LayoutRow, Result};
use impress_service_core::Refusal;

// ---------------------------------------------------------------------------
// Payload fields and constants
// ---------------------------------------------------------------------------

/// Payload field names of an `impress/ui/preset@1.0.0` row. Spelled once,
/// here, for the same reason the schema ref is: a reader that spells a field
/// differently from its writer reads `None` forever and looks exactly like
/// "nothing saved yet".
pub mod field {
    pub const NAME: &str = "name";
    pub const APP_ID: &str = "app_id";
    pub const PURPOSE: &str = "purpose";
    pub const QUERIES: &str = "queries";
    pub const LAYOUT: &str = "layout";
    pub const ROLES: &str = "roles";
    pub const VERSION: &str = "version";
}

/// Hiding a pane is one weight for the whole suite, defined in
/// [`impress_layout::shares`] and re-exported here so a preset reads in one
/// place. `impress-store-ffi`'s `resize_share` — what the role toggles
/// actually call — uses the same constant, because two thresholds would be
/// two answers to "is this pane hidden?" and the renderer only gets one.
pub use impress_layout::{is_hidden, HIDDEN_SHARE, HIDDEN_SHARE_CEILING};

/// The parameter an app's list pane binds when its scope is a library: the
/// library the navigator has selected, on channel 1.
pub const LIBRARY_PARAM: &str = "library";

/// The parameter the "cited in this manuscript" query walks from.
pub const MANUSCRIPT_PARAM: &str = "manuscript";

/// The parameter the Exploration section's collection fills. imbib's
/// `ExplorationService` materializes an exploration as a collection and the
/// pane queries that — ADR-0031 D2's stance, exactly.
pub const EXPLORATION_PARAM: &str = "exploration";

/// The parameter imbib's Dismissed section binds: the Dismissed **library**.
/// imbib dismisses a paper by MOVING it there, never by writing `status`
/// (`triage_ops`' module docs), which is why this is a `Parent` scope and
/// imprint's dismissed manuscripts are a `Status` filter.
pub const DISMISSED_LIBRARY_PARAM: &str = "dismissed_library";

/// The version every preset in the table ships at. Bumping one entry's
/// `version` is how "we shipped a newer Triage" becomes distinguishable from
/// "the user edited Triage" (`schemas/ui.rs`, `version`).
///
/// **2** (plan wave 6 W5): the `inbox` named query stopped filtering to
/// unread papers, for parity with the chassis' own Inbox list (see
/// [`q::inbox`]). [`previous_revision`] rebuilds revision 1 from the table,
/// and [`PresetStore::ensure_shipped`] upgrades a stored row only while it is
/// still exactly revision 1 — an edited row stays the user's.
const SHIPPED_VERSION: u32 = 2;

/// Sections a preset permits that have **no** `PaneQuery`, with the reason.
///
/// ADR-0031 D2: "what the algebra cannot express is materialized first".
/// Every entry is a section of some app's `visibleSections`, so the pair
/// (this list, [`named_queries`]) covers the chassis completely — which is
/// what `every_visible_section_is_accounted_for` proves.
pub const MATERIALIZE_FIRST: &[(&str, &str)] = &[
    (
        "sharedWithMe",
        "share state is neither an envelope field nor an edge the algebra walks; imbib's \
         sharing sync materializes shared papers into a library, and a pane queries that",
    ),
    (
        "scixLibraries",
        "the rows live on the ADS/SciX server behind the user's credentials, not in the \
         store; a synced SciX library becomes a `library` row and is then `libraries` above",
    ),
    (
        "search (the online forms)",
        "ADS/SciX results are remote until the form materializes them into a feed or an \
         exploration collection; the LOCAL search over what is already stored is the \
         `search` named query, which is the same section's other half",
    ),
    (
        "tags",
        "a tag path is not a record kind, and `Filter::Tag` takes a literal path — so the \
         tag OUTLINE is not a query over records, while a selected tag's contents are \
         (`Filter::Tag { path }` with that path, built when the user picks one)",
    ),
    (
        "reviewQueue",
        "its rows are `review-request@1.0.0`, which `builtin_manifest()` names no kind \
         for; binding it to a kind it does not list would be a lie a future reader trusts \
         (the same reason `AppShellConfiguration.impress` leaves it unbound)",
    ),
];

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/// One shipped preset: what an app is, as a value.
#[derive(Debug, Clone, PartialEq)]
pub struct ShippedPreset {
    /// Display name, recalled by ⌃⌘1–9 and by [`crate::LayoutService::apply_preset`].
    pub name: &'static str,
    /// The preset family: `imbib`, `imprint`, `implore`, `impel`, `impart`,
    /// `impress`.
    pub app_id: &'static str,
    /// What the preset is for, in the user's words. Uninterpreted, and not a
    /// closed vocabulary (`schemas/ui.rs`, `purpose`).
    pub purpose: &'static str,
    /// Name → query. Every section of the app's `visibleSections` that is a
    /// value in the algebra, plus the three pane queries by role.
    pub queries: BTreeMap<String, PaneQuery>,
    /// The default tree.
    pub layout: Layout,
    /// Role → the tile of `layout` that holds it.
    pub roles: BTreeMap<Role, TileId>,
    /// The shipped revision.
    pub version: u32,
}

impl ShippedPreset {
    /// The deterministic row id this preset seeds under.
    pub fn item_id(&self) -> ItemId {
        preset_id(self.app_id, self.name)
    }
}

/// Every shipped preset, in **table order** — which is ordinal order: for
/// imbib, ⌃⌘1 is Default, ⌃⌘2 Triage, ⌃⌘3 Reading, ⌃⌘4 Full.
///
/// Rebuilt on each call rather than kept in a `static`: a [`Layout`] is an
/// arena with `BTreeMap`s in it, so there is nothing to make `const`, and a
/// caller that mutates its copy must not be able to mutate the table.
pub fn shipped_presets() -> Vec<ShippedPreset> {
    vec![
        imbib_default(),
        imbib_triage(),
        imbib_reading(),
        imbib_full(),
        imprint_default(),
        imprint_writing(),
        implore_default(),
        impel_default(),
        impart_default(),
        impress_default(),
        // impress is the shell that shows everything (ADR-0022 D9), and it is
        // the only window that renders these arrangements today: imbib's own
        // window is its pre-chassis `ContentView`, so its Triage / Reading /
        // Full rendered nowhere, and imprint's Writing only in imprint. Decided
        // with Tom 2026-09-24: impress lists them after its own Default, so
        // ⌃⌘1–5 are Default, Triage, Reading, Full, Writing and a saved layout
        // starts at ⌃⌘6.
        for_impress(imbib_triage()),
        for_impress(imbib_reading()),
        for_impress(imbib_full()),
        for_impress(imprint_writing()),
    ]
}

/// A sibling app's preset, offered by impress as impress's own.
///
/// The SAME tree the sibling ships, built by the same function, so there is
/// one definition of each arrangement. What changes is whose row it is: preset
/// rows are per app family (`PresetStore::rows(app_id)`), so impress gets its
/// own row under `preset_id("impress", name)`. Editing Reading in impress
/// therefore edits impress's Reading, never imbib's, which is what a per-app
/// preset means. The named queries are impress's union, because a preset
/// carries its app's sections and impress's are every app's.
fn for_impress(preset: ShippedPreset) -> ShippedPreset {
    ShippedPreset {
        app_id: "impress",
        queries: named_queries("impress"),
        ..preset
    }
}

/// The shipped presets of one app, in table order.
pub fn shipped_presets_for(app_id: &str) -> Vec<ShippedPreset> {
    let app_id = app_id.trim();
    shipped_presets()
        .into_iter()
        .filter(|p| p.app_id == app_id)
        .collect()
}

/// The shipped preset of `app_id` named `name` (case-insensitively).
pub fn shipped_preset(app_id: &str, name: &str) -> Option<ShippedPreset> {
    let wanted = name.trim().to_lowercase();
    shipped_presets_for(app_id)
        .into_iter()
        .find(|p| p.name.to_lowercase() == wanted)
}

/// Revision 1 of a shipped preset, rebuilt from revision 2.
///
/// No longer what the upgrade reads — [`SHIPPED_FINGERPRINTS`] is — but kept
/// so a test can pin revision 1's recorded fingerprints to the value they
/// were computed from: a change to how a preset serializes then fails that
/// test instead of silently orphaning every untouched revision-1 row.
///
/// The only difference between the two is the `inbox` named query
/// ([`q::inbox`] vs [`q::inbox_revision_1`]), wherever it occurs: in the
/// query map and as a pane's query in the tree. A preset that never named the
/// Inbox is its own revision 1 with the version number changed.
pub fn previous_revision(preset: &ShippedPreset) -> ShippedPreset {
    let current = q::inbox();
    let previous = q::inbox_revision_1();
    let mut out = preset.clone();
    for query in out.queries.values_mut() {
        if *query == current {
            *query = previous.clone();
        }
    }
    for tile in out.layout.panes() {
        if let Some(spec) = out.layout.pane_mut(tile) {
            if spec.query == current {
                spec.query = previous.clone();
            }
        }
    }
    out.version = SHIPPED_VERSION - 1;
    out
}

/// Every list query `app_id`'s presets have shipped, newest first: the
/// current `list` named query, then what earlier revisions shipped in its
/// place. A live layout written before an upgrade still holds the old one,
/// and it is still the preset's list rather than a place the user chose.
pub fn shipped_list_queries(app_id: &str) -> Vec<PaneQuery> {
    let mut out = Vec::new();
    if let Some(list) = named_queries(app_id).remove("list") {
        if list == q::inbox() {
            out.push(list);
            out.push(q::inbox_revision_1());
        } else {
            out.push(list);
        }
    }
    out
}

/// Is `query` a list query an earlier shipped revision wrote, whatever id its
/// scope was bound to?
///
/// Revision 1's Inbox reached live layouts two ways: as the preset's list
/// (`$library` still a parameter) and through the outline's Inbox row, which
/// substitutes the Inbox library's id before its `set-query`. Both are the
/// suite's query, not a place the user chose, so both are superseded.
pub fn is_superseded_list_query(query: &PaneQuery) -> bool {
    let mut normalized = query.clone();
    if let Scope::Parent { id } = &mut normalized.scope {
        *id = ItemRef::Param {
            name: LIBRARY_PARAM.into(),
        };
    }
    normalized == q::inbox_revision_1()
}

/// The fingerprint of a preset's stored form — tree, named queries, roles —
/// as the hex of a 64-bit FNV-1a over its canonical JSON (every map in it is
/// a `BTreeMap`, so the JSON is canonical). FNV rather than `std`'s hasher,
/// whose output may change between Rust releases: these are written down in
/// [`SHIPPED_FINGERPRINTS`] and must mean the same thing next year.
pub fn fingerprint(
    layout: &Layout,
    queries: &BTreeMap<String, PaneQuery>,
    roles: &BTreeMap<String, u64>,
) -> String {
    let canonical = serde_json::json!({
        "layout": layout,
        "queries": queries,
        "roles": roles,
    })
    .to_string();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in canonical.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The fingerprint of a shipped preset as it is stored.
pub fn shipped_fingerprint(preset: &ShippedPreset) -> String {
    fingerprint(&preset.layout, &preset.queries, &role_ids(&preset.roles))
}

/// Every revision of every preset the suite has shipped, as
/// `(app_id, name, version, fingerprint)` — the history the upgrade reads
/// (review RL-L16). A stored row whose fingerprint is here, at an older
/// version, is untouched and is upgraded to the current revision; a row that
/// matches none of them is the user's and is left alone, with a log line
/// saying so.
///
/// `tests::every_shipped_preset_is_in_the_fingerprint_table` fails when a
/// preset changes without a new line here (and prints the line to add):
/// changing a shipped preset means bumping its `version` and appending, never
/// editing an old entry.
pub const SHIPPED_FINGERPRINTS: &[(&str, &str, u32, &str)] = &[
    ("imbib", "Default", 1, "1d4d6bf842f5b68a"),
    ("imbib", "Default", 2, "2e76ec1b9af8902b"),
    ("imbib", "Full", 1, "1d4d6bf842f5b68a"),
    ("imbib", "Full", 2, "2e76ec1b9af8902b"),
    ("imbib", "Reading", 1, "ba7146ac7ea67fb8"),
    ("imbib", "Reading", 2, "d6f0c0488f0fd1b1"),
    ("imbib", "Triage", 1, "f0a65bd119c492aa"),
    ("imbib", "Triage", 2, "e860fcd8ee2e6d81"),
    ("impart", "Default", 1, "87c206ba4c821c0e"),
    ("impart", "Default", 2, "87c206ba4c821c0e"),
    ("impel", "Default", 1, "5be9ae35cd763ee2"),
    ("impel", "Default", 2, "5be9ae35cd763ee2"),
    ("implore", "Default", 1, "0f23c19d42b4a43e"),
    ("implore", "Default", 2, "0f23c19d42b4a43e"),
    ("impress", "Default", 1, "908b6a72c5f22f73"),
    ("impress", "Default", 2, "2baaa7e598b4eba8"),
    ("impress", "Full", 1, "908b6a72c5f22f73"),
    ("impress", "Full", 2, "2baaa7e598b4eba8"),
    ("impress", "Reading", 1, "d09d9d634af99d49"),
    ("impress", "Reading", 2, "1f1bbc2411a8c30e"),
    ("impress", "Triage", 1, "8ba86ed4b129d693"),
    ("impress", "Triage", 2, "3dc3f5344687cabe"),
    ("impress", "Writing", 1, "a34b6df84ee138d8"),
    ("impress", "Writing", 2, "621f537e94c7e05e"),
    ("imprint", "Default", 1, "78dd8e1de27e211b"),
    ("imprint", "Default", 2, "78dd8e1de27e211b"),
    ("imprint", "Writing", 1, "7461c5fb0fdd4f96"),
    ("imprint", "Writing", 2, "7461c5fb0fdd4f96"),
];

/// Is `print` a revision of `app_id`'s preset `name` older than `version`?
fn is_older_shipped(app_id: &str, name: &str, version: u32, print: &str) -> Option<u32> {
    SHIPPED_FINGERPRINTS
        .iter()
        .find(|(app, n, v, f)| *app == app_id && *n == name && *v < version && *f == print)
        .map(|(_, _, v, _)| *v)
}

/// Does a stored row carry exactly `shipped` — tree, queries, roles, version?
fn stored_matches(row: &PresetRow, stored: &StoredPreset, shipped: &ShippedPreset) -> bool {
    stored.layout.as_ref() == Some(&shipped.layout)
        && stored.queries == shipped.queries
        && stored.roles == role_ids(&shipped.roles)
        && row.version == Some(shipped.version)
}

/// The apps the table ships a preset for, in table order.
pub fn preset_app_ids() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for preset in shipped_presets() {
        if !out.contains(&preset.app_id) {
            out.push(preset.app_id);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The presets themselves
// ---------------------------------------------------------------------------

/// imbib: publications, landing in the Inbox on the Info tab
/// (`AppShellConfiguration.imbib`: `defaultSection: .inbox`,
/// `defaultDetailTab: .info`).
fn imbib_default() -> ShippedPreset {
    let (layout, roles) = three_columns(
        q::inbox(),
        ViewKindId::INFO,
        [1.0, 2.0, 3.0],
        vec![library_param()],
    );
    ShippedPreset {
        name: "Default",
        app_id: "imbib",
        purpose: "imbib's chassis: libraries and collections, the inbox, and the paper",
        queries: named_queries("imbib"),
        layout,
        roles,
        version: SHIPPED_VERSION,
    }
}

/// imbib Triage — `PaneLayoutStore.builtInLayouts`' first entry:
/// `detailPaneVisible = false`, everything else as it was.
fn imbib_triage() -> ShippedPreset {
    let (layout, roles) = three_columns(
        q::inbox(),
        ViewKindId::INFO,
        [1.0, 2.0, HIDDEN_SHARE],
        vec![library_param()],
    );
    ShippedPreset {
        name: "Triage",
        app_id: "imbib",
        purpose: "sorting the inbox: the detail pane is out of the way (⌘0 brings it back)",
        queries: named_queries("imbib"),
        layout,
        roles,
        version: SHIPPED_VERSION,
    }
}

/// imbib Reading — `sidebarVisible = false`, `detailTab = "pdf"`.
fn imbib_reading() -> ShippedPreset {
    let (layout, roles) = three_columns(
        q::inbox(),
        ViewKindId::PDF,
        [HIDDEN_SHARE, 2.0, 3.0],
        vec![library_param()],
    );
    ShippedPreset {
        name: "Reading",
        app_id: "imbib",
        purpose: "reading the PDF: the navigator is out of the way (⌃⌘S brings it back)",
        queries: named_queries("imbib"),
        layout,
        roles,
        version: SHIPPED_VERSION,
    }
}

/// imbib Full — all three panes, `detailTab = "info"`.
///
/// Deliberately identical to imbib's default: `PaneLayoutStore`'s "Full" is
/// `PaneLayoutState()` with the tab set explicitly, i.e. the app as it starts.
/// It is kept as its own preset because it is the layout the user reaches for
/// to undo Triage or Reading in one chord.
fn imbib_full() -> ShippedPreset {
    let (layout, roles) = three_columns(
        q::inbox(),
        ViewKindId::INFO,
        [1.0, 2.0, 3.0],
        vec![library_param()],
    );
    ShippedPreset {
        name: "Full",
        app_id: "imbib",
        purpose: "everything back: navigator, list and the paper's info",
        queries: named_queries("imbib"),
        layout,
        roles,
        version: SHIPPED_VERSION,
    }
}

/// imprint: manuscripts, landing in Manuscripts on the Source tab.
fn imprint_default() -> ShippedPreset {
    let (layout, roles) = three_columns(
        q::manuscripts(),
        ViewKindId::from("source"),
        [1.0, 2.0, 3.0],
        vec![],
    );
    ShippedPreset {
        name: "Default",
        app_id: "imprint",
        purpose: "imprint's chassis: folders, the manuscript list, and the source",
        queries: named_queries("imprint"),
        layout,
        roles,
        version: SHIPPED_VERSION,
    }
}

/// imprint Writing — the source and its compiled preview side by side.
///
/// The fourth pane is what the tree buys that `PaneLayoutState`'s three
/// Booleans could not express at all: a second view of the SAME `$item`,
/// following the same channel, with the role `preview` so ⌘0's sibling chords
/// can act on it.
fn imprint_writing() -> ShippedPreset {
    let list = q::manuscripts();
    let detail = detail_query(&list);
    let item_kind = list_kind(&list);

    let (layout, roles) = build(
        vec![
            Pane::new(Role::NAVIGATOR, navigator_query(), ViewKindId::OUTLINE, 1.0),
            Pane::new(Role::LIST, list.clone(), ViewKindId::LIST, 2.0),
            Pane::new(
                Role::DETAIL,
                detail.clone(),
                ViewKindId::from("source"),
                3.0,
            )
            .with_param(detail_param(&item_kind)),
            Pane::new(Role::PREVIEW, detail, ViewKindId::PDF, 3.0)
                .with_param(detail_param(&item_kind)),
        ],
        &Role::DETAIL,
    );

    ShippedPreset {
        name: "Writing",
        app_id: "imprint",
        purpose: "writing: the source and its preview on the same manuscript, focus in the source",
        queries: named_queries("imprint"),
        layout,
        roles,
        version: SHIPPED_VERSION,
    }
}

/// implore: figures, landing in Figures on Info.
fn implore_default() -> ShippedPreset {
    let (layout, roles) = three_columns(q::figures(), ViewKindId::INFO, [1.0, 2.0, 3.0], vec![]);
    ShippedPreset {
        name: "Default",
        app_id: "implore",
        purpose: "implore's chassis: figure folders, the figure list, and the figure",
        queries: named_queries("implore"),
        layout,
        roles,
        version: SHIPPED_VERSION,
    }
}

/// impel: tasks, landing in Agents on Info.
fn impel_default() -> ShippedPreset {
    let (layout, roles) = three_columns(q::agents(), ViewKindId::INFO, [1.0, 2.0, 3.0], vec![]);
    ShippedPreset {
        name: "Default",
        app_id: "impel",
        purpose: "impel's chassis: the agent outline, the task list, and the task",
        queries: named_queries("impel"),
        layout,
        roles,
        version: SHIPPED_VERSION,
    }
}

/// impart: messages, landing in Mail on Info.
fn impart_default() -> ShippedPreset {
    let (layout, roles) = three_columns(q::mail(), ViewKindId::INFO, [1.0, 2.0, 3.0], vec![]);
    ShippedPreset {
        name: "Default",
        app_id: "impart",
        purpose: "impart's chassis: mailboxes, the message list, and the message",
        queries: named_queries("impart"),
        layout,
        roles,
        version: SHIPPED_VERSION,
    }
}

/// impress: the shell that shows everything (ADR-0022 D9) — the same three
/// columns as imbib, over the union of every app's named queries.
///
/// Shipped even though no `impress` binary exists, for the reason
/// `AppShellConfiguration.impress` itself exists: the seams the future app
/// stands on are exercised today, and `list_presets("impress")` answering
/// with nothing would be a silent gap rather than a decision.
fn impress_default() -> ShippedPreset {
    let (layout, roles) = three_columns(
        q::inbox(),
        ViewKindId::INFO,
        [1.0, 2.0, 3.0],
        vec![library_param()],
    );
    ShippedPreset {
        name: "Default",
        app_id: "impress",
        purpose: "every kind in one window: the unified inbox, the list, and the record",
        queries: named_queries("impress"),
        layout,
        roles,
        version: SHIPPED_VERSION,
    }
}

// ---------------------------------------------------------------------------
// Named queries
// ---------------------------------------------------------------------------

/// The named queries of one app: every section of its `visibleSections` that
/// is a value in the algebra, plus `navigator` / `list` / `detail` — the
/// three pane queries of its default tree, named so a reader can find them
/// without walking the layout.
///
/// The section names are the `SidebarSectionType` cases verbatim, so the
/// Swift side needs no translation table. Sections with no query are in
/// [`MATERIALIZE_FIRST`].
pub fn named_queries(app_id: &str) -> BTreeMap<String, PaneQuery> {
    let mut out: BTreeMap<String, PaneQuery> = BTreeMap::new();
    let (sections, list): (&[Section], PaneQuery) = match app_id.trim() {
        "imbib" => (IMBIB_SECTIONS, q::inbox()),
        "imprint" => (IMPRINT_SECTIONS, q::manuscripts()),
        "implore" => (IMPLORE_SECTIONS, q::figures()),
        "impel" => (IMPEL_SECTIONS, q::agents()),
        "impart" => (IMPART_SECTIONS, q::mail()),
        "impress" => (IMPRESS_SECTIONS, q::inbox()),
        _ => return out,
    };
    for (name, build) in sections {
        out.insert((*name).to_string(), build());
    }
    out.insert("navigator".into(), navigator_query());
    out.insert("detail".into(), detail_query(&list));
    out.insert("list".into(), list);
    out
}

/// The record kind each section serves in each app's shell — the chassis'
/// `AppShellConfiguration.sectionBindings`, which the shipped Swift presets
/// READ from here (`section_bindings_json` over the FFI; plan wave 6 W5).
///
/// Preset data, like [`named_queries`]: the same section is a different kind
/// per app (Flagged is publications in imbib, manuscripts in imprint; Tags is
/// figures in implore), and a named section query must list the kind its
/// binding names — `bindings_agree_with_the_named_queries` holds the two
/// together. Keys are `SidebarSectionType` case names, values the manifest's
/// short kind ids (`RecordKindID`'s raw values).
///
/// A section with no entry is the shell default. `reviewQueue` is bound
/// nowhere, for the reason [`MATERIALIZE_FIRST`] gives. A shell that is not
/// shipped (the Litmus proofs of a new kind) passes its own bindings in Swift
/// and never reads this table.
pub fn section_bindings(app_id: &str) -> BTreeMap<&'static str, &'static str> {
    let table: &[(&str, &str)] = match app_id.trim() {
        "imbib" => &[
            ("flagged", "publication"),
            ("tags", "publication"),
            ("dismissed", "publication"),
        ],
        "imprint" => &[
            ("flagged", "manuscript"),
            ("tags", "manuscript"),
            ("dismissed", "manuscript"),
        ],
        "implore" => &[("tags", "figure")],
        "impart" => &[("tags", "message")],
        "impel" => &[("tags", "task")],
        "impress" => &[
            ("inbox", "publication"),
            ("libraries", "publication"),
            ("sharedWithMe", "publication"),
            ("scixLibraries", "publication"),
            ("search", "publication"),
            ("exploration", "publication"),
            ("flagged", "publication"),
            ("tags", "publication"),
            ("citedInManuscripts", "publication"),
            ("artifacts", "artifact"),
            ("manuscripts", "manuscript"),
            ("figures", "figure"),
            ("mail", "message"),
            ("agents", "task"),
            ("dismissed", "publication"),
        ],
        _ => &[],
    };
    table.iter().copied().collect()
}

/// One entry of an app's section table: the `SidebarSectionType` case,
/// verbatim, and the query it is.
type Section = (&'static str, fn() -> PaneQuery);

/// imbib's `visibleSections`, minus the five in [`MATERIALIZE_FIRST`].
/// `citedInManuscripts` stays: its children are PUBLICATIONS, and it is
/// imbib's half of the imprint bridge.
const IMBIB_SECTIONS: &[Section] = &[
    ("inbox", q::inbox),
    ("libraries", q::libraries),
    ("search", q::search_publications),
    ("exploration", q::exploration),
    ("flagged", q::flagged_publications),
    ("citedInManuscripts", q::cited_in_manuscripts),
    ("artifacts", q::artifacts),
    ("dismissed", q::dismissed_publications),
];

/// imprint's `visibleSections`. `flagged` and `dismissed` bind `.manuscript`
/// here — the same section, a different kind, which is the whole point of
/// `sectionBindings`.
const IMPRINT_SECTIONS: &[Section] = &[
    ("manuscripts", q::manuscripts),
    ("citedInManuscripts", q::cited_in_manuscripts),
    ("flagged", q::flagged_manuscripts),
    ("dismissed", q::dismissed_manuscripts),
];

const IMPLORE_SECTIONS: &[Section] = &[("figures", q::figures)];

const IMPEL_SECTIONS: &[Section] = &[("agents", q::agents)];

const IMPART_SECTIONS: &[Section] = &[("mail", q::mail)];

/// impress permits every section, so its map is the union. `flagged` and
/// `dismissed` bind `.publication`, exactly as the Swift preset does.
const IMPRESS_SECTIONS: &[Section] = &[
    ("inbox", q::inbox),
    ("libraries", q::libraries),
    ("search", q::search_publications),
    ("exploration", q::exploration),
    ("flagged", q::flagged_publications),
    ("citedInManuscripts", q::cited_in_manuscripts),
    ("artifacts", q::artifacts),
    ("manuscripts", q::manuscripts),
    ("figures", q::figures),
    ("mail", q::mail),
    ("agents", q::agents),
    ("dismissed", q::dismissed_publications),
];

/// The queries themselves. Shapes copied from the L0 sidebar table
/// (`impress_core::pane_query`'s `sidebar_nodes`), which is where ADR-0031
/// D2's claim — every navigable node the chassis ships is a value in this
/// algebra — is checked against the compiler.
pub mod q {
    use super::*;

    fn kinds(k: &[&str]) -> Vec<String> {
        k.iter().map(|s| (*s).to_string()).collect()
    }

    fn newest_first() -> Vec<SortKey> {
        vec![SortKey {
            field: "created".into(),
            descending: true,
        }]
    }

    /// Inbox — every publication of the selected library, read and unread,
    /// newest first.
    ///
    /// Parity with the chassis' own Inbox list, which the tree replaced as
    /// the only root in W5: `SectionContentView` shows `.inbox(id)`, which
    /// `RustStoreAdapter` answers with `queryPublications(parentId:)` — no
    /// read predicate — and the list is built with `disableUnreadFilter`, so
    /// the user cannot even narrow it to unread. Only the sidebar BADGE is
    /// unread (`countUnread`). Revision 1 of this query filtered to unread
    /// ([`inbox_revision_1`]), and the Inbox listed 57 papers where the
    /// chassis listed 68.
    pub fn inbox() -> PaneQuery {
        PaneQuery {
            kinds: kinds(&["publication"]),
            scope: Scope::Parent {
                id: ItemRef::Param {
                    name: LIBRARY_PARAM.into(),
                },
            },
            sort: newest_first(),
            ..PaneQuery::default()
        }
    }

    /// Revision 1 of [`inbox`]: the unread papers only. Kept so a stored
    /// preset row that still carries it can be recognised as untouched and
    /// upgraded ([`super::previous_revision`]), and so a live list still on
    /// it counts as "on the preset's query" at launch.
    pub fn inbox_revision_1() -> PaneQuery {
        PaneQuery {
            filters: vec![Filter::Read { read: false }],
            ..inbox()
        }
    }

    /// Libraries — the library rows themselves.
    pub fn libraries() -> PaneQuery {
        PaneQuery {
            kinds: kinds(&["library"]),
            ..PaneQuery::default()
        }
    }

    /// Search — the LOCAL search over what is already stored, at rest.
    ///
    /// The blank term is a value, not a placeholder: it adds no predicate
    /// (`a_blank_text_term_adds_no_predicate`), so the pane shows the kind
    /// until the user types and `set_query` rewrites `text`. The ADS/SciX
    /// forms are the same section's other half and are in
    /// [`MATERIALIZE_FIRST`].
    pub fn search_publications() -> PaneQuery {
        PaneQuery {
            kinds: kinds(&["publication"]),
            text: Some(String::new()),
            ..PaneQuery::default()
        }
    }

    /// Exploration — the papers of the exploration collection imbib's
    /// `ExplorationService` materialized.
    pub fn exploration() -> PaneQuery {
        PaneQuery {
            kinds: kinds(&["publication"]),
            scope: Scope::Collection {
                id: ItemRef::Param {
                    name: EXPLORATION_PARAM.into(),
                },
            },
            ..PaneQuery::default()
        }
    }

    /// Flagged, bound to publications (imbib, impress).
    pub fn flagged_publications() -> PaneQuery {
        flagged("publication")
    }

    /// Flagged, bound to manuscripts (imprint).
    pub fn flagged_manuscripts() -> PaneQuery {
        flagged("manuscript")
    }

    fn flagged(kind: &str) -> PaneQuery {
        PaneQuery {
            kinds: kinds(&[kind]),
            filters: vec![Filter::Flag { color: None }],
            sort: newest_first(),
            ..PaneQuery::default()
        }
    }

    /// Cited in Manuscripts — the papers this manuscript cites.
    pub fn cited_in_manuscripts() -> PaneQuery {
        PaneQuery {
            kinds: kinds(&["publication"]),
            relation: Some(RelationWalk {
                edge: impress_item::reference::EdgeType::Cites.into(),
                from: ItemRef::Param {
                    name: MANUSCRIPT_PARAM.into(),
                },
                direction: Direction::Outgoing,
            }),
            ..PaneQuery::default()
        }
    }

    /// Artifacts — every artifact kind.
    pub fn artifacts() -> PaneQuery {
        PaneQuery {
            kinds: kinds(&["artifact"]),
            sort: newest_first(),
            ..PaneQuery::default()
        }
    }

    /// Manuscripts — all of them, most recently touched first.
    pub fn manuscripts() -> PaneQuery {
        PaneQuery {
            kinds: kinds(&["manuscript"]),
            sort: vec![SortKey {
                field: "modified".into(),
                descending: true,
            }],
            ..PaneQuery::default()
        }
    }

    /// Figures — implore's library facet.
    pub fn figures() -> PaneQuery {
        PaneQuery {
            kinds: kinds(&["figure"]),
            sort: newest_first(),
            ..PaneQuery::default()
        }
    }

    /// Mail — every message kind (email and chat alike).
    pub fn mail() -> PaneQuery {
        PaneQuery {
            kinds: kinds(&["message"]),
            sort: newest_first(),
            ..PaneQuery::default()
        }
    }

    /// Agents — the tasks. `agent-run` has no section of its own by design:
    /// runs are a tree child of Agents and share the section's scope.
    ///
    /// No `limit`: the L0 table's `limit: 200` was exercising the compiler,
    /// not stating a product rule, and a shipped list pane pages in the
    /// renderer rather than truncating in the query.
    pub fn agents() -> PaneQuery {
        PaneQuery {
            kinds: kinds(&["task"]),
            sort: vec![SortKey {
                field: "payload.title".into(),
                descending: false,
            }],
            ..PaneQuery::default()
        }
    }

    /// Dismissed, imbib-style: the papers in the Dismissed **library**. imbib
    /// dismisses by MOVING, never by writing `status`.
    pub fn dismissed_publications() -> PaneQuery {
        PaneQuery {
            kinds: kinds(&["publication"]),
            scope: Scope::Parent {
                id: ItemRef::Param {
                    name: DISMISSED_LIBRARY_PARAM.into(),
                },
            },
            ..PaneQuery::default()
        }
    }

    /// Dismissed, imprint-style: the manuscripts whose payload `status` says
    /// so.
    pub fn dismissed_manuscripts() -> PaneQuery {
        PaneQuery {
            kinds: kinds(&["manuscript"]),
            filters: vec![Filter::Status {
                status: "dismissed".into(),
            }],
            ..PaneQuery::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Tree building
// ---------------------------------------------------------------------------

/// One pane of a preset under construction.
struct Pane {
    role: Role,
    query: PaneQuery,
    view_kind: ViewKindId,
    share: f32,
    params: Vec<ParamBinding>,
}

impl Pane {
    fn new(role: Role, query: PaneQuery, view_kind: ViewKindId, share: f32) -> Self {
        Self {
            role,
            query,
            view_kind,
            share,
            params: Vec::new(),
        }
    }

    fn with_param(mut self, binding: ParamBinding) -> Self {
        self.params.push(binding);
        self
    }

    fn with_params(mut self, bindings: Vec<ParamBinding>) -> Self {
        self.params.extend(bindings);
        self
    }
}

/// The first kind a list pane shows — what its detail pane's `$item` is.
fn list_kind(list: &PaneQuery) -> String {
    list.kinds
        .first()
        .cloned()
        .unwrap_or_else(|| "item".to_string())
}

/// The detail pane's binding: `$item`, of the list's kind, following channel
/// 1 — the whole of what "selecting in the list drives the detail pane" means.
///
/// **Optional**, like every parameter a preset binds (ADR-0031 D3): an
/// unfilled parameter renders the view kind's empty state, never an error, so
/// a detail pane before the first selection compiles to a query that matches
/// nothing. `impress_layout::preset::three_column` says the same thing about
/// the same pane, and the two must not disagree.
fn detail_param(item_kind: &str) -> ParamBinding {
    ParamBinding::optional(
        DETAIL_PARAM,
        item_kind.to_string(),
        ParamSource::Channel {
            channel: ChannelId::ONE,
        },
    )
}

/// The list pane's library binding: `$library`, following channel 1.
///
/// Optional, like `$item`: at cold start no library is selected, and an
/// unfilled parameter compiles to a query that matches nothing rather than to
/// a refusal. An empty inbox is an empty state; only a WRONG query is an
/// error (`pane_query`'s "Empty is not an error, and an error is not empty").
fn library_param() -> ParamBinding {
    ParamBinding::optional(
        LIBRARY_PARAM,
        "library",
        ParamSource::Channel {
            channel: ChannelId::ONE,
        },
    )
}

/// Today's chassis with explicit shares: navigator | list | detail.
fn three_columns(
    list: PaneQuery,
    detail_view_kind: ViewKindId,
    shares: [f32; 3],
    list_params: Vec<ParamBinding>,
) -> (Layout, BTreeMap<Role, TileId>) {
    let item_kind = list_kind(&list);
    let detail = detail_query(&list);
    build(
        vec![
            Pane::new(
                Role::NAVIGATOR,
                navigator_query(),
                ViewKindId::OUTLINE,
                shares[0],
            ),
            Pane::new(Role::LIST, list, ViewKindId::LIST, shares[1]).with_params(list_params),
            Pane::new(Role::DETAIL, detail, detail_view_kind, shares[2])
                .with_param(detail_param(&item_kind)),
        ],
        &Role::LIST,
    )
}

/// Assemble panes into one horizontal window and collect the role map.
///
/// Shares are relative weights, so `1 : 2 : 3` means what it says and the
/// renderer divides by their sum ([`Container::linear`]).
fn build(panes: Vec<Pane>, focus: &Role) -> (Layout, BTreeMap<Role, TileId>) {
    let mut layout = Layout::empty();
    let mut children: Vec<TileId> = Vec::new();
    let mut shares: Vec<f32> = Vec::new();
    let mut roles: BTreeMap<Role, TileId> = BTreeMap::new();

    for pane in panes {
        let mut spec = PaneSpec::new(pane.query, pane.view_kind)
            .with_role(pane.role.clone())
            .with_channel(ChannelId::ONE);
        for binding in pane.params {
            spec = spec.with_param(binding);
        }
        let id = layout.insert_pane(spec);
        roles.insert(pane.role, id);
        children.push(id);
        shares.push(pane.share);
    }

    let root = layout.insert_container(Container::linear(LinearDir::Horizontal, children, shares));
    let window = layout.add_window(root);
    let focused = roles.get(focus).copied();
    if let Some(w) = layout.window_mut(window) {
        w.focused = focused;
    }
    layout.normalize();
    (layout, roles)
}

// ---------------------------------------------------------------------------
// The store side
// ---------------------------------------------------------------------------

/// The UUIDv5 namespace preset ids are derived in. A constant namespace plus
/// `(app_id, name)` makes the id a **function of the preset**, which is what
/// lets [`PresetStore::ensure_shipped`] run on every launch without ever
/// making a second Triage.
fn preset_namespace() -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_URL, b"impress://ui/preset")
}

/// The row id of `(app_id, name)`. Case- and whitespace-insensitive, so
/// "Triage" and " triage " are the same preset rather than two.
pub fn preset_id(app_id: &str, name: &str) -> ItemId {
    let key = format!(
        "{}/{}",
        app_id.trim().to_lowercase(),
        name.trim().to_lowercase()
    );
    Uuid::new_v5(&preset_namespace(), key.as_bytes())
}

/// One `impress/ui/preset@1.0.0` row, without its tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetRow {
    pub id: ItemId,
    pub name: String,
    pub app_id: String,
    pub purpose: Option<String>,
    pub version: Option<u32>,
    pub created: DateTime<Utc>,
    pub modified: DateTime<Utc>,
}

/// What a preset row carries beyond its identity.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StoredPreset {
    /// Absent on a user-edited preset that overrides only the queries or the
    /// roles and inherits the tree from the shipped preset it `DerivedFrom`.
    pub layout: Option<Layout>,
    pub queries: BTreeMap<String, PaneQuery>,
    pub roles: BTreeMap<String, u64>,
}

/// Store-backed access to the preset rows.
///
/// Like [`crate::store::LayoutStore`], nothing here may run during the first
/// ~90 s of an app launch (root CLAUDE.md, ADR-0019 D6): seeding writes rows,
/// and a `.storeDidMutate` storm in the settling window is the
/// perpetual-render-loop bug. This crate cannot enforce that — it has no idea
/// when its host launched — so the rule belongs to the caller, and the L6
/// projection is where it is implemented.
#[derive(Clone)]
pub struct PresetStore {
    store: Arc<dyn AttributedStore>,
}

impl PresetStore {
    pub fn new(store: Arc<dyn AttributedStore>) -> Self {
        Self { store }
    }

    /// Seed the shipped presets of `app_id` that are **absent**, and answer
    /// with every preset row of the app in ordinal order.
    ///
    /// Idempotent by construction rather than by checking afterwards: the id
    /// is a function of `(app_id, name)`, and a row that exists is left
    /// exactly as it is — which is how a user's edit to Triage survives every
    /// later launch. Restoring the shipped revision is `reset_preset`, a verb
    /// the user asks for.
    ///
    /// Seeded rows are attributed to [`ActorKind::System`]: the suite shipped
    /// them, whoever happened to trigger the first read.
    pub fn ensure_shipped(&self, app_id: &str) -> Result<Vec<PresetRow>> {
        let existing = self.rows(app_id)?;
        for shipped in shipped_presets_for(app_id) {
            let id = shipped.item_id();
            if let Some(row) = existing.iter().find(|row| row.id == id) {
                self.upgrade_if_untouched(row, &shipped)?;
                continue;
            }
            self.insert(&shipped)?;
        }
        self.rows(app_id)
    }

    /// Rewrite a row that is still EXACTLY the previous shipped revision to
    /// the current one, and leave every other row alone.
    ///
    /// "Exactly" is the whole safeguard: a row the user edited differs from
    /// both revisions and is theirs, as [`Self::ensure_shipped`] promises. The
    /// write is attributed to [`ActorKind::System`], like the seed, and its
    /// operation's reason names the upgrade, so the store's history says why
    /// the row changed.
    fn upgrade_if_untouched(&self, row: &PresetRow, shipped: &ShippedPreset) -> Result<()> {
        if row.version == Some(shipped.version) {
            return Ok(());
        }
        let Some(item) = self.item(row.id)? else {
            return Ok(());
        };
        let stored = stored_of(&item)?;
        let Some(layout) = stored.layout.as_ref() else {
            // An inheriting preset (queries or roles only) is always a user's.
            return Ok(());
        };
        let print = fingerprint(layout, &stored.queries, &stored.roles);
        let Some(from) = is_older_shipped(shipped.app_id, shipped.name, shipped.version, &print)
        else {
            tracing::info!(
                target: "layout",
                "preset '{}' of {} left at {:?}: it matches no revision the suite shipped \
                 (fingerprint {print}), so it is the user's",
                shipped.name,
                shipped.app_id,
                row.version
            );
            return Ok(());
        };
        self.save(
            shipped.app_id,
            shipped.name,
            Some(shipped.purpose),
            &shipped.layout,
            &shipped.queries,
            &shipped.roles,
            Some(shipped.version),
            ActorKind::System,
            &format!(
                "upgrade the untouched preset '{}' from shipped revision {from} to {}",
                shipped.name, shipped.version
            ),
        )?;
        tracing::info!(
            target: "layout",
            "preset '{}' of {} upgraded from shipped revision {from} to {}",
            shipped.name,
            shipped.app_id,
            shipped.version
        );
        Ok(())
    }

    /// Every preset row of `app_id`, in **ordinal order**: the shipped
    /// presets first, in table order, then the user's own by creation.
    ///
    /// Creation rather than modification, for the reason
    /// [`crate::store::LayoutStore::list_named`] gives: re-saving a preset
    /// must not move it to another chord.
    pub fn rows(&self, app_id: &str) -> Result<Vec<PresetRow>> {
        let app_id = app_id.trim();
        let table = shipped_presets_for(app_id);
        let mut rows: Vec<PresetRow> = self
            .items(app_id)?
            .iter()
            .map(row_of)
            .collect::<Result<Vec<_>>>()?;
        let rank = |row: &PresetRow| -> usize {
            table
                .iter()
                .position(|p| p.item_id() == row.id)
                .unwrap_or(usize::MAX)
        };
        rows.sort_by(|a, b| {
            rank(a)
                .cmp(&rank(b))
                .then_with(|| a.created.cmp(&b.created))
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(rows)
    }

    /// One preset by name (case-insensitive) or by id.
    pub fn load(&self, app_id: &str, key: &str) -> Result<Option<(PresetRow, StoredPreset)>> {
        let key = key.trim();
        if let Ok(id) = key.parse::<ItemId>() {
            if let Some(item) = self.item(id)? {
                return Ok(Some((row_of(&item)?, stored_of(&item)?)));
            }
        }
        let wanted = key.to_lowercase();
        for item in self.items(app_id)? {
            let row = row_of(&item)?;
            if row.name.trim().to_lowercase() == wanted {
                return Ok(Some((row, stored_of(&item)?)));
            }
        }
        Ok(None)
    }

    /// Write a preset: insert when absent, patch field by field when present.
    ///
    /// **Durable**, with [`OperationIntent::Editorial`] — a preset is a
    /// decision about how work is arranged, which is exactly what `commit`
    /// and `save_named` mean by the word (ADR-0031 D7).
    #[allow(clippy::too_many_arguments)]
    pub fn save(
        &self,
        app_id: &str,
        name: &str,
        purpose: Option<&str>,
        layout: &Layout,
        queries: &BTreeMap<String, PaneQuery>,
        roles: &BTreeMap<Role, TileId>,
        version: Option<u32>,
        actor: ActorKind,
        intent: &str,
    ) -> Result<PresetRow> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Refusal::invalid_argument("a preset needs a name"));
        }
        let app_id = app_id.trim();
        if app_id.is_empty() {
            return Err(Refusal::invalid_argument(
                "a preset belongs to an app: `app_id` is required",
            ));
        }
        let id = preset_id(app_id, name);
        let exists = self.item(id)?.is_some();
        if !exists {
            let item = build_item(
                id, app_id, name, purpose, layout, queries, roles, version, actor,
            )?;
            self.store
                .insert(item)
                .map_err(|e| Refusal::store(format!("write preset: {e}")))?;
            return self
                .row(id)?
                .ok_or_else(|| Refusal::store("the preset vanished mid-save"));
        }

        let mut fields: Vec<(&str, Value)> = vec![
            (field::LAYOUT, layout_value(layout)?),
            (field::QUERIES, queries_value(queries)?),
            (field::ROLES, roles_value(roles)),
        ];
        if let Some(purpose) = purpose {
            fields.push((field::PURPOSE, Value::String(purpose.trim().to_string())));
        }
        if let Some(version) = version {
            fields.push((field::VERSION, Value::Int(i64::from(version))));
        }
        for (name, value) in fields {
            self.patch(id, name, value, actor, intent)?;
        }
        self.row(id)?
            .ok_or_else(|| Refusal::store("the preset vanished mid-save"))
    }

    /// Restore the shipped revision of `(app_id, name)` over a user-edited
    /// row. Refused for a name the table does not ship: there would be
    /// nothing to restore, and inventing one would be worse than saying so.
    pub fn reset(&self, app_id: &str, name: &str, actor: ActorKind) -> Result<PresetRow> {
        let Some(shipped) = shipped_preset(app_id, name) else {
            return Err(Refusal::not_found(format!(
                "'{name}' is not a preset {app_id} ships, so there is no shipped revision to \
                 reset it to. Shipped: {}",
                shipped_names(app_id)
            )));
        };
        self.save(
            shipped.app_id,
            shipped.name,
            Some(shipped.purpose),
            &shipped.layout,
            &shipped.queries,
            &shipped.roles,
            Some(shipped.version),
            actor,
            &format!(
                "reset the preset '{}' to its shipped revision",
                shipped.name
            ),
        )
    }

    /// Does this row still match the revision the table ships? `false` for a
    /// preset the user has edited, and `None` for one the table never shipped.
    pub fn matches_shipped(&self, row: &PresetRow, stored: &StoredPreset) -> Option<bool> {
        let shipped = shipped_preset(&row.app_id, &row.name)?;
        Some(stored_matches(row, stored, &shipped))
    }

    // ------------------------------------------------------------ internals

    fn row(&self, id: ItemId) -> Result<Option<PresetRow>> {
        self.item(id)?.as_ref().map(row_of).transpose()
    }

    fn item(&self, id: ItemId) -> Result<Option<Item>> {
        self.store
            .get(id)
            .map_err(|e| Refusal::store(format!("read preset {id}: {e}")))
            .map(|item| item.filter(|i| i.schema == PRESET_SCHEMA_REF))
    }

    /// Every preset row of `app_id`.
    ///
    /// Filtered in Rust rather than by a payload predicate, for the reason
    /// [`crate::store::LayoutStore`] gives: a workspace holds a handful of
    /// these rows, and matching a JSON field through `json_extract` is the
    /// kind of silently-empty comparison this repo has a lint about.
    fn items(&self, app_id: &str) -> Result<Vec<Item>> {
        let query = ItemQuery {
            schema: Some(PRESET_SCHEMA_REF),
            include_tags: false,
            include_references: false,
            ..Default::default()
        };
        let app_id = app_id.trim();
        Ok(self
            .store
            .query(&query)
            .map_err(|e| Refusal::store(format!("read presets: {e}")))?
            .into_iter()
            .filter(|item| string_field(item, field::APP_ID).as_deref() == Some(app_id))
            .collect())
    }

    fn insert(&self, shipped: &ShippedPreset) -> Result<ItemId> {
        let item = build_item(
            shipped.item_id(),
            shipped.app_id,
            shipped.name,
            Some(shipped.purpose),
            &shipped.layout,
            &shipped.queries,
            &shipped.roles,
            Some(shipped.version),
            // The suite ships these, whoever triggered the first read.
            ActorKind::System,
        )?;
        self.store
            .insert(item)
            .map_err(|e| Refusal::store(format!("seed preset '{}': {e}", shipped.name)))
    }

    fn patch(
        &self,
        id: ItemId,
        field: &str,
        value: Value,
        actor: ActorKind,
        intent: &str,
    ) -> Result<()> {
        self.store
            .apply_operation(OperationSpec {
                target_id: id,
                op_type: OperationType::SetPayload(field.to_string(), value),
                intent: OperationIntent::Editorial,
                reason: Some(intent.to_string()),
                batch_id: None,
                author: author_for(actor),
                author_kind: actor,
                retention: RetentionTier::Durable,
            })
            .map(|_| ())
            .map_err(|e| Refusal::store(format!("write preset: {e}")))
    }
}

// ---------------------------------------------------------------------------
// Ordinals and provenance
// ---------------------------------------------------------------------------

/// What ⌃⌘`n` recalls: a preset or a saved layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrdinalTarget {
    Preset(PresetRow),
    Layout(LayoutRow),
}

impl OrdinalTarget {
    pub fn label(&self) -> String {
        match self {
            OrdinalTarget::Preset(row) => row.name.clone(),
            OrdinalTarget::Layout(row) => row.name.clone().unwrap_or_else(|| row.id.to_string()),
        }
    }
}

/// The ⌃⌘1–9 union of one app, in the ONE documented order:
///
/// 1. the **shipped presets**, in [`shipped_presets`] table order — so ⌃⌘1 is
///    always the app's default, ⌃⌘2 Triage, ⌃⌘3 Reading, whatever the user
///    has saved since;
/// 2. then the user's **own presets**, oldest first;
/// 3. then the **named layouts**, oldest first.
///
/// Fixed points before moving ones, and creation order rather than
/// modification order within each group: re-saving Triage must not move it to
/// another chord, and a preset must not change chord because a layout was
/// renamed. The shipped presets are seeded on the way past, which is what
/// makes position 1 mean the same thing on a machine that has never opened
/// the app as on one that has.
pub fn ordinal_targets(
    presets: &PresetStore,
    layouts: &crate::store::LayoutStore,
    app_id: &str,
) -> Result<Vec<OrdinalTarget>> {
    let mut out: Vec<OrdinalTarget> = presets
        .ensure_shipped(app_id)?
        .into_iter()
        .map(OrdinalTarget::Preset)
        .collect();
    out.extend(
        layouts
            .list_named(app_id)?
            .into_iter()
            .map(OrdinalTarget::Layout),
    );
    Ok(out)
}

/// Record that a live layout came from a preset: one `DerivedFrom` edge from
/// the layout row to the preset row.
///
/// ADR-0019 D1 names this edge as the reason "reset to preset" is a graph
/// walk rather than a remembered string, so a layout derives from **one**
/// preset: an edge to any other preset is removed first, and the edge itself
/// is written `Durable` (it is provenance of a commit, not a gesture, and a
/// compaction that dropped it would lose the answer to "what was this?").
///
/// The store's insert is `INSERT OR IGNORE`, so re-applying the same preset
/// re-states the edge rather than duplicating it.
pub fn record_derived_from(
    store: &Arc<dyn AttributedStore>,
    layout_row: ItemId,
    revision: Option<u64>,
    preset_row: ItemId,
    actor: ActorKind,
    intent: &str,
) -> Result<Option<u64>> {
    let item = store
        .get(layout_row)
        .map_err(|e| Refusal::store(format!("read layout {layout_row}: {e}")))?
        .ok_or_else(|| format!("layout row {layout_row} is gone"))?;
    let stale: Vec<ItemId> = item
        .references
        .iter()
        .filter(|r| r.edge_type == EdgeType::DerivedFrom && r.target != preset_row)
        .map(|r| r.target)
        .collect();

    let mut ops: Vec<OperationType> = stale
        .into_iter()
        .map(|target| OperationType::RemoveReference(target, EdgeType::DerivedFrom))
        .collect();
    ops.push(OperationType::AddReference(TypedReference {
        target: preset_row,
        edge_type: EdgeType::DerivedFrom,
        metadata: None,
    }));

    // Guarded on the live session's revision when there is one, like every
    // other write to the live row (store.rs, "Revisions"): these edges land
    // on the same row the tree does, and each one moves its clock.
    let mut revision = revision;
    for op_type in ops {
        let spec = OperationSpec {
            target_id: layout_row,
            op_type,
            intent: OperationIntent::Editorial,
            reason: Some(intent.to_string()),
            batch_id: None,
            author: author_for(actor),
            author_kind: actor,
            retention: RetentionTier::Durable,
        };
        let fail = |e: impress_item::store::StoreError| {
            Refusal::store(format!("record the preset this layout came from: {e}"))
        };
        match revision {
            Some(expected) => match store
                .apply_operation_if_clock(spec, expected)
                .map_err(fail)?
            {
                GuardedWrite::Applied { clock, .. } => revision = Some(clock),
                GuardedWrite::Moved { .. } => return Err(Refusal::conflict(crate::session::STALE)),
            },
            None => {
                store.apply_operation(spec).map_err(fail)?;
            }
        }
    }
    Ok(revision)
}

/// The preset a layout row derives from, if any — the graph walk the edge
/// exists for.
pub fn derived_from(
    store: &Arc<dyn AttributedStore>,
    layout_row: ItemId,
) -> Result<Option<ItemId>> {
    let item = store
        .get(layout_row)
        .map_err(|e| Refusal::store(format!("read layout {layout_row}: {e}")))?;
    Ok(item.and_then(|item| {
        item.references
            .iter()
            .find(|r| r.edge_type == EdgeType::DerivedFrom)
            .map(|r| r.target)
    }))
}

/// Role → tile, read off a tree. What `save_preset` writes as `roles`.
pub fn roles_of(layout: &Layout) -> BTreeMap<Role, TileId> {
    let mut out: BTreeMap<Role, TileId> = BTreeMap::new();
    for tile in layout.panes() {
        if let Some(role) = layout.pane(tile).and_then(|pane| pane.role.clone()) {
            out.entry(role).or_insert(tile);
        }
    }
    out
}

fn shipped_names(app_id: &str) -> String {
    let names: Vec<&str> = shipped_presets_for(app_id).iter().map(|p| p.name).collect();
    if names.is_empty() {
        format!("{app_id} ships no presets")
    } else {
        names.join(", ")
    }
}

#[allow(clippy::too_many_arguments)]
fn build_item(
    id: ItemId,
    app_id: &str,
    name: &str,
    purpose: Option<&str>,
    layout: &Layout,
    queries: &BTreeMap<String, PaneQuery>,
    roles: &BTreeMap<Role, TileId>,
    version: Option<u32>,
    actor: ActorKind,
) -> Result<Item> {
    let mut payload: BTreeMap<String, Value> = BTreeMap::new();
    payload.insert(field::NAME.into(), Value::String(name.trim().into()));
    payload.insert(field::APP_ID.into(), Value::String(app_id.trim().into()));
    payload.insert(field::LAYOUT.into(), layout_value(layout)?);
    payload.insert(field::QUERIES.into(), queries_value(queries)?);
    payload.insert(field::ROLES.into(), roles_value(roles));
    if let Some(purpose) = purpose {
        payload.insert(field::PURPOSE.into(), Value::String(purpose.trim().into()));
    }
    if let Some(version) = version {
        payload.insert(field::VERSION.into(), Value::Int(i64::from(version)));
    }

    let now = Utc::now();
    Ok(Item {
        id,
        schema: PRESET_SCHEMA_REF,
        payload,
        created: now,
        modified: now,
        author: author_for(actor),
        author_kind: actor,
        logical_clock: 0,
        origin: None,
        canonical_id: None,
        tags: vec![],
        flag: None,
        is_read: false,
        is_starred: false,
        priority: Priority::None,
        // Private + Durable per ADR-0019 D2's table; a preset syncs and
        // carries no device.
        visibility: Visibility::Private,
        message_type: None,
        produced_by: None,
        version: None,
        batch_id: None,
        references: vec![],
        parent: None,
    })
}

fn role_ids(roles: &BTreeMap<Role, TileId>) -> BTreeMap<String, u64> {
    roles
        .iter()
        .map(|(role, tile)| (role.to_string(), tile.raw()))
        .collect()
}

fn roles_value(roles: &BTreeMap<Role, TileId>) -> Value {
    Value::Object(
        roles
            .iter()
            .map(|(role, tile)| (role.to_string(), Value::Int(tile.raw() as i64)))
            .collect(),
    )
}

fn queries_value(queries: &BTreeMap<String, PaneQuery>) -> Result<Value> {
    let json = serde_json::to_value(queries)
        .map_err(|e| Refusal::store(format!("encode preset queries: {e}")))?;
    serde_json::from_value(json).map_err(|e| Refusal::store(format!("encode preset queries: {e}")))
}

fn layout_value(layout: &Layout) -> Result<Value> {
    let json = serde_json::to_value(layout)
        .map_err(|e| Refusal::store(format!("encode preset tree: {e}")))?;
    serde_json::from_value(json).map_err(|e| Refusal::store(format!("encode preset tree: {e}")))
}

fn string_field(item: &Item, field: &str) -> Option<String> {
    match item.payload.get(field) {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn row_of(item: &Item) -> Result<PresetRow> {
    let name = string_field(item, field::NAME)
        .ok_or_else(|| Refusal::store(format!("preset row {} has no `name`", item.id)))?;
    let app_id = string_field(item, field::APP_ID)
        .ok_or_else(|| Refusal::store(format!("preset row {} has no `app_id`", item.id)))?;
    let version = match item.payload.get(field::VERSION) {
        Some(Value::Int(v)) if *v >= 0 => u32::try_from(*v).ok(),
        _ => None,
    };
    Ok(PresetRow {
        id: item.id,
        name,
        app_id,
        purpose: string_field(item, field::PURPOSE),
        version,
        created: item.created,
        modified: item.modified,
    })
}

fn stored_of(item: &Item) -> Result<StoredPreset> {
    let layout = match item.payload.get(field::LAYOUT) {
        Some(value) => Some(from_value::<Layout>(value, "tree")?),
        None => None,
    };
    let queries = match item.payload.get(field::QUERIES) {
        Some(value) => from_value::<BTreeMap<String, PaneQuery>>(value, "queries")?,
        None => BTreeMap::new(),
    };
    let roles = match item.payload.get(field::ROLES) {
        Some(Value::Object(map)) => map
            .iter()
            .filter_map(|(role, value)| match value {
                Value::Int(tile) if *tile >= 0 => Some((role.clone(), *tile as u64)),
                _ => None,
            })
            .collect(),
        _ => BTreeMap::new(),
    };
    Ok(StoredPreset {
        layout,
        queries,
        roles,
    })
}

fn from_value<T: serde::de::DeserializeOwned>(value: &Value, what: &str) -> Result<T> {
    let json = serde_json::to_value(value)
        .map_err(|e| Refusal::store(format!("read preset {what}: {e}")))?;
    serde_json::from_value(json).map_err(|e| Refusal::store(format!("read preset {what}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review RL-L16: every revision the suite ships is written down, and a
    /// changed preset without a new entry fails here with the line to add.
    #[test]
    fn every_shipped_preset_is_in_the_fingerprint_table() {
        let mut missing = Vec::new();
        for preset in shipped_presets() {
            for (version, print) in [
                (preset.version, shipped_fingerprint(&preset)),
                (
                    preset.version - 1,
                    shipped_fingerprint(&previous_revision(&preset)),
                ),
            ] {
                let line = (preset.app_id, preset.name, version, print.as_str());
                if !SHIPPED_FINGERPRINTS
                    .iter()
                    .any(|(a, n, v, f)| (*a, *n, *v, *f) == line)
                {
                    missing.push(format!(
                        "    (\"{}\", \"{}\", {}, \"{}\"),",
                        line.0, line.1, line.2, line.3
                    ));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "a shipped preset changed (or serializes differently) without a new line in \
             SHIPPED_FINGERPRINTS. Bump its version and append:\n{}",
            missing.join("\n")
        );
    }

    /// A row two revisions behind is still upgraded, and one matching no
    /// revision is left alone (review RL-L16).
    #[test]
    fn an_older_fingerprint_upgrades_and_an_unknown_one_does_not() {
        let preset = shipped_presets()
            .into_iter()
            .find(|p| p.app_id == "imbib")
            .expect("imbib ships a preset");
        let old = shipped_fingerprint(&previous_revision(&preset));
        assert_eq!(
            is_older_shipped(preset.app_id, preset.name, preset.version, &old),
            Some(preset.version - 1)
        );
        assert_eq!(
            is_older_shipped(preset.app_id, preset.name, preset.version + 5, &old),
            Some(preset.version - 1),
            "several revisions behind is still a shipped revision"
        );
        assert_eq!(
            is_older_shipped(
                preset.app_id,
                preset.name,
                preset.version,
                "0000000000000000"
            ),
            None
        );
    }
    use impress_layout::Tile;
    use impress_pane_query::{Bindings, KindManifest, ParamDecl};
    use impress_pane_query_compile::compile;
    use impress_pane_query_compile::test_support::builtin_manifest;

    fn manifest() -> KindManifest {
        builtin_manifest()
    }

    /// Every pane of every window of a preset.
    fn panes(preset: &ShippedPreset) -> Vec<&PaneSpec> {
        preset
            .layout
            .panes()
            .into_iter()
            .filter_map(|tile| preset.layout.pane(tile))
            .collect()
    }

    // ── The table itself ────────────────────────────────────────────────

    #[test]
    fn the_table_has_one_preset_per_app_and_purpose() {
        let mut seen: Vec<(String, String)> = Vec::new();
        for preset in shipped_presets() {
            let key = (preset.app_id.to_string(), preset.name.to_lowercase());
            assert!(
                !seen.contains(&key),
                "two presets claim ({}, {}) — their ids would collide",
                preset.app_id,
                preset.name
            );
            seen.push(key);
        }
    }

    #[test]
    fn every_app_ships_a_default_first() {
        for app in preset_app_ids() {
            let presets = shipped_presets_for(app);
            assert_eq!(
                presets.first().map(|p| p.name),
                Some("Default"),
                "⌃⌘1 must be {app}'s default"
            );
        }
    }

    #[test]
    fn the_ordinal_order_is_the_one_the_grammar_documents() {
        let names: Vec<&str> = shipped_presets_for("imbib")
            .iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, vec!["Default", "Triage", "Reading", "Full"]);
    }

    /// impress offers the arrangements that render nowhere else, after its
    /// own Default, in a fixed order: ⌃⌘1–5 (decided 2026-09-24).
    #[test]
    fn impress_lists_the_sibling_arrangements_after_its_default() {
        let impress = shipped_presets_for("impress");
        let names: Vec<&str> = impress.iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec!["Default", "Triage", "Reading", "Full", "Writing"]
        );
        for preset in &impress {
            assert_eq!(
                preset.app_id, "impress",
                "{} is impress's own row",
                preset.name
            );
            assert_eq!(preset.queries, named_queries("impress"), "{}", preset.name);
        }
    }

    /// Borrowed, not redefined: the tree is the sibling's, byte for byte, and
    /// the row is impress's, so an edit in one app never reaches the other.
    #[test]
    fn a_borrowed_preset_is_the_siblings_tree_under_impresss_own_row() {
        for (sibling, name) in [
            ("imbib", "Triage"),
            ("imbib", "Reading"),
            ("imbib", "Full"),
            ("imprint", "Writing"),
        ] {
            let theirs = shipped_preset(sibling, name).expect("the sibling ships it");
            let ours = shipped_preset("impress", name).expect("impress offers it");
            assert_eq!(ours.layout, theirs.layout, "{name}: same tree");
            assert_eq!(ours.roles, theirs.roles, "{name}: same roles");
            assert_ne!(ours.item_id(), theirs.item_id(), "{name}: a row of its own");
        }
        // The siblings' own families are unchanged.
        let imbib: Vec<&str> = shipped_presets_for("imbib")
            .iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(imbib, vec!["Default", "Triage", "Reading", "Full"]);
        let imprint: Vec<&str> = shipped_presets_for("imprint")
            .iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(imprint, vec!["Default", "Writing"]);
    }

    #[test]
    fn preset_ids_are_a_function_of_app_and_name() {
        assert_eq!(preset_id("imbib", "Triage"), preset_id("imbib", " triage "));
        assert_ne!(preset_id("imbib", "Triage"), preset_id("imprint", "Triage"));
        // And stable across runs, which is the property seeding depends on.
        assert_eq!(
            preset_id("imbib", "Triage").to_string(),
            preset_id("imbib", "Triage").to_string()
        );
    }

    // ── Queries compile ─────────────────────────────────────────────────

    /// The deliverable's own proof: a preset can never ship with a query that
    /// does not compile.
    ///
    /// Each query is compiled twice — with every declared parameter bound to
    /// a synthetic id, and with **nothing** bound. Both must compile: every
    /// parameter a preset binds is optional (ADR-0031 D3), so a pane before
    /// the first selection renders the view kind's empty state rather than a
    /// refusal. A preset that compiles only once bound would put an error
    /// message in a window the moment it opened.
    #[test]
    fn every_query_of_every_preset_compiles() {
        for preset in shipped_presets() {
            for (name, query) in &preset.queries {
                assert_compiles(
                    &format!("{}/{} query '{name}'", preset.app_id, preset.name),
                    query,
                    &[],
                );
            }
            for pane in panes(&preset) {
                let decls: Vec<ParamDecl> = pane.params.iter().map(|b| b.decl.clone()).collect();
                assert_compiles(
                    &format!(
                        "{}/{} pane '{}'",
                        preset.app_id, preset.name, pane.view_kind
                    ),
                    &pane.query,
                    &decls,
                );
            }
        }
    }

    fn assert_compiles(what: &str, query: &PaneQuery, decls: &[ParamDecl]) {
        let mut bound = Bindings::new();
        for decl in decls {
            assert!(
                !decl.required,
                "{what}: the parameter '{}' is required, but ADR-0031 D3 says an unfilled \
                 parameter renders the empty state — a preset must never ship one that \
                 refuses before the first selection",
                decl.name
            );
            bound = bound.with(decl.name.clone(), Uuid::new_v4());
        }
        compile(query, decls, &bound, &manifest())
            .unwrap_or_else(|e| panic!("{what} does not compile with its parameters bound: {e}"));
        compile(query, decls, &Bindings::new(), &manifest())
            .unwrap_or_else(|e| panic!("{what} does not compile with nothing bound: {e}"));
    }

    #[test]
    fn every_preset_normalizes_to_itself() {
        for preset in shipped_presets() {
            let mut again = preset.layout.clone();
            again.normalize();
            assert_eq!(
                again, preset.layout,
                "{}/{} is not in canonical shape",
                preset.app_id, preset.name
            );
        }
    }

    #[test]
    fn every_role_resolves_to_a_pane_of_its_own_tree() {
        for preset in shipped_presets() {
            assert!(
                !preset.roles.is_empty(),
                "{}/{} assigns no roles",
                preset.app_id,
                preset.name
            );
            for (role, tile) in &preset.roles {
                assert_eq!(
                    preset.layout.pane_with_role(role),
                    Some(*tile),
                    "{}/{}: role '{role}' does not resolve to tile {tile}",
                    preset.app_id,
                    preset.name
                );
                assert!(
                    preset.layout.pane(*tile).is_some(),
                    "{}/{}: role '{role}' names tile {tile}, which is not a pane",
                    preset.app_id,
                    preset.name
                );
            }
        }
    }

    #[test]
    fn every_preset_round_trips_through_json() {
        for preset in shipped_presets() {
            let json = serde_json::to_string(&preset.layout).expect("encode");
            let back: Layout = serde_json::from_str(&json).expect("decode");
            assert_eq!(
                back, preset.layout,
                "{}/{} does not survive the wire",
                preset.app_id, preset.name
            );
        }
    }

    // ── Parity with the Swift presets ───────────────────────────────────

    /// The detail pane's view kind is the app's `defaultDetailTab`, spelled as
    /// the `DetailTab` raw value.
    #[test]
    fn each_default_preset_opens_on_its_apps_default_detail_tab() {
        for (app, tab) in [
            ("imbib", "info"),
            ("imprint", "source"),
            ("implore", "info"),
            ("impel", "info"),
            ("impart", "info"),
            ("impress", "info"),
        ] {
            let preset = shipped_preset(app, "Default").expect("a default preset");
            let detail = preset
                .roles
                .get(&Role::DETAIL)
                .copied()
                .expect("detail role");
            assert_eq!(
                preset.layout.pane(detail).map(|p| p.view_kind.to_string()),
                Some(tab.to_string()),
                "{app}'s default detail tab"
            );
        }
    }

    /// The list pane is the app's `defaultSection` query.
    #[test]
    fn each_default_preset_lists_its_apps_default_section() {
        for (app, section) in [
            ("imbib", "inbox"),
            ("imprint", "manuscripts"),
            ("implore", "figures"),
            ("impel", "agents"),
            ("impart", "mail"),
            ("impress", "inbox"),
        ] {
            let preset = shipped_preset(app, "Default").expect("a default preset");
            let list = preset.roles.get(&Role::LIST).copied().expect("list role");
            let pane = preset.layout.pane(list).expect("a list pane");
            assert_eq!(
                Some(&pane.query),
                preset.queries.get(section),
                "{app}'s list pane should be its '{section}' query"
            );
        }
    }

    /// Every section of every app's `visibleSections` is either a named query
    /// or an entry in [`MATERIALIZE_FIRST`]. A section in neither list is the
    /// silent gap this test exists to refuse.
    #[test]
    fn every_visible_section_is_accounted_for() {
        // AppShellConfiguration, read once and transcribed here. Read-only:
        // the Swift file is the source, and this is the claim under test.
        let visible: &[(&str, &[&str])] = &[
            (
                "imbib",
                &[
                    "inbox",
                    "libraries",
                    "sharedWithMe",
                    "scixLibraries",
                    "search",
                    "exploration",
                    "flagged",
                    "tags",
                    "citedInManuscripts",
                    "artifacts",
                    "reviewQueue",
                    "dismissed",
                ],
            ),
            (
                "imprint",
                &[
                    "manuscripts",
                    "citedInManuscripts",
                    "flagged",
                    "tags",
                    "dismissed",
                ],
            ),
            ("implore", &["figures", "tags"]),
            ("impart", &["mail", "tags"]),
            ("impel", &["agents", "tags"]),
            (
                "impress",
                &[
                    "inbox",
                    "libraries",
                    "sharedWithMe",
                    "scixLibraries",
                    "search",
                    "exploration",
                    "flagged",
                    "tags",
                    "citedInManuscripts",
                    "artifacts",
                    "manuscripts",
                    "figures",
                    "mail",
                    "agents",
                    "reviewQueue",
                    "dismissed",
                ],
            ),
        ];
        let deferred: Vec<&str> = MATERIALIZE_FIRST
            .iter()
            .map(|(name, _)| name.split_whitespace().next().unwrap_or(name))
            .collect();

        for (app, sections) in visible {
            let queries = named_queries(app);
            for section in *sections {
                let expressed = queries.contains_key(*section);
                let materialized = deferred.contains(section);
                assert!(
                    expressed || materialized,
                    "{app}'s '{section}' section is neither a named query nor listed in \
                     MATERIALIZE_FIRST"
                );
            }
            // And the outline shows exactly this app's sections: no more (a
            // section the chassis would hide), no fewer (a section the
            // outline would drop and only log).
            let mut outline: Vec<String> = crate::outline::outline_sections(app)
                .into_iter()
                .map(|s| s.section)
                .collect();
            outline.sort();
            let mut expected: Vec<String> = sections.iter().map(|s| s.to_string()).collect();
            expected.sort();
            assert_eq!(outline, expected, "{app}'s outline sections");
        }
    }

    #[test]
    fn materialize_first_says_why_for_every_entry() {
        for (name, reason) in MATERIALIZE_FIRST {
            assert!(
                reason.len() > 40,
                "'{name}' needs a real reason, not '{reason}'"
            );
        }
    }

    // ── Hiding a pane ───────────────────────────────────────────────────
    //
    // That `0.0` is refused by `Verb::Resize` and rewritten to a full column
    // by `normalize` — the reason HIDDEN_SHARE exists at all — is proven
    // where the constant lives, in `impress_layout::shares`. What is left
    // here is the presets' own claim: that Triage and Reading actually hide
    // the pane they say they hide.

    /// Triage's hidden detail pane survives normalization, reads as hidden,
    /// and is still a pane.
    #[test]
    fn triage_hides_the_detail_pane_without_taking_it_out_of_the_tree() {
        let preset = imbib_triage();
        let mut layout = preset.layout.clone();
        layout.normalize();
        layout.normalize();
        assert_eq!(
            layout, preset.layout,
            "Triage must be idempotent under normalize"
        );

        let window = layout.current_window().expect("a window");
        let root = layout.window(window).expect("the window").root;
        let shares = match layout.tile(root) {
            Some(Tile::Container(Container::Linear { shares, .. })) => shares.clone(),
            _ => panic!("the root should be a linear container"),
        };
        assert_eq!(shares.len(), 3);
        assert!(
            is_hidden(shares[2]),
            "Triage hides the detail pane; share was {}",
            shares[2]
        );
        assert!(!is_hidden(shares[0]) && !is_hidden(shares[1]));

        // And the hidden pane is still a pane: its query, role and view kind
        // are all there, which is why ⌘0 is a resize and not a split.
        let detail = preset
            .roles
            .get(&Role::DETAIL)
            .copied()
            .expect("detail role");
        assert!(layout.pane(detail).is_some());
    }

    #[test]
    fn reading_hides_the_navigator_and_shows_the_pdf() {
        let preset = imbib_reading();
        let window = preset.layout.current_window().expect("a window");
        let root = preset.layout.window(window).expect("the window").root;
        let shares = match preset.layout.tile(root) {
            Some(Tile::Container(Container::Linear { shares, .. })) => shares.clone(),
            _ => panic!("linear root"),
        };
        assert!(is_hidden(shares[0]), "Reading hides the navigator");
        let detail = preset.roles.get(&Role::DETAIL).copied().expect("detail");
        assert_eq!(
            preset.layout.pane(detail).map(|p| p.view_kind.clone()),
            Some(ViewKindId::PDF)
        );
    }

    #[test]
    fn writing_puts_a_preview_on_the_same_item_as_the_source() {
        let preset = imprint_writing();
        let source = preset.roles.get(&Role::DETAIL).copied().expect("detail");
        let preview = preset.roles.get(&Role::PREVIEW).copied().expect("preview");
        let source = preset.layout.pane(source).expect("source pane");
        let preview = preset.layout.pane(preview).expect("preview pane");
        assert_eq!(
            source.query, preview.query,
            "both views show the same $item"
        );
        assert_eq!(
            source.param(DETAIL_PARAM).map(|b| b.source.clone()),
            preview.param(DETAIL_PARAM).map(|b| b.source.clone()),
            "and follow the same channel"
        );
        assert_eq!(preview.view_kind, ViewKindId::PDF);
        assert_eq!(preset.layout.panes().len(), 4);
    }

    // ── The store side ──────────────────────────────────────────────────

    fn store() -> Arc<dyn AttributedStore> {
        Arc::new(koine_store_memory::MemoryStore::new())
    }

    #[test]
    fn seeding_twice_leaves_one_row_per_preset() {
        let presets = PresetStore::new(store());
        let first = presets.ensure_shipped("imbib").expect("seed");
        let second = presets.ensure_shipped("imbib").expect("re-seed");
        assert_eq!(first.len(), shipped_presets_for("imbib").len());
        assert_eq!(
            first.iter().map(|r| r.id).collect::<Vec<_>>(),
            second.iter().map(|r| r.id).collect::<Vec<_>>(),
            "re-seeding must not mint a second Triage"
        );
    }

    #[test]
    fn a_seeded_preset_reads_back_as_the_table_shipped_it() {
        let presets = PresetStore::new(store());
        presets.ensure_shipped("imprint").expect("seed");
        let (row, stored) = presets
            .load("imprint", "Writing")
            .expect("load")
            .expect("Writing is shipped");
        assert_eq!(row.version, Some(SHIPPED_VERSION));
        assert_eq!(stored.layout.as_ref(), Some(&imprint_writing().layout));
        assert_eq!(stored.queries, named_queries("imprint"));
        assert_eq!(presets.matches_shipped(&row, &stored), Some(true));
    }

    #[test]
    fn an_edited_preset_survives_re_seeding_and_reset_restores_it() {
        let presets = PresetStore::new(store());
        presets.ensure_shipped("imbib").expect("seed");

        // The user rearranges Triage.
        let edited = imbib_reading().layout;
        presets
            .save(
                "imbib",
                "Triage",
                Some("mine now"),
                &edited,
                &named_queries("imbib"),
                &imbib_reading().roles,
                None,
                ActorKind::Human,
                "test edit",
            )
            .expect("save");

        presets.ensure_shipped("imbib").expect("re-seed");
        let (row, stored) = presets.load("imbib", "Triage").expect("load").expect("row");
        assert_eq!(
            stored.layout.as_ref(),
            Some(&edited),
            "re-seeding must never overwrite the user's edit"
        );
        assert_eq!(row.purpose.as_deref(), Some("mine now"));
        assert_eq!(presets.matches_shipped(&row, &stored), Some(false));

        presets
            .reset("imbib", "Triage", ActorKind::Human)
            .expect("reset");
        let (row, stored) = presets.load("imbib", "Triage").expect("load").expect("row");
        assert_eq!(stored.layout.as_ref(), Some(&imbib_triage().layout));
        assert_eq!(presets.matches_shipped(&row, &stored), Some(true));
    }

    #[test]
    fn resetting_a_preset_the_table_never_shipped_is_refused() {
        let presets = PresetStore::new(store());
        let err = presets
            .reset("imbib", "Mine", ActorKind::Human)
            .expect_err("nothing to reset to");
        assert!(
            err.message.contains("Triage"),
            "the refusal should name what IS shipped: {err}"
        );
        assert_eq!(err.code, "not-found");
    }

    #[test]
    fn preset_rows_come_back_in_ordinal_order() {
        let presets = PresetStore::new(store());
        presets.ensure_shipped("imbib").expect("seed");
        presets
            .save(
                "imbib",
                "Mine",
                None,
                &imbib_default().layout,
                &named_queries("imbib"),
                &imbib_default().roles,
                None,
                ActorKind::Human,
                "a preset of my own",
            )
            .expect("save");
        let names: Vec<String> = presets
            .rows("imbib")
            .expect("rows")
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(names, vec!["Default", "Triage", "Reading", "Full", "Mine"]);
    }

    #[test]
    fn one_apps_presets_are_not_another_apps() {
        let presets = PresetStore::new(store());
        presets.ensure_shipped("imbib").expect("seed imbib");
        presets.ensure_shipped("imprint").expect("seed imprint");
        assert_eq!(presets.rows("imbib").expect("rows").len(), 4);
        assert_eq!(presets.rows("imprint").expect("rows").len(), 2);
        assert!(presets.load("imprint", "Triage").expect("load").is_none());
    }

    // ------------------------------------------------ W5: Inbox parity

    /// A library row, so a paper's `parent_id` has something to reference.
    fn library(id: ItemId) -> Item {
        Item {
            schema: impress_item::schema::refs::IMBIB_LIBRARY,
            parent: None,
            ..paper(id, id, false)
        }
    }

    /// A publication row parented to `parent`.
    fn paper(id: ItemId, parent: ItemId, is_read: bool) -> Item {
        let now = Utc::now();
        let mut payload = BTreeMap::new();
        payload.insert("title".to_string(), Value::String(format!("paper {id}")));
        Item {
            id,
            schema: impress_item::schema::refs::IMBIB_BIBLIOGRAPHY_ENTRY,
            payload,
            created: now,
            modified: now,
            author: "test".into(),
            author_kind: ActorKind::Human,
            logical_clock: 0,
            origin: None,
            canonical_id: None,
            tags: vec![],
            flag: None,
            is_read,
            is_starred: false,
            priority: Priority::None,
            visibility: Visibility::Private,
            message_type: None,
            produced_by: None,
            version: None,
            batch_id: None,
            references: vec![],
            parent: Some(parent),
        }
    }

    /// The chassis' Inbox lists EVERY paper in the Inbox library, read and
    /// unread (`RustStoreAdapter.queryPublications(for: .inbox)` →
    /// `queryPublications(parentId:)`, no read predicate; the list is built
    /// with `disableUnreadFilter`). The tree is now the only root, so the
    /// `inbox` named query must list the same rows — run here against a real
    /// store, not compared as a shape.
    #[test]
    fn the_inbox_lists_read_and_unread_papers_like_the_chassis_list() {
        let store = store();
        let inbox = Uuid::new_v4();
        let elsewhere = Uuid::new_v4();
        let unread = Uuid::new_v4();
        let read = Uuid::new_v4();
        store.insert(library(inbox)).expect("the Inbox library");
        store.insert(library(elsewhere)).expect("another library");
        store.insert(paper(unread, inbox, false)).expect("unread");
        store.insert(paper(read, inbox, true)).expect("read");
        store
            .insert(paper(Uuid::new_v4(), elsewhere, false))
            .expect("another library's paper");

        let decls = vec![ParamDecl {
            name: LIBRARY_PARAM.into(),
            kind: "library".into(),
            required: false,
        }];
        let bindings = Bindings::new().with(LIBRARY_PARAM, inbox);
        let run = |query: &PaneQuery| -> Vec<ItemId> {
            let compiled = compile(query, &decls, &bindings, &manifest()).expect("compiles");
            let mut ids: Vec<ItemId> = store
                .query(&compiled.item_query)
                .expect("runs")
                .into_iter()
                .map(|item| item.id)
                .collect();
            ids.sort();
            ids
        };

        let mut both = vec![unread, read];
        both.sort();
        assert_eq!(run(&q::inbox()), both, "read AND unread, only the Inbox's");
        assert_eq!(
            run(&q::inbox_revision_1()),
            vec![unread],
            "revision 1 was the unread subset — the 57-of-68 the W3 log recorded"
        );
    }

    #[test]
    fn a_seeded_revision_1_preset_is_upgraded_and_then_matches_the_table() {
        let presets = PresetStore::new(store());
        let triage = shipped_preset("imbib", "Triage").expect("imbib ships Triage");
        let old = previous_revision(&triage);
        assert!(
            old.queries.values().any(|q| *q == q::inbox_revision_1()),
            "Triage named the Inbox, so its revision 1 carries the unread query"
        );
        presets
            .save(
                old.app_id,
                old.name,
                Some(old.purpose),
                &old.layout,
                &old.queries,
                &old.roles,
                Some(old.version),
                ActorKind::System,
                "seed revision 1",
            )
            .expect("seed revision 1");

        presets.ensure_shipped("imbib").expect("upgrade");
        let (row, stored) = presets.load("imbib", "Triage").expect("load").expect("row");
        assert_eq!(row.version, Some(SHIPPED_VERSION));
        assert_eq!(presets.matches_shipped(&row, &stored), Some(true));
        assert_eq!(stored.queries.get("list"), Some(&q::inbox()));
    }

    #[test]
    fn an_edited_revision_1_preset_is_left_as_the_user_left_it() {
        let presets = PresetStore::new(store());
        let triage = shipped_preset("imbib", "Triage").expect("imbib ships Triage");
        let mut edited = previous_revision(&triage);
        edited
            .queries
            .insert("list".into(), q::flagged_publications());
        presets
            .save(
                edited.app_id,
                edited.name,
                Some(edited.purpose),
                &edited.layout,
                &edited.queries,
                &edited.roles,
                Some(edited.version),
                ActorKind::Human,
                "the user's own Triage",
            )
            .expect("seed an edited revision 1");

        presets.ensure_shipped("imbib").expect("ensure");
        let (row, stored) = presets.load("imbib", "Triage").expect("load").expect("row");
        assert_eq!(row.version, Some(SHIPPED_VERSION - 1), "not upgraded");
        assert_eq!(stored.queries.get("list"), Some(&q::flagged_publications()));
        assert_eq!(presets.matches_shipped(&row, &stored), Some(false));
    }

    #[test]
    fn a_preset_that_never_named_the_inbox_is_its_own_previous_revision() {
        let figures = shipped_preset("implore", "Default").expect("implore ships Default");
        let old = previous_revision(&figures);
        assert_eq!(old.queries, figures.queries);
        assert_eq!(old.layout, figures.layout);
        assert_eq!(old.version, SHIPPED_VERSION - 1);
    }

    #[test]
    fn revision_1_of_the_inbox_is_superseded_whatever_library_it_bound() {
        assert!(is_superseded_list_query(&q::inbox_revision_1()));
        let bound = PaneQuery {
            scope: Scope::Parent {
                id: ItemRef::Id { id: Uuid::new_v4() },
            },
            ..q::inbox_revision_1()
        };
        assert!(
            is_superseded_list_query(&bound),
            "the outline's substituted Inbox"
        );
        assert!(!is_superseded_list_query(&q::inbox()));
        let library_row = PaneQuery {
            scope: Scope::Parent {
                id: ItemRef::Id { id: Uuid::new_v4() },
            },
            ..q::inbox()
        };
        assert!(
            !is_superseded_list_query(&library_row),
            "a library row is the user's"
        );
    }

    #[test]
    fn the_inbox_list_of_either_revision_is_the_presets_list() {
        assert_eq!(
            shipped_list_queries("imbib"),
            vec![q::inbox(), q::inbox_revision_1()]
        );
        assert_eq!(shipped_list_queries("implore"), vec![q::figures()]);
    }

    // --------------------------------------- W5: section bindings as preset data

    /// A section whose named query lists records binds the kind it lists. The
    /// one exception is `libraries`: its query is the library ROWS (the
    /// outline's own), while its binding is the kind those libraries hold.
    #[test]
    fn bindings_agree_with_the_named_queries() {
        for app in preset_app_ids() {
            let named = named_queries(app);
            for (section, kind) in section_bindings(app) {
                if section == "libraries" {
                    continue;
                }
                let Some(query) = named.get(section) else {
                    continue; // a MATERIALIZE_FIRST section: no query to disagree with
                };
                assert_eq!(
                    query.kinds.first().map(String::as_str),
                    Some(kind),
                    "{app}.{section} is bound to {kind} but its query lists {:?}",
                    query.kinds
                );
            }
        }
    }

    #[test]
    fn every_binding_is_a_section_the_app_shows_and_review_is_never_bound() {
        for app in preset_app_ids() {
            let shown: Vec<String> = crate::outline::outline_sections(app)
                .into_iter()
                .map(|s| s.section)
                .collect();
            for (section, _) in section_bindings(app) {
                assert!(
                    shown.iter().any(|s| s == section),
                    "{app} binds {section}, which its outline does not show"
                );
            }
            assert!(!section_bindings(app).contains_key("reviewQueue"));
        }
        assert!(section_bindings("nobody").is_empty());
    }

    /// Tags is the one binding every app has, and the one the tree reads to
    /// turn a tag row into `Filter::Tag` over the right kind.
    #[test]
    fn every_app_binds_tags_to_its_own_kind() {
        let tags: Vec<(&str, &str)> = preset_app_ids()
            .into_iter()
            .map(|app| (app, section_bindings(app)["tags"]))
            .collect();
        assert_eq!(
            tags,
            vec![
                ("imbib", "publication"),
                ("imprint", "manuscript"),
                ("implore", "figure"),
                ("impel", "task"),
                ("impart", "message"),
                ("impress", "publication"),
            ]
        );
    }
}
