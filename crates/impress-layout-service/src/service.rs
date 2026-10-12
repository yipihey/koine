//! `LayoutService` — every ADR-0031 D8 verb, as an `#[impress_method]`.
//!
//! One trait, one method per gesture, so MCP, the CLI and impel's agent loop
//! get split / move / retype / re-link / save / recall together and nobody
//! keeps a translation table in their head (invariant 6: every layout gesture
//! is a `layout-service` verb; no Swift-only layout operation).
//!
//! # The shape every verb has
//!
//! * `app_id` — which preset family this window belongs to (`imbib`,
//!   `imprint`, …). It scopes the live row and the saved layouts.
//! * `device` — `null` means *this* device; see [`crate::device`].
//! * a **pane reference** ([`PaneRefDto`]) where the verb acts on a pane:
//!   by tile id, by role, by direction from the focused leaf, or the focused
//!   leaf itself. Resolution happens once, in Rust.
//! * `actor` — `human` | `agent` | `system`, defaulting to **agent**, because
//!   these verbs arrive over MCP and the CLI and an agent that forgets to say
//!   who it is must not be recorded as the user. The GUI passes `human`.
//!
//! and every result carries `focused` and `affected_panes`, which is the whole
//! of what a renderer needs to know what to redraw.
//!
//! # The written contract (plan wave 7 T6)
//!
//! * **Strict arguments.** An argument object is checked against the
//!   method's input schema (`strict_args` below): a field it does not name is
//!   `invalid-argument` naming it. A pane reference has one spelling (see
//!   [`PaneRefDto`]).
//! * **Versioned, revisioned results.** Every envelope carries
//!   `wire_version`; verbs and `get_layout` carry the live row's `revision`,
//!   and a verb that moves the live tree takes `expected_revision`, refused
//!   `conflict` when it went stale.
//! * **Reads and the cold start (review RL-L15, decided).** A read that finds
//!   no live row writes one, and over MCP and the CLI that write is
//!   attributed to the **agent** whose read caused it. The read verbs take no
//!   `actor` argument, on purpose: an agent must not be able to record itself
//!   as the person, the same rule T5 applied to surface dispatch. The GUI
//!   reads as the human through `get_layout_as` / `compiled_pane`, so the
//!   workspace a person opens is theirs. A read answered from the in-memory
//!   fallback store says so (`store: "fallback"`).

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use impress_item::item::{ActorKind, ItemId};
use impress_item::store::AttributedStore;
use impress_layout::{
    ChannelId, Geometry, LayoutError, PaneSpec, ParamSource, Role, TileId, Verb, WindowId,
};
use impress_pane_query::{
    process_manifest, Bindings, KindManifest, PaneQuery, PaneQueryError, ParamDecl,
};
use impress_pane_query_compile::{compile_with, CompiledQuery, RootOnly, SubtreeResolver};
use impress_service_core::async_trait;
use impress_service_core::wire::WIRE_VERSION;
use impress_service_core::Refusal;
use impress_service_macros::{impress_service, impress_service_impl};

#[allow(unused_imports)]
use impress_service_macros::impress_method;

use crate::device::resolve_device;
use crate::dto::{
    parse_channel, parse_container_kind, parse_direction, parse_ids, parse_linear_dir,
    parse_placement, parse_stack, raw_tiles, view_kind, ChannelResult, CompiledQueryDto,
    LayoutListResult, LayoutResult, LayoutVerbResult, MaterializeFirstDto, PaneRefDto, PaneResult,
    PresetDto, PresetListResult, PresetResult, ReferenceResult, SavedLayoutDto,
};
use crate::presets::{self, PresetRow, PresetStore};
use crate::session::{LayoutSession, SessionRegistry, Stack, UndoTarget};
use crate::store::{actor_from, LayoutRow, LayoutStore};

/// The workspace layout: every gesture that shapes a window, as a verb.
///
/// A **pane** is a query rendered by a view kind, with its parameters filled
/// from channels (ADR-0031 D1). A **layout** is a tree of containers over
/// panes. There is no sidebar type, no list type and no detail type — the
/// difference between them is entirely the value of a pane's spec, which is
/// why the same thirty verbs serve every app.
#[impress_service]
pub trait LayoutService: Send + Sync + 'static {
    // ----------------------------------------------------------- arrangement

    /// Split a pane, putting a new pane beside it. Focus follows the new pane.
    ///
    /// `direction` is `horizontal` (side by side) or `vertical` (stacked).
    /// `after` puts the new pane on the right / below; false puts it before.
    /// `new_pane` is a whole pane spec; omit it to duplicate the pane being
    /// split, which is what a bare "split this" gesture means.
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-split-list",
        args = r#"{"app_id":"g3-layout","device":"g3-split","target":{"role":"list"},"direction":"horizontal","after":true}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn split(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        direction: String,
        after: bool,
        new_pane: Option<PaneSpec>,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    /// Move a pane (or a whole subtree) next to another, or into its tabs.
    ///
    /// `placement` is `left` | `right` | `above` | `below` | `into-tabs`.
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-move-navigator",
        args = r#"{"app_id":"g3-layout","device":"g3-move-tile","tile":{"role":"navigator"},"target":{"role":"detail"},"placement":"right"}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn move_tile(
        &self,
        app_id: String,
        device: Option<String>,
        tile: PaneRefDto,
        target: PaneRefDto,
        placement: String,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    /// Close a pane or a whole subtree. Never the last pane of the layout.
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-close-navigator",
        args = r#"{"app_id":"g3-layout","device":"g3-close","target":{"role":"navigator"}}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn close(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    /// Exchange two tiles' positions, each keeping the share of the position
    /// it lands in.
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-swap-list-detail",
        args = r#"{"app_id":"g3-layout","device":"g3-swap","a":{"role":"list"},"b":{"role":"detail"}}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn swap(
        &self,
        app_id: String,
        device: Option<String>,
        a: PaneRefDto,
        b: PaneRefDto,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    /// Set a split container's relative shares — one per child, positive.
    ///
    /// They are weights, not fractions: `1 2 3` means the three-column
    /// chassis, and the renderer divides by their sum.
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-resize-columns",
        args = r#"{"app_id":"g3-layout","device":"g3-resize","container":4,"shares":[3.0,2.0,1.0]}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn resize(
        &self,
        app_id: String,
        device: Option<String>,
        container: u64,
        shares: Vec<f32>,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    /// Retype a container, keeping its children in order: `tabs` |
    /// `horizontal` | `vertical` | `grid`.
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-tabs-root",
        args = r#"{"app_id":"g3-layout","device":"g3-set-container-kind","container":4,"kind":"tabs"}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn set_container_kind(
        &self,
        app_id: String,
        device: Option<String>,
        container: u64,
        kind: String,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    /// Collapse a pane to no width in its split, or show it again at exactly
    /// the share it had — the ⌃⌘S gesture, as a verb (review RL-L13).
    /// `collapsed` omitted toggles. The pane stays in the tree with its
    /// session; only its share moves. Refused (`not-in-a-split`) for a pane
    /// whose parent is not a split.
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-collapse-detail",
        args = r#"{"app_id":"g3-layout","device":"g3-set-collapsed","target":{"role":"detail"},"collapsed":true}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn set_collapsed(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        collapsed: Option<bool>,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    /// Show one pane alone in its window. Zoom is a view state, not a mutation
    /// of the tree: every share and every session survives it.
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-zoom-detail",
        args = r#"{"app_id":"g3-layout","device":"g3-maximize","target":{"role":"detail"}}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn maximize(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    /// Undo a maximize. A no-op when nothing is maximized.
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-restore-zoom",
        args = r#"{"app_id":"g3-layout","device":"g3-restore"}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn restore(
        &self,
        app_id: String,
        device: Option<String>,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    /// Move a pane out into a new window whose root it becomes — the detached
    /// PDF. Refused when the pane is already the whole of its window.
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-detach-detail",
        args = r#"{"app_id":"g3-layout","device":"g3-detach","target":{"role":"detail"}}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn detach(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    // --------------------------------------------------------------- content

    /// Replace a pane's whole spec: query, view kind, parameters, channel,
    /// role and view state at once.
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-replace-list",
        args = r#"{"app_id":"g3-layout","device":"g3-set-pane","target":{"role":"list"},"spec":{"query":{"kinds":["figure"]},"view_kind":"plot","role":"preview","channel":{"number":3}}}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn set_pane(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        spec: PaneSpec,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    /// Point a pane at a different query. The query algebra is closed
    /// (ADR-0031 D2): kinds, a scope, filters, a text term, one relation walk,
    /// sort and limit. Anything it cannot express is materialized in the store
    /// first and queried from there.
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-query-manuscripts",
        args = r#"{"app_id":"g3-layout","device":"g3-set-query","target":{"role":"list"},"query":{"kinds":["manuscript"],"text":"dark matter"}}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn set_query(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        query: PaneQuery,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    /// Re-render a pane with a different view kind: `outline`, `list`,
    /// `info`, `pdf`, `notes`, `bibtex`, `source`, `plot`, `console`,
    /// `surface`, `legacy`, `placeholder` — `impress_layout::ViewKindId::KNOWN`,
    /// the whole vocabulary. Anything else is refused (`unknown-view-kind`).
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-render-pdf",
        args = r#"{"app_id":"g3-layout","device":"g3-set-view-kind","target":{"role":"detail"},"view_kind":"pdf"}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn set_view_kind(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        view_kind: String,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    /// Re-point one of a pane's declared parameters: follow a channel, pin it
    /// to one item, or fall back to the view kind's default.
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-pin-detail-item",
        args = r#"{"app_id":"g3-layout","device":"g3-bind-param","target":{"role":"detail"},"name":"item","source":{"source":"fixed","item":"5a000000-0000-4000-8000-000000000001"}}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn bind_param(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        name: String,
        source: ParamSource,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    /// Change the channel a pane publishes its selection on: `1`–`8`, or
    /// `follow` for the window's default.
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-publish-on-two",
        args = r#"{"app_id":"g3-layout","device":"g3-set-channel","target":{"role":"list"},"channel":"2"}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn set_channel(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        channel: String,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    /// Set what `follow` means in one window. `follow` itself is refused: a
    /// window default that follows itself is not a value.
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-follow-four",
        args = r#"{"app_id":"g3-layout","device":"g3-set-default-channel","channel":"4"}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn set_default_channel(
        &self,
        app_id: String,
        device: Option<String>,
        window: Option<u64>,
        channel: String,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    /// Give, move or clear a role — `navigator`, `list`, `detail`, `preview`,
    /// `console`, or one of your own. The universal chords act on whichever
    /// pane holds the role, so this is how ⌃⌘S is re-aimed.
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-reassign-navigator",
        args = r#"{"app_id":"g3-layout","device":"g3-set-role","target":{"role":"navigator"},"role":"console"}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn set_role(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        role: Option<String>,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    // ------------------------------------------------------- focus/selection

    /// Focus a pane. Focus is a value in the layout, so it is legible to
    /// agents and tests rather than living in a view.
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-focus-navigator",
        args = r#"{"app_id":"g3-layout","device":"g3-focus","target":{"role":"navigator"}}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn focus(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    /// Step focus: `left` | `right` | `up` | `down` | `next` | `prev`. This is
    /// the h / l grammar, as a tree walk rather than a geometric one.
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-focus-next",
        args = r#"{"app_id":"g3-layout","device":"g3-focus-direction","direction":"next"}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn focus_direction(
        &self,
        app_id: String,
        device: Option<String>,
        direction: String,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    /// Publish a selection of `kind` on a pane's channel — what "clicking a
    /// row" is.
    ///
    /// A channel carries one current value PER RECORD KIND, so publishing a
    /// manuscript leaves a publication parameter on the same channel
    /// untouched. An empty `ids` is a real value: it records that nothing of
    /// that kind is selected, which is what a detail pane renders its empty
    /// state from.
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-publish-paper",
        args = r#"{"app_id":"g3-layout","device":"g3-select","target":{"role":"list"},"kind":"publication","ids":["5a000000-0000-4000-8000-000000000003"]}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn select(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        kind: String,
        ids: Vec<String>,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    /// Replace (or clear, with null) a window's frame. Device-scoped: it
    /// persists locally and is filtered out before a layout is applied
    /// somewhere else.
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-set-frame",
        args = r#"{"app_id":"g3-layout","device":"g3-set-window-geometry","geometry":{"x":12.0,"y":34.0,"w":1440.0,"h":900.0}}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn set_window_geometry(
        &self,
        app_id: String,
        device: Option<String>,
        window: Option<u64>,
        geometry: Option<Geometry>,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    // ----------------------------------------------------------- persistence

    /// Materialize the current arrangement as a durable, attributed record —
    /// the commit of ADR-0031 D7, which undo never crosses.
    ///
    /// `as_kind` is `layout` today, which saves the tree under `name`.
    /// Committing a set of bindings as a **figure** or a **collection** is the
    /// same verb with a different `as_kind` and arrives with the implore and
    /// preset work (L7); asking for one now is refused rather than
    /// approximated.
    #[impress_method(replay = full, safety = destructive, effects(reads = ["impress/ui/layout@1.0.0", "impress/ui/preset@1.0.0", "impress/ui/surface@1.0.0"], writes = ["impress/ui/layout@1.0.0", "impress/ui/preset@1.0.0"]))]
    #[impress_example(
        name = "g3-commit-layout",
        args = r#"{"app_id":"g3-layout","device":"g3-commit","as_kind":"layout","name":"G3 Commit","purpose":"G3 example arrangement"}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn commit(
        &self,
        app_id: String,
        device: Option<String>,
        as_kind: String,
        name: String,
        purpose: Option<String>,
        actor: Option<String>,
    ) -> LayoutVerbResult;

    /// Save the current arrangement under a name, durably. Re-saving an
    /// existing name overwrites it.
    #[impress_method(replay = full, safety = destructive)]
    #[impress_example(
        name = "g3-save-named",
        args = r#"{"app_id":"g3-layout","device":"g3-save-layout","name":"G3 Saved","purpose":"G3 saved arrangement"}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn save_layout(
        &self,
        app_id: String,
        device: Option<String>,
        name: String,
        purpose: Option<String>,
        actor: Option<String>,
    ) -> LayoutVerbResult;

    /// Recall a saved layout by `name` (or id), or by `ordinal` 1–9 — the
    /// ⌃⌘1–9 chords.
    ///
    /// An ordinal numbers ONE union: this app's **presets** first, in the
    /// shipped table's order, then its named layouts, oldest first. So ⌃⌘1 is
    /// always the app's default arrangement and ⌃⌘2 its Triage, whatever the
    /// user has saved since. A `name` that is a preset rather than a layout
    /// applies that preset, for the same reason: a palette types into one
    /// name space.
    ///
    /// Window geometry is dropped on the way in: the logical tree is what
    /// ports between devices, and a 27" frame has no business landing on a
    /// laptop (ADR-0019 D2).
    #[impress_method(replay = full, safety = destructive)]
    #[impress_example(
        name = "g3-recall-saved",
        args = r#"{"app_id":"g3-layout","device":"g3-apply-layout","name":"G3 Apply"}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn apply_layout(
        &self,
        app_id: String,
        device: Option<String>,
        name: Option<String>,
        ordinal: Option<u32>,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    /// Remove a saved layout by name or id. Refuses the live arrangement (it
    /// is not a saved layout) and any preset (`reset-preset` is how a preset
    /// goes back to shipped); deleting a name that does not exist is `ok:
    /// false` with a message, never an error.
    #[impress_method(replay = full, safety = destructive, effects(reads = ["impress/ui/layout@1.0.0", "impress/ui/preset@1.0.0"], writes = ["impress/ui/layout@1.0.0"]))]
    #[impress_example(
        name = "g3-delete-saved",
        args = r#"{"app_id":"g3-layout","name_or_id":"G3 Delete"}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn delete_layout(
        &self,
        app_id: String,
        name_or_id: String,
        actor: Option<String>,
    ) -> LayoutVerbResult;

    /// Undo on one ring: `arrangement` (the window's shape) or `exploration`
    /// (one pane's bindings and view state, named by `target`).
    ///
    /// These are two of the three stacks of ADR-0031 D7. The third — the
    /// editor session's own undo manager — is never ours and is not reachable
    /// from here.
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-undo-split",
        args = r#"{"app_id":"g3-layout","device":"g3-undo","stack":"arrangement"}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn undo(
        &self,
        app_id: String,
        device: Option<String>,
        stack: String,
        target: Option<PaneRefDto>,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    /// Redo on one ring. Same stacks as `undo`.
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-redo-arrangement",
        args = r#"{"app_id":"g3-layout","device":"g3-redo","stack":"arrangement"}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn redo(
        &self,
        app_id: String,
        device: Option<String>,
        stack: String,
        target: Option<PaneRefDto>,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    // ------------------------------------------------------------------ read

    /// The whole live tree for this scope: windows, the tile arena, channel
    /// state. Creates the three-column preset on a cold start, so a caller
    /// never has to ask whether a layout exists.
    // Mutating, not read-only: on a device with no live row this writes the
    // default layout (the store spy observed it, plan self-reflective-layer
    // E1); idempotent thereafter.
    #[impress_method(replay = full, safety = mutating, idempotent = true, effects(reads = ["impress/ui/layout@1.0.0", "impress/ui/preset@1.0.0"], writes = ["impress/ui/layout@1.0.0"]))]
    #[impress_example(
        name = "default",
        args = r#"{"app_id": "impress", "device": "effects-device"}"#
    )]
    #[impress_example(
        name = "g3-read-owned-layout",
        args = r#"{"app_id":"g3-layout","device":"g3-get-layout"}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn get_layout(&self, app_id: String, device: Option<String>) -> LayoutResult;

    /// One pane: its spec, its query COMPILED against the record-kind manifest
    /// with its parameters bound, and what those parameters currently resolve
    /// to.
    ///
    /// The compiled `item_query` is literally what the store will be asked —
    /// which is how "why is this pane empty?" becomes a question with an
    /// answer instead of a debugging session.
    #[impress_method(replay = full, safety = read_only, effects(reads = ["impress/ui/layout@1.0.0", any("compiles the pane's query over the kind it names")]))]
    #[impress_example(
        name = "g3-read-detail",
        args = r#"{"app_id":"g3-layout","device":"g3-get-pane","target":{"role":"detail"}}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn get_pane(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
    ) -> PaneResult;

    /// What a channel currently carries, per record kind, and which panes it
    /// drives. `channel` is `1`–`8` or `follow`.
    #[impress_method(replay = full, safety = read_only, effects(reads = ["impress/ui/layout@1.0.0"]))]
    #[impress_example(
        name = "g3-read-channel",
        args = r#"{"app_id":"g3-layout","device":"g3-get-channel","channel":"1"}"#,
        expect = r#"{"ok":true,"channel":1}"#
    )]
    async fn get_channel(
        &self,
        app_id: String,
        device: Option<String>,
        channel: String,
        kind: Option<String>,
    ) -> ChannelResult;

    /// What a pane reference resolves to right now — "which pane is `right`?"
    /// answered without doing anything to it.
    #[impress_method(replay = full, safety = read_only, effects(reads = ["impress/ui/layout@1.0.0"]))]
    #[impress_example(
        name = "g3-resolve-detail",
        args = r#"{"app_id":"g3-layout","device":"g3-resolve-reference","target":{"role":"detail"}}"#,
        expect = r#"{"ok":true,"role":"detail"}"#
    )]
    async fn resolve_reference(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
    ) -> ReferenceResult;

    /// The saved layouts of an app, in ⌃⌘1–9 order.
    ///
    /// Their ordinals are OFFSET by the app's presets, which come first in
    /// the union `apply_layout(ordinal:)` recalls — see `list_presets`.
    // Mutating, not read-only: the first call on a store seeds the shipped
    // presets (the store spy observed the write, plan self-reflective-layer
    // E1); idempotent thereafter.
    #[impress_method(replay = full, safety = mutating, idempotent = true, effects(reads = ["impress/ui/layout@1.0.0", "impress/ui/preset@1.0.0"], writes = ["impress/ui/preset@1.0.0"]))]
    #[impress_example(name = "default", args = r#"{"app_id": "impress"}"#)]
    #[impress_example(
        name = "g3-list-owned-layout",
        args = r#"{"app_id":"g3-layout"}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn list_layouts(&self, app_id: String) -> LayoutListResult;

    // --------------------------------------------------------------- presets

    /// The presets of an app — what the app IS, as arrangements the user can
    /// recall (ADR-0031 D10): its default, plus Triage / Reading / Full /
    /// Writing where the table ships them, plus any the user saved.
    ///
    /// Seeds the shipped presets if this workspace has never seen them, so a
    /// caller never has to ask whether they exist. Ordinals are the ⌃⌘1–9
    /// chords: presets first, then named layouts. The answer also carries the
    /// sections this app permits that are NOT expressible as queries, with
    /// the reason (ADR-0031 D2).
    #[impress_method(replay = full, safety = read_only, effects(reads = ["impress/ui/layout@1.0.0", "impress/ui/preset@1.0.0"]))]
    #[impress_example(name = "default", args = r#"{"app_id": "impress"}"#)]
    #[impress_example(
        name = "g3-list-owned-preset",
        args = r#"{"app_id":"g3-layout"}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn list_presets(&self, app_id: String) -> PresetListResult;

    /// Apply a preset: the live arrangement becomes its tree, and the live
    /// row records a `DerivedFrom` edge to the preset it came from — which is
    /// what makes "reset to the preset" a graph walk rather than a remembered
    /// string.
    ///
    /// Window geometry is dropped on the way in, exactly as `apply_layout`
    /// drops it: the logical tree is what ports between devices (ADR-0019
    /// D2).
    #[impress_method(replay = full, safety = destructive)]
    #[impress_example(
        name = "g3-recall-preset",
        args = r#"{"app_id":"g3-layout","device":"g3-apply-preset","name":"G3 Apply Preset"}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn apply_preset(
        &self,
        app_id: String,
        device: Option<String>,
        name: String,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult;

    /// Save the live arrangement AS a preset, durably — a preset of the
    /// user's own, or an edit to one the suite ships.
    ///
    /// `from_live` must be true today: the live tree is the only source a
    /// preset can be built from here, and asking for another is refused
    /// rather than approximated. Editing a shipped preset leaves its
    /// `version` alone, so "the user edited Triage" stays distinguishable
    /// from "we shipped a newer Triage" and `reset_preset` can undo it.
    #[impress_method(replay = full, safety = destructive, effects(reads = ["impress/ui/layout@1.0.0", "impress/ui/preset@1.0.0"], writes = ["impress/ui/preset@1.0.0", "impress/ui/layout@1.0.0"]))]
    #[impress_example(
        name = "g3-save-owned-preset",
        args = r#"{"app_id":"g3-layout","device":"g3-save-preset","name":"G3 Saved Preset","purpose":"G3 reading arrangement","from_live":true}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn save_preset(
        &self,
        app_id: String,
        device: Option<String>,
        name: String,
        purpose: Option<String>,
        from_live: bool,
        actor: Option<String>,
    ) -> PresetResult;

    /// Restore the shipped revision of a preset over a user-edited row.
    ///
    /// Refused for a name the suite does not ship: there would be nothing to
    /// restore it to, and the refusal names what IS shipped.
    #[impress_method(replay = full, safety = destructive, effects(reads = ["impress/ui/layout@1.0.0", "impress/ui/preset@1.0.0"], writes = ["impress/ui/layout@1.0.0", "impress/ui/preset@1.0.0"]))]
    #[impress_example(
        name = "g3-reset-shipped",
        args = r#"{"app_id":"imbib","name":"Triage"}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn reset_preset(
        &self,
        app_id: String,
        name: String,
        actor: Option<String>,
    ) -> PresetResult;
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

/// Store-backed `LayoutService`.
///
/// `new()` uses the process-wide store and the process-wide session registry —
/// the same singletons every other store-generic service uses, so a
/// `--store-path` parsed by `impress-cli` applies here too. `with_store` takes
/// an explicit store AND gives the instance its own session registry, which is
/// what makes the tests hermetic: no test can see another's panes.
/// Run [`crate::rename::RenamePass`] against `store` once per process
/// (plan-verb-pipeline-and-transport.md P3b: "at store open or at service
/// start, once per rename-table version"). The shipped table
/// ([`impress_service_core::lifecycle::SHIPPED_RENAMES`]) is empty, so this
/// costs one branch and touches the store not at all until a real rename is
/// appended; the `store_metadata` marker the pass itself records is what
/// then keeps a later launch from redoing work an earlier one already did.
fn ensure_renamed(store: &Arc<dyn AttributedStore>) {
    use impress_service_core::lifecycle::SHIPPED_RENAMES;
    if SHIPPED_RENAMES.is_empty() {
        return;
    }
    static RAN: OnceLock<()> = OnceLock::new();
    RAN.get_or_init(|| {
        if let Err(e) =
            crate::rename::RenamePass::new(store.clone()).run_if_needed(&SHIPPED_RENAMES)
        {
            tracing::error!(target: "layout", "rename pass: {e}");
        }
    });
}

#[derive(Clone)]
pub struct DefaultLayoutService {
    store: Option<Arc<dyn AttributedStore>>,
    sessions: Option<Arc<SessionRegistry>>,
    /// The record kinds this service knows: `check_verb` refuses a query
    /// naming one outside it, and `compiled_pane` compiles against it. Passed
    /// at construction, never reached for.
    manifest: Arc<KindManifest>,
    /// How `Scope::CollectionSubtree` finds a folder's descendants when a
    /// pane is compiled: the host's resolver ([`Self::with_subtree_resolver`]),
    /// or [`RootOnly`] — the folder's own members. [`Self::new`] takes the
    /// process's (`koine_store::process::subtree_resolver`).
    subtrees: Arc<dyn SubtreeResolver + Send + Sync>,
    /// Put the tree in every verb result that has one
    /// ([`LayoutVerbResult::tree_json`]). Off unless asked for.
    tree_in_results: bool,
}

impl Default for DefaultLayoutService {
    fn default() -> Self {
        Self::new()
    }
}

impl DefaultLayoutService {
    /// The process-wide store and session registry, with the process's kind
    /// manifest ([`process_manifest::current`]) and subtree resolver
    /// ([`koine_store::process::subtree_resolver`]). What the
    /// `#[impress_service]` inventory instantiates.
    pub fn new() -> Self {
        Self {
            store: None,
            sessions: None,
            manifest: process_manifest::current(),
            subtrees: koine_store::process::subtree_resolver(),
            tree_in_results: false,
        }
    }

    /// An instance over an explicit store, with a private session registry
    /// and the record kinds in `manifest`.
    pub fn with_store(
        store: Arc<dyn AttributedStore>,
        manifest: impl Into<Arc<KindManifest>>,
    ) -> Self {
        Self::with_store_and_sessions(store, Arc::new(SessionRegistry::new()), manifest)
    }

    /// This instance, with the record kinds in `manifest` instead of the
    /// ones it was built with — for [`Self::new`], which owns the process
    /// store and has no manifest argument.
    pub fn with_manifest(mut self, manifest: impl Into<Arc<KindManifest>>) -> Self {
        self.manifest = manifest.into();
        self
    }

    /// This instance, with `resolver` finding a folder's descendants when a
    /// pane's `Scope::CollectionSubtree` compiles. An instance over an
    /// explicit store resolves nothing but the folder itself until it is
    /// given one: the resolver is the host's, and it reads the host's store.
    pub fn with_subtree_resolver(
        mut self,
        resolver: Arc<dyn SubtreeResolver + Send + Sync>,
    ) -> Self {
        self.subtrees = resolver;
        self
    }

    /// The record kinds this instance checks and compiles against.
    pub fn manifest(&self) -> &Arc<KindManifest> {
        &self.manifest
    }

    /// An instance over an explicit store AND an explicit registry — for the
    /// several objects one process opens on one store (the FFI's
    /// `SharedLayout` and the surface executor) to share their sessions, so
    /// a verb one of them applies is the tree the other one reads.
    pub fn with_store_and_sessions(
        store: Arc<dyn AttributedStore>,
        sessions: Arc<SessionRegistry>,
        manifest: impl Into<Arc<KindManifest>>,
    ) -> Self {
        Self {
            store: Some(store),
            sessions: Some(sessions),
            manifest: manifest.into(),
            subtrees: Arc::new(RootOnly),
            tree_in_results: false,
        }
    }

    /// Answer every verb that leaves a tree with that tree, serialized once
    /// under the lock the verb already holds ([`LayoutVerbResult::tree_json`],
    /// review RL-L14) — what the FFI renders from. Costs a serialization per
    /// verb, so only the host that draws the tree asks for it.
    pub fn with_tree_in_results(mut self) -> Self {
        self.tree_in_results = true;
        self
    }

    /// The tree for [`LayoutVerbResult::tree_json`], when this instance was
    /// asked for it. A tree that does not serialize is left out, and the
    /// reader falls back to reading it (and reports why).
    fn tree_for_result(&self, layout: &impress_layout::Layout) -> Option<String> {
        if !self.tree_in_results {
            return None;
        }
        serde_json::to_string(layout)
            .map_err(|e| tracing::error!(target: "layout", "the tree does not encode: {e}"))
            .ok()
    }

    /// [`Self::with_session_for_write`] for a verb that answers with a
    /// [`LayoutVerbResult`]: the tree it leaves goes in the result.
    fn verb_for_write(
        &self,
        app_id: &str,
        device: Option<String>,
        actor: ActorKind,
        f: impl FnOnce(&mut LayoutSession, &LayoutStore) -> Result<LayoutVerbResult, Refusal>,
    ) -> Result<LayoutVerbResult, Refusal> {
        self.with_session_for_write(app_id, device, actor, |session, store| {
            let mut result = f(session, store)?;
            result.tree_json = self.tree_for_result(&session.layout);
            Ok(result)
        })
    }

    /// The registry this instance's sessions live in.
    pub fn sessions(&self) -> Arc<SessionRegistry> {
        self.registry()
    }

    /// Drop the cached session for one scope, so the next read re-reads the
    /// stored row.
    ///
    /// The registry keeps a `LayoutSession` per (app, device) and only loads
    /// from the store when there is none — which is right for this process's
    /// own verbs and wrong the moment ANOTHER process writes the same row
    /// (ADR-0033 D6). The FFI's invalidation feed calls this when its external
    /// `data_version` poll sees a layout write it did not make; without it the
    /// host reloads, the service answers from the stale session, and the
    /// window redraws exactly what it already had — verified live on
    /// 2026-09-22, where `surface_show` from `impress-mcp` reached the store
    /// and the window kept rendering four leaves.
    pub fn forget_session(&self, app_id: &str, device: Option<&str>) {
        self.registry().forget(app_id, &resolve_device(device));
    }

    /// The store this instance runs against: the explicit one it was built
    /// with, or the process-wide singleton. Runs the rename pass
    /// ([`crate::rename::RenamePass`]) against it once per process — see
    /// [`ensure_renamed`] — so every path below that resolves a store this
    /// way has it already rewritten before it reads or writes a row.
    fn resolved_store(&self) -> Arc<dyn AttributedStore> {
        self.resolved_store_checked().0
    }

    /// [`Self::resolved_store`], and whether it is the in-memory stand-in the
    /// store service substitutes for a store it could not open. The question
    /// is asked of the concrete handle the service returned, before it is
    /// widened to the port: a stand-in is identified by that handle.
    fn resolved_store_checked(&self) -> (Arc<dyn AttributedStore>, bool) {
        let (store, fallback): (Arc<dyn AttributedStore>, bool) = match &self.store {
            Some(store) => (store.clone(), false),
            None => koine_store::process::acquire_or_stand_in(),
        };
        ensure_renamed(&store);
        (store, fallback)
    }

    fn layout_store(&self) -> LayoutStore {
        LayoutStore::new(self.resolved_store())
    }

    /// The store for a call that writes, acquired once: refused with
    /// `store-unavailable` when it is the in-memory stand-in the store
    /// service substitutes for a store it could not open, because a write
    /// there answers `ok` and vanishes (review AC-F20).
    fn layout_store_for_write(&self) -> Result<LayoutStore, Refusal> {
        let (store, fallback) = self.resolved_store_checked();
        if fallback {
            return Err(Refusal::store_unavailable(format!(
                "the store{} could not be opened, so this write would land in a temporary \
                 in-memory stand-in and vanish; nothing was written",
                match koine_store::process::intended_location() {
                    Some(at) => format!(" at {at}"),
                    None => String::new(),
                }
            )));
        }
        Ok(LayoutStore::new(store))
    }

    /// `Some("fallback")` when this process is reading the in-memory
    /// stand-in for a store that could not be opened: a read answered from
    /// it is not the user's data and says so (review AC-F20).
    fn store_marker(&self) -> Option<String> {
        let (_, fallback) = self.resolved_store_checked();
        fallback.then(|| "fallback".to_string())
    }

    /// A read's message, with the fallback warning in front when it applies.
    fn read_message(&self, marker: &Option<String>, message: String) -> String {
        match marker {
            Some(_) => format!(
                "FALLBACK STORE: the store{} could not be opened, so this answer comes from \
                 an in-memory stand-in, not the user's layout. {message}",
                match koine_store::process::intended_location() {
                    Some(at) => format!(" at {at}"),
                    None => String::new(),
                }
            ),
            None => message,
        }
    }

    fn registry(&self) -> Arc<SessionRegistry> {
        self.sessions
            .clone()
            .unwrap_or_else(SessionRegistry::shared)
    }

    /// Apply one verb and persist the live row.
    ///
    /// `build` gets the session because some verbs need it to fill in a
    /// default the caller left out — "this window" for the geometry and
    /// default-channel verbs, "the pane I am splitting" for a bare split.
    ///
    /// The save is compare-and-swap (see `session`'s module docs). When it
    /// loses the race — another writer committed between this session's
    /// revision check and its write — the session reloads and the SAME verb
    /// is applied once more against the reloaded tree, which is what the
    /// caller asked for: a reference like "the focused pane" means the pane
    /// focused now. A second loss is reported, not retried.
    fn apply_verb(
        &self,
        app_id: &str,
        device: Option<String>,
        actor: Option<String>,
        expected_revision: Option<u64>,
        build: impl FnOnce(&LayoutSession) -> Result<Verb, Refusal>,
    ) -> LayoutVerbResult {
        self.apply_verb_as(
            app_id,
            device,
            actor_from(actor.as_deref()),
            expected_revision,
            build,
        )
    }

    /// [`Self::apply_verb`] with the actor already parsed — what the FFI's
    /// batch and the verbs a surface effect composes call.
    pub fn apply_verb_as(
        &self,
        app_id: &str,
        device: Option<String>,
        actor_kind: ActorKind,
        expected_revision: Option<u64>,
        build: impl FnOnce(&LayoutSession) -> Result<Verb, Refusal>,
    ) -> LayoutVerbResult {
        let device = resolve_device(device.as_deref());
        let store = match self.layout_store_for_write() {
            Ok(store) => store,
            Err(refusal) => return refused_verb(app_id, &device, actor_kind, None, refusal),
        };
        let registry = self.registry();
        let mut build = Some(build);
        let mut built: Option<Verb> = None;
        let mut notices: Vec<String> = Vec::new();
        for _attempt in 0..2 {
            let outcome = registry.with(&store, app_id, &device, actor_kind, |session| {
                if let Some(notice) = session.take_notice() {
                    notices.push(notice);
                }
                // Checked after the registry's own reload, so "the row moved
                // since you read it" is caught whether this process or
                // another moved it — and on the retry below, a write that
                // won the race is a conflict for a caller that named the
                // revision it expected.
                check_expected(session, expected_revision)?;
                let verb = match (&built, build.take()) {
                    (Some(verb), _) => verb.clone(),
                    (None, Some(build)) => {
                        let verb = supply_open_list_fallback(build(session)?);
                        built = Some(verb.clone());
                        check_verb(&verb, &self.manifest)
                            .map_err(|r| r.context(verb_label(&verb)))?;
                        verb
                    }
                    (None, None) => return Err(Refusal::internal("the verb could not be rebuilt")),
                };
                let intent = intent_text(&verb);
                let applied = session
                    .apply(verb.clone())
                    .map_err(|e| layout_refusal(e).context(verb_label(&verb)))?;
                if applied.patch.is_empty() {
                    // Nothing changed (focus on the focused pane, restore
                    // with nothing maximized): nothing to write, and no
                    // write for every other reader of the row to wake on.
                    let tree = self.tree_for_result(&session.layout);
                    return Ok(Some((intent, applied, session.revision, tree)));
                }
                match session.save(&store, actor_kind, &intent) {
                    Ok(()) => {
                        let tree = self.tree_for_result(&session.layout);
                        Ok(Some((intent, applied, session.revision, tree)))
                    }
                    // Lost the race: the session is stale and reloads on
                    // the next touch — the retry below.
                    Err(_) if session.is_stale() => Ok(None),
                    Err(e) => Err(e),
                }
            });
            match flatten(outcome) {
                Ok(Some((intent, applied, revision, tree))) => {
                    tracing::info!(
                        target: "layout",
                        "{app_id}/{device}: {} {intent} (revision {revision:?}, {} pane(s) affected{})",
                        actor_name(actor_kind),
                        applied.affected.len(),
                        if applied.patch.is_empty() { ", nothing changed" } else { "" }
                    );
                    let mut result =
                        LayoutVerbResult::applied(intent, &applied).with_revision(revision);
                    result.tree_json = tree;
                    return with_notices(result, &notices);
                }
                Ok(None) => continue,
                Err(refusal) => {
                    return with_notices(
                        refused_verb(app_id, &device, actor_kind, built.as_ref(), refusal),
                        &notices,
                    )
                }
            }
        }
        refused_verb(
            app_id,
            &device,
            actor_kind,
            built.as_ref(),
            Refusal::conflict(crate::session::STALE),
        )
    }

    /// Apply several verbs as ONE gesture: all or none, one undo step
    /// (review PH-M2). What an outline click is — focus the navigator,
    /// publish the row, re-point the list — so one ⌘Z takes the click back
    /// and a refusal of any verb applies none of them. See
    /// `impress_layout::UndoStacks::apply_all` for which ring the step lands
    /// on.
    pub fn apply_verbs_as(
        &self,
        app_id: &str,
        device: Option<String>,
        actor_kind: ActorKind,
        expected_revision: Option<u64>,
        verbs: Vec<Verb>,
    ) -> LayoutVerbResult {
        let device = resolve_device(device.as_deref());
        if verbs.is_empty() {
            return LayoutVerbResult::refused(Refusal::invalid_argument(
                "apply_verbs needs at least one verb",
            ));
        }
        let verbs: Vec<Verb> = verbs.into_iter().map(supply_open_list_fallback).collect();
        for verb in &verbs {
            if let Err(refusal) = check_verb(verb, &self.manifest) {
                return refused_verb(
                    app_id,
                    &device,
                    actor_kind,
                    Some(verb),
                    refusal.context(verb_label(verb)),
                );
            }
        }
        let store = match self.layout_store_for_write() {
            Ok(store) => store,
            Err(refusal) => return refused_verb(app_id, &device, actor_kind, None, refusal),
        };
        let label = verbs.iter().map(verb_label).collect::<Vec<_>>().join(", ");
        let registry = self.registry();
        let mut notices: Vec<String> = Vec::new();
        for _attempt in 0..2 {
            let verbs = verbs.clone();
            let outcome = registry.with(&store, app_id, &device, actor_kind, |session| {
                if let Some(notice) = session.take_notice() {
                    notices.push(notice);
                }
                check_expected(session, expected_revision)?;
                let intent = format!(
                    "{} (one gesture)",
                    verbs.iter().map(intent_text).collect::<Vec<_>>().join("; ")
                );
                let applied = session
                    .apply_all(verbs)
                    .map_err(|e| layout_refusal(e).context(&label))?;
                if applied.patch.is_empty() {
                    let tree = self.tree_for_result(&session.layout);
                    return Ok(Some((intent, applied, session.revision, tree)));
                }
                match session.save(&store, actor_kind, &intent) {
                    Ok(()) => {
                        let tree = self.tree_for_result(&session.layout);
                        Ok(Some((intent, applied, session.revision, tree)))
                    }
                    Err(_) if session.is_stale() => Ok(None),
                    Err(e) => Err(e),
                }
            });
            match flatten(outcome) {
                Ok(Some((intent, applied, revision, tree))) => {
                    tracing::info!(
                        target: "layout",
                        "{app_id}/{device}: {} {intent} (revision {revision:?}, {} pane(s) affected)",
                        actor_name(actor_kind),
                        applied.affected.len(),
                    );
                    let mut result =
                        LayoutVerbResult::applied(intent, &applied).with_revision(revision);
                    result.tree_json = tree;
                    return with_notices(result, &notices);
                }
                Ok(None) => continue,
                Err(refusal) => {
                    tracing::warn!(
                        target: "layout",
                        "{app_id}/{device}: {} [{label}] refused as one gesture, nothing applied \
                         [{}]: {}",
                        actor_name(actor_kind),
                        refusal.code,
                        refusal.message
                    );
                    return with_notices(LayoutVerbResult::refused(refusal), &notices);
                }
            }
        }
        LayoutVerbResult::refused(Refusal::conflict(crate::session::STALE))
    }

    /// Run `f` against the session without applying a verb — the reads, and
    /// the persistence verbs that replace the tree wholesale.
    ///
    /// `actor` is carried because a read that finds no live row **writes one**
    /// (the cold start), and that row is the user's workspace: attributing it
    /// to `System` would claim the chassis arranged it, and attributing an
    /// agent's read to the human would be worse.
    fn with_session<R>(
        &self,
        app_id: &str,
        device: Option<String>,
        actor: ActorKind,
        f: impl FnOnce(&mut LayoutSession, &LayoutStore) -> Result<R, Refusal>,
    ) -> Result<R, Refusal> {
        let device = resolve_device(device.as_deref());
        let store = self.layout_store();
        flatten(
            self.registry()
                .with(&store, app_id, &device, actor, |session| f(session, &store)),
        )
    }

    /// [`Self::with_session`] for a call that writes (a save, a preset, a
    /// reset): refused with `store-unavailable` when this process is on the
    /// in-memory stand-in for the real store (review AC-F20).
    fn with_session_for_write<R>(
        &self,
        app_id: &str,
        device: Option<String>,
        actor: ActorKind,
        f: impl FnOnce(&mut LayoutSession, &LayoutStore) -> Result<R, Refusal>,
    ) -> Result<R, Refusal> {
        let device = resolve_device(device.as_deref());
        let store = self.layout_store_for_write()?;
        flatten(
            self.registry()
                .with(&store, app_id, &device, actor, |session| f(session, &store)),
        )
    }
}

/// One pane, resolved and compiled: what `get_pane` answers, and what the
/// FFI renders from without a JSON round trip (review RL-L14).
#[derive(Debug, Clone)]
pub struct CompiledPane {
    pub tile: TileId,
    pub spec: PaneSpec,
    pub bindings: Bindings,
    /// The channel the pane publishes on, resolved against its window.
    pub channel: Option<u8>,
    /// The focused leaf of the window the reference resolved in.
    pub focused: Option<TileId>,
    /// The compiled query, or the typed refusal — never an empty query that
    /// reads as "no data yet".
    pub compiled: Result<CompiledQuery, PaneQueryError>,
}

impl DefaultLayoutService {
    /// The whole tree, read as `actor` — the GUI passes `Human`, because a
    /// read that finds no live row cold-starts one, and that row is the
    /// user's workspace (review RL-L15). The trait method, which MCP and the
    /// CLI call, keeps its `Agent` default.
    pub fn get_layout_as(
        &self,
        app_id: &str,
        device: Option<String>,
        actor: ActorKind,
    ) -> LayoutResult {
        let marker = self.store_marker();
        let outcome = self.with_session(app_id, device, actor, |session, _| {
            let window = session.layout.current_window().ok();
            Ok(LayoutResult {
                ok: true,
                wire_version: WIRE_VERSION,
                code: None,
                message: self.read_message(
                    &marker,
                    format!(
                        "{} window(s), {} pane(s), revision {}.",
                        session.layout.windows.len(),
                        session.layout.panes().len(),
                        session
                            .revision
                            .map(|r| r.to_string())
                            .unwrap_or_else(|| "unknown".to_string())
                    ),
                ),
                revision: session.revision,
                store: marker.clone(),
                focused: window.and_then(|w| session.focused(w)).map(TileId::raw),
                affected_panes: raw_tiles(&session.layout.panes()),
                window: window.map(WindowId::raw),
                item_id: Some(session.item_id.to_string()),
                device: Some(session.device.clone()),
                layout: Some(session.layout.clone()),
            })
        });
        outcome.unwrap_or_else(LayoutResult::refused)
    }

    /// Resolve and compile one pane, as `actor` (see [`Self::get_layout_as`]).
    ///
    /// Only the resolution and a clone of the spec happen under the session
    /// registry's lock; the compile — and the collection-tree read it needs
    /// only for a `collection-subtree` scope, done lazily — happen outside
    /// it. Every display pass used to run four collection-tree scans inside
    /// the process-wide lock every verb and the surface executor also take
    /// (review RL-L14).
    pub fn compiled_pane(
        &self,
        app_id: &str,
        device: Option<String>,
        reference: &impress_layout::PaneRef,
        actor: ActorKind,
    ) -> Result<CompiledPane, Refusal> {
        let (tile, spec, bindings, channel, focused) =
            self.with_session(app_id, device, actor, |session, _| {
                let window = session.layout.current_window().map_err(layout_refusal)?;
                let tile = session
                    .layout
                    .resolve(window, reference)
                    .map_err(layout_refusal)?;
                let spec = session
                    .layout
                    .pane(tile)
                    .ok_or_else(|| layout_refusal(LayoutError::NotAPane { tile }))?
                    .clone();
                Ok((
                    tile,
                    spec,
                    session.layout.bindings_for(tile),
                    session.channel_of(tile),
                    session.focused(window),
                ))
            })?;
        let decls: Vec<ParamDecl> = spec.params.iter().map(|b| b.decl.clone()).collect();
        let compiled = compile_with(
            &spec.query,
            &decls,
            &bindings,
            &self.manifest,
            &*self.subtrees,
        );
        Ok(CompiledPane {
            tile,
            spec,
            bindings,
            channel,
            focused,
            compiled,
        })
    }
}

/// Refuse with `conflict` unless the session is at the revision the caller
/// read (review RL-L1's wire half). `None` expects nothing.
fn check_expected(session: &LayoutSession, expected: Option<u64>) -> Result<(), Refusal> {
    let Some(expected) = expected else {
        return Ok(());
    };
    if session.revision == Some(expected) {
        return Ok(());
    }
    Err(Refusal::conflict(format!(
        "the layout is at revision {}, not {expected}: it changed since you read it, so nothing \
         was done. Read it again (get-layout) and retry with its revision.",
        session
            .revision
            .map(|r| r.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    )))
}

/// The checks every verb passes before it reaches the tree, on every path
/// (MCP, the CLI, the FFI, HTTP, a surface effect, a batch): a query naming a
/// record kind the manifest does not know is refused at verb time rather
/// than stored to match nothing (review RL-L12). The view-kind vocabulary is
/// the tree's own check (`unknown-view-kind`).
///
/// A `select`'s kind is NOT checked: a surface's `publish` may name a kind
/// this build has never heard of (the generic `item`, a newer build's kind —
/// `impress-surface-service`'s `publish_kind_and_ids`), and refusing it is a
/// change to the surface contract, not this one's.
pub fn check_verb(verb: &Verb, manifest: &KindManifest) -> Result<(), Refusal> {
    match verb {
        Verb::SetQuery { query, .. } => check_query_kinds(query, manifest),
        Verb::SetPane { spec, .. } => check_query_kinds(&spec.query, manifest),
        Verb::Split {
            new: Some(spec), ..
        } => check_query_kinds(&spec.query, manifest),
        Verb::OpenList { kinds: Some(kinds) } => {
            for kind in kinds {
                check_record_kind(kind, manifest)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// A bare `open-list` names no kinds. Fill the cold-start list so the tree
/// has a fallback when the window no longer has a detail pane to copy.
/// A detail pane's kinds still win: the tree reads those first.
fn supply_open_list_fallback(verb: Verb) -> Verb {
    match verb {
        Verb::OpenList { kinds: None } => Verb::OpenList {
            kinds: Some(crate::store::default_list_query().kinds),
        },
        other => other,
    }
}

/// Refuse a record kind the manifest does not know (review RL-L12): a
/// selection of `publications` used to be stored and bind nothing, forever.
fn check_record_kind(kind: &str, manifest: &KindManifest) -> Result<(), Refusal> {
    if manifest.kinds.contains_key(kind) {
        return Ok(());
    }
    Err(Refusal::invalid_argument(format!(
        "unknown record kind '{kind}'; the manifest has: {}",
        manifest
            .kinds
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

/// Refuse a query naming a record kind the manifest does not know, at verb
/// time — not when a pane first fails to render it (review RL-L12). An
/// unbound parameter is NOT refused here: a pane binds it from its channel
/// later, and that is an empty state, not an error.
fn check_query_kinds(query: &PaneQuery, manifest: &KindManifest) -> Result<(), Refusal> {
    for kind in &query.kinds {
        check_record_kind(kind, manifest)?;
    }
    Ok(())
}

impl DefaultLayoutService {
    /// The answer of a verb that persists outside the per-verb path
    /// (`save_layout`, `apply_layout`, `apply_preset`, `delete_layout`),
    /// logged once under `layout` either way — who asked, what, and the
    /// refusal's code — so these reach `/api/logs` like every other verb
    /// (plan wave 7, T5's finding 3).
    fn persisted(
        &self,
        verb: &str,
        app_id: &str,
        actor: ActorKind,
        target: &str,
        outcome: Result<LayoutVerbResult, Refusal>,
    ) -> LayoutVerbResult {
        match outcome {
            Ok(result) => {
                tracing::info!(
                    target: "layout",
                    "{app_id}: {} {verb} {target}: {} (revision {:?})",
                    actor_name(actor),
                    result.message,
                    result.revision
                );
                result
            }
            Err(refusal) => {
                tracing::warn!(
                    target: "layout",
                    "{app_id}: {} {verb} {target} refused [{}]: {}",
                    actor_name(actor),
                    refusal.code,
                    refusal.message
                );
                LayoutVerbResult::refused(refusal)
            }
        }
    }

    /// `delete_layout`'s work. Who removed the row is logged by
    /// [`Self::persisted`] under `layout` (review RL-L24, narrowed): the row
    /// is a hard delete, its operation rows cascade with it, and the sync
    /// tombstone it leaves carries no author and is pruned — a durable
    /// "who deleted it" needs either a retire-instead-of-delete or an author
    /// on tombstones, which is a store decision, not this verb's.
    fn delete_named_as(
        &self,
        app_id: &str,
        key: &str,
        _actor: ActorKind,
    ) -> Result<LayoutVerbResult, Refusal> {
        let store = self.layout_store_for_write()?;
        // A preset shares the ⌃⌘1–9 union's name space with saved layouts
        // (see `apply_layout`), so "delete Triage" must be refused by NAME,
        // not silently answered "no such saved layout" — `reset-preset` is
        // the verb that undoes an edit to a preset.
        let presets = PresetStore::new(store.store().clone());
        presets.ensure_shipped(app_id)?;
        if presets.load(app_id, key)?.is_some() {
            return Err(Refusal::new(
                "preset-not-deletable",
                format!(
                    "'{key}' is a preset, not a saved layout — presets are never deleted; use \
                     reset-preset to restore the shipped revision."
                ),
            ));
        }
        // `delete_named` never finds the live row: it resolves by name/id
        // through `load_named`, which skips `is_live` rows, so the live
        // arrangement reads as "not found" — the `ok: false` this verb
        // promises for it.
        if store.delete_named(app_id, key)? {
            Ok(LayoutVerbResult::done(format!("Deleted '{key}'.")))
        } else {
            Err(Refusal::not_found(format!(
                "no saved layout named or id'd '{key}'"
            )))
        }
    }

    /// [`Self::persisted`] for the preset verbs' own envelope.
    fn persisted_preset(
        &self,
        verb: &str,
        app_id: &str,
        actor: ActorKind,
        target: &str,
        outcome: Result<PresetResult, Refusal>,
    ) -> PresetResult {
        match outcome {
            Ok(result) => {
                tracing::info!(
                    target: "layout",
                    "{app_id}: {} {verb} {target}: {}",
                    actor_name(actor),
                    result.message
                );
                result
            }
            Err(refusal) => {
                tracing::warn!(
                    target: "layout",
                    "{app_id}: {} {verb} {target} refused [{}]: {}",
                    actor_name(actor),
                    refusal.code,
                    refusal.message
                );
                PresetResult::refused(refusal)
            }
        }
    }
}

fn flatten<T>(outcome: Result<Result<T, Refusal>, Refusal>) -> Result<T, Refusal> {
    outcome.and_then(|inner| inner)
}

/// A refusal by the tree, as the `Refusal` every result carries: its serde
/// tag is the code (review RL-L11).
pub(crate) fn layout_refusal(error: LayoutError) -> Refusal {
    Refusal::new(error.code(), error.to_string())
}

/// The verb and what it named, for the front of a refusal's message —
/// `close {"ref":"role","role":"detail"}` — so the caller learns which verb
/// and which argument the tree refused (review RL-L11).
fn verb_label(verb: &Verb) -> String {
    let value = serde_json::to_value(verb).unwrap_or(serde_json::Value::Null);
    let name = value
        .get("verb")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("verb")
        .to_string();
    let named = ["target", "tile", "container", "window", "a"]
        .iter()
        .find_map(|key| value.get(*key));
    match named {
        Some(target) => format!("{name} {target}"),
        None => name,
    }
}

fn actor_name(actor: ActorKind) -> &'static str {
    match actor {
        ActorKind::Human => "human",
        ActorKind::Agent => "agent",
        ActorKind::System => "system",
    }
}

/// A refused verb's result, logged once under `layout` with who asked, what
/// was refused and why (review RL-L6).
fn refused_verb(
    app_id: &str,
    device: &str,
    actor: ActorKind,
    verb: Option<&Verb>,
    refusal: Refusal,
) -> LayoutVerbResult {
    tracing::warn!(
        target: "layout",
        "{app_id}/{device}: {} {} refused [{}]: {}",
        actor_name(actor),
        verb.map(verb_label).unwrap_or_else(|| "verb".to_string()),
        refusal.code,
        refusal.message
    );
    LayoutVerbResult::refused(refusal)
}

/// Put what the session reported about its own loading (a reload because the
/// row moved, a quarantined row) in front of a result's message, so the
/// caller learns that its undo history was dropped or its layout set aside.
fn with_notices(mut result: LayoutVerbResult, notices: &[String]) -> LayoutVerbResult {
    if !notices.is_empty() {
        result.message = format!("{} {}", notices.join(" "), result.message);
    }
    result
}

/// The sentence recorded as the operation's `reason` (ADR-0019 D4: *why* was
/// this change made). One place, so the log reads consistently whether the
/// gesture came from a chord, an agent or a test.
fn intent_text(verb: &Verb) -> String {
    match verb {
        Verb::Split { dir, .. } => format!("split a pane {dir:?} and focused the new one"),
        Verb::MoveTile { placement, .. } => format!("moved a tile {placement:?}"),
        Verb::Close { .. } => "closed a pane".to_string(),
        Verb::Swap { .. } => "swapped two tiles".to_string(),
        Verb::Resize { container, .. } => format!("resized container {container}"),
        Verb::SetContainerKind { container, kind } => {
            format!("retyped container {container} as {kind:?}")
        }
        Verb::SetCollapsed { collapsed, .. } => match collapsed {
            Some(true) => "collapsed a pane".to_string(),
            Some(false) => "showed a collapsed pane again".to_string(),
            None => "toggled a pane's collapse".to_string(),
        },
        Verb::Maximize { .. } => "maximized a pane".to_string(),
        Verb::Restore => "restored from maximize".to_string(),
        Verb::Detach { .. } => "detached a pane into its own window".to_string(),
        Verb::SetPane { .. } => "replaced a pane's spec".to_string(),
        Verb::SetQuery { .. } => "re-pointed a pane's query".to_string(),
        Verb::SetViewKind { view_kind, .. } => format!("re-rendered a pane as '{view_kind}'"),
        Verb::OpenList { .. } => "opened the list".to_string(),
        Verb::SetTabOrder { tabs, .. } => format!("ordered {} detail tabs", tabs.len()),
        Verb::BindParam { name, .. } => format!("re-bound the parameter '{name}'"),
        Verb::SetChannel { channel, .. } => format!("moved a pane to channel {channel}"),
        Verb::SetRole { role, .. } => match role {
            Some(role) => format!("gave a pane the role '{role}'"),
            None => "cleared a pane's role".to_string(),
        },
        Verb::Focus { .. } => "focused a pane".to_string(),
        Verb::FocusDirection { direction } => format!("stepped focus {direction:?}"),
        Verb::Select { kind, ids, .. } => {
            format!("selected {} {kind}(s) on a pane's channel", ids.len())
        }
        Verb::SetWindowGeometry { window, geometry } => match geometry {
            Some(_) => format!("moved window {window}"),
            None => format!("cleared window {window}'s frame"),
        },
        Verb::SetDefaultChannel { window, channel } => {
            format!("window {window} now follows channel {channel}")
        }
    }
}

/// A saved layout's label: its name, or its id when a hand-edited row has
/// none.
fn label_of(row: &LayoutRow) -> String {
    row.name.clone().unwrap_or_else(|| row.id.to_string())
}

/// Make `layout` the live arrangement of this session and persist it.
///
/// ADR-0019 D2: the logical tree ports between devices; the window frame does
/// not, so geometry is dropped on the way in — a 27" frame has no business
/// landing on a laptop.
fn apply_tree(
    session: &mut LayoutSession,
    store: &LayoutStore,
    mut layout: impress_layout::Layout,
    label: &str,
    actor: ActorKind,
    what: &str,
) -> Result<LayoutVerbResult, Refusal> {
    for window in &mut layout.windows {
        window.geometry = None;
    }
    // ADR-0031 D6: the editor in a role survives the tree being replaced —
    // a preset carries no sessions, so the live one of the pane in the same
    // role is carried over, and every other session-bearing pane gets its own.
    layout.adopt_sessions_by_role(&session.layout);
    session.replace(layout);
    session.save(store, actor, &format!("applied the {what} '{label}'"))?;
    Ok(
        LayoutVerbResult::from_layout(format!("Applied '{label}'."), &session.layout, None, None)
            .with_revision(session.revision),
    )
}

/// Apply one preset and record where the live arrangement came from.
///
/// Shared by `apply_preset` and by `apply_layout`'s ordinal path, so a preset
/// recalled by ⌃⌘2 leaves exactly the same `DerivedFrom` edge as one applied
/// by name. A preset row with no tree is refused rather than approximated: an
/// inheriting preset (one that overrides only queries or roles) is a shape
/// `schemas/ui.rs` allows and nothing composes yet.
fn apply_preset_row(
    session: &mut LayoutSession,
    store: &LayoutStore,
    presets: &PresetStore,
    name: &str,
    actor: ActorKind,
) -> Result<LayoutVerbResult, Refusal> {
    let (row, stored) = presets.load(&session.app_id, name)?.ok_or_else(|| {
        Refusal::not_found(format!("no preset named '{name}' for {}", session.app_id))
    })?;
    let layout = stored.layout.ok_or_else(|| {
        Refusal::new(
            "preset-without-tree",
            format!(
                "the preset '{}' carries no tree of its own (it inherits one), which nothing \
                 composes yet",
                row.name
            ),
        )
    })?;
    let result = apply_tree(session, store, layout, &row.name, actor, "preset")?;
    let revision = presets::record_derived_from(
        store.store(),
        session.item_id,
        session.revision,
        row.id,
        actor,
        &format!("this arrangement came from the preset '{}'", row.name),
    )?;
    if let Some(revision) = revision {
        session.advance_revision(revision);
    }
    Ok(result.with_revision(session.revision))
}

/// One preset row, as the wire shows it.
fn preset_dto(presets: &PresetStore, row: &PresetRow, ordinal: u32) -> Result<PresetDto, Refusal> {
    let (_, stored) = presets
        .load(&row.app_id, &row.id.to_string())?
        .ok_or_else(|| Refusal::not_found(format!("preset {} vanished", row.id)))?;
    let edited = presets.matches_shipped(row, &stored).map(|same| !same);
    let panes = stored
        .layout
        .as_ref()
        .map(|layout| layout.panes().len() as u32)
        .unwrap_or(0);
    let mut roles: Vec<String> = stored.roles.keys().cloned().collect();
    roles.sort();
    Ok(PresetDto {
        id: row.id.to_string(),
        ordinal,
        name: row.name.clone(),
        app_id: row.app_id.clone(),
        purpose: row.purpose.clone(),
        version: row.version,
        shipped: presets::shipped_preset(&row.app_id, &row.name).is_some(),
        edited,
        panes,
        roles,
        modified: row.modified.to_rfc3339(),
    })
}

#[async_trait::async_trait]
impl LayoutService for DefaultLayoutService {
    async fn split(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        direction: String,
        after: bool,
        new_pane: Option<PaneSpec>,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        // A missing `new_pane` duplicates the pane being split; that rule is
        // `impress_layout`'s (`Verb::Split`), so MCP, the CLI, the FFI and
        // HTTP send one shape and get one answer (review RL-L20).
        self.apply_verb(&app_id, device, actor, expected_revision, |_| {
            Ok(Verb::Split {
                target: target.to_pane_ref()?,
                dir: parse_linear_dir(&direction)?,
                after,
                new: new_pane,
            })
        })
    }

    async fn move_tile(
        &self,
        app_id: String,
        device: Option<String>,
        tile: PaneRefDto,
        target: PaneRefDto,
        placement: String,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        self.apply_verb(&app_id, device, actor, expected_revision, |_| {
            Ok(Verb::MoveTile {
                tile: tile.to_pane_ref()?,
                target: target.to_pane_ref()?,
                placement: parse_placement(&placement)?,
            })
        })
    }

    async fn close(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        self.apply_verb(&app_id, device, actor, expected_revision, |_| {
            Ok(Verb::Close {
                target: target.to_pane_ref()?,
            })
        })
    }

    async fn swap(
        &self,
        app_id: String,
        device: Option<String>,
        a: PaneRefDto,
        b: PaneRefDto,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        self.apply_verb(&app_id, device, actor, expected_revision, |_| {
            Ok(Verb::Swap {
                a: a.to_pane_ref()?,
                b: b.to_pane_ref()?,
            })
        })
    }

    async fn resize(
        &self,
        app_id: String,
        device: Option<String>,
        container: u64,
        shares: Vec<f32>,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        self.apply_verb(&app_id, device, actor, expected_revision, |_| {
            Ok(Verb::Resize {
                container: TileId::new(container),
                shares,
            })
        })
    }

    async fn set_container_kind(
        &self,
        app_id: String,
        device: Option<String>,
        container: u64,
        kind: String,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        self.apply_verb(&app_id, device, actor, expected_revision, |_| {
            Ok(Verb::SetContainerKind {
                container: TileId::new(container),
                kind: parse_container_kind(&kind)?,
            })
        })
    }

    async fn set_collapsed(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        collapsed: Option<bool>,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        self.apply_verb(&app_id, device, actor, expected_revision, |_| {
            Ok(Verb::SetCollapsed {
                target: target.to_pane_ref()?,
                collapsed,
            })
        })
    }

    async fn maximize(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        self.apply_verb(&app_id, device, actor, expected_revision, |_| {
            Ok(Verb::Maximize {
                target: target.to_pane_ref()?,
            })
        })
    }

    async fn restore(
        &self,
        app_id: String,
        device: Option<String>,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        self.apply_verb(&app_id, device, actor, expected_revision, |_| {
            Ok(Verb::Restore)
        })
    }

    async fn detach(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        self.apply_verb(&app_id, device, actor, expected_revision, |_| {
            Ok(Verb::Detach {
                target: target.to_pane_ref()?,
            })
        })
    }

    async fn set_pane(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        spec: PaneSpec,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        self.apply_verb(&app_id, device, actor, expected_revision, |_| {
            Ok(Verb::SetPane {
                target: target.to_pane_ref()?,
                spec,
            })
        })
    }

    async fn set_query(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        query: PaneQuery,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        self.apply_verb(&app_id, device, actor, expected_revision, |_| {
            Ok(Verb::SetQuery {
                target: target.to_pane_ref()?,
                query,
            })
        })
    }

    async fn set_view_kind(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        view_kind_id: String,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        self.apply_verb(&app_id, device, actor, expected_revision, |_| {
            Ok(Verb::SetViewKind {
                target: target.to_pane_ref()?,
                view_kind: view_kind(&view_kind_id)?,
            })
        })
    }

    async fn bind_param(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        name: String,
        source: ParamSource,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        self.apply_verb(&app_id, device, actor, expected_revision, |_| {
            Ok(Verb::BindParam {
                target: target.to_pane_ref()?,
                name,
                source,
            })
        })
    }

    async fn set_channel(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        channel: String,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        self.apply_verb(&app_id, device, actor, expected_revision, |_| {
            Ok(Verb::SetChannel {
                target: target.to_pane_ref()?,
                channel: parse_channel(&channel)?,
            })
        })
    }

    async fn set_default_channel(
        &self,
        app_id: String,
        device: Option<String>,
        window: Option<u64>,
        channel: String,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        self.apply_verb(&app_id, device, actor, expected_revision, |session| {
            Ok(Verb::SetDefaultChannel {
                window: window_or_current(session, window)?,
                channel: parse_channel(&channel)?,
            })
        })
    }

    async fn set_role(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        role: Option<String>,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        self.apply_verb(&app_id, device, actor, expected_revision, |_| {
            Ok(Verb::SetRole {
                target: target.to_pane_ref()?,
                role: role
                    .map(|r| r.trim().to_string())
                    .filter(|r| !r.is_empty())
                    .map(Role::from),
            })
        })
    }

    async fn focus(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        self.apply_verb(&app_id, device, actor, expected_revision, |_| {
            Ok(Verb::Focus {
                target: target.to_pane_ref()?,
            })
        })
    }

    async fn focus_direction(
        &self,
        app_id: String,
        device: Option<String>,
        direction: String,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        self.apply_verb(&app_id, device, actor, expected_revision, |_| {
            Ok(Verb::FocusDirection {
                direction: parse_direction(&direction)?,
            })
        })
    }

    async fn select(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
        kind: String,
        ids: Vec<String>,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        self.apply_verb(&app_id, device, actor, expected_revision, |_| {
            Ok(Verb::Select {
                target: target.to_pane_ref()?,
                kind: kind.trim().to_string(),
                ids: parse_ids(&ids)?,
            })
        })
    }

    async fn set_window_geometry(
        &self,
        app_id: String,
        device: Option<String>,
        window: Option<u64>,
        geometry: Option<Geometry>,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        self.apply_verb(&app_id, device, actor, expected_revision, |session| {
            Ok(Verb::SetWindowGeometry {
                window: window_or_current(session, window)?,
                geometry,
            })
        })
    }

    async fn commit(
        &self,
        app_id: String,
        device: Option<String>,
        as_kind: String,
        name: String,
        purpose: Option<String>,
        actor: Option<String>,
    ) -> LayoutVerbResult {
        match as_kind.trim().to_ascii_lowercase().as_str() {
            "layout" | "" => self.save_layout(app_id, device, name, purpose, actor).await,
            // L7: a preset IS a materializable commit of the current
            // arrangement, so the verb that promised it now keeps its word.
            // The result envelope differs, so this reports what `save_preset`
            // said rather than returning its row.
            "preset" => {
                let saved = self
                    .save_preset(app_id, device, name, purpose, true, actor)
                    .await;
                if saved.ok {
                    LayoutVerbResult::done(saved.message)
                } else {
                    LayoutVerbResult::refused(Refusal::new(
                        saved.code.unwrap_or_else(|| "refused".to_string()),
                        saved.message,
                    ))
                }
            }
            other => {
                let refusal = Refusal::invalid_argument(format!(
                    "cannot commit the current bindings as a '{other}' yet — only 'layout' and \
                     'preset' are materializable today. Committing a figure or a collection is \
                     the same verb with a different `as_kind` and arrives with the implore work."
                ));
                self.persisted(
                    "commit",
                    &app_id,
                    actor_from(actor.as_deref()),
                    &format!("'{}' as '{other}'", name.trim()),
                    Err(refusal),
                )
            }
        }
    }

    async fn save_layout(
        &self,
        app_id: String,
        device: Option<String>,
        name: String,
        purpose: Option<String>,
        actor: Option<String>,
    ) -> LayoutVerbResult {
        let actor_kind = actor_from(actor.as_deref());
        let outcome = self.verb_for_write(&app_id, device, actor_kind, |session, store| {
            let intent = format!("committed the arrangement as '{}'", name.trim());
            let row = store.save_named(
                &session.app_id,
                &name,
                purpose.as_deref(),
                &session.layout,
                actor_kind,
                &intent,
            )?;
            // ADR-0031 D7: undo never crosses a commit.
            session.commit_boundary();
            Ok(LayoutVerbResult::from_layout(
                format!("Saved '{}' ({}).", row.name.unwrap_or_default(), row.id),
                &session.layout,
                None,
                None,
            )
            .with_revision(session.revision))
        });
        self.persisted(
            "save_layout",
            &app_id,
            actor_kind,
            &format!("'{}'", name.trim()),
            outcome,
        )
    }

    async fn apply_layout(
        &self,
        app_id: String,
        device: Option<String>,
        name: Option<String>,
        ordinal: Option<u32>,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        let actor_kind = actor_from(actor.as_deref());
        let what = match (&name, ordinal) {
            (Some(name), _) => format!("'{}'", name.trim()),
            (None, Some(ordinal)) => format!("ordinal {ordinal}"),
            (None, None) => "nothing".to_string(),
        };
        let outcome = self.verb_for_write(&app_id, device, actor_kind, |session, store| {
            check_expected(session, expected_revision)?;
            // An ordinal spans the SAME union `list_presets` numbers: this
            // app's presets, then its named layouts (`presets::ordinal_targets`).
            // So ⌃⌘1 is the app's default preset on a machine that has never
            // saved a layout and on one that has saved nine.
            if let (None, Some(ordinal)) = (
                name.as_deref().map(str::trim).filter(|n| !n.is_empty()),
                ordinal,
            ) {
                if ordinal == 0 {
                    return Err(Refusal::invalid_argument(
                        "layout ordinals are 1-based (⌃⌘1–9)",
                    ));
                }
                let presets = PresetStore::new(store.store().clone());
                let targets = presets::ordinal_targets(&presets, store, &session.app_id)?;
                let target = targets.get(ordinal as usize - 1).ok_or_else(|| {
                    Refusal::not_found(format!(
                        "no preset or saved layout at ordinal {ordinal}; this app has {}",
                        targets.len()
                    ))
                })?;
                return match target {
                    presets::OrdinalTarget::Preset(row) => {
                        apply_preset_row(session, store, &presets, &row.name, actor_kind)
                    }
                    presets::OrdinalTarget::Layout(row) => {
                        let id = row.id.to_string();
                        let (row, layout) =
                            store.load_named(&session.app_id, &id)?.ok_or_else(|| {
                                Refusal::not_found(format!("saved layout {id} vanished"))
                            })?;
                        apply_tree(
                            session,
                            store,
                            layout,
                            &label_of(&row),
                            actor_kind,
                            "saved layout",
                        )
                    }
                };
            }

            let Some(name) = name.as_deref().map(str::trim).filter(|n| !n.is_empty()) else {
                return Err(Refusal::invalid_argument(
                    "apply_layout needs a name or an ordinal",
                ));
            };
            let (row, layout) = match store.load_named(&session.app_id, name)? {
                Some(found) => found,
                // A name that is not a saved layout may be a PRESET — the two
                // share the ⌃⌘1–9 union, so they must share the name space a
                // palette types into as well. Refusing "Triage" because it is
                // a preset rather than a layout would be a distinction only
                // this crate can see.
                None => {
                    let presets = PresetStore::new(store.store().clone());
                    presets.ensure_shipped(&session.app_id)?;
                    if presets.load(&session.app_id, name)?.is_some() {
                        return apply_preset_row(session, store, &presets, name, actor_kind);
                    }
                    return Err(Refusal::not_found(format!(
                        "no saved layout or preset named '{name}'"
                    )));
                }
            };
            apply_tree(
                session,
                store,
                layout,
                &label_of(&row),
                actor_kind,
                "saved layout",
            )
        });
        self.persisted("apply_layout", &app_id, actor_kind, &what, outcome)
    }

    async fn delete_layout(
        &self,
        app_id: String,
        name_or_id: String,
        actor: Option<String>,
    ) -> LayoutVerbResult {
        let actor_kind = actor_from(actor.as_deref());
        let key = name_or_id.trim().to_string();
        let outcome = self.delete_named_as(&app_id, &key, actor_kind);
        self.persisted(
            "delete_layout",
            &app_id,
            actor_kind,
            &format!("'{key}'"),
            outcome,
        )
    }

    async fn undo(
        &self,
        app_id: String,
        device: Option<String>,
        stack: String,
        target: Option<PaneRefDto>,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        self.step_ring(
            &app_id,
            device,
            &stack,
            target,
            actor,
            expected_revision,
            true,
        )
    }

    async fn redo(
        &self,
        app_id: String,
        device: Option<String>,
        stack: String,
        target: Option<PaneRefDto>,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        self.step_ring(
            &app_id,
            device,
            &stack,
            target,
            actor,
            expected_revision,
            false,
        )
    }

    async fn get_layout(&self, app_id: String, device: Option<String>) -> LayoutResult {
        self.get_layout_as(&app_id, device, ActorKind::Agent)
    }

    async fn get_pane(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
    ) -> PaneResult {
        let reference = match target.to_pane_ref() {
            Ok(reference) => reference,
            Err(e) => return PaneResult::refused(Refusal::invalid_argument(e)),
        };
        match self.compiled_pane(&app_id, device, &reference, ActorKind::Agent) {
            Ok(pane) => {
                let compiled = match &pane.compiled {
                    Ok(compiled) => CompiledQueryDto::compiled(compiled),
                    Err(e) => CompiledQueryDto::refused(e),
                };
                let marker = self.store_marker();
                PaneResult {
                    ok: true,
                    wire_version: WIRE_VERSION,
                    code: None,
                    message: self.read_message(
                        &marker,
                        format!("pane {} renders '{}'", pane.tile, pane.spec.view_kind),
                    ),
                    store: marker,
                    tile: Some(pane.tile.raw()),
                    query: Some(compiled),
                    bindings: binding_map(&pane.bindings),
                    channel: pane.channel,
                    focused: pane.focused.map(TileId::raw),
                    affected_panes: vec![pane.tile.raw()],
                    spec: Some(pane.spec),
                }
            }
            Err(e) => PaneResult::refused(e),
        }
    }

    async fn get_channel(
        &self,
        app_id: String,
        device: Option<String>,
        channel: String,
        kind: Option<String>,
    ) -> ChannelResult {
        let marker = self.store_marker();
        let outcome = self.with_session(&app_id, device, ActorKind::Agent, |session, _| {
            let id = parse_channel(&channel)?;
            let window = session.layout.current_window().ok();
            let default = window
                .and_then(|w| session.layout.window(w))
                .map(|w| w.default_channel)
                .unwrap_or(ChannelId::ONE);
            let number = id.resolve(default);
            let wanted = kind.as_deref().map(str::trim).filter(|k| !k.is_empty());

            let mut selections: BTreeMap<String, Vec<String>> = BTreeMap::new();
            if let Some(per_kind) = session.layout.channels.channels.get(&number) {
                for (record_kind, ids) in per_kind {
                    if wanted.map(|w| w != record_kind).unwrap_or(false) {
                        continue;
                    }
                    selections.insert(
                        record_kind.clone(),
                        ids.iter().map(|id| id.to_string()).collect(),
                    );
                }
            }

            let mut affected: Vec<TileId> = Vec::new();
            let kinds: Vec<String> = match wanted {
                Some(k) => vec![k.to_string()],
                None => selections.keys().cloned().collect(),
            };
            for record_kind in &kinds {
                for tile in session
                    .layout
                    .affected_panes(ChannelId::Number(number), record_kind)
                {
                    if !affected.contains(&tile) {
                        affected.push(tile);
                    }
                }
            }

            Ok(ChannelResult {
                ok: true,
                wire_version: WIRE_VERSION,
                code: None,
                message: self.read_message(
                    &marker,
                    format!("channel {number} carries {} kind(s)", selections.len()),
                ),
                store: marker.clone(),
                channel: number,
                selections,
                affected_panes: raw_tiles(&affected),
                focused: window.and_then(|w| session.focused(w)).map(TileId::raw),
            })
        });
        outcome.unwrap_or_else(ChannelResult::refused)
    }

    async fn resolve_reference(
        &self,
        app_id: String,
        device: Option<String>,
        target: PaneRefDto,
    ) -> ReferenceResult {
        let marker = self.store_marker();
        let outcome = self.with_session(&app_id, device, ActorKind::Agent, |session, _| {
            let window = session.layout.current_window().map_err(layout_refusal)?;
            let reference = target.to_pane_ref()?;
            let tile = session
                .layout
                .resolve(window, &reference)
                .map_err(layout_refusal)?;
            let spec = session.layout.pane(tile);
            Ok(ReferenceResult {
                ok: true,
                wire_version: WIRE_VERSION,
                code: None,
                message: self.read_message(
                    &marker,
                    format!(
                        "{} is tile {tile}",
                        serde_json::to_string(&reference).unwrap_or_default()
                    ),
                ),
                store: marker.clone(),
                tile: Some(tile.raw()),
                role: spec.and_then(|s| s.role.as_ref()).map(|r| r.to_string()),
                view_kind: spec.map(|s| s.view_kind.to_string()),
                is_pane: spec.is_some(),
                focused: session.focused(window).map(TileId::raw),
                affected_panes: vec![tile.raw()],
            })
        });
        outcome.unwrap_or_else(ReferenceResult::refused)
    }

    async fn list_layouts(&self, app_id: String) -> LayoutListResult {
        let store = self.layout_store();
        // The presets hold the first ordinals of the union (see
        // `presets::ordinal_targets`), so a saved layout's chord starts after
        // them. Seeding here is what makes the offset the same number on
        // every machine rather than "however many presets happen to exist".
        let presets = PresetStore::new(store.store().clone());
        let offset = match presets.ensure_shipped(&app_id) {
            Ok(rows) => rows.len() as u32,
            Err(e) => return LayoutListResult::refused(e),
        };
        match store.list_named(&app_id) {
            Ok(rows) => {
                let layouts: Vec<SavedLayoutDto> = rows
                    .into_iter()
                    .enumerate()
                    .map(|(index, row)| SavedLayoutDto {
                        id: row.id.to_string(),
                        ordinal: offset + index as u32 + 1,
                        name: row.name,
                        purpose: row.purpose,
                        app_id: row.app_id,
                        device: row.device,
                        is_live: row.is_live,
                        modified: row.modified.to_rfc3339(),
                    })
                    .collect();
                LayoutListResult {
                    ok: true,
                    wire_version: WIRE_VERSION,
                    code: None,
                    message: self.read_message(
                        &self.store_marker(),
                        format!("{} saved layout(s).", layouts.len()),
                    ),
                    store: self.store_marker(),
                    layouts,
                }
            }
            Err(e) => LayoutListResult::refused(e),
        }
    }

    // --------------------------------------------------------------- presets

    async fn list_presets(&self, app_id: String) -> PresetListResult {
        let store = self.layout_store();
        let presets = PresetStore::new(store.store().clone());
        let rows = match presets.ensure_shipped(&app_id) {
            Ok(rows) => rows,
            Err(e) => return PresetListResult::refused(e),
        };
        let mut dtos: Vec<PresetDto> = Vec::with_capacity(rows.len());
        for (index, row) in rows.iter().enumerate() {
            match preset_dto(&presets, row, index as u32 + 1) {
                Ok(dto) => dtos.push(dto),
                Err(e) => return PresetListResult::refused(e),
            }
        }
        let known = presets::named_queries(&app_id);
        PresetListResult {
            ok: true,
            wire_version: WIRE_VERSION,
            code: None,
            message: self.read_message(
                &self.store_marker(),
                format!("{} preset(s), {} named quer(ies).", dtos.len(), known.len()),
            ),
            store: self.store_marker(),
            presets: dtos,
            materialize_first: presets::MATERIALIZE_FIRST
                .iter()
                .map(|(section, reason)| MaterializeFirstDto {
                    section: (*section).to_string(),
                    reason: (*reason).to_string(),
                })
                .collect(),
        }
    }

    async fn apply_preset(
        &self,
        app_id: String,
        device: Option<String>,
        name: String,
        actor: Option<String>,
        expected_revision: Option<u64>,
    ) -> LayoutVerbResult {
        let actor_kind = actor_from(actor.as_deref());
        let outcome = self.verb_for_write(&app_id, device, actor_kind, |session, store| {
            check_expected(session, expected_revision)?;
            let presets = PresetStore::new(store.store().clone());
            presets.ensure_shipped(&session.app_id)?;
            apply_preset_row(session, store, &presets, &name, actor_kind)
        });
        self.persisted(
            "apply_preset",
            &app_id,
            actor_kind,
            &format!("'{name}'"),
            outcome,
        )
    }

    async fn save_preset(
        &self,
        app_id: String,
        device: Option<String>,
        name: String,
        purpose: Option<String>,
        from_live: bool,
        actor: Option<String>,
    ) -> PresetResult {
        let actor_kind = actor_from(actor.as_deref());
        if !from_live {
            return self.persisted_preset(
                "save_preset",
                &app_id,
                actor_kind,
                &format!("'{}'", name.trim()),
                Err(Refusal::invalid_argument(
                    "save_preset builds a preset from the LIVE arrangement, so `from_live` must \
                     be true. Building one from a saved layout or from another preset is a \
                     different verb and does not exist yet.",
                )),
            );
        }
        let outcome = self.with_session_for_write(&app_id, device, actor_kind, |session, store| {
            let presets = PresetStore::new(store.store().clone());
            presets.ensure_shipped(&session.app_id)?;
            let roles = presets::roles_of(&session.layout);
            let queries = presets::named_queries(&session.app_id);
            let intent = format!("saved the arrangement as the preset '{}'", name.trim());
            let row = presets.save(
                &session.app_id,
                &name,
                purpose.as_deref(),
                &session.layout,
                &queries,
                &roles,
                // The shipped revision is NOT rewritten by a user's edit:
                // keeping it is what lets `reset_preset` undo this and what
                // distinguishes "the user edited Triage" from "we shipped a
                // newer Triage" (`schemas/ui.rs`, `version`).
                None,
                actor_kind,
                &intent,
            )?;
            // ADR-0031 D7: undo never crosses a commit, and a preset is one.
            session.commit_boundary();
            if let Some(revision) = presets::record_derived_from(
                store.store(),
                session.item_id,
                session.revision,
                row.id,
                actor_kind,
                &intent,
            )? {
                session.advance_revision(revision);
            }
            let ordinal = ordinal_of(&presets, store, &session.app_id, row.id)?;
            Ok(PresetResult {
                ok: true,
                wire_version: WIRE_VERSION,
                code: None,
                message: format!("Saved the preset '{}' ({}).", row.name, row.id),
                preset: Some(preset_dto(&presets, &row, ordinal)?),
            })
        });
        self.persisted_preset(
            "save_preset",
            &app_id,
            actor_kind,
            &format!("'{}'", name.trim()),
            outcome,
        )
    }

    async fn reset_preset(
        &self,
        app_id: String,
        name: String,
        actor: Option<String>,
    ) -> PresetResult {
        let actor_kind = actor_from(actor.as_deref());
        let outcome = (|| {
            let store = self.layout_store_for_write()?;
            let presets = PresetStore::new(store.store().clone());
            presets.ensure_shipped(&app_id)?;
            let row = presets.reset(&app_id, &name, actor_kind)?;
            let ordinal = ordinal_of(&presets, &store, &app_id, row.id)?;
            Ok(PresetResult {
                ok: true,
                wire_version: WIRE_VERSION,
                code: None,
                message: format!(
                    "Reset '{}' to the revision the suite ships (v{}).",
                    row.name,
                    row.version.unwrap_or(0)
                ),
                preset: Some(preset_dto(&presets, &row, ordinal)?),
            })
        })();
        self.persisted_preset(
            "reset_preset",
            &app_id,
            actor_kind,
            &format!("'{name}'"),
            outcome,
        )
    }
}

/// Where a preset sits in the ⌃⌘1–9 union, 1-based. `0` when it is somehow
/// not in it at all, which a caller reads as "no chord" rather than as a
/// claim about position one.
fn ordinal_of(
    presets: &PresetStore,
    layouts: &LayoutStore,
    app_id: &str,
    id: ItemId,
) -> Result<u32, Refusal> {
    Ok(presets::ordinal_targets(presets, layouts, app_id)?
        .iter()
        .position(|target| match target {
            presets::OrdinalTarget::Preset(row) => row.id == id,
            presets::OrdinalTarget::Layout(row) => row.id == id,
        })
        .map(|index| index as u32 + 1)
        .unwrap_or(0))
}

impl DefaultLayoutService {
    // The trait method's own arguments plus the direction; bundling them in
    // a struct would only restate the trait signature.
    #[allow(clippy::too_many_arguments)]
    fn step_ring(
        &self,
        app_id: &str,
        device: Option<String>,
        stack: &str,
        target: Option<PaneRefDto>,
        actor: Option<String>,
        expected_revision: Option<u64>,
        undo: bool,
    ) -> LayoutVerbResult {
        let actor_kind = actor_from(actor.as_deref());
        let mut notice = None;
        let outcome = self.verb_for_write(app_id, device, actor_kind, |session, store| {
            notice = session.take_notice();
            if notice.is_some() {
                // Reloaded on this very touch: the rings this ⌘Z was aimed at
                // are gone with the tree they described. Say so rather than
                // answer "nothing to undo" as if nothing had happened.
                return Err(Refusal::conflict("nothing was undone or redone"));
            }
            check_expected(session, expected_revision)?;
            let arrangement = parse_stack(stack)?;
            let ring = if arrangement {
                UndoTarget::Arrangement
            } else {
                let target = target.as_ref().ok_or_else(|| {
                    Refusal::invalid_argument(
                        "an exploration undo or redo needs `target`: the pane whose ring it steps \
                         ({\"focused\": true} for the focused one)",
                    )
                })?;
                UndoTarget::Exploration(target.to_pane_ref()?)
            };
            let stepped = if undo {
                session.undo(&ring).map_err(layout_refusal)?
            } else {
                session.redo(&ring).map_err(layout_refusal)?
            };
            let word = if undo { "undo" } else { "redo" };
            if let (None, Some(dropped)) = (&stepped, session.dropped_history()) {
                // The ring is empty because a reload emptied it, not because
                // there was never anything to undo.
                return Err(Refusal::conflict(format!("nothing to {word}: {dropped}")));
            }
            let Some(patch) = stepped else {
                // An empty ring is a no-op, not a failure: ⌘Z with nothing to
                // undo does nothing everywhere else in macOS too.
                return Ok(LayoutVerbResult::from_layout(
                    format!("nothing to {word} on the {} ring", ring_name(&ring)),
                    &session.layout,
                    None,
                    None,
                )
                .with_revision(session.revision));
            };
            let intent = format!("{word}: {}", intent_text(&patch.verb));
            session.save(store, actor_kind, &intent)?;
            let named = match &ring {
                UndoTarget::Arrangement => Stack::Arrangement,
                UndoTarget::Exploration(_) => session
                    .layout
                    .current_window()
                    .ok()
                    .and_then(|w| match &ring {
                        UndoTarget::Exploration(reference) => {
                            session.layout.resolve(w, reference).ok()
                        }
                        UndoTarget::Arrangement => None,
                    })
                    .map(Stack::Exploration)
                    .unwrap_or(Stack::Unrecorded),
            };
            Ok(
                LayoutVerbResult::from_layout(intent, &session.layout, Some(&patch), Some(&named))
                    .with_revision(session.revision),
            )
        });
        let notices: Vec<String> = notice.into_iter().collect();
        with_notices(outcome.unwrap_or_else(LayoutVerbResult::refused), &notices)
    }
}

fn ring_name(target: &UndoTarget) -> &'static str {
    match target {
        UndoTarget::Arrangement => "arrangement",
        UndoTarget::Exploration(_) => "exploration",
    }
}

fn window_or_current(session: &LayoutSession, window: Option<u64>) -> Result<WindowId, Refusal> {
    match window {
        Some(raw) => Ok(WindowId::new(raw)),
        None => session.layout.current_window().map_err(layout_refusal),
    }
}

fn binding_map(bindings: &Bindings) -> BTreeMap<String, String> {
    bindings
        .values
        .iter()
        .map(|(name, id)| (name.clone(), id.to_string()))
        .collect()
}

impress_service_impl! {
    service = LayoutService,
    safety = mutating,
    since = "0.1.0",
    effects = {
        reads: ["impress/ui/layout@1.0.0", "impress/ui/preset@1.0.0"],
        writes: ["impress/ui/layout@1.0.0"],
        reach: [],
    },
    impl = DefaultLayoutService,
    instance = DefaultLayoutService::new,
    // A field the input schema does not name is refused with
    // `invalid-argument`, never parsed around (review RL-L3, AC-F3).
    strict_args = true,
    methods = [
        split(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Pane reference by exactly one id, role, direction, or focused selector.
            target: PaneRefDto,
            /// Split direction: horizontal (side by side) or vertical (stacked).
            direction: String,
            /// Place the newly split pane after the target when true.
            after: bool,
            /// Optional full spec for the new pane; omitted duplicates the target.
            new_pane: Option<PaneSpec>,
            /// Optional attribution for the layout or preset change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        move_tile(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Pane or subtree to move, named by exactly one reference selector.
            tile: PaneRefDto,
            /// Pane reference by exactly one id, role, direction, or focused selector.
            target: PaneRefDto,
            /// Where to move the tile relative to the target: left, right, above, below, or into-tabs.
            placement: String,
            /// Optional attribution for the layout change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        close(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Pane reference by exactly one id, role, direction, or focused selector.
            target: PaneRefDto,
            /// Optional attribution for the layout change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        swap(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// First pane reference to swap.
            a: PaneRefDto,
            /// Second pane reference to swap.
            b: PaneRefDto,
            /// Optional attribution for the layout or preset change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        resize(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Tile ID of the containing split or other container to change.
            container: u64,
            /// Positive relative weights, one for each split child in tree order.
            shares: Vec<f32>,
            /// Optional attribution for the layout or preset change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        set_container_kind(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Tile ID of the containing split or other container to change.
            container: u64,
            /// Record kind whose selection is published on the pane’s channel.
            kind: String,
            /// Optional attribution for the layout or preset change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        set_collapsed(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Pane reference by exactly one id, role, direction, or focused selector.
            target: PaneRefDto,
            /// `true` hides, `false` shows at the remembered share; omit to
            /// toggle.
            /// True hides the pane, false restores its remembered share, absent toggles.
            collapsed: Option<bool>,
            /// Optional attribution for the layout or preset change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        maximize(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Pane reference by exactly one id, role, direction, or focused selector.
            target: PaneRefDto,
            /// Optional attribution for the layout change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        restore(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Optional attribution for the layout or preset change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        detach(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Pane reference by exactly one id, role, direction, or focused selector.
            target: PaneRefDto,
            /// Optional attribution for the layout change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        set_pane(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Pane reference by exactly one id, role, direction, or focused selector.
            target: PaneRefDto,
            /// Complete replacement pane spec, including query, view kind and bindings.
            spec: PaneSpec,
            /// Optional attribution for the layout or preset change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        set_query(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Pane reference by exactly one id, role, direction, or focused selector.
            target: PaneRefDto,
            /// Closed pane query to display (kinds, scope, filters, sort and limit).
            query: PaneQuery,
            /// Optional attribution for the layout or preset change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        set_view_kind(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Pane reference by exactly one id, role, direction, or focused selector.
            target: PaneRefDto,
            /// Registered view kind to render this pane (for example `pdf`).
            view_kind: String,
            /// Optional attribution for the layout or preset change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        bind_param(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Pane reference by exactly one id, role, direction, or focused selector.
            target: PaneRefDto,
            /// The exact saved layout, preset, or parameter name for this operation.
            name: String,
            /// Parameter source: follow a channel, pin a fixed item, or use the view default.
            source: ParamSource,
            /// Optional attribution for the layout change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        set_channel(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Pane reference by exactly one id, role, direction, or focused selector.
            target: PaneRefDto,
            /// Numbered selection channel 1–8, or `follow` where supported.
            channel: String,
            /// Optional attribution for the layout or preset change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        set_default_channel(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Optional window tile ID; absent uses the currently focused window.
            window: Option<u64>,
            /// Numbered selection channel 1–8, or `follow` where supported.
            channel: String,
            /// Optional attribution for the layout or preset change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        set_role(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Pane reference by exactly one id, role, direction, or focused selector.
            target: PaneRefDto,
            /// New role for this pane, or null to clear its role.
            role: Option<String>,
            /// Optional attribution for the layout or preset change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        focus(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Pane reference by exactly one id, role, direction, or focused selector.
            target: PaneRefDto,
            /// Optional attribution for the layout change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        focus_direction(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Focus step: left, right, up, down, next, or prev.
            direction: String,
            /// Optional attribution for the layout change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        select(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Pane reference by exactly one id, role, direction, or focused selector.
            target: PaneRefDto,
            /// Record kind whose selection is published on the pane’s channel.
            kind: String,
            /// Selected item IDs of that kind; an empty list explicitly clears selection.
            ids: Vec<String>,
            /// Optional attribution for the layout or preset change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        set_window_geometry(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Optional window tile ID; absent uses the currently focused window.
            window: Option<u64>,
            /// Optional window frame; null clears a stored device-local frame.
            geometry: Option<Geometry>,
            /// Optional attribution for the layout or preset change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        commit(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Materialization kind; only `layout` is supported today.
            as_kind: String,
            /// The exact saved layout, preset, or parameter name for this operation.
            name: String,
            /// Optional human explanation retained on the saved layout.
            purpose: Option<String>,
            /// Optional attribution for the layout change (for example `human`).
            actor: Option<String>
        ) -> LayoutVerbResult,
        save_layout(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Saved layout or preset name in this app’s namespace.
            name: String,
            /// Optional human explanation stored with the named layout or preset.
            purpose: Option<String>,
            /// Optional attribution for the layout or preset change (for example `human`).
            actor: Option<String>
        ) -> LayoutVerbResult,
        apply_layout(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// The exact saved layout, preset, or parameter name for this operation.
            name: Option<String>,
            /// One-based shortcut position: app presets first, then saved layouts.
            ordinal: Option<u32>,
            /// Optional attribution for the layout change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        delete_layout(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Name or item ID of a saved layout; shipped presets cannot be deleted.
            name_or_id: String,
            /// Optional attribution for the layout change (for example `human`).
            actor: Option<String>
        ) -> LayoutVerbResult,
        undo(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Undo ring: arrangement or the target pane’s exploration history.
            stack: String,
            /// Pane reference by exactly one id, role, direction, or focused selector.
            target: Option<PaneRefDto>,
            /// Optional attribution for the layout or preset change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        redo(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Undo ring to redo: `arrangement` or one pane’s `exploration`.
            stack: String,
            /// Pane reference by exactly one id, role, direction, or focused selector.
            target: Option<PaneRefDto>,
            /// Optional attribution for the layout change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        get_layout(
            /// The owning app/facet whose live arrangement to read.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>
        ) -> LayoutResult,
        get_pane(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Pane reference by exactly one id, role, direction, or focused selector.
            target: PaneRefDto
        ) -> PaneResult,
        get_channel(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Numbered selection channel 1–8, or `follow` for the window default.
            channel: String,
            /// Optional record kind to select from the channel result.
            kind: Option<String>
        ) -> ChannelResult,
        resolve_reference(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Pane reference by exactly one id, role, direction, or focused selector.
            target: PaneRefDto
        ) -> ReferenceResult,
        list_layouts(
            /// The app/facet whose durable named layouts to list.
            app_id: String
        ) -> LayoutListResult,
        list_presets(
            /// The app/facet whose shipped and user presets to list.
            app_id: String
        ) -> PresetListResult,
        apply_preset(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// The exact saved layout, preset, or parameter name for this operation.
            name: String,
            /// Optional attribution for the layout change (for example `human`).
            actor: Option<String>,
            /// Refuse with `conflict`, changing nothing, unless the live
            /// layout is still at this revision (a result's `revision`).
            expected_revision: Option<u64>
        ) -> LayoutVerbResult,
        save_preset(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Optional stable device identifier; omitted uses the host device.
            device: Option<String>,
            /// Saved layout or preset name in this app’s namespace.
            name: String,
            /// Optional human explanation stored with the named layout or preset.
            purpose: Option<String>,
            /// Must be true: this preset is captured from the current live arrangement.
            from_live: bool,
            /// Optional attribution for the layout or preset change (for example `human`).
            actor: Option<String>
        ) -> PresetResult,
        reset_preset(
            /// The owning app/facet whose layouts and presets are scoped separately.
            app_id: String,
            /// Saved layout or preset name in this app’s namespace.
            name: String,
            /// Optional attribution for the layout or preset change (for example `human`).
            actor: Option<String>
        ) -> PresetResult,
    ],
}

#[cfg(test)]
mod open_list_fallback_tests {
    use super::supply_open_list_fallback;
    use impress_layout::Verb;

    #[test]
    fn a_bare_open_list_uses_the_cold_start_kinds() {
        let verb = supply_open_list_fallback(Verb::OpenList { kinds: None });
        assert_eq!(
            verb,
            Verb::OpenList {
                kinds: Some(vec!["publication".into()])
            }
        );
        let kept = Verb::OpenList {
            kinds: Some(vec!["manuscript".into()]),
        };
        assert_eq!(supply_open_list_fallback(kept.clone()), kept);
    }
}
