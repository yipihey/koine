//! `/layout`: the window tree, performed (plan D7).
//!
//! The page fetches `GET /api/layout/tree` and draws every window's tile tree:
//! a `Linear` is a flex container whose children grow by their share, a `Tabs`
//! is a strip on the top edge of the pane it shows, a `Grid` is a CSS grid.
//! Every pane has the frame `koine-tui` draws as cells: a grip and a title on
//! the top edge, four edges, and a divider between siblings. A pane whose view
//! kind is `surface` draws that surface live ([`SurfaceView`], the surface
//! page's component, with its own wait loop). Any other kind is a placeholder
//! that names its query kinds, role and tile, as `kit-demo` does: imbib's list
//! and detail are Swift views, and the web shows what koine can show
//! (ADR-0032).
//!
//! # The page decides nothing about what a pointer means
//!
//! The browser does not hit-test cells. Each frame element carries what it is
//! (`data-koine-edge`, `data-koine-grip`, `data-koine-tab`,
//! `data-koine-divider`), and the handler on that element already knows its
//! [`Hit`]. From there:
//!
//! | Pointer | Decided by |
//! |---|---|
//! | click a pane | [`frame::verb_for_pointer`] (`Focus`), then `koine::gesture::layout_verb` |
//! | click a tab | the same, on the tab's child tile: a focus activates that tab |
//! | grip or title down, then up on another pane's edge or body | [`frame::drag_gesture`], then `koine::gesture::layout_verb`; a pane dropped on its own edge inside a tab strip asks `koine::gesture::tab_split_target` for the sibling, as that function says a host does |
//! | divider down, then up | `koine::gesture::resize_verb` (which calls `share_beside`) over the lengths the browser measured |
//! | `h` / `l` | [`keys::on_layout_keydown`], which is `keys::decide` |
//!
//! There is no wait route for the layout (the surface routes have one), so the
//! page fetches again after every verb it posts and every [`POLL_MS`] while the
//! tab is visible.

use std::cell::Cell;
use std::rc::Rc;

use dioxus::prelude::*;
use impress_layout::{Container, Layout, LinearDir, Tile, TileId, Verb, Window, WindowId};
use koine::frame::{self, Hit};
use koine::gesture::{self, Edge, Element as GestureElement, Gesture, Pointer};
use koine::pane_query::{ItemRef, Scope};

use crate::api::{self, LayoutTree};
use crate::dom;
use crate::keys;
use crate::session::Session;
use crate::surface_page::SurfaceView;

/// How often the page asks for the tree again while it is visible.
const POLL_MS: u32 = 5_000;

/// A divider drag shorter than this many pixels is a click, not a resize.
const MIN_RESIZE_PX: f32 = 2.0;

/// The page's own styles. The surface and the focus colour are `koine.css`.
const LAYOUT_CSS: &str = r#"
.koine-layout { display: flex; flex-direction: column; gap: 16px; outline: none; min-height: 60vh; }
.koine-window h2 { font-size: 0.95rem; font-weight: 600; margin: 0 0 6px; }
.koine-window-body { display: flex; height: 70vh; min-height: 280px; }
.koine-window-body > *, .koine-cell > * { flex: 1 1 auto; min-width: 0; min-height: 0; }
.koine-linear { display: flex; }
.koine-horizontal { flex-direction: row; }
.koine-vertical { flex-direction: column; }
.koine-cell { display: flex; flex: 1 1 0; min-width: 0; min-height: 0; }
.koine-gridbox { display: grid; gap: 6px; }
.koine-tabbed { display: flex; flex-direction: column; }
.koine-tabbed > :last-child { flex: 1 1 auto; min-height: 0; }
.koine-divider { flex: none; background: #8884; touch-action: none; user-select: none; }
.koine-horizontal > .koine-divider { width: 6px; cursor: col-resize; }
.koine-vertical > .koine-divider { height: 6px; cursor: row-resize; }
.koine-divider:hover { background: var(--koine-color-accent, #2563eb); }
.koine-pane { position: relative; display: flex; flex-direction: column; overflow: hidden;
  border: 1px solid #8886; border-radius: 4px; }
.koine-pane.koine-focused { outline-offset: -2px; }
.koine-top { display: flex; align-items: center; gap: 6px; flex: none; padding: 2px 8px;
  border-bottom: 1px solid #8884; font-size: 0.8rem; }
.koine-grip { cursor: grab; touch-action: none; user-select: none; padding: 0 4px; }
.koine-title { margin-left: auto; opacity: 0.7; white-space: nowrap; overflow: hidden;
  text-overflow: ellipsis; cursor: grab; touch-action: none; user-select: none; }
.koine-tabs { display: inline-flex; gap: 2px; }
.koine-tabs button { font: inherit; color: inherit; background: none; border: 0;
  border-bottom: 2px solid transparent; padding: 0 6px; cursor: pointer; }
.koine-tabs button[aria-selected="true"] { border-bottom-color: var(--koine-color-accent, #2563eb); font-weight: 600; }
.koine-body { flex: 1 1 auto; min-height: 0; overflow: auto; padding: 8px; }
.koine-edge { position: absolute; z-index: 2; pointer-events: none; }
.koine-edge[data-koine-edge="top"] { top: 0; left: 0; right: 0; height: 10px; }
.koine-edge[data-koine-edge="bottom"] { bottom: 0; left: 0; right: 0; height: 10px; }
.koine-edge[data-koine-edge="left"] { top: 0; bottom: 0; left: 0; width: 10px; }
.koine-edge[data-koine-edge="right"] { top: 0; bottom: 0; right: 0; width: 10px; }
.koine-layout[data-koine-dragging] .koine-edge { pointer-events: auto; }
.koine-layout[data-koine-dragging] .koine-edge:hover { background: var(--koine-color-accent, #2563eb); opacity: 0.35; }
.koine-unavailable { display: flex; flex-direction: column; gap: 2px; align-items: center;
  justify-content: center; text-align: center; height: 100%; color: var(--koine-color-secondary, #6b7280); }
.koine-unavailable strong { color: var(--koine-color-primary, inherit); }
.koine-unavailable small { font-size: 0.8rem; }
@media (max-width: 700px) {
  /* A phone: the panes stack, each at its content's height (a surface pane
     at least a few rows), instead of side by side in a 70vh window. The
     shares still set flex-grow, which has nothing to grow into here; a
     divider is drawn but a resize is a mouse gesture (README). */
  .koine-layout { min-height: 0; }
  .koine-window-body { height: auto; min-height: 0; flex-direction: column; }
  .koine-window-body > *, .koine-cell, .koine-cell > * { flex-basis: auto; }
  .koine-horizontal { flex-direction: column; }
  .koine-horizontal > .koine-divider { width: auto; height: 6px; cursor: row-resize; }
  .koine-pane { min-height: 160px; }
  .koine-body { max-height: 70vh; }
  .koine-tabs button { min-height: 44px; padding: 0 12px; }
  .koine-grip { padding: 8px; }
}
"#;

/// The four edges of a frame, with the word the DOM carries for each.
const EDGES: [(Edge, &str); 4] = [
    (Edge::Top, "top"),
    (Edge::Bottom, "bottom"),
    (Edge::Left, "left"),
    (Edge::Right, "right"),
];

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

/// The children of the tab strip that holds `tile`, and its index among them.
fn tab_siblings(layout: &Layout, tile: u64) -> Option<(Vec<u64>, usize)> {
    let parent = layout.parent_of(TileId(tile))?;
    let Some(Container::Tabs { children, .. }) = layout.tile(parent).and_then(Tile::as_container)
    else {
        return None;
    };
    let ids: Vec<u64> = children.iter().map(|child| child.raw()).collect();
    let index = ids.iter().position(|id| *id == tile)?;
    Some((ids, index))
}

/// The verb for dropping pane `tile`'s handle (`from`) on `target` at `to`.
///
/// The gesture is [`frame::drag_gesture`], the verb `koine::gesture::layout_verb`.
/// A pane dropped on its own edge is no verb until the host names the
/// sibling it splits beside; for a pane in a tab strip that sibling is
/// `koine::gesture::tab_split_target`'s, and the gesture is asked again.
pub fn drop_verb(layout: &Layout, from: Hit, to: Hit, tile: u64, target: u64) -> Option<Verb> {
    let dragged = GestureElement::Tile { id: tile };
    let landed = GestureElement::Tile { id: target };
    let gesture = frame::drag_gesture(from, to, &dragged, &landed, tile == target)?;
    if let Some(verb) = gesture::layout_verb(&gesture, Some(target)) {
        return Some(verb);
    }
    let Gesture::Drag { placement, .. } = gesture else {
        return None;
    };
    let (children, index) = tab_siblings(layout, tile)?;
    let sibling = gesture::tab_split_target(&children, index, placement)?;
    gesture::layout_verb(
        &Gesture::Drag {
            element: dragged,
            target: GestureElement::Tile { id: sibling },
            placement,
        },
        Some(sibling),
    )
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

/// A pointer press that is waiting for its release.
#[derive(Debug, Clone, PartialEq)]
enum DragState {
    /// The grip or title of `tile` is down; the drop names the target.
    Handle { from: Hit, tile: u64 },
    /// A divider is down at `start` (client x or y). `lengths` are the two
    /// neighbours measured at that moment; none until the browser answers.
    Divider {
        container: u64,
        index: usize,
        horizontal: bool,
        start: f64,
        lengths: Option<(f32, f32)>,
    },
}

/// What every component of the page reads and writes. All signals: `Copy`.
#[derive(Clone, Copy)]
struct Board {
    tree: Signal<Option<LayoutTree>>,
    drag: Signal<Option<DragState>>,
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
                        Ok(tree) => self.show(tree),
                        Err(error) => self.session.refuse(error),
                    }
                }
                Err(error) => self.session.refuse(error),
            }
        });
    }

    /// A press on `tile`'s frame. A tile that already has focus needs no verb.
    fn press(self, at: Hit, tile: u64) {
        if self
            .tree
            .peek()
            .as_ref()
            .is_some_and(|tree| tree.focused == Some(tile))
        {
            return;
        }
        let pane = GestureElement::Tile { id: tile };
        if let Some(verb) = frame::verb_for_pointer(at, Pointer::Primary, &pane, &[]) {
            self.post(verb);
        }
    }

    fn start_handle(mut self, from: Hit, tile: u64) {
        self.drag.set(Some(DragState::Handle { from, tile }));
    }

    /// A handle was released over `target`'s `to`.
    fn drop_on(mut self, target: u64, to: Hit) {
        let handle = match &*self.drag.peek() {
            Some(DragState::Handle { from, tile }) => Some((*from, *tile)),
            _ => None,
        };
        let Some((from, tile)) = handle else { return };
        self.drag.set(None);
        let verb = self
            .tree
            .peek()
            .as_ref()
            .and_then(|tree| drop_verb(&tree.layout, from, to, tile, target));
        if let Some(verb) = verb {
            self.post(verb);
        }
    }

    fn start_divider(mut self, container: u64, index: usize, horizontal: bool, start: f64) {
        self.drag.set(Some(DragState::Divider {
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
            let mut state = self.drag.write();
            if let Some(DragState::Divider {
                container: held,
                index: held_index,
                lengths,
                ..
            }) = state.as_mut()
            {
                if *held == container && *held_index == index {
                    *lengths = Some(measured);
                }
            }
        });
    }

    /// Any release inside the page: a divider drag ends here; a handle that
    /// was not dropped on a pane is forgotten.
    fn pointer_up(mut self, event: &dioxus::events::PointerEvent) {
        let drag = std::mem::take(&mut *self.drag.write());
        let Some(DragState::Divider {
            container,
            index,
            horizontal,
            start,
            lengths: Some((before, after)),
        }) = drag
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
    let drag = use_signal(|| None);
    let board = use_context_provider(move || Board {
        tree,
        drag,
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
            "drag a pane's ≡ onto another pane's edge to move it; drag a divider to resize; "
            "click a pane or a tab to focus it; h and l step between panes. "
            "The page asks for the tree every five seconds."
        }
        LayoutBody {}
    }
}

/// The root of the drawing: every window, with the keys and the release.
#[component]
fn LayoutBody() -> Element {
    let session = use_context::<Session>();
    let board = use_context::<Board>();
    let vars = koine_dioxus::css_variables(session.style.read().sheet());
    let dragging = matches!(&*board.drag.read(), Some(DragState::Handle { .. }));
    let guard = board.tree.read();
    rsx! {
        div {
            class: "koine koine-layout",
            style: "{vars}",
            tabindex: "0",
            "data-koine-dragging": dragging.then_some("true"),
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
                            }
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
                // The strip shares the shown pane's top edge, after its grip.
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

/// A pane with its frame: four edges, the grip, the title (and a tab strip
/// when a `Tabs` container shows this pane), and the body.
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
            onclick: move |_| board.press(Hit::Body, tile),
            onpointerup: move |_| board.drop_on(tile, Hit::Body),
            for (edge , name) in EDGES {
                div {
                    key: "{name}",
                    class: "koine-edge",
                    "data-koine-edge": name,
                    onpointerup: move |event| {
                        event.stop_propagation();
                        board.drop_on(tile, Hit::Frame(edge));
                    },
                }
            }
            div { class: "koine-top",
                span {
                    class: "koine-grip",
                    "data-koine-grip": "",
                    title: "drag onto another pane's edge to move this pane",
                    onpointerdown: move |event| {
                        event.prevent_default();
                        event.stop_propagation();
                        board.start_handle(Hit::Grip, tile);
                    },
                    "≡"
                }
                if let Some(strip) = strip {
                    TabBar { strip }
                }
                span {
                    class: "koine-title",
                    onpointerdown: move |event| {
                        event.prevent_default();
                        event.stop_propagation();
                        board.start_handle(Hit::Title, tile);
                    },
                    "{title}"
                }
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
                        board.press(Hit::Title, child);
                    },
                    "{label}"
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
