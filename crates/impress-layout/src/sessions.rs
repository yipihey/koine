//! Who holds which session (ADR-0031 D6).
//!
//! A session-bearing pane (a `source` editor) owns state that lives OUTSIDE
//! the tree — on macOS an `NSTextView`, its undo stack and an in-flight
//! compile, in a registry keyed by the pane's [`SessionId`]. The tree stores
//! only the id, and these rules decide it, so every renderer and every
//! process agrees on which pane is which editor:
//!
//! 1. **Every session-bearing pane holds a session**, and **no two panes hold
//!    the same one** — an `NSView` lives in one view hierarchy, so a shared
//!    id would make two panes fight over one editor.
//! 2. **A layout mutation never changes a pane's session.** `swap`, `move`,
//!    `resize`, `close`, `set_view_kind`, focus and the rest carry the spec
//!    — and its session — as it was. `set_pane` keeps the session of the pane
//!    it replaces when the new spec names none (the outline re-points the
//!    detail pane that way).
//! 3. **`split` gives the NEW pane a fresh session**, never the target's,
//!    even when the new spec was copied from the target with its session.
//! 4. **Re-applying a preset or a saved layout keeps the live session of the
//!    pane in the same role** ([`Layout::adopt_sessions_by_role`]): roles are
//!    stable across presets and a preset carries no sessions, so the editor
//!    in the `detail` role survives ⌃⌘1.
//!
//! Presets themselves are built without sessions: nothing in
//! [`Layout::normalize`] assigns one, so a shipped preset stays a
//! deterministic value that `PresetStore::matches_shipped` can compare.

use std::collections::{BTreeMap, HashMap};

use crate::ids::{SessionId, TileId};
use crate::Layout;

impl Layout {
    /// Which pane holds each session, by tile.
    pub fn session_holders(&self) -> HashMap<SessionId, TileId> {
        let mut out = HashMap::new();
        for tile in self.panes() {
            if let Some(session) = self.pane(tile).and_then(|p| p.session.clone()) {
                out.entry(session).or_insert(tile);
            }
        }
        out
    }

    /// Rule 1 with no history: a duplicated session stays with the pane of
    /// the lowest tile id (the older pane — tile ids only grow), and every
    /// session-bearing pane without one gets a fresh one. Returns whether
    /// anything changed, so a caller loading a stored tree knows to save it.
    pub fn ensure_sessions(&mut self) -> bool {
        self.ensure_sessions_keeping(&HashMap::new())
    }

    /// Rule 1, where `holders` says who held a session BEFORE the change
    /// being normalized: on a duplicate, that pane keeps it and every other
    /// holder is given a fresh one. A session `holders` does not mention goes
    /// to the pane with the lowest tile id.
    pub fn ensure_sessions_keeping(&mut self, holders: &HashMap<SessionId, TileId>) -> bool {
        // Tile order, not tree order: a swap must not change who keeps what.
        let mut panes = self.panes();
        panes.sort();

        let mut claimed: BTreeMap<SessionId, TileId> = BTreeMap::new();
        // The previous holders first, when they still hold it.
        for (session, tile) in holders {
            if self.pane(*tile).and_then(|p| p.session.as_ref()) == Some(session) {
                claimed.insert(session.clone(), *tile);
            }
        }
        let mut changed = false;
        for tile in panes {
            let Some(spec) = self.pane_mut(tile) else {
                continue;
            };
            if let Some(session) = spec.session.clone() {
                match claimed.get(&session) {
                    Some(owner) if *owner != tile => {
                        // Someone else's editor: this pane gets its own.
                        spec.session = None;
                        changed = true;
                    }
                    Some(_) => {}
                    None => {
                        claimed.insert(session, tile);
                    }
                }
            }
            if spec.session.is_none() && spec.view_kind.is_session_bearing() {
                let fresh = SessionId::fresh();
                claimed.insert(fresh.clone(), tile);
                spec.session = Some(fresh);
                changed = true;
            }
        }
        changed
    }

    /// Rule 4: this tree is about to REPLACE `previous` (a preset or a saved
    /// layout applied over the live one). Every session-bearing pane with a
    /// role takes the session of `previous`'s pane in the same role, if that
    /// pane had one; then rule 1 fills in the rest, the adopted sessions
    /// winning any duplicate (a saved layout can carry an id of its own).
    pub fn adopt_sessions_by_role(&mut self, previous: &Layout) {
        let mut adopted: HashMap<SessionId, TileId> = HashMap::new();
        for tile in self.panes() {
            let Some(spec) = self.pane(tile) else {
                continue;
            };
            if !spec.view_kind.is_session_bearing() {
                continue;
            }
            let Some(role) = spec.role.clone() else {
                continue;
            };
            let live = previous
                .pane_with_role(&role)
                .and_then(|t| previous.pane(t))
                .and_then(|p| p.session.clone());
            if let Some(session) = live {
                if let Some(spec) = self.pane_mut(tile) {
                    spec.session = Some(session.clone());
                }
                adopted.insert(session, tile);
            }
        }
        self.ensure_sessions_keeping(&adopted);
    }
}
