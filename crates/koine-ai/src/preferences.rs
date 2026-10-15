//! The device-local AI preferences file: `<workspace>/ai/preferences.json`.
//!
//! This is the one place the suite records which provider and model a
//! device uses, its non-secret endpoint overrides, whether oMLX may be
//! started on demand, and the per-task-category model assignments. Every
//! app and daemon on the device reads and writes the same file through
//! [`PreferencesStore`]; nothing in it is synced, because which laptop runs
//! oMLX is a fact about the device, not the researcher.
//!
//! Secrets never enter this file. API keys live in the platform keychain and
//! reach Rust in memory only (see [`crate::credentials`]).

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::catalogue::{self, OMLX_ID, OPENAI_COMPATIBLE_ID};
use crate::fs_lock::FileLock;

pub const PREFERENCES_VERSION: u32 = 1;
pub const PREFERENCES_DIRECTORY: &str = "ai";
pub const PREFERENCES_FILE: &str = "preferences.json";
const LOCK_FILE: &str = "preferences.lock";

/// Helper pseudo-models that must never survive as a selection.
const HELPER_MODEL_IDS: &[&str] = &["markitdown"];

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelRef {
    pub provider: String,
    /// `None` pins the provider only; the model then resolves through the
    /// registry's fallback chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

impl ModelRef {
    pub fn new(provider: impl Into<String>, model: Option<String>) -> Self {
        Self {
            provider: provider.into(),
            model,
            display_name: None,
        }
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CategoryAssignment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<ModelRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comparison: Vec<ModelRef>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for CategoryAssignment {
    fn default() -> Self {
        Self {
            primary: None,
            comparison: vec![],
            enabled: true,
        }
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiPreferences {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<ModelRef>,
    /// Provider id → endpoint override. Absent means the catalogue default.
    #[serde(default)]
    pub endpoints: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub auto_start_omlx: bool,
    #[serde(default)]
    pub task_categories: BTreeMap<String, CategoryAssignment>,
    /// The models the researcher made available to the suite. Task-category
    /// pickers offer these and nothing else, so choosing a model for a job is
    /// a choice among the handful that are actually wanted rather than every
    /// model every catalogued provider lists.
    ///
    /// Empty means "no restriction": a device that never curated a list must
    /// keep seeing every model, and an upgrade must not silently empty every
    /// picker.
    #[serde(default)]
    pub enabled_models: Vec<ModelRef>,
    #[serde(default)]
    pub updated_at_ms: i64,
    /// Set once the Swift-side `SharedDefaults` selection has been imported,
    /// so the import never runs twice.
    #[serde(default)]
    pub migrated_from_shared_defaults: bool,
}

impl Default for AiPreferences {
    fn default() -> Self {
        Self {
            version: PREFERENCES_VERSION,
            selected: None,
            endpoints: BTreeMap::new(),
            auto_start_omlx: true,
            task_categories: BTreeMap::new(),
            enabled_models: Vec::new(),
            updated_at_ms: 0,
            migrated_from_shared_defaults: false,
        }
    }
}

fn default_true() -> bool {
    true
}

pub fn preferences_directory(workspace: impl AsRef<Path>) -> PathBuf {
    workspace.as_ref().join(PREFERENCES_DIRECTORY)
}

pub fn preferences_path(workspace: impl AsRef<Path>) -> PathBuf {
    preferences_directory(workspace).join(PREFERENCES_FILE)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    modified: Option<SystemTime>,
    len: u64,
}

impl Fingerprint {
    fn of(path: &Path) -> io::Result<Option<Self>> {
        match fs::metadata(path) {
            Ok(metadata) => Ok(Some(Self {
                modified: metadata.modified().ok(),
                len: metadata.len(),
            })),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

/// Reads and writes the preferences file with a fingerprint-guarded cache
/// (re-parse only when mtime or length changed), atomic writes (temp file +
/// `fsync` + rename) and a flock around read-modify-write so six apps and two
/// daemons never lose each other's updates.
pub struct PreferencesStore {
    path: PathBuf,
    lock_path: PathBuf,
    cache: Mutex<Option<(AiPreferences, Option<Fingerprint>)>>,
}

impl PreferencesStore {
    pub fn open(workspace: impl AsRef<Path>) -> Self {
        let directory = preferences_directory(workspace);
        Self {
            path: directory.join(PREFERENCES_FILE),
            lock_path: directory.join(LOCK_FILE),
            cache: Mutex::new(None),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Current preferences; the defaults when the file does not exist. A
    /// corrupt or newer-versioned file is an error, never a silent reset.
    pub fn load(&self) -> io::Result<AiPreferences> {
        let fingerprint = Fingerprint::of(&self.path)?;
        if let Some((cached, cached_fingerprint)) = self.cache.lock().unwrap().as_ref() {
            if *cached_fingerprint == fingerprint {
                return Ok(cached.clone());
            }
        }
        let preferences = self.read_fresh()?;
        *self.cache.lock().unwrap() = Some((preferences.clone(), fingerprint));
        Ok(preferences)
    }

    fn read_fresh(&self) -> io::Result<AiPreferences> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(AiPreferences::default())
            }
            Err(error) => return Err(error),
        };
        let preferences: AiPreferences = serde_json::from_slice(&bytes).map_err(|error| {
            io::Error::new(
                ErrorKind::InvalidData,
                format!("decode {}: {error}", self.path.display()),
            )
        })?;
        if preferences.version > PREFERENCES_VERSION {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                format!(
                    "{} is version {}, newer than this build understands ({PREFERENCES_VERSION})",
                    self.path.display(),
                    preferences.version
                ),
            ));
        }
        Ok(preferences)
    }

    /// Write atomically and return the stored value (with `updated_at_ms`
    /// and `version` stamped).
    pub fn save(&self, mut preferences: AiPreferences) -> io::Result<AiPreferences> {
        preferences.version = PREFERENCES_VERSION;
        preferences.updated_at_ms = now_ms();
        let directory = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(directory)?;
        let temporary = directory.join(format!(".{PREFERENCES_FILE}.{}.tmp", std::process::id()));
        let bytes = serde_json::to_vec_pretty(&preferences).map_err(io::Error::other)?;
        let write_result = (|| {
            let mut file = File::create(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, &self.path)
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        write_result?;
        let fingerprint = Fingerprint::of(&self.path)?;
        *self.cache.lock().unwrap() = Some((preferences.clone(), fingerprint));
        Ok(preferences)
    }

    /// Read-modify-write under the flock, so concurrent writers from other
    /// processes serialise instead of clobbering each other.
    pub fn update<F: FnOnce(&mut AiPreferences)>(&self, mutate: F) -> io::Result<AiPreferences> {
        let _lock = FileLock::exclusive(&self.lock_path)?;
        let mut preferences = self.read_fresh()?;
        mutate(&mut preferences);
        self.save(preferences)
    }

    /// Cheap change check for pollers: has the file been written since
    /// `updated_at_ms`?
    pub fn changed_since(&self, updated_at_ms: i64) -> bool {
        match self.load() {
            Ok(preferences) => preferences.updated_at_ms > updated_at_ms,
            Err(_) => true,
        }
    }

    /// One-time import of the Swift-side selection. Only fields that are
    /// still unset are filled, so a device that already chose through Rust
    /// is never overwritten by stale UserDefaults; the flag is set either way.
    pub fn import_legacy_swift(
        &self,
        legacy: &LegacyPreferences,
    ) -> io::Result<(AiPreferences, LegacyImportReport)> {
        let mut report = LegacyImportReport::default();
        let preferences = self.update(|preferences| {
            preferences.migrated_from_shared_defaults = true;
            let remap = |provider: &str, report: &mut LegacyImportReport| -> String {
                if provider == OPENAI_COMPATIBLE_ID
                    && legacy
                        .endpoints
                        .get(OPENAI_COMPATIBLE_ID)
                        .map(|endpoint| {
                            endpoint.trim().is_empty()
                                || catalogue::is_managed_omlx_endpoint(endpoint)
                        })
                        .unwrap_or(true)
                {
                    if !report.remapped_providers.contains(&provider.to_string()) {
                        report.remapped_providers.push(provider.to_string());
                    }
                    OMLX_ID.to_string()
                } else {
                    provider.to_string()
                }
            };
            for (provider, endpoint) in &legacy.endpoints {
                let endpoint = endpoint.trim();
                if endpoint.is_empty() || provider.trim().is_empty() {
                    continue;
                }
                let target = remap(provider, &mut report);
                if catalogue::descriptor(&target).is_none() {
                    continue;
                }
                let is_default = catalogue::descriptor(&target)
                    .and_then(|descriptor| descriptor.default_endpoint)
                    .is_some_and(|default| endpoint_root(default) == endpoint_root(endpoint));
                if is_default {
                    continue;
                }
                preferences
                    .endpoints
                    .entry(target)
                    .or_insert_with(|| endpoint.to_string());
            }
            if preferences.selected.is_none() {
                if let Some(provider) = legacy
                    .selected_provider
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    let provider = remap(provider, &mut report);
                    if catalogue::descriptor(&provider).is_some() {
                        let model = legacy
                            .selected_model
                            .as_deref()
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .and_then(|model| {
                                if is_helper_model(model) {
                                    report.dropped_models.push(model.to_string());
                                    None
                                } else {
                                    Some(model.to_string())
                                }
                            });
                        report.imported_selection =
                            Some(ModelRef::new(provider.clone(), model.clone()));
                        preferences.selected = Some(ModelRef::new(provider, model));
                    } else {
                        report.dropped_providers.push(provider);
                    }
                }
            }
            if let Some(auto_start) = legacy.auto_start_omlx {
                preferences.auto_start_omlx = auto_start;
            }
            if preferences.task_categories.is_empty() {
                if let Some(json) = legacy.task_category_assignments_json.as_deref() {
                    match parse_swift_assignments(json) {
                        Ok(assignments) => {
                            for (category, mut assignment) in assignments {
                                let mut sanitise = |reference: ModelRef| -> Option<ModelRef> {
                                    let provider = remap(&reference.provider, &mut report);
                                    if catalogue::descriptor(&provider).is_none() {
                                        report.dropped_providers.push(provider);
                                        return None;
                                    }
                                    let model = reference.model.filter(|model| {
                                        if is_helper_model(model) {
                                            report.dropped_models.push(model.clone());
                                            false
                                        } else {
                                            true
                                        }
                                    });
                                    Some(ModelRef {
                                        provider,
                                        model,
                                        display_name: reference.display_name,
                                    })
                                };
                                assignment.primary = assignment.primary.and_then(&mut sanitise);
                                assignment.comparison = assignment
                                    .comparison
                                    .into_iter()
                                    .filter_map(&mut sanitise)
                                    .collect();
                                report.imported_categories += 1;
                                preferences.task_categories.insert(category, assignment);
                            }
                        }
                        Err(error) => report.errors.push(format!("task categories: {error}")),
                    }
                }
            }
        })?;
        Ok((preferences, report))
    }
}

/// What the Swift side stored before Rust owned the selection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LegacyPreferences {
    pub selected_provider: Option<String>,
    pub selected_model: Option<String>,
    pub auto_start_omlx: Option<bool>,
    /// The `impressai.taskCategoryAssignments` JSON blob verbatim.
    pub task_category_assignments_json: Option<String>,
    /// Keychain `endpoint` fields the GUI lifted before deleting them.
    pub endpoints: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LegacyImportReport {
    pub imported_selection: Option<ModelRef>,
    pub imported_categories: usize,
    pub remapped_providers: Vec<String>,
    pub dropped_models: Vec<String>,
    pub dropped_providers: Vec<String>,
    pub errors: Vec<String>,
}

impl LegacyImportReport {
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        match &self.imported_selection {
            Some(selection) => parts.push(format!(
                "selection {}/{}",
                selection.provider,
                selection.model.as_deref().unwrap_or("<none>")
            )),
            None => parts.push("no selection".into()),
        }
        parts.push(format!("{} task categories", self.imported_categories));
        if !self.remapped_providers.is_empty() {
            parts.push(format!(
                "remapped {} → omlx",
                self.remapped_providers.join(", ")
            ));
        }
        if !self.dropped_models.is_empty() {
            parts.push(format!(
                "dropped helper models {}",
                self.dropped_models.join(", ")
            ));
        }
        if !self.dropped_providers.is_empty() {
            parts.push(format!(
                "dropped unknown providers {}",
                self.dropped_providers.join(", ")
            ));
        }
        parts.extend(self.errors.iter().cloned());
        parts.join("; ")
    }
}

/// Endpoint identity for comparisons: no trailing slash, no pasted `/v1`.
fn endpoint_root(endpoint: &str) -> String {
    let trimmed = endpoint.trim().trim_end_matches('/');
    trimmed
        .strip_suffix("/v1")
        .unwrap_or(trimmed)
        .trim_end_matches('/')
        .to_string()
}

pub fn is_helper_model(model: &str) -> bool {
    let lowered = model.trim().to_lowercase();
    HELPER_MODEL_IDS.contains(&lowered.as_str())
}

/// Swift's `[String: AITaskCategoryAssignment]` encoding.
fn parse_swift_assignments(json: &str) -> Result<BTreeMap<String, CategoryAssignment>, String> {
    #[derive(Deserialize)]
    struct SwiftReference {
        #[serde(rename = "providerId")]
        provider_id: String,
        #[serde(rename = "modelId")]
        model_id: String,
        #[serde(rename = "displayName")]
        display_name: Option<String>,
    }
    #[derive(Deserialize)]
    struct SwiftAssignment {
        #[serde(rename = "categoryId")]
        category_id: Option<String>,
        #[serde(rename = "primaryModel")]
        primary_model: Option<SwiftReference>,
        #[serde(rename = "comparisonModels", default)]
        comparison_models: Vec<SwiftReference>,
        #[serde(rename = "isEnabled", default = "default_true")]
        is_enabled: bool,
    }
    let convert = |reference: SwiftReference| ModelRef {
        provider: reference.provider_id,
        model: Some(reference.model_id),
        display_name: reference.display_name,
    };
    let decoded: BTreeMap<String, SwiftAssignment> =
        serde_json::from_str(json).map_err(|error| error.to_string())?;
    Ok(decoded
        .into_iter()
        .map(|(key, assignment)| {
            let category = assignment.category_id.unwrap_or(key);
            (
                category,
                CategoryAssignment {
                    primary: assignment.primary_model.map(convert),
                    comparison: assignment
                        .comparison_models
                        .into_iter()
                        .map(convert)
                        .collect(),
                    enabled: assignment.is_enabled,
                },
            )
        })
        .collect())
}

pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_defaults_and_save_round_trips() {
        let directory = tempfile::tempdir().unwrap();
        let store = PreferencesStore::open(directory.path());
        assert_eq!(store.load().unwrap(), AiPreferences::default());
        assert!(!store.changed_since(0));

        let saved = store
            .update(|preferences| {
                preferences.selected = Some(ModelRef::new("omlx", Some("qwen".into())));
                preferences
                    .endpoints
                    .insert("ollama".into(), "http://box:11434".into());
            })
            .unwrap();
        assert!(saved.updated_at_ms > 0);
        assert_eq!(saved.version, PREFERENCES_VERSION);
        assert_eq!(store.load().unwrap(), saved);
        assert!(store.changed_since(saved.updated_at_ms - 1));
        assert!(!store.changed_since(saved.updated_at_ms));
        assert!(!directory
            .path()
            .join("ai")
            .read_dir()
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")));

        // A second store (another process) sees the write through its own read.
        let other = PreferencesStore::open(directory.path());
        assert_eq!(other.load().unwrap().selected.unwrap().provider, "omlx");
    }

    #[test]
    fn corrupt_or_newer_files_are_errors_not_resets() {
        let directory = tempfile::tempdir().unwrap();
        let store = PreferencesStore::open(directory.path());
        fs::create_dir_all(directory.path().join("ai")).unwrap();
        fs::write(store.path(), b"{ not json").unwrap();
        assert_eq!(store.load().unwrap_err().kind(), ErrorKind::InvalidData);
        fs::write(store.path(), br#"{"version": 99}"#).unwrap();
        let error = store.load().unwrap_err();
        assert!(error.to_string().contains("newer"));
    }

    #[test]
    fn concurrent_updates_do_not_lose_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_path_buf();
        let handles: Vec<_> = (0..8)
            .map(|index| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let store = PreferencesStore::open(&path);
                    store
                        .update(|preferences| {
                            preferences
                                .endpoints
                                .insert(format!("p{index}"), format!("http://h{index}"));
                        })
                        .unwrap();
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let preferences = PreferencesStore::open(&path).load().unwrap();
        assert_eq!(preferences.endpoints.len(), 8);
    }

    #[test]
    fn legacy_import_remaps_drops_helpers_and_runs_once() {
        let directory = tempfile::tempdir().unwrap();
        let store = PreferencesStore::open(directory.path());
        let legacy = LegacyPreferences {
            selected_provider: Some("openai-compatible".into()),
            selected_model: Some("MarkItDown".into()),
            auto_start_omlx: Some(false),
            task_category_assignments_json: Some(
                r#"{"research.rag":{"categoryId":"research.rag","primaryModel":{"providerId":"openai-compatible","modelId":"mlx-community--Qwen3.5-4B-4bit","displayName":"Local - Qwen"},"comparisonModels":[{"providerId":"nope","modelId":"x","displayName":"x"}],"isEnabled":true}}"#.into(),
            ),
            endpoints: BTreeMap::from([
                ("openai-compatible".into(), "http://127.0.0.1:8000/v1".into()),
                ("ollama".into(), "http://localhost:11434".into()),
            ]),
        };
        let (preferences, report) = store.import_legacy_swift(&legacy).unwrap();
        assert!(preferences.migrated_from_shared_defaults);
        assert_eq!(preferences.selected, Some(ModelRef::new("omlx", None)));
        assert!(!preferences.auto_start_omlx);
        assert!(
            preferences.endpoints.is_empty(),
            "defaults are not stored as overrides"
        );
        let rag = &preferences.task_categories["research.rag"];
        assert_eq!(rag.primary.as_ref().unwrap().provider, "omlx");
        assert_eq!(
            rag.primary.as_ref().unwrap().model.as_deref(),
            Some("mlx-community--Qwen3.5-4B-4bit")
        );
        assert!(rag.comparison.is_empty(), "unknown providers are dropped");
        assert_eq!(report.dropped_models, ["MarkItDown"]);
        assert_eq!(report.remapped_providers, ["openai-compatible"]);
        assert_eq!(report.dropped_providers, ["nope"]);
        assert!(report
            .summary()
            .contains("dropped helper models MarkItDown"));

        // Re-import never overwrites a Rust-owned selection.
        let (again, report) = store
            .import_legacy_swift(&LegacyPreferences {
                selected_provider: Some("anthropic".into()),
                selected_model: Some("claude-opus-5".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(again.selected, Some(ModelRef::new("omlx", None)));
        assert!(report.imported_selection.is_none());
    }

    #[test]
    fn remote_openai_compatible_endpoints_stay_generic() {
        let directory = tempfile::tempdir().unwrap();
        let store = PreferencesStore::open(directory.path());
        let (preferences, report) = store
            .import_legacy_swift(&LegacyPreferences {
                selected_provider: Some("openai-compatible".into()),
                selected_model: Some("llama-70b".into()),
                endpoints: BTreeMap::from([(
                    "openai-compatible".into(),
                    "http://lab-box:8000/v1".into(),
                )]),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            preferences.selected,
            Some(ModelRef::new("openai-compatible", Some("llama-70b".into())))
        );
        assert_eq!(
            preferences
                .endpoints
                .get("openai-compatible")
                .map(String::as_str),
            Some("http://lab-box:8000/v1")
        );
        assert!(report.remapped_providers.is_empty());
    }
}
