//! What a person did, as every frontend reports it.
//!
//! A Swift click, a terminal SGR report, and a key are the same value here.
//! The frontend decides which cell or view was hit. [`layout_verb`] is the
//! layout verb that value already determines: a tile focus, a direction, a
//! drag of one tile onto another, a tab click that names a known view kind.
//! A button's activate stays a surface `click` for `reduce`. A secondary
//! click stays a menu the host opens. A drag of a tab out of its pane needs
//! the new spec, so [`split_axes`] names the split and the host attaches the
//! spec. A resize names an edge; [`resize_verb`] turns the lengths the host
//! measured into the shares `resize` takes.
//!
//! Directions and placements are the layout tree's words
//! ([`Direction`](crate::layout::Direction),
//! [`Placement`](crate::layout::Placement)), so a drag that ends on an edge
//! is already the argument `move-tile` takes.

use serde::{Deserialize, Serialize};

use crate::layout::{
    Container, Direction, Layout, LinearDir, PaneRef, Placement, Tile, TileId, Verb, ViewKindId,
};

/// The element a gesture names. A surface widget and a layout tile are both
/// addresses a frontend can point at. [`Element::Focused`] is the one the
/// frontend's cursor is on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Element {
    Widget { id: String },
    Tile { id: u64 },
    Focused,
}

/// One edge of a frame. Dragging the frame resizes; dropping a handle on an
/// edge places the pane on that side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    /// The placement a drop on this edge is.
    pub fn placement(self) -> Placement {
        match self {
            Edge::Left => Placement::Left,
            Edge::Right => Placement::Right,
            Edge::Top => Placement::Above,
            Edge::Bottom => Placement::Below,
        }
    }
}

/// A pointer button the chassis acts on. The wheel and the middle button
/// stay with the pane's content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Pointer {
    Primary,
    Secondary,
}

/// The shared gesture. Serialized with an internal `gesture` tag, so a host
/// on either frontend can send the same JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "gesture", rename_all = "kebab-case")]
pub enum Gesture {
    /// The pointer or a focus key landed on `element`.
    Focus { element: Element },
    /// Enter, or a primary click that activates (a tab, a button).
    Activate { element: Element },
    /// The menu for `element`: a secondary click, or the menu key.
    Secondary { element: Element },
    /// A directional step. `j`/`k` and `h`/`l` are this, on every frontend.
    Move { direction: Direction },
    /// A handle dragged onto another pane. `placement` is where it landed.
    Drag {
        element: Element,
        target: Element,
        placement: Placement,
    },
    /// A frame dragged. `edge` is the side that moved.
    Resize { element: Element, edge: Edge },
}

/// Move `items[from]` so that it lands at the drop index `to`.
///
/// `to` is an index in the list *before* the removal, and `to == len` means
/// after the last item. Dropping an item on itself, or on the neighbour that
/// leaves the order unchanged, returns `None`.
pub fn reorder<T: Clone>(items: &[T], from: usize, to: usize) -> Option<Vec<T>> {
    if from >= items.len() || to > items.len() {
        return None;
    }
    let mut dest = to;
    if from < dest {
        dest -= 1;
    }
    if from == dest {
        return None;
    }
    let mut out = items.to_vec();
    let item = out.remove(from);
    out.insert(dest.min(out.len()), item);
    Some(out)
}

/// The layout verb `gesture` already determines.
///
/// `pane` is the tile the pointer landed in. It is used when an activate
/// names a known view kind: that click shows the kind on the pane. Any other
/// activate is a surface event, and this returns `None`. A drag of a tile
/// onto itself returns `None`; the host asks [`tab_split_target`] for the
/// sibling and calls this again. A resize returns `None`; the host measures
/// the two sides and calls [`resize_verb`].
pub fn layout_verb(gesture: &Gesture, pane: Option<u64>) -> Option<Verb> {
    match gesture {
        Gesture::Focus {
            element: Element::Tile { id },
        } => Some(focus_tile(*id)),
        Gesture::Focus {
            element: Element::Focused,
        } => Some(Verb::Focus {
            target: PaneRef::Focused,
        }),
        Gesture::Activate {
            element: Element::Widget { id },
        } => show_kind(pane?, id),
        Gesture::Move { direction } => Some(Verb::FocusDirection {
            direction: *direction,
        }),
        Gesture::Drag {
            element: Element::Tile { id: tile },
            target: Element::Tile { id: target },
            placement,
        } if tile != target => Some(Verb::MoveTile {
            tile: PaneRef::id(TileId(*tile)),
            target: PaneRef::id(TileId(*target)),
            placement: *placement,
        }),
        Gesture::Focus { .. }
        | Gesture::Activate { .. }
        | Gesture::Secondary { .. }
        | Gesture::Drag { .. }
        | Gesture::Resize { .. } => None,
    }
}

fn focus_tile(id: u64) -> Verb {
    Verb::Focus {
        target: PaneRef::id(TileId(id)),
    }
}

fn show_kind(pane: u64, kind: &str) -> Option<Verb> {
    let view_kind = ViewKindId::from(kind.to_string());
    if !view_kind.is_known() {
        return None;
    }
    Some(Verb::SetViewKind {
        target: PaneRef::id(TileId(pane)),
        view_kind,
    })
}

/// Where a spawned pane is inserted relative to its target.
///
/// `IntoTabs` is a split to the right. A pane that does not exist yet cannot
/// join a tab strip, so the center of another pane opens it beside that pane.
pub fn split_axes(placement: Placement) -> (LinearDir, bool) {
    match placement {
        Placement::Left => (LinearDir::Horizontal, false),
        Placement::Right | Placement::IntoTabs => (LinearDir::Horizontal, true),
        Placement::Above => (LinearDir::Vertical, false),
        Placement::Below => (LinearDir::Vertical, true),
    }
}

/// The sibling a drop on one's own edge splits beside.
///
/// `children` is the tab strip, in order. Left and above prefer the previous
/// tab; right and below prefer the next. A tab at the end of the strip uses
/// the tab on the other side. The center of the strip is not a split.
pub fn tab_split_target(children: &[u64], index: usize, placement: Placement) -> Option<u64> {
    if children.len() < 2 || index >= children.len() || placement == Placement::IntoTabs {
        return None;
    }
    let neighbour = match placement {
        Placement::Left | Placement::Above => {
            if index > 0 {
                index - 1
            } else {
                index + 1
            }
        }
        Placement::Right | Placement::Below => {
            if index + 1 < children.len() {
                index + 1
            } else {
                index - 1
            }
        }
        Placement::IntoTabs => return None,
    };
    Some(children[neighbour])
}

/// `set-tab-order` after moving `tabs[from]` to the drop index `to`.
///
/// `to` is the index [`reorder`] takes. A drop that leaves the order
/// unchanged returns `None`.
pub fn tab_order_verb(
    pane: u64,
    tabs: &[impl AsRef<str> + Clone],
    from: usize,
    to: usize,
) -> Option<Verb> {
    let next = reorder(tabs, from, to)?;
    Some(Verb::SetTabOrder {
        target: PaneRef::id(TileId(pane)),
        tabs: next
            .into_iter()
            .map(|tab| ViewKindId::from(tab.as_ref().to_string()))
            .collect(),
    })
}

/// The child before the divider `edge` of child `index` names.
///
/// Right and bottom are the divider after the child. Left and top are the
/// divider before it. The outer edge of the container is not a divider.
pub fn divider_index(child_count: usize, index: usize, edge: Edge) -> Option<usize> {
    if child_count < 2 || index >= child_count {
        return None;
    }
    match edge {
        Edge::Right | Edge::Bottom if index + 1 < child_count => Some(index),
        Edge::Left | Edge::Top if index > 0 => Some(index - 1),
        _ => None,
    }
}

/// The share of the child before a divider, after the pointer moved.
///
/// `before` and `after` are the on-screen lengths of that child and the
/// neighbour that gave up the space. The share is `neighbour * before / after`,
/// which is one child's weight relative to that neighbour. `None` when a
/// length is not a positive finite number.
pub fn share_beside(neighbour: f32, before: f32, after: f32) -> Option<f32> {
    if !positive(neighbour) || !positive(before) || !positive(after) {
        return None;
    }
    let share = neighbour * before / after;
    positive(share).then_some(share)
}

fn positive(value: f32) -> bool {
    value.is_finite() && value > 0.0
}

/// `resize` of `container` after the divider following child `index` moved.
///
/// `shares` is the container's current weights. `before` and `after` are the
/// lengths [`share_beside`] takes. The other shares stay as they are.
pub fn resize_verb(
    container: u64,
    shares: &[f32],
    index: usize,
    before: f32,
    after: f32,
) -> Option<Verb> {
    let neighbour = *shares.get(index + 1)?;
    let share = share_beside(neighbour, before, after)?;
    let mut next = shares.to_vec();
    let slot = next.get_mut(index)?;
    *slot = share;
    Some(Verb::Resize {
        container: TileId(container),
        shares: next,
    })
}

/// The `move-tile` a tree widget's `move` asks for, over a window's tiles
/// drawn as an outline (docs/plan-koine-affordances.md A-D3).
///
/// The outline's rows are a window's tiles in container order, without the
/// window's root container: its children are the roots. A `move` names the
/// dragged rows (`ids`), the row they go under (`to`, `None` for the roots,
/// which is the window's root container) and their position among that row's
/// children as they are *before* the move (`impress_surface::tree::drop_gap`;
/// `index == len` is after the last). The verb is `MoveTile` beside the child
/// now at `index`:
///
/// - into a `Linear`: before that child (`Left` / `Above` by the container's
///   axis), or after the last one (`Right` / `Below`) when `index == len`;
/// - into a `Tabs` strip: `IntoTabs` after the child at `index - 1`, which
///   is where `MoveTile` puts a tile it tabs beside a tab. A strip has no
///   "before the first tab", so `index == 0` joins the strip at its end
///   (`IntoTabs` on the strip), which is also what a drop *on* the strip's
///   row means; for a tile already in that strip it is `None`;
/// - onto a pane (`to` is a pane, `index == 0`: a drop on the pane's row):
///   `IntoTabs` with that pane, the centre drop of a pane frame.
///
/// `None` when the move is not one tile, names a tile the layout lacks, would
/// put a tile under itself, leaves the order unchanged, lands in a `Grid`
/// (which has no axis to say before or after by), or `to` is `None` and the
/// window's root is a pane.
pub fn tree_move_verb(layout: &Layout, ids: &[u64], to: Option<u64>, index: usize) -> Option<Verb> {
    let [dragged] = ids else {
        return None;
    };
    let dragged = TileId(*dragged);
    layout.tile(dragged)?;
    let to = match to {
        Some(to) => TileId(to),
        None => {
            let window = layout.window_of(dragged)?;
            let root = layout.window(window)?.root;
            layout.tile(root)?.as_container()?;
            root
        }
    };
    if layout.is_ancestor(dragged, to) {
        return None;
    }
    let move_tile = |target: TileId, placement: Placement| {
        (target != dragged && !layout.is_ancestor(dragged, target)).then(|| Verb::MoveTile {
            tile: PaneRef::id(dragged),
            target: PaneRef::id(target),
            placement,
        })
    };
    let container = match layout.tile(to)? {
        Tile::Pane(_) if index == 0 => return move_tile(to, Placement::IntoTabs),
        Tile::Pane(_) => return None,
        Tile::Container(container) => container,
    };
    let children = container.children();
    if index > children.len() {
        return None;
    }
    // Already a child here, and the drop is on either side of itself.
    if let Some(at) = container.index_of(dragged) {
        if index == at || index == at + 1 {
            return None;
        }
    }
    match container {
        Container::Linear { dir, .. } => {
            let (before, after) = match dir {
                LinearDir::Horizontal => (Placement::Left, Placement::Right),
                LinearDir::Vertical => (Placement::Above, Placement::Below),
            };
            match children.get(index) {
                Some(child) => move_tile(*child, before),
                None => move_tile(*children.last()?, after),
            }
        }
        Container::Tabs { .. } => {
            let target = match index.checked_sub(1) {
                Some(previous) => children[previous],
                None if container.index_of(dragged).is_some() => return None,
                None => to,
            };
            // `IntoTabs` on a strip nested in this one would join the nested
            // strip instead.
            if target != to
                && matches!(
                    layout.tile(target),
                    Some(Tile::Container(Container::Tabs { .. }))
                )
            {
                return None;
            }
            move_tile(target, Placement::IntoTabs)
        }
        Container::Grid { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gesture_round_trips_as_the_same_json_every_frontend_sends() {
        let gesture = Gesture::Drag {
            element: Element::Tile { id: 3 },
            target: Element::Tile { id: 1 },
            placement: Placement::Left,
        };
        let json = serde_json::to_value(&gesture).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "gesture": "drag",
                "element": {"kind": "tile", "id": 3},
                "target": {"kind": "tile", "id": 1},
                "placement": "left",
            })
        );
        let back: Gesture = serde_json::from_value(json).unwrap();
        assert_eq!(back, gesture);

        let menu = Gesture::Secondary {
            element: Element::Focused,
        };
        assert_eq!(
            serde_json::to_value(&menu).unwrap(),
            serde_json::json!({"gesture": "secondary", "element": {"kind": "focused"}})
        );
    }

    #[test]
    fn reorder_moves_an_item_and_a_drop_on_itself_changes_nothing() {
        let tabs = ["info", "pdf", "notes", "bibtex"];
        assert_eq!(
            reorder(&tabs, 0, 3).unwrap(),
            vec!["pdf", "notes", "info", "bibtex"]
        );
        assert_eq!(
            reorder(&tabs, 3, 0).unwrap(),
            vec!["bibtex", "info", "pdf", "notes"]
        );
        assert!(reorder(&tabs, 1, 1).is_none());
        assert!(reorder(&tabs, 0, 1).is_none());
        assert!(reorder(&tabs, 4, 0).is_none());
    }

    #[test]
    fn a_self_drop_becomes_a_move_once_the_sibling_is_named() {
        let children = [1_u64, 2, 3];
        let own = Gesture::Drag {
            element: Element::Tile { id: 2 },
            target: Element::Tile { id: 2 },
            placement: Placement::Right,
        };
        assert!(layout_verb(&own, None).is_none());
        let sibling = tab_split_target(&children, 1, Placement::Right).unwrap();
        let retargeted = Gesture::Drag {
            element: Element::Tile { id: 2 },
            target: Element::Tile { id: sibling },
            placement: Placement::Right,
        };
        assert_eq!(
            layout_verb(&retargeted, None),
            Some(Verb::MoveTile {
                tile: PaneRef::id(TileId(2)),
                target: PaneRef::id(TileId(3)),
                placement: Placement::Right,
            })
        );
    }

    #[test]
    fn the_gesture_contract_matches_the_committed_fixture() {
        let generated = serde_json::to_string_pretty(&contract()).unwrap() + "\n";
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/gesture-verbs.json");
        if std::env::var_os("UPDATE_GESTURE_VERBS").is_some() {
            std::fs::write(&path, &generated).unwrap();
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            committed, generated,
            "gesture verbs drifted; regenerate with UPDATE_GESTURE_VERBS=1"
        );
    }

    fn contract() -> serde_json::Value {
        let case = |name, gesture: Gesture, pane, verb: Option<Verb>| {
            serde_json::json!({
                "name": name,
                "gesture": gesture,
                "pane": pane,
                "verb": verb,
            })
        };
        let tabs = ["info", "pdf", "notes", "bibtex"];
        serde_json::json!({
            "wire_version": 1,
            "cases": [
                case(
                    "focus a tile",
                    Gesture::Focus { element: Element::Tile { id: 12 } },
                    None::<u64>,
                    layout_verb(&Gesture::Focus { element: Element::Tile { id: 12 } }, None),
                ),
                case(
                    "focus the cursor",
                    Gesture::Focus { element: Element::Focused },
                    None::<u64>,
                    layout_verb(&Gesture::Focus { element: Element::Focused }, None),
                ),
                case(
                    "a widget focus is not a layout verb",
                    Gesture::Focus { element: Element::Widget { id: "run".into() } },
                    None::<u64>,
                    layout_verb(&Gesture::Focus { element: Element::Widget { id: "run".into() } }, None),
                ),
                case(
                    "a direction focuses that way",
                    Gesture::Move { direction: Direction::Down },
                    None::<u64>,
                    layout_verb(&Gesture::Move { direction: Direction::Down }, None),
                ),
                case(
                    "a drag onto another pane moves",
                    Gesture::Drag {
                        element: Element::Tile { id: 3 },
                        target: Element::Tile { id: 1 },
                        placement: Placement::Left,
                    },
                    None::<u64>,
                    layout_verb(
                        &Gesture::Drag {
                            element: Element::Tile { id: 3 },
                            target: Element::Tile { id: 1 },
                            placement: Placement::Left,
                        },
                        None,
                    ),
                ),
                case(
                    "a drag onto itself is not yet a verb",
                    Gesture::Drag {
                        element: Element::Tile { id: 3 },
                        target: Element::Tile { id: 3 },
                        placement: Placement::Left,
                    },
                    None::<u64>,
                    layout_verb(
                        &Gesture::Drag {
                            element: Element::Tile { id: 3 },
                            target: Element::Tile { id: 3 },
                            placement: Placement::Left,
                        },
                        None,
                    ),
                ),
                case(
                    "a tab click shows that view kind",
                    Gesture::Activate { element: Element::Widget { id: "pdf".into() } },
                    Some(3_u64),
                    layout_verb(
                        &Gesture::Activate { element: Element::Widget { id: "pdf".into() } },
                        Some(3),
                    ),
                ),
                case(
                    "a button click stays a surface event",
                    Gesture::Activate { element: Element::Widget { id: "run".into() } },
                    Some(3_u64),
                    layout_verb(
                        &Gesture::Activate { element: Element::Widget { id: "run".into() } },
                        Some(3),
                    ),
                ),
                case(
                    "a menu click is not a verb",
                    Gesture::Secondary { element: Element::Focused },
                    None::<u64>,
                    layout_verb(&Gesture::Secondary { element: Element::Focused }, None),
                ),
                case(
                    "a resize names an edge and waits for lengths",
                    Gesture::Resize { element: Element::Tile { id: 4 }, edge: Edge::Bottom },
                    None::<u64>,
                    layout_verb(
                        &Gesture::Resize { element: Element::Tile { id: 4 }, edge: Edge::Bottom },
                        None,
                    ),
                ),
            ],
            "tab_split": [
                {"children": [2, 3, 4], "index": 0, "placement": "right", "target": tab_split_target(&[2, 3, 4], 0, Placement::Right)},
                {"children": [2, 3, 4], "index": 2, "placement": "right", "target": tab_split_target(&[2, 3, 4], 2, Placement::Right)},
                {"children": [2, 3], "index": 1, "placement": "into-tabs", "target": tab_split_target(&[2, 3], 1, Placement::IntoTabs)},
            ],
            "split_axes": split_axis_rows(),
            "tab_order": [
                {
                    "pane": 3,
                    "tabs": tabs,
                    "from": 0,
                    "to": 3,
                    "verb": tab_order_verb(3, &tabs, 0, 3),
                },
                {
                    "pane": 3,
                    "tabs": ["info", "pdf"],
                    "from": 0,
                    "to": 1,
                    "verb": tab_order_verb(3, &["info", "pdf"], 0, 1),
                },
            ],
            "resize": [{
                "container": 4,
                "shares": [1.0, 1.0],
                "index": 0,
                "before": 2.0,
                "after": 1.0,
                "verb": resize_verb(4, &[1.0, 1.0], 0, 2.0, 1.0),
            }],
            "dividers": [
                {"child_count": 2, "index": 0, "edge": "right", "divider": divider_index(2, 0, Edge::Right)},
                {"child_count": 2, "index": 0, "edge": "left", "divider": divider_index(2, 0, Edge::Left)},
                {"child_count": 2, "index": 1, "edge": "top", "divider": divider_index(2, 1, Edge::Top)},
            ],
        })
    }

    fn split_axis_rows() -> Vec<serde_json::Value> {
        ["left", "right", "above", "below", "into-tabs"]
            .into_iter()
            .map(|name| {
                let placement = match name {
                    "left" => Placement::Left,
                    "right" => Placement::Right,
                    "above" => Placement::Above,
                    "below" => Placement::Below,
                    _ => Placement::IntoTabs,
                };
                let (dir, after) = split_axes(placement);
                serde_json::json!({"placement": name, "dir": dir, "after": after})
            })
            .collect()
    }

    // ---- tree_move_verb (docs/plan-koine-affordances.md A-D3) ----

    fn pane(kind: &str) -> serde_json::Value {
        serde_json::json!({"pane": {"view_kind": kind, "query": {"kinds": ["surface"]}}})
    }

    fn layout_of(tiles: serde_json::Value, root: u64) -> Layout {
        serde_json::from_value(serde_json::json!({
            "windows": [{"id": 1, "root": root, "focused": 1}],
            "tiles": tiles,
        }))
        .unwrap()
    }

    /// Tile 4 is a row of panes 1, 2, 3.
    fn row_of_three(dir: &str) -> Layout {
        layout_of(
            serde_json::json!({
                "1": pane("surface"), "2": pane("surface"), "3": pane("surface"),
                "4": {"container": {"linear": {"dir": dir, "children": [1, 2, 3], "shares": [1.0, 1.0, 1.0]}}},
            }),
            4,
        )
    }

    fn moved(tile: u64, target: u64, placement: Placement) -> Option<Verb> {
        Some(Verb::MoveTile {
            tile: PaneRef::id(TileId(tile)),
            target: PaneRef::id(TileId(target)),
            placement,
        })
    }

    /// The children of `container` after applying `verb`.
    fn children_after(mut layout: Layout, verb: Verb, container: u64) -> Vec<u64> {
        layout.apply(verb).unwrap();
        layout
            .tile(TileId(container))
            .and_then(Tile::as_container)
            .unwrap()
            .children()
            .iter()
            .map(|child| child.raw())
            .collect()
    }

    #[test]
    fn a_tree_move_in_a_row_is_beside_the_tile_at_the_index_by_the_rows_axis() {
        let layout = row_of_three("horizontal");
        // The roots of the outline are the root container's children.
        assert_eq!(
            tree_move_verb(&layout, &[3], None, 0),
            moved(3, 1, Placement::Left)
        );
        assert_eq!(
            tree_move_verb(&layout, &[1], Some(4), 2),
            moved(1, 3, Placement::Left)
        );
        assert_eq!(
            tree_move_verb(&layout, &[1], None, 3),
            moved(1, 3, Placement::Right),
            "index == len is after the last"
        );
        // The verb does what the outline drew.
        let verb = tree_move_verb(&layout, &[3], None, 0).unwrap();
        assert_eq!(children_after(layout.clone(), verb, 4), vec![3, 1, 2]);
        let verb = tree_move_verb(&layout, &[1], None, 2).unwrap();
        assert_eq!(children_after(layout.clone(), verb, 4), vec![2, 1, 3]);
        let verb = tree_move_verb(&layout, &[1], None, 3).unwrap();
        assert_eq!(children_after(layout, verb, 4), vec![2, 3, 1]);

        let column = row_of_three("vertical");
        assert_eq!(
            tree_move_verb(&column, &[3], None, 1),
            moved(3, 2, Placement::Above)
        );
        assert_eq!(
            tree_move_verb(&column, &[1], None, 3),
            moved(1, 3, Placement::Below)
        );
    }

    #[test]
    fn a_tree_move_that_changes_nothing_or_cannot_be_said_is_none() {
        let layout = row_of_three("horizontal");
        // Either side of itself.
        assert_eq!(tree_move_verb(&layout, &[2], None, 1), None);
        assert_eq!(tree_move_verb(&layout, &[2], None, 2), None);
        // Past the end, two tiles, a tile the layout lacks.
        assert_eq!(tree_move_verb(&layout, &[2], None, 4), None);
        assert_eq!(tree_move_verb(&layout, &[1, 2], None, 0), None);
        assert_eq!(tree_move_verb(&layout, &[9], None, 0), None);
        // A pane's row has no gaps under it.
        assert_eq!(tree_move_verb(&layout, &[1], Some(2), 1), None);
        // A window whose root is a pane has no root container to move into.
        let single = layout_of(serde_json::json!({"1": pane("surface")}), 1);
        assert_eq!(tree_move_verb(&single, &[1], None, 0), None);
        // A grid has no axis to say before or after by.
        let grid = layout_of(
            serde_json::json!({
                "1": pane("surface"), "2": pane("surface"),
                "3": {"container": {"grid": {"children": [1, 2]}}},
            }),
            3,
        );
        assert_eq!(tree_move_verb(&grid, &[2], None, 0), None);
    }

    /// Tile 5 is a row of pane 1 and a column (4) of panes 2 and 3.
    fn nested() -> Layout {
        layout_of(
            serde_json::json!({
                "1": pane("surface"), "2": pane("surface"), "3": pane("surface"),
                "4": {"container": {"linear": {"dir": "vertical", "children": [2, 3], "shares": [1.0, 1.0]}}},
                "5": {"container": {"linear": {"dir": "horizontal", "children": [1, 4], "shares": [1.0, 1.0]}}},
            }),
            5,
        )
    }

    #[test]
    fn a_tree_move_into_a_nested_container_uses_that_containers_axis() {
        let layout = nested();
        assert_eq!(
            tree_move_verb(&layout, &[1], Some(4), 1),
            moved(1, 3, Placement::Above)
        );
        assert_eq!(
            tree_move_verb(&layout, &[1], Some(4), 2),
            moved(1, 3, Placement::Below)
        );
        let verb = tree_move_verb(&layout, &[1], Some(4), 1).unwrap();
        let mut after = layout.clone();
        after.apply(verb).unwrap();
        let column: Vec<u64> = after
            .tile(TileId(4))
            .and_then(Tile::as_container)
            .unwrap()
            .children()
            .iter()
            .map(|child| child.raw())
            .collect();
        assert_eq!(column, vec![2, 1, 3]);
        // A drop on a container's row is before its first child.
        assert_eq!(
            tree_move_verb(&layout, &[1], Some(4), 0),
            moved(1, 2, Placement::Above)
        );
        // Out of the column, to the roots.
        assert_eq!(
            tree_move_verb(&layout, &[3], None, 0),
            moved(3, 1, Placement::Left)
        );
        // Never under itself.
        assert_eq!(tree_move_verb(&layout, &[4], Some(4), 0), None);
        assert_eq!(tree_move_verb(&layout, &[4], Some(2), 0), None);
    }

    #[test]
    fn a_tree_move_into_tabs_or_onto_a_pane_is_into_tabs() {
        // Tile 5 is a row of pane 1 and a strip (4) of panes 2 and 3.
        let layout = layout_of(
            serde_json::json!({
                "1": pane("surface"), "2": pane("surface"), "3": pane("surface"),
                "4": {"container": {"tabs": {"children": [2, 3], "active": 2}}},
                "5": {"container": {"linear": {"dir": "horizontal", "children": [1, 4], "shares": [1.0, 1.0]}}},
            }),
            5,
        );
        // After the tab before the index; index 0 (or a drop on the strip's
        // row) joins the strip at its end.
        assert_eq!(
            tree_move_verb(&layout, &[1], Some(4), 1),
            moved(1, 2, Placement::IntoTabs)
        );
        assert_eq!(
            tree_move_verb(&layout, &[1], Some(4), 2),
            moved(1, 3, Placement::IntoTabs)
        );
        assert_eq!(
            tree_move_verb(&layout, &[1], Some(4), 0),
            moved(1, 4, Placement::IntoTabs)
        );
        let verb = tree_move_verb(&layout, &[1], Some(4), 1).unwrap();
        let mut after = layout.clone();
        after.apply(verb).unwrap();
        let strip: Vec<u64> = after
            .tile(TileId(4))
            .and_then(Tile::as_container)
            .unwrap()
            .children()
            .iter()
            .map(|child| child.raw())
            .collect();
        assert_eq!(strip, vec![2, 1, 3]);
        // Reordering inside the strip.
        assert_eq!(
            tree_move_verb(&layout, &[2], Some(4), 2),
            moved(2, 3, Placement::IntoTabs)
        );
        assert_eq!(
            tree_move_verb(&layout, &[3], Some(4), 0),
            None,
            "a strip has no place before its first tab"
        );
        // A drop on a pane's row tabs the two together.
        assert_eq!(
            tree_move_verb(&layout, &[2], Some(1), 0),
            moved(2, 1, Placement::IntoTabs)
        );
        assert_eq!(tree_move_verb(&layout, &[1], Some(1), 0), None);
    }
}
