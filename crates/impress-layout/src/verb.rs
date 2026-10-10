//! The closed verb vocabulary (ADR-0031 D8) and the pane references every verb
//! takes.
//!
//! What is *not* here is as deliberate as what is: `commit`, `save_layout`,
//! `apply_layout`, `undo` and `redo` are service verbs (L3) because they touch
//! the store or a ring, and this crate is pure. The rings themselves live here
//! ([`crate::UndoRing`]) so the service only has to route.

use impress_pane_query::{ItemId, PaneQuery, ParamName, RecordKindId};
use serde::{Deserialize, Serialize};

use crate::ids::{ChannelId, Role, TileId, ViewKindId, WindowId};
use crate::spec::{PaneSpec, ParamSource};
use crate::tree::{ContainerKind, Geometry, LinearDir};

/// How a verb names a pane. Resolution happens once, in Rust
/// ([`crate::Layout::resolve`]).
///
/// **One spelling everywhere** (review RL-L3, AC-F3): MCP, the CLI, HTTP's
/// `/api/layout/verb`, the FFI, the outline's verbs and every message that
/// echoes a verb write a reference as an object with exactly ONE of
///
/// ```json
/// {"id": 7}   {"role": "detail"}   {"direction": "left"}   {"focused": true}
/// ```
///
/// ([`PaneRefWire`]). The internally tagged `{"ref": "id", "tile": 7}` this
/// type used to serialize as was retired with wire version 1: nothing
/// persisted it (undo rings live in memory), so no transition reads it, and
/// a caller still sending it is told `unknown field 'ref'`. An empty `{}`
/// no longer means the focused pane — `{"focused": true}` says so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PaneRefWire", into = "PaneRefWire")]
pub enum PaneRef {
    /// Canonical: what the operation log and the tests use.
    Id { tile: TileId },
    /// What chords and agents say: "the detail pane", whichever tile that is.
    Role { role: Role },
    /// What h / l and drag gestures produce: a step from the focused leaf.
    Direction { direction: Direction },
    /// The focused leaf itself.
    Focused,
}

impl PaneRef {
    pub fn id(tile: TileId) -> Self {
        PaneRef::Id { tile }
    }

    pub fn role(role: Role) -> Self {
        PaneRef::Role { role }
    }

    pub fn direction(direction: Direction) -> Self {
        PaneRef::Direction { direction }
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for PaneRef {
    fn schema_name() -> String {
        "PaneRef".to_string()
    }

    fn json_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        <PaneRefWire as schemars::JsonSchema>::json_schema(gen)
    }
}

/// A pane reference as it is written: exactly one of `id`, `role`,
/// `direction` or `focused: true` (see [`PaneRef`]). Unknown fields are
/// refused — a reference is only ever an argument, never a stored value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PaneRefWire {
    /// Canonical: the tile id. What the log and the tests use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    /// `navigator` | `list` | `detail` | `preview` | `console` | any role a
    /// preset assigned. What chords and agents say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// `left` | `right` | `up` | `down` | `next` | `prev` (also `h` `l` `k`
    /// `j`) — a step from the focused leaf. What h / l produce.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
    /// `true`: the focused leaf itself. Must be said; `{}` names nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<bool>,
}

impl PaneRefWire {
    pub fn tile(id: TileId) -> Self {
        Self {
            id: Some(id.raw()),
            ..Default::default()
        }
    }

    pub fn role(role: &str) -> Self {
        Self {
            role: Some(role.to_string()),
            ..Default::default()
        }
    }

    pub fn direction(direction: Direction) -> Self {
        Self {
            direction: Some(direction),
            ..Default::default()
        }
    }

    pub fn focused() -> Self {
        Self {
            focused: Some(true),
            ..Default::default()
        }
    }

    /// The reference this names. Exactly one selector, or a refusal saying
    /// what was wrong — never a guess (an empty reference used to mean the
    /// focused pane, so a reference in the wrong spelling closed it).
    pub fn to_pane_ref(&self) -> Result<PaneRef, String> {
        let role = self.role.as_deref().map(str::trim);
        let mut named: Vec<&str> = Vec::new();
        if self.id.is_some() {
            named.push("id");
        }
        if role.is_some() {
            named.push("role");
        }
        if self.direction.is_some() {
            named.push("direction");
        }
        match self.focused {
            Some(true) => named.push("focused"),
            Some(false) => {
                return Err(
                    "a pane reference with `\"focused\": false` names no pane; give \
                            {\"id\": N}, {\"role\": …} or {\"direction\": …} instead"
                        .to_string(),
                )
            }
            None => {}
        }
        match named.as_slice() {
            [] => Err(
                "a pane reference must name a pane: give exactly one of {\"id\": N}, \
                       {\"role\": \"detail\"}, {\"direction\": \"left\"} or \
                       {\"focused\": true}"
                    .to_string(),
            ),
            ["id"] => Ok(PaneRef::id(TileId::new(self.id.unwrap_or_default()))),
            ["role"] => match role {
                Some(role) if !role.is_empty() => Ok(PaneRef::role(Role::from(role.to_string()))),
                _ => Err("a pane reference's `role` is empty".to_string()),
            },
            ["direction"] => Ok(PaneRef::direction(
                self.direction.unwrap_or(Direction::Next),
            )),
            ["focused"] => Ok(PaneRef::Focused),
            several => Err(format!(
                "a pane reference names one pane, but this one sets {}; keep exactly one",
                several.join(" and ")
            )),
        }
    }
}

impl TryFrom<PaneRefWire> for PaneRef {
    type Error = String;

    fn try_from(wire: PaneRefWire) -> Result<Self, Self::Error> {
        wire.to_pane_ref()
    }
}

impl From<PaneRef> for PaneRefWire {
    fn from(reference: PaneRef) -> Self {
        PaneRefWire::from(&reference)
    }
}

impl From<&PaneRef> for PaneRefWire {
    fn from(reference: &PaneRef) -> Self {
        match reference {
            PaneRef::Id { tile } => PaneRefWire::tile(*tile),
            PaneRef::Role { role } => PaneRefWire::role(role.as_str()),
            PaneRef::Direction { direction } => PaneRefWire::direction(*direction),
            PaneRef::Focused => PaneRefWire::focused(),
        }
    }
}

/// A step from the focused leaf.
///
/// `Next` / `Prev` walk the window's leaves in tree order and wrap, which is
/// exactly what `PaneFocusCycler` does today. `Left` / `Right` / `Up` / `Down`
/// are spatial-ish rather than spatial: see [`crate::Layout::resolve`] for the
/// rule, which is stated there once and holds for every caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum Direction {
    #[serde(alias = "h")]
    Left,
    #[serde(alias = "l")]
    Right,
    #[serde(alias = "k")]
    Up,
    #[serde(alias = "j")]
    Down,
    #[serde(alias = "forward")]
    Next,
    #[serde(alias = "previous", alias = "back")]
    Prev,
}

impl Direction {
    /// The linear axis this direction steps along, if any.
    pub fn axis(self) -> Option<LinearDir> {
        match self {
            Direction::Left | Direction::Right => Some(LinearDir::Horizontal),
            Direction::Up | Direction::Down => Some(LinearDir::Vertical),
            Direction::Next | Direction::Prev => None,
        }
    }

    /// Whether the step goes towards later siblings.
    pub fn is_forward(self) -> bool {
        matches!(self, Direction::Right | Direction::Down | Direction::Next)
    }
}

/// Where a moved tile lands relative to its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum Placement {
    Left,
    Right,
    Above,
    Below,
    /// Tab the moved tile alongside the target (pyqtgraph's centre drop zone).
    IntoTabs,
}

impl Placement {
    pub(crate) fn linear(self) -> Option<(LinearDir, bool)> {
        match self {
            Placement::Left => Some((LinearDir::Horizontal, false)),
            Placement::Right => Some((LinearDir::Horizontal, true)),
            Placement::Above => Some((LinearDir::Vertical, false)),
            Placement::Below => Some((LinearDir::Vertical, true)),
            Placement::IntoTabs => None,
        }
    }
}

/// Every gesture, as a value (ADR-0031 D8, invariant 6: no Swift-only layout
/// operation).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "verb", rename_all = "kebab-case")]
pub enum Verb {
    // ---- arrangement ----
    /// Split `target` along `dir`, putting `new` after it (or before, when
    /// `after` is false). Focus follows the new pane.
    ///
    /// `new` omitted duplicates the pane being split — minus its role (D5)
    /// and its session (D6) — which is what a bare "split this" gesture
    /// means. One shape on every path (review RL-L20): MCP, the CLI, the FFI
    /// and HTTP all send this verb, and the rule lives in `apply`.
    Split {
        target: PaneRef,
        dir: LinearDir,
        #[serde(default)]
        after: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new: Option<PaneSpec>,
    },
    /// Move `tile` next to (or into the tabs of) `target`.
    MoveTile {
        tile: PaneRef,
        target: PaneRef,
        placement: Placement,
    },
    /// Close a pane or a whole subtree. Never the last pane of a window.
    Close {
        target: PaneRef,
    },
    /// Exchange two tiles' positions, keeping each position's share.
    Swap {
        a: PaneRef,
        b: PaneRef,
    },
    /// Set a linear container's relative shares.
    Resize {
        container: TileId,
        shares: Vec<f32>,
    },
    /// Retype a container, keeping its children in order.
    SetContainerKind {
        container: TileId,
        kind: ContainerKind,
    },
    /// Collapse a pane to no width in its split, or show it again at the
    /// share it had — what ⌃⌘S does to the navigator (review RL-L13).
    /// `collapsed` omitted toggles, decided here against the tree as it is,
    /// under the same lock as every other verb. Refused (`not-in-a-split`)
    /// for a pane that is not the child of a split.
    SetCollapsed {
        target: PaneRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        collapsed: Option<bool>,
    },
    /// Show `target` alone in its window. Does not mutate the tree.
    Maximize {
        target: PaneRef,
    },
    /// Undo a maximize. A no-op when nothing is maximized.
    Restore,
    /// Move a pane (or a whole subtree) out into a **new window** whose root
    /// it becomes — D4's detached PDF. Focus follows it; the window it left
    /// re-normalizes. Refused when the tile is the whole source window: that
    /// would be a rename, not a detach.
    Detach {
        target: PaneRef,
    },

    // ---- content ----
    /// Replace a pane's whole spec.
    SetPane {
        target: PaneRef,
        spec: PaneSpec,
    },
    SetQuery {
        target: PaneRef,
        query: PaneQuery,
    },
    SetViewKind {
        target: PaneRef,
        view_kind: ViewKindId,
    },
    /// Focus the window's list, or split one in beside the navigator.
    ///
    /// The new list's kinds come from a remaining detail pane (or any
    /// item-scoped pane), never from the navigator's collections and
    /// libraries. `kinds`, when set, is the fallback for a window that no
    /// longer has such a pane — the app's cold-start list. With neither, the
    /// verb is refused `no-list-kinds`. A list the window already has is
    /// focused, and that focus is not an arrangement step.
    OpenList {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kinds: Option<Vec<String>>,
    },
    /// Write a pane's detail-strip order (`view_state.tab_order`).
    ///
    /// `tabs` is the view-kind ids in display order. An unknown id is
    /// `unknown-view-kind`; an empty list is `empty-tab-order`; a repeated
    /// id is `duplicate-tab`. Other `view_state` keys, the query, and focus
    /// stay as they are. This is exploration: ⌘Z undoes it.
    SetTabOrder {
        target: PaneRef,
        tabs: Vec<ViewKindId>,
    },
    /// Re-point one declared parameter at a different source.
    BindParam {
        target: PaneRef,
        name: ParamName,
        source: ParamSource,
    },
    /// Change the channel a pane publishes on.
    SetChannel {
        target: PaneRef,
        channel: ChannelId,
    },
    /// Give, move or clear a role.
    SetRole {
        target: PaneRef,
        #[serde(default)]
        role: Option<Role>,
    },

    // ---- focus / selection ----
    Focus {
        target: PaneRef,
    },
    FocusDirection {
        direction: Direction,
    },
    /// Publish a selection of `kind` on the pane's channel.
    Select {
        target: PaneRef,
        kind: RecordKindId,
        #[cfg_attr(feature = "schema", schemars(with = "Vec<String>"))]
        ids: Vec<ItemId>,
    },

    // ---- window ----
    /// Replace (or clear) a window's device-scoped frame.
    SetWindowGeometry {
        window: WindowId,
        #[serde(default)]
        geometry: Option<Geometry>,
    },
    /// Set what [`ChannelId::Follow`] means in one window. `Follow` itself is
    /// refused: a window default that follows itself is not a value.
    SetDefaultChannel {
        window: WindowId,
        channel: ChannelId,
    },
}
