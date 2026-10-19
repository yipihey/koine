//! `gadget4-adapt launch`: validate, render, spawn GADGET-4, and write the run
//! directory from what comes back (R-D9.1).
//!
//! Transports (surveyed in the GADGET-4 source, `src/main/run.cc`):
//! - stop: an empty file `stop` in the *output directory* (`All.OutputDir`, not
//!   the working directory); GADGET-4 writes restart files, deletes the file and
//!   ends after the current step.
//! - checkpoint: an empty file `restart` in the output directory; GADGET-4
//!   writes restart files and carries on.
//! - set: not supported live. A `restart`-class parameter is recorded in
//!   `restart-pending.json` for the next restart; a `never` one is refused.

use crate::echo::{self, Echo};
use crate::error::{io, Error, Problem};
use crate::exitmap::{classify, Ending, Exit};
use crate::info::{self, SyncPoint};
use crate::post;
use crate::render;
use crate::rundir::{self, output_entry, rel, RunWriter};
use crate::table;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub run_dir: PathBuf,
    /// The contents of the `params.json` to run (convention or bare form).
    pub params: Value,
    /// Names the file in each parameter's `file:<name>` source.
    pub params_name: String,
    pub schema: Option<Value>,
    pub name: Option<String>,
    /// `problem.name` (R-D2.4): what this run is a run of.
    pub problem: String,
    /// The whole `problem` object, verbatim, when the caller (run-service
    /// passes `--problem <json>`) has one: the key it predicted is computed
    /// over it, so nothing is added to it. `problem` is then ignored.
    pub problem_json: Option<Value>,
    /// Resume from this checkpoint (a `restartfiles/restart.N`, the
    /// directory, or a snapshot): see [`crate::resume`].
    pub resume_from: Option<PathBuf>,
    pub mpi: Option<u32>,
    /// The binary and any arguments after the parameter file (`0`, `1`, ...).
    pub command: Vec<String>,
    pub poll: Duration,
    /// Emit one `progress` (and `metric`) per this many sync points.
    pub progress_every: u64,
    /// How long to wait for the startup banner before writing `running` anyway.
    pub banner_timeout: Duration,
}

impl LaunchOptions {
    pub fn new(run_dir: PathBuf, params: Value, command: Vec<String>) -> Self {
        LaunchOptions {
            run_dir,
            params,
            params_name: "params.json".into(),
            schema: None,
            name: None,
            problem: "gadget4".into(),
            problem_json: None,
            resume_from: None,
            mpi: None,
            command,
            poll: Duration::from_millis(200),
            progress_every: 1,
            banner_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug)]
pub struct Outcome {
    pub run_dir: PathBuf,
    pub exit: Exit,
}

fn cmd_output(cmd: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(cmd).args(args).output().ok()?;
    if !o.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn read_head_tail(p: &Path, head: u64, tail: u64) -> String {
    let Ok(mut f) = File::open(p) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut buf = Vec::new();
    if len <= head + tail {
        let _ = f.read_to_end(&mut buf);
        return String::from_utf8_lossy(&buf).into_owned();
    }
    let mut h = vec![0u8; head as usize];
    let _ = f.read_exact(&mut h);
    let _ = f.seek(SeekFrom::Start(len - tail));
    let mut t = Vec::new();
    let _ = f.read_to_end(&mut t);
    buf.extend(h);
    buf.extend(b"\n...\n");
    buf.extend(t);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Read what was appended to `path` since `*off`, whole lines only.
fn read_new_lines(path: &Path, off: &mut u64) -> Vec<String> {
    let Ok(mut f) = File::open(path) else {
        return Vec::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len < *off {
        *off = 0;
    }
    if len == *off || f.seek(SeekFrom::Start(*off)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let Some(end) = buf.iter().rposition(|b| *b == b'\n') else {
        return Vec::new();
    };
    *off += (end + 1) as u64;
    String::from_utf8_lossy(&buf[..end])
        .lines()
        .map(str::to_string)
        .collect()
}

#[derive(Default)]
struct Watch {
    /// path -> (len, mtime nanos) last seen
    seen: BTreeMap<PathBuf, (u64, u128)>,
    /// path -> signature already reported
    reported: BTreeMap<PathBuf, (u64, u128)>,
}

fn signature(p: &Path) -> Option<(u64, u128)> {
    let m = fs::metadata(p).ok()?;
    if !m.is_file() {
        return None;
    }
    let t = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Some((m.len(), t))
}

struct Launcher {
    w: RunWriter,
    out_dir: PathBuf,
    snapshot_base: String,
    params: Value,
    values: BTreeMap<String, Value>,
    begin: Option<f64>,
    max: Option<f64>,
    comoving: bool,
    progress_every: u64,
    started_at: Instant,
    last_step: u64,
    last_time: Option<f64>,
    info_off: u64,
    mem_off: u64,
    last_mem: Option<f64>,
    n_progress: u64,
    watch_snap: Watch,
    watch_restart: Watch,
    pending_checkpoints: Vec<String>,
    stop_requested: bool,
    first_snapshot: Option<PathBuf>,
    /// `changed{by: "restart"}` events to write right after `started`.
    resume_changes: Vec<crate::resume::Change>,
    resume_step: u64,
}

pub fn launch(opts: &LaunchOptions) -> Result<Outcome, Error> {
    // 0. A resume decides its parent, its parameters and its refusals first.
    let resume = match &opts.resume_from {
        Some(from) => Some(
            crate::resume::plan(from, &opts.params, opts.schema.as_ref(), opts.mpi)
                .map_err(Error::Invalid)?,
        ),
        None => None,
    };
    let run_params = resume
        .as_ref()
        .map(|r| r.params.clone())
        .unwrap_or_else(|| opts.params.clone());

    // 1. Validate and render before anything exists on disk.
    let param_txt = render::render(&run_params, opts.schema.as_ref()).map_err(Error::Invalid)?;
    let values = render::values(&run_params);
    let out_rel_path = post::output_dir(&values).map_err(|p| Error::Invalid(vec![p]))?;
    if opts.command.is_empty() {
        return Err(Error::other("launch needs a binary after `--`"));
    }

    // 2. params.json with sources, key, run.json (`created`).
    let entries = render::entries(&run_params).map_err(Error::Invalid)?;
    let mut pj = Map::new();
    for e in &entries {
        pj.insert(
            e.name.clone(),
            json!({"value": e.value, "source": e.source.clone().unwrap_or_else(|| format!("file:{}", opts.params_name))}),
        );
    }
    let params = Value::Object(pj);

    let problem = match &opts.problem_json {
        Some(p) => p.clone(),
        None => {
            let mut problem = json!({"name": opts.problem});
            if let Some(inp) = input_file(&values, &opts.run_dir) {
                problem["inputs"] = json!([inp]);
            }
            problem
        }
    };
    let key = run_convention::key_of(&problem, &run_convention::values_of(&params));

    let mut argv: Vec<String> = Vec::new();
    if let Some(n) = opts.mpi {
        argv.extend(["mpirun".to_string(), "-np".to_string(), n.to_string()]);
    }
    argv.push(opts.command[0].clone());
    argv.push("param.txt".into());
    if let Some(r) = &resume {
        // The restart mode is a command-line argument of GADGET-4, after the
        // parameter file (src/main/main.cc), never a tag in it.
        argv.push(r.flag.to_string());
        if let Some(n) = r.snapnum {
            argv.push(n.to_string());
        }
    }
    argv.extend(opts.command[1..].iter().cloned());

    let mut run = json!({
        "convention": "run@1",
        "id": uuid::Uuid::new_v4().to_string(),
        "key": key,
        "code": {"name": "GADGET-4", "version": "unknown"},
        "problem": problem,
        "host": {"argv": argv},
        "lineage": {},
        "status": {"state": "created"},
    });
    if let Some(r) = &resume {
        run["lineage"] = json!({"parent": {
            "run": r.parent_id,
            "relation": "resume",
            "checkpoint": {
                "path": r.checkpoint_path.to_string_lossy(),
                "sha256": r.checkpoint_sha256,
            }
        }});
    }
    if let Some(n) = &opts.name {
        run["name"] = json!(n);
    }
    if let Some(h) = cmd_output("hostname", &[]) {
        run["host"]["hostname"] = json!(h);
    }
    if let Some(o) = cmd_output("uname", &["-sr"]) {
        run["host"]["os"] = json!(o);
    }

    let dir = &opts.run_dir;
    let mut w = RunWriter::create(dir, run)?;
    rundir::write_json_atomic(&dir.join("params.json"), &params)?;
    io(
        "write param.txt",
        fs::write(dir.join("param.txt"), &param_txt),
    )?;

    let out_dir = dir.join(&out_rel_path);
    io("create output dir", fs::create_dir_all(&out_dir))?;
    if let Some(r) = &resume {
        for (src, name) in &r.stage {
            let dst = out_dir.join(name);
            if let Some(d) = dst.parent() {
                io("create staging dir", fs::create_dir_all(d))?;
            }
            io(
                format!("stage {} for the restart", src.display()),
                fs::copy(src, &dst),
            )?;
        }
    }
    let log_path = dir.join("stdout.log");
    let log = io("create stdout.log", File::create(&log_path))?;
    let log_err = io("clone stdout.log", log.try_clone())?;

    // 3. Spawn.
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let exit = Exit {
                code: 160,
                label: "spawn-failed",
                message: Some(format!("cannot start `{}`: {e}", argv[0])),
                invalidates_reuse: false,
                state: "failed",
            };
            finish_without_run(&mut w, &exit)?;
            return Ok(Outcome {
                run_dir: dir.clone(),
                exit,
            });
        }
    };

    let begin = values.get("time.TimeBegin").and_then(Value::as_f64);
    let max = values.get("time.TimeMax").and_then(Value::as_f64);
    let mut l = Launcher {
        w,
        out_dir,
        snapshot_base: values
            .get("io.SnapshotFileBase")
            .and_then(Value::as_str)
            .unwrap_or("snapshot")
            .to_string(),
        values: values.clone(),
        params,
        resume_changes: resume
            .as_ref()
            .map(|r| r.changes.clone())
            .unwrap_or_default(),
        resume_step: resume.as_ref().map(|r| r.step).unwrap_or(0),
        begin,
        max,
        comoving: values
            .get("time.ComovingIntegrationOn")
            .and_then(Value::as_i64)
            == Some(1),
        progress_every: opts.progress_every.max(1),
        started_at: Instant::now(),
        last_step: 0,
        last_time: None,
        info_off: 0,
        mem_off: 0,
        last_mem: None,
        n_progress: 0,
        watch_snap: Watch::default(),
        watch_restart: Watch::default(),
        pending_checkpoints: Vec::new(),
        stop_requested: false,
        first_snapshot: None,
    };

    let status = l.run_loop(&mut child, &log_path, opts)?;
    let exit = l.finish(&log_path, status)?;
    Ok(Outcome {
        run_dir: dir.clone(),
        exit,
    })
}

fn finish_without_run(w: &mut RunWriter, exit: &Exit) -> Result<(), Error> {
    let t = rundir::now();
    w.event_at(&t, "started", json!({}))?;
    w.event_at(&t, "finished", json!({"exit": exit.to_json()}))?;
    let seq = w.last_seq();
    let ej = exit.to_json();
    w.set_status(|s| {
        s.insert("state".into(), json!(exit.state));
        s.insert("started_at".into(), json!(t));
        s.insert("ended_at".into(), json!(t));
        s.insert("exit".into(), ej);
        s.insert("last_event_seq".into(), json!(seq));
    })
}

/// `problem.inputs` entry for the initial-conditions file, when it exists.
fn input_file(values: &BTreeMap<String, Value>, run_dir: &Path) -> Option<Value> {
    let ic = values.get("io.InitCondFile")?.as_str()?;
    for cand in [ic.to_string(), format!("{ic}.hdf5")] {
        let p = Path::new(&cand);
        let full = if p.is_absolute() {
            p.to_path_buf()
        } else {
            run_dir.join(p)
        };
        if full.is_file() {
            if let Ok(sha) = rundir::sha256_file(&full) {
                return Some(json!({"path": cand, "sha256": sha}));
            }
        }
    }
    None
}

impl Launcher {
    fn run_loop(
        &mut self,
        child: &mut Child,
        log_path: &Path,
        opts: &LaunchOptions,
    ) -> Result<Option<i32>, Error> {
        let mut started = false;
        loop {
            let exited = io("poll child", child.try_wait())?;
            if !started {
                let head = read_head_tail(log_path, 256 * 1024, 0);
                let echo = echo::parse(&head);
                if echo.version.is_some()
                    || exited.is_some()
                    || self.started_at.elapsed() >= opts.banner_timeout
                {
                    self.write_started(&echo)?;
                    started = true;
                }
            }
            if started {
                self.drain_info()?;
                self.scan_outputs(exited.is_some())?;
                self.serve_control(exited.is_some())?;
            }
            if let Some(st) = exited {
                return Ok(st.code());
            }
            std::thread::sleep(opts.poll);
        }
    }

    fn write_started(&mut self, echo: &Echo) -> Result<(), Error> {
        let t = rundir::now();
        {
            let code = &mut self.w.run["code"];
            code["version"] = json!(echo.version.clone().unwrap_or_else(|| "unknown".into()));
            if let Some(c) = &echo.git_commit {
                code["commit"] = json!(c);
            }
            if !echo.config.is_empty() {
                code["build_flags"] = json!(echo.config);
                code["build"] = Value::Object(post::config_from_flags(&echo.config));
            }
        }
        self.w.event_at(&t, "started", json!({}))?;
        for c in std::mem::take(&mut self.resume_changes) {
            self.w.event_at(
                &t,
                "changed",
                json!({"param": c.param, "from": c.from, "to": c.to, "by": "restart", "step": self.resume_step}),
            )?;
        }
        let seq = self.w.last_seq();
        self.w.set_status(|s| {
            s.insert("state".into(), json!("running"));
            s.insert("started_at".into(), json!(t));
            s.insert("last_event_seq".into(), json!(seq));
        })
    }

    fn drain_info(&mut self) -> Result<(), Error> {
        // memory.txt first, so a progress event carries the allocation of its own step
        let mem_path = self.out_dir.join("memory.txt");
        for line in read_new_lines(&mem_path, &mut self.mem_off) {
            if let Some(m) = info::parse_memory_line(&line) {
                self.last_mem = Some(m);
            }
        }
        let info_path = self.out_dir.join("info.txt");
        for line in read_new_lines(&info_path, &mut self.info_off) {
            if let Some(sp) = info::parse_sync_point(&line) {
                self.on_sync_point(&sp)?;
            }
        }
        Ok(())
    }

    fn on_sync_point(&mut self, sp: &SyncPoint) -> Result<(), Error> {
        self.last_step = sp.step;
        self.last_time = Some(sp.time);
        let due = self.n_progress.is_multiple_of(self.progress_every);
        self.n_progress += 1;
        if !due {
            return Ok(());
        }
        let mut f = json!({"step": sp.step, "time": sp.time, "dt": sp.dt});
        if let (Some(b), Some(m)) = (self.begin, self.max) {
            if let Some(fr) = info::fraction(sp.time, b, m, self.comoving) {
                f["fraction"] = json!(fr);
            }
        }
        if let Some(z) = sp.redshift {
            f["redshift"] = json!(z);
        }
        self.w.event("progress", f)?;
        let mut vals = json!({"wall_s": self.started_at.elapsed().as_secs_f64()});
        if let Some(m) = self.last_mem {
            vals["memory_largest_mb"] = json!(m);
        }
        self.w
            .event("metric", json!({"step": sp.step, "values": vals}))?;
        Ok(())
    }

    /// Snapshots and restart files: report a file once its size and mtime have
    /// not changed for one poll (GADGET-4 writes them in place), or at exit.
    fn scan_outputs(&mut self, final_sweep: bool) -> Result<(), Error> {
        let mut snaps: Vec<PathBuf> = Vec::new();
        if let Ok(rd) = fs::read_dir(&self.out_dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if post::is_snapshot_file(&name, &self.snapshot_base) {
                    snaps.push(e.path());
                }
            }
        }
        snaps.sort();
        for p in snaps {
            if self.stable(&p, true, final_sweep) {
                self.report_snapshot(&p)?;
            }
        }
        let mut restarts: Vec<PathBuf> = Vec::new();
        if let Ok(rd) = fs::read_dir(self.out_dir.join("restartfiles")) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with("restart.") {
                    restarts.push(e.path());
                }
            }
        }
        restarts.sort();
        for p in restarts {
            if self.stable(&p, false, final_sweep) {
                self.report_restart(&p)?;
            }
        }
        Ok(())
    }

    fn stable(&mut self, p: &Path, snap: bool, final_sweep: bool) -> bool {
        let Some(sig) = signature(p) else {
            return false;
        };
        let watch = if snap {
            &mut self.watch_snap
        } else {
            &mut self.watch_restart
        };
        if watch.reported.get(p) == Some(&sig) {
            return false;
        }
        let prev = watch.seen.insert(p.to_path_buf(), sig);
        sig.0 > 0 && (final_sweep || prev == Some(sig))
    }

    fn report_snapshot(&mut self, p: &Path) -> Result<(), Error> {
        let sig = signature(p).unwrap_or((0, 0));
        self.watch_snap.reported.insert(p.to_path_buf(), sig);
        let relp = rel(&self.w.dir, p);
        let step = self.last_step;
        let time = self.last_time;
        let entry = output_entry(&self.w.dir, &relp, "snapshot", "hdf5", Some(step), time)?;
        self.w.index_output(entry)?;
        let mut f = json!({"path": relp, "kind_of": "snapshot", "step": step});
        if let Some(t) = time {
            f["time"] = json!(t);
        }
        self.w.event("output", f)?;
        if self.first_snapshot.is_none() {
            self.first_snapshot = Some(p.to_path_buf());
        }
        Ok(())
    }

    fn report_restart(&mut self, p: &Path) -> Result<(), Error> {
        let sig = signature(p).unwrap_or((0, 0));
        self.watch_restart.reported.insert(p.to_path_buf(), sig);
        let relp = rel(&self.w.dir, p);
        let step = self.last_step;
        let entry = output_entry(
            &self.w.dir,
            &relp,
            "checkpoint",
            "gadget4-restart",
            Some(step),
            self.last_time,
        )?;
        let sha = entry["sha256"].clone();
        self.w.index_output(entry)?;
        let mut f = json!({"path": relp, "sha256": sha, "step": step});
        if !self.pending_checkpoints.is_empty() {
            f["request"] = json!(self.pending_checkpoints.remove(0));
        }
        self.w.event("checkpoint", f)?;
        Ok(())
    }

    /// R-D8: serve `control/*.json`. After the run has ended, anything left is
    /// refused rather than left to rot.
    fn serve_control(&mut self, ended: bool) -> Result<(), Error> {
        let cdir = self.w.dir.join("control");
        let Ok(rd) = fs::read_dir(&cdir) else {
            return Ok(());
        };
        let mut files: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect();
        files.sort();
        for p in files {
            let stem = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let Ok(text) = fs::read_to_string(&p) else {
                continue; // being written
            };
            let Ok(mut body) = serde_json::from_str::<Value>(&text) else {
                continue; // partial write: try again next poll
            };
            if body.get("id").is_none() {
                body["id"] = json!(stem);
            }
            let verdict = if ended {
                Err("the run ended before the request was served".to_string())
            } else {
                self.apply(&stem, &body)
            };
            match verdict {
                Ok(note) => {
                    if let Some(n) = note {
                        body["note"] = json!(n);
                    }
                    self.move_control(&p, "done", &body)?;
                }
                Err(reason) => {
                    body["reason"] = json!(reason);
                    self.move_control(&p, "refused", &body)?;
                }
            }
        }
        Ok(())
    }

    fn move_control(&mut self, p: &Path, sub: &str, body: &Value) -> Result<(), Error> {
        let dest_dir = self.w.dir.join("control").join(sub);
        io("create control dir", fs::create_dir_all(&dest_dir))?;
        let dest = dest_dir.join(p.file_name().unwrap_or_default());
        rundir::write_json_atomic(&dest, body)?;
        let _ = fs::remove_file(p);
        Ok(())
    }

    fn apply(&mut self, id: &str, body: &Value) -> Result<Option<String>, String> {
        let req = body.get("request").and_then(Value::as_str).unwrap_or("");
        match req {
            "stop" => {
                let after = body.get("after").and_then(Value::as_str).unwrap_or("checkpoint");
                touch(&self.out_dir.join("stop"))?;
                self.stop_requested = true;
                let _ = self.w.event(
                    "note",
                    json!({"message": format!("stop requested by control:{id}"), "data": {"request": id, "after": after}}),
                );
                Ok(Some(
                    "touched <OutputDir>/stop; GADGET-4 writes restart files and ends after the current step (always checkpoint-then-stop)".into(),
                ))
            }
            "checkpoint" => {
                touch(&self.out_dir.join("restart"))?;
                self.pending_checkpoints.push(id.to_string());
                Ok(Some(
                    "touched <OutputDir>/restart; GADGET-4 writes restart files after the current step and continues".into(),
                ))
            }
            "set" => {
                let param = body.get("param").and_then(Value::as_str).unwrap_or("");
                let value = body.get("value").cloned().unwrap_or(Value::Null);
                let Some((_, native)) = table::split_qualified(param) else {
                    return Err(format!("`{param}` is not a group.Tag parameter name"));
                };
                if !self.values.contains_key(param) {
                    return Err(format!("`{param}` is not a parameter of this run"));
                }
                match table::lookup(native).map(|r| r.def.mutable) {
                    Some(m) if m == "restart" => {
                        self.record_pending(param, &value, id)?;
                        Ok(Some(format!(
                            "GADGET-4 has no live set; `{param}` is a restart-class parameter, recorded in restart-pending.json for the next restart"
                        )))
                    }
                    _ => Err(format!(
                        "`{param}` is mutable: never (GADGET-4 ignores a changed value on restart), and GADGET-4 has no live set"
                    )),
                }
            }
            other => Err(format!(
                "GADGET-4 has no `{other}`: its only runtime controls are the stop and restart files"
            )),
        }
    }

    fn record_pending(&mut self, param: &str, value: &Value, id: &str) -> Result<(), String> {
        let path = self.w.dir.join("restart-pending.json");
        let mut cur: Value = fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_else(|| json!({}));
        cur[param] = json!({"value": value, "by": format!("control:{id}")});
        rundir::write_json_atomic(&path, &cur).map_err(|e| e.to_string())
    }

    /// Everything after the child has gone.
    fn finish(&mut self, log_path: &Path, status: Option<i32>) -> Result<Exit, Error> {
        self.drain_info()?;
        self.scan_outputs(true)?;
        self.serve_control(true)?;

        let log = read_head_tail(log_path, 1 << 20, 4 << 20);
        let exit = classify(&Ending {
            log: &log,
            status,
            stop_requested: self.stop_requested,
            cont_file: self.out_dir.join("cont").exists(),
        });

        self.read_back()?;
        self.index_leftovers()?;

        let t = rundir::now();
        self.w
            .event_at(&t, "finished", json!({"exit": exit.to_json()}))?;
        let seq = self.w.last_seq();
        let ej = exit.to_json();
        self.w.set_status(|s| {
            s.insert("state".into(), json!(exit.state));
            s.insert("ended_at".into(), json!(t));
            s.insert("exit".into(), ej);
            s.insert("last_event_seq".into(), json!(seq));
        })?;
        Ok(exit)
    }

    /// `parameters-usedvalues` against `params.json`, and the first snapshot's
    /// `Config` and `Parameters` groups into `run.json` and against `params.json`.
    ///
    /// `params.json` is frozen once the run is `running` (R-D3, R-D5), and the key
    /// hashes it, so the used values are *compared*, not written over it: every
    /// difference is a `warning` event. The one late write to `run.json` is the
    /// `code.build` block, which nothing before the first snapshot could know
    /// when the startup echo carried no compile settings.
    fn read_back(&mut self) -> Result<(), Error> {
        let uv_path = self.out_dir.join("parameters-usedvalues");
        match fs::read_to_string(&uv_path) {
            Ok(text) => {
                let used = post::used_values(&text);
                for d in post::diff_native(
                    &self.params,
                    &used,
                    post::USEDVALUES_REL_TOL,
                    "parameters-usedvalues",
                ) {
                    self.w.warning(&d, None)?;
                }
            }
            Err(_) => {
                self.w
                    .warning("no parameters-usedvalues in the output directory", None)?;
            }
        }
        let Some(snap) = self.first_snapshot.clone() else {
            return Ok(());
        };
        match post::read_snapshot(&snap) {
            Ok(info) => {
                for d in post::diff_native(
                    &self.params,
                    &info
                        .parameters
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                    post::SNAPSHOT_REL_TOL,
                    "snapshot Parameters",
                ) {
                    self.w.warning(&d, None)?;
                }
                let echo_cfg = self.w.run["code"]
                    .get("build")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                if !echo_cfg.is_empty() {
                    for d in post::diff_config(&echo_cfg, &info.config) {
                        self.w.warning(&d, None)?;
                    }
                }
                if !info.config.is_empty() {
                    self.w.run["code"]["build"] = Value::Object(info.config.clone());
                    self.w.run["code"]["build_flags"] = json!(post::build_flags(&info.config));
                }
                self.w.save_run()?;
            }
            Err(e) => {
                self.w.warning(
                    &format!("could not read the snapshot's Config/Parameters groups: {e}"),
                    None,
                )?;
            }
        }
        Ok(())
    }

    /// Everything else GADGET-4 and the adapter left: diagnostics, the log, the
    /// parameter files.
    fn index_leftovers(&mut self) -> Result<(), Error> {
        let dir = self.w.dir.clone();
        let mut extra: Vec<PathBuf> = Vec::new();
        if let Ok(rd) = fs::read_dir(&self.out_dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_file() {
                    extra.push(p);
                }
            }
        }
        for name in ["stdout.log", "param.txt", "param.txt-usedvalues"] {
            let p = dir.join(name);
            if p.is_file() {
                extra.push(p);
            }
        }
        extra.sort();
        for p in extra {
            let relp = rel(&dir, &p);
            if self.w.has_output(&relp) {
                continue;
            }
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let (kind, format) = if name == "stdout.log" {
                ("log", "txt")
            } else if name == "param.txt"
                || name == "param.txt-usedvalues"
                || name == "parameters-usedvalues"
            {
                ("other", "gadget4-param")
            } else {
                post::classify_file(name)
            };
            // `stop`/`restart`/`cont` marker files are transient controls, not outputs.
            if matches!(name, "stop" | "restart" | "cont") {
                continue;
            }
            let entry = output_entry(&dir, &relp, kind, format, None, None)?;
            self.w.index_output(entry)?;
        }
        Ok(())
    }
}

fn touch(p: &Path) -> Result<(), String> {
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    File::create(p)
        .map(|_| ())
        .map_err(|e| format!("cannot touch {}: {e}", p.display()))
}

/// The problems of a failed launch, one per line, for the CLI.
pub fn describe(problems: &[Problem]) -> String {
    problems
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}
