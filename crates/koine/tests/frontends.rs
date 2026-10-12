//! The conformance suite: one tree, two performances (docs/plan-koine-web.md D10).
//!
//! Every tree in [`trees`] is performed by `koine-tui` and by `koine-dioxus`
//! (as a string), and the six assertions of D10 are made over the pair. A
//! frontend that drops a node, reorders the tab stops, answers a key with a
//! different event, ignores a slot or hides an unknown kind fails here, not in
//! review.
//!
//! The goldens under `tests/golden/{web,tui}` are the performances themselves.
//! Regenerate them with
//! `UPDATE_KOINE_GOLDENS=1 cargo test -p koine --test frontends` and review the
//! diff: a change to a mapping is a reviewed golden change.

use std::path::PathBuf;

use koine::cursor::{Cursor, Key};
use koine::style::{
    editor_spec, editor_state, slots, StyleSheet, ValueKind, COLOR_TOKENS, FONT_TOKENS,
    REQUIRED_SLOTS,
};
use koine::surface::{
    example_job_monitor, example_paper_triage, example_signal_explorer, resolve, Event, EventKind,
    RenderKind, RenderNode, RenderTree, SurfaceSpec,
};
use koine_dioxus::{css_variables, render_html};
use koine_tui::TextFrontend;
use serde_json::{json, Value};

fn web() -> StyleSheet {
    StyleSheet::default_for("web").unwrap()
}

fn tree_of(root: Value, focus_order: Value) -> RenderTree {
    serde_json::from_value(json!({ "root": root, "focus_order": focus_order })).unwrap()
}

/// One node of every `RenderKind`, every field kind and a status of each
/// level. The focus order is the one `resolve` would write, in document order.
fn everything() -> RenderTree {
    let focus = [
        "b-go", "t-tabs", "t-table", "t-list", "t-tree", "f-text", "f-number", "f-slider",
        "f-select", "f-toggle", "f-date", "f-color", "f-file",
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
                { "id": "c", "label": "docs", "depth": 0, "has_children": true, "expanded": false, "row": { "id": "c" } },
            ]}},
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
            { "id": "f-file", "label": "Data", "node": { "kind": "field", "field": { "file": { "accept": ".csv,.xlsx" } }, "bind": "state.f", "value": null } },
            { "id": "code-diff", "node": { "kind": "code", "language": "diff", "text": "@@ -1,2 +1,2 @@\n context\n-old\n+new\n" } },
            { "id": "code-plain", "node": { "kind": "code", "text": "plain text" } },
            { "id": "st-error", "node": { "kind": "status", "level": "error", "message": "bad" } },
            { "id": "st-warning", "node": { "kind": "status", "level": "warning", "message": "hmm" } },
            { "id": "st-info", "node": { "kind": "status", "level": "info", "message": "fyi" } },
            { "id": "log", "node": { "kind": "log", "lines": ["one", "two"] } },
            { "id": "kv", "node": { "kind": "kv", "pairs": { "a": 1, "b": "two" } } },
            { "id": "divider", "node": { "kind": "divider" } },
            { "id": "ph", "node": { "kind": "placeholder", "unknown_kind": "hologram", "reason": "this build has no 'hologram' widget" } },
        ]}
    });
    tree_of(root, json!(focus))
}

/// A tree whose only content is a placeholder of an unknown kind.
fn holo() -> RenderTree {
    tree_of(
        json!({ "id": "column", "node": { "kind": "column", "items": [
            { "id": "lead", "node": { "kind": "text", "text": "before" } },
            { "id": "holo", "node": {
                "kind": "placeholder",
                "unknown_kind": "holo",
                "reason": "nobody draws holograms",
            }},
        ]}}),
        json!([]),
    )
}

/// plan-koine-live end to end: a spec using the envelope (a table whose
/// columns come from the result), a bound plot, a diff and a file field that
/// has sent a file, resolved the way the app does.
fn live_kinds() -> RenderTree {
    let spec: SurfaceSpec = serde_json::from_value(json!({
        "surface": "1.0", "name": "live",
        "state": {
            "result": {
                "rows": [
                    { "id": "a", "t": 0, "v": 1.5 },
                    { "id": "b", "t": 1, "v": 2.5 },
                    { "id": "c", "t": 2, "v": 2.0 }
                ],
                "columns": ["id", "v"],
                "id_field": "id",
                "next": "page-2"
            },
            "patch": "@@ -1 +1 @@\n-old\n+new\n",
            "upload": { "blob": { "item": "s1", "name": "data.csv" } }
        },
        "root": { "id": "live", "column": [
            { "id": "rows", "table": { "rows": "{{state.result}}", "columns": [] } },
            { "id": "series", "plot": { "rows": "{{state.result}}", "x": "t", "y": ["v"] } },
            { "id": "patch", "code": { "language": "diff", "text": "{{state.patch}}" } },
            { "id": "upload", "bind": "state.upload", "field": { "file": { "accept": ".csv" } } },
            { "id": "more", "button": { "label": "More", "on_click": [] } }
        ] }
    }))
    .unwrap();
    resolve(&spec, &spec.state, &json!({}), &json!({}))
}

fn resolved(spec: &SurfaceSpec, sources: &Value) -> RenderTree {
    resolve(spec, &spec.state, &json!({}), sources)
}

fn fake_papers() -> Value {
    json!({ "papers": [
        { "id": "11111111-1111-1111-1111-111111111111", "title": "A dark matter survey", "year": 2024, "author_text": "Zwicky, F." },
        { "id": "22222222-2222-2222-2222-222222222222", "title": "Signal processing notes", "year": 2023, "author_text": "Shannon, C." },
        { "id": "33333333-3333-3333-3333-333333333333", "title": "On the shoulders of giants", "year": 1676, "author_text": "Newton, I." },
    ]})
}

/// The fixed set. The name is the golden's file stem.
fn trees() -> Vec<(&'static str, RenderTree)> {
    let editor = |frontend: &str| {
        let sheet = StyleSheet::default_for(frontend).unwrap();
        let spec = editor_spec(frontend).unwrap();
        resolve(&spec, &editor_state(&sheet), &json!({}), &json!({}))
    };
    let all = [
        (
            "signal-explorer",
            resolved(&example_signal_explorer(), &json!({})),
        ),
        (
            "paper-triage",
            resolved(&example_paper_triage(), &json!({})),
        ),
        (
            "paper-triage-rows",
            resolved(&example_paper_triage(), &fake_papers()),
        ),
        ("job-monitor", resolved(&example_job_monitor(), &json!({}))),
        ("style-editor-web", editor("web")),
        ("style-editor-tui", editor("tui")),
        ("every-kind", everything()),
        ("live-kinds", live_kinds()),
        ("placeholder-holo", holo()),
    ];
    all.into()
}

fn children(node: &RenderNode) -> Vec<&RenderNode> {
    match &node.node {
        RenderKind::Column { items }
        | RenderKind::Row { items }
        | RenderKind::Grid { items, .. } => items.iter().collect(),
        RenderKind::Section { body, .. } => vec![body.as_ref()],
        RenderKind::Tabs { tabs } => tabs.iter().map(|tab| &tab.body).collect(),
        _ => Vec::new(),
    }
}

/// Every node, pre-order: the order the text frontend prints its lines in.
fn nodes(tree: &RenderTree) -> Vec<&RenderNode> {
    fn walk<'a>(node: &'a RenderNode, out: &mut Vec<&'a RenderNode>) {
        out.push(node);
        for child in children(node) {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(&tree.root, &mut out);
    out
}

fn tui(tree: &RenderTree) -> String {
    TextFrontend::new().render(tree)
}

/// The first focusable id, which is where a fresh cursor sits.
fn first_focus(tree: &RenderTree) -> Option<&str> {
    tree.focus_order.first().map(String::as_str)
}

fn html(tree: &RenderTree) -> String {
    render_html(tree, &web(), first_focus(tree))
}

/// The text of every tag in the page, in document order.
fn tags(html: &str) -> Vec<&str> {
    html.split('<')
        .filter_map(|chunk| chunk.split('>').next())
        .collect()
}

/// An attribute's value, quoted or not (Dioxus writes numbers bare).
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!(" {name}=");
    let start = tag.find(&key)? + key.len();
    let rest = &tag[start..];
    match rest.strip_prefix('"') {
        Some(quoted) => Some(&quoted[..quoted.find('"')?]),
        None => Some(rest.split(' ').next().unwrap_or(rest)),
    }
}

fn dom_ids(html: &str) -> Vec<String> {
    tags(html)
        .into_iter()
        .filter_map(|tag| attr(tag, "data-koine-id").map(str::to_string))
        .collect()
}

fn tab_stops(html: &str) -> Vec<String> {
    tags(html)
        .into_iter()
        .filter(|tag| attr(tag, "tabindex") == Some("0"))
        .filter_map(|tag| attr(tag, "data-koine-id").map(str::to_string))
        .collect()
}

// (1) ---------------------------------------------------------------------

#[test]
fn every_node_id_is_performed_exactly_once_by_each_frontend() {
    for (name, tree) in trees() {
        let all = nodes(&tree);
        assert!(!all.is_empty(), "{name}");

        let page = render_html(&tree, &web(), None);
        let ids = dom_ids(&page);
        for node in &all {
            let n = ids.iter().filter(|id| **id == node.id).count();
            assert_eq!(
                n, 1,
                "{name}: '{}' is on the page {n} times\n{page}",
                node.id
            );
        }
        assert_eq!(
            ids.len(),
            all.len(),
            "{name}: the page carries ids the tree does not have: {ids:?}"
        );

        let text = TextFrontend::new().render(&tree);
        assert_eq!(
            text.lines().count(),
            all.len(),
            "{name}: one line per node\n{text}"
        );
    }
}

// (2) ---------------------------------------------------------------------

#[test]
fn the_tab_stops_in_dom_order_are_the_focus_order() {
    for (name, tree) in trees() {
        let page = html(&tree);
        assert_eq!(tab_stops(&page), tree.focus_order, "{name}\n{page}");
    }
}

// (3) ---------------------------------------------------------------------

#[test]
fn j_j_focuses_the_same_node_on_both_and_marks_only_it() {
    for (name, tree) in trees() {
        if tree.focus_order.is_empty() {
            continue;
        }
        let mut cursor = Cursor::new();
        assert!(cursor.event_for(&tree, Key::Down).is_none());
        assert!(cursor.event_for(&tree, Key::Down).is_none());
        let focused = cursor.focused_id(&tree).expect("a focused id").to_string();
        let expected = tree.focus_order[2 % tree.focus_order.len()].clone();
        assert_eq!(focused, expected, "{name}");

        // The text frontend keeps its own cursor and takes the same keys.
        let mut frontend = TextFrontend::new();
        frontend.key(&tree, Key::Down);
        frontend.key(&tree, Key::Down);
        assert_eq!(frontend.focused_id(&tree), Some(focused.as_str()), "{name}");
        let text = frontend.render(&tree);
        let marked: Vec<usize> = text
            .lines()
            .enumerate()
            .filter(|(_, line)| line.starts_with('>'))
            .map(|(i, _)| i)
            .collect();
        let line = nodes(&tree)
            .iter()
            .position(|node| node.id == focused)
            .expect("the focused id is a node");
        assert_eq!(marked, vec![line], "{name}: the tui mark\n{text}");

        let page = render_html(&tree, &web(), Some(&focused));
        let on_page: Vec<String> = tags(&page)
            .into_iter()
            .filter(|tag| attr(tag, "data-koine-focused").is_some())
            .filter_map(|tag| attr(tag, "data-koine-id").map(str::to_string))
            .collect();
        assert_eq!(
            on_page,
            vec![focused.clone()],
            "{name}: the web mark\n{page}"
        );
        assert_eq!(page.matches("data-koine-focused=").count(), 1, "{name}");
    }
}

#[test]
fn enter_on_a_focused_button_is_the_click_the_web_sends() {
    let mut buttons = 0;
    for (name, tree) in trees() {
        for node in nodes(&tree) {
            if !matches!(node.node, RenderKind::Button { .. }) {
                continue;
            }
            buttons += 1;
            let mut cursor = Cursor::new();
            assert!(cursor.set_focus(&tree, &node.id), "{name}: {}", node.id);
            // What `koine-dioxus` sends from a button's `onclick`.
            let click = Event {
                widget: node.id.clone(),
                kind: EventKind::Click,
                value: Value::Null,
            };
            assert_eq!(cursor.event_for(&tree, Key::Activate), Some(click.clone()));
            let mut frontend = TextFrontend::new();
            while frontend.focused_id(&tree) != Some(node.id.as_str()) {
                frontend.key(&tree, Key::Down);
            }
            assert_eq!(frontend.key(&tree, Key::Activate), Some(click));
        }
    }
    assert!(
        buttons >= 3,
        "the suite should hold buttons, found {buttons}"
    );
}

// (4) ---------------------------------------------------------------------

/// A valid value other than the slot's default, for the frontend's kind.
fn other_value(slot: &koine::style::Slot) -> String {
    match slot.kind {
        ValueKind::Spacing => (slot.default_value.parse::<u32>().unwrap() + 5).to_string(),
        ValueKind::Color => COLOR_TOKENS
            .iter()
            .find(|token| **token != slot.default_value)
            .unwrap()
            .to_string(),
        ValueKind::Font => FONT_TOKENS
            .iter()
            .find(|token| **token != slot.default_value)
            .unwrap()
            .to_string(),
        ValueKind::Line => format!("<<{}>>", slot.default_value),
    }
}

#[test]
fn every_required_slot_changes_the_web_performance() {
    let table = slots("web").unwrap();
    let names: Vec<&str> = table.iter().map(|slot| slot.name).collect();
    assert_eq!(names, REQUIRED_SLOTS);

    let tree = everything();
    let base_sheet = web();
    let base_vars = css_variables(&base_sheet);
    let base = render_html(&tree, &base_sheet, first_focus(&tree));
    for slot in table {
        let value = other_value(slot);
        let mut sheet = web();
        sheet.set(slot.name, &value).unwrap();
        let changed = render_html(&tree, &sheet, first_focus(&tree));
        assert!(
            css_variables(&sheet) != base_vars || changed != base,
            "web slot {} = {value} changed nothing: a slot nobody applies is not a slot",
            slot.name
        );
    }
}

#[test]
fn every_required_slot_changes_the_tui_performance() {
    let table = slots("tui").unwrap();
    let names: Vec<&str> = table.iter().map(|slot| slot.name).collect();
    assert_eq!(names, REQUIRED_SLOTS);

    let tree = everything();
    let base = tui(&tree);
    for slot in table {
        let value = other_value(slot);
        let mut frontend = TextFrontend::new();
        frontend.set_style(slot.name, &value).unwrap();
        assert_ne!(
            frontend.render(&tree),
            base,
            "tui slot {} = {value} changed nothing: a slot nobody applies is not a slot",
            slot.name
        );
    }
}

// (5) ---------------------------------------------------------------------

#[test]
fn an_unknown_kind_is_a_placeholder_naming_it_in_both() {
    let tree = holo();
    let text = tui(&tree);
    assert!(
        text.contains("placeholder holo: nobody draws holograms"),
        "{text}"
    );
    let page = html(&tree);
    assert!(
        page.contains("placeholder holo: nobody draws holograms"),
        "{page}"
    );

    // And from a spec this build cannot read, end to end through resolve.
    let spec: SurfaceSpec = serde_json::from_value(json!({
        "surface": "1.0", "name": "n",
        "root": { "id": "u", "holo": { "depth": 3 } }
    }))
    .unwrap();
    let tree = resolve(&spec, &json!({}), &json!({}), &json!({}));
    assert!(tui(&tree).contains("placeholder holo"), "{}", tui(&tree));
    assert!(html(&tree).contains("placeholder holo"), "{}", html(&tree));
}

// (6) ---------------------------------------------------------------------

fn golden_path(frontend: &str, name: &str, ext: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(frontend)
        .join(format!("{name}.{ext}"))
}

fn check_golden(path: &PathBuf, generated: &str) {
    if std::env::var_os("UPDATE_KOINE_GOLDENS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, generated).unwrap();
    }
    let committed = std::fs::read_to_string(path).unwrap_or_default();
    assert_eq!(
        committed,
        generated,
        "{} drifted; a change to a mapping is a reviewed golden change: \
         regenerate with UPDATE_KOINE_GOLDENS=1 cargo test -p koine --test frontends \
         and read the diff",
        path.display()
    );
}

#[test]
fn the_web_goldens_are_byte_stable() {
    for (name, tree) in trees() {
        let page = html(&tree) + "\n";
        check_golden(&golden_path("web", name, "html"), &page);
    }
}

#[test]
fn the_tui_goldens_are_byte_stable() {
    for (name, tree) in trees() {
        let text = tui(&tree) + "\n";
        check_golden(&golden_path("tui", name, "txt"), &text);
    }
}
