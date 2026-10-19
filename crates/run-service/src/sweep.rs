//! A sweep is a record of its own (R-D10) and, in koine, a koine-tasks
//! workflow (R-D13): `sweep.json` written to a directory, its `vary`
//! expanded into one `run-service_start` task per run (fan-out), and one
//! parent job whose ring records each run as it starts and which finishes
//! when the last run has (fan-in, done by the last `start` itself).
//!
//! Generators: `values` (a list), `linspace` and `logspace`
//! (`{start, stop, n}`), and `grid` (`{start, stop, step}`). A `sampler`
//! needs a sampling library this crate does not carry and is refused
//! `unsupported`, by name, rather than drawn with a stand-in RNG.

use std::fs;
use std::path::{Path, PathBuf};

use impress_item::item::ItemId;
use impress_item::store::AttributedStore;
use impress_service_core::pipeline::CallerIdentity;
use koine_tasks::job;
use koine_tasks::service::{file_verb_task, VerbTaskDraft};
use koine_tasks::TaskState;
use run_convention::schemas;
use serde_json::{json, Map, Value};

use crate::codes::CodeProvider;
use crate::dto::Refused;
use crate::kind::AUTHOR;

/// What `sweep` filed.
#[derive(Debug, Clone)]
pub struct Filed {
    pub id: String,
    pub dir: PathBuf,
    pub job: ItemId,
    pub tasks: Vec<ItemId>,
    pub planned: usize,
}

fn number(v: &Value) -> Option<f64> {
    v.as_f64()
}

/// The values one `vary` entry takes.
fn values_of(entry: &Value) -> Result<Vec<Value>, Refused> {
    let param = entry.get("param").and_then(Value::as_str).unwrap_or("?");
    if let Some(values) = entry.get("values").and_then(Value::as_array) {
        return Ok(values.clone());
    }
    let generator = entry.get("generator").ok_or_else(|| {
        Refused::invalid(format!("vary `{param}`: give `values` or a `generator`"))
    })?;
    let kind = generator.get("kind").and_then(Value::as_str).unwrap_or("");
    let field = |name: &str| -> Result<f64, Refused> {
        generator.get(name).and_then(number).ok_or_else(|| {
            Refused::invalid(format!("vary `{param}`: `{kind}` needs a numeric `{name}`"))
        })
    };
    match kind {
        "linspace" | "logspace" => {
            let (start, stop) = (field("start")?, field("stop")?);
            let n = generator.get("n").and_then(Value::as_u64).unwrap_or(0);
            if n < 2 {
                return Err(Refused::invalid(format!("vary `{param}`: `{kind}` needs `n` >= 2")));
            }
            if kind == "logspace" && (start <= 0.0 || stop <= 0.0) {
                return Err(Refused::invalid(format!("vary `{param}`: logspace needs positive bounds")));
            }
            Ok((0..n)
                .map(|i| {
                    let t = i as f64 / (n - 1) as f64;
                    let x = if kind == "linspace" {
                        start + t * (stop - start)
                    } else {
                        (start.ln() + t * (stop.ln() - start.ln())).exp()
                    };
                    json!(x)
                })
                .collect())
        }
        "grid" => {
            let (start, stop, step) = (field("start")?, field("stop")?, field("step")?);
            if step <= 0.0 || stop < start {
                return Err(Refused::invalid(format!("vary `{param}`: grid needs step > 0 and stop >= start")));
            }
            let n = ((stop - start) / step).floor() as u64 + 1;
            if n > 10_000 {
                return Err(Refused::invalid(format!("vary `{param}`: a grid of {n} points is more than 10000")));
            }
            Ok((0..n).map(|i| json!(start + i as f64 * step)).collect())
        }
        "sampler" => Err(Refused::new(
            "unsupported",
            format!("vary `{param}`: `sampler` generators are not supported by this host; give `values`, `linspace`, `logspace` or `grid`"),
        )),
        other => Err(Refused::invalid(format!("vary `{param}`: unknown generator kind `{other}`"))),
    }
}

/// Expand `sweep.json`'s `base` and `vary` into one parameter object per
/// run (`combine`: `product` by default, or `zip`).
pub fn expand(sweep: &Value) -> Result<Vec<Map<String, Value>>, Refused> {
    let base = sweep
        .get("base")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let vary = sweep
        .get("vary")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut axes: Vec<(String, Vec<Value>)> = Vec::new();
    for entry in &vary {
        let param = entry
            .get("param")
            .and_then(Value::as_str)
            .ok_or_else(|| Refused::invalid("every `vary` entry names a `param`"))?;
        let values = values_of(entry)?;
        if values.is_empty() {
            return Err(Refused::invalid(format!("vary `{param}` has no values")));
        }
        axes.push((param.to_string(), values));
    }
    let combine = sweep
        .get("combine")
        .and_then(Value::as_str)
        .unwrap_or("product");
    let mut runs: Vec<Map<String, Value>> = Vec::new();
    match combine {
        "zip" => {
            let n = axes.iter().map(|(_, v)| v.len()).max().unwrap_or(0);
            if axes.iter().any(|(_, v)| v.len() != n) {
                return Err(Refused::invalid(
                    "`zip` needs every varied parameter to have the same number of values",
                ));
            }
            for i in 0..n {
                let mut p = base.clone();
                for (param, values) in &axes {
                    p.insert(param.clone(), values[i].clone());
                }
                runs.push(p);
            }
        }
        "product" => {
            runs.push(base.clone());
            for (param, values) in &axes {
                let mut next = Vec::with_capacity(runs.len() * values.len());
                for p in &runs {
                    for v in values {
                        let mut q = p.clone();
                        q.insert(param.clone(), v.clone());
                        next.push(q);
                    }
                }
                runs = next;
            }
            if axes.is_empty() {
                runs.clear();
            }
        }
        other => {
            return Err(Refused::invalid(format!(
                "`combine` is `product` or `zip`, not `{other}`"
            )))
        }
    }
    if runs.len() > 10_000 {
        return Err(Refused::invalid(format!(
            "the sweep expands to {} runs, more than 10000",
            runs.len()
        )));
    }
    Ok(runs)
}

/// Validate a sweep against `sweep.schema.json` (with `id` and `runs`, which
/// koine fills, allowed absent).
pub fn validate(sweep: &Value) -> Result<(), Refused> {
    let mut schema: Value = serde_json::from_str(schemas::SWEEP).expect("bundled schema is JSON");
    if let Some(req) = schema.get_mut("required").and_then(Value::as_array_mut) {
        req.retain(|r| r != "id" && r != "runs");
    }
    let validator = jsonschema::options()
        .build(&schema)
        .expect("bundled schema compiles");
    if let Some(e) = validator.iter_errors(sweep).next() {
        let at = e.instance_path.to_string();
        return Err(Refused::invalid(format!(
            "sweep{}: {e}",
            if at.is_empty() {
                String::new()
            } else {
                format!(" {at}")
            }
        )));
    }
    Ok(())
}

/// Write `sweep.json`, create the parent job, file one `start` task per
/// run. Every run's parameters are checked against the code's schema first
/// (R-D2.5), so a sweep with one misspelt name files nothing.
pub fn file(
    store: &dyn AttributedStore,
    code: &CodeProvider,
    mut sweep: Value,
    dir: &Path,
    caller: &CallerIdentity,
) -> Result<Filed, Refused> {
    validate(&sweep)?;
    let runs = expand(&sweep)?;
    if runs.is_empty() {
        return Err(Refused::invalid(
            "the sweep expands to no run: `vary` is empty",
        ));
    }
    for (i, p) in runs.iter().enumerate() {
        code.resolve(p)
            .map_err(|e| Refused::invalid(format!("run {i} of the sweep: {e}")))?;
    }
    if dir.join("sweep.json").exists() {
        return Err(Refused::new(
            "conflict",
            format!("{} already holds a sweep.json", dir.display()),
        ));
    }
    fs::create_dir_all(dir).map_err(|e| Refused::new("io", format!("{}: {e}", dir.display())))?;
    let dir = dir
        .canonicalize()
        .map_err(|e| Refused::new("io", format!("{}: {e}", dir.display())))?;
    let id = sweep
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let job = job::create_job(
        store,
        "run-service_sweep",
        &json!({"sweep": id, "dir": dir.to_string_lossy(), "code": code.name, "planned": runs.len()}),
        &job::inline_runner_name(),
        AUTHOR,
    )
    .map_err(|e| Refused::new("store-error", e.to_string()))?;
    {
        let o = sweep.as_object_mut().expect("validated as an object");
        o.insert("convention".into(), json!("run@1"));
        o.insert("id".into(), json!(id));
        o.insert("code".into(), json!(code.name));
        o.entry("created_at")
            .or_insert_with(|| json!(chrono::Utc::now().to_rfc3339()));
        o.insert("runs".into(), json!([]));
        o.insert(
            "koine".into(),
            json!({"job": job.to_string(), "planned": runs.len()}),
        );
    }
    write_sweep(&dir, &sweep)?;
    let mut tasks = Vec::with_capacity(runs.len());
    for (i, params) in runs.iter().enumerate() {
        let run_dir = dir.join(format!("run-{i:04}"));
        let draft = VerbTaskDraft {
            verb: "run-service_start".into(),
            args: json!({
                "code": code.name,
                "params": params,
                "dir": run_dir.to_string_lossy(),
                "name": format!("{} {i}", sweep.get("name").and_then(Value::as_str).unwrap_or(&id[..8])),
                "sweep": id,
                "wait": true,
            }),
            depends_on: vec![],
            not_before_ms: None,
            title: Some(format!(
                "run-service_start {} run {i} of {}",
                code.name,
                runs.len()
            )),
            retry_of: None,
        };
        let item = file_verb_task(store, draft, caller).map_err(|(code, message)| {
            Refused::new(
                if code == "not-found" {
                    "not-found"
                } else {
                    "store-error"
                },
                message,
            )
        })?;
        tasks.push(item.id);
        job::append_event(
            store,
            job,
            "filed",
            &json!({"index": i, "task": item.id.to_string(), "params": params}),
            AUTHOR,
        )
        .map_err(|e| Refused::new("store-error", e.to_string()))?;
    }
    Ok(Filed {
        id,
        dir,
        job,
        tasks,
        planned: runs.len(),
    })
}

fn write_sweep(dir: &Path, sweep: &Value) -> Result<(), Refused> {
    let text = serde_json::to_string_pretty(sweep).unwrap_or_default() + "\n";
    let tmp = dir.join(".sweep.json.tmp");
    fs::write(&tmp, text).map_err(|e| Refused::new("io", format!("{}: {e}", tmp.display())))?;
    fs::rename(&tmp, dir.join("sweep.json"))
        .map_err(|e| Refused::new("io", format!("sweep.json: {e}")))?;
    Ok(())
}

/// The fan-in, run by every `start` that was given `sweep`: record the run
/// in `sweep.json`'s `runs` and on the parent job's ring; when the planned
/// number has finished, finish the job with every run's id and state.
pub fn note_run(
    store: &dyn AttributedStore,
    run_dir: &Path,
    sweep_id: &str,
    run_id: &str,
    state: &str,
) -> Result<(), Refused> {
    let Some(dir) = run_dir.parent() else {
        return Ok(());
    };
    let path = dir.join("sweep.json");
    let Ok(text) = fs::read_to_string(&path) else {
        return Err(Refused::not_found(format!(
            "{}: no sweep.json beside the run",
            dir.display()
        )));
    };
    let mut sweep: Value =
        serde_json::from_str(&text).map_err(|e| Refused::new("io", format!("sweep.json: {e}")))?;
    if sweep.get("id").and_then(Value::as_str) != Some(sweep_id) {
        return Err(Refused::invalid(format!(
            "{} is sweep {}, not {sweep_id}",
            path.display(),
            sweep.get("id").and_then(Value::as_str).unwrap_or("?")
        )));
    }
    let runs = sweep
        .get_mut("runs")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| Refused::invalid("sweep.json has no `runs`"))?;
    if !runs.iter().any(|r| r.as_str() == Some(run_id)) {
        runs.push(json!(run_id));
    }
    let finished: Vec<Value> = sweep
        .pointer("/koine/finished")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut finished: Vec<Value> = finished
        .into_iter()
        .filter(|f| f.get("run").and_then(Value::as_str) != Some(run_id))
        .collect();
    finished.push(json!({"run": run_id, "state": state}));
    let planned = sweep
        .pointer("/koine/planned")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let job: Option<ItemId> = sweep
        .pointer("/koine/job")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok());
    let complete = planned > 0 && finished.len() >= planned;
    if let Some(k) = sweep.get_mut("koine").and_then(Value::as_object_mut) {
        k.insert("finished".into(), json!(finished));
        if complete {
            k.insert("finished_at".into(), json!(chrono::Utc::now().to_rfc3339()));
        }
    }
    write_sweep(dir, &sweep)?;
    if let Some(job_id) = job {
        job::append_event(
            store,
            job_id,
            "run",
            &json!({"run": run_id, "state": state, "finished": finished.len(), "planned": planned}),
            AUTHOR,
        )
        .map_err(|e| Refused::new("store-error", e.to_string()))?;
        if complete {
            let failed = finished
                .iter()
                .filter(|f| f.get("state").and_then(Value::as_str) != Some("done"))
                .count();
            let result = json!({"sweep": sweep_id, "dir": dir.to_string_lossy(), "runs": finished});
            let to = if failed == 0 {
                TaskState::Done
            } else {
                TaskState::Failed
            };
            let error =
                (failed > 0).then(|| format!("{failed} of {planned} runs did not finish done"));
            // A second finisher (two last runs racing) loses the transition;
            // the job is already terminal, which is what both wanted.
            if let Err(e) = job::finish_job(store, job_id, to, &result, error.as_deref(), AUTHOR) {
                tracing::debug!(job = %job_id, "sweep job already finished: {e}");
            }
        }
    }
    Ok(())
}
