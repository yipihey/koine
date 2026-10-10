//! Process-owned runtime provider registry and transport hooks.
//!
//! The pure service crate never opens a store or makes an HTTP request. A host
//! installs the validated, persisted registry and its AppTransport adapter.
//! Merely listing linked descriptors (including documentation generation) does
//! not open the user's database.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, LazyLock, RwLock};

use serde_json::Value;

use crate::descriptor_handle::VerbHandle;
use crate::provider::{
    ProviderConnection, ProviderStatus, RegistrationError, RegistrationReceipt,
    RegistrationRequest, Registry,
};
use crate::refusal::Refusal;
use crate::ServiceFuture;

pub type HealthFuture = Pin<Box<dyn Future<Output = bool> + Send + 'static>>;

/// Implemented by AppTransport, outside the kit's pure dependency boundary.
pub trait ProviderInvoker: Send + Sync {
    fn validate_endpoint(&self, endpoint: &str) -> Result<(), String>;
    fn health(&self, connection: ProviderConnection) -> HealthFuture;
    fn invoke(&self, connection: ProviderConnection, name: String, args: Value) -> ServiceFuture;
}

static REGISTRY: LazyLock<RwLock<Arc<Registry>>> =
    LazyLock::new(|| RwLock::new(Arc::new(Registry::new())));
static INVOKER: RwLock<Option<Arc<dyn ProviderInvoker>>> = RwLock::new(None);

pub fn current() -> Arc<Registry> {
    REGISTRY.read().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn install(registry: Arc<Registry>) {
    let mut current = REGISTRY.write().unwrap_or_else(|e| e.into_inner());
    if !Arc::ptr_eq(&current, &registry) {
        registry.bump_revision();
    }
    *current = registry;
}

pub fn install_invoker(invoker: Arc<dyn ProviderInvoker>) {
    *INVOKER.write().unwrap_or_else(|e| e.into_inner()) = Some(invoker);
}

fn invoker() -> Option<Arc<dyn ProviderInvoker>> {
    INVOKER.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Endpoint validation is shared with the actual outbound client, before any
/// provider credential is issued or metadata persisted.
pub fn register(request: RegistrationRequest) -> Result<RegistrationReceipt, RegistrationError> {
    let transport = invoker()
        .ok_or_else(|| RegistrationError::Invalid("provider transport is not installed".into()))?;
    transport
        .validate_endpoint(&request.provider.endpoint)
        .map_err(RegistrationError::Invalid)?;
    let registry = current();
    // Another local host may have registered this id (or a different
    // provider) since startup. The store's per-id CAS remains the authority
    // when two processes race after this refresh.
    registry.refresh_persisted()?;
    registry.register(request)
}

fn unavailable(verb: &VerbHandle) -> Refusal {
    Refusal::new(
        crate::refusal::codes::HOST_UNAVAILABLE,
        format!(
            "{}: provider {} is unavailable or its registration changed",
            verb.name(),
            verb.provider_id().unwrap_or("unknown")
        ),
    )
}

/// Refresh before strict arguments and policy read a provider descriptor.
/// A changed registration starts unavailable; probe just that provider once
/// so a cross-process trust grant can take effect on this very call. The
/// invoke step refreshes again and checks the generation before dispatch.
pub(crate) async fn current_for_call(verb: VerbHandle) -> Result<VerbHandle, Refusal> {
    let Some(provider_id) = verb.provider_id().map(str::to_owned) else {
        return Ok(verb);
    };
    let name = verb.name().to_owned();
    let registry = current();
    let refresh = registry.clone();
    if !matches!(
        tokio::task::spawn_blocking(move || refresh.refresh_persisted()).await,
        Ok(Ok(()))
    ) {
        return Err(unavailable(&verb));
    }
    let fresh = registry.find(&name).ok_or_else(|| unavailable(&verb))?;
    if fresh.provider_id() != Some(provider_id.as_str()) {
        return Err(unavailable(&verb));
    }
    if fresh.provider_status() == Some(ProviderStatus::Unavailable) {
        if let (Some(transport), Some((_, connection))) = (
            invoker(),
            registry
                .health_connections()
                .into_iter()
                .find(|(id, _)| id == &provider_id),
        ) {
            let generation = connection.generation();
            let healthy = transport.health(connection).await;
            let _ = registry.set_health_if_current(&provider_id, generation, healthy);
        }
    }
    registry.find(&name).ok_or_else(|| unavailable(&verb))
}

pub(crate) fn check_available(verb: &VerbHandle, args: &Value) -> Result<(), Refusal> {
    if let VerbHandle::Provider(provider) = verb {
        if invoker().is_none() || current().current_connection(provider).is_none() {
            return Err(unavailable(verb));
        }
        current().validate_args(provider, args)?;
    }
    Ok(())
}

/// Called only by the pipeline's invoke layer, under its identity/trace scope.
pub(crate) async fn invoke(verb: &VerbHandle, args: Value) -> Result<Value, crate::BoxError> {
    let VerbHandle::Provider(provider) = verb else {
        return Err("provider invoker received a linked descriptor".into());
    };
    let registry = current();
    let refresh = registry.clone();
    if !matches!(
        tokio::task::spawn_blocking(move || refresh.refresh_persisted()).await,
        Ok(Ok(()))
    ) {
        return Ok(crate::strict::refusal_value(&unavailable(verb)));
    }
    let Some(connection) = registry.current_connection(provider) else {
        return Ok(crate::strict::refusal_value(&unavailable(verb)));
    };
    let Some(transport) = invoker() else {
        return Ok(crate::strict::refusal_value(&unavailable(verb)));
    };
    let result = transport
        .invoke(connection, verb.name().to_owned(), args)
        .await;
    // An old in-flight call must not change a newer registration's health.
    if result
        .as_ref()
        .ok()
        .and_then(|v| v.get("code"))
        .and_then(Value::as_str)
        == Some(crate::refusal::codes::HOST_UNAVAILABLE)
    {
        let _ = registry.set_health_if_current(&provider.provider_id, provider.generation, false);
    }
    match result {
        Ok(value) if value.get("ok").and_then(Value::as_bool) == Some(false) => Ok(value),
        Ok(_) if registry.current_connection(provider).is_none() => {
            Ok(crate::strict::refusal_value(&unavailable(verb)))
        }
        Ok(value) => match registry.validate_output(provider, &value) {
            Ok(()) => Ok(value),
            Err(refusal) => Ok(crate::strict::refusal_value(&refusal)),
        },
        Err(error) => Err(error),
    }
}

/// Hosts probe on startup and periodically; a departed verb stays in the
/// catalogue and a restored registration becomes callable only after health.
pub async fn refresh_health() {
    let Some(transport) = invoker() else { return };
    let registry = current();
    let refresh = registry.clone();
    if !matches!(
        tokio::task::spawn_blocking(move || refresh.refresh_persisted()).await,
        Ok(Ok(()))
    ) {
        tracing::warn!("provider persistence refresh failed; calls remain unavailable");
        return;
    }
    for (id, connection) in registry.health_connections() {
        let generation = connection.generation();
        let healthy = transport.health(connection).await;
        let _ = registry.set_health_if_current(&id, generation, healthy);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;
    use crate::pipeline::{self, Call};
    use crate::provider::{PersistedProvider, ProviderPersistence, SchemaValidator};

    #[derive(Default)]
    struct MemoryPersistence {
        row: RwLock<Option<PersistedProvider>>,
        token: RwLock<Option<String>>,
        fail_load: AtomicBool,
    }

    impl ProviderPersistence for MemoryPersistence {
        fn load(&self) -> Result<Vec<PersistedProvider>, String> {
            if self.fail_load.load(Ordering::Relaxed) {
                return Err("fixture store unavailable".into());
            }
            Ok(self.row.read().unwrap().iter().cloned().collect())
        }

        fn save_registration(
            &self,
            row: &PersistedProvider,
            token: &str,
            _previous: Option<&PersistedProvider>,
        ) -> Result<(), String> {
            *self.row.write().unwrap() = Some(row.clone());
            *self.token.write().unwrap() = Some(token.into());
            Ok(())
        }

        fn save_trust(&self, row: &PersistedProvider) -> Result<(), String> {
            *self.row.write().unwrap() = Some(row.clone());
            Ok(())
        }

        fn load_token(&self, _: &str) -> Result<Option<String>, String> {
            Ok(self.token.read().unwrap().clone())
        }

        fn save_status(&self, row: &PersistedProvider) -> Result<(), String> {
            *self.row.write().unwrap() = Some(row.clone());
            Ok(())
        }
    }

    struct TestValidator;

    // Registry and transport are process-wide. Serialize the two replacement
    // tests and restore the prior host even when an assertion unwinds.
    static HOST_TEST: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct RestoreHost {
        registry: Arc<Registry>,
        invoker: Option<Arc<dyn ProviderInvoker>>,
    }
    impl RestoreHost {
        fn capture() -> Self {
            Self {
                registry: current(),
                invoker: invoker(),
            }
        }
    }
    impl Drop for RestoreHost {
        fn drop(&mut self) {
            install(self.registry.clone());
            *INVOKER.write().unwrap_or_else(|e| e.into_inner()) = self.invoker.clone();
        }
    }

    impl SchemaValidator for TestValidator {
        fn validate(&self, _: &Value) -> Result<(), String> {
            Ok(())
        }

        fn validate_instance(&self, schema: &Value, value: &Value) -> Result<(), String> {
            let key = schema["required"][0]
                .as_str()
                .ok_or("required key missing")?;
            if value[key].is_string() {
                Ok(())
            } else {
                Err(format!("{key} must be a string"))
            }
        }
    }

    struct TestInvoker(Arc<AtomicUsize>);

    impl ProviderInvoker for TestInvoker {
        fn validate_endpoint(&self, _: &str) -> Result<(), String> {
            Ok(())
        }

        fn health(&self, _: ProviderConnection) -> HealthFuture {
            Box::pin(async { true })
        }

        fn invoke(&self, _: ProviderConnection, _: String, args: Value) -> ServiceFuture {
            self.0.fetch_add(1, Ordering::Relaxed);
            Box::pin(async move {
                Ok(match args["text"].as_str() {
                    Some("bad") => json!({"echo":42}),
                    Some("refuse") => {
                        json!({"ok":false,"code":"provider-refusal","message":"refused"})
                    }
                    Some(text) => json!({"echo":text}),
                    None => json!({"echo":null}),
                })
            })
        }
    }

    #[tokio::test]
    async fn cross_process_trust_is_applied_before_policy_and_outputs_are_checked() {
        let _serial = HOST_TEST.lock().await;
        let _restore = RestoreHost::capture();
        let persistence = Arc::new(MemoryPersistence::default());
        let validator: Arc<dyn SchemaValidator> = Arc::new(TestValidator);
        let first = Registry::new()
            .with_validator(validator.clone())
            .with_persistence(persistence.clone())
            .unwrap();
        let request = serde_json::from_value(json!({
            "provider":{"id":"policy-refresh-fixture","language":"Python","version":"1.0.0","endpoint":"http://127.0.0.1:1"},
            "verbs":[{"name":"policy-refresh-fixture-service_echo","description":"Echo text",
                "input_schema":{"type":"object","properties":{"text":{"type":"string","description":"Text"}},"required":["text"],"additionalProperties":false},
                "output_schema":{"type":"object","properties":{"echo":{"type":"string"}},"required":["echo"]},
                "safety":{"class":"read_only"},"since":"1.0.0",
                "examples":[{"name":"echo","args":{"text":"hello"}}]}]
        })).unwrap();
        first.register(request).unwrap();
        let second = Arc::new(
            Registry::new()
                .with_validator(validator)
                .with_persistence(persistence)
                .unwrap(),
        );
        second.set_health("policy-refresh-fixture", true).unwrap();
        let name = "policy-refresh-fixture-service_echo";
        let stale_untrusted = second.find(name).unwrap();
        assert_eq!(stale_untrusted.safety().class.as_str(), "external");
        let invocations = Arc::new(AtomicUsize::new(0));
        install(second.clone());
        install_invoker(Arc::new(TestInvoker(invocations.clone())));

        first.set_trusted("policy-refresh-fixture", true).unwrap();
        let result = pipeline::invoke_handle(
            stale_untrusted,
            Call::agent("test", json!({"text":"hello"})),
        )
        .await
        .unwrap();
        assert_eq!(
            result,
            json!({"echo":"hello"}),
            "fresh trusted safety must run before policy"
        );
        assert_eq!(invocations.load(Ordering::Relaxed), 1);

        let bad = pipeline::invoke_handle(
            second.find(name).unwrap(),
            Call::agent("test", json!({"text":"bad"})),
        )
        .await
        .unwrap();
        assert_eq!(bad["code"], crate::refusal::codes::VERB_FAILED);
        assert!(!bad.to_string().contains("42"));
        let refusal = pipeline::invoke_handle(
            second.find(name).unwrap(),
            Call::agent("test", json!({"text":"refuse"})),
        )
        .await
        .unwrap();
        assert_eq!(refusal["code"], "provider-refusal");

        let stale_trusted = second.find(name).unwrap();
        first.set_trusted("policy-refresh-fixture", false).unwrap();
        let before = invocations.load(Ordering::Relaxed);
        let reviewed =
            pipeline::invoke_handle(stale_trusted, Call::agent("test", json!({"text":"hello"})))
                .await
                .unwrap();
        assert_eq!(reviewed["code"], "review-pending");
        assert_eq!(invocations.load(Ordering::Relaxed), before);
    }

    #[tokio::test]
    async fn failed_refresh_has_named_audited_refusal() {
        let _serial = HOST_TEST.lock().await;
        let _restore = RestoreHost::capture();
        let persistence = Arc::new(MemoryPersistence::default());
        let registry = Arc::new(
            Registry::new()
                .with_validator(Arc::new(TestValidator))
                .with_persistence(persistence.clone())
                .unwrap(),
        );
        let request = serde_json::from_value(json!({
            "provider":{"id":"refresh-failure-fixture","language":"Python","version":"1.0.0","endpoint":"http://127.0.0.1:1"},
            "verbs":[{"name":"refresh-failure-fixture-service_echo","description":"Echo text",
                "input_schema":{"type":"object","properties":{"text":{"type":"string","description":"Text"}},"required":["text"],"additionalProperties":false},
                "output_schema":{"type":"object","properties":{"echo":{"type":"string"}},"required":["echo"]},
                "safety":{"class":"read_only"},"since":"1.0.0",
                "examples":[{"name":"echo","args":{"text":"hello"}}]}]
        }))
        .unwrap();
        registry.register(request).unwrap();
        registry
            .set_trusted("refresh-failure-fixture", true)
            .unwrap();
        let name = "refresh-failure-fixture-service_echo";
        let handle = registry.find(name).unwrap();
        let sink = crate::pipeline::tests::captured();
        install(registry);
        persistence.fail_load.store(true, Ordering::Relaxed);
        let answer = pipeline::invoke_handle(handle, Call::person(json!({"text":"hello"})))
            .await
            .unwrap();
        assert_eq!(answer["code"], crate::refusal::codes::HOST_UNAVAILABLE);
        let records = sink.0.lock().unwrap();
        let row = records.iter().find(|row| row.verb == name).unwrap();
        assert!(!row.ok);
        assert_eq!(
            row.code.as_deref(),
            Some(crate::refusal::codes::HOST_UNAVAILABLE)
        );
        drop(records);
    }
}
