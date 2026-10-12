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

use std::collections::BTreeMap;

use dioxus::prelude::*;
use impress_surface::{Event, RenderTree};
use koine::style::{slots, StyleSheet, ValueKind, COLOR_TOKENS, FONT_TOKENS};

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
    pub on_event: EventHandler<Event>,
    /// The tab each `tabs` widget shows. It is the page's own memory of a
    /// gesture, not surface state: the grammar's `tabs` has no handler, and
    /// the `select` event is sent anyway.
    pub active_tab: Signal<BTreeMap<String, usize>>,
    /// Where a chosen file goes (plan-koine-live L-D6). `None` where nothing
    /// is wired (the string renderer, a test): the input is drawn and does
    /// nothing.
    pub on_file: Option<EventHandler<FileChosen>>,
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

/// One performance of a [`RenderTree`].
///
/// `sheet` must be tagged `web`; another tag draws a placeholder. `focused` is
/// the id of the focused widget (the host keeps the cursor, see
/// `koine::cursor`); it gets `data-koine-focused="true"` and `koine-focused`.
#[component]
pub fn Surface(
    tree: ReadSignal<RenderTree>,
    sheet: ReadSignal<StyleSheet>,
    focused: ReadSignal<Option<String>>,
    on_event: EventHandler<Event>,
    on_file: Option<EventHandler<FileChosen>>,
) -> Element {
    let active_tab = use_signal(BTreeMap::<String, usize>::new);
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
        on_event,
        active_tab,
        on_file,
    };
    rsx! {
        div { class: "koine", style: css_variables(&sheet), {map::node(&tree.root, &ctx)} }
    }
}

/// [`Surface`] as a string: the conformance suite's performance, and the first
/// paint a host may serve before the wasm loads.
#[cfg(any(feature = "ssr", test))]
pub fn render_html(tree: &RenderTree, sheet: &StyleSheet, focused: Option<&str>) -> String {
    #[derive(Clone, PartialEq, Props)]
    struct Root {
        tree: RenderTree,
        sheet: StyleSheet,
        focused: Option<String>,
    }

    #[component]
    fn Page(props: Root) -> Element {
        let tree = use_signal(|| props.tree.clone());
        let sheet = use_signal(|| props.sheet.clone());
        let focused = use_signal(|| props.focused.clone());
        rsx! {
            Surface { tree, sheet, focused, on_event: move |_| {} }
        }
    }

    let mut dom = VirtualDom::new_with_props(
        Page,
        Root {
            tree: tree.clone(),
            sheet: sheet.clone(),
            focused: focused.map(str::to_string),
        },
    );
    dom.rebuild_in_place();
    dioxus_ssr::render(&dom)
}

#[cfg(test)]
mod tests;
