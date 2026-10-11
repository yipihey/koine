use impress_surface::{resolve, RenderTree};
use koine::style::{StyleSheet, REQUIRED_SLOTS};
use serde_json::{json, Value};

use crate::{css_variables, render_html};

fn web() -> StyleSheet {
    StyleSheet::default_for("web").unwrap()
}

fn tree(root: Value, focus_order: Value) -> RenderTree {
    serde_json::from_value(json!({ "root": root, "focus_order": focus_order })).unwrap()
}

/// One node of every `RenderKind`, plus every field kind and a status of each
/// level. Ids are plain so a substring count is an exact count.
fn everything() -> RenderTree {
    let focus = [
        "t-tabs", "t-table", "t-list", "f-text", "f-number", "f-slider", "f-select", "f-toggle",
        "f-date", "f-color", "b-go",
    ];
    let root = json!({
        "id": "root",
        "node": { "kind": "column", "items": [
            { "id": "row", "node": { "kind": "row", "items": [
                { "id": "b-go", "node": { "kind": "button", "label": "Go" } },
                { "id": "spacer", "node": { "kind": "spacer" } },
            ]}},
            { "id": "grid", "node": { "kind": "grid", "columns": 3, "items": [
                { "id": "g-text", "node": { "kind": "text", "text": "in a grid" } },
            ]}},
            { "id": "section", "label": "A section", "node": {
                "kind": "section", "title": "Hidden", "collapsed": true,
                "body": { "id": "s-body", "node": { "kind": "text", "text": "kept in the DOM" } }
            }},
            { "id": "t-tabs", "node": { "kind": "tabs", "tabs": [
                { "title": "One", "body": { "id": "tab-1", "node": { "kind": "text", "text": "first" } } },
                { "title": "Two", "body": { "id": "tab-2", "node": { "kind": "text", "text": "second" } } },
            ]}},
            { "id": "t-table", "node": { "kind": "table", "columns": ["title", "year"], "rows": [
                { "id": "p1", "title": "A", "year": 2024 },
                { "id": "p2", "title": "B", "year": 2023 },
            ]}},
            { "id": "t-list", "node": { "kind": "list", "rows": ["alpha", "beta"] } },
            { "id": "plot", "node": { "kind": "plot", "spec": { "x": [1, 2], "y": [3, 4] } } },
            { "id": "img-url", "node": { "kind": "image", "blob": null, "url": "https://example.test/a.png" } },
            { "id": "img-blob", "node": { "kind": "image", "blob": "b-1", "url": null } },
            { "id": "f-text", "label": "Name", "node": { "kind": "field", "field": { "text": {} }, "bind": "state.name", "value": "Ada" } },
            { "id": "f-number", "node": { "kind": "field", "field": { "number": {} }, "bind": "state.n", "value": 3 } },
            { "id": "f-slider", "node": { "kind": "field", "field": { "slider": { "min": 0, "max": 8, "step": 0.5 } }, "bind": "state.s", "value": 2 } },
            { "id": "f-select", "node": { "kind": "field", "field": { "select": { "options": ["a", "b"] } }, "bind": "state.o", "value": "b" } },
            { "id": "f-toggle", "node": { "kind": "field", "field": { "toggle": {} }, "bind": "state.t", "value": true } },
            { "id": "f-date", "node": { "kind": "field", "field": { "date": {} }, "bind": "state.d", "value": "2026-01-02" } },
            { "id": "f-color", "node": { "kind": "field", "field": { "color": {} }, "bind": "state.c", "value": "red" } },
            { "id": "st-error", "node": { "kind": "status", "level": "error", "message": "bad" } },
            { "id": "st-warning", "node": { "kind": "status", "level": "warning", "message": "hmm" } },
            { "id": "st-info", "node": { "kind": "status", "level": "info", "message": "fyi" } },
            { "id": "log", "node": { "kind": "log", "lines": ["one", "two"] } },
            { "id": "kv", "node": { "kind": "kv", "pairs": { "a": 1, "b": "two" } } },
            { "id": "divider", "node": { "kind": "divider" } },
            { "id": "ph", "node": { "kind": "placeholder", "unknown_kind": "hologram", "reason": "this build has no 'hologram' widget" } },
        ]}
    });
    tree(root, json!(focus))
}

/// Every `(tag text)` of the page, in document order.
fn tags(html: &str) -> Vec<&str> {
    html.split('<')
        .filter_map(|chunk| chunk.split('>').next())
        .collect()
}

/// An attribute's value. Dioxus writes a number unquoted (`tabindex=0`).
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{name}=");
    let start = tag.find(&format!(" {key}"))? + key.len() + 1;
    let rest = &tag[start..];
    match rest.strip_prefix('"') {
        Some(quoted) => Some(&quoted[..quoted.find('"')?]),
        None => Some(rest.split(' ').next().unwrap_or(rest)),
    }
}

/// The `data-koine-id` of every element that is a tab stop, in DOM order.
fn tab_stops(html: &str) -> Vec<String> {
    tags(html)
        .into_iter()
        .filter(|tag| attr(tag, "tabindex") == Some("0"))
        .filter_map(|tag| attr(tag, "data-koine-id").map(str::to_string))
        .collect()
}

fn ids_of(node: &Value, out: &mut Vec<String>) {
    match node {
        Value::Object(map) => {
            if let (Some(Value::String(id)), Some(Value::Object(_))) =
                (map.get("id"), map.get("node"))
            {
                out.push(id.clone());
            }
            for value in map.values() {
                ids_of(value, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| ids_of(v, out)),
        _ => {}
    }
}

#[test]
fn every_kind_renders_and_every_id_is_on_the_dom_exactly_once() {
    let tree = everything();
    let html = render_html(&tree, &web(), None);

    let mut ids = Vec::new();
    ids_of(&serde_json::to_value(&tree.root).unwrap(), &mut ids);
    assert!(ids.len() >= 25, "{ids:?}");
    for id in &ids {
        let needle = format!("data-koine-id=\"{id}\"");
        assert_eq!(html.matches(&needle).count(), 1, "{id}\n{html}");
    }
    let total = html.matches("data-koine-id=").count();
    assert_eq!(total, ids.len(), "{html}");

    // The element for each kind, per the D2 table.
    for fragment in [
        "<section",
        "<h2",
        "role=\"tablist\"",
        "role=\"tab\"",
        "role=\"tabpanel\"",
        "<table",
        "<thead",
        "<ul",
        "<li",
        "<dl",
        "<pre",
        "<hr",
        "<button",
        "type=\"range\"",
        "type=\"checkbox\"",
        "type=\"date\"",
        "type=\"number\"",
        "<select",
        "role=\"alert\"",
        "role=\"status\"",
        "koine-grid",
        "repeat(3, minmax(0, 1fr))",
        "koine-spacer",
        "koine-plot",
        "data-koine-spec=",
        "src=\"https://example.test/a.png\"",
        "placeholder image: blob b-1",
        "data-koine-field-kind=\"color\"",
        "<label for=\"koine-f-text\"",
    ] {
        assert!(html.contains(fragment), "missing {fragment}\n{html}");
    }
}

#[test]
fn a_collapsed_section_keeps_its_body_in_the_dom_hidden() {
    let html = render_html(&everything(), &web(), None);
    assert!(html.contains("kept in the DOM"));
    let body = tags(&html)
        .into_iter()
        .find(|tag| tag.contains("koine-section-body"))
        .unwrap();
    assert!(body.contains("hidden"), "{body}");
    // The second tab's panel is hidden and present.
    assert!(html.contains("second"));
}

#[test]
fn the_dom_tab_order_is_the_focus_order_of_a_resolved_surface() {
    let spec = impress_surface::example_paper_triage();
    let papers = json!({ "papers": [
        { "id": "11111111-1111-1111-1111-111111111111", "title": "A dark matter survey", "year": 2024, "author_text": "Zwicky, F." },
        { "id": "22222222-2222-2222-2222-222222222222", "title": "Signal processing notes", "year": 2023, "author_text": "Shannon, C." },
    ]});
    let tree = resolve(&spec, &json!({ "selected": [] }), &json!({}), &papers);
    assert!(tree.focus_order.len() >= 4, "{:?}", tree.focus_order);
    let html = render_html(&tree, &web(), None);
    assert_eq!(tab_stops(&html), tree.focus_order, "{html}");
    assert!(html.contains("A dark matter survey"), "{html}");

    let spec = impress_surface::example_signal_explorer();
    let tree = resolve(
        &spec,
        &json!({ "freq": 2.0, "bins": 16 }),
        &json!({}),
        &json!({}),
    );
    let html = render_html(&tree, &web(), None);
    assert_eq!(tab_stops(&html), tree.focus_order, "{html}");

    let tree = everything();
    let html = render_html(&tree, &web(), None);
    let mut expected = tree.focus_order.clone();
    // `everything` lists its focus order by hand; the DOM's is document order.
    expected.sort_by_key(|id| html.find(&format!("data-koine-id=\"{id}\"")));
    assert_eq!(tab_stops(&html), expected);
}

#[test]
fn the_focused_id_is_marked_and_nothing_else_is() {
    let tree = everything();
    let html = render_html(&tree, &web(), Some("b-go"));
    assert_eq!(html.matches("data-koine-focused=").count(), 1, "{html}");
    let tag = tags(&html)
        .into_iter()
        .find(|tag| attr(tag, "data-koine-id") == Some("b-go"))
        .unwrap();
    assert_eq!(attr(tag, "data-koine-focused"), Some("true"), "{tag}");
    assert!(
        attr(tag, "class").unwrap().contains("koine-focused"),
        "{tag}"
    );

    let none = render_html(&tree, &web(), None);
    assert!(!none.contains("data-koine-focused"));
    assert!(!none.contains("koine-focused"));
}

#[test]
fn a_placeholder_names_its_unknown_kind_and_reason_in_the_tuis_words() {
    let html = render_html(&everything(), &web(), None);
    assert!(
        html.contains("placeholder hologram: this build has no &#39;hologram&#39; widget")
            || html.contains("placeholder hologram: this build has no 'hologram' widget"),
        "{html}"
    );

    let bare = tree(
        json!({ "id": "x", "node": { "kind": "placeholder", "reason": "source 'libs' failed" } }),
        json!([]),
    );
    let html = render_html(&bare, &web(), None);
    assert!(html.contains("placeholder placeholder: source"), "{html}");

    // And an unknown kind through resolve, end to end.
    let spec: impress_surface::SurfaceSpec = serde_json::from_value(json!({
        "surface": "1.0", "name": "n",
        "root": { "id": "u", "hologram": { "depth": 3 } }
    }))
    .unwrap();
    let tree = resolve(&spec, &json!({}), &json!({}), &json!({}));
    let html = render_html(&tree, &web(), None);
    assert!(html.contains("placeholder hologram"), "{html}");
}

#[test]
fn every_required_slot_changes_the_output_and_automatic_is_no_class() {
    let tree = everything();
    let base_sheet = web();
    let base_vars = css_variables(&base_sheet);
    let base_html = render_html(&tree, &base_sheet, None);
    assert!(!base_html.contains("koine-font-"), "automatic is no class");

    let changes: [(&str, &str); 12] = [
        ("column", "13"),
        ("row", "13"),
        ("text", "mono"),
        ("section", "title"),
        ("button", "accent"),
        ("field", "mono"),
        ("list", "5"),
        ("table", "5"),
        ("status.error", "primary"),
        ("status.warning", "secondary"),
        ("status.info", "accent"),
        ("focus", "error"),
    ];
    let named: Vec<&str> = changes.iter().map(|(slot, _)| *slot).collect();
    assert_eq!(named, REQUIRED_SLOTS, "a slot was added without a test");

    for (slot, value) in changes {
        let mut sheet = web();
        sheet.set(slot, value).unwrap();
        let vars = css_variables(&sheet);
        let html = render_html(&tree, &sheet, None);
        assert!(
            vars != base_vars || html != base_html,
            "slot {slot} = {value} changed nothing"
        );
        sheet.reset(slot).unwrap();
        assert_eq!(css_variables(&sheet), base_vars, "{slot} resets");
    }

    let mut sheet = web();
    sheet.set("text", "mono").unwrap();
    assert!(render_html(&tree, &sheet, None).contains("koine-font-mono"));
    sheet.set("text", "automatic").unwrap();
    assert!(!render_html(&tree, &sheet, None).contains("koine-font-"));
}

#[test]
fn the_sheet_is_written_as_custom_properties() {
    let mut sheet = web();
    sheet.set("column", "12").unwrap();
    sheet.set("button", "accent").unwrap();
    sheet.set("focus", "error").unwrap();
    let vars = css_variables(&sheet);
    assert!(vars.contains("--koine-column: 12px;"), "{vars}");
    assert!(vars.contains("--koine-list: 0px;"), "{vars}");
    assert!(
        vars.contains("--koine-button: var(--koine-color-accent);"),
        "{vars}"
    );
    assert!(
        vars.contains("--koine-status-error: var(--koine-color-error);"),
        "{vars}"
    );
    assert!(
        vars.contains("--koine-focus: var(--koine-color-error);"),
        "{vars}"
    );
    // Fonts are classes and `automatic` is nothing.
    assert!(!vars.contains("--koine-text"), "{vars}");
    let default = css_variables(&web());
    assert!(!default.contains("--koine-button"), "{default}");
    assert!(!default.contains("--koine-focus"), "{default}");

    let html = render_html(&everything(), &sheet, None);
    assert!(html.contains("--koine-column: 12px;"));
    // The stylesheet reads what the mapping writes.
    for name in [
        "--koine-column",
        "--koine-button",
        "--koine-focus",
        "--koine-status-error",
    ] {
        assert!(
            crate::STYLESHEET.contains(name),
            "koine.css never reads {name}"
        );
    }
}

#[test]
fn a_sheet_for_another_frontend_is_refused() {
    let tui = StyleSheet::default_for("tui").unwrap();
    assert_eq!(css_variables(&tui), "");
    let html = render_html(&everything(), &tui, None);
    assert!(html.contains("koine-placeholder"), "{html}");
    assert!(html.contains("placeholder style"), "{html}");
    assert!(html.contains("&#39;tui&#39;"), "{html}");
    assert!(!html.contains("data-koine-id"), "{html}");
}
