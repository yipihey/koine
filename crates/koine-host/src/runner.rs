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
//! Not here, on purpose: progress. A verb in this codebase has no channel to
//! report it, so a job is running or it is not. `GET /api/job/<id>` and
//! `POST /api/job/<id>/cancel` are the host's reading and cancelling; the
//! `impel-service_job-*` verbs the job module's docs name belong to impel and
//! are not linked here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use impress_item::schema::refs;
use impress_item::{ActorKind, AttributedStore, FieldMutation, Item, ItemId, Priority, Value};
use impress_service_core::job::JobHandle;
use impress_service_core::pipeline::adverbs::BackgroundRunner;
use impress_service_core::pipeline::{self, Call, CallerIdentity};
use impress_service_core::refusal::{codes, Refusal};
use impress_service_core::wire::WIRE_VERSION;
use impress_service_core::VerbHandle;
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
}

/// Tokio-task job runner over a store. See the module docs.
pub struct JobRunner {
    store: Arc<dyn AttributedStore>,
    runtime: tokio::runtime::Handle,
    live: Arc<Mutex<HashMap<ItemId, tokio::task::AbortHandle>>>,
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

    /// How many jobs this process is running now.
    pub fn running(&self) -> usize {
        self.live.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Abort every running job (the host is stopping), writing `cancelled`.
    pub fn cancel_all(&self) {
        let ids: Vec<ItemId> = self
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .copied()
            .collect();
        for id in ids {
            let _ = self.cancel(id);
        }
    }

    /// The job's row as the wire shows it, or `None` when there is no such
    /// job.
    pub fn status(&self, id: ItemId) -> Option<Json> {
        let item = self.store.get(id).ok().flatten()?;
        if item.schema != refs::TASK {
            return None;
        }
        let text = |key: &str| match item.payload.get(key) {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        };
        let mut state = text(field::STATE).unwrap_or_default();
        let live = self
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&id);
        if state == "running" && !live {
            state = "interrupted".into();
        }
        let mut out = json!({
            "ok": true,
            "wire_version": WIRE_VERSION,
            "job": { "id": id.to_string(), "kind": text(field::KIND), "state": state },
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
        Some(out)
    }

    /// Cancel a running job. `Ok(true)` when this call stopped it,
    /// `Ok(false)` when it was not running here (finished, or never
    /// started by this process).
    pub fn cancel(&self, id: ItemId) -> Result<bool, Refusal> {
        let handle = self
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        let Some(handle) = handle else {
            return Ok(false);
        };
        handle.abort();
        write_end(&*self.store, id, "cancelled", None, None, None)?;
        Ok(true)
    }
}

impl BackgroundRunner for JobRunner {
    fn start(&self, verb: VerbHandle, call: Call) -> Result<JobHandle, Refusal> {
        let id = uuid::Uuid::new_v4();
        let kind = verb.name().to_string();
        let now = Utc::now();
        let (author_kind, author) = match &call.caller {
            CallerIdentity::Person => (ActorKind::Human, call.caller.author()),
            CallerIdentity::Agent(_) | CallerIdentity::Provider(_) => {
                (ActorKind::Agent, call.caller.author())
            }
            _ => (ActorKind::System, call.caller.author()),
        };
        let item = Item {
            id,
            schema: refs::TASK,
            payload: [
                (field::KIND.to_string(), Value::String(kind.clone())),
                (field::STATE.to_string(), Value::String("running".into())),
                (field::CALLER.to_string(), Value::String(author.clone())),
                (field::STARTED.to_string(), Value::String(now.to_rfc3339())),
            ]
            .into_iter()
            .collect(),
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
        };
        self.store
            .insert(item)
            .map_err(|e| Refusal::store(format!("write job row: {e}")))?;

        let store = self.store.clone();
        let live = self.live.clone();
        // Held across the spawn and the insert, so a job that finishes at
        // once cannot remove an entry that is not there yet.
        let mut table = self.live.lock().unwrap_or_else(|e| e.into_inner());
        let task = self.runtime.spawn(async move {
            let outcome = pipeline::invoke_handle(verb, call).await;
            let (state, result, code, message) = match outcome {
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
            };
            // A cancel removed the entry first and wrote its own end.
            let still_ours = live
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id)
                .is_some();
            if still_ours {
                if let Err(e) = write_end(&*store, id, state, result, code, message) {
                    tracing::warn!(target: "koine-host", "job {id}: could not record its end: {e}");
                }
            }
        });
        table.insert(id, task.abort_handle());
        drop(table);
        Ok(JobHandle {
            id: id.to_string(),
            kind,
            state: "running".into(),
        })
    }
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
