//! Runtime provider discovery and the person's explicit trust decision (P8).
//! Authentication and registration never grant trust. The generated form uses
//! the ordinary surface call path, whose native user action carries Person.
use std::sync::Arc;

use impress_service_core::pipeline::{context, CallerIdentity};
use impress_service_core::provider::{ProviderSummary, RegistrationError, Registry};
use impress_service_core::registry_runtime;
use impress_service_macros::{impress_service, impress_service_impl};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProviderListResult {
    pub ok: bool,
    pub providers: Vec<ProviderSummary>,
    pub wire_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProviderTrustResult {
    pub ok: bool,
    pub provider_id: String,
    pub trusted: Option<bool>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub wire_version: u32,
}

#[impress_service]
pub trait ProviderService: Send + Sync + 'static {
    /// List registered providers, endpoints, availability, and the person's
    /// trust decision. Credentials and credential hashes are never returned.
    #[impress_method(safety = read_only, effects(reads = [], writes = [], reach = []))]
    #[impress_example(
        name = "registered-providers",
        args = r#"{}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn list(&self) -> ProviderListResult;

    /// Apply or revoke the person's trust in a runtime provider's declared
    /// safety classes. Only a native Person call may change trust; agents,
    /// providers, app credentials, and scheduled work cannot authorize it.
    /// Use the generated form after reviewing the provider's endpoint and verbs.
    #[impress_method(safety = mutating, idempotent = true, effects(reads = ["provider@1.0.0"], writes = ["provider@1.0.0"], reach = []))]
    #[impress_example(
        name = "non-person-refused",
        args = r#"{"provider_id":"example","trusted":true}"#,
        expect = r#"{"ok":false,"code":"forbidden"}"#
    )]
    async fn set_trusted(&self, provider_id: String, trusted: bool) -> ProviderTrustResult;
}

#[derive(Default)]
pub struct DefaultProviderService {
    registry: Option<Arc<Registry>>,
}
impl DefaultProviderService {
    pub fn new() -> Self {
        Self::default()
    }
    /// Use a caller-owned registry, including its explicit store and validator.
    pub fn with_registry(registry: Arc<Registry>) -> Self {
        Self {
            registry: Some(registry),
        }
    }
    fn registry(&self) -> Arc<Registry> {
        self.registry
            .clone()
            .unwrap_or_else(registry_runtime::current)
    }
}

#[async_trait::async_trait]
impl ProviderService for DefaultProviderService {
    async fn list(&self) -> ProviderListResult {
        let providers = self.registry().summaries();
        tracing::debug!(
            "provider display: {} registrations, {} available",
            providers.len(),
            providers
                .iter()
                .filter(|provider| provider.available)
                .count()
        );
        ProviderListResult {
            ok: true,
            providers,
            wire_version: 1,
        }
    }

    async fn set_trusted(&self, provider_id: String, trusted: bool) -> ProviderTrustResult {
        let mut result = ProviderTrustResult {
            ok: false,
            provider_id: provider_id.clone(),
            trusted: None,
            message: "Only the person using the native review form may change provider trust."
                .into(),
            code: Some("forbidden".into()),
            wire_version: 1,
        };
        if !context::current().is_some_and(|call| matches!(call.caller, CallerIdentity::Person)) {
            return result;
        }
        let registry = self.registry();
        match registry
            .refresh_persisted()
            .and_then(|()| registry.set_trusted(&provider_id, trusted))
        {
            Ok(()) => {
                result.ok = true;
                result.trusted = Some(trusted);
                result.code = None;
                result.message = if trusted {
                    "The provider's declared safety classes now apply.".into()
                } else {
                    "Provider calls are now treated as external.".into()
                };
            }
            Err(error) => {
                result.message = error.to_string();
                result.code = Some(
                    match error {
                        RegistrationError::Invalid(_) => "invalid-argument",
                        RegistrationError::Credential(_) => "forbidden",
                        RegistrationError::ValidatorUnavailable => "host-unavailable",
                        RegistrationError::Persistence(_) => "store-error",
                    }
                    .into(),
                );
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use impress_service_core::descriptor::SafetyClass;
    use impress_service_core::pipeline::context::{CallContext, MutationIds};
    use impress_service_core::provider::{RegistrationRequest, SchemaValidator};
    use serde_json::{json, Value};

    struct TestValidator;
    impl SchemaValidator for TestValidator {
        fn validate(&self, _: &Value) -> Result<(), String> {
            Ok(())
        }
        fn validate_instance(&self, _: &Value, _: &Value) -> Result<(), String> {
            Ok(())
        }
    }

    fn service() -> DefaultProviderService {
        let registry = Registry::new().with_validator(Arc::new(TestValidator));
        let request: RegistrationRequest = serde_json::from_value(json!({
            "provider": {"id":"fixture", "language":"Python", "version":"1.0.0",
                "endpoint":"http://127.0.0.1:23190"},
            "verbs": [{"name":"fixture-service_echo", "description":"Echo text",
                "input_schema":{"type":"object", "properties":{}, "additionalProperties":false},
                "output_schema":{"type":"object"}, "safety":{"class":"read_only"},
                "since":"1.0.0", "examples":[{"name":"empty", "args":{}}]}]
        }))
        .unwrap();
        registry.register(request).unwrap();
        DefaultProviderService {
            registry: Some(Arc::new(registry)),
        }
    }

    fn call_context(caller: CallerIdentity) -> Arc<CallContext> {
        Arc::new(CallContext {
            call_id: "provider-trust-test".into(),
            trace_id: "provider-trust-trace".into(),
            parent_call: None,
            caller,
            verb: "provider-service_set-trusted".into(),
            store_override: None,
            mutation_ids: MutationIds::default(),
            adverbs: Default::default(),
            inverse: Default::default(),
            notes: Default::default(),
        })
    }

    #[tokio::test]
    async fn credentials_cannot_grant_trust() {
        let service = service();
        for caller in [
            CallerIdentity::agent("test-agent"),
            CallerIdentity::Provider("fixture".into()),
            CallerIdentity::App("impress".into()),
            CallerIdentity::system("test-scheduler"),
        ] {
            let result = context::scope(
                call_context(caller),
                service.set_trusted("fixture".into(), true),
            )
            .await;
            assert!(!result.ok);
            assert_eq!(result.code.as_deref(), Some("forbidden"));
            assert_eq!(result.trusted, None);
        }
        assert!(!service.set_trusted("fixture".into(), true).await.ok);
        assert!(!service.list().await.providers[0].trusted);
    }

    #[tokio::test]
    async fn person_can_grant_and_revoke_declared_safety() {
        let service = service();
        for (trusted, safety) in [
            (true, SafetyClass::ReadOnly),
            (false, SafetyClass::External),
        ] {
            let result = context::scope(
                call_context(CallerIdentity::Person),
                service.set_trusted("fixture".into(), trusted),
            )
            .await;
            assert!(result.ok, "{}", result.message);
            assert_eq!(result.trusted, Some(trusted));
            assert_eq!(
                service
                    .registry()
                    .find("fixture-service_echo")
                    .unwrap()
                    .safety()
                    .class,
                safety
            );
            let listed = service.list().await;
            assert_eq!(listed.providers[0].trusted, trusted);
            let json = serde_json::to_string(&listed).unwrap();
            assert!(!json.contains("token"));
            assert!(!json.contains("hash"));
        }
    }

    #[tokio::test]
    async fn unknown_provider_does_not_report_a_trust_change() {
        let service = service();
        let result = context::scope(
            call_context(CallerIdentity::Person),
            service.set_trusted("missing".into(), true),
        )
        .await;
        assert!(!result.ok);
        assert_eq!(result.code.as_deref(), Some("invalid-argument"));
        assert_eq!(result.trusted, None);
    }
}

impress_service_impl! {
    service = ProviderService,
    safety = read_only,
    effects = { reads: ["provider@1.0.0"], writes: [], reach: [] },
    since = "0.1.0",
    impl = DefaultProviderService,
    instance = DefaultProviderService::new,
    strict_args = true,
    methods = [
        list() -> ProviderListResult,
        set_trusted(
            /// Registered provider id from provider-service_list.
            provider_id: String,
            /// True applies the reviewed provider's declared safety; false restores external safety.
            trusted: bool
        ) -> ProviderTrustResult,
    ],
}
