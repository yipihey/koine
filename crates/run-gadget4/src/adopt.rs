//! `gadget4-adapt adopt`: the R5-style import (R-D11) for a run GADGET-4 already
//! did. Written beside the files, nothing moved: `parameters-usedvalues` becomes
//! `params.json`, `info.txt` becomes events, snapshots and restart files become
//! outputs, and the record says `provenance: imported`.
//!
//! What the files cannot say is stated rather than invented: event times are
//! interpolated between the run's first and last file (each such event carries
//! `t_source`), and without the run's stdout the way it ended is inferred.

use crate::echo;
use crate::error::{io, Error};
use crate::exitmap::{classify, Ending, Exit};
use crate::info;
use crate::post;
use crate::render;
use crate::rundir::{self, output_entry, rel, RunWriter};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AdoptOptions {
    pub name: Option<String>,
    pub problem: String,
    /// GADGET-4's stdout, if kept: lets the exit be classified instead of inferred.
    pub log: Option<PathBuf>,
}

impl Default for AdoptOptions {
    fn default() -> Self {
        AdoptOptions {
            name: None,
            problem: "gadget4".into(),
            log: None,
        }
    }
}

fn mtime(p: &Path) -> Option<DateTime<Utc>> {
    fs::metadata(p)
        .ok()?
        .modified()
        .ok()
        .map(DateTime::<Utc>::from)
}

struct Ev {
    t: DateTime<Utc>,
    kind: &'static str,
    fields: Value,
}

pub fn adopt(dir: &Path, opts: &AdoptOptions) -> Result<Exit, Error> {
    let uv_path = dir.join("parameters-usedvalues");
    let uv_text = io(
        format!(
            "read {} (the output directory of a GADGET-4 run)",
            uv_path.display()
        ),
        fs::read_to_string(&uv_path),
    )?;
    let echo_uv = echo::parse(&uv_text);
    let params = render::params_from_echo(&echo_uv, "usedvalues:parameters-usedvalues")
        .map_err(Error::Invalid)?;
    let values = render::values(&params);

    let snapshot_base = values
        .get("io.SnapshotFileBase")
        .and_then(|v| v.as_str())
        .unwrap_or("snapshot")
        .to_string();

    // Inventory.
    let mut snaps: Vec<PathBuf> = Vec::new();
    let mut others: Vec<PathBuf> = Vec::new();
    for e in io("list output dir", fs::read_dir(dir))?.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if post::is_snapshot_file(&name, &snapshot_base) {
            snaps.push(p);
        } else if !matches!(
            name.as_str(),
            "run.json"
                | "params.json"
                | "events.jsonl"
                | "outputs.json"
                | "notes.json"
                | "stop"
                | "restart"
                | "cont"
        ) {
            others.push(p);
        }
    }
    snaps.sort();
    others.sort();
    let mut restarts: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = fs::read_dir(dir.join("restartfiles")) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with("restart.") {
                restarts.push(e.path());
            }
        }
    }
    restarts.sort();

    // Times: from the first to the last file the run wrote.
    let start = mtime(&uv_path).unwrap_or_else(Utc::now);
    let last = snaps
        .iter()
        .chain(&others)
        .chain(&restarts)
        .filter_map(|p| mtime(p))
        .max()
        .unwrap_or(start)
        .max(start);

    // info.txt -> progress.
    let info_path = dir.join("info.txt");
    let mut points = Vec::new();
    if let Ok(text) = fs::read_to_string(&info_path) {
        for l in text.lines() {
            if let Some(sp) = info::parse_sync_point(l) {
                points.push(sp);
            }
        }
    }
    let begin = values.get("time.TimeBegin").and_then(Value::as_f64);
    let max = values.get("time.TimeMax").and_then(Value::as_f64);
    let comoving = values
        .get("time.ComovingIntegrationOn")
        .and_then(Value::as_i64)
        == Some(1);

    let mut evs: Vec<Ev> = Vec::new();
    let span = (last - start).num_milliseconds().max(0);
    let n = points.len().max(1) as i64;
    for (i, sp) in points.iter().enumerate() {
        let t = start + chrono::Duration::milliseconds(span * (i as i64 + 1) / n);
        let mut f =
            json!({"step": sp.step, "time": sp.time, "dt": sp.dt, "t_source": "interpolated"});
        if let (Some(b), Some(m)) = (begin, max) {
            if let Some(fr) = info::fraction(sp.time, b, m, comoving) {
                f["fraction"] = json!(fr);
            }
        }
        if let Some(z) = sp.redshift {
            f["redshift"] = json!(z);
        }
        evs.push(Ev {
            t,
            kind: "progress",
            fields: f,
        });
    }

    // Outputs.
    let info_h5 = snaps.first().and_then(|p| post::read_snapshot(p).ok());
    let mut index: Vec<Value> = Vec::new();
    for p in &snaps {
        let relp = rel(dir, p);
        let time = post::snapshot_number(&relp).and_then(|_| {
            // the snapshot's own Header Time, if h5dump can read it
            crate::h5::read_group(p, "Header")
                .ok()
                .and_then(|h| h.get("Time").and_then(Value::as_f64))
        });
        index.push(output_entry(dir, &relp, "snapshot", "hdf5", None, time)?);
        let t = mtime(p).unwrap_or(last);
        evs.push(Ev {
            t,
            kind: "output",
            fields: json!({"path": relp, "kind_of": "snapshot"}),
        });
    }
    for p in &restarts {
        let relp = rel(dir, p);
        let e = output_entry(dir, &relp, "checkpoint", "gadget4-restart", None, None)?;
        let sha = e["sha256"].clone();
        index.push(e);
        // `step` is required by the event schema; the last known sync point is
        // the best this file can say.
        let step = points.last().map(|s| s.step).unwrap_or(0);
        evs.push(Ev {
            t: mtime(p).unwrap_or(last),
            kind: "checkpoint",
            fields: json!({"path": relp, "sha256": sha, "step": step}),
        });
    }
    for p in &others {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let (kind, format) = match name {
            "parameters-usedvalues" | "param.txt-usedvalues" => ("other", "gadget4-param"),
            _ => post::classify_file(name),
        };
        index.push(output_entry(dir, &rel(dir, p), kind, format, None, None)?);
    }

    // The ending.
    let finished_normally = points
        .last()
        .zip(max)
        .map(|(sp, m)| value_reached(sp.time, m))
        .unwrap_or(false);
    let exit = match &opts.log {
        Some(lp) => {
            let log = io(format!("read log {}", lp.display()), fs::read_to_string(lp))?;
            classify(&Ending {
                log: &log,
                status: Some(0),
                stop_requested: false,
                cont_file: dir.join("cont").exists(),
            })
        }
        None if finished_normally => Exit {
            code: 0,
            label: "ok",
            message: Some("inferred: the last sync point reached TimeMax (no log was given)".into()),
            invalidates_reuse: false,
            state: "done",
        },
        None if dir.join("cont").exists() => Exit {
            code: 110,
            label: "walltime",
            message: Some("inferred from the `cont` file GADGET-4 leaves at its CPU-time limit".into()),
            invalidates_reuse: false,
            state: "suspended",
        },
        None if !restarts.is_empty() => Exit {
            code: 130,
            label: "stopped",
            message: Some("ended before TimeMax with restart files; the cause is unknown (no log was given)".into()),
            invalidates_reuse: false,
            state: "suspended",
        },
        None => Exit {
            code: 400,
            label: "incomplete",
            message: Some("ended before TimeMax with no restart files; the cause is unknown (no log was given)".into()),
            invalidates_reuse: true,
            state: "failed",
        },
    };

    // The record.
    let problem = json!({"name": opts.problem});
    let key = run_convention::key_of(&problem, &run_convention::values_of(&params));
    let mut code = json!({"name": "GADGET-4", "version": "unknown"});
    if let Some(i) = &info_h5 {
        if let Some(c) = i.header.get("Git_commit").and_then(Value::as_str) {
            if c.len() == 40 && c.bytes().all(|b| b.is_ascii_hexdigit()) {
                code["commit"] = json!(c.to_ascii_lowercase());
            }
        }
        if !i.config.is_empty() {
            code["build"] = Value::Object(i.config.clone());
            code["build_flags"] = json!(post::build_flags(&i.config));
        }
    }
    let mut sources = vec!["parameters-usedvalues".to_string()];
    if info_path.exists() {
        sources.push("info.txt".into());
    }
    let mut run = json!({
        "convention": "run@1",
        "id": uuid::Uuid::new_v4().to_string(),
        "key": key,
        "provenance": "imported",
        "code": code,
        "problem": problem,
        "lineage": {},
        "status": {"state": "created"},
        "imported": {
            "from": sources,
            "adapter": "gadget4-adapt adopt",
            "event_times": "interpolated between the first and last file mtime",
        },
    });
    if let Some(n) = &opts.name {
        run["name"] = json!(n);
    }

    let mut w = RunWriter::create(dir, run)?;
    rundir::write_json_atomic(&dir.join("params.json"), &params)?;

    evs.sort_by_key(|e| e.t);
    w.event_at(&rundir::iso(start), "started", json!({}))?;
    for e in evs {
        let t = e.t.max(start);
        w.event_at(&rundir::iso(t), e.kind, e.fields)?;
    }
    // Warn about disagreement between the snapshot's Parameters and the used values.
    if let Some(i) = &info_h5 {
        let have: std::collections::BTreeMap<String, Value> = i
            .parameters
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for d in post::diff_native(
            &params,
            &have,
            post::SNAPSHOT_REL_TOL,
            "snapshot Parameters",
        ) {
            w.warning(&d, None)?;
        }
    }
    for e in index {
        w.index_output(e)?;
    }
    let end = rundir::iso(last);
    w.event_at(&end, "finished", json!({"exit": exit.to_json()}))?;
    let seq = w.last_seq();
    let ej = exit.to_json();
    let started = rundir::iso(start);
    w.set_status(|s| {
        s.insert("state".into(), json!(exit.state));
        s.insert("started_at".into(), json!(started));
        s.insert("ended_at".into(), json!(end));
        s.insert("exit".into(), ej);
        s.insert("last_event_seq".into(), json!(seq));
    })?;
    Ok(exit)
}

fn value_reached(time: f64, max: f64) -> bool {
    crate::value::close(time, max, 1e-9)
}
