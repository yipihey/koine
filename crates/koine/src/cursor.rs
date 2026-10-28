//! The focus cursor and the key vocabulary every frontend shares.
//!
//! A [`Cursor`] is an index into [`RenderTree::focus_order`]. [`Key`] is the
//! small set of keys a frontend understands. [`Cursor::event_for`] turns a key
//! into a surface [`Event`] for the focused widget, and [`Cursor::gesture_for`]
//! turns the same keys into a [`Gesture`]. The text frontend and the web
//! frontend both call these, so `j j Enter` means the same on each
//! (`docs/plan-koine-web.md` D4). Nothing here uses the `verbs` feature.
//!
//! # The row cursor (`docs/plan-koine-outline.md` O-D1)
//!
//! Inside a focused row widget (a `tree`, a `table` or a `list`) the cursor
//! also holds an *active row*, kept by the row's id so a re-render that
//! inserts a row above it does not move it. `j`/`k` move the active row and
//! stop at the ends; `Tab` still leaves the widget. [`Cursor::reply_for`] is
//! the whole rule, and answers a [`Reply`]; [`Cursor::event_for`] is that
//! answer reduced to "an event, or not".
//!
//! | key | row widget with an active row | otherwise |
//! |-----|-------------------------------|-----------|
//! | `j` `k` | move the active row, clamped (the first press sets the first row) | walk the focus order |
//! | `Tab` (`Next`/`Prev`) | walk the focus order, leaving the widget | the same |
//! | `Enter` | `select` `[id]` | `click` on the widget |
//! | `Space` | tree branch: `change` with the full expanded list toggled; leaf: nothing; table/list: `select` `[id]` | `Unhandled` |
//! | `l` | tree: collapsed branch `change` (expand), expanded branch moves to the first child, leaf nothing | `Unhandled` |
//! | `h` | tree: expanded branch `change` (collapse); else moves to the parent; at a root with nothing to collapse `Unhandled` | `Unhandled` |
//! | `d` | `delete` `[id]` | `Unhandled` |
//! | `o` | `activate` `[id]` | `Unhandled` |
//! | `r` `m` `x` `p` `P` | `Unhandled`: the renderer needs UI (an input, a prompt, a menu) and reads [`Cursor::active_row`] | `Unhandled` |
//!
//! `Unhandled` is the fall-through: the cursor has no meaning for the key
//! here, so the frontend may give it another (`h` and `l` become the layout
//! verb when the surface fills a pane; the renderer's own keys act on the
//! active row). [`Reply::Moved`] means the cursor took the key and the page
//! redraws, even when nothing changed (`l` on a leaf is swallowed, not passed
//! on).

use crate::gesture::{layout_verb, Element, Gesture};
use crate::layout::{Direction, Verb};
use crate::surface::{Event, EventKind, RenderKind, RenderNode, RenderTree};
use serde_json::Value;
use std::collections::BTreeMap;

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
    /// The menu for the focused widget (`m`).
    Secondary,
    /// Toggle the active row (`Space`): a tree branch opens or closes, a
    /// table or list row is selected.
    Toggle,
    /// Rename the active row (`r`). The renderer's: it needs an input.
    Rename,
    /// Delete the active row (`d`).
    Delete,
    /// Open the active row (`o`): an `activate` event.
    Open,
    /// The row menu, from the physical menu key (`ContextMenu`). `m` is
    /// [`Key::Secondary`]; a frontend treats the two alike.
    Menu,
    /// Cut the active row (`x`). The renderer's.
    Cut,
    /// Paste into the active row (`p`). The renderer's.
    Paste,
    /// Paste beside the active row (`P`). The renderer's.
    PasteBeside,
    /// Ask an agent about what is shown (`a`, plan-koine-research-os G-D5).
    /// The frontend's: it opens a prompt with the surface as the context.
    Ask,
}

/// What the cursor did with a key; see [`Cursor::reply_for`].
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// The cursor sends this surface event.
    Event(Event),
    /// The cursor took the key and nothing is sent; redraw. Nothing may have
    /// changed (a clamped `j`, `l` on a leaf).
    Moved,
    /// The cursor has no meaning for the key here: the frontend may give it
    /// another (the layout verb for `h`/`l`, the renderer's `r`/`m`/`x`/`p`/`P`).
    Unhandled,
}

impl Key {
    /// The key for a browser `KeyboardEvent.key` value.
    ///
    /// `j`, `k`, `h`, `l` and the four arrows are the directions; `Enter` is
    /// [`Key::Activate`]; `Tab` is [`Key::Next`]; `m` is [`Key::Secondary`]
    /// (the text frontend has no character for the menu key, so `m` is the
    /// one chosen here). The row keys are `Space` (`" "` or `"Space"`,
    /// [`Key::Toggle`]), `r`, `d`, `o`, `x`, `p`, `P` and `ContextMenu`
    /// ([`Key::Menu`]); see the module docs. Anything else is `None`.
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
            " " | "Space" => Some(Key::Toggle),
            "r" => Some(Key::Rename),
            "d" => Some(Key::Delete),
            "o" => Some(Key::Open),
            "ContextMenu" | "Menu" => Some(Key::Menu),
            "x" => Some(Key::Cut),
            "p" => Some(Key::Paste),
            "P" => Some(Key::PasteBeside),
            "a" => Some(Key::Ask),
            _ => None,
        }
    }
}

/// A position in [`RenderTree::focus_order`], and the active row of each row
/// widget (by row id).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Cursor {
    index: usize,
    active: BTreeMap<String, Value>,
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
    /// [`Self::reply_for`] with the answer reduced: `Some` for a
    /// [`Reply::Event`], `None` for [`Reply::Moved`] and [`Reply::Unhandled`].
    /// A frontend that must tell "the cursor took the key" from "not mine"
    /// (to fall through to the layout) uses `reply_for`.
    ///
    /// `Down` and `Up` return `None`: they change which widget is focused (or
    /// which row is active) and do not tell the grammar anything happened.
    /// `Activate` on a widget without an active row returns a `click` on the
    /// focused id; with one, a `select` of its id. An empty focus order
    /// activates nothing. A direction or menu key that is only a [`Gesture`]
    /// returns `None` here; [`Self::gesture_for`] is the one that reports it.
    pub fn event_for(&mut self, tree: &RenderTree, key: Key) -> Option<Event> {
        match self.reply_for(tree, key) {
            Reply::Event(event) => Some(event),
            Reply::Moved | Reply::Unhandled => None,
        }
    }

    /// What the cursor does with `key`: the rule in the module docs.
    pub fn reply_for(&mut self, tree: &RenderTree, key: Key) -> Reply {
        let n = tree.focus_order.len();
        if n > 0 {
            self.index = self.index.min(n - 1);
        }
        let widget = tree
            .focus_order
            .get(self.index)
            .and_then(|id| find(&tree.root, id));
        let rows = widget.and_then(|w| row_ids(w).map(|ids| (w, ids)));
        // The active row's position, or `None` (and the stored id dropped)
        // when the row is gone.
        let active = match &rows {
            Some((w, ids)) => {
                let at = self
                    .active
                    .get(&w.id)
                    .and_then(|id| ids.iter().position(|i| i == id));
                if at.is_none() {
                    self.active.remove(&w.id);
                }
                at
            }
            None => None,
        };
        let sends = |w: &RenderNode, kind: EventKind, value: Value| {
            Reply::Event(Event {
                widget: w.id.clone(),
                kind,
                value,
            })
        };

        match key {
            Key::Down | Key::Up => {
                match &rows {
                    Some((w, ids)) if !ids.is_empty() => {
                        let last = ids.len() - 1;
                        let to = match (active, key) {
                            (None, _) => 0,
                            (Some(i), Key::Down) => (i + 1).min(last),
                            (Some(i), _) => i.saturating_sub(1),
                        };
                        self.active.insert(w.id.clone(), ids[to].clone());
                        // `follow`: the row the cursor lands on is selected,
                        // as a click on it would select it.
                        if follows(w) && active != Some(to) {
                            return sends(
                                w,
                                EventKind::Select,
                                Value::Array(vec![ids[to].clone()]),
                            );
                        }
                    }
                    Some(_) => {}
                    None => self.step(tree, key == Key::Down),
                }
                Reply::Moved
            }
            Key::Activate => match (&rows, active) {
                (Some((w, ids)), Some(i)) => {
                    sends(w, EventKind::Select, Value::Array(vec![ids[i].clone()]))
                }
                _ => match widget {
                    Some(w) => sends(w, EventKind::Click, Value::Null),
                    None => Reply::Unhandled,
                },
            },
            Key::Toggle => match (&rows, active) {
                (Some((w, ids)), Some(i)) => match &w.node {
                    RenderKind::Tree {
                        nodes, expanded, ..
                    } => {
                        if nodes[i].has_children {
                            sends(
                                w,
                                EventKind::Change,
                                toggled(expanded, &ids[i], !nodes[i].expanded),
                            )
                        } else {
                            Reply::Moved
                        }
                    }
                    _ => sends(w, EventKind::Select, Value::Array(vec![ids[i].clone()])),
                },
                _ => Reply::Unhandled,
            },
            Key::Left | Key::Right => match (&rows, active) {
                (
                    Some((
                        w @ RenderNode {
                            node:
                                RenderKind::Tree {
                                    nodes, expanded, ..
                                },
                            ..
                        },
                        ids,
                    )),
                    Some(i),
                ) => {
                    let node = &nodes[i];
                    if key == Key::Right {
                        if node.has_children && !node.expanded {
                            sends(w, EventKind::Change, toggled(expanded, &ids[i], true))
                        } else {
                            if node.has_children {
                                if let Some(child) =
                                    nodes.get(i + 1).filter(|c| c.depth > node.depth)
                                {
                                    self.active.insert(w.id.clone(), child.id.clone());
                                }
                            }
                            Reply::Moved
                        }
                    } else if node.has_children && node.expanded {
                        sends(w, EventKind::Change, toggled(expanded, &ids[i], false))
                    } else if let Some(parent) =
                        nodes[..i].iter().rev().find(|p| p.depth < node.depth)
                    {
                        self.active.insert(w.id.clone(), parent.id.clone());
                        Reply::Moved
                    } else {
                        Reply::Unhandled
                    }
                }
                _ => Reply::Unhandled,
            },
            Key::Delete | Key::Open => match (&rows, active) {
                (Some((w, ids)), Some(i)) => {
                    let kind = if key == Key::Delete {
                        EventKind::Delete
                    } else {
                        EventKind::Activate
                    };
                    sends(w, kind, Value::Array(vec![ids[i].clone()]))
                }
                _ => Reply::Unhandled,
            },
            Key::Next | Key::Prev => {
                self.step(tree, key == Key::Next);
                Reply::Moved
            }
            Key::Secondary
            | Key::Menu
            | Key::Rename
            | Key::Cut
            | Key::Paste
            | Key::PasteBeside
            | Key::Ask => Reply::Unhandled,
        }
    }

    /// The id of the active row of `widget`, when that row is still in the
    /// widget in `tree`. A row that a re-render removed answers `None`.
    /// The `menu` event a row key sends (`MenuAction::key`): `name` (a
    /// `KeyboardEvent.key`, one character) is one of the focused row
    /// widget's keys and the widget has an active row. A frontend asks this
    /// before [`Cursor::reply_for`], because a widget's own key wins over the
    /// renderer's row key of the same letter.
    pub fn row_key(&self, tree: &RenderTree, name: &str) -> Option<Event> {
        let widget = self.focused_id(tree)?;
        let node = find(&tree.root, widget)?;
        let keys = match &node.node {
            RenderKind::Table { action_keys, .. }
            | RenderKind::List { action_keys, .. }
            | RenderKind::Tree { action_keys, .. } => action_keys,
            _ => return None,
        };
        let label = keys.get(name)?;
        let row = self.active_row(tree, widget)?;
        Some(Event {
            widget: widget.to_string(),
            kind: EventKind::Menu,
            value: serde_json::json!({ "id": row, "action": label }),
        })
    }

    pub fn active_row<'a>(&'a self, tree: &RenderTree, widget: &str) -> Option<&'a Value> {
        let ids = row_ids(find(&tree.root, widget)?)?;
        let id = self.active.get(widget)?;
        ids.contains(id).then_some(id)
    }

    /// The active row's position among `widget`'s rows in `tree`, in drawing
    /// order (a tree's visible nodes).
    pub fn active_index(&self, tree: &RenderTree, widget: &str) -> Option<usize> {
        let ids = row_ids(find(&tree.root, widget)?)?;
        let id = self.active.get(widget)?;
        ids.iter().position(|i| i == id)
    }

    /// Make `id` the active row of `widget` (a click on a row does this). The
    /// id is not checked until [`Self::active_row`] is asked.
    pub fn set_active_row(&mut self, widget: &str, id: Value) {
        self.active.insert(widget.to_string(), id);
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
            _ => None,
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
        _ => None,
    }
}

fn find<'a>(node: &'a RenderNode, id: &str) -> Option<&'a RenderNode> {
    if node.id == id {
        return Some(node);
    }
    let children: Vec<&RenderNode> = match &node.node {
        RenderKind::Column { items }
        | RenderKind::Row { items }
        | RenderKind::Grid { items, .. } => items.iter().collect(),
        RenderKind::Section { body, .. } => vec![body.as_ref()],
        RenderKind::Tabs { tabs } => tabs.iter().map(|tab| &tab.body).collect(),
        _ => Vec::new(),
    };
    children.into_iter().find_map(|child| find(child, id))
}

/// The row ids of a row widget, in drawing order; `None` for any other node.
/// A tree's rows are its visible nodes (id = the node's). A table's and a
/// list's rows are the array (or the `{rows}` envelope around it); a row's id
/// is, for an object, the table's `id_field` then `id`, and for anything else
/// (or an object with neither) its index as a number.
/// Whether a row widget's selection follows the cursor (`follow`).
fn follows(node: &RenderNode) -> bool {
    matches!(
        &node.node,
        RenderKind::Table { follow: true, .. }
            | RenderKind::List { follow: true, .. }
            | RenderKind::Tree { follow: true, .. }
    )
}

fn row_ids(node: &RenderNode) -> Option<Vec<Value>> {
    let (rows, id_field) = match &node.node {
        RenderKind::Tree { nodes, .. } => {
            return Some(nodes.iter().map(|n| n.id.clone()).collect())
        }
        RenderKind::Table { rows, id_field, .. } => (rows, id_field.as_deref()),
        RenderKind::List { rows, .. } => (rows, None),
        _ => return None,
    };
    let rows = match rows {
        Value::Array(rows) => rows,
        Value::Object(map) => match map.get("rows") {
            Some(Value::Array(rows)) => rows,
            _ => return Some(Vec::new()),
        },
        _ => return Some(Vec::new()),
    };
    Some(
        rows.iter()
            .enumerate()
            .map(|(index, row)| {
                id_field
                    .into_iter()
                    .chain(["id"])
                    .filter_map(|field| row.get(field))
                    .find(|v| !v.is_null())
                    .cloned()
                    .unwrap_or_else(|| Value::from(index))
            })
            .collect(),
    )
}

/// The full expanded list with `id` added (`open`) or removed.
fn toggled(expanded: &[Value], id: &Value, open: bool) -> Value {
    let mut list: Vec<Value> = expanded.iter().filter(|v| *v != id).cloned().collect();
    if open {
        list.push(id.clone());
    }
    Value::Array(list)
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
            (" ", Key::Toggle),
            ("Space", Key::Toggle),
            ("r", Key::Rename),
            ("d", Key::Delete),
            ("o", Key::Open),
            ("ContextMenu", Key::Menu),
            ("x", Key::Cut),
            ("p", Key::Paste),
            ("P", Key::PasteBeside),
            ("a", Key::Ask),
        ] {
            assert_eq!(Key::from_name(name), Some(key), "{name}");
        }
        for name in ["", "J", "Escape", "Shift", "jj", "D", "R"] {
            assert_eq!(Key::from_name(name), None, "{name:?}");
        }
    }

    // ---- the row cursor (plan-koine-outline O-D1) ----

    use crate::surface::RenderKind as K;
    use serde_json::json;

    fn tnode(
        id: Value,
        depth: u32,
        has_children: bool,
        expanded: bool,
    ) -> crate::surface::tree::TreeNode {
        crate::surface::tree::TreeNode {
            id,
            label: String::new(),
            depth,
            has_children,
            expanded,
            count: None,
            row: Value::Null,
        }
    }

    fn widget(id: &str, node: K) -> RenderNode {
        RenderNode {
            id: id.into(),
            label: None,
            help: None,
            node,
        }
    }

    fn page(widgets: Vec<RenderNode>) -> RenderTree {
        let focus_order = widgets.iter().map(|w| w.id.clone()).collect();
        RenderTree {
            root: widget("page", K::Column { items: widgets }),
            focus_order,
        }
    }

    /// root(1, open) > [a(2, open) > [a1(3, leaf)], b(4, closed)] ; root2(5, leaf)
    /// Expanded also holds 9, a branch hidden under a collapsed ancestor.
    fn outline() -> RenderTree {
        page(vec![widget(
            "tree",
            K::Tree {
                action_keys: Default::default(),
                follow: Default::default(),
                nodes: vec![
                    tnode(json!(1), 0, true, true),
                    tnode(json!(2), 1, true, true),
                    tnode(json!(3), 2, false, false),
                    tnode(json!(4), 1, true, false),
                    tnode(json!(5), 0, false, false),
                ],
                bind: Some("open".into()),
                expanded: vec![json!(1), json!(2), json!(9)],
                actions: Vec::new(),
                movable: false,
                renameable: false,
                deletable: false,
            },
        )])
    }

    fn change(value: Value) -> Reply {
        Reply::Event(Event {
            widget: "tree".into(),
            kind: EventKind::Change,
            value,
        })
    }

    fn at(cursor: &Cursor, tree: &RenderTree) -> Option<Value> {
        cursor.active_row(tree, "tree").cloned()
    }

    #[test]
    fn j_and_k_move_the_active_row_and_clamp_at_the_ends() {
        let tree = outline();
        let mut cursor = Cursor::new();
        assert_eq!(at(&cursor, &tree), None);
        assert_eq!(cursor.reply_for(&tree, Key::Down), Reply::Moved);
        assert_eq!(
            at(&cursor, &tree),
            Some(json!(1)),
            "the first press sets the first row"
        );
        assert!(cursor.event_for(&tree, Key::Down).is_none());
        assert_eq!(cursor.active_index(&tree, "tree"), Some(1));
        for _ in 0..10 {
            cursor.event_for(&tree, Key::Down);
        }
        assert_eq!(at(&cursor, &tree), Some(json!(5)), "clamped, not wrapped");
        assert_eq!(cursor.focused_id(&tree), Some("tree"));
        for _ in 0..10 {
            cursor.event_for(&tree, Key::Up);
        }
        assert_eq!(at(&cursor, &tree), Some(json!(1)));

        // k on a fresh cursor also lands on the first row.
        let mut fresh = Cursor::new();
        fresh.event_for(&tree, Key::Up);
        assert_eq!(at(&fresh, &tree), Some(json!(1)));
    }

    #[test]
    fn j_and_k_still_walk_widgets_when_the_focus_is_not_a_row_widget() {
        let mut tree = two_buttons();
        tree.root = widget(
            "c",
            K::Column {
                items: vec![
                    match tree.root.node {
                        K::Column { items } => items.into_iter().next().unwrap(),
                        _ => unreachable!(),
                    },
                    widget(
                        "list",
                        K::List {
                            action_keys: Default::default(),
                            follow: Default::default(),
                            rows: json!(["x", "y"]),
                            actions: Vec::new(),
                            count_field: None,
                        },
                    ),
                ],
            },
        );
        tree.focus_order = vec!["a".into(), "list".into()];
        let mut cursor = Cursor::new();
        cursor.event_for(&tree, Key::Down);
        assert_eq!(cursor.focused_id(&tree), Some("list"));
        cursor.event_for(&tree, Key::Down);
        assert_eq!(
            cursor.focused_id(&tree),
            Some("list"),
            "j stays inside the list"
        );
        assert_eq!(cursor.active_index(&tree, "list"), Some(0));
        cursor.event_for(&tree, Key::Prev);
        assert_eq!(cursor.focused_id(&tree), Some("a"), "Tab leaves");
    }

    #[test]
    fn enter_selects_the_active_row_and_clicks_without_one() {
        let tree = outline();
        let mut cursor = Cursor::new();
        let click = cursor.event_for(&tree, Key::Activate).unwrap();
        assert_eq!((click.kind, click.value), (EventKind::Click, Value::Null));
        cursor.set_active_row("tree", json!(4));
        assert_eq!(
            cursor.event_for(&tree, Key::Activate),
            Some(Event {
                widget: "tree".into(),
                kind: EventKind::Select,
                value: json!([4]),
            })
        );
    }

    #[test]
    fn space_toggles_the_active_branch_and_carries_the_full_expanded_list() {
        let tree = outline();
        let mut cursor = Cursor::new();
        cursor.set_active_row("tree", json!(2));
        // 2 is open: removed, the hidden 9 kept.
        assert_eq!(cursor.reply_for(&tree, Key::Toggle), change(json!([1, 9])));
        cursor.set_active_row("tree", json!(4));
        assert_eq!(
            cursor.reply_for(&tree, Key::Toggle),
            change(json!([1, 2, 9, 4]))
        );
        cursor.set_active_row("tree", json!(3));
        assert_eq!(
            cursor.reply_for(&tree, Key::Toggle),
            Reply::Moved,
            "a leaf has nothing to toggle"
        );
        let mut none = Cursor::new();
        assert_eq!(none.reply_for(&tree, Key::Toggle), Reply::Unhandled);
    }

    #[test]
    fn space_selects_on_a_table_and_a_list() {
        let tree = page(vec![widget(
            "t",
            K::Table {
                action_keys: Default::default(),
                follow: Default::default(),
                rows: json!([{"k": "a"}, {"k": "b"}]),
                columns: vec!["k".into()],
                id_field: Some("k".into()),
                actions: Vec::new(),
                count_field: None,
            },
        )]);
        let mut cursor = Cursor::new();
        cursor.event_for(&tree, Key::Down);
        cursor.event_for(&tree, Key::Down);
        assert_eq!(
            cursor.event_for(&tree, Key::Toggle),
            Some(Event {
                widget: "t".into(),
                kind: EventKind::Select,
                value: json!(["b"]),
            })
        );
    }

    fn keyed_table(follow: bool) -> RenderTree {
        page(vec![widget(
            "t",
            K::Table {
                action_keys: [("d".to_string(), "Dismiss".to_string())].into(),
                follow,
                rows: json!([{"k": "a"}, {"k": "b"}]),
                columns: vec!["k".into()],
                id_field: Some("k".into()),
                actions: vec!["Dismiss".into()],
                count_field: None,
            },
        )])
    }

    #[test]
    fn a_row_key_sends_its_menu_entry_for_the_active_row_only() {
        let tree = keyed_table(false);
        let mut cursor = Cursor::new();
        assert_eq!(cursor.row_key(&tree, "d"), None, "no active row yet");
        cursor.event_for(&tree, Key::Down);
        assert_eq!(
            cursor.row_key(&tree, "d"),
            Some(Event {
                widget: "t".into(),
                kind: EventKind::Menu,
                value: json!({"id": "a", "action": "Dismiss"}),
            })
        );
        assert_eq!(
            cursor.row_key(&tree, "s"),
            None,
            "an undeclared key is not the widget's"
        );
    }

    #[test]
    fn with_follow_j_and_k_select_the_row_they_land_on() {
        let tree = keyed_table(true);
        let mut cursor = Cursor::new();
        let select = |id: &str| {
            Some(Event {
                widget: "t".into(),
                kind: EventKind::Select,
                value: json!([id]),
            })
        };
        assert_eq!(cursor.event_for(&tree, Key::Down), select("a"));
        assert_eq!(cursor.event_for(&tree, Key::Down), select("b"));
        assert_eq!(
            cursor.event_for(&tree, Key::Down),
            None,
            "clamped: nothing new to select"
        );
        assert_eq!(cursor.event_for(&tree, Key::Up), select("a"));
        // Without `follow` the cursor only marks the row.
        let plain = keyed_table(false);
        let mut cursor = Cursor::new();
        assert_eq!(cursor.event_for(&plain, Key::Down), None);
    }

    #[test]
    fn h_and_l_walk_a_three_level_tree_and_h_at_a_root_falls_through() {
        let tree = outline();
        let mut cursor = Cursor::new();
        cursor.set_active_row("tree", json!(1));
        // l on an expanded branch: first child.
        assert_eq!(cursor.reply_for(&tree, Key::Right), Reply::Moved);
        assert_eq!(at(&cursor, &tree), Some(json!(2)));
        assert_eq!(cursor.reply_for(&tree, Key::Right), Reply::Moved);
        assert_eq!(at(&cursor, &tree), Some(json!(3)));
        // l on a leaf: swallowed.
        assert_eq!(cursor.reply_for(&tree, Key::Right), Reply::Moved);
        assert_eq!(at(&cursor, &tree), Some(json!(3)));
        // h on a leaf: its parent; h on an expanded branch: collapse.
        assert_eq!(cursor.reply_for(&tree, Key::Left), Reply::Moved);
        assert_eq!(at(&cursor, &tree), Some(json!(2)));
        assert_eq!(cursor.reply_for(&tree, Key::Left), change(json!([1, 9])));
        // l on a collapsed branch: expand, carrying the list.
        cursor.set_active_row("tree", json!(4));
        assert_eq!(
            cursor.reply_for(&tree, Key::Right),
            change(json!([1, 2, 9, 4]))
        );
        // h on a collapsed branch: its parent (the nearest shallower node above).
        assert_eq!(cursor.reply_for(&tree, Key::Left), Reply::Moved);
        assert_eq!(at(&cursor, &tree), Some(json!(1)));
        // h on an expanded root: collapse; on a leaf root: nothing to do, fall through.
        assert_eq!(cursor.reply_for(&tree, Key::Left), change(json!([2, 9])));
        cursor.set_active_row("tree", json!(5));
        assert_eq!(cursor.reply_for(&tree, Key::Left), Reply::Unhandled);
        assert_eq!(at(&cursor, &tree), Some(json!(5)));
        // event_for reduces Moved and Unhandled alike to None.
        assert!(cursor.event_for(&tree, Key::Left).is_none());
        // Without an active row, h and l are the layout's.
        let mut fresh = Cursor::new();
        assert_eq!(fresh.reply_for(&tree, Key::Left), Reply::Unhandled);
        assert_eq!(fresh.reply_for(&tree, Key::Right), Reply::Unhandled);
    }

    #[test]
    fn a_collapsed_root_branch_h_falls_through() {
        let tree = page(vec![widget(
            "tree",
            K::Tree {
                action_keys: Default::default(),
                follow: Default::default(),
                nodes: vec![tnode(json!("r"), 0, true, false)],
                bind: None,
                expanded: Vec::new(),
                actions: Vec::new(),
                movable: false,
                renameable: false,
                deletable: false,
            },
        )]);
        let mut cursor = Cursor::new();
        cursor.set_active_row("tree", json!("r"));
        assert_eq!(cursor.reply_for(&tree, Key::Left), Reply::Unhandled);
    }

    #[test]
    fn table_and_list_row_ids_follow_the_id_field_then_id_then_the_index() {
        let table = |rows: Value, id_field: Option<&str>| {
            page(vec![widget(
                "w",
                K::Table {
                    action_keys: Default::default(),
                    follow: Default::default(),
                    rows,
                    columns: Vec::new(),
                    id_field: id_field.map(String::from),
                    actions: Vec::new(),
                    count_field: None,
                },
            )])
        };
        let ids = |tree: &RenderTree| row_ids(find(&tree.root, "w").unwrap()).unwrap();
        let by_field = table(
            json!([{"key": "a", "id": 1}, {"id": 2}, {"x": 0}]),
            Some("key"),
        );
        assert_eq!(ids(&by_field), vec![json!("a"), json!(2), json!(2)]);
        let by_id = table(json!([{"id": "p"}, {"id": "q"}]), None);
        assert_eq!(ids(&by_id), vec![json!("p"), json!("q")]);
        let envelope = table(json!({"rows": [{"id": 7}]}), None);
        assert_eq!(ids(&envelope), vec![json!(7)]);
        let list = page(vec![widget(
            "w",
            K::List {
                action_keys: Default::default(),
                follow: Default::default(),
                rows: json!(["x", {"id": "y"}, {"n": 1}, 4]),
                actions: Vec::new(),
                count_field: None,
            },
        )]);
        assert_eq!(ids(&list), vec![json!(0), json!("y"), json!(2), json!(3)]);

        let mut cursor = Cursor::new();
        cursor.event_for(&list, Key::Down);
        cursor.event_for(&list, Key::Down);
        assert_eq!(cursor.active_row(&list, "w"), Some(&json!("y")));
        assert_eq!(
            cursor.event_for(&list, Key::Activate).map(|e| e.value),
            Some(json!(["y"]))
        );
    }

    #[test]
    fn a_stale_active_row_drops_and_a_row_inserted_above_does_not_move_it() {
        let mut cursor = Cursor::new();
        let before = page(vec![widget(
            "tree",
            K::Tree {
                action_keys: Default::default(),
                follow: Default::default(),
                nodes: vec![
                    tnode(json!(1), 0, false, false),
                    tnode(json!(2), 0, false, false),
                ],
                bind: None,
                expanded: Vec::new(),
                actions: Vec::new(),
                movable: false,
                renameable: false,
                deletable: false,
            },
        )]);
        cursor.set_active_row("tree", json!(2));
        assert_eq!(cursor.active_index(&before, "tree"), Some(1));
        let inserted = page(vec![widget(
            "tree",
            K::Tree {
                action_keys: Default::default(),
                follow: Default::default(),
                nodes: vec![
                    tnode(json!(0), 0, false, false),
                    tnode(json!(1), 0, false, false),
                    tnode(json!(2), 0, false, false),
                ],
                bind: None,
                expanded: Vec::new(),
                actions: Vec::new(),
                movable: false,
                renameable: false,
                deletable: false,
            },
        )]);
        assert_eq!(cursor.active_row(&inserted, "tree"), Some(&json!(2)));
        assert_eq!(cursor.active_index(&inserted, "tree"), Some(2));

        let deleted = page(vec![widget(
            "tree",
            K::Tree {
                action_keys: Default::default(),
                follow: Default::default(),
                nodes: vec![tnode(json!(1), 0, false, false)],
                bind: None,
                expanded: Vec::new(),
                actions: Vec::new(),
                movable: false,
                renameable: false,
                deletable: false,
            },
        )]);
        assert_eq!(cursor.active_row(&deleted, "tree"), None);
        assert_eq!(cursor.active_index(&deleted, "tree"), None);
        // Keys on the stale widget behave as with no row: Enter clicks, j starts again.
        let click = cursor.event_for(&deleted, Key::Activate).unwrap();
        assert_eq!(click.kind, EventKind::Click);
        cursor.event_for(&deleted, Key::Down);
        assert_eq!(cursor.active_row(&deleted, "tree"), Some(&json!(1)));
        // An unknown widget has no row.
        assert_eq!(cursor.active_row(&deleted, "nope"), None);
    }

    #[test]
    fn the_active_row_survives_set_focus() {
        let mut tree = outline();
        tree.focus_order = vec!["tree".into()];
        let mut cursor = Cursor::new();
        cursor.set_active_row("tree", json!(4));
        assert!(cursor.set_focus(&tree, "tree"));
        assert_eq!(at(&cursor, &tree), Some(json!(4)));
    }

    #[test]
    fn d_and_o_send_delete_and_activate_and_the_ui_keys_are_the_renderers() {
        let tree = outline();
        let mut cursor = Cursor::new();
        for key in [Key::Delete, Key::Open, Key::Rename, Key::Cut, Key::Paste] {
            assert_eq!(
                cursor.reply_for(&tree, key),
                Reply::Unhandled,
                "{key:?} with no row"
            );
        }
        cursor.set_active_row("tree", json!(3));
        let event = |kind| {
            Reply::Event(Event {
                widget: "tree".into(),
                kind,
                value: json!([3]),
            })
        };
        assert_eq!(
            cursor.reply_for(&tree, Key::Delete),
            event(EventKind::Delete)
        );
        assert_eq!(
            cursor.reply_for(&tree, Key::Open),
            event(EventKind::Activate)
        );
        for key in [
            Key::Rename,
            Key::Secondary,
            Key::Menu,
            Key::Cut,
            Key::Paste,
            Key::PasteBeside,
        ] {
            assert_eq!(cursor.reply_for(&tree, key), Reply::Unhandled, "{key:?}");
        }
        assert_eq!(
            at(&cursor, &tree),
            Some(json!(3)),
            "the renderer reads the active row"
        );
    }

    #[test]
    fn the_row_keys_mean_nothing_to_the_gestures() {
        let tree = outline();
        let mut cursor = Cursor::new();
        for key in [
            Key::Toggle,
            Key::Delete,
            Key::Open,
            Key::Rename,
            Key::Menu,
            Key::Cut,
        ] {
            assert!(cursor.gesture_for(&tree, key).is_none(), "{key:?}");
            assert!(cursor.layout_verb_for(&tree, key).is_none(), "{key:?}");
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
