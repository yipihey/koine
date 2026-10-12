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
//! and cannot do that. Off wasm there is no DOM to read, so the guard answers
//! "editable" and the shortcuts stay off: never steal typing is the safe
//! failure.
//!
//! Tab and Shift+Tab are not handled here at all. The DOM's tab order is the
//! tree's focus order (plan D2), so the browser moves focus; the page reads
//! the focused `data-koine-id` back afterwards ([`KeyAction::ReadBack`]).

use impress_layout::Verb;
use impress_surface::{Event, RenderKind, RenderNode, RenderTree};
use koine::cursor::{Cursor, Key};

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
}

impl KeyAction {
    /// Whether the page claims the key (calls `prevent_default`).
    pub fn claims(&self) -> bool {
        matches!(self, Self::Focus | Self::Event(_) | Self::Layout(_))
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
    match key {
        Key::Next | Key::Prev => KeyAction::ReadBack,
        Key::Down | Key::Up => {
            cursor.event_for(tree, key);
            KeyAction::Focus
        }
        Key::Activate => match cursor.event_for(tree, key) {
            Some(event) => KeyAction::Event(no_position(tree, event)),
            None => KeyAction::Ignored,
        },
        Key::Left | Key::Right if layout_keys => match cursor.layout_verb_for(tree, key) {
            Some(verb) => KeyAction::Layout(Box::new(verb)),
            None => KeyAction::Ignored,
        },
        Key::Left | Key::Right => KeyAction::Ignored,
        // The menu key names a widget, and the web has no menu yet.
        Key::Secondary => KeyAction::Ignored,
        // The row keys (plan-koine-outline O-D1) are Q2's to wire through
        // `Cursor::reply_for`.
        _ => KeyAction::Ignored,
    }
}

/// A keyboard `click` on a `plot` or an `image` has no pointer: it carries
/// `{"x": null, "y": null}` (plan-koine-tree-stream T-D4), the shape `koine-tui`
/// sends, so a handler reading `{{event.value.x}}` finds a null, not a missing
/// path. Any other event is as it was.
fn no_position(tree: &RenderTree, mut event: Event) -> Event {
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
    if event.kind == impress_surface::EventKind::Click
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
    use impress_surface::{EventKind, RenderKind, RenderNode};

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
