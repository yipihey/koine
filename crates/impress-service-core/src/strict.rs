//! Strict arguments: a field the published schema does not name is refused.
//!
//! An agent that misspells a field used to get the field's default and
//! `ok: true` — `{"kind": "publication"}` for `kinds` compiled to every kind
//! in the manifest, and a pane reference copied in another spelling read as
//! "the focused pane", which `close` then closed (review RL-L3, AC-F3). A
//! typo is an error, not a silent default.
//!
//! The check is driven by the JSON schema `schemars` derives — the same
//! `inputSchema` MCP publishes and the CLI builds its flags from — so the
//! rule is exactly "what the schema says", with no second list of field names
//! to keep in step. It walks `$ref`s, `allOf`, and picks the matching branch
//! of a `oneOf`/`anyOf` (an internally tagged enum's tag decides), so a typo
//! deep inside a pane spec or a query is found as well as one at the top.
//!
//! It is applied on **argument** paths only. A persisted row (a layout tree, a
//! surface document) stays lenient on purpose: a newer build may have written
//! a field this one does not know, and refusing to read it would lose the
//! user's data.
//!
//! A schema that says nothing about an object's keys (`serde_json::Value`, a
//! map without `additionalProperties`) accepts any key: free-form JSON is
//! free-form.

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::refusal::Refusal;

/// One field the schema does not name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownField {
    /// Where, dotted from the argument object: `target.ref`,
    /// `spec.query.kind`, `verbs.2.targett`.
    pub path: String,
    /// The fields that object does take, sorted.
    pub allowed: Vec<String>,
}

/// Every key of `value` the schema does not name. `schema` is a root schema
/// as `schemars::schema_for!` serializes it (`definitions` at the top).
pub fn unknown_fields(value: &Value, schema: &Value) -> Vec<UnknownField> {
    let mut out = Vec::new();
    walk(value, schema, schema, "", &mut out);
    out
}

/// Refuse `value` with `invalid-argument` when it carries a field `schema`
/// does not name, naming each such field and what its object takes.
pub fn check(value: &Value, schema: &Value) -> Result<(), Refusal> {
    let unknown = unknown_fields(value, schema);
    if unknown.is_empty() {
        return Ok(());
    }
    Err(Refusal::invalid_argument(describe(&unknown)))
}

/// Parse an argument strictly: [`check`] against `T`'s schema, then
/// deserialize, a parse failure being `invalid-argument` too.
pub fn from_value<T: DeserializeOwned + schemars::JsonSchema>(value: Value) -> Result<T, Refusal> {
    let schema = serde_json::to_value(schemars::schema_for!(T))
        .map_err(|e| Refusal::internal(format!("schema did not serialize: {e}")))?;
    check(&value, &schema)?;
    serde_json::from_value(value).map_err(|e| Refusal::invalid_argument(e.to_string()))
}

/// What the generated invoker of a strict service does with its argument
/// object (`impress_service_impl! { strict_args = true, … }`): check it
/// against the method's input schema and parse it, or say why not — naming
/// the tool, so an agent reading only the message knows which call it was.
///
/// The argument object is a method's whole parameter list, never free-form
/// JSON: a method whose schema is a plain object naming no properties takes
/// no arguments, so a key there is refused too (a free-form object deeper
/// down stays free, and an enum root is walked by [`check`]).
pub fn args<T: DeserializeOwned>(tool: &str, value: Value, schema: &Value) -> Result<T, Refusal> {
    check_args(tool, &value, schema)?;
    serde_json::from_value(value)
        .map_err(|e| Refusal::invalid_argument(e.to_string()).context(tool))
}

/// The schema half of [`args`] alone — what the pipeline's strict-args
/// layer runs for every verb of a `strict_args` service before its
/// generated handler parses the object (plan-verb-pipeline P2). Same rule,
/// same message, naming the tool.
pub fn check_args(tool: &str, value: &Value, schema: &Value) -> Result<(), Refusal> {
    // Only a plain object schema that names nothing — an args struct with no
    // fields. A schema that describes its object any other way (an enum's
    // `oneOf`, a `$ref`, `additionalProperties`) is `check`'s to walk.
    let names_nothing = schema.get("type").and_then(Value::as_str) == Some("object")
        && [
            "properties",
            "additionalProperties",
            "patternProperties",
            "oneOf",
            "anyOf",
            "allOf",
            "$ref",
        ]
        .iter()
        .all(|key| schema.get(*key).is_none());
    if names_nothing {
        if let Some(extra) = value.as_object().and_then(|m| m.keys().next()) {
            return Err(Refusal::invalid_argument(format!(
                "unknown field '{extra}' (it takes no arguments)"
            ))
            .context(tool));
        }
    }
    check(value, schema).map_err(|r| r.context(tool))
}

/// The refusal a strict service's generated handler answers when its
/// argument object does not deserialize (a type error): the same
/// `invalid-argument`, naming the tool, that [`args`] produced when the
/// parse lived in the invoker.
pub fn parse_refusal(tool: &str, error: impl std::fmt::Display) -> Value {
    refusal_value(&Refusal::invalid_argument(error.to_string()).context(tool))
}

/// The result a strict invoker answers for a refused argument: the envelope
/// every layout and surface verb answers (`ok`, `code`, `message`), at the
/// current [`crate::wire::WIRE_VERSION`]. A refusal, not a transport error,
/// so MCP carries it as a result and the CLI exits 3 on it.
pub fn refusal_value(refusal: &Refusal) -> Value {
    serde_json::json!({
        "ok": false,
        "code": refusal.code,
        "message": refusal.message,
        "wire_version": crate::wire::WIRE_VERSION,
    })
}

fn describe(unknown: &[UnknownField]) -> String {
    unknown
        .iter()
        .map(|field| {
            let (parent, name) = match field.path.rsplit_once('.') {
                Some((parent, name)) => (Some(parent), name),
                None => (None, field.path.as_str()),
            };
            let takes = if field.allowed.is_empty() {
                "no fields".to_string()
            } else {
                field.allowed.join(", ")
            };
            match parent {
                Some(parent) => format!("unknown field '{name}' in '{parent}' (it takes: {takes})"),
                None => format!("unknown field '{name}' (this call takes: {takes})"),
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

/// Follow `$ref` (and the `allOf: [{$ref}]` wrapper schemars puts around a
/// described reference) to the schema it names.
fn resolve<'a>(schema: &'a Value, root: &'a Value) -> &'a Value {
    let mut current = schema;
    for _ in 0..32 {
        let Some(object) = current.as_object() else {
            return current;
        };
        if let Some(Value::String(reference)) = object.get("$ref") {
            match lookup(reference, root) {
                Some(target) => {
                    current = target;
                    continue;
                }
                None => return current,
            }
        }
        if let Some(Value::Array(parts)) = object.get("allOf") {
            if parts.len() == 1 && !object.contains_key("properties") {
                current = &parts[0];
                continue;
            }
        }
        return current;
    }
    current
}

fn lookup<'a>(reference: &str, root: &'a Value) -> Option<&'a Value> {
    let pointer = reference.strip_prefix('#')?;
    root.pointer(pointer)
}

/// Could a value matching `schema` be a JSON object?
fn admits_object(schema: &Value, root: &Value) -> bool {
    let schema = resolve(schema, root);
    let Some(object) = schema.as_object() else {
        // `true` admits anything; `false` nothing.
        return schema.as_bool().unwrap_or(false);
    };
    match object.get("type") {
        Some(Value::String(t)) => t == "object",
        Some(Value::Array(types)) => types.iter().any(|t| t == "object"),
        // No type: an object if it describes one, or anything at all.
        _ => true,
    }
}

fn walk(value: &Value, schema: &Value, root: &Value, path: &str, out: &mut Vec<UnknownField>) {
    let schema = resolve(schema, root);
    let Some(object) = schema.as_object() else {
        return; // `true`: anything goes.
    };
    match value {
        Value::Object(fields) => {
            for key in ["oneOf", "anyOf"] {
                if let Some(Value::Array(alternatives)) = object.get(key) {
                    walk_alternatives(fields, alternatives, root, path, out);
                    return;
                }
            }
            walk_object(fields, object, root, path, out);
        }
        Value::Array(items) => {
            for key in ["oneOf", "anyOf"] {
                if let Some(Value::Array(alternatives)) = object.get(key) {
                    // The branch that is an array schema, if one is.
                    if let Some(branch) = alternatives.iter().find(|b| {
                        resolve(b, root)
                            .get("type")
                            .map(|t| {
                                t == "array"
                                    || t.as_array().is_some_and(|a| a.iter().any(|x| x == "array"))
                            })
                            .unwrap_or(false)
                    }) {
                        walk(value, branch, root, path, out);
                    }
                    return;
                }
            }
            match object.get("items") {
                Some(Value::Array(tuple)) => {
                    for (index, (item, item_schema)) in items.iter().zip(tuple).enumerate() {
                        walk(
                            item,
                            item_schema,
                            root,
                            &join(path, &index.to_string()),
                            out,
                        );
                    }
                }
                Some(item_schema) => {
                    for (index, item) in items.iter().enumerate() {
                        walk(
                            item,
                            item_schema,
                            root,
                            &join(path, &index.to_string()),
                            out,
                        );
                    }
                }
                None => {}
            }
        }
        _ => {}
    }
}

/// The properties and `additionalProperties` an object schema allows,
/// `allOf` parts merged (a `#[serde(flatten)]` field).
fn object_shape<'a>(
    object: &'a Map<String, Value>,
    root: &'a Value,
) -> (Map<String, Value>, Option<&'a Value>, bool) {
    let mut properties = Map::new();
    let mut additional: Option<&Value> = None;
    let mut described = false;
    if let Some(Value::Object(own)) = object.get("properties") {
        described = true;
        for (k, v) in own {
            properties.insert(k.clone(), v.clone());
        }
    }
    if let Some(extra) = object.get("additionalProperties") {
        described = true;
        additional = Some(extra);
    }
    if let Some(Value::Array(parts)) = object.get("allOf") {
        for part in parts {
            if let Some(part) = resolve(part, root).as_object() {
                let (p, a, d) = object_shape(part, root);
                described |= d;
                properties.extend(p);
                if additional.is_none() {
                    additional = a;
                }
            }
        }
    }
    (properties, additional, described)
}

fn walk_object(
    fields: &Map<String, Value>,
    object: &Map<String, Value>,
    root: &Value,
    path: &str,
    out: &mut Vec<UnknownField>,
) {
    let (properties, additional, described) = object_shape(object, root);
    if !described {
        return; // an object the schema says nothing about: free-form.
    }
    for (key, field) in fields {
        if let Some(field_schema) = properties.get(key) {
            walk(field, field_schema, root, &join(path, key), out);
            continue;
        }
        match additional {
            Some(Value::Bool(false)) | None => {
                let mut allowed: Vec<String> = properties.keys().cloned().collect();
                allowed.sort();
                out.push(UnknownField {
                    path: join(path, key),
                    allowed,
                });
            }
            Some(extra) => walk(field, extra, root, &join(path, key), out),
        }
    }
}

/// Pick the branch of a `oneOf`/`anyOf` the object means, and walk that one.
///
/// A branch whose single-valued property (an internally tagged enum's tag:
/// `{"verb": {"enum": ["split"]}}`) equals the object's is the one; failing
/// that, the branch with the fewest unknown fields. Branches that cannot be
/// objects (`null`, a string) are never picked.
fn walk_alternatives(
    fields: &Map<String, Value>,
    alternatives: &[Value],
    root: &Value,
    path: &str,
    out: &mut Vec<UnknownField>,
) {
    let candidates: Vec<&Value> = alternatives
        .iter()
        .filter(|b| admits_object(b, root))
        .collect();
    if candidates.is_empty() {
        return;
    }
    let tagged: Vec<&Value> = candidates
        .iter()
        .copied()
        .filter(|b| tag_matches(fields, b, root))
        .collect();
    let pool = if tagged.is_empty() {
        candidates
    } else {
        tagged
    };
    let mut best: Option<Vec<UnknownField>> = None;
    for branch in pool {
        let mut found = Vec::new();
        walk(
            &Value::Object(fields.clone()),
            branch,
            root,
            path,
            &mut found,
        );
        if found.is_empty() {
            return;
        }
        if best.as_ref().is_none_or(|b| found.len() < b.len()) {
            best = Some(found);
        }
    }
    out.extend(best.unwrap_or_default());
}

fn tag_matches(fields: &Map<String, Value>, branch: &Value, root: &Value) -> bool {
    let Some(object) = resolve(branch, root).as_object() else {
        return false;
    };
    let (properties, _, _) = object_shape(object, root);
    properties.iter().any(|(key, schema)| {
        let schema = resolve(schema, root);
        let single = match (schema.get("enum"), schema.get("const")) {
            (Some(Value::Array(values)), _) if values.len() == 1 => Some(&values[0]),
            (_, Some(value)) => Some(value),
            _ => None,
        };
        match (single, fields.get(key)) {
            (Some(expected), Some(actual)) => expected == actual,
            _ => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, schemars::JsonSchema)]
    #[allow(dead_code)]
    struct Target {
        id: Option<u64>,
        role: Option<String>,
    }

    #[derive(Debug, Deserialize, schemars::JsonSchema)]
    #[serde(tag = "verb", rename_all = "kebab-case")]
    #[allow(dead_code)]
    enum Verb {
        Close { target: Target },
        Split { target: Target, after: bool },
        Restore,
    }

    #[derive(Debug, Deserialize, schemars::JsonSchema)]
    #[allow(dead_code)]
    struct Args {
        app_id: String,
        target: Option<Target>,
        verbs: Vec<Verb>,
        view_state: serde_json::Value,
        tags: std::collections::BTreeMap<String, Target>,
    }

    fn schema() -> Value {
        serde_json::to_value(schemars::schema_for!(Args)).unwrap()
    }

    #[test]
    fn a_clean_argument_passes_and_free_form_json_is_free() {
        let value = serde_json::json!({
            "app_id": "impress",
            "target": {"id": 7},
            "verbs": [{"verb": "close", "target": {"role": "detail"}}, {"verb": "restore"}],
            "view_state": {"anything": {"goes": true}},
            "tags": {"a": {"id": 1}},
        });
        assert!(unknown_fields(&value, &schema()).is_empty());
    }

    #[test]
    fn a_typo_is_named_with_its_path_at_any_depth() {
        let value = serde_json::json!({
            "app_id": "impress",
            "appid": "x",
            "target": {"ref": "id", "tile": 7},
            "verbs": [{"verb": "split", "target": {"id": 1}, "after": true, "neww": {}}],
            "view_state": null,
            "tags": {"a": {"idd": 1}},
        });
        let paths: Vec<String> = unknown_fields(&value, &schema())
            .into_iter()
            .map(|f| f.path)
            .collect();
        assert_eq!(
            paths,
            [
                "appid",
                "tags.a.idd",
                "target.ref",
                "target.tile",
                "verbs.0.neww"
            ]
        );
    }

    #[test]
    fn the_refusal_is_invalid_argument_and_says_what_the_object_takes() {
        let value = serde_json::json!({"app_id": "x", "target": {"ref": "id"}, "verbs": [],
            "view_state": null, "tags": {}});
        let refusal = check(&value, &schema()).unwrap_err();
        assert_eq!(refusal.code, crate::refusal::codes::INVALID_ARGUMENT);
        assert!(
            refusal
                .message
                .contains("unknown field 'ref' in 'target' (it takes: id, role)"),
            "{}",
            refusal.message
        );
    }

    #[test]
    fn a_strict_parse_refuses_a_typo_and_a_type_error_alike() {
        let bad = from_value::<Target>(serde_json::json!({"idd": 1})).unwrap_err();
        assert!(bad.message.contains("'idd'"), "{}", bad.message);
        let wrong = from_value::<Target>(serde_json::json!({"id": "seven"})).unwrap_err();
        assert_eq!(wrong.code, "invalid-argument");
        let ok: Target = from_value(serde_json::json!({"id": 7})).unwrap();
        assert_eq!(ok.id, Some(7));
    }

    #[test]
    fn a_refused_argument_is_an_envelope_with_the_wire_version() {
        let value = refusal_value(&Refusal::invalid_argument("no"));
        assert_eq!(value["ok"], false);
        assert_eq!(value["code"], "invalid-argument");
        assert_eq!(value["wire_version"], crate::wire::WIRE_VERSION);
    }

    /// A method that takes no arguments refuses one, rather than reading its
    /// empty schema as free-form.
    #[test]
    fn a_method_with_no_arguments_refuses_one() {
        let schema = serde_json::json!({ "type": "object", "title": "NoArgs" });
        #[derive(serde::Deserialize)]
        struct NoArgs {}
        let refused = args::<NoArgs>("x_y", serde_json::json!({ "zzz": 1 }), &schema)
            .err()
            .unwrap();
        assert_eq!(refused.code, "invalid-argument");
        assert!(refused.message.contains("'zzz'"), "{}", refused.message);
        assert!(args::<NoArgs>("x_y", serde_json::json!({}), &schema).is_ok());
    }

    /// An enum root (a `oneOf`) is not "no arguments": it is walked.
    #[test]
    fn an_enum_root_is_not_read_as_taking_no_arguments() {
        let schema = serde_json::json!({ "oneOf": [
            { "type": "object", "required": ["verb"],
              "properties": { "verb": { "enum": ["focus"] }, "target": {} } }
        ] });
        let ok: Result<Value, _> = args(
            "x_y",
            serde_json::json!({ "verb": "focus", "target": 1 }),
            &schema,
        );
        assert!(ok.is_ok(), "{ok:?}");
    }
}
