//! Keys: the browser's `keydown` through `koine::cursor`.
//!
//! [`decide`] is the whole of the rule and is pure: a key name, Shift, a
//! cursor and a tree give what the page should do. The page does it
//! ([`KeyAction`]) and decides nothing else, so `j j Enter` here and in
//! `koine-tui` are the same calls into `koine::cursor` (plan D4).
//!
//! # The editable-element guard
//!
//! A shortcut never steals typing (the `ImpressKeyboard` rule on the Mac): a
//! key pressed while an `input`, `textarea`, `select` or content-editable
//! element has focus is not ours. [`active_element_is_editable`] reads
//! `document.activeElement` through `web-sys`, synchronously, because the
//! handler must decide `prevent_default` before it returns (an Enter that is
//! ours must not also click the focused button). `document::eval` is async
//! and cannot do that. Off wasm there is no DOM to read synchronously (a
//! `desktop` or `mobile` WebView is driven through `document::eval`, never
//! `web-sys`), so the guard answers "editable" and the character shortcuts
//! stay off: never steal typing is the safe failure. In a WebView the touch
//! and pointer paths are the page's input; Tab (the WebView moves
//! focus, and the page reads it back) and Enter on a focused button (the
//! WebView clicks it) still work from a hardware keyboard.
//!
//! Tab and Shift+Tab are not handled here at all. The DOM's tab order is the
//! tree's focus order (plan D2), so the browser moves focus; the page reads
//! the focused `data-koine-id` back afterwards ([`KeyAction::ReadBack`]).

use impress_layout::Verb;
use impress_surface::{Event, EventKind, RenderKind, RenderNode, RenderTree};
use koine::cursor::{Cursor, Key, Reply};
use koine::outline::RowMenu;
use serde_json::Value;
use std::collections::BTreeMap;

/// What the page does about one `keydown`.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyAction {
    /// Not ours: leave the event alone.
    Ignored,
    /// The browser moves focus; read it back into the cursor afterwards.
    ReadBack,
    /// The cursor moved, or has nothing to send: show it. The event is ours.
    Focus,
    /// Send this surface event. The cursor is on its widget.
    Event(Event),
    /// Post this layout verb (`h` and `l`, the arrows left and right).
    Layout(Box<Verb>),
    /// `r`: rename this row of this widget (an input in place of its label).
    /// The widget id and the row id, from the cursor's active row.
    Rename(String, Value),
    /// `m` or the menu key: open this row's menu. Only for a widget that
    /// declares `actions`.
    Menu(String, Value),
    /// `x`: remember this row as the one to move (only in a movable tree).
    Cut(String, Value),
    /// `p`: move the cut row under this one.
    Paste(String, Value),
    /// `P`: move the cut row to just after this one.
    PasteBeside(String, Value),
}

impl KeyAction {
    /// Whether the page claims the key (calls `prevent_default`).
    pub fn claims(&self) -> bool {
        !matches!(self, Self::Ignored | Self::ReadBack)
    }
}

/// The action for one key. `name` is `KeyboardEvent.key`; `shift` is Shift.
/// A key held with Control, Alt or Meta is never ours, so the caller does not
/// call this for one.
///
/// `layout_keys` says whether `h` and `l` mean a layout verb on this page. The
/// surface page says yes; the style page has no panes and says no.
pub fn decide(
    tree: &RenderTree,
    cursor: &mut Cursor,
    name: &str,
    shift: bool,
    layout_keys: bool,
) -> KeyAction {
    let key = if name == "Tab" && shift {
        Some(Key::Prev)
    } else {
        Key::from_name(name)
    };
    let Some(key) = key else {
        return KeyAction::Ignored;
    };
    if matches!(key, Key::Next | Key::Prev) {
        return KeyAction::ReadBack;
    }
    match cursor.reply_for(tree, key) {
        Reply::Event(event) => KeyAction::Event(no_position(tree, event)),
        Reply::Moved => KeyAction::Focus,
        Reply::Unhandled => match key {
            Key::Left | Key::Right if layout_keys => match cursor.layout_verb_for(tree, key) {
                Some(verb) => KeyAction::Layout(Box::new(verb)),
                None => KeyAction::Ignored,
            },
            // The renderer's keys (plan-koine-outline O-D1): each acts on the
            // active row of the focused widget, and means nothing without one.
            Key::Rename | Key::Secondary | Key::Menu | Key::Cut | Key::Paste | Key::PasteBeside => {
                row_action(tree, cursor, key)
            }
            _ => KeyAction::Ignored,
        },
    }
}

/// The action for one of the renderer's row keys: the focused widget's active
/// row, if the widget can do what the key asks (rename and cut are a tree's;
/// a menu needs an entry, `koine::outline::menu_entries`; paste needs a
/// movable tree).
fn row_action(tree: &RenderTree, cursor: &Cursor, key: Key) -> KeyAction {
    let Some(widget) = cursor.focused_id(tree) else {
        return KeyAction::Ignored;
    };
    let Some(row) = cursor.active_row(tree, widget).cloned() else {
        return KeyAction::Ignored;
    };
    let Some(node) = find(&tree.root, widget) else {
        return KeyAction::Ignored;
    };
    let widget = widget.to_string();
    match (&node.node, key) {
        (RenderKind::Tree { .. }, Key::Rename) => KeyAction::Rename(widget, row),
        (kind, Key::Secondary | Key::Menu)
            if RowMenu::of(kind).is_some_and(|m| !m.entries().is_empty()) =>
        {
            KeyAction::Menu(widget, row)
        }
        (RenderKind::Tree { movable: true, .. }, Key::Cut) => KeyAction::Cut(widget, row),
        (RenderKind::Tree { movable: true, .. }, Key::Paste) => KeyAction::Paste(widget, row),
        (RenderKind::Tree { movable: true, .. }, Key::PasteBeside) => {
            KeyAction::PasteBeside(widget, row)
        }
        _ => KeyAction::Ignored,
    }
}

/// The row menu of a `tree`, `table` or `list` widget, whose `entries()`
/// are the open menu's list (`koine::outline::menu_entries`: the digit keys
/// index into it) and whose `choice(i)` is what picking one means; an empty
/// menu for any other widget or an unknown id.
pub fn menu_of(tree: &RenderTree, widget: &str) -> RowMenu {
    find(&tree.root, widget)
        .and_then(|node| RowMenu::of(&node.node))
        .unwrap_or_default()
}

/// The active row of every row widget in the focus order, by widget id: what
/// the mapping needs to mark them. A widget without an active row is absent.
pub fn active_rows(tree: &RenderTree, cursor: &Cursor) -> BTreeMap<String, Value> {
    tree.focus_order
        .iter()
        .filter_map(|id| Some((id.clone(), cursor.active_row(tree, id)?.clone())))
        .collect()
}

/// The row a `select` event names, when the event is a row click: a `select`
/// on a `tree`, `table` or `list` carrying `[id]`. The page makes that row
/// the widget's active row, so a click and `j`/`k` share one cursor.
pub fn selected_row(tree: &RenderTree, event: &Event) -> Option<Value> {
    if event.kind != EventKind::Select {
        return None;
    }
    let is_row_widget = matches!(
        find(&tree.root, &event.widget).map(|node| &node.node),
        Some(RenderKind::Tree { .. } | RenderKind::Table { .. } | RenderKind::List { .. })
    );
    match &event.value {
        Value::Array(ids) if is_row_widget && ids.len() == 1 => Some(ids[0].clone()),
        _ => None,
    }
}

/// The `move` event a paste sends: the row cut earlier goes under (`p`) or
/// right after (`P`) the active row of the same tree. `None` when the key is
/// not a paste, the cut row is from another widget, the tree is gone, or the
/// move would put a branch inside itself.
pub fn paste_event(tree: &RenderTree, cut: &(String, Value), action: &KeyAction) -> Option<Event> {
    let (widget, active, beside) = match action {
        KeyAction::Paste(widget, active) => (widget, active, false),
        KeyAction::PasteBeside(widget, active) => (widget, active, true),
        _ => return None,
    };
    if &cut.0 != widget {
        return None;
    }
    let Some(RenderKind::Tree { nodes, .. }) = find(&tree.root, widget).map(|node| &node.node)
    else {
        return None;
    };
    let value = if beside {
        koine_dioxus::outline::paste_beside(nodes, &cut.1, active)?
    } else {
        koine_dioxus::outline::paste_into(nodes, &cut.1, active)?
    };
    Some(Event {
        widget: widget.clone(),
        kind: EventKind::Move,
        value,
    })
}

/// The node with this id, anywhere under `node`.
fn find<'a>(node: &'a RenderNode, id: &str) -> Option<&'a RenderNode> {
    if node.id == id {
        return Some(node);
    }
    let children: Vec<&RenderNode> = match &node.node {
        RenderKind::Column { items }
        | RenderKind::Row { items }
        | RenderKind::Grid { items, .. } => items.iter().collect(),
        RenderKind::Section { body, .. } => vec![body.as_ref()],
        RenderKind::Tabs { tabs } => tabs.iter().map(|t| &t.body).collect(),
        _ => Vec::new(),
    };
    children.into_iter().find_map(|child| find(child, id))
}

/// A keyboard `click` on a `plot` or an `image` has no pointer: it carries
/// `{"x": null, "y": null}` (plan-koine-tree-stream T-D4), the shape `koine-tui`
/// sends, so a handler reading `{{event.value.x}}` finds a null, not a missing
/// path. Any other event is as it was.
fn no_position(tree: &RenderTree, mut event: Event) -> Event {
    if event.kind == EventKind::Click
        && matches!(
            find(&tree.root, &event.widget).map(|node| &node.node),
            Some(RenderKind::Plot { .. } | RenderKind::Image { .. })
        )
    {
        event.value = serde_json::json!({"x": null, "y": null});
    }
    event
}

/// A tree with nothing in it: what a page without a surface asks [`decide`]
/// about, and what the surface page holds before its first render answers.
pub fn empty_tree() -> RenderTree {
    RenderTree {
        root: RenderNode {
            id: String::new(),
            label: None,
            help: None,
            node: RenderKind::Column { items: Vec::new() },
        },
        focus_order: Vec::new(),
    }
}

/// The layout verb one key names on a page of panes: `h` and `l`, and the left
/// and right arrows. The rule is [`decide`]'s; every other key is `None`.
pub fn layout_verb_for_key(name: &str, shift: bool) -> Option<Verb> {
    match decide(&empty_tree(), &mut Cursor::new(), name, shift, true) {
        KeyAction::Layout(verb) => Some(*verb),
        _ => None,
    }
}

/// [`on_keydown`] for a page with panes and no surface of its own (the layout
/// page): the same guards, and only a layout verb is claimed. `j`, `k` and the
/// vertical arrows stay with the browser (they scroll), or with a surface pane
/// inside, which handled them before the event bubbled here.
pub fn on_layout_keydown(event: &dioxus::events::KeyboardEvent) -> Option<Verb> {
    use dioxus::prelude::{Modifiers, ModifiersInteraction};
    let modifiers = event.modifiers();
    if modifiers.intersects(Modifiers::CONTROL | Modifiers::ALT | Modifiers::META)
        || active_element_is_editable()
    {
        return None;
    }
    let verb = layout_verb_for_key(
        &event.key().to_string(),
        modifiers.contains(Modifiers::SHIFT),
    )?;
    event.prevent_default();
    Some(verb)
}

/// What one `keydown` on a page root means, with the DOM guard applied.
///
/// A key with Control, Alt or Meta held is never ours. Tab is the browser's
/// whatever has focus. Any other key is ignored while an editable element has
/// focus. A key the page claims has its default prevented here, before the
/// handler returns.
pub fn on_keydown(
    event: &dioxus::events::KeyboardEvent,
    tree: &RenderTree,
    cursor: &mut Cursor,
    layout_keys: bool,
) -> KeyAction {
    use dioxus::prelude::{Modifiers, ModifiersInteraction};
    let modifiers = event.modifiers();
    if modifiers.intersects(Modifiers::CONTROL | Modifiers::ALT | Modifiers::META) {
        return KeyAction::Ignored;
    }
    let name = event.key().to_string();
    let shift = modifiers.contains(Modifiers::SHIFT);
    if name != "Tab" && active_element_is_editable() {
        return KeyAction::Ignored;
    }
    let action = decide(tree, cursor, &name, shift, layout_keys);
    if action.claims() {
        event.prevent_default();
    }
    action
}

/// Whether an element of this shape takes typing. `input_type` is the
/// `type` attribute of an `input`, lower-cased. Only the browser has
/// elements; native builds compile it for the tests.
#[cfg(any(target_arch = "wasm32", test))]
pub fn is_editable(tag: &str, input_type: &str, content_editable: bool) -> bool {
    if content_editable {
        return true;
    }
    match tag.to_ascii_lowercase().as_str() {
        "textarea" | "select" => true,
        "input" => !matches!(
            input_type,
            "button" | "checkbox" | "radio" | "submit" | "reset" | "image" | "file" | "color"
        ),
        _ => false,
    }
}

/// Whether the focused element in the DOM takes typing.
#[cfg(target_arch = "wasm32")]
pub fn active_element_is_editable() -> bool {
    let Some(active) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.active_element())
    else {
        return false;
    };
    let tag = active.tag_name();
    let input_type = active
        .get_attribute("type")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let editable = content_editable(&active);
    is_editable(&tag, &input_type, editable)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn active_element_is_editable() -> bool {
    true
}

/// `contenteditable="true"` (or `""`, or `plaintext-only`) on the element
/// itself: the spellings a surface's own elements use.
#[cfg(target_arch = "wasm32")]
fn content_editable(element: &web_sys::Element) -> bool {
    matches!(
        element.get_attribute("contenteditable").as_deref(),
        Some("") | Some("true") | Some("plaintext-only")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use impress_surface::{RenderKind, RenderNode};

    fn button(id: &str) -> RenderNode {
        RenderNode {
            id: id.into(),
            label: None,
            help: None,
            node: RenderKind::Button { label: id.into() },
        }
    }

    fn tree() -> RenderTree {
        RenderTree {
            root: RenderNode {
                id: "c".into(),
                label: None,
                help: None,
                node: RenderKind::Column {
                    items: vec![button("a"), button("b"), button("c2")],
                },
            },
            focus_order: vec!["a".into(), "b".into(), "c2".into()],
        }
    }

    #[test]
    fn j_j_enter_sends_a_click_on_the_third_widget() {
        let tree = tree();
        let mut cursor = Cursor::new();
        assert_eq!(
            decide(&tree, &mut cursor, "j", false, true),
            KeyAction::Focus
        );
        assert_eq!(
            decide(&tree, &mut cursor, "j", false, true),
            KeyAction::Focus
        );
        assert_eq!(cursor.focused_id(&tree), Some("c2"));
        let KeyAction::Event(event) = decide(&tree, &mut cursor, "Enter", false, true) else {
            panic!("Enter on a focused widget is an event");
        };
        assert_eq!(event.widget, "c2");
        assert_eq!(event.kind, EventKind::Click);
    }

    #[test]
    fn enter_on_a_plot_or_an_image_clicks_with_no_position() {
        for node in [
            RenderKind::Plot {
                spec: serde_json::json!({"series": []}),
            },
            RenderKind::Image {
                blob: None,
                url: None,
            },
        ] {
            let tree = RenderTree {
                root: RenderNode {
                    id: "c".into(),
                    label: None,
                    help: None,
                    node: RenderKind::Column {
                        items: vec![RenderNode {
                            id: "w".into(),
                            label: None,
                            help: None,
                            node,
                        }],
                    },
                },
                focus_order: vec!["w".into()],
            };
            let mut cursor = Cursor::new();
            decide(&tree, &mut cursor, "j", false, true);
            let KeyAction::Event(event) = decide(&tree, &mut cursor, "Enter", false, true) else {
                panic!("Enter on a focused plot is an event");
            };
            assert_eq!(event.widget, "w");
            assert_eq!(event.kind, EventKind::Click);
            assert_eq!(event.value, serde_json::json!({"x": null, "y": null}));
        }
    }

    #[test]
    fn k_and_the_arrows_walk_the_same_order() {
        let tree = tree();
        let mut cursor = Cursor::new();
        decide(&tree, &mut cursor, "ArrowDown", false, true);
        assert_eq!(cursor.focused_id(&tree), Some("b"));
        decide(&tree, &mut cursor, "k", false, true);
        assert_eq!(cursor.focused_id(&tree), Some("a"));
        decide(&tree, &mut cursor, "ArrowUp", false, true);
        assert_eq!(cursor.focused_id(&tree), Some("c2"));
    }

    #[test]
    fn tab_and_shift_tab_are_the_browsers_and_read_back() {
        let tree = tree();
        let mut cursor = Cursor::new();
        assert_eq!(
            decide(&tree, &mut cursor, "Tab", false, true),
            KeyAction::ReadBack
        );
        assert_eq!(
            decide(&tree, &mut cursor, "Tab", true, true),
            KeyAction::ReadBack
        );
        assert_eq!(cursor.focused_id(&tree), Some("a"));
        assert!(!KeyAction::ReadBack.claims());
    }

    #[test]
    fn h_and_l_are_layout_verbs_and_leave_the_surface_cursor() {
        let tree = tree();
        let mut cursor = Cursor::new();
        let KeyAction::Layout(verb) = decide(&tree, &mut cursor, "h", false, true) else {
            panic!("h is a layout verb");
        };
        assert_eq!(
            serde_json::to_value(*verb).unwrap(),
            serde_json::json!({"verb": "focus-direction", "direction": "left"})
        );
        let KeyAction::Layout(verb) = decide(&tree, &mut cursor, "ArrowRight", false, true) else {
            panic!("ArrowRight is a layout verb");
        };
        assert_eq!(
            serde_json::to_value(*verb).unwrap(),
            serde_json::json!({"verb": "focus-direction", "direction": "right"})
        );
        assert_eq!(cursor.focused_id(&tree), Some("a"));
    }

    #[test]
    fn the_layout_page_claims_h_and_l_and_nothing_else() {
        assert_eq!(
            layout_verb_for_key("l", false).map(|verb| serde_json::to_value(verb).unwrap()),
            Some(serde_json::json!({"verb": "focus-direction", "direction": "right"}))
        );
        assert!(layout_verb_for_key("ArrowLeft", false).is_some());
        for other in ["j", "k", "ArrowDown", "Enter", "Tab", "x"] {
            assert!(layout_verb_for_key(other, false).is_none(), "{other}");
        }
    }

    #[test]
    fn a_page_without_panes_does_not_claim_h_and_l() {
        let tree = tree();
        let mut cursor = Cursor::new();
        assert_eq!(
            decide(&tree, &mut cursor, "h", false, false),
            KeyAction::Ignored
        );
        assert_eq!(
            decide(&tree, &mut cursor, "ArrowRight", false, false),
            KeyAction::Ignored
        );
        assert_eq!(
            decide(&tree, &mut cursor, "j", false, false),
            KeyAction::Focus
        );
    }

    #[test]
    fn unmapped_keys_the_menu_key_and_an_empty_tree_are_ignored() {
        let tree = tree();
        let mut cursor = Cursor::new();
        for name in ["", "J", "Escape", "Shift", " ", "x", "m"] {
            assert_eq!(
                decide(&tree, &mut cursor, name, false, true),
                KeyAction::Ignored,
                "{name:?}"
            );
        }
        let mut empty = tree.clone();
        empty.focus_order.clear();
        assert_eq!(
            decide(&empty, &mut cursor, "Enter", false, true),
            KeyAction::Ignored
        );
        assert!(!KeyAction::Ignored.claims());
    }

    // ── the outline keys (plan-koine-outline) ─────────────────────────────

    /// A movable tree with a menu, a table with one, a list without, and a
    /// button, all in the focus order.
    fn outline() -> RenderTree {
        serde_json::from_value(serde_json::json!({
            "root": { "id": "root", "node": { "kind": "column", "items": [
                { "id": "side", "node": { "kind": "tree", "bind": "state.open",
                    "expanded": ["a"], "actions": ["Rename", "Delete"], "movable": true,
                    "nodes": [
                        { "id": "a", "label": "src", "depth": 0, "has_children": true, "expanded": true, "row": {} },
                        { "id": "b", "label": "main.rs", "depth": 1, "has_children": false, "expanded": false, "row": {} },
                        { "id": "c", "label": "docs", "depth": 0, "has_children": false, "expanded": false, "row": {} }
                    ]}},
                { "id": "ro", "node": { "kind": "tree", "nodes": [
                        { "id": "r1", "label": "read-only", "depth": 0, "has_children": false, "expanded": false, "row": {} }
                    ]}},
                { "id": "tbl", "node": { "kind": "table", "columns": ["title"], "actions": ["Open"],
                    "rows": [{ "id": "p1", "title": "A" }, { "id": "p2", "title": "B" }]}},
                { "id": "lst", "node": { "kind": "list", "rows": ["x", "y"] } },
                { "id": "go", "node": { "kind": "button", "label": "Go" } }
            ]}},
            "focus_order": ["side", "ro", "tbl", "lst", "go"]
        }))
        .unwrap()
    }

    fn press(tree: &RenderTree, cursor: &mut Cursor, names: &[&str]) -> KeyAction {
        let mut last = KeyAction::Ignored;
        for name in names {
            last = decide(tree, cursor, name, *name == "P", true);
        }
        last
    }

    #[test]
    fn j_and_k_move_the_active_row_inside_a_tree_and_the_page_redraws() {
        let tree = outline();
        let mut cursor = Cursor::new();
        assert_eq!(press(&tree, &mut cursor, &["j"]), KeyAction::Focus);
        assert_eq!(
            cursor.active_row(&tree, "side"),
            Some(&serde_json::json!("a"))
        );
        press(&tree, &mut cursor, &["j", "j", "j"]);
        assert_eq!(
            cursor.active_row(&tree, "side"),
            Some(&serde_json::json!("c"))
        );
        press(&tree, &mut cursor, &["k"]);
        assert_eq!(
            cursor.active_row(&tree, "side"),
            Some(&serde_json::json!("b"))
        );
        // The widget focus did not leave the tree.
        assert_eq!(cursor.focused_id(&tree), Some("side"));
        assert_eq!(
            active_rows(&tree, &cursor),
            BTreeMap::from([("side".to_string(), serde_json::json!("b"))])
        );
    }

    #[test]
    fn enter_space_d_and_o_send_row_events_for_the_active_row() {
        let tree = outline();
        let mut cursor = Cursor::new();
        press(&tree, &mut cursor, &["j"]);
        let cases = [
            ("Enter", EventKind::Select, serde_json::json!(["a"])),
            ("d", EventKind::Delete, serde_json::json!(["a"])),
            ("o", EventKind::Activate, serde_json::json!(["a"])),
            // Collapsing `a` sends the whole list without it.
            (" ", EventKind::Change, serde_json::json!([])),
        ];
        for (name, kind, value) in cases {
            let KeyAction::Event(event) = press(&tree, &mut cursor, &[name]) else {
                panic!("{name} is an event");
            };
            assert_eq!(
                (event.widget.as_str(), event.kind, event.value),
                ("side", kind, value),
                "{name}"
            );
        }
    }

    #[test]
    fn h_inside_a_tree_moves_to_the_parent_and_at_a_root_it_is_the_layout_verb() {
        let tree = outline();
        let mut cursor = Cursor::new();
        press(&tree, &mut cursor, &["j", "j"]);
        assert_eq!(press(&tree, &mut cursor, &["h"]), KeyAction::Focus);
        assert_eq!(
            cursor.active_row(&tree, "side"),
            Some(&serde_json::json!("a"))
        );
        // `a` is an expanded branch: h collapses it (an event)...
        assert!(matches!(
            press(&tree, &mut cursor, &["h"]),
            KeyAction::Event(_)
        ));
        // ...and on a leaf root with nothing to do, h falls through.
        press(&tree, &mut cursor, &["j", "j"]);
        assert!(matches!(
            press(&tree, &mut cursor, &["h"]),
            KeyAction::Layout(_)
        ));
        // Without layout keys the fall-through is ignored.
        assert_eq!(
            decide(&tree, &mut cursor, "h", false, false),
            KeyAction::Ignored
        );
    }

    #[test]
    fn r_renames_the_active_row_of_a_tree_and_nothing_else() {
        let tree = outline();
        let mut cursor = Cursor::new();
        // No active row yet.
        assert_eq!(press(&tree, &mut cursor, &["r"]), KeyAction::Ignored);
        press(&tree, &mut cursor, &["j", "j"]);
        assert_eq!(
            press(&tree, &mut cursor, &["r"]),
            KeyAction::Rename("side".into(), serde_json::json!("b"))
        );
        assert!(KeyAction::Rename("side".into(), serde_json::json!("b")).claims());
        // A table's rows are not renamed here.
        cursor.set_focus(&tree, "tbl");
        press(&tree, &mut cursor, &["j"]);
        assert_eq!(press(&tree, &mut cursor, &["r"]), KeyAction::Ignored);
    }

    #[test]
    fn m_and_the_menu_key_open_the_menu_of_a_widget_that_has_actions() {
        let tree = outline();
        let mut cursor = Cursor::new();
        assert_eq!(
            press(&tree, &mut cursor, &["m"]),
            KeyAction::Ignored,
            "no row"
        );
        press(&tree, &mut cursor, &["j"]);
        let menu = KeyAction::Menu("side".into(), serde_json::json!("a"));
        assert_eq!(press(&tree, &mut cursor, &["m"]), menu);
        assert_eq!(press(&tree, &mut cursor, &["ContextMenu"]), menu);
        assert!(menu.claims());
        // A table with actions, a list and a read-only tree.
        cursor.set_focus(&tree, "tbl");
        press(&tree, &mut cursor, &["j"]);
        assert_eq!(
            press(&tree, &mut cursor, &["m"]),
            KeyAction::Menu("tbl".into(), serde_json::json!("p1"))
        );
        for widget in ["ro", "lst"] {
            cursor.set_focus(&tree, widget);
            press(&tree, &mut cursor, &["j"]);
            assert_eq!(
                press(&tree, &mut cursor, &["m"]),
                KeyAction::Ignored,
                "{widget}"
            );
        }
    }

    #[test]
    fn x_p_and_capital_p_are_a_movable_trees_and_nothing_elses() {
        let tree = outline();
        let mut cursor = Cursor::new();
        assert_eq!(
            press(&tree, &mut cursor, &["x"]),
            KeyAction::Ignored,
            "no row"
        );
        press(&tree, &mut cursor, &["j", "j"]);
        assert_eq!(
            press(&tree, &mut cursor, &["x"]),
            KeyAction::Cut("side".into(), serde_json::json!("b"))
        );
        assert_eq!(
            press(&tree, &mut cursor, &["p"]),
            KeyAction::Paste("side".into(), serde_json::json!("b"))
        );
        assert_eq!(
            press(&tree, &mut cursor, &["P"]),
            KeyAction::PasteBeside("side".into(), serde_json::json!("b"))
        );
        // A tree without on_move, a table, a list.
        for widget in ["ro", "tbl", "lst"] {
            cursor.set_focus(&tree, widget);
            press(&tree, &mut cursor, &["j"]);
            for key in ["x", "p", "P"] {
                assert_eq!(
                    press(&tree, &mut cursor, &[key]),
                    KeyAction::Ignored,
                    "{widget} {key}"
                );
            }
        }
    }

    #[test]
    fn a_paste_is_a_move_under_or_beside_the_active_row() {
        let tree = outline();
        let cut = ("side".to_string(), serde_json::json!("c"));
        let into = paste_event(
            &tree,
            &cut,
            &KeyAction::Paste("side".into(), serde_json::json!("a")),
        )
        .unwrap();
        assert_eq!((into.widget.as_str(), into.kind), ("side", EventKind::Move));
        assert_eq!(
            into.value,
            serde_json::json!({"ids": ["c"], "to": "a", "index": 0})
        );
        // Beside `b` (the first child of `a`): second child of `a`.
        let beside = paste_event(
            &tree,
            &cut,
            &KeyAction::PasteBeside("side".into(), serde_json::json!("b")),
        )
        .unwrap();
        assert_eq!(
            beside.value,
            serde_json::json!({"ids": ["c"], "to": "a", "index": 1})
        );
        // Beside a root: the roots, after it.
        let beside = paste_event(
            &tree,
            &cut,
            &KeyAction::PasteBeside("side".into(), serde_json::json!("a")),
        )
        .unwrap();
        assert_eq!(
            beside.value,
            serde_json::json!({"ids": ["c"], "to": null, "index": 1})
        );
        // A branch into itself, a cut from another widget, a key that is not
        // a paste.
        let branch = ("side".to_string(), serde_json::json!("a"));
        assert!(paste_event(
            &tree,
            &branch,
            &KeyAction::Paste("side".into(), serde_json::json!("b"))
        )
        .is_none());
        let other = ("ro".to_string(), serde_json::json!("c"));
        assert!(paste_event(
            &tree,
            &other,
            &KeyAction::Paste("side".into(), serde_json::json!("a"))
        )
        .is_none());
        assert!(paste_event(&tree, &cut, &KeyAction::Focus).is_none());
    }

    #[test]
    fn a_click_on_a_row_names_the_row_and_nothing_else_does() {
        let tree = outline();
        let select = |widget: &str, value: serde_json::Value| Event {
            widget: widget.into(),
            kind: EventKind::Select,
            value,
        };
        assert_eq!(
            selected_row(&tree, &select("side", serde_json::json!(["b"]))),
            Some(serde_json::json!("b"))
        );
        assert_eq!(
            selected_row(&tree, &select("tbl", serde_json::json!(["p2"]))),
            Some(serde_json::json!("p2"))
        );
        // Not a row widget, not a one-id list, not a select.
        assert_eq!(
            selected_row(&tree, &select("go", serde_json::json!(["b"]))),
            None
        );
        assert_eq!(
            selected_row(&tree, &select("side", serde_json::json!(2))),
            None
        );
        assert_eq!(
            selected_row(&tree, &select("side", serde_json::json!(["a", "b"]))),
            None
        );
        let change = Event {
            kind: EventKind::Change,
            ..select("side", serde_json::json!(["b"]))
        };
        assert_eq!(selected_row(&tree, &change), None);
    }

    #[test]
    fn the_menu_entries_come_from_the_widget() {
        let tree = outline();
        assert_eq!(menu_of(&tree, "side").entries(), vec!["Rename", "Delete"]);
        assert_eq!(menu_of(&tree, "tbl").entries(), vec!["Open"]);
        assert!(menu_of(&tree, "lst").entries().is_empty());
        assert!(menu_of(&tree, "go").entries().is_empty());
        assert!(menu_of(&tree, "missing").entries().is_empty());
    }

    #[test]
    fn a_tree_that_only_renames_and_deletes_has_a_menu_and_its_digits_index_the_entries() {
        use koine::outline::MenuChoice;
        let mut tree = outline();
        if let RenderKind::Column { items } = &mut tree.root.node {
            if let RenderKind::Tree {
                renameable,
                deletable,
                ..
            } = &mut items[1].node
            {
                *renameable = true;
                *deletable = true;
            }
        }
        let menu = menu_of(&tree, "ro");
        assert_eq!(menu.entries(), vec!["Rename", "Delete"]);
        assert_eq!(menu.choice(0), Some(MenuChoice::Rename));
        assert_eq!(menu.choice(1), Some(MenuChoice::Delete));
        let mut cursor = Cursor::new();
        cursor.set_focus(&tree, "ro");
        press(&tree, &mut cursor, &["j"]);
        assert_eq!(
            press(&tree, &mut cursor, &["m"]),
            KeyAction::Menu("ro".into(), serde_json::json!("r1"))
        );
    }

    #[test]
    fn typing_surfaces_are_editable_and_buttons_are_not() {
        assert!(is_editable("INPUT", "text", false));
        assert!(is_editable("input", "", false));
        assert!(is_editable("input", "date", false));
        assert!(is_editable("input", "range", false));
        assert!(is_editable("TEXTAREA", "", false));
        assert!(is_editable("SELECT", "", false));
        assert!(is_editable("DIV", "", true));
        assert!(!is_editable("INPUT", "checkbox", false));
        assert!(!is_editable("INPUT", "button", false));
        assert!(!is_editable("BUTTON", "", false));
        assert!(!is_editable("DIV", "", false));
    }
}
