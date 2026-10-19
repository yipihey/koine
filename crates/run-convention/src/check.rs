//! `check_dir`: validate one run directory against the convention.

use crate::canonical::{key_of, values_of};
use crate::schemas;
use jsonschema::Validator;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// One thing wrong: which file, which rule, what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Path relative to the run directory.
    pub file: String,
    /// `parse`, `schema`, `exit-band`, `key`, `seq`, `output-missing`,
    /// `output-index`, `path`, `missing-file`, `control`.
    pub rule: &'static str,
    pub message: String,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: [{}] {}", self.file, self.rule, self.message)
    }
}

/// Everything `check_dir` found.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub dir: PathBuf,
    pub findings: Vec<Finding>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.findings.is_empty()
    }

    /// Findings under one rule.
    pub fn with_rule(&self, rule: &str) -> Vec<&Finding> {
        self.findings.iter().filter(|f| f.rule == rule).collect()
    }

    fn add(&mut self, file: &str, rule: &'static str, message: impl Into<String>) {
        self.findings.push(Finding {
            file: file.to_string(),
            rule,
            message: message.into(),
        });
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for x in &self.findings {
            writeln!(f, "{x}")?;
        }
        if self.ok() {
            write!(f, "{}: ok", self.dir.display())
        } else {
            write!(
                f,
                "{}: {} finding{}",
                self.dir.display(),
                self.findings.len(),
                if self.findings.len() == 1 { "" } else { "s" }
            )
        }
    }
}

fn validator(text: &str) -> Validator {
    let schema: Value = serde_json::from_str(text).expect("bundled schema is JSON");
    jsonschema::options()
        .should_validate_formats(true)
        .build(&schema)
        .expect("bundled schema is a valid JSON Schema")
}

/// Run `v` against `schema`, adding one finding per violation. A violation of
/// an `exit.code` is the `exit-band` rule; everything else is `schema`.
fn validate(report: &mut Report, file: &str, v: &Validator, instance: &Value, prefix: &str) {
    for e in v.iter_errors(instance) {
        let at = e.instance_path.to_string();
        let rule = if at.ends_with("/exit/code") {
            "exit-band"
        } else {
            "schema"
        };
        let msg = if rule == "exit-band" {
            format!(
                "{prefix}{at}: exit code {} is outside the bands (0; 100-199; 200-299; 300-399; 400-499)",
                e.instance
            )
        } else {
            format!("{prefix}{}: {e}", if at.is_empty() { "/" } else { &at })
        };
        report.add(file, rule, msg);
    }
}

fn read_json(report: &mut Report, dir: &Path, file: &str) -> Option<Value> {
    let text = match fs::read_to_string(dir.join(file)) {
        Ok(t) => t,
        Err(e) => {
            report.add(file, "parse", format!("cannot read: {e}"));
            return None;
        }
    };
    match serde_json::from_str(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            report.add(file, "parse", format!("not JSON: {e}"));
            None
        }
    }
}

/// Validate a code's emitted parameter schema (`<code> --schema`) against the
/// meta-schema (R-D2.1). Returns one message per violation; empty is valid.
pub fn check_parameter_schema(schema: &Value) -> Vec<String> {
    let v = validator(schemas::PARAMETER_SCHEMA);
    v.iter_errors(schema)
        .map(|e| {
            let at = e.instance_path.to_string();
            format!("{}: {e}", if at.is_empty() { "/" } else { &at })
        })
        .collect()
}

/// Check the run directory at `dir`. Never panics on a bad directory: every
/// problem is a [`Finding`].
pub fn check_dir(dir: impl AsRef<Path>) -> Report {
    let dir = dir.as_ref();
    let mut r = Report {
        dir: dir.to_path_buf(),
        findings: Vec::new(),
    };
    if !dir.is_dir() {
        r.add(".", "missing-file", "not a directory");
        return r;
    }

    let run = if dir.join("run.json").exists() {
        read_json(&mut r, dir, "run.json")
    } else {
        r.add("run.json", "missing-file", "a run directory has a run.json");
        None
    };
    let params = if dir.join("params.json").exists() {
        read_json(&mut r, dir, "params.json")
    } else {
        r.add(
            "params.json",
            "missing-file",
            "a run directory has a params.json",
        );
        None
    };

    if let Some(run) = &run {
        validate(&mut r, "run.json", &validator(schemas::RUN), run, "");
    }
    if let Some(params) = &params {
        validate(
            &mut r,
            "params.json",
            &validator(schemas::PARAMS),
            params,
            "",
        );
    }

    // The key: sha256(canonical({problem, params-values})) (R-D4.3).
    if let (Some(run), Some(params)) = (&run, &params) {
        if let (Some(key), Some(problem)) =
            (run.get("key").and_then(Value::as_str), run.get("problem"))
        {
            let want = key_of(problem, &values_of(params));
            if key != want {
                r.add(
                    "run.json",
                    "key",
                    format!("key {key} != sha256(canonical({{problem, params}})) = {want}"),
                );
            }
        }
    }

    let state = run
        .as_ref()
        .and_then(|r| r.pointer("/status/state"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let last_event_seq = run
        .as_ref()
        .and_then(|r| r.pointer("/status/last_event_seq"))
        .and_then(Value::as_u64);

    // events.jsonl
    let mut output_event_paths: Vec<String> = Vec::new();
    if dir.join("events.jsonl").exists() {
        let n = check_events(&mut r, dir, &mut output_event_paths);
        if let Some(l) = last_event_seq {
            if l > n {
                r.add(
                    "run.json",
                    "seq",
                    format!(
                        "status.last_event_seq is {l} but events.jsonl has {n} events (truncated?)"
                    ),
                );
            }
        }
    } else if state != "created" && !state.is_empty() {
        r.add(
            "events.jsonl",
            "missing-file",
            format!("a run in state `{state}` has an events.jsonl"),
        );
    }

    // outputs.json
    if dir.join("outputs.json").exists() {
        check_outputs(&mut r, dir, &output_event_paths);
    } else if !output_event_paths.is_empty() {
        r.add(
            "outputs.json",
            "missing-file",
            "events.jsonl has `output` events but there is no outputs.json index",
        );
    }

    // notes.json is mutable and never hashed: it must only be an object.
    if dir.join("notes.json").exists() {
        if let Some(v) = read_json(&mut r, dir, "notes.json") {
            if !v.is_object() {
                r.add("notes.json", "schema", "/: must be an object");
            }
        }
    }

    check_control(&mut r, dir);
    r
}

/// Returns the number of events read.
fn check_events(r: &mut Report, dir: &Path, output_paths: &mut Vec<String>) -> u64 {
    let file = "events.jsonl";
    let bytes = match fs::read(dir.join(file)) {
        Ok(b) => b,
        Err(e) => {
            r.add(file, "parse", format!("cannot read: {e}"));
            return 0;
        }
    };
    let text = String::from_utf8_lossy(&bytes);
    let ends_nl = text.ends_with('\n');
    let lines: Vec<&str> = text.split('\n').collect();
    // `split` leaves an empty tail after a final newline; otherwise the tail
    // is a line still being written.
    let complete = lines.len().saturating_sub(1);
    let v = validator(schemas::EVENT);
    let mut n = 0u64;
    let mut seq_reported = false;
    for (i, line) in lines.iter().enumerate() {
        let trailing = i == complete;
        if line.trim().is_empty() {
            continue;
        }
        let ev: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                // R-D6.6: a reader ignores a trailing partial line.
                if trailing && !ends_nl {
                    continue;
                }
                r.add(file, "parse", format!("line {}: not JSON: {e}", i + 1));
                n += 1;
                continue;
            }
        };
        n += 1;
        validate(r, file, &v, &ev, &format!("line {}: ", i + 1));
        match ev.get("seq").and_then(Value::as_u64) {
            Some(s) if s == n => {}
            Some(s) if !seq_reported => {
                r.add(
                    file,
                    "seq",
                    format!(
                        "line {}: seq is {s}, expected {n} (seq must be 1..n, gap-free)",
                        i + 1
                    ),
                );
                seq_reported = true;
            }
            None if !seq_reported => {
                r.add(file, "seq", format!("line {}: missing integer seq", i + 1));
                seq_reported = true;
            }
            _ => {}
        }
        if ev.get("kind").and_then(Value::as_str) == Some("output") {
            if let Some(p) = ev.get("path").and_then(Value::as_str) {
                output_paths.push(p.to_string());
            }
        }
    }
    n
}

/// A relative path that stays inside the run directory.
fn inside(path: &str) -> bool {
    let p = Path::new(path);
    p.is_relative()
        && !p
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
}

fn check_outputs(r: &mut Report, dir: &Path, event_paths: &[String]) {
    let file = "outputs.json";
    let Some(v) = read_json(r, dir, file) else {
        return;
    };
    validate(r, file, &validator(schemas::OUTPUTS), &v, "");
    let mut indexed: BTreeSet<&str> = BTreeSet::new();
    for (i, o) in v
        .get("outputs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let Some(p) = o.get("path").and_then(Value::as_str) else {
            continue;
        };
        indexed.insert(p);
        if !inside(p) {
            r.add(
                file,
                "path",
                format!("outputs[{i}]: path `{p}` must be relative and stay inside the run"),
            );
        } else if !dir.join(p).exists() {
            r.add(
                file,
                "output-missing",
                format!("outputs[{i}]: `{p}` is indexed but does not exist"),
            );
        }
    }
    for p in event_paths {
        if !indexed.contains(p.as_str()) {
            r.add(
                file,
                "output-index",
                format!("`{p}` has an `output` event in events.jsonl but no entry here (R-D7)"),
            );
        }
    }
}

fn check_control(r: &mut Report, dir: &Path) {
    let v = validator(schemas::CONTROL);
    for sub in ["", "done", "refused"] {
        let d = dir.join("control").join(sub);
        let Ok(rd) = fs::read_dir(&d) else { continue };
        let mut names: Vec<_> = rd.filter_map(Result::ok).collect();
        names.sort_by_key(|e| e.file_name());
        for e in names {
            let name = e.file_name().to_string_lossy().into_owned();
            if !e.path().is_file() || !name.ends_with(".json") {
                continue;
            }
            let rel = if sub.is_empty() {
                format!("control/{name}")
            } else {
                format!("control/{sub}/{name}")
            };
            let Some(body) = read_json(r, dir, &rel) else {
                continue;
            };
            validate(r, &rel, &v, &body, "");
            let stem = name.trim_end_matches(".json");
            if let Some(id) = body.get("id").and_then(Value::as_str) {
                if id != stem {
                    r.add(&rel, "control", format!("id `{id}` != file name `{stem}`"));
                }
            }
            if sub == "refused" && body.get("reason").and_then(Value::as_str).is_none() {
                r.add(&rel, "control", "a refused request carries a `reason`");
            }
        }
    }
}
