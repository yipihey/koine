//! What koine's `run-service` needs to start GADGET-4 through its generic
//! verbs (R-D9.1, R-D13): the schema `gadget4-adapt --schema` prints and the
//! `run-service_register-code` arguments that launch `gadget4-adapt run`.
//!
//! This crate does not depend on `run-service` (a demo host should not carry
//! GADGET-4, and the adapter should not carry koine's store): [`provider`]
//! returns the registration as JSON, the arguments of the verb, which a host
//! passes to `run-service_register-code` and a person can paste.
//!
//! **The launch contract.** run-service runs
//! `argv + [--dir D, --params koine-input.json, --problem JSON, (--from CKPT)]`
//! and appends nothing else, so there is nowhere after a `--` for the GADGET-4
//! binary. It goes in `launch.env` instead, which the contract provides for
//! exactly what is fixed per host and not per run:
//!
//! | variable | meaning |
//! |---|---|
//! | `GADGET4_BIN` | the GADGET-4 executable (required) |
//! | `GADGET4_MPI` | ranks: run as `mpirun -np N <bin>` (absent: no mpirun) |
//! | `GADGET4_ARGS` | extra arguments after the parameter file, whitespace-split (the real code's special options; the fake's `steps=`) |
//! | `GADGET4_ECHO` | a startup echo / `parameters-usedvalues` to harvest the schema from instead of the committed one (a different build has different tags) |
//!
//! The binary and the rank count belong to the host that registers the code,
//! not to a run: the key a run is reused by is computed over the parameters
//! and the problem, never over where the code lives. (`gadget4-adapt run ...
//! -- <bin> [--mpi N]` also works by hand and overrides the environment.)

use crate::{echo, schema};
use serde_json::{json, Value};
use std::path::Path;

/// `problem.name` a start gets when it names none.
pub const DEFAULT_PROBLEM_NAME: &str = "gadget4";

/// The recorded startup echo the committed schema is harvested from.
const ECHO: &str = include_str!("../fixtures/startup-echo.txt");

/// The parameter schema of the recorded build (R-D2.1), harvested from the
/// echo of a real run and `data/parameters.tsv`; needs no GADGET-4.
pub fn builtin_schema() -> Value {
    schema_from_echo(ECHO, "startup-echo.txt").expect("the committed echo harvests")
}

/// Harvest a schema from echo text (a startup echo, `parameters-usedvalues`
/// or a `param.txt`).
pub fn schema_from_echo(text: &str, name: &str) -> Result<Value, String> {
    let e = echo::parse(text);
    if e.params.is_empty() {
        return Err(format!("{name}: no `Tag value` lines found"));
    }
    schema::harvest(&e, name).map_err(|ps| {
        ps.iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    })
}

/// The build tag of the recorded echo: `4.0+6e4f112`.
pub fn build_tag() -> String {
    let e = echo::parse(ECHO);
    let v = e.version.unwrap_or_else(|| "unknown".into());
    match e.git_commit {
        Some(c) => format!("{v}+{}", &c[..7]),
        None => v,
    }
}

/// The arguments of `run-service_register-code` for GADGET-4:
/// `adapt` is the `gadget4-adapt` binary, `gadget4` the GADGET-4 executable,
/// `mpi` the rank count.
pub fn provider(adapt: &Path, gadget4: &Path, mpi: Option<u32>) -> Value {
    let mut env = serde_json::Map::new();
    env.insert("GADGET4_BIN".into(), json!(gadget4.to_string_lossy()));
    if let Some(n) = mpi {
        env.insert("GADGET4_MPI".into(), json!(n.to_string()));
    }
    json!({
        "name": "GADGET-4",
        "version": build_tag(),
        "launch": {
            "argv": [adapt.to_string_lossy(), "run"],
            "params": "file",
            "params_flag": "--params",
            "dir": "flag",
            "dir_name": "--dir",
            "problem_flag": "--problem",
            "resume_flag": "--from",
            "env": Value::Object(env),
        },
        "default_problem": {"name": DEFAULT_PROBLEM_NAME, "version": build_tag()},
    })
}
