//! Runtime provider declarations and lifecycle (ADR-0034 D7, P8).
//!
//! The linked inventory remains static. Provider metadata is owned by `Arc`
//! snapshots and never leaked to satisfy a generated descriptor's lifetime.
//! This module has no store dependency: the host supplies schema validation,
//! persistence, credential storage, and an authenticated registration route.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, RwLock,
};

// Replacing a registry must not make an old handle current again. Generations
// are unique across registries in this Rust image, not just within a provider.
static GENERATION: AtomicU64 = AtomicU64::new(1);
fn next_generation() -> u64 {
    GENERATION.fetch_add(1, Ordering::Relaxed)
}

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::descriptor::{Safety, SafetyClass, VerbDescriptor};
use crate::descriptor_handle::VerbHandle;
use crate::McpToolDescriptor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderStatus {
    Available,
    Unavailable,
}

#[derive(Debug, Clone)]
pub struct ProviderExample {
    pub name: String,
    pub args: String,
    pub expect: Option<String>,
}

/// An immutable view of one provider verb. A registration or trust change
/// replaces the Arc; old handles remain memory-safe but are rejected by
/// `Registry::current_connection` when their generation is stale.
#[derive(Debug)]
pub struct ProviderVerb {
    pub name: String,
    pub service: String,
    pub method: String,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Value,
    pub declared_safety: Safety,
    pub effective_safety: Safety,
    pub since: String,
    pub examples: Vec<ProviderExample>,
    pub provider_id: String,
    pub status: ProviderStatus,
    pub deprecated_since: Option<String>,
    pub generation: u64,
}

/// Only the host's authenticated route may submit this request. An incoming
/// `token` proves an existing registration; the host always issues the next
/// token itself and never accepts a submitted `trusted` value.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderIdentityInput {
    pub id: String,
    pub language: String,
    pub version: String,
    pub endpoint: String,
    #[serde(default)]
    pub token: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrationRequest {
    pub provider: ProviderIdentityInput,
    pub verbs: Vec<ProviderVerbInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SafetyClaim {
    pub class: String,
    #[serde(default)]
    pub idempotent: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderExampleInput {
    pub name: String,
    pub args: Value,
    #[serde(default)]
    pub expect: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderVerbInput {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Value,
    pub safety: SafetyClaim,
    pub since: String,
    pub examples: Vec<ProviderExampleInput>,
}

/// The host stores this under `provider@1.0.0`. It contains only a token
/// hash; raw credentials belong in the host's private credential store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedProvider {
    pub id: String,
    pub language: String,
    pub version: String,
    pub endpoint: String,
    pub trusted: bool,
    pub token_hash: String,
    #[serde(default)]
    pub deregistered: bool,
    pub verbs: Vec<PersistedVerb>,
}

/// Public catalogue metadata for a person's provider trust review. Raw
/// credentials and their hashes are deliberately absent from this DTO.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProviderSummary {
    pub id: String,
    pub language: String,
    pub version: String,
    pub endpoint: String,
    pub trusted: bool,
    pub available: bool,
    pub deregistered: bool,
    pub verbs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedVerb {
    pub descriptor: ProviderVerbInput,
    pub dropped: bool,
    pub deprecated_since: Option<String>,
}

/// Implemented by the host's JSON Schema engine. Without an installed
/// validator, registration fails closed, including on a restored record.
pub trait SchemaValidator: Send + Sync {
    fn validate(&self, schema: &Value) -> Result<(), String>;
    /// Validate one provider argument object against the whole declared
    /// schema, including required fields, types, and nested constraints.
    fn validate_instance(&self, schema: &Value, value: &Value) -> Result<(), String>;
}

/// Store/credential adapter implemented below the service-core dependency
/// boundary. `save_registration` must commit the row and private token
/// together or leave the old row unchanged. The credential is written to an
/// immutable private file keyed by its hash before the row points to it; a
/// failed row commit can leave an unreferenced private file. `save_trust`
/// never modifies credentials.
pub trait ProviderPersistence: Send + Sync {
    fn load(&self) -> Result<Vec<PersistedProvider>, String>;
    /// `previous` is the registry's complete persisted snapshot (or `None`
    /// for a new id). Implementations must refuse a stale snapshot before
    /// replacing any descriptor, trust decision, or credential hash.
    fn save_registration(
        &self,
        row: &PersistedProvider,
        token: &str,
        previous: Option<&PersistedProvider>,
    ) -> Result<(), String>;
    fn save_trust(&self, row: &PersistedProvider) -> Result<(), String>;
    fn load_token(&self, provider_id: &str) -> Result<Option<String>, String>;
    fn save_status(&self, row: &PersistedProvider) -> Result<(), String>;
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegistrationError {
    #[error("JSON Schema validator is not installed")]
    ValidatorUnavailable,
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Credential(String),
    #[error("provider persistence failed: {0}")]
    Persistence(String),
}

/// The raw token is returned once to the authenticated registering process.
/// Do not log or serialize the receipt as a whole.
pub struct RegistrationReceipt {
    pub provider_id: String,
    pub token: String,
    pub version: String,
    pub registered: usize,
    pub collisions: Vec<String>,
}

/// Private transport facts, available only for a current, healthy handle.
/// Deliberately has no `Debug` or `Serialize` implementation.
pub struct ProviderConnection {
    endpoint: String,
    token: String,
    generation: u64,
}

impl ProviderConnection {
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
    pub fn token(&self) -> &str {
        &self.token
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

struct ProviderEntry {
    row: PersistedProvider,
    token: Option<String>,
    healthy: bool,
    generation: u64,
    verbs: BTreeMap<String, Arc<ProviderVerb>>,
}

#[derive(Default)]
struct State {
    providers: BTreeMap<String, ProviderEntry>,
}

/// A local registry. The host owns its process-global installation; tests can
/// use a separate instance and scratch persistence without touching a store.
pub struct Registry {
    state: RwLock<State>,
    validator: Option<Arc<dyn SchemaValidator>>,
    persistence: Option<Arc<dyn ProviderPersistence>>,
    revision: AtomicU64,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    pub fn new() -> Self {
        Self {
            state: RwLock::new(State::default()),
            validator: None,
            persistence: None,
            revision: AtomicU64::new(next_generation()),
        }
    }

    /// Monotonic catalogue epoch for native UI caches. A repeated identical
    /// health result leaves it stable; observable provider changes advance it.
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    pub(crate) fn bump_revision(&self) {
        self.revision
            .fetch_max(next_generation(), Ordering::Release);
    }

    pub fn with_validator(mut self, validator: Arc<dyn SchemaValidator>) -> Self {
        self.validator = Some(validator);
        self
    }

    /// Validate a provider call before transport. The generated strict-args
    /// check names unknown keys; the injected schema engine then enforces the
    /// complete input contract. Never include an argument value in a refusal.
    pub fn validate_args(
        &self,
        verb: &ProviderVerb,
        args: &Value,
    ) -> Result<(), crate::refusal::Refusal> {
        crate::strict::check_args(&verb.name, args, &verb.input_schema)?;
        let validator = self.validator.as_ref().ok_or_else(|| {
            crate::refusal::Refusal::internal("provider schema validator unavailable")
        })?;
        validator
            .validate_instance(&verb.input_schema, args)
            .map_err(|error| crate::refusal::Refusal::invalid_argument(error).context(&verb.name))
    }

    /// A provider owns its output bytes, so a successful HTTP response is
    /// still untrusted until it matches the declared output contract. Never
    /// echo the value or the validator's potentially value-bearing error.
    pub fn validate_output(
        &self,
        verb: &ProviderVerb,
        value: &Value,
    ) -> Result<(), crate::refusal::Refusal> {
        let validator = self.validator.as_ref().ok_or_else(|| {
            crate::refusal::Refusal::internal("provider schema validator unavailable")
        })?;
        validator
            .validate_instance(&verb.output_schema, value)
            .map_err(|_| {
                crate::refusal::Refusal::new(
                    crate::refusal::codes::VERB_FAILED,
                    format!(
                        "{} returned a value outside its declared output schema",
                        verb.name
                    ),
                )
            })
    }

    /// Hydrated verbs start unavailable. A successful explicit health probe
    /// can revive them without re-registration when the private token exists.
    pub fn with_persistence(
        mut self,
        persistence: Arc<dyn ProviderPersistence>,
    ) -> Result<Self, RegistrationError> {
        let validator = self
            .validator
            .as_ref()
            .ok_or(RegistrationError::ValidatorUnavailable)?;
        let mut state = State::default();
        for row in persistence.load().map_err(RegistrationError::Persistence)? {
            validate_row(&row, validator.as_ref())?;
            if state.providers.contains_key(&row.id) {
                return Err(RegistrationError::Invalid(format!(
                    "duplicate persisted provider {}",
                    row.id
                )));
            }
            for verb in &row.verbs {
                if state
                    .providers
                    .values()
                    .any(|entry| entry.verbs.contains_key(&verb.descriptor.name))
                {
                    return Err(RegistrationError::Invalid(format!(
                        "persisted verb {} has multiple provider owners",
                        verb.descriptor.name
                    )));
                }
            }
            let token = persistence
                .load_token(&row.id)
                .map_err(RegistrationError::Persistence)?;
            let token = token.filter(|token| token_matches(&row.token_hash, token));
            let generation = next_generation();
            let verbs = build_verbs(&row, false, generation);
            state.providers.insert(
                row.id.clone(),
                ProviderEntry {
                    row,
                    token,
                    healthy: false,
                    generation,
                    verbs,
                },
            );
        }
        self.state = RwLock::new(state);
        self.persistence = Some(persistence);
        self.bump_revision();
        Ok(self)
    }

    /// Incorporate another local host's registration or trust change. Unchanged
    /// rows retain their live handles; changed rows must pass health again.
    /// Holding our state lock across the load prevents an older snapshot from
    /// overwriting a registration this same host just committed.
    pub fn refresh_persisted(&self) -> Result<(), RegistrationError> {
        let Some(persistence) = &self.persistence else {
            return Ok(());
        };
        let validator = self
            .validator
            .as_ref()
            .ok_or(RegistrationError::ValidatorUnavailable)?;
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        let loaded = Registry::new()
            .with_validator(validator.clone())
            .with_persistence(persistence.clone());
        let loaded = match loaded {
            Ok(loaded) => loaded,
            Err(error) => {
                // A failed trust read cannot leave a stale trusted handle live.
                let changed = state.providers.values().any(|entry| entry.healthy);
                for entry in state.providers.values_mut() {
                    entry.healthy = false;
                    entry.generation = next_generation();
                    entry.verbs = build_verbs(&entry.row, false, entry.generation);
                }
                if changed {
                    self.bump_revision();
                }
                return Err(error);
            }
        };
        let mut fresh = loaded.state.into_inner().unwrap_or_else(|e| e.into_inner());
        let mut changed = state.providers.keys().collect::<Vec<_>>()
            != fresh.providers.keys().collect::<Vec<_>>();
        for (id, mut old) in std::mem::take(&mut state.providers) {
            match fresh.providers.get(&id) {
                Some(new)
                    if serde_json::to_value(&old.row).ok()
                        == serde_json::to_value(&new.row).ok()
                        && old.token == new.token =>
                {
                    fresh.providers.insert(id, old);
                }
                Some(_) => changed = true,
                None => {
                    changed = true;
                    // A deleted row is retained for named diagnostics, with its
                    // credential revoked. Health cannot revive that entry.
                    old.token = None;
                    old.healthy = false;
                    old.row.trusted = false;
                    old.generation = next_generation();
                    old.verbs = build_verbs(&old.row, false, old.generation);
                    fresh.providers.insert(id, old);
                }
            }
        }
        *state = fresh;
        if changed {
            self.bump_revision();
        }
        Ok(())
    }

    pub fn register(
        &self,
        request: RegistrationRequest,
    ) -> Result<RegistrationReceipt, RegistrationError> {
        let validator = self
            .validator
            .as_ref()
            .ok_or(RegistrationError::ValidatorUnavailable)?;
        validate_request(&request, validator.as_ref())?;
        let id = &request.provider.id;
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        let previous = state.providers.get(id);
        if let Some(previous) = previous {
            let old_token = request.provider.token.as_deref().ok_or_else(|| {
                RegistrationError::Credential(format!(
                    "provider {id} must authenticate re-registration"
                ))
            })?;
            if !token_matches(&previous.row.token_hash, old_token) {
                return Err(RegistrationError::Credential(format!(
                    "invalid credential for provider {id}"
                )));
            }
            if version_cmp(&request.provider.version, &previous.row.version)?
                == std::cmp::Ordering::Less
            {
                return Err(RegistrationError::Invalid(format!(
                    "provider {id} version went backwards"
                )));
            }
        } else if request.provider.token.is_some() {
            return Err(RegistrationError::Credential(
                "a new provider cannot choose its token".into(),
            ));
        }

        let incoming: BTreeSet<String> = request.verbs.iter().map(|v| v.name.clone()).collect();
        for verb in &request.verbs {
            if linked_name_or_alias(&verb.name) {
                tracing::warn!(provider = %id, verb = %verb.name, "linked verb wins provider collision");
            }
            if state
                .providers
                .iter()
                .any(|(other, entry)| other != id && entry.verbs.contains_key(&verb.name))
            {
                return Err(RegistrationError::Invalid(format!(
                    "{} is already owned by another provider",
                    verb.name
                )));
            }
            if let Some(old) =
                previous.and_then(|p| p.row.verbs.iter().find(|v| v.descriptor.name == verb.name))
            {
                if version_cmp(&verb.since, &old.descriptor.since)? == std::cmp::Ordering::Less {
                    return Err(RegistrationError::Invalid(format!(
                        "{} since went backwards",
                        verb.name
                    )));
                }
            }
        }
        let mut verbs: Vec<PersistedVerb> = request
            .verbs
            .into_iter()
            .map(|descriptor| PersistedVerb {
                descriptor,
                dropped: false,
                deprecated_since: None,
            })
            .collect();
        if let Some(previous) = previous {
            for old in &previous.row.verbs {
                if !incoming.contains(old.descriptor.name.as_str()) {
                    let mut dropped = old.clone();
                    dropped.dropped = true;
                    dropped
                        .deprecated_since
                        .get_or_insert_with(|| request.provider.version.clone());
                    verbs.push(dropped);
                }
            }
        }
        let row = PersistedProvider {
            id: id.clone(),
            language: request.provider.language,
            version: request.provider.version,
            endpoint: request.provider.endpoint,
            trusted: previous.is_some_and(|p| p.row.trusted),
            token_hash: String::new(),
            deregistered: false,
            verbs,
        };
        let token = new_token();
        let row = PersistedProvider {
            token_hash: hash_token(&token),
            ..row
        };
        if let Some(store) = &self.persistence {
            store
                .save_registration(&row, &token, previous.map(|entry| &entry.row))
                .map_err(RegistrationError::Persistence)?;
        }
        let generation = next_generation();
        let built = build_verbs(&row, true, generation);
        let collisions = built
            .keys()
            .filter(|name| linked_name_or_alias(name))
            .cloned()
            .collect();
        let registered = built
            .values()
            .filter(|verb| verb.deprecated_since.is_none())
            .count();
        state.providers.insert(
            id.clone(),
            ProviderEntry {
                row: row.clone(),
                token: Some(token.clone()),
                healthy: true,
                generation,
                verbs: built,
            },
        );
        self.bump_revision();
        Ok(RegistrationReceipt {
            provider_id: id.clone(),
            token,
            version: row.version,
            registered,
            collisions,
        })
    }

    /// Host-only: the authenticated route must require a real person's
    /// authority before calling this. Provider registration cannot set it.
    pub fn set_trusted(&self, id: &str, trusted: bool) -> Result<(), RegistrationError> {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        let entry = state
            .providers
            .get_mut(id)
            .ok_or_else(|| RegistrationError::Invalid(format!("unknown provider {id}")))?;
        if entry.row.trusted == trusted {
            return Ok(());
        }
        let mut row = entry.row.clone();
        row.trusted = trusted;
        if let Some(store) = &self.persistence {
            store
                .save_trust(&row)
                .map_err(RegistrationError::Persistence)?;
        }
        entry.generation = next_generation();
        entry.verbs = build_verbs(&row, entry.healthy, entry.generation);
        entry.row = row;
        self.bump_revision();
        Ok(())
    }

    /// Health changes liveness without changing descriptor facts or token.
    pub fn set_health(&self, id: &str, healthy: bool) -> Result<(), RegistrationError> {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        let entry = state
            .providers
            .get_mut(id)
            .ok_or_else(|| RegistrationError::Invalid(format!("unknown provider {id}")))?;
        if healthy && entry.token.is_none() {
            return Err(RegistrationError::Credential(format!(
                "provider {id} has no private credential"
            )));
        }
        if healthy && entry.row.deregistered {
            return Err(RegistrationError::Invalid(format!(
                "provider {id} was deregistered and must re-register"
            )));
        }
        if entry.healthy != healthy {
            self.bump_revision();
        }
        entry.healthy = healthy;
        entry.verbs = build_verbs(&entry.row, healthy, entry.generation);
        Ok(())
    }

    /// Apply a probe result only if no registration, trust, or deregistration
    /// happened while the probe was in flight. Returns false for stale probes.
    pub fn set_health_if_current(
        &self,
        id: &str,
        generation: u64,
        healthy: bool,
    ) -> Result<bool, RegistrationError> {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        let entry = state
            .providers
            .get_mut(id)
            .ok_or_else(|| RegistrationError::Invalid(format!("unknown provider {id}")))?;
        if entry.generation != generation || entry.row.deregistered {
            return Ok(false);
        }
        if healthy && entry.token.is_none() {
            return Err(RegistrationError::Credential(format!(
                "provider {id} has no private credential"
            )));
        }
        if entry.healthy != healthy {
            self.bump_revision();
        }
        entry.healthy = healthy;
        entry.verbs = build_verbs(&entry.row, healthy, entry.generation);
        Ok(true)
    }

    pub fn deregister(&self, id: &str, token: &str) -> Result<(), RegistrationError> {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        let entry = state
            .providers
            .get_mut(id)
            .ok_or_else(|| RegistrationError::Invalid(format!("unknown provider {id}")))?;
        if !token_matches(&entry.row.token_hash, token) {
            return Err(RegistrationError::Credential(format!(
                "invalid credential for provider {id}"
            )));
        }
        let mut row = entry.row.clone();
        row.deregistered = true;
        if let Some(store) = &self.persistence {
            store
                .save_status(&row)
                .map_err(RegistrationError::Persistence)?;
        }
        entry.row = row;
        entry.healthy = false;
        entry.generation = next_generation();
        entry.verbs = build_verbs(&entry.row, false, entry.generation);
        self.bump_revision();
        Ok(())
    }

    pub fn authenticate(&self, id: &str, token: &str) -> bool {
        let state = self
            .state
            .read()
            .unwrap_or_else(|poison| poison.into_inner());
        state.providers.get(id).is_some_and(|entry| {
            !entry.row.deregistered && token_matches(&entry.row.token_hash, token)
        })
    }

    pub fn find(&self, name: &str) -> Option<VerbHandle> {
        if let Some(linked) = VerbDescriptor::find(name) {
            return Some(VerbHandle::Linked(linked));
        }
        if let Some(linked) = McpToolDescriptor::iter()
            .find(|descriptor| descriptor.verb.has_alias(name))
            .map(|descriptor| descriptor.verb)
        {
            return Some(VerbHandle::Linked(linked));
        }
        let state = self
            .state
            .read()
            .unwrap_or_else(|poison| poison.into_inner());
        state
            .providers
            .values()
            .find_map(|entry| entry.verbs.get(name).cloned().map(VerbHandle::Provider))
    }

    pub fn descriptors(&self) -> Vec<VerbHandle> {
        let mut out: Vec<VerbHandle> = VerbDescriptor::iter().map(VerbHandle::Linked).collect();
        let linked: BTreeSet<String> = out.iter().map(|verb| verb.name().to_owned()).collect();
        let state = self
            .state
            .read()
            .unwrap_or_else(|poison| poison.into_inner());
        for entry in state.providers.values() {
            for (name, verb) in &entry.verbs {
                if !linked.contains(name.as_str()) && !linked_name_or_alias(name) {
                    out.push(VerbHandle::Provider(verb.clone()));
                }
            }
        }
        out
    }

    /// Stable id-order metadata for a generated person-review form. No
    /// credential material or unhashed token facts leave the registry.
    pub fn summaries(&self) -> Vec<ProviderSummary> {
        let state = self
            .state
            .read()
            .unwrap_or_else(|poison| poison.into_inner());
        state
            .providers
            .iter()
            .map(|(id, entry)| ProviderSummary {
                id: id.clone(),
                language: entry.row.language.clone(),
                version: entry.row.version.clone(),
                endpoint: entry.row.endpoint.clone(),
                trusted: entry.row.trusted,
                available: entry.healthy && entry.token.is_some() && !entry.row.deregistered,
                deregistered: entry.row.deregistered,
                verbs: entry
                    .row
                    .verbs
                    .iter()
                    .filter(|verb| !verb.dropped)
                    .map(|verb| verb.descriptor.name.clone())
                    .collect(),
            })
            .collect()
    }

    /// Connections available for explicit liveness probes. A restored entry
    /// may be unhealthy yet still has a valid private credential. A
    /// deregistered entry remains listed in inventory but is not probed.
    pub fn health_connections(&self) -> Vec<(String, ProviderConnection)> {
        let state = self
            .state
            .read()
            .unwrap_or_else(|poison| poison.into_inner());
        state
            .providers
            .iter()
            .filter_map(|(id, entry)| {
                if entry.row.deregistered {
                    return None;
                }
                entry.token.as_ref().map(|token| {
                    (
                        id.clone(),
                        ProviderConnection {
                            endpoint: entry.row.endpoint.clone(),
                            token: token.clone(),
                            generation: entry.generation,
                        },
                    )
                })
            })
            .collect()
    }

    /// Reject an old snapshot after rotation, trust revocation, or drop.
    pub fn current_connection(&self, verb: &ProviderVerb) -> Option<ProviderConnection> {
        let state = self.state.read().ok()?;
        let entry = state.providers.get(&verb.provider_id)?;
        let current = entry.verbs.get(&verb.name)?;
        if entry.generation != verb.generation
            || !entry.healthy
            || entry.row.deregistered
            || current.deprecated_since.is_some()
            || current.status != ProviderStatus::Available
        {
            return None;
        }
        Some(ProviderConnection {
            endpoint: entry.row.endpoint.clone(),
            token: entry.token.clone()?,
            generation: entry.generation,
        })
    }
}

fn linked_name_or_alias(name: &str) -> bool {
    McpToolDescriptor::iter().any(|d| d.name == name || d.verb.has_alias(name))
}

fn build_verbs(
    row: &PersistedProvider,
    healthy: bool,
    generation: u64,
) -> BTreeMap<String, Arc<ProviderVerb>> {
    let service = format!("{}-service", row.id);
    row.verbs
        .iter()
        .map(|stored| {
            let input = &stored.descriptor;
            let method = input
                .name
                .strip_prefix(&format!("{service}_"))
                .unwrap_or_default()
                .to_string();
            let class = SafetyClass::parse(&input.safety.class).expect("validated safety class");
            let declared_safety = Safety {
                class,
                idempotent: input
                    .safety
                    .idempotent
                    .unwrap_or(class.default_idempotent()),
            };
            let effective_safety = if row.trusted {
                declared_safety
            } else {
                Safety {
                    class: SafetyClass::External,
                    idempotent: false,
                }
            };
            let verb = Arc::new(ProviderVerb {
                name: input.name.clone(),
                service: service.clone(),
                method,
                description: input.description.clone(),
                input_schema: input.input_schema.clone(),
                output_schema: input.output_schema.clone(),
                declared_safety,
                effective_safety,
                since: input.since.clone(),
                examples: input
                    .examples
                    .iter()
                    .map(|example| ProviderExample {
                        name: example.name.clone(),
                        args: example.args.to_string(),
                        expect: example.expect.as_ref().map(Value::to_string),
                    })
                    .collect(),
                provider_id: row.id.clone(),
                status: if healthy && !stored.dropped {
                    ProviderStatus::Available
                } else {
                    ProviderStatus::Unavailable
                },
                deprecated_since: stored.deprecated_since.clone(),
                generation,
            });
            (verb.name.clone(), verb)
        })
        .collect()
}

fn validate_request(
    request: &RegistrationRequest,
    validator: &dyn SchemaValidator,
) -> Result<(), RegistrationError> {
    let p = &request.provider;
    if !valid_kebab(&p.id) || p.language.trim().is_empty() || p.endpoint.trim().is_empty() {
        return Err(RegistrationError::Invalid(
            "provider id, language, and endpoint are required; id must be kebab-case".into(),
        ));
    }
    version_parts(&p.version)?;
    if request.verbs.is_empty() {
        return Err(RegistrationError::Invalid(
            "provider must register at least one verb".into(),
        ));
    }
    let mut names = BTreeSet::new();
    for verb in &request.verbs {
        validate_verb(&p.id, verb, validator)?;
        if !names.insert(verb.name.as_str()) {
            return Err(RegistrationError::Invalid(format!(
                "duplicate verb {}",
                verb.name
            )));
        }
    }
    Ok(())
}

fn validate_row(
    row: &PersistedProvider,
    validator: &dyn SchemaValidator,
) -> Result<(), RegistrationError> {
    validate_request(
        &RegistrationRequest {
            provider: ProviderIdentityInput {
                id: row.id.clone(),
                language: row.language.clone(),
                version: row.version.clone(),
                endpoint: row.endpoint.clone(),
                token: None,
            },
            verbs: row.verbs.iter().map(|v| v.descriptor.clone()).collect(),
        },
        validator,
    )
}

fn validate_verb(
    id: &str,
    verb: &ProviderVerbInput,
    validator: &dyn SchemaValidator,
) -> Result<(), RegistrationError> {
    let service = format!("{id}-service_");
    let Some(method) = verb.name.strip_prefix(&service) else {
        return Err(RegistrationError::Invalid(format!(
            "{} must start with {service}",
            verb.name
        )));
    };
    if !valid_kebab(method) {
        return Err(RegistrationError::Invalid(format!(
            "{} has an invalid method name",
            verb.name
        )));
    }
    if verb.description.trim().is_empty() {
        return Err(RegistrationError::Invalid(format!(
            "{} has no description",
            verb.name
        )));
    }
    version_parts(&verb.since)?;
    if SafetyClass::parse(&verb.safety.class).is_none() {
        return Err(RegistrationError::Invalid(format!(
            "{} has no valid safety class",
            verb.name
        )));
    }
    validator
        .validate(&verb.input_schema)
        .map_err(|e| RegistrationError::Invalid(format!("{} input schema: {e}", verb.name)))?;
    validator
        .validate(&verb.output_schema)
        .map_err(|e| RegistrationError::Invalid(format!("{} output schema: {e}", verb.name)))?;
    if verb.input_schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err(RegistrationError::Invalid(format!(
            "{} arguments must have an object schema",
            verb.name
        )));
    }
    let properties = verb
        .input_schema
        .get("properties")
        .and_then(Value::as_object);
    if let Some(properties) = properties {
        for (name, property) in properties {
            if property
                .get("description")
                .and_then(Value::as_str)
                .is_none_or(|doc| doc.trim().is_empty())
            {
                return Err(RegistrationError::Invalid(format!(
                    "{} argument {name} is undocumented",
                    verb.name
                )));
            }
        }
    }
    if verb
        .input_schema
        .get("additionalProperties")
        .is_none_or(|extra| extra != &Value::Bool(false))
    {
        return Err(RegistrationError::Invalid(format!(
            "{} arguments must declare additionalProperties: false",
            verb.name
        )));
    }
    if verb.examples.is_empty() {
        return Err(RegistrationError::Invalid(format!(
            "{} needs an example",
            verb.name
        )));
    }
    for example in &verb.examples {
        if example.name.trim().is_empty() || !example.args.is_object() {
            return Err(RegistrationError::Invalid(format!(
                "{} has an invalid example",
                verb.name
            )));
        }
        crate::strict::check_args(&verb.name, &example.args, &verb.input_schema).map_err(|e| {
            RegistrationError::Invalid(format!("{} example {}: {e}", verb.name, example.name))
        })?;
        validator
            .validate_instance(&verb.input_schema, &example.args)
            .map_err(|error| {
                RegistrationError::Invalid(format!(
                    "{} example {}: {error}",
                    verb.name, example.name
                ))
            })?;
    }
    Ok(())
}

fn valid_kebab(value: &str) -> bool {
    !value.is_empty()
        && value.split('-').all(|word| {
            !word.is_empty()
                && word
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

fn version_parts(value: &str) -> Result<Vec<u64>, RegistrationError> {
    if value.is_empty() {
        return Err(RegistrationError::Invalid(
            "version must be numeric dotted notation".into(),
        ));
    }
    value
        .split('.')
        .map(|part| {
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(RegistrationError::Invalid(format!(
                    "invalid version {value}"
                )));
            }
            part.parse::<u64>()
                .map_err(|_| RegistrationError::Invalid(format!("invalid version {value}")))
        })
        .collect()
}

fn version_cmp(a: &str, b: &str) -> Result<std::cmp::Ordering, RegistrationError> {
    let a = version_parts(a)?;
    let b = version_parts(b)?;
    let len = a.len().max(b.len());
    Ok((0..len)
        .map(|i| {
            a.get(i)
                .copied()
                .unwrap_or(0)
                .cmp(&b.get(i).copied().unwrap_or(0))
        })
        .find(|order| *order != std::cmp::Ordering::Equal)
        .unwrap_or(std::cmp::Ordering::Equal))
}

fn new_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
fn hash_token(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}
fn token_matches(expected_hash: &str, candidate: &str) -> bool {
    let actual = hash_token(candidate);
    if expected_hash.len() != actual.len() {
        return false;
    }
    expected_hash
        .as_bytes()
        .iter()
        .zip(actual.as_bytes())
        .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Default)]
    struct MemoryPersistence {
        row: RwLock<Option<PersistedProvider>>,
        token: RwLock<Option<String>>,
    }

    impl ProviderPersistence for MemoryPersistence {
        fn load(&self) -> Result<Vec<PersistedProvider>, String> {
            Ok(self.row.read().unwrap().iter().cloned().collect())
        }

        fn save_registration(
            &self,
            row: &PersistedProvider,
            token: &str,
            previous: Option<&PersistedProvider>,
        ) -> Result<(), String> {
            let mut stored = self.row.write().unwrap();
            let matches = stored
                .as_ref()
                .map(serde_json::to_value)
                .transpose()
                .map_err(|error| error.to_string())?
                == previous
                    .map(serde_json::to_value)
                    .transpose()
                    .map_err(|error| error.to_string())?;
            if !matches {
                return Err("provider registration changed; refresh before retrying".into());
            }
            *stored = Some(row.clone());
            *self.token.write().unwrap() = Some(token.to_owned());
            Ok(())
        }

        fn save_trust(&self, row: &PersistedProvider) -> Result<(), String> {
            *self.row.write().unwrap() = Some(row.clone());
            Ok(())
        }

        fn load_token(&self, provider_id: &str) -> Result<Option<String>, String> {
            if self.row.read().unwrap().as_ref().map(|row| row.id.as_str()) != Some(provider_id) {
                return Ok(None);
            }
            Ok(self.token.read().unwrap().clone())
        }

        fn save_status(&self, row: &PersistedProvider) -> Result<(), String> {
            *self.row.write().unwrap() = Some(row.clone());
            Ok(())
        }
    }

    struct AcceptSchema;
    impl SchemaValidator for AcceptSchema {
        fn validate(&self, schema: &Value) -> Result<(), String> {
            if schema.is_object() {
                Ok(())
            } else {
                Err("not an object".into())
            }
        }

        fn validate_instance(&self, schema: &Value, value: &Value) -> Result<(), String> {
            let args = value.as_object().ok_or("arguments must be an object")?;
            for required in schema["required"].as_array().into_iter().flatten() {
                let name = required.as_str().ok_or("invalid required field")?;
                if !args.contains_key(name) {
                    return Err(format!("missing required argument {name}"));
                }
            }
            for (name, property) in schema["properties"].as_object().into_iter().flatten() {
                if property["type"] == "string" && args.get(name).is_some_and(|v| !v.is_string()) {
                    return Err(format!("argument {name} must be a string"));
                }
            }
            Ok(())
        }
    }

    fn request(id: &str) -> RegistrationRequest {
        RegistrationRequest {
            provider: ProviderIdentityInput {
                id: id.into(),
                language: "Python".into(),
                version: "1.0".into(),
                endpoint: "http://127.0.0.1:23190".into(),
                token: None,
            },
            verbs: vec![ProviderVerbInput {
                name: format!("{id}-service_echo"),
                description: "Echo a message".into(),
                input_schema: json!({"type":"object","properties":{"message":{"type":"string","description":"Message to echo"}},"required":["message"],"additionalProperties":false}),
                output_schema: json!({"type":"object","properties":{"message":{"type":"string"}}}),
                safety: SafetyClaim {
                    class: "read_only".into(),
                    idempotent: None,
                },
                since: "1.0".into(),
                examples: vec![ProviderExampleInput {
                    name: "one".into(),
                    args: json!({"message":"hi"}),
                    expect: Some(json!({"message":"hi"})),
                }],
            }],
        }
    }

    #[test]
    fn persisted_refresh_preserves_live_handles_but_revokes_changed_trust() {
        let storage = Arc::new(MemoryPersistence::default());
        let first = Registry::new()
            .with_validator(Arc::new(AcceptSchema))
            .with_persistence(storage.clone())
            .unwrap();
        first.register(request("fixture")).unwrap();
        first.set_trusted("fixture", true).unwrap();
        let other = Registry::new()
            .with_validator(Arc::new(AcceptSchema))
            .with_persistence(storage)
            .unwrap();
        other.set_health("fixture", true).unwrap();
        let VerbHandle::Provider(handle) = other.find("fixture-service_echo").unwrap() else {
            panic!("provider")
        };
        other.refresh_persisted().unwrap();
        assert!(other.current_connection(&handle).is_some());
        // Another registry's similarly named entry never accepts our handle.
        assert!(first.current_connection(&handle).is_none());
        first.set_trusted("fixture", false).unwrap();
        other.refresh_persisted().unwrap();
        assert!(other.current_connection(&handle).is_none());
        let refreshed = other.find("fixture-service_echo").unwrap();
        assert_eq!(refreshed.safety().class, SafetyClass::External);
        assert_eq!(
            refreshed.provider_status(),
            Some(ProviderStatus::Unavailable)
        );
    }

    #[test]
    fn registration_is_validated_and_untrusted_safety_is_external() {
        let registry = Registry::new().with_validator(Arc::new(AcceptSchema));
        let failed = Registry::new().register(request("fixture"));
        assert!(matches!(
            failed,
            Err(RegistrationError::ValidatorUnavailable)
        ));
        let receipt = registry.register(request("fixture")).unwrap();
        assert_eq!(receipt.registered, 1);
        let VerbHandle::Provider(verb) = registry.find("fixture-service_echo").unwrap() else {
            panic!("provider handle")
        };
        assert_eq!(verb.declared_safety.class, SafetyClass::ReadOnly);
        assert_eq!(verb.effective_safety.class, SafetyClass::External);
        assert_eq!(
            registry.current_connection(&verb).unwrap().token(),
            receipt.token
        );
    }

    #[test]
    fn summaries_are_stable_and_omit_credentials() {
        let registry = Registry::new().with_validator(Arc::new(AcceptSchema));
        registry.register(request("zeta")).unwrap();
        registry.register(request("alpha")).unwrap();
        let summaries = registry.summaries();
        assert_eq!(
            summaries
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "zeta"]
        );
        assert_eq!(summaries[0].verbs, vec!["alpha-service_echo"]);
        assert!(summaries[0].available);
        assert!(!summaries[0].trusted);
        let json = serde_json::to_value(&summaries).unwrap();
        assert!(json.to_string().contains("alpha-service_echo"));
        assert!(!json.to_string().contains("token"));
        assert!(!json.to_string().contains("hash"));
        registry.set_health("alpha", false).unwrap();
        assert!(!registry.summaries()[0].available);
    }

    #[test]
    fn revision_advances_only_for_observable_provider_changes() {
        let registry = Registry::new().with_validator(Arc::new(AcceptSchema));
        let initial = registry.revision();
        registry.register(request("epoch-fixture")).unwrap();
        let registered = registry.revision();
        assert!(registered > initial);
        registry.set_health("epoch-fixture", true).unwrap();
        assert_eq!(registry.revision(), registered);
        let generation = registry.health_connections()[0].1.generation();
        registry
            .set_health_if_current("epoch-fixture", generation, true)
            .unwrap();
        assert_eq!(registry.revision(), registered);
        registry.set_health("epoch-fixture", false).unwrap();
        let offline = registry.revision();
        assert!(offline > registered);
        registry.set_health("epoch-fixture", false).unwrap();
        assert_eq!(registry.revision(), offline);
        registry.set_trusted("epoch-fixture", true).unwrap();
        let trusted = registry.revision();
        assert!(trusted > offline);
        registry.set_trusted("epoch-fixture", true).unwrap();
        assert_eq!(registry.revision(), trusted);
    }

    #[test]
    fn trust_revocation_rotates_handle_and_registration_rotates_token() {
        let registry = Registry::new().with_validator(Arc::new(AcceptSchema));
        let first = registry.register(request("fixture")).unwrap();
        registry.set_trusted("fixture", true).unwrap();
        let VerbHandle::Provider(trusted) = registry.find("fixture-service_echo").unwrap() else {
            panic!("provider")
        };
        assert_eq!(trusted.effective_safety.class, SafetyClass::ReadOnly);
        registry.set_trusted("fixture", false).unwrap();
        assert!(registry.current_connection(&trusted).is_none());
        let mut next = request("fixture");
        next.provider.token = Some(first.token.clone());
        let second = registry.register(next).unwrap();
        assert_ne!(first.token, second.token);
        assert!(!registry.authenticate("fixture", &first.token));
        assert!(registry.authenticate("fixture", &second.token));
    }

    #[test]
    fn dropout_keeps_a_deprecated_unavailable_verb_and_checks_versions() {
        let registry = Registry::new().with_validator(Arc::new(AcceptSchema));
        let first = registry.register(request("fixture")).unwrap();
        let mut next = request("fixture");
        next.provider.token = Some(first.token);
        next.provider.version = "0.9".into();
        assert!(registry.register(next.clone()).is_err());
        next.provider.version = "1.1".into();
        next.verbs[0].name = "fixture-service_other".into();
        registry.register(next).unwrap();
        let old = registry.find("fixture-service_echo").unwrap();
        assert_eq!(old.provider_status(), Some(ProviderStatus::Unavailable));
        assert!(old.deprecation_notice().is_some());
    }

    #[test]
    fn invalid_names_docs_examples_and_trusted_submission_fail() {
        let registry = Registry::new().with_validator(Arc::new(AcceptSchema));
        let mut bad = request("fixture");
        bad.verbs[0].input_schema["properties"]["message"]
            .as_object_mut()
            .unwrap()
            .remove("description");
        assert!(registry.register(bad).is_err());
        let mut bad = request("fixture");
        bad.verbs[0].examples.clear();
        assert!(registry.register(bad).is_err());
        let mut bad = request("fixture");
        bad.verbs[0].examples[0].args = json!({});
        assert!(registry.register(bad).is_err(), "required example argument");
        let mut bad = request("fixture");
        bad.verbs[0].examples[0].args = json!({"message": 42});
        assert!(registry.register(bad).is_err(), "typed example argument");
        let submitted = json!({"provider":{"id":"fixture","language":"Python","version":"1.0","endpoint":"http://127.0.0.1:23190","trusted":true},"verbs":[]});
        assert!(serde_json::from_value::<RegistrationRequest>(submitted).is_err());
    }

    #[test]
    fn provider_call_arguments_are_validated_before_transport() {
        let registry = Registry::new().with_validator(Arc::new(AcceptSchema));
        registry.register(request("fixture")).unwrap();
        let VerbHandle::Provider(verb) = registry.find("fixture-service_echo").unwrap() else {
            panic!("provider handle")
        };
        registry
            .validate_args(&verb, &json!({"message": "hello"}))
            .unwrap();
        for args in [
            json!({}),
            json!({"message": 42}),
            json!({"unknown": "hello"}),
        ] {
            let refusal = registry
                .validate_args(&verb, &args)
                .expect_err("invalid args");
            assert_eq!(refusal.code, crate::refusal::codes::INVALID_ARGUMENT);
        }
    }

    #[test]
    fn restored_credentials_need_probe_but_deregistration_cannot_be_probed_back() {
        let store = Arc::new(MemoryPersistence::default());
        let registry = Registry::new()
            .with_validator(Arc::new(AcceptSchema))
            .with_persistence(store.clone())
            .unwrap();
        let receipt = registry.register(request("fixture")).unwrap();
        assert_eq!(registry.health_connections().len(), 1);
        let first_generation = registry.health_connections()[0].1.generation();
        registry.set_trusted("fixture", true).unwrap();
        assert!(!registry
            .set_health_if_current("fixture", first_generation, false)
            .unwrap());
        drop(registry);

        let restored = Registry::new()
            .with_validator(Arc::new(AcceptSchema))
            .with_persistence(store.clone())
            .unwrap();
        let old = restored.find("fixture-service_echo").unwrap();
        assert_eq!(old.provider_status(), Some(ProviderStatus::Unavailable));
        assert_eq!(restored.health_connections().len(), 1);
        let probe_generation = restored.health_connections()[0].1.generation();
        assert!(restored
            .set_health_if_current("fixture", probe_generation, true)
            .unwrap());
        let VerbHandle::Provider(current) = restored.find("fixture-service_echo").unwrap() else {
            panic!("provider handle")
        };
        assert!(restored.current_connection(&current).is_some());
        restored.deregister("fixture", &receipt.token).unwrap();
        assert!(!restored.authenticate("fixture", &receipt.token));
        assert!(restored.current_connection(&current).is_none());
        assert!(restored.health_connections().is_empty());
        assert!(restored.set_health("fixture", true).is_err());
        drop(restored);

        let restored = Registry::new()
            .with_validator(Arc::new(AcceptSchema))
            .with_persistence(store)
            .unwrap();
        assert!(restored.health_connections().is_empty());
        let mut next = request("fixture");
        next.provider.token = Some(receipt.token);
        next.provider.version = "1.1".into();
        restored.register(next).unwrap();
        assert_eq!(restored.health_connections().len(), 1);
    }
}
