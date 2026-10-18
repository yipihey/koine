//! Small, product-neutral MCP host over the generated Impress service inventory.
//!
//! Product binaries choose which service crates to link and which tool-name
//! prefixes to expose. The host owns JSON-RPC/MCP mechanics only.
//!
//! This is `impress-mcp-host` moved to koine (docs/plan-koine-host.md H-D5),
//! unchanged in behaviour, with two additions: a `tools/list` entry carries
//! the verb's `_meta` (`returns_kind` H-D8, `rows` and `secrets` plan-koine-followups
//! F-D2 and F-D1) when it declares any, and
//! [`handle_message`] is the public form of the triage both transports apply,
//! for a host that mounts `/mcp` inside its own server (`koine-host`).

mod providers;

use std::io::{BufRead, Write};
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
#[cfg(test)]
use axum::http::HeaderValue;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
#[cfg(test)]
use impress_service_core::McpToolDescriptor;
use impress_service_core::{
    call,
    pipeline::{Call, CallerIdentity},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resource {
    pub uri: String,
    pub name: String,
    pub description: String,
    pub mime_type: String,
    pub text: String,
    /// Optional MCP Apps metadata attached to both resource discovery and the
    /// resource contents returned by `resources/read`.
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolUi {
    pub tool_name: String,
    pub resource_uri: String,
    pub invoking: String,
    pub invoked: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolFileParams {
    pub tool_name: String,
    pub params: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostConfig {
    pub server_name: String,
    pub server_version: String,
    pub instructions: String,
    /// A descriptor is exposed when its name starts with one of these. Empty
    /// means expose the full linked inventory.
    pub allowed_tool_prefixes: Vec<String>,
    pub resources: Vec<Resource>,
    /// UI bindings remain a host concern: generated service descriptors stay
    /// product-neutral while a focused product can choose its presentation.
    pub tool_ui: Vec<ToolUi>,
    /// Top-level tool arguments that ChatGPT may populate from files shared in
    /// the conversation. The parameter schemas still come from Rust types.
    pub tool_file_params: Vec<ToolFileParams>,
}

impl HostConfig {
    pub fn allows(&self, name: &str) -> bool {
        self.allowed_tool_prefixes.is_empty()
            || self
                .allowed_tool_prefixes
                .iter()
                .any(|prefix| name.starts_with(prefix))
    }
}

pub fn run_stdio(config: HostConfig) -> Result<(), Box<dyn std::error::Error>> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) => {
                writeln!(
                    stdout,
                    "{}",
                    json_rpc_error(Value::Null, -32700, format!("Parse error: {error}"))
                )?;
                stdout.flush()?;
                continue;
            }
        };
        if let Some(response) = dispatch_message(&config, &request) {
            writeln!(stdout, "{response}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

/// Triage one parsed top-level message for either transport. Returns the
/// single response to send, or `None` for a notification (a message without
/// an `id` — JSON-RPC 2.0 forbids replying to those, and clients do send
/// them: Claude Code emits `notifications/roots/list_changed`).
///
/// A top-level array is a JSON-RPC batch, which this host does not implement
/// — the reason it pins protocolVersion 2024-11-05 rather than 2025-03-26,
/// whose batch support is mandatory. Dropping a batch silently left the
/// client waiting forever; refuse it loudly instead, with the null id
/// JSON-RPC prescribes when no single request id can be extracted.
fn dispatch_message(config: &HostConfig, request: &Value) -> Option<Value> {
    dispatch_message_as(
        config,
        request,
        &CallerIdentity::agent(format!("mcp-host:{}", config.server_name)),
        None,
        None,
    )
}

fn dispatch_message_as(
    config: &HostConfig,
    request: &Value,
    caller: &CallerIdentity,
    trace: Option<String>,
    parent: Option<String>,
) -> Option<Value> {
    if request.is_array() {
        return Some(json_rpc_error(
            Value::Null,
            -32600,
            "JSON-RPC batch requests are not supported".into(),
        ));
    }
    request.get("id")?;
    Some(handle_request_as(config, request, caller, trace, parent))
}

/// Triage and answer one parsed top-level message as `caller`, for a host
/// that carries MCP inside its own server: `Some(response)` for a request
/// (a batch gets its one `-32600` error), `None` for a notification, which
/// JSON-RPC forbids replying to (answer it `202 Accepted`).
///
/// Synchronous, and a `tools/call` runs the verb to completion with
/// `runtime::block_on`, so call it from a thread that may block
/// (`tokio::task::spawn_blocking`), never from a runtime worker.
pub fn handle_message(
    config: &HostConfig,
    request: &Value,
    caller: &CallerIdentity,
) -> Option<Value> {
    dispatch_message_as(config, request, caller, None, None)
}

/// Run the same focused MCP surface as a stateless Streamable HTTP endpoint.
///
/// Authentication is intentionally owned by the transport rather than domain
/// services. The caller supplies a high-entropy bearer token and may safely
/// place this endpoint behind a public HTTPS reverse proxy.
pub fn run_http(
    config: HostConfig,
    bind: &str,
    bearer_token: String,
) -> Result<(), Box<dyn std::error::Error>> {
    if bearer_token.trim().is_empty() {
        return Err("the MCP HTTP bearer token must not be empty".into());
    }
    let address: SocketAddr = bind.parse()?;
    let state = HttpState {
        config: Arc::new(config),
        bearer_token: Arc::from(bearer_token),
    };
    impress_service_core::runtime::block_on(async move {
        let app = Router::new()
            // Serving both paths makes the host work directly and behind a
            // reverse proxy that strips its configured path prefix.
            .route("/", post(http_mcp))
            .route("/mcp", post(http_mcp))
            .route("/healthz", get(http_health))
            .route("/api/providers/register", post(providers::register))
            .route("/api/providers/deregister", post(providers::deregister))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind(address).await?;
        eprintln!("koine-mcp-host: listening on http://{address}/mcp");
        // Read-only probes begin immediately: they do not mutate user records or
        // emit store notifications. The task belongs to this server runtime.
        let health = tokio::spawn(async {
            loop {
                impress_service_core::registry_runtime::refresh_health().await;
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });
        let outcome = axum::serve(listener, app).await;
        health.abort();
        outcome?;
        Ok::<(), Box<dyn std::error::Error>>(())
    })
}

#[derive(Clone)]
struct HttpState {
    config: Arc<HostConfig>,
    bearer_token: Arc<str>,
}

async fn http_health() -> impl IntoResponse {
    Json(json!({ "service": "impress-mcp", "status": "ok" }))
}

async fn http_mcp(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    let Some(caller) = providers::identity(&state, &headers).await else {
        return providers::unauthorized();
    };
    let trace = headers
        .get("traceparent")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let parent = headers
        .get("x-impress-parent-call")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let config = state.config.clone();
    // Same triage as stdio (`dispatch_message`): a batch gets its one error
    // response, a notification gets the bodiless 202 Streamable HTTP
    // prescribes, everything else is handled.
    match tokio::task::spawn_blocking(move || {
        dispatch_message_as(&config, &request, &caller, trace, parent)
    })
    .await
    {
        Ok(Some(response)) => (StatusCode::OK, Json(response)).into_response(),
        Ok(None) => StatusCode::ACCEPTED.into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("MCP request task failed: {error}") })),
        )
            .into_response(),
    }
}

fn has_bearer_token(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|value| value == expected)
}

pub fn handle_request(config: &HostConfig, request: &Value) -> Value {
    handle_request_as(
        config,
        request,
        &CallerIdentity::agent(format!("mcp-host:{}", config.server_name)),
        None,
        None,
    )
}

fn handle_request_as(
    config: &HostConfig,
    request: &Value,
    caller: &CallerIdentity,
    trace: Option<String>,
    parent: Option<String>,
) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    match request.get("method").and_then(Value::as_str).unwrap_or("") {
        "initialize" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                // 2024-11-05, like impress-mcp's stdio server: the 2025-03-26
                // revision makes JSON-RPC batching mandatory, and this host
                // refuses batches (see `dispatch_message`), so claiming it
                // would advertise support that is not there.
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {}, "resources": {} },
                "serverInfo": {
                    "name": config.server_name,
                    "version": config.server_version,
                },
                "instructions": config.instructions,
            }
        }),
        "ping" => success(id, json!({})),
        "tools/list" => success(id, json!({ "tools": tool_definitions(config) })),
        "tools/call" => handle_tool_call(config, id, request, caller, trace, parent),
        "resources/list" => success(
            id,
            json!({
                "resources": config.resources.iter().map(|resource| {
                    let mut descriptor = json!({
                        "uri": resource.uri,
                        "name": resource.name,
                        "description": resource.description,
                        "mimeType": resource.mime_type,
                    });
                    if let Some(meta) = &resource.meta {
                        descriptor["_meta"] = meta.clone();
                    }
                    descriptor
                }).collect::<Vec<_>>()
            }),
        ),
        "resources/read" => handle_resource_read(config, id, request),
        method => json_rpc_error(id, -32601, format!("Method not found: {method}")),
    }
}

pub fn tool_definitions(config: &HostConfig) -> Vec<Value> {
    let mut descriptors: Vec<_> = call::descriptors()
        .filter(|descriptor| config.allows(descriptor.name()))
        .collect();
    descriptors.sort_by(|a, b| a.name().cmp(b.name()));
    descriptors
        .into_iter()
        .map(|descriptor| {
            let mut definition = json!({
                "name": descriptor.name(),
                "description": descriptor.description(),
                "inputSchema": impress_service_core::with_adverbs_property(descriptor.input_schema().into_owned()),
                "annotations": descriptor.mcp_annotations(),
            });
            if let Some(ui) = config
                .tool_ui
                .iter()
                .find(|ui| ui.tool_name == descriptor.name())
            {
                definition["_meta"] = json!({
                    "ui": {
                        "resourceUri": ui.resource_uri,
                        "visibility": ["model", "app"],
                    },
                    // Compatibility aliases for ChatGPT clients predating the
                    // shared MCP Apps fields.
                    "openai/outputTemplate": ui.resource_uri,
                    "openai/toolInvocation/invoking": ui.invoking,
                    "openai/toolInvocation/invoked": ui.invoked,
                });
            }
            // `returns_kind` (plan-koine-host H-D8), `rows` and `secrets`
            // (plan-koine-followups F-D2, F-D1): additive beside any UI
            // binding.
            if let Some(Value::Object(extra)) = descriptor.mcp_meta() {
                let meta = definition
                    .as_object_mut()
                    .expect("tool definition is an object")
                    .entry("_meta")
                    .or_insert_with(|| json!({}));
                for (key, value) in extra {
                    meta[key] = value;
                }
            }
            // plan-koine-trust TR-D2: what a grant for this tool was made
            // against; a changed fingerprint is a changed tool.
            definition
                .as_object_mut()
                .expect("tool definition is an object")
                .entry("_meta")
                .or_insert_with(|| json!({}))["fingerprint"] = json!(descriptor.fingerprint());
            if let Some(file_params) = config
                .tool_file_params
                .iter()
                .find(|entry| entry.tool_name == descriptor.name())
            {
                let meta = definition
                    .as_object_mut()
                    .expect("tool definition is an object")
                    .entry("_meta")
                    .or_insert_with(|| json!({}));
                meta["openai/fileParams"] = json!(file_params.params);
                definition["annotations"] = json!({
                    "readOnlyHint": false,
                    "openWorldHint": true,
                    "destructiveHint": false,
                });
            }
            definition
        })
        .collect()
}

fn handle_tool_call(
    config: &HostConfig,
    id: Value,
    request: &Value,
    caller: &CallerIdentity,
    trace: Option<String>,
    parent: Option<String>,
) -> Value {
    let Some(name) = request
        .pointer("/params/name")
        .and_then(serde_json::Value::as_str)
    else {
        return json_rpc_error(id, -32602, "params.name is required".into());
    };
    if !config.allows(name) {
        return tool_result(
            id,
            Err(format!("Tool is not exposed by this profile: {name}")),
        );
    }
    let Some(descriptor) = call::find(name) else {
        return tool_result(id, Err(format!("Unknown tool: {name}")));
    };
    // MCP clients may omit `arguments` entirely or send an explicit null;
    // the generated deserializers expect an object, so both become `{}` and
    // zero-argument tools work either way — the same coercion impress-mcp's
    // stdio server applies.
    let arguments = match request.pointer("/params/arguments") {
        Some(value) if !value.is_null() => value.clone(),
        _ => json!({}),
    };
    // A hosted MCP client is an agent named by its profile (ADR-0034 D3).
    let mut call = Call::new(caller.clone(), arguments);
    call.trace_id = trace;
    call.parent_call = parent;
    let result = impress_service_core::runtime::block_on(
        impress_service_core::pipeline::invoke_handle(descriptor, call),
    )
    .map_err(|error| match error {
        impress_service_core::pipeline::PipelineError::Handler(e) => e.to_string(),
        unavailable => unavailable.to_string(),
    });
    tool_result(id, result)
}

fn handle_resource_read(config: &HostConfig, id: Value, request: &Value) -> Value {
    let Some(uri) = request
        .pointer("/params/uri")
        .and_then(serde_json::Value::as_str)
    else {
        return json_rpc_error(id, -32602, "params.uri is required".into());
    };
    let Some(resource) = config.resources.iter().find(|resource| resource.uri == uri) else {
        return json_rpc_error(id, -32002, format!("Resource not found: {uri}"));
    };
    let mut content = json!({
        "uri": resource.uri,
        "mimeType": resource.mime_type,
        "text": resource.text,
    });
    if let Some(meta) = &resource.meta {
        content["_meta"] = meta.clone();
    }
    success(id, json!({ "contents": [content] }))
}

fn tool_result(id: Value, result: Result<Value, String>) -> Value {
    match result {
        Ok(value) => {
            let (mut content, raw) = impress_service_core::split_mcp_content(value);
            // The human-readable text block keeps the RAW pre-envelope shape
            // (a bare string stays bare, everything else compact JSON) — the
            // rule impress-mcp's stdio server applies, and compact because a
            // pretty print duplicates structuredContent at a 30-80% token
            // premium on every result.
            let text = match &raw {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            // MCP requires `structuredContent` to be an object; generated
            // handlers may return arrays, scalars or null (Vec<T>, counts,
            // Option<T> misses), so envelope at the transport like
            // impress-mcp's stdio server does.
            let structured = impress_service_core::envelope_structured_content(raw);
            content.push(json!({ "type": "text", "text": text }));
            let is_error = structured
                .get("ok")
                .and_then(Value::as_bool)
                .is_some_and(|ok| !ok);
            success(
                id,
                json!({
                    "content": content,
                    "structuredContent": structured,
                    "isError": is_error,
                }),
            )
        }
        Err(message) => success(
            id,
            json!({
                "content": [{ "type": "text", "text": message }],
                "isError": true,
            }),
        ),
    }
}

fn success(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn json_rpc_error(id: Value, code: i64, message: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // A test-only inventory entry: this crate deliberately links no service
    // crates, so give `tools/call` something real to dispatch to.
    static FIXTURE: impress_service_core::VerbDescriptor = impress_service_core::VerbDescriptor {
        name: "allowed-fixture_echo-arguments",
        service: "allowed-fixture",
        method: "echo-arguments",
        description: "Test fixture: echoes the argument object it was handed.",
        input_schema: fixture_schema,
        output_schema: fixture_schema,
        safety: impress_service_core::Safety {
            class: impress_service_core::SafetyClass::ReadOnly,
            idempotent: true,
        },
        since: "0.1.0",
        effects: impress_service_core::Effects::NONE,
        deprecated: None,
        aliases: &[],
        supports: &[],
        adjective: None,
        examples: &[],
        strict: false,
        budget_ms: None,
        replay_full: false,
        returns_kind: None,
        rows: None,
        secrets: &[],
        source: impress_service_core::Source::Linked,
        handler: fixture_echo,
    };
    impress_service_core::inventory::submit! { McpToolDescriptor::of(&FIXTURE) }

    // A second fixture that declares the kind of its rows (H-D8).
    static KINDED: impress_service_core::VerbDescriptor = impress_service_core::VerbDescriptor {
        name: "allowed-fixture_list-papers",
        service: "allowed-fixture",
        method: "list-papers",
        description: "Test fixture: declares that it returns papers.",
        returns_kind: Some("paper"),
        ..FIXTURE
    };
    impress_service_core::inventory::submit! { McpToolDescriptor::of(&KINDED) }

    // A third that also names the id of its rows and needs a secret
    // (plan-koine-followups F-D1, F-D2).
    static SEARCHING: impress_service_core::VerbDescriptor = impress_service_core::VerbDescriptor {
        name: "allowed-fixture_search-papers",
        service: "allowed-fixture",
        method: "search-papers",
        description: "Test fixture: rows with an id, and a secret.",
        output_schema: papers_schema,
        returns_kind: Some("paper"),
        rows: Some(impress_service_core::RowSchema {
            id: "bibcode",
            title: Some("title"),
            columns: &[],
        }),
        secrets: &["scix-api-token"],
        ..FIXTURE
    };
    impress_service_core::inventory::submit! { McpToolDescriptor::of(&SEARCHING) }

    fn papers_schema() -> Value {
        json!({"type": "array", "items": {"type": "object", "properties": {
            "bibcode": {"type": "string"}, "title": {"type": "string"}, "year": {"type": "integer"}
        }}})
    }

    fn fixture_schema() -> Value {
        json!({ "type": "object" })
    }

    fn fixture_echo(arguments: Value) -> impress_service_core::ServiceFuture {
        Box::pin(async move { Ok(json!({ "ok": true, "arguments": arguments })) })
    }

    fn config() -> HostConfig {
        HostConfig {
            server_name: "test".into(),
            server_version: "1".into(),
            instructions: "test host".into(),
            allowed_tool_prefixes: vec!["allowed-".into()],
            resources: vec![Resource {
                uri: "impress://test/guide".into(),
                name: "Guide".into(),
                description: "Test".into(),
                mime_type: "text/plain".into(),
                text: "hello".into(),
                meta: None,
            }],
            tool_ui: Vec::new(),
            tool_file_params: Vec::new(),
        }
    }

    #[test]
    fn profile_prefix_is_an_allowlist() {
        let config = config();
        assert!(config.allows("allowed-tool_call"));
        assert!(!config.allows("raw-store_delete"));
    }

    #[test]
    fn initializes_and_reads_profile_resource() {
        let init = handle_request(
            &config(),
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "test");
        // Pinned to impress-mcp's revision: 2025-03-26 makes batching
        // mandatory and this host refuses batches.
        assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
        let read = handle_request(
            &config(),
            &json!({
                "jsonrpc":"2.0",
                "id":2,
                "method":"resources/read",
                "params":{"uri":"impress://test/guide"}
            }),
        );
        assert_eq!(read["result"]["contents"][0]["text"], "hello");
    }

    /// `tools/list` carries each descriptor's annotations (ADR-0034 D1), read
    /// off the same record impress-mcp's flat and grouped listings read.
    #[test]
    fn tools_list_carries_the_descriptor_annotations() {
        let response = handle_request(
            &config(),
            &json!({"jsonrpc":"2.0","id":4,"method":"tools/list"}),
        );
        let tools = response["result"]["tools"]
            .as_array()
            .expect("tools is an array");
        let fixture = tools
            .iter()
            .find(|t| t["name"] == "allowed-fixture_echo-arguments")
            .expect("the fixture is listed");
        assert_eq!(fixture["annotations"], FIXTURE.mcp_annotations());
        assert_eq!(
            fixture["annotations"],
            json!({"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false})
        );
    }

    /// `_meta.returns_kind` rides on exactly the verbs that declare one, and
    /// does not displace a UI binding's `_meta`.
    #[test]
    fn tools_list_carries_returns_kind_only_where_declared() {
        let mut config = config();
        config.tool_ui = vec![ToolUi {
            tool_name: "allowed-fixture_list-papers".into(),
            resource_uri: "ui://papers".into(),
            invoking: "Listing".into(),
            invoked: "Listed".into(),
        }];
        let tools = tool_definitions(&config);
        let find = |name: &str| tools.iter().find(|t| t["name"] == name).unwrap().clone();
        let kinded = find("allowed-fixture_list-papers");
        assert_eq!(kinded["_meta"]["returns_kind"], "paper");
        assert_eq!(kinded["_meta"]["ui"]["resourceUri"], "ui://papers");
        let plain = find("allowed-fixture_echo-arguments");
        let plain_meta = plain["_meta"].as_object().unwrap();
        assert_eq!(
            plain_meta.keys().collect::<Vec<_>>(),
            ["fingerprint"],
            "only the fingerprint, which every tool carries: {plain}"
        );
        assert_eq!(plain_meta["fingerprint"].as_str().unwrap().len(), 64);
    }

    /// `_meta.secrets` and `_meta.rows` ride beside `returns_kind` on the
    /// verb that declares them, as one object; the row `columns` default to
    /// the row object's properties rather than going out empty.
    #[test]
    fn tools_list_carries_secrets_and_rows_where_declared() {
        let tools = tool_definitions(&config());
        let find = |name: &str| tools.iter().find(|t| t["name"] == name).unwrap().clone();
        let searching = find("allowed-fixture_search-papers");
        let mut meta = searching["_meta"].clone();
        meta.as_object_mut().unwrap().remove("fingerprint");
        assert_eq!(
            meta,
            json!({
                "returns_kind": "paper",
                "secrets": ["scix-api-token"],
                "rows": {"id": "bibcode", "title": "title", "columns": ["bibcode", "title", "year"]},
            })
        );
        let kinded = find("allowed-fixture_list-papers");
        assert!(kinded["_meta"].get("secrets").is_none(), "{kinded}");
        assert!(kinded["_meta"].get("rows").is_none(), "{kinded}");
    }

    #[test]
    fn handle_message_triages_like_the_transports() {
        let caller = CallerIdentity::agent("test");
        let ping = json!({"jsonrpc":"2.0","id":1,"method":"ping"});
        assert_eq!(
            handle_message(&config(), &ping, &caller),
            Some(json!({"jsonrpc":"2.0","id":1,"result":{}}))
        );
        let note = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
        assert_eq!(handle_message(&config(), &note, &caller), None);
        let batch = json!([ping]);
        assert_eq!(
            handle_message(&config(), &batch, &caller).unwrap()["error"]["code"],
            -32600
        );
    }

    #[test]
    fn profile_refuses_non_allowlisted_tools_before_lookup() {
        let response = handle_request(
            &config(),
            &json!({
                "jsonrpc":"2.0",
                "id":3,
                "method":"tools/call",
                "params":{"name":"store-query-service_list-items","arguments":{}}
            }),
        );
        assert_eq!(response["result"]["isError"], true);
    }

    #[test]
    fn http_bearer_auth_is_exact() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret"),
        );
        assert!(has_bearer_token(&headers, "secret"));
        assert!(!has_bearer_token(&headers, "other"));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Basic secret"),
        );
        assert!(!has_bearer_token(&headers, "secret"));
    }

    /// A JSON-RPC batch used to be dropped without a byte of output (stdio)
    /// or with a bare 202 (HTTP), leaving the client waiting forever. Both
    /// transports triage through `dispatch_message`, so this covers both.
    #[test]
    fn batches_get_one_invalid_request_error_instead_of_silence() {
        let batch = json!([
            {"jsonrpc":"2.0","id":1,"method":"ping"},
            {"jsonrpc":"2.0","id":2,"method":"tools/list"}
        ]);
        let response =
            dispatch_message(&config(), &batch).expect("a batch is answered, not dropped");
        assert_eq!(response["id"], Value::Null);
        assert_eq!(response["error"]["code"], -32600);
        assert_eq!(
            response["error"]["message"],
            "JSON-RPC batch requests are not supported"
        );
    }

    /// JSON-RPC 2.0 forbids replying to notifications (id-less messages).
    #[test]
    fn notifications_get_no_reply() {
        let notification = json!({"jsonrpc":"2.0","method":"notifications/roots/list_changed"});
        assert_eq!(dispatch_message(&config(), &notification), None);
    }

    /// An explicit `"arguments": null` must reach the generated deserializer
    /// as `{}`, exactly like an omitted `arguments` — `Some(Null)` fails
    /// serde deserialization even for zero-argument tools.
    #[test]
    fn explicit_null_arguments_reach_the_handler_as_an_empty_object() {
        for params in [
            json!({"name": "allowed-fixture_echo-arguments", "arguments": null}),
            json!({"name": "allowed-fixture_echo-arguments"}),
        ] {
            let response = handle_request(
                &config(),
                &json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params": params}),
            );
            assert_eq!(response["result"]["isError"], false, "{response}");
            assert_eq!(
                response["result"]["structuredContent"]["arguments"],
                json!({}),
                "arguments must arrive as an object: {response}"
            );
        }
    }

    /// The text block derives from the RAW pre-envelope value — bare strings
    /// stay bare, everything else compact — matching impress-mcp's stdio
    /// server instead of pretty-reprinting `structuredContent`.
    #[test]
    fn text_block_is_the_raw_value_not_a_pretty_reprint() {
        let response = tool_result(json!(9), Ok(json!("Café")));
        assert_eq!(response["result"]["content"][0]["text"], "Café");
        assert_eq!(
            response["result"]["structuredContent"],
            json!({"value": "Café"})
        );

        let response = tool_result(json!(9), Ok(json!(["a", "b"])));
        assert_eq!(response["result"]["content"][0]["text"], r#"["a","b"]"#);

        let response = tool_result(json!(9), Ok(json!({"ok": false, "message": "m"})));
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(!text.contains('\n'), "text must be compact: {text:?}");
        assert_eq!(
            serde_json::from_str::<Value>(text).unwrap(),
            json!({"ok": false, "message": "m"})
        );
        // `isError` keeps reading the post-envelope structured value.
        assert_eq!(response["result"]["isError"], true);
    }

    #[test]
    fn non_object_results_are_enveloped_into_objects() {
        // MCP clients reject a structuredContent that is not an object;
        // Vec<T>-, Option<T>- and scalar-returning generated handlers produce
        // exactly those shapes, so the transport envelopes them.
        for (raw, expected) in [
            (json!(["a", "b"]), json!({"items": ["a", "b"]})),
            (json!(null), json!({"item": null})),
            (json!(3), json!({"value": 3})),
        ] {
            let response = tool_result(json!(9), Ok(raw));
            assert_eq!(response["result"]["structuredContent"], expected);
            assert_eq!(response["result"]["isError"], false);
        }
    }

    #[test]
    fn native_image_content_is_split_from_structured_result() {
        let response = tool_result(
            json!(9),
            Ok(json!({
                "ok": true,
                "status": "resolved",
                "_mcp_content": [{"type":"image","data":"iVBORw0KGgo=","mimeType":"image/png"}]
            })),
        );
        assert_eq!(response["result"]["content"][0]["type"], "image");
        assert_eq!(response["result"]["content"][0]["mimeType"], "image/png");
        assert_eq!(
            response["result"]["structuredContent"]["status"],
            "resolved"
        );
        assert!(response["result"]["structuredContent"]
            .get("_mcp_content")
            .is_none());
    }
}
