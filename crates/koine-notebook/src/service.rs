//! `notebook-service`: the lab notebook's verbs (plan-koine-notebook N-D7).

use std::collections::{BTreeMap, BTreeSet};

use impress_item::ItemId;
use impress_service_core::async_trait;
use impress_service_core::pipeline::{self, context, Call, CallerIdentity};
use impress_service_core::VerbDescriptor;
#[allow(unused_imports)]
use impress_service_macros::impress_method;
use impress_service_macros::{impress_service, impress_service_impl};
use serde_json::{json, Map, Value};

use crate::analysis;
use crate::dto::*;
use crate::records::{self, text};
use crate::runs::{self, Run};

/// Rows `timeline` answers when no `limit` is given, and at most.
pub const DEFAULT_LIMIT: u32 = 100;
pub const MAX_LIMIT: u32 = 1000;

#[impress_service]
pub trait NotebookService: Send + Sync + 'static {
    /// Every run, newest first, with how many call records name each, and
    /// which one is open. A run groups the calls made while it was open.
    #[impress_method(effects(reads = [any("koine/run@1.0.0"), "core/verb-call@1.0.0"]))]
    #[impress_example(name = "default", args = r#"{}"#, expect = r#"{"ok": true}"#)]
    async fn runs(&self) -> RunsResult;

    /// Open a run named `name`, closing any open one: every call recorded
    /// until `run-stop` names it. With `record_reads`, read-only calls are
    /// recorded too while it is open. Refuses an empty name.
    #[impress_method(
        safety = mutating,
        effects(reads = [any("koine/run@1.0.0")], writes = [any("koine/run@1.0.0")])
    )]
    #[impress_example(
        name = "default",
        args = r#"{"name": "Fit sweep, 40 bins", "record_reads": true}"#
    )]
    async fn run_start(&self, name: String, record_reads: Option<bool>) -> RunResult;

    /// Close the open run. Answers the run closed, or no run when none was
    /// open (not a refusal).
    #[impress_method(
        safety = mutating,
        effects(reads = [any("koine/run@1.0.0")], writes = [any("koine/run@1.0.0")])
    )]
    #[impress_example(name = "default", args = r#"{}"#, expect = r#"{"ok": true}"#)]
    async fn run_stop(&self) -> RunResult;

    /// The call records, newest first: time, verb, caller, outcome, total and
    /// self time, steps, writes and reads. Only top-level calls unless
    /// `nested`; narrowed by `run`, `caller` (`human`, `agent`, `agent:mcp`),
    /// a `verb` substring and `failed_only`. The notebook's own read verbs are
    /// left out unless `verb` names them. `limit` defaults to 100, at most 1000.
    #[impress_method(effects(reads = ["core/verb-call@1.0.0"]))]
    #[impress_example(name = "default", args = r#"{}"#, expect = r#"{"ok": true}"#)]
    #[impress_example(
        name = "failed",
        args = r#"{"failed_only": true, "limit": 20}"#,
        expect = r#"{"ok": true}"#
    )]
    #[allow(clippy::too_many_arguments)]
    async fn timeline(
        &self,
        run: Option<String>,
        caller: Option<String>,
        verb: Option<String>,
        failed_only: Option<bool>,
        nested: Option<bool>,
        limit: Option<u32>,
    ) -> TimelineResult;

    /// One call record in full: a summary, the stored record, every step under
    /// it with total and self time, the slowest steps, folded stacks, a bar
    /// chart of self time per step, its notes, and whether it can be
    /// replayed. With no `call_id` (nothing selected yet) it answers an
    /// empty entry, not a refusal. Refuses an unknown call (`not-found`).
    #[impress_method(effects(reads = ["core/verb-call@1.0.0", any("koine/notebook-note@1.0.0")]))]
    #[impress_example(
        name = "unknown",
        args = r#"{"call_id": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    #[impress_example(name = "nothing-selected", args = r#"{}"#, expect = r#"{"ok": true}"#)]
    async fn entry(&self, call_id: Option<String>) -> EntryResult;

    /// Attach a note to a call record. Refuses an empty text and an unknown
    /// call.
    #[impress_method(
        safety = mutating,
        effects(reads = ["core/verb-call@1.0.0"], writes = [any("koine/notebook-note@1.0.0")])
    )]
    #[impress_example(
        name = "unknown",
        args = r#"{"call_id": "00000000-0000-4000-8000-000000000000", "text": "Bins too coarse."}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn annotate(&self, call_id: String, text: String) -> AnnotateResult;

    /// The timings of every step of `verb`'s calls in run `run_a` against run
    /// `run_b` (either absent: every run): count, mean, p50, p95 of total
    /// time, mean self time, and the p50 ratio. Which step got slower.
    #[impress_method(effects(reads = ["core/verb-call@1.0.0"]))]
    #[impress_example(
        name = "default",
        args = r#"{"verb": "surface-demo-service_note-add"}"#,
        expect = r#"{"ok": true}"#
    )]
    async fn compare(
        &self,
        verb: String,
        run_a: Option<String>,
        run_b: Option<String>,
    ) -> CompareResult;

    /// Run a recorded call again with its recorded arguments, as the caller
    /// of `replay`; policy decides again. Refuses a call whose arguments were
    /// not kept whole (`not-replayable`), an unknown call and a verb no longer
    /// linked.
    #[impress_method(
        safety = destructive,
        effects(reads = ["core/verb-call@1.0.0"], writes = [any("whatever the replayed verb writes")])
    )]
    #[impress_example(
        name = "unknown",
        args = r#"{"call_id": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn replay(&self, call_id: String) -> ReplayResult;

    /// A run as Markdown, oldest call first: when, who, which verb, the
    /// outcome, total and self time, steps and writes; then each call's step
    /// table and notes; with the host and build. For a methods section or a
    /// collaborator. Refuses an unknown run.
    #[impress_method(effects(reads = [any("koine/run@1.0.0"), "core/verb-call@1.0.0"]))]
    #[impress_example(
        name = "unknown",
        args = r#"{"run": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn export(&self, run: String) -> ExportResult;

    /// Delete the oldest call records that belong to no run beyond the
    /// newest `keep`, with their notes. A record in a run is never pruned.
    #[impress_method(
        safety = destructive,
        effects(writes = ["core/verb-call@1.0.0", any("koine/notebook-note@1.0.0")])
    )]
    #[impress_example(
        name = "default",
        args = r#"{"keep": 5000}"#,
        expect = r#"{"ok": true}"#
    )]
    async fn prune(&self, keep: u64) -> PruneResult;
}

#[derive(Clone, Default)]
pub struct DefaultNotebookService;

impl DefaultNotebookService {
    pub fn new() -> Self {
        Self
    }
}

fn caller() -> CallerIdentity {
    context::current()
        .map(|c| c.caller.clone())
        .unwrap_or(CallerIdentity::Person)
}

fn run_row(run: &Run, calls: u64) -> RunRow {
    RunRow {
        id: run.id.to_string(),
        name: run.name.clone(),
        status: run.status.clone(),
        started_at: run.started_at.clone(),
        ended_at: run.ended_at.clone(),
        record_reads: run.record_reads,
        host: run.host.clone(),
        calls,
    }
}

/// Every call record's payload, newest first, narrowed to a run.
fn call_records(
    store: &dyn impress_item::AttributedStore,
    run: Option<&str>,
    newest_first: bool,
) -> Result<Vec<Map<String, Value>>, String> {
    let equal: Vec<(&str, &str)> = run.map(|r| ("run", r)).into_iter().collect();
    Ok(
        records::all_of(store, records::VERB_CALL, &equal, newest_first)?
            .iter()
            .map(records::payload_json)
            .collect(),
    )
}

fn find_record(
    store: &dyn impress_item::AttributedStore,
    call_id: &str,
) -> Result<Option<impress_item::Item>, String> {
    let item = store
        .get(records::record_id(call_id.trim()))
        .map_err(|e| e.to_string())?;
    Ok(item.filter(|item| item.schema.as_str() == records::VERB_CALL))
}

fn notes_of(
    store: &dyn impress_item::AttributedStore,
    record: ItemId,
) -> Result<Vec<NoteRow>, String> {
    Ok(records::all_of(store, records::NOTE, &[], false)?
        .iter()
        .filter(|n| n.parent == Some(record))
        .map(note_row)
        .collect())
}

fn note_row(item: &impress_item::Item) -> NoteRow {
    let p = records::payload_json(item);
    NoteRow {
        id: item.id.to_string(),
        text: text(&p, "text").unwrap_or("").to_string(),
        author: item.author.clone(),
        at: item.created.to_rfc3339(),
    }
}

/// The notebook's own read verbs: left out of the timeline and the export
/// unless asked for, as the sink leaves out their successful calls.
fn is_plumbing(verb: &str) -> bool {
    verb.starts_with("notebook-service_")
        && VerbDescriptor::find(verb)
            .is_some_and(|v| v.safety.class == impress_service_core::SafetyClass::ReadOnly)
}

/// Whether a timeline `caller` filter admits a row's caller label.
fn caller_matches(label: &str, filter: &str) -> bool {
    let filter = filter.trim();
    filter.is_empty()
        || label == filter
        || label.split(':').next() == Some(filter)
        || (filter == "person" && label == "human")
}

macro_rules! refuse {
    ($ty:ident, $code:expr, $message:expr) => {
        $ty {
            ok: false,
            code: Some(String::from($code)),
            message: Some($message.to_string()),
            ..Default::default()
        }
    };
}

impl Default for RunsResult {
    fn default() -> Self {
        Self {
            ok: true,
            runs: Vec::new(),
            open: None,
            code: None,
            message: None,
        }
    }
}
impl Default for RunResult {
    fn default() -> Self {
        Self {
            ok: true,
            run: None,
            code: None,
            message: None,
        }
    }
}
impl Default for TimelineResult {
    fn default() -> Self {
        Self {
            ok: true,
            rows: Vec::new(),
            total: 0,
            code: None,
            message: None,
        }
    }
}
impl Default for EntryResult {
    fn default() -> Self {
        Self {
            ok: true,
            call_id: None,
            summary: BTreeMap::new(),
            record: Value::Null,
            steps: Vec::new(),
            slowest: Vec::new(),
            folded: Vec::new(),
            plot: Value::Null,
            notes: Vec::new(),
            replayable: false,
            code: None,
            message: None,
        }
    }
}
impl Default for AnnotateResult {
    fn default() -> Self {
        Self {
            ok: true,
            note: None,
            code: None,
            message: None,
        }
    }
}
impl Default for CompareResult {
    fn default() -> Self {
        Self {
            ok: true,
            verb: String::new(),
            a: String::new(),
            b: String::new(),
            rows: Vec::new(),
            code: None,
            message: None,
        }
    }
}
impl Default for ReplayResult {
    fn default() -> Self {
        Self {
            ok: true,
            replay_of: None,
            call_id: None,
            answer: Value::Null,
            code: None,
            message: None,
        }
    }
}
impl Default for ExportResult {
    fn default() -> Self {
        Self {
            ok: true,
            markdown: String::new(),
            code: None,
            message: None,
        }
    }
}
impl Default for PruneResult {
    fn default() -> Self {
        Self {
            ok: true,
            deleted: 0,
            code: None,
            message: None,
        }
    }
}

#[async_trait::async_trait]
impl NotebookService for DefaultNotebookService {
    async fn runs(&self) -> RunsResult {
        crate::flush();
        let store = match records::store(false) {
            Ok(store) => store,
            Err((code, message)) => return refuse!(RunsResult, code, message),
        };
        let all = match runs::list(store.as_ref()) {
            Ok(all) => all,
            Err(e) => return refuse!(RunsResult, "store-error", e),
        };
        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        if let Ok(calls) = call_records(store.as_ref(), None, true) {
            for payload in &calls {
                if let Some(run) = crate::sink::run_of(payload) {
                    *counts.entry(run.to_string()).or_insert(0) += 1;
                }
            }
        }
        RunsResult {
            open: all.iter().find(|r| r.is_open()).map(|r| r.id.to_string()),
            runs: all
                .iter()
                .map(|r| run_row(r, counts.get(&r.id.to_string()).copied().unwrap_or(0)))
                .collect(),
            ..Default::default()
        }
    }

    async fn run_start(&self, name: String, record_reads: Option<bool>) -> RunResult {
        let name = name.trim();
        if name.is_empty() {
            return refuse!(RunResult, "invalid-argument", "a run needs a name");
        }
        let store = match records::store(true) {
            Ok(store) => store,
            Err((code, message)) => return refuse!(RunResult, code, message),
        };
        let who = caller();
        let (host, build) = crate::stamp();
        match runs::start(
            &store,
            name,
            record_reads.unwrap_or(false),
            who.author(),
            records::actor_kind(&who),
            &host,
            build,
        ) {
            Ok(run) => RunResult {
                run: Some(run_row(&run, 0)),
                ..Default::default()
            },
            Err(e) => refuse!(RunResult, "store-error", e),
        }
    }

    async fn run_stop(&self) -> RunResult {
        crate::flush();
        let store = match records::store(true) {
            Ok(store) => store,
            Err((code, message)) => return refuse!(RunResult, code, message),
        };
        match runs::stop(store.as_ref()) {
            Ok(closed) => match closed.into_iter().next() {
                Some(run) => RunResult {
                    run: Some(run_row(&run, 0)),
                    ..Default::default()
                },
                None => RunResult {
                    message: Some("no run was open".into()),
                    ..Default::default()
                },
            },
            Err(e) => refuse!(RunResult, "store-error", e),
        }
    }

    async fn timeline(
        &self,
        run: Option<String>,
        caller: Option<String>,
        verb: Option<String>,
        failed_only: Option<bool>,
        nested: Option<bool>,
        limit: Option<u32>,
    ) -> TimelineResult {
        crate::flush();
        let store = match records::store(false) {
            Ok(store) => store,
            Err((code, message)) => return refuse!(TimelineResult, code, message),
        };
        let run = run.filter(|r| !r.trim().is_empty());
        let all = match call_records(store.as_ref(), run.as_deref(), true) {
            Ok(all) => all,
            Err(e) => return refuse!(TimelineResult, "store-error", e),
        };
        let logged: BTreeSet<String> = all
            .iter()
            .filter_map(|p| text(p, "_id").map(str::to_string))
            .collect();
        let verb = verb.filter(|v| !v.trim().is_empty());
        let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as usize;
        let rows: Vec<TimelineRow> = all
            .iter()
            .filter_map(|payload| {
                let top = analysis::is_top_level(payload, &logged);
                if !top && !nested.unwrap_or(false) {
                    return None;
                }
                let row = analysis::timeline_row(payload, !top);
                match &verb {
                    Some(v) if !row.verb.contains(v.trim()) => return None,
                    None if is_plumbing(&row.verb) => return None,
                    _ => {}
                }
                if failed_only.unwrap_or(false) && row.ok {
                    return None;
                }
                if let Some(filter) = &caller {
                    if !caller_matches(&row.caller, filter) {
                        return None;
                    }
                }
                Some(row)
            })
            .collect();
        TimelineResult {
            total: rows.len() as u64,
            rows: rows.into_iter().take(limit).collect(),
            ..Default::default()
        }
    }

    async fn entry(&self, call_id: Option<String>) -> EntryResult {
        let Some(call_id) = call_id.filter(|c| !c.trim().is_empty()) else {
            return EntryResult::default();
        };
        crate::flush();
        let store = match records::store(false) {
            Ok(store) => store,
            Err((code, message)) => return refuse!(EntryResult, code, message),
        };
        let item = match find_record(store.as_ref(), &call_id) {
            Ok(Some(item)) => item,
            Ok(None) => {
                return refuse!(
                    EntryResult,
                    "not-found",
                    format!("no call record {call_id} in the log")
                )
            }
            Err(e) => return refuse!(EntryResult, "store-error", e),
        };
        let payload = records::payload_json(&item);
        let steps = analysis::step_rows(&payload);
        let row = analysis::timeline_row(&payload, false);
        let mut summary = BTreeMap::new();
        summary.insert("verb".into(), row.verb.clone());
        summary.insert("caller".into(), row.caller.clone());
        summary.insert("started".into(), row.started_at.clone());
        summary.insert(
            "outcome".into(),
            if row.ok {
                "ok".into()
            } else {
                row.code.clone().unwrap_or_else(|| "failed".into())
            },
        );
        summary.insert("total ms".into(), row.duration_ms.to_string());
        summary.insert("self ms".into(), row.self_ms.to_string());
        summary.insert("steps".into(), row.steps.to_string());
        summary.insert("writes".into(), row.writes.to_string());
        summary.insert("reads".into(), row.reads.to_string());
        for field in ["run", "host", "trace_id", "parent_call"] {
            if let Some(value) = text(&payload, field) {
                summary.insert(field.replace('_', " "), value.to_string());
            }
        }
        if let Some(build) = payload.get("build") {
            summary.insert("build".into(), build.to_string());
        }
        if let Some(args) = payload.get("args") {
            let mut shown = args.to_string();
            if shown.len() > 400 {
                shown.truncate(400);
                shown.push('…');
            }
            summary.insert("args".into(), shown);
        }
        let notes = notes_of(store.as_ref(), item.id).unwrap_or_default();
        EntryResult {
            call_id: Some(item.id.to_string()),
            summary,
            slowest: analysis::slowest(&steps, 5),
            folded: analysis::folded(&payload),
            plot: analysis::self_time_plot(&steps),
            replayable: payload
                .get("args_replayable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            record: Value::Object(payload),
            steps,
            notes,
            ..Default::default()
        }
    }

    async fn annotate(&self, call_id: String, text_value: String) -> AnnotateResult {
        let body = text_value.trim();
        if body.is_empty() {
            return refuse!(AnnotateResult, "invalid-argument", "a note needs text");
        }
        crate::flush();
        let store = match records::store(true) {
            Ok(store) => store,
            Err((code, message)) => return refuse!(AnnotateResult, code, message),
        };
        let record = match find_record(store.as_ref(), &call_id) {
            Ok(Some(item)) => item,
            Ok(None) => {
                return refuse!(
                    AnnotateResult,
                    "not-found",
                    format!("no call record {call_id} in the log")
                )
            }
            Err(e) => return refuse!(AnnotateResult, "store-error", e),
        };
        let who = caller();
        let mut payload = Map::new();
        payload.insert("text".into(), json!(body));
        payload.insert("call_id".into(), json!(record.id.to_string()));
        let note = records::new_item(
            ItemId::new_v4(),
            records::NOTE,
            payload,
            who.author(),
            records::actor_kind(&who),
            chrono::Utc::now(),
            Some(record.id),
        );
        match store.insert(note.clone()) {
            Ok(_) => AnnotateResult {
                note: Some(note_row(&note)),
                ..Default::default()
            },
            Err(e) => refuse!(AnnotateResult, "store-error", e),
        }
    }

    async fn compare(
        &self,
        verb: String,
        run_a: Option<String>,
        run_b: Option<String>,
    ) -> CompareResult {
        crate::flush();
        let store = match records::store(false) {
            Ok(store) => store,
            Err((code, message)) => return refuse!(CompareResult, code, message),
        };
        let set = |run: &Option<String>| -> Result<Vec<Map<String, Value>>, String> {
            let run = run.as_deref().filter(|r| !r.trim().is_empty());
            Ok(call_records(store.as_ref(), run, false)?
                .into_iter()
                .filter(|p| text(p, "verb") == Some(verb.as_str()))
                .collect())
        };
        let (a, b) = match (set(&run_a), set(&run_b)) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(e), _) | (_, Err(e)) => return refuse!(CompareResult, "store-error", e),
        };
        let label = |run: &Option<String>| run.clone().unwrap_or_else(|| "all".into());
        CompareResult {
            rows: analysis::compare(&a, &b),
            a: label(&run_a),
            b: label(&run_b),
            verb,
            ..Default::default()
        }
    }

    async fn replay(&self, call_id: String) -> ReplayResult {
        crate::flush();
        let store = match records::store(false) {
            Ok(store) => store,
            Err((code, message)) => return refuse!(ReplayResult, code, message),
        };
        let item = match find_record(store.as_ref(), &call_id) {
            Ok(Some(item)) => item,
            Ok(None) => {
                return refuse!(
                    ReplayResult,
                    "not-found",
                    format!("no call record {call_id} in the log")
                )
            }
            Err(e) => return refuse!(ReplayResult, "store-error", e),
        };
        let payload = records::payload_json(&item);
        if payload.get("args_replayable").and_then(Value::as_bool) != Some(true) {
            return refuse!(
                ReplayResult,
                "not-replayable",
                "the log kept a summary of this call's arguments, not the arguments \
                 themselves, so it cannot be run again"
            );
        }
        let name = text(&payload, "verb").unwrap_or("");
        let Some(verb) = VerbDescriptor::find(name) else {
            return refuse!(
                ReplayResult,
                "not-found",
                format!("{name} is not linked in this process")
            );
        };
        let args = payload.get("args").cloned().unwrap_or(json!({}));
        match pipeline::invoke(verb, Call::new(caller(), args)).await {
            Ok(answer) => ReplayResult {
                ok: answer.get("ok").and_then(Value::as_bool) != Some(false),
                replay_of: Some(item.id.to_string()),
                call_id: context::current()
                    .and_then(|c| c.notes.steps().0.last().map(|s| s.call_id.clone())),
                answer,
                ..Default::default()
            },
            Err(e) => refuse!(ReplayResult, "verb-failed", e),
        }
    }

    async fn export(&self, run: String) -> ExportResult {
        crate::flush();
        let store = match records::store(false) {
            Ok(store) => store,
            Err((code, message)) => return refuse!(ExportResult, code, message),
        };
        let all = match runs::list(store.as_ref()) {
            Ok(all) => all,
            Err(e) => return refuse!(ExportResult, "store-error", e),
        };
        let Some(found) = all.iter().find(|r| r.id.to_string() == run.trim()) else {
            return refuse!(ExportResult, "not-found", format!("no run {run}"));
        };
        let calls = match call_records(store.as_ref(), Some(run.trim()), false) {
            Ok(calls) => calls,
            Err(e) => return refuse!(ExportResult, "store-error", e),
        };
        let logged: BTreeSet<String> = calls
            .iter()
            .filter_map(|p| text(p, "_id").map(str::to_string))
            .collect();
        let top: Vec<_> = calls
            .into_iter()
            .filter(|p| analysis::is_top_level(p, &logged))
            .filter(|p| !is_plumbing(text(p, "verb").unwrap_or("")))
            .collect();
        let mut notes: BTreeMap<String, Vec<String>> = BTreeMap::new();
        if let Ok(all_notes) = records::all_of(store.as_ref(), records::NOTE, &[], false) {
            for note in all_notes {
                if let Some(parent) = note.parent {
                    let p = records::payload_json(&note);
                    notes
                        .entry(parent.to_string())
                        .or_default()
                        .push(text(&p, "text").unwrap_or("").to_string());
                }
            }
        }
        ExportResult {
            markdown: analysis::export_markdown(found, &top, &notes),
            ..Default::default()
        }
    }

    async fn prune(&self, keep: u64) -> PruneResult {
        crate::flush();
        let store = match records::store(true) {
            Ok(store) => store,
            Err((code, message)) => return refuse!(PruneResult, code, message),
        };
        match crate::sink::prune(store.as_ref(), keep as usize) {
            Ok(deleted) => PruneResult {
                deleted,
                ..Default::default()
            },
            Err(e) => refuse!(PruneResult, "store-error", e),
        }
    }
}

impress_service_impl! {
    service = NotebookService,
    safety = read_only,
    since = "0.2.0",
    effects = {
        reads: ["core/verb-call@1.0.0"],
        writes: [],
        reach: [],
    },
    impl = DefaultNotebookService,
    instance = DefaultNotebookService::new,
    methods = [
        /// Every run, newest first, with call counts and the open one.
        runs() -> RunsResult,
        /// Open a run, closing any open one.
        run_start(
            /// What the run is, as a person would name it.
            name: String,
            /// Record read-only calls too while the run is open.
            record_reads: Option<bool>
        ) -> RunResult,
        /// Close the open run.
        run_stop() -> RunResult,
        /// Call records, newest first, filtered.
        timeline(
            /// Only calls of this run (its id).
            run: Option<String>,
            /// Only this caller: `human`, `agent`, `agent:<name>`, `system`.
            caller: Option<String>,
            /// Only verbs whose name contains this.
            verb: Option<String>,
            /// Only failed calls.
            failed_only: Option<bool>,
            /// Include calls made inside other logged calls.
            nested: Option<bool>,
            /// At most this many rows (default 100, at most 1000).
            limit: Option<u32>
        ) -> TimelineResult,
        /// One call record in full, with its steps and notes.
        entry(
            /// The call's id (the record's id); absent: an empty entry.
            call_id: Option<String>
        ) -> EntryResult,
        /// Attach a note to a call record.
        annotate(
            /// The call's id.
            call_id: String,
            /// The note.
            text: String
        ) -> AnnotateResult,
        /// Step timings of a verb's calls in two runs.
        compare(
            /// The verb whose calls to compare.
            verb: String,
            /// The first run's id; absent: every run.
            run_a: Option<String>,
            /// The second run's id; absent: every run.
            run_b: Option<String>
        ) -> CompareResult,
        /// Run a recorded call again.
        replay(
            /// The call's id.
            call_id: String
        ) -> ReplayResult,
        /// A run as Markdown.
        export(
            /// The run's id.
            run: String
        ) -> ExportResult,
        /// Delete the oldest records outside any run.
        prune(
            /// How many records outside a run to keep.
            keep: u64
        ) -> PruneResult,
    ],
}
