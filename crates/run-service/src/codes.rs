//! The codes koine can start: a process-wide registry of
//! [`CodeProvider`]s, each a conforming code's name, its emitted parameter
//! schema (`<code> --schema`, R-D2) and how to launch it ([`Launch`]).
//!
//! The plan (R-D13) says a code is "registered by the code's process as a
//! runtime provider". koine's runtime providers (`impress_service_core::
//! provider`, P8) are *verb* providers: a process that serves verbs over an
//! endpoint, with a token and a trust decision. A simulation code serves no
//! verb — it is launched, writes files and exits — so registering it there
//! would mean inventing an endpoint it does not have. What a code hands
//! koine is exactly this registration: its schema and a launch template.
//! The registry is in-process, like `source-service`'s adapter registry;
//! a host registers what it links (`demo_code::register`) and
//! `run-service_register-code` registers one from its binary at run time.
//!
//! What R3 (titan) and R7 (the GADGET-4 adapter) must hand koine is one
//! [`CodeProvider`]: `name`, `version`, the `--schema` output, and a
//! [`Launch`] saying how the run directory, the parameters and the problem
//! reach the process.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use run_convention::check_parameter_schema;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// How the parameters reach the process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ParamsVia {
    /// koine writes the effective values as a `params.json`-shaped file
    /// into the run directory and appends `<flag> <path>` (the same file a
    /// code's `--params` reads, R-D3). The default.
    File,
    /// `--<name>=<json value>` per parameter, as `RunConvention.jl` reads.
    Argv,
    /// Each parameter's `x-run.env` variable (R-D11); a parameter without
    /// one refuses the registration.
    Env,
}

/// How the run directory reaches the process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DirVia {
    /// `<flag> <dir>` on the command line (`--dir` by default).
    Flag,
    /// The process runs with the run directory as its working directory.
    Cwd,
    /// An environment variable named in `dir_name`.
    Env,
}

/// The launch template: what koine runs, and how it passes the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Launch {
    /// The program and its fixed arguments (`["julia", "qstar_run.jl"]`).
    pub argv: Vec<String>,
    /// How the parameters are passed; `file` when absent.
    #[serde(default = "default_params_via")]
    pub params: ParamsVia,
    /// The flag before the params file (`--params` when absent).
    #[serde(default = "default_params_flag")]
    pub params_flag: String,
    /// How the run directory is passed; `flag` when absent.
    #[serde(default = "default_dir_via")]
    pub dir: DirVia,
    /// The flag (`--dir`) or the variable (`RUN_DIR`) that carries it.
    #[serde(default = "default_dir_name")]
    pub dir_name: String,
    /// The flag before the problem as JSON (`--problem`); absent means the
    /// code fixes its own problem and `default_problem` says what it writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem_flag: Option<String>,
    /// The flag before a checkpoint path to resume from (`--from`); absent
    /// means the code cannot resume and `run-service_resume` refuses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_flag: Option<String>,
    /// Extra environment for every launch.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

fn default_params_via() -> ParamsVia {
    ParamsVia::File
}
fn default_params_flag() -> String {
    "--params".into()
}
fn default_dir_via() -> DirVia {
    DirVia::Flag
}
fn default_dir_name() -> String {
    "--dir".into()
}

/// A registered code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CodeProvider {
    /// The code's name, as its `run.json` `code.name` spells it.
    pub name: String,
    pub version: String,
    /// The emitted parameter schema (R-D2.1), valid against
    /// `parameter-schema.schema.json`.
    pub schema: Value,
    pub launch: Launch,
    /// The `problem` the code records when the start names none
    /// (`{"name": "default"}` when absent). The key a start predicts for
    /// reuse (R-D8) is computed over this, so it must be what the code
    /// writes.
    #[serde(default = "default_problem")]
    pub default_problem: Value,
}

fn default_problem() -> Value {
    json!({"name": "default"})
}

impl CodeProvider {
    /// The declared parameters: name → property schema.
    pub fn properties(&self) -> &Map<String, Value> {
        static EMPTY: std::sync::LazyLock<Map<String, Value>> = std::sync::LazyLock::new(Map::new);
        self.schema
            .get("properties")
            .and_then(Value::as_object)
            .unwrap_or(&EMPTY)
    }

    /// `never` | `restart` | `always` of a declared parameter (R-D2.3;
    /// `never` when the schema says nothing), or `None` for an unknown name.
    pub fn mutability(&self, param: &str) -> Option<&str> {
        let prop = self.properties().get(param)?;
        Some(
            prop.pointer("/x-run/mutable")
                .and_then(Value::as_str)
                .unwrap_or("never"),
        )
    }

    /// The declared metric names (R-D6.4).
    pub fn metrics(&self) -> Vec<String> {
        self.schema
            .pointer("/x-run/metrics")
            .and_then(Value::as_array)
            .map(|m| {
                m.iter()
                    .filter_map(|x| x.get("name").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The effective values of a start: the schema's defaults under
    /// `params`, every name checked (R-D2.5: an unknown name is an error,
    /// named) and every value checked against its property schema.
    pub fn resolve(&self, params: &Map<String, Value>) -> Result<Map<String, Value>, String> {
        let props = self.properties();
        let mut unknown: Vec<&String> = params.keys().filter(|k| !props.contains_key(*k)).collect();
        unknown.sort();
        if let Some(first) = unknown.first() {
            let mut names: Vec<&String> = props.keys().collect();
            names.sort();
            return Err(format!(
                "unknown parameter `{first}`{}; {} declares {}",
                if unknown.len() > 1 {
                    format!(
                        " (and {} more: {})",
                        unknown.len() - 1,
                        unknown[1..]
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                } else {
                    String::new()
                },
                self.name,
                names
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let mut values = Map::new();
        for (name, prop) in props {
            match params.get(name) {
                Some(v) => {
                    values.insert(name.clone(), v.clone());
                }
                None => {
                    if let Some(d) = prop.get("default") {
                        values.insert(name.clone(), d.clone());
                    }
                }
            }
        }
        // Types, ranges, enums: the schema's own words, through jsonschema.
        let mut schema = self.schema.clone();
        if let Some(o) = schema.as_object_mut() {
            o.remove("$schema");
            o.insert("additionalProperties".into(), json!(false));
            o.remove("x-run");
        }
        let validator = jsonschema::options()
            .build(&schema)
            .map_err(|e| format!("the schema of {} does not compile: {e}", self.name))?;
        let instance = Value::Object(values.clone());
        if let Some(e) = validator.iter_errors(&instance).next() {
            let at = e.instance_path.to_string();
            return Err(format!(
                "`{}`: {e}",
                at.trim_start_matches('/').replace('/', ".")
            ));
        }
        // R-D2.6: cross-parameter constraints declared on the parameter.
        for (name, prop) in props {
            let Some(reqs) = prop.pointer("/x-run/requires").and_then(Value::as_array) else {
                continue;
            };
            if !params.contains_key(name) {
                continue;
            }
            for r in reqs {
                let Some(other) = r.get("param").and_then(Value::as_str) else {
                    continue;
                };
                match (values.get(other), r.get("equals")) {
                    (None, _) => {
                        return Err(format!("`{name}` requires `{other}` to be set"));
                    }
                    (Some(v), Some(want)) if v != want => {
                        return Err(format!("`{name}` requires `{other}` = {want}, it is {v}"));
                    }
                    _ => {}
                }
            }
        }
        Ok(values)
    }

    /// Check one value for `param` the way `resolve` would.
    pub fn check_value(&self, param: &str, value: &Value) -> Result<(), String> {
        let mut one = Map::new();
        one.insert(param.to_string(), value.clone());
        self.resolve(&one).map(|_| ())
    }
}

/// Validate a provider before it is registered: the schema against the
/// meta-schema, the launch template against what the schema allows.
pub fn validate(code: &CodeProvider) -> Result<(), String> {
    if code.name.trim().is_empty() {
        return Err("a code has a name".into());
    }
    let problems = check_parameter_schema(&code.schema);
    if !problems.is_empty() {
        return Err(format!(
            "the schema of `{}` does not follow R-D2.1: {}",
            code.name,
            problems.join("; ")
        ));
    }
    if code.launch.argv.is_empty() {
        return Err(format!("`{}`: launch.argv names no program", code.name));
    }
    if code.launch.params == ParamsVia::Env {
        let missing: Vec<&String> = code
            .properties()
            .iter()
            .filter(|(_, p)| p.pointer("/x-run/env").is_none())
            .map(|(n, _)| n)
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "`{}` passes parameters by environment but these declare no `x-run.env`: {}",
                code.name,
                missing
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    if !code.default_problem.is_object()
        || code
            .default_problem
            .get("name")
            .and_then(Value::as_str)
            .is_none()
    {
        return Err(format!("`{}`: default_problem needs a `name`", code.name));
    }
    Ok(())
}

static REGISTRY: RwLock<BTreeMap<String, Arc<CodeProvider>>> = RwLock::new(BTreeMap::new());

/// Register (or replace) a code. Refuses an invalid one with the reason.
pub fn register(code: CodeProvider) -> Result<Arc<CodeProvider>, String> {
    validate(&code)?;
    let code = Arc::new(code);
    REGISTRY
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(code.name.clone(), code.clone());
    Ok(code)
}

/// The code named `name`.
pub fn get(name: &str) -> Option<Arc<CodeProvider>> {
    REGISTRY
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(name)
        .cloned()
}

/// Every registered code, by name.
pub fn all() -> Vec<Arc<CodeProvider>> {
    REGISTRY
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .cloned()
        .collect()
}

/// The registered names, for a refusal to list.
pub fn names() -> Vec<String> {
    REGISTRY
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .keys()
        .cloned()
        .collect()
}

/// Ask a code for its schema (`argv + ["--schema"]`) and register it.
/// Refuses when the process fails or prints something that is not a
/// parameter schema.
pub fn register_from_binary(
    name: &str,
    version: &str,
    launch: Launch,
    default_problem: Option<Value>,
) -> Result<Arc<CodeProvider>, String> {
    let (program, rest) = launch
        .argv
        .split_first()
        .ok_or_else(|| "launch.argv names no program".to_string())?;
    let out = std::process::Command::new(program)
        .args(rest)
        .arg("--schema")
        .envs(&launch.env)
        .output()
        .map_err(|e| format!("cannot run `{program} --schema`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`{program} --schema` exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let schema: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("`{program} --schema` did not print JSON: {e}"))?;
    register(CodeProvider {
        name: name.into(),
        version: version.into(),
        schema,
        launch,
        default_problem: default_problem.unwrap_or_else(self::default_problem),
    })
}
