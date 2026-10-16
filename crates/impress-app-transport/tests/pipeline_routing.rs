//! Client routing keeps refusal, identity and store-selection semantics intact.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use impress_service_core::pipeline::{self, Call, CallerIdentity};
use impress_service_core::{Effects, Safety, SafetyClass, ServiceFuture, Source, VerbDescriptor};
use serde_json::{json, Value};

static LOCAL_CALLS: AtomicUsize = AtomicUsize::new(0);
fn schema() -> Value {
    json!({"type":"object", "properties":{"n":{"type":"integer"}}})
}
fn local(_: Value) -> ServiceFuture {
    Box::pin(async {
        LOCAL_CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(
            json!({"ok":true,"local":true,"store":pipeline::context::store_override::<u64>().map(|n| *n)}),
        )
    })
}
static APP: VerbDescriptor = VerbDescriptor {
    name: "implore-service_routing-proof",
    service: "implore-service",
    method: "routing-proof",
    description: "Routing proof",
    input_schema: schema,
    output_schema: schema,
    safety: Safety {
        class: SafetyClass::ReadOnly,
        idempotent: true,
    },
    effects: Effects::NONE,
    since: "0.1.0",
    deprecated: None,
    aliases: &[],
    supports: &[],
    adjective: None,
    examples: &[],
    strict: true,
    budget_ms: None,
    replay_full: false,
    returns_kind: None,
    rows: None,
    secrets: &[],
    source: Source::Linked,
    handler: local,
};
static STORE: VerbDescriptor = VerbDescriptor {
    name: "imbib-library-service_routing-proof",
    service: "imbib-library-service",
    ..APP
};

// One test owns all process-global URL/backend variables and router installation.
#[tokio::test]
async fn pipeline_routes_once_preserves_refusals_and_keeps_owned_store_calls_local() {
    let mut server = mockito::Server::new_async().await;
    std::env::set_var("IMPRESS_IMPLORE_HTTP_URL", server.url());
    std::env::set_var("IMPRESS_IMBIB_HTTP_URL", server.url());
    let status = server
        .mock("GET", "/api/status")
        .with_status(200)
        .expect(1)
        .create_async()
        .await;
    impress_app_transport::install(true);

    let success = server
        .mock("POST", "/api/verb/implore-service_routing-proof")
        .match_body(mockito::Matcher::Json(json!({"n":17})))
        .match_header("traceparent", "owned-trace")
        .match_header(
            "x-impress-parent-call",
            mockito::Matcher::Regex("^[a-f0-9-]{36}$".into()),
        )
        .with_status(200)
        .with_body(r#"{"ok":true,"value":17}"#)
        .expect(1)
        .create_async()
        .await;
    let mut call = Call::person(json!({"n":17}));
    call.trace_id = Some("owned-trace".into());
    let value = pipeline::invoke(&APP, call).await.unwrap();
    assert_eq!(value["value"], 17);
    assert_eq!(LOCAL_CALLS.load(Ordering::SeqCst), 0);
    success.assert_async().await;
    success.remove_async().await;

    let refusal = server
        .mock("POST", "/api/verb/implore-service_routing-proof")
        .with_status(404)
        .with_body(
            r#"{"ok":false,"code":"not-found","message":"owned dataset missing","wire_version":1}"#,
        )
        .expect(1)
        .create_async()
        .await;
    let value = pipeline::invoke(&APP, Call::person(json!({})))
        .await
        .unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["code"], "not-found");
    assert_eq!(value["message"], "owned dataset missing");
    assert_eq!(
        LOCAL_CALLS.load(Ordering::SeqCst),
        0,
        "no fallback after a remote refusal"
    );
    refusal.assert_async().await;
    refusal.remove_async().await;

    let mut native = Call::person(json!({}));
    native.caller = CallerIdentity::App("implore".into());
    assert_eq!(pipeline::invoke(&APP, native).await.unwrap()["local"], true);
    assert_eq!(
        pipeline::invoke_on(Arc::new(73u64), &APP, Call::person(json!({})))
            .await
            .unwrap()["store"],
        73
    );

    // A selected scratch database is authoritative even if the owning app is up.
    std::env::set_var(
        "IMPRESS_STORE_PATH",
        "/tmp/p5b-routing-proof-not-opened.sqlite",
    );
    assert_eq!(
        pipeline::invoke(&STORE, Call::person(json!({})))
            .await
            .unwrap()["local"],
        true
    );
    status.assert_async().await;
    assert_eq!(LOCAL_CALLS.load(Ordering::SeqCst), 3);

    // The impel client has no local fallback, even when a store is selected.
    impress_app_transport::install(false);
    std::env::set_var("IMBIB_BACKEND", "off");
    let down = pipeline::invoke(&STORE, Call::person(json!({}))).await;
    assert!(matches!(
        down,
        Err(pipeline::PipelineError::Unavailable { .. })
    ));
    assert_eq!(LOCAL_CALLS.load(Ordering::SeqCst), 3);
    assert_eq!(
        pipeline::invoke_on(Arc::new(89u64), &STORE, Call::person(json!({})))
            .await
            .unwrap()["store"],
        89,
        "an explicit scenario store is independent of app availability"
    );
    assert_eq!(LOCAL_CALLS.load(Ordering::SeqCst), 4);
    for key in [
        "IMPRESS_IMPLORE_HTTP_URL",
        "IMPRESS_IMBIB_HTTP_URL",
        "IMBIB_BACKEND",
    ] {
        std::env::remove_var(key);
    }
}

#[tokio::test]
async fn concurrent_calls_share_one_probe_and_changed_endpoint_gets_its_own_verdict() {
    let mut first = mockito::Server::new_async().await;
    let first_status = first
        .mock("GET", "/api/status")
        .with_status(200)
        .expect(1)
        .create_async()
        .await;
    let url = first.url();
    let (a, b) = tokio::join!(
        impress_app_transport::is_reachable("concurrent-proof", &url),
        impress_app_transport::is_reachable("concurrent-proof", &url)
    );
    assert!(a && b);
    first_status.assert_async().await;
    let mut second = mockito::Server::new_async().await;
    let second_status = second
        .mock("GET", "/api/status")
        .with_status(503)
        .expect(1)
        .create_async()
        .await;
    assert!(!impress_app_transport::is_reachable("concurrent-proof", &second.url()).await);
    second_status.assert_async().await;
}
