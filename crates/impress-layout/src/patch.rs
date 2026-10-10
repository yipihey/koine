//! What a verb did, in a form that undoes exactly.
//!
//! # Why a computed diff and not an explicit inverse op
//!
//! The obvious design is an inverse-op enum — `Split` records "remove tile 7,
//! unwrap container 6", and so on. It is compact and semantically legible, and
//! it is wrong here for one reason: **every verb is followed by
//! normalization** ([`crate::Layout::normalize`]), which may collapse a
//! container three levels away, rescale the shares of a joined linear, retarget
//! a tabs `active`, move focus and garbage-collect tiles the verb never
//! mentioned. An inverse op would have to predict all of that, and the first
//! normalization rule added later would silently make old inverses inexact —
//! a class of bug that only shows up as "undo put my layout somewhere else",
//! long after the change that caused it.
//!
//! So a [`Patch`] is a **diff of the whole layout value, computed after the
//! fact**: only the entries that actually differ are stored, and reverting
//! restores exactly those. It is exact by construction for any verb and any
//! future normalization rule, and the property test
//! (`apply(v); revert()` == original, over random verb sequences) is a real
//! test rather than a restatement of the implementation.
//!
//! The cost is one clone of the layout per verb (layouts are tens of tiles,
//! not thousands) and a patch that names no gesture — so the verb that produced
//! it rides along in [`Patch::verb`], which is what the operation log and the
//! undo-stack classifier read.
//!
//! # Undo is out of order, so a step is checked before it replays
//!
//! [`Layout::revert`] and [`Layout::reapply`] are the exact inverses the
//! property tests pin. An undo *ring* cannot use them blindly: there is one
//! arrangement ring and one exploration ring per pane (ADR-0031 D7), so the
//! step being undone is often not the last thing that changed. Replaying a
//! whole recorded value would silently undo whatever happened since — pane
//! B's later selection on another channel, or the role a later `set-role`
//! moved (review RL-L4). So the rings go through [`Layout::undo_step`] /
//! [`Layout::redo_step`]:
//!
//! * a selection patch records only the `(channel, kind)` entries it changed
//!   ([`Patch::channels`] is *partial*), so undoing it restores exactly those;
//! * a pane that exists on both sides is restored field by field, and only
//!   the fields the step changed;
//! * every field and entry the step would restore must still hold the value
//!   the step left there. If one does not — something else changed it since
//!   — the step is refused with [`LayoutError::UndoConflict`], naming what
//!   moved, and nothing is written. Focus and the key window are the
//!   exception: they move constantly and never block a step, they are
//!   simply left where the user put them;
//! * the result is normalized and must keep every role unique per window.
//!
//! The id allocators are never rolled back (review RL-L8): ids are documented
//! as never reused, and handing a split's id out again after undoing it let a
//! new pane inherit a dead pane's exploration ring.

use std::collections::{BTreeMap, BTreeSet};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::channels::ChannelState;
use crate::error::LayoutError;
use crate::ids::{TileId, WindowId};
use crate::tree::{Tile, Window};
use crate::verb::Verb;
use crate::Layout;

/// What happened to one arena slot. `None` on a side means the tile did not
/// exist then.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TileChange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<Tile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<Tile>,
}

/// A before/after pair for one of the layout's non-arena fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Change<T> {
    pub before: T,
    pub after: T,
}

/// The reversible record of one verb.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Patch {
    /// The gesture that produced this patch. Carried for the operation log and
    /// for [`crate::UndoStacks`] routing; never read when reverting.
    pub verb: Verb,
    /// Arena slots that differ, keyed by tile id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tiles: BTreeMap<TileId, TileChange>,
    /// The window list, when any window changed (root, focus, geometry,
    /// default channel, maximized, or the set of windows itself).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<Change<Vec<Window>>>,
    /// The channel entries a selection changed — **only** those. Each side
    /// holds the `(channel, kind)` entries that differ, as they were on that
    /// side; an entry present on one side and absent on the other did not
    /// exist on the side it is missing from. An entry the verb did not touch
    /// appears on neither, so undoing one pane's selection can never wipe
    /// another pane's (review RL-L4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<Change<ChannelState>>,
    /// The tile-id allocator, when tiles were created. Recorded, never
    /// restored: ids are not reused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_tile: Option<Change<u64>>,
    /// The window-id allocator, when windows were created. Recorded, never
    /// restored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_window: Option<Change<u64>>,
    /// The key window ([`Layout::current`]), when a verb moved it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<Change<Option<WindowId>>>,
}

impl Patch {
    /// Compute the patch from the two layout values around a verb.
    pub fn diff(verb: Verb, before: &Layout, after: &Layout) -> Patch {
        let mut tiles = BTreeMap::new();
        let keys: BTreeSet<TileId> = before
            .tiles
            .keys()
            .chain(after.tiles.keys())
            .copied()
            .collect();
        for id in keys {
            let b = before.tiles.get(&id);
            let a = after.tiles.get(&id);
            if b != a {
                tiles.insert(
                    id,
                    TileChange {
                        before: b.cloned(),
                        after: a.cloned(),
                    },
                );
            }
        }
        let windows = if before.windows == after.windows {
            None
        } else {
            Some(Change {
                before: before.windows.clone(),
                after: after.windows.clone(),
            })
        };
        let channels = channel_delta(&before.channels, &after.channels);
        let next_tile = if before.next_tile == after.next_tile {
            None
        } else {
            Some(Change {
                before: before.next_tile,
                after: after.next_tile,
            })
        };
        let next_window = if before.next_window == after.next_window {
            None
        } else {
            Some(Change {
                before: before.next_window,
                after: after.next_window,
            })
        };
        let current = if before.current == after.current {
            None
        } else {
            Some(Change {
                before: before.current,
                after: after.current,
            })
        };
        Patch {
            verb,
            tiles,
            windows,
            channels,
            next_tile,
            next_window,
            current,
        }
    }

    /// Whether the verb changed nothing at all (a `Restore` with nothing
    /// maximized, a `Focus` on the already-focused pane). The service does not
    /// push these onto a ring.
    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
            && self.windows.is_none()
            && self.channels.is_none()
            && self.next_tile.is_none()
            && self.next_window.is_none()
            && self.current.is_none()
    }

    /// Every tile this patch names on either side — what an undo ring keeps a
    /// closed pane's exploration ring alive for.
    pub fn mentions(&self, tile: TileId) -> bool {
        self.tiles.contains_key(&tile)
    }
}

/// The `(channel, kind)` entries that differ between two channel states, as
/// a pair of partial states.
fn channel_delta(before: &ChannelState, after: &ChannelState) -> Option<Change<ChannelState>> {
    let mut b = ChannelState::new();
    let mut a = ChannelState::new();
    for (channel, key) in channel_keys(before, after) {
        let old = before.channels.get(&channel).and_then(|k| k.get(&key));
        let new = after.channels.get(&channel).and_then(|k| k.get(&key));
        if old != new {
            if let Some(ids) = old {
                b.publish(channel, key.clone(), ids.clone());
            }
            if let Some(ids) = new {
                a.publish(channel, key, ids.clone());
            }
        }
    }
    if b.channels.is_empty() && a.channels.is_empty() {
        None
    } else {
        Some(Change {
            before: b,
            after: a,
        })
    }
}

fn channel_keys(x: &ChannelState, y: &ChannelState) -> BTreeSet<(u8, String)> {
    x.channels
        .iter()
        .chain(y.channels.iter())
        .flat_map(|(channel, kinds)| kinds.keys().map(move |kind| (*channel, kind.clone())))
        .collect()
}

/// Which way a patch is being replayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// Undo: from the `after` side back to `before`.
    Back,
    /// Redo: from `before` forward to `after`.
    Forward,
}

impl Direction {
    fn sides<'a, T>(self, before: &'a T, after: &'a T) -> (&'a T, &'a T) {
        match self {
            Direction::Back => (after, before),
            Direction::Forward => (before, after),
        }
    }
}

impl Layout {
    /// Undo a patch: restore exactly the entries it recorded — except the id
    /// allocators, which only ever move forward (review RL-L8).
    ///
    /// Exact by construction — `apply(v)` then `revert(&patch)` is the
    /// identity up to the allocators (`tests/properties.rs`). Unchecked: this
    /// is the primitive, and an undo *ring* uses [`Layout::undo_step`].
    pub fn revert(&mut self, patch: &Patch) {
        self.restore_exact(patch, Direction::Back);
    }

    /// Redo a patch: re-apply exactly the entries it recorded, without
    /// re-running the verb (which might not resolve the same way twice).
    /// Unchecked; a ring uses [`Layout::redo_step`].
    pub fn reapply(&mut self, patch: &Patch) {
        self.restore_exact(patch, Direction::Forward);
    }

    /// Undo one ring step, checked: refused with
    /// [`LayoutError::UndoConflict`] when anything the step would restore has
    /// changed since it was recorded, in which case the layout is untouched.
    /// See the module docs.
    pub fn undo_step(&mut self, patch: &Patch) -> Result<(), LayoutError> {
        self.restore_checked(patch, Direction::Back)
    }

    /// Redo one ring step, checked the same way as [`Layout::undo_step`].
    pub fn redo_step(&mut self, patch: &Patch) -> Result<(), LayoutError> {
        self.restore_checked(patch, Direction::Forward)
    }

    fn restore_exact(&mut self, patch: &Patch, direction: Direction) {
        for (id, change) in &patch.tiles {
            let (_, target) = direction.sides(&change.before, &change.after);
            match target {
                Some(tile) => {
                    self.tiles.insert(*id, tile.clone());
                }
                None => {
                    self.tiles.remove(id);
                }
            }
        }
        if let Some(change) = &patch.windows {
            let (_, target) = direction.sides(&change.before, &change.after);
            self.windows = target.clone();
        }
        if let Some(change) = &patch.channels {
            let (from, to) = direction.sides(&change.before, &change.after);
            for (channel, kind) in channel_keys(from, to) {
                set_entry(
                    &mut self.channels,
                    channel,
                    &kind,
                    to.channels
                        .get(&channel)
                        .and_then(|k| k.get(&kind))
                        .cloned(),
                );
            }
        }
        if let Some(change) = &patch.current {
            let (_, target) = direction.sides(&change.before, &change.after);
            self.current = *target;
        }
    }

    fn restore_checked(&mut self, patch: &Patch, direction: Direction) -> Result<(), LayoutError> {
        let mut scratch = self.clone();

        for (id, change) in &patch.tiles {
            let (from, to) = direction.sides(&change.before, &change.after);
            let current = scratch.tiles.get(id).cloned();
            match (from, to) {
                // Created by the step, so replaying removes it — with whatever
                // it shows now: undoing a split closes the pane it made.
                (_, None) => {
                    scratch.tiles.remove(id);
                }
                // Removed by the step, so replaying brings it back. Ids are
                // never reused, so nothing else can be standing in its slot.
                (None, Some(tile)) => {
                    if current.is_some() {
                        return Err(conflict(format!("tile {id}")));
                    }
                    scratch.tiles.insert(*id, tile.clone());
                }
                (Some(from), Some(to)) => {
                    let current = current.ok_or_else(|| conflict(format!("tile {id}")))?;
                    let restored = match (&current, from, to) {
                        (Tile::Pane(now), Tile::Pane(from), Tile::Pane(to)) => {
                            Tile::Pane(merge_fields(now, from, to, &[], |field| {
                                format!("pane {id}'s {field}")
                            })?)
                        }
                        _ if &current == from => to.clone(),
                        _ => return Err(conflict(format!("tile {id}"))),
                    };
                    scratch.tiles.insert(*id, restored);
                }
            }
        }

        if let Some(change) = &patch.windows {
            let (from, to) = direction.sides(&change.before, &change.after);
            scratch.windows = merge_windows(&scratch.windows, from, to)?;
        }

        if let Some(change) = &patch.channels {
            let (from, to) = direction.sides(&change.before, &change.after);
            for (channel, kind) in channel_keys(from, to) {
                let expected = from.channels.get(&channel).and_then(|k| k.get(&kind));
                let now = scratch
                    .channels
                    .channels
                    .get(&channel)
                    .and_then(|k| k.get(&kind));
                if now != expected {
                    return Err(conflict(format!("channel {channel}'s {kind} selection")));
                }
                set_entry(
                    &mut scratch.channels,
                    channel,
                    &kind,
                    to.channels
                        .get(&channel)
                        .and_then(|k| k.get(&kind))
                        .cloned(),
                );
            }
        }

        // Soft, like focus: the key window moves back only if nothing moved it
        // since.
        if let Some(change) = &patch.current {
            let (from, to) = direction.sides(&change.before, &change.after);
            if scratch.current == *from {
                scratch.current = *to;
            }
        }

        scratch.normalize();
        // Field by field, a step only restores what nothing touched since —
        // but a role another pane took *after* the step (set-role on the
        // arrangement ring) is not a field of this pane, so it is checked
        // here: a step may not hand out a role a second time.
        if let Some((role, window)) = scratch.new_duplicate_role(self) {
            return Err(conflict(format!(
                "the role '{role}' (another pane in window {window} holds it now)"
            )));
        }
        *self = scratch;
        Ok(())
    }
}

fn conflict(what: String) -> LayoutError {
    LayoutError::UndoConflict { what }
}

fn set_entry(
    state: &mut ChannelState,
    channel: u8,
    kind: &str,
    ids: Option<Vec<impress_pane_query::ItemId>>,
) {
    match ids {
        Some(ids) => state.publish(channel, kind.to_string(), ids),
        None => {
            if let Some(kinds) = state.channels.get_mut(&channel) {
                kinds.remove(kind);
                if kinds.is_empty() {
                    state.channels.remove(&channel);
                }
            }
        }
    }
}

/// Restore, in `now`, exactly the fields `from → to` changed, provided each
/// still holds its `from` value. A field in `soft` that moved since is left
/// where it is rather than refused.
fn merge_fields<T: Serialize + DeserializeOwned>(
    now: &T,
    from: &T,
    to: &T,
    soft: &[&str],
    label: impl Fn(&str) -> String,
) -> Result<T, LayoutError> {
    let as_object = |value: &T| -> Result<serde_json::Map<String, serde_json::Value>, LayoutError> {
        match serde_json::to_value(value) {
            Ok(serde_json::Value::Object(map)) => Ok(map),
            _ => Err(conflict(label("value (not an object)"))),
        }
    };
    let mut merged = as_object(now)?;
    let from = as_object(from)?;
    let to = as_object(to)?;
    let fields: BTreeSet<&String> = from.keys().chain(to.keys()).collect();
    for field in fields {
        let (f, t) = (from.get(field), to.get(field));
        if f == t {
            continue;
        }
        if merged.get(field) != f {
            if soft.contains(&field.as_str()) {
                continue;
            }
            return Err(conflict(label(field)));
        }
        match t {
            Some(value) => {
                merged.insert(field.clone(), value.clone());
            }
            None => {
                merged.remove(field);
            }
        }
    }
    serde_json::from_value(serde_json::Value::Object(merged))
        .map_err(|_| conflict(label("value (it no longer decodes)")))
}

/// The window list with one step replayed on it, window by window and field
/// by field. A window the step created is dropped, one it removed comes back,
/// and the order is the step's target order with any window made since kept
/// at the end.
fn merge_windows(
    now: &[Window],
    from: &[Window],
    to: &[Window],
) -> Result<Vec<Window>, LayoutError> {
    let by_id = |windows: &[Window]| -> BTreeMap<WindowId, Window> {
        windows.iter().map(|w| (w.id, w.clone())).collect()
    };
    let (mut current, from_map) = (by_id(now), by_id(from));
    let mut out = Vec::new();
    for target in to {
        let restored = match (current.remove(&target.id), from_map.get(&target.id)) {
            (Some(now), Some(from)) => merge_fields(&now, from, target, &["focused"], |field| {
                format!("window {}'s {field}", target.id)
            })?,
            // Removed by the step and not back since: bring it back whole.
            (None, None) => target.clone(),
            (Some(_), None) => return Err(conflict(format!("window {}", target.id))),
            (None, Some(_)) => return Err(conflict(format!("window {}", target.id))),
        };
        out.push(restored);
    }
    for window in now {
        // A window the step created goes; one made since stays.
        if current.contains_key(&window.id) && !from_map.contains_key(&window.id) {
            out.push(window.clone());
        }
    }
    Ok(out)
}
