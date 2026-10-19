//! The best-effort importer of R-D11: turns an old run folder (vrun
//! `manifest.toml`, Vespa `metadata.toml`, `command.sh`) into a conforming
//! `run.json` (plus `params.json`, `outputs.json`, `notes.json`) written
//! *beside* what is there. Nothing is moved or modified.
//!
//! The rule that shapes everything: never pretend to know what was never
//! recorded. A `key` is written only when the driver's known parameter family
//! (`MHD_*` for the PMF driver, `BAM_*`, `CIC_*`) is present in what was
//! recovered; otherwise `run.json` has no `key` and says why under
//! `imported.key_withheld`.

use crate::{key_of, values_of};
use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Files at or above this size get `bytes` only, never a `sha256`.
pub const HASH_LIMIT: u64 = 64 * 1024 * 1024;
const LOG_TAIL: u64 = 256 * 1024;
const MAX_OUTPUTS: usize = 2000;
const MAX_DEPTH: usize = 3;

/// The namespace the stable run ids are derived in (UUID v5 of the folder name).
const ID_NAMESPACE: Uuid = Uuid::from_u128(0x6b1f_0c2e_7a44_5d1b_9e0a_52c1_7d3e_a001);

#[derive(Debug, Default, Clone)]
pub struct ImportOptions {
    /// Compute the plan and report it; write nothing, hash nothing.
    pub dry_run: bool,
    /// Overwrite an existing `run.json` (and the other record files).
    pub force: bool,
}

/// The old layouts, in precedence order (a folder may have several).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Style {
    Manifest,
    Command,
    Metadata,
}

impl Style {
    pub fn file(self) -> &'static str {
        match self {
            Style::Manifest => "manifest.toml",
            Style::Command => "command.sh",
            Style::Metadata => "metadata.toml",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Style::Manifest => "manifest",
            Style::Command => "command",
            Style::Metadata => "metadata",
        }
    }
}

#[derive(Debug)]
pub enum ImportError {
    Io(String, std::io::Error),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::Io(what, e) => write!(f, "{what}: {e}"),
        }
    }
}

impl std::error::Error for ImportError {}

fn io<T>(what: impl Into<String>, r: std::io::Result<T>) -> Result<T, ImportError> {
    r.map_err(|e| ImportError::Io(what.into(), e))
}

/// Everything the importer would write for one folder.
#[derive(Debug, Clone)]
pub struct Plan {
    /// Every style found, in precedence order; the first is the primary one.
    pub styles: Vec<Style>,
    pub run: Value,
    pub params: Value,
    pub outputs: Value,
    pub notes: Option<Value>,
    /// `events.jsonl` text: the checker requires one for any started run, so
    /// an import writes the two events its facts support (`started` when the
    /// start time is known, `finished` with the recovered exit).
    pub events: String,
}

impl Plan {
    pub fn primary(&self) -> Style {
        self.styles[0]
    }
    pub fn has_key(&self) -> bool {
        self.run.get("key").is_some()
    }
    pub fn summary(&self) -> String {
        let n = |v: &Value| v.as_object().map_or(0, |o| o.len());
        let outs = self
            .outputs
            .get("outputs")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        format!(
            "run.json params.json({}) outputs.json({}){} [style={} key={}]",
            n(&self.params),
            outs,
            if self.notes.is_some() {
                " notes.json"
            } else {
                ""
            },
            self.primary().label(),
            self.run
                .get("key")
                .and_then(Value::as_str)
                .map_or("none", |k| &k[..12]),
        )
    }
}

#[derive(Debug)]
pub enum Outcome {
    Written(Plan),
    DryRun(Plan),
    /// A record file already exists and `--force` was not given.
    Skipped(String),
    /// None of the three styles is present.
    Unrecognised,
}

/// Which styles a folder has, in precedence order.
pub fn detect(dir: &Path) -> Vec<Style> {
    [Style::Manifest, Style::Command, Style::Metadata]
        .into_iter()
        .filter(|s| dir.join(s.file()).is_file())
        .collect()
}

/// Import one folder: plan, then (unless dry-run) write.
pub fn import_dir(dir: impl AsRef<Path>, opts: &ImportOptions) -> Result<Outcome, ImportError> {
    let dir = dir.as_ref();
    if detect(dir).is_empty() {
        return Ok(Outcome::Unrecognised);
    }
    if !opts.force {
        for f in ["run.json", "params.json", "outputs.json", "events.jsonl"] {
            if dir.join(f).exists() {
                return Ok(Outcome::Skipped(format!("{f} exists; --force overwrites")));
            }
        }
    }
    let Some(plan) = plan_import(dir, !opts.dry_run, Utc::now())? else {
        return Ok(Outcome::Unrecognised);
    };
    if opts.dry_run {
        return Ok(Outcome::DryRun(plan));
    }
    if !opts.force && plan.notes.is_some() && dir.join("notes.json").exists() {
        return Ok(Outcome::Skipped(
            "notes.json exists; --force overwrites".into(),
        ));
    }
    let put = |name: &str, v: &Value| -> Result<(), ImportError> {
        let text = serde_json::to_string_pretty(v).unwrap_or_default() + "\n";
        io(format!("write {name}"), fs::write(dir.join(name), text))
    };
    // run.json last: its presence is what marks a finished import.
    put("params.json", &plan.params)?;
    put("outputs.json", &plan.outputs)?;
    io(
        "write events.jsonl",
        fs::write(dir.join("events.jsonl"), &plan.events),
    )?;
    if let Some(n) = &plan.notes {
        put("notes.json", n)?;
    }
    put("run.json", &plan.run)?;
    Ok(Outcome::Written(plan))
}

/// Build the records for a folder without writing. `hash` computes `sha256`
/// of outputs under [`HASH_LIMIT`] (skipped for dry runs: it reads every file).
pub fn plan_import(
    dir: &Path,
    hash: bool,
    now: DateTime<Utc>,
) -> Result<Option<Plan>, ImportError> {
    let styles = detect(dir);
    if styles.is_empty() {
        return Ok(None);
    }
    let folder = dir
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
        .or_else(|| dir.file_name().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "run".into());

    let mut f = Facts::default();
    // Lowest precedence first, so a higher style overrides a scalar.
    for s in styles.iter().rev() {
        let text = io(
            format!("read {}", s.file()),
            fs::read_to_string(dir.join(s.file())),
        )?;
        match s {
            Style::Metadata => f.read_metadata(&text),
            Style::Command => f.read_command(&text, &folder),
            Style::Manifest => f.read_manifest(&text, &folder),
        }
        f.sources.push(s.file().to_string());
    }
    f.sources.reverse();
    f.read_notes(dir);

    let log = read_tail(&dir.join("run.log"));
    if f.julia.is_none() {
        f.julia = log.as_deref().and_then(julia_from_text);
    }
    let (state, exit, ended_src) = f.status(log.as_deref(), dir, now);

    // ---- params ----
    let mut params = Map::new();
    for (name, (v, src)) in &f.params {
        params.insert(name.clone(), json!({"value": v, "source": src}));
    }
    let params = Value::Object(params);

    // ---- problem ----
    let problem_name = f
        .problem_name
        .clone()
        .or_else(|| f.campaign.clone())
        .unwrap_or_else(|| "unknown".into());
    let mut problem = Map::new();
    problem.insert("name".into(), json!(problem_name));
    problem.insert("params".into(), Value::Object(f.problem_params.clone()));
    let problem = Value::Object(problem);

    // ---- key ----
    let (complete, why) = f.completeness();
    let mut run = Map::new();
    run.insert("convention".into(), json!("run@1"));
    run.insert(
        "id".into(),
        json!(Uuid::new_v5(&ID_NAMESPACE, folder.as_bytes()).to_string()),
    );
    run.insert("name".into(), json!(folder));
    run.insert("provenance".into(), json!("imported"));
    let mut imported = Map::new();
    imported.insert("from".into(), json!(f.sources));
    imported.insert(
        "at".into(),
        json!(now.to_rfc3339_opts(SecondsFormat::Secs, true)),
    );
    if complete {
        run.insert("key".into(), json!(key_of(&problem, &values_of(&params))));
    } else {
        imported.insert("key_withheld".into(), json!(why));
    }
    if let Some(s) = &ended_src {
        imported.insert("ended_at_source".into(), json!(s));
    }
    if !f.warnings.is_empty() {
        imported.insert("warnings".into(), json!(f.warnings));
    }
    run.insert("imported".into(), Value::Object(imported));

    // ---- code ----
    let mut code = Map::new();
    code.insert("name".into(), json!(f.code_name));
    match &f.sha {
        Some(sha) if sha.len() == 40 => {
            code.insert("version".into(), json!(sha));
            code.insert("commit".into(), json!(sha));
        }
        Some(sha) => {
            // The schema's `commit` is the full hash; a short one is kept as
            // the version and marked, never padded.
            code.insert("version".into(), json!(sha));
            code.insert("commit_short".into(), json!(true));
        }
        None => {
            code.insert("version".into(), json!("unknown"));
        }
    }
    if let Some(d) = f.dirty {
        code.insert("dirty".into(), json!(d));
    }
    if let Some(j) = &f.julia {
        code.insert("julia".into(), json!(j));
    }
    run.insert("code".into(), Value::Object(code));
    run.insert("problem".into(), problem);

    // ---- host ----
    let mut host = Map::new();
    if let Some(h) = &f.hostname {
        host.insert("hostname".into(), json!(h));
    }
    if let Some(a) = &f.accelerator {
        host.insert("accelerator".into(), json!(a));
    }
    if !f.argv.is_empty() {
        host.insert("argv".into(), json!(f.argv));
    }
    if !f.host_env.is_empty() {
        host.insert("env".into(), Value::Object(f.host_env.clone()));
    }
    if !host.is_empty() {
        run.insert("host".into(), Value::Object(host));
    }

    // ---- status ----
    let mut status = Map::new();
    status.insert("state".into(), json!(state));
    if let Some(t) = f.started {
        status.insert("started_at".into(), json!(iso(t)));
    }
    status.insert("ended_at".into(), json!(iso(f.ended.unwrap_or(now))));
    let mut lines = Vec::new();
    if let Some(t) = f.started {
        lines.push(json!({"seq": lines.len() + 1, "t": iso(t), "kind": "started"}));
    }
    lines.push(json!({"seq": lines.len() + 1,
        "t": iso(f.ended.unwrap_or(now)), "kind": "finished", "exit": exit}));
    status.insert("last_event_seq".into(), json!(lines.len()));
    let events: String = lines.iter().map(|l| format!("{l}\n")).collect();
    status.insert("exit".into(), exit);
    run.insert("status".into(), Value::Object(status));

    // ---- outputs ----
    let outputs = outputs_of(dir, hash);

    // ---- notes ----
    let notes = if f.notes.is_empty() && f.tags.is_empty() {
        None
    } else {
        let mut m = Map::new();
        if !f.notes.is_empty() {
            m.insert("notes".into(), json!(f.notes));
        }
        if !f.tags.is_empty() {
            m.insert("tags".into(), json!(f.tags));
        }
        Some(Value::Object(m))
    };

    Ok(Some(Plan {
        styles,
        run: Value::Object(run),
        params,
        outputs,
        notes,
        events,
    }))
}

fn iso(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

// ---------------------------------------------------------------------------
// Facts: what the three styles yield
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Facts {
    sources: Vec<String>,
    /// namespaced name -> (value, source)
    params: BTreeMap<String, (Value, String)>,
    /// original name -> value, the physics keys as the old files spelled them
    problem_params: Map<String, Value>,
    host_env: Map<String, Value>,
    hostname: Option<String>,
    accelerator: Option<String>,
    argv: Vec<String>,
    code_name: String,
    sha: Option<String>,
    dirty: Option<bool>,
    julia: Option<String>,
    problem_name: Option<String>,
    campaign: Option<String>,
    tags: Vec<String>,
    notes: Vec<String>,
    started: Option<DateTime<Utc>>,
    ended: Option<DateTime<Utc>>,
    wall_sec: Option<f64>,
    manifest_status: Option<String>,
    /// Parameter families the driver is known to read (`MHD_`, `BAM_`, `CIC_`).
    families: BTreeSet<&'static str>,
    /// Families for which an env value was actually recovered.
    present: BTreeSet<String>,
    /// What the folder's text names, to infer the driver when no command exists.
    hints: String,
    warnings: Vec<String>,
}

const FAMILIES: [(&str, &str); 3] = [("MHD_", "mhd"), ("BAM_", "bam"), ("CIC_", "cic")];

impl Facts {
    fn add_param(&mut self, name: String, orig: &str, v: Value, source: &str) {
        self.problem_params.insert(orig.to_string(), v.clone());
        self.params.insert(name, (v, format!("imported:{source}")));
    }

    /// An environment variable: a physics parameter, or part of the launching
    /// environment (thread counts, output paths) that belongs to the host.
    fn add_env(&mut self, key: &str, raw: &str, folder: &str, source: &str) {
        if key.starts_with("JULIA_") || looks_like_path(raw, folder) {
            self.host_env.insert(key.to_string(), json!(raw));
            return;
        }
        let (ns, rest) = match FAMILIES.iter().find(|(p, _)| key.starts_with(p)) {
            Some((p, ns)) => {
                self.present.insert((*p).to_string());
                (*ns, &key[p.len()..])
            }
            None => ("env", key),
        };
        let name = format!("{ns}.{}", sanitize(&rest.to_lowercase()));
        self.add_param(name, key, scalar(raw), source);
    }

    fn read_metadata(&mut self, text: &str) {
        let folder = "";
        let mut seen = BTreeSet::new();
        for (section, key, v) in parse_lenient(text) {
            let id = format!("{section}\0{key}");
            if !seen.insert(id) {
                self.warnings.push(format!(
                    "metadata.toml: duplicate key `{key}`, last one kept"
                ));
            }
            let s = v.as_str().map(str::to_owned);
            match (section.as_str(), key.as_str()) {
                (_, "machine") => {
                    if let Some(m) = s.filter(|m| m != "unknown") {
                        self.hostname = Some(m);
                    }
                }
                (_, "created_utc") => {
                    if let Some(t) = s.as_deref().and_then(parse_time) {
                        self.started = Some(t);
                    }
                }
                (_, "campaign") => self.campaign = s.clone(),
                (_, "note") => {
                    if let Some(n) = s {
                        self.notes.push(n);
                    }
                }
                (_, "script") => {
                    if let Some(sc) = s {
                        self.hints.push_str(&format!(" {sc}"));
                        self.problem_name = stem(&sc);
                    }
                }
                (_, "vespa_sha") => {
                    if let Some(sha) = s.filter(|x| is_hex(x)) {
                        self.sha = Some(sha);
                    }
                }
                (_, k) if k.ends_with("_sha") || k.ends_with("_root") || k == "vespa" => {}
                _ => {
                    if v.as_str().is_some_and(|x| looks_like_path(x, folder)) {
                        self.host_env.insert(key.clone(), v.clone());
                        continue;
                    }
                    let ns = if section.is_empty() {
                        "meta".to_string()
                    } else {
                        sanitize(&section)
                    };
                    let name = format!("{ns}.{}", sanitize(&key));
                    self.add_param(name, &key, v.clone(), "metadata.toml");
                }
            }
        }
        if self.code_name.is_empty() {
            self.code_name = "vespa".into();
        }
    }

    fn read_command(&mut self, text: &str, folder: &str) {
        let mut tokens = Vec::new();
        let mut joined = String::new();
        for line in text.lines() {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            if let Some(stripped) = t.strip_suffix('\\') {
                joined.push_str(stripped);
                joined.push(' ');
                continue;
            }
            joined.push_str(t);
            tokens.extend(shell_split(&joined));
            joined.clear();
        }
        tokens.extend(shell_split(&joined));
        let mut i = 0;
        if tokens.first().is_some_and(|t| t == "env" || t == "export") {
            i = 1;
        }
        while i < tokens.len() {
            match split_assign(&tokens[i]) {
                Some((k, v)) => self.add_env(&k, &v, folder, "command.sh"),
                None => break,
            }
            i += 1;
        }
        self.argv = tokens[i..].to_vec();
        // Keep a host-independent argv: drop the machine's absolute prefix
        // only where it is the julia binary; paths stay as recorded.
        if let Some(drv) = self.argv.iter().find(|a| a.ends_with(".jl")) {
            self.problem_name = stem(drv);
            self.hints.push_str(&format!(" {drv}"));
        }
        if let Some(j) = self.argv.first().and_then(|a| julia_from_text(a)) {
            self.julia = Some(j);
        }
        if self.code_name.is_empty() {
            self.code_name = "vespa".into();
        }
    }

    fn read_manifest(&mut self, text: &str, folder: &str) {
        for (section, key, v) in parse_lenient(text) {
            let s = v.as_str().map(str::to_owned);
            match (section.as_str(), key.as_str()) {
                ("run", "code") => {
                    if let Some(c) = &s {
                        self.hints.push_str(&format!(" {c}"));
                    }
                    self.problem_name = s;
                }
                ("run", "date_utc") => {
                    if let Some(t) = s.as_deref().and_then(parse_time) {
                        self.started = Some(t);
                    }
                }
                ("run", "tag") => {
                    if let Some(t) = s {
                        self.hints.push_str(&format!(" {t}"));
                        if !t.is_empty() {
                            self.tags.push(t);
                        }
                    }
                }
                ("run", "status") => self.manifest_status = s,
                ("run", "wall_sec") => self.wall_sec = v.as_f64(),
                ("provenance", "host") => {
                    if let Some(h) = s.filter(|h| h != "unknown") {
                        self.hostname = Some(h);
                    }
                }
                ("provenance", "vespa_sha") => {
                    self.sha = s.filter(|x| is_hex(x));
                }
                ("provenance", "vespa_dirty") => self.dirty = v.as_bool(),
                ("provenance", "launch_config") => {
                    if let Some(l) = s {
                        self.hints.push_str(&format!(" {l}"));
                        self.argv = vec![l];
                    }
                }
                ("provenance", "julia_ver") => {
                    if let Some(j) = s.filter(|j| !j.is_empty() && j != "unknown") {
                        self.julia = Some(j);
                    }
                }
                ("provenance", "gpu") => {
                    if let Some(g) = s.filter(|g| !g.is_empty() && g != "unknown") {
                        self.accelerator = Some(g);
                    }
                }
                // The hand-written variant (no `[run] code`): flat provenance
                // at the top, the run's settings under `[run]`.
                ("suite", "machine") => {
                    if let Some(h) = s.filter(|h| h != "unknown") {
                        self.hostname = Some(h);
                    }
                }
                ("suite", "vespa_sha") => {
                    self.sha = s.filter(|x| is_hex(x));
                }
                ("suite", "name") => {
                    if let Some(n) = s {
                        self.hints.push_str(&format!(" {n}"));
                        self.problem_name = Some(n);
                    }
                }
                ("suite", k)
                    if !k.ends_with("_sha")
                        && !k.ends_with("_root")
                        && !v.is_object()
                        && !v.is_array()
                        && !v.as_str().is_some_and(|x| looks_like_path(x, folder)) =>
                {
                    self.add_param(
                        format!("suite.{}", sanitize(k)),
                        k,
                        v.clone(),
                        "manifest.toml",
                    );
                }
                ("", "machine") => {
                    if let Some(h) = s.filter(|h| h != "unknown") {
                        self.hostname = Some(h);
                    }
                }
                ("", "vespa_commit") => {
                    self.sha = s.filter(|x| is_hex(x));
                }
                ("", "vespa_dirty") => self.dirty = v.as_bool(),
                ("", "id") => {
                    if let Some(i) = s {
                        self.hints.push_str(&format!(" {i}"));
                        let bare = i.trim_start_matches(|c: char| c.is_ascii_digit() || c == '-');
                        self.problem_name = Some(if bare.is_empty() {
                            i.clone()
                        } else {
                            bare.to_string()
                        });
                    }
                }
                ("run", k) if !v.is_object() && !v.is_array() => {
                    let skip = v.as_str().is_some_and(|x| looks_like_path(x, folder));
                    if !skip {
                        self.add_param(
                            format!("run.{}", sanitize(k)),
                            k,
                            v.clone(),
                            "manifest.toml",
                        );
                    }
                }
                ("config.env", k) => {
                    let raw = match &v {
                        Value::String(x) => x.clone(),
                        other => other.to_string(),
                    };
                    self.add_env(k, &raw, folder, "manifest.toml");
                }
                _ => {}
            }
        }
        self.code_name = "vespa".into();
    }

    fn read_notes(&mut self, dir: &Path) {
        if let Ok(t) = fs::read_to_string(dir.join("NOTES.md")) {
            let t = t.trim();
            if !t.is_empty() {
                self.sources.push("NOTES.md".into());
                self.notes.insert(0, t.to_string());
            }
        }
        if let Some(c) = self.campaign.clone() {
            if !self.tags.contains(&c) {
                self.tags.push(c);
            }
        }
    }

    /// A key is only honest when the driver's whole known parameter family is
    /// present. Returns `(complete, why-not)`.
    fn completeness(&mut self) -> (bool, String) {
        let h = self.hints.to_lowercase();
        for (needle, fam) in [
            ("pmf", "MHD_"),
            ("mhd", "MHD_"),
            ("bamr", "BAM_"),
            ("cicass", "CIC_"),
        ] {
            if h.contains(needle) {
                self.families.insert(fam);
            }
        }
        for (p, _) in FAMILIES {
            if self.present.contains(p) && p == "MHD_" {
                self.families.insert(p);
            }
        }
        if self.families.is_empty() {
            return (
                false,
                "driver not identifiable from the folder; cannot tell which parameters are missing"
                    .into(),
            );
        }
        let missing: Vec<&str> = self
            .families
            .iter()
            .copied()
            .filter(|p| !self.present.contains(*p))
            .collect();
        if missing.is_empty() {
            (true, String::new())
        } else {
            (
                false,
                format!(
                    "{}* values were never recorded (only {})",
                    missing.join("*, "),
                    self.sources.join(", ")
                ),
            )
        }
    }

    /// `(state, exit block, source of ended_at)`.
    fn status(
        &mut self,
        log: Option<&str>,
        dir: &Path,
        now: DateTime<Utc>,
    ) -> (&'static str, Value, Option<String>) {
        let ok = |label: &str| json!({"code": 0, "label": label});
        let failed = |code: i64, label: &str, msg: String| json!({"code": code, "label": label, "message": msg, "invalidates_reuse": true});
        let (state, exit): (&'static str, Value) = 'st: {
            if let Some(s) = &self.manifest_status {
                break 'st match s.as_str() {
                    "ok" | "done" => ("done", ok("ok")),
                    "failed" | "error" => (
                        "failed",
                        failed(400, "failed", "vrun manifest status = failed".into()),
                    ),
                    other => (
                        "done",
                        json!({"code": 0, "label": "unknown",
                               "message": format!("manifest status `{other}` not understood")}),
                    ),
                };
            }
            if let Some(log) = log {
                if let Some(n) = exit_sentinel(log) {
                    break 'st if n == 0 {
                        ("done", ok("ok"))
                    } else {
                        (
                            "failed",
                            failed(
                                400 + n.clamp(1, 99),
                                &format!("exit-{n}"),
                                format!("run.log sentinel: EXIT {n}"),
                            ),
                        )
                    };
                }
                if let Some((code, label, msg)) = log_failure(log) {
                    break 'st ("failed", failed(code, label, msg));
                }
                if log.lines().any(|l| l.starts_with("COMPLETE")) {
                    break 'st ("done", ok("ok"));
                }
            }
            (
                "done",
                json!({"code": 0, "label": "unknown",
                       "message": "outcome was not recorded: no manifest status, no EXIT sentinel and no recognisable end of run.log"}),
            )
        };
        // ended_at: manifest start + wall time, else the last write to run.log.
        let src;
        if let (Some(s), Some(w)) = (self.started, self.wall_sec) {
            self.ended = Some(s + chrono::Duration::milliseconds((w * 1000.0) as i64));
            src = Some("started_at+wall_sec".to_string());
        } else if let Some(t) = mtime(&dir.join("run.log")) {
            self.ended = Some(t);
            src = Some("mtime:run.log".into());
        } else if let Some(t) = newest_mtime(dir) {
            self.ended = Some(t);
            src = Some("mtime:newest-file".into());
        } else {
            self.ended = Some(now);
            src = Some("import-time".into());
        }
        (state, exit, src)
    }
}

fn mtime(p: &Path) -> Option<DateTime<Utc>> {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .map(DateTime::<Utc>::from)
}

fn newest_mtime(dir: &Path) -> Option<DateTime<Utc>> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| mtime(&e.path()))
        .max()
}

fn exit_sentinel(log: &str) -> Option<i64> {
    log.lines().rev().find_map(|l| {
        let t = l.trim();
        let inner = t.strip_prefix("=====")?.strip_suffix("=====")?.trim();
        inner.strip_prefix("EXIT")?.trim().parse::<i64>().ok()
    })
}

/// A Julia failure in the log, classified into the exit bands.
fn log_failure(log: &str) -> Option<(i64, &'static str, String)> {
    let line = log
        .lines()
        .find(|l| l.starts_with("ERROR:"))
        .map(str::to_owned);
    if line.is_none() && !log.lines().any(|l| l.starts_with("Stacktrace:")) {
        return None;
    }
    let msg = line.unwrap_or_else(|| "Stacktrace in run.log".into());
    let m = msg.to_lowercase();
    let (code, label) = if m.contains("metal backend is not functional")
        || m.contains("xpc_err")
        || m.contains("interrupted connection")
    {
        (110, "backend-unavailable")
    } else if m.contains("nan") || m.contains("inf") || m.contains("invalid timestep") {
        (300, "numerical-failure")
    } else if m.contains("must be") || m.contains("invalid") {
        (200, "invalid-input")
    } else {
        (400, "error")
    };
    let msg: String = msg.chars().take(300).collect();
    Some((code, label, msg))
}

fn read_tail(p: &Path) -> Option<String> {
    let mut f = fs::File::open(p).ok()?;
    let len = f.metadata().ok()?.len();
    if len > LOG_TAIL {
        f.seek(SeekFrom::Start(len - LOG_TAIL)).ok()?;
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

// ---------------------------------------------------------------------------
// Outputs
// ---------------------------------------------------------------------------

fn outputs_of(dir: &Path, hash: bool) -> Value {
    let mut files = Vec::new();
    let mut truncated = false;
    walk(dir, dir, 0, &mut files, &mut truncated);
    files.sort();
    let sidecars: BTreeSet<PathBuf> = files
        .iter()
        .filter(|p| p.file_name().is_some_and(|n| n == "fields.toml"))
        .cloned()
        .collect();
    let mut out = Vec::new();
    for rel in &files {
        let full = dir.join(rel);
        let Ok(meta) = fs::metadata(&full) else {
            continue;
        };
        let name = rel.file_name().map(|n| n.to_string_lossy().to_lowercase());
        let name = name.unwrap_or_default();
        let ext = Path::new(&name)
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut e = Map::new();
        e.insert("path".into(), json!(rel_str(rel)));
        e.insert("kind".into(), json!(kind_of(&name, &ext)));
        let sidecar = rel
            .parent()
            .map(|p| p.join("fields.toml"))
            .unwrap_or_default();
        let has_sidecar = ext == "bin" && sidecars.contains(&sidecar);
        if has_sidecar {
            e.insert("format".into(), json!("bin+toml"));
            e.insert("schema".into(), json!(rel_str(&sidecar)));
        } else if !ext.is_empty() {
            e.insert("format".into(), json!(ext));
        }
        e.insert("bytes".into(), json!(meta.len()));
        if meta.len() < HASH_LIMIT {
            if hash {
                if let Some(h) = sha256_file(&full) {
                    e.insert("sha256".into(), json!(h));
                }
            }
        } else {
            e.insert(
                "note".into(),
                json!("larger than 64 MiB: sha256 not computed"),
            );
        }
        out.push(Value::Object(e));
    }
    let mut top = Map::new();
    top.insert("outputs".into(), Value::Array(out));
    if truncated {
        top.insert(
            "note".into(),
            json!(format!(
                "listing truncated at {MAX_OUTPUTS} files or depth {MAX_DEPTH}"
            )),
        );
    }
    Value::Object(top)
}

fn rel_str(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<PathBuf>, truncated: &mut bool) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        let path = e.path();
        let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            if depth + 1 >= MAX_DEPTH {
                *truncated = true;
            } else {
                walk(root, &path, depth + 1, out, truncated);
            }
        } else if ft.is_file() {
            if depth == 0
                && matches!(
                    name.as_str(),
                    "run.json" | "params.json" | "outputs.json" | "notes.json" | "events.jsonl"
                )
            {
                continue;
            }
            if out.len() >= MAX_OUTPUTS {
                *truncated = true;
                return;
            }
            out.push(rel);
        }
    }
}

fn kind_of(name: &str, ext: &str) -> &'static str {
    if name.contains("ckpt") || name.contains("checkpoint") || ext == "jls" {
        "checkpoint"
    } else if matches!(ext, "h5" | "hdf5" | "bin" | "npy" | "npz" | "vtk") {
        "snapshot"
    } else if matches!(ext, "tsv" | "csv" | "txt")
        && (name.contains("pk") || name.contains("spectr"))
    {
        "spectrum"
    } else if matches!(ext, "tsv" | "csv") {
        "series"
    } else if matches!(
        ext,
        "png" | "jpg" | "jpeg" | "pdf" | "svg" | "gif" | "mp4" | "mov"
    ) {
        "image"
    } else if ext == "log" {
        "log"
    } else {
        "other"
    }
}

fn sha256_file(p: &Path) -> Option<String> {
    let mut f = fs::File::open(p).ok()?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Some(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

// ---------------------------------------------------------------------------
// Small parsers
// ---------------------------------------------------------------------------

/// A TOML-ish reader that tolerates what `toml` refuses: Vespa's
/// `metadata.toml` files repeat keys. Yields `(section, key, value)` in file
/// order; the caller decides what a duplicate means.
fn parse_lenient(text: &str) -> Vec<(String, String, Value)> {
    let mut section = String::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if t.starts_with('[') {
            section = t.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            continue;
        }
        let Some((k, v)) = t.split_once('=') else {
            continue;
        };
        let key = k.trim().trim_matches('"').to_string();
        out.push((section.clone(), key, toml_value(v.trim())));
    }
    out
}

fn toml_value(v: &str) -> Value {
    if let Some(rest) = v.strip_prefix('"') {
        // up to the closing unescaped quote
        let mut end = None;
        let mut esc = false;
        for (i, c) in rest.char_indices() {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                end = Some(i);
                break;
            }
        }
        let body = &rest[..end.unwrap_or(rest.len())];
        return serde_json::from_str::<String>(&format!("\"{body}\""))
            .map(Value::String)
            .unwrap_or_else(|_| Value::String(body.to_string()));
    }
    if let Some(rest) = v.strip_prefix('\'') {
        return Value::String(rest.split('\'').next().unwrap_or("").to_string());
    }
    if v.starts_with('[') {
        if let Ok(a) = serde_json::from_str::<Value>(v) {
            return a;
        }
    }
    let bare = v.split('#').next().unwrap_or(v).trim();
    match bare {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => scalar_num(bare).unwrap_or_else(|| Value::String(bare.to_string())),
    }
}

/// An env-var string as a value: a number when it is plainly one, else text.
fn scalar(raw: &str) -> Value {
    scalar_num(raw).unwrap_or_else(|| Value::String(raw.to_string()))
}

fn scalar_num(s: &str) -> Option<Value> {
    let t = s.replace('_', "");
    if t.is_empty() || !t.chars().all(|c| "0123456789+-.eE".contains(c)) {
        return None;
    }
    let digits = t.trim_start_matches(['+', '-']);
    if digits.len() > 1 && digits.starts_with('0') && !digits.starts_with("0.") {
        return None; // "007" stays text
    }
    if let Ok(i) = t.parse::<i64>() {
        return Some(json!(i));
    }
    let f = t.parse::<f64>().ok().filter(|f| f.is_finite())?;
    serde_json::Number::from_f64(f).map(Value::Number)
}

fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&Utc));
    }
    for fmt in ["%Y%m%dT%H%M%SZ", "%Y%m%dT%H%M%S", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(n) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(n.and_utc());
        }
    }
    None
}

fn is_hex(s: &str) -> bool {
    (s.len() == 7 || s.len() == 40) && s.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
}

fn sanitize(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if t.is_empty() {
        "_".into()
    } else {
        t
    }
}

fn stem(p: &str) -> Option<String> {
    Path::new(p)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
}

fn looks_like_path(v: &str, folder: &str) -> bool {
    v.starts_with('/') || v.starts_with("~/") || (!folder.is_empty() && v.contains(folder))
}

fn julia_from_text(t: &str) -> Option<String> {
    let i = t.find("julia-")? + "julia-".len();
    let v: String = t[i..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let v = v.trim_end_matches('.').to_string();
    (v.contains('.')).then_some(v)
}

fn split_assign(tok: &str) -> Option<(String, String)> {
    let (k, v) = tok.split_once('=')?;
    let mut cs = k.chars();
    let first = cs.next()?;
    if !(first.is_ascii_alphabetic() || first == '_')
        || !cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    Some((k.to_string(), v.to_string()))
}

/// Shell word splitting with single quotes, double quotes and backslashes.
fn shell_split(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut live = false;
    let mut q: Option<char> = None;
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match (q, c) {
            (Some(qc), c) if c == qc => q = None,
            (Some('"'), '\\') => {
                if let Some(n) = it.next() {
                    cur.push(n);
                }
            }
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                q = Some(c);
                live = true;
            }
            (None, '\\') => {
                if let Some(n) = it.next() {
                    cur.push(n);
                    live = true;
                }
            }
            (None, c) if c.is_whitespace() => {
                if live || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    live = false;
                }
            }
            (None, c) => {
                cur.push(c);
                live = true;
            }
        }
    }
    if live || !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_a_shell() {
        assert_eq!(
            shell_split(r#"env A=1 B="x y" 'c d' e\ f"#),
            vec!["env", "A=1", "B=x y", "c d", "e f"]
        );
    }

    #[test]
    fn numbers_only_when_plainly_numbers() {
        assert_eq!(scalar("32"), json!(32));
        assert_eq!(scalar("9.1e-05"), json!(9.1e-05));
        assert_eq!(scalar("007"), json!("007"));
        assert_eq!(scalar("nan"), json!("nan"));
        assert_eq!(scalar("auto"), json!("auto"));
        assert_eq!(scalar("1,2"), json!("1,2"));
    }

    #[test]
    fn lenient_parse_keeps_duplicates() {
        let v = parse_lenient("a = 1\n[s]\nb = \"x\"\na = 2 # c\n");
        assert_eq!(v.len(), 3);
        assert_eq!(v[2], ("s".into(), "a".into(), json!(2)));
    }

    #[test]
    fn compact_times_parse() {
        assert!(parse_time("20260705T083045Z").is_some());
        assert!(parse_time("2026-07-13T09:03:29Z").is_some());
    }

    #[test]
    fn exit_sentinel_and_failures() {
        assert_eq!(exit_sentinel("x\n===== EXIT 0 =====\n"), Some(0));
        assert_eq!(exit_sentinel("===== EXIT 3 =====\n"), Some(3));
        let (c, l, _) = log_failure("ERROR: LoadError: Metal backend is not functional\n").unwrap();
        assert_eq!((c, l), (110, "backend-unavailable"));
    }
}
