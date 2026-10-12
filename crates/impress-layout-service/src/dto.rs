//! The wire types the verbs take and return.
//!
//! Where a crate type already derives `serde` **and** `schemars::JsonSchema`
//! under `impress-layout`'s `schema` feature — [`impress_layout::Layout`],
//! [`impress_layout::PaneSpec`], [`impress_layout::ParamSource`],
//! [`impress_layout::Geometry`], [`impress_pane_query::PaneQuery`] — it
//! is used **directly** on the trait. A mirror DTO of a type that already
//! publishes its schema is a second definition of the same value, which is the
//! failure mode this repo has a CI workflow about.
//!
//! What is here is the shapes the crate types do not have: the pane reference
//! in the form an argument list can carry, the string spellings of the small
//! enums, and the result envelopes.

use std::collections::BTreeMap;

use impress_item::item::ItemId;
use impress_layout::{
    ChannelId, ContainerKind, Direction, Layout, LinearDir, PaneSpec, Patch, Placement, TileId,
    ViewKindId, WindowId,
};
use impress_pane_query::PaneQueryError;
use impress_pane_query_compile::CompiledQuery;
use impress_service_core::wire::{wire_version, WIRE_VERSION};
use impress_service_core::Refusal;
use serde::{Deserialize, Serialize};

use crate::session::{AppliedVerb, Stack};

/// How a verb names a pane (ADR-0031 D8): exactly one of `{"id": 7}`,
/// `{"role": "detail"}`, `{"direction": "left"}` or `{"focused": true}`.
///
/// The one spelling (review RL-L3, AC-F3): this IS `impress_layout::PaneRef`'s
/// wire form, so a reference copied from any result, message or HTTP body
/// works as an argument everywhere. Unknown fields, an empty `{}` and two
/// selectors at once are refused with `invalid-argument` — an empty
/// reference used to mean the focused pane, so a reference in the other
/// spelling closed it.
pub use impress_layout::PaneRefWire as PaneRefDto;

/// `horizontal` | `vertical`.
pub fn parse_linear_dir(raw: &str) -> Result<LinearDir, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "horizontal" | "h" | "row" | "left-right" => Ok(LinearDir::Horizontal),
        "vertical" | "v" | "column" | "top-bottom" => Ok(LinearDir::Vertical),
        other => Err(format!(
            "unknown split direction '{other}'. Use 'horizontal' or 'vertical'."
        )),
    }
}

/// `left` | `right` | `up` | `down` | `next` | `prev`.
pub fn parse_direction(raw: &str) -> Result<Direction, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "left" | "h" => Ok(Direction::Left),
        "right" | "l" => Ok(Direction::Right),
        "up" | "k" => Ok(Direction::Up),
        "down" | "j" => Ok(Direction::Down),
        "next" | "forward" => Ok(Direction::Next),
        "prev" | "previous" | "back" => Ok(Direction::Prev),
        other => Err(format!(
            "unknown direction '{other}'. Use left, right, up, down, next or prev."
        )),
    }
}

/// `left` | `right` | `above` | `below` | `into-tabs`.
pub fn parse_placement(raw: &str) -> Result<Placement, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "left" | "before" => Ok(Placement::Left),
        "right" | "after" => Ok(Placement::Right),
        "above" | "up" | "top" => Ok(Placement::Above),
        "below" | "down" | "bottom" => Ok(Placement::Below),
        "into-tabs" | "into_tabs" | "tabs" | "tab" | "centre" | "center" => Ok(Placement::IntoTabs),
        other => Err(format!(
            "unknown placement '{other}'. Use left, right, above, below or into-tabs."
        )),
    }
}

/// `tabs` | `horizontal` | `vertical` | `grid`.
pub fn parse_container_kind(raw: &str) -> Result<ContainerKind, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "tabs" | "tab" => Ok(ContainerKind::Tabs),
        "horizontal" | "h" | "row" => Ok(ContainerKind::Horizontal),
        "vertical" | "v" | "column" => Ok(ContainerKind::Vertical),
        "grid" => Ok(ContainerKind::Grid),
        other => Err(format!(
            "unknown container kind '{other}'. Use tabs, horizontal, vertical or grid."
        )),
    }
}

/// `1`..`8`, or `follow` for the window's default channel.
pub fn parse_channel(raw: &str) -> Result<ChannelId, String> {
    let raw = raw.trim().to_ascii_lowercase();
    if raw == "follow" {
        return Ok(ChannelId::Follow);
    }
    match raw.parse::<u8>() {
        // Refused, not clamped: an agent asking for channel 12 used to land
        // on channel 8 and be told `ok` (review RL-L21). A gesture never
        // names a channel out of range, so only a typo reaches this.
        Ok(n) if (1..=ChannelId::MAX).contains(&n) => Ok(ChannelId::number(n)),
        _ => Err(format!(
            "unknown channel '{raw}'. Use 1-{}, or 'follow' for the window's default.",
            ChannelId::MAX
        )),
    }
}

/// `arrangement` (the whole window's shape) or `exploration` (one pane's
/// bindings and view state) — the two rings this crate owns. The third,
/// the editor session's own undo manager, is never ours (ADR-0031 D7).
pub fn parse_stack(raw: &str) -> Result<bool, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "arrangement" | "layout" | "window" => Ok(true),
        "exploration" | "pane" | "explore" => Ok(false),
        other => Err(format!(
            "unknown undo stack '{other}'. Use 'arrangement' or 'exploration'."
        )),
    }
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

/// What one verb changed, in the form a renderer redraws from.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PatchSummary {
    /// Tiles whose value differs, added or removed included.
    pub tiles: Vec<u64>,
    /// Tiles the verb created.
    pub added: Vec<u64>,
    /// Tiles the verb removed (normalization included — a collapse takes a
    /// container with it).
    pub removed: Vec<u64>,
    /// The window list changed: root, focus, geometry, default channel,
    /// maximized, or the set of windows itself.
    pub windows_changed: bool,
    /// A selection was published.
    pub channels_changed: bool,
    /// Nothing changed at all — a `restore` with nothing maximized, a `focus`
    /// on the already-focused pane. Such a patch is never pushed onto a ring:
    /// a no-op gesture must not cost the user a ⌘Z.
    pub empty: bool,
}

impl From<&Patch> for PatchSummary {
    fn from(patch: &Patch) -> Self {
        let mut added = Vec::new();
        let mut removed = Vec::new();
        for (id, change) in &patch.tiles {
            match (&change.before, &change.after) {
                (None, Some(_)) => added.push(id.raw()),
                (Some(_), None) => removed.push(id.raw()),
                _ => {}
            }
        }
        Self {
            tiles: patch.tiles.keys().map(|t| t.raw()).collect(),
            added,
            removed,
            windows_changed: patch.windows.is_some(),
            channels_changed: patch.channels.is_some(),
            empty: patch.is_empty(),
        }
    }
}

/// The result of every mutating verb.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LayoutVerbResult {
    pub ok: bool,
    /// The wire convention this answer is written in
    /// (`impress_service_core::wire`): snake_case, refusals `{code,
    /// message}`. Moves only when a shape changes incompatibly.
    #[serde(default = "wire_version")]
    pub wire_version: u32,
    pub message: String,
    /// Why it was refused, machine-readable: a `LayoutError` tag
    /// (`unknown-tile`, `cannot-close-last-pane`, …) or a generic code
    /// (`invalid-argument`, `not-found`, `conflict`, `store-error`,
    /// `store-unavailable`). Absent when `ok` is true. See
    /// `impress_service_core::refusal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The focused leaf afterwards — half of what a renderer needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<u64>,
    /// The panes to redraw — the other half. For `select`, every pane whose
    /// bindings the publication changed; otherwise, the tiles the patch
    /// touched.
    pub affected_panes: Vec<u64>,
    /// The window the verb resolved against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
    /// Which undo ring the patch landed on: `arrangement`, `exploration`, or
    /// `none` for the verbs that record nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    /// The pane whose exploration ring took the patch, when `stack` is
    /// `exploration`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_pane: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<PatchSummary>,
    /// The live layout row's revision after this verb — what an agent
    /// passes back as `expected_revision` on its next verb, so a change the
    /// person made in between is refused as `conflict` instead of silently
    /// acted on (review RL-L1). Absent for verbs that do not touch the live
    /// row (`save_layout`, `delete_layout`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    /// The tree after the verb, as JSON — for an in-process host that
    /// renders it (the FFI's `SharedLayout`), so it need not take the
    /// registry lock again and clone the tree to read what the verb just
    /// wrote (review RL-L14). Filled only by a service built
    /// `with_tree_in_results`, only when the verb left a tree to show, and
    /// never on the wire: MCP, the CLI and `/api/layout/*` answer exactly
    /// what they did.
    #[serde(skip)]
    #[schemars(skip)]
    pub tree_json: Option<String>,
}

impl LayoutVerbResult {
    /// A refusal: `ok: false`, the refusal's `code` and `message`.
    pub fn refused(refusal: Refusal) -> Self {
        Self {
            ok: false,
            wire_version: WIRE_VERSION,
            message: refusal.message,
            code: Some(refusal.code),
            focused: None,
            affected_panes: Vec::new(),
            window: None,
            stack: None,
            stack_pane: None,
            patch: None,
            revision: None,
            tree_json: None,
        }
    }

    /// The same result, carrying the live row's revision.
    pub fn with_revision(mut self, revision: Option<u64>) -> Self {
        self.revision = revision;
        self
    }

    /// A success that did not touch the tree (`delete_layout`, `commit` as a
    /// preset): nothing to redraw.
    pub fn done(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            wire_version: WIRE_VERSION,
            message: message.into(),
            code: None,
            focused: None,
            affected_panes: Vec::new(),
            window: None,
            stack: None,
            stack_pane: None,
            patch: None,
            revision: None,
            tree_json: None,
        }
    }

    pub fn applied(message: impl Into<String>, applied: &AppliedVerb) -> Self {
        Self {
            ok: true,
            wire_version: WIRE_VERSION,
            message: message.into(),
            code: None,
            focused: applied.focused.map(TileId::raw),
            affected_panes: raw_tiles(&applied.affected),
            window: Some(applied.window.raw()),
            stack: Some(applied.stack.name().to_string()),
            stack_pane: applied.stack.pane().map(TileId::raw),
            patch: Some(PatchSummary::from(&applied.patch)),
            revision: None,
            tree_json: None,
        }
    }

    /// For the verbs that touch the store rather than the tree (`save_layout`,
    /// `apply_layout`, `commit`, `undo`, `redo`): still carries `focused` and
    /// `affected_panes`, because a renderer has to redraw after them too.
    pub fn from_layout(
        message: impl Into<String>,
        layout: &Layout,
        patch: Option<&Patch>,
        stack: Option<&Stack>,
    ) -> Self {
        let window = layout.current_window().ok();
        let focused = window
            .and_then(|w| layout.window(w))
            .and_then(|w| w.focused);
        let affected = match patch {
            Some(patch) => patch
                .tiles
                .keys()
                .copied()
                .filter(|tile| layout.pane(*tile).is_some())
                .collect(),
            // No patch means the whole tree is new: redraw every pane.
            None => layout.panes(),
        };
        Self {
            ok: true,
            wire_version: WIRE_VERSION,
            message: message.into(),
            code: None,
            focused: focused.map(TileId::raw),
            affected_panes: raw_tiles(&affected),
            window: window.map(WindowId::raw),
            stack: stack.map(|s| s.name().to_string()),
            stack_pane: stack.and_then(Stack::pane).map(TileId::raw),
            patch: patch.map(PatchSummary::from),
            revision: None,
            tree_json: None,
        }
    }
}

/// The whole tree, for a renderer opening a window or an agent reading the
/// workspace.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LayoutResult {
    pub ok: bool,
    /// The wire convention this answer is written in
    /// (`impress_service_core::wire`): snake_case, refusals `{code,
    /// message}`. Moves only when a shape changes incompatibly.
    #[serde(default = "wire_version")]
    pub wire_version: u32,
    pub message: String,
    /// Why it was refused, machine-readable: a `LayoutError` tag
    /// (`unknown-tile`, `cannot-close-last-pane`, …) or a generic code
    /// (`invalid-argument`, `not-found`, `conflict`, `store-error`,
    /// `store-unavailable`). Absent when `ok` is true. See
    /// `impress_service_core::refusal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<Layout>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<u64>,
    pub affected_panes: Vec<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
    /// The `impress/ui/layout@1.0.0` row this tree is the value of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    /// The `(app_id, device)` scope, echoed — a caller that passed `device:
    /// null` finds out here which machine it was answered for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// The live row's revision this tree is. Pass it as `expected_revision`
    /// to a verb to have it refused (`conflict`) if anyone changed the
    /// layout since this read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    /// `"fallback"` when the real store could not be opened and this answer
    /// comes from the in-memory stand-in (review AC-F20): it is not the
    /// user's layout. Absent for the real store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<String>,
}

impl LayoutResult {
    /// A refusal: `ok: false`, the refusal's `code` and `message`.
    pub fn refused(refusal: Refusal) -> Self {
        Self {
            ok: false,
            wire_version: WIRE_VERSION,
            message: refusal.message,
            code: Some(refusal.code),
            layout: None,
            focused: None,
            affected_panes: Vec::new(),
            window: None,
            item_id: None,
            device: None,
            revision: None,
            store: None,
        }
    }
}

/// A compiled pane query: what the store will actually be asked (ADR-0031 D2).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CompiledQueryDto {
    /// The `impress_item::query::ItemQuery` the algebra compiled to.
    #[schemars(with = "serde_json::Value")]
    pub item_query: serde_json::Value,
    /// Every schema ref the query can touch — the incremental-invalidation key.
    pub schema_refs: Vec<String>,
    /// The single item, when the scope was `Item` and it resolved. A detail
    /// pane short-circuits on this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub single_item: Option<String>,
    /// Why the query did not compile — an unknown kind, an unbound required
    /// parameter. A typed refusal, never an empty list that reads as "no data
    /// yet".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl CompiledQueryDto {
    pub fn compiled(compiled: &CompiledQuery) -> Self {
        Self {
            item_query: serde_json::to_value(&compiled.item_query)
                .unwrap_or(serde_json::Value::Null),
            schema_refs: compiled.schema_refs.clone(),
            single_item: compiled.single_item.map(|id| id.to_string()),
            error: None,
        }
    }

    pub fn refused(error: &PaneQueryError) -> Self {
        Self {
            item_query: serde_json::Value::Null,
            schema_refs: Vec::new(),
            single_item: None,
            error: Some(error.to_string()),
        }
    }
}

/// One pane: its spec, its compiled query, and what its parameters currently
/// resolve to.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneResult {
    pub ok: bool,
    /// The wire convention this answer is written in
    /// (`impress_service_core::wire`): snake_case, refusals `{code,
    /// message}`. Moves only when a shape changes incompatibly.
    #[serde(default = "wire_version")]
    pub wire_version: u32,
    pub message: String,
    /// Why it was refused, machine-readable: a `LayoutError` tag
    /// (`unknown-tile`, `cannot-close-last-pane`, …) or a generic code
    /// (`invalid-argument`, `not-found`, `conflict`, `store-error`,
    /// `store-unavailable`). Absent when `ok` is true. See
    /// `impress_service_core::refusal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tile: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<PaneSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<CompiledQueryDto>,
    /// Parameter name → the item id it resolves to right now. A parameter
    /// whose source is `Default`, or whose channel carries nothing of its
    /// kind, is simply absent — that is an empty state, not an error.
    pub bindings: BTreeMap<String, String>,
    /// The channel this pane publishes on, resolved against its window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<u64>,
    pub affected_panes: Vec<u64>,
    /// `"fallback"` when this answer comes from the in-memory stand-in for
    /// a store that could not be opened (review AC-F20). Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<String>,
}

impl PaneResult {
    /// A refusal: `ok: false`, the refusal's `code` and `message`.
    pub fn refused(refusal: Refusal) -> Self {
        Self {
            ok: false,
            wire_version: WIRE_VERSION,
            message: refusal.message,
            code: Some(refusal.code),
            tile: None,
            spec: None,
            query: None,
            bindings: BTreeMap::new(),
            channel: None,
            focused: None,
            affected_panes: Vec::new(),
            store: None,
        }
    }
}

/// What a channel currently carries: one selection per record kind.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ChannelResult {
    pub ok: bool,
    /// The wire convention this answer is written in
    /// (`impress_service_core::wire`): snake_case, refusals `{code,
    /// message}`. Moves only when a shape changes incompatibly.
    #[serde(default = "wire_version")]
    pub wire_version: u32,
    pub message: String,
    /// Why it was refused, machine-readable: a `LayoutError` tag
    /// (`unknown-tile`, `cannot-close-last-pane`, …) or a generic code
    /// (`invalid-argument`, `not-found`, `conflict`, `store-error`,
    /// `store-unavailable`). Absent when `ok` is true. See
    /// `impress_service_core::refusal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The resolved channel number (`follow` already collapsed).
    pub channel: u8,
    /// Record kind → the ids currently selected on this channel. An empty
    /// list is a real value: it records that nothing of that kind is
    /// selected, which is what a detail pane renders its empty state from.
    pub selections: BTreeMap<String, Vec<String>>,
    /// The panes this channel drives.
    pub affected_panes: Vec<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<u64>,
    /// `"fallback"` when this answer comes from the in-memory stand-in for
    /// a store that could not be opened (review AC-F20). Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<String>,
}

impl ChannelResult {
    /// A refusal: `ok: false`, the refusal's `code` and `message`.
    pub fn refused(refusal: Refusal) -> Self {
        Self {
            ok: false,
            wire_version: WIRE_VERSION,
            message: refusal.message,
            code: Some(refusal.code),
            channel: 0,
            selections: BTreeMap::new(),
            affected_panes: Vec::new(),
            focused: None,
            store: None,
        }
    }
}

/// What a pane reference resolved to. The verb an agent calls when it wants to
/// know what "the pane to the right" is before doing anything to it.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReferenceResult {
    pub ok: bool,
    /// The wire convention this answer is written in
    /// (`impress_service_core::wire`): snake_case, refusals `{code,
    /// message}`. Moves only when a shape changes incompatibly.
    #[serde(default = "wire_version")]
    pub wire_version: u32,
    pub message: String,
    /// Why it was refused, machine-readable: a `LayoutError` tag
    /// (`unknown-tile`, `cannot-close-last-pane`, …) or a generic code
    /// (`invalid-argument`, `not-found`, `conflict`, `store-error`,
    /// `store-unavailable`). Absent when `ok` is true. See
    /// `impress_service_core::refusal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tile: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_kind: Option<String>,
    /// False when the tile is a container rather than a pane — legal for
    /// `resize` and `set_container_kind`, and nothing else.
    pub is_pane: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<u64>,
    pub affected_panes: Vec<u64>,
    /// `"fallback"` when this answer comes from the in-memory stand-in for
    /// a store that could not be opened (review AC-F20). Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<String>,
}

impl ReferenceResult {
    /// A refusal: `ok: false`, the refusal's `code` and `message`.
    pub fn refused(refusal: Refusal) -> Self {
        Self {
            ok: false,
            wire_version: WIRE_VERSION,
            message: refusal.message,
            code: Some(refusal.code),
            tile: None,
            role: None,
            view_kind: None,
            is_pane: false,
            focused: None,
            affected_panes: Vec::new(),
            store: None,
        }
    }
}

/// One saved layout, as the ⌃⌘1–9 list shows it.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SavedLayoutDto {
    pub id: String,
    /// 1-based, and what `apply_layout(ordinal: n)` recalls. Only the first
    /// nine have a chord.
    pub ordinal: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The user's own words for what the layout is for. Uninterpreted, and not
    /// a closed vocabulary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    /// Set only on the live arrangement, which is device-scoped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    pub is_live: bool,
    pub modified: String,
}

/// The saved layouts of one app.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LayoutListResult {
    pub ok: bool,
    /// The wire convention this answer is written in
    /// (`impress_service_core::wire`): snake_case, refusals `{code,
    /// message}`. Moves only when a shape changes incompatibly.
    #[serde(default = "wire_version")]
    pub wire_version: u32,
    pub message: String,
    /// Why it was refused, machine-readable: a `LayoutError` tag
    /// (`unknown-tile`, `cannot-close-last-pane`, …) or a generic code
    /// (`invalid-argument`, `not-found`, `conflict`, `store-error`,
    /// `store-unavailable`). Absent when `ok` is true. See
    /// `impress_service_core::refusal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub layouts: Vec<SavedLayoutDto>,
    /// `"fallback"` when this answer comes from the in-memory stand-in for
    /// a store that could not be opened (review AC-F20). Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<String>,
}

impl LayoutListResult {
    /// A refusal: `ok: false`, the refusal's `code` and `message`.
    pub fn refused(refusal: Refusal) -> Self {
        Self {
            ok: false,
            wire_version: WIRE_VERSION,
            message: refusal.message,
            code: Some(refusal.code),
            layouts: Vec::new(),
            store: None,
        }
    }
}

pub(crate) fn raw_tiles(tiles: &[TileId]) -> Vec<u64> {
    tiles.iter().map(|t| t.raw()).collect()
}

pub(crate) fn parse_ids(ids: &[String]) -> Result<Vec<ItemId>, String> {
    ids.iter()
        .map(|id| {
            id.trim()
                .parse::<ItemId>()
                .map_err(|e| format!("'{id}' is not an item id: {e}"))
        })
        .collect()
}

/// A view kind as an argument names it. Only emptiness is refused here; a
/// name outside the vocabulary is refused by the tree itself
/// (`unknown-view-kind`, `impress_layout::ViewKindId::KNOWN`), so every path
/// into a verb — MCP, the CLI, the FFI, HTTP — gets the same answer.
pub(crate) fn view_kind(raw: &str) -> Result<ViewKindId, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(format!(
            "a pane needs a view kind: one of {}",
            ViewKindId::known_list()
        ));
    }
    Ok(ViewKindId::from(raw.to_string()))
}

// ---------------------------------------------------------------------------
// Presets (L7)
// ---------------------------------------------------------------------------

/// One preset, as `list_presets` shows it (ADR-0031 D10).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PresetDto {
    pub id: String,
    /// 1-based over the **union** `apply_layout(ordinal:)` recalls: this
    /// app's presets first, then its named layouts. So ⌃⌘1 is the app's
    /// default preset and a preset's ordinal here is its chord.
    pub ordinal: u32,
    pub name: String,
    pub app_id: String,
    /// What the preset is for, in the user's words. Uninterpreted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// The shipped revision this row represents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    /// True when the shipped table ships a preset of this name for this app.
    pub shipped: bool,
    /// True when the row no longer matches what the table ships — "the user
    /// edited Triage". `null` for a preset the table never shipped, which has
    /// nothing to differ from (and which `reset_preset` therefore refuses).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited: Option<bool>,
    /// How many panes its tree has, and which roles they carry — enough for a
    /// menu to describe the preset without fetching the whole tree.
    pub panes: u32,
    pub roles: Vec<String>,
    pub modified: String,
}

/// The presets of one app, in ⌃⌘1–9 order.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PresetListResult {
    pub ok: bool,
    /// The wire convention this answer is written in
    /// (`impress_service_core::wire`): snake_case, refusals `{code,
    /// message}`. Moves only when a shape changes incompatibly.
    #[serde(default = "wire_version")]
    pub wire_version: u32,
    pub message: String,
    /// Why it was refused, machine-readable: a `LayoutError` tag
    /// (`unknown-tile`, `cannot-close-last-pane`, …) or a generic code
    /// (`invalid-argument`, `not-found`, `conflict`, `store-error`,
    /// `store-unavailable`). Absent when `ok` is true. See
    /// `impress_service_core::refusal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub presets: Vec<PresetDto>,
    /// The sections this app permits that are NOT queries, with the reason —
    /// ADR-0031 D2's "materialize it first" list, so a caller asking what a
    /// preset can show is told what it cannot, and why, in the same answer.
    pub materialize_first: Vec<MaterializeFirstDto>,
    /// `"fallback"` when this answer comes from the in-memory stand-in for
    /// a store that could not be opened (review AC-F20). Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<String>,
}

impl PresetListResult {
    /// A refusal: `ok: false`, the refusal's `code` and `message`.
    pub fn refused(refusal: Refusal) -> Self {
        Self {
            ok: false,
            wire_version: WIRE_VERSION,
            message: refusal.message,
            code: Some(refusal.code),
            presets: Vec::new(),
            materialize_first: Vec::new(),
            store: None,
        }
    }
}

/// One section that is not a value in the query algebra, and why.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MaterializeFirstDto {
    pub section: String,
    pub reason: String,
}

/// The result of `save_preset` and `reset_preset`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PresetResult {
    pub ok: bool,
    /// The wire convention this answer is written in
    /// (`impress_service_core::wire`): snake_case, refusals `{code,
    /// message}`. Moves only when a shape changes incompatibly.
    #[serde(default = "wire_version")]
    pub wire_version: u32,
    pub message: String,
    /// Why it was refused, machine-readable: a `LayoutError` tag
    /// (`unknown-tile`, `cannot-close-last-pane`, …) or a generic code
    /// (`invalid-argument`, `not-found`, `conflict`, `store-error`,
    /// `store-unavailable`). Absent when `ok` is true. See
    /// `impress_service_core::refusal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<PresetDto>,
}

impl PresetResult {
    /// A refusal: `ok: false`, the refusal's `code` and `message`.
    pub fn refused(refusal: Refusal) -> Self {
        Self {
            ok: false,
            wire_version: WIRE_VERSION,
            message: refusal.message,
            code: Some(refusal.code),
            preset: None,
        }
    }
}
