//! A text performance of a [`koine::surface::RenderTree`].
//!
//! [`render`] walks the tree and writes one line per node. [`TextFrontend::key`]
//! turns j/k/Enter into an [`koine::surface::Event`] for the focused widget;
//! the rule is [`koine::cursor::Cursor`], shared with the web frontend.
//! [`TextFrontend::gesture_key`] turns those keys, and h/l and the menu key,
//! into a [`koine::gesture::Gesture`]. [`TextFrontend::layout_verb`] is that
//! gesture through [`koine::gesture::layout_verb`]. [`koine::surface::reduce`]
//! still decides a surface event. The host applies the verb.
//!
//! [`frame`] is a pane drawn as cells: the box is the border, `≡` on the top
//! edge is the grip, and tab labels share that edge. An SGR 1006 report
//! becomes the same gesture a key does.
//!
//! How a line is spelled, and how far a column or a row indents, comes from
//! the tui [`koine::style::StyleSheet`]. The defaults are the lines this
//! frontend already wrote. A sheet tagged for another frontend is not
//! applied here: this host only opens the `tui` sheet.
//!
//! [`host`] edits the shared file `<workspace>/settings/koine-style.json`.

pub mod frame;
pub mod host;

pub use koine::cursor::Key;

use koine::cursor::Cursor;
use koine::style::{editor_apply, editor_spec, StyleError, StyleHost};
use koine::surface::{Event, EventKind, RenderKind, RenderNode, RenderTree, SurfaceSpec};
use serde_json::Value;

/// One text performance of a tree: a cursor into [`RenderTree::focus_order`]
/// and the tui style sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextFrontend {
    cursor: Cursor,
    style: StyleHost,
    /// Draw the blocks under their lines (`with_blocks`): a `plot` node's
    /// sketch and a `code` node's source. Off by default so [`render`] keeps
    /// one line per node, the contract the cursor and the frontends suite
    /// rely on.
    draw_blocks: bool,
}

impl Default for TextFrontend {
    fn default() -> Self {
        Self::new()
    }
}

impl TextFrontend {
    pub fn new() -> Self {
        Self {
            cursor: Cursor::new(),
            style: StyleHost::new("tui").expect("tui is a koine frontend"),
            draw_blocks: false,
        }
    }

    /// Print each `plot` node's sketch (`koine_plot::text`, 60x14) and each
    /// `code` node's source (one line per source line, indented by two) under
    /// its header line. The binary turns this on; a renderer that counts
    /// lines per node leaves it off.
    pub fn with_blocks(mut self, draw: bool) -> Self {
        self.draw_blocks = draw;
        self
    }

    /// [`Self::with_blocks`], under the name it had when plots were the only
    /// block.
    #[deprecated(note = "use `with_blocks`: it also prints `code` nodes")]
    pub fn with_plots(self, draw: bool) -> Self {
        self.with_blocks(draw)
    }

    pub fn sheet(&self) -> &koine::style::StyleSheet {
        self.style.sheet()
    }

    pub fn set_style(&mut self, slot: &str, raw: &str) -> Result<(), StyleError> {
        self.style.set(slot, raw)
    }

    pub fn reset_style(&mut self, slot: &str) -> Result<(), StyleError> {
        self.style.reset(slot)
    }

    /// The editor surface for this sheet. Resolve it with
    /// [`koine::style::editor_state`] to show the current values.
    pub fn editor_spec(&self) -> SurfaceSpec {
        editor_spec(self.style.frontend()).expect("tui is a koine frontend")
    }

    /// A field change sets the named slot. A click on `reset:<slot>` resets it.
    pub fn apply_editor(&mut self, event: &Event) -> Result<bool, StyleError> {
        editor_apply(&mut self.style, event)
    }

    /// The focused widget id, when the tree has a focus order.
    pub fn focused_id<'a>(&self, tree: &'a RenderTree) -> Option<&'a str> {
        self.cursor.focused_id(tree)
    }

    /// The tree as lines. The focused widget's line starts with the `focus` slot.
    pub fn render(&self, tree: &RenderTree) -> String {
        let focused = self.focused_id(tree);
        let mut lines = Vec::new();
        write_node(
            &tree.root,
            0,
            focused,
            self.style.sheet(),
            self.draw_blocks,
            &mut lines,
        );
        lines.join("\n")
    }

    /// Move the cursor, or build the event Enter would send.
    ///
    /// `Down` and `Up` return `None`: they change which widget is focused and
    /// do not tell the grammar anything happened. `Activate` returns a `click`
    /// on the focused id. An empty focus order activates nothing. A direction
    /// or menu key that is only a [`koine::gesture::Gesture`] returns `None`
    /// here; [`Self::gesture_key`] is the one that reports it.
    ///
    /// `Activate` on a `tree` is a `select` of its first visible node, with
    /// `[id]` as the value, rather than the `click` a tree ignores: this
    /// frontend has no cursor inside a widget, so the first line is the
    /// outline's one reachable row. An empty tree sends the click.
    ///
    /// `Activate` on a `graph` is a `select` of its first node, `[id]`, for
    /// the same reason (the nodes are listed as lines under `with_blocks`).
    /// On a `plot` or an `image` it is a `click` whose value is `{"x": null,
    /// "y": null}`: there is no pointer, and a handler can tell.
    pub fn key(&mut self, tree: &RenderTree, key: Key) -> Option<Event> {
        let event = self.cursor.event_for(tree, key)?;
        if event.kind == EventKind::Click {
            match find(&tree.root, &event.widget).map(|node| &node.node) {
                Some(RenderKind::Tree { nodes, .. }) => {
                    if let Some(first) = nodes.first() {
                        return Some(Event {
                            widget: event.widget,
                            kind: EventKind::Select,
                            value: Value::Array(vec![first.id.clone()]),
                        });
                    }
                }
                Some(RenderKind::Graph { spec, .. }) => {
                    if let Some(first) = spec.pointer("/nodes/0/id") {
                        return Some(Event {
                            widget: event.widget,
                            kind: EventKind::Select,
                            value: Value::Array(vec![first.clone()]),
                        });
                    }
                }
                Some(RenderKind::Plot { .. } | RenderKind::Image { .. }) => {
                    return Some(Event {
                        value: serde_json::json!({"x": null, "y": null}),
                        ..event
                    });
                }
                _ => {}
            }
        }
        Some(event)
    }

    /// The same keys as [`Self::key`], as a [`koine::gesture::Gesture`].
    ///
    /// `Down`, `Up`, `Next`, and `Prev` walk the focus order and return
    /// `Move`. `Left` and `Right` return `Move` and leave the cursor.
    /// `Activate` and `Secondary` name the focused widget. An empty focus
    /// order still moves, and activates nothing.
    pub fn gesture_key(&mut self, tree: &RenderTree, key: Key) -> Option<koine::gesture::Gesture> {
        self.cursor.gesture_for(tree, key)
    }

    /// [`Self::gesture_key`] as the layout verb it determines. A direction is
    /// `focus-direction`. Activate and the menu key are not layout verbs:
    /// this frontend's cursor is a surface widget, and no pane was named.
    pub fn layout_verb(&mut self, tree: &RenderTree, key: Key) -> Option<koine::layout::Verb> {
        self.cursor.layout_verb_for(tree, key)
    }
}

/// [`TextFrontend::render`] on a fresh cursor and the default sheet.
pub fn render(tree: &RenderTree) -> String {
    TextFrontend::new().render(tree)
}

/// The size of the sketch a `plot` node prints.
const PLOT_COLS: usize = 60;
const PLOT_ROWS: usize = 14;

fn write_node(
    node: &RenderNode,
    indent: usize,
    focused: Option<&str>,
    sheet: &koine::style::StyleSheet,
    draw_blocks: bool,
    out: &mut Vec<String>,
) {
    let mut focus_mark = fill(sheet.value("focus"), &[]);
    if focus_mark.is_empty() {
        focus_mark = ">".to_string();
    }
    let focus_width = focus_mark.chars().count();
    let mark = if focused == Some(node.id.as_str()) {
        focus_mark
    } else {
        " ".repeat(focus_width)
    };
    let pad = " ".repeat(indent);
    if let (true, RenderKind::Code { text, .. }) = (draw_blocks, &node.node) {
        // A code block is its header and then its source, one line each,
        // indented two under the header.
        out.push(format!("{mark}{pad}{}", line_of(node, sheet)));
        let under = " ".repeat(focus_width);
        out.extend(text.lines().map(|line| format!("{under}{pad}  {line}")));
        return;
    }
    if let (true, RenderKind::Tree { nodes, .. }) = (draw_blocks, &node.node) {
        // A tree is a header and then its visible outline: two columns per
        // depth, `▾`/`▸` for a branch, blanks for a leaf.
        out.push(format!("{mark}{pad}{}", line_of(node, sheet)));
        let under = " ".repeat(focus_width);
        for n in nodes {
            let disclosure = match (n.has_children, n.expanded) {
                (false, _) => "  ",
                (true, true) => "\u{25be} ",
                (true, false) => "\u{25b8} ",
            };
            out.push(format!(
                "{under}{pad}  {}{disclosure}{}",
                "  ".repeat(n.depth as usize),
                n.label
            ));
        }
        return;
    }
    if let (true, RenderKind::Graph { spec, .. }) = (draw_blocks, &node.node) {
        // A graph is a header and then one line per node: `label (id)`.
        out.push(format!("{mark}{pad}{}", line_of(node, sheet)));
        let under = " ".repeat(focus_width);
        for n in spec
            .get("nodes")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice)
        {
            let id = n.get("id").and_then(Value::as_str).unwrap_or_default();
            let label = n.get("label").and_then(Value::as_str).unwrap_or(id);
            out.push(format!("{under}{pad}  {label} ({id})"));
        }
        return;
    }
    if let (true, RenderKind::Plot { spec }) = (draw_blocks, &node.node) {
        // A plot is a header line and then the sketch, one line each, under
        // the same indent. A spec that does not parse is the one line.
        match koine_plot::render_text_json(spec, PLOT_COLS, PLOT_ROWS) {
            Ok(sketch) => {
                out.push(format!("{mark}{pad}plot"));
                let under = " ".repeat(focus_width);
                out.extend(sketch.lines().map(|line| format!("{under}{pad}  {line}")));
            }
            Err(error) => out.push(format!("{mark}{pad}plot: {error}")),
        }
        return;
    }
    out.push(format!("{mark}{pad}{}", line_of(node, sheet)));
    let child_indent = indent + indent_of(node, sheet);
    for child in children_of(node) {
        write_node(child, child_indent, focused, sheet, draw_blocks, out);
    }
}

fn indent_of(node: &RenderNode, sheet: &koine::style::StyleSheet) -> usize {
    match &node.node {
        RenderKind::Column { .. } => spacing(sheet, "column"),
        RenderKind::Row { .. } => spacing(sheet, "row"),
        _ => 2,
    }
}

fn spacing(sheet: &koine::style::StyleSheet, slot: &str) -> usize {
    sheet.value(slot).parse().unwrap_or(0)
}

fn line_of(node: &RenderNode, sheet: &koine::style::StyleSheet) -> String {
    let label = node.label.as_deref().unwrap_or("");
    match &node.node {
        RenderKind::Column { .. } => "column".into(),
        RenderKind::Row { .. } => "row".into(),
        RenderKind::Grid { columns, .. } => format!("grid {columns}"),
        RenderKind::Section {
            title, collapsed, ..
        } => {
            let mut line = fill(
                sheet.value("section"),
                &[("title", title.as_str()), ("label", label)],
            );
            if *collapsed {
                line.push_str(" (collapsed)");
            }
            line
        }
        RenderKind::Tabs { tabs } => format!("tabs {}", tabs.len()),
        RenderKind::Text { text } => fill(
            sheet.value("text"),
            &[("text", text.as_str()), ("label", label)],
        ),
        RenderKind::Table { columns, .. } => {
            let joined = columns.join(",");
            fill(
                sheet.value("table"),
                &[("columns", joined.as_str()), ("label", label)],
            )
        }
        RenderKind::List { .. } => fill(sheet.value("list"), &[("label", label)]),
        // `write_node` prints the outline under `with_blocks`.
        RenderKind::Tree { nodes, .. } => format!("tree {}", nodes.len()),
        RenderKind::Graph { spec, .. } => format!(
            "graph {}",
            spec.get("nodes")
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
        ),
        // `write_node` prints a plot's sketch itself.
        RenderKind::Plot { .. } => "plot".into(),
        RenderKind::Code { language, .. } => match language {
            Some(language) => format!("code {language}"),
            None => "code".into(),
        },
        RenderKind::Image { .. } => "image".into(),
        RenderKind::Field { field, value, .. } => {
            // A file field holds a blob reference; what it shows is the name.
            let shown = if field.get("file").is_some() {
                match value.pointer("/blob/name").and_then(Value::as_str) {
                    Some(name) => format!("file {name}"),
                    None => "file (none)".to_string(),
                }
            } else {
                value.to_string()
            };
            fill(
                sheet.value("field"),
                &[("value", shown.as_str()), ("label", label)],
            )
        }
        RenderKind::Button {
            label: button_label,
        } => fill(sheet.value("button"), &[("label", button_label.as_str())]),
        RenderKind::Status { level, message } => {
            let slot = match level.as_str() {
                "error" => "status.error",
                "warning" => "status.warning",
                "info" => "status.info",
                _ => "",
            };
            if slot.is_empty() {
                format!("status {level}: {message}")
            } else {
                fill(
                    sheet.value(slot),
                    &[
                        ("level", level.as_str()),
                        ("message", message.as_str()),
                        ("label", label),
                    ],
                )
            }
        }
        RenderKind::Log { .. } => "log".into(),
        RenderKind::Kv { .. } => "kv".into(),
        RenderKind::Divider => "divider".into(),
        RenderKind::Spacer => "spacer".into(),
        RenderKind::Placeholder {
            unknown_kind,
            reason,
            ..
        } => {
            let kind = unknown_kind.as_deref().unwrap_or("placeholder");
            match reason {
                Some(reason) => format!("placeholder {kind}: {reason}"),
                None => format!("placeholder {kind}"),
            }
        }
    }
}

/// The node with `id`, depth first.
fn find<'a>(node: &'a RenderNode, id: &str) -> Option<&'a RenderNode> {
    if node.id == id {
        return Some(node);
    }
    children_of(node)
        .into_iter()
        .find_map(|child| find(child, id))
}

fn children_of(node: &RenderNode) -> Vec<&RenderNode> {
    match &node.node {
        RenderKind::Column { items }
        | RenderKind::Row { items }
        | RenderKind::Grid { items, .. } => items.iter().collect(),
        RenderKind::Section { body, .. } => vec![body.as_ref()],
        RenderKind::Tabs { tabs } => tabs.iter().map(|tab| &tab.body).collect(),
        _ => Vec::new(),
    }
}

/// Replace `{name}` from `vars`. A name with no value expands to empty.
fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(end) = after.find('}') {
            let name = &after[..end];
            if let Some((_, value)) = vars.iter().find(|(key, _)| *key == name) {
                out.push_str(value);
            }
            rest = &after[end + 1..];
        } else {
            out.push_str(&rest[start..]);
            return out;
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use koine::surface::{resolve, EventKind, RenderKind, RenderNode};
    use serde_json::Value;

    #[test]
    fn a_resolved_surface_renders_as_text_and_enter_names_the_focused_widget() {
        let spec = koine::surface::example_paper_triage();
        let tree = resolve(
            &spec,
            &serde_json::json!({}),
            &serde_json::json!({}),
            &serde_json::json!({}),
        );
        let mut frontend = TextFrontend::new();
        let text = frontend.render(&tree);
        assert!(
            text.contains("column")
                || text.contains("text")
                || text.contains("button")
                || text.contains("table")
                || text.contains("list"),
            "{text}"
        );
        assert!(text.lines().any(|line| line.starts_with('>')), "{text}");

        let first = frontend.focused_id(&tree).unwrap().to_string();
        assert!(frontend.key(&tree, Key::Down).is_none());
        if tree.focus_order.len() > 1 {
            assert_ne!(frontend.focused_id(&tree), Some(first.as_str()));
        }
        frontend.key(&tree, Key::Up);
        assert_eq!(frontend.focused_id(&tree), Some(first.as_str()));

        let event = frontend
            .key(&tree, Key::Activate)
            .expect("a focused widget");
        assert_eq!(event.widget, first);
        assert_eq!(event.kind, EventKind::Click);
    }

    #[test]
    fn the_gesture_and_layout_keys_delegate_to_the_shared_cursor() {
        use koine::cursor::Cursor;

        let tree = two_buttons();
        let mut frontend = TextFrontend::new();
        let mut cursor = Cursor::new();
        for key in [
            Key::Down,
            Key::Left,
            Key::Next,
            Key::Activate,
            Key::Secondary,
        ] {
            assert_eq!(
                frontend.gesture_key(&tree, key),
                cursor.gesture_for(&tree, key)
            );
            assert_eq!(frontend.focused_id(&tree), cursor.focused_id(&tree));
        }
        assert_eq!(
            TextFrontend::new()
                .layout_verb(&tree, Key::Down)
                .map(|verb| serde_json::to_value(verb).unwrap()),
            Some(serde_json::json!({"verb": "focus-direction", "direction": "down"}))
        );
        assert!(frontend.key(&tree, Key::Left).is_none());
    }

    #[test]
    fn the_default_sheet_spells_a_button_the_way_the_renderer_always_did() {
        let text = TextFrontend::new().render(&button_tree());
        assert_eq!(text, ">button Save");
    }

    #[test]
    fn a_button_template_changes_the_next_render_and_reset_restores_it() {
        let mut frontend = TextFrontend::new();
        frontend.set_style("button", "[{label}]").unwrap();
        let text = frontend.render(&button_tree());
        assert_eq!(text, ">[Save]");

        let changed = frontend
            .apply_editor(&Event {
                widget: "button".into(),
                kind: EventKind::Change,
                value: Value::String("({label})".into()),
            })
            .unwrap();
        assert!(changed);
        assert_eq!(frontend.render(&button_tree()), ">(Save)");

        frontend
            .apply_editor(&Event {
                widget: "reset:button".into(),
                kind: EventKind::Click,
                value: Value::Null,
            })
            .unwrap();
        assert_eq!(frontend.render(&button_tree()), ">button Save");
    }

    #[test]
    fn set_refuses_an_unknown_slot_and_a_spacing_that_is_not_an_integer() {
        let mut frontend = TextFrontend::new();
        assert!(frontend.set_style("grid", "2").is_err());
        assert!(frontend.set_style("column", "wide").is_err());
        assert_eq!(frontend.sheet().value("column"), "2");
        let text = frontend.render(&column_of_button());
        assert!(text.contains("\n>  button Save"), "{text}");
    }

    fn plot_tree(spec: Value) -> RenderTree {
        RenderTree {
            root: RenderNode {
                id: "p".into(),
                label: None,
                help: None,
                node: RenderKind::Plot { spec },
            },
            focus_order: vec![],
        }
    }

    #[test]
    fn a_plot_prints_its_sketch_under_a_header() {
        let spec = serde_json::json!({"title": "t", "series": [{"label": "s", "x": [0, 1, 2], "y": [0, 1, 4]}]});
        let text = TextFrontend::new()
            .with_blocks(true)
            .render(&plot_tree(spec));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], " plot", "{text}");
        assert_eq!(lines[1], "   t", "{text}");
        assert!(lines.len() > 10, "{text}");
        assert!(lines.iter().skip(2).all(|l| l.starts_with("   ")), "{text}");
        assert!(text.contains("* s"), "{text}");
        assert!(lines.iter().all(|l| l.chars().count() <= 64), "{text}");
    }

    #[test]
    fn a_plot_that_does_not_parse_is_one_line_naming_the_error() {
        let text = TextFrontend::new()
            .with_blocks(true)
            .render(&plot_tree(serde_json::json!({"series": [{"x": [1]}]})));
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(text.starts_with(" plot: plot spec: "), "{text}");
        let wrong = TextFrontend::new()
            .with_blocks(true)
            .render(&plot_tree(serde_json::json!({"series": 3})));
        assert!(wrong.starts_with(" plot: plot spec: "), "{wrong}");
    }

    fn node_tree(node: RenderKind) -> RenderTree {
        RenderTree {
            root: RenderNode {
                id: "n".into(),
                label: None,
                help: None,
                node,
            },
            focus_order: vec![],
        }
    }

    #[test]
    fn a_code_node_is_one_line_until_blocks_are_on_and_then_its_source_indented() {
        let tree = node_tree(RenderKind::Code {
            language: Some("diff".into()),
            text: "@@ -1 +1 @@\n-old\n+new\n".into(),
            spans: vec![],
        });
        assert_eq!(render(&tree), " code diff");
        let text = TextFrontend::new().with_blocks(true).render(&tree);
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            vec![" code diff", "   @@ -1 +1 @@", "   -old", "   +new"],
            "{text}"
        );
        let bare = node_tree(RenderKind::Code {
            language: None,
            text: "x".into(),
            spans: vec![],
        });
        assert_eq!(render(&bare), " code");
        assert_eq!(
            TextFrontend::new().with_blocks(true).render(&bare),
            " code\n   x"
        );
    }

    fn outline() -> RenderTree {
        let node = |id: serde_json::Value, label: &str, depth, has_children, expanded| {
            koine::surface::tree::TreeNode {
                id,
                label: label.into(),
                depth,
                has_children,
                expanded,
                row: serde_json::json!({}),
            }
        };
        let mut tree = node_tree(RenderKind::Tree {
            nodes: vec![
                node(serde_json::json!("a"), "src", 0, true, true),
                node(serde_json::json!("b"), "main.rs", 1, false, false),
                node(serde_json::json!(7), "docs", 0, true, false),
            ],
            bind: Some("state.open".into()),
        });
        tree.root.id = "files".into();
        tree.focus_order = vec!["files".into()];
        tree
    }

    #[test]
    fn a_tree_is_a_header_until_blocks_are_on_and_then_its_outline() {
        let tree = outline();
        assert_eq!(render(&tree), ">tree 3");
        let text = TextFrontend::new().with_blocks(true).render(&tree);
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            vec![
                ">tree 3",
                "   \u{25be} src",
                "       main.rs",
                "   \u{25b8} docs"
            ],
            "{text}"
        );
    }

    #[test]
    fn enter_on_a_tree_selects_its_first_visible_node() {
        let mut frontend = TextFrontend::new();
        let event = frontend.key(&outline(), Key::Activate).unwrap();
        assert_eq!(event.widget, "files");
        assert_eq!(event.kind, EventKind::Select);
        assert_eq!(event.value, serde_json::json!(["a"]));

        let mut empty = outline();
        empty.root.node = RenderKind::Tree {
            nodes: vec![],
            bind: None,
        };
        let event = TextFrontend::new().key(&empty, Key::Activate).unwrap();
        assert_eq!(event.kind, EventKind::Click);
    }

    #[test]
    fn highlighted_spans_do_not_change_the_text_performance() {
        let plain = node_tree(RenderKind::Code {
            language: Some("rust".into()),
            text: "fn a() {}".into(),
            spans: vec![],
        });
        let classed = node_tree(RenderKind::Code {
            language: Some("rust".into()),
            text: "fn a() {}".into(),
            spans: vec![vec![
                ("keyword".into(), "fn".into()),
                (String::new(), " a() {}".into()),
            ]],
        });
        let tui = TextFrontend::new().with_blocks(true);
        assert_eq!(tui.render(&plain), tui.render(&classed));
        assert_eq!(render(&plain), render(&classed));
    }

    fn network() -> RenderTree {
        let mut tree = node_tree(RenderKind::Graph {
            spec: serde_json::json!({
                "nodes": [{"id": "1", "label": "root"}, {"id": "b"}],
                "edges": [{"source": "1", "target": "b"}],
            }),
            svg: "<svg/>".into(),
        });
        tree.root.id = "net".into();
        tree.focus_order = vec!["net".into()];
        tree
    }

    #[test]
    fn a_graph_is_a_header_until_blocks_are_on_and_then_a_line_per_node() {
        let tree = network();
        assert_eq!(render(&tree), ">graph 2");
        let text = TextFrontend::new().with_blocks(true).render(&tree);
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            vec![">graph 2", "   root (1)", "   b (b)"],
            "{text}"
        );
    }

    #[test]
    fn enter_on_a_graph_selects_its_first_node() {
        let event = TextFrontend::new().key(&network(), Key::Activate).unwrap();
        assert_eq!(event.widget, "net");
        assert_eq!(event.kind, EventKind::Select);
        assert_eq!(event.value, serde_json::json!(["1"]));

        let mut empty = network();
        empty.root.node = RenderKind::Graph {
            spec: serde_json::json!({"nodes": [], "edges": []}),
            svg: String::new(),
        };
        let event = TextFrontend::new().key(&empty, Key::Activate).unwrap();
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
                url: Some(serde_json::json!("http://x/y.png")),
            },
        ] {
            let mut tree = node_tree(node);
            tree.root.id = "w".into();
            tree.focus_order = vec!["w".into()];
            let event = TextFrontend::new().key(&tree, Key::Activate).unwrap();
            assert_eq!(event.widget, "w");
            assert_eq!(event.kind, EventKind::Click);
            assert_eq!(event.value, serde_json::json!({"x": null, "y": null}));
        }
    }

    #[test]
    #[allow(deprecated)]
    fn with_plots_is_with_blocks() {
        let tree = node_tree(RenderKind::Code {
            language: None,
            text: "x".into(),
            spans: vec![],
        });
        assert_eq!(
            TextFrontend::new().with_plots(true).render(&tree),
            TextFrontend::new().with_blocks(true).render(&tree)
        );
    }

    #[test]
    fn a_file_field_shows_the_name_it_sent_or_none() {
        let field = |value: Value| {
            node_tree(RenderKind::Field {
                field: serde_json::json!({"file": {"accept": ".csv"}}),
                bind: Some("state.f".into()),
                value,
            })
        };
        assert_eq!(render(&field(Value::Null)), " field file (none)");
        assert_eq!(
            render(&field(
                serde_json::json!({"blob": {"item": "s1", "name": "data.csv"}})
            )),
            " field file data.csv"
        );
        // A text field is as it was.
        let text = node_tree(RenderKind::Field {
            field: serde_json::json!({"text": {}}),
            bind: None,
            value: serde_json::json!("Ada"),
        });
        assert_eq!(render(&text), " field \"Ada\"");
    }

    fn two_buttons() -> RenderTree {
        RenderTree {
            root: RenderNode {
                id: "c".into(),
                label: None,
                help: None,
                node: RenderKind::Column {
                    items: vec![
                        RenderNode {
                            id: "a".into(),
                            label: None,
                            help: None,
                            node: RenderKind::Button { label: "A".into() },
                        },
                        RenderNode {
                            id: "b".into(),
                            label: None,
                            help: None,
                            node: RenderKind::Button { label: "B".into() },
                        },
                    ],
                },
            },
            focus_order: vec!["a".into(), "b".into()],
        }
    }

    fn button_tree() -> RenderTree {
        RenderTree {
            root: RenderNode {
                id: "b".into(),
                label: None,
                help: None,
                node: RenderKind::Button {
                    label: "Save".into(),
                },
            },
            focus_order: vec!["b".into()],
        }
    }

    fn column_of_button() -> RenderTree {
        RenderTree {
            root: RenderNode {
                id: "c".into(),
                label: None,
                help: None,
                node: RenderKind::Column {
                    items: vec![RenderNode {
                        id: "b".into(),
                        label: None,
                        help: None,
                        node: RenderKind::Button {
                            label: "Save".into(),
                        },
                    }],
                },
            },
            focus_order: vec!["b".into()],
        }
    }
}
