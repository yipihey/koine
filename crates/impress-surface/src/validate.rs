//! `validate(&SurfaceSpec) -> Vec<Problem>`: every reason a spec is not
//! well-formed, named by path, so an authoring agent (or `surface_validate`)
//! can fix each one without a screenshot. [`validate_json`] does the same for
//! a spec that is still JSON — a structural mistake (a missing `root`, a
//! source with two kinds, an unknown key) is a located problem too, never a
//! bare parse failure (review AC-F13). Never panics on a malformed spec.
//!
//! # Paths and severity
//!
//! `path` is a JSON pointer into the spec as written (`/sources/hist/args`,
//! `/root/column/1/field/select/options`) — `""` is the spec itself. A
//! problem is an `error` (the spec would not work: `surface_create` and
//! `surface_update` refuse it) or a `warning` (it works, but probably not as
//! meant — a node kind this build does not know, which renders as a
//! placeholder by design; a widget with no `id`; a `{{stat.x}}` kept as
//! text).
//!
//! # Checks
//!
//! 1. `surface` is exactly [`SURFACE_VERSION`]; `state` is an object; param
//!    names are non-empty and unique.
//! 2. Every `bind`, `set.path`, `publish.ids` and `call.into` is a
//!    `state.…` path ([`crate::state_path`]).
//! 3. Every template reference's root is `state`/`param`/`source`/`event`
//!    (and `item` inside an action with `each`); a `state.…` reference names
//!    a declared state key, a `source.…` one a declared source, a `param.…`
//!    one a declared param. A dotted `{{…}}` that is not a reference is a
//!    warning (it renders as text).
//! 4. Source `args` form no dependency cycle; verb names match
//!    `^[a-z0-9-]+_[a-z0-9-]+$`.
//! 5. Node ids (given or auto-derived) are unique; an interactive widget
//!    (`field`, `button`, `table`, `list`) with no `id` is a warning.
//! 6. Per kind: a `select` field has options, `grid.columns >= 1`, a
//!    `table.id_field` is a non-empty literal field name, a `tree`, `table`
//!    or `list` row menu has non-empty, unique labels and a literal
//!    `count_field`, a node
//!    that could not be read is an error with the reader's own message, an
//!    unknown kind a warning.
//! 7. `each` is a literal path with a data root; `open` names a view kind and
//!    a query that parses when it holds no template; `refresh` names a
//!    declared source.
//!
//! What this function does **not** check: whether a verb exists or accepts
//! its arguments, whether a view kind is registered — those need the linked
//! inventory; `impress-surface-service`'s `surface_validate` adds them.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use crate::spec::{
    walk_actions, walk_with_ids, walk_with_pointers, Action, FieldKind, Node, NodeKind, ParamDecl,
    Source, SurfaceSpec, SURFACE_VERSION,
};
use crate::state_path;
use crate::template::{literal_path_like, source_refs_in, Template};

/// How bad a [`Problem`] is. See the module docs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// The spec would not work as written; create and update refuse it.
    #[default]
    Error,
    /// It works, but probably not as meant.
    Warning,
}

/// One thing wrong with a spec: `path` is a JSON pointer (see the module
/// docs), `message` says what and how to fix it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Problem {
    pub path: String,
    pub message: String,
    #[serde(default)]
    pub severity: Severity,
}

impl Problem {
    pub fn error(path: impl Into<String>, message: impl Into<String>) -> Self {
        Problem {
            path: path.into(),
            message: message.into(),
            severity: Severity::Error,
        }
    }

    pub fn warning(path: impl Into<String>, message: impl Into<String>) -> Self {
        Problem {
            path: path.into(),
            message: message.into(),
            severity: Severity::Warning,
        }
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// What a template reference may name, for one spec.
struct Scope<'a> {
    state_keys: BTreeSet<&'a str>,
    sources: BTreeSet<&'a str>,
    params: BTreeSet<&'a str>,
}

/// See the module docs for the full list of checks.
pub fn validate(spec: &SurfaceSpec) -> Vec<Problem> {
    let mut problems = Vec::new();

    if spec.surface != SURFACE_VERSION {
        problems.push(Problem::error(
            "/surface",
            format!(
                "unsupported surface version '{}', this build understands '{SURFACE_VERSION}'",
                spec.surface
            ),
        ));
    }
    if !spec.state.is_object() {
        problems.push(Problem::error("/state", "`state` must be a JSON object"));
    }
    let mut seen_params = BTreeSet::new();
    for (i, param) in spec.params.iter().enumerate() {
        if param.name.trim().is_empty() {
            problems.push(Problem::error(
                format!("/params/{i}/name"),
                "a param needs a name",
            ));
        } else if !seen_params.insert(param.name.as_str()) {
            problems.push(Problem::error(
                format!("/params/{i}/name"),
                format!("param '{}' is declared twice", param.name),
            ));
        }
    }

    let scope = Scope {
        state_keys: spec
            .state
            .as_object()
            .map(|m| m.keys().map(String::as_str).collect())
            .unwrap_or_default(),
        sources: spec.sources.keys().map(String::as_str).collect(),
        params: spec.params.iter().map(|p| p.name.as_str()).collect(),
    };

    // ── sources: verb-name shape, references, dependency cycles ────────────
    let mut deps: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (name, source) in &spec.sources {
        match source {
            Source::Verb { verb, args } => {
                check_verb_name(verb, &format!("/sources/{name}/verb"), &mut problems);
                deps.insert(name.as_str(), source_refs_in(args));
                check_template_refs(
                    args,
                    &format!("/sources/{name}/args"),
                    &scope,
                    false,
                    &mut problems,
                );
            }
            Source::Value { .. } | Source::Query { .. } => {
                // A query's parameters are bound from the surface's params
                // at run time (`$param` in the query), not by templates.
                deps.insert(name.as_str(), Vec::new());
            }
        }
    }
    for name in find_cycle(&deps) {
        problems.push(Problem::error(
            format!("/sources/{name}"),
            "participates in a source dependency cycle",
        ));
    }

    // ── ids ─────────────────────────────────────────────────────────────────
    let mut ids: BTreeMap<String, usize> = BTreeMap::new();
    for (id, _) in walk_with_ids(&spec.root) {
        *ids.entry(id).or_insert(0) += 1;
    }
    for (id, count) in &ids {
        if *count > 1 {
            problems.push(Problem::error(
                "/root",
                format!("node id '{id}' is used by {count} nodes"),
            ));
        }
    }

    // ── the tree ────────────────────────────────────────────────────────────
    for (at, node) in walk_with_pointers(&spec.root) {
        check_node(node, &at, &scope, &mut problems);
    }
    for (at, action) in walk_actions(&spec.root) {
        check_action(action, &at, &scope, &mut problems);
    }

    problems
}

/// [`validate`] for a spec that is still JSON: structural problems (not an
/// object, a missing or mistyped field, a source that is not exactly one
/// kind, an unknown key anywhere) come back located, like every other
/// problem. Returns the parsed spec when it parsed.
pub fn validate_json(raw: &Value) -> (Option<SurfaceSpec>, Vec<Problem>) {
    let mut problems = Vec::new();
    let Some(obj) = raw.as_object() else {
        problems.push(Problem::error(
            "",
            format!(
                "a surface spec must be a JSON object, got {}",
                crate::spec::json_type(raw)
            ),
        ));
        return (None, problems);
    };
    const FIELDS: &[&str] = &["surface", "name", "params", "state", "sources", "root"];
    for key in obj.keys() {
        if !FIELDS.contains(&key.as_str()) {
            problems.push(Problem::error(
                format!("/{}", escape(key)),
                format!("unknown field `{key}`; a spec has {}", FIELDS.join(", ")),
            ));
        }
    }
    let string_field = |key: &str, problems: &mut Vec<Problem>| -> Option<String> {
        match obj.get(key) {
            Some(Value::String(s)) => Some(s.clone()),
            Some(other) => {
                problems.push(Problem::error(
                    format!("/{key}"),
                    format!(
                        "`{key}` must be a string, got {}",
                        crate::spec::json_type(other)
                    ),
                ));
                None
            }
            None => {
                problems.push(Problem::error("", format!("missing field `{key}`")));
                None
            }
        }
    };
    let surface = string_field("surface", &mut problems);
    let name = string_field("name", &mut problems);

    let mut params = Vec::new();
    match obj.get("params") {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) => {
            for (i, item) in items.iter().enumerate() {
                match serde_json::from_value::<ParamDecl>(item.clone()) {
                    Ok(p) => params.push(p),
                    Err(e) => problems.push(Problem::error(format!("/params/{i}"), e.to_string())),
                }
            }
        }
        Some(other) => problems.push(Problem::error(
            "/params",
            format!(
                "`params` must be an array, got {}",
                crate::spec::json_type(other)
            ),
        )),
    }

    let mut sources = BTreeMap::new();
    match obj.get("sources") {
        None | Some(Value::Null) => {}
        Some(Value::Object(map)) => {
            for (key, value) in map {
                match Source::from_value(value.clone()) {
                    Ok(source) => {
                        sources.insert(key.clone(), source);
                    }
                    Err(e) => problems.push(Problem::error(format!("/sources/{}", escape(key)), e)),
                }
            }
        }
        Some(other) => problems.push(Problem::error(
            "/sources",
            format!(
                "`sources` must be an object, got {}",
                crate::spec::json_type(other)
            ),
        )),
    }

    let root = match obj.get("root") {
        Some(value) => Some(Node::from_value(value.clone())),
        None => {
            problems.push(Problem::error("", "missing field `root`"));
            None
        }
    };

    let (Some(surface), Some(name), Some(root)) = (surface, name, root) else {
        return (None, problems);
    };
    if problems.iter().any(Problem::is_error) {
        return (None, problems);
    }
    let spec = SurfaceSpec {
        surface,
        name,
        params,
        state: obj
            .get("state")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new())),
        sources,
        root,
        extra: Default::default(),
    };
    problems.extend(validate(&spec));
    // What this build read, written back out: a key the author wrote that is
    // not in it is a key nothing reads — a typo, or a field of another kind.
    if let Ok(canonical) = serde_json::to_value(&spec) {
        unknown_fields(raw, &canonical, "", &mut problems);
    }
    (Some(spec), problems)
}

/// `~` and `/` escaped for a JSON pointer segment.
fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// Every key of `raw` that `canonical` (the same document as this build read
/// it) does not have. A key whose value is `null`, `[]` or `{}` is not
/// reported — an empty value changes nothing, and several optional fields
/// are left out of the canonical form when empty.
fn unknown_fields(raw: &Value, canonical: &Value, at: &str, problems: &mut Vec<Problem>) {
    match (raw, canonical) {
        (Value::Object(r), Value::Object(c)) => {
            for (key, value) in r {
                let here = format!("{at}/{}", escape(key));
                match c.get(key) {
                    Some(cv) => unknown_fields(value, cv, &here, problems),
                    None if is_empty(value) => {}
                    None => {
                        let known: Vec<&str> = c.keys().map(String::as_str).collect();
                        problems.push(Problem::error(
                            here,
                            format!(
                                "unknown field `{key}`; nothing reads it (here: {})",
                                known.join(", ")
                            ),
                        ));
                    }
                }
            }
        }
        (Value::Array(r), Value::Array(c)) => {
            for (i, (rv, cv)) in r.iter().zip(c.iter()).enumerate() {
                unknown_fields(rv, cv, &format!("{at}/{i}"), problems);
            }
        }
        _ => {}
    }
}

fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

fn check_verb_name(verb: &str, path: &str, problems: &mut Vec<Problem>) {
    let parts: Vec<&str> = verb.split('_').collect();
    let ok = parts.len() == 2
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        });
    if !ok {
        problems.push(Problem::error(
            path,
            format!("verb name '{verb}' does not match ^[a-z0-9-]+_[a-z0-9-]+$"),
        ));
    }
}

fn check_state_path(path_value: &str, at: &str, problems: &mut Vec<Problem>) {
    if let Err(e) = state_path::segments(path_value) {
        problems.push(Problem::error(at, e.to_string()));
    }
}

/// Scan every string inside `value` for template references and check their
/// roots. `item_allowed` is true only inside a `call`/`emit` that carries
/// `each`.
fn check_template_refs(
    value: &Value,
    at: &str,
    scope: &Scope,
    item_allowed: bool,
    problems: &mut Vec<Problem>,
) {
    match value {
        Value::String(s) => {
            for path in reference_paths(s) {
                check_reference_path(&path, at, scope, item_allowed, problems);
            }
            for text in literal_path_like(s) {
                problems.push(Problem::warning(
                    at,
                    format!(
                        "'{{{{{text}}}}}' is shown as text: '{}' is not a template root (state, \
                         param, source, event, item)",
                        text.split('.').next().unwrap_or_default()
                    ),
                ));
            }
        }
        Value::Array(items) => {
            for item in items {
                check_template_refs(item, at, scope, item_allowed, problems);
            }
        }
        Value::Object(map) => {
            for v in map.values() {
                check_template_refs(v, at, scope, item_allowed, problems);
            }
        }
        _ => {}
    }
}

fn has_template(value: &Value) -> bool {
    match value {
        Value::String(s) => !reference_paths(s).is_empty(),
        Value::Array(items) => items.iter().any(has_template),
        Value::Object(map) => map.values().any(has_template),
        _ => false,
    }
}

fn reference_paths(s: &str) -> Vec<Vec<String>> {
    match Template::parse(s) {
        Template::Literal(_) => Vec::new(),
        Template::Single(p) => vec![p],
        Template::Mixed(segments) => segments
            .into_iter()
            .filter_map(|seg| match seg {
                crate::template::Segment::Ref(p) => Some(p),
                crate::template::Segment::Literal(_) => None,
            })
            .collect(),
    }
}

fn check_reference_path(
    path: &[String],
    at: &str,
    scope: &Scope,
    item_allowed: bool,
    problems: &mut Vec<Problem>,
) {
    let Some(root) = path.first() else {
        return;
    };
    let second = path.get(1).map(String::as_str);
    match root.as_str() {
        "state" => {
            if let Some(key) = second {
                if !scope.state_keys.contains(key) {
                    problems.push(Problem::error(
                        at,
                        format!(
                            "'{}' references state key '{key}', which `state` does not declare",
                            path.join(".")
                        ),
                    ));
                }
            }
        }
        "source" => {
            if let Some(name) = second {
                if !scope.sources.contains(name) {
                    problems.push(Problem::error(
                        at,
                        format!("'{}' references undeclared source '{name}'", path.join(".")),
                    ));
                }
            }
        }
        "param" => match second {
            Some(name) if !scope.params.contains(name) => problems.push(Problem::error(
                at,
                format!(
                    "'{}' references param '{name}', which `params` does not declare",
                    path.join(".")
                ),
            )),
            None => problems.push(Problem::error(
                at,
                "'param' alone names no parameter; write param.<name>",
            )),
            _ => {}
        },
        "event" => {}
        "item" if item_allowed => {}
        other => problems.push(Problem::error(
            at,
            format!(
                "'{}' has root '{other}'{}",
                path.join("."),
                if other == "item" {
                    ", which is only bound inside an action with `each`"
                } else {
                    ", which is not a template root"
                }
            ),
        )),
    }
}

/// A literal path with a data root — `each` and `when.path`.
fn check_literal_path(path_value: &str, at: &str, scope: &Scope, problems: &mut Vec<Problem>) {
    let segments: Vec<String> = path_value.split('.').map(str::to_string).collect();
    if segments.iter().any(|s| s.is_empty()) {
        problems.push(Problem::error(
            at,
            format!("'{path_value}' is not a dotted path"),
        ));
        return;
    }
    check_reference_path(&segments, at, scope, false, problems);
}

fn check_node(node: &Node, at: &str, scope: &Scope, problems: &mut Vec<Problem>) {
    if let Some(bind) = &node.bind {
        check_state_path(bind, &format!("{at}/bind"), problems);
    }
    if let Some(when) = &node.when {
        check_literal_path(&when.path, &format!("{at}/when/path"), scope, problems);
    }
    for (key, text) in [("label", &node.label), ("help", &node.help)] {
        if let Some(text) = text {
            check_template_refs(
                &Value::String(text.clone()),
                &format!("{at}/{key}"),
                scope,
                false,
                problems,
            );
        }
    }
    if node.id.is_none()
        && matches!(
            node.kind,
            NodeKind::Field(_)
                | NodeKind::Button(_)
                | NodeKind::Table(_)
                | NodeKind::List(_)
                | NodeKind::Tree(_)
        )
    {
        problems.push(Problem::warning(
            at,
            format!(
                "this {} has no `id`: its events name a positional id that changes when the \
                 spec does, so give it one",
                node.kind_name()
            ),
        ));
    }
    let mut refs = |value: &Value, key: &str| {
        check_template_refs(value, &format!("{at}/{key}"), scope, false, problems)
    };
    match &node.kind {
        NodeKind::Column(_) | NodeKind::Row(_) | NodeKind::Divider | NodeKind::Spacer => {}
        NodeKind::Grid(g) => {
            if g.columns < 1 {
                problems.push(Problem::error(
                    format!("{at}/grid/columns"),
                    "grid.columns must be >= 1",
                ));
            }
        }
        NodeKind::Section(s) => refs(&Value::String(s.title.clone()), "section/title"),
        NodeKind::Tabs(tabs) => {
            for (i, tab) in tabs.iter().enumerate() {
                refs(
                    &Value::String(tab.title.clone()),
                    &format!("tabs/{i}/title"),
                );
            }
        }
        NodeKind::Text(s) => refs(&Value::String(s.clone()), "text"),
        NodeKind::Table(t) => {
            refs(&t.rows, "table/rows");
            // `id_field` is a literal property name (plan-koine-followups
            // F-D2): the renderer reads it off each row, so a template here
            // would be a field called `{{…}}`.
            if let Some(field) = &t.id_field {
                if field.is_empty() || field.contains("{{") {
                    problems.push(Problem::error(
                        format!("{at}/table/id_field"),
                        "table.id_field must be a non-empty literal field name of the rows, \
                         not a template (for example \"bibcode\")",
                    ));
                }
            }
            check_row_menu("table", &t.actions, &t.count_field, at, problems);
        }
        NodeKind::List(l) => {
            refs(&l.rows, "list/rows");
            check_row_menu("list", &l.actions, &l.count_field, at, problems);
        }
        NodeKind::Tree(t) => {
            refs(&t.rows, "tree/rows");
            check_row_menu("tree", &t.actions, &t.count_field, at, problems);
            if let Some(bind) = &t.bind {
                check_state_path(bind, &format!("{at}/tree/bind"), problems);
            }
            // The three field names are literal property names, read off
            // each row by the resolver: a template here would be a field
            // called `{{…}}`.
            for (key, field) in [
                ("id_field", &t.id_field),
                ("parent_field", &t.parent_field),
                ("label_field", &t.label_field),
            ] {
                if let Some(field) = field {
                    if field.is_empty() || field.contains("{{") {
                        problems.push(Problem::error(
                            format!("{at}/tree/{key}"),
                            format!(
                                "tree.{key} must be a non-empty literal field name of the \
                                 rows, not a template"
                            ),
                        ));
                    }
                }
            }
        }
        NodeKind::Graph(g) => {
            refs(&g.nodes, "graph/nodes");
            refs(&g.edges, "graph/edges");
            // Literal property names read off each row by the resolver, as
            // the tree's are.
            for (key, field) in [
                ("id_field", &g.id_field),
                ("label_field", &g.label_field),
                ("source_field", &g.source_field),
                ("target_field", &g.target_field),
            ] {
                if let Some(field) = field {
                    if field.is_empty() || field.contains("{{") {
                        problems.push(Problem::error(
                            format!("{at}/graph/{key}"),
                            format!(
                                "graph.{key} must be a non-empty literal field name of the \
                                 rows, not a template"
                            ),
                        ));
                    }
                }
            }
        }
        NodeKind::Plot(p) => check_plot(p, at, scope, problems),
        NodeKind::Code(c) => {
            refs(&c.text, "code/text");
            if !c.text.is_string() {
                problems.push(Problem::error(
                    format!("{at}/code/text"),
                    "code.text must be a string (a {{…}} template is a string)",
                ));
            }
        }
        NodeKind::Image(img) => {
            if let Some(v) = &img.blob {
                refs(v, "image/blob");
            }
            if let Some(v) = &img.url {
                refs(v, "image/url");
            }
        }
        NodeKind::Field(f) => {
            if node.bind.is_none() {
                problems.push(Problem::error(
                    format!("{at}/bind"),
                    "a field needs `bind` (the state path it edits)",
                ));
            }
            if let FieldKind::Select(v) = f {
                // A literal list, or a template the page fills from a source
                // (a library picker's choices are a verb's answer); `resolve`
                // templates a field's options like any other value.
                let options = v.get("options");
                let non_empty = options
                    .and_then(Value::as_array)
                    .is_some_and(|a| !a.is_empty());
                let templated = options.and_then(Value::as_str).is_some_and(|t| {
                    t.trim_start().starts_with("{{") && t.trim_end().ends_with("}}")
                });
                if !non_empty && !templated {
                    problems.push(Problem::error(
                        format!("{at}/field/select/options"),
                        "a select field's options must be a non-empty array, or a `{{…}}` template naming one",
                    ));
                }
            }
        }
        NodeKind::Button(b) => refs(&Value::String(b.label.clone()), "button/label"),
        NodeKind::Status(s) => refs(&s.message, "status/message"),
        NodeKind::Log(v) => refs(v, "log"),
        NodeKind::Kv(v) => refs(v, "kv"),
        NodeKind::Unknown { kind, .. } => problems.push(Problem::warning(
            at,
            format!("this build has no '{kind}' widget: it renders as a placeholder"),
        )),
        NodeKind::Invalid { error, .. } => problems.push(Problem::error(at, error.clone())),
    }
}

/// `actions` and `count_field` of a `tree`, `table` or `list`
/// (plan-koine-outline O-D5, O-D6): a label is non-empty and unique within
/// the node, a `key` is one character that is not a navigation key or a
/// digit and is unique within the node, and `count_field` is a literal
/// property name. The handlers
/// inside are checked with every other action list ([`handler_pointers`]).
fn check_row_menu(
    kind: &str,
    actions: &[crate::spec::MenuAction],
    count_field: &Option<String>,
    at: &str,
    problems: &mut Vec<Problem>,
) {
    let mut seen = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for (i, action) in actions.iter().enumerate() {
        if let Some(key) = &action.key {
            let path = format!("{at}/{kind}/actions/{i}/key");
            let mut chars = key.chars();
            let one = match (chars.next(), chars.next()) {
                (Some(c), None) => Some(c),
                _ => None,
            };
            match one {
                None => problems.push(Problem::error(
                    path,
                    format!("{kind}.actions[{i}].key must be one character, not '{key}'"),
                )),
                Some(c) if c.is_whitespace() || c.is_ascii_digit() || "jkhl".contains(c) => {
                    problems.push(Problem::error(
                        path,
                        format!(
                            "{kind}.actions[{i}].key '{key}' is taken: j, k, h, l and Space move \
                             the cursor, and digits pick an open menu's entry"
                        ),
                    ))
                }
                Some(_) if !keys.insert(key.as_str()) => problems.push(Problem::error(
                    path,
                    format!("{kind}.actions key '{key}' is used twice"),
                )),
                Some(_) => {}
            }
        }
        let path = format!("{at}/{kind}/actions/{i}/label");
        if action.label.trim().is_empty() {
            problems.push(Problem::error(
                path,
                format!("{kind}.actions[{i}].label must not be empty"),
            ));
        } else if !seen.insert(action.label.as_str()) {
            problems.push(Problem::error(
                path,
                format!(
                    "{kind}.actions label '{}' is used twice: a `menu` event names its entry \
                     by label",
                    action.label
                ),
            ));
        }
    }
    if let Some(field) = count_field {
        if field.trim().is_empty() || field.contains("{{") {
            problems.push(Problem::error(
                format!("{at}/{kind}/count_field"),
                format!(
                    "{kind}.count_field must be a non-empty literal field name of the rows, \
                     not a template"
                ),
            ));
        }
    }
}

/// A plot is exactly one of `spec`, or the bound form `rows` + `x` + `y`
/// (plan-koine-live L-D8). `x` and `y` are literal field names of the rows.
fn check_plot(plot: &crate::spec::Plot, at: &str, scope: &Scope, problems: &mut Vec<Problem>) {
    let bound = [
        ("rows", plot.rows.is_some()),
        ("x", plot.x.is_some()),
        ("y", plot.y.is_some()),
    ];
    let given: Vec<&str> = bound.iter().filter(|(_, g)| *g).map(|(k, _)| *k).collect();
    match (&plot.spec, given.len()) {
        (Some(spec), 0) => {
            check_template_refs(spec, &format!("{at}/plot/spec"), scope, false, problems)
        }
        (Some(_), _) => problems.push(Problem::error(
            format!("{at}/plot"),
            format!(
                "a plot is either `spec` or `rows`, `x` and `y`, not both (found spec and {})",
                given.join(", ")
            ),
        )),
        (None, 3) => {
            if let Some(rows) = &plot.rows {
                check_template_refs(rows, &format!("{at}/plot/rows"), scope, false, problems);
            }
            let literal = |name: &str| !name.is_empty() && !name.contains("{{");
            if !plot.x.as_deref().is_some_and(literal) {
                problems.push(Problem::error(
                    format!("{at}/plot/x"),
                    "plot.x must be a non-empty literal field name of the rows",
                ));
            }
            let ys_ok = match &plot.y {
                Some(Value::String(y)) => literal(y),
                Some(Value::Array(ys)) => {
                    !ys.is_empty() && ys.iter().all(|y| y.as_str().is_some_and(literal))
                }
                _ => false,
            };
            if !ys_ok {
                problems.push(Problem::error(
                    format!("{at}/plot/y"),
                    "plot.y must be a literal field name, or a non-empty array of them",
                ));
            }
        }
        (None, 0) => problems.push(Problem::error(
            format!("{at}/plot"),
            "a plot needs `spec`, or `rows`, `x` and `y`",
        )),
        (None, _) => {
            let missing: Vec<&str> = bound.iter().filter(|(_, g)| !*g).map(|(k, _)| *k).collect();
            problems.push(Problem::error(
                format!("{at}/plot"),
                format!(
                    "a bound plot needs `rows`, `x` and `y`; missing {}",
                    missing.join(", ")
                ),
            ));
        }
    }
}

/// `adverbs` is an object of adverb name to value. The names themselves are
/// the pipeline's to judge (`impress-service-core::adverb`); only the shape is
/// checked here, so a typo in a key is caught as an identifier, not as a word.
fn check_adverbs(
    adverbs: &Value,
    at: &str,
    scope: &Scope,
    has_each: bool,
    problems: &mut Vec<Problem>,
) {
    let Some(map) = adverbs.as_object() else {
        problems.push(Problem::error(
            at,
            "`adverbs` is an object of adverb name to value",
        ));
        return;
    };
    for key in map.keys() {
        if !is_snake_ident(key) {
            problems.push(Problem::error(
                at,
                format!("adverb name '{key}' is not a snake_case identifier ([a-z][a-z0-9_]*)"),
            ));
        }
    }
    check_template_refs(adverbs, at, scope, has_each, problems);
}

fn is_snake_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn check_action(action: &Action, at: &str, scope: &Scope, problems: &mut Vec<Problem>) {
    match action {
        Action::Set { path, value } => {
            check_state_path(path, &format!("{at}/set/path"), problems);
            check_template_refs(value, &format!("{at}/set/value"), scope, false, problems);
        }
        Action::Append { path, value } => {
            check_state_path(path, &format!("{at}/append/path"), problems);
            check_template_refs(value, &format!("{at}/append/value"), scope, false, problems);
        }
        Action::Call {
            verb,
            args,
            into,
            each,
            adverbs,
        } => {
            check_verb_name(verb, &format!("{at}/call/verb"), problems);
            if let Some(each_path) = each {
                check_literal_path(each_path, &format!("{at}/call/each"), scope, problems);
            }
            if let Some(into) = into {
                check_state_path(into, &format!("{at}/call/into"), problems);
            }
            check_template_refs(
                args,
                &format!("{at}/call/args"),
                scope,
                each.is_some(),
                problems,
            );
            if let Some(adverbs) = adverbs {
                check_adverbs(
                    adverbs,
                    &format!("{at}/call/adverbs"),
                    scope,
                    each.is_some(),
                    problems,
                );
            }
        }
        Action::Publish { ids } => {
            if let Some(ids) = ids {
                check_state_path(ids, &format!("{at}/publish/ids"), problems);
            }
        }
        Action::Emit {
            name,
            payload,
            each,
        } => {
            if name.trim().is_empty() {
                problems.push(Problem::error(
                    format!("{at}/emit/name"),
                    "an event needs a name",
                ));
            }
            if let Some(each_path) = each {
                check_literal_path(each_path, &format!("{at}/emit/each"), scope, problems);
            }
            check_template_refs(
                payload,
                &format!("{at}/emit/payload"),
                scope,
                each.is_some(),
                problems,
            );
        }
        Action::Open {
            query,
            view_kind,
            target,
        } => {
            if view_kind.trim().is_empty() {
                problems.push(Problem::error(
                    format!("{at}/open/view_kind"),
                    "`view_kind` is empty",
                ));
            }
            if target.as_deref().is_some_and(|t| t.trim().is_empty()) {
                problems.push(Problem::error(
                    format!("{at}/open/target"),
                    "`target` is a role name; leave it out to open a new pane",
                ));
            }
            check_template_refs(query, &format!("{at}/open/query"), scope, false, problems);
            if !has_template(query) {
                if let Err(e) = serde_json::from_value::<crate::spec::PaneQuery>(query.clone()) {
                    problems.push(Problem::error(
                        format!("{at}/open/query"),
                        format!("not a pane query: {e}"),
                    ));
                }
            }
        }
        Action::Refresh { source } => {
            if !scope.sources.contains(source.as_str()) {
                problems.push(Problem::error(
                    format!("{at}/refresh/source"),
                    format!("refreshes undeclared source '{source}'"),
                ));
            }
        }
    }
}

/// Every source name that participates in a dependency cycle (empty if the
/// graph is a DAG). A three-colour DFS: white (unvisited), grey (on the
/// current path), black (finished) — a grey node reached again is the
/// back-edge that proves a cycle, and everything from it to the top of the
/// stack is a member.
fn find_cycle(deps: &BTreeMap<&str, Vec<String>>) -> Vec<String> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        White,
        Grey,
        Black,
    }
    let mut mark: BTreeMap<&str, Mark> = deps.keys().map(|k| (*k, Mark::White)).collect();
    let mut cycle_members = BTreeSet::new();

    fn visit<'a>(
        node: &'a str,
        deps: &'a BTreeMap<&'a str, Vec<String>>,
        mark: &mut BTreeMap<&'a str, Mark>,
        stack: &mut Vec<&'a str>,
        cycle_members: &mut BTreeSet<String>,
    ) {
        match mark.get(node).copied().unwrap_or(Mark::Black) {
            Mark::Black => return,
            Mark::Grey => {
                // Back-edge: everything on the stack from `node` onward is in
                // the cycle.
                if let Some(pos) = stack.iter().position(|n| *n == node) {
                    for n in &stack[pos..] {
                        cycle_members.insert((*n).to_string());
                    }
                }
                return;
            }
            Mark::White => {}
        }
        mark.insert(node, Mark::Grey);
        stack.push(node);
        if let Some(refs) = deps.get(node) {
            for r in refs {
                if deps.contains_key(r.as_str()) {
                    visit(r.as_str(), deps, mark, stack, cycle_members);
                }
            }
        }
        stack.pop();
        mark.insert(node, Mark::Black);
    }

    for name in deps.keys() {
        if mark.get(name).copied() == Some(Mark::White) {
            let mut stack = Vec::new();
            visit(name, deps, &mut mark, &mut stack, &mut cycle_members);
        }
    }
    cycle_members.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{Button, FieldKind, Grid, Node, NodeKind, Source, When};
    use std::collections::BTreeMap;

    /// The error-severity problems only.
    fn errors(problems: Vec<Problem>) -> Vec<Problem> {
        problems.into_iter().filter(Problem::is_error).collect()
    }

    fn minimal(root: Node) -> SurfaceSpec {
        SurfaceSpec {
            surface: SURFACE_VERSION.to_string(),
            name: "t".to_string(),
            params: Vec::new(),
            state: serde_json::json!({ "x": 1 }),
            sources: BTreeMap::new(),
            root,
            extra: Default::default(),
        }
    }

    #[test]
    fn an_empty_spec_has_no_problems() {
        let spec = minimal(Node::leaf(NodeKind::Spacer));
        assert_eq!(errors(validate(&spec)), Vec::new());
    }

    #[test]
    fn a_wrong_surface_version_is_a_problem() {
        let mut spec = minimal(Node::leaf(NodeKind::Spacer));
        spec.surface = "0.9".to_string();
        let problems = validate(&spec);
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].path, "/surface");
    }

    #[test]
    fn non_object_state_is_a_problem() {
        let mut spec = minimal(Node::leaf(NodeKind::Spacer));
        spec.state = serde_json::json!([1, 2, 3]);
        let problems = validate(&spec);
        assert!(problems.iter().any(|p| p.path == "/state"));
    }

    #[test]
    fn a_bind_not_starting_with_state_is_a_problem() {
        let node = Node::leaf(NodeKind::Field(FieldKind::Text(serde_json::json!({}))))
            .with_bind("param.oops");
        let problems = validate(&minimal(node));
        assert!(problems.iter().any(|p| p.path == "/root/bind"));
    }

    #[test]
    fn a_set_path_not_starting_with_state_is_a_problem() {
        let node = Node::leaf(NodeKind::Button(Button {
            label: "go".to_string(),
            on_click: vec![Action::Set {
                path: "oops.x".to_string(),
                value: serde_json::json!(1),
            }],
        }));
        let problems = validate(&minimal(node));
        assert!(
            problems
                .iter()
                .any(|p| p.path == "/root/button/on_click/0/set/path"),
            "{problems:?}"
        );
    }

    #[test]
    fn a_reference_to_an_undeclared_state_key_is_a_problem() {
        let node = Node::leaf(NodeKind::Text("{{state.missing}}".to_string()));
        let problems = validate(&minimal(node));
        assert!(problems.iter().any(|p| p.message.contains("missing")));
    }

    #[test]
    fn a_when_path_with_an_undeclared_state_key_is_a_problem() {
        let node = Node::leaf(NodeKind::Text("hi".to_string())).with_when(When {
            path: "state.missing".to_string(),
            equals: None,
        });
        let problems = validate(&minimal(node));
        assert!(
            problems.iter().any(|p| p.path == "/root/when/path"),
            "{problems:?}"
        );
    }

    #[test]
    fn a_reference_to_an_undeclared_source_is_a_problem() {
        let node = Node::leaf(NodeKind::Text("{{source.nope}}".to_string()));
        let problems = validate(&minimal(node));
        assert!(problems.iter().any(|p| p.message.contains("nope")));
    }

    #[test]
    fn a_source_dependency_cycle_is_a_problem() {
        let mut spec = minimal(Node::leaf(NodeKind::Spacer));
        spec.sources.insert(
            "a".to_string(),
            Source::Verb {
                verb: "x-service_a".to_string(),
                args: serde_json::json!({ "v": "{{source.b}}" }),
            },
        );
        spec.sources.insert(
            "b".to_string(),
            Source::Verb {
                verb: "x-service_b".to_string(),
                args: serde_json::json!({ "v": "{{source.a}}" }),
            },
        );
        let problems = validate(&spec);
        assert!(
            problems.iter().any(|p| p.message.contains("cycle")),
            "{problems:?}"
        );
    }

    #[test]
    fn duplicate_node_ids_are_a_problem() {
        let root = Node::leaf(NodeKind::Column(vec![
            Node::leaf(NodeKind::Spacer).with_id("dup"),
            Node::leaf(NodeKind::Divider).with_id("dup"),
        ]));
        let problems = validate(&minimal(root));
        assert!(problems.iter().any(|p| p.message.contains("dup")));
    }

    #[test]
    fn empty_select_options_are_a_problem() {
        let node = Node::leaf(NodeKind::Field(FieldKind::Select(serde_json::json!({
            "options": []
        }))));
        let problems = validate(&minimal(node));
        assert!(problems
            .iter()
            .any(|p| p.path.ends_with("/field/select/options")));
    }

    #[test]
    fn non_empty_select_options_are_fine() {
        let node = Node::leaf(NodeKind::Field(FieldKind::Select(serde_json::json!({
            "options": ["a"]
        }))))
        .with_id("pick")
        .with_bind("state.x");
        let problems = validate(&minimal(node));
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn select_options_from_a_source_are_fine_and_plain_text_is_not() {
        let from_source = Node::leaf(NodeKind::Field(FieldKind::Select(serde_json::json!({
            "options": "{{source.choices}}"
        }))))
        .with_id("pick")
        .with_bind("state.x");
        let problems = validate(&minimal(from_source));
        assert!(problems.is_empty(), "{problems:?}");
        let text = Node::leaf(NodeKind::Field(FieldKind::Select(serde_json::json!({
            "options": "a, b"
        }))))
        .with_id("pick")
        .with_bind("state.x");
        assert!(validate(&minimal(text))
            .iter()
            .any(|p| p.path.ends_with("/field/select/options")));
    }

    #[test]
    fn a_grid_with_zero_columns_is_a_problem() {
        let node = Node::leaf(NodeKind::Grid(Grid {
            columns: 0,
            items: Vec::new(),
        }));
        let problems = validate(&minimal(node));
        assert!(problems.iter().any(|p| p.path.ends_with("/grid/columns")));
    }

    #[test]
    fn a_malformed_verb_name_is_a_problem() {
        let mut spec = minimal(Node::leaf(NodeKind::Spacer));
        spec.sources.insert(
            "s".to_string(),
            Source::Verb {
                verb: "not_a_valid_verb_name".to_string(),
                args: serde_json::json!({}),
            },
        );
        let problems = validate(&spec);
        assert!(problems.iter().any(|p| p.path == "/sources/s/verb"));
    }

    #[test]
    fn a_well_formed_verb_name_is_fine() {
        let mut spec = minimal(Node::leaf(NodeKind::Spacer));
        spec.sources.insert(
            "s".to_string(),
            Source::Verb {
                verb: "surface-demo-service_series".to_string(),
                args: serde_json::json!({}),
            },
        );
        assert_eq!(errors(validate(&spec)), Vec::new());
    }

    #[test]
    fn an_unrecognized_node_kind_is_reported_but_does_not_panic() {
        let node = Node::leaf(NodeKind::Unknown {
            kind: "sparkline".to_string(),
            node: serde_json::json!({"values": [1, 2, 3]}),
        });
        let problems = validate(&minimal(node));
        assert!(problems.iter().any(|p| p.message.contains("sparkline")));
    }

    // ── V5: `each` and `item` ───────────────────────────────────────────

    fn star_button(each: Option<&str>, args: Value) -> Node {
        Node::leaf(NodeKind::Button(Button {
            label: "Star".to_string(),
            on_click: vec![Action::Call {
                verb: "triage-service_set-starred".to_string(),
                args,
                into: None,
                each: each.map(str::to_string),
                adverbs: None,
            }],
        }))
    }

    fn call_with_adverbs(adverbs: Value) -> SurfaceSpec {
        minimal(Node::leaf(NodeKind::Button(Button {
            label: "Go".to_string(),
            on_click: vec![Action::Call {
                verb: "triage-service_set-starred".to_string(),
                args: serde_json::json!({}),
                into: None,
                each: None,
                adverbs: Some(adverbs),
            }],
        })))
    }

    #[test]
    fn non_object_adverbs_are_one_error_at_the_adverbs_path() {
        let errs = errors(validate(&call_with_adverbs(serde_json::json!(5))));
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(format!("{errs:?}").contains("/call/adverbs"));
        assert!(format!("{errs:?}").contains("object of adverb name to value"));
    }

    #[test]
    fn a_non_snake_case_adverb_key_is_one_error_naming_it() {
        let errs = errors(validate(&call_with_adverbs(
            serde_json::json!({"Bad-Key": true}),
        )));
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(format!("{errs:?}").contains("Bad-Key"));
        assert!(format!("{errs:?}").contains("/call/adverbs"));
    }

    #[test]
    fn well_formed_adverbs_validate_clean() {
        let spec =
            call_with_adverbs(serde_json::json!({"in_background": true, "within": {"ms": 5}}));
        assert_eq!(errors(validate(&spec)), Vec::new());
    }

    #[test]
    fn a_call_each_over_a_declared_state_key_is_fine() {
        let mut spec = minimal(star_button(
            Some("state.selected"),
            serde_json::json!({"id": "{{item}}"}),
        ));
        spec.state = serde_json::json!({"selected": []});
        assert_eq!(errors(validate(&spec)), Vec::new());
    }

    #[test]
    fn a_call_each_with_an_unknown_root_is_a_problem_at_the_each_path() {
        let node = star_button(Some("oops.selected"), serde_json::json!({}));
        let problems = validate(&minimal(node));
        assert!(
            problems.iter().any(
                |p| p.path == "/root/button/on_click/0/call/each" && p.message.contains("oops")
            ),
            "{problems:?}"
        );
    }

    #[test]
    fn an_each_path_that_references_item_is_a_problem() {
        let node = star_button(Some("item.selected"), serde_json::json!({}));
        let problems = validate(&minimal(node));
        assert!(
            problems
                .iter()
                .any(|p| p.path == "/root/button/on_click/0/call/each"),
            "{problems:?}"
        );
    }

    #[test]
    fn an_item_reference_in_call_args_without_each_is_a_problem() {
        let node = star_button(None, serde_json::json!({"id": "{{item}}"}));
        let problems = validate(&minimal(node));
        assert!(
            problems
                .iter()
                .any(|p| p.path == "/root/button/on_click/0/call/args"
                    && p.message
                        .contains("only bound inside an action with `each`")),
            "{problems:?}"
        );
    }

    #[test]
    fn an_item_reference_in_call_args_with_each_is_fine() {
        let mut spec = minimal(star_button(
            Some("state.selected"),
            serde_json::json!({"id": "{{item}}", "label": "star {{item.id}}"}),
        ));
        spec.state = serde_json::json!({"selected": []});
        assert_eq!(errors(validate(&spec)), Vec::new());
    }

    #[test]
    fn an_emit_each_and_its_item_reference_are_checked_the_same_way_as_call() {
        let with_each = Node::leaf(NodeKind::Button(Button {
            label: "go".to_string(),
            on_click: vec![Action::Emit {
                name: "triaged".to_string(),
                payload: serde_json::json!({"id": "{{item}}"}),
                each: Some("state.selected".to_string()),
            }],
        }));
        let mut spec = minimal(with_each);
        spec.state = serde_json::json!({"selected": []});
        assert_eq!(errors(validate(&spec)), Vec::new());

        let without_each = Node::leaf(NodeKind::Button(Button {
            label: "go".to_string(),
            on_click: vec![Action::Emit {
                name: "triaged".to_string(),
                payload: serde_json::json!({"id": "{{item}}"}),
                each: None,
            }],
        }));
        let problems = validate(&minimal(without_each));
        assert!(
            problems
                .iter()
                .any(|p| p.path == "/root/button/on_click/0/emit/payload"),
            "{problems:?}"
        );
    }

    fn table_with_id_field(id_field: Option<&str>) -> SurfaceSpec {
        minimal(
            Node::leaf(NodeKind::Table(crate::spec::Table {
                follow: Default::default(),
                rows: serde_json::json!([]),
                columns: vec!["bibcode".to_string()],
                id_field: id_field.map(str::to_string),
                on_select: Vec::new(),
                actions: Vec::new(),
                count_field: None,
            }))
            .with_id("t"),
        )
    }

    /// `id_field` is a literal property name: a template or an empty string
    /// is an error at its own path; a name, or none, is clean.
    #[test]
    fn a_table_id_field_must_be_a_literal_field_name() {
        assert_eq!(errors(validate(&table_with_id_field(None))), Vec::new());
        assert_eq!(
            errors(validate(&table_with_id_field(Some("bibcode")))),
            Vec::new()
        );
        for bad in ["", "{{state.f}}", "a{{b"] {
            let problems = errors(validate(&table_with_id_field(Some(bad))));
            assert!(
                problems.iter().any(|p| p.path == "/root/table/id_field"),
                "{bad:?}: {problems:?}"
            );
        }
    }
}
