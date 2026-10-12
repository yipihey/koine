//! `GET/POST /api/host/network`: the network toggle (plan-koine-mobile M-D4).
//!
//! The host binds one address at start. A host that started on loopback (an
//! app on a phone) opens a second listener on the same routes, behind the same
//! guard, with `POST {"allow": true, "bearer": "<optional>"}`: `0.0.0.0` on
//! the host's own port, or any port when that one is taken. The guard reads
//! one [`crate::NetworkAccess`] for both listeners, so a non-loopback peer
//! presents the network bearer on every request, whichever listener it came
//! through. `{"allow": false}` aborts the listener and clears the access; a
//! connection that outlives the abort is refused by the guard on its next
//! request.
//!
//! The POST is loopback-only on top of the guard's token rule: a remote peer
//! holding the network bearer can use the verbs, but cannot rotate or close
//! the door it came through.
//!
//! A host that started in network mode (`Config::allow_network`) is already
//! the thing this opens, bound to its one concrete address: both directions
//! refuse `already-network`.

use std::net::{IpAddr, SocketAddr};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::response::Response;
use impress_service_core::loopback_token;
use impress_service_core::refusal::codes;
use impress_service_core::wire::WIRE_VERSION;
use serde_json::{json, Value};
use tokio::task::JoinHandle;

use super::{json_response, refusal, refusal_body, Shared};

/// The wire code for a host that started in network mode.
pub(crate) const ALREADY_NETWORK: &str = "already-network";
/// The wire code for a caller that is not on loopback.
pub(crate) const LOOPBACK_ONLY: &str = "loopback-only";

/// The second listener: where it is bound and the task serving it.
pub(crate) struct NetworkListener {
    pub addr: SocketAddr,
    pub task: JoinHandle<()>,
}

/// `GET /api/host/network`: `{ok, wire_version, allowed, addr?, port?,
/// started_in_network_mode}`. Never the bearer.
pub(super) async fn network_state(State(state): State<Shared>) -> Response {
    let network = state.network.read().unwrap_or_else(|e| e.into_inner());
    let mut body = json!({
        "ok": true,
        "wire_version": WIRE_VERSION,
        "allowed": network.allow,
        "started_in_network_mode": network.started_in_network_mode,
    });
    if let Some(listener) = &network.listener {
        body["addr"] = json!(listener.addr.to_string());
        body["port"] = json!(listener.addr.port());
    }
    json_response(200, &body)
}

/// `POST /api/host/network` with `{"allow": bool, "bearer": "<optional>"}`.
pub(super) async fn network_set(
    State(state): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    body: Bytes,
) -> Response {
    if !peer.ip().to_canonical().is_loopback() {
        return json_response(
            403,
            &refusal_body(
                LOOPBACK_ONLY,
                "the network toggle is for the machine the host runs on",
            ),
        );
    }
    let (allow, bearer) = match parse(&body) {
        Ok(parsed) => parsed,
        Err(message) => return refusal(codes::INVALID_ARGUMENT, message),
    };
    if state
        .network
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .started_in_network_mode
    {
        return json_response(
            409,
            &refusal_body(
                ALREADY_NETWORK,
                "this host started in network mode: it is bound to its one network address \
                 for its whole life (restart it without --allow-network to change that)",
            ),
        );
    }
    if !allow {
        state.close_network();
        return json_response(
            200,
            &json!({ "ok": true, "wire_version": WIRE_VERSION, "allowed": false }),
        );
    }

    // Idempotent: an open listener keeps its address; a bearer in the body
    // rotates the bearer.
    if let Some(answer) = reuse(&state, bearer.as_deref()) {
        return json_response(200, &answer);
    }
    let listener = match bind(state.port).await {
        Ok(listener) => listener,
        Err(error) => {
            return refusal(
                codes::INTERNAL,
                format!("cannot open a network listener: {error}"),
            )
        }
    };
    let addr = match listener.local_addr() {
        Ok(addr) => addr,
        Err(error) => return refusal(codes::INTERNAL, format!("network listener: {error}")),
    };
    let app = crate::app(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
    let task = tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app).await {
            tracing::error!(target: "koine-host", "network listener stopped: {error}");
        }
    });
    let bearer = bearer.unwrap_or_else(loopback_token::generate);
    {
        let mut network = state.network.write().unwrap_or_else(|e| e.into_inner());
        if network.listener.is_some() {
            // Another request opened one while this one was binding.
            task.abort();
            drop(network);
            if let Some(answer) = reuse(&state, Some(&bearer)) {
                return json_response(200, &answer);
            }
            return refusal(codes::CONFLICT, "the network listener changed; retry");
        }
        network.allow = true;
        network.bearer = Some(bearer.clone());
        network.listener = Some(NetworkListener { addr, task });
    }
    tracing::info!(target: "koine-host", "network access on at {addr}");
    json_response(200, &opened(addr, &bearer))
}

/// The state answer for an open listener, with its bearer rotated when one is
/// given; `None` when there is no listener.
fn reuse(state: &Shared, rotate_to: Option<&str>) -> Option<Value> {
    let mut network = state.network.write().unwrap_or_else(|e| e.into_inner());
    let addr = network.listener.as_ref()?.addr;
    if let Some(bearer) = rotate_to {
        network.bearer = Some(bearer.to_string());
    }
    let bearer = network.bearer.clone().unwrap_or_default();
    Some(opened(addr, &bearer))
}

fn opened(addr: SocketAddr, bearer: &str) -> Value {
    json!({
        "ok": true,
        "wire_version": WIRE_VERSION,
        "allowed": true,
        "addr": addr.to_string(),
        "port": addr.port(),
        "bearer": bearer,
    })
}

/// `0.0.0.0:<port>`, or any port when that one is taken (the loopback
/// listener may already hold it on a platform that does not let both bind).
async fn bind(port: u16) -> std::io::Result<tokio::net::TcpListener> {
    let any = IpAddr::from([0, 0, 0, 0]);
    match tokio::net::TcpListener::bind(SocketAddr::new(any, port)).await {
        Ok(listener) => Ok(listener),
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            tokio::net::TcpListener::bind(SocketAddr::new(any, 0)).await
        }
        Err(error) => Err(error),
    }
}

/// `{"allow": bool, "bearer": string?}`, nothing else.
fn parse(body: &[u8]) -> Result<(bool, Option<String>), String> {
    const SHAPE: &str = "the body is {\"allow\": true|false, \"bearer\": \"<optional>\"}";
    let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(body) else {
        return Err(format!("{SHAPE}: a JSON object"));
    };
    if let Some(unknown) = map
        .keys()
        .find(|k| !matches!(k.as_str(), "allow" | "bearer"))
    {
        return Err(format!("{SHAPE}: unknown field `{unknown}`"));
    }
    let Some(Value::Bool(allow)) = map.get("allow") else {
        return Err(format!("{SHAPE}: `allow` must be a boolean"));
    };
    let bearer = match map.get("bearer") {
        None | Some(Value::Null) => None,
        Some(Value::String(bearer)) if !bearer.trim().is_empty() => Some(bearer.clone()),
        Some(_) => return Err(format!("{SHAPE}: `bearer` must be a non-empty string")),
    };
    if !allow && bearer.is_some() {
        return Err(format!("{SHAPE}: `bearer` goes with `allow: true`"));
    }
    Ok((*allow, bearer))
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn the_body_is_allow_and_an_optional_bearer() {
        assert_eq!(parse(br#"{"allow":true}"#), Ok((true, None)));
        assert_eq!(
            parse(br#"{"allow":true,"bearer":"b"}"#),
            Ok((true, Some("b".into())))
        );
        assert_eq!(parse(br#"{"allow":false}"#), Ok((false, None)));
        for bad in [
            &br#"{}"#[..],
            br#"{"allow":"yes"}"#,
            br#"{"allow":true,"bearer":""}"#,
            br#"{"allow":true,"bearer":5}"#,
            br#"{"allow":false,"bearer":"b"}"#,
            br#"{"allow":true,"port":1}"#,
            b"[]",
            b"",
        ] {
            assert!(parse(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
    }
}
