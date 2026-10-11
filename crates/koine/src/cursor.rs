//! The focus cursor and the key vocabulary every frontend shares.
//!
//! A [`Cursor`] is an index into [`RenderTree::focus_order`]. [`Key`] is the
//! small set of keys a frontend understands. [`Cursor::event_for`] turns a key
//! into a surface [`Event`] for the focused widget, and [`Cursor::gesture_for`]
//! turns the same keys into a [`Gesture`]. The text frontend and the web
//! frontend both call these, so `j j Enter` means the same on each
//! (`docs/plan-koine-web.md` D4). Nothing here uses the `verbs` feature.

use crate::gesture::{layout_verb, Element, Gesture};
use crate::layout::{Direction, Verb};
use crate::surface::{Event, EventKind, RenderTree};
use serde_json::Value;

/// A key this frontend understands. Anything else is ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Next focusable widget (`j`).
    Down,
    /// Previous focusable widget (`k`).
    Up,
    /// The pane to the left (`h`). Does not walk the surface focus order.
    Left,
    /// The pane to the right (`l`). Does not walk the surface focus order.
    Right,
    /// Next leaf, in tree order.
    Next,
    /// Previous leaf, in tree order.
    Prev,
    /// Activate the focused widget (`Enter`).
    Activate,
    /// The menu for the focused widget.
    Secondary,
}

impl Key {
    /// The key for a browser `KeyboardEvent.key` value.
    ///
    /// `j`, `k`, `h`, `l` and the four arrows are the directions; `Enter` is
    /// [`Key::Activate`]; `Tab` is [`Key::Next`]; `m` is [`Key::Secondary`]
    /// (the text frontend has no character for the menu key, so `m` is the
    /// one chosen here). Anything else is `None`.
    ///
    /// Shift+Tab is [`Key::Prev`], and the host detects it: Shift is a
    /// modifier, not a key name, so `"Tab"` always maps to [`Key::Next`] here.
    pub fn from_name(name: &str) -> Option<Key> {
        match name {
            "j" | "ArrowDown" => Some(Key::Down),
            "k" | "ArrowUp" => Some(Key::Up),
            "h" | "ArrowLeft" => Some(Key::Left),
            "l" | "ArrowRight" => Some(Key::Right),
            "Enter" => Some(Key::Activate),
            "Tab" => Some(Key::Next),
            "m" => Some(Key::Secondary),
            _ => None,
        }
    }
}

/// A position in [`RenderTree::focus_order`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    index: usize,
}

impl Cursor {
    pub fn new() -> Self {
        Self::default()
    }

    /// The focused widget id, when the tree has a focus order.
    pub fn focused_id<'a>(&self, tree: &'a RenderTree) -> Option<&'a str> {
        tree.focus_order.get(self.index).map(String::as_str)
    }

    /// Focus the widget `id`. Returns `false`, and leaves the cursor, when
    /// `id` is not in the focus order.
    pub fn set_focus(&mut self, tree: &RenderTree, id: &str) -> bool {
        match tree.focus_order.iter().position(|focus| focus == id) {
            Some(index) => {
                self.index = index;
                true
            }
            None => false,
        }
    }

    /// Move the cursor, or build the event Enter would send.
    ///
    /// `Down` and `Up` return `None`: they change which widget is focused and
    /// do not tell the grammar anything happened. `Activate` returns a `click`
    /// on the focused id. An empty focus order activates nothing. A direction
    /// or menu key that is only a [`Gesture`] returns `None` here;
    /// [`Self::gesture_for`] is the one that reports it.
    pub fn event_for(&mut self, tree: &RenderTree, key: Key) -> Option<Event> {
        match key {
            Key::Down => {
                self.step(tree, true);
                None
            }
            Key::Up => {
                self.step(tree, false);
                None
            }
            Key::Activate => {
                let n = tree.focus_order.len();
                if n == 0 {
                    return None;
                }
                self.index = self.index.min(n - 1);
                Some(Event {
                    widget: tree.focus_order[self.index].clone(),
                    kind: EventKind::Click,
                    value: Value::Null,
                })
            }
            Key::Left | Key::Right | Key::Next | Key::Prev | Key::Secondary => None,
        }
    }

    /// The same keys as [`Self::event_for`], as a [`Gesture`].
    ///
    /// `Down`, `Up`, `Next`, and `Prev` walk the focus order and return
    /// `Move`. `Left` and `Right` return `Move` and leave the cursor.
    /// `Activate` and `Secondary` name the focused widget. An empty focus
    /// order still moves, and activates nothing.
    pub fn gesture_for(&mut self, tree: &RenderTree, key: Key) -> Option<Gesture> {
        if let Some(direction) = direction_of(key) {
            if matches!(key, Key::Down | Key::Up | Key::Next | Key::Prev) {
                self.step(tree, matches!(key, Key::Down | Key::Next));
            }
            return Some(Gesture::Move { direction });
        }
        let n = tree.focus_order.len();
        if n == 0 {
            return None;
        }
        self.index = self.index.min(n - 1);
        let element = Element::Widget {
            id: tree.focus_order[self.index].clone(),
        };
        match key {
            Key::Activate => Some(Gesture::Activate { element }),
            Key::Secondary => Some(Gesture::Secondary { element }),
            Key::Down | Key::Up | Key::Left | Key::Right | Key::Next | Key::Prev => None,
        }
    }

    /// [`Self::gesture_for`] as the layout verb it determines. A direction is
    /// `focus-direction`. Activate and the menu key are not layout verbs:
    /// this cursor is a surface widget, and no pane was named.
    pub fn layout_verb_for(&mut self, tree: &RenderTree, key: Key) -> Option<Verb> {
        self.gesture_for(tree, key)
            .as_ref()
            .and_then(|gesture| layout_verb(gesture, None))
    }

    fn step(&mut self, tree: &RenderTree, forward: bool) {
        let n = tree.focus_order.len();
        if n == 0 {
            return;
        }
        self.index = self.index.min(n - 1);
        self.index = if forward {
            (self.index + 1) % n
        } else {
            (self.index + n - 1) % n
        };
    }
}

fn direction_of(key: Key) -> Option<Direction> {
    match key {
        Key::Left => Some(Direction::Left),
        Key::Right => Some(Direction::Right),
        Key::Up => Some(Direction::Up),
        Key::Down => Some(Direction::Down),
        Key::Prev => Some(Direction::Prev),
        Key::Next => Some(Direction::Next),
        Key::Activate | Key::Secondary => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::{resolve, RenderKind, RenderNode};

    #[test]
    fn enter_names_the_focused_widget_and_j_k_walk_the_focus_order() {
        let spec = crate::surface::example_paper_triage();
        let tree = resolve(
            &spec,
            &serde_json::json!({}),
            &serde_json::json!({}),
            &serde_json::json!({}),
        );
        let mut cursor = Cursor::new();
        let first = cursor.focused_id(&tree).unwrap().to_string();
        assert!(cursor.event_for(&tree, Key::Down).is_none());
        if tree.focus_order.len() > 1 {
            assert_ne!(cursor.focused_id(&tree), Some(first.as_str()));
        }
        cursor.event_for(&tree, Key::Up);
        assert_eq!(cursor.focused_id(&tree), Some(first.as_str()));

        let event = cursor
            .event_for(&tree, Key::Activate)
            .expect("a focused widget");
        assert_eq!(event.widget, first);
        assert_eq!(event.kind, EventKind::Click);
    }

    #[test]
    fn a_direction_key_is_a_move_and_only_the_vertical_ones_walk_the_widgets() {
        let tree = two_buttons();
        let mut cursor = Cursor::new();
        assert_eq!(
            cursor.gesture_for(&tree, Key::Down),
            Some(Gesture::Move {
                direction: Direction::Down
            })
        );
        assert_eq!(cursor.focused_id(&tree), Some("b"));
        assert_eq!(
            cursor.gesture_for(&tree, Key::Left),
            Some(Gesture::Move {
                direction: Direction::Left
            })
        );
        assert_eq!(cursor.focused_id(&tree), Some("b"));
        assert!(cursor.event_for(&tree, Key::Left).is_none());
        assert_eq!(cursor.focused_id(&tree), Some("b"));

        assert_eq!(
            cursor.gesture_for(&tree, Key::Next),
            Some(Gesture::Move {
                direction: Direction::Next
            })
        );
        assert_eq!(cursor.focused_id(&tree), Some("a"));

        assert_eq!(
            cursor.gesture_for(&tree, Key::Activate),
            Some(Gesture::Activate {
                element: Element::Widget { id: "a".into() }
            })
        );
        assert_eq!(
            cursor.gesture_for(&tree, Key::Secondary),
            Some(Gesture::Secondary {
                element: Element::Widget { id: "a".into() }
            })
        );

        let mut verbs = Cursor::new();
        assert_eq!(
            verbs
                .layout_verb_for(&tree, Key::Down)
                .map(|verb| serde_json::to_value(verb).unwrap()),
            Some(serde_json::json!({"verb": "focus-direction", "direction": "down"}))
        );
        assert!(verbs.layout_verb_for(&tree, Key::Activate).is_none());

        let empty = RenderTree {
            root: two_buttons().root,
            focus_order: Vec::new(),
        };
        assert_eq!(
            cursor.gesture_for(&empty, Key::Up),
            Some(Gesture::Move {
                direction: Direction::Up
            })
        );
        assert!(cursor.gesture_for(&empty, Key::Activate).is_none());
        assert!(cursor.gesture_for(&empty, Key::Secondary).is_none());
        assert!(cursor.event_for(&empty, Key::Activate).is_none());
        assert!(cursor.focused_id(&empty).is_none());
    }

    #[test]
    fn set_focus_moves_to_an_id_in_the_order_and_refuses_any_other() {
        let tree = two_buttons();
        let mut cursor = Cursor::new();
        assert!(cursor.set_focus(&tree, "b"));
        assert_eq!(cursor.focused_id(&tree), Some("b"));
        assert!(!cursor.set_focus(&tree, "c"));
        assert_eq!(cursor.focused_id(&tree), Some("b"));
    }

    #[test]
    fn the_browser_key_names_map_to_the_vocabulary() {
        for (name, key) in [
            ("j", Key::Down),
            ("k", Key::Up),
            ("h", Key::Left),
            ("l", Key::Right),
            ("Enter", Key::Activate),
            ("Tab", Key::Next),
            ("ArrowDown", Key::Down),
            ("ArrowUp", Key::Up),
            ("ArrowLeft", Key::Left),
            ("ArrowRight", Key::Right),
            ("m", Key::Secondary),
        ] {
            assert_eq!(Key::from_name(name), Some(key), "{name}");
        }
        for name in ["", "J", "Escape", "Shift", " ", "jj"] {
            assert_eq!(Key::from_name(name), None, "{name:?}");
        }
    }

    fn two_buttons() -> RenderTree {
        let button = |id: &str, label: &str| RenderNode {
            id: id.into(),
            label: None,
            help: None,
            node: RenderKind::Button {
                label: label.into(),
            },
        };
        RenderTree {
            root: RenderNode {
                id: "c".into(),
                label: None,
                help: None,
                node: RenderKind::Column {
                    items: vec![button("a", "A"), button("b", "B")],
                },
            },
            focus_order: vec!["a".into(), "b".into()],
        }
    }
}
