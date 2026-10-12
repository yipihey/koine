//! The in-memory half of a layout: the tree, its two undo rings, and the
//! channel state that lives inside the tree already.
//!
//! A [`LayoutSession`] is one `(app_id, device)` scope. The store holds the
//! tree; the session holds the tree **plus the rings**, which are deliberately
//! not persisted: an undo ring is a record of what this sitting has done, and
//! restoring one from a file would let ⌘Z revert a gesture made on another
//! device last week (ADR-0031 D7 — undo never crosses a commit, and a quit is
//! at least as strong a boundary as a commit).
//!
//! Sessions live in a process-wide registry, the same shape
//! `impress_store_service::store_instance()` uses for the store: one map, one
//! mutex, entries created on first touch.
//!
//! # A cached session is checked against its row before every use
//!
//! Other writers hold sessions on the same row: `impress-mcp` and
//! `impress-cli` in their own processes, another chassis app on the same
//! file, another registry in this one. A session remembers the row revision
//! its tree came from ([`LayoutSession::revision`], the row's
//! `logical_clock`); [`SessionRegistry::with`] compares it with the row
//! before running anything — one indexed lookup — and when the row has moved
//! it **reloads**, dropping both undo rings, because their patches describe a
//! tree that is no longer the stored one. That is the honest outcome, and the
//! caller is told ([`LayoutSession::take_notice`]). Every save is
//! compare-and-swap on the same revision ([`LayoutSession::save`]), so the
//! window between the check and the write cannot lose a change either: a
//! save that finds the row moved writes nothing and marks the session stale
//! (review RL-L1). Before, a session loaded once was trusted forever, and
//! the next verb from a long-lived `impress-mcp` wrote its stale tree over
//! whatever the user had done since.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use impress_item::item::{ActorKind, ItemId};
use impress_layout::{
    stack_for, ChannelId, Layout, LayoutError, PaneRef, Patch, StackKind, TileId, UndoStacks, Verb,
    WindowId,
};

use crate::store::{LayoutStore, LiveWrite};
use impress_service_core::Refusal;

/// Which ring a verb's patch landed on — what a caller needs to know to route
/// the right ⌘Z back to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stack {
    /// The arrangement ring: split, move, close, resize, roles, geometry.
    Arrangement,
    /// One pane's exploration ring: query, view kind, bindings, selection.
    Exploration(TileId),
    /// Focus verbs record nothing (ADR-0031 D7): undoing a focus move is what
    /// the opposite focus move is for.
    Unrecorded,
}

impl Stack {
    pub fn name(&self) -> &'static str {
        match self {
            Stack::Arrangement => "arrangement",
            Stack::Exploration(_) => "exploration",
            Stack::Unrecorded => "none",
        }
    }

    pub fn pane(&self) -> Option<TileId> {
        match self {
            Stack::Exploration(tile) => Some(*tile),
            _ => None,
        }
    }
}

/// Which ring an `undo` / `redo` verb addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoTarget {
    Arrangement,
    /// The exploration ring of the pane this reference names.
    Exploration(PaneRef),
}

/// What applying a verb did — the renderer's whole input.
#[derive(Debug, Clone)]
pub struct AppliedVerb {
    pub patch: Patch,
    /// The window the verb resolved against.
    pub window: WindowId,
    /// The focused leaf afterwards.
    pub focused: Option<TileId>,
    /// The panes a renderer must redraw. For `select` this is
    /// [`Layout::affected_panes`] — every pane whose bindings the publication
    /// changed — plus the publishing pane itself; for every other verb it is
    /// the tiles the patch touched that are still panes.
    pub affected: Vec<TileId>,
    /// Which ring the patch went on.
    pub stack: Stack,
}

/// One `(app_id, device)` scope, live in memory.
pub struct LayoutSession {
    pub app_id: String,
    pub device: String,
    /// The `impress/ui/layout@1.0.0` row this session is the live value of.
    pub item_id: ItemId,
    pub layout: Layout,
    pub undo: UndoStacks,
    /// The live row's revision (`logical_clock`) this tree is. `None` once a
    /// guarded save found the row moved: the session is stale and the next
    /// touch reloads it.
    pub revision: Option<u64>,
    /// What the caller should be told about how this session came to be —
    /// a reload because the row moved, a quarantined row. Taken once, by the
    /// next verb (reads leave it, so a background read cannot swallow it).
    notice: Option<String>,
    /// Set when a reload dropped undo steps, until the next step is recorded:
    /// what an undo that finds its ring empty says instead of "nothing to
    /// undo", because there WAS something and it went with the old tree.
    dropped: Option<String>,
    /// The registry's write generation, bumped by every mutation below so a
    /// renderer sharing the registry learns the tree moved under it. See
    /// [`SessionRegistry::generation`].
    pub(crate) generation: Arc<std::sync::atomic::AtomicU64>,
}

impl LayoutSession {
    fn note_write(&self) {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// Persist the tree, compare-and-swap on [`Self::revision`] (see the
    /// module docs). Ephemeral retention, coalescing left to the caller: see
    /// `store`'s module docs and ADR-0019 D6.
    ///
    /// When the row moved, nothing is written, the session is marked stale
    /// ([`Self::is_stale`]) and the error says so; the caller either retries
    /// against the reloaded tree or reports the refusal.
    pub fn save(
        &mut self,
        store: &LayoutStore,
        actor: ActorKind,
        intent: &str,
    ) -> Result<(), Refusal> {
        let Some(revision) = self.revision else {
            return Err(Refusal::conflict(STALE));
        };
        match store.save_live_if(self.item_id, revision, &self.layout, actor, intent)? {
            LiveWrite::Written { revision } => {
                self.revision = Some(revision);
                Ok(())
            }
            LiveWrite::Moved { revision: now } => {
                tracing::warn!(
                    target: "layout",
                    "{}/{}: live row {} moved ({revision} → {now:?}) under a write ('{intent}'); \
                     nothing written, the session reloads",
                    self.app_id,
                    self.device,
                    self.item_id
                );
                self.revision = None;
                Err(Refusal::conflict(STALE))
            }
        }
    }

    /// Whether a save found the row moved: the tree in memory is not the
    /// stored one, and the next touch reloads it.
    pub fn is_stale(&self) -> bool {
        self.revision.is_none()
    }

    /// Record the revision a write to the live row made outside
    /// [`Self::save`] (the preset edge `apply_preset` adds), so the next
    /// check does not mistake this session's own write for someone else's.
    pub fn advance_revision(&mut self, revision: u64) {
        if self.revision.is_some() {
            self.revision = Some(revision);
        }
    }

    /// What happened while loading this session that the caller should pass
    /// on — `None` for an ordinary touch. Taken, so it is reported once.
    pub fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    /// Why the undo history is shorter than the user may expect: undo steps
    /// a reload dropped, since no new step was recorded. See the field.
    pub fn dropped_history(&self) -> Option<&str> {
        self.dropped.as_deref()
    }
}

/// What a save that lost the race says. The session has reloaded by the
/// next touch, so trying again acts on the current tree.
pub const STALE: &str = "the layout changed elsewhere while this was being applied, so nothing \
     was written; it has been reloaded — try again";

impl LayoutSession {
    /// Apply a verb, recording its patch on the ring [`stack_for`] chooses.
    pub fn apply(&mut self, verb: Verb) -> Result<AppliedVerb, LayoutError> {
        let applied = self.apply_inner(verb)?;
        if applied.stack != Stack::Unrecorded && !applied.patch.is_empty() {
            self.dropped = None;
        }
        self.note_write();
        Ok(applied)
    }

    fn apply_inner(&mut self, verb: Verb) -> Result<AppliedVerb, LayoutError> {
        let window = self.layout.current_window()?;

        // Resolved BEFORE the verb runs, for the same reason `UndoStacks::apply`
        // resolves its own destination first: after a split or a move,
        // `Focused` and a direction mean something else.
        let stack = match stack_for(&verb) {
            StackKind::Arrangement => Stack::Arrangement,
            StackKind::Exploration(reference) => match self.layout.resolve(window, &reference) {
                Ok(tile) => Stack::Exploration(tile),
                Err(_) => Stack::Unrecorded,
            },
            StackKind::None => Stack::Unrecorded,
        };

        // A selection's fan-out is a property of the channel it is published
        // on, which is the publishing pane's channel — read before the verb,
        // because `Select` may be the thing that changes what is focused.
        let selection = match &verb {
            Verb::Select { target, kind, .. } => {
                self.layout.resolve(window, target).ok().and_then(|tile| {
                    self.layout
                        .pane(tile)
                        .map(|spec| (tile, spec.channel, kind.clone()))
                })
            }
            _ => None,
        };

        let patch = self.undo.apply(&mut self.layout, verb)?;

        let affected = match selection {
            Some((tile, channel, kind)) => {
                let mut out = vec![tile];
                for pane in self.layout.affected_panes(channel, &kind) {
                    if !out.contains(&pane) {
                        out.push(pane);
                    }
                }
                out
            }
            None => patch
                .tiles
                .keys()
                .copied()
                .filter(|tile| self.layout.pane(*tile).is_some())
                .collect(),
        };

        Ok(AppliedVerb {
            patch,
            window,
            focused: self.focused(window),
            affected,
            stack,
        })
    }

    /// Apply several verbs as one gesture — all or none, one undo step
    /// (`impress_layout::UndoStacks::apply_all`). The caller refuses an
    /// empty list before it gets here.
    pub fn apply_all(&mut self, verbs: Vec<Verb>) -> Result<AppliedVerb, LayoutError> {
        let window = self.layout.current_window()?;
        let stack = match verbs
            .iter()
            .map(stack_for)
            .find(|kind| *kind != StackKind::None)
        {
            Some(StackKind::Arrangement) => Stack::Arrangement,
            Some(StackKind::Exploration(reference)) => {
                match self.layout.resolve(window, &reference) {
                    Ok(tile) => Stack::Exploration(tile),
                    Err(_) => Stack::Unrecorded,
                }
            }
            _ => Stack::Unrecorded,
        };
        // Each selection's fan-out, read before the gesture runs, exactly as
        // a lone `Select` does.
        let selections: Vec<(TileId, ChannelId, String)> = verbs
            .iter()
            .filter_map(|verb| match verb {
                Verb::Select { target, kind, .. } => {
                    self.layout.resolve(window, target).ok().and_then(|tile| {
                        self.layout
                            .pane(tile)
                            .map(|spec| (tile, spec.channel, kind.clone()))
                    })
                }
                _ => None,
            })
            .collect();
        let Some(patch) = self.undo.apply_all(&mut self.layout, verbs)? else {
            return Err(LayoutError::UndoConflict {
                what: "an empty gesture".to_string(),
            });
        };
        let mut affected: Vec<TileId> = patch
            .tiles
            .keys()
            .copied()
            .filter(|tile| self.layout.pane(*tile).is_some())
            .collect();
        for (tile, channel, kind) in selections {
            for pane in std::iter::once(tile).chain(self.layout.affected_panes(channel, &kind)) {
                if !affected.contains(&pane) && self.layout.pane(pane).is_some() {
                    affected.push(pane);
                }
            }
        }
        if stack != Stack::Unrecorded && !patch.is_empty() {
            self.dropped = None;
        }
        self.note_write();
        Ok(AppliedVerb {
            patch,
            window,
            focused: self.focused(window),
            affected,
            stack,
        })
    }

    /// Undo on one ring. `None` means the ring was empty — not an error: a ⌘Z
    /// with nothing to undo is a no-op everywhere else in macOS too.
    pub fn undo(&mut self, target: &UndoTarget) -> Result<Option<Patch>, LayoutError> {
        let pane = self.ring_pane(target)?;
        let stepped = match pane {
            Some(tile) => self.undo.undo_exploration(&mut self.layout, tile),
            None => self.undo.undo_arrangement(&mut self.layout),
        }
        .inspect_err(|e| self.log_refused_step("undo", e))?;
        if stepped.is_some() {
            self.note_write();
        }
        Ok(stepped)
    }

    /// Redo on one ring.
    pub fn redo(&mut self, target: &UndoTarget) -> Result<Option<Patch>, LayoutError> {
        let pane = self.ring_pane(target)?;
        let stepped = match pane {
            Some(tile) => self.undo.redo_exploration(&mut self.layout, tile),
            None => self.undo.redo_arrangement(&mut self.layout),
        }
        .inspect_err(|e| self.log_refused_step("redo", e))?;
        if stepped.is_some() {
            self.note_write();
        }
        Ok(stepped)
    }

    /// Forget both rings.
    ///
    /// Called on a commit, because ADR-0031 D7 says undo never crosses one:
    /// after "save this arrangement as Triage", ⌘Z must not quietly take the
    /// user back through the gestures that produced it. Reverting a commit is
    /// the ordinary item-level `inverse_of` operation on the saved row.
    pub fn commit_boundary(&mut self) {
        self.undo = UndoStacks::new(self.undo.capacity);
    }

    /// Replace the whole tree (`apply_layout`). The rings go with it: the
    /// patches on them describe tiles that no longer exist.
    pub fn replace(&mut self, layout: Layout) {
        self.layout = layout;
        self.undo = UndoStacks::new(self.undo.capacity);
        self.note_write();
    }

    /// The focused leaf of `window`, or of whatever window survived.
    pub fn focused(&self, window: WindowId) -> Option<TileId> {
        self.layout
            .window(window)
            .and_then(|w| w.focused)
            .or_else(|| {
                self.layout
                    .current_window()
                    .ok()
                    .and_then(|w| self.layout.window(w))
                    .and_then(|w| w.focused)
            })
    }

    /// The channel a pane publishes on, resolved against its window's default.
    pub fn channel_of(&self, tile: TileId) -> Option<u8> {
        let spec = self.layout.pane(tile)?;
        let window = self.layout.window_of(tile)?;
        let default = self
            .layout
            .window(window)
            .map(|w| w.default_channel)
            .unwrap_or(ChannelId::ONE);
        Some(spec.channel.resolve(default))
    }

    fn log_refused_step(&self, word: &str, error: &LayoutError) {
        tracing::info!(
            target: "layout",
            "{}/{}: {word} refused and dropped: {error}",
            self.app_id,
            self.device
        );
    }

    fn ring_pane(&self, target: &UndoTarget) -> Result<Option<TileId>, LayoutError> {
        match target {
            UndoTarget::Arrangement => Ok(None),
            UndoTarget::Exploration(reference) => {
                let window = self.layout.current_window()?;
                Ok(Some(self.layout.resolve(window, reference)?))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

type SessionMap = HashMap<(String, String), LayoutSession>;

/// The process-wide session table. Private sessions (tests, embedders) get
/// their own [`SessionRegistry`]; everything else shares this one.
#[derive(Default)]
pub struct SessionRegistry {
    sessions: Mutex<SessionMap>,
    /// Bumped by every session mutation made through this registry — see
    /// [`Self::generation`]. An `Arc` so each session can hold a handle and
    /// bump it without reaching back through the registry's lock.
    generation: Arc<std::sync::atomic::AtomicU64>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many live-row writes this registry has made. A renderer that
    /// SHARES the registry with other writers in its process (ADR-0033 D6:
    /// a surface's `open`/`publish` effect composes layout verbs) polls
    /// this to learn the tree changed under it — the in-process mutation
    /// channel reports the write as a pane invalidation, not as a tree
    /// change, and `PRAGMA data_version` is silent for the connection's own
    /// writes, so without this number a pane a surface opened sat in the
    /// store, in this very process, invisible to the window (2026-09-23).
    pub fn generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The shared registry, matching `store_instance()`'s shape.
    pub fn shared() -> Arc<SessionRegistry> {
        static GLOBAL: OnceLock<Arc<SessionRegistry>> = OnceLock::new();
        GLOBAL
            .get_or_init(|| Arc::new(SessionRegistry::new()))
            .clone()
    }

    /// Run `f` against the session for `(app_id, device)`, loading the live
    /// row (or cold-starting it) if this is the first touch, and reloading it
    /// if the row moved since this session read it (see the module docs).
    ///
    /// The lock is held for the whole closure — including the store write a
    /// verb makes — so two concurrent verbs on one scope cannot interleave
    /// into a tree that is neither's. The closure is synchronous on purpose:
    /// a guard must never be held across an `.await`.
    pub fn with<R>(
        &self,
        store: &LayoutStore,
        app_id: &str,
        device: &str,
        actor: ActorKind,
        f: impl FnOnce(&mut LayoutSession) -> R,
    ) -> Result<R, Refusal> {
        let mut sessions = self.lock();
        let key = (app_id.to_string(), device.to_string());
        let mut notice = None;
        let mut dropped = None;
        if let Some(session) = sessions.get(&key) {
            let stored = store.revision_of(session.item_id)?;
            if stored.is_none() || stored != session.revision {
                let steps = session.undo.arrangement.done.len()
                    + session.undo.arrangement.undone.len()
                    + session
                        .undo
                        .exploration
                        .values()
                        .map(|ring| ring.done.len() + ring.undone.len())
                        .sum::<usize>();
                tracing::info!(
                    target: "layout",
                    "{app_id}/{device}: live row {} moved ({:?} → {stored:?}); reloading, \
                     {steps} undo step(s) dropped",
                    session.item_id,
                    session.revision
                );
                notice = Some(if steps == 0 {
                    "The layout had changed elsewhere, so it was reloaded first.".to_string()
                } else {
                    format!(
                        "The layout had changed elsewhere, so it was reloaded first and its undo \
                         history ({steps} step(s)) was dropped."
                    )
                });
                if steps > 0 {
                    dropped = Some(format!(
                        "the layout changed elsewhere and was reloaded, which dropped {steps} undo \
                         step(s)"
                    ));
                }
                sessions.remove(&key);
            }
        }
        if !sessions.contains_key(&key) {
            let load = store.load_live(app_id, device, actor)?;
            let notice = match (notice, load.note) {
                (Some(a), Some(b)) => Some(format!("{a} {b}")),
                (a, b) => a.or(b),
            };
            let reloaded = notice.is_some();
            let mut session = LayoutSession {
                app_id: app_id.to_string(),
                device: device.to_string(),
                item_id: load.item_id,
                layout: load.layout,
                undo: UndoStacks::default(),
                revision: Some(load.revision),
                notice,
                dropped,
                generation: self.generation.clone(),
            };
            // A tree stored before its panes had sessions (or cold-started
            // from a preset, which carries none) is given them now, and saved
            // at once: a second process loading the same row must read the
            // SAME ids, or its next save would re-key every editor (D6).
            if session.layout.ensure_sessions() {
                session.save(store, actor, "gave session-bearing panes their sessions")?;
            }
            if reloaded {
                // A renderer sharing this registry holds the old tree.
                session.note_write();
            }
            sessions.insert(key.clone(), session);
        }
        let session = sessions
            .get_mut(&key)
            .expect("the session was just inserted");
        Ok(f(session))
    }

    /// The revision the cached session for a scope holds, without touching
    /// the store: `None` when no session is loaded (or it is stale). What a
    /// renderer compares a row it saw change with, to tell its own write
    /// from someone else's.
    pub fn revision_of(&self, app_id: &str, device: &str) -> Option<u64> {
        self.lock()
            .get(&(app_id.to_string(), device.to_string()))
            .and_then(|session| session.revision)
    }

    /// Drop a scope's session, so the next touch re-reads the live row.
    ///
    /// No longer how another writer's change is picked up — [`Self::with`]
    /// checks the row's revision on every touch and reloads by itself, and
    /// the FFI feed stopped calling this (it used to, for every layout row
    /// any app wrote, which cost every running app its undo rings on each
    /// keystroke elsewhere — review RL-L2). It remains for a caller that
    /// wants the rings gone.
    pub fn forget(&self, app_id: &str, device: &str) {
        self.lock()
            .remove(&(app_id.to_string(), device.to_string()));
    }

    /// How many scopes are live in memory. Diagnostics and tests.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> MutexGuard<'_, SessionMap> {
        // A panic inside the closure cannot leave a session half-written — a
        // verb applies to a scratch clone and commits whole — so recover from
        // poisoning rather than propagate it, exactly as the store cell does.
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
