//! Launch a registered code (R-D13 `run-service_start`): make the run
//! directory, hand the process its parameters, its problem and the
//! directory as the code's [`Launch`] template says, and follow it —
//! ingesting the directory as it fills — until it exits.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use impress_item::store::AttributedStore;
use serde_json::{json, Map, Value};

use crate::codes::{CodeProvider, DirVia, ParamsVia};
use crate::dto::Refused;
use crate::ingest::{ingest_dir, Ingested, Launched};

/// The file koine writes the effective values into for a code launched
/// with `params: file` — a `params.json`-shaped object the code's
/// `--params` reads (R-D3). Not `params.json` itself: that one is the
/// code's, written before its first step.
pub const INPUT_FILE: &str = "koine-input.json";

/// How often the tail looks at the directory while the process runs.
pub const TAIL_INTERVAL: Duration = Duration::from_millis(200);

/// What `start` launched.
pub struct Launching {
    pub dir: PathBuf,
    pub child: Child,
}

/// The run directory a start without `dir` gets: `<workspace>/runs/<code>/<uuid>`.
pub fn default_dir(code: &str) -> Result<PathBuf, Refused> {
    let ws = koine_store::process::workspace().ok_or_else(|| {
        Refused::invalid(
            "no `dir` given and this host names no workspace; say where the run directory goes",
        )
    })?;
    Ok(ws
        .join("runs")
        .join(code)
        .join(uuid::Uuid::new_v4().to_string()))
}

/// Make the run directory, write the input, spawn the process.
pub fn launch(
    code: &CodeProvider,
    dir: &Path,
    values: &Map<String, Value>,
    problem: &Value,
    resume_from: Option<&Path>,
) -> Result<Launching, Refused> {
    if dir.join("run.json").exists() {
        return Err(Refused::new(
            "conflict",
            format!(
                "{} already holds a run (run.json); a run is a directory of its own",
                dir.display()
            ),
        ));
    }
    fs::create_dir_all(dir).map_err(|e| Refused::new("io", format!("{}: {e}", dir.display())))?;
    let dir = dir
        .canonicalize()
        .map_err(|e| Refused::new("io", format!("{}: {e}", dir.display())))?;
    let (program, fixed) = code
        .launch
        .argv
        .split_first()
        .ok_or_else(|| Refused::invalid("launch.argv names no program"))?;
    let mut cmd = Command::new(program);
    cmd.args(fixed);
    cmd.envs(&code.launch.env);
    match code.launch.dir {
        DirVia::Flag => {
            cmd.arg(&code.launch.dir_name).arg(&dir);
        }
        DirVia::Cwd => {
            cmd.current_dir(&dir);
        }
        DirVia::Env => {
            cmd.env(&code.launch.dir_name, &dir);
        }
    }
    match code.launch.params {
        ParamsVia::File => {
            let shaped: Map<String, Value> = values
                .iter()
                .map(|(k, v)| (k.clone(), json!({"value": v, "source": "koine:start"})))
                .collect();
            let path = dir.join(INPUT_FILE);
            let text = serde_json::to_string_pretty(&Value::Object(shaped)).unwrap_or_default();
            fs::write(&path, text + "\n")
                .map_err(|e| Refused::new("io", format!("{}: {e}", path.display())))?;
            cmd.arg(&code.launch.params_flag).arg(&path);
        }
        ParamsVia::Argv => {
            for (k, v) in values {
                let text = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                cmd.arg(format!("--{k}={text}"));
            }
        }
        ParamsVia::Env => {
            for (k, v) in values {
                let Some(env) = code
                    .properties()
                    .get(k)
                    .and_then(|p| p.pointer("/x-run/env"))
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                let text = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                cmd.env(env, text);
            }
        }
    }
    if let Some(flag) = &code.launch.problem_flag {
        cmd.arg(flag).arg(problem.to_string());
    }
    if let Some(from) = resume_from {
        let Some(flag) = &code.launch.resume_flag else {
            return Err(Refused::new(
                "unsupported",
                format!(
                    "{} declares no resume flag; it cannot resume from a checkpoint",
                    code.name
                ),
            ));
        };
        cmd.arg(flag).arg(from);
    }
    let stdout = fs::File::create(dir.join("koine-stdout.log"))
        .map_err(|e| Refused::new("io", format!("stdout log: {e}")))?;
    let stderr = fs::File::create(dir.join("koine-stderr.log"))
        .map_err(|e| Refused::new("io", format!("stderr log: {e}")))?;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    let child = cmd
        .spawn()
        .map_err(|e| Refused::new("io", format!("cannot start `{program}`: {e}")))?;
    Ok(Launching { dir, child })
}

/// Follow the process: ingest the directory every [`TAIL_INTERVAL`] until
/// the process exits, then once more with its exit status. The first
/// ingests may refuse while the code has not yet written `run.json`; that
/// is not an error until the process is gone. Answers the last ingest, or
/// the refusal that stood when the process exited.
pub fn follow(
    store: &dyn AttributedStore,
    mut launching: Launching,
    launched: &Launched,
) -> Result<Ingested, Refused> {
    let pid = launching.child.id();
    let mut with_pid = launched.clone();
    with_pid.pid = Some(pid);
    loop {
        let exited = match launching.child.try_wait() {
            Ok(Some(status)) => Some(status.code().unwrap_or(-1)),
            Ok(None) => None,
            Err(e) => Some({
                tracing::warn!(pid, "try_wait: {e}");
                -1
            }),
        };
        if let Some(code) = exited {
            with_pid.exited = Some(code);
            let mut last = ingest_dir(store, &launching.dir, &with_pid);
            if let Err(e) = &last {
                let stderr =
                    fs::read_to_string(launching.dir.join("koine-stderr.log")).unwrap_or_default();
                last = Err(Refused::new(
                    e.code,
                    format!(
                        "the process exited {code} and {}{}",
                        e.message,
                        if stderr.trim().is_empty() {
                            String::new()
                        } else {
                            format!(" (stderr: {})", stderr.trim())
                        }
                    ),
                ));
            }
            // The fan-in of a sweep (R-D10): the last run to finish finishes
            // the parent job.
            if let (Ok(done), Some(sweep)) = (&last, launched.sweep.as_deref()) {
                let state = crate::kind::payload_json(&done.item)
                    .get("state")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("created")
                    .to_string();
                if let Err(e) = crate::sweep::note_run(
                    store,
                    &launching.dir,
                    sweep,
                    &done.item.id.to_string(),
                    &state,
                ) {
                    tracing::warn!(sweep, run = %done.item.id, "could not record the run in its sweep: {e}");
                }
            }
            return last;
        }
        // A refusal here is the directory still being born; the exit path
        // reports the one that stands when the process is gone.
        let _ = ingest_dir(store, &launching.dir, &with_pid);
        std::thread::sleep(TAIL_INTERVAL);
    }
}

/// Wait until the code has written a readable `run.json` (so the start can
/// answer the run's id), or until the process has exited, whichever first.
/// Answers the first ingest; a process that died before writing anything
/// answers the refusal with its stderr.
pub fn first_ingest(
    store: &dyn AttributedStore,
    launching: &mut Launching,
    launched: &Launched,
) -> Result<Ingested, Refused> {
    let pid = launching.child.id();
    let mut with_pid = launched.clone();
    with_pid.pid = Some(pid);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(done) = ingest_dir(store, &launching.dir, &with_pid) {
            return Ok(done);
        }
        if let Ok(Some(status)) = launching.child.try_wait() {
            with_pid.exited = Some(status.code().unwrap_or(-1));
            return ingest_dir(store, &launching.dir, &with_pid).map_err(|e| {
                let stderr =
                    fs::read_to_string(launching.dir.join("koine-stderr.log")).unwrap_or_default();
                Refused::new(
                    "process-failed",
                    format!(
                        "{} exited {} before writing a conforming run: {}{}",
                        launching.dir.display(),
                        status.code().unwrap_or(-1),
                        e.message,
                        if stderr.trim().is_empty() {
                            String::new()
                        } else {
                            format!(" (stderr: {})", stderr.trim())
                        }
                    ),
                )
            });
        }
        if std::time::Instant::now() > deadline {
            return Err(Refused::new(
                "timed-out",
                format!(
                    "{} wrote no conforming run.json within 30 s",
                    launching.dir.display()
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Follow in a thread of its own: the start answers at once, the ring fills
/// while the code runs.
pub fn follow_in_background(
    store: Arc<dyn AttributedStore>,
    launching: Launching,
    launched: Launched,
) {
    let dir = launching.dir.clone();
    std::thread::Builder::new()
        .name(format!("run-tail {}", dir.display()))
        .spawn(move || {
            if let Err(e) = follow(store.as_ref(), launching, &launched) {
                tracing::warn!(dir = %dir.display(), "tail ended with a refusal: {e}");
            }
        })
        .map(|_| ())
        .unwrap_or_else(|e| tracing::warn!("cannot spawn the tail thread: {e}"));
}
