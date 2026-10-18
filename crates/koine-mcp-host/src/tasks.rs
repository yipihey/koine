//! MCP tasks for `tools/call` (protocol 2025-11-25, plan-koine-trust TR-D5).
//!
//! A `tools/call` with a `task` parameter answers a `CreateTaskResult` at
//! once and runs the verb on its own thread. A call the policy holds for a
//! person is a task in `input_required`: polling it reads the pending call
//! (`review-service_show`, when linked), so the task completes with the
//! approved call's answer, or fails when the person rejects it or it
//! expires. `tasks/result` blocks until the task is terminal.
//!
//! Tasks live in this process, bound to the caller that made them: another
//! caller asking for a task id is told it does not exist. Ids are random
//! UUIDs, and a task is forgotten after its ttl (at most a day).

use std::collections::HashMap;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
use impress_service_core::call;
use impress_service_core::pipeline::{Call, CallerIdentity};
use serde_json::{json, Value};

/// The `_meta` key that ties a message to its task.
pub const RELATED_TASK: &str = "io.modelcontextprotocol/related-task";
/// A task's lifetime when the requestor names none: 15 minutes.
const DEFAULT_TTL_MS: u64 = 15 * 60 * 1000;
/// The longest lifetime granted: a day, long enough for a person to review.
const MAX_TTL_MS: u64 = 24 * 60 * 60 * 1000;
/// How often a requestor is asked to poll.
const POLL_MS: u64 = 2000;
/// How often a blocked `tasks/result` re-reads a held call.
const REFRESH: Duration = Duration::from_secs(2);
/// Page size of `tasks/list`.
const PAGE: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Working,
    InputRequired,
    Completed,
    Failed,
    Cancelled,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Working => "working",
            Status::InputRequired => "input_required",
            Status::Completed => "completed",
            Status::Failed => "failed",
            Status::Cancelled => "cancelled",
        }
    }

    fn terminal(self) -> bool {
        matches!(self, Status::Completed | Status::Failed | Status::Cancelled)
    }
}

struct Task {
    owner: String,
    caller: CallerIdentity,
    status: Status,
    message: Option<String>,
    created: DateTime<Utc>,
    updated: DateTime<Utc>,
    ttl_ms: u64,
    expires: Instant,
    /// The pending call a held task waits on.
    pending: Option<String>,
    /// What the `tools/call` answers: a JSON-RPC response without its id
    /// (`result` or `error`).
    outcome: Option<Value>,
}

impl Task {
    fn wire(&self, id: &str) -> Value {
        let mut task = json!({
            "taskId": id,
            "status": self.status.as_str(),
            "createdAt": self.created.to_rfc3339_opts(SecondsFormat::Millis, true),
            "lastUpdatedAt": self.updated.to_rfc3339_opts(SecondsFormat::Millis, true),
            "ttl": self.ttl_ms,
        });
        if !self.status.terminal() {
            task["pollInterval"] = json!(POLL_MS);
        }
        if let Some(message) = &self.message {
            task["statusMessage"] = json!(message);
        }
        task
    }

    fn set(&mut self, status: Status, message: Option<String>) {
        // A cancelled task stays cancelled whatever its verb does after.
        if self.status == Status::Cancelled {
            return;
        }
        self.status = status;
        self.message = message;
        self.updated = Utc::now();
    }
}

#[derive(Default)]
struct Table {
    tasks: Mutex<HashMap<String, Task>>,
    changed: Condvar,
}

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(Table::default)
}

fn forget_expired(tasks: &mut HashMap<String, Task>) {
    let now = Instant::now();
    tasks.retain(|_, task| task.expires > now);
}

/// Start `run` as a task for `caller`; answer the `CreateTaskResult`.
/// `run` answers the `tools/call` response object (`result` or `error`).
pub(crate) fn create(
    id: Value,
    caller: &CallerIdentity,
    requested: &Value,
    run: impl FnOnce() -> Value + Send + 'static,
) -> Value {
    let ttl_ms = requested
        .get("ttl")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TTL_MS)
        .clamp(1, MAX_TTL_MS);
    let task_id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now();
    let task = Task {
        owner: caller.author(),
        caller: caller.clone(),
        status: Status::Working,
        message: Some("The call is running.".into()),
        created: now,
        updated: now,
        ttl_ms,
        expires: Instant::now() + Duration::from_millis(ttl_ms),
        pending: None,
        outcome: None,
    };
    let wire = task.wire(&task_id);
    {
        let mut tasks = table().tasks.lock().unwrap_or_else(|e| e.into_inner());
        forget_expired(&mut tasks);
        tasks.insert(task_id.clone(), task);
    }
    let finished = task_id.clone();
    std::thread::spawn(move || {
        let outcome = run();
        finish(&finished, outcome);
    });
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "task": wire,
            "_meta": {
                "io.modelcontextprotocol/model-immediate-response":
                    "The call is running as a task; poll tasks/get for its status.",
            },
        },
    })
}

/// Record what the call answered: held for a person, done or failed.
fn finish(task_id: &str, outcome: Value) {
    let table = table();
    let mut tasks = table.tasks.lock().unwrap_or_else(|e| e.into_inner());
    let Some(task) = tasks.get_mut(task_id) else {
        return;
    };
    let structured = outcome.pointer("/result/structuredContent");
    let held = structured
        .filter(|s| s.get("status").and_then(Value::as_str) == Some("input_required"))
        .and_then(|s| s.get("pending_id").and_then(Value::as_str))
        .map(str::to_owned);
    match held {
        Some(pending) => {
            task.pending = Some(pending.clone());
            task.outcome = Some(outcome);
            task.set(
                Status::InputRequired,
                Some(format!(
                    "Waiting for a person to approve pending call {pending} in the app's review list."
                )),
            );
        }
        None => settle(task, outcome),
    }
    table.changed.notify_all();
}

/// A terminal outcome: `failed` for a JSON-RPC error or `isError`.
fn settle(task: &mut Task, outcome: Value) {
    let failed = outcome.get("error").is_some()
        || outcome.pointer("/result/isError").and_then(Value::as_bool) == Some(true);
    let message = if failed {
        outcome
            .pointer("/error/message")
            .or_else(|| outcome.pointer("/result/structuredContent/message"))
            .or_else(|| outcome.pointer("/result/content/0/text"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    } else {
        None
    };
    task.outcome = Some(outcome);
    task.set(
        if failed {
            Status::Failed
        } else {
            Status::Completed
        },
        message,
    );
}

/// Re-read the pending call a held task waits on. Runs without the table
/// lock held, since the read is a verb call.
fn refresh(task_id: &str) {
    let (pending, caller) = {
        let tasks = table().tasks.lock().unwrap_or_else(|e| e.into_inner());
        match tasks.get(task_id) {
            Some(task) if task.status == Status::InputRequired => {
                match (&task.pending, task.caller.clone()) {
                    (Some(pending), caller) => (pending.clone(), caller),
                    _ => return,
                }
            }
            _ => return,
        }
    };
    let Some(show) = call::find("review-service_show") else {
        return;
    };
    let answer =
        impress_service_core::runtime::block_on(impress_service_core::pipeline::invoke_handle(
            show,
            Call::new(caller, json!({ "id": pending })),
        ));
    let Ok(answer) = answer else {
        return;
    };
    let Some(row) = answer.get("pending") else {
        return;
    };
    let state = row.get("state").and_then(Value::as_str).unwrap_or("");
    let table = table();
    let mut tasks = table.tasks.lock().unwrap_or_else(|e| e.into_inner());
    let Some(task) = tasks.get_mut(task_id) else {
        return;
    };
    if task.status != Status::InputRequired {
        return;
    }
    match state {
        "running" => task.set(
            Status::Working,
            Some("Approved; the call is running.".into()),
        ),
        "done" | "failed" => {
            let result = row.get("result").cloned().unwrap_or(Value::Null);
            settle(task, crate::tool_outcome(Ok(result)));
        }
        "rejected" | "expired" => {
            let refusal = json!({
                "ok": false,
                "code": if state == "rejected" { "rejected" } else { "expired" },
                "status": "rejected",
                "message": format!("The person did not approve pending call {pending} ({state})."),
            });
            settle(task, crate::tool_outcome(Ok(refusal)));
        }
        _ => {}
    }
    table.changed.notify_all();
}

/// Find `task_id` for `caller`, or the `-32602` JSON-RPC error.
fn lookup<T>(
    id: &Value,
    task_id: Option<&str>,
    caller: &CallerIdentity,
    f: impl FnOnce(&str, &mut Task) -> T,
) -> Result<T, Value> {
    let Some(task_id) = task_id else {
        return Err(crate::json_rpc_error(
            id.clone(),
            -32602,
            "params.taskId is required".into(),
        ));
    };
    let mut tasks = table().tasks.lock().unwrap_or_else(|e| e.into_inner());
    forget_expired(&mut tasks);
    match tasks.get_mut(task_id) {
        Some(task) if task.owner == caller.author() => Ok(f(task_id, task)),
        _ => Err(crate::json_rpc_error(
            id.clone(),
            -32602,
            "Failed to retrieve task: Task not found".into(),
        )),
    }
}

fn task_id(request: &Value) -> Option<&str> {
    request.pointer("/params/taskId").and_then(Value::as_str)
}

/// `tasks/get`.
pub(crate) fn get(id: Value, request: &Value, caller: &CallerIdentity) -> Value {
    if let Some(task_id) = task_id(request) {
        refresh(task_id);
    }
    match lookup(&id, task_id(request), caller, |tid, task| task.wire(tid)) {
        Ok(task) => crate::success(id, task),
        Err(error) => error,
    }
}

/// `tasks/result`: the call's own answer once the task is terminal, with
/// the related-task `_meta`. Blocks until then (or until the task expires).
pub(crate) fn result(id: Value, request: &Value, caller: &CallerIdentity) -> Value {
    let Some(tid) = task_id(request).map(str::to_owned) else {
        return crate::json_rpc_error(id, -32602, "params.taskId is required".into());
    };
    loop {
        refresh(&tid);
        let ready = lookup(&id, Some(&tid), caller, |_, task| {
            task.status
                .terminal()
                .then(|| task.outcome.clone())
                .map(|o| (task.status, o))
        });
        match ready {
            Err(error) => return error,
            Ok(Some((status, outcome))) => {
                let mut response = outcome.unwrap_or_else(|| {
                    // Cancelled before the call answered.
                    json!({ "error": { "code": -32603, "message": "The task was cancelled." } })
                });
                if status == Status::Cancelled && response.get("result").is_some() {
                    response = json!({ "error": { "code": -32603, "message": "The task was cancelled." } });
                }
                if let Some(result) = response.get_mut("result").and_then(Value::as_object_mut) {
                    let meta = result.entry("_meta").or_insert_with(|| json!({}));
                    meta[RELATED_TASK] = json!({ "taskId": tid });
                }
                response["jsonrpc"] = json!("2.0");
                response["id"] = id;
                return response;
            }
            Ok(None) => {
                let table = table();
                let tasks = table.tasks.lock().unwrap_or_else(|e| e.into_inner());
                let _ = table.changed.wait_timeout(tasks, REFRESH);
            }
        }
    }
}

/// `tasks/list`: the caller's tasks, oldest first, paged by an opaque
/// cursor (an offset).
pub(crate) fn list(id: Value, request: &Value, caller: &CallerIdentity) -> Value {
    let start = match request.pointer("/params/cursor") {
        None | Some(Value::Null) => 0,
        Some(cursor) => match cursor.as_str().and_then(|c| c.parse::<usize>().ok()) {
            Some(start) => start,
            None => {
                return crate::json_rpc_error(id, -32602, "Invalid cursor".into());
            }
        },
    };
    let mut tasks = table().tasks.lock().unwrap_or_else(|e| e.into_inner());
    forget_expired(&mut tasks);
    let owner = caller.author();
    let mut mine: Vec<(&String, &Task)> = tasks.iter().filter(|(_, t)| t.owner == owner).collect();
    mine.sort_by(|a, b| a.1.created.cmp(&b.1.created).then(a.0.cmp(b.0)));
    if start > mine.len() {
        return crate::json_rpc_error(id, -32602, "Invalid cursor".into());
    }
    let page: Vec<Value> = mine
        .iter()
        .skip(start)
        .take(PAGE)
        .map(|(tid, task)| task.wire(tid))
        .collect();
    let mut result = json!({ "tasks": page });
    if start + PAGE < mine.len() {
        result["nextCursor"] = json!((start + PAGE).to_string());
    }
    crate::success(id, result)
}

/// `tasks/cancel`: a task not yet terminal becomes `cancelled`. A held
/// call is withdrawn (`review-service_reject`, when linked) so it does not
/// run later; a running verb finishes, but its answer is not delivered.
pub(crate) fn cancel(id: Value, request: &Value, caller: &CallerIdentity) -> Value {
    let cancelled = lookup(&id, task_id(request), caller, |tid, task| {
        if task.status.terminal() {
            return Err(format!(
                "Cannot cancel task: already in terminal status '{}'",
                task.status.as_str()
            ));
        }
        task.set(
            Status::Cancelled,
            Some("The task was cancelled by request.".into()),
        );
        Ok((task.wire(tid), task.pending.clone()))
    });
    table().changed.notify_all();
    match cancelled {
        Err(error) => error,
        Ok(Err(message)) => crate::json_rpc_error(id, -32602, message),
        Ok(Ok((wire, pending))) => {
            if let (Some(pending), Some(reject)) = (pending, call::find("review-service_reject")) {
                let _ = impress_service_core::runtime::block_on(
                    impress_service_core::pipeline::invoke_handle(
                        reject,
                        Call::new(caller.clone(), json!({ "id": pending })),
                    ),
                );
            }
            crate::success(id, wire)
        }
    }
}
