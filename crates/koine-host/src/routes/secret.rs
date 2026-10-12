//! `GET /api/secret`, `PUT/DELETE /api/secret/<name>`: the secrets linked
//! verbs declare (plan-koine-followups F-D1).
//!
//! Only a name some linked verb declares (`secrets = ["…"]`) can be set or
//! removed — the slot is explicit or it is not a slot. No answer here ever
//! carries a value: the list is `{name, present, source, verbs}`. `PUT` and
//! `DELETE` are non-GET, so the guard already demands the loopback token.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::response::Response;
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
