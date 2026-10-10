//! A text performance of a [`koine::surface::RenderTree`].
//!
//! [`render`] walks the tree and writes one line per node. [`TextFrontend::key`]
//! turns j/k/Enter into an [`koine::surface::Event`] for the focused widget.
//! [`TextFrontend::gesture_key`] turns those keys, and h/l and the menu key,
//! into a [`koine::gesture::Gesture`]. [`TextFrontend::layout_verb`] is that
//! gesture through [`koine::gesture::layout_verb`]. [`koine::surface::reduce`]
//! still decides a surface event. The host applies the verb.
//!
//! [`frame`] is a pane drawn as cells: the box is the border, `≡` on the top
//! edge is the grip, and tab labels share that edge. An SGR 1006 report
//! becomes the same gesture a key does.
//!
//! How a line is spelled, and how far a column or a row indents, comes from
//! the tui [`koine::style::StyleSheet`]. The defaults are the lines this
//! frontend already wrote. A sheet tagged for another frontend is not
//! applied here: this host only opens the `tui` sheet.
//!
//! [`host`] edits the shared file `<workspace>/settings/koine-style.json`.

pub mod frame;
pub mod host;

use koine::style::{editor_apply, editor_spec, StyleError, StyleHost};
use koine::surface::{Event, EventKind, RenderKind, RenderNode, RenderTree, SurfaceSpec};
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

/// One text performance of a tree: a cursor into [`RenderTree::focus_order`]
/// and the tui style sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextFrontend {
    focus: usize,
    style: StyleHost,
}

impl Default for TextFrontend {
    fn default() -> Self {
        Self::new()
    }
}

impl TextFrontend {
    pub fn new() -> Self {
        Self {
            focus: 0,
            style: StyleHost::new("tui").expect("tui is a koine frontend"),
        }
    }

    pub fn sheet(&self) -> &koine::style::StyleSheet {
        self.style.sheet()
    }

    pub fn set_style(&mut self, slot: &str, raw: &str) -> Result<(), StyleError> {
        self.style.set(slot, raw)
    }

    pub fn reset_style(&mut self, slot: &str) -> Result<(), StyleError> {
        self.style.reset(slot)
    }

    /// The editor surface for this sheet. Resolve it with
    /// [`koine::style::editor_state`] to show the current values.
    pub fn editor_spec(&self) -> SurfaceSpec {
        editor_spec(self.style.frontend()).expect("tui is a koine frontend")
    }

    /// A field change sets the named slot. A click on `reset:<slot>` resets it.
    pub fn apply_editor(&mut self, event: &Event) -> Result<bool, StyleError> {
        editor_apply(&mut self.style, event)
    }

    /// The focused widget id, when the tree has a focus order.
    pub fn focused_id<'a>(&self, tree: &'a RenderTree) -> Option<&'a str> {
        tree.focus_order.get(self.focus).map(String::as_str)
    }

    /// The tree as lines. The focused widget's line starts with the `focus` slot.
    pub fn render(&self, tree: &RenderTree) -> String {
        let focused = self.focused_id(tree);
        let mut lines = Vec::new();
        write_node(&tree.root, 0, focused, self.style.sheet(), &mut lines);
        lines.join("\n")
    }

    /// Move the cursor, or build the event Enter would send.
    ///
    /// `Down` and `Up` return `None`: they change which widget is focused and
    /// do not tell the grammar anything happened. `Activate` returns a `click`
    /// on the focused id. An empty focus order activates nothing. A direction
    /// or menu key that is only a [`koine::gesture::Gesture`] returns `None`
    /// here; [`Self::gesture_key`] is the one that reports it.
    pub fn key(&mut self, tree: &RenderTree, key: Key) -> Option<Event> {
        match key {
            Key::Down => {
                self.step_focus(tree, true);
                None
            }
            Key::Up => {
                self.step_focus(tree, false);
                None
            }
            Key::Activate => {
                let n = tree.focus_order.len();
                if n == 0 {
                    return None;
                }
                self.focus = self.focus.min(n - 1);
                Some(Event {
                    widget: tree.focus_order[self.focus].clone(),
                    kind: EventKind::Click,
                    value: Value::Null,
                })
            }
            Key::Left | Key::Right | Key::Next | Key::Prev | Key::Secondary => None,
        }
    }

    /// The same keys as [`Self::key`], as a [`koine::gesture::Gesture`].
    ///
    /// `Down`, `Up`, `Next`, and `Prev` walk the focus order and return
    /// `Move`. `Left` and `Right` return `Move` and leave the cursor.
    /// `Activate` and `Secondary` name the focused widget. An empty focus
    /// order still moves, and activates nothing.
    pub fn gesture_key(&mut self, tree: &RenderTree, key: Key) -> Option<koine::gesture::Gesture> {
        use koine::gesture::{Element, Gesture};

        if let Some(direction) = direction_of(key) {
            if matches!(key, Key::Down | Key::Up | Key::Next | Key::Prev) {
                self.step_focus(tree, matches!(key, Key::Down | Key::Next));
            }
            return Some(Gesture::Move { direction });
        }
        let n = tree.focus_order.len();
        if n == 0 {
            return None;
        }
        self.focus = self.focus.min(n - 1);
        let element = Element::Widget {
            id: tree.focus_order[self.focus].clone(),
        };
        match key {
            Key::Activate => Some(Gesture::Activate { element }),
            Key::Secondary => Some(Gesture::Secondary { element }),
            Key::Down | Key::Up | Key::Left | Key::Right | Key::Next | Key::Prev => None,
        }
    }

    /// [`Self::gesture_key`] as the layout verb it determines. A direction is
    /// `focus-direction`. Activate and the menu key are not layout verbs:
    /// this frontend's cursor is a surface widget, and no pane was named.
    pub fn layout_verb(&mut self, tree: &RenderTree, key: Key) -> Option<koine::layout::Verb> {
        self.gesture_key(tree, key)
            .as_ref()
            .and_then(|gesture| koine::gesture::layout_verb(gesture, None))
    }

    fn step_focus(&mut self, tree: &RenderTree, forward: bool) {
        let n = tree.focus_order.len();
        if n == 0 {
            return;
        }
        self.focus = self.focus.min(n - 1);
        self.focus = if forward {
            (self.focus + 1) % n
        } else {
            (self.focus + n - 1) % n
        };
    }
}

fn direction_of(key: Key) -> Option<koine::layout::Direction> {
    use koine::layout::Direction;
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

/// [`TextFrontend::render`] on a fresh cursor and the default sheet.
pub fn render(tree: &RenderTree) -> String {
    TextFrontend::new().render(tree)
}

fn write_node(
    node: &RenderNode,
    indent: usize,
    focused: Option<&str>,
    sheet: &koine::style::StyleSheet,
    out: &mut Vec<String>,
) {
    let mut focus_mark = fill(sheet.value("focus"), &[]);
    if focus_mark.is_empty() {
        focus_mark = ">".to_string();
    }
    let focus_width = focus_mark.chars().count();
    let mark = if focused == Some(node.id.as_str()) {
        focus_mark
    } else {
        " ".repeat(focus_width)
    };
    let pad = " ".repeat(indent);
    out.push(format!("{mark}{pad}{}", line_of(node, sheet)));
    let child_indent = indent + indent_of(node, sheet);
    for child in children_of(node) {
        write_node(child, child_indent, focused, sheet, out);
    }
}

fn indent_of(node: &RenderNode, sheet: &koine::style::StyleSheet) -> usize {
    match &node.node {
        RenderKind::Column { .. } => spacing(sheet, "column"),
        RenderKind::Row { .. } => spacing(sheet, "row"),
        _ => 2,
    }
}

fn spacing(sheet: &koine::style::StyleSheet, slot: &str) -> usize {
    sheet.value(slot).parse().unwrap_or(0)
}

fn line_of(node: &RenderNode, sheet: &koine::style::StyleSheet) -> String {
    let label = node.label.as_deref().unwrap_or("");
    match &node.node {
        RenderKind::Column { .. } => "column".into(),
        RenderKind::Row { .. } => "row".into(),
        RenderKind::Grid { columns, .. } => format!("grid {columns}"),
        RenderKind::Section {
            title, collapsed, ..
        } => {
            let mut line = fill(
                sheet.value("section"),
                &[("title", title.as_str()), ("label", label)],
            );
            if *collapsed {
                line.push_str(" (collapsed)");
            }
            line
        }
        RenderKind::Tabs { tabs } => format!("tabs {}", tabs.len()),
        RenderKind::Text { text } => fill(
            sheet.value("text"),
            &[("text", text.as_str()), ("label", label)],
        ),
        RenderKind::Table { columns, .. } => {
            let joined = columns.join(",");
            fill(
                sheet.value("table"),
                &[("columns", joined.as_str()), ("label", label)],
            )
        }
        RenderKind::List { .. } => fill(sheet.value("list"), &[("label", label)]),
        RenderKind::Plot { .. } => "plot".into(),
        RenderKind::Image { .. } => "image".into(),
        RenderKind::Field { value, .. } => {
            let shown = value.to_string();
            fill(
                sheet.value("field"),
                &[("value", shown.as_str()), ("label", label)],
            )
        }
        RenderKind::Button {
            label: button_label,
        } => fill(sheet.value("button"), &[("label", button_label.as_str())]),
        RenderKind::Status { level, message } => {
            let slot = match level.as_str() {
                "error" => "status.error",
                "warning" => "status.warning",
                "info" => "status.info",
                _ => "",
            };
            if slot.is_empty() {
                format!("status {level}: {message}")
            } else {
                fill(
                    sheet.value(slot),
                    &[
                        ("level", level.as_str()),
                        ("message", message.as_str()),
                        ("label", label),
                    ],
                )
            }
        }
        RenderKind::Log { .. } => "log".into(),
        RenderKind::Kv { .. } => "kv".into(),
        RenderKind::Divider => "divider".into(),
        RenderKind::Spacer => "spacer".into(),
        RenderKind::Placeholder {
            unknown_kind,
            reason,
            ..
        } => {
            let kind = unknown_kind.as_deref().unwrap_or("placeholder");
            match reason {
                Some(reason) => format!("placeholder {kind}: {reason}"),
                None => format!("placeholder {kind}"),
            }
        }
    }
}

fn children_of(node: &RenderNode) -> Vec<&RenderNode> {
    match &node.node {
        RenderKind::Column { items }
        | RenderKind::Row { items }
        | RenderKind::Grid { items, .. } => items.iter().collect(),
        RenderKind::Section { body, .. } => vec![body.as_ref()],
        RenderKind::Tabs { tabs } => tabs.iter().map(|tab| &tab.body).collect(),
        _ => Vec::new(),
    }
}

/// Replace `{name}` from `vars`. A name with no value expands to empty.
fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(end) = after.find('}') {
            let name = &after[..end];
            if let Some((_, value)) = vars.iter().find(|(key, _)| *key == name) {
                out.push_str(value);
            }
            rest = &after[end + 1..];
        } else {
            out.push_str(&rest[start..]);
            return out;
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use koine::surface::{resolve, RenderKind, RenderNode};

    #[test]
    fn a_resolved_surface_renders_as_text_and_enter_names_the_focused_widget() {
        let spec = koine::surface::example_paper_triage();
        let tree = resolve(
            &spec,
            &serde_json::json!({}),
            &serde_json::json!({}),
            &serde_json::json!({}),
        );
        let mut frontend = TextFrontend::new();
        let text = frontend.render(&tree);
        assert!(
            text.contains("column")
                || text.contains("text")
                || text.contains("button")
                || text.contains("table")
                || text.contains("list"),
            "{text}"
        );
        assert!(text.lines().any(|line| line.starts_with('>')), "{text}");

        let first = frontend.focused_id(&tree).unwrap().to_string();
        assert!(frontend.key(&tree, Key::Down).is_none());
        if tree.focus_order.len() > 1 {
            assert_ne!(frontend.focused_id(&tree), Some(first.as_str()));
        }
        frontend.key(&tree, Key::Up);
        assert_eq!(frontend.focused_id(&tree), Some(first.as_str()));

        let event = frontend
            .key(&tree, Key::Activate)
            .expect("a focused widget");
        assert_eq!(event.widget, first);
        assert_eq!(event.kind, EventKind::Click);
    }

    #[test]
    fn a_direction_key_is_a_move_and_only_the_vertical_ones_walk_the_widgets() {
        use koine::gesture::{Element, Gesture};
        use koine::layout::Direction;

        let tree = two_buttons();
        let mut frontend = TextFrontend::new();
        assert_eq!(
            frontend.gesture_key(&tree, Key::Down),
            Some(Gesture::Move {
                direction: Direction::Down
            })
        );
        assert_eq!(frontend.focused_id(&tree), Some("b"));
        assert_eq!(
            frontend.gesture_key(&tree, Key::Left),
            Some(Gesture::Move {
                direction: Direction::Left
            })
        );
        assert_eq!(frontend.focused_id(&tree), Some("b"));
        assert!(frontend.key(&tree, Key::Left).is_none());
        assert_eq!(frontend.focused_id(&tree), Some("b"));

        assert_eq!(
            frontend.gesture_key(&tree, Key::Next),
            Some(Gesture::Move {
                direction: Direction::Next
            })
        );
        assert_eq!(frontend.focused_id(&tree), Some("a"));

        assert_eq!(
            frontend.gesture_key(&tree, Key::Activate),
            Some(Gesture::Activate {
                element: Element::Widget { id: "a".into() }
            })
        );
        assert_eq!(
            frontend.gesture_key(&tree, Key::Secondary),
            Some(Gesture::Secondary {
                element: Element::Widget { id: "a".into() }
            })
        );

        let mut verbs = TextFrontend::new();
        assert_eq!(
            verbs
                .layout_verb(&tree, Key::Down)
                .map(|verb| serde_json::to_value(verb).unwrap()),
            Some(serde_json::json!({"verb": "focus-direction", "direction": "down"}))
        );
        assert!(verbs.layout_verb(&tree, Key::Activate).is_none());

        let empty = RenderTree {
            root: two_buttons().root,
            focus_order: Vec::new(),
        };
        assert_eq!(
            frontend.gesture_key(&empty, Key::Up),
            Some(Gesture::Move {
                direction: Direction::Up
            })
        );
        assert!(frontend.gesture_key(&empty, Key::Activate).is_none());
        assert!(frontend.gesture_key(&empty, Key::Secondary).is_none());
    }

    #[test]
    fn the_default_sheet_spells_a_button_the_way_the_renderer_always_did() {
        let text = TextFrontend::new().render(&button_tree());
        assert_eq!(text, ">button Save");
    }

    #[test]
    fn a_button_template_changes_the_next_render_and_reset_restores_it() {
        let mut frontend = TextFrontend::new();
        frontend.set_style("button", "[{label}]").unwrap();
        let text = frontend.render(&button_tree());
        assert_eq!(text, ">[Save]");

        let changed = frontend
            .apply_editor(&Event {
                widget: "button".into(),
                kind: EventKind::Change,
                value: Value::String("({label})".into()),
            })
            .unwrap();
        assert!(changed);
        assert_eq!(frontend.render(&button_tree()), ">(Save)");

        frontend
            .apply_editor(&Event {
                widget: "reset:button".into(),
                kind: EventKind::Click,
                value: Value::Null,
            })
            .unwrap();
        assert_eq!(frontend.render(&button_tree()), ">button Save");
    }

    #[test]
    fn set_refuses_an_unknown_slot_and_a_spacing_that_is_not_an_integer() {
        let mut frontend = TextFrontend::new();
        assert!(frontend.set_style("grid", "2").is_err());
        assert!(frontend.set_style("column", "wide").is_err());
        assert_eq!(frontend.sheet().value("column"), "2");
        let text = frontend.render(&column_of_button());
        assert!(text.contains("\n>  button Save"), "{text}");
    }

    fn two_buttons() -> RenderTree {
        RenderTree {
            root: RenderNode {
                id: "c".into(),
                label: None,
                help: None,
                node: RenderKind::Column {
                    items: vec![
                        RenderNode {
                            id: "a".into(),
                            label: None,
                            help: None,
                            node: RenderKind::Button { label: "A".into() },
                        },
                        RenderNode {
                            id: "b".into(),
                            label: None,
                            help: None,
                            node: RenderKind::Button { label: "B".into() },
                        },
                    ],
                },
            },
            focus_order: vec!["a".into(), "b".into()],
        }
    }

    fn button_tree() -> RenderTree {
        RenderTree {
            root: RenderNode {
                id: "b".into(),
                label: None,
                help: None,
                node: RenderKind::Button {
                    label: "Save".into(),
                },
            },
            focus_order: vec!["b".into()],
        }
    }

    fn column_of_button() -> RenderTree {
        RenderTree {
            root: RenderNode {
                id: "c".into(),
                label: None,
                help: None,
                node: RenderKind::Column {
                    items: vec![RenderNode {
                        id: "b".into(),
                        label: None,
                        help: None,
                        node: RenderKind::Button {
                            label: "Save".into(),
                        },
                    }],
                },
            },
            focus_order: vec!["b".into()],
        }
    }
}
