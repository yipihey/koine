//! The tree itself (ADR-0031 D4): an arena of tiles, each a pane or a
//! container, with one root per window. This is the egui_tiles shape —
//! `Tabs | Linear | Grid` over a flat `Map<TileId, Tile>` — chosen because the
//! whole thing is then one serde value that diffs, rings and syncs for free.

use serde::{Deserialize, Serialize};

use crate::ids::{ChannelId, TileId, WindowId};
use crate::spec::PaneSpec;

/// Which way a [`Container::Linear`] lays its children out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum LinearDir {
    /// Children side by side, left to right. An `NSSplitView` with vertical
    /// dividers; the h / l grammar walks this axis.
    Horizontal,
    /// Children stacked, top to bottom.
    Vertical,
}

impl LinearDir {
    pub fn is_horizontal(self) -> bool {
        matches!(self, LinearDir::Horizontal)
    }
}

/// The kind of a container, without its contents: what
/// [`crate::Verb::SetContainerKind`] retypes a container *to*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum ContainerKind {
    Tabs,
    Horizontal,
    Vertical,
    Grid,
}

/// A container tile: what holds other tiles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum Container {
    /// A tab strip. `active` is the visible child.
    Tabs {
        children: Vec<TileId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active: Option<TileId>,
    },
    /// A split. `shares` are *relative weights*, one per child, positive and
    /// finite; the renderer divides by their sum. Relative rather than
    /// normalized so that a preset can say "1 : 2 : 3" and mean it, and so
    /// that joining a nested linear into its parent is plain arithmetic.
    Linear {
        dir: LinearDir,
        children: Vec<TileId>,
        #[serde(default)]
        shares: Vec<f32>,
    },
    /// A grid. `columns` is a hint; `None` means "let the renderer choose".
    Grid {
        children: Vec<TileId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        columns: Option<u16>,
    },
}

impl Container {
    /// An empty container of the given kind.
    pub fn empty(kind: ContainerKind) -> Self {
        Self::with_children(kind, Vec::new())
    }

    /// A container of the given kind holding `children`, with uniform shares.
    pub fn with_children(kind: ContainerKind, children: Vec<TileId>) -> Self {
        match kind {
            ContainerKind::Tabs => Container::Tabs {
                active: children.first().copied(),
                children,
            },
            ContainerKind::Horizontal | ContainerKind::Vertical => {
                let dir = if matches!(kind, ContainerKind::Horizontal) {
                    LinearDir::Horizontal
                } else {
                    LinearDir::Vertical
                };
                let shares = vec![1.0; children.len()];
                Container::Linear {
                    dir,
                    children,
                    shares,
                }
            }
            ContainerKind::Grid => Container::Grid {
                children,
                columns: None,
            },
        }
    }

    /// A linear container holding `children` with the given relative shares.
    pub fn linear(dir: LinearDir, children: Vec<TileId>, shares: Vec<f32>) -> Self {
        Container::Linear {
            dir,
            children,
            shares,
        }
    }

    pub fn kind(&self) -> ContainerKind {
        match self {
            Container::Tabs { .. } => ContainerKind::Tabs,
            Container::Linear { dir, .. } => match dir {
                LinearDir::Horizontal => ContainerKind::Horizontal,
                LinearDir::Vertical => ContainerKind::Vertical,
            },
            Container::Grid { .. } => ContainerKind::Grid,
        }
    }

    pub fn children(&self) -> &[TileId] {
        match self {
            Container::Tabs { children, .. }
            | Container::Linear { children, .. }
            | Container::Grid { children, .. } => children,
        }
    }

    pub fn len(&self) -> usize {
        self.children().len()
    }

    pub fn is_empty(&self) -> bool {
        self.children().is_empty()
    }

    pub fn index_of(&self, id: TileId) -> Option<usize> {
        self.children().iter().position(|c| *c == id)
    }

    /// The relative shares, for a linear container.
    pub fn shares(&self) -> Option<&[f32]> {
        match self {
            Container::Linear { shares, .. } => Some(shares),
            _ => None,
        }
    }

    /// Insert `child` at `index`, giving it `share` (linear containers only;
    /// `None` means "the average of what is already there", so an insert never
    /// shrinks the tree to nothing).
    pub(crate) fn insert_child(&mut self, index: usize, child: TileId, share: Option<f32>) {
        match self {
            Container::Tabs { children, active } => {
                let index = index.min(children.len());
                children.insert(index, child);
                if active.is_none() {
                    *active = Some(child);
                }
            }
            Container::Linear {
                children, shares, ..
            } => {
                let index = index.min(children.len());
                let fallback = average_share(shares);
                children.insert(index, child);
                shares.insert(index.min(shares.len()), share.unwrap_or(fallback));
            }
            Container::Grid { children, .. } => {
                let index = index.min(children.len());
                children.insert(index, child);
            }
        }
    }

    /// Append `child` with the default share.
    pub(crate) fn push_child(&mut self, child: TileId) {
        self.insert_child(self.len(), child, None);
    }

    /// Remove the child at `index`, returning it and (for a linear container)
    /// the share it held.
    pub(crate) fn remove_child_at(&mut self, index: usize) -> (TileId, Option<f32>) {
        match self {
            Container::Tabs { children, active } => {
                let removed = children.remove(index);
                if *active == Some(removed) {
                    *active = children.get(index).or_else(|| children.last()).copied();
                }
                (removed, None)
            }
            Container::Linear {
                children, shares, ..
            } => {
                let removed = children.remove(index);
                let share = if index < shares.len() {
                    Some(shares.remove(index))
                } else {
                    None
                };
                (removed, share)
            }
            Container::Grid { children, .. } => (children.remove(index), None),
        }
    }

    /// Replace the child at `index` in place, keeping its share and (for tabs)
    /// its active-ness.
    pub(crate) fn replace_child_at(&mut self, index: usize, new: TileId) -> TileId {
        match self {
            Container::Tabs { children, active } => {
                let old = std::mem::replace(&mut children[index], new);
                if *active == Some(old) {
                    *active = Some(new);
                }
                old
            }
            Container::Linear { children, .. } | Container::Grid { children, .. } => {
                std::mem::replace(&mut children[index], new)
            }
        }
    }

    /// The share held by the child at `index`, for a linear container.
    pub(crate) fn share_at(&self, index: usize) -> Option<f32> {
        match self {
            Container::Linear { shares, .. } => shares.get(index).copied(),
            _ => None,
        }
    }

    pub(crate) fn set_share_at(&mut self, index: usize, share: f32) {
        if let Container::Linear { shares, .. } = self {
            if index < shares.len() {
                shares[index] = share;
            }
        }
    }
}

pub(crate) fn average_share(shares: &[f32]) -> f32 {
    if shares.is_empty() {
        1.0
    } else {
        let sum: f32 = shares.iter().sum();
        let avg = sum / shares.len() as f32;
        if avg.is_finite() && avg > 0.0 {
            avg
        } else {
            1.0
        }
    }
}

/// A tile: the fundamental unit of the arena. Either a pane or a container.
///
/// A `PaneSpec` is much larger than a `Container` — it carries a whole query,
/// its parameters and the view kind's opaque state. It is not boxed anyway: a
/// window holds tens of tiles, not thousands, and every walk in this crate
/// reads panes far more often than it moves tiles, so the indirection would
/// cost more than the padding.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum Tile {
    Pane(PaneSpec),
    Container(Container),
}

impl Tile {
    pub fn as_pane(&self) -> Option<&PaneSpec> {
        match self {
            Tile::Pane(p) => Some(p),
            Tile::Container(_) => None,
        }
    }

    pub fn as_pane_mut(&mut self) -> Option<&mut PaneSpec> {
        match self {
            Tile::Pane(p) => Some(p),
            Tile::Container(_) => None,
        }
    }

    pub fn as_container(&self) -> Option<&Container> {
        match self {
            Tile::Container(c) => Some(c),
            Tile::Pane(_) => None,
        }
    }

    pub fn as_container_mut(&mut self) -> Option<&mut Container> {
        match self {
            Tile::Container(c) => Some(c),
            Tile::Pane(_) => None,
        }
    }

    pub fn is_pane(&self) -> bool {
        matches!(self, Tile::Pane(_))
    }
}

/// Window frame, in screen points. **Device-scoped** (ADR-0019 D2): it
/// serializes with the layout but is opaque to every rule in this crate — no
/// resolution, normalization or verb reads it except
/// [`crate::Verb::SetWindowGeometry`], which replaces it wholesale.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Geometry {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// The display this frame is in, when the host can name one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

/// One window: a tree, the focused leaf, the default channel `follow`
/// resolves to, and a device-scoped frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Window {
    pub id: WindowId,
    pub root: TileId,
    /// The focused *leaf*. Kept valid by every verb: it is a pane of this
    /// window, or `None` only while the window is being built.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<TileId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<Geometry>,
    /// What [`ChannelId::Follow`] means in this window (ADR-0031 D3).
    #[serde(default)]
    pub default_channel: ChannelId,
    /// The tile shown alone, if any. Zoom is a view state of the window, not a
    /// mutation of the tree: maximizing and restoring must leave every share
    /// and every session exactly as they were.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximized: Option<TileId>,
}

impl Window {
    pub fn new(id: WindowId, root: TileId) -> Self {
        Self {
            id,
            root,
            focused: None,
            geometry: None,
            default_channel: ChannelId::ONE,
            maximized: None,
        }
    }
}
