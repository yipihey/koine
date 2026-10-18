//! The default [`BackgroundRunner`]: tokio tasks, with a `task@1.0.0` row per
//! job so the answer to "what became of it" outlives the request.
//!
//! This is the smallest runner that makes `in_background` answer
//! `JobStarted` (docs/grammar.md § Adverbs; `impress_service_core::job`):
//!
//! * **start** inserts one `task@1.0.0` row (`state: "running"`, the verb, who
//!   asked, when) and spawns the call, which the pipeline has already checked
//!   and which runs through the pipeline again, so policy and audit see it.
//!   The row's id is `job.id`.
//! * **finish** writes `state` (`completed`, or `failed` with the refusal's
//!   `code` and `message` when the verb answered `ok: false` or the pipeline
//!   refused), `finished`, and the verb's answer as `result` (a JSON string).
//! * **cancel** aborts the task and writes `state: "cancelled"`. A task is
//!   aborted at its next await point; a verb that blocks a thread finishes
//!   what it was doing and its answer is discarded.
//! * **status** reads the row. A row that says `running` for a job this
//!   process is not running (a previous launch died) reads as `interrupted`.
//!
//! * **repeat** (`every`, plan-koine-live L-D1) writes a `task@1.0.0` row with
//!   `kind: "routine"` (`verb`, `args` as a JSON string, `every_ms`, `runs`,
//!   `caller`) and loops: run the verb through the pipeline, record the run
//!   (`runs`, `last_result` truncated to 4 KiB, `last_finished`, and
//!   `last_code`/`last_message` when it was refused), sleep the interval,
//!   again, until `cancel`. [`JobRunner::rearm_routines`] re-arms every routine
//!   row that says `running` when the host starts, from the row's own verb,
//!   arguments and interval; one whose verb is no longer linked is written
//!   `interrupted`.
//! * **progress** (L-D3): the verb reports through
//!   `impress_service_core::job::progress`; the runner runs it inside a
//!   task-local sink that writes `progress` (0..1, or null) and
//!   `progress_message` to the row at most twice a second, and the last value
//!   lands when the job ends. A routine's progress is its current run's.
//! * **lines** (plan-koine-tree-stream T-D2): a verb that calls
//!   `impress_service_core::job::emit` is a stream. The runner keeps the last
//!   [`LINES_LIMIT`] values in memory and writes them to the row as `lines`
//!   (a `Value::Array`, so each emitted JSON value stays the structured value
//!   it was) with `line_count`, the total ever emitted, in one `SetPayload`
//!   update at most every 100 ms; what is held lands when the job ends or is
//!   cancelled, before `state` flips. A routine's lines are its runs' lines,
//!   cumulative, with the same limit, and survive a re-arm.
//!   [`JobRunner::lines`] answers `GET /api/job/<id>/lines`.
//!
//! `GET /api/job`, `GET /api/job/<id>` and `POST /api/job/<id>/cancel` are the
//! host's listing, reading and cancelling; the `impel-service_job-*` verbs the
//! job module's docs name belong to impel and are not linked here.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use impress_item::schema::refs;
use impress_item::{
    ActorKind, AttributedStore, FieldMutation, Item, ItemId, ItemQuery, Priority, Value,
};
use impress_service_core::job::{EmitSink, JobHandle, ProgressSink, EMIT, PROGRESS};
use impress_service_core::pipeline::adverbs::BackgroundRunner;
use impress_service_core::pipeline::{self, Call, CallerIdentity};
use impress_service_core::refusal::{codes, Refusal};
use impress_service_core::wire::WIRE_VERSION;
use impress_service_core::{Adverbs, VerbHandle};
use serde_json::{json, Value as Json};

/// Field names of a job's `task@1.0.0` row.
mod field {
    pub const KIND: &str = "kind";
    pub const STATE: &str = "state";
    pub const CALLER: &str = "caller";
    pub const STARTED: &str = "started";
    pub const FINISHED: &str = "finished";
    pub const RESULT: &str = "result";
    pub const CODE: &str = "code";
    pub const MESSAGE: &str = "message";
    pub const VERB: &str = "verb";
    pub const ARGS: &str = "args";
    pub const ADVERBS: &str = "adverbs";
    pub const EVERY_MS: &str = "every_ms";
    pub const RUNS: &str = "runs";
    pub const LAST_RESULT: &str = "last_result";
    pub const LAST_TRUNCATED: &str = "last_truncated";
    pub const LAST_FINISHED: &str = "last_finished";
    pub const LAST_CODE: &str = "last_code";
    pub const LAST_MESSAGE: &str = "last_message";
    pub const PROGRESS: &str = "progress";
    pub const PROGRESS_MESSAGE: &str = "progress_message";
    pub const LINES: &str = "lines";
    pub const LINE_COUNT: &str = "line_count";
}

/// A routine row's `kind`.
const ROUTINE: &str = "routine";

/// The most of a run's answer a routine row keeps.
pub const LAST_RESULT_LIMIT: usize = 4096;

/// The least time between two progress writes to one row.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(500);

/// The least time between two writes of a job's lines to its row.
const LINES_INTERVAL: Duration = Duration::from_millis(100);

/// How many of a job's most recent lines its row keeps.
pub const LINES_LIMIT: usize = 1000;

/// What the runner keeps for a job it is running now.
struct LiveJob {
    abort: tokio::task::AbortHandle,
    progress: Arc<ProgressWriter>,
    /// A routine ([`BackgroundRunner::repeat`]) rather than a one-off job.
    routine: bool,
}

/// Writes a job's reported progress to its row, at most twice a second. A
/// report inside the window is held, and one trailing write lands when the
/// window ends; [`Self::close`] writes whatever is held, so the last value
/// always lands.
struct ProgressWriter {
    store: Arc<dyn AttributedStore>,
    id: ItemId,
    runtime: tokio::runtime::Handle,
    state: Mutex<ProgressState>,
}

#[derive(Default)]
struct ProgressState {
    last_write: Option<Instant>,
    pending: Option<(Option<f32>, String)>,
    trailing: bool,
    closed: bool,
    /// The last [`LINES_LIMIT`] emitted values, oldest first.
    lines: VecDeque<Value>,
    /// Every value ever emitted, including the ones the ring dropped.
    line_count: i64,
    lines_dirty: bool,
    lines_last_write: Option<Instant>,
    lines_trailing: bool,
}

impl ProgressWriter {
    fn new(
        store: Arc<dyn AttributedStore>,
        id: ItemId,
        runtime: tokio::runtime::Handle,
    ) -> Arc<Self> {
        // A re-armed routine carries its lines on: seed from the row.
        let mut state = ProgressState::default();
        if let Ok(Some(item)) = store.get(id) {
            if let Some(Value::Array(held)) = item.payload.get(field::LINES) {
                state.lines = held.iter().cloned().collect();
            }
            if let Some(Value::Int(n)) = item.payload.get(field::LINE_COUNT) {
                state.line_count = *n;
            }
        }
        Arc::new(Self {
            store,
            id,
            runtime,
            state: Mutex::new(state),
        })
    }

    fn emit_sink(self: &Arc<Self>) -> EmitSink {
        let writer = self.clone();
        EmitSink::new(move |line| writer.emit(line))
    }

    /// Append one emitted value; write the lines now when the window since
    /// the last write has passed, else once when it ends.
    fn emit(self: &Arc<Self>, line: Json) {
        let line = serde_json::from_value::<Value>(line.clone())
            .unwrap_or_else(|_| Value::String(line.to_string()));
        let mut state = self.lock();
        if state.closed {
            return;
        }
        state.lines.push_back(line);
        while state.lines.len() > LINES_LIMIT {
            state.lines.pop_front();
        }
        state.line_count += 1;
        state.lines_dirty = true;
        let wait = state.lines_last_write.map_or(Duration::ZERO, |at| {
            LINES_INTERVAL.saturating_sub(at.elapsed())
        });
        if wait.is_zero() {
            self.write_lines(&mut state);
        } else if !state.lines_trailing {
            state.lines_trailing = true;
            let writer = self.clone();
            self.runtime.spawn(async move {
                tokio::time::sleep(wait).await;
                let mut state = writer.lock();
                state.lines_trailing = false;
                if !state.closed {
                    writer.write_lines(&mut state);
                }
            });
        }
    }

    /// One update: the held lines and the running total together, so a
    /// reader never sees one without the other.
    fn write_lines(&self, state: &mut ProgressState) {
        if !state.lines_dirty {
            return;
        }
        state.lines_dirty = false;
        state.lines_last_write = Some(Instant::now());
        if let Err(e) = self.store.update(
            self.id,
            vec![
                FieldMutation::SetPayload(
                    field::LINES.into(),
                    Value::Array(state.lines.iter().cloned().collect()),
                ),
                FieldMutation::SetPayload(field::LINE_COUNT.into(), Value::Int(state.line_count)),
            ],
        ) {
            tracing::warn!(target: "koine-host", "job {}: could not record lines: {e}", self.id);
        }
    }

    fn sink(self: &Arc<Self>) -> ProgressSink {
        let writer = self.clone();
        ProgressSink::new(move |fraction, message| writer.report(fraction, message))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ProgressState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn report(self: &Arc<Self>, fraction: Option<f32>, message: &str) {
        let mut state = self.lock();
        if state.closed {
            return;
        }
        state.pending = Some((fraction, message.to_string()));
        let wait = state.last_write.map_or(Duration::ZERO, |at| {
            PROGRESS_INTERVAL.saturating_sub(at.elapsed())
        });
        if wait.is_zero() {
            self.write_pending(&mut state);
        } else if !state.trailing {
            state.trailing = true;
            let writer = self.clone();
            self.runtime.spawn(async move {
                tokio::time::sleep(wait).await;
                let mut state = writer.lock();
                state.trailing = false;
                if !state.closed {
                    writer.write_pending(&mut state);
                }
            });
        }
    }

    fn write_pending(&self, state: &mut ProgressState) {
        let Some((fraction, message)) = state.pending.take() else {
            return;
        };
        state.last_write = Some(Instant::now());
        let fraction = fraction.map_or(Value::Null, |f| Value::Float(f64::from(f)));
        if let Err(e) = self.store.update(
            self.id,
            vec![
                FieldMutation::SetPayload(field::PROGRESS.into(), fraction),
                FieldMutation::SetPayload(field::PROGRESS_MESSAGE.into(), Value::String(message)),
            ],
        ) {
            tracing::warn!(target: "koine-host", "job {}: could not record progress: {e}", self.id);
        }
    }

    /// Write whatever is held now (a run ended).
    fn flush(&self) {
        let mut state = self.lock();
        self.write_pending(&mut state);
        self.write_lines(&mut state);
    }

    /// Write whatever is held and take no more (the job ended or was
    /// cancelled).
    fn close(&self) {
        let mut state = self.lock();
        self.write_pending(&mut state);
        self.write_lines(&mut state);
        state.closed = true;
    }
}

/// Tokio-task job runner over a store. See the module docs.
pub struct JobRunner {
    store: Arc<dyn AttributedStore>,
    runtime: tokio::runtime::Handle,
    live: Arc<Mutex<HashMap<ItemId, LiveJob>>>,
}

impl JobRunner {
    /// A runner that keeps its rows in `store` and spawns on `runtime`.
    pub fn new(store: Arc<dyn AttributedStore>, runtime: tokio::runtime::Handle) -> Self {
        Self {
            store,
            runtime,
            live: Arc::default(),
        }
    }

    fn live(&self) -> std::sync::MutexGuard<'_, HashMap<ItemId, LiveJob>> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// How many jobs (and routines) this process is running now.
    pub fn running(&self) -> usize {
        self.live().len()
    }

    /// Stop everything this process runs (the host is stopping). A job is
    /// aborted and written `cancelled`; a routine is aborted and its row
    /// left `running`, so the next launch re-arms it
    /// ([`Self::rearm_routines`]).
    pub fn cancel_all(&self) {
        let jobs: Vec<(ItemId, bool)> = self
            .live()
            .iter()
            .map(|(id, job)| (*id, job.routine))
            .collect();
        for (id, routine) in jobs {
            if routine {
                if let Some(job) = self.live().remove(&id) {
                    job.abort.abort();
                    job.progress.close();
                }
            } else {
                let _ = self.cancel(id);
            }
        }
    }

    /// The job's row as the wire shows it, or `None` when there is no such
    /// job.
    pub fn status(&self, id: ItemId) -> Option<Json> {
        let item = self.store.get(id).ok().flatten()?;
        if item.schema != refs::TASK {
            return None;
        }
        Some(self.render(&item))
    }

    fn render(&self, item: &Item) -> Json {
        let id = item.id;
        let text = |key: &str| match item.payload.get(key) {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        };
        let int = |key: &str| match item.payload.get(key) {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        };
        let routine = text(field::KIND).as_deref() == Some(ROUTINE);
        let mut state = text(field::STATE).unwrap_or_default();
        let live = self.live().contains_key(&id);
        if state == "running" && !live {
            state = "interrupted".into();
        }
        let kind = if routine {
            text(field::VERB)
        } else {
            text(field::KIND)
        };
        let mut out = json!({
            "ok": true,
            "wire_version": WIRE_VERSION,
            "job": { "id": id.to_string(), "kind": kind, "state": state },
            "started": text(field::STARTED),
        });
        if let Some(finished) = text(field::FINISHED) {
            out["finished"] = json!(finished);
        }
        if let Some(result) = text(field::RESULT) {
            out["result"] = serde_json::from_str(&result).unwrap_or(Json::String(result));
        }
        if let Some(code) = text(field::CODE) {
            out["code"] = json!(code);
        }
        if let Some(message) = text(field::MESSAGE) {
            out["message"] = json!(message);
        }
        match item.payload.get(field::PROGRESS) {
            Some(Value::Float(f)) => out["progress"] = json!(f),
            Some(Value::Null) => out["progress"] = Json::Null,
            _ => {}
        }
        if let Some(message) = text(field::PROGRESS_MESSAGE) {
            out["progress_message"] = json!(message);
        }
        let (held, total) = line_state(item);
        out["line_count"] = json!(total);
        out["lines_held"] = json!(held.len());
        if routine {
            out["routine"] = json!(true);
            out["verb"] = json!(text(field::VERB));
            out["every_ms"] = json!(int(field::EVERY_MS));
            out["runs"] = json!(int(field::RUNS).unwrap_or(0));
            if let Some(args) = text(field::ARGS) {
                out["args"] = serde_json::from_str(&args).unwrap_or(Json::String(args));
            }
            if let Some(result) = text(field::LAST_RESULT) {
                out["last_result"] = serde_json::from_str(&result).unwrap_or(Json::String(result));
            }
            if matches!(
                item.payload.get(field::LAST_TRUNCATED),
                Some(Value::Bool(true))
            ) {
                out["last_truncated"] = json!(true);
            }
            for key in [field::LAST_FINISHED, field::LAST_CODE, field::LAST_MESSAGE] {
                if let Some(value) = text(key) {
                    out[key] = json!(value);
                }
            }
        }
        out
    }

    /// `GET /api/job/<id>/lines?after=<n>`: the lines from index `after` of
    /// the job's total on, or `None` when there is no such job. The row holds
    /// only the last [`LINES_LIMIT`]; when `after` is older than that the
    /// answer starts at the oldest line kept and `dropped` says how many of
    /// the asked-for lines are gone. `next` is the `after` for the next call.
    pub fn lines(&self, id: ItemId, after: u64) -> Option<Json> {
        let item = self.store.get(id).ok().flatten()?;
        if item.schema != refs::TASK {
            return None;
        }
        let state = self.render(&item)["job"]["state"].clone();
        let (held, total) = line_state(&item);
        let first = total.saturating_sub(held.len() as u64);
        let from = after.max(first);
        let skip = usize::try_from(from - first).unwrap_or(usize::MAX);
        let lines: Vec<Json> = held
            .iter()
            .skip(skip)
            .map(|v| serde_json::to_value(v).unwrap_or(Json::Null))
            .collect();
        Some(json!({
            "ok": true,
            "wire_version": WIRE_VERSION,
            "lines": lines,
            "next": total,
            "total": total,
            "dropped": first.saturating_sub(after),
            "state": state,
        }))
    }

    /// Every job this process is running now, and every routine row (running,
    /// cancelled or interrupted), newest first, each as [`Self::status`]
    /// shows it.
    pub fn list(&self) -> Vec<Json> {
        let mut ids: Vec<ItemId> = self.live().keys().copied().collect();
        let rows = self
            .store
            .query(&ItemQuery {
                schema: Some(refs::TASK),
                ..Default::default()
            })
            .unwrap_or_default();
        let mut out: Vec<Json> = Vec::new();
        for item in rows {
            let routine = matches!(item.payload.get(field::KIND),
                Some(Value::String(kind)) if kind == ROUTINE);
            let live = ids.contains(&item.id);
            if routine || live {
                ids.retain(|id| *id != item.id);
                out.push(self.render(&item));
            }
        }
        // A live job whose row the query did not return still reads by id.
        out.extend(ids.into_iter().filter_map(|id| self.status(id)));
        out.sort_by(|a, b| {
            let started = |v: &Json| v["started"].as_str().unwrap_or_default().to_string();
            started(b).cmp(&started(a))
        });
        out
    }

    /// Cancel a running job. `Ok(true)` when this call stopped it,
    /// `Ok(false)` when it was not running here (finished, or never
    /// started by this process).
    pub fn cancel(&self, id: ItemId) -> Result<bool, Refusal> {
        let job = self.live().remove(&id);
        let Some(job) = job else {
            return Ok(false);
        };
        job.abort.abort();
        job.progress.close();
        write_end(&*self.store, id, "cancelled", None, None, None)?;
        Ok(true)
    }

    /// Re-arm every routine row that says `running` and that this process is
    /// not running, from the row's own verb, arguments and interval. A row
    /// whose verb is no longer linked (or whose interval or arguments cannot
    /// be read) is written `interrupted` with a message. Returns how many
    /// were re-armed. `serve` calls this once, after the runner is
    /// installed.
    pub fn rearm_routines(&self) -> usize {
        let rows = self
            .store
            .query(&ItemQuery {
                schema: Some(refs::TASK),
                ..Default::default()
            })
            .unwrap_or_default();
        let mut armed = 0;
        for item in rows {
            let text = |key: &str| match item.payload.get(key) {
                Some(Value::String(s)) => Some(s.clone()),
                _ => None,
            };
            if text(field::KIND).as_deref() != Some(ROUTINE)
                || text(field::STATE).as_deref() != Some("running")
                || self.live().contains_key(&item.id)
            {
                continue;
            }
            match self.routine_call(&item) {
                Ok((verb, call, interval, runs)) => {
                    self.spawn_routine(item.id, verb, call, interval, runs);
                    armed += 1;
                }
                Err(message) => {
                    tracing::warn!(target: "koine-host", "routine {}: {message}", item.id);
                    let _ = self.store.update(
                        item.id,
                        vec![
                            FieldMutation::SetPayload(
                                field::STATE.into(),
                                Value::String("interrupted".into()),
                            ),
                            FieldMutation::SetPayload(
                                field::MESSAGE.into(),
                                Value::String(message),
                            ),
                            FieldMutation::SetPayload(
                                field::FINISHED.into(),
                                Value::String(Utc::now().to_rfc3339()),
                            ),
                        ],
                    );
                }
            }
        }
        armed
    }

    /// The call a routine row says to repeat, or why it cannot be.
    fn routine_call(&self, item: &Item) -> Result<(VerbHandle, Call, Duration, i64), String> {
        let text = |key: &str| match item.payload.get(key) {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        };
        let name = text(field::VERB).ok_or("the routine row names no verb")?;
        let verb = impress_service_core::call::find(&name).ok_or_else(|| {
            format!("{name} is not linked in this process, so the routine cannot be re-armed")
        })?;
        let every_ms = match item.payload.get(field::EVERY_MS) {
            Some(Value::Int(n)) if *n > 0 => *n as u64,
            _ => return Err("the routine row has no usable every_ms".into()),
        };
        let args = match text(field::ARGS) {
            Some(raw) => serde_json::from_str::<Json>(&raw)
                .map_err(|e| format!("the routine's args are not JSON: {e}"))?,
            None => json!({}),
        };
        let adverbs = match text(field::ADVERBS) {
            Some(raw) => serde_json::from_str::<Adverbs>(&raw)
                .map_err(|e| format!("the routine's adverbs are not readable: {e}"))?,
            None => Adverbs::default(),
        };
        let caller = caller_from_author(&text(field::CALLER).unwrap_or_default());
        let runs = match item.payload.get(field::RUNS) {
            Some(Value::Int(n)) => *n,
            _ => 0,
        };
        Ok((
            verb,
            Call::new(caller, args).with_adverbs(adverbs),
            Duration::from_millis(every_ms.max(1_000)),
            runs,
        ))
    }

    /// A new `task@1.0.0` row, `running`, for a job or a routine.
    fn insert_row(
        &self,
        id: ItemId,
        caller: &CallerIdentity,
        fields: Vec<(&str, Value)>,
    ) -> Result<(), Refusal> {
        let now = Utc::now();
        let author = caller.author();
        let author_kind = match caller {
            CallerIdentity::Person => ActorKind::Human,
            CallerIdentity::Agent(_) | CallerIdentity::Provider(_) => ActorKind::Agent,
            _ => ActorKind::System,
        };
        let mut payload: std::collections::BTreeMap<String, Value> = fields
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect();
        payload.insert(field::STATE.into(), Value::String("running".into()));
        payload.insert(field::CALLER.into(), Value::String(author.clone()));
        payload.insert(field::STARTED.into(), Value::String(now.to_rfc3339()));
        let item = Item {
            id,
            schema: refs::TASK,
            payload: payload.into_iter().collect(),
            created: now,
            modified: now,
            author,
            author_kind,
            logical_clock: 0,
            origin: None,
            canonical_id: None,
            tags: vec![],
            flag: None,
            is_read: false,
            is_starred: false,
            priority: Priority::None,
            visibility: impress_item::Visibility::Private,
            message_type: None,
            produced_by: None,
            version: None,
            batch_id: None,
            references: vec![],
            parent: None,
            extra: Default::default(),
        };
        self.store
            .insert(item)
            .map(|_| ())
            .map_err(|e| Refusal::store(format!("write job row: {e}")))
    }

    /// Spawn the loop of a routine whose row exists. `runs` is how many runs
    /// the row already records.
    fn spawn_routine(
        &self,
        id: ItemId,
        verb: VerbHandle,
        call: Call,
        interval: Duration,
        runs: i64,
    ) {
        let store = self.store.clone();
        let progress = ProgressWriter::new(store.clone(), id, self.runtime.clone());
        let sink = progress.sink();
        let emit = progress.emit_sink();
        let writer = progress.clone();
        let mut table = self.live();
        let task = self.runtime.spawn(async move {
            let mut runs = runs;
            loop {
                let outcome = PROGRESS
                    .scope(
                        sink.clone(),
                        EMIT.scope(
                            emit.clone(),
                            pipeline::invoke_handle(verb.clone(), call.clone()),
                        ),
                    )
                    .await;
                runs += 1;
                writer.flush();
                let (_, result, code, message) = outcome_fields(outcome);
                if let Err(e) = write_run(&*store, id, runs, result, code, message) {
                    tracing::warn!(target: "koine-host", "routine {id}: could not record a run: {e}");
                }
                tokio::time::sleep(interval).await;
            }
        });
        table.insert(
            id,
            LiveJob {
                abort: task.abort_handle(),
                progress,
                routine: true,
            },
        );
    }
}

impl BackgroundRunner for JobRunner {
    fn start(&self, verb: VerbHandle, call: Call) -> Result<JobHandle, Refusal> {
        let id = uuid::Uuid::new_v4();
        let kind = verb.name().to_string();
        self.insert_row(
            id,
            &call.caller,
            vec![(field::KIND, Value::String(kind.clone()))],
        )?;

        let store = self.store.clone();
        let live = self.live.clone();
        let progress = ProgressWriter::new(store.clone(), id, self.runtime.clone());
        let sink = progress.sink();
        let emit = progress.emit_sink();
        let writer = progress.clone();
        // Held across the spawn and the insert, so a job that finishes at
        // once cannot remove an entry that is not there yet.
        let mut table = self.live();
        let task = self.runtime.spawn(async move {
            let outcome = PROGRESS
                .scope(sink, EMIT.scope(emit, pipeline::invoke_handle(verb, call)))
                .await;
            let (state, result, code, message) = outcome_fields(outcome);
            // A cancel removed the entry first and wrote its own end.
            let still_ours = live
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id)
                .is_some();
            if still_ours {
                writer.close();
                if let Err(e) = write_end(&*store, id, state, result, code, message) {
                    tracing::warn!(target: "koine-host", "job {id}: could not record its end: {e}");
                }
            }
        });
        table.insert(
            id,
            LiveJob {
                abort: task.abort_handle(),
                progress,
                routine: false,
            },
        );
        drop(table);
        Ok(JobHandle {
            id: id.to_string(),
            kind,
            state: "running".into(),
        })
    }

    fn repeat(
        &self,
        verb: VerbHandle,
        call: Call,
        interval: Duration,
    ) -> Result<JobHandle, Refusal> {
        let id = uuid::Uuid::new_v4();
        let kind = verb.name().to_string();
        let mut fields = vec![
            (field::KIND, Value::String(ROUTINE.into())),
            (field::VERB, Value::String(kind.clone())),
            (field::ARGS, Value::String(call.args.to_string())),
            (
                field::EVERY_MS,
                Value::Int(i64::try_from(interval.as_millis()).unwrap_or(i64::MAX)),
            ),
            (field::RUNS, Value::Int(0)),
        ];
        if !call.adverbs.is_empty() {
            if let Ok(adverbs) = serde_json::to_string(&call.adverbs) {
                fields.push((field::ADVERBS, Value::String(adverbs)));
            }
        }
        self.insert_row(id, &call.caller, fields)?;
        self.spawn_routine(id, verb, call, interval, 0);
        Ok(JobHandle {
            id: id.to_string(),
            kind,
            state: "running".into(),
        })
    }
}

/// The lines a row holds (oldest first) and how many were ever emitted. A
/// row that never emitted holds none and counts zero.
fn line_state(item: &Item) -> (Vec<Value>, u64) {
    let held = match item.payload.get(field::LINES) {
        Some(Value::Array(held)) => held.clone(),
        _ => Vec::new(),
    };
    let total = match item.payload.get(field::LINE_COUNT) {
        Some(Value::Int(n)) => u64::try_from(*n).unwrap_or(0),
        _ => 0,
    };
    (held, total)
}

/// `human`, `agent:mcp`, `system:impel-taskd`: the inverse of
/// [`CallerIdentity::author`]. An author this does not know runs as the
/// host's own system caller.
fn caller_from_author(author: &str) -> CallerIdentity {
    match author.split_once(':') {
        None if author == "human" => CallerIdentity::Person,
        Some(("agent", name)) => CallerIdentity::agent(name),
        Some(("app", name)) => CallerIdentity::App(name.into()),
        Some(("provider", name)) => CallerIdentity::Provider(name.into()),
        Some(("system", name)) => CallerIdentity::system(name),
        _ => CallerIdentity::system("koine-host"),
    }
}

/// `(state, result, code, message)` of one finished run: `failed` with the
/// refusal's code and message when the verb answered `ok: false` or the
/// pipeline refused.
type Outcome = (&'static str, Option<String>, Option<String>, Option<String>);

fn outcome_fields(outcome: Result<Json, pipeline::PipelineError>) -> Outcome {
    match outcome {
        Ok(value) => {
            let refused = value.get("ok").and_then(Json::as_bool) == Some(false);
            let code = refused
                .then(|| value.get("code").and_then(Json::as_str).map(str::to_string))
                .flatten();
            let message = refused
                .then(|| {
                    value
                        .get("message")
                        .and_then(Json::as_str)
                        .map(str::to_string)
                })
                .flatten();
            (
                if refused { "failed" } else { "completed" },
                Some(value.to_string()),
                code,
                message,
            )
        }
        Err(error) => (
            "failed",
            None,
            Some(codes::VERB_FAILED.to_string()),
            Some(error.to_string()),
        ),
    }
}

/// `text` cut to at most `limit` bytes at a character boundary, and whether
/// it was cut.
fn truncate(text: String, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text, false);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

/// Record one finished run of a routine.
fn write_run(
    store: &dyn AttributedStore,
    id: ItemId,
    runs: i64,
    result: Option<String>,
    code: Option<String>,
    message: Option<String>,
) -> Result<(), Refusal> {
    let (last, truncated) = match result {
        Some(text) => {
            let (text, cut) = truncate(text, LAST_RESULT_LIMIT);
            (Value::String(text), cut)
        }
        None => (Value::Null, false),
    };
    let optional = |value: Option<String>| value.map_or(Value::Null, Value::String);
    store
        .update(
            id,
            vec![
                FieldMutation::SetPayload(field::RUNS.into(), Value::Int(runs)),
                FieldMutation::SetPayload(field::LAST_RESULT.into(), last),
                FieldMutation::SetPayload(field::LAST_TRUNCATED.into(), Value::Bool(truncated)),
                FieldMutation::SetPayload(
                    field::LAST_FINISHED.into(),
                    Value::String(Utc::now().to_rfc3339()),
                ),
                FieldMutation::SetPayload(field::LAST_CODE.into(), optional(code)),
                FieldMutation::SetPayload(field::LAST_MESSAGE.into(), optional(message)),
            ],
        )
        .map_err(|e| Refusal::store(format!("write routine row: {e}")))
}

fn write_end(
    store: &dyn AttributedStore,
    id: ItemId,
    state: &str,
    result: Option<String>,
    code: Option<String>,
    message: Option<String>,
) -> Result<(), Refusal> {
    let mut mutations = vec![
        FieldMutation::SetPayload(field::STATE.into(), Value::String(state.into())),
        FieldMutation::SetPayload(
            field::FINISHED.into(),
            Value::String(Utc::now().to_rfc3339()),
        ),
    ];
    for (key, value) in [
        (field::RESULT, result),
        (field::CODE, code),
        (field::MESSAGE, message),
    ] {
        if let Some(value) = value {
            mutations.push(FieldMutation::SetPayload(key.into(), Value::String(value)));
        }
    }
    store
        .update(id, mutations)
        .map_err(|e| Refusal::store(format!("write job row: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_author_round_trips_to_its_caller() {
        for caller in [
            CallerIdentity::Person,
            CallerIdentity::agent("http"),
            CallerIdentity::App("imbib".into()),
            CallerIdentity::Provider("p".into()),
            CallerIdentity::system("impel-taskd"),
        ] {
            assert_eq!(caller_from_author(&caller.author()), caller);
        }
        assert_eq!(
            caller_from_author("???"),
            CallerIdentity::system("koine-host")
        );
    }

    #[tokio::test]
    async fn progress_writes_at_most_twice_a_second_and_the_last_value_lands() {
        let store: Arc<dyn AttributedStore> = Arc::from(koine_store::open("memory:").unwrap());
        let runner = JobRunner::new(store.clone(), tokio::runtime::Handle::current());
        let id = uuid::Uuid::new_v4();
        runner
            .insert_row(id, &CallerIdentity::Person, vec![])
            .unwrap();
        let writer = ProgressWriter::new(store.clone(), id, tokio::runtime::Handle::current());
        let read = || {
            let item = store.get(id).unwrap().unwrap();
            (
                item.payload.get(field::PROGRESS).cloned(),
                item.payload.get(field::PROGRESS_MESSAGE).cloned(),
            )
        };
        for step in 1..=5u8 {
            writer.report(Some(f32::from(step) / 10.0), &format!("step {step}"));
        }
        // The first report landed; the rest are held.
        let (fraction, message) = read();
        assert_eq!(fraction, Some(Value::Float(f64::from(0.1_f32))));
        assert_eq!(message, Some(Value::String("step 1".into())));
        // One trailing write lands when the window ends, with the last value.
        tokio::time::sleep(PROGRESS_INTERVAL + Duration::from_millis(250)).await;
        assert_eq!(read().1, Some(Value::String("step 5".into())));
        // A report inside a new window is held until close, which writes it.
        writer.report(None, "unknown");
        writer.report(Some(1.0), "done");
        assert_eq!(read().1, Some(Value::String("step 5".into())));
        writer.close();
        assert_eq!(
            read(),
            (Some(Value::Float(1.0)), Some(Value::String("done".into())))
        );
        // Closed: later reports are dropped.
        writer.report(Some(0.0), "late");
        assert_eq!(read().1, Some(Value::String("done".into())));
    }

    #[tokio::test]
    async fn lines_batch_to_one_write_per_window_keep_the_last_thousand_and_land_at_close() {
        let store: Arc<dyn AttributedStore> = Arc::from(koine_store::open("memory:").unwrap());
        let runner = JobRunner::new(store.clone(), tokio::runtime::Handle::current());
        let id = uuid::Uuid::new_v4();
        runner
            .insert_row(id, &CallerIdentity::Person, vec![])
            .unwrap();
        let writer = ProgressWriter::new(store.clone(), id, tokio::runtime::Handle::current());
        let count = || line_state(&store.get(id).unwrap().unwrap()).1;
        for i in 0..2500 {
            writer.emit(json!({ "i": i }));
        }
        // The first emit wrote at once; the rest are held in the window.
        assert_eq!(count(), 1);
        tokio::time::sleep(LINES_INTERVAL + Duration::from_millis(250)).await;
        assert_eq!(count(), 2500);
        writer.emit(json!("last"));
        writer.close();
        let answer = runner.lines(id, 0).unwrap();
        assert_eq!(answer["total"], 2501);
        assert_eq!(answer["lines"].as_array().unwrap().len(), LINES_LIMIT);
        assert_eq!(answer["dropped"], 1501);
        assert_eq!(answer["lines"][LINES_LIMIT - 1], "last");
        assert_eq!(answer["next"], 2501);
        // Paging inside the ring drops nothing.
        let tail = runner.lines(id, 2499).unwrap();
        assert_eq!(tail["dropped"], 0);
        assert_eq!(tail["lines"], json!([{ "i": 2499 }, "last"]));
        assert_eq!(runner.lines(id, 9_999).unwrap()["lines"], json!([]));
        // Closed: later emits are dropped.
        writer.emit(json!("late"));
        assert_eq!(count(), 2501);
        // A new writer over the same row (a re-armed routine) carries on.
        let again = ProgressWriter::new(store.clone(), id, tokio::runtime::Handle::current());
        again.emit(json!("more"));
        assert_eq!(count(), 2502);
        assert!(runner.lines(uuid::Uuid::new_v4(), 0).is_none());
    }

    #[test]
    fn truncation_keeps_a_character_boundary() {
        assert_eq!(truncate("abc".into(), 8), ("abc".into(), false));
        let (cut, was) = truncate("ééé".into(), 3);
        assert!(was);
        assert_eq!(cut, "é");
    }
}
