//! Client transport for a runtime provider registered with the host.
//!
//! Unlike app routing, these calls receive an explicit endpoint and the
//! host-issued token from the provider registry. They never consult the app
//! port table or fall back to a local handler after a provider refusal.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, LazyLock, RwLock};
use std::time::Duration;

use impress_service_core::pipeline::context;
use impress_service_core::provider::ProviderConnection;
use impress_service_core::provider::SchemaValidator;
use impress_service_core::refusal::{codes, Refusal};
use impress_service_core::registry_runtime::{self, HealthFuture, ProviderInvoker};
use impress_service_core::ServiceFuture;
use serde_json::Value;
use url::{Host, Url};

use crate::{CALL_TIMEOUT, PROBE_TIMEOUT};

/// Host-only definition validator for descriptors submitted at runtime.
/// The pure service registry injects this implementation; no kit crate gains
/// a JSON Schema engine or access to external schema resources.
#[derive(Default)]
pub struct JsonSchemaValidator;

struct DenyExternalSchemas;

impl jsonschema::Retrieve for DenyExternalSchemas {
    fn retrieve(
        &self,
        _uri: &jsonschema::Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        Err("external JSON Schema references are not allowed".into())
    }
}

impl SchemaValidator for JsonSchemaValidator {
    fn validate(&self, schema: &Value) -> Result<(), String> {
        compiled_schema(schema).map(|_| ())
    }

    fn validate_instance(&self, schema: &Value, value: &Value) -> Result<(), String> {
        compiled_schema(schema)?.validate(value).map_err(|error| {
            // `ValidationError`'s Display includes the rejected value. The
            // refusal/audit path must not echo an argument's private bytes.
            format!("arguments fail JSON Schema at {}", error.instance_path)
        })
    }
}

type CompiledSchema = Arc<jsonschema::Validator>;
static SCHEMAS: LazyLock<RwLock<HashMap<String, CompiledSchema>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

fn compiled_schema(schema: &Value) -> Result<CompiledSchema, String> {
    const MAX_CACHE_ENTRIES: usize = 128;
    let key = bounded_schema_key(schema)?;
    if let Some(compiled) = SCHEMAS
        .read()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(&key)
    {
        return Ok(compiled.clone());
    }
    match jsonschema::meta::try_validate(schema) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return Err(format!("invalid JSON Schema: {error}")),
        Err(error) => return Err(format!("unsupported JSON Schema draft: {error}")),
    }
    // Do not use validator_for: another workspace crate may enable the
    // resolver's HTTP/file features through Cargo feature unification.
    // This explicit retriever fails closed even in such a build.
    let compiled = Arc::new(
        jsonschema::options()
            .with_retriever(DenyExternalSchemas)
            .build(schema)
            .map_err(|error| format!("cannot compile JSON Schema: {error}"))?,
    );
    let mut cache = SCHEMAS.write().unwrap_or_else(|poison| poison.into_inner());
    if let Some(existing) = cache.get(&key) {
        return Ok(existing.clone());
    }
    if cache.len() < MAX_CACHE_ENTRIES {
        cache.insert(key, compiled.clone());
    }
    Ok(compiled)
}

fn bounded_schema_key(schema: &Value) -> Result<String, String> {
    const MAX_BYTES: usize = 128 * 1024;
    const MAX_DEPTH: usize = 64;
    const MAX_NODES: usize = 8192;
    let mut pending = vec![(schema, 0_usize)];
    let mut nodes = 0_usize;
    while let Some((value, depth)) = pending.pop() {
        nodes += 1;
        if depth > MAX_DEPTH || nodes > MAX_NODES {
            return Err("JSON Schema exceeds the provider size or depth limit".into());
        }
        match value {
            Value::Array(values) => pending.extend(values.iter().map(|value| (value, depth + 1))),
            Value::Object(fields) => {
                pending.extend(fields.values().map(|value| (value, depth + 1)))
            }
            _ => {}
        }
    }
    let key =
        serde_json::to_string(schema).map_err(|error| format!("encode JSON Schema: {error}"))?;
    if key.len() > MAX_BYTES {
        return Err("JSON Schema exceeds the provider size or depth limit".into());
    }
    Ok(key)
}

struct HttpProviderInvoker;

impl ProviderInvoker for HttpProviderInvoker {
    fn validate_endpoint(&self, endpoint: &str) -> Result<(), String> {
        self::validate_endpoint(endpoint).map_err(|refusal| refusal.message)
    }

    fn health(&self, connection: ProviderConnection) -> HealthFuture {
        Box::pin(async move {
            self::health(connection.endpoint(), connection.token())
                .await
                .is_ok()
        })
    }

    fn invoke(&self, connection: ProviderConnection, name: String, args: Value) -> ServiceFuture {
        Box::pin(async move {
            Ok(
                match self::call(connection.endpoint(), connection.token(), &name, args, None).await
                {
                    Ok(value) => value,
                    Err(refusal) => impress_service_core::strict::refusal_value(&refusal),
                },
            )
        })
    }
}

/// Install the provider transport alongside the app transport router.
pub fn install() {
    registry_runtime::install_invoker(Arc::new(HttpProviderInvoker));
}

fn endpoint_url(endpoint: &str) -> Result<Url, Refusal> {
    let url = Url::parse(endpoint)
        .map_err(|_| Refusal::invalid_argument("provider endpoint must be a loopback HTTP URL"))?;
    let local = match url.host() {
        Some(Host::Domain("localhost")) => true,
        Some(Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
        Some(Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
        _ => false,
    };
    let has_userinfo = endpoint
        .split_once("://")
        .and_then(|(_, rest)| rest.split('/').next())
        .is_some_and(|authority| authority.contains('@'));
    if !matches!(url.scheme(), "http" | "https")
        || !local
        || has_userinfo
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Refusal::invalid_argument(
            "provider endpoint must be loopback HTTP(S) without credentials, query, or fragment",
        ));
    }
    Ok(url)
}

/// Registration uses the same endpoint boundary as invocation. A URL that
/// could move a host-issued credential off machine is rejected before one is
/// issued; redirects are also disabled on requests below.
pub fn validate_endpoint(endpoint: &str) -> Result<(), Refusal> {
    endpoint_url(endpoint).map(|_| ())
}

fn provider_client(timeout: Duration) -> Result<reqwest::Client, Refusal> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .map_err(|_| Refusal::new(codes::HOST_UNAVAILABLE, "provider HTTP client unavailable"))
}

fn child_url(endpoint: &Url, parts: &[&str]) -> Url {
    let mut url = endpoint.clone();
    url.path_segments_mut()
        .expect("validated HTTP endpoint has path segments")
        .pop_if_empty()
        .extend(parts.iter().copied());
    url
}

fn unavailable(detail: &str) -> Refusal {
    Refusal::new(
        codes::HOST_UNAVAILABLE,
        format!("provider host is unavailable: {detail}"),
    )
}

// A provider is an untrusted local process even before the person reviews it.
// Bound reads while streaming: Content-Length may be absent or dishonest.
const MAX_HEALTH_BODY_BYTES: usize = 4 * 1024;
const MAX_VERB_BODY_BYTES: usize = 2 * 1024 * 1024;

async fn bounded_body(
    mut response: reqwest::Response,
    limit: usize,
    failure: Refusal,
) -> Result<Vec<u8>, Refusal> {
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err(failure);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| failure.clone())? {
        if chunk.len() > limit - body.len() {
            return Err(failure);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Probe `GET <endpoint>/health` with the host-issued bearer credential.
/// A failed probe is a named `host-unavailable` refusal. No app port default
/// or store fallback applies to a runtime provider.
pub async fn health(endpoint: &str, token: &str) -> Result<(), Refusal> {
    let endpoint = endpoint_url(endpoint)?;
    if token.is_empty() {
        return Err(Refusal::invalid_argument("provider token is empty"));
    }
    let url = child_url(&endpoint, &["health"]);
    let response = provider_client(PROBE_TIMEOUT)?
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| unavailable("health request failed or timed out"))?;
    if !response.status().is_success() {
        return Err(unavailable("health check refused"));
    }
    // A provider may return an empty 2xx health response. If it sends the
    // standard JSON envelope, its explicit refusal still takes precedence.
    let bytes = bounded_body(
        response,
        MAX_HEALTH_BODY_BYTES,
        unavailable("health response failed or exceeded 4 KiB"),
    )
    .await?;
    if !bytes.is_empty()
        && serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|body| body.get("ok").and_then(Value::as_bool))
            == Some(false)
    {
        return Err(unavailable("health check reported unavailable"));
    }
    Ok(())
}

/// Probe and invoke a runtime provider's exact verb. `trace` takes precedence
/// over the current pipeline trace when supplied; otherwise the current
/// context is propagated, or a fresh trace is made for an outside caller.
/// The current call id is sent as `x-impress-parent-call` when available.
/// A provider refusal is returned unchanged as `Err`, with no local fallback.
pub async fn call(
    endpoint: &str,
    token: &str,
    name: &str,
    args: Value,
    trace: Option<&str>,
) -> Result<Value, Refusal> {
    let current = context::current();
    call_with_parent(
        endpoint,
        token,
        name,
        args,
        trace,
        current.as_ref().map(|call| call.call_id.as_str()),
    )
    .await
}

/// Raw transport entry for a caller in another native image. The caller's
/// pipeline establishes both identifiers; this layer only forwards them.
pub async fn call_with_parent(
    endpoint: &str,
    token: &str,
    name: &str,
    args: Value,
    trace: Option<&str>,
    parent_call: Option<&str>,
) -> Result<Value, Refusal> {
    let endpoint = endpoint_url(endpoint)?;
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(Refusal::invalid_argument("invalid provider verb name"));
    }
    health(endpoint.as_str(), token).await?;
    let url = child_url(&endpoint, &["verb", name]);
    let current = context::current();
    let traceparent = trace
        .or_else(|| current.as_ref().map(|call| call.trace_id.as_str()))
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut request = provider_client(CALL_TIMEOUT)?
        .post(url)
        .bearer_auth(token)
        .header("traceparent", traceparent)
        .json(&args);
    if let Some(parent_call) = parent_call {
        request = request.header("x-impress-parent-call", parent_call);
    }
    let response = request
        .send()
        .await
        .map_err(|_| unavailable(&format!("{name} request failed or timed out")))?;
    let status = response.status();
    let bytes = bounded_body(
        response,
        MAX_VERB_BODY_BYTES,
        Refusal::new(
            codes::VERB_FAILED,
            format!("{name}: provider JSON response failed or exceeded 2 MiB"),
        ),
    )
    .await?;
    let body: Value = serde_json::from_slice(&bytes).map_err(|_| {
        Refusal::new(
            codes::VERB_FAILED,
            format!("{name}: invalid provider JSON response"),
        )
    })?;
    if !status.is_success() || body.get("ok").and_then(Value::as_bool) == Some(false) {
        let code = body
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or(codes::VERB_FAILED);
        let message = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("provider refused the verb");
        return Err(Refusal::new(code, message));
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use impress_service_core::pipeline::{context, CallerIdentity};
    use mockito::Matcher;
    use serde_json::json;

    use super::*;

    #[test]
    fn json_schema_validator_accepts_local_refs_and_rejects_invalid_definitions() {
        let validator = JsonSchemaValidator;
        validator
            .validate(&json!({
                "type": "object",
                "$defs": {"text": {"type": "string"}},
                "properties": {"text": {"$ref": "#/$defs/text"}}
            }))
            .expect("local reference");
        assert!(validator.validate(&json!({"type": "not-a-type"})).is_err());
        assert!(validator
            .validate(&json!({
                "$schema": "https://example.invalid/unsupported-draft/schema",
                "type": "object"
            }))
            .is_err());
        assert!(validator.validate(&json!({"type": 42})).is_err());
    }

    #[test]
    fn provider_arguments_use_full_schema_constraints_without_echoing_values() {
        let validator = JsonSchemaValidator;
        let schema = json!({
            "type": "object",
            "properties": {"text": {
                "type": "string", "minLength": 3, "pattern": "^[a-z]+$"
            }},
            "required": ["text"],
            "additionalProperties": false
        });
        validator.validate(&schema).unwrap();
        validator
            .validate_instance(&schema, &json!({"text":"hello"}))
            .unwrap();
        for args in [
            json!({}),
            json!({"text": 42}),
            json!({"text": "ab"}),
            json!({"text": "Private Secret"}),
        ] {
            let error = validator.validate_instance(&schema, &args).unwrap_err();
            assert!(error.contains("JSON Schema"));
            assert!(!error.contains("Private Secret"));
        }
    }

    #[tokio::test]
    async fn external_refs_are_refused_without_http_or_file_retrieval() {
        let validator = JsonSchemaValidator;
        let mut server = mockito::Server::new_async().await;
        let retrieval = server
            .mock("GET", "/schema.json")
            .with_status(200)
            .with_body(r#"{"type":"string"}"#)
            .expect(0)
            .create_async()
            .await;
        let http = json!({"$ref": format!("{}/schema.json", server.url())});
        assert!(validator.validate(&http).is_err());
        assert!(validator
            .validate(&json!({"$ref": "file:///tmp/provider-schema-must-not-open.json"}))
            .is_err());
        retrieval.assert_async().await;
    }

    #[test]
    fn oversized_or_overdeep_schemas_are_refused_before_compilation() {
        let validator = JsonSchemaValidator;
        assert!(validator
            .validate(&json!({"type":"object","description":"x".repeat(129 * 1024)}))
            .is_err());
        let mut deep = json!({"type": "string"});
        for _ in 0..70 {
            deep = json!({"allOf": [deep]});
        }
        assert!(validator.validate(&deep).is_err());
    }

    #[test]
    fn only_credential_free_loopback_endpoints_are_accepted() {
        for endpoint in [
            "http://127.0.0.1:23001",
            "http://[::1]:23001/provider",
            "https://localhost:23001",
        ] {
            validate_endpoint(endpoint).unwrap_or_else(|e| panic!("{endpoint}: {e}"));
        }
        for endpoint in [
            "http://example.com:23001",
            "http://192.168.1.5:23001",
            "file:///tmp/provider",
            "http://user:pass@127.0.0.1:23001",
            "http://@127.0.0.1:23001",
            "http://127.0.0.1:23001?token=leak",
            "http://127.0.0.1:23001/#fragment",
        ] {
            let error = validate_endpoint(endpoint).expect_err(endpoint);
            assert_eq!(error.code, codes::INVALID_ARGUMENT);
        }
    }

    #[tokio::test]
    async fn call_propagates_bearer_trace_and_current_parent() {
        let mut server = mockito::Server::new_async().await;
        let health = server
            .mock("GET", "/prefix/health")
            .match_header("authorization", "Bearer rotated-secret")
            .with_status(200)
            .with_body(r#"{"ok":true}"#)
            .expect(1)
            .create_async()
            .await;
        let verb = server
            .mock("POST", "/prefix/verb/example-service_echo")
            .match_header("authorization", "Bearer rotated-secret")
            .match_header("traceparent", "explicit-trace")
            .match_header("x-impress-parent-call", "parent-call-1")
            .match_body(Matcher::Json(json!({"text":"hello"})))
            .with_status(200)
            .with_body(r#"{"echo":"hello"}"#)
            .expect(1)
            .create_async()
            .await;
        let current = Arc::new(context::CallContext {
            call_id: "parent-call-1".into(),
            trace_id: "inherited-trace".into(),
            parent_call: None,
            caller: CallerIdentity::Person,
            verb: "test-service_parent".into(),
            store_override: None,
            mutation_ids: context::MutationIds::default(),
            adverbs: Default::default(),
            inverse: Default::default(),
            notes: Default::default(),
        });
        let answer = context::scope(current, async {
            call(
                &format!("{}/prefix", server.url()),
                "rotated-secret",
                "example-service_echo",
                json!({"text":"hello"}),
                Some("explicit-trace"),
            )
            .await
        })
        .await
        .expect("provider result");
        assert_eq!(answer["echo"], "hello");
        health.assert_async().await;
        verb.assert_async().await;
    }

    #[tokio::test]
    async fn raw_call_forwards_explicit_parent_without_local_pipeline_context() {
        let mut server = mockito::Server::new_async().await;
        let health = server
            .mock("GET", "/health")
            .with_status(200)
            .expect(1)
            .create_async()
            .await;
        let verb = server
            .mock("POST", "/verb/example-service_echo")
            .match_header("traceparent", "owner-trace")
            .match_header("x-impress-parent-call", "owner-call")
            .with_status(200)
            .with_body(r#"{"echo":"ok"}"#)
            .expect(1)
            .create_async()
            .await;
        let answer = call_with_parent(
            &server.url(),
            "private-token",
            "example-service_echo",
            json!({"text":"hello"}),
            Some("owner-trace"),
            Some("owner-call"),
        )
        .await
        .expect("raw result");
        assert_eq!(answer["echo"], "ok");
        health.assert_async().await;
        verb.assert_async().await;
    }

    #[tokio::test]
    async fn provider_refusal_is_not_a_success_even_with_http_200() {
        let mut server = mockito::Server::new_async().await;
        let _health = server
            .mock("GET", "/health")
            .with_status(200)
            .create_async()
            .await;
        let verb = server
            .mock("POST", "/verb/example-service_echo")
            .with_status(200)
            .with_body(r#"{"ok":false,"code":"conflict","message":"stale revision"}"#)
            .expect(1)
            .create_async()
            .await;
        let error = call(
            &server.url(),
            "host-token",
            "example-service_echo",
            json!({}),
            None,
        )
        .await
        .expect_err("provider refusal");
        assert_eq!(error.code, codes::CONFLICT);
        assert_eq!(error.message, "stale revision");
        verb.assert_async().await;
    }

    #[tokio::test]
    async fn failed_health_stops_before_verb_and_never_exposes_token() {
        let mut server = mockito::Server::new_async().await;
        let _health = server
            .mock("GET", "/health")
            .with_status(503)
            .create_async()
            .await;
        let verb = server
            .mock("POST", "/verb/example-service_echo")
            .expect(0)
            .create_async()
            .await;
        let error = call(
            &server.url(),
            "secret-not-for-errors",
            "example-service_echo",
            json!({}),
            None,
        )
        .await
        .expect_err("health refusal");
        assert_eq!(error.code, codes::HOST_UNAVAILABLE);
        assert!(!error.message.contains("secret-not-for-errors"));
        verb.assert_async().await;
    }

    #[tokio::test]
    async fn health_redirect_is_unavailable_without_following_it() {
        let mut server = mockito::Server::new_async().await;
        let health = server
            .mock("GET", "/health")
            .with_status(302)
            .with_header("location", "http://example.invalid/capture")
            .expect(1)
            .create_async()
            .await;
        let error = self::health(&server.url(), "private-token")
            .await
            .expect_err("redirect must refuse");
        assert_eq!(error.code, codes::HOST_UNAVAILABLE);
        assert!(!error.message.contains("private-token"));
        health.assert_async().await;
    }

    #[tokio::test]
    async fn chunked_health_body_is_bounded_without_content_length() {
        use std::io::Write;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let body = vec![b'x'; MAX_HEALTH_BODY_BYTES + 1];
            let headers = format!(
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n",
                body.len()
            );
            stream.write_all(headers.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
            stream.write_all(b"\r\n0\r\n\r\n").unwrap();
        });
        let error = health(&format!("http://{address}"), "private-token")
            .await
            .expect_err("oversized streamed health body must be refused");
        assert_eq!(error.code, codes::HOST_UNAVAILABLE);
        assert!(!error.message.contains("private-token"));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn oversized_provider_result_is_refused_without_leaking_it() {
        let mut server = mockito::Server::new_async().await;
        let _health = server
            .mock("GET", "/health")
            .with_status(200)
            .create_async()
            .await;
        let oversized = format!("{{\"echo\":\"{}\"}}", "x".repeat(MAX_VERB_BODY_BYTES));
        let verb = server
            .mock("POST", "/verb/example-service_echo")
            .with_status(200)
            .with_body(oversized)
            .expect(1)
            .create_async()
            .await;
        let error = call(
            &server.url(),
            "private-token",
            "example-service_echo",
            json!({}),
            None,
        )
        .await
        .expect_err("oversized provider result must be refused");
        assert_eq!(error.code, codes::VERB_FAILED);
        assert!(!error.message.contains("private-token"));
        verb.assert_async().await;
    }
}
