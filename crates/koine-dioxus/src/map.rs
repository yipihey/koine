//! The D2 table as code: one element per [`RenderKind`], the node id on the
//! DOM, the event each gesture sends.
//!
//! Nothing here computes. A value is read from the node and written to an
//! attribute, and an interaction is turned into an [`Event`] and handed to
//! the host. What an event means is `impress_surface::reduce`'s.
//!
//! Focus: a `field`, `button`, `table`, `list`, `tree`, `graph` or `tabs` node (and a
//! `plot` or `image` that declares `on_click`) is one tab stop
//! (`tabindex="0"`), in the order the tree's `focus_order` lists them, so the
//! DOM's tab order is the tree's. Rows and tab buttons are clicked, not
//! tabbed: they carry `tabindex="-1"` so they stay out of that order.

use std::rc::Rc;

use dioxus::prelude::*;
use impress_surface::resolve::RenderTab;
use impress_surface::tree::TreeNode;
use impress_surface::{Event, EventKind, RenderKind, RenderNode};
use serde_json::{json, Value};

use crate::outline::{self, RowRef};
use crate::{font_class, Ctx, FileChosen};

/// One node, and everything under it.
pub(crate) fn node(node: &RenderNode, ctx: &Ctx<'_>) -> Element {
    let id = node.id.as_str();
    let focused = is_focused(ctx, id).then_some("true");
    let label = node.label.clone();
    let help = node.help.clone();
    match &node.node {
        RenderKind::Column { items } => rsx! {
            div {
                class: class("koine-column", None, ctx, id),
                "data-koine-id": "{id}",
                "data-koine-focused": focused,
                aria_label: label,
                title: help,
                for item in items {
                    {self::node(item, ctx)}
                }
            }
        },
        RenderKind::Row { items } => rsx! {
            div {
                class: class("koine-row", None, ctx, id),
                "data-koine-id": "{id}",
                "data-koine-focused": focused,
                aria_label: label,
                title: help,
                for item in items {
                    {self::node(item, ctx)}
                }
            }
        },
        RenderKind::Grid { columns, items } => rsx! {
            div {
                class: class("koine-grid", None, ctx, id),
                style: "grid-template-columns: repeat({columns}, minmax(0, 1fr));",
                "data-koine-id": "{id}",
                "data-koine-focused": focused,
                aria_label: label,
                title: help,
                for item in items {
                    {self::node(item, ctx)}
                }
            }
        },
        RenderKind::Section {
            title,
            collapsed,
            body,
        } => {
            let heading = class(
                "koine-section-title",
                font_class(ctx.sheet, "section"),
                ctx,
                "",
            );
            rsx! {
                section {
                    class: class("koine-section", None, ctx, id),
                    "data-koine-id": "{id}",
                    "data-koine-focused": focused,
                    "data-koine-collapsed": collapsed.then_some("true"),
                    aria_label: label,
                    title: help,
                    h2 { class: heading, "{title}" }
                    div { class: "koine-section-body", hidden: *collapsed, {self::node(body, ctx)} }
                }
            }
        }
        RenderKind::Tabs { tabs } => tabs_node(id, label, help, tabs, ctx),
        RenderKind::Text { text } => rsx! {
            p {
                class: class("koine-text", font_class(ctx.sheet, "text"), ctx, id),
                "data-koine-id": "{id}",
                "data-koine-focused": focused,
                aria_label: label,
                title: help,
                "{text}"
            }
        },
        RenderKind::Table { .. } => table_node(node, ctx),
        RenderKind::List { .. } => list_node(node, ctx),
        RenderKind::Tree { .. } => tree_node(node, ctx),
        RenderKind::Graph { svg, .. } => {
            let on_event = ctx.on_event;
            let widget = id.to_string();
            rsx! {
                div {
                    class: class("koine-graph", None, ctx, id),
                    tabindex: 0,
                    "data-koine-id": "{id}",
                    "data-koine-focused": focused,
                    aria_label: label,
                    title: help,
                    // One handler for the whole drawing: the node under the
                    // pointer is found from the event's target (`data-id`).
                    onclick: move |e| {
                        if let Some(node) = graph_click(&e) {
                            on_event.call(event(&widget, EventKind::Select, json!([node])));
                        }
                    },
                    dangerous_inner_html: svg.clone(),
                }
            }
        }
        RenderKind::Plot { spec } => {
            let spec_text = spec.to_string();
            match koine_plot::parse_spec(spec) {
                Ok(parsed) => {
                    let svg = koine_plot::svg::render_svg(&parsed);
                    let frame = koine_plot::frame(&parsed);
                    let on_event = ctx.on_event;
                    let widget = id.to_string();
                    rsx! {
                        div {
                            class: class("koine-plot", None, ctx, id),
                            tabindex: stop(ctx, id),
                            "data-koine-id": "{id}",
                            "data-koine-focused": focused,
                            "data-koine-spec": spec_text,
                            aria_label: label,
                            title: help,
                            // A click in the plot area is a `click` carrying the
                            // data coordinates under it; outside it, nothing.
                            onclick: move |e| {
                                if let Some(at) = plot_click(&e, &frame) {
                                    on_event.call(event(&widget, EventKind::Click, at));
                                }
                            },
                            dangerous_inner_html: svg,
                        }
                    }
                }
                // A spec that does not parse keeps its slot and names why.
                Err(error) => rsx! {
                    div {
                        class: class("koine-plot koine-plot-error", None, ctx, id),
                        "data-koine-id": "{id}",
                        "data-koine-focused": focused,
                        "data-koine-spec": spec_text,
                        aria_label: label,
                        title: help,
                        "{error}"
                    }
                },
            }
        }
        RenderKind::Code {
            language,
            text,
            spans,
        } => {
            let is_diff = language.as_deref() == Some("diff");
            let inner = language
                .as_ref()
                .map(|language| format!("language-{language}"));
            rsx! {
                pre {
                    class: class("koine-code", None, ctx, id),
                    "data-koine-id": "{id}",
                    "data-koine-focused": focused,
                    "data-language": language.clone(),
                    aria_label: label,
                    title: help,
                    code { class: inner,
                        if is_diff {
                            for (index , line) in text.lines().enumerate() {
                                span { key: "{index}", class: diff_class(line), "{line}\n" }
                            }
                        } else if !spans.is_empty() {
                            // Highlighted: a classed `span` per piece, plain
                            // text as a bare text node, a newline between lines.
                            for (index , line) in spans.iter().enumerate() {
                                Fragment { key: "{index}",
                                    for (piece , (class , piece_text)) in line.iter().enumerate() {
                                        if class.is_empty() {
                                            Fragment { key: "{piece}", "{piece_text}" }
                                        } else {
                                            span { key: "{piece}", class: "koine-hl-{class}", "{piece_text}" }
                                        }
                                    }
                                    if index + 1 < spans.len() {
                                        "\n"
                                    }
                                }
                            }
                        } else {
                            "{text}"
                        }
                    }
                }
            }
        }
        RenderKind::Image { blob, url } => {
            if let Some(src) = url.as_ref().and_then(Value::as_str) {
                let on_event = ctx.on_event;
                let widget = id.to_string();
                rsx! {
                    img {
                        class: class("koine-image", None, ctx, id),
                        tabindex: stop(ctx, id),
                        "data-koine-id": "{id}",
                        "data-koine-focused": focused,
                        src: "{src}",
                        alt: label.clone().unwrap_or_default(),
                        title: help,
                        // A click is a `click` carrying where in the displayed
                        // image it landed, as fractions of its size.
                        onclick: move |e| {
                            if let Some(at) = image_click(&e) {
                                on_event.call(event(&widget, EventKind::Click, at));
                            }
                        },
                    }
                }
            } else {
                let blob = blob.as_ref().map_or("none".to_string(), display);
                rsx! {
                    div {
                        class: class("koine-placeholder", None, ctx, id),
                        tabindex: stop(ctx, id),
                        "data-koine-id": "{id}",
                        "data-koine-focused": focused,
                        title: help,
                        "placeholder image: blob {blob}"
                    }
                }
            }
        }
        RenderKind::Field { field, bind, value } => {
            field_node(id, label, help, field, bind.as_deref(), value, ctx)
        }
        RenderKind::Button { label: text } => {
            let on_event = ctx.on_event;
            let widget = id.to_string();
            rsx! {
                button {
                    class: class("koine-button", None, ctx, id),
                    r#type: "button",
                    tabindex: 0,
                    "data-koine-id": "{id}",
                    "data-koine-focused": focused,
                    aria_label: label,
                    title: help,
                    onclick: move |_| on_event.call(event(&widget, EventKind::Click, Value::Null)),
                    "{text}"
                }
            }
        }
        RenderKind::Status { level, message } => {
            let (level_class, role) = match level.as_str() {
                "error" => ("koine-status koine-status-error", "alert"),
                "warning" => ("koine-status koine-status-warning", "status"),
                "info" => ("koine-status koine-status-info", "status"),
                _ => ("koine-status", "status"),
            };
            rsx! {
                div {
                    class: class(level_class, None, ctx, id),
                    role: role,
                    "data-koine-id": "{id}",
                    "data-koine-focused": focused,
                    "data-koine-level": "{level}",
                    aria_label: label,
                    title: help,
                    "{message}"
                }
            }
        }
        RenderKind::Log { lines } => {
            let text = lines_of(lines);
            rsx! {
                pre {
                    class: class("koine-log", None, ctx, id),
                    "data-koine-id": "{id}",
                    "data-koine-focused": focused,
                    aria_label: label,
                    title: help,
                    "{text}"
                }
            }
        }
        RenderKind::Kv { pairs } => {
            let pairs = pairs_of(pairs);
            rsx! {
                dl {
                    class: class("koine-kv", None, ctx, id),
                    "data-koine-id": "{id}",
                    "data-koine-focused": focused,
                    aria_label: label,
                    title: help,
                    for (i , (key , value)) in pairs.into_iter().enumerate() {
                        Fragment { key: "{i}",
                            dt { "{key}" }
                            dd { "{value}" }
                        }
                    }
                }
            }
        }
        RenderKind::Divider => rsx! {
            hr {
                class: class("koine-divider", None, ctx, id),
                "data-koine-id": "{id}",
                "data-koine-focused": focused,
            }
        },
        RenderKind::Spacer => rsx! {
            div {
                class: class("koine-spacer", None, ctx, id),
                "data-koine-id": "{id}",
                "data-koine-focused": focused,
            }
        },
        RenderKind::Placeholder {
            unknown_kind,
            reason,
            ..
        } => {
            // The tui's words, exactly.
            let kind = unknown_kind.as_deref().unwrap_or("placeholder");
            let line = match reason {
                Some(reason) => format!("placeholder {kind}: {reason}"),
                None => format!("placeholder {kind}"),
            };
            rsx! {
                div {
                    class: class("koine-placeholder", None, ctx, id),
                    "data-koine-id": "{id}",
                    "data-koine-focused": focused,
                    "data-koine-unknown-kind": unknown_kind.clone(),
                    aria_label: label,
                    title: help,
                    "{line}"
                }
            }
        }
    }
}

fn tabs_node(
    id: &str,
    label: Option<String>,
    help: Option<String>,
    tabs: &[RenderTab],
    ctx: &Ctx<'_>,
) -> Element {
    let focused = is_focused(ctx, id).then_some("true");
    let active = ctx
        .active_tab
        .read()
        .get(id)
        .copied()
        .unwrap_or(0)
        .min(tabs.len().saturating_sub(1));
    rsx! {
        div {
            class: class("koine-tabs", None, ctx, id),
            tabindex: 0,
            "data-koine-id": "{id}",
            "data-koine-focused": focused,
            aria_label: label,
            title: help,
            div { class: "koine-tablist", role: "tablist",
                for (index , tab) in tabs.iter().enumerate() {
                    {
                        let on_event = ctx.on_event;
                        let mut active_tab = ctx.active_tab;
                        let widget = id.to_string();
                        rsx! {
                            button {
                                key: "{index}",
                                class: "koine-tab",
                                role: "tab",
                                r#type: "button",
                                tabindex: -1,
                                aria_selected: (index == active).to_string(),
                                onclick: move |_| {
                                    active_tab.write().insert(widget.clone(), index);
                                    on_event.call(event(&widget, EventKind::Select, Value::from(index)));
                                },
                                "{tab.title}"
                            }
                        }
                    }
                }
            }
            for (index , tab) in tabs.iter().enumerate() {
                div {
                    key: "{index}",
                    class: "koine-tabpanel",
                    role: "tabpanel",
                    hidden: index != active,
                    {node(&tab.body, ctx)}
                }
            }
        }
    }
}

/// Whether `row` is the widget's active row.
fn is_active(ctx: &Ctx<'_>, widget: &str, row: &Value) -> bool {
    ctx.active.get(widget) == Some(row)
}

/// `koine-active` for the active row, no class otherwise.
fn row_class(base: &str, active: bool) -> String {
    if active {
        format!("{base} koine-active")
    } else {
        base.to_string()
    }
}

/// Whether the menu is open on this row.
fn menu_is_open(ctx: &Ctx<'_>, widget: &str, row: &Value) -> bool {
    matches!(&*ctx.menu.read(), Some((w, r)) if w == widget && r == row)
}

/// The row menu: a `ul.koine-menu` of the labels, drawn inside the row it
/// belongs to (the row is `position: relative`, the menu is placed under it).
/// Choosing an entry sends `menu` with `{"id", "action"}` and closes; the
/// click does not reach the row, which would select it.
fn menu_list(ctx: &Ctx<'_>, widget: &str, row: &Value, actions: &[String]) -> Element {
    if actions.is_empty() || !menu_is_open(ctx, widget, row) {
        return rsx! {};
    }
    rsx! {
        ul {
            class: "koine-menu",
            role: "menu",
            "data-koine-menu": "{widget}",
            onclick: move |e| e.stop_propagation(),
            oncontextmenu: move |e| {
                e.prevent_default();
                e.stop_propagation();
            },
            for (index , action) in actions.iter().enumerate() {
                {
                    let on_event = ctx.on_event;
                    let mut menu = ctx.menu;
                    let widget = widget.to_string();
                    let row = row.clone();
                    let action = action.clone();
                    rsx! {
                        li {
                            key: "{index}",
                            class: "koine-menu-item",
                            role: "menuitem",
                            onclick: move |e| {
                                e.stop_propagation();
                                menu.set(None);
                                on_event
                                    .call(
                                        event(&widget, EventKind::Menu, outline::menu_value(&row, &action)),
                                    );
                            },
                            "{action}"
                        }
                    }
                }
            }
        }
    }
}

/// The row's `button.koine-more`: a "…" that opens the same menu a
/// right-click does, for a screen with no right-click (a phone). The sheet
/// hides it where there is a pointer; nothing when the widget has no
/// `actions`. Not a tab stop: `m` on the row is the key.
fn more_button(ctx: &Ctx<'_>, widget: &str, row: &Value, actions: &[String]) -> Element {
    if actions.is_empty() {
        return rsx! {};
    }
    let mut menu = ctx.menu;
    let target = (widget.to_string(), row.clone());
    rsx! {
        button {
            class: "koine-more",
            r#type: "button",
            tabindex: -1,
            aria_label: "row actions",
            onclick: move |e| {
                e.stop_propagation();
                menu.set(Some(target.clone()));
            },
            "\u{2026}"
        }
    }
}

/// `span.koine-count` for a row that has a count.
fn count_span(count: Option<String>) -> Element {
    match count {
        Some(count) => rsx! {
            span { class: "koine-count", "{count}" }
        },
        None => rsx! {},
    }
}

fn table_node(node: &RenderNode, ctx: &Ctx<'_>) -> Element {
    let RenderKind::Table {
        rows,
        columns,
        id_field,
        actions,
        count_field,
    } = &node.node
    else {
        return rsx! {};
    };
    let id = node.id.as_str();
    let focused = is_focused(ctx, id).then_some("true");
    let rows: &[Value] = rows.as_array().map_or(&[], Vec::as_slice);
    rsx! {
        table {
            class: class("koine-table", None, ctx, id),
            tabindex: 0,
            "data-koine-id": "{id}",
            "data-koine-focused": focused,
            aria_label: node.label.clone(),
            title: node.help.clone(),
            thead {
                tr {
                    for column in columns {
                        th { key: "{column}", scope: "col", "{column}" }
                    }
                }
            }
            tbody {
                for (index , row) in rows.iter().enumerate() {
                    {
                        let on_event = ctx.on_event;
                        let widget = id.to_string();
                        let key = outline::row_key(row, id_field.as_deref(), index);
                        let active = is_active(ctx, id, &key);
                        let selected = selection_of(row, id_field.as_deref(), index);
                        let activated = Value::Array(vec![key.clone()]);
                        let has_menu = !actions.is_empty();
                        let (menu_widget, menu_row) = (widget.clone(), key.clone());
                        let menu = ctx.menu;
                        let count = outline::count_text(row, count_field.as_deref());
                        let menu_el = menu_list(ctx, id, &key, actions);
                        let more_el = more_button(ctx, id, &key, actions);
                        rsx! {
                            tr {
                                key: "{index}",
                                class: active.then_some("koine-active"),
                                tabindex: -1,
                                "data-koine-row": "{index}",
                                "data-koine-active": active.then_some("true"),
                                onclick: move |_| on_event.call(event(&widget, EventKind::Select, selected.clone())),
                                ondoubleclick: {
                                    let widget = menu_widget.clone();
                                    move |_| on_event.call(event(&widget, EventKind::Activate, activated.clone()))
                                },
                                oncontextmenu: move |e| {
                                    if has_menu {
                                        e.prevent_default();
                                        let mut menu = menu;
                                        menu.set(Some((menu_widget.clone(), menu_row.clone())));
                                    }
                                },
                                for (position , column) in columns.iter().enumerate() {
                                    td { key: "{column}",
                                        "{cell(row, column)}"
                                        if position == 0 {
                                            {count_span(count.clone())}
                                            {more_el.clone()}
                                            {menu_el.clone()}
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn list_node(node: &RenderNode, ctx: &Ctx<'_>) -> Element {
    let RenderKind::List {
        rows,
        actions,
        count_field,
    } = &node.node
    else {
        return rsx! {};
    };
    let id = node.id.as_str();
    let focused = is_focused(ctx, id).then_some("true");
    let rows: &[Value] = rows.as_array().map_or(&[], Vec::as_slice);
    rsx! {
        ul {
            class: class("koine-list", None, ctx, id),
            tabindex: 0,
            "data-koine-id": "{id}",
            "data-koine-focused": focused,
            aria_label: node.label.clone(),
            title: node.help.clone(),
            for (index , row) in rows.iter().enumerate() {
                {
                    let on_event = ctx.on_event;
                    let widget = id.to_string();
                    let key = outline::row_key(row, None, index);
                    let active = is_active(ctx, id, &key);
                    let selected = selection_of(row, None, index);
                    let activated = Value::Array(vec![key.clone()]);
                    let has_menu = !actions.is_empty();
                    let (menu_widget, menu_row) = (widget.clone(), key.clone());
                    let menu = ctx.menu;
                    let count = outline::count_text(row, count_field.as_deref());
                    let menu_el = menu_list(ctx, id, &key, actions);
                    let more_el = more_button(ctx, id, &key, actions);
                    rsx! {
                        li {
                            key: "{index}",
                            class: active.then_some("koine-active"),
                            tabindex: -1,
                            "data-koine-row": "{index}",
                            "data-koine-active": active.then_some("true"),
                            onclick: move |_| on_event.call(event(&widget, EventKind::Select, selected.clone())),
                            ondoubleclick: {
                                let widget = menu_widget.clone();
                                move |_| on_event.call(event(&widget, EventKind::Activate, activated.clone()))
                            },
                            oncontextmenu: move |e| {
                                if has_menu {
                                    e.prevent_default();
                                    let mut menu = menu;
                                    menu.set(Some((menu_widget.clone(), menu_row.clone())));
                                }
                            },
                            "{row_text(row)}"
                            {count_span(count)}
                            {more_el}
                            {menu_el}
                        }
                    }
                }
            }
        }
    }
}

/// A `tree`: the visible outline the resolver computed, one `li` per node.
///
/// - A disclosure click sends `change` with the *whole* expanded list the
///   resolver carried (O-D2), this node's id added or removed; a label click
///   sends `select` with `[id]`; a double-click on the label sends `activate`.
/// - The active row is `data-koine-active` / `koine-active`; a node's count
///   is a `span.koine-count` after its label.
/// - Right-click on a row opens its menu when the tree declares `actions`.
/// - A row being renamed shows an `input.koine-rename` in place of its label:
///   Enter sends `rename` `{"id", "label"}` and closes, Escape closes.
/// - A `movable` tree's rows are `draggable`. Dropping on a row sends `move`
///   with that row as the new parent (`index` 0); while a drag is in progress
///   a `li.koine-drop-gap` between rows drops as a sibling at that position
///   (see [`outline`]). One row at a time: no multi-select drag.
///
/// Rows are clicked, not tabbed, as a table's are.
fn tree_node(node: &RenderNode, ctx: &Ctx<'_>) -> Element {
    let RenderKind::Tree {
        nodes,
        expanded,
        actions,
        movable,
        ..
    } = &node.node
    else {
        return rsx! {};
    };
    let id = node.id.as_str();
    let focused = is_focused(ctx, id).then_some("true");
    // What a drop handler needs to decide with, shared by every row.
    let all: Rc<Vec<TreeNode>> = Rc::new(nodes.clone());
    let dragged: Option<Value> = if *movable {
        ctx.dragging
            .read()
            .as_ref()
            .filter(|(w, _)| w == id)
            .map(|(_, row)| row.clone())
    } else {
        None
    };
    let renaming: Option<Value> = ctx
        .renaming
        .read()
        .as_ref()
        .filter(|(w, _)| w == id)
        .map(|(_, row)| row.clone());
    let end_gap = dragged
        .as_ref()
        .map(|dragged| gap_row(ctx, id, &all, dragged, all.len()));
    rsx! {
        ul {
            class: class("koine-tree", None, ctx, id),
            tabindex: 0,
            "data-koine-id": "{id}",
            "data-koine-focused": focused,
            aria_label: node.label.clone(),
            title: node.help.clone(),
            for (index , node) in nodes.iter().enumerate() {
                {
                    let on_event = ctx.on_event;
                    let widget = id.to_string();
                    let selected = Value::Array(vec![node.id.clone()]);
                    let opened = Value::Array(vec![node.id.clone()]);
                    let toggled = outline::set_open(expanded, &node.id, !node.expanded);
                    let widget_for_toggle = widget.clone();
                    let is_open = node.expanded;
                    let indent = format!("padding-left: calc({} * 1.25em)", node.depth);
                    let active = is_active(ctx, id, &node.id);
                    let has_menu = !actions.is_empty();
                    let menu_el = menu_list(ctx, id, &node.id, actions);
                    let more_el = more_button(ctx, id, &node.id, actions);
                    let count = node.count.as_ref().map(display);
                    let menu_target = (widget.clone(), node.id.clone());
                    let menu = ctx.menu;
                    let gap = dragged.as_ref().map(|dragged| gap_row(ctx, id, &all, dragged, index));
                    let editing = renaming.as_ref() == Some(&node.id);
                    let rename_el = if editing {
                        rename_input(ctx, id, node)
                    } else {
                        rsx! {}
                    };
                    let drag = drag_handlers(ctx, id, &all, index, *movable);
                    rsx! {
                        Fragment { key: "{index}",
                            {gap}
                            li {
                                class: row_class("koine-tree-node", active),
                                style: indent,
                                tabindex: -1,
                                draggable: movable.then_some("true"),
                                "data-koine-depth": "{node.depth}",
                                "data-koine-row": "{index}",
                                "data-koine-active": active.then_some("true"),
                                oncontextmenu: move |e| {
                                    if has_menu {
                                        e.prevent_default();
                                        let mut menu = menu;
                                        menu.set(Some(menu_target.clone()));
                                    }
                                },
                                ondragstart: drag.start,
                                ondragend: drag.end,
                                ondragover: drag.over,
                                ondrop: drag.drop,
                                if node.has_children {
                                    button {
                                        class: "koine-tree-disclosure",
                                        tabindex: -1,
                                        aria_expanded: if is_open { "true" } else { "false" },
                                        onclick: move |_| on_event.call(event(&widget_for_toggle, EventKind::Change, toggled.clone())),
                                        if is_open {
                                            "\u{25be}"
                                        } else {
                                            "\u{25b8}"
                                        }
                                    }
                                }
                                if editing {
                                    {rename_el}
                                } else {
                                    span {
                                        class: "koine-tree-label",
                                        onclick: move |_| on_event.call(event(&widget, EventKind::Select, selected.clone())),
                                        ondoubleclick: {
                                            let widget = id.to_string();
                                            move |_| on_event.call(event(&widget, EventKind::Activate, opened.clone()))
                                        },
                                        "{node.label}"
                                    }
                                }
                                {count_span(count)}
                                {more_el}
                                {menu_el}
                            }
                        }
                    }
                }
            }
            {end_gap}
        }
    }
}

/// The rename input that stands in for a label: a one-field form so Enter
/// submits it natively, with the current text selected for typing over.
fn rename_input(ctx: &Ctx<'_>, widget: &str, node: &TreeNode) -> Element {
    let on_event = ctx.on_event;
    let mut renaming = ctx.renaming;
    let (widget, row) = (widget.to_string(), node.id.clone());
    let current = node.label.clone();
    let fallback = node.label.clone();
    rsx! {
        form {
            class: "koine-tree-label koine-rename-form",
            onsubmit: move |e| {
                e.prevent_default();
                let text = match e.get_first("label") {
                    Some(FormValue::Text(text)) => text,
                    _ => fallback.clone(),
                };
                renaming.set(None);
                on_event.call(event(&widget, EventKind::Rename, outline::rename_value(&row, &text)));
            },
            onclick: move |e| e.stop_propagation(),
            input {
                class: "koine-rename",
                name: "label",
                r#type: "text",
                value: "{current}",
                autofocus: true,
                onmounted: move |e| {
                    spawn(async move {
                        let _ = e.set_focus(true).await;
                    });
                },
                onkeydown: move |e| {
                    if e.key() == Key::Escape {
                        e.stop_propagation();
                        renaming.set(None);
                    }
                },
            }
        }
    }
}

/// The four drag-and-drop handlers of one row; they do nothing for a tree
/// that is not `movable` (and a row that is not `draggable` never starts one).
struct DragHandlers {
    start: EventHandler<DragEvent>,
    end: EventHandler<DragEvent>,
    over: EventHandler<DragEvent>,
    drop: EventHandler<DragEvent>,
}

/// The `move` a drop on row `index` would send, when a drag of this widget is
/// in progress and the row may take it.
fn drop_value(
    dragging: &Signal<Option<RowRef>>,
    widget: &str,
    all: &[TreeNode],
    index: usize,
) -> Option<Value> {
    let (from, dragged) = dragging.peek().clone()?;
    if from != widget {
        return None;
    }
    outline::drop_on(all, &dragged, index)
}

/// The handlers for row `index`. `start` records the dragged id and gives the
/// browser something to carry (Firefox starts no drag without data); `over`
/// accepts a drop only on a row this drag may land on; `drop` sends `move`.
fn drag_handlers(
    ctx: &Ctx<'_>,
    widget: &str,
    all: &Rc<Vec<TreeNode>>,
    index: usize,
    movable: bool,
) -> DragHandlers {
    if !movable {
        let inert = || EventHandler::new(|_: DragEvent| {});
        return DragHandlers {
            start: inert(),
            end: inert(),
            over: inert(),
            drop: inert(),
        };
    }
    let mut dragging = ctx.dragging;
    let on_event = ctx.on_event;
    let row = all[index].id.clone();
    let start_widget = widget.to_string();
    let start = EventHandler::new(move |e: DragEvent| {
        let transfer = e.data_transfer();
        let _ = transfer.set_data("text/plain", &display(&row));
        transfer.set_effect_allowed("move");
        dragging.set(Some((start_widget.clone(), row.clone())));
    });
    let end = EventHandler::new(move |_: DragEvent| dragging.set(None));
    let (over_all, over_widget) = (all.clone(), widget.to_string());
    let over = EventHandler::new(move |e: DragEvent| {
        if drop_value(&dragging, &over_widget, &over_all, index).is_some() {
            e.prevent_default();
        }
    });
    let (drop_all, drop_widget) = (all.clone(), widget.to_string());
    let drop = EventHandler::new(move |e: DragEvent| {
        e.prevent_default();
        e.stop_propagation();
        if let Some(value) = drop_value(&dragging, &drop_widget, &drop_all, index) {
            dragging.set(None);
            on_event.call(event(&drop_widget, EventKind::Move, value));
        }
    });
    DragHandlers {
        start,
        end,
        over,
        drop,
    }
}

/// A thin drop target before row `at` (`at == len` is after the last row),
/// drawn only while a drag is in progress. Dropping sends `move` as a sibling
/// at that position; a gap that would put the dragged branch under itself is
/// not drawn.
fn gap_row(
    ctx: &Ctx<'_>,
    widget: &str,
    all: &Rc<Vec<TreeNode>>,
    dragged: &Value,
    at: usize,
) -> Element {
    let Some(value) = outline::drop_gap(all, dragged, at) else {
        return rsx! {};
    };
    let mut dragging = ctx.dragging;
    let on_event = ctx.on_event;
    let widget = widget.to_string();
    let depth = all.get(at).map_or(0, |n| n.depth);
    rsx! {
        li {
            key: "gap-{at}",
            class: "koine-drop-gap",
            role: "presentation",
            style: "margin-left: calc({depth} * 1.25em)",
            "data-koine-gap": "{at}",
            ondragover: move |e| e.prevent_default(),
            ondrop: move |e| {
                e.prevent_default();
                e.stop_propagation();
                dragging.set(None);
                on_event.call(event(&widget, EventKind::Move, value.clone()));
            },
        }
    }
}

fn field_node(
    id: &str,
    label: Option<String>,
    help: Option<String>,
    field: &Value,
    bind: Option<&str>,
    value: &Value,
    ctx: &Ctx<'_>,
) -> Element {
    let focused = is_focused(ctx, id).then_some("true");
    let dom_id = format!("koine-{id}");
    let (tag, options) = match field.as_object().and_then(|o| o.iter().next()) {
        Some((tag, options)) => (tag.as_str(), options.clone()),
        None => ("", Value::Null),
    };
    let class = class(
        "koine-field-control",
        font_class(ctx.sheet, "field"),
        ctx,
        id,
    );
    let on_event = ctx.on_event;
    let widget = id.to_string();
    let bind = bind.map(str::to_string);
    let control = match tag {
        "number" | "slider" => {
            let kind = if tag == "number" { "number" } else { "range" };
            let (min, max, step) = (
                options.get("min").map(display),
                options.get("max").map(display),
                options.get("step").map(display),
            );
            let shown = if value.is_null() {
                String::new()
            } else {
                display(value)
            };
            rsx! {
                input {
                    id: "{dom_id}",
                    class: class,
                    r#type: kind,
                    tabindex: 0,
                    "data-koine-id": "{id}",
                    "data-koine-focused": focused,
                    "data-koine-bind": bind,
                    min: min,
                    max: max,
                    step: step,
                    value: shown,
                    title: help,
                    aria_label: label.clone(),
                    onchange: move |e| on_event.call(event(&widget, EventKind::Change, number_of(&e.value()))),
                }
            }
        }
        "toggle" => rsx! {
            input {
                id: "{dom_id}",
                class: class,
                r#type: "checkbox",
                tabindex: 0,
                "data-koine-id": "{id}",
                "data-koine-focused": focused,
                "data-koine-bind": bind,
                checked: value.as_bool().unwrap_or(false),
                title: help,
                aria_label: label.clone(),
                onchange: move |e| on_event.call(event(&widget, EventKind::Change, Value::Bool(e.checked()))),
            }
        },
        "select" => {
            let choices: Vec<Value> = options
                .get("options")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let chosen = choices.iter().position(|c| choice_value(c) == value);
            let sent = choices.clone();
            rsx! {
                select {
                    id: "{dom_id}",
                    class: class,
                    tabindex: 0,
                    "data-koine-id": "{id}",
                    "data-koine-focused": focused,
                    "data-koine-bind": bind,
                    title: help,
                    aria_label: label.clone(),
                    onchange: move |e| {
                        let picked = e
                            .value()
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| sent.get(i))
                            .map_or(Value::Null, |c| choice_value(c).clone());
                        on_event.call(event(&widget, EventKind::Change, picked));
                    },
                    for (index , choice) in choices.iter().enumerate() {
                        option {
                            key: "{index}",
                            value: "{index}",
                            selected: chosen == Some(index),
                            "{choice_label(choice)}"
                        }
                    }
                }
            }
        }
        "file" => {
            let accept = options
                .get("accept")
                .and_then(Value::as_str)
                .map(str::to_string);
            // The blob reference the field holds names the file last sent.
            let chosen = value
                .pointer("/blob/name")
                .and_then(Value::as_str)
                .map(str::to_string);
            let on_file = ctx.on_file;
            rsx! {
                input {
                    id: "{dom_id}",
                    class: class,
                    r#type: "file",
                    accept: accept,
                    tabindex: 0,
                    "data-koine-id": "{id}",
                    "data-koine-focused": focused,
                    "data-koine-bind": bind,
                    "data-koine-file": chosen,
                    title: help,
                    aria_label: label.clone(),
                    onchange: move |e: FormEvent| {
                        let (Some(on_file), Some(file)) = (on_file, e.files().into_iter().next())
                        else {
                            return;
                        };
                        let widget = widget.clone();
                        spawn(async move {
                            if let Ok(bytes) = file.read_bytes().await {
                                on_file
                                    .call(FileChosen {
                                        widget,
                                        name: file.name(),
                                        bytes: bytes.to_vec(),
                                    });
                            }
                        });
                    },
                }
            }
        }
        "date" => {
            let shown = value.as_str().unwrap_or_default().to_string();
            rsx! {
                input {
                    id: "{dom_id}",
                    class: class,
                    r#type: "date",
                    tabindex: 0,
                    "data-koine-id": "{id}",
                    "data-koine-focused": focused,
                    "data-koine-bind": bind,
                    value: shown,
                    title: help,
                    aria_label: label.clone(),
                    onchange: move |e| on_event.call(event(&widget, EventKind::Change, Value::String(e.value()))),
                }
            }
        }
        other => {
            // `text`, and a kind this build has no native control for: a text
            // input that names the kind, as the plan's known limits say.
            let shown = match value {
                Value::Null => String::new(),
                Value::String(s) => s.clone(),
                v => v.to_string(),
            };
            let unknown = (other != "text").then(|| other.to_string());
            rsx! {
                input {
                    id: "{dom_id}",
                    class: class,
                    r#type: "text",
                    tabindex: 0,
                    "data-koine-id": "{id}",
                    "data-koine-focused": focused,
                    "data-koine-bind": bind,
                    "data-koine-field-kind": unknown,
                    value: shown,
                    title: help,
                    aria_label: label.clone(),
                    onchange: move |e| on_event.call(event(&widget, EventKind::Change, Value::String(e.value()))),
                }
            }
        }
    };
    rsx! {
        div { class: "koine-field",
            if let Some(text) = label {
                label { r#for: "{dom_id}", "{text}" }
            }
            {control}
        }
    }
}

fn event(widget: &str, kind: EventKind, value: Value) -> Event {
    Event {
        widget: widget.to_string(),
        kind,
        value,
    }
}

/// `tabindex="0"` for a `plot` or `image` the resolver made a tab stop
/// (it is in the tree's `focus_order`), no attribute otherwise.
fn stop(ctx: &Ctx<'_>, id: &str) -> Option<i32> {
    ctx.stops.iter().any(|s| s == id).then_some(0)
}

fn is_focused(ctx: &Ctx<'_>, id: &str) -> bool {
    ctx.focused == Some(id)
}

/// `base`, the font class if any, and `koine-focused` for the focused id.
fn class(base: &str, font: Option<String>, ctx: &Ctx<'_>, id: &str) -> String {
    let mut out = base.to_string();
    if let Some(font) = font {
        out.push(' ');
        out.push_str(&font);
    }
    if !id.is_empty() && is_focused(ctx, id) {
        out.push_str(" koine-focused");
    }
    out
}

/// A click on a plot, in data coordinates `{"x", "y"}`, or `None` outside the
/// plot area (or when the page cannot say where the click landed).
///
/// In a browser the size the SVG is drawn at is read from the DOM (CSS scales
/// it to fit, `max-width: 100%`): the scale is the drawn width over the frame's
/// width, and the pixel is the pointer's offset from the drawn SVG's corner.
/// Anywhere else the pointer's element coordinates are used at scale 1.
fn plot_click(e: &MouseEvent, frame: &koine_plot::Frame) -> Option<Value> {
    #[cfg(target_arch = "wasm32")]
    {
        let (px, py, drawn_width) = crate::pointer::plot_pixel(e)?;
        plot_click_to_data(frame, px, py, drawn_width / frame.width)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let at = e.element_coordinates();
        plot_click_to_data(frame, at.x, at.y, 1.0)
    }
}

/// The mapping behind [`plot_click`], pure: a pixel `(px, py)` measured from
/// the drawn SVG's top-left corner, in a drawing `scale` times the SVG's own
/// viewBox units (1 when it is drawn at its natural size), to the
/// `{"x", "y"}` data coordinates under it. A `scale` that is not a positive
/// number is 1. `None` outside the plot area, as [`koine_plot::Frame::to_data`]
/// answers.
pub(crate) fn plot_click_to_data(
    frame: &koine_plot::Frame,
    px: f64,
    py: f64,
    scale: f64,
) -> Option<Value> {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let (x, y) = frame.to_data(px / scale, py / scale)?;
    Some(json!({ "x": x, "y": y }))
}

/// A click on an image as `{"x", "y"}`, fractions of its displayed size.
fn image_click(e: &MouseEvent) -> Option<Value> {
    #[cfg(target_arch = "wasm32")]
    {
        let (px, py, width, height) = crate::pointer::image_pixel(e)?;
        image_click_to_fraction(px, py, width, height)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // Outside a browser the page cannot learn the displayed size.
        let _ = e;
        None
    }
}

/// The fraction of a `width` by `height` element a pixel is at, each clamped
/// to `0..=1`. `None` for an element with no size.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn image_click_to_fraction(px: f64, py: f64, width: f64, height: f64) -> Option<Value> {
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
        return None;
    }
    if !(px.is_finite() && py.is_finite()) {
        return None;
    }
    Some(json!({
        "x": (px / width).clamp(0.0, 1.0),
        "y": (py / height).clamp(0.0, 1.0),
    }))
}

/// The id of the graph node under a click: the `data-id` of the nearest
/// element, from the event's target upward, that has one. `None` when the
/// click was on the background (or outside a browser).
fn graph_click(e: &MouseEvent) -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        crate::pointer::data_id(e)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = e;
        None
    }
}

/// The class of one line of a `diff`: added (`+`), removed (`-`), a hunk
/// header (`@@`), or context. A sheet colours them; the mapping only names
/// them.
fn diff_class(line: &str) -> &'static str {
    if line.starts_with("@@") {
        "koine-diff-hunk"
    } else if line.starts_with('+') {
        "koine-diff-add"
    } else if line.starts_with('-') {
        "koine-diff-del"
    } else {
        "koine-diff-ctx"
    }
}

/// A number field's value is a JSON number: an integer when it is one.
fn number_of(raw: &str) -> Value {
    let Ok(parsed) = raw.trim().parse::<f64>() else {
        return Value::Null;
    };
    if parsed.fract() == 0.0 && parsed.abs() < 9.0e15 {
        // Exact: the fractional part is zero and the magnitude fits an i64.
        return Value::from(parsed as i64);
    }
    serde_json::Number::from_f64(parsed).map_or(Value::Null, Value::Number)
}

/// A select event's value, as every `select` in the grammar carries it: an
/// array of the ids selected. A row with an `id` selects that id; a row
/// without one selects its index.
/// What a row click selects: the row's `id_field` when the table names one
/// (plan-koine-followups F-D2), else its `id`, else its index.
fn selection_of(row: &Value, id_field: Option<&str>, index: usize) -> Value {
    let id = row
        .get(id_field.unwrap_or("id"))
        .filter(|id| id.is_string() || id.is_number())
        .cloned()
        .unwrap_or_else(|| Value::from(index));
    Value::Array(vec![id])
}

/// A JSON value as the plain text a cell or an attribute shows.
fn display(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn cell(row: &Value, column: &str) -> String {
    row.get(column).map(display).unwrap_or_default()
}

/// A list row: a string as is; an object by its `title`, `name` or `label`;
/// anything else as JSON.
fn row_text(row: &Value) -> String {
    if let Some(object) = row.as_object() {
        for key in ["title", "name", "label"] {
            if let Some(text) = object.get(key).and_then(Value::as_str) {
                return text.to_string();
            }
        }
    }
    display(row)
}

fn lines_of(lines: &Value) -> String {
    match lines {
        Value::Array(items) => items.iter().map(display).collect::<Vec<_>>().join("\n"),
        other => display(other),
    }
}

/// `kv` pairs: an object, or an array of `[key, value]`.
fn pairs_of(pairs: &Value) -> Vec<(String, String)> {
    match pairs {
        Value::Object(map) => map.iter().map(|(k, v)| (k.clone(), display(v))).collect(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                let pair = item.as_array()?;
                Some((display(pair.first()?), display(pair.get(1)?)))
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A select option is a value, or `{value, label}`.
fn choice_value(choice: &Value) -> &Value {
    choice.get("value").unwrap_or(choice)
}

fn choice_label(choice: &Value) -> String {
    choice
        .get("label")
        .map(display)
        .unwrap_or_else(|| display(choice_value(choice)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_select_carries_an_array_of_ids_as_every_select_does() {
        assert_eq!(selection_of(&json!({"id": "p1"}), None, 4), json!(["p1"]));
        assert_eq!(selection_of(&json!("alpha"), None, 4), json!([4]));
        // A table that names its id field selects by it (F-D2).
        assert_eq!(
            selection_of(&json!({"id": "p1", "bibcode": "2024A"}), Some("bibcode"), 4),
            json!(["2024A"])
        );
        assert_eq!(
            selection_of(&json!({"id": "p1"}), Some("bibcode"), 4),
            json!([4])
        );
    }

    #[test]
    fn a_diff_line_is_classed_by_its_prefix() {
        assert_eq!(diff_class("@@ -1,2 +1,2 @@"), "koine-diff-hunk");
        assert_eq!(diff_class("+added"), "koine-diff-add");
        assert_eq!(diff_class("-removed"), "koine-diff-del");
        assert_eq!(diff_class(" context"), "koine-diff-ctx");
        assert_eq!(diff_class(""), "koine-diff-ctx");
    }

    #[test]
    fn a_number_field_sends_a_json_number() {
        assert_eq!(number_of("16"), json!(16));
        assert_eq!(number_of("2.5"), json!(2.5));
        assert_eq!(number_of(""), Value::Null);
    }

    #[test]
    fn a_select_option_keeps_its_json_type() {
        assert_eq!(
            choice_value(&json!({"value": 3, "label": "three"})),
            &json!(3)
        );
        assert_eq!(
            choice_label(&json!({"value": 3, "label": "three"})),
            "three"
        );
        assert_eq!(choice_label(&json!("a")), "a");
    }
}
