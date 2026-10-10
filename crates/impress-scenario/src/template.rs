//! `{{…}}` resolution for a scenario, reusing `impress_surface::template`
//! (docs/plan-self-reflective-layer.md § Scenarios: "Templates are the
//! surface's `{{…}}` resolver, reused, extended with `{{uuid}}` and
//! captures").
//!
//! Captures are supplied through the surface template's `state` root — a
//! scenario has no `param`/`source`/`event`/`item` context, so those roots
//! stay empty and any reference into them is simply `MissingPath`.
//! `{{uuid}}` is not a path into any root (it names a fresh value each
//! time it appears), so it is substituted in a text pre-pass before the
//! surface resolver ever sees the string.

use impress_surface::template::{resolve_value, Context};
use serde_json::Value;

/// Resolve every `{{…}}` in `value` against `captures` (exposed as the
/// `state` root), substituting a fresh `{{uuid}}` per occurrence first.
pub fn resolve(value: &Value, captures: &Value) -> Result<Value, String> {
    let nonce = loop {
        let candidate = format!("scenario-literal-{}-", uuid::Uuid::new_v4());
        if !value.to_string().contains(&candidate) && !captures.to_string().contains(&candidate) {
            break candidate;
        }
    };
    let mut literals = Vec::new();
    let protected = protect_literals(value, &nonce, &mut literals)?;
    let with_uuids = substitute_uuids(&protected);
    let empty = Value::Null;
    let ctx = Context::new(captures, &empty, &empty, &empty);
    let resolved = resolve_value(&with_uuids, &ctx).map_err(|e| e.to_string())?;
    Ok(restore_literals(resolved, &literals))
}

/// `{{!state.x}}` passes the literal `{{state.x}}` to a nested surface.
/// The escape also works inside serialized JSON and with `{{!uuid}}`.
fn protect_literals(
    value: &Value,
    nonce: &str,
    literals: &mut Vec<(String, String)>,
) -> Result<Value, String> {
    Ok(match value {
        Value::String(text) => {
            let mut rest = text.as_str();
            let mut out = String::new();
            while let Some(start) = rest.find("{{!") {
                out.push_str(&rest[..start]);
                let token = &rest[start + 3..];
                let end = token
                    .find("}}")
                    .ok_or("unterminated scenario literal escape")?;
                let placeholder = format!("{nonce}{}-end", literals.len());
                out.push_str(&placeholder);
                literals.push((placeholder, format!("{{{{{}}}}}", &token[..end])));
                rest = &token[end + 2..];
            }
            out.push_str(rest);
            Value::String(out)
        }
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|v| protect_literals(v, nonce, literals))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(values) => Value::Object(
            values
                .iter()
                .map(|(k, v)| Ok((k.clone(), protect_literals(v, nonce, literals)?)))
                .collect::<Result<_, String>>()?,
        ),
        value => value.clone(),
    })
}
fn restore_literals(value: Value, literals: &[(String, String)]) -> Value {
    match value {
        Value::String(mut text) => {
            for (placeholder, literal) in literals {
                text = text.replace(placeholder, literal);
            }
            Value::String(text)
        }
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|v| restore_literals(v, literals))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(k, v)| (k, restore_literals(v, literals)))
                .collect(),
        ),
        value => value,
    }
}

/// Encode recorded argument strings so replay does not reinterpret their
/// literal templates as scenario captures. Refuse text that cannot round-trip.
pub fn escape_literals(value: &Value) -> Result<Value, String> {
    fn walk(value: &Value) -> Value {
        match value {
            Value::String(text) => {
                let mut rest = text.as_str();
                let mut out = String::new();
                while let Some(start) = rest.find("{{") {
                    let tail = &rest[start + 2..];
                    let Some(end) = tail.find("}}") else {
                        break;
                    };
                    out.push_str(&rest[..start]);
                    out.push_str("{{!");
                    out.push_str(&tail[..end]);
                    out.push_str("}}");
                    rest = &tail[end + 2..];
                }
                out.push_str(rest);
                Value::String(out)
            }
            Value::Array(values) => Value::Array(values.iter().map(walk).collect()),
            Value::Object(values) => {
                Value::Object(values.iter().map(|(k, v)| (k.clone(), walk(v))).collect())
            }
            value => value.clone(),
        }
    }
    let escaped = walk(value);
    if resolve(&escaped, &Value::Null)? != *value {
        return Err("literal templates did not round-trip".into());
    }
    Ok(escaped)
}

/// Walk `value`, replacing every literal occurrence of `{{uuid}}` inside a
/// string with a freshly minted v4 UUID. Two occurrences in the same string
/// get two different values — a scenario that wants the same generated
/// value twice captures it once and references `{{state.<name>}}`
/// instead.
fn substitute_uuids(value: &Value) -> Value {
    match value {
        Value::String(s) => Value::String(replace_uuid_tokens(s)),
        Value::Array(items) => Value::Array(items.iter().map(substitute_uuids).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), substitute_uuids(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn replace_uuid_tokens(s: &str) -> String {
    const TOKEN: &str = "{{uuid}}";
    if !s.contains(TOKEN) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(idx) = rest.find(TOKEN) {
        out.push_str(&rest[..idx]);
        out.push_str(&uuid::Uuid::new_v4().to_string());
        rest = &rest[idx + TOKEN.len()..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn uuid_tokens_are_distinct_and_valid() {
        let resolved = resolve(&json!({"a": "{{uuid}}", "b": "{{uuid}}"}), &Value::Null).unwrap();
        let a = resolved["a"].as_str().unwrap();
        let b = resolved["b"].as_str().unwrap();
        assert_ne!(a, b);
        assert!(uuid::Uuid::parse_str(a).is_ok());
        assert!(uuid::Uuid::parse_str(b).is_ok());
    }

    #[test]
    fn captures_resolve_through_the_state_root() {
        let captures = json!({"name": "layout-42"});
        let resolved = resolve(&json!({"name": "{{state.name}}"}), &captures).unwrap();
        assert_eq!(resolved["name"], json!("layout-42"));
    }

    #[test]
    fn mixed_text_stringifies_the_capture() {
        let captures = json!({"n": 3});
        let resolved = resolve(&json!("count is {{state.n}}"), &captures).unwrap();
        assert_eq!(resolved, json!("count is 3"));
    }

    #[test]
    fn a_missing_capture_is_an_error() {
        assert!(resolve(&json!("{{state.missing}}"), &Value::Null).is_err());
    }
}
