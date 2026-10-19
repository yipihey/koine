//! Ingest a conforming run directory (R-D11, R-D13): `run.json` becomes
//! the `run@1.0.0` row (the item it already is), `events.jsonl` is tailed
//! into the run's task-event ring with its own `seq`, `outputs.json` is
//! indexed, and a `3xx` exit files a `diagnose` request on the agent desk.
//! Idempotent: a second ingest advances from the row's `last_event_seq`
//! and rewrites nothing the code wrote.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use impress_item::item::{ActorKind, Item, ItemId};
use impress_item::operation::{OperationIntent, OperationSpec, OperationType, RetentionTier};
use impress_item::store::AttributedStore;
use koine_tasks::job;
use run_convention::check_dir;
use serde_json::{json, Map, Value};

use crate::dto::Refused;
use crate::kind::{self, AUTHOR};

/// How many trailing events a `diagnose` request carries (R-D13 says 50;
/// the brief for this package says 20, and 20 of a code's `progress` and
/// `metric` lines is what fits a model's window beside the record).
pub const DIAGNOSE_CONTEXT_EVENTS: usize = 20;

/// What a caller knows about a run it launched, recorded beside what the
/// code wrote.
#[derive(Debug, Clone, Default)]
pub struct Launched {
    pub pid: Option<u32>,
    pub sweep: Option<String>,
    pub name: Option<String>,
    /// The process has exited with this status (set by the tail after the
    /// last ingest).
    pub exited: Option<i32>,
}

/// What one ingest did.
#[derive(Debug, Clone)]
pub struct Ingested {
    pub item: Item,
    pub events_added: u64,
    pub findings: Vec<String>,
}

/// The parsed lines of `events.jsonl`, a trailing partial line ignored
/// (R-D6.6).
pub fn read_events(dir: &Path) -> Vec<Value> {
    let Ok(bytes) = fs::read(dir.join("events.jsonl")) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let ends_nl = text.ends_with('\n');
    let lines: Vec<&str> = text.split('\n').collect();
    let complete = lines.len().saturating_sub(1);
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .filter_map(|(i, l)| match serde_json::from_str::<Value>(l) {
            Ok(v) => Some(v),
            Err(_) if i == complete && !ends_nl => None,
            Err(_) => None,
        })
        .collect()
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

/// Ingest `dir` into `store`. Refuses a directory that does not conform
/// (`invalid-argument`, with `check_dir`'s findings) unless its record is
/// `provenance: "imported"` (R-D11), whose findings are kept on the row.
pub fn ingest_dir(
    store: &dyn AttributedStore,
    dir: &Path,
    launched: &Launched,
) -> Result<Ingested, Refused> {
    let dir: PathBuf = dir
        .canonicalize()
        .map_err(|e| Refused::invalid(format!("{}: {e}", dir.display())))?;
    let report = check_dir(&dir);
    let run = read_json(&dir.join("run.json"))
        .ok_or_else(|| Refused::invalid(format!("{}: no readable run.json", dir.display())))?;
    let imported = run.get("provenance").and_then(Value::as_str) == Some("imported");
    let findings: Vec<String> = report.findings.iter().map(ToString::to_string).collect();
    if !report.ok() && !imported {
        return Err(Refused::invalid(format!(
            "{} does not conform (run-check): {}",
            dir.display(),
            findings.join("; ")
        )));
    }
    let id: ItemId = run
        .get("id")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| Refused::invalid("run.json has no uuid `id`"))?;

    let existing = store
        .get(id)
        .map_err(|e| Refused::new("store-error", e.to_string()))?;
    let existing_payload = existing.as_ref().map(kind::payload_json);
    let from = existing_payload
        .as_ref()
        .and_then(|p| p.get("last_event_seq"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    // The ring: every line past the cursor, under its own seq.
    let events = read_events(&dir);
    let mut added = 0u64;
    let mut last_seq = from;
    let mut progress: Option<Value> = None;
    let mut metrics: Map<String, Value> = Map::new();
    for ev in &events {
        let Some(seq) = ev.get("seq").and_then(Value::as_u64) else {
            continue;
        };
        let name = ev.get("kind").and_then(Value::as_str).unwrap_or("note");
        match name {
            "progress" => progress = Some(ev.clone()),
            "metric" => {
                if let Some(v) = ev.get("values").and_then(Value::as_object) {
                    metrics.extend(v.clone());
                }
            }
            _ => {}
        }
        if seq > from {
            let wrote = job::append_event_at(store, id, seq, name, ev, AUTHOR)
                .map_err(|e| Refused::new("store-error", e.to_string()))?;
            if wrote {
                added += 1;
            }
            last_seq = last_seq.max(seq);
        }
    }

    // The row: run.json's blocks, then what koine derives.
    let mut fields: BTreeMap<String, Value> = BTreeMap::new();
    if let Some(o) = run.as_object() {
        for (k, v) in o {
            if k != "id" {
                fields.insert(k.clone(), v.clone());
            }
        }
    }
    fields.insert("dir".into(), json!(dir.to_string_lossy()));
    let state = run
        .pointer("/status/state")
        .and_then(Value::as_str)
        .unwrap_or("created")
        .to_string();
    fields.insert("state".into(), json!(state));
    fields.insert("last_event_seq".into(), json!(last_seq));
    if let Some(p) = progress {
        fields.insert("progress".into(), p);
    }
    if !metrics.is_empty() {
        fields.insert("metrics".into(), Value::Object(metrics));
    }
    if let Some(outs) = read_json(&dir.join("outputs.json")) {
        if let Some(a) = outs.get("outputs") {
            fields.insert("outputs".into(), a.clone());
        }
    }
    if let Some(notes) = read_json(&dir.join("notes.json")) {
        fields.insert("notes".into(), notes);
    }
    if !fields.contains_key("name") {
        if let Some(n) = &launched.name {
            fields.insert("name".into(), json!(n));
        }
    }
    let mut koine: Map<String, Value> = existing_payload
        .as_ref()
        .and_then(|p| p.get("koine"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(pid) = launched.pid {
        koine.insert("pid".into(), json!(pid));
    }
    if let Some(s) = &launched.sweep {
        koine.insert("sweep".into(), json!(s));
    }
    if let Some(code) = launched.exited {
        koine.insert(
            "exited".into(),
            json!({"code": code, "at": chrono::Utc::now().to_rfc3339()}),
        );
    }
    if !findings.is_empty() {
        koine.insert("findings".into(), json!(findings));
    }

    // R-D13: a numerical failure goes to the desk, once.
    let exit_code = run
        .pointer("/status/exit/code")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if state == "failed"
        && (300..400).contains(&exit_code)
        && !koine.contains_key("diagnose_request")
    {
        match file_diagnose(&run, &events, &dir) {
            Ok(request) => {
                koine.insert("diagnose_request".into(), json!(request));
            }
            Err(e) => tracing::warn!(run = %id, "could not file the diagnose request: {e}"),
        }
    }
    fields.insert("koine".into(), Value::Object(koine));

    let item = match existing {
        None => {
            let item = kind::run_item(id, fields);
            store
                .insert(item.clone())
                .map_err(|e| Refused::new("store-error", e.to_string()))?;
            item
        }
        Some(_) => {
            let patch: BTreeMap<String, impress_item::item::Value> = fields
                .iter()
                .map(|(k, v)| (k.clone(), kind::to_item(v)))
                .collect();
            store
                .apply_operation(OperationSpec {
                    target_id: id,
                    op_type: OperationType::PatchPayload(patch),
                    intent: OperationIntent::Routine,
                    reason: None,
                    batch_id: None,
                    author: AUTHOR.into(),
                    author_kind: ActorKind::System,
                    retention: RetentionTier::Compactable,
                })
                .map_err(|e| Refused::new("store-error", e.to_string()))?;
            store
                .get(id)
                .map_err(|e| Refused::new("store-error", e.to_string()))?
                .ok_or_else(|| Refused::new("store-error", "the run row vanished"))?
        }
    };
    Ok(Ingested {
        item,
        events_added: added,
        findings,
    })
}

/// File the `diagnose` request (R-D13) with the record, the exit and the
/// last [`DIAGNOSE_CONTEXT_EVENTS`] events. Returns the request id.
fn file_diagnose(run: &Value, events: &[Value], dir: &Path) -> Result<String, String> {
    let exit = run.pointer("/status/exit").cloned().unwrap_or(Value::Null);
    let code_name = run
        .pointer("/code/name")
        .and_then(Value::as_str)
        .unwrap_or("the code");
    let id = run.get("id").and_then(Value::as_str).unwrap_or("");
    let tail: Vec<Value> = events
        .iter()
        .rev()
        .take(DIAGNOSE_CONTEXT_EVENTS)
        .rev()
        .cloned()
        .collect();
    let prompt = format!(
        "Run {id} of {code_name} failed with exit {} ({}){}. Why, and what would fix it?",
        exit.get("code").and_then(Value::as_i64).unwrap_or(0),
        exit.get("label").and_then(Value::as_str).unwrap_or("?"),
        exit.get("message")
            .and_then(Value::as_str)
            .map(|m| format!(": {m}"))
            .unwrap_or_default()
    );
    let context = koine_agent::desk::tagged(
        "run",
        json!({
            "run": run,
            "dir": dir.to_string_lossy(),
            "params": read_json(&dir.join("params.json")),
            "events": tail,
        }),
    );
    let row = koine_agent::desk::file(
        koine_agent::desk::DIAGNOSE,
        &prompt,
        context,
        None,
        None,
        None,
        AUTHOR.into(),
        ActorKind::System,
    )
    .map_err(|e| e.message)?;
    Ok(row.id)
}
