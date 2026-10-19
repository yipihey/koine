//! Writing the run directory (R-D1): `run.json`, `params.json`, an append-only
//! `events.jsonl` with a gap-free `seq`, and the `outputs.json` index.

use crate::error::{io, Error};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub fn iso(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn now() -> String {
    iso(Utc::now())
}

pub fn sha256_file(p: &Path) -> std::io::Result<String> {
    let mut f = File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Write a JSON file by writing a sibling and renaming it over the target, so a
/// reader never sees half a file.
pub fn write_json_atomic(path: &Path, v: &Value) -> Result<(), Error> {
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(v).map_err(|e| Error::other(e.to_string()))?;
    io(
        format!("write {}", tmp.display()),
        fs::write(&tmp, text + "\n"),
    )?;
    io(format!("rename {}", path.display()), fs::rename(&tmp, path))
}

pub struct RunWriter {
    pub dir: PathBuf,
    seq: u64,
    events: File,
    outputs: Vec<Value>,
    pub run: Value,
}

impl RunWriter {
    /// Create (or, for `adopt`, open beside existing files) the run directory.
    /// Refuses a directory that already holds a `run.json`.
    pub fn create(dir: &Path, run: Value) -> Result<Self, Error> {
        io(format!("create {}", dir.display()), fs::create_dir_all(dir))?;
        if dir.join("run.json").exists() {
            return Err(Error::other(format!(
                "{} already has a run.json; refusing to overwrite a run",
                dir.display()
            )));
        }
        let events = io(
            "open events.jsonl",
            OpenOptions::new()
                .create_new(true)
                .append(true)
                .open(dir.join("events.jsonl")),
        )?;
        let w = RunWriter {
            dir: dir.to_path_buf(),
            seq: 0,
            events,
            outputs: Vec::new(),
            run,
        };
        w.save_run()?;
        Ok(w)
    }

    pub fn save_run(&self) -> Result<(), Error> {
        write_json_atomic(&self.dir.join("run.json"), &self.run)
    }

    pub fn last_seq(&self) -> u64 {
        self.seq
    }

    /// Append one event; one `write` call per line (R-D6.6). `fields` is merged
    /// into `{seq, t, kind}`.
    pub fn event(&mut self, kind: &str, fields: Value) -> Result<u64, Error> {
        self.event_at(&now(), kind, fields)
    }

    pub fn event_at(&mut self, t: &str, kind: &str, fields: Value) -> Result<u64, Error> {
        self.seq += 1;
        let mut o = Map::new();
        o.insert("seq".into(), json!(self.seq));
        o.insert("t".into(), json!(t));
        o.insert("kind".into(), json!(kind));
        if let Value::Object(f) = fields {
            for (k, v) in f {
                o.insert(k, v);
            }
        }
        let mut line =
            serde_json::to_string(&Value::Object(o)).map_err(|e| Error::other(e.to_string()))?;
        line.push('\n');
        io(
            "append events.jsonl",
            self.events.write_all(line.as_bytes()),
        )?;
        Ok(self.seq)
    }

    pub fn warning(&mut self, message: &str, step: Option<u64>) -> Result<u64, Error> {
        let m: String = message.chars().take(1500).collect();
        let mut f = json!({"message": m});
        if let Some(s) = step {
            f["step"] = json!(s);
        }
        self.event("warning", f)
    }

    /// Insert or replace (by path) an entry of `outputs.json` and rewrite it.
    pub fn index_output(&mut self, entry: Value) -> Result<(), Error> {
        let path = entry
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        match self
            .outputs
            .iter_mut()
            .find(|o| o.get("path").and_then(Value::as_str) == Some(&path))
        {
            Some(slot) => *slot = entry,
            None => self.outputs.push(entry),
        }
        self.save_outputs()
    }

    pub fn save_outputs(&self) -> Result<(), Error> {
        write_json_atomic(
            &self.dir.join("outputs.json"),
            &json!({"outputs": self.outputs}),
        )
    }

    pub fn has_output(&self, path: &str) -> bool {
        self.outputs
            .iter()
            .any(|o| o.get("path").and_then(Value::as_str) == Some(path))
    }

    /// Set `status` fields and rewrite `run.json`.
    pub fn set_status(&mut self, f: impl FnOnce(&mut Map<String, Value>)) -> Result<(), Error> {
        if let Some(s) = self.run.get_mut("status").and_then(Value::as_object_mut) {
            f(s);
        }
        self.save_run()
    }
}

/// An `outputs.json` entry for a file under `dir` (path relative to it).
pub fn output_entry(
    dir: &Path,
    rel: &str,
    kind: &str,
    format: &str,
    step: Option<u64>,
    time: Option<f64>,
) -> Result<Value, Error> {
    let full = dir.join(rel);
    let sha = io(format!("hash {rel}"), sha256_file(&full))?;
    let bytes = io(format!("stat {rel}"), fs::metadata(&full))?.len();
    let mut e = json!({"path": rel, "kind": kind, "format": format, "sha256": sha, "bytes": bytes});
    if let Some(s) = step {
        e["step"] = json!(s);
    }
    if let Some(t) = time {
        if t.is_finite() {
            e["time"] = json!(t);
        }
    }
    Ok(e)
}

/// `a/b` for a path under `dir`, never with a leading `./`.
pub fn rel(dir: &Path, p: &Path) -> String {
    p.strip_prefix(dir)
        .unwrap_or(p)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}
