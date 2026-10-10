//! The undo rings (ADR-0031 D7).
//!
//! There are three undo stacks and ⌘Z is routed by the focused leaf. The first
//! — the editor session's own undo manager — is not ours and is never touched.
//! The other two are here:
//!
//! 1. **arrangement**: split, move, close, swap, resize, retype, maximize,
//!    restore, role, window geometry — one ring for the whole layout, on its
//!    own chord;
//! 2. **exploration**: query, view kind, parameter bindings, channel and
//!    selection — **one ring per pane**, because exploring in one pane must
//!    not undo exploring in another.
//!
//! Focus verbs record nothing: undoing a focus move is what the opposite focus
//! move is for, and putting them on a ring means every ⌘Z spends a step on
//! where the cursor was.
//!
//! Undo never crosses a commit (D7); commits are `Durable` operations on the
//! store and are reverted item-wise, which is the service's business, not this
//! crate's.

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::error::LayoutError;
use crate::ids::TileId;
use crate::patch::Patch;
use crate::verb::{PaneRef, Verb};
use crate::Layout;

/// How many gestures a ring remembers by default.
pub const DEFAULT_CAPACITY: usize = 64;

/// A bounded ring of patches with a redo side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UndoRing {
    /// The oldest patch is dropped once this many are held.
    pub capacity: usize,
    /// Applied patches, oldest first.
    pub done: VecDeque<Patch>,
    /// Patches undone and not yet re-applied, most recently undone last.
    pub undone: Vec<Patch>,
}

impl Default for UndoRing {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl UndoRing {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            done: VecDeque::new(),
            undone: Vec::new(),
        }
    }

    /// Record a patch. A patch that changed nothing is dropped: a no-op
    /// gesture must not cost the user a ⌘Z.
    pub fn push(&mut self, patch: Patch) {
        if patch.is_empty() {
            return;
        }
        self.undone.clear();
        self.done.push_back(patch);
        while self.done.len() > self.capacity {
            self.done.pop_front();
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.done.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    /// Revert the most recent patch, returning it. `Ok(None)` is an empty
    /// ring.
    ///
    /// Checked ([`Layout::undo_step`]): when something the patch would
    /// restore has changed since, the step is refused and **dropped** — it
    /// can never apply again, and leaving it on top would make every later
    /// ⌘Z refuse on the same entry — and the layout is untouched.
    pub fn undo(&mut self, layout: &mut Layout) -> Result<Option<Patch>, LayoutError> {
        let Some(patch) = self.done.pop_back() else {
            return Ok(None);
        };
        layout.undo_step(&patch)?;
        self.undone.push(patch.clone());
        Ok(Some(patch))
    }

    /// Re-apply the most recently undone patch, returning it. Checked and
    /// dropped on conflict, exactly as [`Self::undo`].
    pub fn redo(&mut self, layout: &mut Layout) -> Result<Option<Patch>, LayoutError> {
        let Some(patch) = self.undone.pop() else {
            return Ok(None);
        };
        layout.redo_step(&patch)?;
        self.done.push_back(patch.clone());
        Ok(Some(patch))
    }

    /// Whether any patch on this ring, either side, names `tile`.
    pub fn mentions(&self, tile: TileId) -> bool {
        self.done
            .iter()
            .chain(self.undone.iter())
            .any(|p| p.mentions(tile))
    }
}

/// Which ring a verb belongs on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StackKind {
    /// The arrangement ring.
    Arrangement,
    /// The exploration ring of the pane this reference names. The reference,
    /// not a tile id: classification is a property of the *verb*, and the
    /// reference only resolves against a layout ([`UndoStacks::apply`] does
    /// that, before the verb runs, since applying can change what `Focused`
    /// or a direction means).
    Exploration(PaneRef),
    /// Not recorded.
    None,
}

/// Which ring a verb belongs on (ADR-0031 D7).
pub fn stack_for(verb: &Verb) -> StackKind {
    match verb {
        Verb::Split { .. }
        | Verb::MoveTile { .. }
        | Verb::Close { .. }
        | Verb::Swap { .. }
        | Verb::Resize { .. }
        | Verb::SetCollapsed { .. }
        | Verb::SetContainerKind { .. }
        | Verb::Maximize { .. }
        | Verb::Restore
        | Verb::Detach { .. }
        // Where a role lives is arrangement, not exploration: it changes what
        // every universal chord in the window points at.
        | Verb::SetRole { .. }
        | Verb::SetWindowGeometry { .. }
        | Verb::SetDefaultChannel { .. }
        // Opening a missing list is a split. Focusing a list that is already
        // there is not recorded — `apply` drops that patch (focus records
        // nothing).
        | Verb::OpenList { .. } => StackKind::Arrangement,

        Verb::SetPane { target, .. }
        | Verb::SetQuery { target, .. }
        | Verb::SetViewKind { target, .. }
        | Verb::SetTabOrder { target, .. }
        | Verb::BindParam { target, .. }
        | Verb::SetChannel { target, .. }
        | Verb::Select { target, .. } => StackKind::Exploration(target.clone()),

        Verb::Focus { .. } | Verb::FocusDirection { .. } => StackKind::None,
    }
}

/// The two rings this crate owns, with the routing that puts a patch on the
/// right one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UndoStacks {
    pub arrangement: UndoRing,
    /// One ring per pane, created on first exploration in that pane and
    /// dropped when the pane closes ([`UndoStacks::forget_closed_panes`]).
    pub exploration: BTreeMap<TileId, UndoRing>,
    /// Capacity for rings created later.
    pub capacity: usize,
}

impl Default for UndoStacks {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl UndoStacks {
    pub fn new(capacity: usize) -> Self {
        Self {
            arrangement: UndoRing::new(capacity),
            exploration: BTreeMap::new(),
            capacity: capacity.max(1),
        }
    }

    /// Apply a verb and record its patch on the ring [`stack_for`] chooses.
    ///
    /// The exploration target is resolved **before** the verb runs: after a
    /// split or a move, `Focused` and a direction mean something else.
    pub fn apply(&mut self, layout: &mut Layout, verb: Verb) -> Result<Patch, LayoutError> {
        let window = layout.current_window()?;
        let destination = match stack_for(&verb) {
            StackKind::Arrangement => Destination::Arrangement,
            StackKind::Exploration(reference) => match layout.resolve(window, &reference) {
                Ok(tile) => Destination::Pane(tile),
                // Unresolvable: the verb is about to fail anyway.
                Err(_) => Destination::Unrecorded,
            },
            StackKind::None => Destination::Unrecorded,
        };
        let patch = layout.apply_in(window, verb)?;
        let destination = recorded_destination(destination, &patch);
        match destination {
            Destination::Arrangement => self.arrangement.push(patch.clone()),
            Destination::Pane(tile) => {
                let capacity = self.capacity;
                self.exploration
                    .entry(tile)
                    .or_insert_with(|| UndoRing::new(capacity))
                    .push(patch.clone());
            }
            Destination::Unrecorded => {}
        }
        self.prune(layout);
        Ok(patch)
    }

    /// Apply several verbs as ONE gesture: all or none, and one undo step.
    ///
    /// What an outline click is (review PH-M2): focus the navigator, publish
    /// the row on its channel, re-point the list — three verbs the person
    /// made with one click, so one ⌘Z must take all three back, and a refusal
    /// of the third must leave the first two unapplied. The verbs run in
    /// order on a scratch copy; the first refusal returns with the layout
    /// untouched. The step is the diff of the whole run, recorded on the ring
    /// the FIRST recorded verb's rule picks (its target resolved before
    /// anything runs, as [`Self::apply`] does) — for an outline click that is
    /// the navigator's exploration ring, which is where ⌘Z goes after the
    /// click, since focus is on the navigator. The step's `verb` is that
    /// first recorded verb (the log's label); reverting never reads it.
    /// `Ok(None)` for an empty list: nothing to apply.
    pub fn apply_all(
        &mut self,
        layout: &mut Layout,
        verbs: Vec<Verb>,
    ) -> Result<Option<Patch>, LayoutError> {
        let window = layout.current_window()?;
        let Some(principal) = verbs
            .iter()
            .find(|verb| stack_for(verb) != StackKind::None)
            .or_else(|| verbs.first())
            .cloned()
        else {
            return Ok(None);
        };
        let destination = match stack_for(&principal) {
            StackKind::Arrangement => Destination::Arrangement,
            StackKind::Exploration(reference) => match layout.resolve(window, &reference) {
                Ok(tile) => Destination::Pane(tile),
                Err(_) => Destination::Unrecorded,
            },
            StackKind::None => Destination::Unrecorded,
        };
        let before = layout.clone();
        let mut scratch = before.clone();
        for verb in verbs {
            // Each verb resolves against the key window as it stands after
            // the ones before it — exactly as if they were applied one by one.
            scratch.apply(verb)?;
        }
        let patch = Patch::diff(principal, &before, &scratch);
        *layout = scratch;
        let destination = recorded_destination(destination, &patch);
        match destination {
            Destination::Arrangement => self.arrangement.push(patch.clone()),
            Destination::Pane(tile) => self.exploration_ring(tile).push(patch.clone()),
            Destination::Unrecorded => {}
        }
        self.prune(layout);
        Ok(Some(patch))
    }

    /// The exploration ring of one pane, created if absent.
    pub fn exploration_ring(&mut self, pane: TileId) -> &mut UndoRing {
        let capacity = self.capacity;
        self.exploration
            .entry(pane)
            .or_insert_with(|| UndoRing::new(capacity))
    }

    /// Undo the last arrangement gesture.
    pub fn undo_arrangement(&mut self, layout: &mut Layout) -> Result<Option<Patch>, LayoutError> {
        let stepped = self.arrangement.undo(layout);
        self.prune(layout);
        stepped
    }

    /// Redo the last undone arrangement gesture.
    pub fn redo_arrangement(&mut self, layout: &mut Layout) -> Result<Option<Patch>, LayoutError> {
        let stepped = self.arrangement.redo(layout);
        self.prune(layout);
        stepped
    }

    /// Undo the last exploration in one pane.
    pub fn undo_exploration(
        &mut self,
        layout: &mut Layout,
        pane: TileId,
    ) -> Result<Option<Patch>, LayoutError> {
        let Some(ring) = self.exploration.get_mut(&pane) else {
            return Ok(None);
        };
        let stepped = ring.undo(layout);
        self.prune(layout);
        stepped
    }

    /// Redo the last undone exploration in one pane.
    pub fn redo_exploration(
        &mut self,
        layout: &mut Layout,
        pane: TileId,
    ) -> Result<Option<Patch>, LayoutError> {
        let Some(ring) = self.exploration.get_mut(&pane) else {
            return Ok(None);
        };
        let stepped = ring.redo(layout);
        self.prune(layout);
        stepped
    }

    /// Drop the exploration rings of panes the layout no longer has AND no
    /// arrangement step can bring back.
    ///
    /// Runs after every applied verb and every undo or redo step (review
    /// RL-L8: before, nothing outside tests ever pruned). A closed pane's ring
    /// is kept while the arrangement ring still holds the close — undoing the
    /// close brings the pane back with its history — and goes once that step
    /// has fallen off the ring or been dropped by a commit. Tile ids are never
    /// reused, so a kept ring can never be inherited by a new pane.
    pub fn prune(&mut self, layout: &Layout) {
        let arrangement = &self.arrangement;
        self.exploration
            .retain(|tile, _| layout.pane(*tile).is_some() || arrangement.mentions(*tile));
    }

    /// Drop the exploration rings of every pane the layout no longer has,
    /// whether or not an arrangement step could bring it back.
    pub fn forget_closed_panes(&mut self, layout: &Layout) {
        self.exploration
            .retain(|tile, _| layout.pane(*tile).is_some());
    }
}

/// Where a patch landed, once the reference has been resolved.
enum Destination {
    Arrangement,
    Pane(TileId),
    Unrecorded,
}

/// `open-list` of a list the window already has only moves focus. Focus is
/// not an undo step, so that patch is dropped even though the verb's rule
/// is the arrangement ring (the ring a newly split list lands on).
fn recorded_destination(planned: Destination, patch: &Patch) -> Destination {
    if matches!(patch.verb, Verb::OpenList { .. })
        && patch.tiles.is_empty()
        && patch.next_tile.is_none()
    {
        Destination::Unrecorded
    } else {
        planned
    }
}
