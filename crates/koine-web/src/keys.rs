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
use impress_surface::{Event, RenderTree};
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
            Some(event) => KeyAction::Event(event),
            None => KeyAction::Ignored,
        },
        Key::Left | Key::Right if layout_keys => match cursor.layout_verb_for(tree, key) {
            Some(verb) => KeyAction::Layout(Box::new(verb)),
            None => KeyAction::Ignored,
        },
        Key::Left | Key::Right => KeyAction::Ignored,
        // The menu key names a widget, and the web has no menu yet.
        Key::Secondary => KeyAction::Ignored,
    }
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
