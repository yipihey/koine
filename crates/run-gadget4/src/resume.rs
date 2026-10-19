//! Resume (R-D12 `resume(dir; from)`): what the child run of a GADGET-4
//! restart is, decided before anything is written.
//!
//! GADGET-4 takes the restart mode on its **command line**, not in the
//! parameter file (`Gadget4 <param.txt> [<RestartFlag> [<snapnum>]]`, see
//! `src/main/main.cc`); a `RestartFlag` tag in `param.txt` is an unknown tag
//! the code refuses. So `RestartFlag` here is an argument after `param.txt`,
//! recorded in the child's `host.argv`:
//!
//! | `--from` | flag | what the adapter stages in the child's output dir |
//! |---|---|---|
//! | a `restartfiles/restart.N` (or the `restartfiles` directory) | `1` (`RST_RESUME`) | every `restart.N` of that directory, copied |
//! | a snapshot `<base>_NNN.hdf5` | `2` (`RST_STARTFROMSNAP`), snapshot number `NNN` | the snapshot, copied |
//!
//! The parent is the run whose directory holds the checkpoint (the nearest
//! ancestor with a `run.json`); the child asserts it (R-D4.2). Parameters:
//! the parent's `params.json`, then the parent's `restart-pending.json`, then
//! the values supplied; a difference from the parent's is a `changed` event
//! `by: "restart"`, and a difference in a `never` parameter is refused (R-D2.3).

use crate::error::{Problem, BAD_RESTART_SOURCE, NEVER_CHANGED};
use crate::render;
use crate::rundir::sha256_file;
use crate::table;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Change {
    pub param: String,
    pub from: Value,
    pub to: Value,
}

#[derive(Debug, Clone)]
pub struct ResumePlan {
    /// GADGET-4's `RestartFlag`: 1 restart files, 2 snapshot.
    pub flag: u8,
    /// The snapshot number (flag 2): the argument after the flag.
    pub snapnum: Option<u64>,
    pub parent_id: String,
    /// The checkpoint as given (made absolute) and its content hash.
    pub checkpoint_path: PathBuf,
    pub checkpoint_sha256: String,
    /// The step the parent reached at that checkpoint, when its events say.
    pub step: u64,
    /// `(source, name under the child's output dir)` to stage before spawning.
    pub stage: Vec<(PathBuf, PathBuf)>,
    pub changes: Vec<Change>,
    /// The parameters to run: parent carried forward, caller's applied.
    pub params: Value,
}

/// `never` | `restart` | `always` of a qualified name, from the schema if
/// given, else the built-in table; `never` when neither knows it.
pub fn mutability(name: &str, schema: Option<&Value>) -> String {
    if let Some(m) = schema
        .and_then(|s| {
            s.pointer(&format!(
                "/properties/{}/x-run/mutable",
                name.replace('/', "~1")
            ))
        })
        .and_then(Value::as_str)
    {
        return m.to_string();
    }
    table::split_qualified(name)
        .and_then(|(_, native)| table::lookup(native))
        .map(|r| r.def.mutable.to_string())
        .unwrap_or_else(|| "never".into())
}

/// JSON equality that does not tell `1` from `1.0`.
pub fn same(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) if a.is_number() && b.is_number() => x == y,
        _ => a == b,
    }
}

fn bad(message: impl Into<String>) -> Vec<Problem> {
    vec![Problem::new(BAD_RESTART_SOURCE, message)]
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(p).ok()?).ok()
}

/// `restart.N` file names of a directory, sorted.
fn restart_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.is_file()
                        && p.file_name()
                            .and_then(|n| n.to_str())
                            .and_then(|n| n.strip_prefix("restart."))
                            .is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit()))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// The step of the parent's `checkpoint` event naming `path` (relative to
/// the parent's run directory), else its last `progress` step, else 0.
fn parent_step(parent_dir: &Path, rel: &str) -> u64 {
    let Ok(text) = fs::read_to_string(parent_dir.join("events.jsonl")) else {
        return 0;
    };
    let mut last_progress = 0;
    for l in text.lines() {
        let Ok(e) = serde_json::from_str::<Value>(l) else {
            continue;
        };
        match e["kind"].as_str() {
            Some("checkpoint") if e["path"].as_str() == Some(rel) => {
                if let Some(s) = e["step"].as_u64() {
                    return s;
                }
            }
            Some("progress") => last_progress = e["step"].as_u64().unwrap_or(last_progress),
            _ => {}
        }
    }
    last_progress
}

/// Decide a resume from `from`. `supplied` is the `--params` object (a full
/// set or only the changes); `mpi` is the rank count the child will run with.
pub fn plan(
    from: &Path,
    supplied: &Value,
    schema: Option<&Value>,
    mpi: Option<u32>,
) -> Result<ResumePlan, Vec<Problem>> {
    let from = from
        .canonicalize()
        .map_err(|e| bad(format!("--from {}: {e}", from.display())))?;
    // What is being restarted from.
    let (flag, snapnum, ckpt_files, ckpt_dir): (u8, Option<u64>, Vec<PathBuf>, PathBuf) = if from
        .is_dir()
    {
        let rf = if from.file_name().is_some_and(|n| n == "restartfiles") {
            from.clone()
        } else {
            from.join("restartfiles")
        };
        let files = restart_files(&rf);
        if files.is_empty() {
            return Err(bad(format!(
                    "{} holds no restart.N files; --from is a restartfiles directory, one of its restart.N, or a snapshot",
                    from.display()
                )));
        }
        (1, None, files, rf)
    } else {
        let name = from
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let parent = from.parent().map(Path::to_path_buf).unwrap_or_default();
        if name.starts_with("restart.") && parent.file_name().is_some_and(|n| n == "restartfiles") {
            (1, None, restart_files(&parent), parent)
        } else if let Some(n) = crate::post::snapshot_number(&name) {
            (2, Some(n), vec![from.clone()], parent)
        } else {
            return Err(bad(format!(
                "{} is neither a restartfiles/restart.N nor a snapshot <base>_NNN.hdf5",
                from.display()
            )));
        }
    };
    if flag == 1 {
        let ranks = ckpt_files.len() as u32;
        let want = mpi.unwrap_or(1);
        if ranks != want {
            return Err(bad(format!(
                "{} holds restart files for {ranks} MPI rank(s); this resume would run {want} (GADGET-4 needs the same count)",
                ckpt_dir.display()
            )));
        }
    }
    // The parent: nearest ancestor with a run.json.
    let parent_dir = from
        .ancestors()
        .skip(1)
        .find(|a| a.join("run.json").is_file())
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            bad(format!(
                "no run.json above {}: a resume asserts the run it resumed from (R-D4.2)",
                from.display()
            ))
        })?;
    let parent_id = read_json(&parent_dir.join("run.json"))
        .and_then(|r| r["id"].as_str().map(str::to_string))
        .ok_or_else(|| bad(format!("{}/run.json has no id", parent_dir.display())))?;
    let parent_params = read_json(&parent_dir.join("params.json"))
        .ok_or_else(|| bad(format!("{}/params.json is missing", parent_dir.display())))?;

    // Parameters: parent, then its pending restart values, then the caller's.
    let mut merged: Map<String, Value> = match &parent_params {
        Value::Object(o) => o
            .iter()
            .map(|(k, v)| {
                let v = if v.get("value").is_some() {
                    v.clone()
                } else {
                    json!({"value": v})
                };
                (k.clone(), v)
            })
            .collect(),
        _ => Map::new(),
    };
    if let Some(Value::Object(p)) = read_json(&parent_dir.join("restart-pending.json")) {
        for (k, v) in p {
            if let Some(value) = v.get("value") {
                merged.insert(
                    k,
                    json!({"value": value, "source": format!("restart:{parent_id}")}),
                );
            }
        }
    }
    for e in render::entries(supplied)? {
        let mut o = Map::new();
        o.insert("value".into(), e.value);
        if let Some(s) = e.source {
            o.insert("source".into(), json!(s));
        }
        merged.insert(e.name, Value::Object(o));
    }
    let merged = Value::Object(merged);

    // What changed, and the `never` ones refused (all of them, named).
    let was: BTreeMap<String, Value> = render::values(&parent_params);
    let is: BTreeMap<String, Value> = render::values(&merged);
    let mut changes = Vec::new();
    let mut problems = Vec::new();
    for (name, now) in &is {
        let before = was.get(name).cloned().unwrap_or(Value::Null);
        if same(&before, now) {
            continue;
        }
        if mutability(name, schema) == "never" {
            problems.push(Problem::new(
                NEVER_CHANGED,
                format!(
                    "`{name}` is `never`: the parent run has {before}, the resume asks {now}; a different value is a new run (R-D2.3)"
                ),
            ));
        } else {
            changes.push(Change {
                param: name.clone(),
                from: before,
                to: now.clone(),
            });
        }
    }
    if !problems.is_empty() {
        return Err(problems);
    }

    // What to stage.
    let stage: Vec<(PathBuf, PathBuf)> = if flag == 1 {
        ckpt_files
            .iter()
            .map(|p| {
                (
                    p.clone(),
                    Path::new("restartfiles").join(p.file_name().unwrap_or_default()),
                )
            })
            .collect()
    } else {
        vec![(
            from.clone(),
            PathBuf::from(from.file_name().unwrap_or_default()),
        )]
    };
    let rel = from
        .strip_prefix(&parent_dir)
        .map(|r| r.to_string_lossy().into_owned())
        .unwrap_or_default();
    let sha = if flag == 2 || ckpt_files.len() == 1 {
        sha256_file(&ckpt_files[0]).map_err(|e| bad(format!("{}: {e}", ckpt_files[0].display())))?
    } else {
        let mut h = Sha256::new();
        for f in &ckpt_files {
            let s = sha256_file(f).map_err(|e| bad(format!("{}: {e}", f.display())))?;
            h.update(format!(
                "{} {s}\n",
                f.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    };
    Ok(ResumePlan {
        flag,
        snapnum,
        parent_id,
        checkpoint_path: from.clone(),
        checkpoint_sha256: sha,
        step: parent_step(&parent_dir, &rel),
        stage,
        changes,
        params: merged,
    })
}
