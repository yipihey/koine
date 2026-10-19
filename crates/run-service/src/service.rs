//! `RunService`: the generic verbs over any conforming code (R-D13), none
//! of them per code. Safety, honestly: `codes`, `list`, `status`, `events`
//! and `form` read; `ingest` writes rows (the run row, its ring) and is
//! `mutating, idempotent`; `start`, `resume` and `register-code` launch a
//! process, which leaves this one, so they are `external` (koine's own
//! rule, pinned by koine-host's effects test: a `subprocess` reach is
//! `external`, and the generated form previews before it runs); `set`,
//! `stop` and `checkpoint` write one control file each (`reach: fs`) and
//! are `mutating`, which is what routes them through trust (TR-D4: a
//! 7-hour GPU job's parameter change is a review request); `sweep` files
//! tasks.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use impress_item::item::{ActorKind, Item, ItemId};
use impress_item::operation::{OperationIntent, OperationSpec, OperationType, RetentionTier};
use impress_item::query::ItemQuery;
use impress_item::store::AttributedStore;
use impress_service_core::async_trait;
use impress_service_core::pipeline::{context as call_context, CallerIdentity};
use impress_service_core::refusal::codes as refusal;
#[allow(unused_imports)]
use impress_service_macros::impress_method;
use impress_service_macros::{impress_service, impress_service_impl};
use koine_tasks::job;
use run_convention::{key_of, values_of};
use serde_json::{json, Map, Value};

use crate::codes::{self, CodeProvider, Launch};
use crate::control;
use crate::dto::*;
use crate::ingest::{ingest_dir, Launched};
use crate::kind::{self, AUTHOR, RUN};
use crate::launch;
use crate::sweep;

/// `list`'s default and ceiling.
pub const DEFAULT_LIMIT: usize = 100;
pub const MAX_LIMIT: usize = 500;
/// `events`'s default and ceiling (the ring holds 200).
pub const DEFAULT_EVENTS: usize = 100;
pub const MAX_EVENTS: usize = 200;

#[impress_service]
pub trait RunService: Send + Sync + 'static {
    /// The codes this host can start: each registered code's name, version,
    /// declared parameters, groups, metrics and launch template (R-D12).
    #[impress_method(effects())]
    #[impress_example(name = "inventory", args = r#"{}"#, expect = r#"{"ok": true}"#)]
    async fn codes(&self) -> CodesResult;

    /// Register a code from its binary: runs `launch.argv + ["--schema"]`,
    /// checks what it prints against the parameter meta-schema (R-D2.1) and
    /// adds it to `codes`. `launch` says how the run directory (`dir`:
    /// flag | cwd | env), the parameters (`params`: file | argv | env), the
    /// problem (`problem_flag`) and a checkpoint to resume from
    /// (`resume_flag`) reach the process. Refuses a program that cannot
    /// run, a schema that does not conform, or a launch the schema cannot
    /// serve (`invalid-argument`).
    #[impress_method(safety = external, effects(reach = [subprocess]))]
    #[impress_example(
        name = "no-such-program",
        args = r#"{"name": "nope", "version": "0", "launch": {"argv": ["/nonexistent/koine-no-such-code"]}}"#,
        expect = r#"{"ok": false, "code": "invalid-argument"}"#
    )]
    async fn register_code(
        &self,
        name: String,
        version: String,
        launch: Launch,
        default_problem: Option<Value>,
    ) -> CodesResult;

    /// Ingest a conforming run directory: `run.json` becomes the
    /// `run@1.0.0` row (its id the run's), `events.jsonl` is tailed into
    /// the run's task-event ring under its own `seq`, `outputs.json` and
    /// `notes.json` ride along, and a `failed` run with a `3xx` exit files a
    /// `diagnose` request on the agent desk. Idempotent: a second ingest
    /// advances from `last_event_seq`. Refuses a directory `run-check`
    /// faults (`invalid-argument`, with the findings) unless its record is
    /// `provenance: "imported"` (R-D11).
    #[impress_method(
        safety = mutating,
        idempotent = true,
        effects(
            reads = ["run@1.0.0", "task-event@1.0.0"],
            writes = ["run@1.0.0", "task-event@1.0.0", any("koine/agent-request@1.0.0")],
            reach = [fs]
        )
    )]
    #[impress_example(
        name = "not-a-run",
        args = r#"{"dir": "/nonexistent/koine-run"}"#,
        expect = r#"{"ok": false, "code": "invalid-argument"}"#
    )]
    async fn ingest(&self, dir: String) -> IngestResult;

    /// The runs this store knows, newest start first, narrowed by `state`
    /// (`running`, `done`, `failed`, …), `code` (name), `problem` (name) or
    /// `key` ("the run with these settings"); at most `limit` (default
    /// 100, at most 500).
    #[impress_method(returns_kind = "run", rows(id = "id", title = "name"))]
    #[impress_example(
        name = "done",
        args = r#"{"state": "done"}"#,
        expect = r#"{"ok": true}"#
    )]
    async fn list(
        &self,
        state: Option<String>,
        code: Option<String>,
        problem: Option<String>,
        key: Option<String>,
        limit: Option<u32>,
    ) -> RunsResult;

    /// One run: its record, the last `progress`, the last value of each
    /// metric, the outputs index, the notes, and every control request with
    /// its outcome (`pending`, `applied`, `refused` with the code's reason).
    /// `orphaned` is true when the record says `running` but the process
    /// this host launched has exited (R-D4.4: derived, never written).
    #[impress_method(effects(reads = ["run@1.0.0"], reach = [fs]))]
    #[impress_example(
        name = "missing",
        args = r#"{"id": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn status(&self, id: String) -> StatusResult;

    /// The run's events from its ring, `seq > after_seq`, oldest first, at
    /// most `limit` (default 100, at most 200); `cursor` to continue, `gap`
    /// when the ring was pruned past `after_seq` (the file still has them).
    #[impress_method(effects(reads = ["run@1.0.0", "task-event@1.0.0"]))]
    #[impress_example(
        name = "missing",
        args = r#"{"id": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn events(&self, id: String, after_seq: Option<u64>, limit: Option<u32>) -> EventsResult;

    /// Start a run of a registered code: `params` is checked against its
    /// emitted schema (an unknown name refuses the run, named, R-D2.5;
    /// defaults fill the rest), the key is predicted
    /// (`sha256(canonical({problem, params}))`, R-D4.3) and, unless `force`,
    /// a `done` run with that key, exit 0 and no `invalidates_reuse` is
    /// answered instead of a launch (`reused: true`). Otherwise the run
    /// directory is made (`dir`, else `<workspace>/runs/<code>/<uuid>`),
    /// the process launched as the code's launch template says, and the
    /// directory ingested as it fills — in the background, or to the end
    /// when `wait` is true. `sweep` names the sweep this run belongs to.
    #[impress_method(
        safety = external,
        effects(
            reads = ["run@1.0.0", "task-event@1.0.0"],
            writes = ["run@1.0.0", "task-event@1.0.0", "task@1.0.0", any("koine/agent-request@1.0.0")],
            reach = [fs, subprocess]
        )
    )]
    #[impress_example(
        name = "unknown-code",
        args = r#"{"code": "no-such-code", "params": {}}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn start(
        &self,
        code: String,
        params: Option<Value>,
        problem: Option<Value>,
        dir: Option<String>,
        name: Option<String>,
        force: Option<bool>,
        wait: Option<bool>,
        sweep: Option<String>,
    ) -> StartResult;

    /// Steer a running run: write a `set` request into its control inbox
    /// (R-D8) and answer the request id; `status` reports whether the code
    /// applied or refused it. The parameter's class decides (R-D2.3):
    /// `never` is refused here (`invalid-argument`); `restart` is written
    /// and the run marked as needing a restart (the value is applied by
    /// `resume`); `always` is written for the code to apply.
    #[impress_method(
        safety = mutating,
        effects(reads = ["run@1.0.0"], writes = ["run@1.0.0"], reach = [fs])
    )]
    #[impress_example(
        name = "missing",
        args = r#"{"id": "00000000-0000-4000-8000-000000000000", "param": "io.every", "value": 10}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn set(&self, id: String, param: String, value: Value) -> ControlResult;

    /// Ask a running run to stop (`after`: `checkpoint` to checkpoint
    /// first, `now`), through its control inbox; answers the request id.
    #[impress_method(safety = mutating, effects(reads = ["run@1.0.0"], reach = [fs]))]
    #[impress_example(
        name = "missing",
        args = r#"{"id": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn stop(&self, id: String, after: Option<String>) -> ControlResult;

    /// Ask a running run to write a checkpoint, through its control inbox;
    /// answers the request id.
    #[impress_method(safety = mutating, effects(reads = ["run@1.0.0"], reach = [fs]))]
    #[impress_example(
        name = "missing",
        args = r#"{"id": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn checkpoint(&self, id: String) -> ControlResult;

    /// Resume a stopped run from a checkpoint as a new run (R-D12
    /// `resume(dir; from)`): `from` is a checkpoint path relative to the
    /// run (else the last `checkpoint` event's), the parameters are the
    /// run's with every pending `restart` value from `set` and `params`
    /// applied, and the code is launched with its resume flag, so the child
    /// records `lineage.parent` and the code enforces `never`. Refuses a
    /// run still `running` (`conflict`), a code without a resume flag
    /// (`unsupported`) and a run with no checkpoint (`not-found`).
    #[impress_method(
        safety = external,
        effects(
            reads = ["run@1.0.0", "task-event@1.0.0"],
            writes = ["run@1.0.0", "task-event@1.0.0", any("koine/agent-request@1.0.0")],
            reach = [fs, subprocess]
        )
    )]
    #[impress_example(
        name = "missing",
        args = r#"{"id": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn resume(
        &self,
        id: String,
        from: Option<String>,
        params: Option<Value>,
        dir: Option<String>,
        wait: Option<bool>,
    ) -> StartResult;

    /// Expand a sweep (R-D10: `{code, base, vary: [{param, values |
    /// generator}], combine?}`) into one `run-service_start` task per run
    /// as a koine-tasks workflow: `sweep.json` is written to `dir` (else
    /// `<workspace>/sweeps/<id>`), one `koine.verb` task filed per run for
    /// the host's scheduler, and a parent job answered whose ring records
    /// each run and which finishes when the last has. Every run's
    /// parameters are checked first; one misspelt name files nothing.
    #[impress_method(
        safety = mutating,
        effects(writes = ["task@1.0.0", "task-event@1.0.0"], reach = [fs])
    )]
    #[impress_example(
        name = "no-code",
        args = r#"{"sweep": {"base": {}, "vary": []}}"#,
        expect = r#"{"ok": false, "code": "invalid-argument"}"#
    )]
    async fn sweep(&self, sweep: Value, dir: Option<String>) -> SweepResult;

    /// The generated start form of a code: one field per declared
    /// parameter from its emitted schema, grouped by `x-run.group`, a Run
    /// button that calls `start`. A surface spec `surface_validate` accepts.
    #[impress_method(effects())]
    #[impress_example(
        name = "unknown-code",
        args = r#"{"code": "no-such-code"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn form(&self, code: String) -> FormResult;
}

#[derive(Clone, Default)]
pub struct DefaultRunService;

impl DefaultRunService {
    pub fn new() -> Self {
        Self
    }
}

fn caller() -> CallerIdentity {
    call_context::current()
        .map(|c| c.caller.clone())
        .unwrap_or(CallerIdentity::Person)
}

/// The store the verbs use; a write to a stand-in is refused.
fn store(for_write: bool) -> Result<Arc<dyn AttributedStore>, Refused> {
    let (store, fallback) = koine_store::process::acquire_or_stand_in();
    if for_write && fallback {
        return Err(Refused::new(
            refusal::STORE_UNAVAILABLE,
            "no store is installed in this process; runs need a host (nothing was written)",
        ));
    }
    Ok(store)
}

fn parse_id(id: &str) -> Result<ItemId, Refused> {
    id.trim()
        .parse()
        .map_err(|_| Refused::invalid(format!("'{id}' is not a run id")))
}

fn text(p: &Map<String, Value>, field: &str) -> Option<String> {
    p.get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A run row as its item says it is.
pub fn row_of(item: &Item) -> RunRow {
    let p = kind::payload_json(item);
    let status = p.get("status").cloned().unwrap_or(Value::Null);
    let koine = p.get("koine").cloned().unwrap_or(Value::Null);
    let state = text(&p, "state")
        .or_else(|| {
            status
                .get("state")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "created".into());
    RunRow {
        id: item.id.to_string(),
        name: text(&p, "name"),
        dir: text(&p, "dir").unwrap_or_default(),
        code: p.get("code").cloned().unwrap_or(json!({})),
        problem: p.get("problem").cloned().unwrap_or(json!({})),
        key: text(&p, "key"),
        orphaned: state == "running" && koine.get("exited").is_some(),
        state,
        provenance: text(&p, "provenance"),
        started_at: status
            .get("started_at")
            .and_then(Value::as_str)
            .map(str::to_string),
        ended_at: status
            .get("ended_at")
            .and_then(Value::as_str)
            .map(str::to_string),
        exit: status.get("exit").cloned(),
        last_event_seq: p.get("last_event_seq").and_then(Value::as_u64).unwrap_or(0),
        progress: p.get("progress").cloned(),
        metrics: p.get("metrics").cloned(),
        lineage: p
            .get("lineage")
            .cloned()
            .filter(|l| l.as_object().is_some_and(|o| !o.is_empty())),
        needs_restart: koine
            .get("needs_restart")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        sweep: koine
            .get("sweep")
            .and_then(Value::as_str)
            .map(str::to_string),
        diagnose_request: koine
            .get("diagnose_request")
            .and_then(Value::as_str)
            .map(str::to_string),
        pid: koine.get("pid").and_then(Value::as_u64).map(|p| p as u32),
    }
}

fn get_run(store: &dyn AttributedStore, id: &str) -> Result<Item, Refused> {
    let id = parse_id(id)?;
    match store.get(id) {
        Ok(Some(item)) if item.schema == RUN => Ok(item),
        Ok(Some(item)) => Err(Refused::not_found(format!(
            "{id} is a {}, not a run",
            item.schema
        ))),
        Ok(None) => Err(Refused::not_found(format!("no run {id}"))),
        Err(e) => Err(Refused::new(refusal::STORE_ERROR, e.to_string())),
    }
}

fn runs(store: &dyn AttributedStore, limit: usize) -> Result<Vec<Item>, Refused> {
    let q = ItemQuery {
        schema: Some(RUN),
        predicates: vec![],
        limit: Some(limit),
        include_tags: false,
        include_references: false,
        assume_schema_rare: true,
        ..Default::default()
    };
    store
        .query(&q)
        .map_err(|e| Refused::new(refusal::STORE_ERROR, e.to_string()))
}

fn patch_koine(
    store: &dyn AttributedStore,
    item: &Item,
    edit: impl FnOnce(&mut Map<String, Value>),
) -> Result<(), Refused> {
    let p = kind::payload_json(item);
    let mut koine = p
        .get("koine")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    edit(&mut koine);
    store
        .apply_operation(OperationSpec {
            target_id: item.id,
            op_type: OperationType::SetPayload(
                "koine".into(),
                kind::to_item(&Value::Object(koine)),
            ),
            intent: OperationIntent::Routine,
            reason: None,
            batch_id: None,
            author: AUTHOR.into(),
            author_kind: ActorKind::System,
            retention: RetentionTier::Compactable,
        })
        .map(|_| ())
        .map_err(|e| Refused::new(refusal::STORE_ERROR, e.to_string()))
}

fn code_of(item: &Item) -> Result<Arc<CodeProvider>, Refused> {
    let p = kind::payload_json(item);
    let name = p
        .get("code")
        .and_then(|c| c.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    codes::get(&name).ok_or_else(|| {
        Refused::not_found(format!(
            "the run's code `{name}` is not registered on this host (register-code first); the codes are {}",
            list(&codes::names())
        ))
    })
}

fn list(items: &[String]) -> String {
    if items.is_empty() {
        "none".into()
    } else {
        items.join(", ")
    }
}

fn summary(code: &CodeProvider) -> CodeSummary {
    let mut parameters: Vec<String> = code.properties().keys().cloned().collect();
    parameters.sort();
    let mut groups: Vec<String> = Vec::new();
    for p in code.properties().values() {
        if let Some(g) = p.pointer("/x-run/group").and_then(Value::as_str) {
            if !groups.iter().any(|x| x == g) {
                groups.push(g.to_string());
            }
        }
    }
    CodeSummary {
        name: code.name.clone(),
        version: code.version.clone(),
        parameters,
        groups,
        metrics: code.metrics(),
        default_problem: code.default_problem.clone(),
        launch: Some(code.launch.clone()),
    }
}

fn params_object(params: Option<Value>) -> Result<Map<String, Value>, Refused> {
    match params {
        None | Some(Value::Null) => Ok(Map::new()),
        Some(Value::Object(o)) => Ok(o),
        Some(other) => Err(Refused::invalid(format!(
            "`params` is an object of `component.name: value`, not {other}"
        ))),
    }
}

/// A `done` run with `key`, exit 0 and no `invalidates_reuse` (R-D8).
fn reusable(store: &dyn AttributedStore, key: &str) -> Result<Option<Item>, Refused> {
    let mut candidates: Vec<Item> = runs(store, MAX_LIMIT)?
        .into_iter()
        .filter(|item| {
            let p = kind::payload_json(item);
            text(&p, "key").as_deref() == Some(key)
                && text(&p, "state").as_deref() == Some("done")
                && p.get("status")
                    .and_then(|s| s.pointer("/exit/code"))
                    .and_then(Value::as_i64)
                    == Some(0)
                && p.get("status")
                    .and_then(|s| s.pointer("/exit/invalidates_reuse"))
                    .and_then(Value::as_bool)
                    != Some(true)
        })
        .collect();
    candidates.sort_by_key(|a| std::cmp::Reverse(a.created));
    Ok(candidates.into_iter().next())
}

/// Launch, first-ingest, and follow (now or in the background).
#[allow(clippy::too_many_arguments)]
async fn launch_and_follow(
    store: Arc<dyn AttributedStore>,
    code: Arc<CodeProvider>,
    dir: PathBuf,
    values: Map<String, Value>,
    problem: Value,
    resume_from: Option<PathBuf>,
    launched: Launched,
    wait: bool,
) -> Result<Item, Refused> {
    tokio::task::spawn_blocking(move || {
        let mut launching = launch::launch(&code, &dir, &values, &problem, resume_from.as_deref())?;
        let first = launch::first_ingest(store.as_ref(), &mut launching, &launched)?;
        if wait {
            Ok(launch::follow(store.as_ref(), launching, &launched)?.item)
        } else {
            launch::follow_in_background(store, launching, launched);
            Ok(first.item)
        }
    })
    .await
    .map_err(|e| Refused::new(refusal::INTERNAL, format!("the launch task panicked: {e}")))?
}

#[async_trait::async_trait]
impl RunService for DefaultRunService {
    async fn codes(&self) -> CodesResult {
        CodesResult::answer(CodesAnswer {
            codes: codes::all().iter().map(|c| summary(c)).collect(),
        })
    }

    async fn register_code(
        &self,
        name: String,
        version: String,
        launch: Launch,
        default_problem: Option<Value>,
    ) -> CodesResult {
        let name = name.trim().to_string();
        if name.is_empty() {
            return CodesResult::refused(refusal::INVALID_ARGUMENT, "a code has a name");
        }
        let registered = tokio::task::spawn_blocking(move || {
            codes::register_from_binary(&name, &version, launch, default_problem)
        })
        .await
        .unwrap_or_else(|e| Err(format!("registration panicked: {e}")));
        match registered {
            Ok(code) => CodesResult::answer(CodesAnswer {
                codes: vec![summary(&code)],
            }),
            Err(message) => CodesResult::refused(refusal::INVALID_ARGUMENT, message),
        }
    }

    async fn ingest(&self, dir: String) -> IngestResult {
        let store = match store(true) {
            Ok(s) => s,
            Err(r) => return r.into(),
        };
        let path = PathBuf::from(dir.trim());
        if !path.is_dir() {
            return IngestResult::refused(
                refusal::INVALID_ARGUMENT,
                format!("{} is not a directory", path.display()),
            );
        }
        let done = tokio::task::spawn_blocking(move || {
            ingest_dir(store.as_ref(), &path, &Launched::default())
        })
        .await
        .unwrap_or_else(|e| Err(Refused::new(refusal::INTERNAL, e.to_string())));
        match done {
            Ok(i) => IngestResult::answer(IngestAnswer {
                run: row_of(&i.item),
                events_added: i.events_added,
                findings: i.findings,
            }),
            Err(r) => r.into(),
        }
    }

    async fn list(
        &self,
        state: Option<String>,
        code: Option<String>,
        problem: Option<String>,
        key: Option<String>,
        limit: Option<u32>,
    ) -> RunsResult {
        let store = match store(false) {
            Ok(s) => s,
            Err(r) => return r.into(),
        };
        let limit = match limit {
            None => DEFAULT_LIMIT,
            Some(0) => {
                return RunsResult::refused(refusal::INVALID_ARGUMENT, "`limit` must be at least 1")
            }
            Some(n) if n as usize > MAX_LIMIT => {
                return RunsResult::refused(
                    refusal::INVALID_ARGUMENT,
                    format!("`limit` is at most {MAX_LIMIT}, got {n}"),
                )
            }
            Some(n) => n as usize,
        };
        let items = match runs(store.as_ref(), MAX_LIMIT) {
            Ok(i) => i,
            Err(r) => return r.into(),
        };
        let want = |filter: &Option<String>, have: Option<&str>| match filter {
            None => true,
            Some(f) => have == Some(f.as_str()),
        };
        let mut rows: Vec<RunRow> = items
            .iter()
            .map(row_of)
            .filter(|r| want(&state, Some(&r.state)))
            .filter(|r| want(&code, r.code.get("name").and_then(Value::as_str)))
            .filter(|r| want(&problem, r.problem.get("name").and_then(Value::as_str)))
            .filter(|r| want(&key, r.key.as_deref()))
            .collect();
        rows.sort_by(|a, b| {
            b.started_at
                .cmp(&a.started_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        rows.truncate(limit);
        RunsResult::answer(RunsAnswer { runs: rows })
    }

    async fn status(&self, id: String) -> StatusResult {
        let store = match store(false) {
            Ok(s) => s,
            Err(r) => return r.into(),
        };
        let item = match get_run(store.as_ref(), &id) {
            Ok(i) => i,
            Err(r) => return r.into(),
        };
        let row = row_of(&item);
        let dir = Path::new(&row.dir);
        let p = kind::payload_json(&item);
        StatusResult::answer(StatusAnswer {
            controls: control::outcomes(dir),
            outputs: p
                .get("outputs")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            notes: p.get("notes").cloned(),
            run: row,
        })
    }

    async fn events(&self, id: String, after_seq: Option<u64>, limit: Option<u32>) -> EventsResult {
        let store = match store(false) {
            Ok(s) => s,
            Err(r) => return r.into(),
        };
        let item = match get_run(store.as_ref(), &id) {
            Ok(i) => i,
            Err(r) => return r.into(),
        };
        let limit = match limit {
            None => DEFAULT_EVENTS,
            Some(0) => {
                return EventsResult::refused(
                    refusal::INVALID_ARGUMENT,
                    "`limit` must be at least 1",
                )
            }
            Some(n) => (n as usize).min(MAX_EVENTS),
        };
        let after = after_seq.unwrap_or(0);
        let rows = match job::events_after(store.as_ref(), item.id, after, limit) {
            Ok(r) => r,
            Err(e) => return EventsResult::refused(refusal::STORE_ERROR, e.to_string()),
        };
        let (cursor, gap) = job::cursor_after(&rows, after);
        EventsResult::answer(EventsAnswer {
            events: rows
                .into_iter()
                .map(|r| EventRow {
                    seq: r.seq,
                    kind: r.name,
                    at: r.at,
                    event: serde_json::from_str(&r.payload_json).unwrap_or(Value::Null),
                })
                .collect(),
            cursor,
            gap,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn start(
        &self,
        code: String,
        params: Option<Value>,
        problem: Option<Value>,
        dir: Option<String>,
        name: Option<String>,
        force: Option<bool>,
        wait: Option<bool>,
        sweep: Option<String>,
    ) -> StartResult {
        let Some(provider) = codes::get(code.trim()) else {
            return StartResult::refused(
                refusal::NOT_FOUND,
                format!(
                    "no code `{code}` is registered; the codes are {}",
                    list(&codes::names())
                ),
            );
        };
        let store = match store(true) {
            Ok(s) => s,
            Err(r) => return r.into(),
        };
        let params = match params_object(params) {
            Ok(p) => p,
            Err(r) => return r.into(),
        };
        let values = match provider.resolve(&params) {
            Ok(v) => v,
            Err(message) => return StartResult::refused(refusal::INVALID_ARGUMENT, message),
        };
        let problem = match problem {
            None | Some(Value::Null) => provider.default_problem.clone(),
            Some(p) if p.is_object() && p.get("name").and_then(Value::as_str).is_some() => p,
            Some(_) => {
                return StartResult::refused(
                    refusal::INVALID_ARGUMENT,
                    "`problem` is an object with a `name` (R-D2.4)",
                )
            }
        };
        let key = key_of(&problem, &Value::Object(values.clone()));
        if !force.unwrap_or(false) {
            match reusable(store.as_ref(), &key) {
                Ok(Some(item)) => {
                    return StartResult::answer(StartAnswer {
                        run: row_of(&item),
                        reused: true,
                        key,
                    })
                }
                Ok(None) => {}
                Err(r) => return r.into(),
            }
        }
        let dir = match dir.map(|d| d.trim().to_string()).filter(|d| !d.is_empty()) {
            Some(d) => PathBuf::from(d),
            None => match launch::default_dir(&provider.name) {
                Ok(d) => d,
                Err(r) => return r.into(),
            },
        };
        let launched = Launched {
            pid: None,
            sweep: sweep.filter(|s| !s.trim().is_empty()),
            name: name.filter(|n| !n.trim().is_empty()),
            exited: None,
        };
        match launch_and_follow(
            store,
            provider,
            dir,
            values,
            problem,
            None,
            launched,
            wait.unwrap_or(false),
        )
        .await
        {
            Ok(item) => StartResult::answer(StartAnswer {
                run: row_of(&item),
                reused: false,
                key,
            }),
            Err(r) => r.into(),
        }
    }

    async fn set(&self, id: String, param: String, value: Value) -> ControlResult {
        let store = match store(true) {
            Ok(s) => s,
            Err(r) => return r.into(),
        };
        let item = match get_run(store.as_ref(), &id) {
            Ok(i) => i,
            Err(r) => return r.into(),
        };
        let code = match code_of(&item) {
            Ok(c) => c,
            Err(r) => return r.into(),
        };
        let param = param.trim().to_string();
        let class = match code.mutability(&param) {
            Some(c) => c,
            None => {
                return ControlResult::refused(
                    refusal::INVALID_ARGUMENT,
                    format!(
                        "`{param}` is not a parameter of {}; it declares {}",
                        code.name,
                        {
                            let mut n: Vec<&String> = code.properties().keys().collect();
                            n.sort();
                            n.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                        }
                    ),
                )
            }
        };
        if class == "never" {
            return ControlResult::refused(
                refusal::INVALID_ARGUMENT,
                format!("`{param}` is `never`: fixed once the run starts; a different value is a new run (R-D2.3)"),
            );
        }
        if let Err(message) = code.check_value(&param, &value) {
            return ControlResult::refused(refusal::INVALID_ARGUMENT, message);
        }
        let row = row_of(&item);
        if row.state != "running" {
            return ControlResult::refused(
                refusal::CONFLICT,
                format!(
                    "run {} is {}; only a running run reads its inbox",
                    row.id, row.state
                ),
            );
        }
        let request = json!({"request": "set", "param": param, "value": value});
        let control = match control::write_request(Path::new(&row.dir), request.clone()) {
            Ok(c) => c,
            Err(r) => return r.into(),
        };
        if class == "restart" {
            let (p, v, c) = (param.clone(), value.clone(), control.clone());
            if let Err(r) = patch_koine(store.as_ref(), &item, move |k| {
                let mut pending = k
                    .get("needs_restart")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                pending.retain(|e| e.get("param").and_then(Value::as_str) != Some(p.as_str()));
                pending.push(json!({"param": p, "value": v, "control": c}));
                k.insert("needs_restart".into(), json!(pending));
            }) {
                return r.into();
            }
        }
        let item = store.get(item.id).ok().flatten().unwrap_or(item);
        ControlResult::answer(ControlAnswer {
            run: row_of(&item),
            control,
            request,
        })
    }

    async fn stop(&self, id: String, after: Option<String>) -> ControlResult {
        let store = match store(false) {
            Ok(s) => s,
            Err(r) => return r.into(),
        };
        let item = match get_run(store.as_ref(), &id) {
            Ok(i) => i,
            Err(r) => return r.into(),
        };
        let row = row_of(&item);
        if row.state != "running" {
            return ControlResult::refused(
                refusal::CONFLICT,
                format!(
                    "run {} is {}; only a running run can be stopped",
                    row.id, row.state
                ),
            );
        }
        let mut request = json!({"request": "stop"});
        if let Some(after) = after.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
            if after != "checkpoint" && after != "now" {
                return ControlResult::refused(
                    refusal::INVALID_ARGUMENT,
                    format!("`after` is `checkpoint` or `now`, not `{after}`"),
                );
            }
            request["after"] = json!(after);
        }
        match control::write_request(Path::new(&row.dir), request.clone()) {
            Ok(control) => ControlResult::answer(ControlAnswer {
                run: row,
                control,
                request,
            }),
            Err(r) => r.into(),
        }
    }

    async fn checkpoint(&self, id: String) -> ControlResult {
        let store = match store(false) {
            Ok(s) => s,
            Err(r) => return r.into(),
        };
        let item = match get_run(store.as_ref(), &id) {
            Ok(i) => i,
            Err(r) => return r.into(),
        };
        let row = row_of(&item);
        if row.state != "running" {
            return ControlResult::refused(
                refusal::CONFLICT,
                format!(
                    "run {} is {}; only a running run can checkpoint",
                    row.id, row.state
                ),
            );
        }
        let request = json!({"request": "checkpoint"});
        match control::write_request(Path::new(&row.dir), request.clone()) {
            Ok(control) => ControlResult::answer(ControlAnswer {
                run: row,
                control,
                request,
            }),
            Err(r) => r.into(),
        }
    }

    async fn resume(
        &self,
        id: String,
        from: Option<String>,
        params: Option<Value>,
        dir: Option<String>,
        wait: Option<bool>,
    ) -> StartResult {
        let store = match store(true) {
            Ok(s) => s,
            Err(r) => return r.into(),
        };
        let item = match get_run(store.as_ref(), &id) {
            Ok(i) => i,
            Err(r) => return r.into(),
        };
        let row = row_of(&item);
        if row.state == "running" && !row.orphaned {
            return StartResult::refused(
                refusal::CONFLICT,
                format!(
                    "run {} is still running; stop it (after a checkpoint) first",
                    row.id
                ),
            );
        }
        let code = match code_of(&item) {
            Ok(c) => c,
            Err(r) => return r.into(),
        };
        if code.launch.resume_flag.is_none() {
            return StartResult::refused(
                "unsupported",
                format!(
                    "{} declares no resume flag; it cannot resume from a checkpoint",
                    code.name
                ),
            );
        }
        let run_dir = PathBuf::from(&row.dir);
        // The checkpoint: named, or the last `checkpoint` event's path.
        let from = match from.map(|f| f.trim().to_string()).filter(|f| !f.is_empty()) {
            Some(f) => f,
            None => {
                let last = crate::ingest::read_events(&run_dir)
                    .iter()
                    .rev()
                    .find(|e| e.get("kind").and_then(Value::as_str) == Some("checkpoint"))
                    .and_then(|e| e.get("path").and_then(Value::as_str).map(str::to_string));
                match last {
                    Some(p) => p,
                    None => {
                        return StartResult::refused(
                            refusal::NOT_FOUND,
                            format!("run {} wrote no checkpoint to resume from", row.id),
                        )
                    }
                }
            }
        };
        let ckpt = if Path::new(&from).is_absolute() {
            PathBuf::from(&from)
        } else {
            run_dir.join(&from)
        };
        if !ckpt.is_file() {
            return StartResult::refused(
                refusal::NOT_FOUND,
                format!("no checkpoint at {}", ckpt.display()),
            );
        }
        // The parameters: the run's, then the pending restart values, then
        // the caller's.
        let mut merged: Map<String, Value> = std::fs::read_to_string(run_dir.join("params.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .map(|p| values_of(&p))
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        for pending in &row.needs_restart {
            if let (Some(p), Some(v)) = (
                pending.get("param").and_then(Value::as_str),
                pending.get("value"),
            ) {
                merged.insert(p.to_string(), v.clone());
            }
        }
        match params_object(params) {
            Ok(extra) => merged.extend(extra),
            Err(r) => return r.into(),
        }
        let values = match code.resolve(&merged) {
            Ok(v) => v,
            Err(message) => return StartResult::refused(refusal::INVALID_ARGUMENT, message),
        };
        let problem = row.problem.clone();
        let key = key_of(&problem, &Value::Object(values.clone()));
        let dir = match dir.map(|d| d.trim().to_string()).filter(|d| !d.is_empty()) {
            Some(d) => PathBuf::from(d),
            None => {
                let n = std::fs::read_dir(run_dir.parent().unwrap_or(&run_dir))
                    .map(|rd| rd.count())
                    .unwrap_or(0);
                run_dir.with_file_name(format!(
                    "{}-resume-{n}",
                    run_dir
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_default()
                ))
            }
        };
        let launched = Launched {
            pid: None,
            sweep: row.sweep.clone(),
            name: row.name.clone(),
            exited: None,
        };
        let parent = item.clone();
        match launch_and_follow(
            store.clone(),
            code,
            dir,
            values,
            problem,
            Some(ckpt),
            launched,
            wait.unwrap_or(false),
        )
        .await
        {
            Ok(child) => {
                let child_id = child.id.to_string();
                if let Err(r) = patch_koine(store.as_ref(), &parent, move |k| {
                    k.insert("needs_restart".into(), json!([]));
                    k.insert("resumed_by".into(), json!(child_id));
                }) {
                    tracing::warn!(run = %parent.id, "could not clear needs_restart: {r}");
                }
                StartResult::answer(StartAnswer {
                    run: row_of(&child),
                    reused: false,
                    key,
                })
            }
            Err(r) => r.into(),
        }
    }

    async fn sweep(&self, sweep: Value, dir: Option<String>) -> SweepResult {
        let store = match store(true) {
            Ok(s) => s,
            Err(r) => return r.into(),
        };
        if !sweep.is_object() {
            return SweepResult::refused(
                refusal::INVALID_ARGUMENT,
                "`sweep` is a sweep.json object (R-D10)",
            );
        }
        let Some(code_name) = sweep
            .get("code")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|c| !c.is_empty())
        else {
            return SweepResult::refused(
                refusal::INVALID_ARGUMENT,
                "the sweep names no `code`; a sweep over the `code` parameter itself is not supported by this host",
            );
        };
        let Some(code) = codes::get(code_name) else {
            return SweepResult::refused(
                refusal::NOT_FOUND,
                format!(
                    "no code `{code_name}` is registered; the codes are {}",
                    list(&codes::names())
                ),
            );
        };
        let dir =
            match dir.map(|d| d.trim().to_string()).filter(|d| !d.is_empty()) {
                Some(d) => PathBuf::from(d),
                None => match koine_store::process::workspace() {
                    Some(ws) => ws.join("sweeps").join(uuid::Uuid::new_v4().to_string()),
                    None => return SweepResult::refused(
                        refusal::INVALID_ARGUMENT,
                        "no `dir` given and this host names no workspace; say where the sweep goes",
                    ),
                },
            };
        let who = caller();
        let filed = tokio::task::spawn_blocking(move || {
            sweep::file(store.as_ref(), &code, sweep, &dir, &who)
        })
        .await
        .unwrap_or_else(|e| Err(Refused::new(refusal::INTERNAL, e.to_string())));
        match filed {
            Ok(f) => SweepResult::answer(SweepAnswer {
                sweep: f.id,
                dir: f.dir.to_string_lossy().into_owned(),
                job: f.job.to_string(),
                tasks: f.tasks.iter().map(ToString::to_string).collect(),
                planned: f.planned as u32,
            }),
            Err(r) => r.into(),
        }
    }

    async fn form(&self, code: String) -> FormResult {
        let Some(provider) = codes::get(code.trim()) else {
            return FormResult::refused(
                refusal::NOT_FOUND,
                format!(
                    "no code `{code}` is registered; the codes are {}",
                    list(&codes::names())
                ),
            );
        };
        let spec = crate::form::start_form(&provider);
        FormResult::answer(FormAnswer {
            spec: serde_json::to_value(&spec).unwrap_or(Value::Null),
        })
    }
}

impress_service_impl! {
    service = RunService,
    safety = read_only,
    since = "0.1.0",
    effects = { reads: ["run@1.0.0"], writes: [], reach: [] },
    impl = DefaultRunService,
    instance = DefaultRunService::new,
    methods = [
        /// The codes this host can start.
        codes() -> CodesResult,
        /// Register a code from its binary's `--schema`.
        register_code(
            /// The code's name, as its run.json `code.name` spells it.
            name: String,
            /// The code's version.
            version: String,
            /// `{argv, params: file|argv|env, params_flag, dir: flag|cwd|env, dir_name, problem_flag?, resume_flag?, env}`.
            launch: Launch,
            /// The `problem` the code records when a start names none (`{"name": "default"}`).
            default_problem: Option<Value>
        ) -> CodesResult,
        /// Ingest a conforming run directory as a run@1.0.0 row and its event ring.
        ingest(
            /// The run directory (run.json, params.json, events.jsonl, …).
            dir: String
        ) -> IngestResult,
        /// The runs this store knows, newest first.
        list(
            /// Only runs in this state: created, running, suspended, done, failed, cancelled.
            state: Option<String>,
            /// Only runs of this code (name).
            code: Option<String>,
            /// Only runs of this problem (name).
            problem: Option<String>,
            /// Only runs with this key (same problem and parameters).
            key: Option<String>,
            /// At most this many (default 100, at most 500).
            limit: Option<u32>
        ) -> RunsResult,
        /// One run: record, last progress, metrics, outputs, notes, control outcomes.
        status(
            /// The run's id.
            id: String
        ) -> StatusResult,
        /// The run's events from its ring, after a cursor.
        events(
            /// The run's id.
            id: String,
            /// Events with seq greater than this (0 for the start).
            after_seq: Option<u64>,
            /// At most this many (default 100, at most 200).
            limit: Option<u32>
        ) -> EventsResult,
        /// Start a run of a registered code, or answer the done run with the same key.
        start(
            /// A registered code's name (`codes`).
            code: String,
            /// `{"component.name": value, ...}`; unknown names are refused, defaults fill the rest.
            params: Option<Value>,
            /// `{name, version?, params?}` of the problem; the code's default when absent.
            problem: Option<Value>,
            /// The run directory to create; `<workspace>/runs/<code>/<uuid>` when absent.
            dir: Option<String>,
            /// A human alias for the run.
            name: Option<String>,
            /// Launch even when a done run with the same key exists.
            force: Option<bool>,
            /// Follow the process to its end before answering.
            wait: Option<bool>,
            /// The sweep this run belongs to (filled by `sweep`).
            sweep: Option<String>
        ) -> StartResult,
        /// Write a `set` request into a running run's control inbox.
        set(
            /// The run's id.
            id: String,
            /// The parameter (`component.name`); must be `restart` or `always`.
            param: String,
            /// The new value.
            value: Value
        ) -> ControlResult,
        /// Ask a running run to stop.
        stop(
            /// The run's id.
            id: String,
            /// `checkpoint` (checkpoint first) or `now`.
            after: Option<String>
        ) -> ControlResult,
        /// Ask a running run to write a checkpoint.
        checkpoint(
            /// The run's id.
            id: String
        ) -> ControlResult,
        /// Resume a stopped run from a checkpoint, as a child run.
        resume(
            /// The run's id.
            id: String,
            /// A checkpoint path (relative to the run); the last checkpoint when absent.
            from: Option<String>,
            /// Parameter values to change on the restart (`restart` or `always` class).
            params: Option<Value>,
            /// The child's run directory; `<run>-resume-<n>` when absent.
            dir: Option<String>,
            /// Follow the process to its end before answering.
            wait: Option<bool>
        ) -> StartResult,
        /// Expand a sweep into one start task per run; answers the parent job.
        sweep(
            /// `{code, name?, base: {param: value}, vary: [{param, values | generator: {kind, ...}}], combine?: product|zip}`.
            sweep: Value,
            /// The sweep directory; `<workspace>/sweeps/<id>` when absent.
            dir: Option<String>
        ) -> SweepResult,
        /// The generated start form of a code, grouped by x-run.group.
        form(
            /// A registered code's name.
            code: String
        ) -> FormResult,
    ],
}
