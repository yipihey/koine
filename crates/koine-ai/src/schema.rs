//! Schema lowering (docs/plan-koine-local-models.md LM-D3).
//!
//! A verb's input schema is what `schemars` 0.8 writes: `definitions` at the
//! root, `$ref: "#/definitions/X"`, `allOf` wrapping a `$ref` to attach a
//! description, `anyOf`/`oneOf` for options and enums. Each consumer wants a
//! narrower dialect, so [`lower`] prepares the schema for one [`Profile`]:
//!
//! - every profile inlines local `$ref`s (a cycle is refused), merges a
//!   one-element `allOf` into its parent (the outer description wins), and
//!   drops `$schema`, `definitions`, `$defs`, `default`, `examples`,
//!   `readOnly`, `writeOnly` and `deprecated`;
//! - [`Profile::AppleGuided`] is the strict dialect Foundation Models'
//!   `GenerationSchema` decodes: on every object `additionalProperties:
//!   false`, `required`, `x-order` and a `title`;
//! - [`Profile::LlamaGrammar`] keeps everything llama.cpp's
//!   schema-to-grammar conversion reads and refuses what it cannot;
//! - [`Profile::OpenAiStrict`] is OpenAI's structured-output strict mode:
//!   every property required, optional ones nullable.
//!
//! Nothing that changes the shape (properties, required, items, enum, type)
//! is dropped silently (vendor `x-*` annotations such as `x-private` are not
//! shape and go from the strict profiles): what a profile cannot express is [`Unsupported`],
//! with the path that failed. Value checks (`format`, `pattern`, length
//! bounds, `uniqueItems`) are not shape; Apple's profile drops them because
//! Koine validates arguments itself when the verb runs.

use std::fmt;

use serde_json::{json, Map, Value};

/// The consumer a schema is prepared for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Foundation Models `GenerationSchema`.
    AppleGuided,
    /// llama.cpp `json-schema-to-grammar`.
    LlamaGrammar,
    /// OpenAI structured outputs, strict mode.
    OpenAiStrict,
}

/// A schema a profile cannot express.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    /// Where, dotted from the root: `args.filters.0`. Array items are `0`,
    /// `anyOf`/`oneOf` branches their index. Empty is the root.
    pub path: String,
    /// What cannot be expressed.
    pub reason: String,
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = if self.path.is_empty() {
            "(root)"
        } else {
            &self.path
        };
        write!(f, "unsupported schema at {path}: {}", self.reason)
    }
}

impl std::error::Error for Unsupported {}

type R<T> = Result<T, Unsupported>;

fn unsupported<T>(path: &str, reason: impl Into<String>) -> R<T> {
    Err(Unsupported {
        path: path.to_string(),
        reason: reason.into(),
    })
}

fn join(path: &str, segment: impl fmt::Display) -> String {
    if path.is_empty() {
        segment.to_string()
    } else {
        format!("{path}.{segment}")
    }
}

const DROPPED: &[&str] = &[
    "$schema",
    "definitions",
    "$defs",
    "default",
    "examples",
    "readOnly",
    "writeOnly",
    "deprecated",
];

/// llama.cpp's conversion has no rule for these.
const LLAMA_REFUSED: &[&str] = &[
    "patternProperties",
    "if",
    "then",
    "else",
    "dependentSchemas",
    "not",
];

/// Prepare `schema` for `profile`. `title` names the root (Apple needs one).
pub fn lower(schema: &Value, title: &str, profile: Profile) -> Result<Value, Unsupported> {
    let mut stack = Vec::new();
    let inlined = inline(schema, schema, &mut stack, "", profile)?;
    if profile == Profile::LlamaGrammar {
        return Ok(inlined);
    }
    let (lowered, nullable) = strict(&inlined, title, "", profile, true)?;
    if nullable {
        return unsupported("", "a nullable root");
    }
    if profile == Profile::OpenAiStrict && lowered.get("type") != Some(&json!("object")) {
        return unsupported("", "OpenAI strict mode needs an object at the root");
    }
    Ok(lowered)
}

// ---------------------------------------------------------------------------
// Pass 1: inline references, merge single `allOf`, drop the common keywords.
// ---------------------------------------------------------------------------

fn inline(
    node: &Value,
    root: &Value,
    stack: &mut Vec<String>,
    path: &str,
    profile: Profile,
) -> R<Value> {
    let Value::Object(map) = node else {
        return Ok(node.clone());
    };
    if profile == Profile::LlamaGrammar {
        if let Some(k) = LLAMA_REFUSED.iter().find(|k| map.contains_key(**k)) {
            return unsupported(
                path,
                format!("`{k}` is not supported by llama.cpp's grammar conversion"),
            );
        }
    }
    let mut out = Map::new();
    let merge_base = |base: Value, out: &mut Map<String, Value>| {
        if let Value::Object(b) = base {
            out.extend(b);
        }
    };
    if let Some(reference) = map.get("$ref") {
        let Some(reference) = reference.as_str() else {
            return unsupported(path, "`$ref` is not a string");
        };
        if stack.iter().any(|r| r == reference) {
            return unsupported(path, format!("recursive reference {reference}"));
        }
        let target = reference
            .strip_prefix('#')
            .and_then(|pointer| root.pointer(pointer));
        let Some(target) = target else {
            return unsupported(
                path,
                format!("unresolved or non-local reference {reference}"),
            );
        };
        stack.push(reference.to_string());
        let base = inline(target, root, stack, path, profile);
        stack.pop();
        merge_base(base?, &mut out);
    }
    let single_all_of = matches!(map.get("allOf"), Some(Value::Array(a)) if a.len() == 1);
    if single_all_of {
        let Some(Value::Array(a)) = map.get("allOf") else {
            unreachable!()
        };
        let base = inline(&a[0], root, stack, path, profile)?;
        merge_base(base, &mut out);
    }
    for (key, value) in map {
        if key == "$ref" || (key == "allOf" && single_all_of) || DROPPED.contains(&key.as_str()) {
            continue;
        }
        let lowered = match key.as_str() {
            "properties" | "patternProperties" | "dependentSchemas" => match value {
                Value::Object(children) => {
                    let mut inlined = Map::new();
                    for (name, child) in children {
                        inlined.insert(
                            name.clone(),
                            inline(child, root, stack, &join(path, name), profile)?,
                        );
                    }
                    Value::Object(inlined)
                }
                other => other.clone(),
            },
            "items" => match value {
                Value::Array(list) => inline_list(list, root, stack, path, profile)?,
                other => inline(other, root, stack, &join(path, 0), profile)?,
            },
            "prefixItems" | "anyOf" | "oneOf" | "allOf" => match value {
                Value::Array(list) => inline_list(list, root, stack, path, profile)?,
                other => other.clone(),
            },
            "additionalProperties"
            | "not"
            | "if"
            | "then"
            | "else"
            | "contains"
            | "propertyNames" => inline(value, root, stack, path, profile)?,
            _ => value.clone(),
        };
        out.insert(key.clone(), lowered);
    }
    Ok(Value::Object(out))
}

fn inline_list(
    list: &[Value],
    root: &Value,
    stack: &mut Vec<String>,
    path: &str,
    profile: Profile,
) -> R<Value> {
    list.iter()
        .enumerate()
        .map(|(i, child)| inline(child, root, stack, &join(path, i), profile))
        .collect::<R<Vec<_>>>()
        .map(Value::Array)
}

// ---------------------------------------------------------------------------
// Pass 2 (Apple, OpenAI): the strict dialects.
// ---------------------------------------------------------------------------

fn is_null_schema(v: &Value) -> bool {
    v.get("type") == Some(&json!("null")) && v.as_object().is_some_and(|m| m.len() <= 2)
}

/// `oneOf` of `{"const": "a"}` / `{"enum": ["a"]}` string branches.
fn unit_enum(branches: &[Value]) -> Option<Vec<Value>> {
    let mut values = Vec::new();
    for b in branches {
        let m = b.as_object()?;
        if m.keys().any(|k| {
            !matches!(
                k.as_str(),
                "const" | "enum" | "description" | "type" | "title"
            )
        }) {
            return None;
        }
        if m.get("type").is_some_and(|t| t != "string") {
            return None;
        }
        match (m.get("const"), m.get("enum")) {
            (Some(c), None) if c.is_string() => values.push(c.clone()),
            (None, Some(Value::Array(e))) if e.iter().all(Value::is_string) && !e.is_empty() => {
                values.extend(e.iter().cloned())
            }
            _ => return None,
        }
    }
    Some(values)
}

fn keep(map: &mut Map<String, Value>, out: &mut Map<String, Value>, keys: &[&str]) {
    for k in keys {
        if let Some(v) = map.remove(*k) {
            out.insert((*k).to_string(), v);
        }
    }
}

fn make_nullable(v: Value) -> Value {
    if let Value::Object(mut m) = v.clone() {
        if let (Some(Value::String(t)), false) = (m.get("type"), m.contains_key("anyOf")) {
            let t = t.clone();
            m.insert("type".into(), json!([t, "null"]));
            if let Some(Value::Array(e)) = m.get_mut("enum") {
                e.push(Value::Null);
            }
            return Value::Object(m);
        }
    }
    json!({ "anyOf": [v, { "type": "null" }] })
}

/// Lower one node. The flag is whether the node accepted null, in which case
/// the returned schema is the non-null one and the parent decides what that
/// means for `required`.
fn strict(
    node: &Value,
    name: &str,
    path: &str,
    profile: Profile,
    is_root: bool,
) -> R<(Value, bool)> {
    let apple = profile == Profile::AppleGuided;
    let mut map = match node {
        Value::Object(m) => m.clone(),
        Value::Bool(true) => return unsupported(path, "any value"),
        Value::Bool(false) => return unsupported(path, "a schema nothing matches"),
        _ => return unsupported(path, "not a schema"),
    };
    let mut nullable = false;
    if map.remove("nullable") == Some(Value::Bool(true)) {
        nullable = true;
    }
    if let Some(Value::Array(types)) = map.get("type").cloned() {
        let rest: Vec<&Value> = types.iter().filter(|t| *t != "null").collect();
        if rest.len() < types.len() {
            nullable = true;
        }
        match rest.len() {
            1 => {
                map.insert("type".into(), rest[0].clone());
            }
            0 => return unsupported(path, "a null-only type"),
            _ => return unsupported(path, "a union of types"),
        }
    }
    if map.get("type") == Some(&json!("null")) {
        return unsupported(path, "a null-only type");
    }
    if let Some(Value::Array(e)) = map.get_mut("enum") {
        if e.iter().any(Value::is_null) {
            e.retain(|v| !v.is_null());
            nullable = true;
        }
    }
    if map.contains_key("allOf") {
        return unsupported(path, "`allOf` with several branches");
    }

    // anyOf / oneOf
    let combiner = ["anyOf", "oneOf"]
        .into_iter()
        .find(|k| map.contains_key(*k));
    if let Some(combiner) = combiner {
        if map.contains_key("anyOf") && map.contains_key("oneOf") {
            return unsupported(path, "`anyOf` beside `oneOf`");
        }
        if let Some(k) = map
            .keys()
            .find(|k| !matches!(k.as_str(), "description" | "title") && *k != combiner)
        {
            return unsupported(path, format!("`{k}` beside `{combiner}`"));
        }
        let Some(Value::Array(branches)) = map.remove(combiner) else {
            return unsupported(path, format!("`{combiner}` is not a list"));
        };
        let description = map.remove("description");
        let (nulls, rest): (Vec<Value>, Vec<Value>) =
            branches.into_iter().partition(is_null_schema);
        if !nulls.is_empty() {
            nullable = true;
        }
        let with_description = |mut v: Value| {
            if let (Some(d), Value::Object(m)) = (&description, &mut v) {
                m.insert("description".into(), d.clone());
            }
            v
        };
        if rest.is_empty() {
            return unsupported(path, "a null-only type");
        }
        if rest.len() == 1 {
            let (v, n) = strict(&rest[0], name, path, profile, is_root)?;
            return Ok((with_description(v), nullable || n));
        }
        if let Some(values) = unit_enum(&rest) {
            return Ok((
                with_description(json!({ "type": "string", "enum": values })),
                nullable,
            ));
        }
        let mut lowered = Vec::new();
        for (i, branch) in rest.iter().enumerate() {
            let (v, n) = strict(
                branch,
                &format!("{name}_{i}"),
                &join(path, i),
                profile,
                false,
            )?;
            if n {
                return unsupported(&join(path, i), "a nullable branch");
            }
            lowered.push(v);
        }
        let mut any = Map::new();
        if apple {
            // `GenerationSchema` refuses an anyOf without a title.
            any.insert("title".into(), json!(name));
        }
        any.insert("anyOf".into(), Value::Array(lowered));
        return Ok((with_description(Value::Object(any)), nullable));
    }

    // const
    if let Some(c) = map.remove("const") {
        if !c.is_string() {
            return unsupported(path, "a `const` that is not a string");
        }
        map.insert("type".into(), json!("string"));
        map.insert("enum".into(), json!([c]));
    }

    let kind = match map.get("type") {
        Some(Value::String(t)) => t.clone(),
        Some(_) => return unsupported(path, "`type` is not a string"),
        None if map.contains_key("properties") => "object".into(),
        None if map.contains_key("items") => "array".into(),
        None => match map.get("enum") {
            Some(Value::Array(e)) if !e.is_empty() && e.iter().all(Value::is_string) => {
                "string".into()
            }
            Some(Value::Array(e)) if !e.is_empty() && e.iter().all(Value::is_i64) => {
                "integer".into()
            }
            _ => return unsupported(path, "any value"),
        },
    };
    map.remove("type");
    // Vendor annotations (`x-private`, ...) describe the verb, not the shape.
    map.retain(|k, _| !k.starts_with("x-"));
    map.remove("title");
    map.remove("$comment");
    let mut out = Map::new();
    match kind.as_str() {
        "object" => {
            let props = map.remove("properties");
            match map.remove("additionalProperties") {
                None | Some(Value::Bool(false)) => {}
                Some(_) => return unsupported(path, "free-form object"),
            }
            let props = match props {
                Some(Value::Object(p)) => p,
                Some(_) => return unsupported(path, "`properties` is not an object"),
                None if is_root => Map::new(),
                None => return unsupported(path, "free-form object"),
            };
            let required: Vec<String> = match map.remove("required") {
                Some(Value::Array(r)) => r
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect(),
                _ => Vec::new(),
            };
            let mut lowered = Map::new();
            let mut order = Vec::new();
            let mut required_out = Vec::new();
            for (key, child) in &props {
                let (v, child_nullable) = strict(child, key, &join(path, key), profile, false)?;
                let optional = !required.contains(key);
                order.push(json!(key));
                if apple {
                    if !optional && !child_nullable {
                        required_out.push(json!(key));
                    }
                    lowered.insert(key.clone(), v);
                } else {
                    required_out.push(json!(key));
                    lowered.insert(
                        key.clone(),
                        if optional || child_nullable {
                            make_nullable(v)
                        } else {
                            v
                        },
                    );
                }
            }
            out.insert("type".into(), json!("object"));
            if apple {
                out.insert("title".into(), json!(name));
            }
            out.insert("properties".into(), Value::Object(lowered));
            out.insert("required".into(), Value::Array(required_out));
            if apple {
                out.insert("x-order".into(), Value::Array(order));
            }
            out.insert("additionalProperties".into(), json!(false));
            keep(&mut map, &mut out, &["description"]);
        }
        "array" => {
            let items = match map.remove("items") {
                Some(Value::Array(_)) => return unsupported(path, "a tuple array"),
                Some(items) => items,
                None => return unsupported(&join(path, 0), "any value"),
            };
            let (v, n) = strict(
                &items,
                &format!("{name}_item"),
                &join(path, 0),
                profile,
                false,
            )?;
            if n {
                return unsupported(&join(path, 0), "a nullable array item");
            }
            map.remove("uniqueItems");
            out.insert("type".into(), json!("array"));
            out.insert("items".into(), v);
            keep(&mut map, &mut out, &["minItems", "maxItems", "description"]);
        }
        "string" => {
            out.insert("type".into(), json!("string"));
            if apple {
                for k in ["format", "pattern", "minLength", "maxLength"] {
                    map.remove(k);
                }
                keep(&mut map, &mut out, &["enum", "description"]);
            } else {
                keep(
                    &mut map,
                    &mut out,
                    &[
                        "enum",
                        "description",
                        "format",
                        "pattern",
                        "minLength",
                        "maxLength",
                    ],
                );
            }
        }
        "integer" | "number" => {
            out.insert("type".into(), json!(kind));
            map.remove("format");
            if apple {
                keep(
                    &mut map,
                    &mut out,
                    &["minimum", "maximum", "enum", "description"],
                );
            } else {
                keep(
                    &mut map,
                    &mut out,
                    &[
                        "minimum",
                        "maximum",
                        "exclusiveMinimum",
                        "exclusiveMaximum",
                        "multipleOf",
                        "enum",
                        "description",
                    ],
                );
            }
        }
        "boolean" => {
            out.insert("type".into(), json!("boolean"));
            keep(&mut map, &mut out, &["description"]);
        }
        other => return unsupported(path, format!("type `{other}`")),
    }
    if let Some(k) = map.keys().next() {
        return unsupported(path, format!("keyword `{k}`"));
    }
    Ok((Value::Object(out), nullable))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Profile; 3] = [
        Profile::AppleGuided,
        Profile::LlamaGrammar,
        Profile::OpenAiStrict,
    ];

    fn flat() -> Value {
        json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "Args",
            "type": "object",
            "required": ["city"],
            "properties": {
                "city": { "type": "string", "description": "Where", "minLength": 1 },
                "days": { "type": "integer", "format": "uint32", "minimum": 0.0, "default": 3 }
            }
        })
    }

    #[test]
    fn a_flat_object_with_an_optional_field() {
        assert_eq!(
            lower(&flat(), "Args", Profile::AppleGuided).unwrap(),
            json!({
                "type": "object", "title": "Args",
                "properties": {
                    "city": { "type": "string", "description": "Where" },
                    "days": { "type": "integer", "minimum": 0.0 }
                },
                "required": ["city"],
                "x-order": ["city", "days"],
                "additionalProperties": false
            })
        );
        assert_eq!(
            lower(&flat(), "Args", Profile::OpenAiStrict).unwrap(),
            json!({
                "type": "object",
                "properties": {
                    "city": { "type": "string", "description": "Where", "minLength": 1 },
                    "days": { "type": ["integer", "null"], "minimum": 0.0 }
                },
                "required": ["city", "days"],
                "additionalProperties": false
            })
        );
        assert_eq!(
            lower(&flat(), "Args", Profile::LlamaGrammar).unwrap(),
            json!({
                "title": "Args", "type": "object", "required": ["city"],
                "properties": {
                    "city": { "type": "string", "description": "Where", "minLength": 1 },
                    "days": { "type": "integer", "format": "uint32", "minimum": 0.0 }
                }
            })
        );
    }

    fn nested() -> Value {
        json!({
            "type": "object",
            "required": ["target"],
            "properties": {
                "target": { "description": "The target", "allOf": [{ "$ref": "#/definitions/Target" }] }
            },
            "definitions": {
                "Target": {
                    "description": "inner doc",
                    "type": "object",
                    "required": ["id"],
                    "properties": { "id": { "type": "string", "format": "uuid" } }
                }
            }
        })
    }

    #[test]
    fn a_nested_object_through_ref_and_all_of_keeps_the_outer_description() {
        assert_eq!(
            lower(&nested(), "Root", Profile::AppleGuided).unwrap(),
            json!({
                "type": "object", "title": "Root",
                "properties": { "target": {
                    "type": "object", "title": "target", "description": "The target",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"], "x-order": ["id"], "additionalProperties": false
                } },
                "required": ["target"], "x-order": ["target"], "additionalProperties": false
            })
        );
        let openai = lower(&nested(), "Root", Profile::OpenAiStrict).unwrap();
        assert_eq!(
            openai["properties"]["target"]["properties"]["id"],
            json!({ "type": "string", "format": "uuid" })
        );
        let llama = lower(&nested(), "Root", Profile::LlamaGrammar).unwrap();
        assert_eq!(llama["properties"]["target"]["description"], "The target");
        assert!(llama.get("definitions").is_none());
        assert_eq!(llama["properties"]["target"]["type"], "object");
    }

    #[test]
    fn option_string_in_both_spellings() {
        for spelling in [
            json!({ "type": ["string", "null"] }),
            json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] }),
            json!({ "type": "string", "nullable": true }),
        ] {
            let schema = json!({ "type": "object", "required": ["a"],
                "properties": { "a": spelling } });
            let apple = lower(&schema, "T", Profile::AppleGuided).unwrap();
            assert_eq!(apple["required"], json!([]), "{spelling}");
            assert_eq!(apple["properties"]["a"], json!({ "type": "string" }));
            let openai = lower(&schema, "T", Profile::OpenAiStrict).unwrap();
            assert_eq!(openai["required"], json!(["a"]));
            assert_eq!(
                openai["properties"]["a"],
                json!({ "type": ["string", "null"] })
            );
        }
        let kept =
            json!({ "type": "object", "properties": { "a": { "type": ["string", "null"] } } });
        assert_eq!(
            lower(&kept, "T", Profile::LlamaGrammar).unwrap()["properties"]["a"],
            json!({ "type": ["string", "null"] })
        );
    }

    #[test]
    fn a_unit_enum_becomes_one_string_enum() {
        let schema = json!({ "type": "object", "required": ["unit"], "properties": {
            "unit": { "description": "Which", "oneOf": [
                { "type": "string", "enum": ["c"] },
                { "const": "f" },
                { "description": "kelvin", "type": "string", "enum": ["k"] }
            ] } } });
        for p in [Profile::AppleGuided, Profile::OpenAiStrict] {
            assert_eq!(
                lower(&schema, "T", p).unwrap()["properties"]["unit"],
                json!({ "type": "string", "enum": ["c", "f", "k"], "description": "Which" })
            );
        }
        // Llama reads oneOf and const as written.
        assert_eq!(
            lower(&schema, "T", Profile::LlamaGrammar).unwrap()["properties"]["unit"]["oneOf"][1],
            json!({ "const": "f" })
        );
    }

    #[test]
    fn an_internally_tagged_enum_is_an_any_of_of_titled_objects() {
        let schema = json!({ "type": "object", "required": ["q"], "properties": { "q": {
            "oneOf": [
                { "type": "object", "required": ["kind", "text"], "properties": {
                    "kind": { "type": "string", "enum": ["text"] },
                    "text": { "type": "string" } } },
                { "type": "object", "required": ["kind"], "properties": {
                    "kind": { "type": "string", "enum": ["all"] },
                    "limit": { "type": "integer" } } }
            ] } } });
        assert_eq!(
            lower(&schema, "T", Profile::AppleGuided).unwrap()["properties"]["q"],
            json!({ "title": "q", "anyOf": [
                { "type": "object", "title": "q_0",
                  "properties": { "kind": { "type": "string", "enum": ["text"] },
                                  "text": { "type": "string" } },
                  "required": ["kind", "text"], "x-order": ["kind", "text"],
                  "additionalProperties": false },
                { "type": "object", "title": "q_1",
                  "properties": { "kind": { "type": "string", "enum": ["all"] },
                                  "limit": { "type": "integer" } },
                  "required": ["kind"], "x-order": ["kind", "limit"],
                  "additionalProperties": false }
            ] })
        );
        let openai = lower(&schema, "T", Profile::OpenAiStrict).unwrap();
        assert_eq!(
            openai["properties"]["q"]["anyOf"][1]["properties"]["limit"],
            json!({ "type": ["integer", "null"] })
        );
        assert_eq!(
            openai["properties"]["q"]["anyOf"][1]["required"],
            json!(["kind", "limit"])
        );
    }

    #[test]
    fn an_array_of_objects_titles_its_items() {
        let schema = json!({ "type": "object", "required": ["filters"], "properties": {
            "filters": { "type": "array", "minItems": 1, "maxItems": 5, "uniqueItems": true,
                "items": { "type": "object", "required": ["f"],
                           "properties": { "f": { "type": "string" } } } } } });
        assert_eq!(
            lower(&schema, "T", Profile::AppleGuided).unwrap()["properties"]["filters"],
            json!({ "type": "array", "minItems": 1, "maxItems": 5,
                "items": { "type": "object", "title": "filters_item",
                    "properties": { "f": { "type": "string" } }, "required": ["f"],
                    "x-order": ["f"], "additionalProperties": false } })
        );
    }

    #[test]
    fn a_parameterless_root() {
        let schema = json!({ "type": "object" });
        let empty_apple = json!({ "type": "object", "title": "Ping", "properties": {},
            "required": [], "x-order": [], "additionalProperties": false });
        assert_eq!(
            lower(&schema, "Ping", Profile::AppleGuided).unwrap(),
            empty_apple
        );
        assert_eq!(
            lower(&schema, "Ping", Profile::OpenAiStrict).unwrap(),
            json!({ "type": "object", "properties": {}, "required": [],
                "additionalProperties": false })
        );
        assert_eq!(
            lower(&schema, "Ping", Profile::LlamaGrammar).unwrap(),
            schema
        );
    }

    #[test]
    fn a_cycle_is_unsupported_with_its_path() {
        let schema = json!({ "type": "object", "required": ["tree"],
            "properties": { "tree": { "$ref": "#/definitions/Node" } },
            "definitions": { "Node": { "type": "object", "properties": {
                "children": { "type": "array", "items": { "$ref": "#/definitions/Node" } } } } } });
        for p in ALL {
            let err = lower(&schema, "T", p).unwrap_err();
            assert_eq!(err.path, "tree.children.0", "{p:?}");
            assert!(err.reason.contains("recursive"), "{err}");
        }
    }

    #[test]
    fn a_free_form_object_is_unsupported_where_the_profile_cannot_say_it() {
        let schema = json!({ "type": "object", "properties": {
            "a": { "type": "object", "properties": { "extra": { "type": "object" } } } } });
        for p in [Profile::AppleGuided, Profile::OpenAiStrict] {
            let err = lower(&schema, "T", p).unwrap_err();
            assert_eq!(err.path, "a.extra", "{p:?}");
            assert_eq!(err.reason, "free-form object");
        }
        assert!(lower(&schema, "T", Profile::LlamaGrammar).is_ok());
        let map = json!({ "type": "object", "properties": {
            "m": { "type": "object", "additionalProperties": { "type": "string" } } } });
        assert_eq!(
            lower(&map, "T", Profile::AppleGuided).unwrap_err().path,
            "m"
        );
        let any = json!({ "type": "object", "properties": { "v": true } });
        let err = lower(&any, "T", Profile::OpenAiStrict).unwrap_err();
        assert_eq!((err.path.as_str(), err.reason.as_str()), ("v", "any value"));
        let empty = json!({ "type": "object", "properties": { "v": {} } });
        assert_eq!(
            lower(&empty, "T", Profile::AppleGuided).unwrap_err().reason,
            "any value"
        );
    }

    #[test]
    fn llama_refuses_what_its_grammar_cannot_say() {
        for key in ["patternProperties", "if", "dependentSchemas", "not"] {
            let schema = json!({ "type": "object", "properties": {
                "a": { "type": "object", key: {} } } });
            let err = lower(&schema, "T", Profile::LlamaGrammar).unwrap_err();
            assert_eq!(err.path, "a");
            assert!(err.reason.contains(key));
        }
    }

    #[test]
    fn a_property_named_like_a_dropped_keyword_survives() {
        let schema = json!({ "type": "object", "required": ["default"],
            "properties": { "default": { "type": "string", "default": "x" } } });
        let out = lower(&schema, "T", Profile::AppleGuided).unwrap();
        assert_eq!(out["properties"]["default"], json!({ "type": "string" }));
    }

    #[test]
    fn lowering_is_deterministic_and_uses_defs_too() {
        let schema = json!({ "type": "object", "required": ["x"],
            "properties": { "x": { "$ref": "#/$defs/X" } },
            "$defs": { "X": { "type": "string", "enum": ["a", "b"] } } });
        for p in ALL {
            assert_eq!(lower(&schema, "T", p), lower(&schema, "T", p));
        }
        assert_eq!(
            lower(&schema, "T", Profile::AppleGuided).unwrap()["properties"]["x"],
            json!({ "type": "string", "enum": ["a", "b"] })
        );
    }

    #[test]
    fn vendor_annotations_leave_the_strict_profiles_only() {
        let schema = json!({ "type": "object", "required": ["k"],
            "properties": { "k": { "type": "string", "x-private": true } } });
        for p in [Profile::AppleGuided, Profile::OpenAiStrict] {
            let out = lower(&schema, "T", p).unwrap();
            assert_eq!(out["properties"]["k"], json!({ "type": "string" }));
        }
        let llama = lower(&schema, "T", Profile::LlamaGrammar).unwrap();
        assert_eq!(llama["properties"]["k"]["x-private"], true);
    }
}
