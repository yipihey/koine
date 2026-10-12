//! The host, started on port 0 and driven over HTTP.
//!
//! The host installs the process's store, manifest and background runner, so
//! two hosts in one test binary would stomp each other: every test takes
//! `TURN` for its whole length and gives its host its own `app_id`, so the
//! process-wide layout session registry does not carry one test's tree into
//! the next.

use std::time::Duration;

use impress_service_core::pipeline::adverbs::BackgroundRunner;
use impress_service_core::{
    inventory, Adverb, Effects, McpToolDescriptor, Safety, SafetyClass, ServiceFuture, Source,
    VerbDescriptor,
};
use koine_host::{ensure_linked, serve, Config, Served};
use reqwest::{header, Client, Method, Response, StatusCode};
use serde_json::{json, Value};

// ------------------------------------------------------------------ fixture

/// A verb that declares `in_background` and sleeps for `ms`, so the job tests
/// do not depend on what the demo's macro defaults say.
static NAP: VerbDescriptor = VerbDescriptor {
    name: "host-fixture_nap",
    service: "host-fixture",
    method: "nap",
    description: "Test fixture: sleeps for `ms` milliseconds, then answers.",
    input_schema: nap_schema,
    output_schema: nap_schema,
    safety: Safety {
        class: SafetyClass::ReadOnly,
        idempotent: true,
    },
    since: "0.1.0",
    effects: Effects::NONE,
    deprecated: None,
    aliases: &[],
    supports: &[Adverb::InBackground],
    adjective: None,
    examples: &[],
    strict: false,
    budget_ms: None,
    replay_full: false,
    returns_kind: None,
    rows: None,
    secrets: &[],
    source: Source::Linked,
    handler: nap,
};
inventory::submit! { McpToolDescriptor::of(&NAP) }

/// A verb that answers a refusal.
static REFUSE: VerbDescriptor = VerbDescriptor {
    name: "host-fixture_refuse",
    method: "refuse",
    description: "Test fixture: always refuses.",
    handler: refuse,
    ..NAP
};
inventory::submit! { McpToolDescriptor::of(&REFUSE) }

fn refuse(_: Value) -> ServiceFuture {
    Box::pin(async { Ok(json!({ "ok": false, "code": "nope", "message": "not today" })) })
}

/// A verb that declares a secret and answers how long it is (never what it
/// is), so the secret tests do not depend on any demo verb.
static NEEDS_TOKEN: VerbDescriptor = VerbDescriptor {
    name: "host-fixture_needs-token",
    method: "needs_token",
    description: "Test fixture: needs the secret t-host-token; answers its length.",
    handler: needs_token,
    secrets: &["t-host-token"],
    ..NAP
};
inventory::submit! { McpToolDescriptor::of(&NEEDS_TOKEN) }

fn needs_token(_: Value) -> ServiceFuture {
    Box::pin(async {
        let token = impress_service_core::secrets::get("t-host-token")
            .expect("the pipeline refuses a missing secret before the handler");
        Ok(json!({ "ok": true, "token_len": token.expose().len() }))
    })
}

/// A verb that declares `every` and `in_background` and counts its runs.
static TICK: VerbDescriptor = VerbDescriptor {
    name: "host-fixture_tick",
    method: "tick",
    description: "Test fixture: counts how many times it ran.",
    handler: tick,
    supports: &[Adverb::InBackground, Adverb::Every],
    ..NAP
};
inventory::submit! { McpToolDescriptor::of(&TICK) }

static TICKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn tick(_: Value) -> ServiceFuture {
    Box::pin(async {
        let n = TICKS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        Ok(json!({ "ok": true, "n": n }))
    })
}

/// A verb that reports progress, then works for a while.
static WORK: VerbDescriptor = VerbDescriptor {
    name: "host-fixture_work",
    method: "work",
    description: "Test fixture: reports half done, works 600 ms.",
    handler: work,
    ..NAP
};
inventory::submit! { McpToolDescriptor::of(&WORK) }

fn work(_: Value) -> ServiceFuture {
    Box::pin(async {
        impress_service_core::job::progress(Some(0.5), "half");
        tokio::time::sleep(Duration::from_millis(600)).await;
        Ok(json!({ "ok": true }))
    })
}

/// A verb that emits `n` numbered lines at once (a flood).
static FLOOD: VerbDescriptor = VerbDescriptor {
    name: "host-fixture_flood",
    method: "flood",
    description: "Test fixture: emits `ms` numbered lines as fast as it can.",
    handler: flood,
    ..NAP
};
inventory::submit! { McpToolDescriptor::of(&FLOOD) }

fn flood(args: Value) -> ServiceFuture {
    Box::pin(async move {
        let n = args.get("ms").and_then(Value::as_u64).unwrap_or(0);
        for i in 0..n {
            impress_service_core::job::emit(json!({ "i": i }));
        }
        Ok(json!({ "ok": true, "emitted": n }))
    })
}

fn nap_schema() -> Value {
    json!({ "type": "object", "properties": { "ms": { "type": "integer" } } })
}

fn nap(args: Value) -> ServiceFuture {
    Box::pin(async move {
        let ms = args.get("ms").and_then(Value::as_u64).unwrap_or(0);
        tokio::time::sleep(Duration::from_millis(ms)).await;
        Ok(json!({ "ok": true, "slept": ms }))
    })
}

// ------------------------------------------------------------------ harness

static TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Host {
    served: Served,
    base: String,
    client: Client,
    workspace: tempfile::TempDir,
}

fn anchor() {
    ensure_linked();
    std::hint::black_box(surface_demo_service::DefaultSurfaceDemoService::new as *const ());
}

async fn start_with(store: &str, app: &str, tweak: impl FnOnce(&mut Config)) -> Host {
    anchor();
    let workspace = tempfile::tempdir().unwrap();
    let mut config = Config::new(store, workspace.path());
    config.app_id = app.to_string();
    config.device = format!("{app}-device");
    tweak(&mut config);
    let served = serve(config).await.expect("the host starts");
    Host {
        base: served.url(),
        served,
        client: Client::new(),
        workspace,
    }
}

async fn start(store: &str, app: &str) -> Host {
    start_with(store, app, |_| {}).await
}

impl Host {
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    async fn get(&self, path: &str) -> Response {
        self.client.get(self.url(path)).send().await.unwrap()
    }

    /// A request with this launch's token.
    async fn send(&self, method: Method, path: &str, body: Option<Value>) -> Response {
        let mut request = self
            .client
            .request(method, self.url(path))
            .bearer_auth(self.served.token());
        if let Some(body) = body {
            request = request.json(&body);
        }
        request.send().await.unwrap()
    }

    async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        json_of(self.send(Method::POST, path, Some(body)).await).await
    }

    async fn get_json(&self, path: &str) -> (StatusCode, Value) {
        json_of(self.get(path).await).await
    }
}

async fn json_of(response: Response) -> (StatusCode, Value) {
    let status = response.status();
    let text = response.text().await.unwrap();
    let value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{status}: not JSON ({e}): {text:?}"));
    (status, value)
}

// ------------------------------------------------------------------- status

#[tokio::test]
async fn status_names_the_app_and_needs_no_token() {
    let _turn = TURN.lock().await;
    let host = start("memory:", "status-app").await;
    let (status, body) = host.get_json("/api/status").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["app"], "status-app");
    assert_eq!(body["port"], host.served.addr().port());
    assert_eq!(body["wire_version"], 1);
    assert_eq!(body["store"]["url"], "memory:");
    assert_eq!(body["store"]["capabilities"]["blobs"], true);
    assert_eq!(body["store"]["capabilities"]["durable"], false);
    // The two runtimes, the demo and the test fixture are all linked.
    assert!(body["verbs"].as_u64().unwrap() > 40, "{body}");
    host.served.shutdown().await;
}

// -------------------------------------------------------------------- guard

#[tokio::test]
async fn a_post_needs_this_launchs_token_and_a_get_does_not() {
    let _turn = TURN.lock().await;
    let host = start("memory:", "token-app").await;
    let path = "/api/verb/surface-demo-service_series";
    let args = json!({ "freq": 1.0, "n": 4 });

    let response = host
        .client
        .post(host.url(path))
        .json(&args)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
    let (_, body) = json_of(response).await;
    assert_eq!(body, json!({ "status": "error", "reason": "unauthorized" }));

    let wrong = host
        .client
        .post(host.url(path))
        .bearer_auth("not-the-token")
        .json(&args)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);

    // The same request with the token the host wrote to disk.
    let on_disk = std::fs::read_to_string(host.served.token_path()).unwrap();
    assert_eq!(on_disk.trim(), host.served.token());
    assert_eq!(
        host.served.token_path(),
        host.workspace
            .path()
            .join("automation")
            .join(format!("loopback-{}.token", host.served.addr().port()))
    );
    let (status, body) = host.post(path, args).await;
    assert_eq!(status, 200, "{body}");

    assert_eq!(host.get("/api/status").await.status(), 200);

    let token_path = host.served.token_path().to_path_buf();
    host.served.shutdown().await;
    assert!(!token_path.exists(), "the token goes when the host does");
}

#[tokio::test]
async fn a_host_header_that_is_not_this_machine_is_400() {
    let _turn = TURN.lock().await;
    let host = start("memory:", "host-app").await;
    let port = host.served.addr().port();
    for name in ["evil.example", &format!("evil.example:{port}"), "10.0.0.5"] {
        let response = host
            .client
            .get(host.url("/api/status"))
            .header(header::HOST, name)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "Host: {name}");
    }
    for name in [
        "localhost".to_string(),
        format!("localhost:{port}"),
        format!("127.0.0.1:{port}"),
        format!("[::1]:{port}"),
    ] {
        let response = host
            .client
            .get(host.url("/api/status"))
            .header(header::HOST, &name)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "Host: {name}");
    }
    host.served.shutdown().await;
}

#[tokio::test]
async fn no_response_carries_a_cors_header() {
    let _turn = TURN.lock().await;
    let host = start("memory:", "cors-app").await;
    let origin = "https://attacker.example";
    let mut seen = 0;
    let mut check = |what: &str, response: &Response| {
        seen += 1;
        for name in response.headers().keys() {
            assert!(
                !name.as_str().starts_with("access-control-"),
                "{what} answered {name}"
            );
        }
    };

    // An allowed read, an unknown route, a refused write, a bad Host.
    let ok = host
        .client
        .get(host.url("/api/status"))
        .header(header::ORIGIN, origin)
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    check("GET /api/status", &ok);
    let missing = host
        .client
        .get(host.url("/api/nope"))
        .header(header::ORIGIN, origin)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
    check("GET /api/nope", &missing);
    let refused = host
        .client
        .post(host.url("/api/verb/x"))
        .header(header::ORIGIN, origin)
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 401);
    check("POST without a token", &refused);
    let wrong_host = host
        .client
        .get(host.url("/api/status"))
        .header(header::HOST, "evil.example")
        .header(header::ORIGIN, origin)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_host.status(), 400);
    check("GET with a bad Host", &wrong_host);

    // A cross-origin preflight is a bare 204 that grants nothing.
    let preflight = host
        .client
        .request(Method::OPTIONS, host.url("/api/verb/x"))
        .header(header::ORIGIN, origin)
        .header("Access-Control-Request-Method", "POST")
        .header("Access-Control-Request-Headers", "authorization")
        .send()
        .await
        .unwrap();
    assert_eq!(preflight.status(), 204);
    check("OPTIONS preflight", &preflight);
    assert_eq!(seen, 5);
    host.served.shutdown().await;
}

// ------------------------------------------------------------------ surface

#[tokio::test]
async fn a_verb_has_a_generated_surface_that_runs_it() {
    let _turn = TURN.lock().await;
    let host = start("memory:", "verb-surface-app").await;

    let (status, answer) = host
        .get_json("/api/verb/surface-demo-service_histogram/surface")
        .await;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["ok"], true);
    assert_eq!(answer["wire_version"], 1);
    let spec = answer["spec"].clone();
    assert_eq!(spec["name"], "Run surface-demo-service_histogram");
    assert!(spec["state"].get("result").is_some(), "{spec}");
    assert!(
        spec.to_string().contains("\"run\""),
        "the Run button: {spec}"
    );

    // It is a surface like any other: create, render, click Run.
    let (status, created) = host.post("/api/surface", spec).await;
    assert_eq!(status, 200, "{created}");
    let id = created["id"].as_str().expect("an id").to_string();
    let (status, rendered) = host.get_json(&format!("/api/surface/{id}/render")).await;
    assert_eq!(status, 200, "{rendered}");
    assert!(
        rendered["tree"].to_string().contains("\"run\""),
        "{rendered}"
    );
    let (status, dispatched) = host
        .post(
            &format!("/api/surface/{id}/dispatch"),
            json!({ "widget": "run", "kind": "click" }),
        )
        .await;
    assert_eq!(status, 200, "{dispatched}");
    assert_eq!(dispatched["ok"], true, "{dispatched}");

    // What a text input sends is characters: typed into the raw-JSON `values`
    // field, `[1, 2, 3, 4]` must reach the verb as the array it spells.
    for event in [
        json!({ "widget": "arg-values", "kind": "change", "value": "[1, 2, 3, 4]" }),
        json!({ "widget": "arg-bins", "kind": "change", "value": 2 }),
        json!({ "widget": "run", "kind": "click" }),
    ] {
        let (status, dispatched) = host
            .post(&format!("/api/surface/{id}/dispatch"), event)
            .await;
        assert_eq!(status, 200, "{dispatched}");
    }
    let (status, state) = host.get_json(&format!("/api/surface/{id}/state")).await;
    assert_eq!(status, 200, "{state}");
    assert_eq!(state["state"]["values"], json!([1, 2, 3, 4]), "{state}");
    assert_ne!(state["state"]["result"]["ok"], false, "{state}");
    assert_eq!(state["state"]["result"]["counts"], json!([2, 2]), "{state}");

    let (status, missing) = host.get_json("/api/verb/nope_nothing/surface").await;
    assert_eq!(status, 404, "{missing}");
    assert_eq!(missing["code"], "not-found");

    host.served.shutdown().await;
}

fn explorer() -> Value {
    serde_json::to_value(impress_surface::example_signal_explorer()).unwrap()
}

#[tokio::test]
async fn a_surface_is_created_rendered_dispatched_and_waited_on() {
    let _turn = TURN.lock().await;
    let host = start("memory:", "surface-app").await;

    // The surface route table needs the token to write.
    let anonymous = host
        .client
        .post(host.url("/api/surface"))
        .json(&explorer())
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);

    // A bare spec body is the `spec` argument.
    let (status, created) = host.post("/api/surface", explorer()).await;
    assert_eq!(status, 200, "{created}");
    assert_eq!(created["ok"], true);
    assert_eq!(created["revision"], 1);
    let id = created["id"].as_str().expect("an id").to_string();

    let (status, listed) = host.get_json("/api/surface").await;
    assert_eq!(status, 200, "{listed}");
    assert!(
        listed["surfaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == id.as_str()),
        "{listed}"
    );

    let (status, rendered) = host.get_json(&format!("/api/surface/{id}/render")).await;
    assert_eq!(status, 200, "{rendered}");
    assert_eq!(rendered["ok"], true);
    assert!(rendered["tree"].is_object(), "{rendered}");
    let text = rendered["tree"].to_string();
    assert!(
        text.contains("use-bins-btn"),
        "the button is in the tree: {text}"
    );
    // The demo verbs the sources call ran through the pipeline (the
    // `publication` query source has no such kind in the minimal manifest, and
    // says so instead of failing the render).
    assert!(
        rendered["source_errors"]
            .as_array()
            .map(|errors| errors.iter().all(|e| e.to_string().contains("papers")))
            .unwrap_or(true),
        "{rendered}"
    );

    // A bare event body is the `event` argument.
    let (status, dispatched) = host
        .post(
            &format!("/api/surface/{id}/dispatch"),
            json!({ "widget": "use-bins-btn", "kind": "click" }),
        )
        .await;
    assert_eq!(status, 200, "{dispatched}");
    assert_eq!(dispatched["ok"], true, "{dispatched}");

    let (status, waited) = host
        .get_json(&format!(
            "/api/surface/{id}/wait?after_seq=0&timeout_ms=100"
        ))
        .await;
    assert_eq!(status, 200, "{waited}");
    assert_eq!(waited["timed_out"], false, "{waited}");
    let events = waited["events"].as_array().unwrap();
    assert!(
        events.iter().any(|e| e.to_string().contains("bins-chosen")),
        "the emit is in the ring: {waited}"
    );
    assert!(waited["next_seq"].as_u64().unwrap() >= 1);

    // Past the cursor there is nothing, and the wait says it timed out.
    let after = waited["next_seq"].as_u64().unwrap();
    let (_, quiet) = host
        .get_json(&format!(
            "/api/surface/{id}/wait?after_seq={after}&timeout_ms=100"
        ))
        .await;
    assert_eq!(quiet["timed_out"], true, "{quiet}");

    // Refusals carry the status their code maps to.
    let (status, missing) = host
        .get_json("/api/surface/00000000-0000-4000-8000-000000000000/render")
        .await;
    assert_eq!(status, 404, "{missing}");
    assert_eq!(missing["code"], "not-found");
    let (status, nothing) = host.get_json(&format!("/api/surface/{id}/nonsense")).await;
    assert_eq!(status, 404, "{nothing}");
    let (status, bad) = host.get_json("/api/surface/x/wait?after_seq=soon").await;
    assert_eq!(status, 400, "{bad}");

    host.served.shutdown().await;
}

/// The paper-triage example calls `triage-service_*`, which is impress-apps'
/// and is not linked into a koine host: it is refused for exactly that, at
/// the status of `invalid-spec`, rather than stored and broken later.
#[tokio::test]
async fn a_spec_naming_a_verb_this_host_does_not_link_is_refused() {
    let _turn = TURN.lock().await;
    let host = start("memory:", "triage-app").await;
    let spec = serde_json::to_value(impress_surface::example_paper_triage()).unwrap();
    let (status, refused) = host.post("/api/surface", spec).await;
    assert_eq!(status, 422, "{refused}");
    assert_eq!(refused["ok"], false);
    assert!(refused.to_string().contains("no such verb"), "{refused}");
    host.served.shutdown().await;
}

// -------------------------------------------------------------------- verbs

#[tokio::test]
async fn a_verb_is_reached_by_name_and_refusals_have_statuses() {
    let _turn = TURN.lock().await;
    let host = start("memory:", "verb-app").await;
    let (status, series) = host
        .post(
            "/api/verb/surface-demo-service_series",
            json!({ "freq": 1.0, "n": 8 }),
        )
        .await;
    assert_eq!(status, 200, "{series}");
    assert_eq!(series["values"].as_array().unwrap().len(), 8, "{series}");

    // Strict: an argument the schema does not name is refused by name.
    let (status, unknown) = host
        .post(
            "/api/verb/surface-demo-service_series",
            json!({ "freq": 1.0, "n": 8, "bogus": 1 }),
        )
        .await;
    assert!(status == 400 || status == 422, "{status} {unknown}");
    assert_eq!(unknown["ok"], false, "{unknown}");

    let (status, none) = host.post("/api/verb/no-such_verb", json!({})).await;
    assert_eq!(status, 404, "{none}");
    assert_eq!(none["code"], "not-found");

    let bad = host
        .client
        .post(host.url("/api/verb/surface-demo-service_series"))
        .bearer_auth(host.served.token())
        .body("{not json")
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
    host.served.shutdown().await;
}

// ------------------------------------------------------------------- layout

#[tokio::test]
async fn the_layout_routes_run_the_layout_verbs() {
    let _turn = TURN.lock().await;
    let host = start("memory:", "layout-app").await;

    // A cold start: `get-layout` writes the default arrangement, so the tree
    // is there to read, with the revision a verb can be conditioned on.
    let (status, tree) = host.get_json("/api/layout/tree").await;
    assert_eq!(status, 200, "{tree}");
    assert_eq!(tree["ok"], true);
    assert_eq!(tree["wire_version"], 1);
    assert_eq!(tree["app"], "layout-app");
    assert_eq!(tree["device"], "layout-app-device");
    assert!(tree["layout"].is_object(), "{tree}");
    let revision = tree["revision"].as_u64().expect("a revision");
    assert_eq!(tree["version"].as_u64(), Some(revision));
    assert!(tree["focused"].is_u64(), "{tree}");
    let before = tree["focused"].as_u64().unwrap();

    // A verb needs the token, takes `Verb` JSON and answers the verb envelope.
    let anonymous = host
        .client
        .post(host.url("/api/layout/verb"))
        .json(&json!({ "verb": "focus-direction", "direction": "next" }))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);
    let (status, moved) = host
        .post(
            "/api/layout/verb",
            json!({
                "verb": "focus-direction",
                "direction": "next",
                "expected_revision": revision,
            }),
        )
        .await;
    assert_eq!(status, 200, "{moved}");
    assert_eq!(moved["ok"], true);
    assert_ne!(moved["focused"].as_u64(), Some(before), "{moved}");
    let (_, after) = host.get_json("/api/layout/tree").await;
    assert_eq!(after["focused"], moved["focused"], "{after}");

    // The revision it was conditioned on is stale now: 409 conflict.
    let (status, stale) = host
        .post(
            "/api/layout/verb",
            json!({
                "verb": "focus-direction",
                "direction": "next",
                "expected_revision": revision,
            }),
        )
        .await;
    assert_eq!(status, 409, "{stale}");
    assert_eq!(stale["code"], "conflict");

    // Strictly parsed: a field the verb does not take is named.
    let (status, strict) = host
        .post(
            "/api/layout/verb",
            json!({ "verb": "focus-direction", "direction": "next", "bogus": 1 }),
        )
        .await;
    assert_eq!(status, 400, "{strict}");
    assert!(strict["message"].as_str().unwrap().contains("bogus"));

    // The operations that are not `Verb` cases.
    let (status, saved) = host
        .post(
            "/api/layout/op",
            json!({ "op": "save-layout", "name": "Mine", "purpose": "a test" }),
        )
        .await;
    assert_eq!(status, 200, "{saved}");
    let (status, listed) = host.get_json("/api/layout/layouts").await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(listed["app"], "layout-app");
    assert!(listed.to_string().contains("Mine"), "{listed}");
    let (status, applied) = host
        .post(
            "/api/layout/op",
            json!({ "op": "apply-layout", "name": "Mine" }),
        )
        .await;
    assert_eq!(status, 200, "{applied}");
    let (status, undone) = host.post("/api/layout/op", json!({ "op": "undo" })).await;
    assert_eq!(status, 400, "undo names its stack: {undone}");
    let (status, undone) = host
        .post(
            "/api/layout/op",
            json!({ "op": "undo", "stack": "arrangement" }),
        )
        .await;
    assert_eq!(status, 200, "{undone}");
    let (status, deleted) = host
        .post(
            "/api/layout/op",
            json!({ "op": "delete-layout", "name": "Mine" }),
        )
        .await;
    assert_eq!(status, 200, "{deleted}");
    assert_eq!(deleted["ok"], true);
    let (_, listed) = host.get_json("/api/layout/layouts").await;
    assert!(!listed.to_string().contains("Mine"), "{listed}");

    let (status, unknown) = host
        .post("/api/layout/op", json!({ "op": "explode" }))
        .await;
    assert_eq!(status, 400, "{unknown}");
    assert!(unknown["message"].as_str().unwrap().contains("explode"));
    let (status, no_op) = host.post("/api/layout/op", json!({})).await;
    assert_eq!(status, 400, "{no_op}");

    // resize-share: one pane's share in its parent split.
    let (_, tree) = host.get_json("/api/layout/tree").await;
    let tiles = tree["layout"].to_string();
    assert!(
        tiles.contains("tiles") || tiles.contains("windows"),
        "{tiles}"
    );
    let (status, resized) = host
        .post(
            "/api/layout/op",
            json!({ "op": "resize-share", "pane": 999_999, "share": 0.5 }),
        )
        .await;
    assert_eq!(status, 422, "an unknown pane is a tree refusal: {resized}");
    assert_eq!(resized["ok"], false);

    host.served.shutdown().await;
}

// ----------------------------------------------------------------------- ui

#[tokio::test]
async fn ui_is_503_naming_the_build_when_there_is_no_bundle() {
    let _turn = TURN.lock().await;
    let host = start("memory:", "ui-none-app").await;
    for path in ["/ui", "/ui/", "/ui/surface/abc", "/ui/assets/x.js"] {
        let (status, body) = host.get_json(path).await;
        assert_eq!(status, 503, "{path}");
        assert_eq!(body["status"], "error");
        assert!(
            body["fix"]
                .as_str()
                .unwrap()
                .contains("dx build --platform web --release"),
            "{body}"
        );
        assert!(body["fix"].as_str().unwrap().contains("crates/koine-web"));
    }
    host.served.shutdown().await;

    // A directory without an index.html is the same answer.
    let empty = tempfile::tempdir().unwrap();
    let host = start_with("memory:", "ui-empty-app", |c| {
        c.web_dir = Some(empty.path().to_path_buf())
    })
    .await;
    assert_eq!(host.get("/ui/").await.status(), 503);
    host.served.shutdown().await;
}

#[tokio::test]
async fn ui_serves_files_and_the_page_with_the_token_for_loopback() {
    let _turn = TURN.lock().await;
    let bundle = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(bundle.path().join("assets")).unwrap();
    std::fs::write(
        bundle.path().join("index.html"),
        "<!doctype html><html><head><title>koine</title></head><body>page</body></html>",
    )
    .unwrap();
    std::fs::write(bundle.path().join("assets/x.js"), "console.log(1)").unwrap();
    std::fs::write(bundle.path().join("assets/y.wasm"), [0, 97, 115, 109]).unwrap();
    std::fs::write(bundle.path().join(".secret"), "no").unwrap();
    let host = start_with("memory:", "ui-app", |c| {
        c.web_dir = Some(bundle.path().to_path_buf())
    })
    .await;

    let js = host.get("/ui/assets/x.js").await;
    assert_eq!(js.status(), 200);
    assert_eq!(
        js.headers()[header::CONTENT_TYPE],
        "text/javascript; charset=utf-8"
    );
    assert_eq!(js.text().await.unwrap(), "console.log(1)");
    let wasm = host.get("/ui/assets/y.wasm").await;
    assert_eq!(wasm.headers()[header::CONTENT_TYPE], "application/wasm");

    // Anything that is not a file is the page, and the page carries the token.
    for path in ["/ui", "/ui/", "/ui/surface/abc", "/ui/layout"] {
        let page = host.get(path).await;
        assert_eq!(page.status(), 200, "{path}");
        assert_eq!(
            page.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        let html = page.text().await.unwrap();
        assert!(
            html.contains("<meta name=\"koine-token\""),
            "{path}: {html}"
        );
        assert!(
            html.contains(&format!("content=\"{}\"", host.served.token())),
            "{html}"
        );
        assert!(html.contains("body>page"), "{html}");
    }

    // A file that is not there, a climb out of the bundle, a dot-file.
    assert_eq!(host.get("/ui/assets/missing.js").await.status(), 404);
    assert_eq!(host.get("/ui/.secret").await.status(), 404);
    assert_eq!(host.get("/ui/assets/%2e%2e/x.js").await.status(), 404);

    // It is a read: a write to /ui is not a route (and needs the token first).
    let post = host.send(Method::POST, "/ui/", Some(json!({}))).await;
    assert_eq!(post.status(), 405);
    host.served.shutdown().await;
}

// --------------------------------------------------------------------- blob

/// An item to hang blobs on: the blob port wants a row to exist.
async fn an_item(host: &Host) -> String {
    let (status, created) = host.post("/api/surface", explorer()).await;
    assert_eq!(status, 200, "{created}");
    created["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn a_blob_round_trips_on_a_store_that_holds_blobs() {
    let _turn = TURN.lock().await;
    let host = start("memory:", "blob-app").await;
    let item = an_item(&host).await;
    let path = format!("/api/blob/{item}/a.txt");

    let anonymous = host
        .client
        .put(host.url(&path))
        .body("hello")
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);

    let (status, put) = json_of(
        host.client
            .put(host.url(&path))
            .bearer_auth(host.served.token())
            .body("hello, blob")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 200, "{put}");
    assert_eq!(put["bytes"], 11);

    let got = host.get(&path).await;
    assert_eq!(got.status(), 200);
    assert_eq!(
        got.headers()[header::CONTENT_TYPE],
        "text/plain; charset=utf-8"
    );
    assert_eq!(got.bytes().await.unwrap().as_ref(), b"hello, blob");

    let (status, gone) = json_of(host.send(Method::DELETE, &path, None).await).await;
    assert_eq!(status, 200, "{gone}");
    let (status, missing) = host.get_json(&path).await;
    assert_eq!(status, 404, "{missing}");
    assert_eq!(missing["code"], "not-found");

    let (status, bad) = host.get_json("/api/blob/not-an-id/a.txt").await;
    assert_eq!(status, 400, "{bad}");
    host.served.shutdown().await;
}

// ------------------------------------------------------------------ restart

#[tokio::test]
async fn a_dir_store_survives_a_restart() {
    let _turn = TURN.lock().await;
    let store_root = tempfile::tempdir().unwrap();
    let url = format!("dir:{}", store_root.path().display());

    let host = start(&url, "restart-app").await;
    let (_, status) = host.get_json("/api/status").await;
    assert_eq!(status["store"]["capabilities"]["durable"], true);
    let item = an_item(&host).await;
    let (put, _) = json_of(
        host.client
            .put(host.url(&format!("/api/blob/{item}/keep.txt")))
            .bearer_auth(host.served.token())
            .body("kept")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(put, 200);
    host.served.shutdown().await;

    let host = start(&url, "restart-app").await;
    let (status, listed) = host.get_json("/api/surface").await;
    assert_eq!(status, 200, "{listed}");
    assert!(
        listed["surfaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == item.as_str()),
        "the surface is still listed after a restart: {listed}"
    );
    let blob = host.get(&format!("/api/blob/{item}/keep.txt")).await;
    assert_eq!(blob.bytes().await.unwrap().as_ref(), b"kept");
    host.served.shutdown().await;
}

// ---------------------------------------------------------------------- mcp

#[tokio::test]
async fn mcp_lists_the_linked_verbs_and_calls_them() {
    let _turn = TURN.lock().await;
    let host = start("memory:", "mcp-app").await;

    // POST is a write: the token applies to MCP as to everything else.
    let anonymous = host
        .client
        .post(host.url("/mcp"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);

    let (status, init) = host
        .post(
            "/mcp",
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }),
        )
        .await;
    assert_eq!(status, 200, "{init}");
    assert_eq!(init["result"]["serverInfo"]["name"], "mcp-app");

    let (status, listed) = host
        .post(
            "/mcp",
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        )
        .await;
    assert_eq!(status, 200, "{listed}");
    let tools = listed["result"]["tools"].as_array().unwrap();
    for name in [
        "surface-demo-service_series",
        "surface-demo-service_histogram",
        "impress-surface-service_surface-create",
        "layout-service_get-layout",
    ] {
        assert!(tools.iter().any(|t| t["name"] == name), "{name} is listed");
    }
    // The demo declares no `returns_kind`, so no `_meta` carries one.
    let series = tools
        .iter()
        .find(|t| t["name"] == "surface-demo-service_series")
        .unwrap();
    assert!(
        series
            .get("_meta")
            .and_then(|m| m.get("returns_kind"))
            .is_none(),
        "{series}"
    );

    let (status, called) = host
        .post(
            "/mcp",
            json!({
                "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": {
                    "name": "surface-demo-service_series",
                    "arguments": { "freq": 1.0, "n": 3 },
                },
            }),
        )
        .await;
    assert_eq!(status, 200, "{called}");
    assert_eq!(called["result"]["isError"], false, "{called}");
    assert_eq!(
        called["result"]["structuredContent"]["values"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

    // A notification is not answered; a body that is not JSON is a parse error.
    let note = host
        .send(
            Method::POST,
            "/mcp",
            Some(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })),
        )
        .await;
    assert_eq!(note.status(), 202);
    let garbled = host
        .client
        .post(host.url("/mcp"))
        .bearer_auth(host.served.token())
        .body("nope")
        .send()
        .await
        .unwrap();
    assert_eq!(garbled.status(), 400);
    let (_, garbled) = json_of(garbled).await;
    assert_eq!(garbled["error"]["code"], -32700);
    host.served.shutdown().await;
}

// --------------------------------------------------------------------- jobs

#[tokio::test]
async fn in_background_starts_a_job_that_finishes_or_is_cancelled() {
    let _turn = TURN.lock().await;
    let host = start("memory:", "job-app").await;

    // A verb that honours the adverb answers a job handle, at once.
    let (status, started) = host
        .post(
            "/api/verb/host-fixture_nap",
            json!({ "ms": 50, "adverbs": { "in_background": true } }),
        )
        .await;
    assert_eq!(status, 200, "{started}");
    assert_eq!(started["ok"], true);
    assert_eq!(started["job"]["state"], "running");
    assert_eq!(started["job"]["kind"], "host-fixture_nap");
    let id = started["job"]["id"].as_str().unwrap().to_string();

    let mut finished = Value::Null;
    for _ in 0..100 {
        let (status, job) = host.get_json(&format!("/api/job/{id}")).await;
        assert_eq!(status, 200, "{job}");
        if job["job"]["state"] != "running" {
            finished = job;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(finished["job"]["state"], "completed", "{finished}");
    assert_eq!(finished["result"]["slept"], 50, "{finished}");
    assert!(finished["finished"].is_string());

    // A long one is cancelled, and says so.
    let (_, started) = host
        .post(
            "/api/verb/host-fixture_nap",
            json!({ "ms": 60_000, "adverbs": { "in_background": true } }),
        )
        .await;
    let id = started["job"]["id"].as_str().unwrap().to_string();
    assert_eq!(host.served.runner().running(), 1);
    let (status, cancelled) = host.post(&format!("/api/job/{id}/cancel"), json!({})).await;
    assert_eq!(status, 200, "{cancelled}");
    assert_eq!(cancelled["cancelled"], true);
    assert_eq!(cancelled["job"]["state"], "cancelled");
    assert_eq!(host.served.runner().running(), 0);
    // Cancelling again is not an error, and does not claim to have stopped it.
    let (_, again) = host.post(&format!("/api/job/{id}/cancel"), json!({})).await;
    assert_eq!(again["cancelled"], false);
    assert_eq!(again["job"]["state"], "cancelled");

    let (status, none) = host
        .get_json("/api/job/00000000-0000-4000-8000-000000000000")
        .await;
    assert_eq!(status, 404, "{none}");
    let (status, bad) = host.get_json("/api/job/nope").await;
    assert_eq!(status, 400, "{bad}");
    host.served.shutdown().await;
}

#[tokio::test]
async fn a_job_whose_verb_refuses_is_failed_and_says_why() {
    let _turn = TURN.lock().await;
    let host = start("memory:", "job-fail-app").await;
    let store = koine_store::process::current().expect("the host installed its store");
    let runner = koine_host::runner::JobRunner::new(store, tokio::runtime::Handle::current());
    let verb = impress_service_core::call::find("host-fixture_refuse").unwrap();
    let call = impress_service_core::pipeline::Call::agent("test", json!({}));
    let handle = runner.start(verb, call).expect("the job starts");
    assert_eq!(handle.state, "running");
    let id = handle.id.parse().unwrap();
    let mut job = Value::Null;
    for _ in 0..100 {
        job = runner.status(id).expect("a row");
        if job["job"]["state"] != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(job["job"]["state"], "failed", "{job}");
    assert_eq!(job["code"], "nope", "{job}");
    assert_eq!(job["message"], "not today", "{job}");
    assert_eq!(job["result"]["ok"], false);
    host.served.shutdown().await;
}

// ------------------------------------------------------------------ secrets

#[tokio::test]
async fn a_declared_secret_is_listed_set_used_and_removed_over_http() {
    let _turn = TURN.lock().await;
    let host = start("memory:", "secret-app").await;
    let verb = "/api/verb/host-fixture_needs-token";
    let file = host.workspace.path().join("secrets").join("t-host-token");

    // Fresh: declared, absent, and the verb is refused before its handler.
    let (status, listed) = host.get_json("/api/secret").await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(listed["ok"], true);
    assert_eq!(listed["wire_version"], 1);
    let row = listed["secrets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "t-host-token")
        .unwrap_or_else(|| panic!("t-host-token is declared: {listed}"))
        .clone();
    assert_eq!(row["present"], false, "{row}");
    assert_eq!(row["source"], Value::Null);
    assert_eq!(row["verbs"], json!(["host-fixture_needs-token"]));

    let (_, status_body) = host.get_json("/api/status").await;
    assert!(status_body["secrets"]["declared"].as_u64().unwrap() >= 1);
    assert!(status_body["secrets"]["missing"]
        .as_array()
        .unwrap()
        .contains(&json!("t-host-token")));

    let (status, refused) = host.post(verb, json!({})).await;
    assert_eq!(status, 412, "{refused}");
    assert_eq!(refused["code"], "missing-secret");
    assert!(
        refused["message"]
            .as_str()
            .unwrap()
            .contains("t-host-token"),
        "{refused}"
    );

    // The generated form opens with the warning.
    let (status, form) = host
        .get_json("/api/verb/host-fixture_needs-token/surface")
        .await;
    assert_eq!(status, 200, "{form}");
    assert!(
        form["spec"].to_string().contains("missing-secret"),
        "{form}"
    );

    // Set it: the file exists, 0600, and the verb runs.
    let (status, put) = json_of(
        host.send(
            Method::PUT,
            "/api/secret/t-host-token",
            Some(json!({ "value": "abcdef" })),
        )
        .await,
    )
    .await;
    assert_eq!(status, 200, "{put}");
    assert_eq!(
        put,
        json!({ "ok": true, "wire_version": 1, "name": "t-host-token", "source": "file" })
    );
    assert!(file.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(file.parent().unwrap()), 0o700);
    }
    let (status, ran) = host.post(verb, json!({})).await;
    assert_eq!(status, 200, "{ran}");
    assert_eq!(ran["token_len"], 6);

    let (_, listed) = host.get_json("/api/secret").await;
    assert!(!listed.to_string().contains("abcdef"), "never a value");
    let (_, status_body) = host.get_json("/api/status").await;
    assert!(!status_body["secrets"]["missing"]
        .as_array()
        .unwrap()
        .contains(&json!("t-host-token")));
    assert!(!status_body.to_string().contains("abcdef"));
    let (_, form) = host
        .get_json("/api/verb/host-fixture_needs-token/surface")
        .await;
    assert!(
        !form["spec"].to_string().contains("missing-secret"),
        "no warning once it is set"
    );

    // Remove it: gone from disk, the verb is refused again.
    let (status, deleted) = json_of(
        host.send(Method::DELETE, "/api/secret/t-host-token", None)
            .await,
    )
    .await;
    assert_eq!(status, 200, "{deleted}");
    assert_eq!(
        deleted,
        json!({ "ok": true, "wire_version": 1, "name": "t-host-token", "removed": true })
    );
    assert!(!file.exists());
    let (status, refused) = host.post(verb, json!({})).await;
    assert_eq!(status, 412, "{refused}");
    let (_, again) = json_of(
        host.send(Method::DELETE, "/api/secret/t-host-token", None)
            .await,
    )
    .await;
    assert_eq!(again["removed"], false);

    host.served.shutdown().await;
}

#[tokio::test]
async fn the_secret_routes_refuse_what_is_not_a_slot() {
    let _turn = TURN.lock().await;
    let host = start("memory:", "secret-refusals-app").await;

    // A name no verb declares: 404, and the declared ones are listed.
    let (status, body) = json_of(
        host.send(
            Method::PUT,
            "/api/secret/not-declared",
            Some(json!({ "value": "x" })),
        )
        .await,
    )
    .await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(body["code"], "not-found");
    assert!(
        body["message"].as_str().unwrap().contains("t-host-token"),
        "{body}"
    );
    let (status, body) = json_of(
        host.send(Method::DELETE, "/api/secret/not-declared", None)
            .await,
    )
    .await;
    assert_eq!(status, 404, "{body}");
    assert!(!host
        .workspace
        .path()
        .join("secrets")
        .join("not-declared")
        .exists());

    // Empty value, a non-string value and a non-object body.
    for body in [
        json!({ "value": "" }),
        json!({ "value": 7 }),
        json!({}),
        json!("abc"),
        json!(["abc"]),
    ] {
        let (status, refused) = json_of(
            host.send(Method::PUT, "/api/secret/t-host-token", Some(body.clone()))
                .await,
        )
        .await;
        assert_eq!(status, 400, "{body}: {refused}");
        assert_eq!(refused["code"], "invalid-argument", "{body}");
    }
    let (status, _) = json_of(
        host.send(Method::PUT, "/api/secret/t-host-token", None)
            .await,
    )
    .await;
    assert_eq!(status, 400);
    assert!(!host
        .workspace
        .path()
        .join("secrets")
        .join("t-host-token")
        .exists());

    // The guard: a PUT or DELETE without this launch's token is 401.
    let unauthorised = host
        .client
        .put(host.url("/api/secret/t-host-token"))
        .json(&json!({ "value": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorised.status(), 401);
    let unauthorised = host
        .client
        .delete(host.url("/api/secret/t-host-token"))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorised.status(), 401);
    assert!(!host
        .workspace
        .path()
        .join("secrets")
        .join("t-host-token")
        .exists());

    host.served.shutdown().await;
}

#[tokio::test]
async fn secrets_dir_moves_the_file_layer() {
    let _turn = TURN.lock().await;
    let elsewhere = tempfile::tempdir().unwrap();
    let dir = elsewhere.path().join("keys");
    let configured = dir.clone();
    let host = start_with("memory:", "secret-dir-app", move |c| {
        c.secrets_dir = Some(configured);
    })
    .await;
    let (status, put) = json_of(
        host.send(
            Method::PUT,
            "/api/secret/t-host-token",
            Some(json!({ "value": "from-file" })),
        )
        .await,
    )
    .await;
    assert_eq!(status, 200, "{put}");
    assert!(dir.join("t-host-token").exists());
    assert!(!host.workspace.path().join("secrets").exists());
    host.served.shutdown().await;
}

// ------------------------------------------------------------------- routines

/// Poll `GET /api/job/<id>` until `done` holds of the answer.
async fn job_until(host: &Host, id: &str, done: impl Fn(&Value) -> bool) -> Value {
    let mut last = Value::Null;
    for _ in 0..300 {
        let (status, job) = host.get_json(&format!("/api/job/{id}")).await;
        assert_eq!(status, 200, "{job}");
        if done(&job) {
            return job;
        }
        last = job;
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the job never got there: {last}");
}

#[tokio::test]
async fn every_refuses_what_the_grammar_refuses() {
    let _turn = TURN.lock().await;
    let host = start("memory:", "every-refuse-app").await;
    async fn every_call(host: &Host, every: Value, verb: &str) -> (StatusCode, Value) {
        host.post(
            &format!("/api/verb/{verb}"),
            json!({ "adverbs": { "every": every } }),
        )
        .await
    }
    let post = |every: Value, verb: &'static str| every_call(&host, every, verb);
    // A verb that does not declare `every` is refused for exactly that.
    let (_, refused) = post(json!("2s"), "host-fixture_nap").await;
    assert_eq!(refused["code"], "unsupported-adverb", "{refused}");
    assert_eq!(
        refused["message"],
        "host-fixture_nap does not support the adverb 'every'"
    );
    // A bad interval is `invalid-argument`, saying what it takes.
    let (status, sub) = post(json!("500ms"), "host-fixture_tick").await;
    assert_eq!(status, 400, "{sub}");
    assert_eq!(sub["code"], "invalid-argument");
    assert!(
        sub["message"]
            .as_str()
            .unwrap()
            .contains("'every' must be at least 1s"),
        "{sub}"
    );
    let (_, number) = post(json!(5), "host-fixture_tick").await;
    assert_eq!(number["code"], "invalid-argument", "{number}");
    let (_, word) = post(json!("soon"), "host-fixture_tick").await;
    assert_eq!(word["code"], "invalid-argument", "{word}");
    let (_, days) = post(json!("999d"), "host-fixture_tick").await;
    assert!(
        days["message"].as_str().unwrap().contains("at most 365d"),
        "{days}"
    );
    assert_eq!(host.served.runner().running(), 0, "nothing was started");
    host.served.shutdown().await;
}

#[tokio::test]
async fn every_runs_a_verb_repeatedly_until_cancelled_and_job_lists_it() {
    let _turn = TURN.lock().await;
    let host = start("memory:", "every-app").await;

    let (status, started) = host
        .post(
            "/api/verb/host-fixture_tick",
            json!({ "adverbs": { "every": "1s" } }),
        )
        .await;
    assert_eq!(status, 200, "{started}");
    assert_eq!(started["job"]["state"], "running");
    assert_eq!(started["job"]["kind"], "host-fixture_tick");
    let id = started["job"]["id"].as_str().unwrap().to_string();

    // The first run is at once; the second a second later.
    let job = job_until(&host, &id, |j| j["runs"].as_i64().unwrap_or(0) >= 2).await;
    assert_eq!(job["routine"], true, "{job}");
    assert_eq!(job["verb"], "host-fixture_tick");
    assert_eq!(job["every_ms"], 1000);
    assert_eq!(job["job"]["state"], "running");
    assert_eq!(job["last_result"]["ok"], true, "{job}");
    assert!(job["last_result"]["n"].as_u64().unwrap() >= 2, "{job}");
    assert!(job["last_finished"].is_string());
    assert_eq!(job["args"], json!({}));

    // `GET /api/job` lists it, with the rest of the shape.
    let (status, listed) = host.get_json("/api/job").await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(listed["ok"], true);
    assert_eq!(listed["wire_version"], 1);
    let jobs = listed["jobs"].as_array().expect("an array of jobs");
    assert!(
        jobs.iter().any(|j| j["job"]["id"] == id.as_str()),
        "{listed}"
    );

    // Cancel stops it for good.
    let (_, cancelled) = host.post(&format!("/api/job/{id}/cancel"), json!({})).await;
    assert_eq!(cancelled["cancelled"], true, "{cancelled}");
    assert_eq!(cancelled["job"]["state"], "cancelled");
    assert_eq!(host.served.runner().running(), 0);
    let at = cancelled["runs"].as_i64().unwrap();
    tokio::time::sleep(Duration::from_millis(1300)).await;
    let (_, after) = host.get_json(&format!("/api/job/{id}")).await;
    assert_eq!(after["runs"].as_i64().unwrap(), at, "{after}");
    // A cancelled routine is still listed.
    let (_, listed) = host.get_json("/api/job").await;
    assert!(
        listed["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|j| j["job"]["id"] == id.as_str() && j["job"]["state"] == "cancelled"),
        "{listed}"
    );
    host.served.shutdown().await;
}

fn routine_row(verb: &str) -> impress_item::Item {
    use impress_item::Value as V;
    let now = chrono_now();
    impress_item::Item {
        id: uuid::Uuid::new_v4(),
        schema: impress_item::schema::refs::TASK,
        payload: [
            ("kind", V::String("routine".into())),
            ("verb", V::String(verb.into())),
            ("args", V::String("{}".into())),
            ("every_ms", V::Int(1000)),
            ("runs", V::Int(0)),
            ("state", V::String("running".into())),
            ("caller", V::String("agent:http".into())),
            ("started", V::String(now.to_rfc3339())),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
        created: now,
        modified: now,
        author: "agent:http".into(),
        author_kind: impress_item::ActorKind::Agent,
        logical_clock: 0,
        origin: None,
        canonical_id: None,
        tags: vec![],
        flag: None,
        is_read: false,
        is_starred: false,
        priority: impress_item::Priority::None,
        visibility: impress_item::Visibility::Private,
        message_type: None,
        produced_by: None,
        version: None,
        batch_id: None,
        references: vec![],
        parent: None,
    }
}

fn chrono_now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

#[tokio::test]
async fn a_routine_left_running_is_re_armed_and_one_without_its_verb_is_interrupted() {
    let _turn = TURN.lock().await;
    let store_root = tempfile::tempdir().unwrap();
    let url = format!("dir:{}", store_root.path().display());

    let host = start(&url, "rearm-app").await;
    let (_, started) = host
        .post(
            "/api/verb/host-fixture_tick",
            json!({ "adverbs": { "every": "1s" } }),
        )
        .await;
    let id = started["job"]["id"].as_str().unwrap().to_string();
    let before = job_until(&host, &id, |j| j["runs"].as_i64().unwrap_or(0) >= 1).await;
    let ran = before["runs"].as_i64().unwrap();
    // A routine whose verb this binary does not link.
    let orphan = routine_row("host-fixture_gone");
    let orphan_id = orphan.id.to_string();
    koine_store::process::current()
        .unwrap()
        .insert(orphan)
        .unwrap();
    host.served.shutdown().await;

    // The host stopped without cancelling the routine: the row still says so.
    let host = start(&url, "rearm-app").await;
    let after = job_until(&host, &id, |j| j["runs"].as_i64().unwrap_or(0) > ran).await;
    assert_eq!(after["job"]["state"], "running", "{after}");
    assert_eq!(host.served.runner().running(), 1);
    let (_, orphan) = host.get_json(&format!("/api/job/{orphan_id}")).await;
    assert_eq!(orphan["job"]["state"], "interrupted", "{orphan}");
    assert!(
        orphan["message"]
            .as_str()
            .unwrap()
            .contains("host-fixture_gone is not linked"),
        "{orphan}"
    );
    host.served.shutdown().await;
}

#[tokio::test]
async fn a_job_reports_progress_that_status_shows() {
    let _turn = TURN.lock().await;
    let host = start("memory:", "progress-app").await;
    let (_, started) = host
        .post(
            "/api/verb/host-fixture_work",
            json!({ "adverbs": { "in_background": true } }),
        )
        .await;
    let id = started["job"]["id"].as_str().unwrap().to_string();
    // While it works the report is already on the row.
    let working = job_until(&host, &id, |j| j.get("progress").is_some()).await;
    assert_eq!(working["job"]["state"], "running", "{working}");
    assert_eq!(working["progress"], 0.5);
    assert_eq!(working["progress_message"], "half");
    // And it is still there when the job has ended.
    let done = job_until(&host, &id, |j| j["job"]["state"] != "running").await;
    assert_eq!(done["job"]["state"], "completed", "{done}");
    assert_eq!(done["progress"], 0.5);
    assert_eq!(done["progress_message"], "half");
    host.served.shutdown().await;
}

/// Poll `GET /api/job/<id>/lines` until its `total` is `total`.
async fn lines_until(host: &Host, id: &str, total: u64) -> Value {
    let mut last = Value::Null;
    for _ in 0..300 {
        let (status, lines) = host.get_json(&format!("/api/job/{id}/lines?after=0")).await;
        assert_eq!(status, 200, "{lines}");
        if lines["total"] == total {
            return lines;
        }
        last = lines;
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the job never emitted {total} lines: {last}");
}

#[tokio::test]
async fn a_job_that_emits_is_a_stream_the_lines_route_pages() {
    let _turn = TURN.lock().await;
    let host = start("memory:", "stream-app").await;
    let (status, started) = host
        .post(
            "/api/verb/surface-demo-service_tick",
            json!({ "n": 5, "every_ms": 10, "adverbs": { "in_background": true } }),
        )
        .await;
    assert_eq!(status, 200, "{started}");
    let id = started["job"]["id"].as_str().unwrap().to_string();

    let all = lines_until(&host, &id, 5).await;
    assert_eq!(all["ok"], true);
    assert_eq!(all["wire_version"], 1);
    assert_eq!(all["dropped"], 0);
    assert_eq!(all["next"], 5);
    let lines = all["lines"].as_array().unwrap();
    let indices: Vec<u64> = lines.iter().map(|l| l["i"].as_u64().unwrap()).collect();
    assert_eq!(indices, vec![1, 2, 3, 4, 5], "{all}");
    assert!(lines.iter().all(|l| l["at"].is_u64()));

    // `after` pages: from 3 on is the last two; at the end is empty.
    let (_, tail) = host.get_json(&format!("/api/job/{id}/lines?after=3")).await;
    let tail_lines = tail["lines"].as_array().unwrap();
    assert_eq!(tail_lines, &lines[3..]);
    assert_eq!(
        (tail["next"].as_u64(), tail["total"].as_u64()),
        (Some(5), Some(5))
    );
    let (_, none) = host.get_json(&format!("/api/job/{id}/lines?after=5")).await;
    assert_eq!(none["lines"], json!([]));
    assert_eq!(none["next"], 5);
    let (status, bad) = host.get_json(&format!("/api/job/{id}/lines?after=x")).await;
    assert_eq!(status, 400, "{bad}");

    // The job's own row shows the count, and it ended after the last line.
    let done = job_until(&host, &id, |j| j["job"]["state"] != "running").await;
    assert_eq!(done["job"]["state"], "completed", "{done}");
    assert_eq!(done["line_count"], 5);
    assert_eq!(done["lines_held"], 5);
    assert_eq!(done["progress_message"], "tick 5/5");
    assert_eq!(done["result"]["emitted"], 5);
    assert!(all["state"].is_string(), "{all}");
    let (_, list) = host.get_json("/api/job").await;
    assert!(list["jobs"].is_array());

    // The unknown job is not-found, a bad id is invalid.
    let (status, none) = host
        .get_json("/api/job/00000000-0000-4000-8000-000000000000/lines")
        .await;
    assert_eq!(status, 404, "{none}");
    assert_eq!(none["code"], "not-found");
    let (status, _) = host.get_json("/api/job/nope/lines").await;
    assert_eq!(status, 400);
    host.served.shutdown().await;
}

#[tokio::test]
async fn a_flood_keeps_the_last_thousand_lines_and_counts_the_rest() {
    let _turn = TURN.lock().await;
    let host = start("memory:", "flood-app").await;
    let (_, started) = host
        .post(
            "/api/verb/host-fixture_flood",
            json!({ "ms": 3000, "adverbs": { "in_background": true } }),
        )
        .await;
    let id = started["job"]["id"].as_str().unwrap().to_string();
    let all = lines_until(&host, &id, 3000).await;
    let lines = all["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 1000, "the ring keeps the last thousand");
    assert_eq!(all["dropped"], 2000);
    assert_eq!(lines[0], json!({ "i": 2000 }));
    assert_eq!(lines[999], json!({ "i": 2999 }));
    assert_eq!(all["next"], 3000);
    // Asking from inside the ring drops nothing.
    let (_, inside) = host
        .get_json(&format!("/api/job/{id}/lines?after=2990"))
        .await;
    assert_eq!(inside["dropped"], 0);
    assert_eq!(inside["lines"].as_array().unwrap().len(), 10);
    let done = job_until(&host, &id, |j| j["job"]["state"] != "running").await;
    assert_eq!(done["line_count"], 3000);
    assert_eq!(done["lines_held"], 1000);
    host.served.shutdown().await;
}

// ------------------------------------------------------------------------ feed

/// A surface whose one source reads every `surface` row.
fn watcher() -> Value {
    json!({
        "surface": "1.0",
        "name": "Watcher",
        "sources": { "surfaces": { "query": {
            "kinds": ["surface"], "scope": { "scope": "all" }, "limit": 10 } } },
        "root": { "column": [
            { "text": "# Watching" },
            { "id": "t", "table": { "rows": "{{source.surfaces}}", "columns": ["name"] } }
        ] }
    })
}

/// Create and render the watcher (so the registry knows what it reads), and
/// start a long `surface_wait` on it.
async fn wait_on_a_watcher(
    host: &Host,
) -> (
    String,
    tokio::task::JoinHandle<(StatusCode, Value, Duration)>,
) {
    let (status, created) = host.post("/api/surface", watcher()).await;
    assert_eq!(status, 200, "{created}");
    let id = created["id"].as_str().unwrap().to_string();
    let (status, rendered) = host.get_json(&format!("/api/surface/{id}/render")).await;
    assert_eq!(status, 200, "{rendered}");
    assert!(
        rendered["source_errors"]
            .as_array()
            .is_none_or(|e| e.is_empty()),
        "{rendered}"
    );
    let client = host.client.clone();
    let url = host.url(&format!(
        "/api/surface/{id}/wait?after_seq=0&timeout_ms=30000"
    ));
    let waiting = tokio::spawn(async move {
        let started = std::time::Instant::now();
        let (status, body) = json_of(client.get(url).send().await.unwrap()).await;
        (status, body, started.elapsed())
    });
    // Let the wait take its reading before anything is written.
    tokio::time::sleep(Duration::from_millis(150)).await;
    (id, waiting)
}

#[tokio::test]
async fn a_store_write_wakes_a_surface_that_reads_it() {
    let _turn = TURN.lock().await;
    let host = start("memory:", "wake-app").await;
    let (_, waiting) = wait_on_a_watcher(&host).await;

    // Any write of a kind the surface reads: a second surface.
    let (status, created) = host.post("/api/surface", explorer()).await;
    assert_eq!(status, 200, "{created}");

    let (status, woken, took) = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("the wait ended")
        .unwrap();
    assert_eq!(status, 200, "{woken}");
    assert_eq!(woken["ok"], true);
    assert_eq!(woken["invalidated"], true, "{woken}");
    assert_eq!(woken["events"], json!([]));
    assert_eq!(woken["next_seq"], 0);
    assert_eq!(woken["timed_out"], false);
    assert!(took < Duration::from_secs(2), "woke in {took:?}");

    // A wait that is not woken keeps the old wire: no `invalidated` key.
    let id = created["id"].as_str().unwrap();
    let (_, quiet) = host
        .get_json(&format!(
            "/api/surface/{id}/wait?after_seq=0&timeout_ms=100"
        ))
        .await;
    assert_eq!(quiet["timed_out"], true, "{quiet}");
    assert!(quiet.get("invalidated").is_none(), "{quiet}");
    host.served.shutdown().await;
}

#[tokio::test]
async fn another_process_writing_the_same_directory_wakes_it_too() {
    let _turn = TURN.lock().await;
    let store_root = tempfile::tempdir().unwrap();
    let url = format!("dir:{}", store_root.path().display());
    let host = start(&url, "wake-dir-app").await;
    let (id, waiting) = wait_on_a_watcher(&host).await;

    // A second handle on the same root: what a second process would hold.
    let other = koine_store::open(&url).expect("a second handle opens");
    // Let its watcher finish its first scan before it writes.
    tokio::time::sleep(Duration::from_millis(400)).await;
    other
        .update(
            id.parse().unwrap(),
            vec![impress_item::FieldMutation::SetStarred(true)],
        )
        .expect("the second handle writes");

    let (status, woken, took) = tokio::time::timeout(Duration::from_secs(10), waiting)
        .await
        .expect("the wait ended")
        .unwrap();
    assert_eq!(status, 200, "{woken}");
    assert_eq!(woken["invalidated"], true, "{woken}");
    assert!(took < Duration::from_secs(8), "woke in {took:?}");
    drop(other);
    host.served.shutdown().await;
}

// ------------------------------------------------------------ host/network

/// This machine's address on a route to anywhere, when it has one (the UDP
/// connect sends nothing). A request to it arrives from that same address:
/// a peer that is not loopback.
fn lan_ip() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

async fn refused(client: &Client, url: &str) -> bool {
    match client.get(url).timeout(Duration::from_secs(3)).send().await {
        Err(error) => error.is_connect(),
        Ok(_) => false,
    }
}

#[tokio::test]
async fn the_network_toggle_opens_and_closes_a_second_listener() {
    let _turn = TURN.lock().await;
    let host = start("memory:", "network-toggle-app").await;

    let (status, closed) = host.get_json("/api/host/network").await;
    assert_eq!(status, StatusCode::OK, "{closed}");
    assert_eq!(closed["allowed"], false, "{closed}");
    assert_eq!(closed["started_in_network_mode"], false, "{closed}");
    assert!(closed.get("addr").is_none() && closed.get("bearer").is_none());

    // A mutation needs the loopback token, like every other.
    let bare = host
        .client
        .post(host.url("/api/host/network"))
        .json(&json!({ "allow": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(bare.status(), StatusCode::UNAUTHORIZED);
    for bad in [json!({}), json!({ "allow": true, "port": 1 })] {
        let (status, body) = host.post("/api/host/network", bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["code"], "invalid-argument");
    }

    let (status, opened) = host
        .post(
            "/api/host/network",
            json!({ "allow": true, "bearer": "lan-bearer" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{opened}");
    assert_eq!(opened["allowed"], true);
    assert_eq!(opened["bearer"], "lan-bearer");
    let addr = opened["addr"].as_str().unwrap().to_string();
    assert!(addr.starts_with("0.0.0.0:"), "{addr}");
    let port = opened["port"].as_u64().unwrap();
    let net = format!("http://127.0.0.1:{port}");

    // The loopback peer is treated exactly as on the first listener.
    let status = host.client.get(format!("{net}/api/status")).send().await;
    assert_eq!(status.unwrap().status(), StatusCode::OK);
    let no_token = host
        .client
        .post(format!("{net}/api/host/network"))
        .json(&json!({ "allow": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(no_token.status(), StatusCode::UNAUTHORIZED);
    let on_net = host
        .client
        .post(format!("{net}/api/verb/layout-service_layout-tree"))
        .bearer_auth(host.served.token())
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_ne!(on_net.status(), StatusCode::UNAUTHORIZED);

    // The state names the address and never the bearer.
    let (_, state) = host.get_json("/api/host/network").await;
    assert_eq!(state["allowed"], true, "{state}");
    assert_eq!(state["addr"], addr);
    assert!(state.get("bearer").is_none(), "{state}");

    // Opening again is the same listener; a bearer in the body rotates.
    let (_, again) = host
        .post("/api/host/network", json!({ "allow": true }))
        .await;
    assert_eq!(again["addr"], opened["addr"], "{again}");
    assert_eq!(again["bearer"], "lan-bearer");
    let (_, rotated) = host
        .post(
            "/api/host/network",
            json!({ "allow": true, "bearer": "next" }),
        )
        .await;
    assert_eq!(rotated["addr"], opened["addr"], "{rotated}");
    assert_eq!(rotated["bearer"], "next");

    let (status, off) = host
        .post("/api/host/network", json!({ "allow": false }))
        .await;
    assert_eq!(status, StatusCode::OK, "{off}");
    assert_eq!(off["allowed"], false);
    // The abort lands on the runtime; give it a moment to drop the socket.
    let mut gone = false;
    for _ in 0..50 {
        if refused(&host.client, &format!("{net}/api/status")).await {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(gone, "the network listener still answers");
    let (_, state) = host.get_json("/api/host/network").await;
    assert_eq!(state["allowed"], false, "{state}");
    // The first listener never stopped.
    assert_eq!(host.get("/api/status").await.status(), StatusCode::OK);
    // Closing twice is fine.
    let (status, _) = host
        .post("/api/host/network", json!({ "allow": false }))
        .await;
    assert_eq!(status, StatusCode::OK);
    host.served.shutdown().await;
}

#[tokio::test]
async fn a_remote_peer_needs_the_bearer_on_the_second_listener() {
    let _turn = TURN.lock().await;
    let Some(ip) = lan_ip() else {
        eprintln!("skipped: this machine has no non-loopback address to dial");
        return;
    };
    let host = start("memory:", "network-remote-app").await;
    let (_, opened) = host
        .post(
            "/api/host/network",
            json!({ "allow": true, "bearer": "lan-bearer" }),
        )
        .await;
    let port = opened["port"].as_u64().unwrap();
    let remote = format!("http://{ip}:{port}");
    let call = |bearer: Option<&'static str>, method: Method, path: &'static str| {
        let mut request = host.client.request(method, format!("{remote}{path}"));
        if let Some(bearer) = bearer {
            request = request.bearer_auth(bearer);
        }
        request.json(&json!({ "allow": false })).send()
    };

    let no_bearer = call(None, Method::GET, "/api/status").await.unwrap();
    assert_eq!(no_bearer.status(), StatusCode::UNAUTHORIZED);
    // The loopback token is not the network bearer.
    let token: &'static str = Box::leak(host.served.token().to_string().into_boxed_str());
    let wrong = call(Some(token), Method::GET, "/api/status").await.unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    let ok = call(Some("lan-bearer"), Method::GET, "/api/status")
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    // A remote peer cannot close the door it came through.
    let (status, body) = json_of(
        call(Some("lan-bearer"), Method::POST, "/api/host/network")
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "loopback-only");

    // Closing it refuses the next request even on a live connection, and the
    // port stops answering.
    let (_, off) = host
        .post("/api/host/network", json!({ "allow": false }))
        .await;
    assert_eq!(off["allowed"], false);
    let after = call(Some("lan-bearer"), Method::GET, "/api/status").await;
    assert!(
        after.map_or(true, |r| r.status() == StatusCode::UNAUTHORIZED),
        "a closed listener must not admit the bearer"
    );
    host.served.shutdown().await;
}
