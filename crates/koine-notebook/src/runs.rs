//! Runs: the notebook's "recording" switch (plan-koine-notebook N-D5).
//!
//! A run is a `koine/run@1.0.0` item. While one is open it is the process's
//! `audit::current_run`, so every call record finished meanwhile names it,
//! and when it records reads, read-only calls are recorded too. One run is
//! open at a time; opening a second closes the first. [`reopen`] is what a
//! host calls at start, so a restart does not end a run.

use std::sync::Arc;

use chrono::Utc;
use impress_item::{AttributedStore, FieldMutation, ItemId, Value as ItemValue};
use impress_service_core::pipeline::audit::{self, RunTag};
use serde_json::{json, Map, Value};

use crate::records;

/// One run, as the verbs answer it.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub id: ItemId,
    pub name: String,
    pub status: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub record_reads: bool,
    pub host: String,
    pub build: Value,
}

impl Run {
    pub fn of(item: &impress_item::Item) -> Self {
        let p = records::payload_json(item);
        Run {
            id: item.id,
            name: records::text(&p, "name").unwrap_or("").to_string(),
            status: records::text(&p, "status").unwrap_or("closed").to_string(),
            started_at: records::text(&p, "started_at").unwrap_or("").to_string(),
            ended_at: records::text(&p, "ended_at").map(str::to_string),
            record_reads: p
                .get("record_reads")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            host: records::text(&p, "host").unwrap_or("").to_string(),
            build: p.get("build").cloned().unwrap_or(Value::Null),
        }
    }

    pub fn is_open(&self) -> bool {
        self.status == "open"
    }
}

/// Every run, newest first.
pub fn list(store: &dyn AttributedStore) -> Result<Vec<Run>, String> {
    Ok(records::all_of(store, records::RUN, &[], true)?
        .iter()
        .map(Run::of)
        .collect())
}

/// The open run in the store, if any (the newest, should two be open).
pub fn open_run(store: &dyn AttributedStore) -> Result<Option<Run>, String> {
    Ok(list(store)?.into_iter().find(Run::is_open))
}

/// Open a run named `name`, closing any open one. `host` and `build` are
/// stamped on it (N-D6).
pub fn start(
    store: &Arc<dyn AttributedStore>,
    name: &str,
    record_reads: bool,
    author: String,
    author_kind: impress_item::ActorKind,
    host: &str,
    build: Value,
) -> Result<Run, String> {
    stop(store.as_ref())?;
    let now = Utc::now();
    let mut payload = Map::new();
    payload.insert("name".into(), json!(name));
    payload.insert("status".into(), json!("open"));
    payload.insert(
        "started_at".into(),
        json!(now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
    );
    payload.insert("record_reads".into(), json!(record_reads));
    payload.insert("host".into(), json!(host));
    payload.insert("build".into(), build);
    let id = ItemId::new_v4();
    let item = records::new_item(id, records::RUN, payload, author, author_kind, now, None);
    store.insert(item.clone()).map_err(|e| e.to_string())?;
    audit::set_current_run(Some(RunTag {
        id: id.to_string(),
        record_reads,
    }));
    Ok(Run::of(&item))
}

/// Close every open run. Answers the runs closed.
pub fn stop(store: &dyn AttributedStore) -> Result<Vec<Run>, String> {
    let now = Utc::now()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        .to_string();
    let mut closed = Vec::new();
    for run in list(store)?.into_iter().filter(Run::is_open) {
        store
            .update(
                run.id,
                vec![
                    FieldMutation::SetPayload("status".into(), ItemValue::String("closed".into())),
                    FieldMutation::SetPayload("ended_at".into(), ItemValue::String(now.clone())),
                ],
            )
            .map_err(|e| e.to_string())?;
        closed.push(Run {
            status: "closed".into(),
            ended_at: Some(now.clone()),
            ..run
        });
    }
    audit::set_current_run(None);
    Ok(closed)
}

/// Make the store's open run the process's again (a host at start).
pub fn reopen(store: &dyn AttributedStore) -> Result<Option<Run>, String> {
    let open = open_run(store)?;
    audit::set_current_run(open.as_ref().map(|run| RunTag {
        id: run.id.to_string(),
        record_reads: run.record_reads,
    }));
    Ok(open)
}
