//! A Dioxus performance of a [`RenderTree`].
//!
//! [`Surface`] is the whole of it: a tree, a web [`StyleSheet`] and the focused
//! widget id become an [`Element`]; every interaction becomes an
//! [`impress_surface::Event`] handed to `on_event`. The mapping holds no
//! logic. It fetches nothing, stores nothing and decides nothing: which
//! handler an event runs is `impress_surface::reduce`'s business, as it is for
//! `koine-tui` and the SwiftUI renderer.
//!
//! The element chosen for each kind, and the attributes it carries, are
//! `docs/plan-koine-web.md` D2. [`map`] is that table as code.
//!
//! # The sheet
//!
//! [`css_variables`] writes a web sheet as CSS custom properties on the root
//! element, and [`STYLESHEET`] (`koine.css`) reads them, so a changed slot is a
//! style pass in the browser and nothing else. A `StyleSheet` carries its
//! frontend tag; a sheet that is not tagged `web` is refused, as `koine-tui`
//! refuses a sheet that is not its own: [`Surface`] draws one
//! `div.koine-placeholder` saying so and [`css_variables`] returns nothing.
//!
//! # Hosts
//!
//! Without features this compiles for the browser (wasm32) and for the
//! desktop. The `ssr` feature adds [`render_html`].

mod map;
pub mod markdown;
pub mod outline;
#[cfg(target_arch = "wasm32")]
mod pointer;

use std::collections::BTreeMap;

use dioxus::prelude::*;
use impress_surface::{Event, EventKind, RenderTree};
use koine::style::{slots, StyleSheet, ValueKind, COLOR_TOKENS, FONT_TOKENS};
use serde_json::Value;

/// The frontend id this crate applies.
pub const FRONTEND: &str = "web";

/// The stylesheet every host serves beside the page. Reads the custom
/// properties [`css_variables`] writes.
pub const STYLESHEET: &str = include_str!("koine.css");

/// `--koine-column: 8px; --koine-color-button: …;` for one web sheet.
///
/// Spacing slots become `--koine-<slot>: <n>px`. Colour slots become
/// `--koine-<slot, dots as dashes>: var(--koine-color-<token>)`, and nothing
/// for `automatic`, which leaves the browser's own. Font slots write nothing:
/// a font is a class on the element (see [`font_class`]). A sheet for another
/// frontend writes nothing.
pub fn css_variables(sheet: &StyleSheet) -> String {
    if sheet.frontend != FRONTEND {
        return String::new();
    }
    let Ok(table) = slots(FRONTEND) else {
        return String::new();
    };
    let mut out = Vec::new();
    for slot in table {
        let value = sheet.value(slot.name);
        let name = slot.name.replace('.', "-");
        match slot.kind {
            ValueKind::Spacing => {
                if let Ok(n) = value.parse::<u32>() {
                    out.push(format!("--koine-{name}: {n}px;"));
                }
            }
            ValueKind::Color => {
                if value != "automatic" && COLOR_TOKENS.contains(&value) {
                    out.push(format!("--koine-{name}: var(--koine-color-{value});"));
                }
            }
            ValueKind::Font | ValueKind::Line => {}
        }
    }
    out.join(" ")
}

/// The class a font slot puts on its element: `koine-font-body`, `-title` or
/// `-mono`. `automatic` is no class at all.
pub(crate) fn font_class(sheet: &StyleSheet, slot: &str) -> Option<String> {
    let value = sheet.value(slot);
    (value != "automatic" && FONT_TOKENS.contains(&value)).then(|| format!("koine-font-{value}"))
}

/// What the mapping needs while it walks the tree, and nothing it could
/// decide with.
pub(crate) struct Ctx<'a> {
    pub sheet: &'a StyleSheet,
    pub focused: Option<&'a str>,
    /// The tree's `focus_order`. A `plot` or an `image` is a tab stop only
    /// when it is in it (the resolver puts it there when it declares
    /// `on_click`).
    pub stops: &'a [String],
    pub on_event: EventHandler<Event>,
    /// The tab each `tabs` widget shows. It is the page's own memory of a
    /// gesture, not surface state: the grammar's `tabs` has no handler, and
    /// the `select` event is sent anyway.
    pub active_tab: Signal<BTreeMap<String, usize>>,
    /// Where a chosen file goes (plan-koine-live L-D6). `None` where nothing
    /// is wired (the string renderer, a test): the input is drawn and does
    /// nothing.
    pub on_file: Option<EventHandler<FileChosen>>,
    /// The active row of each row widget, by widget id (the cursor's
    /// `active_row`, which the page collects). A row whose id equals it is
    /// drawn `data-koine-active` / `koine-active`.
    pub active: &'a BTreeMap<String, Value>,
    /// The row whose menu is open: `(widget, row id)`. Right-click sets it;
    /// the page sets it for `m`; a choice, Escape or a click elsewhere clears it.
    pub menu: Signal<Option<RowRef>>,
    /// The row being renamed, as `menu`: the page sets it for `r`; the input
    /// clears it on Enter (after sending `rename`) and on Escape.
    pub renaming: Signal<Option<RowRef>>,
    /// Text typed into a field and not yet sent: `(widget, text)`. A text
    /// input sends `change` on blur, and a tap on a button does not always
    /// blur it first (iOS), so `on_event` sends this before any other event.
    pub typed: Signal<Option<(String, String)>>,
    /// The row being moved by its grip. The mapping's own memory of a gesture.
    pub dragging: Signal<Option<RowRef>>,
    /// The row or gap under the pointer while a row is moved: it is drawn
    /// `data-koine-drop="true"`, and releasing there sends `move`.
    pub drop_target: Signal<Option<DropRef>>,
}

/// A file the person chose in a `file` field. The mapping reads the bytes
/// through the browser's file engine and hands them over; it fetches nothing.
/// The host uploads them and sends the field's `change` event with the blob
/// reference `{"blob": {"item": "<surface id>", "name": "<name>"}}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChosen {
    /// The node id of the `file` field.
    pub widget: String,
    /// The file's name, as the browser names it.
    pub name: String,
    pub bytes: Vec<u8>,
}

pub use outline::{DropAt, DropRef, RowRef};

/// One performance of a [`RenderTree`].
///
/// `sheet` must be tagged `web`; another tag draws a placeholder. `focused` is
/// the id of the focused widget (the host keeps the cursor, see
/// `koine::cursor`); it gets `data-koine-focused="true"` and `koine-focused`.
///
/// The outline seams (`docs/plan-koine-outline.md`), all optional:
///
/// - `active`: the active row of each row widget by widget id. The row gets
///   `data-koine-active="true"` and `koine-active`. A click on a row sends
///   the usual `select`; the page reads the row from that event and tells its
///   cursor, so the mapping has no callback for it.
/// - `menu`: a signal holding the row whose menu is open. The mapping opens
///   it on right-click and closes it; the page opens it for `m`. Absent, the
///   mapping keeps its own.
/// - `renaming`: a signal holding the row being renamed; the page sets it for
///   `r`, the mapping draws an input in place of the label and clears it.
/// - `dragging`: the row being moved by its grip, and `drop_target` the row
///   or gap under the pointer; only a test or a host that wants to see a move
///   in progress passes them.
#[component]
pub fn Surface(
    tree: ReadSignal<RenderTree>,
    sheet: ReadSignal<StyleSheet>,
    focused: ReadSignal<Option<String>>,
    on_event: EventHandler<Event>,
    on_file: Option<EventHandler<FileChosen>>,
    #[props(default)] active: ReadSignal<BTreeMap<String, Value>>,
    menu: Option<Signal<Option<RowRef>>>,
    renaming: Option<Signal<Option<RowRef>>>,
    dragging: Option<Signal<Option<RowRef>>>,
    drop_target: Option<Signal<Option<DropRef>>>,
) -> Element {
    let active_tab = use_signal(BTreeMap::<String, usize>::new);
    let own_menu = use_signal(|| None);
    let own_renaming = use_signal(|| None);
    let own_dragging = use_signal(|| None);
    let own_drop_target = use_signal(|| None);
    let mut menu = menu.unwrap_or(own_menu);
    let renaming = renaming.unwrap_or(own_renaming);
    let dragging = dragging.unwrap_or(own_dragging);
    let drop_target = drop_target.unwrap_or(own_drop_target);
    // Typing not yet sent goes first, so the click that follows it reads it
    // (a change from that same field supersedes it: it carries the text).
    let mut typed = use_signal(|| None::<(String, String)>);
    let on_event = EventHandler::new(move |ev: Event| {
        if let Some((widget, text)) = typed.take() {
            let superseded = ev.kind == EventKind::Change && ev.widget == widget;
            if !superseded {
                on_event.call(Event {
                    widget,
                    kind: EventKind::Change,
                    value: Value::String(text),
                });
            }
        }
        on_event.call(ev);
    });
    let active = active.read();
    let tree = tree.read();
    let sheet = sheet.read();
    let focused = focused.read();
    if sheet.frontend != FRONTEND {
        let reason = format!(
            "sheet is for frontend '{}', and this frontend applies '{}'",
            sheet.frontend, FRONTEND
        );
        return rsx! {
            div { class: "koine koine-placeholder", "placeholder style: {reason}" }
        };
    }
    let ctx = Ctx {
        sheet: &sheet,
        focused: focused.as_deref(),
        stops: &tree.focus_order,
        on_event,
        active_tab,
        on_file,
        active: &active,
        menu,
        renaming,
        dragging,
        drop_target,
        typed,
    };
    rsx! {
        div {
            class: "koine",
            style: css_variables(&sheet),
            // A click anywhere else closes an open row menu; a click on the
            // menu itself stops here.
            onclick: move |_| {
                if menu.peek().is_some() {
                    menu.set(None);
                }
            },
            {map::node(&tree.root, &ctx)}
        }
    }
}

/// What the outline seams hold at the moment of a string render: the same
/// four things [`Surface`] takes as props.
#[cfg(any(feature = "ssr", test))]
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RowView {
    pub active: BTreeMap<String, Value>,
    pub menu: Option<RowRef>,
    pub renaming: Option<RowRef>,
    pub dragging: Option<RowRef>,
    pub drop_target: Option<DropRef>,
}

/// [`Surface`] as a string: the conformance suite's performance, and the first
/// paint a host may serve before the wasm loads.
#[cfg(any(feature = "ssr", test))]
pub fn render_html(tree: &RenderTree, sheet: &StyleSheet, focused: Option<&str>) -> String {
    render_html_with(tree, sheet, focused, &RowView::default())
}

/// [`render_html`] with the outline seams set: an active row per widget, an
/// open menu, a rename in progress, a move in progress and its drop target.
#[cfg(any(feature = "ssr", test))]
pub fn render_html_with(
    tree: &RenderTree,
    sheet: &StyleSheet,
    focused: Option<&str>,
    view: &RowView,
) -> String {
    #[derive(Clone, PartialEq, Props)]
    struct Root {
        tree: RenderTree,
        sheet: StyleSheet,
        focused: Option<String>,
        view: RowView,
    }

    #[component]
    fn Page(props: Root) -> Element {
        let tree = use_signal(|| props.tree.clone());
        let sheet = use_signal(|| props.sheet.clone());
        let focused = use_signal(|| props.focused.clone());
        let active = use_signal(|| props.view.active.clone());
        let menu = use_signal(|| props.view.menu.clone());
        let renaming = use_signal(|| props.view.renaming.clone());
        let dragging = use_signal(|| props.view.dragging.clone());
        let drop_target = use_signal(|| props.view.drop_target.clone());
        rsx! {
            Surface {
                tree,
                sheet,
                focused,
                on_event: move |_| {},
                active,
                menu,
                renaming,
                dragging,
                drop_target,
            }
        }
    }

    let mut dom = VirtualDom::new_with_props(
        Page,
        Root {
            tree: tree.clone(),
            sheet: sheet.clone(),
            focused: focused.map(str::to_string),
            view: view.clone(),
        },
    );
    dom.rebuild_in_place();
    dioxus_ssr::render(&dom)
}

#[cfg(test)]
mod tests;
