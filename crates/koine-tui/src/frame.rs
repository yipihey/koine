//! A pane frame, as cells.
//!
//! The box is the border. The `≡` on the top edge is the grip. Tab labels
//! share that edge. Everything inside the box is the body, and a drag that
//! starts there belongs to the program in the pane, not to the chassis.
//!
//! A pointer report is an SGR mouse sequence (xterm 1006): a cell and a
//! button. [`pointer_gesture`] and [`drag_gesture`] turn a hit into the same
//! [`Gesture`] a key produces. [`verb_for_pointer`] and [`verb_for_drag`]
//! are that gesture through [`koine::gesture::layout_verb`].

use koine::gesture::{Edge, Element, Gesture, Pointer};
use koine::layout::Placement;

/// One pane's frame, in terminal cells. `width` and `height` include the
/// border. The origin is the top-left corner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    /// Tab labels drawn on the top edge, after the grip. Empty means the
    /// rest of that edge is the title.
    pub tabs: Vec<String>,
}

/// Where a cell sits in a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Grip,
    Title,
    Tab(usize),
    Frame(Edge),
    Body,
}

/// One SGR 1006 report. Columns and rows are 0-based cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PointerReport {
    pub button: MouseButton,
    pub phase: Phase,
    pub col: u16,
    pub row: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Primary,
    Middle,
    Secondary,
    WheelUp,
    WheelDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Press,
    Drag,
    Release,
}

impl PointerReport {
    /// The button the chassis acts on, while the button is down.
    /// A release, the wheel, and the middle button are the content's.
    pub fn pointer(self) -> Option<Pointer> {
        if self.phase == Phase::Release {
            return None;
        }
        match self.button {
            MouseButton::Primary => Some(Pointer::Primary),
            MouseButton::Secondary => Some(Pointer::Secondary),
            MouseButton::Middle | MouseButton::WheelUp | MouseButton::WheelDown => None,
        }
    }
}

/// Parse one SGR mouse report (`CSI < Pb ; Px ; Py M`, or `m` for release).
/// `Px` and `Py` are 1-based, as the terminal sends them.
pub fn parse_sgr(input: &str) -> Option<PointerReport> {
    let rest = input.strip_prefix('\u{1b}').unwrap_or(input);
    let rest = rest.strip_prefix('[')?;
    let rest = rest.strip_prefix('<')?;
    let (body, release) = if let Some(body) = rest.strip_suffix('M') {
        (body, false)
    } else {
        (rest.strip_suffix('m')?, true)
    };
    let mut parts = body.split(';');
    let button: u16 = parts.next()?.parse().ok()?;
    let col: u16 = parts.next()?.parse().ok()?;
    let row: u16 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let motion = button & 32 != 0;
    let code = button & !32;
    let (button, phase) = if release {
        (decode_button(code)?, Phase::Release)
    } else if code >= 64 {
        let wheel = match code {
            64 => MouseButton::WheelUp,
            65 => MouseButton::WheelDown,
            _ => return None,
        };
        (wheel, Phase::Press)
    } else if motion {
        (decode_button(code)?, Phase::Drag)
    } else {
        (decode_button(code)?, Phase::Press)
    };
    Some(PointerReport {
        button,
        phase,
        col: col.saturating_sub(1),
        row: row.saturating_sub(1),
    })
}

fn decode_button(code: u16) -> Option<MouseButton> {
    match code {
        0 => Some(MouseButton::Primary),
        1 => Some(MouseButton::Middle),
        2 => Some(MouseButton::Secondary),
        _ => None,
    }
}

/// The cell the grip occupies, when the frame is wide enough for `┌ ≡`.
pub fn grip_column(frame: &Frame) -> Option<u16> {
    if frame.width >= 5 {
        Some(frame.x.saturating_add(2))
    } else {
        None
    }
}

/// Inclusive-exclusive column spans of `frame.tabs` along the top edge,
/// starting after the grip and stopping before the right corner.
pub fn tab_spans(frame: &Frame) -> Vec<(u16, u16)> {
    let right = frame.x.saturating_add(frame.width).saturating_sub(1);
    let mut col = grip_column(frame)
        .map(|grip| grip.saturating_add(2))
        .unwrap_or(frame.x.saturating_add(1));
    let mut spans = Vec::new();
    for label in &frame.tabs {
        let width = label.chars().count() as u16;
        if width == 0 || col.saturating_add(width) >= right {
            break;
        }
        spans.push((col, col + width));
        col = col.saturating_add(width).saturating_add(2);
    }
    spans
}

/// Classify `col`, `row`. A cell outside the frame is `None`.
pub fn hit(frame: &Frame, col: u16, row: u16) -> Option<Hit> {
    if frame.width < 2 || frame.height < 2 {
        return None;
    }
    let right = frame.x + frame.width - 1;
    let bottom = frame.y + frame.height - 1;
    if col < frame.x || col > right || row < frame.y || row > bottom {
        return None;
    }
    if row == frame.y {
        if let Some(grip) = grip_column(frame) {
            if col == grip {
                return Some(Hit::Grip);
            }
        }
        for (index, (start, end)) in tab_spans(frame).into_iter().enumerate() {
            if col >= start && col < end {
                return Some(Hit::Tab(index));
            }
        }
        if col == frame.x || col == right {
            return Some(Hit::Frame(Edge::Top));
        }
        return Some(Hit::Title);
    }
    if row == bottom {
        return Some(Hit::Frame(Edge::Bottom));
    }
    if col == frame.x {
        return Some(Hit::Frame(Edge::Left));
    }
    if col == right {
        return Some(Hit::Frame(Edge::Right));
    }
    Some(Hit::Body)
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
/// as `None` so the host can call [`koine::gesture::reorder`]. A drag that
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
/// for [`koine::gesture::reorder`].
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
) -> Option<koine::layout::Verb> {
    let pane_id = tile_id(pane);
    pointer_gesture(at, pointer, pane, tabs)
        .as_ref()
        .and_then(|gesture| koine::gesture::layout_verb(gesture, pane_id))
}

/// The layout verb a drag already determines. A tab pulled onto an edge is
/// `None`: the host attaches the new spec to [`koine::gesture::split_axes`].
/// A reorder along the strip is `None`: the host calls
/// [`koine::gesture::tab_order_verb`].
pub fn verb_for_drag(
    from: Hit,
    to: Hit,
    dragged: &Element,
    target: &Element,
    same_pane: bool,
) -> Option<koine::layout::Verb> {
    let pane_id = tile_id(target);
    drag_gesture(from, to, dragged, target, same_pane)
        .as_ref()
        .and_then(|gesture| koine::gesture::layout_verb(gesture, pane_id))
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

    fn papers() -> Frame {
        Frame {
            x: 0,
            y: 0,
            width: 24,
            height: 6,
            tabs: vec![],
        }
    }

    fn detail() -> Frame {
        Frame {
            x: 24,
            y: 0,
            width: 28,
            height: 6,
            tabs: vec!["Info".into(), "PDF".into(), "Notes".into(), "BibTeX".into()],
        }
    }

    #[test]
    fn the_grip_is_the_mark_on_the_top_edge_and_the_box_is_the_frame() {
        let frame = papers();
        assert_eq!(hit(&frame, 2, 0), Some(Hit::Grip));
        assert_eq!(hit(&frame, 4, 0), Some(Hit::Title));
        assert_eq!(hit(&frame, 0, 0), Some(Hit::Frame(Edge::Top)));
        assert_eq!(hit(&frame, 0, 3), Some(Hit::Frame(Edge::Left)));
        assert_eq!(hit(&frame, 23, 3), Some(Hit::Frame(Edge::Right)));
        assert_eq!(hit(&frame, 4, 5), Some(Hit::Frame(Edge::Bottom)));
        assert_eq!(hit(&frame, 4, 2), Some(Hit::Body));
        assert_eq!(hit(&frame, 24, 2), None);
    }

    #[test]
    fn tab_labels_share_the_top_edge_after_the_grip() {
        let frame = detail();
        assert_eq!(hit(&frame, 26, 0), Some(Hit::Grip));
        assert_eq!(hit(&frame, 28, 0), Some(Hit::Tab(0)), "Info");
        assert_eq!(
            hit(&frame, 31, 0),
            Some(Hit::Tab(0)),
            "the last cell of Info"
        );
        assert_eq!(
            hit(&frame, 32, 0),
            Some(Hit::Title),
            "the gap between labels"
        );
        assert_eq!(hit(&frame, 34, 0), Some(Hit::Tab(1)), "PDF");
    }

    #[test]
    fn an_sgr_report_is_a_cell_and_a_button() {
        let press = parse_sgr("\u{1b}[<0;3;1M").unwrap();
        assert_eq!(press.button, MouseButton::Primary);
        assert_eq!(press.phase, Phase::Press);
        assert_eq!((press.col, press.row), (2, 0));
        assert_eq!(press.pointer(), Some(Pointer::Primary));

        let menu = parse_sgr("\u{1b}[<2;3;1M").unwrap();
        assert_eq!(menu.button, MouseButton::Secondary);
        assert_eq!(menu.pointer(), Some(Pointer::Secondary));

        let drag = parse_sgr("\u{1b}[<32;8;4M").unwrap();
        assert_eq!(drag.phase, Phase::Drag);
        assert_eq!((drag.col, drag.row), (7, 3));

        let release = parse_sgr("\u{1b}[<0;3;1m").unwrap();
        assert_eq!(release.phase, Phase::Release);
        assert_eq!(release.pointer(), None);

        assert!(parse_sgr("\u{1b}[<64;1;1M").unwrap().button == MouseButton::WheelUp);
        assert!(parse_sgr("not a mouse").is_none());
    }

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
            Some(koine::layout::Verb::Focus {
                target: koine::layout::PaneRef::id(koine::layout::TileId(12)),
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
        let (dir, after) = koine::gesture::split_axes(Placement::Right);
        assert_eq!((dir, after), (koine::layout::LinearDir::Horizontal, true));
    }
}
