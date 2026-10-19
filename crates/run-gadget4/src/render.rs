//! `gadget4-adapt render`: `params.json` -> GADGET-4's `param.txt`, validated
//! before anything is written (R-D9.1: GADGET-4 refuses a whole run for one
//! unknown tag, so the adapter says so first, with a `2xx` code).

use crate::echo::Echo;
use crate::error::{Problem, BAD_VALUE, MISSING_PARAMETER, TYPE_MISMATCH, UNKNOWN_PARAMETER};
use crate::schema::property_kind;
use crate::table::{self, split_qualified, Kind};
use crate::value;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// One parameter as read from a params.json: qualified name, value, source.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub value: Value,
    pub source: Option<String>,
}

/// Read a `params.json`: the convention's `{name: {value, source}}` or a bare
/// `{name: value}`.
pub fn entries(params: &Value) -> Result<Vec<Entry>, Vec<Problem>> {
    let Some(o) = params.as_object() else {
        return Err(vec![Problem::new(
            TYPE_MISMATCH,
            "params.json must be an object",
        )]);
    };
    Ok(o.iter()
        .map(|(k, v)| match v.as_object() {
            Some(m) if m.contains_key("value") => Entry {
                name: k.clone(),
                value: m["value"].clone(),
                source: m.get("source").and_then(Value::as_str).map(str::to_string),
            },
            _ => Entry {
                name: k.clone(),
                value: v.clone(),
                source: None,
            },
        })
        .collect())
}

/// What a name is, from the schema if there is one, else from the built-in table.
struct Resolved {
    kind: Kind,
    order: (usize, u32),
    prop: Option<Value>,
}

fn resolve(q: &str, schema: Option<&Value>) -> Result<Resolved, Problem> {
    let Some((group, native)) = split_qualified(q) else {
        return Err(Problem::new(
            UNKNOWN_PARAMETER,
            format!("`{q}` is not a `group.Tag` name"),
        ));
    };
    let order = table::lookup(native)
        .map(|r| r.order)
        .unwrap_or((usize::MAX, 0));
    if let Some(s) = schema {
        let prop = s.pointer(&format!(
            "/properties/{}",
            q.replace('~', "~0").replace('/', "~1")
        ));
        return match prop {
            Some(p) => match property_kind(p) {
                Some(kind) => Ok(Resolved {
                    kind,
                    order,
                    prop: Some(p.clone()),
                }),
                None => Err(Problem::new(
                    TYPE_MISMATCH,
                    format!("`{q}`: the schema gives it a type GADGET-4 cannot take"),
                )),
            },
            None => Err(Problem::new(
                UNKNOWN_PARAMETER,
                format!(
                    "`{q}` is not in the schema; GADGET-4 refuses the whole run for an unknown tag"
                ),
            )),
        };
    }
    match table::lookup(native) {
        Some(r) if r.def.group == group => Ok(Resolved {
            kind: r.def.kind,
            order,
            prop: None,
        }),
        Some(r) => Err(Problem::new(
            UNKNOWN_PARAMETER,
            format!("`{q}`: `{native}` belongs to group `{}`", r.def.group),
        )),
        None => Err(Problem::new(
            UNKNOWN_PARAMETER,
            format!(
                "`{q}`: GADGET-4 has no tag `{native}` (and no schema was given to say otherwise)"
            ),
        )),
    }
}

fn check_constraints(q: &str, p: &Value, v: &Value) -> Option<Problem> {
    if let Some(en) = p.get("enum").and_then(Value::as_array) {
        if !en.contains(v) {
            return Some(Problem::new(
                BAD_VALUE,
                format!("`{q}` = {v} is not one of {}", Value::Array(en.clone())),
            ));
        }
    }
    if let Some(x) = v.as_f64() {
        if let Some(m) = p.get("minimum").and_then(Value::as_f64) {
            if x < m {
                return Some(Problem::new(BAD_VALUE, format!("`{q}` = {x} is below {m}")));
            }
        }
        if let Some(m) = p.get("maximum").and_then(Value::as_f64) {
            if x > m {
                return Some(Problem::new(BAD_VALUE, format!("`{q}` = {x} is above {m}")));
            }
        }
    }
    None
}

/// Validate and render. `Err` carries every problem found, not the first.
pub fn render(params: &Value, schema: Option<&Value>) -> Result<String, Vec<Problem>> {
    let entries = entries(params)?;
    let mut problems = Vec::new();
    let mut lines: Vec<((usize, u32), String, String)> = Vec::new();
    for e in &entries {
        let r = match resolve(&e.name, schema) {
            Ok(r) => r,
            Err(p) => {
                problems.push(p);
                continue;
            }
        };
        let Some((_, native)) = split_qualified(&e.name) else {
            continue;
        };
        if let Some(p) = &r.prop {
            if let Some(pr) = check_constraints(&e.name, p, &e.value) {
                problems.push(pr);
            }
        }
        match value::to_text(r.kind, &e.value) {
            Ok(text) => lines.push((r.order, native.to_string(), text)),
            Err(m) => problems.push(Problem::new(
                if m.starts_with("expected") {
                    TYPE_MISMATCH
                } else {
                    BAD_VALUE
                },
                format!("`{}`: {m}", e.name),
            )),
        }
    }
    if let Some(req) = schema
        .and_then(|s| s.get("required"))
        .and_then(Value::as_array)
    {
        for q in req.iter().filter_map(Value::as_str) {
            if !entries.iter().any(|e| e.name == q) {
                problems.push(Problem::new(
                    MISSING_PARAMETER,
                    format!(
                        "`{q}` is required; GADGET-4 refuses a parameter file that misses a tag"
                    ),
                ));
            }
        }
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    lines.sort();
    let mut out = String::new();
    for (_, name, text) in lines {
        out.push_str(&format!("{name:<36} {text}\n"));
    }
    Ok(out)
}

/// An `Echo` (or any `Tag value` text) as a convention `params.json` object,
/// every entry with `source`. Tags are typed by the table, else inferred.
pub fn params_from_echo(echo: &Echo, source: &str) -> Result<Value, Vec<Problem>> {
    let mut m: Map<String, Value> = Map::new();
    let mut problems = Vec::new();
    for (tag, text) in &echo.params {
        let (q, kind) = match table::lookup(tag) {
            Some(r) => (r.qualified(), r.def.kind),
            None => (table::qualify("other", tag), value::infer_kind(text)),
        };
        if m.contains_key(&q) {
            continue;
        }
        match value::from_text(kind, text) {
            Ok(v) => {
                m.insert(q, json!({"value": v, "source": source}));
            }
            Err(e) => problems.push(Problem::new(TYPE_MISMATCH, format!("{tag}: {e}"))),
        }
    }
    if problems.is_empty() {
        Ok(Value::Object(m))
    } else {
        Err(problems)
    }
}

/// Sorted `name -> value` of a params object, for comparisons.
pub fn values(params: &Value) -> BTreeMap<String, Value> {
    entries(params)
        .unwrap_or_default()
        .into_iter()
        .map(|e| (e.name, e.value))
        .collect()
}
