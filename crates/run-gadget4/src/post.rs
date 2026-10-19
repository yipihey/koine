//! What both `launch` and `adopt` do with what a finished run left behind:
//! find the output directory, read `parameters-usedvalues`, read the snapshot's
//! `Config`/`Parameters` groups, and say where they disagree with `params.json`.

use crate::echo;
use crate::error::{Problem, BAD_OUTPUT_DIR};
use crate::h5;
use crate::render;
use crate::table;
use crate::value;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// Relative tolerance for comparing a parameter file value with what GADGET-4
/// printed back: `parameters-usedvalues` is written with C's `%g`, six
/// significant digits, so `3.085678e24` comes back as `3.08568e+24`.
pub const USEDVALUES_REL_TOL: f64 = 1e-5;
/// The snapshot's `Parameters` group holds the doubles themselves, but `h5dump`
/// prints them with `%g` too, so the same tolerance applies.
pub const SNAPSHOT_REL_TOL: f64 = 1e-5;

/// `io.OutputDir` as a path relative to the run directory (`output`, `.` for
/// the run directory itself). It must stay inside the run (R-D7: output paths
/// are relative to the run and never leave it).
pub fn output_dir(values: &BTreeMap<String, Value>) -> Result<PathBuf, Problem> {
    let Some(v) = values.get("io.OutputDir").and_then(Value::as_str) else {
        return Err(Problem::new(
            BAD_OUTPUT_DIR,
            "io.OutputDir is required to know where GADGET-4 writes",
        ));
    };
    normalise_dir(v)
}

pub fn normalise_dir(v: &str) -> Result<PathBuf, Problem> {
    let p = Path::new(v);
    if p.is_absolute() {
        return Err(Problem::new(
            BAD_OUTPUT_DIR,
            format!("io.OutputDir `{v}` is absolute; it must be a path inside the run directory"),
        ));
    }
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            _ => {
                return Err(Problem::new(
                    BAD_OUTPUT_DIR,
                    format!("io.OutputDir `{v}` leaves the run directory"),
                ))
            }
        }
    }
    Ok(out)
}

fn path_norm(s: &str) -> String {
    let mut t = s.trim();
    while let Some(r) = t.strip_prefix("./") {
        t = r;
    }
    t.trim_end_matches('/').to_string()
}

fn same(name: &str, want: &Value, got: &Value, tol: f64) -> bool {
    match (want, got) {
        (Value::String(a), Value::String(b)) => {
            if name.ends_with("OutputDir") {
                path_norm(a) == path_norm(b)
            } else {
                a == b
            }
        }
        (a, b) => match (a.as_f64(), b.as_f64()) {
            (Some(x), Some(y)) => value::close(x, y, tol),
            _ => a == b,
        },
    }
}

/// Differences between `params.json` and the tags GADGET-4 says it used.
/// One message per difference; none when they agree. A tag GADGET-4 used that
/// params.json lacks, or the reverse, is a difference too.
pub fn diff_native(
    params: &Value,
    used: &BTreeMap<String, Value>,
    tol: f64,
    what: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut want: BTreeMap<String, Value> = BTreeMap::new();
    for (q, v) in render::values(params) {
        if let Some((_, native)) = table::split_qualified(&q) {
            want.insert(native.to_string(), v);
        }
    }
    for (k, w) in &want {
        match used.get(k) {
            Some(g) => {
                if !same(k, w, g, tol) {
                    out.push(format!("{what}: {k} is {g}, params.json has {w}"));
                }
            }
            None => out.push(format!("{what}: {k} is in params.json but not in {what}")),
        }
    }
    for k in used.keys() {
        if !want.contains_key(k) {
            out.push(format!("{what}: {k} is in {what} but not in params.json"));
        }
    }
    out
}

/// `parameters-usedvalues` text -> `{native tag: typed value}`.
pub fn used_values(text: &str) -> BTreeMap<String, Value> {
    let e = echo::parse(text);
    let mut m = BTreeMap::new();
    for (tag, t) in &e.params {
        let kind = table::lookup(tag)
            .map(|r| r.def.kind)
            .unwrap_or_else(|| value::infer_kind(t));
        if let Ok(v) = value::from_text(kind, t) {
            m.entry(tag.clone()).or_insert(v);
        }
    }
    m
}

/// A snapshot's header groups, if `h5dump` can read them.
#[derive(Debug, Default, Clone)]
pub struct SnapshotInfo {
    pub config: Map<String, Value>,
    pub parameters: Map<String, Value>,
    pub header: Map<String, Value>,
}

/// GADGET-4 writes every numeric `Config` value as a double (`PMGRID` is
/// `256.0` in the file); a whole number is an integer in the record.
fn integral(mut m: Map<String, Value>) -> Map<String, Value> {
    for v in m.values_mut() {
        if let Some(f) = v.as_f64().filter(|_| v.is_f64()) {
            if f.fract() == 0.0 && f.abs() < 9e15 {
                *v = json!(f as i64);
            }
        }
    }
    m
}

pub fn read_snapshot(path: &Path) -> Result<SnapshotInfo, String> {
    Ok(SnapshotInfo {
        config: integral(h5::read_group(path, "Config")?),
        parameters: h5::read_group(path, "Parameters")?,
        header: h5::read_group(path, "Header").unwrap_or_default(),
    })
}

/// `{"PERIODIC": true, "PMGRID": 256, ...}` -> `["PERIODIC", "PMGRID=256", ...]`.
pub fn build_flags(config: &Map<String, Value>) -> Vec<String> {
    config
        .iter()
        .map(|(k, v)| match v {
            Value::Bool(true) => k.clone(),
            Value::String(s) => format!("{k}={s}"),
            other => format!("{k}={other}"),
        })
        .collect()
}

/// `["PERIODIC", "PMGRID=256"]` (the startup echo) -> the Config object.
pub fn config_from_flags(flags: &[String]) -> Map<String, Value> {
    let mut m = Map::new();
    for f in flags {
        match f.split_once('=') {
            Some((k, v)) => {
                let val = v
                    .parse::<i64>()
                    .map(|n| json!(n))
                    .or_else(|_| v.parse::<f64>().map(|x| json!(x)))
                    .unwrap_or_else(|_| json!(v));
                m.insert(k.to_string(), val);
            }
            None => {
                m.insert(f.clone(), json!(true));
            }
        }
    }
    m
}

/// Differences between two Config objects (echo vs snapshot).
pub fn diff_config(echo: &Map<String, Value>, snap: &Map<String, Value>) -> Vec<String> {
    let mut out = Vec::new();
    for (k, v) in echo {
        match snap.get(k) {
            Some(s) if same(k, v, s, 1e-12) => {}
            Some(s) => out.push(format!(
                "Config: {k} is {v} in the startup echo, {s} in the snapshot"
            )),
            None => out.push(format!(
                "Config: {k} is in the startup echo but not the snapshot"
            )),
        }
    }
    for k in snap.keys() {
        if !echo.contains_key(k) {
            out.push(format!(
                "Config: {k} is in the snapshot but not the startup echo"
            ));
        }
    }
    out
}

/// Is `name` a snapshot file of base `base`: `base_NNN.hdf5` or `base_NNN.N.hdf5`?
pub fn is_snapshot_file(name: &str, base: &str) -> bool {
    let Some(rest) = name.strip_prefix(base).and_then(|r| r.strip_prefix('_')) else {
        return false;
    };
    let Some(stem) = rest.strip_suffix(".hdf5") else {
        return false;
    };
    let mut parts = stem.split('.');
    let num = parts.next().unwrap_or("");
    let sub = parts.next();
    parts.next().is_none()
        && !num.is_empty()
        && num.bytes().all(|b| b.is_ascii_digit())
        && sub
            .map(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .unwrap_or(true)
}

/// The snapshot number of `base_NNN[.N].hdf5`.
pub fn snapshot_number(name: &str) -> Option<u64> {
    let stem = name.strip_suffix(".hdf5")?;
    let (_, tail) = stem.rsplit_once('_')?;
    tail.split('.').next()?.parse().ok()
}

/// Kind and format of a file GADGET-4 leaves in its output directory.
pub fn classify_file(name: &str) -> (&'static str, &'static str) {
    if name.ends_with(".hdf5") {
        ("other", "hdf5")
    } else if name == "energy.txt" || name.ends_with(".csv") {
        ("series", if name.ends_with(".csv") { "csv" } else { "txt" })
    } else if name.ends_with(".txt") {
        ("log", "txt")
    } else {
        ("other", "text")
    }
}
