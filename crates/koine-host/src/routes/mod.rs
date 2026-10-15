//! The route table (docs/plan-koine-host.md H-D4).
//!
//! | Route | Answers | Module |
//! |---|---|---|
//! | `GET /api/status` | app, port, store url and capabilities, verb count, declared and missing secrets | here |
//! | `/api/surface`, `/api/surface/*` | the surface route table | [`surface`] |
//! | `GET /api/layout/{tree,layouts}`, `POST /api/layout/{verb,op}` | the layout verbs | [`layout`] |
//! | `POST /api/verb/<name>` | one verb, through the pipeline | here |
//! | `GET /api/verb/<name>/surface` | the generated form for that verb (`verb_surface`) | here |
//! | `GET /ui`, `/ui/*` | the `koine-web` bundle | [`ui`] |
//! | `GET/PUT/DELETE /api/blob/<item>/<name>` | the store's blobs | [`blob`] |
//! | `GET /api/job`, `GET /api/job/<id>`, `POST /api/job/<id>/cancel` | the default job runner: what is running, a job's or routine's row (state, `progress`, `runs`, `line_count`), stopping it | here |
//! | `GET /api/job/<id>/lines?after=<n>` | what the job has emitted (`job::emit`): the lines from index `n` on, `next`, `total`, `dropped`, `state` | here |
//! | `GET /api/secret`, `PUT/DELETE /api/secret/<name>` | the secrets linked verbs declare (never a value) | [`secret`] |
//! | `GET/POST /api/host/network` | whether the host also listens beyond loopback; `POST {"allow", "bearer"?}` opens or closes the second listener (loopback and the token only) | [`host`] |
//! | `POST /mcp` | MCP over JSON-RPC | here |
//!
//! Every JSON answer outside the guard's own is the wire convention:
//! `{"ok": true, …, "wire_version": 1}` or `{"ok": false, "code", "message",
//! "wire_version": 1}` at the status `impress_service_core::refusal::http_status`
//! maps the code to.

mod blob;
pub(crate) mod host;
mod layout;
mod secret;
mod surface;
mod ui;

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::Router;
use impress_service_core::call::{self, CallError};
use impress_service_core::dispatch::dispatch_async;
use impress_service_core::pipeline::CallerIdentity;
use impress_service_core::refusal::{codes, http_status};
use impress_service_core::wire::WIRE_VERSION;
use serde_json::{json, Value};

use crate::HostState;

type Shared = Arc<HostState>;

/// Large enough for a PDF; the default 2 MiB stays on every other route.
const BLOB_LIMIT: usize = 256 * 1024 * 1024;

pub(crate) fn router(state: Shared) -> Router {
    Router::new()
        .route("/api/status", get(status))
        .route("/api/skill", get(skill))
        .route("/api/surface", any(surface::handle))
        .route("/api/surface/{*rest}", any(surface::handle))
        .route("/api/layout/tree", get(layout::tree))
        .route("/api/layout/layouts", get(layout::layouts))
        .route("/api/layout/verb", post(layout::verb))
        .route("/api/layout/op", post(layout::op))
        .route("/api/verb/{name}", post(verb))
        .route("/api/verb/{name}/surface", get(verb_surface))
        .route(
            "/api/blob/{item}/{name}",
            get(blob::get)
                .put(blob::put)
                .delete(blob::delete)
                .layer(DefaultBodyLimit::max(BLOB_LIMIT)),
        )
        .route("/api/secret", get(secret::list))
        .route(
            "/api/secret/{name}",
            axum::routing::put(secret::put).delete(secret::delete),
        )
        .route(
            "/api/host/network",
            get(host::network_state).post(host::network_set),
        )
        .route("/api/job", get(job_list))
        .route("/api/job/{id}", get(job_status))
        .route("/api/job/{id}/lines", get(job_lines))
        .route("/api/job/{id}/cancel", post(job_cancel))
        .route("/mcp", post(mcp))
        .route("/ui", get(ui::serve))
        .route("/ui/", get(ui::serve))
        .route("/ui/{*path}", get(ui::serve))
        .fallback(not_found)
        .with_state(state)
}

// ---------------------------------------------------------------- helpers

/// A JSON body at `status`.
pub(super) fn json_response(status: u16, body: &Value) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (
        status,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        body.to_string(),
    )
        .into_response()
}

/// The wire convention's refusal body.
pub(super) fn refusal_body(code: &str, message: impl Into<String>) -> Value {
    json!({
        "ok": false,
        "wire_version": WIRE_VERSION,
        "code": code,
        "message": message.into(),
    })
}

/// A refusal at the status its code maps to.
pub(super) fn refusal(code: &str, message: impl Into<String>) -> Response {
    json_response(http_status(code), &refusal_body(code, message))
}

/// The status an answer envelope maps to: 200 when `ok` (or when it says
/// nothing about `ok`), else the status of its `code`, 422 when it has none.
pub(super) fn status_of(answer: &Value) -> u16 {
    match (
        answer.get("ok").and_then(Value::as_bool),
        answer.get("code").and_then(Value::as_str),
    ) {
        (Some(false), Some(code)) => http_status(code),
        (Some(false), None) => 422,
        _ => 200,
    }
}

/// Run one linked verb by its tool name as the HTTP caller, through the
/// pipeline, and answer its envelope at its status. A verb that is not
/// linked, a host that is not running and a handler that failed are the
/// codes `dispatch` gives them.
pub(super) async fn call_verb(name: &str, args: Value) -> (u16, Value) {
    match call::call_async_as(name, CallerIdentity::agent("http"), args).await {
        Ok(answer) => (status_of(&answer), answer),
        Err(CallError::UnknownTool(_)) => (
            500,
            refusal_body(
                codes::INTERNAL,
                format!("no verb {name}: is its crate linked?"),
            ),
        ),
        Err(CallError::Unavailable(message)) => {
            (503, refusal_body(codes::HOST_UNAVAILABLE, message))
        }
        Err(CallError::Handler(message)) => (502, refusal_body(codes::VERB_FAILED, message)),
    }
}

// ----------------------------------------------------------------- routes

async fn not_found() -> Response {
    refusal(
        codes::NOT_FOUND,
        "no such route (the host's routes are listed in koine_host::routes)",
    )
}

async fn status(State(state): State<Shared>) -> Response {
    let rows = impress_service_core::secrets::status();
    let declared = rows.len();
    let missing: Vec<&str> = rows
        .iter()
        .filter(|row| !row.present)
        .map(|row| row.name.as_str())
        .collect();
    json_response(
        200,
        &json!({
            "app": state.app_id,
            "port": state.port,
            "device": state.device,
            "store": {
                "url": state.store_url,
                "capabilities": state.capabilities,
            },
            "verbs": call::descriptors().count(),
            "jobs_running": state.runner.running(),
            "secrets": {
                "declared": declared,
                "missing": missing,
            },
            "call_log": {
                "written": koine_notebook::installed_sink()
                    .map(|sink| sink.stats().written.load(std::sync::atomic::Ordering::Relaxed)),
                "dropped": impress_service_core::pipeline::audit::dropped(),
                "run": impress_service_core::pipeline::audit::current_run().map(|run| run.id),
            },
            "wire_version": WIRE_VERSION,
        }),
    )
}

/// `GET /api/skill`: this app's skill as Markdown (plan-koine-research-os
/// S-D2), generated from the linked inventory.
async fn skill() -> Response {
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/markdown; charset=utf-8"),
        )],
        koine_agent::skill::render(&koine_agent::app()),
    )
        .into_response()
}

/// `POST /api/verb/<name>`: the body is the verb's argument object (empty is
/// `{}`), the caller is the HTTP agent, and the answer is `dispatch`'s
/// status and body — 404 for a verb that is not linked, 400 for a body that
/// is not JSON, the code's status for a refusal.
async fn verb(Path(name): Path<String>, body: Bytes) -> Response {
    let Ok(args) = std::str::from_utf8(&body) else {
        return refusal(codes::INVALID_ARGUMENT, "the request body is not UTF-8");
    };
    let result = dispatch_async(&name, args, r#"{"kind":"agent","name":"http"}"#).await;
    (
        StatusCode::from_u16(result.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        result.body_json,
    )
        .into_response()
}

/// `GET /api/verb/<name>/surface`: the generated form for one linked verb
/// (`impress_verb_surface::verb_surface_handle`): a field per argument, a Run
/// button and a result view. `POST` it to `/api/surface` to create the
/// surface, then `render`, `dispatch` and `state` it like any other. 404 for
/// a verb that is not linked (or whose name is an alias, which this answers
/// under its canonical name).
async fn verb_surface(Path(name): Path<String>) -> Response {
    match call::find(&name) {
        Some(handle) => json_response(
            200,
            &json!({
                "ok": true,
                "wire_version": WIRE_VERSION,
                "verb": handle.name(),
                "spec": impress_verb_surface::verb_surface_handle(&handle),
            }),
        ),
        None => refusal(
            codes::NOT_FOUND,
            format!(
                "no verb {name}: is its crate linked? (GET /api/status counts the linked verbs)"
            ),
        ),
    }
}

fn job_id(raw: &str) -> Result<impress_item::ItemId, Box<Response>> {
    raw.parse().map_err(|_| {
        Box::new(refusal(
            codes::INVALID_ARGUMENT,
            format!("'{raw}' is not a job id"),
        ))
    })
}

/// `GET /api/job`: every job this process is running and every routine, each
/// as `GET /api/job/<id>` shows it.
async fn job_list(State(state): State<Shared>) -> Response {
    json_response(
        200,
        &json!({
            "ok": true,
            "wire_version": WIRE_VERSION,
            "jobs": state.runner.list(),
        }),
    )
}

async fn job_status(State(state): State<Shared>, Path(id): Path<String>) -> Response {
    let id = match job_id(&id) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    match state.runner.status(id) {
        Some(answer) => json_response(200, &answer),
        None => refusal(codes::NOT_FOUND, format!("no job {id}")),
    }
}

/// `GET /api/job/<id>/lines?after=<n>`: the lines a job has emitted, from
/// index `after` (default 0) of its total. See `JobRunner::lines`.
async fn job_lines(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let id = match job_id(&id) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let after = match query.get("after") {
        None => 0,
        Some(raw) => match raw.parse::<u64>() {
            Ok(n) => n,
            Err(_) => {
                return refusal(
                    codes::INVALID_ARGUMENT,
                    format!("after must be a non-negative integer, got '{raw}'"),
                )
            }
        },
    };
    match state.runner.lines(id, after) {
        Some(answer) => json_response(200, &answer),
        None => refusal(codes::NOT_FOUND, format!("no job {id}")),
    }
}

async fn job_cancel(State(state): State<Shared>, Path(id): Path<String>) -> Response {
    let id = match job_id(&id) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    match state.runner.cancel(id) {
        Ok(stopped) => match state.runner.status(id) {
            Some(mut answer) => {
                answer["cancelled"] = json!(stopped);
                json_response(200, &answer)
            }
            None => refusal(codes::NOT_FOUND, format!("no job {id}")),
        },
        Err(error) => refusal(&error.code, error.message),
    }
}

/// `POST /mcp`: one JSON-RPC message in, one out (`202` for a notification).
/// The verbs run on a blocking thread, as `koine_mcp_host::handle_message`
/// documents.
async fn mcp(State(state): State<Shared>, body: Bytes) -> Response {
    let request: Value = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(error) => {
            return json_response(
                400,
                &json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("Parse error: {error}") },
                }),
            )
        }
    };
    let config = state.mcp.clone();
    let caller = CallerIdentity::agent(format!("mcp-host:{}", config.server_name));
    let answer = tokio::task::spawn_blocking(move || {
        koine_mcp_host::handle_message(&config, &request, &caller)
    })
    .await;
    match answer {
        Ok(Some(response)) => json_response(200, &response),
        Ok(None) => StatusCode::ACCEPTED.into_response(),
        Err(error) => refusal(
            codes::INTERNAL,
            format!("the MCP request task failed: {error}"),
        ),
    }
}
