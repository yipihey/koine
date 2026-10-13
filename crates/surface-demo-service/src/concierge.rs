//! The in-process concierge agent (docs/plan-koine-tour.md T-D3).
//!
//! An agent in the loop, on the phone, with no model: it waits on its own
//! surface (`surface_wait`), reads the question the person typed, answers
//! by a few rules — a plot, the notes, the map, a job, help — appends the
//! exchange to a transcript in the surface's state, and creates and shows
//! the surface its answer names. Rules, not a model: the point is the loop
//! (`surface_wait` → react → `surface_create` → `surface_show`), and the
//! phone has no model client (plan-koine-mobile M-D7).
//!
//! # Only the public verbs
//!
//! Everything the concierge does is a verb call through the invoker
//! pipeline ([`impress_service_core::pipeline::invoke`]), by the verb's MCP
//! tool name, with the same JSON an agent on a Mac would `POST` to
//! `/api/verb/<name>`. It never reaches into the surface runtime, the store
//! or the layout: a transcript of its calls is one any agent could replay
//! over HTTP. The instance it waits on and writes is the default
//! `host`/`device` (this device's) unless [`ConciergeConfig::host`] says
//! otherwise — the instance the host's own HTTP routes fill in, so the
//! page and the concierge see one state. A state write by the concierge
//! wakes the page's `surface_wait` (T-D4), which is how the transcript
//! appears without the page waiting out its timeout.
//!
//! # Running it
//!
//! [`spawn`] puts the loop on the tokio runtime the caller is inside;
//! [`spawn_on`] takes a runtime handle (what the `on_bound` hook of
//! `koine_host::cli::main_with` is given); [`spawn_thread`] gives the loop a
//! thread and a current-thread runtime of its own, for a program whose main
//! thread belongs to a window toolkit and which holds no runtime (the mobile
//! demo's `main`, whose host runs on `koine_host::embed`'s thread). Each
//! returns a [`ConciergeHandle`] whose [`ConciergeHandle::stop`] ends the
//! loop at its next wait. Tests drive one round at a time with
//! [`Concierge::run_once`].
//!
//! The surface itself is seeded by the tour (`crate::tour`, T-D1) from
//! [`SPEC`] under the row name [`NAME`]; the concierge finds it by that name
//! and keeps looking, one wait period at a time, until it is there.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use impress_service_core::pipeline::{self, Call};
use impress_service_core::McpToolDescriptor;
use serde_json::{json, Value};

/// The row name the tour seeds the concierge surface under, and what
/// [`Concierge::attach`] looks for (case-insensitively) in `surface_list`.
pub const NAME: &str = "Concierge";

/// The concierge surface, as the tour creates it (validated by Tier A).
pub const SPEC: &str = include_str!("../surfaces/concierge.surface.json");

/// The caller identity every call runs as.
const CALLER: &str = "concierge";

/// The surface verbs, by MCP tool name (`impress-surface-service`'s own
/// inventory test lists all fifteen).
const SURFACE_LIST: &str = "impress-surface-service_surface-list";
const SURFACE_WAIT: &str = "impress-surface-service_surface-wait";
const SURFACE_STATE_GET: &str = "impress-surface-service_surface-state-get";
const SURFACE_STATE_SET: &str = "impress-surface-service_surface-state-set";
const SURFACE_VALIDATE: &str = "impress-surface-service_surface-validate";
const SURFACE_CREATE: &str = "impress-surface-service_surface-create";
const SURFACE_SHOW: &str = "impress-surface-service_surface-show";

/// This crate's own verbs the answers call.
const DEMO_SERIES: &str = "surface-demo-service_series";
const DEMO_TICK: &str = "surface-demo-service_tick";

/// Every verb the concierge calls — a test checks each is in the inventory
/// of a binary that links the surface service beside this crate.
pub const VERBS: [&str; 9] = [
    SURFACE_LIST,
    SURFACE_WAIT,
    SURFACE_STATE_GET,
    SURFACE_STATE_SET,
    SURFACE_VALIDATE,
    SURFACE_CREATE,
    SURFACE_SHOW,
    DEMO_SERIES,
    DEMO_TICK,
];

/// How long one `surface_wait` holds: a few seconds, so a stop is noticed
/// soon and a page's own wait (25 s) is never the shorter one.
pub const WAIT_MS: u64 = 3_000;

/// The sine wave's frequency when the question names no number.
const DEFAULT_FREQ: f64 = 3.0;

/// Samples in the plot the concierge makes.
const PLOT_SAMPLES: u32 = 128;

/// The job's shape: `n` values, `every_ms` apart.
const JOB_TICKS: u32 = 20;
const JOB_EVERY_MS: u64 = 500;

/// The most transcript lines kept in state (the oldest go first).
const TRANSCRIPT_LINES: usize = 40;

/// What the concierge needs to know about the host it runs in.
#[derive(Clone, Debug)]
pub struct ConciergeConfig {
    /// The app whose window `surface_show` puts an answer in
    /// (`koine_host::Config::app_id`).
    pub app_id: String,
    /// The `(surface, host)` instance to wait on and write, and the
    /// `device` whose layout to show into. `None`: this device's, which is
    /// what the host's own routes use.
    pub host: Option<String>,
    /// `surface_show`'s `target` for an answer: by default a new pane below
    /// the focused one (`{"split": {"direction": "vertical"}}`), the same
    /// thing an `open` action with no target means.
    pub target: Value,
    /// One `surface_wait`'s timeout.
    pub wait_ms: u64,
}

impl ConciergeConfig {
    pub fn new(app_id: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            host: None,
            target: json!({ "split": { "direction": "vertical" } }),
            wait_ms: WAIT_MS,
        }
    }

    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = Some(host.into());
        self
    }

    pub fn with_target(mut self, target: Value) -> Self {
        self.target = target;
        self
    }

    pub fn with_wait_ms(mut self, wait_ms: u64) -> Self {
        self.wait_ms = wait_ms;
        self
    }
}

impl Default for ConciergeConfig {
    /// The demo app's id (`koine-demo`, what `koine_host::Config::new`
    /// defaults to).
    fn default() -> Self {
        Self::new("koine-demo")
    }
}

/// What one round ([`Concierge::run_once`]) did.
#[derive(Clone, Debug, PartialEq)]
pub enum Round {
    /// The wait timed out: nobody asked.
    Quiet,
    /// The wait returned early because the surface's state or a record it
    /// reads changed (its own writes do this too); nothing to answer.
    Invalidated,
    /// Events arrived, none of them an `ask`.
    Ignored(usize),
    /// A question was answered.
    Answered(Answer),
    /// A verb refused: the message. The loop goes on after one wait period.
    Refused(String),
}

/// An answered question.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub question: String,
    /// What the concierge said, as the transcript shows it.
    pub reply: String,
    /// The id of the surface the answer created, if it made one.
    pub created: Option<String>,
    /// The id of the surface the answer showed, if it showed one.
    pub shown: Option<String>,
}

/// One concierge over one surface instance.
pub struct Concierge {
    config: ConciergeConfig,
    surface_id: String,
    /// The event cursor: the `next_seq` of the last wait.
    after_seq: u64,
}

impl Concierge {
    /// A concierge over the surface `id` — the one the tour seeded, or one
    /// a test just created from [`SPEC`]. Its cursor starts at 0, so a
    /// question asked before the concierge came up is still answered.
    pub fn for_surface(config: ConciergeConfig, id: impl Into<String>) -> Self {
        Self {
            config,
            surface_id: id.into(),
            after_seq: 0,
        }
    }

    /// Find the concierge surface by [`NAME`] (`surface_list`, the oldest
    /// row of that name) and attach to it. `None` when the tour has not
    /// seeded it (yet), or the list refused.
    pub async fn attach(config: ConciergeConfig) -> Option<Self> {
        let id = find_surface(&[NAME]).await?;
        Some(Self::for_surface(config, id))
    }

    pub fn surface_id(&self) -> &str {
        &self.surface_id
    }

    pub fn config(&self) -> &ConciergeConfig {
        &self.config
    }

    /// One wait on the surface and, if an `ask` arrived, its answer. Every
    /// step is a verb call; see the module docs.
    pub async fn run_once(&mut self) -> Round {
        let mut args = json!({
            "id": self.surface_id,
            "after_seq": self.after_seq,
            "timeout_ms": self.config.wait_ms,
        });
        self.host_arg(&mut args, "host");
        let waited = match call(SURFACE_WAIT, args).await {
            Ok(waited) => waited,
            Err(message) => {
                tracing::warn!(target: "concierge", "wait refused: {message}");
                return Round::Refused(message);
            }
        };
        if let Some(seq) = waited.get("next_seq").and_then(Value::as_u64) {
            self.after_seq = seq;
        }
        let events = waited
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if events.is_empty() {
            return if waited
                .get("invalidated")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                Round::Invalidated
            } else {
                Round::Quiet
            };
        }
        // The last `ask` wins: one answer per round, for the newest question.
        let Some(ask) = events
            .iter()
            .rev()
            .find(|e| e.get("name").and_then(Value::as_str) == Some("ask"))
        else {
            return Round::Ignored(events.len());
        };
        let question = match ask
            .get("payload")
            .and_then(|p| p.get("question"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|q| !q.is_empty())
        {
            Some(question) => question.to_string(),
            None => self.question_from_state().await,
        };
        let seq = ask.get("seq").and_then(Value::as_u64).unwrap_or(0);
        tracing::info!(target: "concierge", "asked: {question:?} (event seq {seq})");
        match self.answer(&question).await {
            Ok(answer) => {
                tracing::info!(
                    target: "concierge",
                    "answered {:?}: {} (created {:?}, shown {:?})",
                    answer.question,
                    answer.reply,
                    answer.created,
                    answer.shown
                );
                Round::Answered(answer)
            }
            Err(message) => {
                tracing::warn!(target: "concierge", "answering {question:?} refused: {message}");
                Round::Refused(message)
            }
        }
    }

    /// Wait and answer until `stop` is set. A surface that is not there
    /// any more (deleted, re-seeded) is looked for again by name after one
    /// wait period.
    pub async fn run(mut self, stop: Arc<AtomicBool>) {
        tracing::info!(
            target: "concierge",
            "running over surface {} for app {}",
            self.surface_id,
            self.config.app_id
        );
        while !stop.load(Ordering::SeqCst) {
            match self.run_once().await {
                Round::Refused(message) if message.contains("no surface") => {
                    tokio::time::sleep(Duration::from_millis(self.config.wait_ms)).await;
                    if let Some(id) = find_surface(&[NAME]).await {
                        if id != self.surface_id {
                            tracing::info!(target: "concierge", "surface is now {id}");
                            self.surface_id = id;
                            self.after_seq = 0;
                        }
                    }
                }
                Round::Refused(_) => {
                    tokio::time::sleep(Duration::from_millis(self.config.wait_ms)).await;
                }
                _ => {}
            }
        }
        tracing::info!(target: "concierge", "stopped");
    }

    fn host_arg(&self, args: &mut Value, key: &str) {
        if let (Some(host), Some(map)) = (&self.config.host, args.as_object_mut()) {
            map.insert(key.to_string(), Value::String(host.clone()));
        }
    }

    async fn state(&self) -> Value {
        let mut args = json!({ "id": self.surface_id });
        self.host_arg(&mut args, "host");
        call(SURFACE_STATE_GET, args)
            .await
            .ok()
            .and_then(|r| r.get("state").cloned())
            .unwrap_or_else(|| json!({}))
    }

    async fn question_from_state(&self) -> String {
        self.state()
            .await
            .get("question")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    /// Write `state` for the surface — the whole object, since
    /// `surface_state_set` overwrites — which wakes the page showing it.
    async fn set_state(&self, state: Value) -> Result<(), String> {
        let mut args = json!({ "id": self.surface_id, "state": state });
        self.host_arg(&mut args, "host");
        call(SURFACE_STATE_SET, args).await.map(|_| ())
    }

    async fn answer(&self, question: &str) -> Result<Answer, String> {
        // Say so first: the page wakes and shows "thinking" while the
        // answer is made (a verb call, a layout write).
        let mut state = self.state().await;
        set_field(
            &mut state,
            "status",
            json!(format!("thinking about: {question}")),
        );
        self.set_state(state).await?;

        let (reply, created, shown, status) = match self.react(question).await {
            Ok(reaction) => (
                reaction.reply,
                reaction.created,
                reaction.shown,
                reaction.status,
            ),
            Err(message) => (
                format!("that did not work: {message}"),
                None,
                None,
                "refused".to_string(),
            ),
        };

        // Re-read: the transcript is appended to, never replaced.
        let mut state = self.state().await;
        let mut transcript = state
            .get("transcript")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        transcript.push(json!(format!("you: {question}")));
        transcript.push(json!(format!("concierge: {reply}")));
        if transcript.len() > TRANSCRIPT_LINES {
            let excess = transcript.len() - TRANSCRIPT_LINES;
            transcript.drain(..excess);
        }
        set_field(&mut state, "transcript", Value::Array(transcript));
        set_field(&mut state, "status", json!(status));
        set_field(&mut state, "question", json!(""));
        self.set_state(state).await?;

        Ok(Answer {
            question: question.to_string(),
            reply,
            created,
            shown,
        })
    }

    /// The rules. Case-insensitive substrings, first match wins; anything
    /// else is help.
    async fn react(&self, question: &str) -> Result<Reaction, String> {
        let lower = question.to_ascii_lowercase();
        let has = |words: &[&str]| words.iter().any(|w| lower.contains(w));
        if has(&["plot", "sine", "wave"]) {
            self.plot(question).await
        } else if has(&["note", "tree", "outline"]) {
            self.show_named(&["notes", "note"], "the notes").await
        } else if has(&["map", "graph", "verbs"]) {
            self.show_named(&["map"], "the map of this app").await
        } else if has(&["job", "tick", "stream", "live"]) {
            self.job().await
        } else {
            Ok(Reaction::said(help_text()))
        }
    }

    /// "plot" / "sine" / "wave": call `series` (the frequency is the first
    /// number in the question, else 3), make a surface "Your plot" with the
    /// wave bound to a `plot` (the bound form: rows of `{x, y}`), and show
    /// it.
    async fn plot(&self, question: &str) -> Result<Reaction, String> {
        let freq = first_number(question)
            .filter(|f| f.is_finite() && *f > 0.0)
            .unwrap_or(DEFAULT_FREQ);
        let series = call(DEMO_SERIES, json!({ "freq": freq, "n": PLOT_SAMPLES })).await?;
        if let Some(error) = series.get("error") {
            return Err(format!("series refused: {error}"));
        }
        let xs = series.get("x").and_then(Value::as_array).cloned();
        let ys = series.get("values").and_then(Value::as_array).cloned();
        let (Some(xs), Some(ys)) = (xs, ys) else {
            return Err("series answered without x/values".to_string());
        };
        let rows: Vec<Value> = xs
            .iter()
            .zip(ys.iter())
            .map(|(x, y)| json!({ "x": x, "y": y }))
            .collect();
        let title = "Your plot";
        let spec = json!({
            "surface": "1.0",
            "name": title,
            "state": {},
            "sources": { "wave": { "value": rows } },
            "root": { "column": [
                { "id": "caption",
                  "text": format!(
                      "A sine wave at {freq} Hz: {PLOT_SAMPLES} samples from \
                       `{DEMO_SERIES}`, asked for by \"{question}\"."
                  ) },
                { "id": "wave-plot",
                  "plot": { "rows": "{{source.wave}}", "x": "x", "y": "y" } }
            ] }
        });
        let id = self.create(spec, title).await?;
        let shown = self.show(&id).await;
        Ok(Reaction {
            reply: match &shown {
                Ok(()) => format!("here is a sine wave at {freq} Hz: a new surface, listed on Home and in a pane on Layout"),
                Err(message) => {
                    format!("I made \"{title}\" (surface {id}) but could not show it: {message}")
                }
            },
            created: Some(id.clone()),
            shown: shown.is_ok().then_some(id),
            status: "answered with a plot".to_string(),
        })
    }

    /// "note" / "map": show a seeded surface, found by name or tag.
    async fn show_named(&self, keys: &[&str], what: &str) -> Result<Reaction, String> {
        let Some(id) = find_surface(keys).await else {
            return Ok(Reaction::said(format!(
                "{what} is not seeded on this host, so there is nothing to show"
            )));
        };
        Ok(match self.show(&id).await {
            Ok(()) => Reaction {
                reply: format!("showing {what}"),
                created: None,
                shown: Some(id),
                status: format!("showed {what}"),
            },
            Err(message) => {
                Reaction::said(format!("I found {what} but could not show it: {message}"))
            }
        })
    }

    /// "job": start `tick` in the background — the adverb rides beside the
    /// arguments under the `adverbs` key, exactly as an HTTP caller sends
    /// it — then show the live-job surface.
    async fn job(&self) -> Result<Reaction, String> {
        let started = call(
            DEMO_TICK,
            json!({
                "n": JOB_TICKS,
                "every_ms": JOB_EVERY_MS,
                "adverbs": { "in_background": true },
            }),
        )
        .await;
        let job = match started {
            Ok(answer) => answer
                .get("job")
                .and_then(|j| j.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string),
            Err(message) => {
                return Ok(Reaction::said(format!(
                    "I could not start a job here: {message}"
                )))
            }
        };
        let job_text = match &job {
            Some(id) => {
                format!("started job {id} ({JOB_TICKS} ticks, one every {JOB_EVERY_MS} ms)")
            }
            None => "started a job".to_string(),
        };
        let Some(id) = find_surface(&["live-job", "live job"]).await else {
            return Ok(Reaction {
                reply: format!(
                    "{job_text}; the live-job surface is not seeded, so watch it with the job verbs"
                ),
                created: None,
                shown: None,
                status: "started a job".to_string(),
            });
        };
        Ok(match self.show(&id).await {
            Ok(()) => Reaction {
                reply: format!("{job_text}; showing the live job"),
                created: None,
                shown: Some(id),
                status: "started and showed a job".to_string(),
            },
            Err(message) => Reaction {
                reply: format!("{job_text}, but could not show the live job: {message}"),
                created: None,
                shown: None,
                status: "started a job".to_string(),
            },
        })
    }

    /// `surface_validate`, then `surface_create` — the loop's steps 3 and 4,
    /// never create alone. The row is tagged [`NAME`] (never the tour's own
    /// tag: the tour's seeding counts its surfaces by that). Answers the new
    /// surface's id.
    async fn create(&self, spec: Value, name: &str) -> Result<String, String> {
        let validated = call(SURFACE_VALIDATE, json!({ "spec": spec })).await?;
        if validated.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(format!(
                "the spec does not validate: {}",
                validated.get("problems").cloned().unwrap_or(Value::Null)
            ));
        }
        let created = call(
            SURFACE_CREATE,
            json!({ "spec": spec, "name": name, "tags": [NAME] }),
        )
        .await?;
        created
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| "surface_create answered without an id".to_string())
    }

    /// `surface_show` into the configured target of the app's window.
    async fn show(&self, id: &str) -> Result<(), String> {
        let mut args = json!({
            "id": id,
            "target": self.config.target,
            "app_id": self.config.app_id,
        });
        self.host_arg(&mut args, "device");
        call(SURFACE_SHOW, args).await.map(|_| ())
    }
}

/// What a rule decided.
struct Reaction {
    reply: String,
    created: Option<String>,
    shown: Option<String>,
    status: String,
}

impl Reaction {
    fn said(reply: String) -> Self {
        Self {
            reply,
            created: None,
            shown: None,
            status: "answered".to_string(),
        }
    }
}

fn help_text() -> String {
    "I can plot a sine wave (\"plot a sine wave at 5 Hz\"), show the notes outline (\"notes\"), \
     show the map of this app (\"map\"), or start a live job (\"job\"). Ask in words."
        .to_string()
}

/// The first number in `text` (`"5 Hz"` → 5, `"2.5"` → 2.5).
fn first_number(text: &str) -> Option<f64> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            if let Ok(n) = text[start..i].trim_end_matches('.').parse::<f64>() {
                return Some(n);
            }
        } else {
            i += 1;
        }
    }
    None
}

fn set_field(state: &mut Value, key: &str, value: Value) {
    if !state.is_object() {
        *state = json!({});
    }
    if let Some(map) = state.as_object_mut() {
        map.insert(key.to_string(), value);
    }
}

/// The oldest surface whose row name is one of `keys` (case-insensitively),
/// else the oldest carrying one as a tag, else the oldest whose name
/// contains one — via `surface_list`.
pub async fn find_surface(keys: &[&str]) -> Option<String> {
    let listed = call(SURFACE_LIST, json!({})).await.ok()?;
    let rows = listed.get("surfaces")?.as_array()?;
    let keys: Vec<String> = keys.iter().map(|k| k.to_ascii_lowercase()).collect();
    let name_of = |row: &Value| {
        row.get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase()
    };
    let tags_of = |row: &Value| -> Vec<String> {
        row.get("tags")
            .and_then(Value::as_array)
            .map(|tags| {
                tags.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_ascii_lowercase)
                    .collect()
            })
            .unwrap_or_default()
    };
    let id_of = |row: &Value| row.get("id").and_then(Value::as_str).map(str::to_string);
    rows.iter()
        .find(|row| keys.contains(&name_of(row)))
        .or_else(|| {
            rows.iter()
                .find(|row| tags_of(row).iter().any(|t| keys.contains(t)))
        })
        .or_else(|| {
            rows.iter()
                .find(|row| keys.iter().any(|k| name_of(row).contains(k.as_str())))
        })
        .and_then(id_of)
}

/// Run one verb by its tool name through the pipeline, as the concierge. A
/// refusal — the pipeline's (`Err`) or the verb's own `{"ok": false,
/// "code", "message"}` envelope — is the `Err` message. A verb whose answer
/// has no `ok` field (this crate's `series`) is `Ok` as answered.
pub async fn call(name: &str, args: Value) -> Result<Value, String> {
    let tool = McpToolDescriptor::iter()
        .find(|t| t.name == name)
        .ok_or_else(|| format!("no such verb in this process: {name}"))?;
    let answer = pipeline::invoke(tool.verb, Call::agent(CALLER, args))
        .await
        .map_err(|e| format!("{name}: {e}"))?;
    if answer.get("ok").and_then(Value::as_bool) == Some(false) {
        let code = answer
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("refused");
        let message = answer.get("message").and_then(Value::as_str).unwrap_or("");
        return Err(format!("{name}: {code}: {message}"));
    }
    Ok(answer)
}

/// A running concierge: [`stop`](Self::stop) ends its loop at the next wait.
/// Dropping the handle stops the loop too (without waiting for it).
pub struct ConciergeHandle {
    stop: Arc<AtomicBool>,
    task: Option<tokio::task::JoinHandle<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ConciergeHandle {
    /// Ask the loop to end; it does at its next wait (within
    /// [`ConciergeConfig::wait_ms`]).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Stop now: a task is aborted, a thread is joined (its loop ends at
    /// its next wait, so this blocks up to [`ConciergeConfig::wait_ms`]).
    pub fn shutdown(mut self) {
        self.stop();
        if let Some(task) = self.task.take() {
            task.abort();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ConciergeHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Spawn the loop on the tokio runtime the caller is inside (it panics
/// outside one, as `tokio::spawn` does). The loop attaches to the surface
/// named [`NAME`] and keeps looking until it is seeded.
pub fn spawn(config: ConciergeConfig) -> ConciergeHandle {
    spawn_on(&tokio::runtime::Handle::current(), config)
}

/// [`spawn`] on an explicit runtime handle — what the `on_bound` hook of
/// `koine_host::cli::main_with` is given.
pub fn spawn_on(runtime: &tokio::runtime::Handle, config: ConciergeConfig) -> ConciergeHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let task = runtime.spawn(run(config, stop.clone()));
    ConciergeHandle {
        stop,
        task: Some(task),
        thread: None,
    }
}

/// The loop on a thread of its own, with a current-thread tokio runtime of
/// its own — for a program that holds no runtime, such as the mobile demo's
/// `main`, whose host runs on `koine_host::embed`'s thread.
pub fn spawn_thread(config: ConciergeConfig) -> ConciergeHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let thread = std::thread::Builder::new()
        .name("concierge".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    tracing::error!(target: "concierge", "cannot start a runtime: {error}");
                    return;
                }
            };
            runtime.block_on(run(config, flag));
        });
    let thread = match thread {
        Ok(thread) => Some(thread),
        Err(error) => {
            tracing::error!(target: "concierge", "cannot start the concierge thread: {error}");
            None
        }
    };
    ConciergeHandle {
        stop,
        task: None,
        thread,
    }
}

/// Attach (looking again every wait period until the surface is seeded or
/// `stop` is set), then [`Concierge::run`].
pub async fn run(config: ConciergeConfig, stop: Arc<AtomicBool>) {
    let mut announced = false;
    while !stop.load(Ordering::SeqCst) {
        if let Some(concierge) = Concierge::attach(config.clone()).await {
            concierge.run(stop).await;
            return;
        }
        if !announced {
            tracing::info!(
                target: "concierge",
                "no surface named {NAME:?} yet; looking again every {} ms",
                config.wait_ms
            );
            announced = true;
        }
        tokio::time::sleep(Duration::from_millis(config.wait_ms)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spec_parses_and_validates() {
        let spec: impress_surface::SurfaceSpec =
            serde_json::from_str(SPEC).expect("concierge.surface.json parses");
        assert_eq!(spec.name, "Concierge");
        let problems = impress_surface::validate(&spec);
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn the_first_number_is_the_frequency() {
        assert_eq!(first_number("a sine wave at 5 Hz"), Some(5.0));
        assert_eq!(first_number("plot 2.5hz please"), Some(2.5));
        assert_eq!(first_number("wave"), None);
        assert_eq!(first_number("3."), Some(3.0));
    }
}
