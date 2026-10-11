//! A pane frame's pointer vocabulary, for the browser.
//!
//! This is `koine_tui::frame`'s [`Hit`], `pointer_gesture`, `drag_gesture` and
//! the press half of `verb_for_pointer`, restated here because the crate that
//! owns them cannot be linked into a wasm build: `koine-tui` depends on `koine`
//! with its default `verbs` feature, which links tokio and `inventory`, and
//! `mio` does not build for `wasm32-unknown-unknown` (plan D5). Everything
//! after the gesture is `koine::gesture`, shared: [`koine::gesture::layout_verb`]
//! names the verb, [`koine::gesture::Edge::placement`] the placement.
//!
//! The browser does not hit-test cells. Each frame element carries what it is
//! (`data-koine-edge`, `data-koine-grip`, `data-koine-tab`, `data-koine-divider`),
//! so the element a pointer event lands on already names its [`Hit`].
//!
//! There is no `Hit::Tab` here. A tab of the tui frame is a view kind
//! (`Element::Widget`, `set-view-kind`); a tab of a web strip is a child tile
//! of a `Tabs` container, and clicking it is a focus on that tile, which is
//! the `Title` press on the child.
//!
//! `tests::the_web_frame_decides_what_the_tui_frame_decides` pins this module
//! to `koine_tui::frame` over every hit pair, natively. Moving the decisions
//! into `koine::gesture` and deleting this file is the right end state; it
//! needs a change outside this crate.

use koine::gesture::{Edge, Element, Gesture, Pointer};
use koine::layout::{Placement, Verb};

/// Where a pointer event sits in a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Grip,
    Title,
    Frame(Edge),
    Body,
}

/// A press on the frame. The body focuses the pane; a secondary click is the
/// menu; an edge is a resize.
pub fn pointer_gesture(at: Hit, pointer: Pointer, pane: &Element) -> Gesture {
    if pointer == Pointer::Secondary {
        return Gesture::Secondary {
            element: pane.clone(),
        };
    }
    match at {
        Hit::Frame(edge) => Gesture::Resize {
            element: pane.clone(),
            edge,
        },
        Hit::Grip | Hit::Title | Hit::Body => Gesture::Focus {
            element: pane.clone(),
        },
    }
}

/// A drag from `from` to `to`.
///
/// A grip dropped on another pane's edge moves it there. Its center becomes
/// tabs. Its own center cancels. A drag that starts in the body is the
/// content's pointer.
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
        Hit::Grip | Hit::Title => {
            let placement = match to {
                Hit::Frame(edge) => Some(edge.placement()),
                Hit::Body | Hit::Title | Hit::Grip => {
                    if same_pane {
                        None
                    } else {
                        Some(Placement::IntoTabs)
                    }
                }
            };
            // A pane dropped on its own center cancels. An edge of the same
            // pane still places it, which is how a tabbed pane splits back out.
            if same_pane && placement == Some(Placement::IntoTabs) {
                return None;
            }
            placement.map(|placement| Gesture::Drag {
                element: dragged.clone(),
                target: target.clone(),
                placement,
            })
        }
    }
}

/// The layout verb a press already determines, or `None` when the press is a
/// menu or a resize that still needs lengths.
pub fn verb_for_pointer(at: Hit, pointer: Pointer, pane: &Element) -> Option<Verb> {
    let pane_id = match pane {
        Element::Tile { id } => Some(*id),
        Element::Widget { .. } | Element::Focused => None,
    };
    koine::gesture::layout_verb(&pointer_gesture(at, pointer, pane), pane_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use koine_tui::frame as tui;

    fn to_tui(hit: Hit) -> tui::Hit {
        match hit {
            Hit::Grip => tui::Hit::Grip,
            Hit::Title => tui::Hit::Title,
            Hit::Frame(edge) => tui::Hit::Frame(edge),
            Hit::Body => tui::Hit::Body,
        }
    }

    fn every_hit() -> Vec<Hit> {
        let mut hits = vec![Hit::Grip, Hit::Title, Hit::Body];
        for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
            hits.push(Hit::Frame(edge));
        }
        hits
    }

    #[test]
    fn the_web_frame_decides_what_the_tui_frame_decides() {
        let pane = Element::Tile { id: 12 };
        let other = Element::Tile { id: 3 };
        for at in every_hit() {
            for pointer in [Pointer::Primary, Pointer::Secondary] {
                assert_eq!(
                    Some(pointer_gesture(at, pointer, &pane)),
                    tui::pointer_gesture(to_tui(at), pointer, &pane, &[]),
                    "press {at:?} {pointer:?}"
                );
                assert_eq!(
                    verb_for_pointer(at, pointer, &pane),
                    tui::verb_for_pointer(to_tui(at), pointer, &pane, &[]),
                    "verb for press {at:?} {pointer:?}"
                );
            }
            for to in every_hit() {
                for same_pane in [false, true] {
                    let target = if same_pane { &pane } else { &other };
                    assert_eq!(
                        drag_gesture(at, to, &pane, target, same_pane),
                        tui::drag_gesture(to_tui(at), to_tui(to), &pane, target, same_pane),
                        "drag {at:?} to {to:?}, same pane {same_pane}"
                    );
                }
            }
        }
    }
}
