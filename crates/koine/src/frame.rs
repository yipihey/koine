//! What a pointer on a pane frame means, with no cells in it.
//!
//! The box is the border. The grip is the mark on the top edge. Tab labels
//! share that edge. Everything inside the box is the body, and a drag that
//! starts there belongs to the program in the pane, not to the chassis.
//!
//! [`Hit`] is where a pointer landed; each frontend finds it its own way (the
//! terminal hit-tests cells in `koine-tui`, the browser reads the element's
//! `data-koine-*` attribute). [`pointer_gesture`] and [`drag_gesture`] turn a
//! hit into the same [`Gesture`] a key produces, and [`verb_for_pointer`] and
//! [`verb_for_drag`] are that gesture through [`gesture::layout_verb`]. One
//! copy of the decision, so the two frontends cannot drift.

use crate::gesture::{self, Edge, Element, Gesture, Pointer};
use crate::layout::{Placement, Verb};

/// Where a pointer sits in a frame: a cell in the terminal, an element in the
/// browser. Each frontend decides which `Hit` its pointer landed on; what the
/// hit means is decided here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Grip,
    Title,
    Tab(usize),
    Frame(Edge),
    Body,
}

/// A press on the frame. The body focuses the pane; a tab activates that
/// tab; a secondary click is the menu. The wheel and the middle button
/// return `None` — they belong to the content.
pub fn pointer_gesture(
    at: Hit,
    pointer: Pointer,
    pane: &Element,
    tabs: &[Element],
) -> Option<Gesture> {
    if pointer == Pointer::Secondary {
        let element = match at {
            Hit::Tab(index) => tabs.get(index).cloned().unwrap_or_else(|| pane.clone()),
            _ => pane.clone(),
        };
        return Some(Gesture::Secondary { element });
    }
    Some(match at {
        Hit::Tab(index) => Gesture::Activate {
            element: tabs.get(index).cloned().unwrap_or_else(|| pane.clone()),
        },
        Hit::Frame(edge) => Gesture::Resize {
            element: pane.clone(),
            edge,
        },
        Hit::Grip | Hit::Title | Hit::Body => Gesture::Focus {
            element: pane.clone(),
        },
    })
}

/// A drag from `from` to `to`.
///
/// A grip dropped on another pane's edge moves it there. Its center becomes
/// tabs. Its own center cancels. A tab dropped on an edge splits that tab
/// out; a tab dropped along its own title is a reorder, which this returns
/// as `None` so the host can call [`gesture::reorder`]. A drag that
/// starts in the body is the content's pointer.
pub fn drag_gesture(
    from: Hit,
    to: Hit,
    dragged: &Element,
    target: &Element,
    same_pane: bool,
) -> Option<Gesture> {
    match from {
        Hit::Body => None,
        Hit::Frame(edge) => Some(Gesture::Resize {
            element: dragged.clone(),
            edge,
        }),
        Hit::Grip | Hit::Title => drag_handle(to, dragged, target, same_pane, true),
        Hit::Tab(_) => drag_handle(to, dragged, target, same_pane, false),
    }
}

/// `pane_handle` is the grip or the title. A tab along its own strip is left
/// for [`gesture::reorder`].
fn drag_handle(
    to: Hit,
    dragged: &Element,
    target: &Element,
    same_pane: bool,
    pane_handle: bool,
) -> Option<Gesture> {
    let placement = match to {
        Hit::Frame(edge) => Some(edge.placement()),
        Hit::Body | Hit::Title | Hit::Tab(_) | Hit::Grip => {
            if same_pane {
                None
            } else {
                Some(Placement::IntoTabs)
            }
        }
    };
    // A tab dragged along its own title changes order; it does not split.
    if !pane_handle && same_pane && !matches!(to, Hit::Frame(_)) {
        return None;
    }
    // A pane dropped on its own center cancels. An edge of the same pane
    // still places it, which is how a tabbed pane splits back out.
    if pane_handle && same_pane && placement == Some(Placement::IntoTabs) {
        return None;
    }
    placement.map(|placement| Gesture::Drag {
        element: dragged.clone(),
        target: target.clone(),
        placement,
    })
}

/// The layout verb a press already determines, or `None` when the press is a
/// menu, a surface click, or a resize that still needs lengths.
pub fn verb_for_pointer(
    at: Hit,
    pointer: Pointer,
    pane: &Element,
    tabs: &[Element],
) -> Option<Verb> {
    let pane_id = tile_id(pane);
    pointer_gesture(at, pointer, pane, tabs)
        .as_ref()
        .and_then(|gesture| gesture::layout_verb(gesture, pane_id))
}

/// The layout verb a drag already determines. A tab pulled onto an edge is
/// `None`: the host attaches the new spec to [`koine::gesture::split_axes`].
/// A reorder along the strip is `None`: the host calls
/// [`gesture::tab_order_verb`].
pub fn verb_for_drag(
    from: Hit,
    to: Hit,
    dragged: &Element,
    target: &Element,
    same_pane: bool,
) -> Option<Verb> {
    let pane_id = tile_id(target);
    drag_gesture(from, to, dragged, target, same_pane)
        .as_ref()
        .and_then(|gesture| gesture::layout_verb(gesture, pane_id))
}

fn tile_id(element: &Element) -> Option<u64> {
    match element {
        Element::Tile { id } => Some(*id),
        Element::Widget { .. } | Element::Focused => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{LinearDir, PaneRef, TileId};

    #[test]
    fn the_pointer_and_a_drag_become_the_shared_gesture() {
        let pane = Element::Tile { id: 12 };
        let tabs = [
            Element::Widget { id: "info".into() },
            Element::Widget { id: "pdf".into() },
        ];
        assert_eq!(
            pointer_gesture(Hit::Grip, Pointer::Secondary, &pane, &tabs),
            Some(Gesture::Secondary {
                element: pane.clone()
            })
        );
        assert_eq!(
            pointer_gesture(Hit::Tab(1), Pointer::Primary, &pane, &tabs),
            Some(Gesture::Activate {
                element: Element::Widget { id: "pdf".into() }
            })
        );
        assert_eq!(
            pointer_gesture(Hit::Body, Pointer::Primary, &pane, &tabs),
            Some(Gesture::Focus {
                element: pane.clone()
            })
        );

        let other = Element::Tile { id: 3 };
        assert_eq!(
            drag_gesture(Hit::Grip, Hit::Frame(Edge::Left), &pane, &other, false),
            Some(Gesture::Drag {
                element: pane.clone(),
                target: other.clone(),
                placement: Placement::Left,
            })
        );
        assert_eq!(
            drag_gesture(Hit::Grip, Hit::Body, &pane, &other, false),
            Some(Gesture::Drag {
                element: pane.clone(),
                target: other.clone(),
                placement: Placement::IntoTabs,
            })
        );
        assert_eq!(
            drag_gesture(Hit::Grip, Hit::Body, &pane, &pane, true),
            None,
            "a pane's own center cancels"
        );
        assert_eq!(
            drag_gesture(Hit::Tab(0), Hit::Title, &tabs[0], &pane, true),
            None,
            "along the strip is a reorder"
        );
        assert_eq!(
            drag_gesture(Hit::Tab(0), Hit::Frame(Edge::Right), &tabs[0], &pane, true),
            Some(Gesture::Drag {
                element: tabs[0].clone(),
                target: pane.clone(),
                placement: Placement::Right,
            })
        );
        assert_eq!(
            drag_gesture(Hit::Body, Hit::Frame(Edge::Left), &pane, &other, false),
            None,
            "the body is the content's pointer"
        );
        assert_eq!(
            drag_gesture(Hit::Frame(Edge::Bottom), Hit::Body, &pane, &pane, true),
            Some(Gesture::Resize {
                element: pane.clone(),
                edge: Edge::Bottom,
            })
        );

        assert_eq!(
            verb_for_pointer(Hit::Body, Pointer::Primary, &pane, &tabs),
            Some(Verb::Focus {
                target: PaneRef::id(TileId(12)),
            })
        );
        assert_eq!(
            verb_for_pointer(Hit::Tab(1), Pointer::Primary, &pane, &tabs)
                .map(|verb| serde_json::to_value(verb).unwrap()),
            Some(serde_json::json!({
                "verb": "set-view-kind",
                "target": {"id": 12},
                "view_kind": "pdf",
            }))
        );
        assert!(verb_for_pointer(Hit::Grip, Pointer::Secondary, &pane, &tabs).is_none());
        assert_eq!(
            verb_for_drag(Hit::Grip, Hit::Frame(Edge::Left), &pane, &other, false)
                .map(|verb| serde_json::to_value(verb).unwrap()),
            Some(serde_json::json!({
                "verb": "move-tile",
                "tile": {"id": 12},
                "target": {"id": 3},
                "placement": "left",
            }))
        );
        assert!(
            verb_for_drag(Hit::Tab(0), Hit::Frame(Edge::Right), &tabs[0], &pane, true).is_none()
        );
        let (dir, after) = gesture::split_axes(Placement::Right);
        assert_eq!((dir, after), (LinearDir::Horizontal, true));
    }
}
