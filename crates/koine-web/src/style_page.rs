//! `/style`: the style editor, drawn by the frontend it edits.
//!
//! `koine::style::editor_spec("web")` is a surface like any other. The page
//! resolves it against the shared sheet, draws it with `koine-dioxus`, and an
//! event goes through `editor_apply` on the shared `StyleHost`. The surface
//! page reads the same host, so a slot edited here is visible on
//! `/surface/:id` the next time it paints. Nothing is persisted (plan D12): a
//! reload returns to the defaults. The style file is written by
//! `koine-style-file`, natively.

use dioxus::prelude::*;
use impress_surface::{resolve, Event};
use koine::cursor::Cursor;
use koine::style::{editor_apply, editor_spec, editor_state};
use koine_dioxus::Surface;
use serde_json::json;

use crate::api::ApiError;
use crate::dom;
use crate::keys::{self, KeyAction};
use crate::session::Session;

#[component]
pub fn StylePage() -> Element {
    let session = use_context::<Session>();
    let mut style = session.style;
    let spec = use_hook(|| editor_spec("web").expect("web is a koine frontend"));
    let tree = use_memo(move || {
        resolve(
            &spec,
            &editor_state(style.read().sheet()),
            &json!({}),
            &json!({}),
        )
    });
    let sheet = use_memo(move || style.read().sheet().clone());
    let mut cursor = use_signal(Cursor::new);
    let mut focused = use_signal(|| None::<String>);

    // Show the cursor's widget as focused, once the tree has any.
    use_effect(move || {
        let tree = tree.read();
        if focused.peek().is_none() {
            let first = cursor.peek().focused_id(&tree).map(str::to_string);
            focused.set(first);
        }
    });

    let mut apply = move |event: Event| {
        let mut host = style.peek().clone();
        match editor_apply(&mut host, &event) {
            Ok(true) => {
                style.set(host);
                session.clear();
            }
            Ok(false) => {}
            Err(error) => session.refuse(ApiError::new("invalid-value", error.to_string())),
        }
    };

    let on_event = move |event: Event| {
        cursor.write().set_focus(&tree.peek(), &event.widget);
        focused.set(Some(event.widget.clone()));
        apply(event);
    };

    let on_keydown = move |event: dioxus::events::KeyboardEvent| {
        let action = {
            let tree = tree.peek();
            let mut cursor = cursor.write();
            keys::on_keydown(&event, &tree, &mut cursor, false)
        };
        match action {
            KeyAction::Focus | KeyAction::Event(_) => {
                let id = cursor.peek().focused_id(&tree.peek()).map(str::to_string);
                focused.set(id.clone());
                if let Some(id) = id {
                    spawn(async move { dom::focus_widget(&id).await });
                }
                if let KeyAction::Event(event) = action {
                    apply(event);
                }
            }
            KeyAction::ReadBack => {
                spawn(async move {
                    if let Some(id) = dom::focused_widget().await {
                        if cursor.write().set_focus(&tree.peek(), &id) {
                            focused.set(Some(id));
                        }
                    }
                });
            }
            KeyAction::Layout(_) | KeyAction::Ignored => {}
        }
    };

    rsx! {
        h1 { "Style" }
        p { class: "kw-note",
            "The web sheet, edited with the web frontend. "
            "Nothing is saved: a reload returns to the defaults. "
            "Open a surface in another tab after an edit and it is drawn with it."
        }
        div { class: "kw-surface", tabindex: "0", onkeydown: on_keydown,
            Surface { tree, sheet, focused, on_event }
        }
    }
}
