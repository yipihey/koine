//! [`SettingsStore`]: the per-scope files under `<workspace>/settings/`, and the
//! [`SyncedBackend`] seam for the one synced store row.
//!
//! The file discipline is `impress_ai::preferences::PreferencesStore`'s, the
//! precedent the plan measured (`preferences.rs:161-246`): re-parse only when
//! the file's mtime or length moved, write temp + `fsync` + rename, and hold
//! an advisory `flock` (`impress-fs-lock`) around every read-modify-write.
//! One difference: there is one file per [`Scope`]. Writers retain each
//! scope's flock and share a short cursor lock so cross-file timestamps are
//! monotonic for polling across processes.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use impress_fs_lock::FileLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::registry::{lookup, registry, SettingDef};

pub const SETTINGS_DIRECTORY: &str = "settings";
pub const SETTINGS_FILE_VERSION: u32 = 1;

pub fn settings_directory(workspace: impl AsRef<Path>) -> PathBuf {
    workspace.as_ref().join(SETTINGS_DIRECTORY)
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("no setting named `{0}` is declared in the registry")]
    UnknownKey(String),
    #[error("`{key}` takes a {expected}, not {got}")]
    TypeMismatch {
        key: String,
        expected: &'static str,
        got: String,
    },
    #[error("`{0}` is a synced setting and no synced backend is installed in this process")]
    NoSyncedBackend(String),
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// Where a resolved value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueSource {
    /// The scope's file (or the synced row) holds a value.
    Stored,
    /// Nothing is stored; this is the registry default.
    Default,
}

/// A setting with its current value.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub def: &'static SettingDef,
    pub value: Value,
    pub source: ValueSource,
}

/// The synced scope's storage, implemented by the store-tier service over
/// the `impress/settings@1.0.0` row. This crate never opens a store.
pub trait SyncedBackend: Send + Sync {
    fn load(&self) -> io::Result<BTreeMap<String, Value>>;
    fn save(&self, values: &BTreeMap<String, Value>) -> io::Result<()>;
}

/// One scope file's contents.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
struct ScopeFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    updated_at_ms: i64,
    #[serde(default)]
    values: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    modified: Option<SystemTime>,
    len: u64,
    // Atomic rename replaces the inode. mtime and length alone can remain
    // equal for two same-clock writes of equally sized values.
    #[cfg(unix)]
    inode: u64,
}

impl Fingerprint {
    fn of(path: &Path) -> io::Result<Option<Self>> {
        match fs::metadata(path) {
            Ok(metadata) => Ok(Some(Self {
                modified: metadata.modified().ok(),
                len: metadata.len(),
                #[cfg(unix)]
                inode: metadata.ino(),
            })),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

pub struct SettingsStore {
    directory: PathBuf,
    cache: Mutex<HashMap<String, (ScopeFile, Option<Fingerprint>)>>,
    synced: RwLock<Option<Arc<dyn SyncedBackend>>>,
}

impl std::fmt::Debug for SettingsStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsStore")
            .field("directory", &self.directory)
            .finish()
    }
}

impl SettingsStore {
    /// `workspace` is the directory holding `impress.sqlite`; the files live
    /// beside it under `settings/`.
    pub fn open(workspace: impl AsRef<Path>) -> Self {
        Self {
            directory: settings_directory(workspace),
            cache: Mutex::new(HashMap::new()),
            synced: RwLock::new(None),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Install the synced scope's storage. The store-tier service does this
    /// once per process; a pure caller (tests, the CLI without a store) may
    /// leave it out, and every synced key then answers its default and
    /// refuses `set`.
    pub fn set_synced_backend(&self, backend: Arc<dyn SyncedBackend>) {
        *self.synced.write().unwrap_or_else(|e| e.into_inner()) = Some(backend);
    }

    pub fn has_synced_backend(&self) -> bool {
        self.synced
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    fn path(&self, file_name: &str) -> PathBuf {
        self.directory.join(file_name)
    }

    fn lock_path(&self, file_name: &str) -> PathBuf {
        self.directory.join(format!("{file_name}.lock"))
    }

    fn cursor_lock_path(&self) -> PathBuf {
        self.directory.join(".updated-at.lock")
    }

    fn scope_files() -> Vec<String> {
        let mut files: Vec<String> = registry()
            .iter()
            .filter_map(|def| def.scope.file_name())
            .collect();
        files.sort();
        files.dedup();
        files
    }

    /// Read every known scope directly from disk while holding the cursor
    /// lock. A per-file maximum is insufficient: the public cursor is the
    /// maximum across all files, so a write to another file must exceed it.
    fn newest_on_disk(&self) -> io::Result<i64> {
        Self::scope_files()
            .iter()
            .map(|file_name| self.read_fresh(file_name).map(|file| file.updated_at_ms))
            .try_fold(0, |newest, timestamp| {
                timestamp.map(|value| newest.max(value))
            })
    }

    fn read_fresh(&self, file_name: &str) -> io::Result<ScopeFile> {
        let path = self.path(file_name);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(ScopeFile::default()),
            Err(error) => return Err(error),
        };
        let file: ScopeFile = serde_json::from_slice(&bytes).map_err(|error| {
            io::Error::new(
                ErrorKind::InvalidData,
                format!("decode {}: {error}", path.display()),
            )
        })?;
        if file.version > SETTINGS_FILE_VERSION {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                format!(
                    "{} is version {}, newer than this build understands ({SETTINGS_FILE_VERSION})",
                    path.display(),
                    file.version
                ),
            ));
        }
        Ok(file)
    }

    /// The scope file, re-parsed only when its fingerprint moved.
    fn load(&self, file_name: &str) -> io::Result<ScopeFile> {
        let fingerprint = Fingerprint::of(&self.path(file_name))?;
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((cached, cached_fingerprint)) = cache.get(file_name) {
            if *cached_fingerprint == fingerprint {
                return Ok(cached.clone());
            }
        }
        let file = self.read_fresh(file_name)?;
        cache.insert(file_name.to_string(), (file.clone(), fingerprint));
        Ok(file)
    }

    /// Called only under the cursor lock and this scope's file lock.
    fn save(
        &self,
        file_name: &str,
        mut file: ScopeFile,
        observed_now_ms: i64,
    ) -> io::Result<ScopeFile> {
        file.version = SETTINGS_FILE_VERSION;
        let next = self
            .newest_on_disk()?
            .checked_add(1)
            .ok_or_else(|| io::Error::other("settings update cursor exhausted"))?;
        file.updated_at_ms = observed_now_ms.max(next);
        fs::create_dir_all(&self.directory)?;
        let path = self.path(file_name);
        let temporary = self
            .directory
            .join(format!(".{file_name}.{}.tmp", std::process::id()));
        let bytes = serde_json::to_vec_pretty(&file).map_err(io::Error::other)?;
        let write_result = (|| {
            let mut handle = File::create(&temporary)?;
            handle.write_all(&bytes)?;
            handle.sync_all()?;
            fs::rename(&temporary, &path)
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        write_result?;
        let fingerprint = Fingerprint::of(&path)?;
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(file_name.to_string(), (file.clone(), fingerprint));
        Ok(file)
    }

    /// Read-modify-write one scope file under its flock.
    fn update<F: FnOnce(&mut ScopeFile)>(
        &self,
        file_name: &str,
        mutate: F,
    ) -> io::Result<ScopeFile> {
        self.update_at(file_name, now_ms(), mutate)
    }

    /// `observed_now_ms` is injectable so same-clock writes can be tested
    /// without depending on scheduler timing or wall-clock precision.
    fn update_at<F: FnOnce(&mut ScopeFile)>(
        &self,
        file_name: &str,
        observed_now_ms: i64,
        mutate: F,
    ) -> io::Result<ScopeFile> {
        // Lock order is global cursor, then scope. All writers use this order;
        // separate scopes still retain their own read-modify-write flock.
        let _cursor_lock = FileLock::exclusive(self.cursor_lock_path())?;
        let _lock = FileLock::exclusive(self.lock_path(file_name))?;
        let mut file = self.read_fresh(file_name)?;
        mutate(&mut file);
        self.save(file_name, file, observed_now_ms)
    }

    fn synced(&self) -> Option<Arc<dyn SyncedBackend>> {
        self.synced
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn stored(&self, def: &SettingDef) -> Result<Option<Value>, SettingsError> {
        match def.scope.file_name() {
            Some(file_name) => Ok(self.load(&file_name)?.values.get(&def.key).cloned()),
            None => match self.synced() {
                Some(backend) => Ok(backend.load()?.get(&def.key).cloned()),
                None => Ok(None),
            },
        }
    }

    fn write(&self, def: &SettingDef, value: Option<Value>) -> Result<(), SettingsError> {
        match def.scope.file_name() {
            Some(file_name) => {
                self.update(&file_name, |file| match value {
                    Some(value) => {
                        file.values.insert(def.key.clone(), value);
                    }
                    None => {
                        file.values.remove(&def.key);
                    }
                })?;
                Ok(())
            }
            None => {
                let backend = self
                    .synced()
                    .ok_or_else(|| SettingsError::NoSyncedBackend(def.key.clone()))?;
                let mut values = backend.load()?;
                match value {
                    Some(value) => {
                        values.insert(def.key.clone(), value);
                    }
                    None => {
                        values.remove(&def.key);
                    }
                }
                backend.save(&values)?;
                Ok(())
            }
        }
    }

    fn resolve(&self, def: &'static SettingDef) -> Result<Resolved, SettingsError> {
        // A stored value of the wrong type (a hand-edited file) answers the
        // default rather than a value the reader cannot use; `set` refuses
        // to write one.
        match self.stored(def)?.and_then(|value| def.ty.coerce(&value)) {
            Some(value) => Ok(Resolved {
                def,
                value,
                source: ValueSource::Stored,
            }),
            None => Ok(Resolved {
                def,
                value: def.default.to_json(),
                source: ValueSource::Default,
            }),
        }
    }

    fn def(key: &str) -> Result<&'static SettingDef, SettingsError> {
        lookup(key).ok_or_else(|| SettingsError::UnknownKey(key.to_string()))
    }

    /// The current value of `key`: what is stored, else the default.
    pub fn get(&self, key: &str) -> Result<Resolved, SettingsError> {
        self.resolve(Self::def(key)?)
    }

    /// Store `value` for `key`. A choice label or a string spelling of the
    /// value is accepted (the generated surface posts strings); anything else
    /// of the wrong type is refused and nothing is written.
    pub fn set(&self, key: &str, value: &Value) -> Result<Resolved, SettingsError> {
        let def = Self::def(key)?;
        let value = def
            .accept(value)
            .ok_or_else(|| SettingsError::TypeMismatch {
                key: key.to_string(),
                expected: def.ty.label(),
                got: describe(value),
            })?;
        self.write(def, Some(value))?;
        self.resolve(def)
    }

    /// Forget the stored value, so `key` answers its default again.
    pub fn reset(&self, key: &str) -> Result<Resolved, SettingsError> {
        let def = Self::def(key)?;
        self.write(def, None)?;
        self.resolve(def)
    }

    /// Copy a legacy `UserDefaults` value in, once: writes only when nothing
    /// is stored for `key` yet, and answers whether it did. The caller keeps
    /// the legacy value where it was (D-R5). A legacy value the type cannot
    /// read is not imported and answers `Ok(false)` — the default stands.
    pub fn import_legacy(&self, key: &str, value: &Value) -> Result<bool, SettingsError> {
        let def = Self::def(key)?;
        let Some(value) = def.accept(value) else {
            return Ok(false);
        };
        if def.scope.file_name().is_none() && self.synced().is_none() {
            return Err(SettingsError::NoSyncedBackend(key.to_string()));
        }
        if self.stored(def)?.is_some() {
            return Ok(false);
        }
        self.write(def, Some(value))?;
        Ok(true)
    }

    /// Every setting (of `section`, when given) with its current value, in
    /// registry order.
    pub fn list(&self, section: Option<&str>) -> Result<Vec<Resolved>, SettingsError> {
        registry()
            .iter()
            .filter(|def| section.is_none_or(|s| def.section == s))
            .map(|def| self.resolve(def))
            .collect()
    }

    /// The newest `updated_at_ms` across the scope files this build knows —
    /// the cursor a poller compares.
    pub fn updated_at_ms(&self) -> i64 {
        Self::scope_files()
            .iter()
            .filter_map(|file_name| self.load(file_name).ok())
            .map(|file| file.updated_at_ms)
            .max()
            .unwrap_or(0)
    }

    /// Has any scope file been written since `updated_at_ms`? A file that
    /// cannot be read counts as changed, so a poller re-reads and reports.
    pub fn changed_since(&self, updated_at_ms: i64) -> bool {
        Self::scope_files()
            .iter()
            .any(|file_name| match self.load(file_name) {
                Ok(file) => file.updated_at_ms > updated_at_ms,
                Err(_) => true,
            })
    }
}

fn describe(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(_) => "a bool".into(),
        Value::Number(_) => "a number".into(),
        Value::String(s) => format!("the string {s:?}"),
        Value::Array(_) => "an array".into(),
        Value::Object(_) => "an object".into(),
    }
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Scope;

    const INBOX: &str = "imbib.retention.inbox_days";
    const AUTO_REMOVE: &str = "imbib.retention.auto_remove_read";
    const PORT: &str = "imbib.automation.http_port";

    #[test]
    fn defaults_then_set_reset_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::open(dir.path());
        let got = store.get(INBOX).unwrap();
        assert_eq!(got.value, Value::from(30));
        assert_eq!(got.source, ValueSource::Default);
        assert!(!store.changed_since(0));

        let set = store.set(INBOX, &Value::from(7)).unwrap();
        assert_eq!(set.value, Value::from(7));
        assert_eq!(set.source, ValueSource::Stored);
        assert!(dir.path().join("settings/device.json").exists());
        assert!(store.changed_since(0));
        assert!(!store.changed_since(store.updated_at_ms()));

        // A choice label and a string spelling both land as the integer.
        assert_eq!(
            store.set(INBOX, &Value::from("3 Months")).unwrap().value,
            Value::from(90)
        );
        assert_eq!(
            store.set(INBOX, &Value::from("14")).unwrap().value,
            Value::from(14)
        );

        // Another process sees the write through its own read.
        let other = SettingsStore::open(dir.path());
        assert_eq!(other.get(INBOX).unwrap().value, Value::from(14));

        let reset = store.reset(INBOX).unwrap();
        assert_eq!(reset.source, ValueSource::Default);
        assert_eq!(other.get(INBOX).unwrap().value, Value::from(30));
    }

    #[test]
    fn scopes_write_their_own_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::open(dir.path());
        store.set(PORT, &Value::from(23181)).unwrap();
        store.set(AUTO_REMOVE, &Value::Bool(true)).unwrap();
        assert!(dir.path().join("settings/app-imbib.json").exists());
        assert!(dir.path().join("settings/device.json").exists());
        let app: ScopeFile =
            serde_json::from_slice(&fs::read(dir.path().join("settings/app-imbib.json")).unwrap())
                .unwrap();
        assert_eq!(app.values.get(PORT), Some(&Value::from(23181)));
        assert!(!app.values.contains_key(AUTO_REMOVE));
        assert!(!dir
            .path()
            .join("settings")
            .read_dir()
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")));
    }

    #[test]
    fn unknown_keys_and_wrong_types_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::open(dir.path());
        assert!(matches!(
            store.get("imbib.retention.inboxDays"),
            Err(SettingsError::UnknownKey(_))
        ));
        let error = store.set(INBOX, &Value::from("soon")).unwrap_err();
        assert!(
            matches!(error, SettingsError::TypeMismatch { .. }),
            "{error}"
        );
        assert!(error.to_string().contains("integer"));
        assert!(matches!(
            store.set(AUTO_REMOVE, &Value::from(1)),
            Err(SettingsError::TypeMismatch { .. })
        ));
        assert_eq!(
            store.get(INBOX).unwrap().source,
            ValueSource::Default,
            "nothing written"
        );
    }

    #[test]
    fn legacy_import_runs_once_and_never_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::open(dir.path());
        assert!(store.import_legacy(INBOX, &Value::from(90)).unwrap());
        assert_eq!(store.get(INBOX).unwrap().value, Value::from(90));
        assert!(
            !store.import_legacy(INBOX, &Value::from(7)).unwrap(),
            "second import is a no-op"
        );
        assert_eq!(store.get(INBOX).unwrap().value, Value::from(90));
        assert!(
            !store
                .import_legacy(AUTO_REMOVE, &Value::from("maybe"))
                .unwrap(),
            "unreadable legacy value"
        );
        assert_eq!(store.get(AUTO_REMOVE).unwrap().source, ValueSource::Default);
    }

    #[test]
    fn corrupt_or_newer_files_are_errors_not_resets() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::open(dir.path());
        fs::create_dir_all(dir.path().join("settings")).unwrap();
        fs::write(dir.path().join("settings/device.json"), b"{ not json").unwrap();
        assert!(matches!(store.get(INBOX), Err(SettingsError::Io(_))));
        assert!(
            store.changed_since(i64::MAX),
            "unreadable counts as changed"
        );
        fs::write(
            dir.path().join("settings/device.json"),
            br#"{"version": 99}"#,
        )
        .unwrap();
        assert!(store.get(INBOX).unwrap_err().to_string().contains("newer"));
    }

    #[test]
    fn a_stored_value_of_the_wrong_type_answers_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::open(dir.path());
        fs::create_dir_all(dir.path().join("settings")).unwrap();
        fs::write(
            dir.path().join("settings/device.json"),
            format!(r#"{{"version": 1, "values": {{"{INBOX}": "soon"}}}}"#),
        )
        .unwrap();
        let got = store.get(INBOX).unwrap();
        assert_eq!(got.source, ValueSource::Default);
        assert_eq!(got.value, Value::from(30));
    }

    #[test]
    fn concurrent_writers_do_not_lose_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let keys = [
            "imbib.retention.inbox_days",
            "imbib.retention.auto_remove_read",
            "imbib.retention.exploration_days",
        ];
        let handles: Vec<_> = (0..12)
            .map(|index| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let store = SettingsStore::open(&path);
                    let key = keys[index % 3];
                    let value = match index % 3 {
                        1 => Value::Bool(true),
                        _ => Value::from(90),
                    };
                    store.set(key, &value).unwrap();
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let store = SettingsStore::open(&path);
        for key in keys {
            assert_eq!(store.get(key).unwrap().source, ValueSource::Stored, "{key}");
        }
    }

    #[test]
    fn same_clock_writes_advance_the_cross_process_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let first = SettingsStore::open(dir.path());
        let second = SettingsStore::open(dir.path());
        let clock = 1_000;

        first
            .update_at("device.json", clock, |file| {
                file.values.insert(INBOX.into(), Value::from(7));
            })
            .unwrap();
        let cursor = second.updated_at_ms();
        assert_eq!(cursor, clock);
        assert!(!second.changed_since(cursor));

        second
            .update_at("device.json", clock, |file| {
                file.values.insert(INBOX.into(), Value::from(8));
            })
            .unwrap();
        assert_eq!(second.updated_at_ms(), clock + 1);
        assert!(first.changed_since(cursor));
        assert_eq!(first.get(INBOX).unwrap().value, Value::from(8));

        // The cursor covers all scope files, not merely writes to the same
        // file. An app-scoped write must exceed the device-scoped maximum.
        let next_cursor = first.updated_at_ms();
        first
            .update_at("app-imbib.json", clock, |file| {
                file.values.insert(PORT.into(), Value::from(23181));
            })
            .unwrap();
        assert_eq!(first.updated_at_ms(), clock + 2);
        assert!(second.changed_since(next_cursor));
        assert_eq!(second.get(PORT).unwrap().value, Value::from(23181));
    }

    #[test]
    fn concurrent_scope_writes_share_one_monotonic_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let start = Arc::new(std::sync::Barrier::new(3));
        let handles: Vec<_> = [
            ("device.json", INBOX, Value::from(7)),
            ("app-imbib.json", PORT, Value::from(23181)),
        ]
        .into_iter()
        .map(|(file_name, key, value)| {
            let path = path.clone();
            let start = start.clone();
            std::thread::spawn(move || {
                let store = SettingsStore::open(path);
                start.wait();
                store
                    .update_at(file_name, 1_000, |file| {
                        file.values.insert(key.into(), value);
                    })
                    .unwrap()
                    .updated_at_ms
            })
        })
        .collect();
        start.wait();
        let mut timestamps: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        timestamps.sort();
        assert_eq!(timestamps, [1_000, 1_001]);
        let reader = SettingsStore::open(dir.path());
        assert_eq!(reader.updated_at_ms(), 1_001);
        assert_eq!(reader.get(INBOX).unwrap().value, Value::from(7));
        assert_eq!(reader.get(PORT).unwrap().value, Value::from(23181));
    }

    #[derive(Default)]
    struct MemoryBackend(Mutex<BTreeMap<String, Value>>);

    impl SyncedBackend for MemoryBackend {
        fn load(&self) -> io::Result<BTreeMap<String, Value>> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn save(&self, values: &BTreeMap<String, Value>) -> io::Result<()> {
            *self.0.lock().unwrap() = values.clone();
            Ok(())
        }
    }

    /// No first key is synced, so the seam is exercised on a synthetic
    /// declaration: the store answers the default without a backend, refuses
    /// `set`, and round-trips once one is installed.
    #[test]
    fn synced_scope_goes_through_the_backend() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::open(dir.path());
        let def: &'static SettingDef = Box::leak(Box::new(crate::setting! {
            key = "test.synced.flag", ty = Bool, default = false, scope = Scope::Synced,
            section = "imbib.retention", label = "Flag", doc = "A synced flag."
        }));
        assert_eq!(store.resolve(def).unwrap().source, ValueSource::Default);
        assert!(matches!(
            store.write(def, Some(Value::Bool(true))),
            Err(SettingsError::NoSyncedBackend(_))
        ));
        let backend = Arc::new(MemoryBackend::default());
        store.set_synced_backend(backend.clone());
        store.write(def, Some(Value::Bool(true))).unwrap();
        assert_eq!(store.resolve(def).unwrap().value, Value::Bool(true));
        assert_eq!(backend.load().unwrap().len(), 1);
        store.write(def, None).unwrap();
        assert_eq!(store.resolve(def).unwrap().source, ValueSource::Default);
    }
}
