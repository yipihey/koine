//! The checks every impress automation server applies before any route
//! (docs/chassis-capability-matrix.md § Who may call, in impress-apps):
//!
//! | Check | Rule | Refusal |
//! |---|---|---|
//! | Host | `Host` is `localhost`, `127.0.0.1`, `[::1]` (any port) or the bound address | 400 |
//! | Loopback token | a loopback peer's `GET`/`HEAD`/`OPTIONS` needs nothing; every other method needs `Authorization: Bearer <this launch's token>` | 401 |
//! | Network bearer | a non-loopback peer needs network mode and the network bearer, on every method | 401 |
//!
//! And no response carries an `Access-Control-*` header: a cross-origin
//! preflight (`OPTIONS`) is answered with a bare 204 that grants nothing, so
//! it fails in the browser. If a browser origin is ever a real client it gets
//! an allow-list plus a custom request header, never a wildcard.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::HostState;

/// The middleware: Host, then the token or bearer, then the route, then a
/// scrub of anything CORS-shaped.
pub(crate) async fn guard(
    State(state): State<Arc<HostState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let loopback = peer.ip().to_canonical().is_loopback();
    let (allow_network, network_bearer, opened_at_runtime) = {
        let network = state.network.read().unwrap_or_else(|e| e.into_inner());
        (
            network.allow,
            network.bearer.clone(),
            network.listener.is_some(),
        )
    };
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    if !host_admitted(host, state.bind, loopback, opened_at_runtime) {
        return refusal(
            StatusCode::BAD_REQUEST,
            "Host header does not name this server",
        );
    }
    let bearer = bearer(request.headers());
    let admitted = admitted(
        loopback,
        request.method(),
        bearer,
        &state.token,
        allow_network,
        network_bearer.as_deref(),
    );
    if !admitted {
        let mut response = refusal(StatusCode::UNAUTHORIZED, "unauthorized");
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            header::HeaderValue::from_static("Bearer"),
        );
        return response;
    }
    if request.method() == Method::OPTIONS {
        return StatusCode::NO_CONTENT.into_response();
    }
    let mut response = next.run(request).await;
    let cors: Vec<_> = response
        .headers()
        .keys()
        .filter(|name| name.as_str().starts_with("access-control-"))
        .cloned()
        .collect();
    for name in cors {
        response.headers_mut().remove(name);
    }
    response
}

/// The Host check. The wildcard listener `POST /api/host/network` opens has
/// no one address to name, so a remote peer there may use any IP literal or
/// `.local` name; it still needs the bearer, which is what a DNS-rebinding
/// page cannot present. Loopback peers keep the strict rule.
fn host_admitted(
    host: Option<&str>,
    bound: IpAddr,
    loopback: bool,
    opened_at_runtime: bool,
) -> bool {
    let Some(host) = host else { return false };
    if host_names_this_machine(host, bound) {
        return true;
    }
    opened_at_runtime && !loopback && host_is_lan_name(host)
}

/// An IP literal or an mDNS `.local` name, with or without a port.
fn host_is_lan_name(host: &str) -> bool {
    let host = host.trim().to_ascii_lowercase();
    let name = if host.starts_with('[') {
        return host
            .find(']')
            .is_some_and(|end| host[1..end].parse::<std::net::Ipv6Addr>().is_ok());
    } else {
        match host.rsplit_once(':') {
            Some((name, port)) if port.chars().all(|c| c.is_ascii_digit()) => name,
            _ => host.as_str(),
        }
    };
    name.parse::<std::net::Ipv4Addr>().is_ok()
        || name
            .strip_suffix(".local")
            .is_some_and(|label| !label.is_empty() && !label.contains(".."))
}

fn refusal(status: StatusCode, reason: &str) -> Response {
    (
        status,
        axum::Json(json!({ "status": "error", "reason": reason })),
    )
        .into_response()
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

/// Whether the request may proceed. Pure, so the policy is testable with a
/// peer address no test machine can produce.
fn admitted(
    loopback: bool,
    method: &Method,
    bearer: Option<&str>,
    token: &str,
    allow_network: bool,
    network_bearer: Option<&str>,
) -> bool {
    if loopback {
        let read_only = matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
        return read_only || bearer.is_some_and(|presented| same(presented, token));
    }
    allow_network
        && match (network_bearer, bearer) {
            (Some(expected), Some(presented)) => !expected.is_empty() && same(presented, expected),
            _ => false,
        }
}

/// Equality that does not stop at the first differing byte.
fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut difference = (a.len() != b.len()) as u8;
    for (x, y) in a.iter().zip(b.iter()) {
        difference |= x ^ y;
    }
    difference == 0
}

/// `Host` is `localhost`, `127.0.0.1` or `[::1]` (with or without a port), or
/// the address this host is bound to.
fn host_names_this_machine(host: &str, bound: IpAddr) -> bool {
    let host = host.trim().to_ascii_lowercase();
    let name = if host.starts_with('[') {
        match host.find(']') {
            Some(end) => &host[..=end],
            None => return false,
        }
    } else {
        match host.rsplit_once(':') {
            Some((name, port)) if port.chars().all(|c| c.is_ascii_digit()) => name,
            _ => host.as_str(),
        }
    };
    if matches!(name, "localhost" | "127.0.0.1" | "[::1]") {
        return true;
    }
    if bound.is_unspecified() {
        return false;
    }
    match bound {
        IpAddr::V4(v4) => name == v4.to_string(),
        IpAddr::V6(v6) => name == format!("[{v6}]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

    #[test]
    fn host_names_this_machine_with_or_without_a_port() {
        for ok in [
            "localhost",
            "localhost:23130",
            "LOCALHOST:1",
            "127.0.0.1:80",
            "[::1]",
            "[::1]:23130",
        ] {
            assert!(host_names_this_machine(ok, LOCAL), "{ok}");
        }
        for bad in [
            "evil.example",
            "evil.example:23130",
            "localhost.evil.example",
            "127.0.0.1.evil.example",
            "",
            "[::1",
            "10.0.0.5",
        ] {
            assert!(!host_names_this_machine(bad, LOCAL), "{bad}");
        }
    }

    #[test]
    fn the_bound_address_is_named_too() {
        let lan = IpAddr::from([192, 168, 1, 5]);
        assert!(host_names_this_machine("192.168.1.5:23130", lan));
        assert!(!host_names_this_machine("192.168.1.6", lan));
        assert!(!host_names_this_machine(
            "0.0.0.0",
            IpAddr::from([0, 0, 0, 0])
        ));
    }

    #[test]
    fn a_wildcard_listener_admits_lan_names_for_remote_peers_only() {
        let lan = "192.168.1.9:23130";
        // Only the listener opened at runtime, and only for a remote peer.
        assert!(host_admitted(Some(lan), LOCAL, false, true));
        assert!(host_admitted(
            Some("iphone.local:23130"),
            LOCAL,
            false,
            true
        ));
        assert!(host_admitted(Some("[fe80::1]:23130"), LOCAL, false, true));
        assert!(!host_admitted(Some(lan), LOCAL, false, false));
        assert!(!host_admitted(Some(lan), LOCAL, true, true));
        // A hostname that is not on the LAN is still a rebinding candidate.
        assert!(!host_admitted(Some("evil.example"), LOCAL, false, true));
        assert!(!host_admitted(
            Some("localhost.evil.example"),
            LOCAL,
            false,
            true
        ));
        assert!(!host_admitted(Some(".local"), LOCAL, false, true));
        assert!(!host_admitted(None, LOCAL, false, true));
        // Loopback names pass everywhere.
        assert!(host_admitted(Some("localhost:1"), LOCAL, true, false));
    }

    #[test]
    fn loopback_reads_are_open_and_everything_else_needs_the_token() {
        let get = Method::GET;
        let post = Method::POST;
        assert!(admitted(true, &get, None, "t", false, None));
        assert!(admitted(true, &Method::OPTIONS, None, "t", false, None));
        assert!(!admitted(true, &post, None, "t", false, None));
        assert!(!admitted(true, &post, Some("wrong"), "t", false, None));
        assert!(admitted(true, &post, Some("t"), "t", false, None));
        assert!(!admitted(
            true,
            &Method::DELETE,
            Some("t "),
            "t",
            false,
            None
        ));
    }

    #[test]
    fn a_remote_peer_needs_network_mode_and_the_network_bearer_even_to_read() {
        let get = Method::GET;
        // Network mode off: nothing gets in, whatever it presents.
        assert!(!admitted(false, &get, Some("t"), "t", false, Some("n")));
        assert!(!admitted(false, &get, Some("n"), "t", false, Some("n")));
        // On: the network bearer, not the loopback token.
        assert!(admitted(false, &get, Some("n"), "t", true, Some("n")));
        assert!(!admitted(false, &get, Some("t"), "t", true, Some("n")));
        assert!(!admitted(false, &get, None, "t", true, Some("n")));
        assert!(!admitted(false, &Method::POST, None, "t", true, Some("n")));
        // An empty configured bearer admits nobody.
        assert!(!admitted(false, &get, Some(""), "t", true, Some("")));
    }
}
