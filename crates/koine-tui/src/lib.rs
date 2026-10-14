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

use std::collections::BTreeMap;

use koine::cursor::{Cursor, Reply};
use koine::outline::{MenuChoice, RowMenu};
use koine::style::{editor_apply, editor_spec, StyleError, StyleHost};
use koine::surface::tree::{paste_beside, paste_into};
use koine::surface::{Event, EventKind, RenderKind, RenderNode, RenderTree, SurfaceSpec};
use serde_json::Value;

/// A one-line prompt the text frontend is waiting on (the keys that need UI,
/// plan-koine-outline O-D3, O-D5). While one is pending, typed characters go
/// to it ([`TextFrontend::type_char`]), `Enter` ([`Key::Activate`]) answers
/// it and [`TextFrontend::cancel`] drops it; every other key does nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// `r`: the new text for a row. `label` is the current one, shown so the
    /// person knows which row it is.
    Rename {
        widget: String,
        id: Value,
        label: String,
        text: String,
    },
    /// `m`: the numbered menu of a row; `text` accumulates the digits typed.
    /// The entries are `menu.entries()` (`koine::outline::menu_entries`):
    /// "Rename" first when the tree renames, then the spec's actions, then
    /// "Delete" when the tree deletes and no action has that label.
    Menu {
        widget: String,
        id: Value,
        menu: RowMenu,
        text: String,
    },
}

impl Prompt {
    /// The line [`TextFrontend::render`] draws under the tree.
    pub fn line(&self) -> String {
        match self {
            Prompt::Rename { label, text, .. } => format!("rename \"{label}\": {text}"),
            Prompt::Menu { menu, text, .. } => {
                let entries: Vec<String> = menu
                    .entries()
                    .iter()
                    .enumerate()
                    .map(|(i, action)| format!("{} {action}", i + 1))
                    .collect();
                format!("menu: {} > {text}", entries.join("  "))
            }
        }
    }
}

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
    /// The prompt `r` or `m` opened, when one is waiting.
    pending: Option<Prompt>,
    /// The row `x` cut: the widget and the row id, waiting for `p` or `P`.
    cut: Option<(String, Value)>,
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
            pending: None,
            cut: None,
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
        let active: BTreeMap<String, Value> = tree
            .focus_order
            .iter()
            .filter_map(|id| Some((id.clone(), self.cursor.active_row(tree, id)?.clone())))
            .collect();
        let mut lines = Vec::new();
        write_node(
            &tree.root,
            0,
            focused,
            &active,
            self.style.sheet(),
            self.draw_blocks,
            &mut lines,
        );
        if let Some(prompt) = &self.pending {
            lines.push(prompt.line());
        }
        lines.join("\n")
    }

    /// Move the cursor, or build the event the key sends.
    ///
    /// `Down` and `Up` return `None`: they change which widget is focused (or
    /// which row of a `tree`, `table` or `list` is active) and do not tell
    /// the grammar anything happened. `Activate` on a widget with an active
    /// row is a `select` of it; on any other it is a `click` on the focused
    /// id. An empty focus order activates nothing. A direction or menu key
    /// that is only a [`koine::gesture::Gesture`] returns `None` here;
    /// [`Self::gesture_key`] is the one that reports it.
    ///
    /// The row keys are [`koine::cursor::Cursor::reply_for`]'s: `Space`,
    /// `h`, `l`, `d` and `o` send their events for the active row. The ones
    /// it leaves to the renderer are this frontend's: `r` and `m` open a
    /// one-line [`Prompt`] (answered by [`Self::type_char`] and `Activate`),
    /// `x` cuts the active row of a tree that declares `on_move`, and `p` and
    /// `P` move the cut row under or beside the active one as a `move`.
    ///
    /// `Activate` on a `tree` with no active row is a `select` of its first
    /// visible node, with `[id]` as the value, rather than the `click` a tree
    /// ignores. An empty tree sends the click.
    ///
    /// `Activate` on a `graph` is a `select` of its first node, `[id]`, for
    /// the same reason (the nodes are listed as lines under `with_blocks`).
    /// On a `plot` or an `image` it is a `click` whose value is `{"x": null,
    /// "y": null}`: there is no pointer, and a handler can tell.
    pub fn key(&mut self, tree: &RenderTree, key: Key) -> Option<Event> {
        if let Some(prompt) = self.pending.take() {
            return self.answer(tree, prompt, key);
        }
        let event = match self.cursor.reply_for(tree, key) {
            Reply::Event(event) => event,
            // The cursor took the key and moved; the caller redraws.
            Reply::Moved => return None,
            Reply::Unhandled => return self.row_key(tree, key),
        };
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

    /// The keys the cursor leaves to the renderer (`Reply::Unhandled`), acting
    /// on the focused widget's active row: `r` and `m` open a prompt, `x`
    /// remembers the row, `p` and `P` move it under or beside the active row
    /// (a `move` event). Each does nothing without an active row, and `r` is
    /// a tree's, `m` needs a menu with an entry (`actions`, or a tree that
    /// renames or deletes), and `x` `p` `P` need a tree with `on_move`.
    fn row_key(&mut self, tree: &RenderTree, key: Key) -> Option<Event> {
        let widget = self.cursor.focused_id(tree)?.to_string();
        let row = self.cursor.active_row(tree, &widget)?.clone();
        let kind = &find(&tree.root, &widget)?.node;
        match (kind, key) {
            (RenderKind::Tree { .. }, Key::Rename) => self.start_rename(tree, widget, row)?,
            (_, Key::Secondary | Key::Menu) => {
                let menu = RowMenu::of(kind).filter(|m| !m.entries().is_empty())?;
                self.pending = Some(Prompt::Menu {
                    widget,
                    id: row,
                    menu,
                    text: String::new(),
                });
            }
            (RenderKind::Tree { movable: true, .. }, Key::Cut) => self.cut = Some((widget, row)),
            (
                RenderKind::Tree {
                    nodes,
                    movable: true,
                    ..
                },
                Key::Paste | Key::PasteBeside,
            ) => {
                let (from, cut) = self.cut.clone()?;
                if from != widget {
                    return None;
                }
                let value = if key == Key::Paste {
                    paste_into(nodes, &cut, &row)?
                } else {
                    paste_beside(nodes, &cut, &row)?
                };
                self.cut = None;
                return Some(Event {
                    widget,
                    kind: EventKind::Move,
                    value,
                });
            }
            _ => {}
        }
        None
    }

    /// Open the rename prompt on a tree row, showing its current label.
    fn start_rename(&mut self, tree: &RenderTree, widget: String, row: Value) -> Option<()> {
        let Some(RenderKind::Tree { nodes, .. }) = find(&tree.root, &widget).map(|n| &n.node)
        else {
            return None;
        };
        let label = nodes.iter().find(|n| n.id == row)?.label.clone();
        self.pending = Some(Prompt::Rename {
            widget,
            id: row,
            label,
            text: String::new(),
        });
        Some(())
    }

    /// `Enter` answers a pending prompt: the typed text is the row's new
    /// label (`rename`, nothing for an empty text), the typed number picks the
    /// entry (nothing for a number out of range). The entry is
    /// [`MenuChoice`]'s: "Rename" on a tree that renames opens the rename
    /// prompt, the appended "Delete" sends `delete` with `[id]` (what `d`
    /// sends), any other a `menu` event. Any other key leaves the prompt as
    /// it was.
    fn answer(&mut self, tree: &RenderTree, prompt: Prompt, key: Key) -> Option<Event> {
        if key != Key::Activate {
            self.pending = Some(prompt);
            return None;
        }
        match prompt {
            Prompt::Rename {
                widget, id, text, ..
            } => {
                let text = text.trim();
                (!text.is_empty()).then(|| Event {
                    widget,
                    kind: EventKind::Rename,
                    value: serde_json::json!({"id": id, "label": text}),
                })
            }
            Prompt::Menu {
                widget,
                id,
                menu,
                text,
            } => {
                let picked = text
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| n.checked_sub(1))
                    .and_then(|n| menu.choice(n));
                match picked {
                    Some(MenuChoice::Action(action)) => Some(Event {
                        widget,
                        kind: EventKind::Menu,
                        value: serde_json::json!({"id": id, "action": action}),
                    }),
                    Some(MenuChoice::Delete) => Some(Event {
                        widget,
                        kind: EventKind::Delete,
                        value: Value::Array(vec![id]),
                    }),
                    Some(MenuChoice::Rename) => {
                        self.start_rename(tree, widget, id);
                        None
                    }
                    None => {
                        // Not an entry: ask again.
                        self.pending = Some(Prompt::Menu {
                            widget,
                            id,
                            menu,
                            text: String::new(),
                        });
                        None
                    }
                }
            }
        }
    }

    /// The prompt `r` or `m` opened, while it waits for an answer.
    pub fn prompt(&self) -> Option<&Prompt> {
        self.pending.as_ref()
    }

    /// The row `x` cut: the widget id and the row id.
    pub fn cut(&self) -> Option<&(String, Value)> {
        self.cut.as_ref()
    }

    /// A typed character for the pending prompt: any printable character for
    /// a rename, a digit for a menu. Returns whether a prompt took it, so a
    /// host routes characters here while [`Self::prompt`] is `Some` and to
    /// [`Key::from_name`] otherwise.
    pub fn type_char(&mut self, c: char) -> bool {
        match &mut self.pending {
            Some(Prompt::Rename { text, .. }) => {
                if !c.is_control() {
                    text.push(c);
                }
                true
            }
            Some(Prompt::Menu { text, .. }) => {
                if c.is_ascii_digit() {
                    text.push(c);
                }
                true
            }
            None => false,
        }
    }

    /// Delete the last typed character of the pending prompt.
    pub fn backspace(&mut self) {
        if let Some(Prompt::Rename { text, .. } | Prompt::Menu { text, .. }) = &mut self.pending {
            text.pop();
        }
    }

    /// Drop the pending prompt (Escape). Returns whether there was one.
    pub fn cancel(&mut self) -> bool {
        self.pending.take().is_some()
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
    active: &BTreeMap<String, Value>,
    sheet: &koine::style::StyleSheet,
    draw_blocks: bool,
    out: &mut Vec<String>,
) {
    let focus_mark = focus_mark_of(sheet);
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
        // The active row carries the focus mark, as the focused widget does;
        // a count follows the label in parentheses.
        let active_row = active.get(&node.id);
        for n in nodes {
            let disclosure = match (n.has_children, n.expanded) {
                (false, _) => "  ",
                (true, true) => "\u{25be} ",
                (true, false) => "\u{25b8} ",
            };
            let lead = if active_row == Some(&n.id) {
                &focus_mark_of(sheet)
            } else {
                &under
            };
            let count = match &n.count {
                Some(Value::String(s)) => format!(" ({s})"),
                Some(count) => format!(" ({count})"),
                None => String::new(),
            };
            out.push(format!(
                "{lead}{pad}  {}{disclosure}{}{count}",
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
        write_node(
            child,
            child_indent,
            focused,
            active,
            sheet,
            draw_blocks,
            out,
        );
    }
}

/// The `focus` slot's text, or `>` when the sheet leaves it empty.
fn focus_mark_of(sheet: &koine::style::StyleSheet) -> String {
    let mark = fill(sheet.value("focus"), &[]);
    if mark.is_empty() {
        ">".to_string()
    } else {
        mark
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
                count: None,
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
            expanded: vec![],
            actions: vec![],
            movable: false,
            renameable: false,
            deletable: false,
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
            expanded: vec![],
            actions: vec![],
            movable: false,
            renameable: false,
            deletable: false,
        };
        let event = TextFrontend::new().key(&empty, Key::Activate).unwrap();
        assert_eq!(event.kind, EventKind::Click);
    }

    // ── the outline (plan-koine-outline) ──────────────────────────────────

    /// `outline()` with a menu, `on_move`, and counts on `src` (3) and `docs`
    /// (0): src, main.rs under it, docs.
    fn sidebar() -> RenderTree {
        let mut tree = outline();
        if let RenderKind::Tree {
            nodes,
            actions,
            movable,
            ..
        } = &mut tree.root.node
        {
            nodes[0].count = Some(serde_json::json!(3));
            nodes[2].count = Some(serde_json::json!(0));
            *actions = vec!["Rename".into(), "Delete".into()];
            *movable = true;
        }
        tree
    }

    fn keys(frontend: &mut TextFrontend, tree: &RenderTree, keys: &[Key]) {
        for key in keys {
            frontend.key(tree, *key);
        }
    }

    fn type_text(frontend: &mut TextFrontend, text: &str) {
        for c in text.chars() {
            assert!(frontend.type_char(c));
        }
    }

    #[test]
    fn counts_follow_the_label_and_the_active_row_carries_the_focus_mark() {
        let tree = sidebar();
        let mut frontend = TextFrontend::new().with_blocks(true);
        // No active row yet: only counts, a zero among them.
        assert_eq!(
            frontend.render(&tree).lines().collect::<Vec<_>>(),
            vec![
                ">tree 3",
                "   \u{25be} src (3)",
                "       main.rs",
                "   \u{25b8} docs (0)"
            ]
        );
        // j: the first row is active; j j: the third.
        assert!(frontend.key(&tree, Key::Down).is_none());
        let text = frontend.render(&tree);
        assert_eq!(text.lines().nth(1), Some(">  \u{25be} src (3)"), "{text}");
        keys(&mut frontend, &tree, &[Key::Down, Key::Down]);
        let text = frontend.render(&tree);
        assert_eq!(text.lines().nth(1), Some("   \u{25be} src (3)"), "{text}");
        assert_eq!(text.lines().nth(3), Some(">  \u{25b8} docs (0)"), "{text}");
        // The widget focus mark on the header is still there.
        assert_eq!(text.lines().next(), Some(">tree 3"));
    }

    #[test]
    fn enter_selects_the_active_row_and_the_row_keys_send_their_events() {
        let tree = sidebar();
        let mut frontend = TextFrontend::new();
        keys(&mut frontend, &tree, &[Key::Down, Key::Down]);
        let event = frontend.key(&tree, Key::Activate).unwrap();
        assert_eq!(
            (event.kind, event.value),
            (EventKind::Select, serde_json::json!(["b"]))
        );
        let event = frontend.key(&tree, Key::Delete).unwrap();
        assert_eq!(
            (event.kind, event.value),
            (EventKind::Delete, serde_json::json!(["b"]))
        );
        let event = frontend.key(&tree, Key::Open).unwrap();
        assert_eq!(
            (event.kind, event.value),
            (EventKind::Activate, serde_json::json!(["b"]))
        );
    }

    #[test]
    fn r_asks_for_a_new_label_on_one_line_and_enter_sends_rename() {
        let tree = sidebar();
        let mut frontend = TextFrontend::new().with_blocks(true);
        // No active row: nothing to rename.
        assert!(frontend.key(&tree, Key::Rename).is_none());
        assert!(frontend.prompt().is_none());
        assert!(!frontend.type_char('x'), "no prompt takes no characters");

        keys(&mut frontend, &tree, &[Key::Down, Key::Down, Key::Down]);
        assert!(frontend.key(&tree, Key::Rename).is_none());
        assert_eq!(
            frontend.prompt(),
            Some(&Prompt::Rename {
                widget: "files".into(),
                id: serde_json::json!(7),
                label: "docs".into(),
                text: String::new()
            })
        );
        type_text(&mut frontend, "Papers!");
        frontend.backspace();
        let text = frontend.render(&tree);
        assert_eq!(
            text.lines().last(),
            Some("rename \"docs\": Papers"),
            "{text}"
        );
        // Other keys wait.
        assert!(frontend.key(&tree, Key::Up).is_none());
        assert!(frontend.prompt().is_some());
        let event = frontend.key(&tree, Key::Activate).unwrap();
        assert_eq!(
            (event.widget.as_str(), event.kind, event.value),
            (
                "files",
                EventKind::Rename,
                serde_json::json!({"id": 7, "label": "Papers"})
            )
        );
        assert!(frontend.prompt().is_none());
        assert!(!frontend.render(&tree).contains("rename"));

        // An empty answer sends nothing; Escape drops the prompt.
        frontend.key(&tree, Key::Rename);
        assert!(frontend.key(&tree, Key::Activate).is_none());
        assert!(frontend.prompt().is_none());
        frontend.key(&tree, Key::Rename);
        type_text(&mut frontend, "x");
        assert!(frontend.cancel());
        assert!(!frontend.cancel());
        assert!(frontend.prompt().is_none());
    }

    #[test]
    fn m_lists_the_actions_and_a_digit_with_enter_sends_menu() {
        let tree = sidebar();
        let mut frontend = TextFrontend::new().with_blocks(true);
        assert!(frontend.key(&tree, Key::Secondary).is_none());
        assert!(frontend.prompt().is_none(), "no active row, no menu");

        frontend.key(&tree, Key::Down);
        assert!(frontend.key(&tree, Key::Secondary).is_none());
        let text = frontend.render(&tree);
        assert_eq!(
            text.lines().last(),
            Some("menu: 1 Rename  2 Delete > "),
            "{text}"
        );
        // Only digits are taken.
        type_text(&mut frontend, "x2");
        assert_eq!(
            frontend.render(&tree).lines().last(),
            Some("menu: 1 Rename  2 Delete > 2")
        );
        let event = frontend.key(&tree, Key::Activate).unwrap();
        assert_eq!(
            (event.widget.as_str(), event.kind, event.value),
            (
                "files",
                EventKind::Menu,
                serde_json::json!({"id": "a", "action": "Delete"})
            )
        );
        assert!(frontend.prompt().is_none());

        // The physical menu key is the same; 3 is not an entry and asks again.
        frontend.key(&tree, Key::Menu);
        type_text(&mut frontend, "3");
        assert!(frontend.key(&tree, Key::Activate).is_none());
        assert!(matches!(
            frontend.prompt(),
            Some(Prompt::Menu { text, .. }) if text.is_empty()
        ));
        type_text(&mut frontend, "1");
        let event = frontend.key(&tree, Key::Activate).unwrap();
        assert_eq!(
            event.value,
            serde_json::json!({"id": "a", "action": "Rename"})
        );

        // A tree with no actions has no menu.
        let mut plain = TextFrontend::new();
        let tree = outline();
        plain.key(&tree, Key::Down);
        plain.key(&tree, Key::Secondary);
        assert!(plain.prompt().is_none());
    }

    #[test]
    fn the_menu_lists_rename_and_delete_of_a_tree_that_handles_them() {
        let mut tree = outline();
        if let RenderKind::Tree {
            actions,
            renameable,
            deletable,
            ..
        } = &mut tree.root.node
        {
            *actions = vec!["Add child".into()];
            *renameable = true;
            *deletable = true;
        }
        let mut frontend = TextFrontend::new().with_blocks(true);
        keys(&mut frontend, &tree, &[Key::Down, Key::Down, Key::Down]);
        frontend.key(&tree, Key::Menu);
        assert_eq!(
            frontend.render(&tree).lines().last(),
            Some("menu: 1 Rename  2 Add child  3 Delete > ")
        );
        // "Rename" is the frontend's: it opens the rename prompt, no event.
        type_text(&mut frontend, "1");
        assert!(frontend.key(&tree, Key::Activate).is_none());
        assert!(matches!(
            frontend.prompt(),
            Some(Prompt::Rename { label, .. }) if label == "docs"
        ));
        assert!(frontend.cancel());
        // The appended "Delete" sends what `d` sends.
        frontend.key(&tree, Key::Menu);
        type_text(&mut frontend, "3");
        let event = frontend.key(&tree, Key::Activate).unwrap();
        assert_eq!(
            (event.kind, event.value),
            (EventKind::Delete, serde_json::json!([7]))
        );
        // The spec's own action is a `menu` event.
        frontend.key(&tree, Key::Menu);
        type_text(&mut frontend, "2");
        let event = frontend.key(&tree, Key::Activate).unwrap();
        assert_eq!(
            (event.kind, event.value),
            (
                EventKind::Menu,
                serde_json::json!({"id": 7, "action": "Add child"})
            )
        );
        // A tree that only renames has a menu of one, even with no actions.
        if let RenderKind::Tree {
            actions, deletable, ..
        } = &mut tree.root.node
        {
            actions.clear();
            *deletable = false;
        }
        frontend.key(&tree, Key::Menu);
        assert_eq!(
            frontend.render(&tree).lines().last(),
            Some("menu: 1 Rename > ")
        );
    }

    #[test]
    fn x_then_p_moves_the_cut_row_under_the_active_one_and_capital_p_beside_it() {
        let tree = sidebar();
        let mut frontend = TextFrontend::new();
        // p before x has nothing to paste.
        keys(&mut frontend, &tree, &[Key::Down]);
        assert!(frontend.key(&tree, Key::Paste).is_none());
        // Cut `docs` (the third row), go to `src`, paste under it.
        keys(&mut frontend, &tree, &[Key::Down, Key::Down]);
        assert!(frontend.key(&tree, Key::Cut).is_none());
        assert_eq!(
            frontend.cut(),
            Some(&("files".to_string(), serde_json::json!(7)))
        );
        keys(&mut frontend, &tree, &[Key::Up, Key::Up]);
        let event = frontend.key(&tree, Key::Paste).unwrap();
        assert_eq!(
            (event.widget.as_str(), event.kind, event.value),
            (
                "files",
                EventKind::Move,
                serde_json::json!({"ids": [7], "to": "a", "index": 0})
            )
        );
        assert!(frontend.cut().is_none(), "a paste empties the clipboard");

        // Cut again; beside `main.rs` (second child position of `src`).
        keys(
            &mut frontend,
            &tree,
            &[Key::Down, Key::Down, Key::Cut, Key::Up],
        );
        let event = frontend.key(&tree, Key::PasteBeside).unwrap();
        assert_eq!(
            event.value,
            serde_json::json!({"ids": [7], "to": "a", "index": 1})
        );

        // Under itself: nothing, and the cut stays.
        keys(&mut frontend, &tree, &[Key::Up, Key::Cut]);
        assert!(frontend.key(&tree, Key::Paste).is_none());
        assert!(frontend.cut().is_some());
    }

    #[test]
    fn a_tree_without_on_move_has_no_cut_and_a_rename_needs_a_tree() {
        let tree = outline();
        let mut frontend = TextFrontend::new();
        keys(&mut frontend, &tree, &[Key::Down, Key::Cut]);
        assert!(frontend.cut().is_none());
        assert!(frontend.key(&tree, Key::Paste).is_none());
        assert!(frontend.key(&tree, Key::PasteBeside).is_none());
        // A list: a menu yes (when it has actions), a rename no.
        let list = node_tree(RenderKind::List {
            rows: serde_json::json!(["alpha", "beta"]),
            actions: vec!["Pin".into()],
            count_field: None,
        });
        let mut list = list;
        list.root.id = "l".into();
        list.focus_order = vec!["l".into()];
        let mut frontend = TextFrontend::new();
        frontend.key(&list, Key::Down);
        frontend.key(&list, Key::Rename);
        assert!(frontend.prompt().is_none());
        frontend.key(&list, Key::Secondary);
        assert!(matches!(frontend.prompt(), Some(Prompt::Menu { .. })));
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
