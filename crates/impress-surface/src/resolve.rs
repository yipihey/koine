//! `resolve(&spec, &state, &params, &SourceData) -> RenderTree`: what the human
//! sees. Every template reference is replaced by its value, every `when` is
//! evaluated, every node gets an id, and the whole thing is what
//! `surface_render` (S4) returns verbatim — an agent inspects its own GUI by
//! calling that verb, no screenshot involved (ADR-0033 D2).
//!
//! # The `RenderTree` wire shape is deliberately not the spec's
//!
//! [`Node`]/[`NodeKind`] hand-roll their serde to match the plan's authoring
//! shape byte for byte (see `spec.rs`'s module docs) — that discipline exists so
//! an agent's hand-written JSON parses. `RenderTree` has no such constraint: it
//! is *this crate's* output, read only by a renderer this crate does not ship,
//! so [`RenderKind`] uses the ordinary internally-tagged
//! `#[serde(tag = "kind", rename_all = "snake_case")]` representation
//! (`{"kind": "column", "items": […]}`) — simpler to derive, simpler for a
//! renderer to `switch` on, and, unlike the spec, never needs to tolerate an
//! unrecognized *output* kind (this crate is the only thing that produces one).
//!
//! # Handlers do not appear in a `RenderNode`
//!
//! See the crate docs: the renderer is dumb (D2) and forwards every interaction
//! as a raw [`Event`]; [`crate::reduce::reduce`] is what decides whether a
//! widget's id has a matching handler. A `RenderNode` therefore carries display
//! data only.
//!
//! # Placeholders
//!
//! A node degrades to [`RenderKind::Placeholder`], keeping its slot in the tree,
//! in exactly two cases (ADR-0033 "Defaults" and this crate's own extension of
//! it):
//!
//! - its kind is [`NodeKind::Unknown`] — a tag key this build does not
//!   recognize: `unknown_kind` is that key and `node` the node's JSON, so a
//!   renderer, or an agent reading `surface_render`, can see what the newer
//!   widget was (ADR-0033 "Defaults": a placeholder "keeps the node"; review
//!   RS-S20);
//! - its kind is [`NodeKind::Invalid`] — a node this build could not read:
//!   `reason` says why, `node` is its JSON;
//! - a known kind's own content failed to resolve (an unbound `state`/`source`
//!   reference, a missing dependency) — carrying `reason`, the
//!   [`crate::template::TemplateError`]'s message.
//!
//! Either way the rest of the tree renders normally: one bad reference never
//! blanks the whole surface.

use serde_json::{Map, Value};

use crate::envelope::{first_row_keys, unwrap_rows};
use crate::spec::{node_id, Node, NodeKind, SurfaceSpec, When};
use crate::template::{resolve_value, Context, Template, TemplateError};

/// What `surface_render` returns: the resolved tree plus a flat, reading-order
/// list of focusable widget ids, for keyboard navigation (ADR-0033 "Defaults":
/// "widget focus is keyboard-first: j/k walk widgets").
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RenderTree {
    pub root: RenderNode,
    /// Ids of every `field`, `button`, `table`, `list` and `tabs` node, in
    /// depth-first reading order. A `tabs` container contributes one stop for
    /// itself (switching tabs); the focusable content inside each tab's body
    /// contributes its own stops through ordinary recursion.
    pub focus_order: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RenderNode {
    /// Always present — author-given or auto-derived ([`node_id`]), unlike
    /// [`Node::id`].
    pub id: String,
    /// The common `Node::label` annotation — distinct from, and not to be
    /// confused with, a `button`'s own `label` (its display text): the two are
    /// different concepts that happen to share a name in the spec vocabulary
    /// (see `spec.rs`'s module docs on `Node`'s common keys vs. a kind's own
    /// sibling keys). `node` is nested rather than flattened for exactly this
    /// reason — flattening [`RenderKind`] into this struct would make its
    /// `Button { label }` key collide with this field during deserialization
    /// (the outer, *named* field wins the key, leaving the flattened enum
    /// short one), silently breaking any future kind whose own content also
    /// happens to be called `label`, `id` or `help`. A nested `node` field
    /// makes that class of collision structurally impossible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    pub node: RenderKind,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RenderKind {
    Column {
        items: Vec<RenderNode>,
    },
    Row {
        items: Vec<RenderNode>,
    },
    Grid {
        columns: u32,
        items: Vec<RenderNode>,
    },
    Section {
        title: String,
        collapsed: bool,
        body: Box<RenderNode>,
    },
    Tabs {
        tabs: Vec<RenderTab>,
    },
    Text {
        text: String,
    },
    Table {
        rows: Value,
        columns: Vec<String>,
        /// The row field a `select` carries (`Table::id_field`); absent when
        /// the table selects by `id`, then by index.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id_field: Option<String>,
        /// The labels of the row menu (`Table::actions`); a `menu` event
        /// names one. Absent when the table declares none.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        actions: Vec<String>,
        /// The row field a renderer draws as the count (`Table::count_field`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        count_field: Option<String>,
    },
    List {
        rows: Value,
        /// As [`RenderKind::Table`]'s `actions`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        actions: Vec<String>,
        /// As [`RenderKind::Table`]'s `count_field`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        count_field: Option<String>,
    },
    /// `tree{…}` (plan-koine-tree-stream T-D1): the visible outline, already
    /// nested and filtered by the expanded ids, in drawing order.
    Tree {
        nodes: Vec<crate::tree::TreeNode>,
        /// The state path of the expanded list (`Tree::bind`); a disclosure
        /// click sends `change` for this widget with the new list.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bind: Option<String>,
        /// The whole expanded list read from `bind` (plan-koine-outline
        /// O-D2), including ids of branches a collapsed ancestor hides. A
        /// disclosure toggle sends this list with one id added or removed,
        /// so collapsing a branch keeps its descendants' state.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        expanded: Vec<Value>,
        /// The labels of the row menu (`Tree::actions`).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        actions: Vec<String>,
    },
    /// `graph{…}` (plan-koine-tree-stream T-D3): `spec` is the
    /// `koine_plot::graph::GraphSpec` the rows became (ids as text), `svg`
    /// its drawing (`render_graph_svg`: a `data-id` on every node group). A
    /// click on a node sends `select` with `[id]`.
    Graph {
        spec: Value,
        svg: String,
    },
    Plot {
        spec: Value,
    },
    /// `code{language, text}` (plan-koine-live L-D7): `text` is the resolved
    /// string, shown as written. `spans` (plan-koine-tree-stream T-D5) is
    /// the same text highlighted: one list per line of `(class, text)`, the
    /// class one of `highlight::CLASSES` or empty for plain text. Empty when
    /// the resolver was built without the `highlight` feature, for a
    /// language it does not know, and for a `diff`.
    Code {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        spans: Vec<Vec<(String, String)>>,
    },
    Image {
        blob: Option<Value>,
        url: Option<Value>,
    },
    Field {
        field: Value,
        bind: Option<String>,
        value: Value,
    },
    Button {
        label: String,
    },
    Status {
        level: String,
        message: String,
    },
    Log {
        lines: Value,
    },
    Kv {
        pairs: Value,
    },
    Divider,
    Spacer,
    /// See the module docs. `unknown_kind` (not `kind` — that name is the
    /// enum's own internal tag key and serde refuses the collision) carries the
    /// original tag key for a [`NodeKind::Unknown`] node.
    Placeholder {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unknown_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// The node's own JSON, for an unknown or unreadable node — what the
        /// placeholder stands in for. Absent for a known node whose content
        /// did not resolve (its spec is the author's, and its `reason` says
        /// which reference failed).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node: Option<Value>,
    },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RenderTab {
    pub title: String,
    pub body: RenderNode,
}

/// The labels of a row menu, in declaration order.
fn menu_labels(actions: &[crate::spec::MenuAction]) -> Vec<String> {
    actions.iter().map(|a| a.label.clone()).collect()
}

/// See the module docs.
pub fn resolve(spec: &SurfaceSpec, state: &Value, params: &Value, source: &Value) -> RenderTree {
    resolve_with_source_errors(spec, state, params, source, None)
}

/// [`resolve`], with the reasons the runtime knows for sources that are
/// missing from `source`. A node whose template names such a source becomes
/// a placeholder that says WHY — "source 'libs' failed: imbib is not
/// running" — instead of the bare "did not resolve to a value", which read
/// identically for a typo, a closed app and a refused verb (Mac,
/// 2026-09-23). The tree's shape does not change; only the reason string.
pub fn resolve_with_source_errors(
    spec: &SurfaceSpec,
    state: &Value,
    params: &Value,
    source: &Value,
    source_errors: Option<&std::collections::BTreeMap<String, String>>,
) -> RenderTree {
    let null = Value::Null;
    let mut ctx = Context::new(state, params, source, &null);
    ctx.source_errors = source_errors;
    let mut focus_order = Vec::new();
    let root = resolve_node(&spec.root, vec![0], &ctx, state, &mut focus_order);
    RenderTree { root, focus_order }
}

/// The placeholder's reason: the template error, prefixed with the source's
/// own failure when the unresolved path points at a source the runtime
/// reported on.
fn placeholder_reason(error: &TemplateError, ctx: &Context) -> String {
    if let (TemplateError::MissingPath { path }, Some(errors)) = (error, ctx.source_errors) {
        let mut segments = path.split('.');
        if segments.next() == Some("source") {
            if let Some(name) = segments.next() {
                if let Some(why) = errors.get(name) {
                    return format!("source '{name}' failed: {why} — {error}");
                }
            }
        }
    }
    error.to_string()
}

fn resolve_node(
    node: &Node,
    path: Vec<usize>,
    ctx: &Context,
    state: &Value,
    focus_order: &mut Vec<String>,
) -> RenderNode {
    let id = node_id(node, &path);
    let label = node.label.as_ref().map(|l| template_string(l, ctx));
    let help = node.help.as_ref().map(|h| template_string(h, ctx));

    let kind = match resolve_kind(node, &path, ctx, state, focus_order) {
        Ok(k) => k,
        Err(reason) => RenderKind::Placeholder {
            unknown_kind: None,
            reason: Some(placeholder_reason(&reason, ctx)),
            node: None,
        },
    };

    // A plot or an image is a stop only when it declares `on_click`: a
    // keyboard has something to do there (a TUI sends `click` with null
    // coordinates), and a surface without one keeps its focus order.
    let clickable = match &node.kind {
        NodeKind::Plot(p) => !p.on_click.is_empty(),
        NodeKind::Image(i) => !i.on_click.is_empty(),
        _ => false,
    };
    if clickable
        || matches!(
            kind,
            RenderKind::Field { .. }
                | RenderKind::Button { .. }
                | RenderKind::Table { .. }
                | RenderKind::List { .. }
                | RenderKind::Tree { .. }
                | RenderKind::Graph { .. }
                | RenderKind::Tabs { .. }
        )
    {
        focus_order.push(id.clone());
    }

    RenderNode {
        id,
        label,
        help,
        node: kind,
    }
}

/// A template string that fails to resolve renders as the literal text of the
/// `{{…}}` it could not fill — good enough for a label or a title, which are
/// decoration rather than the node's substance; a widget's *substantive*
/// content failing (its `text`, `rows`, `spec`, …) is what turns the whole node
/// into a placeholder instead (see [`resolve_kind`]).
fn template_string(s: &str, ctx: &Context) -> String {
    match Template::parse(s).resolve(ctx) {
        Ok(Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => s.to_string(),
    }
}

fn resolve_kind(
    node: &Node,
    path: &[usize],
    ctx: &Context,
    state: &Value,
    focus_order: &mut Vec<String>,
) -> Result<RenderKind, TemplateError> {
    Ok(match &node.kind {
        NodeKind::Column(children) => RenderKind::Column {
            items: resolve_children(children, path, ctx, state, focus_order),
        },
        NodeKind::Row(children) => RenderKind::Row {
            items: resolve_children(children, path, ctx, state, focus_order),
        },
        NodeKind::Grid(g) => RenderKind::Grid {
            columns: g.columns,
            items: resolve_children(&g.items, path, ctx, state, focus_order),
        },
        NodeKind::Section(s) => {
            let mut body_path = path.to_vec();
            body_path.push(0);
            RenderKind::Section {
                title: template_string(&s.title, ctx),
                collapsed: s.collapsed,
                body: Box::new(resolve_node(&s.body, body_path, ctx, state, focus_order)),
            }
        }
        NodeKind::Tabs(tabs) => {
            let mut out = Vec::with_capacity(tabs.len());
            for (i, tab) in tabs.iter().enumerate() {
                let mut tab_path = path.to_vec();
                tab_path.push(i);
                out.push(RenderTab {
                    title: template_string(&tab.title, ctx),
                    body: resolve_node(&tab.body, tab_path, ctx, state, focus_order),
                });
            }
            RenderKind::Tabs { tabs: out }
        }
        NodeKind::Text(t) => RenderKind::Text {
            text: value_to_display(&Template::parse(t).resolve(ctx)?),
        },
        NodeKind::Table(t) => {
            // A `{rows, next}` envelope (L-D5) is read for its rows, and for
            // the columns and id field the node leaves undeclared; a bare
            // array is as it always was.
            let resolved = resolve_value(&t.rows, ctx)?;
            let (rows, envelope_columns, envelope_id, _) = unwrap_rows(&resolved);
            let enveloped = !std::ptr::eq(rows, &resolved);
            let columns = if !t.columns.is_empty() {
                t.columns.clone()
            } else if let Some(columns) = envelope_columns {
                columns
            } else if enveloped {
                first_row_keys(rows)
            } else {
                Vec::new()
            };
            RenderKind::Table {
                id_field: t.id_field.clone().or(envelope_id),
                columns,
                rows: rows.clone(),
                actions: menu_labels(&t.actions),
                count_field: t.count_field.clone(),
            }
        }
        NodeKind::List(l) => {
            let resolved = resolve_value(&l.rows, ctx)?;
            RenderKind::List {
                rows: unwrap_rows(&resolved).0.clone(),
                actions: menu_labels(&l.actions),
                count_field: l.count_field.clone(),
            }
        }
        NodeKind::Tree(t) => {
            let resolved = resolve_value(&t.rows, ctx)?;
            let expanded = t
                .bind
                .as_ref()
                .and_then(|b| crate::state_path::read(state, b).ok())
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default();
            RenderKind::Tree {
                nodes: crate::tree::build(
                    unwrap_rows(&resolved).0,
                    &expanded,
                    crate::tree::Fields {
                        id: t.id_field.as_deref(),
                        parent: t.parent_field.as_deref(),
                        label: t.label_field.as_deref(),
                        count: t.count_field.as_deref(),
                    },
                ),
                bind: t.bind.clone(),
                expanded,
                actions: menu_labels(&t.actions),
            }
        }
        NodeKind::Graph(g) => resolve_graph(g, ctx)?,
        NodeKind::Plot(p) => RenderKind::Plot {
            spec: resolve_plot(p, ctx)?,
        },
        NodeKind::Code(c) => {
            let text = value_to_display(&resolve_value(&c.text, ctx)?);
            // A `diff` keeps its line classes (the renderers mark `+`, `-`
            // and `@@`); every other named language is the highlighter's.
            let spans = match c.language.as_deref() {
                Some(language) if !language.eq_ignore_ascii_case("diff") => {
                    crate::highlight::spans(language, &text)
                }
                _ => Vec::new(),
            };
            RenderKind::Code {
                language: c.language.clone(),
                text,
                spans,
            }
        }
        NodeKind::Image(img) => RenderKind::Image {
            blob: img
                .blob
                .as_ref()
                .map(|v| resolve_value(v, ctx))
                .transpose()?,
            url: img
                .url
                .as_ref()
                .map(|v| resolve_value(v, ctx))
                .transpose()?,
        },
        NodeKind::Field(f) => {
            let field_value = field_options(f);
            let value = node
                .bind
                .as_ref()
                .and_then(|b| crate::state_path::read(state, b).ok())
                .unwrap_or(Value::Null);
            RenderKind::Field {
                field: resolve_value(&field_value, ctx)?,
                bind: node.bind.clone(),
                value,
            }
        }
        NodeKind::Button(b) => RenderKind::Button {
            label: template_string(&b.label, ctx),
        },
        NodeKind::Status(s) => RenderKind::Status {
            level: s.level.clone(),
            message: value_to_display(&resolve_value(&s.message, ctx)?),
        },
        NodeKind::Log(v) => RenderKind::Log {
            lines: resolve_value(v, ctx)?,
        },
        NodeKind::Kv(v) => RenderKind::Kv {
            pairs: resolve_value(v, ctx)?,
        },
        NodeKind::Divider => RenderKind::Divider,
        NodeKind::Spacer => RenderKind::Spacer,
        NodeKind::Unknown { kind, .. } => RenderKind::Placeholder {
            unknown_kind: Some(kind.clone()),
            reason: Some(format!("this build has no '{kind}' widget")),
            node: Some(node.to_value()),
        },
        NodeKind::Invalid {
            error, node: raw, ..
        } => RenderKind::Placeholder {
            unknown_kind: None,
            reason: Some(error.clone()),
            node: Some(raw.clone()),
        },
    })
}

/// A graph: the node rows and edge rows built into a
/// `koine_plot::graph::GraphSpec` (ids as text, a number as its text; a row
/// without an id, or an edge missing an end, is skipped) and drawn. Rows that
/// are not an array make the node a placeholder saying so, as a template that
/// does not resolve does.
fn resolve_graph(g: &crate::spec::Graph, ctx: &Context) -> Result<RenderKind, TemplateError> {
    use koine_plot::graph::{GraphEdge, GraphNode, GraphSpec};

    fn text(v: Option<&Value>) -> Option<String> {
        match v? {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    }

    let nodes = resolve_value(&g.nodes, ctx)?;
    let edges = resolve_value(&g.edges, ctx)?;
    let (Some(nodes), Some(edges)) = (
        unwrap_rows(&nodes).0.as_array(),
        unwrap_rows(&edges).0.as_array(),
    ) else {
        return Ok(RenderKind::Placeholder {
            unknown_kind: None,
            reason: Some(
                "graph.nodes and graph.edges must each resolve to an array of rows".into(),
            ),
            node: None,
        });
    };
    let id = g.id_field.as_deref().unwrap_or("id");
    let label = g.label_field.as_deref().unwrap_or("label");
    let source = g.source_field.as_deref().unwrap_or("source");
    let target = g.target_field.as_deref().unwrap_or("target");

    let spec = GraphSpec {
        nodes: nodes
            .iter()
            .filter_map(|row| {
                Some(GraphNode {
                    id: text(row.get(id))?,
                    label: text(row.get(label)),
                })
            })
            .collect(),
        edges: edges
            .iter()
            .filter_map(|row| {
                Some(GraphEdge {
                    source: text(row.get(source))?,
                    target: text(row.get(target))?,
                })
            })
            .collect(),
        ..GraphSpec::default()
    };
    let svg = koine_plot::graph::render_graph_svg(&spec);
    Ok(RenderKind::Graph {
        spec: serde_json::to_value(&spec).unwrap_or(Value::Null),
        svg,
    })
}

/// A plot's spec: the opaque `spec` resolved, or the bound form built into the
/// same shape (L-D8). One series per `y` field, labelled by the field, the x
/// axis labelled by `x`; a row whose `x` or `y` is not a JSON number (a numeric
/// string is not one) is skipped, so a table with a text column still plots.
/// A node with neither form (or both) is `validate`'s to report; here it
/// resolves to what it has, `spec` first.
fn resolve_plot(plot: &crate::spec::Plot, ctx: &Context) -> Result<Value, TemplateError> {
    if let Some(spec) = &plot.spec {
        return resolve_value(spec, ctx);
    }
    let resolved = match &plot.rows {
        Some(rows) => resolve_value(rows, ctx)?,
        None => Value::Null,
    };
    let (rows, ..) = unwrap_rows(&resolved);
    let rows: &[Value] = rows.as_array().map_or(&[], Vec::as_slice);
    let x = plot.x.as_deref().unwrap_or_default();
    let ys: Vec<&str> = match &plot.y {
        Some(Value::String(y)) => vec![y.as_str()],
        Some(Value::Array(ys)) => ys.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    // The colours `koine-plot` cycles through (`PlotColor::from_index`), so the
    // series of a bound plot are told apart.
    const COLOURS: [&str; 6] = ["Blue", "Red", "Green", "Orange", "Purple", "Cyan"];
    let series: Vec<Value> = ys
        .into_iter()
        .enumerate()
        .map(|(index, y)| {
            let (mut xs, mut vs) = (Vec::new(), Vec::new());
            for row in rows {
                if let (Some(Value::Number(a)), Some(Value::Number(b))) = (row.get(x), row.get(y)) {
                    xs.push(Value::Number(a.clone()));
                    vs.push(Value::Number(b.clone()));
                }
            }
            serde_json::json!({
                "label": y,
                "x": xs,
                "y": vs,
                "style": "Line",
                "color": COLOURS[index % COLOURS.len()],
            })
        })
        .collect();
    Ok(serde_json::json!({ "series": series, "x_axis": { "label": x } }))
}

fn resolve_children(
    children: &[Node],
    parent_path: &[usize],
    ctx: &Context,
    state: &Value,
    focus_order: &mut Vec<String>,
) -> Vec<RenderNode> {
    let mut out = Vec::with_capacity(children.len());
    for (i, child) in children.iter().enumerate() {
        if let Some(when) = &child.when {
            if !when_is_true(when, ctx) {
                continue; // omitted, per the plan's `when` semantics
            }
        }
        let mut child_path = parent_path.to_vec();
        child_path.push(i);
        out.push(resolve_node(child, child_path, ctx, state, focus_order));
    }
    out
}

fn when_is_true(when: &When, ctx: &Context) -> bool {
    let resolved = match Template::parse(&format!("{{{{{}}}}}", when.path)).resolve(ctx) {
        Ok(v) => v,
        Err(_) => return false, // an unresolvable condition hides the node
    };
    match &when.equals {
        Some(expected) => &resolved == expected,
        None => is_truthy(&resolved),
    }
}

fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

/// The `field` widget's own tag+options as a `Value` (`{"slider": {…}}`), so it
/// can go through the same `resolve_value` every other opaque payload does.
fn field_options(f: &crate::spec::FieldKind) -> Value {
    let (tag, options) = match f {
        crate::spec::FieldKind::Text(v) => ("text", v),
        crate::spec::FieldKind::Number(v) => ("number", v),
        crate::spec::FieldKind::Slider(v) => ("slider", v),
        crate::spec::FieldKind::Select(v) => ("select", v),
        crate::spec::FieldKind::Toggle(v) => ("toggle", v),
        crate::spec::FieldKind::Date(v) => ("date", v),
        crate::spec::FieldKind::File(v) => ("file", v),
    };
    let mut map = Map::new();
    map.insert(tag.to_string(), options.clone());
    Value::Object(map)
}

/// The same stringification [`Template::resolve`]'s mixed-text branch applies to
/// a single reference, used for `text`/`status.message`: strings raw, numbers
/// and bools via `Display`, everything else compact JSON.
fn value_to_display(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}
