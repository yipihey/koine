//! The `SurfaceSpec` vocabulary (ADR-0033 D3; `docs/plan-agent-surfaces.md`
//! "The vocabulary (normative)" is the one definition — this module implements it
//! exactly, and notes every place it had to fill a gap).
//!
//! # Wire shapes
//!
//! Every serde shape below is chosen to match the plan's worked example byte for
//! byte (as JSON *values*; key order is never significant — `serde_json::Value`'s
//! map is a `BTreeMap` in this workspace, so it can't be anyway). Two patterns
//! recur:
//!
//! - **Externally-tagged struct variants** (`Action`, most widget bodies' own
//!   sub-enums) — Rust's default enum representation already matches the plan:
//!   `Action::Set { path, value }` serializes as `{"set": {"path": …, "value": …}}`.
//! - **Untagged-by-shape** (`Source`) — the plan's `sources` entries carry no tag
//!   key at all (`{"verb": "…", "args": {…}}`, not `{"verb": {"verb": …}}`), so
//!   [`Source`] is `#[serde(untagged)]` and the variant is picked by which fields
//!   are present.
//!
//! [`Node`] is neither: its tag key (`column`, `field`, …) sits beside sibling keys
//! that are common to every kind (`id`, `label`, `help`, `when`) *and* sibling keys
//! that belong only to one kind (`bind`, `on_change`, `on_submit`, all `field`-only
//! in every example the plan gives). No combination of `#[serde(tag/flatten)]`
//! expresses "one required tag key plus a closed set of sibling keys, with an
//! unrecognized tag key kept rather than rejected" — so [`Node`] hand-writes
//! `Serialize`/`Deserialize` against `serde_json::Value` directly. See the crate
//! docs for why the "unrecognized tag key kept" half of that matters.

use std::collections::BTreeMap;

use serde::de::Error as DeError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

// `PaneQuery`/`ParamDecl` are spelled once, in the pane-query algebra. See the
// algebra crate, `impress-pane-query` (ADR-0033 D7): the kit never reaches
// impress-core.
pub use impress_pane_query::{PaneQuery, ParamDecl};

/// The one `surface` version this crate understands. [`validate::validate`]
/// (`crate::validate`) checks a spec's [`SurfaceSpec::surface`] field against it.
pub const SURFACE_VERSION: &str = "1.0";

// ─────────────────────────────────────────────────────────────────────────────
// SurfaceSpec
// ─────────────────────────────────────────────────────────────────────────────

/// A stored, declarative UI document (ADR-0033 D1). This is the whole of what an
/// agent authors and what `impress-surface-service` renders and reduces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
pub struct SurfaceSpec {
    /// Always "1.0" for a spec this build accepts; carried as a plain string
    /// (not an enum) so a newer or older version is a *validation* finding,
    /// not a parse failure — the same choice `PaneQuery` and the layout tree
    /// make about their own version fields.
    #[cfg_attr(feature = "schema", schemars(schema_with = "surface_version_schema"))]
    pub surface: String,
    pub name: String,
    #[serde(default)]
    pub params: Vec<ParamDecl>,
    /// State is not `Option`: a spec with no `state` key means "no fields", which
    /// is the empty object, not null. `default_state` gives the same value a
    /// hand-written `"state": {}` would.
    #[serde(default = "default_state")]
    pub state: Value,
    #[serde(default)]
    pub sources: BTreeMap<String, Source>,
    pub root: Node,
}

fn default_state() -> Value {
    Value::Object(Map::new())
}

// ─────────────────────────────────────────────────────────────────────────────
// Sources
// ─────────────────────────────────────────────────────────────────────────────

/// Where a value a node can reference comes from. The variant is chosen by
/// which of its three keys is present (`value`, `verb`, `query`) — never by an
/// explicit tag — so [`Source`] hand-writes `Deserialize` to name the problem
/// precisely ("a source is exactly one of …; found [verb, query]") where
/// `#[serde(untagged)]` could only say "did not match any variant".
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Source {
    Value {
        value: Value,
    },
    Verb {
        verb: String,
        #[serde(default)]
        args: Value,
    },
    Query {
        query: PaneQuery,
    },
}

impl Source {
    /// Parse one `sources` entry. Keys other than the variant's own are
    /// ignored here (a stored row stays readable); `validate_json` reports
    /// them as unknown fields.
    pub fn from_value(value: Value) -> Result<Source, String> {
        let Value::Object(mut obj) = value else {
            return Err(format!(
                "a source must be a JSON object, got {}",
                json_type(&value)
            ));
        };
        let present: Vec<&str> = ["value", "verb", "query"]
            .into_iter()
            .filter(|k| obj.contains_key(*k))
            .collect();
        match present.as_slice() {
            ["value"] => Ok(Source::Value {
                value: obj.remove("value").unwrap_or(Value::Null),
            }),
            ["verb"] => {
                let verb = match obj.remove("verb") {
                    Some(Value::String(s)) => s,
                    Some(other) => {
                        return Err(format!(
                            "`verb` must be a string, got {}",
                            json_type(&other)
                        ))
                    }
                    None => unreachable!("present"),
                };
                let args = obj.remove("args").unwrap_or(Value::Null);
                Ok(Source::Verb { verb, args })
            }
            ["query"] => {
                let raw = obj.remove("query").unwrap_or(Value::Null);
                serde_json::from_value::<PaneQuery>(raw)
                    .map(|query| Source::Query { query })
                    .map_err(|e| format!("`query`: {e}"))
            }
            [] => Err(format!(
                "a source is exactly one of {{\"value\"}}, {{\"verb\", \"args\"}} or \
                 {{\"query\"}}; found keys [{}]",
                obj.keys().cloned().collect::<Vec<_>>().join(", ")
            )),
            several => Err(format!(
                "a source is exactly one of {{\"value\"}}, {{\"verb\", \"args\"}} or \
                 {{\"query\"}}; found [{}]",
                several.join(", ")
            )),
        }
    }
}

impl<'de> Deserialize<'de> for Source {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        Source::from_value(value).map_err(DeError::custom)
    }
}

/// `object`, `array`, `string`, … — for a message that names what was found.
pub(crate) fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(feature = "schema")]
fn surface_version_schema(_gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
    schema_from_json(serde_json::json!({
        "type": "string",
        "const": SURFACE_VERSION,
        "description": "The surface vocabulary version; this build understands \"1.0\"."
    }))
}

/// A hand-written schema, as JSON. Every schema here is a literal the
/// `schema_is_machine_checkable` tests validate the shipped examples against.
#[cfg(feature = "schema")]
fn schema_from_json(value: Value) -> schemars::schema::Schema {
    serde_json::from_value(value).expect("a hand-written schema literal is a valid Schema")
}

#[cfg(feature = "schema")]
fn schema_json(schema: schemars::schema::Schema) -> Value {
    serde_json::to_value(schema).unwrap_or(Value::Null)
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for Source {
    fn schema_name() -> String {
        "Source".to_string()
    }

    fn json_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        // Exactly one of three shapes, chosen by which key is present (see
        // the type's docs) — a real `oneOf` a validator checks (review
        // AC-F8, RS-S24), not a description.
        // `PaneQuery`'s own schema admits any extra key; a misspelt one
        // (`kind` for `kinds`) silently widens the query, so the keys it has
        // are the only names allowed here.
        let query_keys: Vec<String> = schemars::schema_for!(PaneQuery)
            .schema
            .object
            .map(|o| o.properties.keys().cloned().collect())
            .unwrap_or_default();
        let query = serde_json::json!({
            "allOf": [schema_json(gen.subschema_for::<PaneQuery>())],
            "propertyNames": { "enum": query_keys }
        });
        schema_from_json(serde_json::json!({
            "description": "Where a value a node can reference comes from: exactly one of a \
                            fixed value, a verb call, or a pane query.",
            "oneOf": [
                {
                    "title": "value",
                    "type": "object",
                    "required": ["value"],
                    "properties": { "value": { "description": "Any JSON value." } },
                    "additionalProperties": false
                },
                {
                    "title": "verb",
                    "type": "object",
                    "required": ["verb"],
                    "properties": {
                        "verb": {
                            "type": "string",
                            "pattern": "^[a-z0-9-]+_[a-z0-9-]+$",
                            "description": "An #[impress_service] tool name, e.g. \
                                            surface-demo-service_series."
                        },
                        "args": {
                            "type": "object",
                            "description": "The verb's arguments; strings may hold {{…}} \
                                            references (state, param, source)."
                        }
                    },
                    "additionalProperties": false
                },
                {
                    "title": "query",
                    "type": "object",
                    "required": ["query"],
                    "properties": { "query": query },
                    "additionalProperties": false
                }
            ]
        }))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Actions
// ─────────────────────────────────────────────────────────────────────────────

/// What a handler (`on_click`, `on_select`, …) does. Externally tagged: Rust's
/// default enum representation already matches the plan's `{"set": {…}}` shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Write `value` (template-resolved) to `path` (a literal `state.…` path,
    /// never a template itself — see [`validate::validate`]'s `bind`/`set` check).
    Set { path: String, value: Value },
    /// Call a verb through the host's `#[impress_service]` inventory. `reduce`
    /// never runs this itself (it is pure); it resolves `args`'s templates and
    /// returns [`crate::reduce::Effect::Call`] for the runtime to execute.
    ///
    /// `each`, when present, is a fan-out declaration, not a loop the spec
    /// computes with (ADR-0033 D3 still holds: no expressions, conditionals or
    /// loops in a spec). It exists because a widget's event shape stays
    /// uniform — a `select` on a table or list is always an array of ids — while
    /// a verb like `triage-service_set-starred` keeps its own one-id signature;
    /// `each` is how a spec says "run this call once per selected id" without
    /// either side bending its shape. It is a literal path (the same form as
    /// `publish.ids`/`set.path`, e.g. `"state.selected"`), never a `{{…}}`
    /// template, resolved at reduce time; see [`crate::reduce::reduce`] for the
    /// fan-out and [`crate::validate::validate`] for the checks. Reference the
    /// current element as `{{item}}`/`{{item.field}}` inside `args`.
    Call {
        verb: String,
        #[serde(default)]
        args: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        into: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        each: Option<String>,
    },
    /// Publish a selection on the pane's channel. `ids` is a literal path (like
    /// `bind`), not a template; absent, it defaults to `event.value` at reduce
    /// time — the plan's own example (`{"on_select": [{"publish": {}}]}`) relies
    /// on exactly that default.
    ///
    /// The record kind the selection is published under is decided in this
    /// order: when the value at `ids` is an object `{"kind": K, "ids": [...]}`,
    /// kind `K`; otherwise the kind of the surface's FIRST declared `params`
    /// entry; otherwise `item`. A schema ref (`imbib/bibliography-entry`) is
    /// published as its pane-query kind (`publication`), so a pane bound to
    /// that kind sees it. The pane is the one showing this surface in the
    /// layout at the time of the action; with none, the action fails.
    Publish {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ids: Option<String>,
    },
    /// Emit a named event to the agent (a `impress/ui/surface-event` row).
    /// `each` fans this out the same way it does for [`Action::Call`] — see
    /// that variant's doc comment — so an action list that calls a verb once
    /// per selected id can also emit once per id, `{{item}}` bound the same
    /// way in `payload`.
    Emit {
        name: String,
        payload: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        each: Option<String>,
    },
    /// Open a query in a pane. `query` is kept as an opaque JSON value (not typed
    /// `PaneQuery` directly) because the vocabulary gives no worked example and a
    /// template-resolved id computed from state has nowhere to live inside
    /// `PaneQuery::scope`'s typed `ItemRef` otherwise; the runtime parses it as a
    /// `PaneQuery` once every template in it has been resolved, the same as
    /// `plot.spec` is an opaque `plot-spec@1.0.0` payload until imprint-core reads
    /// it.
    Open {
        query: Value,
        view_kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
    },
    /// Re-run a named source even though its resolved args have not changed.
    Refresh { source: String },
}

// ─────────────────────────────────────────────────────────────────────────────
// Events and `when`
// ─────────────────────────────────────────────────────────────────────────────

/// What the renderer reports happened. `widget` is a node id — author-given or
/// auto-derived, [`node_id`] computes both the same way `resolve` and `reduce` do.
///
/// An argument an agent sends (`surface_dispatch`'s `event`): an unknown key
/// is refused, never ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub widget: String,
    pub kind: EventKind,
    #[serde(default)]
    pub value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Change,
    Click,
    Select,
    Submit,
}

/// A node's visibility condition: truthy at `path` when `equals` is absent, or
/// exactly `equals` when it is present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
pub struct When {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<Value>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Widget bodies
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
pub struct Grid {
    pub columns: u32,
    #[serde(default)]
    pub items: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
pub struct Section {
    pub title: String,
    #[serde(default)]
    pub collapsed: bool,
    pub body: Box<Node>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
pub struct Tab {
    pub title: String,
    pub body: Node,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
pub struct Table {
    pub rows: Value,
    pub columns: Vec<String>,
    #[serde(default)]
    pub on_select: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
pub struct ListWidget {
    pub rows: Value,
    #[serde(default)]
    pub on_select: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
pub struct Plot {
    /// A `plot-spec@1.0.0` payload (ADR-0033 "Defaults"), opaque here — this
    /// crate never renders pixels.
    pub spec: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
pub struct Image {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<Value>,
}

/// The `field` widget's inner tag (`field: { text|number|slider|select|toggle|date:
/// options } `). The vocabulary gives one worked example of `options`
/// (slider's `{min, max, step}`) and no schema for the rest, so every variant
/// carries an opaque `Value` rather than a hand-invented, unverifiable struct per
/// kind — the smallest thing that round-trips the one example the plan gives and
/// leaves room for the others. [`validate::validate`] checks the one documented
/// invariant (`select`'s `options` must be non-empty) directly against the value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text(Value),
    Number(Value),
    Slider(Value),
    Select(Value),
    Toggle(Value),
    Date(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
pub struct Button {
    pub label: String,
    #[serde(default)]
    pub on_click: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
pub struct Status {
    pub level: String,
    pub message: Value,
}

// ─────────────────────────────────────────────────────────────────────────────
// Node
// ─────────────────────────────────────────────────────────────────────────────

/// One node-kind tag plus its payload. `log`/`kv` carry their `lines`/`pairs`
/// payload directly as `Value` (no wrapper struct) since each is a single opaque
/// field; every other widget with more than one field gets a named body struct
/// above so its fields have real names in the schema and in code.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
    Column(Vec<Node>),
    Row(Vec<Node>),
    Grid(Grid),
    Section(Section),
    Tabs(Vec<Tab>),
    /// Markdown, or a single `{{path}}` template — same representation either way
    /// (see `template.rs`); resolved with [`crate::template::Template`].
    Text(String),
    Table(Table),
    List(ListWidget),
    Plot(Plot),
    Image(Image),
    Field(FieldKind),
    Button(Button),
    Status(Status),
    Log(Value),
    Kv(Value),
    Divider,
    Spacer,
    /// A tag key this build does not recognize. Carries the raw key and its raw
    /// value so [`crate::resolve::resolve`] can degrade it to a placeholder node
    /// instead of the whole spec failing to load (ADR-0033 "Defaults").
    Unknown {
        kind: String,
        node: Value,
    },
    /// A node this build could not read: a KNOWN kind whose body does not
    /// parse (`{"table": {"rows": []}}` — no `columns`), a node with zero or
    /// several kind keys, a common key of the wrong type, or not an object at
    /// all. `kind` is the kind key when there was exactly one (else empty),
    /// `node` the node's whole original JSON, `error` what was wrong — kept,
    /// never replaced by "unrecognized kind" (review RS-S7). `validate`
    /// reports it as an error at the node's path; `resolve` draws a
    /// placeholder carrying `error` as its reason and `node` as its content.
    Invalid {
        kind: String,
        node: Value,
        error: String,
    },
}

/// The names of every closed node kind, in the order the vocabulary lists them.
/// Used by the manual `JsonSchema` impl below and by `schema_tests` (see
/// `tests/spec_round_trip.rs`) to assert the generated schema mentions each one.
pub const NODE_KIND_NAMES: &[&str] = &[
    "column", "row", "grid", "section", "tabs", "text", "table", "list", "plot", "image", "field",
    "button", "status", "log", "kv", "divider", "spacer",
];

/// A surface node: the common keys plus exactly one node-kind key (D3). See the
/// module docs for why `Serialize`/`Deserialize` are hand-written rather than
/// derived.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: Option<String>,
    pub label: Option<String>,
    pub help: Option<String>,
    pub when: Option<When>,
    /// `field`-only in every example the plan gives (`bind: state.path`), but not
    /// restricted to `field` at the type level — a node of any other kind simply
    /// never has one, and nothing reads it if present. Keeping it a plain sibling
    /// field (rather than nesting it inside [`NodeKind::Field`]'s payload) is what
    /// lets it sit beside the `field` tag key in the JSON exactly as the plan
    /// shows it, with the same flatten-free `Node` machinery every other kind
    /// uses.
    pub bind: Option<String>,
    /// Runs after a `change` event sets `bind`'s path (`field` only). The plan
    /// does not show `on_change` in its worked example, only describes its
    /// behaviour in prose ("a field change sets its bind path and then runs
    /// on_change if any") — modelled the same way as `bind`, for the same reason.
    pub on_change: Vec<Action>,
    /// Runs on a `submit` event, or is a no-op if empty — the plan gives no
    /// worked example of this either; modelled like `on_change`.
    pub on_submit: Vec<Action>,
    pub kind: NodeKind,
}

impl Node {
    /// A leaf node of the given kind with no common keys set.
    pub fn leaf(kind: NodeKind) -> Self {
        Node {
            id: None,
            label: None,
            help: None,
            when: None,
            bind: None,
            on_change: Vec::new(),
            on_submit: Vec::new(),
            kind,
        }
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    pub fn with_when(mut self, when: When) -> Self {
        self.when = Some(when);
        self
    }

    pub fn with_bind(mut self, bind: impl Into<String>) -> Self {
        self.bind = Some(bind.into());
        self
    }

    pub fn with_on_change(mut self, actions: Vec<Action>) -> Self {
        self.on_change = actions;
        self
    }

    pub fn with_on_submit(mut self, actions: Vec<Action>) -> Self {
        self.on_submit = actions;
        self
    }

    /// The node's direct children, for the tree walkers in `validate`, `resolve`
    /// and `reduce`. Every container kind lists its bodies; every widget kind is a
    /// leaf.
    pub fn children(&self) -> Vec<&Node> {
        match &self.kind {
            NodeKind::Column(items) | NodeKind::Row(items) => items.iter().collect(),
            NodeKind::Grid(g) => g.items.iter().collect(),
            NodeKind::Section(s) => vec![s.body.as_ref()],
            NodeKind::Tabs(tabs) => tabs.iter().map(|t| &t.body).collect(),
            _ => Vec::new(),
        }
    }

    /// The node-kind tag as it appears in JSON (`"column"`, `"field"`, … or the
    /// raw key of an [`NodeKind::Unknown`]).
    pub fn kind_name(&self) -> &str {
        match &self.kind {
            NodeKind::Column(_) => "column",
            NodeKind::Row(_) => "row",
            NodeKind::Grid(_) => "grid",
            NodeKind::Section(_) => "section",
            NodeKind::Tabs(_) => "tabs",
            NodeKind::Text(_) => "text",
            NodeKind::Table(_) => "table",
            NodeKind::List(_) => "list",
            NodeKind::Plot(_) => "plot",
            NodeKind::Image(_) => "image",
            NodeKind::Field(_) => "field",
            NodeKind::Button(_) => "button",
            NodeKind::Status(_) => "status",
            NodeKind::Log(_) => "log",
            NodeKind::Kv(_) => "kv",
            NodeKind::Divider => "divider",
            NodeKind::Spacer => "spacer",
            NodeKind::Unknown { kind, .. } | NodeKind::Invalid { kind, .. } => kind.as_str(),
        }
    }

    /// The node as JSON, exactly as an author would write it (an
    /// [`NodeKind::Invalid`] node is its original JSON, verbatim).
    pub fn to_value(&self) -> Value {
        if let NodeKind::Invalid { node, .. } = &self.kind {
            return node.clone();
        }
        let mut map = Map::new();
        if let Some(id) = &self.id {
            map.insert("id".to_string(), Value::String(id.clone()));
        }
        if let Some(label) = &self.label {
            map.insert("label".to_string(), Value::String(label.clone()));
        }
        if let Some(help) = &self.help {
            map.insert("help".to_string(), Value::String(help.clone()));
        }
        if let Some(when) = &self.when {
            map.insert(
                "when".to_string(),
                serde_json::to_value(when).unwrap_or(Value::Null),
            );
        }
        if let Some(bind) = &self.bind {
            map.insert("bind".to_string(), Value::String(bind.clone()));
        }
        if !self.on_change.is_empty() {
            map.insert(
                "on_change".to_string(),
                serde_json::to_value(&self.on_change).unwrap_or(Value::Null),
            );
        }
        if !self.on_submit.is_empty() {
            map.insert(
                "on_submit".to_string(),
                serde_json::to_value(&self.on_submit).unwrap_or(Value::Null),
            );
        }
        let (key, value) = self.kind.to_entry();
        map.insert(key, value);
        Value::Object(map)
    }

    /// Read a node. Never fails: whatever cannot be read becomes a
    /// [`NodeKind::Invalid`] node carrying the reason, so one bad node
    /// degrades alone (the rest of the surface still renders) and `validate`
    /// can say exactly what is wrong and where.
    pub fn from_value(value: Value) -> Node {
        let original = value.clone();
        let mut obj = match value {
            Value::Object(m) => m,
            other => {
                return Node::leaf(NodeKind::Invalid {
                    kind: String::new(),
                    error: format!(
                        "a surface node must be a JSON object, got {}",
                        json_type(&other)
                    ),
                    node: original,
                })
            }
        };
        let mut errors: Vec<String> = Vec::new();
        let id = noted(take_string(&mut obj, "id"), &mut errors);
        let label = noted(take_string(&mut obj, "label"), &mut errors);
        let help = noted(take_string(&mut obj, "help"), &mut errors);
        let bind = noted(take_string(&mut obj, "bind"), &mut errors);
        let when = noted(take_field::<When>(&mut obj, "when"), &mut errors);
        let on_change = noted(take_actions(&mut obj, "on_change"), &mut errors);
        let on_submit = noted(take_actions(&mut obj, "on_submit"), &mut errors);

        let keys: Vec<String> = obj.keys().cloned().collect();
        let mut kind = match keys.len() {
            1 => {
                let key = keys.into_iter().next().expect("len checked above");
                let raw = obj.remove(&key).expect("key just listed from this map");
                parse_kind(&key, raw, &original)
            }
            0 => NodeKind::Invalid {
                kind: String::new(),
                node: original.clone(),
                error: format!(
                    "a node needs exactly one kind key ({}); found none",
                    NODE_KIND_NAMES.join(", ")
                ),
            },
            _ => NodeKind::Invalid {
                kind: keys
                    .iter()
                    .find(|k| NODE_KIND_NAMES.contains(&k.as_str()))
                    .cloned()
                    .unwrap_or_default(),
                node: original.clone(),
                error: format!(
                    "a node needs exactly one kind key; found [{}] (the common keys are id, \
                     label, help, when, bind, on_change, on_submit)",
                    keys.join(", ")
                ),
            },
        };
        if !errors.is_empty() {
            kind = NodeKind::Invalid {
                kind: match &kind {
                    NodeKind::Invalid { kind, .. } | NodeKind::Unknown { kind, .. } => kind.clone(),
                    other => Node::leaf(other.clone()).kind_name().to_string(),
                },
                node: original,
                error: errors.join("; "),
            };
        }

        Node {
            id,
            label,
            help,
            when,
            bind,
            on_change,
            on_submit,
            kind,
        }
    }
}

impl NodeKind {
    fn to_entry(&self) -> (String, Value) {
        match self {
            NodeKind::Column(items) => (
                "column".to_string(),
                Value::Array(items.iter().map(Node::to_value).collect()),
            ),
            NodeKind::Row(items) => (
                "row".to_string(),
                Value::Array(items.iter().map(Node::to_value).collect()),
            ),
            NodeKind::Grid(g) => (
                "grid".to_string(),
                serde_json::to_value(g).unwrap_or(Value::Null),
            ),
            NodeKind::Section(s) => (
                "section".to_string(),
                serde_json::to_value(s).unwrap_or(Value::Null),
            ),
            NodeKind::Tabs(tabs) => (
                "tabs".to_string(),
                serde_json::to_value(tabs).unwrap_or(Value::Null),
            ),
            NodeKind::Text(t) => ("text".to_string(), Value::String(t.clone())),
            NodeKind::Table(t) => (
                "table".to_string(),
                serde_json::to_value(t).unwrap_or(Value::Null),
            ),
            NodeKind::List(l) => (
                "list".to_string(),
                serde_json::to_value(l).unwrap_or(Value::Null),
            ),
            NodeKind::Plot(p) => (
                "plot".to_string(),
                serde_json::to_value(p).unwrap_or(Value::Null),
            ),
            NodeKind::Image(i) => (
                "image".to_string(),
                serde_json::to_value(i).unwrap_or(Value::Null),
            ),
            NodeKind::Field(f) => (
                "field".to_string(),
                serde_json::to_value(f).unwrap_or(Value::Null),
            ),
            NodeKind::Button(b) => (
                "button".to_string(),
                serde_json::to_value(b).unwrap_or(Value::Null),
            ),
            NodeKind::Status(s) => (
                "status".to_string(),
                serde_json::to_value(s).unwrap_or(Value::Null),
            ),
            NodeKind::Log(v) => ("log".to_string(), v.clone()),
            NodeKind::Kv(v) => ("kv".to_string(), v.clone()),
            NodeKind::Divider => ("divider".to_string(), Value::Object(Map::new())),
            NodeKind::Spacer => ("spacer".to_string(), Value::Object(Map::new())),
            NodeKind::Unknown { kind, node } => (kind.clone(), node.clone()),
            // `Node::to_value` returns an invalid node's original JSON before
            // it gets here; this arm only keeps the match exhaustive.
            NodeKind::Invalid { kind, node, .. } => (kind.clone(), node.clone()),
        }
    }
}

fn parse_kind(key: &str, raw: Value, original: &Value) -> NodeKind {
    fn body<T: serde::de::DeserializeOwned>(
        key: &str,
        raw: Value,
        original: &Value,
        wrap: impl FnOnce(T) -> NodeKind,
    ) -> NodeKind {
        match serde_json::from_value::<T>(raw) {
            Ok(body) => wrap(body),
            Err(e) => NodeKind::Invalid {
                kind: key.to_string(),
                node: original.clone(),
                error: format!("`{key}`: {e}"),
            },
        }
    }
    match key {
        "column" => body(key, raw, original, NodeKind::Column),
        "row" => body(key, raw, original, NodeKind::Row),
        "grid" => body(key, raw, original, NodeKind::Grid),
        "section" => body(key, raw, original, NodeKind::Section),
        "tabs" => body(key, raw, original, NodeKind::Tabs),
        "text" => body(key, raw, original, NodeKind::Text),
        "table" => body(key, raw, original, NodeKind::Table),
        "list" => body(key, raw, original, NodeKind::List),
        "plot" => body(key, raw, original, NodeKind::Plot),
        "image" => body(key, raw, original, NodeKind::Image),
        "field" => body(key, raw, original, NodeKind::Field),
        "button" => body(key, raw, original, NodeKind::Button),
        "status" => body(key, raw, original, NodeKind::Status),
        "log" => NodeKind::Log(raw),
        "kv" => NodeKind::Kv(raw),
        "divider" => NodeKind::Divider,
        "spacer" => NodeKind::Spacer,
        _ => NodeKind::Unknown {
            kind: key.to_string(),
            node: raw,
        },
    }
}

/// The value, or its default with the error noted.
fn noted<T: Default>(result: Result<T, String>, errors: &mut Vec<String>) -> T {
    result.unwrap_or_else(|e| {
        errors.push(e);
        T::default()
    })
}

fn take_string(obj: &mut Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match obj.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(other) => Err(format!("`{key}` must be a string, got {other}")),
    }
}

fn take_field<T: serde::de::DeserializeOwned>(
    obj: &mut Map<String, Value>,
    key: &str,
) -> Result<Option<T>, String> {
    match obj.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v)
            .map(Some)
            .map_err(|e| format!("`{key}`: {e}")),
    }
}

fn take_actions(obj: &mut Map<String, Value>, key: &str) -> Result<Vec<Action>, String> {
    match obj.remove(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(v) => serde_json::from_value(v).map_err(|e| format!("`{key}`: {e}")),
    }
}

impl Serialize for Node {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.to_value().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        Ok(Node::from_value(value))
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for Node {
    fn schema_name() -> String {
        "Node".to_string()
    }

    fn json_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        // One branch per closed kind — the kind key required, its body's own
        // schema, the common keys, and nothing else — plus one branch for a
        // kind this build does not know (kept, and drawn as a placeholder:
        // ADR-0033's forward compatibility). A real `oneOf`, so a JSON Schema
        // validator rejects a node with two kind keys, a misspelt key or a
        // malformed body (review AC-F8, RS-S24).
        let node = schema_json(gen.subschema_for::<Node>());
        let action = schema_json(gen.subschema_for::<Action>());
        let actions = serde_json::json!({ "type": "array", "items": action });
        let common = serde_json::json!({
            "id": {
                "type": "string",
                "description": "The widget id events name. Give every interactive widget one; \
                                an unnamed node gets a positional id (n0.2.1) that changes \
                                when the spec does."
            },
            "label": { "type": "string" },
            "help": { "type": "string" },
            "when": schema_json(gen.subschema_for::<When>()),
            "bind": {
                "type": "string",
                "pattern": "^state(\\.[A-Za-z0-9_-]+)+$",
                "description": "field only: the state path the field edits."
            },
            "on_change": actions.clone(),
            "on_submit": actions,
        });
        let bodies: Vec<(&str, Value)> = vec![
            (
                "column",
                serde_json::json!({ "type": "array", "items": node }),
            ),
            ("row", serde_json::json!({ "type": "array", "items": node })),
            ("grid", schema_json(gen.subschema_for::<Grid>())),
            ("section", schema_json(gen.subschema_for::<Section>())),
            (
                "tabs",
                serde_json::json!({ "type": "array", "items": schema_json(gen.subschema_for::<Tab>()) }),
            ),
            (
                "text",
                serde_json::json!({ "type": "string", "description": "Markdown; {{…}} references are filled in." }),
            ),
            ("table", schema_json(gen.subschema_for::<Table>())),
            ("list", schema_json(gen.subschema_for::<ListWidget>())),
            ("plot", schema_json(gen.subschema_for::<Plot>())),
            ("image", schema_json(gen.subschema_for::<Image>())),
            ("field", schema_json(gen.subschema_for::<FieldKind>())),
            ("button", schema_json(gen.subschema_for::<Button>())),
            ("status", schema_json(gen.subschema_for::<Status>())),
            (
                "log",
                serde_json::json!({ "description": "Lines: an array, or a {{…}} reference to one." }),
            ),
            (
                "kv",
                serde_json::json!({ "description": "Pairs: an object, or a {{…}} reference to one." }),
            ),
            (
                "divider",
                serde_json::json!({ "type": "object", "maxProperties": 0 }),
            ),
            (
                "spacer",
                serde_json::json!({ "type": "object", "maxProperties": 0 }),
            ),
        ];
        let mut branches: Vec<Value> = bodies
            .into_iter()
            .map(|(kind, body)| {
                let mut properties = common.clone();
                properties[kind] = body;
                serde_json::json!({
                    "title": kind,
                    "type": "object",
                    "required": [kind],
                    "properties": properties,
                    "additionalProperties": false
                })
            })
            .collect();
        let known: Vec<Value> = NODE_KIND_NAMES
            .iter()
            .map(|k| serde_json::json!({ "required": [k] }))
            .collect();
        branches.push(serde_json::json!({
            "title": "a kind this build does not know",
            "description": "Kept, and rendered as a placeholder that carries the node — a \
                            spec written for a newer kit still renders the rest of itself. \
                            surface_validate reports it as a warning.",
            "type": "object",
            "not": { "anyOf": known }
        }));
        schema_from_json(serde_json::json!({
            "description": format!(
                "A surface node: exactly one node-kind key ({}) beside the common keys id, \
                 label, help, when, and (field only) bind, on_change, on_submit.",
                NODE_KIND_NAMES.join(", ")
            ),
            "oneOf": branches
        }))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Node ids
// ─────────────────────────────────────────────────────────────────────────────

/// The effective id of a node at `path` (its 0-based child index at every level,
/// root first): the author's [`Node::id`] if given, else `n` followed by the path
/// joined with `.` — `n0.2.1` is the root's third child's second child, matching
/// the plan's own example verbatim. [`crate::resolve::resolve`] and
/// [`crate::reduce::reduce`] both call this (never their own copy), so a widget id
/// a renderer reports in an [`Event`] always finds the same node `resolve` gave it.
pub fn node_id(node: &Node, path: &[usize]) -> String {
    match &node.id {
        Some(id) => id.clone(),
        None => {
            let mut s = String::from("n");
            for (i, p) in path.iter().enumerate() {
                if i > 0 {
                    s.push('.');
                }
                s.push_str(&p.to_string());
            }
            s
        }
    }
}

/// Every node in the tree, depth-first, paired with its effective id
/// ([`node_id`]). The root's path is `[0]` (id `n0` when unnamed).
pub fn walk_with_ids(root: &Node) -> Vec<(String, &Node)> {
    fn go<'a>(node: &'a Node, path: Vec<usize>, out: &mut Vec<(String, &'a Node)>) {
        out.push((node_id(node, &path), node));
        for (i, child) in node.children().into_iter().enumerate() {
            let mut child_path = path.clone();
            child_path.push(i);
            go(child, child_path, out);
        }
    }
    let mut out = Vec::new();
    go(root, vec![0], &mut out);
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Walking the tree by JSON pointer
// ─────────────────────────────────────────────────────────────────────────────
//
// The ONE definition of which keys of a node hold child nodes and which hold
// action lists (review RS-S21). `validate`, `surface_validate`'s verb check
// and the verb-argument check all walk through these, so a container or a
// handler added here is seen by every check at once — before, a kind added to
// `validate`'s walker but not the service's silently skipped the verb check.

/// Each direct child of `node`, with the JSON-pointer suffix that reaches it
/// from the node (`/column/2`, `/section/body`, `/tabs/0/body`).
pub fn child_pointers(node: &Node) -> Vec<(String, &Node)> {
    match &node.kind {
        NodeKind::Column(items) => items
            .iter()
            .enumerate()
            .map(|(i, n)| (format!("/column/{i}"), n))
            .collect(),
        NodeKind::Row(items) => items
            .iter()
            .enumerate()
            .map(|(i, n)| (format!("/row/{i}"), n))
            .collect(),
        NodeKind::Grid(g) => g
            .items
            .iter()
            .enumerate()
            .map(|(i, n)| (format!("/grid/items/{i}"), n))
            .collect(),
        NodeKind::Section(s) => vec![("/section/body".to_string(), s.body.as_ref())],
        NodeKind::Tabs(tabs) => tabs
            .iter()
            .enumerate()
            .map(|(i, t)| (format!("/tabs/{i}/body"), &t.body))
            .collect(),
        _ => Vec::new(),
    }
}

/// Each action list `node` declares, with its JSON-pointer suffix
/// (`/on_change`, `/table/on_select`, `/button/on_click`, …).
pub fn handler_pointers(node: &Node) -> Vec<(String, &[Action])> {
    let mut out: Vec<(String, &[Action])> = vec![
        ("/on_change".to_string(), node.on_change.as_slice()),
        ("/on_submit".to_string(), node.on_submit.as_slice()),
    ];
    match &node.kind {
        NodeKind::Table(t) => out.push(("/table/on_select".to_string(), t.on_select.as_slice())),
        NodeKind::List(l) => out.push(("/list/on_select".to_string(), l.on_select.as_slice())),
        NodeKind::Button(b) => out.push(("/button/on_click".to_string(), b.on_click.as_slice())),
        _ => {}
    }
    out
}

/// Every node, depth-first, with its JSON pointer from the spec root
/// (`/root`, `/root/column/1`, `/root/column/1/row/0`).
pub fn walk_with_pointers(root: &Node) -> Vec<(String, &Node)> {
    fn go<'a>(node: &'a Node, at: String, out: &mut Vec<(String, &'a Node)>) {
        let children = child_pointers(node);
        out.push((at.clone(), node));
        for (suffix, child) in children {
            go(child, format!("{at}{suffix}"), out);
        }
    }
    let mut out = Vec::new();
    go(root, "/root".to_string(), &mut out);
    out
}

/// Every action in the tree, with its JSON pointer
/// (`/root/column/4/button/on_click/0`).
pub fn walk_actions(root: &Node) -> Vec<(String, &Action)> {
    let mut out = Vec::new();
    for (at, node) in walk_with_pointers(root) {
        for (suffix, actions) in handler_pointers(node) {
            for (i, action) in actions.iter().enumerate() {
                out.push((format!("{at}{suffix}/{i}"), action));
            }
        }
    }
    out
}
