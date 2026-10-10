//! Applying a verb (ADR-0031 D8).
//!
//! Every verb goes through [`Layout::apply_in`]: it runs against a scratch
//! clone, normalizes, and only then replaces the live value — so a refused
//! verb leaves the layout byte-identical, and the [`Patch`] is the diff of the
//! two values rather than a prediction of what the verb was going to do.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use crate::error::LayoutError;
use crate::ids::{view_state, ChannelId, Role, TileId, ViewKindId, WindowId};
use crate::layout::{sane_share, Slot};
use crate::patch::Patch;
use crate::spec::PaneSpec;
use crate::tree::{Container, ContainerKind, LinearDir, Tile};
use crate::verb::{PaneRef, Placement, Verb};
use crate::{Layout, PaneQuery, Scope};

impl Layout {
    /// Apply a verb to the window [`Layout::current_window`] picks.
    pub fn apply(&mut self, verb: Verb) -> Result<Patch, LayoutError> {
        let window = self.current_window()?;
        self.apply_in(window, verb)
    }

    /// Apply a verb, resolving window-relative references (`Role`, `Focused`,
    /// `Direction`) against `window`.
    pub fn apply_in(&mut self, window: WindowId, verb: Verb) -> Result<Patch, LayoutError> {
        if self.window(window).is_none() {
            return Err(LayoutError::UnknownWindow { window });
        }
        let before = self.clone();
        let mut scratch = before.clone();
        scratch.apply_inner(window, &verb)?;
        scratch.normalize();
        // ADR-0031 D6, in the one place every verb passes: each
        // session-bearing pane holds a session of its own, and a pane that
        // held one before the verb keeps it — so a split's new pane is the
        // one that gets a fresh id (`sessions.rs`).
        scratch.ensure_sessions_keeping(&before.session_holders());
        // Roles are unique per window (ADR-0031 D5): `set-role` moves one,
        // and every other verb that would leave two panes of a window with
        // the same role — a swap or move across windows, a `set-pane` naming
        // a role another pane holds — is refused rather than turning ⌘0 into
        // a coin toss (review RL-L19).
        if let Some((role, window)) = scratch.new_duplicate_role(&before) {
            return Err(LayoutError::RoleHeldTwice { role, window });
        }
        let patch = Patch::diff(verb, &before, &scratch);
        *self = scratch;
        Ok(patch)
    }

    fn apply_inner(&mut self, window: WindowId, verb: &Verb) -> Result<(), LayoutError> {
        match verb {
            Verb::Split {
                target,
                dir,
                after,
                new,
            } => {
                if let Some(spec) = new {
                    LayoutError::check_view_kind(&spec.view_kind)?;
                }
                self.do_split(window, target, *dir, *after, new.clone())
            }
            Verb::MoveTile {
                tile,
                target,
                placement,
            } => self.do_move(window, tile, target, *placement),
            Verb::Close { target } => self.do_close(window, target),
            Verb::Swap { a, b } => self.do_swap(window, a, b),
            Verb::Resize { container, shares } => self.do_resize(*container, shares),
            Verb::SetContainerKind { container, kind } => {
                self.do_set_container_kind(*container, *kind)
            }
            Verb::SetCollapsed { target, collapsed } => {
                self.do_set_collapsed(window, target, *collapsed)
            }
            Verb::Maximize { target } => self.do_maximize(window, target),
            Verb::Detach { target } => self.do_detach(window, target),
            Verb::Restore => {
                if let Some(w) = self.window_mut(window) {
                    w.maximized = None;
                }
                Ok(())
            }
            Verb::SetPane { target, spec } => {
                LayoutError::check_view_kind(&spec.view_kind)?;
                let tile = self.resolve(window, target)?;
                let slot = self.expect_pane_mut(tile)?;
                let session = slot.session.take();
                let collapsed_share = slot.collapsed_share.take();
                *slot = spec.clone();
                // Replacing a pane's spec is not closing it: a spec that
                // names no session keeps the one the pane had (D6), and a
                // collapsed pane stays restorable to its old share.
                if slot.session.is_none() {
                    slot.session = session;
                }
                if slot.collapsed_share.is_none() {
                    slot.collapsed_share = collapsed_share;
                }
                Ok(())
            }
            Verb::SetQuery { target, query } => {
                let tile = self.resolve(window, target)?;
                self.expect_pane_mut(tile)?.query = query.clone();
                Ok(())
            }
            Verb::SetViewKind { target, view_kind } => {
                LayoutError::check_view_kind(view_kind)?;
                let tile = self.resolve(window, target)?;
                self.expect_pane_mut(tile)?.view_kind = view_kind.clone();
                Ok(())
            }
            Verb::OpenList { kinds } => self.do_open_list(window, kinds),
            Verb::SetTabOrder { target, tabs } => self.do_set_tab_order(window, target, tabs),
            Verb::BindParam {
                target,
                name,
                source,
            } => {
                let tile = self.resolve(window, target)?;
                let spec = self.expect_pane_mut(tile)?;
                match spec.param_mut(name) {
                    Some(binding) => {
                        binding.source = source.clone();
                        Ok(())
                    }
                    None => Err(LayoutError::UnknownParam {
                        tile,
                        name: name.clone(),
                    }),
                }
            }
            Verb::SetChannel { target, channel } => {
                let tile = self.resolve(window, target)?;
                self.expect_pane_mut(tile)?.channel = *channel;
                Ok(())
            }
            Verb::SetRole { target, role } => {
                let tile = self.resolve(window, target)?;
                // A role is what the universal chords aim at, so it is unique
                // per window: giving it to a pane takes it from whichever pane
                // held it. Otherwise ⌘0 would be a coin toss.
                if let Some(role) = role {
                    let home = self.window_of(tile).unwrap_or(window);
                    for other in self.leaves(home) {
                        if other != tile {
                            if let Some(spec) = self.pane_mut(other) {
                                if spec.role.as_ref() == Some(role) {
                                    spec.role = None;
                                }
                            }
                        }
                    }
                }
                self.expect_pane_mut(tile)?.role = role.clone();
                Ok(())
            }
            Verb::Focus { target } => {
                let tile = self.resolve(window, target)?;
                self.focus_tile(window, tile);
                Ok(())
            }
            Verb::FocusDirection { direction } => {
                let tile = self.resolve(
                    window,
                    &PaneRef::Direction {
                        direction: *direction,
                    },
                )?;
                self.focus_tile(window, tile);
                Ok(())
            }
            Verb::Select { target, kind, ids } => {
                let tile = self.resolve(window, target)?;
                let spec = match self.pane(tile) {
                    Some(spec) => spec,
                    None if self.tiles.contains_key(&tile) => {
                        return Err(LayoutError::NotAPane { tile })
                    }
                    None => return Err(LayoutError::UnknownTile { tile }),
                };
                let channel = spec.channel;
                let default_channel = self.default_channel_for(tile);
                self.channels
                    .publish(channel.resolve(default_channel), kind.clone(), ids.clone());
                Ok(())
            }
            Verb::SetWindowGeometry {
                window: target,
                geometry,
            } => {
                let w = self
                    .window_mut(*target)
                    .ok_or(LayoutError::UnknownWindow { window: *target })?;
                w.geometry = geometry.clone();
                Ok(())
            }
            Verb::SetDefaultChannel {
                window: target,
                channel,
            } => {
                if matches!(channel, ChannelId::Follow) {
                    return Err(LayoutError::InvalidChannel { channel: *channel });
                }
                let w = self
                    .window_mut(*target)
                    .ok_or(LayoutError::UnknownWindow { window: *target })?;
                w.default_channel = *channel;
                Ok(())
            }
        }
    }

    // ------------------------------------------------------------- helpers

    /// Focus lands on a *leaf*: focusing a container focuses the pane it
    /// shows, and that pane is revealed through every tab strip above it.
    fn focus_tile(&mut self, fallback: WindowId, tile: TileId) {
        let leaf = self.first_leaf(tile).unwrap_or(tile);
        let home = self.window_of(leaf).unwrap_or(fallback);
        if let Some(w) = self.window_mut(home) {
            w.focused = Some(leaf);
        }
        // Focus moving into a window makes it the key window: the next verb
        // with no explicit window acts there (review RL-L5). Left alone when
        // it is already current, so a single-window layout never grows the
        // field.
        if self.current_window().ok() != Some(home) {
            self.current = Some(home);
        }
        // Focus is always visible: bring the leaf out from under every tab
        // strip above it.
        self.reveal(leaf);
    }

    fn replace_slot(&mut self, slot: Slot, new: TileId) {
        match slot {
            Slot::Root(index) => {
                if let Some(w) = self.windows.get_mut(index) {
                    w.root = new;
                }
            }
            Slot::Child(parent, index) => {
                if let Some(c) = self.container_mut(parent) {
                    if index < c.len() {
                        c.replace_child_at(index, new);
                    }
                }
            }
        }
    }

    /// Put `tile` beside `target` along `dir`, extending `target`'s parent when
    /// it already runs that way and wrapping `target` in a new linear when it
    /// does not. `share` is what the newcomer gets; it is taken out of
    /// `target`'s own share so the rest of the tree does not move.
    fn insert_beside(
        &mut self,
        target: TileId,
        tile: TileId,
        dir: LinearDir,
        after: bool,
    ) -> Result<(), LayoutError> {
        let parent = self.parent_of(target);
        let extends = parent
            .and_then(|p| self.container(p))
            .map(|c| matches!(c, Container::Linear { dir: d, .. } if *d == dir))
            .unwrap_or(false);
        if let (Some(parent), true) = (parent, extends) {
            let index = self
                .container(parent)
                .and_then(|c| c.index_of(target))
                .ok_or(LayoutError::UnknownTile { tile: target })?;
            let container = self
                .container_mut(parent)
                .ok_or(LayoutError::NotAContainer { tile: parent })?;
            let share = sane_share(container.share_at(index)) / 2.0;
            container.set_share_at(index, share);
            container.insert_child(index + usize::from(after), tile, Some(share));
            return Ok(());
        }
        let slot = self
            .slot_of(target)
            .ok_or(LayoutError::UnknownTile { tile: target })?;
        let wrapper = self.alloc_tile();
        let children = if after {
            vec![target, tile]
        } else {
            vec![tile, target]
        };
        self.tiles.insert(
            wrapper,
            Tile::Container(Container::linear(dir, children, vec![1.0, 1.0])),
        );
        self.replace_slot(slot, wrapper);
        Ok(())
    }

    /// Tab `tile` alongside `target` (pyqtgraph's centre drop zone).
    fn insert_into_tabs(&mut self, target: TileId, tile: TileId) -> Result<(), LayoutError> {
        if matches!(self.container(target), Some(Container::Tabs { .. })) {
            let container = self
                .container_mut(target)
                .ok_or(LayoutError::NotAContainer { tile: target })?;
            container.push_child(tile);
            if let Container::Tabs { active, .. } = container {
                *active = Some(tile);
            }
            return Ok(());
        }
        if let Some(parent) = self.parent_of(target) {
            if matches!(self.container(parent), Some(Container::Tabs { .. })) {
                let index = self
                    .container(parent)
                    .and_then(|c| c.index_of(target))
                    .ok_or(LayoutError::UnknownTile { tile: target })?;
                let container = self
                    .container_mut(parent)
                    .ok_or(LayoutError::NotAContainer { tile: parent })?;
                container.insert_child(index + 1, tile, None);
                if let Container::Tabs { active, .. } = container {
                    *active = Some(tile);
                }
                return Ok(());
            }
        }
        let slot = self
            .slot_of(target)
            .ok_or(LayoutError::UnknownTile { tile: target })?;
        let wrapper = self.alloc_tile();
        self.tiles.insert(
            wrapper,
            Tile::Container(Container::Tabs {
                children: vec![target, tile],
                active: Some(tile),
            }),
        );
        self.replace_slot(slot, wrapper);
        Ok(())
    }

    // --------------------------------------------------------------- verbs

    /// Focus the list, or split a new one in to the right of the navigator.
    fn do_open_list(
        &mut self,
        window: WindowId,
        kinds: &Option<Vec<String>>,
    ) -> Result<(), LayoutError> {
        if let Some(existing) = self.pane_with_role_in(window, &Role::LIST) {
            self.focus_tile(window, existing);
            return Ok(());
        }
        let spec = self.restored_list_spec(window, kinds)?;
        let target = if self.pane_with_role_in(window, &Role::NAVIGATOR).is_some() {
            PaneRef::role(Role::NAVIGATOR)
        } else {
            PaneRef::Focused
        };
        self.do_split(window, &target, LinearDir::Horizontal, true, Some(spec))
    }

    /// A list of the detail pane's kinds, scoped to everything. The
    /// navigator's collections are not a paper list.
    fn restored_list_spec(
        &self,
        window: WindowId,
        fallback: &Option<Vec<String>>,
    ) -> Result<PaneSpec, LayoutError> {
        let panes: Vec<&PaneSpec> = self
            .leaves(window)
            .into_iter()
            .filter_map(|id| self.pane(id))
            .collect();
        let copied = list_kinds_from(&panes);
        let kinds = copied.or_else(|| fallback.as_ref().filter(|kinds| !kinds.is_empty()).cloned());
        let Some(kinds) = kinds else {
            return Err(LayoutError::NoListKinds);
        };
        Ok(PaneSpec::new(
            PaneQuery {
                kinds,
                ..PaneQuery::default()
            },
            ViewKindId::LIST,
        )
        .with_role(Role::LIST)
        .with_channel(ChannelId::ONE))
    }

    fn do_set_tab_order(
        &mut self,
        window: WindowId,
        target: &PaneRef,
        tabs: &[ViewKindId],
    ) -> Result<(), LayoutError> {
        if tabs.is_empty() {
            return Err(LayoutError::EmptyTabOrder);
        }
        let mut seen = BTreeSet::new();
        for tab in tabs {
            LayoutError::check_view_kind(tab)?;
            if !seen.insert(tab.as_str()) {
                return Err(LayoutError::DuplicateTab { tab: tab.clone() });
            }
        }
        let tile = self.resolve(window, target)?;
        let spec = self.expect_pane_mut(tile)?;
        let mut state = match &spec.view_state {
            Value::Object(map) => map.clone(),
            _ => Map::new(),
        };
        let order = Value::Array(
            tabs.iter()
                .map(|tab| Value::String(tab.as_str().to_string()))
                .collect(),
        );
        if state.get(view_state::TAB_ORDER) == Some(&order) {
            return Ok(());
        }
        state.insert(view_state::TAB_ORDER.to_string(), order);
        spec.view_state = Value::Object(state);
        Ok(())
    }

    fn do_split(
        &mut self,
        window: WindowId,
        target: &PaneRef,
        dir: LinearDir,
        after: bool,
        new: Option<PaneSpec>,
    ) -> Result<(), LayoutError> {
        let target = self.resolve(window, target)?;
        // A bare split duplicates the pane being split: the only spec that is
        // certainly renderable here, and the user re-points one half at once.
        // Two panes cannot hold one role (D5) nor share a session (D6), so
        // the copy gets neither.
        let new = match new {
            Some(spec) => spec,
            None => {
                let mut spec = self
                    .pane(target)
                    .ok_or(LayoutError::NotAPane { tile: target })?
                    .clone();
                spec.role = None;
                spec.session = None;
                spec.collapsed_share = None;
                spec
            }
        };
        // A copy of the pane being split carries its role, the way it
        // carries its session (D6: the target keeps its own, the copy gets a
        // fresh one). Same rule for roles (D5): a role this window already
        // has stays where it is, and the new pane gets none.
        let mut new = new;
        if let Some(role) = new.role.clone() {
            let home = self.window_of(target).unwrap_or(window);
            if self.pane_with_role_in(home, &role).is_some() {
                new.role = None;
            }
        }
        let tile = self.alloc_tile();
        self.tiles.insert(tile, Tile::Pane(new));
        self.insert_beside(target, tile, dir, after)?;
        // Focus follows the new pane: a split exists to be typed into.
        self.focus_tile(window, tile);
        Ok(())
    }

    fn do_set_collapsed(
        &mut self,
        window: WindowId,
        target: &PaneRef,
        collapsed: Option<bool>,
    ) -> Result<(), LayoutError> {
        let tile = self.resolve(window, target)?;
        self.pane(tile).ok_or(LayoutError::NotAPane { tile })?;
        let parent = self
            .parent_of(tile)
            .filter(|p| matches!(self.container(*p), Some(Container::Linear { .. })))
            .ok_or(LayoutError::NotInASplit { tile })?;
        let container = self
            .container(parent)
            .ok_or(LayoutError::NotAContainer { tile: parent })?;
        let index = container
            .index_of(tile)
            .ok_or(LayoutError::UnknownTile { tile })?;
        let share = sane_share(container.share_at(index));
        let hidden = crate::shares::is_hidden(share);
        let siblings: Vec<f32> = (0..container.len())
            .filter(|i| *i != index)
            .map(|i| sane_share(container.share_at(i)))
            .filter(|s| !crate::shares::is_hidden(*s))
            .collect();
        let want = collapsed.unwrap_or(!hidden);
        if want == hidden {
            return Ok(()); // already so: an empty patch, no ⌘Z spent.
        }
        let remembered = self.pane(tile).and_then(|spec| spec.collapsed_share);
        let (new_share, remember) = if want {
            (crate::shares::HIDDEN_SHARE, Some(share))
        } else {
            // Back to exactly what it had. A pane hidden some other way (a
            // drag to nothing, a preset that ships it hidden) has no memory;
            // it gets its siblings' average, the old rule, as a last resort.
            let restored = remembered.unwrap_or_else(|| {
                if siblings.is_empty() {
                    1.0
                } else {
                    siblings.iter().sum::<f32>() / siblings.len() as f32
                }
            });
            (restored, None)
        };
        self.container_mut(parent)
            .ok_or(LayoutError::NotAContainer { tile: parent })?
            .set_share_at(index, new_share);
        self.expect_pane_mut(tile)?.collapsed_share = remember;
        Ok(())
    }

    fn do_move(
        &mut self,
        window: WindowId,
        tile: &PaneRef,
        target: &PaneRef,
        placement: Placement,
    ) -> Result<(), LayoutError> {
        let tile = self.resolve(window, tile)?;
        let target = self.resolve(window, target)?;
        if tile == target || self.is_ancestor(tile, target) {
            return Err(LayoutError::CyclicMove { tile });
        }
        match self.slot_of(tile) {
            Some(Slot::Child(parent, index)) => {
                if let Some(container) = self.container_mut(parent) {
                    container.remove_child_at(index);
                }
            }
            // A window root holds every pane of its window, and a same-window
            // target is already cyclic — so this is "move the whole of window
            // A into window B". The emptied window goes: a window with no
            // tiles never persists. Unless it is the only one left.
            Some(Slot::Root(index)) => {
                if self.windows.len() < 2 {
                    return Err(LayoutError::CannotMoveWholeWindow { tile });
                }
                self.windows.remove(index);
            }
            None => return Err(LayoutError::UnknownTile { tile }),
        }
        match placement.linear() {
            Some((dir, after)) => self.insert_beside(target, tile, dir, after)?,
            None => self.insert_into_tabs(target, tile)?,
        }
        self.focus_tile(window, tile);
        Ok(())
    }

    fn do_close(&mut self, window: WindowId, target: &PaneRef) -> Result<(), LayoutError> {
        let tile = self.resolve(window, target)?;
        let home = self
            .window_of(tile)
            .ok_or(LayoutError::UnknownTile { tile })?;
        let leaves = self.leaves(home);
        let doomed = self.leaves_of(tile);
        if doomed.len() >= self.panes().len() {
            // The last pane in the whole layout. There is nothing left to
            // show, so this is the one close that is refused.
            return Err(LayoutError::CannotCloseLastPane);
        }
        if doomed.len() >= leaves.len() {
            // The last pane of this window, but not of the layout: closing it
            // closes the window, which is what closing a detached PDF means.
            self.windows.retain(|w| w.id != home);
            return Ok(());
        }
        // Focus moves to the neighbour: the first surviving leaf after the
        // closed subtree, else the last one before it.
        let survivor = leaves
            .iter()
            .skip_while(|l| !doomed.contains(l))
            .find(|l| !doomed.contains(l))
            .copied()
            .or_else(|| {
                leaves
                    .iter()
                    .take_while(|l| !doomed.contains(l))
                    .last()
                    .copied()
            });
        match self.slot_of(tile) {
            Some(Slot::Child(parent, index)) => {
                if let Some(container) = self.container_mut(parent) {
                    container.remove_child_at(index);
                }
            }
            // Unreachable: a root holds every pane of its window, which the
            // two checks above have already dealt with.
            Some(Slot::Root(_)) => return Err(LayoutError::CannotCloseLastPane),
            None => return Err(LayoutError::UnknownTile { tile }),
        }
        if let Some(survivor) = survivor {
            if let Some(w) = self.window_mut(home) {
                w.focused = Some(survivor);
            }
            self.reveal(survivor);
        }
        Ok(())
    }

    fn do_swap(&mut self, window: WindowId, a: &PaneRef, b: &PaneRef) -> Result<(), LayoutError> {
        let a = self.resolve(window, a)?;
        let b = self.resolve(window, b)?;
        if a == b {
            return Ok(());
        }
        if self.is_ancestor(a, b) {
            return Err(LayoutError::CyclicMove { tile: a });
        }
        if self.is_ancestor(b, a) {
            return Err(LayoutError::CyclicMove { tile: b });
        }
        let slot_a = self
            .slot_of(a)
            .ok_or(LayoutError::UnknownTile { tile: a })?;
        let slot_b = self
            .slot_of(b)
            .ok_or(LayoutError::UnknownTile { tile: b })?;
        self.replace_slot(slot_a, b);
        self.replace_slot(slot_b, a);
        Ok(())
    }

    fn do_resize(&mut self, container: TileId, shares: &[f32]) -> Result<(), LayoutError> {
        match self.tiles.get_mut(&container) {
            None => Err(LayoutError::UnknownTile { tile: container }),
            Some(Tile::Pane(_)) => Err(LayoutError::NotAContainer { tile: container }),
            Some(Tile::Container(Container::Linear {
                children,
                shares: current,
                ..
            })) => {
                if shares.len() != children.len() {
                    return Err(LayoutError::InvalidShares {
                        reason: format!("{} shares for {} children", shares.len(), children.len()),
                    });
                }
                if let Some(bad) = shares.iter().find(|s| !s.is_finite() || **s <= 0.0) {
                    return Err(LayoutError::InvalidShares {
                        reason: format!("share {bad} is not a positive, finite weight"),
                    });
                }
                *current = shares.to_vec();
                Ok(())
            }
            Some(Tile::Container(_)) => Err(LayoutError::InvalidShares {
                reason: "only a linear container has shares".into(),
            }),
        }
    }

    fn do_set_container_kind(
        &mut self,
        container: TileId,
        kind: ContainerKind,
    ) -> Result<(), LayoutError> {
        let existing = match self.tiles.get(&container) {
            None => return Err(LayoutError::UnknownTile { tile: container }),
            Some(Tile::Pane(_)) => return Err(LayoutError::NotAContainer { tile: container }),
            Some(Tile::Container(c)) => c.clone(),
        };
        if existing.kind() == kind {
            return Ok(());
        }
        let children = existing.children().to_vec();
        let rebuilt = match (kind, &existing) {
            // Rotating a split keeps its shares: turning a row into a column
            // must not silently re-even the panes.
            (
                ContainerKind::Horizontal | ContainerKind::Vertical,
                Container::Linear { shares, .. },
            ) => Container::Linear {
                dir: if matches!(kind, ContainerKind::Horizontal) {
                    LinearDir::Horizontal
                } else {
                    LinearDir::Vertical
                },
                children,
                shares: shares.clone(),
            },
            (ContainerKind::Grid, Container::Grid { columns, .. }) => Container::Grid {
                children,
                columns: *columns,
            },
            _ => Container::with_children(kind, children),
        };
        self.tiles.insert(container, Tile::Container(rebuilt));
        Ok(())
    }

    /// Move a tile out into a new window of its own (ADR-0031 D4).
    fn do_detach(&mut self, window: WindowId, target: &PaneRef) -> Result<(), LayoutError> {
        let tile = self.resolve(window, target)?;
        let home = self
            .window_of(tile)
            .ok_or(LayoutError::UnknownTile { tile })?;
        let leaves = self.leaves(home);
        let leaving = self.leaves_of(tile);
        if leaving.len() >= leaves.len() {
            // It is already a window of its own.
            return Err(LayoutError::AlreadyItsOwnWindow { tile });
        }
        // The pane the source window falls back to, chosen exactly as a close
        // chooses it: the first survivor after the departing subtree, else the
        // last one before it.
        let survivor = leaves
            .iter()
            .skip_while(|l| !leaving.contains(l))
            .find(|l| !leaving.contains(l))
            .copied()
            .or_else(|| {
                leaves
                    .iter()
                    .take_while(|l| !leaving.contains(l))
                    .last()
                    .copied()
            });
        match self.slot_of(tile) {
            Some(Slot::Child(parent, index)) => {
                if let Some(container) = self.container_mut(parent) {
                    container.remove_child_at(index);
                }
            }
            // Unreachable: the pane count check above covers a window root.
            Some(Slot::Root(_)) => return Err(LayoutError::AlreadyItsOwnWindow { tile }),
            None => return Err(LayoutError::UnknownTile { tile }),
        }
        if let Some(survivor) = survivor {
            if let Some(w) = self.window_mut(home) {
                w.focused = Some(survivor);
            }
            self.reveal(survivor);
        }
        // Focus follows the pane out of the window it left, and so does the
        // key window: `close {focused}` right after a detach closes the
        // detached pane, not the main window's (review RL-L5).
        let detached = self.add_window(tile);
        self.current = Some(detached);
        Ok(())
    }

    fn do_maximize(&mut self, window: WindowId, target: &PaneRef) -> Result<(), LayoutError> {
        let tile = self.resolve(window, target)?;
        let home = self
            .window_of(tile)
            .ok_or(LayoutError::UnknownTile { tile })?;
        let inside = self
            .window(home)
            .and_then(|w| w.focused)
            .map(|f| self.is_ancestor(tile, f))
            .unwrap_or(false);
        if let Some(w) = self.window_mut(home) {
            w.maximized = Some(tile);
        }
        if !inside {
            self.focus_tile(home, tile);
        }
        Ok(())
    }
}

/// Kinds for a restored list: the detail pane's, else an item-scoped pane's.
/// A navigator of collections and libraries is neither.
fn list_kinds_from(panes: &[&PaneSpec]) -> Option<Vec<String>> {
    if let Some(detail) = panes
        .iter()
        .find(|pane| pane.role.as_ref() == Some(&Role::DETAIL) && !pane.query.kinds.is_empty())
    {
        return Some(detail.query.kinds.clone());
    }
    panes.iter().find_map(|pane| {
        if is_item_scope(pane) && !pane.query.kinds.is_empty() && !is_collections_query(pane) {
            Some(pane.query.kinds.clone())
        } else {
            None
        }
    })
}

fn is_item_scope(pane: &PaneSpec) -> bool {
    matches!(pane.query.scope, Scope::Item { .. })
}

/// The sidebar: collections and libraries, and not the papers or manuscripts
/// a list shows.
fn is_collections_query(pane: &PaneSpec) -> bool {
    let kinds: BTreeSet<&str> = pane.query.kinds.iter().map(String::as_str).collect();
    kinds.contains("collection")
        && kinds.contains("library")
        && !kinds.contains("publication")
        && !kinds.contains("manuscript")
}
