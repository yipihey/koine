//! Why a verb did not apply.
//!
//! Every variant is a *typed* refusal that leaves the layout untouched: verbs
//! are applied to a scratch clone and committed only on success, so a failed
//! verb can never leave a half-mutated tree (which, in a tree whose whole point
//! is legibility, would be worse than the failure).

use crate::ids::{ChannelId, Role, TileId, ViewKindId, WindowId};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "error", rename_all = "kebab-case")]
pub enum LayoutError {
    #[error("no tile {tile} in this layout")]
    UnknownTile { tile: TileId },

    #[error("no window {window} in this layout")]
    UnknownWindow { window: WindowId },

    #[error("no pane carries the role '{role}'")]
    NoPaneWithRole { role: Role },

    #[error("tile {tile} is a container, not a pane")]
    NotAPane { tile: TileId },

    #[error("tile {tile} is a pane, not a container")]
    NotAContainer { tile: TileId },

    #[error("a window must keep at least one pane")]
    CannotCloseLastPane,

    /// `detach` of a tile that already holds every pane of its window: there
    /// is nothing to leave behind (review RL-L11 — this used to answer
    /// `CannotCloseLastPane`, whose text is about closing).
    #[error(
        "tile {tile} is already the whole of its window, so there is nothing to detach it from"
    )]
    AlreadyItsOwnWindow { tile: TileId },

    /// `move-tile` of a window root when that window is the only one: moving
    /// it would leave no window at all.
    #[error("tile {tile} is the whole of the only window, so it cannot be moved")]
    CannotMoveWholeWindow { tile: TileId },

    #[error("window {window} has no focused pane")]
    NoFocus { window: WindowId },

    #[error("channel '{channel}' cannot be used here")]
    InvalidChannel { channel: ChannelId },

    #[error("invalid shares: {reason}")]
    InvalidShares { reason: String },

    #[error("cannot move tile {tile} into its own subtree")]
    CyclicMove { tile: TileId },

    #[error("pane {tile} declares no parameter named '{name}'")]
    UnknownParam { tile: TileId, name: String },

    /// A verb that would leave two panes of one window carrying the same role
    /// (a swap or move across windows, a `set-pane` naming a role another
    /// pane holds). Roles are unique per window (ADR-0031 D5) because the
    /// universal chords act on "the" pane with a role; `set-role` is the verb
    /// that moves one.
    #[error(
        "the role '{role}' would be held by two panes in window {window}; use set-role to move it"
    )]
    RoleHeldTwice { role: Role, window: WindowId },

    /// An undo or redo step whose recorded value is no longer what the layout
    /// holds: something else changed it since (another ring, another pane,
    /// another writer). Refused rather than replayed, because replaying would
    /// silently undo that later change too (review RL-L4).
    #[error("{what} changed since this step was recorded, so the step was dropped and nothing was changed")]
    UndoConflict { what: String },

    /// `set-collapsed` of a pane whose parent is not a split: only a split
    /// has shares to collapse to.
    #[error("tile {tile} is not the child of a split, so it has no share to collapse")]
    NotInASplit { tile: TileId },

    /// A verb named a view kind no host renders (`set-view-kind`,
    /// `set-pane`, a split's new pane). The vocabulary is
    /// [`ViewKindId::KNOWN`]; a stored tree naming an unknown kind still
    /// loads and renders a placeholder, but a verb never writes one (review
    /// RL-L12, PH-M7).
    #[error("'{view_kind}' is not a view kind; use one of {known}")]
    UnknownViewKind {
        view_kind: ViewKindId,
        known: String,
    },

    /// `open-list` found nothing to copy and the caller named no kinds.
    #[error("open-list needs a detail pane to copy, or kinds for the list")]
    NoListKinds,

    /// `set-tab-order` with no tabs.
    #[error("set-tab-order needs at least one tab")]
    EmptyTabOrder,

    /// `set-tab-order` named one view kind twice.
    #[error("set-tab-order lists '{tab}' twice")]
    DuplicateTab { tab: ViewKindId },
}

impl LayoutError {
    /// Refuse `view_kind` unless it is in the vocabulary.
    pub fn check_view_kind(view_kind: &ViewKindId) -> Result<(), LayoutError> {
        if view_kind.is_known() {
            Ok(())
        } else {
            Err(LayoutError::UnknownViewKind {
                view_kind: view_kind.clone(),
                known: ViewKindId::known_list(),
            })
        }
    }

    /// The stable, machine-readable name of this refusal — its serde tag
    /// (`unknown-tile`, `cannot-close-last-pane`, …). This is the `code`
    /// every layout result carries next to its prose `message` (review
    /// RL-L11); `tests::the_code_is_the_serde_tag` pins the two together.
    pub fn code(&self) -> &'static str {
        match self {
            LayoutError::UnknownTile { .. } => "unknown-tile",
            LayoutError::UnknownWindow { .. } => "unknown-window",
            LayoutError::NoPaneWithRole { .. } => "no-pane-with-role",
            LayoutError::NotAPane { .. } => "not-a-pane",
            LayoutError::NotAContainer { .. } => "not-a-container",
            LayoutError::CannotCloseLastPane => "cannot-close-last-pane",
            LayoutError::AlreadyItsOwnWindow { .. } => "already-its-own-window",
            LayoutError::CannotMoveWholeWindow { .. } => "cannot-move-whole-window",
            LayoutError::NoFocus { .. } => "no-focus",
            LayoutError::InvalidChannel { .. } => "invalid-channel",
            LayoutError::InvalidShares { .. } => "invalid-shares",
            LayoutError::CyclicMove { .. } => "cyclic-move",
            LayoutError::UnknownParam { .. } => "unknown-param",
            LayoutError::RoleHeldTwice { .. } => "role-held-twice",
            LayoutError::UndoConflict { .. } => "undo-conflict",
            LayoutError::NotInASplit { .. } => "not-in-a-split",
            LayoutError::UnknownViewKind { .. } => "unknown-view-kind",
            LayoutError::NoListKinds => "no-list-kinds",
            LayoutError::EmptyTabOrder => "empty-tab-order",
            LayoutError::DuplicateTab { .. } => "duplicate-tab",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_code_is_the_serde_tag() {
        let tile = TileId::new(3);
        let window = WindowId::new(1);
        let all = [
            LayoutError::UnknownTile { tile },
            LayoutError::UnknownWindow { window },
            LayoutError::NoPaneWithRole {
                role: Role::from("detail".to_string()),
            },
            LayoutError::NotAPane { tile },
            LayoutError::NotAContainer { tile },
            LayoutError::CannotCloseLastPane,
            LayoutError::AlreadyItsOwnWindow { tile },
            LayoutError::CannotMoveWholeWindow { tile },
            LayoutError::NoFocus { window },
            LayoutError::InvalidChannel {
                channel: ChannelId::ONE,
            },
            LayoutError::InvalidShares { reason: "x".into() },
            LayoutError::CyclicMove { tile },
            LayoutError::UnknownParam {
                tile,
                name: "x".into(),
            },
            LayoutError::RoleHeldTwice {
                role: Role::from("list".to_string()),
                window,
            },
            LayoutError::UndoConflict { what: "x".into() },
            LayoutError::NotInASplit { tile },
            LayoutError::UnknownViewKind {
                view_kind: ViewKindId::from("holodeck".to_string()),
                known: ViewKindId::known_list(),
            },
            LayoutError::NoListKinds,
            LayoutError::EmptyTabOrder,
            LayoutError::DuplicateTab {
                tab: ViewKindId::from("pdf".to_string()),
            },
        ];
        for error in all {
            let json = serde_json::to_value(&error).unwrap();
            assert_eq!(json["error"], error.code(), "{error:?}");
        }
    }
}
