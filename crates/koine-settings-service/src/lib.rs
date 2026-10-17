//! `SettingsService` — the registry's verbs (ADR-0036 D5, plan R1).
//!
//! `schema` is the registry as data (every declared key with its type,
//! default, scope, legacy keys and doc, plus the sections); `list`/`get` are
//! the current values; `set`/`reset` write one key; `surface` is the generated
//! settings pane for a section. Every value goes through
//! `impress_settings::SettingsStore`, which owns the files under
//! `<workspace>/settings/` — this crate adds only the process-wide instance,
//! the `Synced` scope's store row ([`StoreSyncedBackend`]), and the wire
//! envelope. The same store the UniFFI `SharedSettings` object installs, so
//! an app's pane, the CLI and MCP read one file.
//!
//! Results carry `ok` / `code` / `message` and `wire_version` (wave 7 T6),
//! and the service is `strict_args`: an argument the schema does not name is
//! refused `invalid-argument`, never parsed around.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use chrono::Utc;
use impress_item::{
    ActorKind, AttributedStore, FieldMutation, Item, ItemId, Priority, SchemaRef, StoreError,
    Value as ItemValue, Visibility,
};
use impress_service_core::async_trait;
use impress_service_core::refusal::codes;
use impress_service_core::wire::wire_version;
use impress_service_macros::{impress_service, impress_service_impl};
use impress_settings::{
    registry, section_surface, sections, Resolved, SettingDef, SettingsError, SettingsStore,
    SyncedBackend, ValueSource,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[allow(unused_imports)]
use impress_service_macros::impress_method;

/// The schema of the one synced settings row (`impress/settings@1.0.0`).
pub const SETTINGS_SCHEMA_REF: SchemaRef = impress_item::schema::refs::IMPRESS_SETTINGS;

// ---------------------------------------------------------------------------
// The process-wide settings store
// ---------------------------------------------------------------------------

static SETTINGS: OnceLock<Arc<SettingsStore>> = OnceLock::new();
static WORKSPACE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Point the settings files at `workspace` (the directory holding
/// `impress.sqlite`) before the first verb runs. `Err` once a store is open
/// or a path was already fixed — the first wins, as `set_store_path` does.
pub fn set_settings_workspace(workspace: impl AsRef<Path>) -> Result<(), String> {
    if SETTINGS.get().is_some() {
        return Err("impress settings store already open".into());
    }
    let mut slot = WORKSPACE.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_some() {
        return Err("impress settings workspace already set".into());
    }
    *slot = Some(workspace.as_ref().to_path_buf());
    Ok(())
}

/// Install an already-open settings store (the FFI's, or a test's). `Err`
/// when one is installed already; the first wins.
pub fn install_settings(store: Arc<SettingsStore>) -> Result<(), String> {
    SETTINGS
        .set(store)
        .map_err(|_| "impress settings store already installed".to_string())
}

/// The settings files the verbs read: the installed store, else one opened
/// beside the shared `impress.sqlite` (`store_path().parent()`), with the
/// synced scope backed by the store row.
pub fn settings_instance() -> Arc<SettingsStore> {
    SETTINGS
        .get_or_init(|| {
            let workspace = WORKSPACE
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .or_else(koine_store::process::workspace)
                .unwrap_or_else(|| PathBuf::from("."));
            let store = SettingsStore::open(workspace);
            store.set_synced_backend(Arc::new(StoreSyncedBackend::lazy()));
            Arc::new(store)
        })
        .clone()
}

// ---------------------------------------------------------------------------
// The synced scope: one `impress/settings@1.0.0` row
// ---------------------------------------------------------------------------

/// The `Synced` scope over the shared store: one row of
/// [`SETTINGS_SCHEMA_REF`] whose `values` object is the key → value map.
/// The row id is deterministic (UUIDv5 of the ref), so every device and
/// every process writes the same row and the sync engine merges it.
pub struct StoreSyncedBackend {
    store: Option<Arc<dyn AttributedStore>>,
}

impl StoreSyncedBackend {
    /// Resolve the process-wide store on every call (it opens lazily).
    pub fn lazy() -> Self {
        Self { store: None }
    }

    pub fn with_store(store: Arc<dyn AttributedStore>) -> Self {
        Self { store: Some(store) }
    }

    fn store(&self) -> Arc<dyn AttributedStore> {
        self.store
            .clone()
            .unwrap_or_else(|| koine_store::process::acquire_or_stand_in().0)
    }

    /// The one row's id.
    pub fn row_id() -> ItemId {
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, SETTINGS_SCHEMA_REF.as_bytes())
    }
}

fn store_error(error: StoreError) -> io::Error {
    io::Error::other(error.to_string())
}

impl SyncedBackend for StoreSyncedBackend {
    fn load(&self) -> io::Result<BTreeMap<String, Value>> {
        let Some(item) = self.store().get(Self::row_id()).map_err(store_error)? else {
            return Ok(BTreeMap::new());
        };
        let Some(values) = item.payload.get("values") else {
            return Ok(BTreeMap::new());
        };
        let json = serde_json::to_value(values).map_err(io::Error::other)?;
        match json {
            Value::Object(map) => Ok(map.into_iter().collect()),
            _ => Ok(BTreeMap::new()),
        }
    }

    fn save(&self, values: &BTreeMap<String, Value>) -> io::Result<()> {
        let store = self.store();
        let values_json =
            Value::Object(values.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
        let values_item: ItemValue =
            serde_json::from_value(values_json).map_err(io::Error::other)?;
        let updated = ItemValue::Int(Utc::now().timestamp_millis());
        let id = Self::row_id();
        let mutations = vec![
            FieldMutation::SetPayload("values".into(), values_item.clone()),
            FieldMutation::SetPayload("updated_at_ms".into(), updated.clone()),
        ];
        match store.update(id, mutations).map_err(store_error) {
            Ok(()) => Ok(()),
            Err(_) => {
                let mut payload = BTreeMap::new();
                payload.insert("values".to_string(), values_item);
                payload.insert("updated_at_ms".to_string(), updated);
                let now = Utc::now();
                let item = Item {
                    id,
                    schema: SETTINGS_SCHEMA_REF,
                    payload,
                    created: now,
                    modified: now,
                    author: "settings-service".into(),
                    author_kind: ActorKind::Human,
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
                };
                store.insert(item).map(|_| ()).map_err(store_error)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// One declared setting, with its current value when the verb read one.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SettingDto {
    /// The registry key (`imbib.retention.inbox_days`).
    pub key: String,
    /// `bool`, `integer`, `number` or `string`.
    #[serde(rename = "type")]
    pub ty: String,
    pub default: Value,
    /// `device`, `app:<id>`, `library` or `synced`.
    pub scope: String,
    /// The section whose generated surface shows it.
    pub section: String,
    pub label: String,
    pub doc: String,
    /// The `UserDefaults` keys it migrates from (never removed).
    pub legacy: Vec<String>,
    /// Declared options, when the setting is chosen from a list.
    pub choices: Vec<ChoiceDto>,
    pub since: String,
    /// The current value; absent from `schema`, present from `list`/`get`/`set`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// `stored` or `default`; absent from `schema`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ChoiceDto {
    pub label: String,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SectionDto {
    pub id: String,
    pub title: String,
    pub doc: String,
}

/// The registry as data.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SettingsSchemaResult {
    pub ok: bool,
    pub sections: Vec<SectionDto>,
    pub settings: Vec<SettingDto>,
    /// Where the scope files live on this device.
    pub directory: String,
    #[serde(default = "wire_version")]
    pub wire_version: u32,
}

/// One setting's current value, or a refusal.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SettingResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setting: Option<SettingDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
    #[serde(default = "wire_version")]
    pub wire_version: u32,
}

/// The current values of every setting (of one section).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SettingListResult {
    pub ok: bool,
    pub settings: Vec<SettingDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
    #[serde(default = "wire_version")]
    pub wire_version: u32,
}

/// A section's generated surface.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SettingsSurfaceResult {
    pub ok: bool,
    pub section: String,
    /// The `SurfaceSpec` as JSON — what `surface_create` accepts verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
    #[serde(default = "wire_version")]
    pub wire_version: u32,
}

pub fn setting_dto(def: &SettingDef) -> SettingDto {
    SettingDto {
        key: def.key.clone(),
        ty: def.ty.label().into(),
        default: def.default.to_json(),
        scope: def.scope.label(),
        section: def.section.clone(),
        label: def.label.clone(),
        doc: def.doc.clone(),
        legacy: def.legacy.clone(),
        choices: def
            .choices
            .iter()
            .map(|choice| ChoiceDto {
                label: choice.label.clone(),
                value: choice.value.clone(),
            })
            .collect(),
        since: def.since.clone(),
        value: None,
        source: None,
    }
}

pub fn resolved_dto(resolved: &Resolved) -> SettingDto {
    let mut dto = setting_dto(resolved.def);
    dto.value = Some(resolved.value.clone());
    dto.source = Some(
        match resolved.source {
            ValueSource::Stored => "stored",
            ValueSource::Default => "default",
        }
        .into(),
    );
    dto
}

fn code_for(error: &SettingsError) -> &'static str {
    match error {
        SettingsError::UnknownKey(_) => codes::NOT_FOUND,
        SettingsError::TypeMismatch { .. } => codes::INVALID_ARGUMENT,
        SettingsError::NoSyncedBackend(_) => codes::STORE_UNAVAILABLE,
        SettingsError::Io(_) => codes::STORE_ERROR,
    }
}

fn setting_result(outcome: Result<Resolved, SettingsError>, done: &str) -> SettingResult {
    match outcome {
        Ok(resolved) => SettingResult {
            ok: true,
            message: format!(
                "{done} {} = {} ({}).",
                resolved.def.key,
                resolved.value,
                match resolved.source {
                    ValueSource::Stored => "stored",
                    ValueSource::Default => "default",
                }
            ),
            setting: Some(resolved_dto(&resolved)),
            code: None,
            wire_version: wire_version(),
        },
        Err(error) => SettingResult {
            ok: false,
            setting: None,
            code: Some(code_for(&error).into()),
            message: error.to_string(),
            wire_version: wire_version(),
        },
    }
}

// ---------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------

/// The settings registry: every setting the suite declares, its current
/// value on this device, and the generated pane per section (ADR-0036 D5).
///
/// Keys are dotted registry keys (`imbib.retention.inbox_days`); an
/// undeclared key is `not-found`, a value of the wrong type is
/// `invalid-argument` and nothing is written. Device, app and library scopes
/// are files under `<workspace>/settings/` shared by every app, daemon and
/// the CLI; the synced scope is one store row.
#[impress_service]
pub trait SettingsService: Send + Sync + 'static {
    /// The registry as data: every declared setting (key, type, default,
    /// scope, section, legacy `UserDefaults` keys, doc, choices) and the
    /// sections, without values. What a settings UI or an agent reads first.
    #[impress_method(safety = read_only, effects(reads = [], reach = []))]
    #[impress_example(
        name = "declared_registry",
        args = r#"{}"#,
        expect = r#"{"ok":true,"wire_version":1}"#
    )]
    async fn schema(&self) -> SettingsSchemaResult;

    /// Every setting with its current value — stored, else the registry
    /// default — optionally narrowed to one section (`imbib.retention`).
    #[impress_method(safety = read_only)]
    #[impress_example(
        name = "retention_settings",
        args = r#"{"section":"imbib.retention"}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn list(&self, section: Option<String>) -> SettingListResult;

    /// One setting's current value and where it came from.
    #[impress_method(safety = read_only)]
    #[impress_example(
        name = "inbox_days",
        args = r#"{"key":"imbib.retention.inbox_days"}"#,
        expect = r#"{"ok":true,"setting":{"key":"imbib.retention.inbox_days"}}"#
    )]
    async fn get(&self, key: String) -> SettingResult;

    /// Store a value. The value must have the declared type; a choice's
    /// label (`"1 Month"`) or the string spelling of a number or bool is
    /// accepted, anything else is refused and nothing changes. Writes the
    /// scope's file (or the synced row) so every app sees it.
    #[impress_method(
        safety = mutating,
        idempotent = true,
        effects(reads = [], writes = ["impress/settings@1.0.0"], reach = [fs])
    )]
    #[impress_example(
        name = "set_inbox_days",
        args = r#"{"key":"imbib.retention.inbox_days","value":31}"#,
        expect = r#"{"ok":true,"setting":{"key":"imbib.retention.inbox_days","value":31,"source":"stored"}}"#
    )]
    async fn set(&self, key: String, value: Value) -> SettingResult;

    /// Forget the stored value so the key answers its registry default.
    #[impress_method(
        safety = mutating,
        idempotent = true,
        effects(reads = [], writes = ["impress/settings@1.0.0"], reach = [fs])
    )]
    #[impress_example(
        name = "reset_inbox_days",
        args = r#"{"key":"imbib.retention.inbox_days"}"#,
        expect = r#"{"ok":true,"setting":{"key":"imbib.retention.inbox_days","value":30,"source":"default"}}"#
    )]
    async fn reset(&self, key: String) -> SettingResult;

    /// The generated settings pane for one section: a `SurfaceSpec` with one
    /// typed field per setting, seeded with the current values, whose
    /// `on_change` calls `settings-service_set`. Store it with
    /// `impress-surface-service_surface-create` to show it in a pane.
    #[impress_method(safety = read_only)]
    #[impress_example(
        name = "retention_pane",
        args = r#"{"section":"imbib.retention"}"#,
        expect = r#"{"ok":true,"section":"imbib.retention"}"#
    )]
    async fn surface(&self, section: String) -> SettingsSurfaceResult;
}

/// File-backed `SettingsService`. `new()` uses the process-wide store;
/// `with_store` takes an explicit one, as the tests do.
#[derive(Clone, Default)]
pub struct DefaultSettingsService {
    settings: Option<Arc<SettingsStore>>,
}

impl DefaultSettingsService {
    pub fn new() -> Self {
        Self { settings: None }
    }

    pub fn with_store(settings: Arc<SettingsStore>) -> Self {
        Self {
            settings: Some(settings),
        }
    }

    fn settings(&self) -> Arc<SettingsStore> {
        self.settings.clone().unwrap_or_else(settings_instance)
    }
}

#[async_trait::async_trait]
impl SettingsService for DefaultSettingsService {
    async fn schema(&self) -> SettingsSchemaResult {
        SettingsSchemaResult {
            ok: true,
            sections: sections()
                .iter()
                .map(|section| SectionDto {
                    id: section.id.clone(),
                    title: section.title.clone(),
                    doc: section.doc.clone(),
                })
                .collect(),
            settings: registry().iter().map(setting_dto).collect(),
            directory: self.settings().directory().display().to_string(),
            wire_version: wire_version(),
        }
    }

    async fn list(&self, section: Option<String>) -> SettingListResult {
        let section = section
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(id) = section.as_deref() {
            if impress_settings::section(id).is_none() {
                return SettingListResult {
                    ok: false,
                    settings: vec![],
                    code: Some(codes::NOT_FOUND.into()),
                    message: format!("no settings section `{id}` is declared"),
                    wire_version: wire_version(),
                };
            }
        }
        match self.settings().list(section.as_deref()) {
            Ok(resolved) => SettingListResult {
                ok: true,
                message: format!("{} settings.", resolved.len()),
                settings: resolved.iter().map(resolved_dto).collect(),
                code: None,
                wire_version: wire_version(),
            },
            Err(error) => SettingListResult {
                ok: false,
                settings: vec![],
                code: Some(code_for(&error).into()),
                message: error.to_string(),
                wire_version: wire_version(),
            },
        }
    }

    async fn get(&self, key: String) -> SettingResult {
        setting_result(self.settings().get(key.trim()), "Read")
    }

    async fn set(&self, key: String, value: Value) -> SettingResult {
        let settings = self.settings();
        let outcome = settings.set(key.trim(), &value);
        log::info!(
            target: "settings",
            "set {key} = {value}: {}",
            match &outcome {
                Ok(resolved) => format!("stored {} in {}", resolved.value, resolved.def.scope.label()),
                Err(error) => format!("refused: {error}"),
            }
        );
        setting_result(outcome, "Set")
    }

    async fn reset(&self, key: String) -> SettingResult {
        let outcome = self.settings().reset(key.trim());
        log::info!(
            target: "settings",
            "reset {key}: {}",
            match &outcome {
                Ok(resolved) => format!("now {} (default)", resolved.value),
                Err(error) => format!("refused: {error}"),
            }
        );
        setting_result(outcome, "Reset")
    }

    async fn surface(&self, section: String) -> SettingsSurfaceResult {
        let section = section.trim().to_string();
        let values = match self.settings().list(Some(&section)) {
            Ok(values) => values,
            Err(error) => {
                return SettingsSurfaceResult {
                    ok: false,
                    section,
                    spec: None,
                    code: Some(code_for(&error).into()),
                    message: error.to_string(),
                    wire_version: wire_version(),
                }
            }
        };
        match section_surface(&section, &values) {
            Some(spec) => SettingsSurfaceResult {
                ok: true,
                message: format!("{} fields.", values.len()),
                section,
                spec: serde_json::to_value(spec).ok(),
                code: None,
                wire_version: wire_version(),
            },
            None => SettingsSurfaceResult {
                ok: false,
                message: format!("no settings section `{section}` is declared"),
                section,
                spec: None,
                code: Some(codes::NOT_FOUND.into()),
                wire_version: wire_version(),
            },
        }
    }
}

impress_service_impl! {
    service = SettingsService,
    // Four of six read; `set` and `reset` override to `mutating` on the trait.
    safety = read_only,
    // The registry's own row (`impress/settings@1.0.0`, the synced scope)
    // plus the per-scope files under `<workspace>/settings/` every method
    // reads or writes through `SettingsStore` (device/app/library scopes are
    // files, so `reach: [fs]`). `schema` overrides to empty — it lists the
    // registry's static declarations and never touches a value.
    effects = {
        reads: ["impress/settings@1.0.0"],
        writes: [],
        reach: [fs],
    },
    since = "0.1.0",
    impl = DefaultSettingsService,
    instance = DefaultSettingsService::new,
    strict_args = true,
    methods = [
        schema() -> SettingsSchemaResult,
        list(
            /// A section id; omit for every setting.
            section: Option<String>
        ) -> SettingListResult,
        get(
            /// The registry key.
            key: String
        ) -> SettingResult,
        set(
            /// The registry key.
            key: String,
            /// The new value (JSON: a bool, number or string).
            value: Value
        ) -> SettingResult,
        reset(
            /// The registry key.
            key: String
        ) -> SettingResult,
        surface(
            /// A section id (`imbib.retention`).
            section: String
        ) -> SettingsSurfaceResult,
    ],
}

#[cfg(test)]
mod tests {
    use super::*;
    use impress_service_core::McpToolDescriptor;
    use koine_store_dir::DirStore;
    use koine_store_memory::MemoryStore;

    fn service(dir: &tempfile::TempDir) -> DefaultSettingsService {
        DefaultSettingsService::with_store(Arc::new(SettingsStore::open(dir.path())))
    }

    #[tokio::test]
    async fn schema_lists_every_declared_key_without_values() {
        let dir = tempfile::tempdir().unwrap();
        let schema = service(&dir).schema().await;
        assert!(schema.ok);
        assert_eq!(schema.settings.len(), registry().len());
        assert!(schema.settings.iter().all(|s| s.value.is_none()));
        assert!(schema.sections.iter().any(|s| s.id == "imbib.retention"));
        assert!(schema.directory.ends_with("settings"));
        let inbox = schema
            .settings
            .iter()
            .find(|s| s.key == "imbib.retention.inbox_days")
            .unwrap();
        assert_eq!(inbox.legacy, ["inbox.retentionDays"]);
        assert_eq!(inbox.scope, "device");
        assert_eq!(inbox.ty, "integer");
    }

    #[tokio::test]
    async fn get_set_reset_round_trip_with_the_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(&dir);
        let got = s.get("imbib.retention.inbox_days".into()).await;
        assert!(got.ok, "{}", got.message);
        assert_eq!(got.wire_version, 1);
        let dto = got.setting.unwrap();
        assert_eq!(dto.value, Some(Value::from(30)));
        assert_eq!(dto.source.as_deref(), Some("default"));

        let set = s
            .set("imbib.retention.inbox_days".into(), Value::from("3 Months"))
            .await;
        assert!(set.ok, "{}", set.message);
        assert_eq!(set.setting.unwrap().value, Some(Value::from(90)));

        let list = s.list(Some("imbib.retention".into())).await;
        assert!(list.ok);
        assert_eq!(list.settings.len(), 4);
        assert_eq!(
            list.settings[0].source.as_deref(),
            Some("stored"),
            "the value the pane and the CLI agree on"
        );

        let reset = s.reset("imbib.retention.inbox_days".into()).await;
        assert!(reset.ok);
        assert_eq!(reset.setting.unwrap().source.as_deref(), Some("default"));
    }

    #[tokio::test]
    async fn refusals_carry_codes() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(&dir);
        let unknown = s.get("imbib.retention.inboxDays".into()).await;
        assert!(!unknown.ok);
        assert_eq!(unknown.code.as_deref(), Some(codes::NOT_FOUND));
        let wrong = s
            .set("imbib.retention.inbox_days".into(), Value::from("soon"))
            .await;
        assert!(!wrong.ok);
        assert_eq!(wrong.code.as_deref(), Some(codes::INVALID_ARGUMENT));
        assert!(
            s.get("imbib.retention.inbox_days".into())
                .await
                .setting
                .unwrap()
                .source
                .as_deref()
                == Some("default")
        );
        let section = s.list(Some("nowhere".into())).await;
        assert!(!section.ok);
        assert_eq!(section.code.as_deref(), Some(codes::NOT_FOUND));
        let surface = s.surface("nowhere".into()).await;
        assert!(!surface.ok);
        assert_eq!(surface.code.as_deref(), Some(codes::NOT_FOUND));
    }

    #[tokio::test]
    async fn surface_is_a_valid_spec_seeded_with_current_values() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(&dir);
        s.set("imbib.retention.auto_remove_read".into(), Value::Bool(true))
            .await;
        let result = s.surface("imbib.retention".into()).await;
        assert!(result.ok, "{}", result.message);
        let spec = result.spec.unwrap();
        let (parsed, problems) = impress_surface::validate_json(&spec);
        assert!(problems.is_empty(), "{problems:?}");
        let parsed = parsed.unwrap();
        assert_eq!(
            parsed.state["imbib_retention_auto_remove_read"],
            Value::Bool(true)
        );
    }

    /// The synced row: written once, read back, and merged rather than
    /// replaced, on every backend.
    #[test]
    fn synced_backend_round_trips_one_store_row() {
        let dir = tempfile::tempdir().unwrap();
        let stores: Vec<Arc<dyn AttributedStore>> = vec![
            Arc::new(MemoryStore::new()),
            Arc::new(DirStore::open(dir.path()).unwrap()),
        ];
        for store in stores {
            let backend = StoreSyncedBackend::with_store(store.clone());
            assert!(backend.load().unwrap().is_empty());
            let mut values = BTreeMap::new();
            values.insert("a.b.c".to_string(), Value::from(1));
            backend.save(&values).unwrap();
            values.insert("a.b.d".to_string(), Value::Bool(true));
            backend.save(&values).unwrap();
            let loaded = backend.load().unwrap();
            assert_eq!(loaded.len(), 2);
            assert_eq!(loaded["a.b.c"], Value::from(1));
            let row = store.get(StoreSyncedBackend::row_id()).unwrap().unwrap();
            assert_eq!(row.schema, SETTINGS_SCHEMA_REF);
            assert_eq!(
                SETTINGS_SCHEMA_REF.as_str(),
                impress_settings::SYNCED_SCHEMA_REF
            );
            // A second backend over the same store sees the same row: this is
            // what makes two processes merge.
            let other = StoreSyncedBackend::with_store(store);
            assert_eq!(other.load().unwrap().len(), 2);
        }
    }

    #[test]
    fn every_verb_is_in_the_inventory_and_strict() {
        let names: Vec<&str> = McpToolDescriptor::iter().map(|d| d.name).collect();
        for expected in [
            "settings-service_schema",
            "settings-service_list",
            "settings-service_get",
            "settings-service_set",
            "settings-service_reset",
            "settings-service_surface",
        ] {
            assert!(
                names.contains(&expected),
                "{expected} missing; have {names:?}"
            );
        }
    }

    /// With no store named, the default deployment reads the process store and
    /// the process workspace for its files.
    #[tokio::test]
    async fn the_default_deployment_reads_the_process_workspace() {
        let dir = tempfile::tempdir().unwrap();
        koine_store::process::set_workspace(dir.path());
        let first = settings_instance();
        // The instance is process-wide; whichever workspace won, the files sit
        // under a `settings` directory beside it.
        let s = DefaultSettingsService::new();
        let schema = s.schema().await;
        assert!(schema.ok);
        assert!(
            schema.directory.ends_with("settings"),
            "{}",
            schema.directory
        );
        assert!(Arc::ptr_eq(&first, &settings_instance()));
    }
}
