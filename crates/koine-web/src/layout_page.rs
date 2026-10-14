//! `/layout`: the window tree, performed (plan D7, and
//! docs/plan-koine-affordances.md A-D3).
//!
//! The page fetches `GET /api/layout/tree` and draws every window twice over:
//! first its tiles as an outline, the tree widget of any surface
//! ([`TileOutline`], drawn through `koine_dioxus::Surface`), then its panes.
//! The outline is how a tile moves, on every pointer: its rows are the
//! window's containers and panes in container order, `movable`, and a row
//! dropped elsewhere sends the widget's `move`, which
//! `koine::gesture::tree_move_verb` turns into the `move-tile` beside the
//! tile now at that place. The widget owns the gesture (a mouse, a finger, a
//! pen, and `x` then `p`/`P` on the keyboard), so the page draws no handle and
//! no drop targets of its own: a touch pointer's `pointerup` fires on the
//! element the press began on, so drop targets that listen for it never hear
//! a finger.
//!
//! Below the outline the panes draw as before: a `Linear` is a flex
//! container whose children grow by their share, a `Tabs` is a strip on the
//! top edge of the pane it shows, a `Grid` is a CSS grid. A pane whose view
//! kind is `surface` draws that surface live ([`SurfaceView`], the surface
//! page's component, with its own wait loop). Any other kind is a placeholder
//! that names its query kinds, role and tile, as `kit-demo` does: imbib's
//! list and detail are Swift views, and the web shows what koine can show
//! (ADR-0032).
//!
//! # The page decides nothing about what a pointer means
//!
//! | Pointer | Decided by |
//! |---|---|
//! | click a pane, a tab, or a row of the outline | `koine::gesture::layout_verb` of a `Focus` (a container's row focuses its first pane) |
//! | a row of the outline dropped (or cut and pasted) | the tree widget's `move`, then `koine::gesture::tree_move_verb` |
//! | a disclosure in the outline | the page's own memory: which containers are folded |
//! | divider down, then up (a fine pointer only) | `koine::gesture::resize_verb` (which calls `share_beside`) over the lengths the browser measured |
//! | `h` / `l` | [`keys::on_layout_keydown`], which is `keys::decide` |
//!
//! A divider is drawn only for a fine pointer on a page wide enough to lay
//! panes side by side (`@media (pointer: fine) and (min-width: 701px)`): a
//! phone stacks its panes and has nothing to resize, and the keyboard's
//! resize verbs serve the rest.
//!
//! There is no wait route for the layout (the surface routes have one), so the
//! page fetches again after every verb it posts and every [`POLL_MS`] while the
//! tab is visible.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::rc::Rc;

use dioxus::prelude::*;
use impress_layout::{Container, Layout, LinearDir, Tile, TileId, Verb, Window, WindowId};
use impress_surface::tree::{self as outline, Fields};
use impress_surface::{Event, EventKind, RenderKind, RenderNode, RenderTree};
use koine::cursor::Cursor;
use koine::gesture::{self, Element as GestureElement, Gesture};
use koine::pane_query::{ItemRef, Scope};
use koine_dioxus::Surface;
use serde_json::{json, Value};

use crate::api::{self, LayoutTree};
use crate::dom;
use crate::keys::{self, KeyAction};
use crate::session::{LayoutAnswer, Session};
use crate::surface_page::SurfaceView;

/// How often the page asks for the tree again while it is visible.
const POLL_MS: u32 = 5_000;

/// A divider drag shorter than this many pixels is a click, not a resize.
const MIN_RESIZE_PX: f32 = 2.0;

/// The page's own styles. The surface, the outline and the focus colour are
/// `koine.css`.
const LAYOUT_CSS: &str = r#"
.koine-layout { display: flex; flex-direction: column; gap: 16px; outline: none; min-height: 60vh; }
.koine-window h2 { margin: 0 0 6px; }
.koine-outline { margin: 0 0 10px; }
.koine-window-body { display: flex; height: 70vh; min-height: 280px; }
.koine-window-body > *, .koine-cell > * { flex: 1 1 auto; min-width: 0; min-height: 0; }
.koine-linear { display: flex; }
.koine-horizontal { flex-direction: row; }
.koine-vertical { flex-direction: column; }
.koine-cell { display: flex; flex: 1 1 0; min-width: 0; min-height: 0; }
.koine-gridbox { display: grid; gap: 6px; }
.koine-tabbed { display: flex; flex-direction: column; }
.koine-tabbed > :last-child { flex: 1 1 auto; min-height: 0; }
/* A divider resizes with a fine pointer on a page that lays panes side by
   side; anywhere else it is not drawn (a phone stacks its panes). Without a
   divider, siblings keep a gap. */
.koine-divider { display: none; }
.koine-linear { gap: 6px; }
@media (pointer: fine) and (min-width: 701px) {
  .koine-linear { gap: 0; }
  .koine-divider { display: block; flex: none; margin: 0; border: 0; background: transparent;
    border-radius: 3px; touch-action: none; user-select: none; }
  .koine-horizontal > .koine-divider { width: 6px; cursor: col-resize; }
  .koine-vertical > .koine-divider { height: 6px; cursor: row-resize; }
  .koine-divider:hover { background: var(--koine-color-accent, #007aff); }
}
.koine-pane { position: relative; display: flex; flex-direction: column; overflow: hidden;
  background: var(--koine-bg-card); border: var(--koine-hairline) solid var(--koine-separator);
  border-radius: var(--koine-radius-card, 10px); }
.koine-pane.koine-focused { outline-offset: -2px; }
.koine-top { display: flex; align-items: center; gap: 6px; flex: none; padding: 4px 10px;
  border-bottom: var(--koine-hairline) solid var(--koine-separator); font-size: 0.85rem;
  background: var(--koine-bg-grouped); user-select: none; -webkit-user-select: none; }
.koine-title { margin-left: auto; opacity: 0.7; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.koine-tabs { display: inline-flex; gap: 2px; }
.koine-tabs button { font: inherit; color: inherit; background: none; border: 0;
  border-bottom: 2px solid transparent; padding: 0 6px; cursor: pointer; }
.koine-tabs button[aria-selected="true"] { border-bottom-color: var(--koine-color-accent, #007aff); font-weight: 600; }
.koine-body { flex: 1 1 auto; min-height: 0; overflow: auto; padding: 10px; }
.koine-unavailable { display: flex; flex-direction: column; gap: 2px; align-items: center;
  justify-content: center; text-align: center; height: 100%; color: var(--koine-color-secondary, #6b7280); }
.koine-unavailable strong { color: var(--koine-color-primary, inherit); }
.koine-unavailable small { font-size: 0.8rem; }
@media (max-width: 700px) {
  /* A phone: the panes stack, each at its content's height (a surface pane
     at least a few rows), instead of side by side in a 70vh window. The
     shares still set flex-grow, which has nothing to grow into here. */
  .koine-layout { min-height: 0; }
  .koine-window-body { height: auto; min-height: 0; flex-direction: column; }
  .koine-window-body > *, .koine-cell, .koine-cell > * { flex-basis: auto; }
  .koine-horizontal { flex-direction: column; }
  .koine-pane { min-height: 160px; }
  .koine-body { max-height: 70vh; }
  .koine-tabs button { min-height: 44px; padding: 0 12px; }
}
"#;

// ---------------------------------------------------------------- pure helpers

/// The window a verb with no window acts on: `current`, else the first.
pub fn current_window(layout: &Layout) -> Option<WindowId> {
    layout
        .current
        .or_else(|| layout.windows.first().map(|window| window.id))
}

/// The windows to draw: the current one first, the rest in the layout's order.
pub fn windows_in_order(layout: &Layout) -> Vec<&Window> {
    let current = current_window(layout);
    let mut out: Vec<&Window> = layout
        .windows
        .iter()
        .filter(|window| Some(window.id) == current)
        .collect();
    out.extend(
        layout
            .windows
            .iter()
            .filter(|window| Some(window.id) != current),
    );
    out
}

/// The tile a window draws: the maximized one alone when the window has one
/// (zoom is a view state of the window, ADR-0031), otherwise the root.
pub fn shown_root(layout: &Layout, window: &Window) -> TileId {
    window
        .maximized
        .filter(|tile| layout.tile(*tile).is_some())
        .unwrap_or(window.root)
}

/// `flex-grow` of child `index`: its share. A share that is missing or not a
/// non-negative finite number is 1, so a malformed container still draws.
pub fn flex_grow(shares: &[f32], index: usize) -> f32 {
    shares
        .get(index)
        .copied()
        .filter(|share| share.is_finite() && *share >= 0.0)
        .unwrap_or(1.0)
}

/// Columns of a grid: the hint, or the nearest square when there is none
/// ("let the renderer choose").
pub fn grid_columns(columns: Option<u16>, len: usize) -> usize {
    match columns {
        Some(columns) if columns > 0 => usize::from(columns),
        _ => (1..=len).find(|n| n * n >= len).unwrap_or(1),
    }
}

/// What a pane's top edge calls it: `role · view_kind`, or the view kind.
pub fn pane_title(spec: &impress_layout::PaneSpec) -> String {
    match &spec.role {
        Some(role) => format!("{role} · {}", spec.view_kind),
        None => spec.view_kind.to_string(),
    }
}

/// A tab's label: the child pane's view kind, or its container kind and id.
pub fn tab_label(layout: &Layout, child: TileId) -> String {
    match layout.tile(child) {
        Some(Tile::Pane(spec)) => spec.view_kind.to_string(),
        Some(Tile::Container(container)) => {
            format!("{:?} {child}", container.kind()).to_lowercase()
        }
        None => format!("tile {child}"),
    }
}

/// What a pane's body draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneBody {
    /// A `surface` pane whose item is known: draw that surface.
    Surface(String),
    /// A `surface` pane whose `item` parameter is unbound.
    NoSurface,
    /// Any other view kind: the placeholder. The web draws what koine can.
    Unavailable {
        view_kind: String,
        kinds: Vec<String>,
        role: Option<String>,
    },
}

/// The body of pane `tile`. A surface pane's item is the swift host's
/// `singleItem ?? bindings["item"]`: the scope's item, resolved through the
/// layout's own bindings (a pinned item, or a channel's current selection).
pub fn pane_body(layout: &Layout, tile: TileId) -> Option<PaneBody> {
    let spec = layout.pane(tile)?;
    if spec.view_kind != impress_layout::ViewKindId::SURFACE {
        return Some(PaneBody::Unavailable {
            view_kind: spec.view_kind.to_string(),
            kinds: spec.query.kinds.clone(),
            role: spec.role.as_ref().map(ToString::to_string),
        });
    }
    let bindings = layout.bindings_for(tile);
    let item = match &spec.query.scope {
        Scope::Item {
            id: ItemRef::Id { id },
        } => Some(*id),
        Scope::Item {
            id: ItemRef::Param { name },
        } => bindings.get(name),
        _ => None,
    }
    .or_else(|| bindings.get("item"));
    Some(match item {
        Some(id) => PaneBody::Surface(id.to_string()),
        None => PaneBody::NoSurface,
    })
}

/// Whether the page has anything to draw: some window's pane is one the page
/// draws (a `surface` pane, bound or not), not a placeholder. The nav shows
/// the layout tab only then (A-D4, `session::layout_tab`).
pub fn drawable(layout: &Layout) -> bool {
    layout.panes().into_iter().any(|tile| {
        matches!(
            pane_body(layout, tile),
            Some(PaneBody::Surface(_) | PaneBody::NoSurface)
        )
    })
}

// ------------------------------------------------------------- the outline

/// The outline widget's id for one window.
pub fn outline_id(window: WindowId) -> String {
    format!("tiles-{window}")
}

/// What a container is called in the outline and the window's heading.
pub fn container_label(container: &Container) -> &'static str {
    match container {
        Container::Linear {
            dir: LinearDir::Horizontal,
            ..
        } => "split-h",
        Container::Linear {
            dir: LinearDir::Vertical,
            ..
        } => "split-v",
        Container::Tabs { .. } => "tabs",
        Container::Grid { .. } => "grid",
    }
}

/// A row's label: a pane's `<view kind> · <role>` (the view kind alone when
/// it has no role), a container's kind.
pub fn tile_label(layout: &Layout, tile: TileId) -> String {
    match layout.tile(tile) {
        Some(Tile::Pane(spec)) => match &spec.role {
            Some(role) => format!("{} · {role}", spec.view_kind),
            None => spec.view_kind.to_string(),
        },
        Some(Tile::Container(container)) => container_label(container).to_string(),
        None => format!("tile {tile}"),
    }
}

/// The outline's rows for one window: every container and pane under the
/// window's root, in container order, as `{id, parent, label}`. The root
/// container is not a row (its kind is in the window's heading): its children
/// are the outline's roots, so a drop among the roots (`to: null`) is a move
/// within it, which is what `tree_move_verb` reads `None` as. A window whose
/// root is a pane has that pane as its one row.
pub fn outline_rows(layout: &Layout, window: &Window) -> Vec<Value> {
    fn walk(layout: &Layout, tile: TileId, parent: Option<TileId>, out: &mut Vec<Value>) {
        // A tree that loops (it never should) is drawn once.
        if out.len() > 4096 || out.iter().any(|row| row["id"] == json!(tile.raw())) {
            return;
        }
        out.push(json!({
            "id": tile.raw(),
            "parent": parent.map(TileId::raw),
            "label": tile_label(layout, tile),
        }));
        if let Some(container) = layout.tile(tile).and_then(Tile::as_container) {
            for child in container.children() {
                walk(layout, *child, Some(tile), out);
            }
        }
    }
    let mut out = Vec::new();
    match layout.tile(window.root).and_then(Tile::as_container) {
        Some(root) => {
            for child in root.children() {
                walk(layout, *child, None, &mut out);
            }
        }
        None => walk(layout, window.root, None, &mut out),
    }
    out
}

/// The containers among a window's rows: the ones the outline can fold.
pub fn outline_containers(layout: &Layout, window: &Window) -> Vec<u64> {
    outline_rows(layout, window)
        .iter()
        .filter_map(|row| row["id"].as_u64())
        .filter(|id| matches!(layout.tile(TileId(*id)), Some(Tile::Container(_))))
        .collect()
}

/// The outline of one window as a `RenderTree`: one movable `tree` widget
/// whose expanded list is every container but the ones folded here.
pub fn outline_tree(layout: &Layout, window: &Window, folded: &BTreeSet<u64>) -> RenderTree {
    let rows = Value::Array(outline_rows(layout, window));
    let expanded: Vec<Value> = outline_containers(layout, window)
        .into_iter()
        .filter(|id| !folded.contains(id))
        .map(Value::from)
        .collect();
    let nodes = outline::build(&rows, &expanded, Fields::default());
    let id = outline_id(window.id);
    RenderTree {
        root: RenderNode {
            id: id.clone(),
            label: Some(format!("tiles of window {}", window.id)),
            help: Some(
                "drag a row to move its tile; x then p or P moves it from the keyboard".into(),
            ),
            node: RenderKind::Tree {
                nodes,
                bind: Some("expanded".into()),
                expanded,
                actions: Vec::new(),
                movable: true,
                // A tile has no name to change, and closing one is the `close` verb
                // (keys and the pane menu), not a row delete.
                renameable: false,
                deletable: false,
            },
        },
        focus_order: vec![id],
    }
}

/// The containers folded after a disclosure sent `expanded`: every container
/// of the window the list leaves out.
pub fn folded_after(layout: &Layout, window: &Window, expanded: &Value) -> BTreeSet<u64> {
    let open: BTreeSet<u64> = expanded
        .as_array()
        .map(|ids| ids.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default();
    outline_containers(layout, window)
        .into_iter()
        .filter(|id| !open.contains(id))
        .collect()
}

/// The layout verb an outline event asks for.
///
/// - `select` of `[id]`: focus that tile, or a container's first pane; none
///   when it is already focused.
/// - `move` of `{"ids", "to", "index"}`: `koine::gesture::tree_move_verb`.
///
/// Anything else (a disclosure is the page's own) is no verb.
pub fn outline_verb(layout: &Layout, focused: Option<u64>, event: &Event) -> Option<Verb> {
    match event.kind {
        EventKind::Select => {
            let tile = TileId(event.value.as_array()?.first()?.as_u64()?);
            let pane = match layout.tile(tile)? {
                Tile::Pane(_) => tile,
                Tile::Container(_) => layout.first_leaf(tile)?,
            };
            if focused == Some(pane.raw()) {
                return None;
            }
            gesture::layout_verb(
                &Gesture::Focus {
                    element: GestureElement::Tile { id: pane.raw() },
                },
                Some(pane.raw()),
            )
        }
        EventKind::Move => {
            let value = &event.value;
            let ids: Vec<u64> = value
                .get("ids")?
                .as_array()?
                .iter()
                .map(Value::as_u64)
                .collect::<Option<_>>()?;
            let to = match value.get("to") {
                None | Some(Value::Null) => None,
                Some(to) => Some(to.as_u64()?),
            };
            let index = usize::try_from(value.get("index")?.as_u64()?).ok()?;
            gesture::tree_move_verb(layout, &ids, to, index)
        }
        _ => None,
    }
}

/// `resize` of `container` after the divider following child `index` moved
/// so that the child before it measures `before` and the one after `after`.
/// `koine::gesture::resize_verb`, over the container's current shares.
pub fn resize_for_drag(
    layout: &Layout,
    container: u64,
    index: usize,
    before: f32,
    after: f32,
) -> Option<Verb> {
    let shares = layout.tile(TileId(container))?.as_container()?.shares()?;
    gesture::resize_verb(container, shares, index, before, after)
}

/// Whether `new` should replace `old`: a fetch that raced a newer one and lost
/// carries an older revision and is dropped.
pub fn newer(old: Option<&LayoutTree>, new: &LayoutTree) -> bool {
    match (old.and_then(|tree| tree.revision), new.revision) {
        (Some(old), Some(new)) => new >= old,
        _ => true,
    }
}

// ------------------------------------------------------------------- the board

/// A divider pressed at `start` (client x or y), waiting for its release.
/// `lengths` are the two neighbours measured at that moment; none until the
/// browser answers.
#[derive(Debug, Clone, PartialEq)]
struct Resize {
    container: u64,
    index: usize,
    horizontal: bool,
    start: f64,
    lengths: Option<(f32, f32)>,
}

/// What every component of the page reads and writes. All signals: `Copy`.
#[derive(Clone, Copy)]
struct Board {
    tree: Signal<Option<LayoutTree>>,
    resize: Signal<Option<Resize>>,
    session: Session,
}

impl Board {
    /// Show a fetched tree, unless a newer one is already shown.
    fn show(mut self, tree: LayoutTree) {
        let replace = {
            let shown = self.tree.peek();
            newer(shown.as_ref(), &tree) && shown.as_ref() != Some(&tree)
        };
        if replace {
            self.tree.set(Some(tree));
        }
    }

    /// Post a verb. A refusal goes to the status line; a success fetches the
    /// tree again.
    fn post(self, verb: Verb) {
        let token = self.session.token.peek().clone();
        spawn(async move {
            match api::layout_verb(&verb, token.as_ref()).await {
                Ok(_) => {
                    self.session.clear();
                    match api::layout_tree().await {
                        Ok(tree) => {
                            self.session
                                .saw_layout(LayoutAnswer::Tree(drawable(&tree.layout)));
                            self.show(tree);
                        }
                        Err(error) => self.session.refuse(error),
                    }
                }
                Err(error) => self.session.refuse(error),
            }
        });
    }

    /// A click on `tile` (a pane, or a tab naming it). A tile that already
    /// has focus needs no verb.
    fn focus(self, tile: u64) {
        if self
            .tree
            .peek()
            .as_ref()
            .is_some_and(|tree| tree.focused == Some(tile))
        {
            return;
        }
        let pane = GestureElement::Tile { id: tile };
        if let Some(verb) = gesture::layout_verb(&Gesture::Focus { element: pane }, Some(tile)) {
            self.post(verb);
        }
    }

    fn start_divider(mut self, container: u64, index: usize, horizontal: bool, start: f64) {
        self.resize.set(Some(Resize {
            container,
            index,
            horizontal,
            start,
            lengths: None,
        }));
        spawn(async move {
            let Some(measured) = dom::measure_neighbours(container, index, horizontal).await else {
                return;
            };
            let mut state = self.resize.write();
            if let Some(held) = state.as_mut() {
                if held.container == container && held.index == index {
                    held.lengths = Some(measured);
                }
            }
        });
    }

    /// Any release inside the page: a divider drag ends here.
    fn pointer_up(mut self, event: &dioxus::events::PointerEvent) {
        let held = std::mem::take(&mut *self.resize.write());
        let Some(Resize {
            container,
            index,
            horizontal,
            start,
            lengths: Some((before, after)),
        }) = held
        else {
            return;
        };
        let at = event.client_coordinates();
        let end = if horizontal { at.x } else { at.y };
        let delta = (end - start) as f32;
        if delta.abs() < MIN_RESIZE_PX {
            return;
        }
        let verb = self.tree.peek().as_ref().and_then(|tree| {
            resize_for_drag(
                &tree.layout,
                container,
                index,
                before + delta,
                after - delta,
            )
        });
        if let Some(verb) = verb {
            self.post(verb);
        }
    }
}

// ------------------------------------------------------------------ components

/// The route component: fetches the tree on mount and every [`POLL_MS`].
#[component]
pub fn LayoutPage() -> Element {
    let session = use_context::<Session>();
    let tree = use_signal(|| None);
    let resize = use_signal(|| None);
    let board = use_context_provider(move || Board {
        tree,
        resize,
        session,
    });
    // The poll's flag, as the surface page's wait loop has: a `Cell`, because a
    // signal's storage goes with the scope and a drop hook may run after it.
    let alive = use_hook(|| Rc::new(Cell::new(true)));
    use_drop({
        let alive = alive.clone();
        move || alive.set(false)
    });
    use_future(move || {
        let alive = alive.clone();
        async move {
            // The first answer, and the first answer after a failure, clear
            // the status line; a later quiet poll does not erase a refusal a
            // verb earned.
            let mut failed = true;
            while alive.get() {
                if !dom::page_hidden().await {
                    let answer = api::layout_tree().await;
                    if !alive.get() {
                        break;
                    }
                    // The nav's layout tab follows every answer (A-D4).
                    session.saw_layout(LayoutAnswer::of_tree(&answer));
                    match answer {
                        Ok(fetched) => {
                            board.show(fetched);
                            if failed {
                                session.clear();
                                failed = false;
                            }
                        }
                        Err(error) => {
                            session.refuse(error);
                            failed = true;
                        }
                    }
                }
                dom::sleep(POLL_MS).await;
            }
        }
    });
    rsx! {
        style { dangerous_inner_html: LAYOUT_CSS }
        h1 { "Layout" }
        p { class: "kw-note",
            "drag a row of a window's outline to move its tile (or x, then p or P); "
            "click a pane, a tab or a row to focus it; h and l step between panes. "
            "With a mouse, drag a divider to resize. The page asks for the tree every five seconds."
        }
        LayoutBody {}
    }
}

/// What the page says when the layout has panes but none it can draw.
pub const NOTHING_TO_LAY_OUT: &str =
    "Nothing to lay out here: this host has no view for any pane of this layout.";

/// The root of the drawing: every window, with the keys and the release.
#[component]
fn LayoutBody() -> Element {
    let session = use_context::<Session>();
    let board = use_context::<Board>();
    let vars = koine_dioxus::css_variables(session.style.read().sheet());
    let guard = board.tree.read();
    rsx! {
        div {
            class: "koine koine-layout",
            style: "{vars}",
            tabindex: "0",
            onkeydown: move |event| {
                if let Some(verb) = keys::on_layout_keydown(&event) {
                    board.post(verb);
                }
            },
            onpointerup: move |event| board.pointer_up(&event),
            match guard.as_ref() {
                None => rsx! { p { class: "kw-note", "loading the layout…" } },
                Some(tree) if tree.layout.windows.is_empty() => {
                    rsx! { p { class: "kw-note", "the layout has no windows" } }
                }
                Some(tree) => rsx! {
                    if !drawable(&tree.layout) {
                        p { class: "kw-note", "{NOTHING_TO_LAY_OUT}" }
                    }
                    for window in windows_in_order(&tree.layout) {
                        section {
                            key: "{window.id}",
                            class: "koine-window",
                            "data-koine-window": "{window.id}",
                            h2 {
                                "window {window.id}"
                                if current_window(&tree.layout) == Some(window.id) {
                                    " (current)"
                                }
                                if let Some(root) = tree.layout.tile(window.root).and_then(Tile::as_container) {
                                    " · {container_label(root)}"
                                }
                            }
                            TileOutline { window: window.id.raw() }
                            div { class: "koine-window-body",
                                TileView { tile: shown_root(&tree.layout, window).raw(), strip: None }
                            }
                        }
                    }
                },
            }
        }
    }
}

/// One window's tiles as the tree widget (A-D3): drawn by
/// `koine_dioxus::Surface` with the shared sheet, so a row moves the way a
/// row of any surface's tree moves. Its events are the page's: a row click
/// focuses the tile, a move posts [`outline_verb`]'s `move-tile`, a
/// disclosure folds a container here. The keys are `keys::on_keydown`'s over
/// the outline: `j`/`k` walk the rows, Enter focuses, `h`/`l` fold and
/// unfold, `x` then `p`/`P` moves.
#[component]
fn TileOutline(window: u64) -> Element {
    let session = use_context::<Session>();
    let board = use_context::<Board>();
    let window_id = WindowId::new(window);
    let id = outline_id(window_id);
    let mut folded = use_signal(BTreeSet::<u64>::new);
    let mut cursor = use_signal(Cursor::new);
    let mut focused = use_signal(|| None::<String>);
    let mut clipboard = use_signal(|| None::<(String, Value)>);
    let tree = use_memo(move || {
        let guard = board.tree.read();
        guard
            .as_ref()
            .and_then(|tree| {
                let window = tree.layout.window(window_id)?;
                Some(outline_tree(&tree.layout, window, &folded.read()))
            })
            .unwrap_or_else(keys::empty_tree)
    });
    // The focused tile is the active row, until j or k moves it.
    let mut seen_focus = use_signal(|| None::<u64>);
    {
        let id = id.clone();
        use_effect(move || {
            let focus = board.tree.read().as_ref().and_then(|tree| tree.focused);
            if *seen_focus.peek() != focus {
                seen_focus.set(focus);
                if let Some(tile) = focus {
                    cursor.write().set_active_row(&id, Value::from(tile));
                }
            }
        });
    }
    // Before the effect has run (and in a server render) the focused tile is
    // the active row.
    let active = use_memo(move || {
        let mut rows = keys::active_rows(&tree.read(), &cursor.read());
        let tree = tree.read();
        if let Some(widget) = tree.focus_order.first() {
            if !rows.contains_key(widget) {
                let focus = board.tree.read().as_ref().and_then(|shown| shown.focused);
                if let Some(focus) = focus {
                    rows.insert(widget.clone(), Value::from(focus));
                }
            }
        }
        rows
    });
    let sheet = use_memo(move || session.style.read().sheet().clone());

    let mut handle = move |event: Event| {
        if let Some(row) = keys::selected_row(&tree.peek(), &event) {
            cursor.write().set_active_row(&event.widget, row);
        }
        let guard = board.tree.peek();
        let Some(shown) = guard.as_ref() else { return };
        let Some(window) = shown.layout.window(window_id) else {
            return;
        };
        if event.kind == EventKind::Change {
            let next = folded_after(&shown.layout, window, &event.value);
            drop(guard);
            folded.set(next);
            return;
        }
        let verb = outline_verb(&shown.layout, shown.focused, &event);
        drop(guard);
        if let Some(verb) = verb {
            board.post(verb);
        }
    };

    let on_keydown = move |event: dioxus::events::KeyboardEvent| {
        let action = {
            let tree = tree.peek();
            let mut cursor = cursor.write();
            keys::on_keydown(&event, &tree, &mut cursor, false)
        };
        if action.claims() {
            // The layout page around the outline reads h and l as a pane
            // step; here they fold and unfold.
            event.stop_propagation();
        }
        match action {
            KeyAction::Focus => {
                let now = cursor.peek().focused_id(&tree.peek()).map(str::to_string);
                focused.set(now);
            }
            KeyAction::Event(event) => handle(event),
            KeyAction::Cut(widget, row) => clipboard.set(Some((widget, row))),
            paste @ (KeyAction::Paste(..) | KeyAction::PasteBeside(..)) => {
                let cut = clipboard.peek().clone();
                let event = cut.and_then(|cut| keys::paste_event(&tree.peek(), &cut, &paste));
                if let Some(event) = event {
                    clipboard.set(None);
                    handle(event);
                }
            }
            _ => {}
        }
    };

    rsx! {
        div { class: "koine-outline", "data-koine-outline": "{window}", onkeydown: on_keydown,
            Surface {
                tree,
                sheet,
                focused,
                on_event: move |event: Event| {
                    focused.set(Some(event.widget.clone()));
                    handle(event);
                },
                active,
            }
        }
    }
}

/// A tab strip: the container, its children's labels, and the active one.
#[derive(Debug, Clone, PartialEq)]
struct Strip {
    tabs: Vec<(u64, String)>,
    active: usize,
}

/// One tile and everything under it.
#[component]
fn TileView(tile: u64, strip: Option<Strip>) -> Element {
    let board = use_context::<Board>();
    let guard = board.tree.read();
    let Some(tree) = guard.as_ref() else {
        return rsx! {};
    };
    let layout = &tree.layout;
    match layout.tile(TileId(tile)) {
        None => rsx! { div { class: "koine-unavailable", "tile {tile} is not in the layout" } },
        Some(Tile::Pane(_)) => rsx! { PaneView { tile, strip } },
        Some(Tile::Container(Container::Linear {
            dir,
            children,
            shares,
        })) => {
            let horizontal = matches!(dir, LinearDir::Horizontal);
            let class = if horizontal {
                "koine-linear koine-horizontal"
            } else {
                "koine-linear koine-vertical"
            };
            rsx! {
                div { class: "{class}", "data-koine-container": "{tile}",
                    for (i, child) in children.iter().enumerate() {
                        // One keyed fragment per child: its divider (before it,
                        // so divider `i - 1` sits between children `i - 1` and `i`)
                        // and its cell.
                        Fragment { key: "{child}",
                            if i > 0 {
                                div {
                                    class: "koine-divider",
                                    role: "separator",
                                    "data-koine-container": "{tile}",
                                    "data-koine-divider": "{i - 1}",
                                    onpointerdown: move |event| {
                                        event.prevent_default();
                                        event.stop_propagation();
                                        let at = event.client_coordinates();
                                        let start = if horizontal { at.x } else { at.y };
                                        board.start_divider(tile, i - 1, horizontal, start);
                                    },
                                }
                            }
                            div {
                                class: "koine-cell",
                                style: "flex-grow: {flex_grow(shares, i)};",
                                TileView { tile: child.raw(), strip: None }
                            }
                        }
                    }
                }
            }
        }
        Some(Tile::Container(Container::Tabs { children, active })) => {
            let Some(shown) = active.or_else(|| children.first().copied()) else {
                return rsx! { div { class: "koine-unavailable", "empty tab strip {tile}" } };
            };
            let strip = Strip {
                tabs: children
                    .iter()
                    .map(|child| (child.raw(), tab_label(layout, *child)))
                    .collect(),
                active: children
                    .iter()
                    .position(|child| *child == shown)
                    .unwrap_or(0),
            };
            let shown_is_pane = layout.pane(shown).is_some();
            if shown_is_pane {
                // The strip shares the shown pane's top edge.
                rsx! {
                    TileView { key: "{shown}", tile: shown.raw(), strip: Some(strip) }
                }
            } else {
                rsx! {
                    div { class: "koine-tabbed", "data-koine-container": "{tile}",
                        div { class: "koine-top", TabBar { strip } }
                        TileView { key: "{shown}", tile: shown.raw(), strip: None }
                    }
                }
            }
        }
        Some(Tile::Container(Container::Grid { children, columns })) => {
            let columns = grid_columns(*columns, children.len());
            rsx! {
                div {
                    class: "koine-gridbox",
                    "data-koine-container": "{tile}",
                    style: "grid-template-columns: repeat({columns}, minmax(0, 1fr));",
                    for child in children.iter() {
                        TileView { key: "{child}", tile: child.raw(), strip: None }
                    }
                }
            }
        }
    }
}

/// A pane: its top edge (a tab strip when a `Tabs` container shows this
/// pane, and the title) and its body. A click focuses it; moving it is the
/// outline's.
#[component]
fn PaneView(tile: u64, strip: Option<Strip>) -> Element {
    let board = use_context::<Board>();
    let guard = board.tree.read();
    let Some(tree) = guard.as_ref() else {
        return rsx! {};
    };
    let Some(spec) = tree.layout.pane(TileId(tile)) else {
        return rsx! {};
    };
    let focused = tree.focused == Some(tile);
    let title = pane_title(spec);
    let body = pane_body(&tree.layout, TileId(tile));
    let class = if focused {
        "koine-pane koine-focused"
    } else {
        "koine-pane"
    };
    rsx! {
        div {
            class: "{class}",
            "data-koine-tile": "{tile}",
            "data-koine-focused": focused.then_some("true"),
            onclick: move |_| board.focus(tile),
            div { class: "koine-top",
                if let Some(strip) = strip {
                    TabBar { strip }
                }
                span { class: "koine-title", "{title}" }
            }
            div { class: "koine-body",
                match body {
                    Some(PaneBody::Surface(id)) => rsx! {
                        SurfaceView { key: "{id}", id: id.clone(), embedded: true }
                    },
                    Some(PaneBody::NoSurface) => rsx! {
                        div { class: "koine-unavailable",
                            strong { "No Surface" }
                            small { "This pane's item parameter is unbound, so it names no surface." }
                            small { "tile {tile}" }
                        }
                    },
                    Some(PaneBody::Unavailable { view_kind, kinds, role }) => {
                        let kinds = kinds.join(", ");
                        rsx! {
                            div { class: "koine-unavailable",
                                strong { "View Kind Unavailable" }
                                small { "This build has no view for “{view_kind}”." }
                                if !kinds.is_empty() {
                                    small { "query kinds: {kinds}" }
                                }
                                if let Some(role) = role {
                                    small { "role: {role}" }
                                }
                                small { "tile {tile}" }
                            }
                        }
                    }
                    None => rsx! {},
                }
            }
        }
    }
}

/// The tab labels of a strip, one button per child tile.
#[component]
fn TabBar(strip: Strip) -> Element {
    let board = use_context::<Board>();
    rsx! {
        span { class: "koine-tabs", role: "tablist",
            for (i , (child , label)) in strip.tabs.iter().cloned().enumerate() {
                button {
                    key: "{child}",
                    role: "tab",
                    "aria-selected": "{i == strip.active}",
                    "data-koine-tab": "{i}",
                    onclick: move |event| {
                        event.stop_propagation();
                        board.focus(child);
                    },
                    "{label}"
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
