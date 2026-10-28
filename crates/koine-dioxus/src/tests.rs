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
        "t-tabs", "t-table", "t-list", "t-tree", "t-graph", "f-text", "f-number", "f-slider",
        "f-select", "f-toggle", "f-date", "f-color", "b-go",
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
            { "id": "t-tree", "node": { "kind": "tree", "bind": "state.open", "nodes": [
                { "id": "a", "label": "src", "depth": 0, "has_children": true, "expanded": true, "row": { "id": "a" } },
                { "id": "b", "label": "main.rs", "depth": 1, "has_children": false, "expanded": false, "row": { "id": "b", "parent": "a" } },
                { "id": 7, "label": "docs", "depth": 0, "has_children": true, "expanded": false, "row": { "id": 7 } },
            ]}},
            { "id": "t-graph", "node": { "kind": "graph",
                "spec": { "nodes": [{ "id": "n1", "label": "one" }, { "id": "n2" }], "edges": [{ "source": "n1", "target": "n2" }] },
                "svg": "<svg viewBox=\"0 0 100 50\"><g class=\"koine-graph-node\" data-id=\"n1\"><circle cx=\"10\" cy=\"10\" r=\"4\"/></g><g class=\"koine-graph-node\" data-id=\"n2\"><circle cx=\"50\" cy=\"20\" r=\"4\"/></g></svg>" } },
            { "id": "plot", "node": { "kind": "plot", "spec": { "series": [{ "x": [1, 2], "y": [3, 4] }] } } },
            { "id": "plot-bad", "node": { "kind": "plot", "spec": { "series": [{ "x": [1, 2] }] } } },
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
            { "id": "hl", "node": { "kind": "code", "language": "rust", "text": "fn a() {}\nlet b = 1;",
                "spans": [[["keyword", "fn"], ["", " "], ["function", "a"], ["punctuation", "() {}"]], [["type", "let"], ["", " b = "], ["constant", "1"], ["punctuation", ";"]]] } },
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
        "<details",
        "<summary",
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
        "koine-graph",
        "koine-hl-keyword",
        "data-koine-spec=",
        "src=\"https://example.test/a.png\"",
        "placeholder image: blob b-1",
        "data-koine-field-kind=\"color\"",
        "<label for=\"koine-f-text\"",
    ] {
        assert!(html.contains(fragment), "missing {fragment}\n{html}");
    }
}

/// The tag of the element carrying `data-koine-id="id"` and the html after it.
fn element<'a>(html: &'a str, id: &str) -> (&'a str, &'a str) {
    let at = html.find(&format!("data-koine-id=\"{id}\"")).unwrap();
    let start = html[..at].rfind('<').unwrap();
    let open_end = at + html[at..].find('>').unwrap();
    (&html[start..open_end], &html[open_end + 1..])
}

#[test]
fn a_tree_is_a_list_of_indented_nodes_with_a_disclosure_per_branch() {
    let html = render_html(&everything(), &web(), None);
    let (tag, rest) = element(&html, "t-tree");
    assert!(tag.contains("koine-tree"), "{tag}");
    assert_eq!(attr(tag, "tabindex"), Some("0"));
    let end = rest.find("</ul>").unwrap();
    let tree_html = &rest[..end];
    // One li per visible node, depth and row on each.
    assert_eq!(tree_html.matches("<li").count(), 3, "{tree_html}");
    assert_eq!(tree_html.matches("data-koine-depth=\"0\"").count(), 2);
    assert_eq!(tree_html.matches("data-koine-depth=\"1\"").count(), 1);
    assert!(
        tree_html.contains("padding-left: calc(1 * 1.25em)"),
        "{tree_html}"
    );
    // A disclosure for each branch (two), none for the leaf; the expanded
    // one says so.
    assert_eq!(tree_html.matches("koine-tree-disclosure").count(), 2);
    assert_eq!(tree_html.matches("aria-expanded=\"true\"").count(), 1);
    assert_eq!(tree_html.matches("aria-expanded=\"false\"").count(), 1);
    assert!(tree_html.contains("\u{25be}") && tree_html.contains("\u{25b8}"));
    for label in ["src", "main.rs", "docs"] {
        assert!(tree_html.contains(&format!(">{label}</span>")), "{label}");
    }
}

#[test]
fn a_disclosure_sends_the_whole_expanded_list_with_one_id_changed() {
    use crate::outline::set_open;
    use serde_json::json;
    let all = [json!("a"), json!("hidden")];
    assert_eq!(set_open(&all, &json!(7), true), json!(["a", "hidden", 7]));
    assert_eq!(set_open(&all, &json!("a"), false), json!(["hidden"]));
    assert_eq!(set_open(&[], &json!("a"), true), json!(["a"]));
}

/// A tree with every outline field: counts, a menu, an expanded list that
/// keeps a hidden branch, and a table and a list with counts and menus.
fn outline() -> RenderTree {
    tree(
        json!({ "id": "root", "node": { "kind": "column", "items": [
            { "id": "side", "node": { "kind": "tree", "bind": "state.open",
                "expanded": ["a", "hidden"], "actions": ["Rename", "Delete"], "movable": true,
                "nodes": [
                    { "id": "a", "label": "src", "depth": 0, "has_children": true, "expanded": true, "count": 3, "row": { "id": "a" } },
                    { "id": "b", "label": "main.rs", "depth": 1, "has_children": false, "expanded": false, "row": { "id": "b" } },
                    { "id": 7, "label": "docs", "depth": 0, "has_children": true, "expanded": false, "count": 0, "row": { "id": 7 } },
                ]}},
            { "id": "tbl", "node": { "kind": "table", "columns": ["title", "year"], "actions": ["Open"],
                "count_field": "n", "rows": [
                    { "id": "p1", "title": "A", "year": 2024, "n": 5 },
                    { "id": "p2", "title": "B", "year": 2023, "n": null },
                ]}},
            { "id": "lst", "node": { "kind": "list", "actions": ["Pin"], "count_field": "n",
                "rows": [{ "id": "x", "title": "alpha", "n": 9 }, "beta"] } },
        ]}}),
        json!(["side", "tbl", "lst"]),
    )
}

fn row(widget: &str, id: Value) -> Option<crate::RowRef> {
    Some((widget.to_string(), id))
}

fn with_active(pairs: &[(&str, Value)]) -> crate::RowView {
    crate::RowView {
        active: pairs
            .iter()
            .map(|(w, id)| (w.to_string(), id.clone()))
            .collect(),
        ..Default::default()
    }
}

#[test]
fn the_active_row_of_a_tree_a_table_and_a_list_is_marked_and_no_other() {
    let view = with_active(&[("side", json!(7)), ("tbl", json!("p2")), ("lst", json!(1))]);
    let html = crate::render_html_with(&outline(), &web(), None, &view);
    assert_eq!(
        html.matches("data-koine-active=\"true\"").count(),
        3,
        "{html}"
    );
    assert_eq!(
        html.matches("koine-active").count(),
        6,
        "attribute + class each"
    );
    for tag in tags(&html) {
        if attr(tag, "data-koine-active").is_some() {
            let class = attr(tag, "class").unwrap_or_default();
            assert!(class.contains("koine-active"), "{tag}");
            let at = attr(tag, "data-koine-row").unwrap_or_default();
            if tag.starts_with("li") && class.contains("koine-tree-node") {
                assert_eq!(attr(tag, "data-koine-depth"), Some("0"));
                assert_eq!(at, "2", "the node with id 7 is the third row");
            }
        }
    }
    // With nothing active, nothing is marked.
    let html = render_html(&outline(), &web(), None);
    assert!(!html.contains("koine-active") && !html.contains("data-koine-active"));
    // An id the widget does not have marks nothing.
    let html = crate::render_html_with(
        &outline(),
        &web(),
        None,
        &with_active(&[("side", json!("gone"))]),
    );
    assert!(!html.contains("data-koine-active"));
}

#[test]
fn a_count_is_a_span_after_the_label_and_zero_is_a_count() {
    let html = render_html(&outline(), &web(), None);
    // Tree: 3 and 0, none on the leaf; table: 5 (not the null); list: 9.
    assert_eq!(html.matches("class=\"koine-count\"").count(), 4, "{html}");
    assert!(html.contains(">src</span><span class=\"koine-count\">3</span>"));
    assert!(html.contains(">docs</span><span class=\"koine-count\">0</span>"));
    assert!(
        html.contains(">A<span class=\"koine-count\">5</span>"),
        "{html}"
    );
    assert!(html.contains(">alpha<span class=\"koine-count\">9</span>"));
    // No field, no span.
    let plain = everything();
    assert!(!render_html(&plain, &web(), None).contains("koine-count"));
}

#[test]
fn an_open_menu_lists_the_labels_inside_its_row_and_only_there() {
    let closed = render_html(&outline(), &web(), None);
    assert!(!closed.contains("koine-menu"), "{closed}");

    let view = crate::RowView {
        menu: row("side", json!("a")),
        ..Default::default()
    };
    let html = crate::render_html_with(&outline(), &web(), None, &view);
    assert_eq!(
        html.matches("<ul class=\"koine-menu\"").count(),
        1,
        "{html}"
    );
    assert_eq!(html.matches("koine-menu-item").count(), 2);
    for label in ["Rename", "Delete"] {
        assert!(html.contains(&format!(">{label}</li>")), "{label}");
    }
    // It is inside the first row's li (before the next li opens).
    let open = html.find("<ul class=\"koine-menu\"").unwrap();
    let rest = &html[open..];
    let close = rest.find("</ul>").unwrap();
    assert!(!rest[..close].contains("main.rs"));
    assert!(
        html[..open]
            .rfind("<li")
            .map(|i| html[i..open].contains("src"))
            == Some(true)
    );

    // A table and a list menu, on their own rows.
    for (widget, id, label) in [("tbl", json!("p1"), "Open"), ("lst", json!("x"), "Pin")] {
        let view = crate::RowView {
            menu: row(widget, id),
            ..Default::default()
        };
        let html = crate::render_html_with(&outline(), &web(), None, &view);
        assert_eq!(html.matches("koine-menu-item").count(), 1, "{widget}");
        assert!(html.contains(&format!(">{label}</li>")), "{widget}");
    }
    // A menu on a widget that declares no actions draws nothing.
    let view = crate::RowView {
        menu: row("t-tree", json!("a")),
        ..Default::default()
    };
    let html = crate::render_html_with(&everything(), &web(), None, &view);
    assert!(!html.contains("koine-menu"));
}

#[test]
fn a_row_being_renamed_shows_an_input_with_its_text_instead_of_its_label() {
    let view = crate::RowView {
        renaming: row("side", json!(7)),
        ..Default::default()
    };
    let html = crate::render_html_with(&outline(), &web(), None, &view);
    assert_eq!(html.matches("class=\"koine-rename\"").count(), 1, "{html}");
    let input = tags(&html)
        .into_iter()
        .find(|t| t.starts_with("input") && t.contains("koine-rename"))
        .unwrap();
    assert_eq!(attr(input, "value"), Some("docs"));
    assert_eq!(attr(input, "name"), Some("label"));
    // The label it replaces is gone; the others stay.
    assert!(!html.contains(">docs</span>"));
    assert!(html.contains(">src</span>") && html.contains(">main.rs</span>"));
    // Another widget's id is not renamed here.
    let view = crate::RowView {
        renaming: row("tbl", json!(7)),
        ..Default::default()
    };
    let html = crate::render_html_with(&outline(), &web(), None, &view);
    assert!(!html.contains("koine-rename"));
}

#[test]
fn a_movable_tree_has_a_grip_per_row_and_gaps_only_while_moving() {
    let html = render_html(&outline(), &web(), None);
    let side = &html
        [html.find("data-koine-id=\"side\"").unwrap()..html.find("data-koine-id=\"tbl\"").unwrap()];
    // One grip per row, after the label, hidden from assistive technology;
    // no HTML5 drag and drop anywhere (A-D1: one way to move a row).
    assert_eq!(
        side.matches("<span class=\"koine-grip\" aria-hidden=\"true\">\u{2261}</span>")
            .count(),
        3,
        "{side}"
    );
    assert!(!html.contains("draggable"), "{html}");
    assert!(!html.contains("koine-drop-gap"));
    assert!(!html.contains("data-koine-moving"));
    // A tree that does not declare on_move has no grip.
    assert!(!render_html(&everything(), &web(), None).contains("koine-grip"));

    // Moving the leaf `b`: a gap before each of the three rows plus one at
    // the end, none of which would put `b` under itself.
    let view = crate::RowView {
        dragging: row("side", json!("b")),
        ..Default::default()
    };
    let html = crate::render_html_with(&outline(), &web(), None, &view);
    assert_eq!(
        html.matches("class=\"koine-drop-gap\"").count(),
        4,
        "{html}"
    );
    assert!(html.contains("data-koine-moving=\"true\""));
    assert!(!html.contains("data-koine-drop="), "no target yet");
    // Moving `a` (which holds `b`): the gap before `b` would put `a` under
    // itself, so it is not drawn.
    let view = crate::RowView {
        dragging: row("side", json!("a")),
        ..Default::default()
    };
    let html = crate::render_html_with(&outline(), &web(), None, &view);
    assert_eq!(
        html.matches("class=\"koine-drop-gap\"").count(),
        3,
        "{html}"
    );
    assert!(!html.contains("data-koine-gap=\"1\""));
    // A move in another widget draws none.
    let view = crate::RowView {
        dragging: row("tbl", json!("p1")),
        ..Default::default()
    };
    let html = crate::render_html_with(&outline(), &web(), None, &view);
    assert!(!html.contains("koine-drop-gap"));
}

#[test]
fn the_row_or_gap_under_the_pointer_is_marked_as_the_drop_target() {
    let target = |at| crate::RowView {
        dragging: row("side", json!("b")),
        drop_target: Some(("side".to_string(), at)),
        ..Default::default()
    };
    // Over `docs` (row 2): that row, and only it.
    let html = crate::render_html_with(&outline(), &web(), None, &target(crate::DropAt::Row(2)));
    assert_eq!(
        html.matches("data-koine-drop=\"true\"").count(),
        1,
        "{html}"
    );
    let marked = tags(&html)
        .into_iter()
        .find(|t| t.contains("data-koine-drop"))
        .unwrap();
    assert_eq!(attr(marked, "data-koine-row"), Some("2"), "{marked}");
    // Over the gap at the end.
    let html = crate::render_html_with(&outline(), &web(), None, &target(crate::DropAt::Gap(3)));
    let marked = tags(&html)
        .into_iter()
        .find(|t| t.contains("data-koine-drop"))
        .unwrap();
    assert_eq!(attr(marked, "data-koine-gap"), Some("3"), "{marked}");
    // A target with no move in progress marks nothing.
    let view = crate::RowView {
        drop_target: Some(("side".to_string(), crate::DropAt::Row(0))),
        ..Default::default()
    };
    let html = crate::render_html_with(&outline(), &web(), None, &view);
    assert!(!html.contains("data-koine-drop"));
}

#[test]
fn a_tree_that_renames_and_deletes_lists_both_around_its_actions() {
    let mut tree = outline();
    if let impress_surface::RenderKind::Column { items } = &mut tree.root.node {
        if let impress_surface::RenderKind::Tree {
            actions,
            renameable,
            deletable,
            ..
        } = &mut items[0].node
        {
            *actions = vec!["Add child".into()];
            *renameable = true;
            *deletable = true;
        }
    }
    let view = crate::RowView {
        menu: row("side", json!("a")),
        ..Default::default()
    };
    let html = crate::render_html_with(&tree, &web(), None, &view);
    let items: Vec<&str> = html
        .split("class=\"koine-menu-item\" role=\"menuitem\">")
        .skip(1)
        .map(|rest| &rest[..rest.find("</li>").unwrap()])
        .collect();
    assert_eq!(items, vec!["Rename", "Add child", "Delete"], "{html}");
    // A tree with no actions but a rename handler still has a `…` button.
    if let impress_surface::RenderKind::Column { items } = &mut tree.root.node {
        if let impress_surface::RenderKind::Tree {
            actions, deletable, ..
        } = &mut items[0].node
        {
            actions.clear();
            *deletable = false;
        }
    }
    let html = render_html(&tree, &web(), None);
    let side = &html
        [html.find("data-koine-id=\"side\"").unwrap()..html.find("data-koine-id=\"tbl\"").unwrap()];
    assert_eq!(side.matches("class=\"koine-more\"").count(), 3, "{side}");
    // And none at all: no button.
    if let impress_surface::RenderKind::Column { items } = &mut tree.root.node {
        if let impress_surface::RenderKind::Tree { renameable, .. } = &mut items[0].node {
            *renameable = false;
        }
    }
    let html = render_html(&tree, &web(), None);
    let side = &html
        [html.find("data-koine-id=\"side\"").unwrap()..html.find("data-koine-id=\"tbl\"").unwrap()];
    assert!(!side.contains("koine-more"), "{side}");
}

#[test]
fn a_plot_is_drawn_as_svg_and_a_bad_spec_names_its_error() {
    let html = render_html(&everything(), &web(), None);

    let (tag, rest) = element(&html, "plot");
    assert!(attr(tag, "class").unwrap().contains("koine-plot"), "{tag}");
    assert!(!tag.contains("koine-plot-error"), "{tag}");
    assert!(attr(tag, "data-koine-spec").is_some(), "{tag}");
    assert!(rest.starts_with("<svg"), "{rest}");
    assert!(rest.contains("<polyline"), "{rest}");

    let (tag, rest) = element(&html, "plot-bad");
    assert!(
        attr(tag, "class").unwrap().contains("koine-plot-error"),
        "{tag}"
    );
    assert!(rest.starts_with("plot spec: "), "{rest}");
    assert!(rest.contains("missing field"), "{rest}");
    assert!(
        !rest[..rest.find("</div>").unwrap()].contains("<svg"),
        "{rest}"
    );
    assert_eq!(html.matches("koine-plot-error").count(), 1, "{html}");
}

#[test]
fn a_collapsed_section_is_a_closed_disclosure_that_keeps_its_body() {
    let html = render_html(&everything(), &web(), None);
    assert!(html.contains("kept in the DOM"));
    let section = tags(&html)
        .into_iter()
        .find(|tag| tag.starts_with("details") && tag.contains("koine-section"))
        .unwrap();
    // Closed, but openable: the heading is its summary.
    assert!(!section.contains(" open"), "{section}");
    assert!(html.contains("<summary"), "{html}");
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

#[test]
fn a_file_field_is_an_input_of_type_file_with_its_accept_and_ids() {
    let held = tree(
        json!({ "id": "up", "label": "Data", "node": {
            "kind": "field", "field": { "file": { "accept": ".csv,.xlsx" } },
            "bind": "state.file", "value": null
        }}),
        json!(["up"]),
    );
    let html = render_html(&held, &web(), None);
    let (tag, _) = element(&html, "up");
    assert!(tag.starts_with("<input"), "{tag}");
    assert_eq!(attr(tag, "type"), Some("file"), "{tag}");
    assert_eq!(attr(tag, "accept"), Some(".csv,.xlsx"), "{tag}");
    assert_eq!(attr(tag, "data-koine-bind"), Some("state.file"), "{tag}");
    assert_eq!(attr(tag, "id"), Some("koine-up"), "{tag}");
    assert_eq!(attr(tag, "tabindex"), Some("0"), "{tag}");
    assert!(!tag.contains("data-koine-file"), "{tag}");
    assert!(html.contains("<label for=\"koine-up\""), "{html}");
    assert_eq!(tab_stops(&html), vec!["up".to_string()]);

    // With a blob reference held, the field says which file it sent.
    let sent = tree(
        json!({ "id": "up", "node": {
            "kind": "field", "field": { "file": {} }, "bind": "state.file",
            "value": { "blob": { "item": "s1", "name": "data.csv" } }
        }}),
        json!(["up"]),
    );
    let html = render_html(&sent, &web(), None);
    let (tag, _) = element(&html, "up");
    assert_eq!(attr(tag, "data-koine-file"), Some("data.csv"), "{tag}");
    assert!(!tag.contains("accept"), "{tag}");
}

#[test]
fn a_diff_is_one_classed_span_per_line_and_a_plain_block_is_its_text() {
    let tree = tree(
        json!({ "id": "root", "node": { "kind": "column", "items": [
            { "id": "patch", "node": { "kind": "code", "language": "diff",
                "text": "@@ -1,2 +1,2 @@\n context\n-old\n+new\n" } },
            { "id": "src", "node": { "kind": "code", "language": "rust", "text": "fn main() {}\n" } },
            { "id": "bare", "node": { "kind": "code", "text": "just text" } },
        ]}}),
        json!([]),
    );
    let html = render_html(&tree, &web(), None);

    let (tag, rest) = element(&html, "patch");
    assert!(tag.starts_with("<pre"), "{tag}");
    assert!(attr(tag, "class").unwrap().contains("koine-code"), "{tag}");
    assert_eq!(attr(tag, "data-language"), Some("diff"), "{tag}");
    let block = &rest[..rest.find("</pre>").unwrap()];
    assert!(block.starts_with("<code"), "{block}");
    assert_eq!(block.matches("<span").count(), 4, "{block}");
    for (class, line) in [
        ("koine-diff-hunk", "@@ -1,2 +1,2 @@"),
        ("koine-diff-ctx", " context"),
        ("koine-diff-del", "-old"),
        ("koine-diff-add", "+new"),
    ] {
        let wanted = format!("class=\"{class}\">{line}\n</span>");
        assert!(block.contains(&wanted), "{wanted} not in {block}");
    }

    let (tag, rest) = element(&html, "src");
    assert_eq!(attr(tag, "data-language"), Some("rust"), "{tag}");
    let block = &rest[..rest.find("</pre>").unwrap()];
    assert!(block.contains("language-rust"), "{block}");
    assert!(block.contains("fn main() {}"), "{block}");
    assert!(!block.contains("<span"), "{block}");

    let (tag, rest) = element(&html, "bare");
    assert!(!tag.contains("data-language"), "{tag}");
    assert!(rest.contains("just text"), "{rest}");
    assert_eq!(html.matches("data-koine-id=").count(), 4, "{html}");
}

#[test]
fn a_bound_plot_is_drawn_as_svg_and_a_table_reads_an_envelope() {
    let spec: impress_surface::SurfaceSpec = serde_json::from_value(json!({
        "surface": "1.0", "name": "n",
        "state": { "result": {
            "rows": [{ "t": 0, "a": 1.0 }, { "t": 1, "a": 2.5 }, { "t": 2, "a": 2.0 }],
            "next": "c2"
        }},
        "root": { "column": [
            { "id": "p", "plot": { "rows": "{{state.result}}", "x": "t", "y": "a" } },
            { "id": "tbl", "table": { "rows": "{{state.result}}", "columns": [] } },
        ]}
    }))
    .unwrap();
    let tree = resolve(&spec, &spec.state, &json!({}), &json!({}));
    let html = render_html(&tree, &web(), None);
    let (tag, rest) = element(&html, "p");
    assert!(attr(tag, "class").unwrap().contains("koine-plot"), "{tag}");
    assert!(!tag.contains("koine-plot-error"), "{tag}");
    assert!(rest.starts_with("<svg"), "{rest}");
    assert!(rest.contains("<polyline"), "{rest}");
    // The table has the envelope's rows, and the first row's keys as columns.
    assert_eq!(html.matches("data-koine-row=").count(), 3, "{html}");
    assert!(html.contains("<th scope=\"col\">a</th>"), "{html}");
    assert!(html.contains("<th scope=\"col\">t</th>"), "{html}");
}

#[test]
fn a_graph_is_its_svg_in_a_tab_stop_with_a_data_id_per_node() {
    let html = render_html(&everything(), &web(), None);
    let (tag, rest) = element(&html, "t-graph");
    assert!(attr(tag, "class").unwrap().contains("koine-graph"), "{tag}");
    assert_eq!(attr(tag, "tabindex"), Some("0"), "{tag}");
    // The click handler is attached through the VDOM, not written to the html.
    assert!(!tag.contains("onclick"), "{tag}");
    let drawing = &rest[..rest.find("</div>").unwrap()];
    assert!(drawing.starts_with("<svg"), "{drawing}");
    for id in ["n1", "n2"] {
        assert!(drawing.contains(&format!("data-id=\"{id}\"")), "{drawing}");
    }
    assert_eq!(drawing.matches("koine-graph-node").count(), 2, "{drawing}");
}

#[test]
fn spans_become_classed_elements_and_plain_text_a_bare_node() {
    let html = render_html(&everything(), &web(), None);
    let (tag, rest) = element(&html, "hl");
    assert_eq!(attr(tag, "data-language"), Some("rust"), "{tag}");
    let block = &rest[..rest.find("</pre>").unwrap()];
    for wanted in [
        "<span class=\"koine-hl-keyword\">fn</span>",
        "<span class=\"koine-hl-function\">a</span>",
        "<span class=\"koine-hl-punctuation\">() {}</span>",
        "<span class=\"koine-hl-type\">let</span>",
        "<span class=\"koine-hl-constant\">1</span>",
    ] {
        assert!(block.contains(wanted), "{wanted} not in {block}");
    }
    // Plain pieces are text nodes, and the lines are joined by one newline.
    assert!(block.contains("</span> <span"), "{block}");
    assert!(block.contains("</span>\n<span"), "{block}");
    assert_eq!(block.matches("<span").count(), 6, "{block}");
    // Stripped of markup the block is the source.
    let mut text = String::new();
    let mut inside = false;
    for c in block.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            c if !inside => text.push(c),
            _ => {}
        }
    }
    assert_eq!(text, "fn a() {}\nlet b = 1;");
}

#[test]
fn a_plot_or_image_is_a_tab_stop_only_when_the_resolver_made_it_one() {
    let root = json!({ "id": "root", "node": { "kind": "column", "items": [
        { "id": "p-click", "node": { "kind": "plot", "spec": { "series": [{ "x": [1, 2], "y": [3, 4] }] } } },
        { "id": "p-bare", "node": { "kind": "plot", "spec": { "series": [{ "x": [1, 2], "y": [3, 4] }] } } },
        { "id": "i-click", "node": { "kind": "image", "blob": null, "url": "https://example.test/a.png" } },
        { "id": "i-bare", "node": { "kind": "image", "blob": null, "url": "https://example.test/b.png" } },
    ]}});
    let html = render_html(&tree(root, json!(["p-click", "i-click"])), &web(), None);
    assert_eq!(tab_stops(&html), vec!["p-click", "i-click"], "{html}");
}

fn plot_frame() -> koine_plot::Frame {
    let spec = koine_plot::parse_spec(&json!({
        "series": [{ "x": [0, 10], "y": [0, 100] }],
    }))
    .unwrap();
    koine_plot::frame(&spec)
}

#[test]
fn a_click_maps_back_through_the_frame_to_data_coordinates() {
    use super::map::plot_click_to_data;
    let frame = plot_frame();
    let (px, py) = frame.to_pixel(5.0, 50.0);
    let at = plot_click_to_data(&frame, px, py, 1.0).unwrap();
    let (x, y) = (at["x"].as_f64().unwrap(), at["y"].as_f64().unwrap());
    assert!((x - 5.0).abs() < 1e-9 && (y - 50.0).abs() < 1e-9, "{at}");

    // The SVG drawn at half its size: the element pixel is half the viewBox's.
    let at = plot_click_to_data(&frame, px / 2.0, py / 2.0, 0.5).unwrap();
    assert!((at["x"].as_f64().unwrap() - 5.0).abs() < 1e-9, "{at}");
    assert!((at["y"].as_f64().unwrap() - 50.0).abs() < 1e-9, "{at}");

    // A scale that is not a positive number is 1.
    for scale in [0.0, -2.0, f64::NAN, f64::INFINITY] {
        assert_eq!(
            plot_click_to_data(&frame, px, py, scale),
            plot_click_to_data(&frame, px, py, 1.0)
        );
    }

    // Outside the plot area (the margin, past the edge) is no event.
    assert_eq!(plot_click_to_data(&frame, 1.0, 1.0, 1.0), None);
    assert_eq!(plot_click_to_data(&frame, frame.width + 5.0, py, 1.0), None);
}

#[test]
fn a_click_on_a_log_axis_inverts_through_the_power() {
    let spec = koine_plot::parse_spec(&json!({
        "series": [{ "x": [10, 100], "y": [1, 10] }],
        "x_axis": { "log_scale": true },
    }))
    .unwrap();
    let frame = koine_plot::frame(&spec);
    let (px, py) = frame.to_pixel(30.0, 5.0);
    let at = super::map::plot_click_to_data(&frame, px, py, 1.0).unwrap();
    assert!((at["x"].as_f64().unwrap() - 30.0).abs() < 1e-6, "{at}");
}

#[test]
fn a_click_on_an_image_is_a_fraction_of_its_displayed_size() {
    use super::map::image_click_to_fraction;
    assert_eq!(
        image_click_to_fraction(50.0, 75.0, 200.0, 100.0),
        Some(json!({"x": 0.25, "y": 0.75}))
    );
    // A pointer on the border is clamped; an element with no size is no event.
    assert_eq!(
        image_click_to_fraction(-1.0, 101.0, 200.0, 100.0),
        Some(json!({"x": 0.0, "y": 1.0}))
    );
    assert_eq!(image_click_to_fraction(1.0, 1.0, 0.0, 100.0), None);
    assert_eq!(image_click_to_fraction(f64::NAN, 1.0, 10.0, 10.0), None);
}

#[test]
fn a_text_node_is_markdown_in_a_div_and_raw_html_stays_text() {
    let page = tree(
        json!({ "id": "t", "node": { "kind": "text",
            "text": "# Map of this app\n\nVerbs are `calls`. <b onclick=\"x()\">bold</b>" } }),
        json!([]),
    );
    let html = render_html(&page, &web(), None);
    let (tag, rest) = element(&html, "t");
    assert!(tag.starts_with("<div"), "{tag}");
    assert!(attr(tag, "class").unwrap().contains("koine-text"), "{tag}");
    assert!(
        rest.starts_with("<h1>Map of this app</h1>\n<p>Verbs are <code>calls</code>. &lt;b"),
        "{rest}"
    );
    assert!(!html.contains("<b "), "{html}");
    assert!(!html.contains("# Map"), "{html}");
}

#[test]
fn a_multiline_text_field_is_a_textarea_with_its_rows() {
    let root = json!({
        "id": "root",
        "node": { "kind": "column", "items": [
            { "id": "f-lines", "label": "Text", "node": { "kind": "field",
                "field": { "text": { "multiline": true, "rows": 4 } },
                "bind": "state.text", "value": "one\ntwo" } },
            { "id": "f-line", "node": { "kind": "field", "field": { "text": {} },
                "bind": "state.line", "value": "x" } },
        ]}
    });
    let html = render_html(&tree(root, json!(["f-lines", "f-line"])), &web(), None);
    let (tag, _) = element(&html, "f-lines");
    assert!(tag.starts_with("<textarea"), "{tag}");
    assert_eq!(attr(tag, "rows"), Some("4"));
    assert_eq!(attr(tag, "data-koine-bind"), Some("state.text"));
    // One line stays an input.
    let (tag, _) = element(&html, "f-line");
    assert!(tag.starts_with("<input"), "{tag}");
}
