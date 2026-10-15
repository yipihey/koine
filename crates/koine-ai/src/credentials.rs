//! Where API keys come from, without any of them being written by Rust.
//!
//! The GUI reads the platform keychain and pushes values into
//! [`InMemoryCredentials`] over the FFI; daemons read the same keychain
//! items through the `security` tool; the environment overrides both. A
//! [`LayeredCredentials`] stack expresses that precedence once.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use crate::catalogue::{self, ProviderDescriptor};
use crate::secret::Secret;

/// The keychain account prefix the Swift `AICredentialManager` writes under:
/// `com.impressai.credentials.<provider>.<field>`.
pub const KEYCHAIN_ACCOUNT_PREFIX: &str = "com.impressai.credentials";

pub trait CredentialSource: Send + Sync {
    fn get(&self, provider: &str, field: &str) -> Option<Secret>;

    /// Diagnostic label (`env`, `memory`, `keychain`) — never a value.
    fn source_name(&self) -> &'static str;

    /// Which concrete source would answer for `field`, for diagnostics.
    /// Layered sources report the layer that holds the value.
    fn source_of(&self, provider: &str, field: &str) -> Option<&'static str> {
        self.get(provider, field).map(|_| self.source_name())
    }
}

/// Values the host pushed for this process only.
#[derive(Default)]
pub struct InMemoryCredentials {
    values: RwLock<HashMap<(String, String), Secret>>,
}

impl InMemoryCredentials {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Replace every field for `provider`; an empty value clears that field.
    pub fn set_all(&self, provider: &str, fields: HashMap<String, String>) {
        let mut values = self.values.write().unwrap();
        values.retain(|(existing, _), _| existing != provider);
        for (field, value) in fields {
            if !value.trim().is_empty() {
                values.insert((provider.to_string(), field), Secret::new(value));
            }
        }
    }

    pub fn clear(&self, provider: &str) {
        self.values
            .write()
            .unwrap()
            .retain(|(existing, _), _| existing != provider);
    }
}

impl CredentialSource for InMemoryCredentials {
    fn get(&self, provider: &str, field: &str) -> Option<Secret> {
        self.values
            .read()
            .unwrap()
            .get(&(provider.to_string(), field.to_string()))
            .cloned()
    }

    fn source_name(&self) -> &'static str {
        "memory"
    }
}

/// `IMPRESS_<PROVIDER>_API_KEY` (provider id upper-snake, so
/// `IMPRESS_OMLX_API_KEY` and `IMPRESS_OPENAI_COMPATIBLE_API_KEY`), plus the
/// impel executors' `IMPEL_LLM_API_KEY` when `IMPEL_LLM_PROVIDER` names the
/// provider being asked about.
pub struct EnvCredentials;

impl EnvCredentials {
    pub fn variable_name(provider: &str, field: &str) -> String {
        let provider = provider
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let field = match field {
            "apiKey" => "API_KEY".to_string(),
            other => other
                .chars()
                .map(|character| {
                    if character.is_ascii_alphanumeric() {
                        character.to_ascii_uppercase()
                    } else {
                        '_'
                    }
                })
                .collect(),
        };
        format!("IMPRESS_{provider}_{field}")
    }
}

impl CredentialSource for EnvCredentials {
    fn get(&self, provider: &str, field: &str) -> Option<Secret> {
        if let Ok(value) = std::env::var(Self::variable_name(provider, field)) {
            if !value.trim().is_empty() {
                return Some(Secret::new(value));
            }
        }
        if field == "apiKey"
            && std::env::var("IMPEL_LLM_PROVIDER").ok().as_deref() == Some(provider)
        {
            if let Ok(value) = std::env::var("IMPEL_LLM_API_KEY") {
                if !value.trim().is_empty() {
                    return Some(Secret::new(value));
                }
            }
        }
        None
    }

    fn source_name(&self) -> &'static str {
        "env"
    }
}

/// The login keychain via `/usr/bin/security`, for daemons that have no
/// keychain framework access. KeychainSwift stores each value as a generic
/// password whose *account* is the prefixed key and which carries no
/// service attribute, so the lookup is by account. Results (including
/// misses) are cached per process; a rotated key needs a restart.
pub struct KeychainCliCredentials {
    cache: Mutex<HashMap<(String, String), Option<Secret>>>,
}

impl Default for KeychainCliCredentials {
    fn default() -> Self {
        Self::new()
    }
}

impl KeychainCliCredentials {
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn account(provider: &str, field: &str) -> String {
        format!("{KEYCHAIN_ACCOUNT_PREFIX}.{provider}.{field}")
    }

    #[cfg(target_os = "macos")]
    fn lookup(account: &str) -> Option<Secret> {
        use std::process::{Command, Stdio};
        use std::sync::mpsc;
        use std::time::Duration;

        let account = account.to_string();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let output = Command::new("/usr/bin/security")
                .args(["find-generic-password", "-w", "-a", &account])
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output();
            let _ = sender.send(output);
        });
        match receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(output)) if output.status.success() => {
                let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
                (!value.is_empty()).then(|| Secret::new(value))
            }
            _ => None,
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn lookup(_account: &str) -> Option<Secret> {
        None
    }
}

impl CredentialSource for KeychainCliCredentials {
    fn get(&self, provider: &str, field: &str) -> Option<Secret> {
        let key = (provider.to_string(), field.to_string());
        if let Some(cached) = self.cache.lock().unwrap().get(&key) {
            return cached.clone();
        }
        let value = Self::lookup(&Self::account(provider, field));
        self.cache.lock().unwrap().insert(key, value.clone());
        value
    }

    fn source_name(&self) -> &'static str {
        "keychain"
    }
}

/// First source with a value wins.
pub struct LayeredCredentials {
    sources: Vec<Arc<dyn CredentialSource>>,
}

impl LayeredCredentials {
    pub fn new(sources: Vec<Arc<dyn CredentialSource>>) -> Self {
        Self { sources }
    }

    /// GUI process: the environment (explicit developer overrides) beats
    /// what the host pushed from its keychain.
    pub fn for_app(memory: Arc<InMemoryCredentials>) -> Self {
        Self::new(vec![Arc::new(EnvCredentials), memory])
    }

    /// Daemons: the environment beats the login keychain read through the
    /// `security` tool.
    pub fn for_daemon() -> Self {
        Self::new(vec![
            Arc::new(EnvCredentials),
            Arc::new(KeychainCliCredentials::new()),
        ])
    }
}

impl CredentialSource for LayeredCredentials {
    fn get(&self, provider: &str, field: &str) -> Option<Secret> {
        self.sources
            .iter()
            .find_map(|source| source.get(provider, field))
    }

    fn source_name(&self) -> &'static str {
        "layered"
    }

    fn source_of(&self, provider: &str, field: &str) -> Option<&'static str> {
        self.sources
            .iter()
            .find_map(|source| source.source_of(provider, field))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialFieldStatus {
    pub field: String,
    pub secret: bool,
    pub optional: bool,
    pub configured: bool,
    pub source: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialStatus {
    pub provider: String,
    pub fields: Vec<CredentialFieldStatus>,
}

impl CredentialStatus {
    pub fn missing_required(&self) -> Vec<String> {
        self.fields
            .iter()
            .filter(|field| !field.optional && !field.configured)
            .map(|field| field.field.clone())
            .collect()
    }
}

/// Status of every credential field a descriptor declares.
pub fn credential_status(
    descriptor: &ProviderDescriptor,
    credentials: &dyn CredentialSource,
) -> CredentialStatus {
    CredentialStatus {
        provider: descriptor.id.into(),
        fields: descriptor
            .credential_fields
            .iter()
            .map(|field| {
                let source = credentials.source_of(descriptor.id, field.id);
                CredentialFieldStatus {
                    field: field.id.into(),
                    secret: field.secret,
                    optional: field.optional,
                    configured: source.is_some(),
                    source,
                }
            })
            .collect(),
    }
}

/// Convenience for callers that only have an id.
pub fn credential_status_for(
    id: &str,
    credentials: &dyn CredentialSource,
) -> Option<CredentialStatus> {
    catalogue::descriptor(id).map(|descriptor| credential_status(descriptor, credentials))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_variable_names_follow_the_impress_grammar() {
        assert_eq!(
            EnvCredentials::variable_name("omlx", "apiKey"),
            "IMPRESS_OMLX_API_KEY"
        );
        assert_eq!(
            EnvCredentials::variable_name("openai-compatible", "apiKey"),
            "IMPRESS_OPENAI_COMPATIBLE_API_KEY"
        );
        assert_eq!(
            KeychainCliCredentials::account("anthropic", "apiKey"),
            "com.impressai.credentials.anthropic.apiKey"
        );
    }

    #[test]
    fn memory_credentials_replace_and_clear_per_provider() {
        let memory = InMemoryCredentials::new();
        memory.set_all(
            "anthropic",
            HashMap::from([("apiKey".to_string(), "sk-ant-1".to_string())]),
        );
        memory.set_all(
            "openai",
            HashMap::from([("apiKey".to_string(), "sk-1".to_string())]),
        );
        assert_eq!(
            memory.get("anthropic", "apiKey").unwrap().expose(),
            "sk-ant-1"
        );
        memory.set_all(
            "anthropic",
            HashMap::from([("apiKey".to_string(), "".to_string())]),
        );
        assert!(memory.get("anthropic", "apiKey").is_none());
        assert!(memory.get("openai", "apiKey").is_some());
        memory.clear("openai");
        assert!(memory.get("openai", "apiKey").is_none());
    }

    #[test]
    fn layered_precedence_and_status_report_the_source_not_the_value() {
        let memory = InMemoryCredentials::new();
        memory.set_all(
            "openai",
            HashMap::from([("apiKey".to_string(), "sk-memory".to_string())]),
        );
        let layered = LayeredCredentials::for_app(memory);
        assert_eq!(
            layered.get("openai", "apiKey").unwrap().expose(),
            "sk-memory"
        );
        assert_eq!(layered.source_of("openai", "apiKey"), Some("memory"));
        assert!(layered.get("google", "apiKey").is_none());

        let status = credential_status_for("openai", &layered).unwrap();
        assert!(status.fields[0].configured);
        assert_eq!(status.fields[0].source, Some("memory"));
        assert!(status.missing_required().is_empty());
        let google = credential_status_for("google", &layered).unwrap();
        assert_eq!(google.missing_required(), ["apiKey"]);
        let omlx = credential_status_for("omlx", &layered).unwrap();
        assert!(omlx.missing_required().is_empty(), "the bearer is optional");
        assert!(!format!("{status:?}").contains("sk-memory"));
    }
}
