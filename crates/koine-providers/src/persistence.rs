//! Private, workspace-local persistence for P8 runtime providers, over the
//! store port (docs/plan-koine-kit-moves.md KM12).
//!
//! The store row holds descriptors, the host trust decision, and only a
//! credential hash. A token is written first to a private hash-addressed file;
//! the row switches to that hash only after the file is durable. A failed row
//! write may leave an unreferenced file, but never points to a missing token.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use impress_item::schema::refs::PROVIDER as PROVIDER_SCHEMA;
use impress_item::{
    ActorKind, AttributedStore, GuardedWrite, Item, ItemId, ItemQuery, OperationIntent,
    OperationSpec, OperationType, Priority, RetentionTier, StoreError, Value as ItemValue,
    Visibility,
};
use impress_service_core::pipeline::{context, CallerIdentity};
use impress_service_core::provider::{
    PersistedProvider, PersistedVerb, ProviderPersistence, RegistrationError, Registry,
    SchemaValidator,
};
use sha2::{Digest, Sha256};

/// The adapter never opens a default store. Its caller supplies the exact
/// already-open store and the workspace that owns its credential files. The
/// rows are this device's: a store that syncs keeps `provider@1.0.0` out of
/// it (impress's SQLite store does, before it builds this).
pub struct StoreProviderPersistence {
    store: Arc<dyn AttributedStore>,
    credential_dir: PathBuf,
}

impl StoreProviderPersistence {
    pub fn new(store: Arc<dyn AttributedStore>, workspace: &Path) -> Result<Self, String> {
        let provider_dir = private_directory(&workspace.join("providers"))?;
        let credential_dir = private_directory(&provider_dir.join("credentials"))?;
        Ok(Self {
            store,
            credential_dir,
        })
    }

    fn row_item(&self, id: &str) -> Result<Option<Item>, String> {
        let item = self
            .store
            .get(row_id(id)?)
            .map_err(|error| format!("read provider row: {error}"))?;
        item.map(|item| {
            if item.schema != PROVIDER_SCHEMA {
                return Err(format!("provider {id} row has an unexpected schema"));
            }
            let row = decode_row(&item.payload)?;
            if row.id != id {
                return Err(format!("provider {id} row has an inconsistent id"));
            }
            Ok(item)
        })
        .transpose()
    }

    fn row(&self, id: &str) -> Result<Option<PersistedProvider>, String> {
        self.row_item(id)?
            .map(|item| decode_row(&item.payload))
            .transpose()
    }

    fn credential_path(&self, hash: &str) -> Result<PathBuf, String> {
        validate_hash(hash)?;
        let parent = self
            .credential_dir
            .parent()
            .ok_or_else(|| "provider credential directory has no parent".to_string())?;
        private_directory(parent)?;
        private_directory(&self.credential_dir)?;
        Ok(self.credential_dir.join(hash))
    }

    fn write_token(&self, row: &PersistedProvider, token: &str) -> Result<(), String> {
        validate_row_identity(row)?;
        if token.is_empty() || token_hash(token) != row.token_hash {
            return Err("provider credential does not match its row hash".into());
        }
        let path = self.credential_path(&row.token_hash)?;
        // create_new refuses an existing regular file or symlink. An existing
        // hash is safe only when its private contents are the same credential.
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                return if self.read_token(&row.token_hash)?.as_deref() == Some(token) {
                    Ok(())
                } else {
                    Err("provider credential path is occupied".into())
                };
            }
            Err(error) => return Err(format!("create private provider credential: {error}")),
        };
        if let Err(error) = file
            .write_all(token.as_bytes())
            .and_then(|()| file.sync_all())
        {
            drop(file);
            let _ = fs::remove_file(&path);
            return Err(format!("write private provider credential: {error}"));
        }
        File::open(&self.credential_dir)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("sync private provider credential directory: {error}"))?;
        Ok(())
    }

    fn read_token(&self, hash: &str) -> Result<Option<String>, String> {
        let path = self.credential_path(hash)?;
        let before = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("inspect provider credential: {error}")),
        };
        if !before.file_type().is_file() || before.permissions().mode() & 0o077 != 0 {
            return Err("provider credential is not a private regular file".into());
        }
        let mut file =
            File::open(&path).map_err(|error| format!("open provider credential: {error}"))?;
        let opened = file
            .metadata()
            .map_err(|error| format!("inspect open provider credential: {error}"))?;
        if !opened.is_file()
            || opened.permissions().mode() & 0o077 != 0
            || opened.len() > 256
            || opened.dev() != before.dev()
            || opened.ino() != before.ino()
        {
            return Err("provider credential changed or is not private".into());
        }
        let mut token = String::new();
        file.read_to_string(&mut token)
            .map_err(|error| format!("read provider credential: {error}"))?;
        if token.is_empty() || token_hash(&token) != hash {
            return Err("provider credential does not match its row hash".into());
        }
        Ok(Some(token))
    }

    fn update_flag(&self, row: &PersistedProvider, name: &str) -> Result<(), String> {
        validate_row_identity(row)?;
        let item = self
            .row_item(&row.id)?
            .ok_or_else(|| format!("provider {} is not stored", row.id))?;
        let existing = decode_row(&item.payload)?;
        let mut expected = row.clone();
        match name {
            "trusted" => expected.trusted = existing.trusted,
            "deregistered" => expected.deregistered = existing.deregistered,
            _ => return Err("unknown provider status field".into()),
        }
        if serde_json::to_value(&expected).map_err(|error| error.to_string())?
            != serde_json::to_value(&existing).map_err(|error| error.to_string())?
        {
            return Err("provider status update changed descriptor or credential facts".into());
        }
        let value = if name == "trusted" {
            row.trusted
        } else {
            row.deregistered
        };
        guarded_patch(
            &*self.store,
            item.id,
            item.logical_clock,
            BTreeMap::from([(name.into(), ItemValue::Bool(value))]),
        )
        .map_err(|error| format!("update provider {name}: {error}"))
    }
}

fn same_row(left: &PersistedProvider, right: &PersistedProvider) -> Result<bool, String> {
    Ok(
        serde_json::to_value(left).map_err(|error| error.to_string())?
            == serde_json::to_value(right).map_err(|error| error.to_string())?,
    )
}

fn guarded_patch(
    store: &dyn AttributedStore,
    id: ItemId,
    expected_clock: u64,
    payload: BTreeMap<String, ItemValue>,
) -> Result<(), String> {
    let (author, author_kind) = provider_actor();
    let spec = OperationSpec {
        target_id: id,
        op_type: OperationType::PatchPayload(payload),
        intent: OperationIntent::Routine,
        reason: None,
        batch_id: None,
        author,
        author_kind,
        retention: RetentionTier::Compactable,
    };
    match store
        .apply_operation_if_clock(spec, expected_clock)
        .map_err(|error| error.to_string())?
    {
        GuardedWrite::Applied { .. } => Ok(()),
        GuardedWrite::Moved { .. } => {
            Err("provider row changed during write; refresh before retrying".into())
        }
    }
}

fn provider_actor() -> (String, ActorKind) {
    context::current().map_or_else(
        || ("system:provider-registry".into(), ActorKind::System),
        |context| {
            let kind = match &context.caller {
                CallerIdentity::Person | CallerIdentity::App(_) => ActorKind::Human,
                CallerIdentity::Agent(_) | CallerIdentity::Provider(_) => ActorKind::Agent,
                CallerIdentity::System(_) => ActorKind::System,
            };
            (context.caller.author(), kind)
        },
    )
}

fn provider_item(id: ItemId, payload: BTreeMap<String, ItemValue>) -> Item {
    let now = chrono::Utc::now();
    let (author, author_kind) = provider_actor();
    Item {
        id,
        schema: PROVIDER_SCHEMA,
        payload,
        created: now,
        modified: now,
        author,
        author_kind,
        logical_clock: 0,
        origin: None,
        canonical_id: None,
        tags: vec![],
        flag: None,
        is_read: false,
        is_starred: false,
        priority: Priority::None,
        visibility: Visibility::Private,
        message_type: None,
        produced_by: None,
        version: None,
        batch_id: None,
        references: vec![],
        parent: None,
        extra: Default::default(),
    }
}

impl ProviderPersistence for StoreProviderPersistence {
    fn load(&self) -> Result<Vec<PersistedProvider>, String> {
        let query = ItemQuery {
            schema: Some(PROVIDER_SCHEMA),
            ..Default::default()
        };
        let rows: Vec<PersistedProvider> = self
            .store
            .query(&query)
            .map_err(|error| format!("list provider rows: {error}"))?
            .into_iter()
            .map(|item| {
                let row = decode_row(&item.payload)?;
                validate_row_identity(&row)?;
                if item.id != row_id(&row.id)? {
                    return Err(format!("provider {} has an inconsistent row id", row.id));
                }
                Ok(row)
            })
            .collect::<Result<_, _>>()?;
        tracing::debug!(
            "provider restore: {} registrations, {} descriptors",
            rows.len(),
            rows.iter().map(|row| row.verbs.len()).sum::<usize>()
        );
        Ok(rows)
    }

    fn save_registration(
        &self,
        row: &PersistedProvider,
        token: &str,
        previous: Option<&PersistedProvider>,
    ) -> Result<(), String> {
        tracing::info!(
            "provider registration requested: id={} descriptors={}",
            row.id,
            row.verbs.len()
        );
        // Read payload and clock from the same row snapshot. A different host
        // may have rotated its token or changed trust after this registry was
        // loaded, even if the submitted old token matched our stale memory.
        let existing = self.row_item(&row.id)?;
        match (&existing, previous) {
            (None, None) => {}
            (Some(item), Some(expected)) if same_row(&decode_row(&item.payload)?, expected)? => {}
            _ => return Err("provider registration changed; refresh before retrying".into()),
        }
        self.write_token(row, token)?;
        let payload = encode_row(row)?;
        match existing {
            Some(item) => guarded_patch(&*self.store, item.id, item.logical_clock, payload)
                .map_err(|error| format!("save provider registration: {error}"))?,
            None => match self.store.insert(provider_item(row_id(&row.id)?, payload)) {
                Ok(_) => {}
                Err(StoreError::AlreadyExists(_)) => {
                    return Err("provider registration changed; refresh before retrying".into());
                }
                Err(error) => return Err(format!("save provider registration: {error}")),
            },
        }
        tracing::info!("provider registration saved: id={}", row.id);
        Ok(())
    }

    fn save_trust(&self, row: &PersistedProvider) -> Result<(), String> {
        tracing::info!("provider trust change requested: id={}", row.id);
        self.update_flag(row, "trusted")?;
        tracing::info!("provider trust change saved: id={}", row.id);
        Ok(())
    }

    fn load_token(&self, provider_id: &str) -> Result<Option<String>, String> {
        self.row(provider_id)?
            .map(|row| self.read_token(&row.token_hash))
            .transpose()
            .map(Option::flatten)
    }

    fn save_status(&self, row: &PersistedProvider) -> Result<(), String> {
        tracing::info!("provider status change requested: id={}", row.id);
        self.update_flag(row, "deregistered")?;
        tracing::info!("provider status change saved: id={}", row.id);
        Ok(())
    }
}

/// Construct a registry without ever selecting or opening a process-default
/// store. The caller injects the JSON Schema definition validator until the
/// host's chosen validator boundary is installed.
pub fn build_registry(
    store: Arc<dyn AttributedStore>,
    workspace: &Path,
    validator: Arc<dyn SchemaValidator>,
) -> Result<Registry, RegistrationError> {
    let persistence = Arc::new(
        StoreProviderPersistence::new(store, workspace).map_err(RegistrationError::Persistence)?,
    );
    Registry::new()
        .with_validator(validator)
        .with_persistence(persistence)
}

/// Explicit startup hook for a host that already owns its exact store path.
/// Inventory enumeration and documentation generation never call this.
pub fn install_for_store(
    store: Arc<dyn AttributedStore>,
    workspace: &Path,
    validator: Arc<dyn SchemaValidator>,
) -> Result<Arc<Registry>, RegistrationError> {
    let registry = Arc::new(build_registry(store, workspace, validator)?);
    impress_service_core::registry_runtime::install(registry.clone());
    Ok(registry)
}

fn row_id(id: &str) -> Result<ItemId, String> {
    validate_id(id)?;
    Ok(uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        format!("provider:{id}").as_bytes(),
    ))
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || !id.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
    {
        return Err("provider id must be kebab-case".into());
    }
    Ok(())
}

fn validate_hash(hash: &str) -> Result<(), String> {
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("provider credential hash is invalid".into());
    }
    Ok(())
}

fn validate_row_identity(row: &PersistedProvider) -> Result<(), String> {
    validate_id(&row.id)?;
    validate_hash(&row.token_hash)
}

fn token_hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

fn encode_row(row: &PersistedProvider) -> Result<BTreeMap<String, ItemValue>, String> {
    validate_row_identity(row)?;
    Ok(BTreeMap::from([
        ("id".into(), ItemValue::String(row.id.clone())),
        ("language".into(), ItemValue::String(row.language.clone())),
        ("version".into(), ItemValue::String(row.version.clone())),
        ("endpoint".into(), ItemValue::String(row.endpoint.clone())),
        ("trusted".into(), ItemValue::Bool(row.trusted)),
        (
            "token_hash".into(),
            ItemValue::String(row.token_hash.clone()),
        ),
        ("deregistered".into(), ItemValue::Bool(row.deregistered)),
        (
            "verbs".into(),
            ItemValue::String(
                serde_json::to_string(&row.verbs)
                    .map_err(|error| format!("encode provider descriptors: {error}"))?,
            ),
        ),
    ]))
}

fn decode_row(payload: &BTreeMap<String, ItemValue>) -> Result<PersistedProvider, String> {
    fn string(payload: &BTreeMap<String, ItemValue>, key: &str) -> Result<String, String> {
        match payload.get(key) {
            Some(ItemValue::String(value)) => Ok(value.clone()),
            _ => Err(format!("provider row lacks string {key}")),
        }
    }
    fn boolean(payload: &BTreeMap<String, ItemValue>, key: &str) -> Result<bool, String> {
        match payload.get(key) {
            Some(ItemValue::Bool(value)) => Ok(*value),
            _ => Err(format!("provider row lacks bool {key}")),
        }
    }
    let verbs: Vec<PersistedVerb> = serde_json::from_str(&string(payload, "verbs")?)
        .map_err(|error| format!("decode provider descriptors: {error}"))?;
    Ok(PersistedProvider {
        id: string(payload, "id")?,
        language: string(payload, "language")?,
        version: string(payload, "version")?,
        endpoint: string(payload, "endpoint")?,
        trusted: boolean(payload, "trusted")?,
        token_hash: string(payload, "token_hash")?,
        deregistered: boolean(payload, "deregistered")?,
        verbs,
    })
}

fn private_directory(path: &Path) -> Result<PathBuf, String> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("create provider directory: {error}")),
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("inspect provider directory: {error}"))?;
    if !metadata.file_type().is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        return Err("provider credential directory is not private or is a symlink".into());
    }
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use impress_item::ItemStore;
    use impress_service_core::descriptor_handle::VerbHandle;
    use impress_service_core::provider::{
        ProviderExampleInput, ProviderIdentityInput, ProviderStatus, ProviderVerbInput,
        RegistrationRequest, SafetyClaim,
    };
    use koine_store_dir::DirStore;
    use koine_store_memory::MemoryStore;
    use serde_json::{json, Value};

    struct TestValidator;

    impl SchemaValidator for TestValidator {
        fn validate(&self, schema: &Value) -> Result<(), String> {
            if schema.is_object() {
                Ok(())
            } else {
                Err("schema must be an object".into())
            }
        }

        fn validate_instance(&self, schema: &Value, value: &Value) -> Result<(), String> {
            let args = value.as_object().ok_or("arguments must be an object")?;
            for required in schema["required"].as_array().into_iter().flatten() {
                let name = required.as_str().ok_or("invalid required field")?;
                if !args.get(name).is_some_and(Value::is_string) {
                    return Err(format!("missing or invalid argument {name}"));
                }
            }
            Ok(())
        }
    }

    fn request(token: Option<String>, version: &str) -> RegistrationRequest {
        RegistrationRequest {
            provider: ProviderIdentityInput {
                id: "python-reference".into(),
                language: "python".into(),
                version: version.into(),
                endpoint: "http://127.0.0.1:23456".into(),
                token,
            },
            verbs: vec![ProviderVerbInput {
                name: "python-reference-service_echo".into(),
                description: "Echo text".into(),
                input_schema: json!({
                    "type":"object",
                    "properties":{"text":{"type":"string","description":"Text to echo"}},
                    "required":["text"],
                    "additionalProperties":false
                }),
                output_schema: json!({
                    "type":"object",
                    "properties":{"echo":{"type":"string"}},
                    "required":["echo"],
                    "additionalProperties":false
                }),
                safety: SafetyClaim {
                    class: "read-only".into(),
                    idempotent: Some(true),
                },
                since: "1.0.0".into(),
                examples: vec![ProviderExampleInput {
                    name: "echo".into(),
                    args: json!({"text":"hello"}),
                    expect: Some(json!({"echo":"hello"})),
                }],
            }],
        }
    }

    #[test]
    fn registration_rotates_private_token_and_persists_trust() {
        let store = Arc::new(MemoryStore::new());
        let workspace = tempfile::tempdir().expect("scratch workspace");
        let validator: Arc<dyn SchemaValidator> = Arc::new(TestValidator);
        let registry = build_registry(store.clone(), workspace.path(), validator.clone())
            .expect("initial registry");

        let first = registry.register(request(None, "1.0.0")).expect("register");
        assert!(registry.authenticate("python-reference", &first.token));
        let persistence =
            StoreProviderPersistence::new(store.clone(), workspace.path()).expect("persistence");
        let row = persistence.row("python-reference").unwrap().unwrap();
        let path = persistence.credential_path(&row.token_hash).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&persistence.credential_dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            persistence.load_token("python-reference").unwrap(),
            Some(first.token.clone())
        );
        let item = store
            .get(row_id("python-reference").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(item.visibility, Visibility::Private);
        assert!(!item.payload.contains_key("token"));
        assert!(!serde_json::to_string(&item.payload)
            .unwrap()
            .contains(&first.token));

        registry
            .set_trusted("python-reference", true)
            .expect("trust");
        let second = registry
            .register(request(Some(first.token.clone()), "1.1.0"))
            .expect("rotate");
        assert_ne!(first.token, second.token);
        assert!(!registry.authenticate("python-reference", &first.token));
        assert!(registry.authenticate("python-reference", &second.token));
        let restored =
            build_registry(store.clone(), workspace.path(), validator.clone()).expect("restore");
        assert!(!restored.authenticate("python-reference", &first.token));
        assert!(restored.authenticate("python-reference", &second.token));
        assert!(
            persistence
                .row("python-reference")
                .unwrap()
                .unwrap()
                .trusted
        );
        // A restored descriptor exists for catalogues but starts unavailable
        // until the host explicitly probes its endpoint.
        match restored.find("python-reference-service_echo") {
            Some(VerbHandle::Provider(verb)) => {
                assert_eq!(verb.status, ProviderStatus::Unavailable)
            }
            _ => panic!("restored provider descriptor missing"),
        }

        registry
            .deregister("python-reference", &second.token)
            .expect("deregister");
        let restored =
            build_registry(store, workspace.path(), validator).expect("restore tombstone");
        assert!(restored.health_connections().is_empty());
        assert!(!restored.authenticate("python-reference", &second.token));
        assert!(
            persistence
                .row("python-reference")
                .unwrap()
                .unwrap()
                .deregistered
        );
    }

    #[test]
    fn two_hosts_cannot_claim_the_same_new_provider_id() {
        let workspace = tempfile::tempdir().expect("scratch workspace");
        let path = workspace.path().join("store");
        let first_store = Arc::new(DirStore::open(&path).unwrap());
        let second_store = Arc::new(DirStore::open(&path).unwrap());
        let validator: Arc<dyn SchemaValidator> = Arc::new(TestValidator);
        let first = build_registry(first_store, workspace.path(), validator.clone()).unwrap();
        let second = build_registry(second_store.clone(), workspace.path(), validator).unwrap();

        let receipt = first.register(request(None, "1.0.0")).unwrap();
        let error = second.register(request(None, "1.0.0")).err().unwrap();
        assert!(
            matches!(error, RegistrationError::Persistence(_)),
            "{error}"
        );
        let persisted = StoreProviderPersistence::new(second_store, workspace.path()).unwrap();
        assert_eq!(
            persisted.load_token("python-reference").unwrap(),
            Some(receipt.token)
        );
        assert!(!persisted.row("python-reference").unwrap().unwrap().trusted);
    }

    #[test]
    fn stale_hosts_cannot_erase_trust_or_rotated_credentials() {
        let workspace = tempfile::tempdir().expect("scratch workspace");
        let path = workspace.path().join("store");
        let first_store = Arc::new(DirStore::open(&path).unwrap());
        let second_store = Arc::new(DirStore::open(&path).unwrap());
        let validator: Arc<dyn SchemaValidator> = Arc::new(TestValidator);
        let first =
            build_registry(first_store.clone(), workspace.path(), validator.clone()).unwrap();
        let original = first.register(request(None, "1.0.0")).unwrap();
        let second = build_registry(second_store.clone(), workspace.path(), validator).unwrap();

        first.set_trusted("python-reference", true).unwrap();
        let id = row_id("python-reference").unwrap();
        let trusted_clock = first_store.logical_clock_of(id).unwrap();
        let stale = second.register(request(Some(original.token.clone()), "1.1.0"));
        assert!(matches!(stale, Err(RegistrationError::Persistence(_))));
        assert_eq!(first_store.logical_clock_of(id).unwrap(), trusted_clock);
        let persisted =
            StoreProviderPersistence::new(second_store.clone(), workspace.path()).unwrap();
        assert!(persisted.row("python-reference").unwrap().unwrap().trusted);
        assert_eq!(
            persisted.load_token("python-reference").unwrap(),
            Some(original.token.clone())
        );

        second.refresh_persisted().unwrap();
        let rotated = second
            .register(request(Some(original.token.clone()), "1.1.0"))
            .unwrap();
        assert_ne!(original.token, rotated.token);
        let stale = first.register(request(Some(original.token.clone()), "1.2.0"));
        assert!(matches!(stale, Err(RegistrationError::Persistence(_))));
        let stale_trust = first.set_trusted("python-reference", false);
        assert!(matches!(
            stale_trust,
            Err(RegistrationError::Persistence(_))
        ));
        let restored =
            build_registry(first_store, workspace.path(), Arc::new(TestValidator)).unwrap();
        assert!(!restored.authenticate("python-reference", &original.token));
        assert!(restored.authenticate("python-reference", &rotated.token));
        let row = persisted.row("python-reference").unwrap().unwrap();
        assert!(row.trusted);
        assert_eq!(row.version, "1.1.0");
    }

    #[tokio::test]
    async fn trust_operation_uses_the_person_from_the_call_context() {
        let store = Arc::new(MemoryStore::new());
        let workspace = tempfile::tempdir().unwrap();
        let registry =
            build_registry(store.clone(), workspace.path(), Arc::new(TestValidator)).unwrap();
        registry.register(request(None, "1.0.0")).unwrap();
        let context = Arc::new(context::CallContext {
            call_id: "person-review-fixture".into(),
            trace_id: "person-review-trace".into(),
            parent_call: None,
            caller: CallerIdentity::Person,
            verb: "provider-service_set-trusted".into(),
            store_override: None,
            mutation_ids: context::MutationIds::default(),
            adverbs: Default::default(),
            inverse: Default::default(),
            notes: Default::default(),
        });
        context::scope(context, async {
            registry.set_trusted("python-reference", true).unwrap();
        })
        .await;
        let operations = store
            .query(&ItemQuery {
                schema: Some(impress_item::schema::refs::CORE_OPERATION),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].author, "human");
        assert_eq!(operations[0].author_kind, ActorKind::Human);
        assert_eq!(
            operations[0].batch_id.as_deref(),
            Some("person-review-fixture")
        );
    }

    #[test]
    fn unsafe_credential_directory_and_symlink_are_rejected() {
        let store = Arc::new(MemoryStore::new());
        let workspace = tempfile::tempdir().expect("scratch workspace");
        let target = tempfile::tempdir().expect("other scratch directory");
        std::os::unix::fs::symlink(target.path(), workspace.path().join("providers"))
            .expect("symlink fixture");
        assert!(StoreProviderPersistence::new(store.clone(), workspace.path()).is_err());
        fs::remove_file(workspace.path().join("providers")).unwrap();

        let registry = build_registry(store, workspace.path(), Arc::new(TestValidator)).unwrap();
        let receipt = registry.register(request(None, "1.0.0")).unwrap();
        let persistence =
            StoreProviderPersistence::new(Arc::new(MemoryStore::new()), workspace.path()).unwrap();
        let hash = token_hash(&receipt.token);
        let path = persistence.credential_path(&hash).unwrap();
        fs::remove_file(&path).unwrap();
        let secret = workspace.path().join("other-private-file");
        fs::write(&secret, &receipt.token).unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&secret, &path).unwrap();
        assert!(persistence.read_token(&hash).is_err());
        assert!(persistence.credential_path("../escape").is_err());
    }
}
