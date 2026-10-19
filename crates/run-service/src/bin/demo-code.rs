//! `demo-code`: the one built-in conforming code, for proof
//! (docs/plan-run-convention.md; run-service work package R6). It is what
//! a code that follows the convention looks like from the outside, and
//! nothing else: a parameter schema on `--schema` (R-D2), an effective
//! parameter set with sources (R-D3), a run directory with `run.json`,
//! `params.json`, `events.jsonl`, `outputs.json` (R-D1, R-D4, R-D6, R-D7),
//! the control inbox (R-D8), and `--from` to resume from a checkpoint with
//! the mutability classes enforced in the setter (R-D2.3).
//!
//! ```text
//! demo-code --schema
//! demo-code --dir <run> [--params <file>] [--problem <json>] [--from <ckpt>]
//!           [--relaxed] [--<name>=<value>]...
//! ```
//!
//! The run: `demo.steps` steps, `demo.sleep_ms` apart; every `demo.cadence`
//! steps a `progress` and a `metric` line and one look at `control/`; at
//! `demo.fail_at` (when > 0) the run fails `320 nan-in-state` with
//! `invalidates_reuse: true`; at the end one `series` output and
//! `finished {code: 0}`.

use run_convention::{key_of, values_of};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::exit;

const NAME: &str = "demo-code";
const VERSION: &str = "0.1.0";

/// The declaration, once, where the values are read (R-D2.0). The schema
/// `--schema` emits is a view of this table.
struct Param {
    name: &'static str,
    ty: &'static str,
    default: Value,
    unit: Option<&'static str>,
    group: &'static str,
    mutable: &'static str,
    env: Option<&'static str>,
    advanced: bool,
    minimum: Option<f64>,
    doc: &'static str,
}

fn params_table() -> Vec<Param> {
    vec![
        Param {
            name: "demo.steps",
            ty: "integer",
            default: json!(10),
            unit: None,
            group: "run",
            mutable: "never",
            env: None,
            advanced: false,
            minimum: Some(1.0),
            doc: "how many steps the run takes",
        },
        Param {
            name: "demo.dt",
            ty: "number",
            default: json!(1.0),
            unit: Some("s"),
            group: "run",
            mutable: "restart",
            env: None,
            advanced: false,
            minimum: Some(0.0),
            doc: "the time step",
        },
        Param {
            name: "demo.sleep_ms",
            ty: "integer",
            default: json!(0),
            unit: Some("ms"),
            group: "run",
            mutable: "never",
            env: None,
            advanced: true,
            minimum: Some(0.0),
            doc: "wall-clock pause per step, so steering can be exercised",
        },
        Param {
            name: "demo.cadence",
            ty: "integer",
            default: json!(2),
            unit: None,
            group: "io",
            mutable: "always",
            env: Some("DEMO_CADENCE"),
            advanced: false,
            minimum: Some(1.0),
            doc: "every how many steps progress and metrics are written and the inbox is read",
        },
        Param {
            name: "demo.label",
            ty: "string",
            default: json!("demo"),
            unit: None,
            group: "io",
            mutable: "never",
            env: None,
            advanced: false,
            minimum: None,
            doc: "a label written into the series output",
        },
        Param {
            name: "demo.fail_at",
            ty: "integer",
            default: json!(0),
            unit: None,
            group: "failure",
            mutable: "never",
            env: None,
            advanced: true,
            minimum: Some(0.0),
            doc: "the step at which the run fails with exit 320 (nan-in-state); 0 never fails",
        },
    ]
}

fn schema() -> Value {
    let mut props = Map::new();
    for p in params_table() {
        let mut x = Map::new();
        if let Some(u) = p.unit {
            x.insert("unit".into(), json!(u));
        }
        x.insert("group".into(), json!(p.group));
        x.insert("mutable".into(), json!(p.mutable));
        if let Some(e) = p.env {
            x.insert("env".into(), json!(e));
        }
        if p.advanced {
            x.insert("advanced".into(), json!(true));
        }
        let mut prop = Map::new();
        prop.insert("type".into(), json!(p.ty));
        prop.insert("default".into(), p.default.clone());
        prop.insert("description".into(), json!(p.doc));
        if let Some(m) = p.minimum {
            prop.insert("minimum".into(), json!(m));
        }
        prop.insert("x-run".into(), Value::Object(x));
        props.insert(p.name.into(), Value::Object(prop));
    }
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": NAME,
        "description": "the built-in demo code of run-service: a counted loop that writes a conforming run",
        "type": "object",
        "additionalProperties": false,
        "properties": props,
        "x-run": {
            "metrics": [
                {"name": "energy", "unit": "", "description": "1/step, so it falls"}
            ],
            "groups": {
                "run": {"description": "how long and how fast"},
                "io": {"description": "what is written and when"},
                "failure": {"description": "make the run fail on purpose"}
            }
        }
    })
}

fn fail(code: i32, msg: &str) -> ! {
    eprintln!("{NAME}: {msg}");
    exit(code)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// One parsed value from the command line or the environment: JSON when it
/// parses, else the string itself.
fn parse_value(ty: &str, raw: &str) -> Value {
    if ty == "string" {
        return json!(raw);
    }
    serde_json::from_str(raw).unwrap_or_else(|_| json!(raw))
}

fn type_ok(ty: &str, v: &Value) -> bool {
    match ty {
        "integer" => v.is_i64() || v.is_u64(),
        "number" => v.is_number(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        _ => true,
    }
}

struct Cli {
    dir: Option<PathBuf>,
    params_file: Option<PathBuf>,
    problem: Option<Value>,
    from: Option<PathBuf>,
    relaxed: bool,
    sets: Vec<(String, String)>,
}

fn parse_cli() -> Cli {
    let mut cli = Cli {
        dir: None,
        params_file: None,
        problem: None,
        from: None,
        relaxed: false,
        sets: Vec::new(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--schema" => {
                println!("{}", serde_json::to_string_pretty(&schema()).unwrap());
                exit(0)
            }
            "--dir" => cli.dir = args.next().map(PathBuf::from),
            "--params" => cli.params_file = args.next().map(PathBuf::from),
            "--from" => cli.from = args.next().map(PathBuf::from),
            "--relaxed" => cli.relaxed = true,
            "--problem" => {
                let text = args.next().unwrap_or_default();
                cli.problem = Some(
                    serde_json::from_str(&text)
                        .unwrap_or_else(|e| fail(2, &format!("--problem is not JSON: {e}"))),
                );
            }
            other => match other.strip_prefix("--").and_then(|s| s.split_once('=')) {
                Some((k, v)) => cli.sets.push((k.to_string(), v.to_string())),
                None => fail(2, &format!("unknown argument `{other}`")),
            },
        }
    }
    cli
}

/// Resolve defaults → file → env → argv (R-D3, R-D11), refusing an unknown
/// name unless relaxed (R-D2.5).
fn resolve(cli: &Cli, table: &[Param]) -> BTreeMap<String, (Value, String)> {
    let mut out: BTreeMap<String, (Value, String)> = table
        .iter()
        .map(|p| (p.name.to_string(), (p.default.clone(), "default".into())))
        .collect();
    let known = |name: &str| table.iter().find(|p| p.name == name);
    let unknown = |name: &str, where_: &str| {
        if cli.relaxed {
            eprintln!("{NAME}: warning: unknown parameter `{name}` ({where_}) ignored (relaxed)");
        } else {
            fail(
                2,
                &format!(
                    "unknown parameter `{name}` ({where_}); the declared names are {}",
                    { table.iter().map(|p| p.name).collect::<Vec<_>>().join(", ") }
                ),
            )
        }
    };
    if let Some(file) = &cli.params_file {
        let text = fs::read_to_string(file)
            .unwrap_or_else(|e| fail(2, &format!("read {}: {e}", file.display())));
        let v: Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| fail(2, &format!("{}: not JSON: {e}", file.display())));
        let Some(obj) = v.as_object() else {
            fail(2, "--params must be an object")
        };
        for (k, e) in obj {
            // Either `{value, source}` (a params.json) or a bare value.
            let value = e.get("value").cloned().unwrap_or_else(|| e.clone());
            match known(k) {
                Some(p) if type_ok(p.ty, &value) => {
                    out.insert(k.clone(), (value, format!("file:{}", file.display())));
                }
                Some(p) => fail(2, &format!("`{k}` must be {} (file)", p.ty)),
                None => unknown(k, "file"),
            }
        }
    }
    for p in table {
        if let Some(env) = p.env {
            if let Ok(raw) = std::env::var(env) {
                let value = parse_value(p.ty, &raw);
                if !type_ok(p.ty, &value) {
                    fail(2, &format!("`{}` must be {} (env {env})", p.name, p.ty));
                }
                out.insert(p.name.into(), (value, format!("env:{env}")));
            }
        }
    }
    for (k, raw) in &cli.sets {
        match known(k) {
            Some(p) => {
                let value = parse_value(p.ty, raw);
                if !type_ok(p.ty, &value) {
                    fail(2, &format!("`{k}` must be {} (cli)", p.ty));
                }
                out.insert(k.clone(), (value, "cli".into()));
            }
            None => unknown(k, "cli"),
        }
    }
    out
}

struct Run {
    dir: PathBuf,
    id: String,
    seq: u64,
    outputs: Vec<Value>,
    run: Value,
}

impl Run {
    fn event(&mut self, mut body: Value) {
        self.seq += 1;
        let obj = body.as_object_mut().expect("event object");
        obj.insert("seq".into(), json!(self.seq));
        obj.insert("t".into(), json!(now()));
        let line = serde_json::to_string(&body).unwrap() + "\n";
        let mut f = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(self.dir.join("events.jsonl"))
            .unwrap_or_else(|e| fail(1, &format!("events.jsonl: {e}")));
        // One write per line (R-D6.6).
        f.write_all(line.as_bytes())
            .unwrap_or_else(|e| fail(1, &format!("events.jsonl: {e}")));
    }

    fn write_outputs(&self) {
        write_json(
            &self.dir.join("outputs.json"),
            &json!({"outputs": self.outputs}),
        );
    }

    fn write_run(&self) {
        write_json(&self.dir.join("run.json"), &self.run);
    }

    fn output(&mut self, path: &str, kind: &str, format: &str, step: u64, bytes: &[u8]) {
        fs::write(self.dir.join(path), bytes).unwrap_or_else(|e| fail(1, &format!("{path}: {e}")));
        let sha = sha256_hex(bytes);
        self.outputs.push(json!({
            "path": path, "kind": kind, "format": format, "step": step,
            "sha256": sha, "bytes": bytes.len()
        }));
        self.write_outputs();
        if kind == "checkpoint" {
            self.event(json!({"kind": "checkpoint", "path": path, "sha256": sha, "step": step}));
        } else {
            self.event(json!({"kind": "output", "path": path, "kind_of": kind, "step": step}));
        }
    }

    fn finish(&mut self, state: &str, exit_: Value) -> ! {
        self.event(json!({"kind": "finished", "exit": exit_}));
        let status = self.run["status"].as_object_mut().unwrap();
        status.insert("state".into(), json!(state));
        status.insert("ended_at".into(), json!(now()));
        status.insert("exit".into(), exit_.clone());
        status.insert("last_event_seq".into(), json!(self.seq));
        self.write_run();
        exit(0)
    }
}

fn write_json(path: &Path, v: &Value) {
    let text = serde_json::to_string_pretty(v).unwrap() + "\n";
    fs::write(path, text).unwrap_or_else(|e| fail(1, &format!("{}: {e}", path.display())));
}

/// The control inbox (R-D8): act on each request, append the event naming
/// it, move the file to `done/` or `refused/`. Returns `true` when a `stop`
/// was honoured (the caller finishes).
fn poll_control(
    run: &mut Run,
    values: &mut BTreeMap<String, (Value, String)>,
    table: &[Param],
    step: u64,
) -> bool {
    let inbox = run.dir.join("control");
    let Ok(rd) = fs::read_dir(&inbox) else {
        return false;
    };
    let mut files: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    let mut stop = false;
    for file in files {
        let Ok(text) = fs::read_to_string(&file) else {
            continue;
        };
        let Ok(mut body) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let id = file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let request = body["request"].as_str().unwrap_or("").to_string();
        let mut refused: Option<String> = None;
        match request.as_str() {
            "set" => {
                let param = body["param"].as_str().unwrap_or("").to_string();
                let value = body["value"].clone();
                match table.iter().find(|p| p.name == param) {
                    // Matched exactly on the class (R-D2.3).
                    Some(p) if p.mutable == "always" => {
                        if !type_ok(p.ty, &value) {
                            refused = Some(format!("`{param}` must be {}", p.ty));
                        } else {
                            let from = values
                                .get(&param)
                                .map(|(v, _)| v.clone())
                                .unwrap_or(Value::Null);
                            values.insert(param.clone(), (value.clone(), format!("control:{id}")));
                            run.event(json!({
                                "kind": "changed", "param": param, "from": from, "to": value,
                                "by": format!("control:{id}"), "step": step
                            }));
                        }
                    }
                    Some(p) => {
                        refused = Some(format!(
                            "`{param}` is `{}`, not `always`; it can only change on a restart",
                            p.mutable
                        ))
                    }
                    None => refused = Some(format!("unknown parameter `{param}`")),
                }
            }
            "checkpoint" => {
                checkpoint(run, values, step);
            }
            "stop" => {
                if body["after"].as_str() == Some("checkpoint") {
                    checkpoint(run, values, step);
                }
                stop = true;
            }
            other => refused = Some(format!("`{other}` is not supported by {NAME}")),
        }
        let (sub, extra) = match &refused {
            Some(reason) => ("refused", Some(reason.clone())),
            None => ("done", None),
        };
        let target = inbox.join(sub);
        let _ = fs::create_dir_all(&target);
        if let Some(reason) = extra {
            body["reason"] = json!(reason);
        }
        body["id"] = json!(id);
        let _ = fs::write(
            target.join(format!("{id}.json")),
            serde_json::to_string(&body).unwrap(),
        );
        let _ = fs::remove_file(&file);
        if stop {
            break;
        }
    }
    stop
}

fn checkpoint(run: &mut Run, values: &BTreeMap<String, (Value, String)>, step: u64) {
    let vals: Map<String, Value> = values
        .iter()
        .map(|(k, (v, _))| (k.clone(), v.clone()))
        .collect();
    let body = serde_json::to_vec(&json!({"step": step, "run": run.id, "params": vals})).unwrap();
    let path = format!("ckpt_{step:06}.json");
    run.output(&path, "checkpoint", "json", step, &body);
}

fn main() {
    let cli = parse_cli();
    let table = params_table();
    let Some(dir) = cli.dir.clone() else {
        fail(2, "--dir <run directory> is required (or --schema)")
    };
    let mut values = resolve(&cli, &table);

    // Resume (R-D12 `resume(dir; from)`): the child asserts its parent
    // (R-D4.2) and `never` refuses a different value (R-D2.3).
    let mut start_step = 0u64;
    let mut lineage = json!({});
    let mut restart_changes: Vec<Value> = Vec::new();
    if let Some(from) = &cli.from {
        let bytes =
            fs::read(from).unwrap_or_else(|e| fail(2, &format!("--from {}: {e}", from.display())));
        let ckpt: Value = serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            fail(
                2,
                &format!("--from {}: not a checkpoint: {e}", from.display()),
            )
        });
        start_step = ckpt["step"].as_u64().unwrap_or(0);
        if let Some(old) = ckpt["params"].as_object() {
            for p in &table {
                let was = old.get(p.name).cloned().unwrap_or(p.default.clone());
                let is = values
                    .get(p.name)
                    .map(|(v, _)| v.clone())
                    .unwrap_or(Value::Null);
                if was != is {
                    if p.mutable == "never" {
                        fail(
                            2,
                            &format!(
                                "`{}` is `never`: the checkpoint has {was}, the resume asks {is}",
                                p.name
                            ),
                        );
                    }
                    restart_changes.push(json!({
                        "kind": "changed", "param": p.name, "from": was, "to": is,
                        "by": "restart", "step": start_step
                    }));
                }
            }
        }
        lineage = json!({"parent": {
            "run": ckpt["run"],
            "relation": "resume",
            "checkpoint": {"path": from.to_string_lossy(), "sha256": sha256_hex(&bytes)}
        }});
    }

    fs::create_dir_all(&dir).unwrap_or_else(|e| fail(1, &format!("{}: {e}", dir.display())));
    let problem = cli
        .problem
        .clone()
        .unwrap_or_else(|| json!({"name": "demo", "version": "1"}));
    let params: Map<String, Value> = values
        .iter()
        .map(|(k, (v, s))| (k.clone(), json!({"value": v, "source": s})))
        .collect();
    let params = Value::Object(params);
    let key = key_of(&problem, &values_of(&params));
    let id = uuid::Uuid::new_v4().to_string();
    let argv: Vec<String> = std::env::args().collect();
    let run_json = json!({
        "convention": "run@1",
        "id": id,
        "key": key,
        "name": values.get("demo.label").map(|(v, _)| v.clone()).unwrap_or(json!("demo")),
        "code": {"name": NAME, "version": VERSION, "git_available": false},
        "problem": problem,
        "host": {
            "hostname": std::env::var("HOSTNAME").unwrap_or_else(|_| "local".into()),
            "os": std::env::consts::OS,
            "argv": argv
        },
        "lineage": lineage,
        "status": {"state": "running", "started_at": now(), "last_event_seq": 0}
    });
    // params.json before the first step and never again (R-D3).
    write_json(&dir.join("params.json"), &params);
    let mut run = Run {
        dir: dir.clone(),
        id,
        seq: 0,
        outputs: Vec::new(),
        run: run_json,
    };
    run.write_run();
    run.write_outputs();
    run.event(json!({"kind": "started"}));
    for c in restart_changes {
        run.event(c);
    }

    let int = |values: &BTreeMap<String, (Value, String)>, name: &str| -> i64 {
        values.get(name).and_then(|(v, _)| v.as_i64()).unwrap_or(0)
    };
    let steps = int(&values, "demo.steps").max(0) as u64;
    let dt = values
        .get("demo.dt")
        .and_then(|(v, _)| v.as_f64())
        .unwrap_or(1.0);
    let sleep_ms = int(&values, "demo.sleep_ms").max(0) as u64;
    let fail_at = int(&values, "demo.fail_at").max(0) as u64;

    let mut step = start_step;
    while step < steps {
        step += 1;
        if sleep_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
        }
        let cadence = int(&values, "demo.cadence").max(1) as u64;
        if step.is_multiple_of(cadence) || step == steps {
            run.event(json!({
                "kind": "progress", "step": step, "time": step as f64 * dt, "dt": dt,
                "fraction": step as f64 / steps as f64
            }));
            run.event(json!({
                "kind": "metric", "step": step, "values": {"energy": 1.0 / step as f64}
            }));
            if poll_control(&mut run, &mut values, &table, step) {
                run.finish(
                    "cancelled",
                    json!({"code": 110, "label": "stopped", "message": "stopped by request"}),
                );
            }
        }
        if fail_at > 0 && step == fail_at {
            run.event(json!({"kind": "warning", "message": "state went non-finite", "step": step}));
            run.finish(
                "failed",
                json!({"code": 320, "label": "nan-in-state",
                       "message": format!("nan in state at step {step}"),
                       "invalidates_reuse": true}),
            );
        }
    }
    let label = values
        .get("demo.label")
        .and_then(|(v, _)| v.as_str())
        .unwrap_or("demo")
        .to_string();
    let series = serde_json::to_vec_pretty(&json!({
        "label": label, "steps": steps,
        "energy": (1..=steps).map(|s| 1.0 / s as f64).collect::<Vec<_>>()
    }))
    .unwrap();
    run.output("series.json", "series", "json", steps, &series);
    run.finish("done", json!({"code": 0, "label": "ok"}));
}
