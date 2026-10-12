//! The D2 table as code: one element per [`RenderKind`], the node id on the
//! DOM, the event each gesture sends.
//!
//! Nothing here computes. A value is read from the node and written to an
//! attribute, and an interaction is turned into an [`Event`] and handed to
//! the host. What an event means is `impress_surface::reduce`'s.
//!
//! Focus: a `field`, `button`, `table`, `list`, `tree` or `tabs` node is one tab stop
//! (`tabindex="0"`), in the order the tree's `focus_order` lists them, so the
//! DOM's tab order is the tree's. Rows and tab buttons are clicked, not
//! tabbed: they carry `tabindex="-1"` so they stay out of that order.

use dioxus::prelude::*;
use impress_surface::resolve::RenderTab;
use impress_surface::tree::TreeNode;
use impress_surface::{Event, EventKind, RenderKind, RenderNode};
use serde_json::Value;

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
        RenderKind::Table {
            rows,
            columns,
            id_field,
        } => table_node(id, label, help, rows, columns, id_field.as_deref(), ctx),
        RenderKind::List { rows } => list_node(id, label, help, rows, ctx),
        RenderKind::Tree { nodes, .. } => tree_node(id, label, help, nodes, ctx),
        RenderKind::Plot { spec } => {
            let spec_text = spec.to_string();
            match koine_plot::render_svg_json(spec) {
                Ok(svg) => rsx! {
                    div {
                        class: class("koine-plot", None, ctx, id),
                        "data-koine-id": "{id}",
                        "data-koine-focused": focused,
                        "data-koine-spec": spec_text,
                        aria_label: label,
                        title: help,
                        dangerous_inner_html: svg,
                    }
                },
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
        RenderKind::Code { language, text } => {
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
                        } else {
                            "{text}"
                        }
                    }
                }
            }
        }
        RenderKind::Image { blob, url } => {
            if let Some(src) = url.as_ref().and_then(Value::as_str) {
                rsx! {
                    img {
                        class: class("koine-image", None, ctx, id),
                        "data-koine-id": "{id}",
                        "data-koine-focused": focused,
                        src: "{src}",
                        alt: label.clone().unwrap_or_default(),
                        title: help,
                    }
                }
            } else {
                let blob = blob.as_ref().map_or("none".to_string(), display);
                rsx! {
                    div {
                        class: class("koine-placeholder", None, ctx, id),
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

fn table_node(
    id: &str,
    label: Option<String>,
    help: Option<String>,
    rows: &Value,
    columns: &[String],
    id_field: Option<&str>,
    ctx: &Ctx<'_>,
) -> Element {
    let focused = is_focused(ctx, id).then_some("true");
    let rows: &[Value] = rows.as_array().map_or(&[], Vec::as_slice);
    rsx! {
        table {
            class: class("koine-table", None, ctx, id),
            tabindex: 0,
            "data-koine-id": "{id}",
            "data-koine-focused": focused,
            aria_label: label,
            title: help,
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
                        let selected = selection_of(row, id_field, index);
                        rsx! {
                            tr {
                                key: "{index}",
                                tabindex: -1,
                                "data-koine-row": "{index}",
                                onclick: move |_| on_event.call(event(&widget, EventKind::Select, selected.clone())),
                                for column in columns {
                                    td { key: "{column}", "{cell(row, column)}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn list_node(
    id: &str,
    label: Option<String>,
    help: Option<String>,
    rows: &Value,
    ctx: &Ctx<'_>,
) -> Element {
    let focused = is_focused(ctx, id).then_some("true");
    let rows: &[Value] = rows.as_array().map_or(&[], Vec::as_slice);
    rsx! {
        ul {
            class: class("koine-list", None, ctx, id),
            tabindex: 0,
            "data-koine-id": "{id}",
            "data-koine-focused": focused,
            aria_label: label,
            title: help,
            for (index , row) in rows.iter().enumerate() {
                {
                    let on_event = ctx.on_event;
                    let widget = id.to_string();
                    let selected = selection_of(row, None, index);
                    rsx! {
                        li {
                            key: "{index}",
                            tabindex: -1,
                            "data-koine-row": "{index}",
                            onclick: move |_| on_event.call(event(&widget, EventKind::Select, selected.clone())),
                            "{row_text(row)}"
                        }
                    }
                }
            }
        }
    }
}

/// A `tree`: the visible outline the resolver computed, one `li` per node.
/// A disclosure click sends `change` with the new expanded list (this node's
/// id toggled in the ids the visible nodes show expanded); a label click
/// sends `select` with `[id]`. Rows are clicked, not tabbed, as a table's are.
fn tree_node(
    id: &str,
    label: Option<String>,
    help: Option<String>,
    nodes: &[TreeNode],
    ctx: &Ctx<'_>,
) -> Element {
    let focused = is_focused(ctx, id).then_some("true");
    let expanded: Vec<Value> = nodes
        .iter()
        .filter(|n| n.expanded)
        .map(|n| n.id.clone())
        .collect();
    rsx! {
        ul {
            class: class("koine-tree", None, ctx, id),
            tabindex: 0,
            "data-koine-id": "{id}",
            "data-koine-focused": focused,
            aria_label: label,
            title: help,
            for (index , node) in nodes.iter().enumerate() {
                {
                    let on_event = ctx.on_event;
                    let widget = id.to_string();
                    let selected = Value::Array(vec![node.id.clone()]);
                    let toggled = toggled(&expanded, &node.id);
                    let widget_for_toggle = widget.clone();
                    let is_open = node.expanded;
                    let indent = format!("padding-left: calc({} * 1.25em)", node.depth);
                    rsx! {
                        li {
                            key: "{index}",
                            class: "koine-tree-node",
                            style: indent,
                            tabindex: -1,
                            "data-koine-depth": "{node.depth}",
                            "data-koine-row": "{index}",
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
                            span {
                                class: "koine-tree-label",
                                onclick: move |_| on_event.call(event(&widget, EventKind::Select, selected.clone())),
                                "{node.label}"
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The expanded list with `id` toggled: removed when it is there, else
/// appended.
pub(crate) fn toggled(expanded: &[Value], id: &Value) -> Value {
    let mut out: Vec<Value> = expanded.iter().filter(|e| *e != id).cloned().collect();
    if out.len() == expanded.len() {
        out.push(id.clone());
    }
    Value::Array(out)
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
