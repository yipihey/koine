//! The layout value: the arena, the tree walks, reference resolution and
//! normalization (ADR-0031 D4, D5).

use std::collections::{BTreeMap, BTreeSet};

use impress_pane_query::{Bindings, ItemId};
use serde::{Deserialize, Serialize};

use crate::channels::ChannelState;
use crate::error::LayoutError;
use crate::ids::{ChannelId, Role, TileId, ViewKindId, WindowId};
use crate::spec::{PaneSpec, ParamSource};
use crate::tree::{Container, ContainerKind, LinearDir, Tile, Window};
use crate::verb::{Direction, PaneRef};

/// A malformed value (hand-edited JSON, a future version, a merge) must not be
/// able to make a tree walk recurse forever. Every walk carries a visited set
/// as well; this is the second belt.
const MAX_DEPTH: usize = 64;

/// The whole layout: windows over one shared arena of tiles, plus the channel
/// state the panes coordinate through.
///
/// Everything in this crate is a pure function of this value. There is no I/O
/// and no store access: persisting it is the `layout-service`'s job (L3), and
/// the value is the payload of an `impress/ui/layout` item (ADR-0019 D1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(from = "LayoutWire")]
pub struct Layout {
    pub windows: Vec<Window>,
    /// The arena. Tile ids are unique across windows.
    pub tiles: BTreeMap<TileId, Tile>,
    #[serde(default)]
    pub channels: ChannelState,
    /// The tile-id allocator. Ids are never reused within a layout.
    #[serde(default)]
    pub next_tile: u64,
    /// The window-id allocator. Windows come and go now that a pane can be
    /// detached into one and a window closes when its last pane does, so ids
    /// are never reused either — a `WindowId` in an operation log means one
    /// window for the life of the layout.
    #[serde(default)]
    pub next_window: u64,
    /// The window a verb with no explicit window acts on — the key window.
    ///
    /// Set wherever focus moves between windows ([`Layout::focus_tile`]'s
    /// callers: focus, a directional step, a split, a detach), repaired by
    /// [`Layout::normalize`] when the window it names is gone. `None` means
    /// "the first window", which is what a single-window layout (every
    /// preset) always is, so a layout that never had two windows serializes
    /// exactly as before this field existed. Before it, "current" was "the
    /// first window with a focused leaf" — and normalization gives every
    /// window one, so every verb acted on window 0 even after focus had
    /// followed a detached pane into its own window (review RL-L5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<WindowId>,
}

/// What [`Layout`] deserializes *through*, so that JSON written before
/// `next_window` existed still loads with a sound allocator: the counter
/// starts above every window the value actually holds, rather than at zero,
/// which would hand the next detached pane an id that is already taken.
#[derive(Deserialize)]
struct LayoutWire {
    #[serde(default)]
    windows: Vec<Window>,
    #[serde(default)]
    tiles: BTreeMap<TileId, Tile>,
    #[serde(default)]
    channels: ChannelState,
    #[serde(default)]
    next_tile: u64,
    #[serde(default)]
    next_window: Option<u64>,
    #[serde(default)]
    current: Option<WindowId>,
}

impl From<LayoutWire> for Layout {
    fn from(wire: LayoutWire) -> Self {
        let floor = wire
            .windows
            .iter()
            .map(|w| w.id.raw())
            .max()
            .map(|max| max + 1)
            .unwrap_or(1);
        Layout {
            windows: wire.windows,
            tiles: wire.tiles,
            channels: wire.channels,
            next_tile: wire.next_tile,
            next_window: wire.next_window.unwrap_or(floor).max(floor),
            current: wire.current,
        }
    }
}

impl Default for Layout {
    fn default() -> Self {
        Self::empty()
    }
}

/// Where a tile sits: which slot holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Slot {
    /// The root of the window at this index in [`Layout::windows`].
    Root(usize),
    /// Child `index` of this container.
    Child(TileId, usize),
}

impl Layout {
    /// A layout with no windows. Only useful as a starting point for a builder
    /// or a deserializer; every verb needs at least one window.
    pub fn empty() -> Self {
        Self {
            windows: Vec::new(),
            tiles: BTreeMap::new(),
            channels: ChannelState::new(),
            next_tile: 0,
            next_window: 1,
            current: None,
        }
    }

    /// One window, one pane, focused. What every preset starts from.
    pub fn new_single_pane(spec: PaneSpec) -> Self {
        let mut layout = Self::empty();
        let root = layout.alloc_tile();
        layout.tiles.insert(root, Tile::Pane(spec));
        let id = layout.alloc_window_id();
        let mut window = Window::new(id, root);
        window.focused = Some(root);
        layout.windows.push(window);
        layout
    }

    // ---------------------------------------------------------------- arena

    /// Reserve a fresh tile id. Ids are never reused within a layout.
    pub fn alloc_tile(&mut self) -> TileId {
        self.next_tile += 1;
        TileId::new(self.next_tile)
    }

    /// Put a pane in the arena and return its id. It belongs to no window
    /// until a container (or [`Layout::add_window`]) refers to it.
    pub fn insert_pane(&mut self, spec: PaneSpec) -> TileId {
        let id = self.alloc_tile();
        self.tiles.insert(id, Tile::Pane(spec));
        id
    }

    /// Put a container in the arena and return its id.
    pub fn insert_container(&mut self, container: Container) -> TileId {
        let id = self.alloc_tile();
        self.tiles.insert(id, Tile::Container(container));
        id
    }

    /// Reserve a fresh window id. Like tile ids, never reused.
    pub fn alloc_window_id(&mut self) -> WindowId {
        let floor = self.windows.iter().map(|w| w.id.raw()).max().unwrap_or(0) + 1;
        self.next_window = self.next_window.max(floor);
        let id = WindowId::new(self.next_window);
        self.next_window += 1;
        id
    }

    /// Add a window whose root is `root`, focused on its first leaf.
    pub fn add_window(&mut self, root: TileId) -> WindowId {
        let id = self.alloc_window_id();
        let mut window = Window::new(id, root);
        window.focused = self.first_leaf(root);
        self.windows.push(window);
        id
    }

    pub fn window(&self, id: WindowId) -> Option<&Window> {
        self.windows.iter().find(|w| w.id == id)
    }

    pub fn window_mut(&mut self, id: WindowId) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    /// The window a verb with no explicit window acts on: [`Layout::current`]
    /// when it names a window that exists, else the first window that has
    /// focus, else the first window at all.
    pub fn current_window(&self) -> Result<WindowId, LayoutError> {
        if let Some(current) = self.current.filter(|id| self.window(*id).is_some()) {
            return Ok(current);
        }
        self.windows
            .iter()
            .find(|w| w.focused.is_some())
            .or_else(|| self.windows.first())
            .map(|w| w.id)
            .ok_or(LayoutError::UnknownWindow {
                window: WindowId::new(0),
            })
    }

    pub fn tile(&self, id: TileId) -> Option<&Tile> {
        self.tiles.get(&id)
    }

    pub fn pane(&self, id: TileId) -> Option<&PaneSpec> {
        self.tiles.get(&id).and_then(Tile::as_pane)
    }

    pub fn pane_mut(&mut self, id: TileId) -> Option<&mut PaneSpec> {
        self.tiles.get_mut(&id).and_then(Tile::as_pane_mut)
    }

    pub(crate) fn expect_pane_mut(&mut self, id: TileId) -> Result<&mut PaneSpec, LayoutError> {
        match self.tiles.get_mut(&id) {
            Some(Tile::Pane(p)) => Ok(p),
            Some(Tile::Container(_)) => Err(LayoutError::NotAPane { tile: id }),
            None => Err(LayoutError::UnknownTile { tile: id }),
        }
    }

    pub(crate) fn container(&self, id: TileId) -> Option<&Container> {
        self.tiles.get(&id).and_then(Tile::as_container)
    }

    pub(crate) fn container_mut(&mut self, id: TileId) -> Option<&mut Container> {
        self.tiles.get_mut(&id).and_then(Tile::as_container_mut)
    }

    // ------------------------------------------------------------ tree walk

    /// The parent container of `tile`, if it has one (a window root does not).
    pub fn parent_of(&self, tile: TileId) -> Option<TileId> {
        self.tiles.iter().find_map(|(id, t)| match t {
            Tile::Container(c) if c.children().contains(&tile) => Some(*id),
            _ => None,
        })
    }

    pub(crate) fn slot_of(&self, tile: TileId) -> Option<Slot> {
        if let Some(index) = self.windows.iter().position(|w| w.root == tile) {
            return Some(Slot::Root(index));
        }
        let parent = self.parent_of(tile)?;
        let index = self.container(parent)?.index_of(tile)?;
        Some(Slot::Child(parent, index))
    }

    /// The window whose tree contains `tile`.
    pub fn window_of(&self, tile: TileId) -> Option<WindowId> {
        let mut cursor = tile;
        for _ in 0..MAX_DEPTH {
            if let Some(w) = self.windows.iter().find(|w| w.root == cursor) {
                return Some(w.id);
            }
            cursor = self.parent_of(cursor)?;
        }
        None
    }

    /// Whether `ancestor` is `tile` or contains it.
    pub fn is_ancestor(&self, ancestor: TileId, tile: TileId) -> bool {
        let mut cursor = tile;
        for _ in 0..MAX_DEPTH {
            if cursor == ancestor {
                return true;
            }
            match self.parent_of(cursor) {
                Some(p) => cursor = p,
                None => return false,
            }
        }
        false
    }

    /// The panes of one window, in tree order (depth first, children in
    /// order). This is the order the h / l grammar and `PaneFocusCycler` walk.
    pub fn leaves(&self, window: WindowId) -> Vec<TileId> {
        match self.window(window) {
            Some(w) => self.leaves_of(w.root),
            None => Vec::new(),
        }
    }

    /// The panes of one subtree, in tree order.
    pub fn leaves_of(&self, root: TileId) -> Vec<TileId> {
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        self.collect_leaves(root, &mut out, &mut seen, 0);
        out
    }

    fn collect_leaves(
        &self,
        tile: TileId,
        out: &mut Vec<TileId>,
        seen: &mut BTreeSet<TileId>,
        depth: usize,
    ) {
        if depth > MAX_DEPTH || !seen.insert(tile) {
            return;
        }
        match self.tiles.get(&tile) {
            Some(Tile::Pane(_)) => out.push(tile),
            Some(Tile::Container(c)) => {
                for child in c.children().to_vec() {
                    self.collect_leaves(child, out, seen, depth + 1);
                }
            }
            None => {}
        }
    }

    /// The pane a container shows first: a tab strip's active tab, otherwise
    /// the first child, recursively. This is what "step into that sibling"
    /// means for direction resolution and what focus lands on.
    pub fn first_leaf(&self, tile: TileId) -> Option<TileId> {
        let mut cursor = tile;
        for _ in 0..MAX_DEPTH {
            match self.tiles.get(&cursor)? {
                Tile::Pane(_) => return Some(cursor),
                Tile::Container(c) => {
                    let next = match c {
                        Container::Tabs { children, active } => active
                            .filter(|a| children.contains(a))
                            .or_else(|| children.first().copied()),
                        Container::Linear { children, .. } | Container::Grid { children, .. } => {
                            children.first().copied()
                        }
                    };
                    cursor = next?;
                }
            }
        }
        None
    }

    /// Make `tile` visible: every [`Container::Tabs`] between it and its
    /// window's root has its `active` set to the child on the path down to
    /// `tile`.
    ///
    /// Focus that cannot be seen is not focus. Without this, focusing a pane
    /// buried under an inactive tab — by chord, by agent verb, or because a
    /// split or a close handed focus to a neighbour inside a tab strip — would
    /// type into a pane the user is not looking at. So every focus assignment
    /// goes through here, and [`Layout::normalize`] runs it once more for the
    /// verbs that restructure the tree *around* the focused pane without
    /// touching focus at all (`Swap`, `SetContainerKind`).
    ///
    /// Idempotent, and a no-op for a tile with no `Tabs` ancestor.
    pub fn reveal(&mut self, tile: TileId) {
        let mut cursor = tile;
        for _ in 0..MAX_DEPTH {
            let Some(parent) = self.parent_of(cursor) else {
                return;
            };
            if let Some(Container::Tabs { children, active }) = self.container_mut(parent) {
                if children.contains(&cursor) {
                    *active = Some(cursor);
                }
            }
            cursor = parent;
        }
    }

    /// Every pane in the layout, window by window, in tree order.
    pub fn panes(&self) -> Vec<TileId> {
        self.windows
            .iter()
            .flat_map(|w| self.leaves_of(w.root))
            .collect()
    }

    // ------------------------------------------------------------- resolve

    /// Resolve a pane reference against one window.
    ///
    /// * `Id` — checked for existence only, so a reference into another
    ///   window (a detached PDF) resolves.
    /// * `Role` — the first pane of *this window* carrying the role, in tree
    ///   order. Roles are kept unique per window by
    ///   [`crate::Verb::SetRole`], so "first" is "the".
    /// * `Focused` — the window's focused leaf.
    /// * `Direction` — a step from the focused leaf:
    ///   - `Next` / `Prev` walk [`Layout::leaves`] and **wrap**, which is what
    ///     `PaneFocusCycler` does today.
    ///   - `Left` / `Right` / `Up` / `Down` walk *up* from the focused leaf to
    ///     the nearest `Linear` ancestor of the matching axis that has a
    ///     sibling on that side, then step to that sibling and descend to its
    ///     first leaf ([`Layout::first_leaf`], so a tab strip yields its
    ///     visible tab). This is a tree rule, not a geometric one: it needs no
    ///     rendered frames, so it gives the same answer headlessly in a Tier A
    ///     test as it does on screen — and for the shapes the presets build
    ///     (a linear row of panes, optionally split) it *is* the geometric
    ///     answer. When there is no such sibling the reference resolves to the
    ///     focused leaf itself: h at the leftmost pane stays put rather than
    ///     failing.
    pub fn resolve(&self, window: WindowId, reference: &PaneRef) -> Result<TileId, LayoutError> {
        match reference {
            PaneRef::Id { tile } => {
                if self.tiles.contains_key(tile) {
                    Ok(*tile)
                } else {
                    Err(LayoutError::UnknownTile { tile: *tile })
                }
            }
            PaneRef::Role { role } => self
                .leaves(window)
                .into_iter()
                .find(|id| self.pane(*id).and_then(|p| p.role.as_ref()) == Some(role))
                .ok_or_else(|| LayoutError::NoPaneWithRole { role: role.clone() }),
            PaneRef::Focused => self.focused_leaf(window),
            PaneRef::Direction { direction } => {
                let from = self.focused_leaf(window)?;
                Ok(self.step(window, from, *direction))
            }
        }
    }

    fn focused_leaf(&self, window: WindowId) -> Result<TileId, LayoutError> {
        let w = self
            .window(window)
            .ok_or(LayoutError::UnknownWindow { window })?;
        w.focused.ok_or(LayoutError::NoFocus { window })
    }

    /// One directional step from `from`. See [`Layout::resolve`] for the rule.
    pub fn step(&self, window: WindowId, from: TileId, direction: Direction) -> TileId {
        match direction.axis() {
            None => {
                let leaves = self.leaves(window);
                if leaves.is_empty() {
                    return from;
                }
                let index = leaves.iter().position(|l| *l == from).unwrap_or(0);
                let next = if direction.is_forward() {
                    (index + 1) % leaves.len()
                } else {
                    (index + leaves.len() - 1) % leaves.len()
                };
                leaves[next]
            }
            Some(axis) => {
                let mut cursor = from;
                for _ in 0..MAX_DEPTH {
                    let Some(parent) = self.parent_of(cursor) else {
                        return from;
                    };
                    let Some(container) = self.container(parent) else {
                        return from;
                    };
                    if let Container::Linear { dir, children, .. } = container {
                        if *dir == axis {
                            if let Some(index) = children.iter().position(|c| *c == cursor) {
                                let sibling = if direction.is_forward() {
                                    children.get(index + 1).copied()
                                } else {
                                    index.checked_sub(1).and_then(|i| children.get(i).copied())
                                };
                                if let Some(sibling) = sibling {
                                    return self.first_leaf(sibling).unwrap_or(from);
                                }
                            }
                        }
                    }
                    cursor = parent;
                }
                from
            }
        }
    }

    // ------------------------------------------------------------ bindings

    /// Resolve a pane's parameters to item ids (ADR-0031 D3).
    ///
    /// `Channel` takes the first id of that channel's current selection for
    /// the parameter's declared kind; `Fixed` takes its pinned id; `Default`
    /// yields no binding at all, which is how a plot with no colormap pane
    /// still renders. An unfilled *required* parameter is likewise absent
    /// here — whether that is an empty state or an error is the query
    /// compiler's call (`impress_core::pane_query::compile`, in the crate
    /// that lowers this query onto a store), not the layout's.
    pub fn bindings_for(&self, pane: TileId) -> Bindings {
        let mut bindings = Bindings::new();
        let Some(spec) = self.pane(pane) else {
            return bindings;
        };
        let default_channel = self.default_channel_for(pane);
        for binding in &spec.params {
            let value: Option<ItemId> = match &binding.source {
                ParamSource::Fixed { item } => Some(*item),
                ParamSource::Channel { channel } => self
                    .channels
                    .current(channel.resolve(default_channel), &binding.decl.kind),
                ParamSource::Default => None,
            };
            if let Some(id) = value {
                bindings.values.insert(binding.decl.name.clone(), id);
            }
        }
        bindings
    }

    pub(crate) fn default_channel_for(&self, tile: TileId) -> ChannelId {
        self.window_of(tile)
            .and_then(|w| self.window(w))
            .map(|w| w.default_channel)
            .unwrap_or(ChannelId::ONE)
    }

    /// The panes whose bindings would change if `channel`'s selection for
    /// `kind` changed — i.e. exactly the panes the renderer must re-run
    /// (ADR-0031 D9's prerequisite, from the layout side).
    ///
    /// `Follow` on either side is resolved against each pane's own window, so
    /// a pane bound to `follow` in a window whose default is 2 is reported for
    /// channel 2 and not for channel 1.
    pub fn affected_panes(&self, channel: ChannelId, kind: &str) -> Vec<TileId> {
        let mut out = Vec::new();
        for window in &self.windows {
            let wanted = channel.resolve(window.default_channel);
            for tile in self.leaves_of(window.root) {
                let Some(spec) = self.pane(tile) else {
                    continue;
                };
                let hit = spec.params.iter().any(|b| {
                    b.decl.kind == kind
                        && matches!(&b.source, ParamSource::Channel { channel }
                            if channel.resolve(window.default_channel) == wanted)
                });
                if hit {
                    out.push(tile);
                }
            }
        }
        out
    }

    /// The panes whose view kind is not in `available` (ADR-0031 D4): the
    /// renderer substitutes a placeholder and **keeps the spec**, so the pane
    /// comes back when the platform that can render it opens the layout. A
    /// read-only report; this crate never rewrites a spec for it.
    pub fn placeholder_for_missing_view_kinds(&self, available: &[ViewKindId]) -> Vec<TileId> {
        self.panes()
            .into_iter()
            .filter(|tile| {
                self.pane(*tile)
                    .map(|spec| !available.contains(&spec.view_kind))
                    .unwrap_or(false)
            })
            .collect()
    }

    /// The pane carrying `role` in the current window — exactly what
    /// [`PaneRef::Role`] resolves to for a verb with no explicit window.
    ///
    /// One rule for roles (review RL-L19): [`crate::Verb::SetRole`] keeps a
    /// role unique *per window*, `PaneRef::Role` resolves in one window, and
    /// so does this. It used to scan every window in tile-id order, so with a
    /// detached window holding its own `detail`, ⌘0 could collapse a pane in a
    /// window the user was not looking at.
    pub fn pane_with_role(&self, role: &Role) -> Option<TileId> {
        let window = self.current_window().ok()?;
        self.pane_with_role_in(window, role)
    }

    /// A `(role, window)` this layout holds twice that `before` did not —
    /// the per-window uniqueness ADR-0031 D5 relies on, checked against the
    /// value a change started from so that a stored tree that already broke
    /// it (hand-edited, merged) is not made unusable.
    pub fn new_duplicate_role(&self, before: &Layout) -> Option<(Role, WindowId)> {
        let before = before.duplicate_roles();
        self.duplicate_roles()
            .into_iter()
            .find(|pair| !before.contains(pair))
    }

    fn duplicate_roles(&self) -> BTreeSet<(Role, WindowId)> {
        let mut out = BTreeSet::new();
        for window in &self.windows {
            let mut seen = BTreeSet::new();
            for leaf in self.leaves(window.id) {
                if let Some(role) = self.pane(leaf).and_then(|p| p.role.as_ref()) {
                    if !seen.insert(role.clone()) {
                        out.insert((role.clone(), window.id));
                    }
                }
            }
        }
        out
    }

    /// The pane carrying `role` in `window`, in tree order.
    pub fn pane_with_role_in(&self, window: WindowId, role: &Role) -> Option<TileId> {
        self.resolve(window, &PaneRef::Role { role: role.clone() })
            .ok()
    }

    // ----------------------------------------------------------- normalize

    /// Bring the tree back to canonical shape. Run after every verb; safe to
    /// run at any time; **idempotent**.
    ///
    /// * empty containers are pruned;
    /// * single-child containers collapse into their child;
    /// * a `Linear` directly inside a `Linear` of the same direction is joined
    ///   into it, its children's shares scaled to the share it occupied;
    /// * `Tabs` directly inside `Tabs` are flattened;
    /// * unreachable tiles are garbage-collected;
    /// * a tab strip's `active`, a linear's `shares`, a window's `focused` and
    ///   `maximized` are repaired if they point at something that is gone;
    /// * every tab strip above a window's focused pane is re-pointed at it
    ///   ([`Layout::reveal`]), so focus is never hidden behind another tab.
    ///
    /// The last pane of a window is never removed: a window whose tree has
    /// collapsed to one pane keeps that pane *as* its root. A window left with
    /// no panes at all is dropped — the refusal that protects the last pane of
    /// the *layout* lives in the verbs ([`crate::LayoutError::CannotCloseLastPane`]).
    pub fn normalize(&mut self) {
        let mut survivors = Vec::with_capacity(self.windows.len());
        for mut window in std::mem::take(&mut self.windows) {
            // A window left with no tiles never persists — whether it was
            // emptied by a move, closed pane by pane, or arrived empty in a
            // malformed value.
            if let Some(root) = self.simplify(window.root, 0) {
                window.root = root;
                survivors.push(window);
            }
        }
        self.windows = survivors;
        self.gc();
        self.repair_windows();
        self.forget_stale_collapses();
    }

    /// A pane's `collapsed_share` means "hidden, and this is what showing it
    /// restores". Once its share is not hidden any more — dragged open,
    /// resized, moved out of its split, restored by an undo — the memory is
    /// dropped, so a later collapse remembers the new width.
    fn forget_stale_collapses(&mut self) {
        let stale: Vec<TileId> = self
            .tiles
            .iter()
            .filter_map(|(id, tile)| match tile {
                Tile::Pane(spec) if spec.collapsed_share.is_some() => Some(*id),
                _ => None,
            })
            .filter(|id| {
                let share = self
                    .parent_of(*id)
                    .and_then(|parent| match self.container(parent) {
                        Some(c @ Container::Linear { .. }) => {
                            c.index_of(*id).map(|index| sane_share(c.share_at(index)))
                        }
                        _ => None,
                    });
                !share.is_some_and(crate::shares::is_hidden)
            })
            .collect();
        for id in stale {
            if let Some(Tile::Pane(spec)) = self.tiles.get_mut(&id) {
                spec.collapsed_share = None;
            }
        }
    }

    /// Simplify one subtree, returning the id that should stand in its place
    /// (which may be a child's, when this container collapsed) or `None` when
    /// the subtree disappeared entirely.
    fn simplify(&mut self, tile: TileId, depth: usize) -> Option<TileId> {
        if depth > MAX_DEPTH {
            return Some(tile);
        }
        let existing = self.tiles.remove(&tile)?;
        let container = match existing {
            Tile::Pane(spec) => {
                self.tiles.insert(tile, Tile::Pane(spec));
                return Some(tile);
            }
            Tile::Container(container) => container,
        };

        let kind = container.kind();
        let columns = match &container {
            Container::Grid { columns, .. } => *columns,
            _ => None,
        };
        let mut active = match &container {
            Container::Tabs { active, .. } => *active,
            _ => None,
        };
        let old_children = container.children().to_vec();
        let old_shares: Vec<f32> = match &container {
            Container::Linear { shares, .. } => shares.clone(),
            _ => Vec::new(),
        };

        let mut children: Vec<TileId> = Vec::new();
        let mut shares: Vec<f32> = Vec::new();

        for (index, old_child) in old_children.iter().enumerate() {
            let share = sane_share(old_shares.get(index).copied());
            let Some(child) = self.simplify(*old_child, depth + 1) else {
                if active == Some(*old_child) {
                    active = None;
                }
                continue;
            };
            if active == Some(*old_child) {
                active = Some(child);
            }
            // Join a same-direction linear, or flatten tabs-in-tabs.
            let joinable = match (kind, self.container(child)) {
                (
                    ContainerKind::Horizontal | ContainerKind::Vertical,
                    Some(Container::Linear { dir, .. }),
                ) => {
                    let same = matches!(
                        (kind, dir),
                        (ContainerKind::Horizontal, LinearDir::Horizontal)
                            | (ContainerKind::Vertical, LinearDir::Vertical)
                    );
                    same
                }
                (ContainerKind::Tabs, Some(Container::Tabs { .. })) => true,
                _ => false,
            };
            if joinable {
                let inner = self.tiles.remove(&child).and_then(|t| match t {
                    Tile::Container(c) => Some(c),
                    Tile::Pane(p) => {
                        self.tiles.insert(child, Tile::Pane(p));
                        None
                    }
                });
                if let Some(inner) = inner {
                    let inner_children = inner.children().to_vec();
                    match &inner {
                        Container::Linear {
                            shares: inner_shares,
                            ..
                        } => {
                            let sane: Vec<f32> = (0..inner_children.len())
                                .map(|i| sane_share(inner_shares.get(i).copied()))
                                .collect();
                            let total: f32 = sane.iter().sum();
                            for (i, gc) in inner_children.iter().enumerate() {
                                children.push(*gc);
                                shares.push(share * sane[i] / total);
                            }
                        }
                        _ => {
                            for gc in &inner_children {
                                children.push(*gc);
                                shares.push(share);
                            }
                        }
                    }
                    if active == Some(child) {
                        // The flattened tab strip's own active tab takes over.
                        active = match &inner {
                            Container::Tabs { active, .. } => {
                                active.or_else(|| inner_children.first().copied())
                            }
                            _ => inner_children.first().copied(),
                        };
                    }
                    continue;
                }
            }
            children.push(child);
            shares.push(share);
        }

        if children.is_empty() {
            return None;
        }
        if children.len() == 1 {
            // This container no longer adds anything; its id disappears.
            return Some(children[0]);
        }

        let rebuilt = match kind {
            ContainerKind::Tabs => Container::Tabs {
                active: active
                    .filter(|a| children.contains(a))
                    .or_else(|| children.first().copied()),
                children,
            },
            ContainerKind::Horizontal | ContainerKind::Vertical => Container::Linear {
                dir: if matches!(kind, ContainerKind::Horizontal) {
                    LinearDir::Horizontal
                } else {
                    LinearDir::Vertical
                },
                children,
                shares,
            },
            ContainerKind::Grid => Container::Grid { children, columns },
        };
        self.tiles.insert(tile, Tile::Container(rebuilt));
        Some(tile)
    }

    /// Drop tiles no window can reach.
    fn gc(&mut self) {
        let mut reachable: BTreeSet<TileId> = BTreeSet::new();
        for window in &self.windows {
            let mut stack = vec![window.root];
            let mut depth = 0;
            while let Some(tile) = stack.pop() {
                depth += 1;
                if depth > MAX_DEPTH * MAX_DEPTH || !reachable.insert(tile) {
                    continue;
                }
                if let Some(Tile::Container(c)) = self.tiles.get(&tile) {
                    stack.extend(c.children().iter().copied());
                }
            }
        }
        self.tiles.retain(|id, _| reachable.contains(id));
    }

    /// Keep focus a leaf of its own window and visible, drop a maximize that
    /// points at a tile the window no longer holds, and keep the window-id
    /// allocator above every window there is — so that a layout assembled by
    /// hand, merged, or loaded from an older build cannot hand out an id
    /// twice.
    fn repair_windows(&mut self) {
        let floor = self
            .windows
            .iter()
            .map(|w| w.id.raw())
            .max()
            .map(|max| max + 1)
            .unwrap_or(1);
        self.next_window = self.next_window.max(floor);
        // A current window that closed falls back to "the first window".
        if let Some(current) = self.current {
            if self.window(current).is_none() {
                self.current = None;
            }
        }
        for index in 0..self.windows.len() {
            let root = self.windows[index].root;
            let leaves = self.leaves_of(root);
            let focused = self.windows[index].focused;
            let valid = focused.map(|f| leaves.contains(&f)).unwrap_or(false);
            if !valid {
                self.windows[index].focused = leaves.first().copied();
            }
            if let Some(focused) = self.windows[index].focused {
                // The focused pane is visible: a verb can move the tree around
                // focus without ever assigning it (`Swap` exchanges two slots,
                // `SetContainerKind` retypes a row into a tab strip), and would
                // otherwise leave the focused pane behind an inactive tab.
                self.reveal(focused);
            }
            if let Some(max) = self.windows[index].maximized {
                if !self.is_ancestor(root, max) || !self.tiles.contains_key(&max) {
                    self.windows[index].maximized = None;
                }
            }
        }
    }
}

pub(crate) fn sane_share(share: Option<f32>) -> f32 {
    match share {
        Some(s) if s.is_finite() && s > 0.0 => s,
        _ => 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::PaneSpec;
    use crate::tree::LinearDir;

    fn pane(kind: ViewKindId) -> PaneSpec {
        PaneSpec::new(impress_pane_query::PaneQuery::default(), kind)
    }

    #[test]
    fn a_new_single_pane_layout_is_one_focused_window() {
        let layout = Layout::new_single_pane(pane(ViewKindId::INFO));
        assert_eq!(layout.windows.len(), 1);
        let window = layout.windows[0].id;
        assert_eq!(layout.leaves(window).len(), 1);
        assert_eq!(layout.windows[0].focused, Some(layout.windows[0].root));
        assert_eq!(layout.current_window().unwrap(), window);
    }

    #[test]
    fn missing_view_kinds_are_reported_and_the_spec_is_left_alone() {
        let mut layout = Layout::new_single_pane(pane(ViewKindId::INFO));
        let root = layout.windows[0].root;
        let extra = layout.insert_pane(pane(ViewKindId::PLOT));
        let container = layout.insert_container(Container::linear(
            LinearDir::Vertical,
            vec![root, extra],
            vec![1.0, 1.0],
        ));
        layout.windows[0].root = container;
        layout.normalize();

        let available = vec![ViewKindId::INFO, ViewKindId::LIST];
        assert_eq!(
            layout.placeholder_for_missing_view_kinds(&available),
            vec![extra]
        );
        assert_eq!(
            layout.pane(extra).unwrap().view_kind,
            ViewKindId::PLOT,
            "the spec is kept; only the renderer substitutes"
        );
        assert!(layout
            .placeholder_for_missing_view_kinds(&[ViewKindId::INFO, ViewKindId::PLOT])
            .is_empty());
    }

    #[test]
    fn a_second_window_is_a_second_tree_over_the_same_arena() {
        let mut layout = Layout::new_single_pane(pane(ViewKindId::INFO));
        let detached = layout.insert_pane(pane(ViewKindId::PDF));
        let second = layout.add_window(detached);
        layout.normalize();

        assert_eq!(layout.windows.len(), 2);
        assert_eq!(layout.window_of(detached), Some(second));
        assert_eq!(layout.leaves(second), vec![detached]);
        assert_eq!(layout.panes().len(), 2);
        // A tile id resolves from either window; roles and directions do not.
        assert_eq!(
            layout.resolve(layout.windows[0].id, &PaneRef::id(detached)),
            Ok(detached)
        );
    }

    #[test]
    fn current_window_prefers_the_one_with_focus() {
        let mut layout = Layout::new_single_pane(pane(ViewKindId::INFO));
        let detached = layout.insert_pane(pane(ViewKindId::PDF));
        let second = layout.add_window(detached);
        layout.windows[0].focused = None;
        assert_eq!(layout.current_window().unwrap(), second);
    }

    #[test]
    fn json_written_before_next_window_existed_loads_with_a_sound_allocator() {
        let mut layout = Layout::new_single_pane(pane(ViewKindId::INFO));
        let detached = layout.insert_pane(pane(ViewKindId::PDF));
        layout.add_window(detached);
        assert_eq!(layout.next_window, 3);

        // Strip the field, as a layout saved by an older build would have it.
        let mut json = serde_json::to_value(&layout).unwrap();
        json.as_object_mut().unwrap().remove("next_window");
        let loaded: Layout = serde_json::from_value(json).unwrap();
        assert_eq!(
            loaded.next_window, 3,
            "the allocator starts above every window the value holds"
        );
        assert_eq!(loaded.windows, layout.windows);

        // A stale counter is raised, never lowered.
        let mut json = serde_json::to_value(&layout).unwrap();
        json.as_object_mut()
            .unwrap()
            .insert("next_window".into(), serde_json::json!(1));
        let loaded: Layout = serde_json::from_value(json).unwrap();
        assert_eq!(loaded.next_window, 3);
    }

    #[test]
    fn window_ids_are_never_reused() {
        let mut layout = Layout::new_single_pane(pane(ViewKindId::INFO));
        let first = layout.windows[0].id;
        let detached = layout.insert_pane(pane(ViewKindId::PDF));
        let second = layout.add_window(detached);
        layout.windows.retain(|w| w.id != second);
        layout.normalize();
        let replacement = layout.insert_pane(pane(ViewKindId::PLOT));
        let third = layout.add_window(replacement);
        assert_ne!(
            third, second,
            "a closed window's id is not handed out again"
        );
        assert_ne!(third, first);
    }

    #[test]
    fn an_empty_layout_has_no_current_window() {
        let layout = Layout::empty();
        assert!(layout.current_window().is_err());
    }
}
