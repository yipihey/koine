//! Reading the `Config`, `Parameters` and `Header` groups of a GADGET-4
//! snapshot.
//!
//! Chosen: `h5dump -g <group>` as a child process, parsed here. The `hdf5`
//! crate needs libhdf5 at build time (a system HDF5, or a long static build),
//! which the rest of this workspace does not have; `h5dump` is one binary that
//! is present wherever GADGET-4 itself was built (the HDF5 it links ships it).
//! When it is missing the adapter degrades to a `warning` event, never an error.

use serde_json::{json, Map, Value};
use std::path::Path;
use std::process::Command;

/// The `h5dump` to run: `$H5DUMP`, else `h5dump` on `PATH`.
pub fn h5dump_command() -> String {
    std::env::var("H5DUMP").unwrap_or_else(|_| "h5dump".to_string())
}

pub fn h5dump_available() -> bool {
    Command::new(h5dump_command())
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `h5dump -g /<group> <file>`, parsed to `{attribute: value}`.
pub fn read_group(file: &Path, group: &str) -> Result<Map<String, Value>, String> {
    let out = Command::new(h5dump_command())
        .arg("-g")
        .arg(format!("/{group}"))
        .arg(file)
        .output()
        .map_err(|e| format!("cannot run {}: {e}", h5dump_command()))?;
    if !out.status.success() {
        return Err(format!(
            "h5dump -g /{group} {} failed: {}",
            file.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(parse_group(&String::from_utf8_lossy(&out.stdout)))
}

/// Parse the `ATTRIBUTE "name" { DATATYPE ... DATA { (0): value } }` blocks of
/// an `h5dump -g` listing. Scalars become numbers or strings; a flag compiled
/// in without a value (an empty string, as GADGET-4 writes `CREATE_GRID`) is
/// `true`; an array is kept as an array.
pub fn parse_group(text: &str) -> Map<String, Value> {
    let mut out = Map::new();
    let mut name: Option<String> = None;
    let mut attr_depth = 0usize;
    let mut depth = 0usize;
    let mut datatype = String::new();
    let mut in_data = false;
    let mut values: Vec<String> = Vec::new();
    for raw in text.lines() {
        let l = raw.trim();
        if l == "}" {
            depth = depth.saturating_sub(1);
            if name.is_some() && depth < attr_depth {
                if let Some(n) = name.take() {
                    out.insert(n, typed(&datatype, &values));
                }
            }
            in_data = false;
            continue;
        }
        if l.ends_with('{') {
            depth += 1;
        }
        if let Some(rest) = l.strip_prefix("ATTRIBUTE \"") {
            if let Some(end) = rest.find('"') {
                name = Some(rest[..end].to_string());
                attr_depth = depth;
                datatype.clear();
                in_data = false;
                values.clear();
            }
        } else if name.is_some() && datatype.is_empty() && l.starts_with("DATATYPE") {
            datatype = l.to_string();
        } else if name.is_some() && l.starts_with("DATA {") {
            in_data = true;
        } else if name.is_some() && datatype.contains("H5T_STRING") && !in_data {
            datatype.push(' ');
            datatype.push_str(l); // STRSIZE/STRPAD lines of a multi-line datatype
        } else if in_data && l.starts_with('(') {
            if let Some((_, v)) = l.split_once("): ") {
                values.extend(split_values(v));
            }
        }
    }
    out
}

fn split_values(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in v.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            ',' if !quoted => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() || out.is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn typed(datatype: &str, values: &[String]) -> Value {
    let one = |s: &str| -> Value {
        if datatype.contains("H5T_STRING") {
            let s = s.trim().trim_matches('"');
            if s.is_empty() {
                json!(true)
            } else {
                json!(s)
            }
        } else if datatype.contains("H5T_IEEE_F") {
            s.parse::<f64>()
                .map(|f| json!(f))
                .unwrap_or_else(|_| json!(s))
        } else if let Ok(i) = s.parse::<i64>() {
            json!(i)
        } else {
            s.parse::<f64>()
                .map(|f| json!(f))
                .unwrap_or_else(|_| json!(s))
        }
    };
    match values {
        [] => Value::Null,
        [v] => one(v),
        many => Value::Array(many.iter().map(|s| one(s)).collect()),
    }
}
