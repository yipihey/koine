//! `GET /api/secret`, `PUT/DELETE /api/secret/<name>`: the secrets linked
//! verbs declare (plan-koine-followups F-D1).
//!
//! Only a name some linked verb declares (`secrets = ["…"]`) can be set or
//! removed — the slot is explicit or it is not a slot. No answer here ever
//! carries a value: the list is `{name, present, source, verbs}`. `PUT` and
//! `DELETE` are non-GET, so the guard already demands the loopback token.
//!
//! `GET /secret/<name>` is the page an MCP client's URL elicitation opens
//! (plan-koine-trust TR-D5): a form that `PUT`s the value from the person's
//! browser straight to this host, so it never passes through the client or
//! the model. Like `/ui`, only a loopback peer gets the token in the page,
//! so only someone at this machine can set the secret.

use std::net::SocketAddr;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use impress_service_core::refusal::codes;
use impress_service_core::secrets::{self, SecretError};
use impress_service_core::wire::WIRE_VERSION;
use serde_json::{json, Value};

use super::{json_response, refusal, Shared};

/// `GET /api/secret`: every declared secret, whether it is present and which
/// layer answers for it.
pub(super) async fn list() -> Response {
    let rows: Vec<Value> = secrets::status()
        .into_iter()
        .map(|row| {
            json!({
                "name": row.name,
                "present": row.present,
                "source": row.source,
                "verbs": row.verbs,
            })
        })
        .collect();
    json_response(
        200,
        &json!({ "ok": true, "wire_version": WIRE_VERSION, "secrets": rows }),
    )
}

/// The 404 for a name no linked verb declares, listing the ones that are.
fn undeclared(name: &str) -> Response {
    let declared: Vec<&str> = secrets::declared().iter().map(|d| d.name).collect();
    let hint = if declared.is_empty() {
        "no linked verb declares a secret".to_string()
    } else {
        format!("declared: {}", declared.join(", "))
    };
    refusal(
        codes::NOT_FOUND,
        format!("no linked verb declares a secret named `{name}` ({hint})"),
    )
}

fn is_declared(name: &str) -> bool {
    secrets::declared().iter().any(|d| d.name == name)
}

fn secret_error(error: SecretError) -> Response {
    let code = match error {
        SecretError::InvalidName(_) => codes::INVALID_ARGUMENT,
        SecretError::ReadOnly | SecretError::Io(_) => codes::STORE_ERROR,
    };
    refusal(code, error.to_string())
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `GET /secret/<name>`: the page that sets secret `name`.
pub(super) async fn page(
    State(state): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(name): Path<String>,
) -> Response {
    if !is_declared(&name) {
        return undeclared(&name);
    }
    let token = if peer.ip().to_canonical().is_loopback() {
        state.token.as_str()
    } else {
        ""
    };
    let verbs = secrets::declared()
        .into_iter()
        .filter(|d| d.name == name)
        .flat_map(|d| d.verbs)
        .collect::<Vec<_>>()
        .join(", ");
    let html = format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<meta name="koine-token" content="{token}">
<title>Set {name}</title>
<style>body{{font:15px system-ui;max-width:32rem;margin:3rem auto;padding:0 1rem}}
input{{font:inherit;width:100%;padding:.4rem;box-sizing:border-box}}
button{{font:inherit;margin-top:.8rem}}</style></head>
<body><h1>Set the secret <code>{name}</code></h1>
<p>Used by: {verbs}. The value goes from this page to the host on this machine and is not
shown to the agent that asked for it.</p>
<form id="f"><input id="v" type="password" autocomplete="off" autofocus required>
<button type="submit">Save</button></form><p id="s" role="status"></p>
<script>
const token = document.querySelector('meta[name="koine-token"]').content;
const status = document.getElementById("s");
if (!token) status.textContent = "Open this page on the machine the host runs on.";
document.getElementById("f").addEventListener("submit", async (e) => {{
  e.preventDefault();
  const r = await fetch("/api/secret/" + encodeURIComponent("{name}"), {{
    method: "PUT",
    headers: {{"Authorization": "Bearer " + token, "Content-Type": "application/json"}},
    body: JSON.stringify({{value: document.getElementById("v").value}}),
  }});
  document.getElementById("v").value = "";
  status.textContent = r.ok ? "Saved. Go back and retry the call." : "Not saved (" + r.status + ").";
}});
</script></body></html>"#,
        token = escape(token),
        name = escape(&name),
        verbs = escape(&verbs),
    );
    let mut response = html.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// `PUT /api/secret/<name>` with `{"value": "…"}`.
pub(super) async fn put(
    State(state): State<Shared>,
    Path(name): Path<String>,
    body: Bytes,
) -> Response {
    if !is_declared(&name) {
        return undeclared(&name);
    }
    let value =
        match serde_json::from_slice::<Value>(&body) {
            Ok(Value::Object(map)) => match map.get("value") {
                Some(Value::String(value)) if !value.is_empty() => value.clone(),
                _ => return refusal(
                    codes::INVALID_ARGUMENT,
                    "the body is {\"value\": \"<the secret>\"}: `value` must be a non-empty string",
                ),
            },
            _ => {
                return refusal(
                    codes::INVALID_ARGUMENT,
                    "the body is {\"value\": \"<the secret>\"}: a JSON object",
                )
            }
        };
    let source = state.secrets.clone();
    let written = tokio::task::spawn_blocking(move || {
        source.set(&name, &value).map(|()| {
            let label = source.source_of(&name);
            (name, label)
        })
    })
    .await;
    match written {
        Ok(Ok((name, source))) => json_response(
            200,
            &json!({ "ok": true, "wire_version": WIRE_VERSION, "name": name, "source": source }),
        ),
        Ok(Err(error)) => secret_error(error),
        Err(error) => refusal(codes::INTERNAL, format!("the secret task failed: {error}")),
    }
}

/// `DELETE /api/secret/<name>`: `removed` is whether a value was there to
/// remove from the writable layer (a value from the environment stays).
pub(super) async fn delete(State(state): State<Shared>, Path(name): Path<String>) -> Response {
    if !is_declared(&name) {
        return undeclared(&name);
    }
    let source = state.secrets.clone();
    let removed = tokio::task::spawn_blocking(move || {
        let held = source.get(&name).is_some();
        source.remove(&name).map(|()| {
            let removed = held && source.get(&name).is_none();
            (name, removed)
        })
    })
    .await;
    match removed {
        Ok(Ok((name, removed))) => json_response(
            200,
            &json!({ "ok": true, "wire_version": WIRE_VERSION, "name": name, "removed": removed }),
        ),
        Ok(Err(error)) => secret_error(error),
        Err(error) => refusal(codes::INTERNAL, format!("the secret task failed: {error}")),
    }
}
