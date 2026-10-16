//! The P5 transport's client side (plan-verb-pipeline-and-transport.md § P5,
//! ADR-0034 D2, finding TR-1..3).
//!
//! One function — [`call`] — replaces what the retired per-app adapters
//! and app client each did their own
//! way: find the app's port, probe it, attach the loopback token, POST the
//! verb's own JSON body, decode the wire envelope. Every adapter carried its
//! own copy of the probe loop (TR-3); this crate is the one copy. Every
//! adapter swallowed a transport error into an empty/default result;
//! this crate answers a real
//! [`Refusal`] instead, so a dead route reads as `host-unavailable` or
//! `not-found`, never as "no data".
//!
//! The server side is `POST /api/verb/<name>` on `ImpressAutomation`
//! (`packages/ImpressAutomation/Sources/ImpressAutomation/VerbAutomation.swift`),
//! which hands the body to the owning app's `*-verbs-ffi` dispatch — the same
//! pipeline this crate's caller would have gone through had the verb been
//! linked in-process.
//!
//! Entry paths such as impress-mcp, impress-cli, impel-tools and
//! impress-ai-tools install this client transport; app-owned FFI entry points
//! dispatch their own native backends (ADR-0034 D7).

pub mod ports;
pub mod provider;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use impress_service_core::refusal::{codes, Refusal};
use serde_json::Value;

use impress_service_core::pipeline::{self, context, reachability, transport};

/// Install the one app transport for a client process. App-owned FFI entry
/// points use their native backend; they do not install this client router.
pub fn install(store_fallback: bool) {
    provider::install();
    transport::install(Arc::new(AppRouter { store_fallback }));
    reachability::install(reachability::Config {
        probe: Arc::new(|app: &str| cached_reachability(app).unwrap_or(true)),
        store_fallback,
        list_all: std::env::var("IMPRESS_MCP_LIST_ALL").as_deref() == Ok("1"),
    });
}

fn backend_mode(app: &str) -> String {
    std::env::var(format!("{}_BACKEND", app.to_uppercase())).unwrap_or_else(|_| "auto".into())
}

/// Probe an app with the same cache and overrides used by invocation.
pub async fn probe_app(app: &str) -> bool {
    if matches!(backend_mode(app).as_str(), "off" | "sqlite") {
        return false;
    }
    match ports::base_url(app) {
        Some(base) => is_reachable(app, &base).await,
        None => false,
    }
}

/// Startup/FFI helper for callers outside a Tokio runtime.
pub fn probe_app_blocking(app: &str) -> bool {
    impress_service_core::runtime::block_on(probe_app(app))
}

pub fn cached_reachability(app: &str) -> Option<bool> {
    if matches!(backend_mode(app).as_str(), "off" | "sqlite") {
        return Some(false);
    }
    let base = ports::base_url(app)?;
    let cached = states().lock().unwrap();
    let entry = cached.get(&(app.to_string(), base))?;
    (entry.at.elapsed() < REPROBE_COOLDOWN).then_some(entry.verdict == Verdict::Up)
}

struct AppRouter {
    store_fallback: bool,
}

fn refusal_value(refusal: Refusal) -> Value {
    serde_json::json!({
        "ok": false, "code": refusal.code, "message": refusal.message,
        "wire_version": impress_service_core::wire::WIRE_VERSION,
    })
}

impl transport::Router for AppRouter {
    fn route(
        &self,
        verb: &'static impress_service_core::VerbDescriptor,
        args: Value,
    ) -> transport::RouteFuture {
        let call = context::current();
        let store_fallback = self.store_fallback;
        Box::pin(async move {
            let app = reachability::owner_of(verb.name)?;
            // A scenario's explicit scratch store and an app serving its own
            // HTTP request must never be redirected to another process.
            if call.as_ref().is_some_and(|call| {
                call.store_override.is_some()
                    || matches!(&call.caller, pipeline::CallerIdentity::App(owner) if owner == app)
            }) {
                return None;
            }
            // An explicit store selection is authoritative for store-backed
            // verbs. App-only verbs still require their owning process.
            if store_fallback
                && reachability::gated_app(verb.name).is_none()
                && (std::env::var_os("IMPRESS_STORE_PATH").is_some()
                    || std::env::var_os("IMBIB_STORE_PATH").is_some())
            {
                return None;
            }
            if !probe_app(app).await {
                if store_fallback
                    && reachability::gated_app(verb.name).is_none()
                    && backend_mode(app) != "http"
                {
                    return None;
                }
                return Some(Ok(refusal_value(Refusal::new(
                    codes::HOST_UNAVAILABLE,
                    format!("{app} is unavailable for {}", verb.name),
                ))));
            }
            let trace = call.as_ref().map(|call| call.trace_id.as_str());
            Some(Ok(
                match call_with_trace(app, verb.name, args, trace).await {
                    Ok(value) => value,
                    Err(refusal) => refusal_value(refusal),
                },
            ))
        })
    }
}

/// How long a probe itself may take before the app is treated as down.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(1);
/// How long a verb call may take before it is treated as failed. No
/// retries (TR-1's finding: "no retries anywhere" was already the rule
/// every adapter followed; this crate keeps it, deliberately, rather than
/// inventing one four call sites never had).
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// How long an `Unavailable` verdict stands before a call may re-probe —
/// the same cooldown impel-tools already used (`REPROBE_COOLDOWN`,
/// `impel-tools/src/lib.rs`), so a burst of calls against a closed app pays
/// the probe once, not once per caller.
pub const REPROBE_COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Verdict {
    Up,
    Down,
}

struct AppState {
    verdict: Verdict,
    at: Instant,
}

fn states() -> &'static Mutex<HashMap<(String, String), AppState>> {
    static STATES: OnceLock<Mutex<HashMap<(String, String), AppState>>> = OnceLock::new();
    STATES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A loopback-safe client, built once. `no_proxy()` because every request
/// this crate makes is to `127.0.0.1`, and a panicking `.build()` (asking
/// macOS for the system proxy from inside a sandboxed process) would take a
/// whole caller down over a transport detail. Keep this safety in the one
/// shared transport client.
fn client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .no_proxy()
                .timeout(CALL_TIMEOUT)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new())
        })
        .clone()
}

fn probe_client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .no_proxy()
                .timeout(PROBE_TIMEOUT)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new())
        })
        .clone()
}

/// Whether `app` (reachable at `base_url`) answered `GET /api/status`
/// recently enough to trust, re-probing when the cached verdict's cooldown
/// has expired. Exposed so a caller that already knows it wants a fresh
/// probe (impel-tools' "an app that was up and has since quit" case) can
/// call it directly; [`call`] calls it itself.
pub async fn is_reachable(app: &str, base_url: &str) -> bool {
    // Serialize only probes for this endpoint. A burst against a closed app
    // pays one timeout, while unrelated apps can still be probed concurrently.
    type ProbeLocks = Mutex<HashMap<(String, String), Arc<tokio::sync::Mutex<()>>>>;
    static LOCKS: OnceLock<ProbeLocks> = OnceLock::new();
    let probe_lock = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .entry((app.to_string(), base_url.to_string()))
        .or_default()
        .clone();
    let _probe = probe_lock.lock().await;
    {
        let cached = states().lock().unwrap();
        if let Some(state) = cached.get(&(app.to_string(), base_url.to_string())) {
            if state.at.elapsed() < REPROBE_COOLDOWN {
                return state.verdict == Verdict::Up;
            }
        }
    }
    let up = probe(base_url).await;
    record(app, base_url, up);
    up
}

async fn probe(base_url: &str) -> bool {
    let url = format!("{}/api/status", base_url.trim_end_matches('/'));
    probe_client()
        .get(&url)
        .send()
        .await
        .map(|response| response.status().is_success())
        .unwrap_or(false)
}

fn record(app: &str, base_url: &str, up: bool) {
    let mut cached = states().lock().unwrap();
    cached.insert(
        (app.to_string(), base_url.to_string()),
        AppState {
            verdict: if up { Verdict::Up } else { Verdict::Down },
            at: Instant::now(),
        },
    );
}

/// Call `verb` on `app`'s running automation server: `POST
/// /api/verb/<verb>` with `args` as the JSON body, decoding the wire
/// envelope (`{"ok": true, …}` or `{"ok": false, "code", "message"}`) into
/// `Ok`/`Err`. Attaches the P0 loopback token
/// (`impress_service_core::loopback_token::client_token_for_url`) and a fresh
/// `traceparent` so the pipeline on the app side joins this call's trace
/// (hook H-P5-1).
///
/// `app` is looked up in [`ports`]; an app this crate does not know is
/// `not-found`, not a panic. A transport failure (connection refused, a
/// timeout) is `host-unavailable` and also flips this app's cached
/// reachability to down, so subsequent calls refuse during the cooldown
/// rather than retrying the same dead socket (TR-3).
pub async fn call(app: &str, verb: &str, args: Value) -> Result<Value, Refusal> {
    call_with_trace(app, verb, args, None).await
}

/// [`call`], joining `trace_id` instead of generating a fresh one — for a
/// caller that already has a `traceparent` to propagate (a nested verb
/// call, a Tier B scenario runner).
pub async fn call_with_trace(
    app: &str,
    verb: &str,
    args: Value,
    trace_id: Option<&str>,
) -> Result<Value, Refusal> {
    let base_url = ports::base_url(app)
        .ok_or_else(|| Refusal::new(codes::NOT_FOUND, format!("no known app '{app}'")))?;

    if !is_reachable(app, &base_url).await {
        return Err(Refusal::new(
            codes::HOST_UNAVAILABLE,
            format!("{app} is not reachable at {base_url}"),
        ));
    }

    let url = format!("{}/api/verb/{}", base_url.trim_end_matches('/'), verb);
    let mut builder = client().post(&url).json(&args);
    if let Some(token) = impress_service_core::loopback_token::client_token_for_url(&base_url) {
        builder = builder.bearer_auth(token);
    }
    let traceparent = trace_id
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    builder = builder.header("traceparent", traceparent);
    if let Some(call) = context::current() {
        builder = builder.header("x-impress-parent-call", &call.call_id);
    }

    let response = builder.send().await.map_err(|error| {
        record(app, &base_url, false);
        Refusal::new(codes::HOST_UNAVAILABLE, format!("{app}/{verb}: {error}"))
    })?;

    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| Refusal::new(codes::VERB_FAILED, error.to_string()))?;
    let body: Value = serde_json::from_slice(&bytes).map_err(|error| {
        Refusal::new(
            codes::VERB_FAILED,
            format!(
                "{app}/{verb}: response did not parse as JSON: {error} ({})",
                String::from_utf8_lossy(&bytes)
            ),
        )
    })?;

    if status.is_success() {
        Ok(body)
    } else {
        let code = body
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or(codes::VERB_FAILED)
            .to_string();
        let message = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Err(Refusal::new(code, message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reset_state(app: &str) {
        states().lock().unwrap().retain(|(name, _), _| name != app);
    }

    #[tokio::test]
    async fn a_successful_verb_call_decodes_the_ok_envelope() {
        let mut server = mockito::Server::new_async().await;
        let _status = server
            .mock("GET", "/api/status")
            .with_status(200)
            .with_body(r#"{"status":"ok"}"#)
            .create_async()
            .await;
        let _verb = server
            .mock("POST", "/api/verb/implore-service_plot-series")
            .with_status(200)
            .with_body(r#"{"ok":true,"wire_version":1,"svg":"<svg/>"}"#)
            .create_async()
            .await;

        std::env::set_var("IMPRESS_TESTAPP_HTTP_URL", server.url());
        reset_state("testapp");
        let answer = call(
            "testapp",
            "implore-service_plot-series",
            json!({"series": ["a"]}),
        )
        .await
        .expect("the verb should answer ok");
        assert_eq!(answer["ok"], true);
        assert_eq!(answer["svg"], "<svg/>");
        std::env::remove_var("IMPRESS_TESTAPP_HTTP_URL");
    }

    #[tokio::test]
    async fn a_refusal_envelope_is_an_err_not_swallowed_into_a_default() {
        let mut server = mockito::Server::new_async().await;
        let _status = server
            .mock("GET", "/api/status")
            .with_status(200)
            .with_body(r#"{"status":"ok"}"#)
            .create_async()
            .await;
        let _verb = server
            .mock("POST", "/api/verb/implore-service_rg-statistics")
            .with_status(404)
            .with_body(
                r#"{"ok":false,"wire_version":1,"code":"not-found","message":"no such dataset"}"#,
            )
            .create_async()
            .await;

        std::env::set_var("IMPRESS_TESTAPP2_HTTP_URL", server.url());
        reset_state("testapp2");
        let error = call(
            "testapp2",
            "implore-service_rg-statistics",
            json!({"params_json": null}),
        )
        .await
        .expect_err("a 404 refusal should be an Err, not an empty Ok");
        assert_eq!(error.code, "not-found");
        assert_eq!(error.message, "no such dataset");
        std::env::remove_var("IMPRESS_TESTAPP2_HTTP_URL");
    }

    #[tokio::test]
    async fn an_unreachable_app_is_host_unavailable_without_a_call_attempt() {
        reset_state("unreachable-app");
        std::env::set_var(
            "IMPRESS_UNREACHABLE_APP_HTTP_URL",
            "http://127.0.0.1:1", // nothing listens here
        );
        let error = call("unreachable-app", "some-service_verb", json!({}))
            .await
            .expect_err("nothing is listening, so this must refuse");
        assert_eq!(error.code, "host-unavailable");
        std::env::remove_var("IMPRESS_UNREACHABLE_APP_HTTP_URL");
    }

    #[tokio::test]
    async fn an_unknown_app_is_not_found() {
        let error = call("no-such-app", "x_y", json!({}))
            .await
            .expect_err("an app not in the port table must not panic");
        assert_eq!(error.code, "not-found");
    }

    #[tokio::test]
    async fn a_second_call_within_the_cooldown_reuses_the_cached_down_verdict() {
        reset_state("cooldown-app");
        std::env::set_var("IMPRESS_COOLDOWN_APP_HTTP_URL", "http://127.0.0.1:1");
        let first = std::time::Instant::now();
        let _ = call("cooldown-app", "x_y", json!({})).await;
        let second_start = std::time::Instant::now();
        let _ = call("cooldown-app", "x_y", json!({})).await;
        // The second call's own reachability check must not re-probe (no
        // network attempt on a cached-down verdict inside the cooldown):
        // proven indirectly by it returning fast, well under the 1s probe
        // timeout that a fresh probe of a black-hole address would pay.
        assert!(second_start.elapsed() < Duration::from_millis(500));
        assert!(first.elapsed() < Duration::from_secs(2));
        std::env::remove_var("IMPRESS_COOLDOWN_APP_HTTP_URL");
    }
}
