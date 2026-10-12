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
//! silent truncation — table 3's "otherwise" column). A verb that declares
//! `returns_kind` (plan-koine-host H-D8) has its table labelled
//! `Result (<kind>)`.
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
use impress_service_core::{
    row_layout, secrets, Adverb, ProviderStatus, RowSchema, VerbDescriptor, VerbHandle,
};
use impress_surface::spec::{
    Action, Button, FieldKind, Node, NodeKind, Section, SurfaceSpec, When, SURFACE_VERSION,
};
use serde_json::{json, Map, Value};

/// The adverbs a verb accepts, in vocabulary order (docs/grammar.md § Adverbs
/// rules 1 and 5): the universal ones, the declared ones, and `within` on any
/// read-only verb (the pipeline drops the future at the deadline, so no
/// declaration is needed). `supports` answers the declaration question.
pub fn accepted_adverbs(class: SafetyClass, supports: impl Fn(Adverb) -> bool) -> Vec<Adverb> {
    Adverb::ALL
        .into_iter()
        .filter(|a| {
            supports(*a) || (*a == Adverb::Within && matches!(class, SafetyClass::ReadOnly))
        })
        .collect()
}

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
        &accepted_adverbs(desc.safety.class, |a| desc.supports(a)),
        desc.returns_kind,
        desc.rows,
        &secrets::missing_for(desc),
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
        &accepted_adverbs(desc.safety().class, |a| desc.supports(a)),
        desc.returns_kind(),
        desc.rows(),
        &desc.linked().map(secrets::missing_for).unwrap_or_default(),
    )
}

#[allow(clippy::too_many_arguments)]
fn render_surface(
    name: &str,
    description: &str,
    safety: SafetyClass,
    input_schema: &Value,
    output_schema: &Value,
    example_args: &Value,
    available: bool,
    adverbs: &[Adverb],
    returns_kind: Option<&str>,
    rows: Option<RowSchema>,
    missing_secrets: &[&str],
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
    // A declared secret that no source holds (plan-koine-followups F-D1): the
    // form says so first. Run stays — the pipeline's `missing-secret` refusal
    // is what a press of it shows.
    if !missing_secrets.is_empty() {
        let how = missing_secrets
            .iter()
            .map(|secret| format!("`{secret}`: {}", secrets::how_to_set(secret)))
            .collect::<Vec<_>>()
            .join("; ");
        column.push(
            Node::leaf(NodeKind::Status(impress_surface::spec::Status {
                level: "warning".to_string(),
                message: json!(format!(
                    "Missing secret {} — {how}. Run is refused until it is set.",
                    missing_secrets
                        .iter()
                        .map(|secret| format!("`{secret}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }))
            .with_id("missing-secret"),
        );
    }
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

    let adverb_fields: Vec<Node> = adverbs.iter().filter_map(|a| adverb_field(*a)).collect();
    if !adverb_fields.is_empty() {
        column.push(
            Node::leaf(NodeKind::Section(Section {
                title: "How to run".to_string(),
                collapsed: true,
                body: Box::new(Node::leaf(NodeKind::Column(adverb_fields))),
            }))
            .with_id("adverbs"),
        );
    }

    let mut run_args = Map::new();
    if let Some(props) = input_schema.get("properties").and_then(|p| p.as_object()) {
        for name in props.keys() {
            run_args.insert(name.clone(), Value::String(format!("{{{{state.{name}}}}}")));
        }
    }

    let more_args = run_args.clone();
    let run_adverbs = {
        let templates: Map<String, Value> = adverbs
            .iter()
            .filter(|a| adverb_field(**a).is_some())
            .map(|a| {
                (
                    a.as_str().to_string(),
                    Value::String(format!("{{{{state.adverbs.{}}}}}", a.as_str())),
                )
            })
            .collect();
        (!templates.is_empty()).then_some(Value::Object(templates))
    };

    let run_button = {
        let button = Node::leaf(NodeKind::Button(Button {
            label: "Run".to_string(),
            on_click: vec![Action::Call {
                verb: name.to_string(),
                args: Value::Object(run_args),
                into: Some("state.result".to_string()),
                each: None,
                adverbs: run_adverbs,
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

    column.push(result_node(output_schema, returns_kind, rows));

    // A verb that pages (a `cursor` argument, a `{rows, next}` result) gets a
    // More button: the same call with the cursor the last page answered,
    // whose rows are added to the table (plan-koine-live L-D5). `when` keeps
    // it off the last page, where `next` is null.
    let pages = available
        && row_envelope(output_schema).is_some()
        && input_schema
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|props| props.contains_key("cursor"));
    if pages {
        column.push(more_button(name, more_args));
    }

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
    let adverb_state: Map<String, Value> = adverbs
        .iter()
        .filter(|a| adverb_field(**a).is_some())
        .map(|a| (a.as_str().to_string(), Value::Bool(false)))
        .collect();
    if !adverb_state.is_empty() {
        state.insert("adverbs".to_string(), Value::Object(adverb_state));
    }
    state.insert("result".to_string(), Value::Null);
    if pages {
        // Where More's answer lands before it is added to the result.
        state.insert("more".to_string(), Value::Null);
    }
    if rows.is_some() {
        // The result table writes its selection here (`rows(…)`, F-D2), so a
        // second button on the same form can pass `{{state.selected}}`.
        state.insert("selected".to_string(), json!([]));
    }

    SurfaceSpec {
        surface: SURFACE_VERSION.to_string(),
        name: format!("Run {name}"),
        params: Vec::new(),
        state: Value::Object(state),
        sources: Default::default(),
        root: Node::leaf(NodeKind::Column(column)).with_id("root"),
    }
}

/// The More button of a paged verb: call it again with the form's arguments
/// and `cursor: {{state.result.next}}`, then add the page's rows to the
/// table's and take its `next`. No adverbs ride along: a tentative or
/// background page would have no rows to add.
fn more_button(verb: &str, mut args: Map<String, Value>) -> Node {
    args.insert("cursor".to_string(), json!("{{state.result.next}}"));
    Node::leaf(NodeKind::Button(Button {
        label: "More".to_string(),
        on_click: vec![
            Action::Call {
                verb: verb.to_string(),
                args: Value::Object(args),
                into: Some("state.more".to_string()),
                each: None,
                adverbs: None,
            },
            Action::Append {
                path: "state.result.rows".to_string(),
                value: json!("{{state.more.rows}}"),
            },
            Action::Set {
                path: "state.result.next".to_string(),
                value: json!("{{state.more.next}}"),
            },
        ],
    }))
    .with_id("more")
    .with_when(When {
        path: "state.result.next".to_string(),
        equals: None,
    })
}

/// The "How to run" field for one adverb, or `None` when the form does not
/// offer it.
///
/// Only the boolean adverbs are offered. `once` and `within` are omitted on
/// purpose: the pipeline refuses `once` of `""` (`'once' key must not be
/// empty`) and a `within` of `0`, and a form has no way yet to leave an
/// adverb out of the Run action, so a pre-filled `once`/`within` template
/// would make every default Run fail `invalid-argument`. The first follow-up
/// is a reduce-time "drop empty adverb" rule (or a `when` per adverb); once
/// that exists, `once` becomes a `text` field (`adverb-once`, "Once
/// (idempotency key)") and `within` a `number` field (`adverb-within_ms`,
/// "Within (ms)", templated as `{"ms": "{{state.adverbs.within_ms}}"}`).
fn adverb_field(adverb: Adverb) -> Option<Node> {
    let (label, help) = match adverb {
        Adverb::Tentatively => ("Tentatively", "Do not run; answer what would happen."),
        Adverb::WithReview => (
            "With review",
            "Queue for a person's confirmation, whatever the policy says.",
        ),
        Adverb::Reversibly => (
            "Reversibly",
            "Record an undo step; the result names the inverse call.",
        ),
        Adverb::InBackground => ("In background", "Start as a job and answer the handle."),
        Adverb::Once | Adverb::Within | Adverb::Every => return None,
    };
    let name = adverb.as_str();
    Some(
        Node::leaf(NodeKind::Field(FieldKind::Toggle(json!({}))))
            .with_id(format!("adverb-{name}"))
            .with_label(label)
            .with_help(help)
            .with_bind(format!("state.adverbs.{name}")),
    )
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
///
/// A verb that declares `returns_kind` (plan-koine-host H-D8) labels its
/// table of rows with that kind, `Result (publication)`, so the reader and a
/// layout pane agree on what a row is. Only a table is labelled: a flat
/// object or a string is not "rows of" anything.
///
/// A verb that also declares `rows(…)` (plan-koine-followups F-D2) says what
/// the rows are, so its result is a table whatever the schema's shape (a
/// `$ref` items schema would otherwise read as raw JSON): `columns` are the
/// declared ones (else the row object's properties), `id_field` is the
/// declared id, and a select writes the selected ids to `state.selected`
/// (`{{event.value}}`, the array of ids every select event carries). A result
/// object that wraps the one array of rows is read through that property.
fn result_node(output_schema: &Value, returns_kind: Option<&str>, rows: Option<RowSchema>) -> Node {
    if let Some(envelope) = row_envelope(output_schema) {
        return envelope_result_node(&envelope, returns_kind, rows);
    }
    let layout = rows.and_then(|_| row_layout(output_schema));
    let shape = match (&rows, &layout) {
        (Some(_), Some(_)) => ResultShape::Table,
        _ => result_shape(output_schema),
    };
    let label = match (&shape, returns_kind) {
        (ResultShape::Table, Some(kind)) => format!("Result ({kind})"),
        _ => "Result".to_string(),
    };
    let node_kind = match shape {
        ResultShape::Table => match (rows, layout) {
            (Some(rows), Some(layout)) => NodeKind::Table(impress_surface::spec::Table {
                rows: json!(match &layout.container {
                    Some(property) => format!("{{{{state.result.{property}}}}}"),
                    None => "{{state.result}}".to_string(),
                }),
                columns: if rows.columns.is_empty() {
                    layout.properties.clone()
                } else {
                    rows.columns.iter().map(|c| (*c).to_string()).collect()
                },
                id_field: Some(rows.id.to_string()),
                on_select: vec![Action::Set {
                    path: "state.selected".to_string(),
                    value: json!("{{event.value}}"),
                }],
            }),
            _ => NodeKind::Table(impress_surface::spec::Table {
                rows: json!("{{state.result}}"),
                columns: table_columns(output_schema),
                id_field: None,
                on_select: Vec::new(),
            }),
        },
        ResultShape::Kv => NodeKind::Kv(json!("{{state.result}}")),
        ResultShape::Text => NodeKind::Text("{{state.result}}".to_string()),
        ResultShape::Raw => NodeKind::Text("{{state.result}}".to_string()),
    };
    Node::leaf(node_kind)
        .with_id("result-view")
        .with_label(label)
}

/// An output schema that is a `{rows, next}` envelope (plan-koine-live L-D5):
/// an object whose `rows` property is an array of objects, with a `next`
/// property beside it (the cursor of the page after this one). `row_layout`
/// reads only a single-property wrapper, so this is read here, from the same
/// schema, by the same `$ref` rules.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RowEnvelope {
    /// The property names of the row object, in schema order.
    properties: Vec<String>,
}

fn row_envelope(output_schema: &Value) -> Option<RowEnvelope> {
    let top = schema_target(output_schema, output_schema);
    let props = top.get("properties")?.as_object()?;
    props.get("next")?;
    let rows = schema_target(output_schema, props.get("rows")?);
    let items = schema_target(output_schema, rows.get("items")?);
    let properties = items
        .get("properties")?
        .as_object()?
        .keys()
        .cloned()
        .collect();
    Some(RowEnvelope { properties })
}

/// Follow a local `$ref`, and look through the wrappers schemars puts around
/// one (`allOf` / `anyOf` / `oneOf` of one schema plus `null`).
fn schema_target<'a>(root: &'a Value, mut node: &'a Value) -> &'a Value {
    for _ in 0..16 {
        if let Some(reference) = node.get("$ref").and_then(Value::as_str) {
            match reference
                .strip_prefix('#')
                .and_then(|pointer| root.pointer(pointer))
            {
                Some(target) => {
                    node = target;
                    continue;
                }
                None => return node,
            }
        }
        let single = ["allOf", "anyOf", "oneOf"].iter().find_map(|key| {
            let live: Vec<&Value> = node
                .get(*key)?
                .as_array()?
                .iter()
                .filter(|item| item.get("type").and_then(Value::as_str) != Some("null"))
                .collect();
            (live.len() == 1).then(|| live[0])
        });
        match single {
            Some(next) => node = next,
            None => return node,
        }
    }
    node
}

/// The result of a verb that answers `{rows, next}`: a table over the
/// envelope itself, so the table reads its rows from it (and `append` can add
/// the next page to `state.result.rows`).
fn envelope_result_node(
    envelope: &RowEnvelope,
    returns_kind: Option<&str>,
    rows: Option<RowSchema>,
) -> Node {
    let (columns, id_field, on_select) = match rows {
        Some(rows) => (
            if rows.columns.is_empty() {
                envelope.properties.clone()
            } else {
                rows.columns.iter().map(|c| (*c).to_string()).collect()
            },
            Some(rows.id.to_string()),
            vec![Action::Set {
                path: "state.selected".to_string(),
                value: json!("{{event.value}}"),
            }],
        ),
        None => (envelope.properties.clone(), None, Vec::new()),
    };
    let label = match returns_kind {
        Some(kind) => format!("Result ({kind})"),
        None => "Result".to_string(),
    };
    Node::leaf(NodeKind::Table(impress_surface::spec::Table {
        rows: json!("{{state.result}}"),
        columns,
        id_field,
        on_select,
    }))
    .with_id("result-view")
    .with_label(label)
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
        id_field: None,
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
                adverbs: None,
            },
            Action::Call {
                verb: "impress-surface-service_surface-create".to_string(),
                args: json!({ "spec": "{{state.generated_spec}}", "name": "{{state.selected}}" }),
                into: Some("state.created".to_string()),
                each: None,
                adverbs: None,
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
                adverbs: None,
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
            supports: &[],
            adjective: None,
            returns_kind: None,
            rows: None,
            secrets: &[],
            source: Source::Linked,
            handler: |_| Box::pin(async { Ok(Value::Null) }),
        }
    }

    fn result_label(spec: &SurfaceSpec) -> String {
        let NodeKind::Column(children) = &spec.root.kind else {
            panic!("root is a column");
        };
        let node = children
            .iter()
            .find(|n| n.id.as_deref() == Some("result-view"))
            .expect("a result view");
        node.label.clone().expect("a label")
    }

    #[test]
    fn a_declared_returns_kind_labels_the_result_table_and_only_the_table() {
        let mut d = desc("t-service_x", SafetyClass::ReadOnly, schema_output_list);
        assert_eq!(result_label(&verb_surface(&d)), "Result");
        d.returns_kind = Some("publication");
        let spec = verb_surface(&d);
        assert_eq!(result_label(&spec), "Result (publication)");
        let problems = validate::validate(&spec);
        assert!(problems.iter().all(|p| !p.is_error()), "{problems:?}");
        // A flat object is not rows of anything.
        let mut flat = desc("t-service_x", SafetyClass::ReadOnly, schema_output_flat);
        flat.returns_kind = Some("publication");
        assert_eq!(result_label(&verb_surface(&flat)), "Result");
    }

    fn result_table(spec: &SurfaceSpec) -> impress_surface::spec::Table {
        let NodeKind::Column(children) = &spec.root.kind else {
            panic!("root is a column");
        };
        let node = children
            .iter()
            .find(|n| n.id.as_deref() == Some("result-view"))
            .expect("a result view");
        match &node.kind {
            NodeKind::Table(table) => table.clone(),
            other => panic!("expected a table, got {other:?}"),
        }
    }

    fn schema_output_papers() -> Value {
        json!({"type": "array", "items": {"type": "object", "properties": {
            "bibcode": {"type": "string"}, "title": {"type": "string"}, "year": {"type": "integer"}
        }}})
    }

    fn papers_verb(rows: RowSchema) -> VerbDescriptor {
        let mut d = desc("t-service_x", SafetyClass::ReadOnly, schema_output_papers);
        d.returns_kind = Some("paper");
        d.rows = Some(rows);
        d
    }

    /// F-D2: the declared columns and id reach the table, and a select writes
    /// the selected ids to `state.selected`.
    #[test]
    fn rows_set_the_tables_columns_id_field_and_selection() {
        let d = papers_verb(RowSchema {
            id: "bibcode",
            title: Some("title"),
            columns: &["bibcode", "year"],
        });
        let spec = verb_surface(&d);
        let problems = validate::validate(&spec);
        assert!(problems.iter().all(|p| !p.is_error()), "{problems:?}");
        assert_eq!(result_label(&spec), "Result (paper)");
        let table = result_table(&spec);
        assert_eq!(table.rows, json!("{{state.result}}"));
        assert_eq!(table.columns, vec!["bibcode", "year"]);
        assert_eq!(table.id_field.as_deref(), Some("bibcode"));
        assert_eq!(
            table.on_select,
            vec![Action::Set {
                path: "state.selected".to_string(),
                value: json!("{{event.value}}"),
            }]
        );
        assert_eq!(spec.state["selected"], json!([]));

        // After a select, `state.selected` holds the selected id array.
        let event = impress_surface::spec::Event {
            widget: "result-view".to_string(),
            kind: impress_surface::spec::EventKind::Select,
            value: json!(["2020ApJ...1B"]),
        };
        let (state, effects) =
            impress_surface::reduce(&spec, &spec.state, &Value::Null, &Value::Null, &event)
                .unwrap();
        assert!(effects.is_empty());
        assert_eq!(state["selected"], json!(["2020ApJ...1B"]));
    }

    #[test]
    fn rows_without_columns_default_to_the_row_objects_properties() {
        let d = papers_verb(RowSchema {
            id: "bibcode",
            title: None,
            columns: &[],
        });
        let table = result_table(&verb_surface(&d));
        assert_eq!(table.columns, vec!["bibcode", "title", "year"]);
        assert_eq!(table.id_field.as_deref(), Some("bibcode"));
    }

    /// An object result wrapping the one array of rows is read through that
    /// property, and a `$ref` items schema (how schemars writes a struct) is
    /// still a table because the verb said it returns rows.
    #[test]
    fn rows_read_through_a_wrapper_property_and_a_ref() {
        fn wrapped() -> Value {
            json!({"type": "object", "properties": {"papers": {"type": "array",
                "items": {"$ref": "#/definitions/Paper"}}},
                "definitions": {"Paper": {"type": "object", "properties": {
                    "bibcode": {"type": "string"}, "title": {"type": "string"}}}}})
        }
        let mut d = desc("t-service_x", SafetyClass::ReadOnly, wrapped);
        d.returns_kind = Some("paper");
        d.rows = Some(RowSchema {
            id: "bibcode",
            title: None,
            columns: &[],
        });
        let spec = verb_surface(&d);
        let problems = validate::validate(&spec);
        assert!(problems.iter().all(|p| !p.is_error()), "{problems:?}");
        let table = result_table(&spec);
        assert_eq!(table.rows, json!("{{state.result.papers}}"));
        assert_eq!(table.columns, vec!["bibcode", "title"]);
    }

    /// Without `rows(…)` the table is as it was: no id field, no selection.
    #[test]
    fn a_table_without_rows_declares_no_id_field_and_no_selection() {
        let d = desc("t-service_x", SafetyClass::ReadOnly, schema_output_list);
        let spec = verb_surface(&d);
        let table = result_table(&spec);
        assert_eq!(table.id_field, None);
        assert!(table.on_select.is_empty());
        assert!(spec.state.get("selected").is_none());
        assert!(!serde_json::to_string(&spec).unwrap().contains("id_field"));
    }

    fn paged_input() -> Value {
        json!({"type": "object", "properties": {
            "query": {"type": "string"}, "cursor": {"type": ["string", "null"]}}})
    }

    fn paged_output() -> Value {
        json!({"type": "object", "properties": {
            "rows": {"type": "array", "items": {"$ref": "#/definitions/Paper"}},
            "next": {"anyOf": [{"type": "string"}, {"type": "null"}]}},
            "definitions": {"Paper": {"type": "object", "properties": {
                "bibcode": {"type": "string"}, "title": {"type": "string"}}}}})
    }

    fn paged_verb() -> VerbDescriptor {
        let mut d = desc("t-service_x", SafetyClass::ReadOnly, paged_output);
        d.input_schema = paged_input;
        d.returns_kind = Some("paper");
        d.rows = Some(RowSchema {
            id: "bibcode",
            title: None,
            columns: &[],
        });
        d
    }

    fn column_of(spec: &SurfaceSpec) -> &Vec<Node> {
        let NodeKind::Column(children) = &spec.root.kind else {
            panic!("root is a column");
        };
        children
    }

    /// L-D5: a verb that takes `cursor` and answers `{rows, next}` gets a
    /// More button that calls it again with the cursor the last page named,
    /// adds the page's rows and takes its `next`.
    #[test]
    fn a_paged_verb_gets_a_more_button() {
        let spec = verb_surface(&paged_verb());
        let problems = validate::validate(&spec);
        assert!(problems.iter().all(|p| !p.is_error()), "{problems:?}");

        let more = column_of(&spec)
            .iter()
            .find(|n| n.id.as_deref() == Some("more"))
            .expect("a More button");
        let NodeKind::Button(button) = &more.kind else {
            panic!("a button")
        };
        assert_eq!(button.label, "More");
        assert_eq!(
            button.on_click,
            vec![
                Action::Call {
                    verb: "t-service_x".to_string(),
                    args: json!({"query": "{{state.query}}", "cursor": "{{state.result.next}}"}),
                    into: Some("state.more".to_string()),
                    each: None,
                    adverbs: None,
                },
                Action::Append {
                    path: "state.result.rows".to_string(),
                    value: json!("{{state.more.rows}}"),
                },
                Action::Set {
                    path: "state.result.next".to_string(),
                    value: json!("{{state.more.next}}"),
                },
            ]
        );
        // Hidden on the last page (and before the first).
        assert_eq!(
            more.when,
            Some(When {
                path: "state.result.next".to_string(),
                equals: None
            })
        );
        assert_eq!(spec.state["more"], Value::Null);

        // It follows the result, and the result is a table over the envelope.
        let ids: Vec<_> = column_of(&spec)
            .iter()
            .filter_map(|n| n.id.as_deref())
            .collect();
        let result = ids.iter().position(|id| *id == "result-view").unwrap();
        assert_eq!(ids.iter().position(|id| *id == "more"), Some(result + 1));
        let table = result_table(&spec);
        assert_eq!(table.rows, json!("{{state.result}}"));
        assert_eq!(table.columns, vec!["bibcode", "title"]);
        assert_eq!(table.id_field.as_deref(), Some("bibcode"));
        assert_eq!(result_label(&spec), "Result (paper)");
    }

    /// Run's cursor is the form's own field; only More reads `next`.
    #[test]
    fn the_run_button_still_sends_the_cursor_field() {
        let spec = verb_surface(&paged_verb());
        let run = column_of(&spec)
            .iter()
            .find(|n| n.id.as_deref() == Some("run"))
            .expect("Run");
        let NodeKind::Button(button) = &run.kind else {
            panic!("a button")
        };
        let Action::Call { args, .. } = &button.on_click[0] else {
            panic!("a call")
        };
        assert_eq!(args["cursor"], json!("{{state.cursor}}"));
    }

    /// Without a `cursor` argument, or without `next` in the result, there is
    /// no More.
    #[test]
    fn an_unpaged_verb_has_no_more_button() {
        let has_more = |spec: &SurfaceSpec| {
            column_of(spec)
                .iter()
                .any(|n| n.id.as_deref() == Some("more"))
        };
        // `next` but no `cursor` argument.
        let mut no_cursor = paged_verb();
        no_cursor.input_schema = schema_input;
        assert!(!has_more(&verb_surface(&no_cursor)));
        // A `cursor` argument but a plain array of rows.
        let mut no_next = papers_verb(RowSchema {
            id: "bibcode",
            title: None,
            columns: &[],
        });
        no_next.input_schema = paged_input;
        assert!(!has_more(&verb_surface(&no_next)));
        // Neither.
        let plain = desc("t-service_x", SafetyClass::ReadOnly, schema_output_list);
        let spec = verb_surface(&plain);
        assert!(!has_more(&spec));
        assert!(spec.state.get("more").is_none());
    }

    /// The More actions run as written: the call's arguments read the cursor
    /// the table last showed, `append` adds rows and `set` takes `next`.
    /// (`state.more` is filled here by hand: `into` is written by the runtime
    /// when the call lands, after `reduce` has run the handler.)
    #[test]
    fn the_more_actions_reduce_as_written() {
        let mut spec = verb_surface(&paged_verb());
        spec.state["result"] = json!({"rows": [{"bibcode": "a"}], "next": "c2"});
        spec.state["more"] = json!({"rows": [{"bibcode": "b"}], "next": null});
        let event = impress_surface::spec::Event {
            widget: "more".to_string(),
            kind: impress_surface::spec::EventKind::Click,
            value: Value::Null,
        };
        let (state, effects) =
            impress_surface::reduce(&spec, &spec.state, &Value::Null, &Value::Null, &event)
                .unwrap();
        assert!(matches!(
            effects.first(),
            Some(impress_surface::Effect::Call { args, into, .. })
                if args["cursor"] == json!("c2") && into.as_deref() == Some("state.more")
        ));
        assert_eq!(
            state["result"],
            json!({"rows": [{"bibcode": "a"}, {"bibcode": "b"}], "next": null})
        );
    }

    struct Held(&'static [&'static str]);
    impl secrets::SecretSource for Held {
        fn get(&self, name: &str) -> Option<secrets::Secret> {
            self.0
                .contains(&name)
                .then(|| secrets::Secret::new("value"))
        }
        fn names(&self) -> Vec<String> {
            self.0.iter().map(|n| (*n).to_string()).collect()
        }
    }

    /// F-D1: a form for a verb whose secret is missing opens with a warning
    /// naming it and how to set it; Run stays; with the secret held there is
    /// no warning.
    #[test]
    fn a_missing_secret_opens_the_form_with_a_warning_and_keeps_run() {
        static SECRETS: &[&str] = &["tok-a", "tok-b"];
        let mut d = desc("t-service_x", SafetyClass::ReadOnly, schema_output_flat);
        d.secrets = SECRETS;
        secrets::install(std::sync::Arc::new(Held(&["tok-b"])));
        let spec = verb_surface(&d);
        let problems = validate::validate(&spec);
        assert!(problems.iter().all(|p| !p.is_error()), "{problems:?}");
        let NodeKind::Column(children) = &spec.root.kind else {
            panic!("root is a column");
        };
        assert_eq!(children[0].id.as_deref(), Some("missing-secret"));
        let NodeKind::Status(status) = &children[0].kind else {
            panic!("a status node, got {:?}", children[0].kind);
        };
        assert_eq!(status.level, "warning");
        let message = status.message.as_str().unwrap();
        assert!(message.contains("`tok-a`"), "{message}");
        assert!(
            !message.contains("tok-b"),
            "only the missing one: {message}"
        );
        assert!(message.contains("KOINE_SECRET_TOK_A"), "{message}");
        assert!(message.contains("PUT /api/secret/tok-a"), "{message}");
        assert!(
            children.iter().any(|n| n.id.as_deref() == Some("run")),
            "Run stays"
        );

        secrets::install(std::sync::Arc::new(Held(&["tok-a", "tok-b"])));
        let spec = verb_surface(&d);
        let NodeKind::Column(children) = &spec.root.kind else {
            panic!("root is a column");
        };
        assert!(children
            .iter()
            .all(|n| n.id.as_deref() != Some("missing-secret")));
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

    fn form_adverb_keys(spec: &SurfaceSpec) -> Vec<String> {
        let mut ids: Vec<String> = impress_surface::spec::walk_with_pointers(&spec.root)
            .into_iter()
            .filter_map(|(_, n)| n.id.clone())
            .filter_map(|id| id.strip_prefix("adverb-").map(str::to_string))
            .collect();
        ids.sort();
        ids
    }

    fn run_adverb_keys(spec: &SurfaceSpec) -> Vec<String> {
        let mut keys = Vec::new();
        for (_, node) in impress_surface::spec::walk_with_pointers(&spec.root) {
            if let NodeKind::Button(b) = &node.kind {
                for a in &b.on_click {
                    if let Action::Call {
                        adverbs: Some(Value::Object(m)),
                        ..
                    } = a
                    {
                        keys.extend(m.keys().cloned());
                    }
                }
            }
        }
        keys.sort();
        keys
    }

    #[test]
    fn a_read_only_verb_accepts_within_but_the_form_offers_only_the_toggles() {
        let d = desc("t-service_x", SafetyClass::ReadOnly, schema_output_flat);
        let accepted = accepted_adverbs(d.safety.class, |a| d.supports(a));
        assert_eq!(
            accepted,
            vec![
                Adverb::Tentatively,
                Adverb::WithReview,
                Adverb::Once,
                Adverb::Within
            ]
        );
        let spec = verb_surface(&d);
        assert!(validate::validate(&spec).iter().all(|p| !p.is_error()));
        assert_eq!(form_adverb_keys(&spec), vec!["tentatively", "with_review"]);
        assert_eq!(run_adverb_keys(&spec), vec!["tentatively", "with_review"]);
        assert_eq!(
            spec.state["adverbs"],
            json!({"tentatively": false, "with_review": false})
        );
    }

    #[test]
    fn a_mutating_verb_offers_reversibly_when_declared_and_never_within() {
        let mut d = desc("t-service_x", SafetyClass::Mutating, schema_output_flat);
        d.supports = &[Adverb::Reversibly];
        let accepted = accepted_adverbs(d.safety.class, |a| d.supports(a));
        assert!(!accepted.contains(&Adverb::Within));
        let spec = verb_surface(&d);
        let problems = validate::validate(&spec);
        assert!(problems.iter().all(|p| !p.is_error()), "{problems:?}");
        let keys = form_adverb_keys(&spec);
        assert_eq!(keys, vec!["reversibly", "tentatively", "with_review"]);
        assert_eq!(run_adverb_keys(&spec), keys);
        assert!(spec.state["adverbs"].get("reversibly").is_some());
    }

    #[test]
    fn the_run_action_adverbs_equal_the_form_fields_for_every_declaration() {
        for class in [SafetyClass::ReadOnly, SafetyClass::Destructive] {
            let mut d = desc("t-service_x", class, schema_output_flat);
            d.supports = &[Adverb::Reversibly, Adverb::Within, Adverb::InBackground];
            let spec = verb_surface(&d);
            let problems = validate::validate(&spec);
            assert!(problems.iter().all(|p| !p.is_error()), "{problems:?}");
            assert_eq!(form_adverb_keys(&spec), run_adverb_keys(&spec));
        }
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
