//! `/api/surface`, `/api/surface/*`: the surface route table, run.
//!
//! The table is `impress_surface_service::http::route` (path and method to
//! verb, id and arguments), shared with the Swift FFI. This module is the
//! rest of what the FFI's `SharedSurface::route` does, for a host with no
//! window: the query string becomes arguments, the id and body join them,
//! this host's device fills `host` / `app_id` / `device`, the verb runs by
//! name through the pipeline as the HTTP agent, and the status is 200 when
//! the answer is `ok`, else the status its `code` maps to.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{Method, Uri};
use axum::response::Response;
use impress_service_core::refusal::Refusal;
use serde_json::{Map, Value};

use super::{call_verb, json_response, refusal_body, Shared};

/// `surface_show` and friends are inventory tools of this service.
const SERVICE: &str = "impress-surface-service";

pub(super) async fn handle(
    State(state): State<Shared>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> Response {
    let (status, answer) = route(&state, method.as_str(), &uri, &body).await;
    json_response(status, &answer)
}

async fn route(state: &Shared, method: &str, uri: &Uri, body: &[u8]) -> (u16, Value) {
    let segments: Vec<&str> = uri
        .path()
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let rest = &segments[segments.len().min(2)..];
    let body: Option<Value> = if body.iter().all(u8::is_ascii_whitespace) {
        None
    } else {
        match serde_json::from_slice(body) {
            Ok(value) => Some(value),
            Err(e) => {
                return fail(Refusal::invalid_argument(format!(
                    "the request body is not JSON: {e}"
                )))
            }
        }
    };
    let Some((verb, id, body_args)) = impress_surface_service::http::route(method, rest, body)
    else {
        return fail(Refusal::not_found(format!(
            "no route {method} {} (docs/agent-surfaces.md lists them)",
            uri.path()
        )));
    };
    let mut args = match query_args(uri.query().unwrap_or("")) {
        Ok(args) => args,
        Err(refusal) => return fail(refusal),
    };
    if let Some(id) = id {
        args.insert("id".into(), Value::String(id));
    }
    match body_args {
        Some(Value::Object(map)) => {
            for (key, value) in map {
                if args.contains_key(&key) {
                    return fail(Refusal::invalid_argument(format!(
                        "`{key}` is given twice (path or query, and body)"
                    )));
                }
                args.insert(key, value);
            }
        }
        Some(other) => {
            return fail(Refusal::invalid_argument(format!(
                "the request body must be a JSON object, got {other}"
            )))
        }
        None => {}
    }
    // This host's own instance unless the caller names another: its device
    // (the state instance its panes use), and for `show` its app and device
    // (the window this process draws).
    let mut fill = |key: &str, value: &str| {
        if !value.is_empty() && !args.contains_key(key) {
            args.insert(key.into(), Value::String(value.to_string()));
        }
    };
    match verb {
        "surface_show" => {
            fill("app_id", &state.app_id);
            fill("device", &state.device);
        }
        "surface_render" | "surface_state_get" | "surface_state_set" | "surface_dispatch"
        | "surface_events" | "surface_wait" => fill("host", &state.device),
        _ => {}
    }
    let tool = format!("{SERVICE}_{}", verb.replace('_', "-"));
    let (status, answer) = call_verb(&tool, Value::Object(args)).await;
    if status != 200 {
        let code = answer.get("code").and_then(Value::as_str).unwrap_or("?");
        let message = answer.get("message").and_then(Value::as_str).unwrap_or("");
        tracing::info!(
            target: "surface",
            "http {method} {} → {status} [{code}]: {message}",
            uri.path(),
        );
    }
    (status, answer)
}

fn fail(refusal: Refusal) -> (u16, Value) {
    (
        refusal.http_status(),
        refusal_body(&refusal.code, refusal.message),
    )
}

/// A query string as verb arguments: every key is an argument name (the verb
/// refuses one it does not take), `after_seq`/`timeout_ms`/`expected_revision`
/// are numbers, `params` is a JSON object, and anything else is a string.
/// Values are percent-decoded. (The Swift FFI keeps its own copy of this
/// beside its `route`; the two answer the same.)
fn query_args(query: &str) -> Result<Map<String, Value>, Refusal> {
    let mut args = Map::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, raw) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode(key);
        let raw = percent_decode(raw);
        let value = match key.as_str() {
            "after_seq" | "timeout_ms" | "expected_revision" => raw
                .parse::<u64>()
                .map(Value::from)
                .map_err(|_| Refusal::invalid_argument(format!("?{key}={raw} is not a number")))?,
            "params" => serde_json::from_str(&raw).map_err(|e| {
                Refusal::invalid_argument(format!("?params= must be a JSON object: {e}"))
            })?,
            _ => Value::String(raw),
        };
        if args.insert(key.clone(), value).is_some() {
            return Err(Refusal::invalid_argument(format!("?{key} is given twice")));
        }
    }
    Ok(args)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(b) => {
                    out.push(b);
                    i += 3;
                    continue;
                }
                Err(_) => out.push(b'%'),
            },
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_arguments_are_typed_by_name_and_decoded() {
        let args = query_args("after_seq=12&timeout_ms=25000&host=a%20b&params=%7B%22x%22%3A1%7D")
            .unwrap();
        assert_eq!(args["after_seq"], 12);
        assert_eq!(args["timeout_ms"], 25000);
        assert_eq!(args["host"], "a b");
        assert_eq!(args["params"]["x"], 1);
        assert!(query_args("after_seq=x").is_err());
        assert!(query_args("host=a&host=b").is_err());
        assert!(query_args("params=nope").is_err());
        assert!(query_args("").unwrap().is_empty());
    }
}
