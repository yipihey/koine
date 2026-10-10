//! `verb_surface(&VerbDescriptor) -> SurfaceSpec` and `catalogue() -> SurfaceSpec`
//! (ADR-0035 D2, plan-auto-gui-and-self-docs.md work package G4).
//!
//! A pure kit crate (`docs/kit-manifest.md`): no store, no IO, no `async`. It
//! reads a [`VerbDescriptor`]'s input and output JSON schemas and produces a
//! [`SurfaceSpec`] a `surface_validate`-equivalent (`impress_surface::validate`)
//! accepts with zero errors, for every verb in the linked inventory
//! (completeness statement 1, plan § "What complete means").
//!
//! # Form (plan table 2)
//!
//! One [`impress_surface::spec::FieldKind`] per input argument:
//!
//! | JSON Schema shape | Field |
//! |---|---|
//! | `string` (no `enum`, no date format) | `text` |
//! | `string` with `enum` | `select` |
//! | `string` with a date-shaped `format` (`date`, `date-time`) | `date` |
//! | `integer` / `number` | `number` |
//! | `boolean` | `toggle` |
//! | anything else (object, array, `$ref`, `allOf`, untyped) | a raw-JSON `text` field — the CLI's own precedent (`impress_service_core::cli::PropertyKind::Json`, table 2 class b) |
//!
//! Every field is prefilled from the verb's first `#[impress_example]`, when
//! it has one and the example names that argument.
//!
//! # Result (plan table 3)
//!
//! The output schema picks the result node bound to `state.result`:
//! array-of-objects → `table`; a flat object (no array-typed properties) →
//! `kv`; `string` → `text`; anything else → `text` of the raw JSON (no
//! silent truncation — table 3's "otherwise" column).
//!
//! # Safety (ADR-0035 D2, "review step inside the surface, state not a modal")
//!
//! A `destructive` or `external` verb's Run button is hidden
//! (`when: {path: "state.confirmed", equals: true}`) behind a `status`
//! warning and a `toggle` field the caller must set first — three vocabulary
//! primitives already in `docs/agent-surfaces.md`, no new widget or action
//! kind (the plan is explicit that D-G3's list-input/record-picker/confirm
//! widgets are separate, ask-first work).
//!
//! # The catalogue
//!
//! [`catalogue`] is a single generated surface: a search field, a table
//! sourced from the read-only verb `capabilities-service_list-verbs`, and a
//! button that — for the row selected — chains `capabilities-service_verb-surface`
//! → `impress-surface-service_surface-create` → `impress-surface-service_surface-show`,
//! entirely through [`impress_surface::spec::Action::Call`] (D2: "through an
//! `open` action or a param, using only the current vocabulary" — this uses
//! the vocabulary's own five-verb loop, `docs/agent-surfaces.md` § Loop,
//! rather than adding a "render this spec inline" node kind).

use impress_service_core::descriptor::SafetyClass;
use impress_service_core::{ProviderStatus, VerbDescriptor, VerbHandle};
use impress_surface::spec::{
    Action, Button, FieldKind, Node, NodeKind, SurfaceSpec, When, SURFACE_VERSION,
};
use serde_json::{json, Map, Value};

/// Build the generated form for one verb.
pub fn verb_surface(desc: &VerbDescriptor) -> SurfaceSpec {
    let input_schema = (desc.input_schema)();
    let output_schema = (desc.output_schema)();
    let example_args = desc
        .examples
        .first()
        .map(|e| e.args_value())
        .unwrap_or(Value::Null);
    render_surface(
        desc.name,
        desc.description,
        desc.safety.class,
        &input_schema,
        &output_schema,
        &example_args,
        true,
    )
}

/// Build a generated form from a linked or runtime-owned descriptor.
pub fn verb_surface_handle(desc: &VerbHandle) -> SurfaceSpec {
    let input_schema = desc.input_schema();
    let output_schema = desc.output_schema();
    let example_args = desc
        .examples()
        .first()
        .map(|e| e.args_value())
        .unwrap_or(Value::Null);
    render_surface(
        desc.name(),
        desc.description(),
        desc.safety().class,
        &input_schema,
        &output_schema,
        &example_args,
        !matches!(desc, VerbHandle::Provider(_))
            || (desc.provider_status() == Some(ProviderStatus::Available)
                && desc.deprecation_notice().is_none()),
    )
}

fn render_surface(
    name: &str,
    description: &str,
    safety: SafetyClass,
    input_schema: &Value,
    output_schema: &Value,
    example_args: &Value,
    available: bool,
) -> SurfaceSpec {
    let mut form_fields: Vec<Node> = Vec::new();
    let required = required_names(input_schema);
    if let Some(props) = input_schema.get("properties").and_then(|p| p.as_object()) {
        for (name, prop) in props {
            let prefill = example_args.get(name).cloned();
            form_fields.push(argument_field(name, prop, required.contains(name), prefill));
        }
    }

    let review = available && matches!(safety, SafetyClass::Destructive | SafetyClass::External);

    let mut column: Vec<Node> = Vec::new();
    column.push(
        Node::leaf(NodeKind::Text(format!("**{}**\n\n{}", name, description)))
            .with_id("description"),
    );

    if !available {
        column.push(
            Node::leaf(NodeKind::Status(impress_surface::spec::Status {
                level: "warning".to_string(),
                message: json!("Provider unavailable. This verb cannot run until it reconnects."),
            }))
            .with_id("provider-unavailable"),
        );
    }

    if review {
        column.push(
            Node::leaf(NodeKind::Status(impress_surface::spec::Status {
                level: "warning".to_string(),
                message: json!(format!(
                    "This is a {} action ({}). Confirm before running.",
                    safety.as_str(),
                    if safety == SafetyClass::External {
                        "it leaves this process"
                    } else {
                        "it cannot be undone by this surface"
                    }
                )),
            }))
            .with_id("safety-warning"),
        );
        column.push(
            Node::leaf(NodeKind::Field(FieldKind::Toggle(json!({}))))
                .with_id("confirmed")
                .with_label("I understand, run it anyway")
                .with_bind("state.confirmed"),
        );
    }

    column.extend(form_fields);

    let mut run_args = Map::new();
    if let Some(props) = input_schema.get("properties").and_then(|p| p.as_object()) {
        for name in props.keys() {
            run_args.insert(name.clone(), Value::String(format!("{{{{state.{name}}}}}")));
        }
    }

    let run_button = {
        let button = Node::leaf(NodeKind::Button(Button {
            label: "Run".to_string(),
            on_click: vec![Action::Call {
                verb: name.to_string(),
                args: Value::Object(run_args),
                into: Some("state.result".to_string()),
                each: None,
            }],
        }))
        .with_id("run");
        if review {
            button.with_when(When {
                path: "state.confirmed".to_string(),
                equals: Some(json!(true)),
            })
        } else {
            button
        }
    };
    if available {
        column.push(run_button);
    }

    column.push(result_node(output_schema));

    let mut state = Map::new();
    if let Some(props) = input_schema.get("properties").and_then(|p| p.as_object()) {
        for (name, prop) in props {
            let value = example_args
                .get(name)
                .cloned()
                .unwrap_or_else(|| default_for(prop));
            state.insert(name.clone(), value);
        }
    }
    if review {
        state.insert("confirmed".to_string(), Value::Bool(false));
    }
    state.insert("result".to_string(), Value::Null);

    SurfaceSpec {
        surface: SURFACE_VERSION.to_string(),
        name: format!("Run {name}"),
        params: Vec::new(),
        state: Value::Object(state),
        sources: Default::default(),
        root: Node::leaf(NodeKind::Column(column)).with_id("root"),
    }
}

/// The set of required property names, from the schema's `"required"` array.
fn required_names(schema: &Value) -> std::collections::HashSet<String> {
    schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Table 2: one JSON-Schema property to one form field.
fn argument_field(name: &str, prop: &Value, required: bool, prefill: Option<Value>) -> Node {
    let label = name.replace('_', " ");
    let help = prop
        .get("description")
        .and_then(|d| d.as_str())
        .map(str::to_string);
    let mut node = match classify(prop) {
        Shape::Text => Node::leaf(NodeKind::Field(FieldKind::Text(json!({})))),
        Shape::Number => Node::leaf(NodeKind::Field(FieldKind::Number(json!({})))),
        Shape::Boolean => Node::leaf(NodeKind::Field(FieldKind::Toggle(json!({})))),
        Shape::Date => Node::leaf(NodeKind::Field(FieldKind::Date(json!({})))),
        Shape::Select(options) => Node::leaf(NodeKind::Field(FieldKind::Select(
            json!({ "options": options }),
        ))),
        // Table 2 class b: array-of-scalars, ref-object, inline-object,
        // array-of-objects, map, tagged-union, other — one raw-JSON field,
        // the CLI's own precedent (`cli.rs` `PropertyKind::Json`).
        Shape::RawJson => Node::leaf(NodeKind::Field(FieldKind::Text(json!({
            "raw_json": true
        })))),
    };
    node = node
        .with_id(format!("arg-{name}"))
        .with_label(if required {
            format!("{label} (required)")
        } else {
            label
        })
        .with_bind(format!("state.{name}"));
    if let Some(help) = help {
        node = node.with_help(help);
    }
    let _ = prefill; // prefill is written into `state`, not the node itself.
    node
}

enum Shape {
    Text,
    Number,
    Boolean,
    Date,
    Select(Vec<Value>),
    RawJson,
}

/// Whether one JSON-Schema property (as `verb.input_schema()` publishes it)
/// gets a raw-JSON field rather than a typed one (table 2 class b). Public
/// so the census test and the docs generator (G6) can classify a verb's
/// whole argument list the same way this crate's own form does, without a
/// second copy of [`classify`].
pub fn argument_is_raw_json(prop: &Value) -> bool {
    matches!(classify(prop), Shape::RawJson)
}

fn classify(prop: &Value) -> Shape {
    // `enum` wins regardless of the carrier type (a string enum is by far
    // the common case, but nothing stops a schema naming an integer enum).
    if let Some(options) = prop.get("enum").and_then(|e| e.as_array()) {
        if !options.is_empty() {
            return Shape::Select(options.clone());
        }
    }
    if let Some(format) = prop.get("format").and_then(|f| f.as_str()) {
        if format == "date" || format == "date-time" {
            return Shape::Date;
        }
    }
    if let Some(t) = prop.get("type") {
        if let Some(s) = t.as_str() {
            return shape_for_type_str(s);
        }
        // `"type": ["string", "null"]` (Option<T>).
        if let Some(arr) = t.as_array() {
            for entry in arr {
                if let Some(s) = entry.as_str() {
                    if s != "null" {
                        return shape_for_type_str(s);
                    }
                }
            }
        }
    }
    // schemars nullable-as-`anyOf`: `Option<String>` etc.
    if let Some(arr) = prop.get("anyOf").and_then(|v| v.as_array()) {
        for entry in arr {
            if entry.get("type").and_then(|t| t.as_str()) == Some("null") {
                continue;
            }
            return classify(entry);
        }
    }
    Shape::RawJson
}

fn shape_for_type_str(s: &str) -> Shape {
    match s {
        "string" => Shape::Text,
        "integer" | "number" => Shape::Number,
        "boolean" => Shape::Boolean,
        _ => Shape::RawJson,
    }
}

/// A reasonable empty default for a field's initial `state` value, by shape —
/// never `Value::Null` for a scalar shape, so the field renders its widget's
/// own empty state rather than a template resolving to the literal text
/// "null".
fn default_for(prop: &Value) -> Value {
    match classify(prop) {
        Shape::Text | Shape::Date => Value::String(String::new()),
        Shape::Number => Value::Null,
        Shape::Boolean => Value::Bool(false),
        Shape::Select(options) => options.first().cloned().unwrap_or(Value::Null),
        Shape::RawJson => Value::String("{}".to_string()),
    }
}

/// Table 3: the output schema picks the result node.
fn result_node(output_schema: &Value) -> Node {
    let node_kind = match result_shape(output_schema) {
        ResultShape::Table => NodeKind::Table(impress_surface::spec::Table {
            rows: json!("{{state.result}}"),
            columns: table_columns(output_schema),
            on_select: Vec::new(),
        }),
        ResultShape::Kv => NodeKind::Kv(json!("{{state.result}}")),
        ResultShape::Text => NodeKind::Text("{{state.result}}".to_string()),
        ResultShape::Raw => NodeKind::Text("{{state.result}}".to_string()),
    };
    Node::leaf(node_kind)
        .with_id("result-view")
        .with_label("Result")
}

enum ResultShape {
    Table,
    Kv,
    Text,
    Raw,
}

fn result_shape(schema: &Value) -> ResultShape {
    match schema.get("type").and_then(|t| t.as_str()) {
        Some("array") => {
            let items_are_objects = schema
                .get("items")
                .and_then(|i| i.get("type"))
                .and_then(|t| t.as_str())
                == Some("object");
            if items_are_objects {
                ResultShape::Table
            } else {
                ResultShape::Text
            }
        }
        Some("object") => ResultShape::Kv,
        Some("string") => ResultShape::Text,
        Some(_) => ResultShape::Text,
        None => {
            // No bare `"type"` (a `$ref`, `allOf`, or untyped schema): the
            // honest reading, per table 3's "otherwise" column, is raw JSON.
            if schema.get("properties").is_some() {
                ResultShape::Kv
            } else {
                ResultShape::Raw
            }
        }
    }
}

/// Column names for a table result: the item schema's own property names
/// when present, else a single `value` column.
fn table_columns(schema: &Value) -> Vec<String> {
    schema
        .get("items")
        .and_then(|i| i.get("properties"))
        .and_then(|p| p.as_object())
        .map(|props| props.keys().cloned().collect())
        .filter(|v: &Vec<String>| !v.is_empty())
        .unwrap_or_else(|| vec!["value".to_string()])
}

// ---------------------------------------------------------------------------
// The catalogue
// ---------------------------------------------------------------------------

/// The generated catalogue surface (ADR-0035 D2): search, a table of every
/// linked verb (sourced from `capabilities-service_list-verbs`, so it needs
/// no static list here — the inventory is read live), and a button that
/// opens the selected verb's generated form through the vocabulary's own
/// create/show loop.
pub fn catalogue() -> SurfaceSpec {
    let includes_providers = impress_service_core::call::descriptors()
        .any(|verb| matches!(verb, VerbHandle::Provider(_)));
    let mut sources = std::collections::BTreeMap::new();
    sources.insert(
        "verbs".to_string(),
        impress_surface::spec::Source::Verb {
            verb: "capabilities-service_list-verbs".to_string(),
            args: json!({ "search": "{{state.query}}", "group": "{{state.group}}" }),
        },
    );

    let search = Node::leaf(NodeKind::Field(FieldKind::Text(json!({}))))
        .with_id("search")
        .with_label("Search")
        .with_bind("state.query");
    let group_filter = Node::leaf(NodeKind::Field(FieldKind::Text(json!({}))))
        .with_id("group-filter")
        .with_label("Service (optional)")
        .with_help("Filter to one service's verbs; empty shows every group.")
        .with_bind("state.group");

    let mut columns = vec![
        "name".to_string(),
        "service".to_string(),
        "description".to_string(),
        "safety".to_string(),
        "since".to_string(),
    ];
    if includes_providers {
        columns.push("availability".to_string());
    }
    let table = Node::leaf(NodeKind::Table(impress_surface::spec::Table {
        rows: json!("{{source.verbs}}"),
        columns,
        on_select: vec![Action::Set {
            path: "state.selected".to_string(),
            value: json!("{{event.value}}"),
        }],
    }))
    .with_id("catalogue-table");

    let open_button = Node::leaf(NodeKind::Button(Button {
        label: "Open form".to_string(),
        on_click: vec![
            Action::Call {
                verb: "capabilities-service_verb-surface".to_string(),
                args: json!({ "verb": "{{state.selected}}" }),
                into: Some("state.generated_spec".to_string()),
                each: None,
            },
            Action::Call {
                verb: "impress-surface-service_surface-create".to_string(),
                args: json!({ "spec": "{{state.generated_spec}}", "name": "{{state.selected}}" }),
                into: Some("state.created".to_string()),
                each: None,
            },
            Action::Call {
                verb: "impress-surface-service_surface-show".to_string(),
                args: json!({
                    "id": "{{state.created.id}}",
                    "target": "{{state.target}}",
                    "app_id": "{{state.app_id}}"
                }),
                into: Some("state.shown".to_string()),
                each: None,
            },
        ],
    }))
    .with_id("open-form")
    .with_when(When {
        path: "state.selected".to_string(),
        equals: None,
    });

    let root = Node::leaf(NodeKind::Column(vec![
        Node::leaf(NodeKind::Text(if includes_providers {
            "**Verb catalogue** — linked and runtime provider capabilities. Unavailable provider verbs cannot run."
                .to_string()
        } else {
            "**Verb catalogue** — every capability the linked inventory exposes.".to_string()
        }))
        .with_id("catalogue-heading"),
        search,
        group_filter,
        table,
        open_button,
    ]))
    .with_id("root");

    SurfaceSpec {
        surface: SURFACE_VERSION.to_string(),
        name: "Verb catalogue".to_string(),
        params: Vec::new(),
        state: json!({
            "query": "",
            "group": "",
            "selected": Value::Null,
            "target": "detail",
            "app_id": "impress",
            "generated_spec": Value::Null,
            "created": Value::Null,
            "shown": Value::Null,
        }),
        sources,
        root,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use impress_service_core::descriptor::{Effects, Safety, Source, VerbDescriptor};
    use impress_service_core::{ProviderVerb, VerbHandle};
    use impress_surface::validate;
    use serde_json::json;
    use std::sync::Arc;

    fn schema_input() -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "who to greet"},
                "count": {"type": "integer"},
                "loud": {"type": "boolean"},
                "mode": {"type": "string", "enum": ["a", "b"]},
                "ids": {"type": "array", "items": {"type": "string"}},
            },
            "required": ["name"],
        })
    }

    fn schema_output_flat() -> Value {
        json!({"type": "object", "properties": {"ok": {"type": "boolean"}}})
    }

    fn schema_output_list() -> Value {
        json!({"type": "array", "items": {"type": "object", "properties": {"id": {"type": "string"}}}})
    }

    fn desc(name: &'static str, class: SafetyClass, out: fn() -> Value) -> VerbDescriptor {
        VerbDescriptor {
            name,
            service: "t-service",
            method: "x",
            description: "Do a thing.",
            input_schema: schema_input,
            output_schema: out,
            safety: Safety {
                class,
                idempotent: class.default_idempotent(),
            },
            effects: Effects::NONE,
            since: "0.1.0",
            deprecated: None,
            aliases: &[],
            examples: &[],
            strict: false,
            budget_ms: None,
            replay_full: false,
            source: Source::Linked,
            handler: |_| Box::pin(async { Ok(Value::Null) }),
        }
    }

    #[test]
    fn a_read_only_verb_gets_a_form_with_no_review_step_and_validates() {
        let d = desc("t-service_x", SafetyClass::ReadOnly, schema_output_flat);
        let spec = verb_surface(&d);
        let problems = validate::validate(&spec);
        assert!(problems.iter().all(|p| !p.is_error()), "{problems:?}");
        assert!(spec.state.get("confirmed").is_none());
    }

    #[test]
    fn a_destructive_verb_gates_run_behind_confirmation() {
        let d = desc("t-service_x", SafetyClass::Destructive, schema_output_flat);
        let spec = verb_surface(&d);
        let problems = validate::validate(&spec);
        assert!(problems.iter().all(|p| !p.is_error()), "{problems:?}");
        assert_eq!(spec.state.get("confirmed"), Some(&json!(false)));
        let text = serde_json::to_string(&spec).unwrap();
        assert!(text.contains("\"confirmed\""));
    }

    #[test]
    fn a_list_of_objects_result_becomes_a_table() {
        let d = desc("t-service_x", SafetyClass::ReadOnly, schema_output_list);
        let spec = verb_surface(&d);
        let problems = validate::validate(&spec);
        assert!(problems.iter().all(|p| !p.is_error()), "{problems:?}");
        let text = serde_json::to_string(&spec).unwrap();
        assert!(text.contains("\"table\""));
    }

    #[test]
    fn the_catalogue_validates() {
        let spec = catalogue();
        let problems = validate::validate(&spec);
        assert!(problems.iter().all(|p| !p.is_error()), "{problems:?}");
    }

    #[test]
    fn unavailable_provider_form_warns_and_has_no_run_action() {
        let handle = VerbHandle::Provider(Arc::new(ProviderVerb {
            name: "fixture-service_echo".into(),
            service: "fixture-service".into(),
            method: "echo".into(),
            description: "Echo text".into(),
            input_schema: json!({"type":"object","properties":{"text":{"type":"string","description":"Text"}},"additionalProperties":false}),
            output_schema: json!({"type":"object","properties":{"echo":{"type":"string"}}}),
            declared_safety: Safety {
                class: SafetyClass::ReadOnly,
                idempotent: true,
            },
            effective_safety: Safety {
                class: SafetyClass::External,
                idempotent: false,
            },
            since: "0.1.0".into(),
            examples: vec![impress_service_core::ProviderExample {
                name: "sample".into(),
                args: r#"{"text":"hello"}"#.into(),
                expect: Some(r#"{"echo":"hello"}"#.into()),
            }],
            provider_id: "fixture".into(),
            status: ProviderStatus::Unavailable,
            deprecated_since: None,
            generation: 1,
        }));
        let spec = verb_surface_handle(&handle);
        let problems = validate::validate(&spec);
        assert!(problems.iter().all(|p| !p.is_error()), "{problems:?}");
        let wire = serde_json::to_value(&spec).unwrap();
        let text = wire.to_string();
        assert!(text.contains("provider-unavailable"));
        assert!(
            !text.contains("\"on_click\""),
            "unavailable form must not call provider"
        );
        assert_eq!(spec.state["text"], "hello");
    }
}
