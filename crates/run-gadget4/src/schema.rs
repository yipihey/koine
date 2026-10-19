//! `gadget4-adapt schema`: harvest the convention's parameter JSON Schema (R-D2.1)
//! from GADGET-4's own echo of its parameters.

use crate::echo::Echo;
use crate::error::{Problem, BAD_SCHEMA, TYPE_MISMATCH};
use crate::table::{self, qualify, Kind};
use crate::value;
use serde_json::{json, Map, Value};

/// The JSON Schema `type` for a kind.
pub fn json_type(k: Kind) -> &'static str {
    match k {
        Kind::Int => "integer",
        Kind::Float => "number",
        Kind::String | Kind::Path => "string",
    }
}

/// The kind a schema property describes (the inverse of what `harvest` writes).
pub fn property_kind(prop: &Value) -> Option<Kind> {
    match prop.get("type").and_then(Value::as_str)? {
        "integer" => Some(Kind::Int),
        "number" => Some(Kind::Float),
        "string" => Some(
            if matches!(
                prop.get("format").and_then(Value::as_str),
                Some("file" | "directory")
            ) {
                Kind::Path
            } else {
                Kind::String
            },
        ),
        _ => None,
    }
}

/// Build the schema from an echo. Known tags take the table's type, group,
/// mutability and unit; unknown ones get group `other`, `mutable: never` and a
/// type inferred from the value. `harvested_from` names the source in the
/// schema's description.
pub fn harvest(echo: &Echo, harvested_from: &str) -> Result<Value, Vec<Problem>> {
    let mut problems = Vec::new();
    let mut rows: Vec<(table::Resolved, Value, Kind, bool)> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (tag, text) in &echo.params {
        if !seen.insert(tag.clone()) {
            continue; // the first value stands; duplicates are reported by `Echo`
        }
        let (resolved, known) = match table::lookup(tag) {
            Some(r) => (r, true),
            None => {
                let kind = value::infer_kind(text);
                let def = table::ParamDef {
                    pattern: tag.clone(),
                    group: "other".into(),
                    kind,
                    mutable: "never".into(),
                    unit: String::new(),
                    advanced: true,
                    description: format!(
                        "Not in the adapter's table; harvested from the echo, type inferred from `{text}`"
                    ),
                };
                (
                    table::Resolved {
                        order: (usize::MAX, 0),
                        native: tag.clone(),
                        description: def.description.clone(),
                        def,
                    },
                    false,
                )
            }
        };
        let kind = resolved.def.kind;
        match value::from_text(kind, text) {
            Ok(v) => rows.push((resolved, v, kind, known)),
            Err(m) => problems.push(Problem::new(TYPE_MISMATCH, format!("{tag}: {m}"))),
        }
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    rows.sort_by(|a, b| (a.0.order, &a.0.native).cmp(&(b.0.order, &b.0.native)));

    let mut props = Map::new();
    let mut required = Vec::new();
    let mut unknown = Vec::new();
    for (r, v, kind, known) in &rows {
        let q = r.qualified();
        let mut p = Map::new();
        p.insert("type".into(), json!(json_type(*kind)));
        p.insert("description".into(), json!(r.description));
        p.insert("default".into(), v.clone());
        if *kind == Kind::Path {
            let dir = r.native == "OutputDir";
            p.insert(
                "format".into(),
                json!(if dir { "directory" } else { "file" }),
            );
        }
        if matches!(r.native.as_str(), "ComovingIntegrationOn" | "OutputListOn") {
            p.insert("enum".into(), json!([0, 1]));
        }
        let mut x = Map::new();
        x.insert("group".into(), json!(r.def.group));
        x.insert("mutable".into(), json!(r.def.mutable));
        if !r.def.unit.is_empty() {
            x.insert("unit".into(), json!(r.def.unit));
        }
        if r.def.advanced {
            x.insert("advanced".into(), json!(true));
        }
        p.insert("x-run".into(), Value::Object(x));
        props.insert(q.clone(), Value::Object(p));
        required.push(json!(q));
        if !known {
            unknown.push(r.native.clone());
        }
    }

    let used_groups: std::collections::BTreeSet<&str> =
        rows.iter().map(|r| r.0.def.group.as_str()).collect();
    let mut groups = Map::new();
    for (g, d) in table::group_descriptions() {
        if used_groups.contains(g) {
            groups.insert(g.into(), json!({"description": d}));
        }
    }

    let mut desc = format!(
        "GADGET-4 runtime parameters, harvested from {harvested_from}. \
         Every tag is required exactly once (GADGET-4 refuses a run that misses or repeats one) \
         and an unknown tag is an error. Names are `group.Tag`; the group is dropped when \
         rendering param.txt. Mutability is GADGET-4's own PARAM_FIXED (never) / PARAM_CHANGEABLE \
         (restart) flag; GADGET-4 has no live set, so nothing is `always`. Units: code_length = \
         units.UnitLength_in_cm cm, code_mass = units.UnitMass_in_g g, code_velocity = \
         units.UnitVelocity_in_cm_per_s cm/s, code_time = code_length/code_velocity (the scale \
         factor in comoving runs)."
    );
    if !unknown.is_empty() {
        desc.push_str(&format!(
            " Tags not in the adapter's table: {}.",
            unknown.join(", ")
        ));
    }

    let mut schema = Map::new();
    schema.insert(
        "$schema".into(),
        json!("https://json-schema.org/draft/2020-12/schema"),
    );
    schema.insert("title".into(), json!("GADGET-4 run parameters"));
    schema.insert("description".into(), json!(desc));
    schema.insert("type".into(), json!("object"));
    schema.insert("additionalProperties".into(), json!(false));
    schema.insert("required".into(), Value::Array(required));
    schema.insert("properties".into(), Value::Object(props));
    schema.insert(
        "x-run".into(),
        json!({
            "groups": Value::Object(groups),
            "metrics": [
                {"name": "wall_s", "unit": "s", "description": "wall-clock seconds since the adapter started the code"},
                {"name": "memory_largest_mb", "unit": "MBy", "description": "largest single allocation reported in output/memory.txt"}
            ]
        }),
    );
    let mut g = Map::new();
    g.insert("harvested_from".into(), json!(harvested_from));
    if let Some(v) = &echo.version {
        g.insert("version".into(), json!(v));
    }
    if let Some(c) = &echo.git_commit {
        g.insert("git_commit".into(), json!(c));
    }
    if !echo.config.is_empty() {
        g.insert("config".into(), json!(echo.config));
    }
    schema.insert("x-gadget4".into(), Value::Object(g));

    let schema = Value::Object(schema);
    let bad = run_convention::check_parameter_schema(&schema);
    if bad.is_empty() {
        Ok(schema)
    } else {
        Err(bad
            .into_iter()
            .map(|m| Problem::new(BAD_SCHEMA, m))
            .collect())
    }
}

/// Qualified names of a schema, for callers that only need the set.
pub fn names(schema: &Value) -> Vec<String> {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}

pub fn qualified(group: &str, native: &str) -> String {
    qualify(group, native)
}
