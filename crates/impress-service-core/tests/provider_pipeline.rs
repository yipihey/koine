//! A separate process keeps registry, policy and audit installation isolated
//! from linked-inventory unit tests while exercising the actual public entry.
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};

use impress_service_core::{
    call,
    pipeline::{self, audit, context, Call, CallerIdentity},
    provider::{ProviderConnection, RegistrationRequest, Registry, SchemaValidator},
    registry_runtime::{self, HealthFuture, ProviderInvoker},
    ServiceFuture,
};
use serde_json::{json, Value};

struct FixtureSchema;
impl SchemaValidator for FixtureSchema {
    fn validate_instance(&self, schema: &Value, value: &Value) -> Result<(), String> {
        if !value.is_object() {
            return Err("fixture requires an object".into());
        }
        if schema["additionalProperties"] == false {
            impress_service_core::strict::check_args("pipeline-fixture-service_echo", value, schema)
                .map_err(|e| e.to_string())
        } else {
            Ok(())
        }
    }
    fn validate(&self, _: &Value) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Default)]
struct Transport {
    healthy: AtomicBool,
    calls: AtomicUsize,
}
impl ProviderInvoker for Transport {
    fn validate_endpoint(&self, endpoint: &str) -> Result<(), String> {
        if endpoint == "http://127.0.0.1:23199" {
            Ok(())
        } else {
            Err("invalid endpoint".into())
        }
    }
    fn health(&self, _: ProviderConnection) -> HealthFuture {
        let healthy = self.healthy.load(Ordering::SeqCst);
        Box::pin(async move { healthy })
    }
    fn invoke(&self, connection: ProviderConnection, name: String, args: Value) -> ServiceFuture {
        assert!(!connection.token().is_empty());
        assert_eq!(name, "pipeline-fixture-service_echo");
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let ctx = context::current().expect("provider runs inside the pipeline context");
            Ok(
                json!({"ok":true, "message":args["message"], "caller":ctx.caller.to_json(), "trace":ctx.trace_id, "parent":ctx.parent_call}),
            )
        })
    }
}

#[derive(Default)]
struct Records(Mutex<Vec<audit::VerbCallRecord>>);
impl audit::Sink for Records {
    fn record(&self, row: audit::VerbCallRecord) {
        self.0.lock().unwrap().push(row);
    }
}

fn registration() -> RegistrationRequest {
    serde_json::from_value(json!({
        "provider":{"id":"pipeline-fixture","language":"Python","version":"1.0.0","endpoint":"http://127.0.0.1:23199"},
        "verbs":[{
            "name":"pipeline-fixture-service_echo", "description":"Echo a message", "since":"1.0.0",
            "input_schema":{"type":"object","properties":{"message":{"type":"string","description":"Message to echo"}},"required":["message"],"additionalProperties":false},
            "output_schema":{"type":"object"}, "safety":{"class":"read_only"},
            "examples":[{"name":"echo","args":{"message":"hello"}}]
        }]
    })).unwrap()
}

#[test]
fn provider_calls_share_policy_identity_trace_audit_and_lifecycle() {
    let registry = Arc::new(Registry::new().with_validator(Arc::new(FixtureSchema)));
    registry_runtime::install(registry.clone());
    let transport = Arc::new(Transport::default());
    transport.healthy.store(true, Ordering::SeqCst);
    registry_runtime::install_invoker(transport.clone());
    let records = Arc::new(Records::default());
    audit::install(records.clone());
    let receipt = registry_runtime::register(registration()).unwrap();
    let name = "pipeline-fixture-service_echo";
    assert!(call::descriptors().any(|verb| verb.name() == name));

    let invoke = |caller, args| {
        impress_service_core::runtime::block_on(pipeline::invoke_handle(
            call::find(name).unwrap(),
            Call::new(caller, args),
        ))
        .unwrap()
    };
    for caller in [
        CallerIdentity::agent("test"),
        CallerIdentity::Provider("pipeline-fixture".into()),
    ] {
        let result = invoke(caller, json!({"message":"hello"}));
        assert_eq!(result["code"], "review-pending");
    }
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    let invalid = invoke(
        CallerIdentity::Person,
        json!({"message":"hello", "unknown":true}),
    );
    assert_eq!(invalid["code"], "invalid-argument");
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);

    let mut call = Call::new(CallerIdentity::Person, json!({"message":"hello"}));
    call.trace_id = Some("provider-trace".into());
    call.parent_call = Some("provider-parent".into());
    let result = impress_service_core::runtime::block_on(pipeline::invoke_handle(
        call::find(name).unwrap(),
        call,
    ))
    .unwrap();
    assert_eq!(result["caller"], json!({"kind":"human"}));
    assert_eq!(result["trace"], "provider-trace");
    assert_eq!(result["parent"], "provider-parent");
    {
        let rows = records.0.lock().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].verb, name);
        assert_eq!(rows[0].caller, CallerIdentity::Person);
        assert_eq!(rows[0].trace_id, "provider-trace");
        assert_eq!(rows[0].since, "1.0.0");
        assert!(rows[0].ok);
    }

    registry.set_trusted("pipeline-fixture", true).unwrap();
    let result = invoke(
        CallerIdentity::Provider("pipeline-fixture".into()),
        json!({"message":"trusted"}),
    );
    assert_eq!(result["message"], "trusted");
    assert_eq!(
        result["caller"],
        json!({"kind":"provider", "name":"pipeline-fixture"})
    );

    transport.healthy.store(false, Ordering::SeqCst);
    impress_service_core::runtime::block_on(registry_runtime::refresh_health());
    let missing = invoke(CallerIdentity::Person, json!({"message":"offline"}));
    assert_eq!(missing["code"], "host-unavailable");
    assert!(missing["message"].as_str().unwrap().contains(name));
    assert!(call::find(name).is_some());
    assert_eq!(transport.calls.load(Ordering::SeqCst), 2);

    registry
        .deregister("pipeline-fixture", &receipt.token)
        .unwrap();
    transport.healthy.store(true, Ordering::SeqCst);
    impress_service_core::runtime::block_on(registry_runtime::refresh_health());
    assert_eq!(
        invoke(CallerIdentity::Person, json!({"message":"deregistered"}))["code"],
        "host-unavailable"
    );
}
