//! Canonical JSON, spec normalisation and digests.

use serde_json::{Map, Value};

/// `value` with every object's keys sorted, recursively.
pub fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = Map::new();
            for key in keys {
                out.insert(key.clone(), canonical(&map[key]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

/// The canonical JSON text of `value`: keys sorted, no whitespace.
pub fn canonical_text(value: &Value) -> String {
    fn write(value: &Value, out: &mut String) {
        match value {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (i, key) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String((*key).clone()).to_string());
                    out.push(':');
                    write(&map[*key], out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut out = String::new();
    write(value, &mut out);
    out
}

/// A spec in the form the surface store keeps it: parsed as a
/// `SurfaceSpec` and serialized again, so a shipped file and the stored
/// copy compare equal when they mean the same surface. A value that does not
/// parse is kept as it is.
pub fn normalize_spec(spec: &Value) -> Value {
    match serde_json::from_value::<impress_surface::SurfaceSpec>(spec.clone()) {
        Ok(parsed) => canonical(&serde_json::to_value(parsed).unwrap_or_else(|_| spec.clone())),
        Err(_) => canonical(spec),
    }
}

/// `sha256:<hex>` of `bytes`.
pub use koine_package_format::sha256;

/// The digest of a spec: of its normalised canonical text.
pub fn spec_digest(spec: &Value) -> String {
    sha256(canonical_text(&normalize_spec(spec)).as_bytes())
}

/// One change between two JSON values, at a JSON pointer.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub pointer: String,
    /// `added`, `removed` or `changed`.
    pub op: &'static str,
    pub before: Option<Value>,
    pub after: Option<Value>,
}

fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// The changes from `before` to `after`, as JSON pointers, in document order.
pub fn diff(before: &Value, after: &Value) -> Vec<Change> {
    fn walk(at: String, before: &Value, after: &Value, out: &mut Vec<Change>) {
        match (before, after) {
            (Value::Object(b), Value::Object(a)) => {
                let mut keys: Vec<&String> = b.keys().chain(a.keys()).collect();
                keys.sort();
                keys.dedup();
                for key in keys {
                    let child = format!("{at}/{}", escape(key));
                    match (b.get(key), a.get(key)) {
                        (Some(bv), Some(av)) => walk(child, bv, av, out),
                        (Some(bv), None) => out.push(Change {
                            pointer: child,
                            op: "removed",
                            before: Some(bv.clone()),
                            after: None,
                        }),
                        (None, Some(av)) => out.push(Change {
                            pointer: child,
                            op: "added",
                            before: None,
                            after: Some(av.clone()),
                        }),
                        (None, None) => {}
                    }
                }
            }
            (Value::Array(b), Value::Array(a)) if b.len() == a.len() => {
                for (i, (bv, av)) in b.iter().zip(a).enumerate() {
                    walk(format!("{at}/{i}"), bv, av, out);
                }
            }
            _ if before == after => {}
            _ => out.push(Change {
                pointer: if at.is_empty() { "/".into() } else { at },
                op: "changed",
                before: Some(before.clone()),
                after: Some(after.clone()),
            }),
        }
    }
    let mut out = Vec::new();
    walk(String::new(), before, after, &mut out);
    out
}

/// A value shown in a diff: its JSON text, cut at `max` characters.
pub fn shown(value: &Option<Value>, max: usize) -> Option<String> {
    value.as_ref().map(|v| {
        let text = v.to_string();
        if text.chars().count() > max {
            let cut: String = text.chars().take(max).collect();
            format!("{cut}…")
        } else {
            text
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_text_sorts_keys_at_every_level() {
        let a = json!({"b": 1, "a": {"d": [1, {"z": 0, "y": 1}], "c": null}});
        assert_eq!(
            canonical_text(&a),
            r#"{"a":{"c":null,"d":[1,{"y":1,"z":0}]},"b":1}"#
        );
    }

    #[test]
    fn a_diff_names_each_change_by_pointer() {
        let before = json!({"name": "A", "root": {"column": [{"text": "x"}]}, "gone": 1});
        let after = json!({"name": "B", "root": {"column": [{"text": "x"}]}, "new/key": 2});
        let changes = diff(&before, &after);
        let summary: Vec<(String, &str)> =
            changes.iter().map(|c| (c.pointer.clone(), c.op)).collect();
        assert_eq!(
            summary,
            vec![
                ("/gone".to_string(), "removed"),
                ("/name".to_string(), "changed"),
                ("/new~1key".to_string(), "added"),
            ]
        );
        assert_eq!(shown(&Some(json!("x".repeat(10))), 4).unwrap(), "\"xxx…");
    }

    #[test]
    fn a_digest_is_sha256_of_the_normalised_spec() {
        let spec = json!({"surface": "1.0", "name": "N", "state": {}, "sources": {},
                          "root": {"column": [{"text": "hi"}]}});
        let reordered = json!({"root": {"column": [{"text": "hi"}]}, "name": "N",
                               "sources": {}, "state": {}, "surface": "1.0"});
        assert_eq!(spec_digest(&spec), spec_digest(&reordered));
        assert!(spec_digest(&spec).starts_with("sha256:"));
        assert_eq!(spec_digest(&spec).len(), "sha256:".len() + 64);
    }
}
