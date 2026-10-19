//! `gadget4-adapt`: make a GADGET-4 run a conforming run directory.
//!
//! ```text
//! gadget4-adapt schema <echo.txt|param-reference.txt> [-o schema.json]
//! gadget4-adapt render <params.json> [--schema schema.json] [-o param.txt]
//! gadget4-adapt launch <run-dir> --params params.json [--schema schema.json]
//!                      [--mpi N] [--name NAME] [--problem NAME]
//!                      [--poll-ms N] [--progress-every N] -- <gadget4> [args after param.txt]
//! gadget4-adapt adopt  <gadget4-output-dir> [--name NAME] [--problem NAME] [--log stdout.log]
//! gadget4-adapt --schema
//! gadget4-adapt run --dir <run-dir> --params <koine-input.json> [--from <ckpt>]
//!                   [--problem <json>] [--schema schema.json] [--poll-ms N] [-- <gadget4> [--mpi N] [args]]
//! ```
//! The last two are the launch contract of koine's `run-service` (the
//! run convention's R-D13): the binary and the rank count come from
//! `GADGET4_BIN`/`GADGET4_MPI`/`GADGET4_ARGS` (see `run_gadget4::provider`).
//! Exit status: 0 ok; 1 the run or operation failed (the labelled exit is
//! printed on stderr); 2 usage; 3 input refused before anything was written
//! (the `2xx` problems are printed).

use run_gadget4::error::Error;
use run_gadget4::{adopt, echo, launch, render, schema, AdoptOptions, LaunchOptions};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "usage:
  gadget4-adapt schema <echo.txt|param-reference.txt> [-o schema.json]
  gadget4-adapt render <params.json> [--schema schema.json] [-o param.txt]
  gadget4-adapt launch <run-dir> --params params.json [--schema schema.json] [--mpi N]
                       [--name NAME] [--problem NAME] [--poll-ms N] [--progress-every N]
                       -- <gadget4 binary> [args after param.txt, e.g. 0]
  gadget4-adapt adopt <gadget4-output-dir> [--name NAME] [--problem NAME] [--log stdout.log]
  gadget4-adapt --schema
  gadget4-adapt run --dir <run-dir> --params <koine-input.json> [--from <restartfile|restartfiles dir|snapshot>]
                    [--problem <json>] [--schema schema.json] [--poll-ms N] [-- <gadget4> [--mpi N] [args]]
                    (binary: $GADGET4_BIN, ranks: $GADGET4_MPI, extra args: $GADGET4_ARGS)";

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

fn read_json(p: &str) -> Result<Value, String> {
    let t = std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?;
    serde_json::from_str(&t).map_err(|e| format!("{p}: not JSON: {e}"))
}

fn refuse(e: Error) -> ExitCode {
    eprintln!("{e}");
    match e {
        Error::Invalid(_) => ExitCode::from(3),
        Error::Other(_) => ExitCode::from(1),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("schema") => cmd_schema(&args[1..]),
        Some("render") => cmd_render(&args[1..]),
        Some("launch") => cmd_launch(&args[1..]),
        Some("adopt") => cmd_adopt(&args[1..]),
        Some("run") => cmd_run(&args[1..]),
        Some("--schema") if args.len() == 1 => cmd_emit_schema(),
        _ => usage(),
    }
}

/// Split `--flag value` pairs from positionals; `--` ends flag parsing.
struct Parsed {
    positional: Vec<String>,
    flags: Vec<(String, String)>,
    rest: Vec<String>,
}

fn parse(args: &[String]) -> Option<Parsed> {
    let mut p = Parsed {
        positional: Vec::new(),
        flags: Vec::new(),
        rest: Vec::new(),
    };
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            p.rest = args[i + 1..].to_vec();
            break;
        }
        if a.starts_with('-') && a.len() > 1 {
            let v = args.get(i + 1)?.clone();
            p.flags.push((a.clone(), v));
            i += 2;
        } else {
            p.positional.push(a.clone());
            i += 1;
        }
    }
    Some(p)
}

impl Parsed {
    fn flag(&self, name: &str) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
    fn only(&self, allowed: &[&str]) -> bool {
        self.flags
            .iter()
            .all(|(k, _)| allowed.contains(&k.as_str()))
    }
}

fn cmd_schema(args: &[String]) -> ExitCode {
    let Some(p) = parse(args) else { return usage() };
    if p.positional.len() != 1 || !p.rest.is_empty() || !p.only(&["-o"]) {
        return usage();
    }
    let src = &p.positional[0];
    let text = match std::fs::read_to_string(src) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{src}: {e}");
            return ExitCode::from(1);
        }
    };
    let e = echo::parse(&text);
    if e.params.is_empty() {
        eprintln!("{src}: no `Tag value` lines found");
        return ExitCode::from(1);
    }
    if !e.duplicates.is_empty() {
        eprintln!(
            "note: tags repeated in {src} (GADGET-4 refuses these; the first value is used): {}",
            e.duplicates.join(", ")
        );
    }
    let name = std::path::Path::new(src)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| src.clone());
    match schema::harvest(&e, &name) {
        Ok(s) => {
            let out = serde_json::to_string_pretty(&s).unwrap_or_default() + "\n";
            match p.flag("-o") {
                Some(path) => {
                    if let Err(err) = std::fs::write(path, out) {
                        eprintln!("{path}: {err}");
                        return ExitCode::from(1);
                    }
                    eprintln!("{path}: {} parameters", schema::names(&s).len());
                }
                None => print!("{out}"),
            }
            ExitCode::SUCCESS
        }
        Err(ps) => {
            for p in ps {
                eprintln!("{p}");
            }
            ExitCode::from(3)
        }
    }
}

fn cmd_render(args: &[String]) -> ExitCode {
    let Some(p) = parse(args) else { return usage() };
    if p.positional.len() != 1 || !p.rest.is_empty() || !p.only(&["--schema", "-o"]) {
        return usage();
    }
    let params = match read_json(&p.positional[0]) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let sch = match p.flag("--schema").map(read_json).transpose() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    match render::render(&params, sch.as_ref()) {
        Ok(text) => {
            match p.flag("-o") {
                Some(path) => {
                    if let Err(e) = std::fs::write(path, text) {
                        eprintln!("{path}: {e}");
                        return ExitCode::from(1);
                    }
                }
                None => print!("{text}"),
            }
            ExitCode::SUCCESS
        }
        Err(ps) => refuse(Error::Invalid(ps)),
    }
}

fn cmd_launch(args: &[String]) -> ExitCode {
    let Some(p) = parse(args) else { return usage() };
    if p.positional.len() != 1
        || p.rest.is_empty()
        || !p.only(&[
            "--params",
            "--schema",
            "--mpi",
            "--name",
            "--problem",
            "--poll-ms",
            "--progress-every",
        ])
    {
        return usage();
    }
    let Some(params_path) = p.flag("--params") else {
        eprintln!("launch needs --params <params.json>");
        return usage();
    };
    let params = match read_json(params_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let mut o = LaunchOptions::new(PathBuf::from(&p.positional[0]), params, p.rest.clone());
    o.params_name = std::path::Path::new(params_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "params.json".into());
    if let Some(s) = p.flag("--schema") {
        match read_json(s) {
            Ok(v) => o.schema = Some(v),
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(1);
            }
        }
    }
    o.name = p.flag("--name").map(str::to_string);
    if let Some(v) = p.flag("--problem") {
        o.problem = v.to_string();
    }
    if let Some(v) = p.flag("--mpi") {
        match v.parse() {
            Ok(n) => o.mpi = Some(n),
            Err(_) => return usage(),
        }
    }
    if let Some(v) = p.flag("--poll-ms") {
        match v.parse() {
            Ok(n) => o.poll = Duration::from_millis(n),
            Err(_) => return usage(),
        }
    }
    if let Some(v) = p.flag("--progress-every") {
        match v.parse() {
            Ok(n) => o.progress_every = n,
            Err(_) => return usage(),
        }
    }
    match launch(&o) {
        Ok(out) => report_exit(&out.run_dir, &out.exit),
        Err(e) => refuse(e),
    }
}

fn report_exit(dir: &std::path::Path, e: &run_gadget4::exitmap::Exit) -> ExitCode {
    eprintln!(
        "{}: {} exit {} {}{}",
        dir.display(),
        e.state,
        e.code,
        e.label,
        e.message
            .as_ref()
            .map(|m| format!(" ({m})"))
            .unwrap_or_default()
    );
    if e.code == 0 || e.state == "suspended" {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn cmd_adopt(args: &[String]) -> ExitCode {
    let Some(p) = parse(args) else { return usage() };
    if p.positional.len() != 1 || !p.rest.is_empty() || !p.only(&["--name", "--problem", "--log"]) {
        return usage();
    }
    let o = AdoptOptions {
        name: p.flag("--name").map(str::to_string),
        problem: p
            .flag("--problem")
            .map(str::to_string)
            .unwrap_or_else(|| AdoptOptions::default().problem),
        log: p.flag("--log").map(PathBuf::from),
    };
    let dir = PathBuf::from(&p.positional[0]);
    match adopt(&dir, &o) {
        Ok(e) => report_exit(&dir, &e),
        Err(e) => refuse(e),
    }
}

/// `--schema`: the parameter schema (R-D2.1), from the committed echo or the
/// one `$GADGET4_ECHO` names.
fn cmd_emit_schema() -> ExitCode {
    match own_schema() {
        Ok(s) => {
            println!("{}", serde_json::to_string_pretty(&s).unwrap_or_default());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(1)
        }
    }
}

fn own_schema() -> Result<Value, String> {
    match std::env::var_os("GADGET4_ECHO").filter(|p| !p.is_empty()) {
        Some(p) => {
            let p = PathBuf::from(p);
            let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            run_gadget4::provider::schema_from_echo(
                &text,
                &p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            )
        }
        None => Ok(run_gadget4::builtin_schema()),
    }
}

/// `run`: what koine's `run-service` starts and resumes (see the crate docs of
/// `run_gadget4::provider`).
fn cmd_run(args: &[String]) -> ExitCode {
    // run-service registers a code by running `launch.argv + ["--schema"]`,
    // and this code's argv is `[gadget4-adapt, run]`.
    if args == ["--schema"] {
        return cmd_emit_schema();
    }
    let Some(p) = parse(args) else { return usage() };
    if !p.positional.is_empty()
        || !p.only(&[
            "--dir",
            "--params",
            "--from",
            "--problem",
            "--schema",
            "--poll-ms",
        ])
    {
        return usage();
    }
    let (Some(dir), Some(params_path)) = (p.flag("--dir"), p.flag("--params")) else {
        eprintln!("run needs --dir <run-dir> and --params <file>");
        return usage();
    };
    let params = match read_json(params_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    // The binary: after `--`, else $GADGET4_BIN; the rank count likewise.
    let mut command = p.rest.clone();
    let mut mpi: Option<u32> = std::env::var("GADGET4_MPI")
        .ok()
        .and_then(|v| v.parse().ok());
    if command.is_empty() {
        match std::env::var("GADGET4_BIN").ok().filter(|b| !b.is_empty()) {
            Some(b) => {
                command.push(b);
                if let Ok(extra) = std::env::var("GADGET4_ARGS") {
                    command.extend(extra.split_whitespace().map(str::to_string));
                }
            }
            None => {
                eprintln!("run needs the GADGET-4 binary: GADGET4_BIN, or after `--`");
                return ExitCode::from(2);
            }
        }
    } else if command.get(1).map(String::as_str) == Some("--mpi") {
        match command.get(2).and_then(|n| n.parse().ok()) {
            Some(n) => {
                mpi = Some(n);
                command.drain(1..3);
            }
            None => return usage(),
        }
    }
    let schema = match p.flag("--schema") {
        Some(s) => read_json(s),
        None => own_schema(),
    };
    let schema = match schema {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let problem = match p.flag("--problem").map(serde_json::from_str::<Value>) {
        None => json!({
            "name": run_gadget4::DEFAULT_PROBLEM_NAME,
            "version": run_gadget4::provider::build_tag(),
        }),
        Some(Ok(v)) if v.get("name").and_then(Value::as_str).is_some() => v,
        Some(_) => {
            eprintln!("--problem is a JSON object with a `name` (R-D2.4)");
            return ExitCode::from(2);
        }
    };
    let mut o = LaunchOptions::new(PathBuf::from(dir), params, command);
    o.params_name = "koine-input.json".into();
    o.schema = Some(schema);
    o.problem_json = Some(problem);
    o.mpi = mpi;
    o.resume_from = p.flag("--from").map(PathBuf::from);
    if let Some(v) = p.flag("--poll-ms") {
        match v.parse() {
            Ok(n) => o.poll = Duration::from_millis(n),
            Err(_) => return usage(),
        }
    }
    match launch(&o) {
        Ok(out) => report_exit(&out.run_dir, &out.exit),
        Err(e) => refuse(e),
    }
}
