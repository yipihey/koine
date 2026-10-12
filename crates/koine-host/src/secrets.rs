//! The host's secret sources (plan-koine-followups F-D1): where the values a
//! verb's `secrets = [...]` names come from, over the slot
//! `impress_service_core::secrets` defines.
//!
//! [`host_source`] layers them in the order a person overrides them:
//!
//! 1. the environment (`KOINE_SECRET_<NAME>`, read-only), so a launcher or a
//!    CI job wins over anything on disk;
//! 2. [`FileSource`], `<workspace>/secrets/<name>`, one line per file, the
//!    directory 0700 and each file 0600 — what `PUT /api/secret/<name>`
//!    writes;
//! 3. [`KeychainSource`], when the host is started with `--keychain`.
//!
//! Nothing here ever prints a value: a [`SecretError`]'s text names the
//! name and the failure, never the secret.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use impress_service_core::secrets::{is_secret_name, EnvSource, Secret, SecretError, SecretSource};

/// A directory of one-line files, one per secret.
#[derive(Debug, Clone)]
pub struct FileSource {
    dir: PathBuf,
}

impl FileSource {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The directory this source reads and writes.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_of(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

fn io_error(error: std::io::Error) -> SecretError {
    SecretError::Io(error.to_string())
}

/// Create `dir` (and parents) readable by this user only.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        // An existing directory keeps its mode under `create`: tighten it.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

fn open_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

static TEMP: AtomicU64 = AtomicU64::new(0);

impl SecretSource for FileSource {
    fn get(&self, name: &str) -> Option<Secret> {
        if !is_secret_name(name) {
            return None;
        }
        let text = std::fs::read_to_string(self.path_of(name)).ok()?;
        let value = text.strip_suffix('\n').unwrap_or(&text);
        let value = value.strip_suffix('\r').unwrap_or(value);
        (!value.is_empty()).then(|| Secret::new(value))
    }

    fn names(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| is_secret_name(name))
            .collect();
        names.sort();
        names
    }

    fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        if !is_secret_name(name) {
            return Err(SecretError::InvalidName(name.to_string()));
        }
        create_private_dir(&self.dir).map_err(io_error)?;
        // Temp file in the same directory (so the rename is atomic), created
        // 0600 from the start so the value is never readable by others.
        let temp = self.dir.join(format!(
            ".{name}.{}.{}.tmp",
            std::process::id(),
            TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let written = (|| {
            let mut file = open_private(&temp)?;
            file.write_all(value.as_bytes())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            std::fs::rename(&temp, self.path_of(name))
        })();
        if let Err(error) = written {
            let _ = std::fs::remove_file(&temp);
            return Err(io_error(error));
        }
        Ok(())
    }

    fn remove(&self, name: &str) -> Result<(), SecretError> {
        if !is_secret_name(name) {
            return Err(SecretError::InvalidName(name.to_string()));
        }
        match std::fs::remove_file(self.path_of(name)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_error(error)),
        }
    }

    fn source_of(&self, name: &str) -> Option<&'static str> {
        self.get(name).map(|_| "file")
    }
}

/// The macOS keychain, through `/usr/bin/security`.
///
/// It compiles everywhere and works where that binary exists; elsewhere
/// [`get`](SecretSource::get) answers `None`. The keychain is not enumerable
/// from here (that would be `security dump-keychain`, which prompts), so
/// [`names`](SecretSource::names) answers an empty list: a status line asks
/// about the declared names one at a time, which is all it needs. A value
/// written by [`set`](SecretSource::set) is passed as a process argument for
/// the length of the `security` call, which is how that tool takes it.
#[derive(Debug, Clone)]
pub struct KeychainSource {
    service: String,
}

impl Default for KeychainSource {
    fn default() -> Self {
        Self::new("koine")
    }
}

const SECURITY: &str = "/usr/bin/security";

impl KeychainSource {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn available() -> bool {
        Path::new(SECURITY).exists()
    }

    fn run(args: &[String]) -> Option<std::process::Output> {
        if !Self::available() {
            return None;
        }
        Command::new(SECURITY)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .ok()
    }
}

/// The argv for `security find-generic-password` of `name` under `service`.
fn find_args(service: &str, name: &str) -> Vec<String> {
    ["find-generic-password", "-s", service, "-a", name, "-w"]
        .map(String::from)
        .to_vec()
}

/// The argv for `security add-generic-password -U` (add or update).
fn add_args(service: &str, name: &str, value: &str) -> Vec<String> {
    [
        "add-generic-password",
        "-U",
        "-s",
        service,
        "-a",
        name,
        "-w",
        value,
    ]
    .map(String::from)
    .to_vec()
}

/// The argv for `security delete-generic-password`.
fn delete_args(service: &str, name: &str) -> Vec<String> {
    ["delete-generic-password", "-s", service, "-a", name]
        .map(String::from)
        .to_vec()
}

impl SecretSource for KeychainSource {
    fn get(&self, name: &str) -> Option<Secret> {
        if !is_secret_name(name) {
            return None;
        }
        let output = Self::run(&find_args(&self.service, name))?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8(output.stdout).ok()?;
        let value = text.trim();
        (!value.is_empty()).then(|| Secret::new(value))
    }

    /// Always empty: see the type's documentation.
    fn names(&self) -> Vec<String> {
        Vec::new()
    }

    fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        if !is_secret_name(name) {
            return Err(SecretError::InvalidName(name.to_string()));
        }
        let output = Self::run(&add_args(&self.service, name, value))
            .ok_or_else(|| SecretError::Io(format!("{SECURITY} is not available here")))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(SecretError::Io(format!(
                "the keychain refused `{name}` ({})",
                output.status
            )))
        }
    }

    fn remove(&self, name: &str) -> Result<(), SecretError> {
        if !is_secret_name(name) {
            return Err(SecretError::InvalidName(name.to_string()));
        }
        let output = Self::run(&delete_args(&self.service, name))
            .ok_or_else(|| SecretError::Io(format!("{SECURITY} is not available here")))?;
        // `security` exits 44 when the item is not there: removing what is
        // absent is not a failure.
        if output.status.success() || output.status.code() == Some(44) {
            Ok(())
        } else {
            Err(SecretError::Io(format!(
                "the keychain refused to delete `{name}` ({})",
                output.status
            )))
        }
    }

    fn source_of(&self, name: &str) -> Option<&'static str> {
        self.get(name).map(|_| "keychain")
    }
}

/// Sources asked in order. Writes go to the first layer that is not
/// read-only.
pub struct LayeredSource {
    layers: Vec<(&'static str, Arc<dyn SecretSource>)>,
}

impl LayeredSource {
    pub fn new(layers: Vec<(&'static str, Arc<dyn SecretSource>)>) -> Self {
        Self { layers }
    }

    fn write(
        &self,
        mut attempt: impl FnMut(&dyn SecretSource) -> Result<(), SecretError>,
    ) -> Result<(), SecretError> {
        for (_, layer) in &self.layers {
            match attempt(&**layer) {
                Err(SecretError::ReadOnly) => continue,
                other => return other,
            }
        }
        Err(SecretError::ReadOnly)
    }
}

impl SecretSource for LayeredSource {
    fn get(&self, name: &str) -> Option<Secret> {
        self.layers.iter().find_map(|(_, layer)| layer.get(name))
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .layers
            .iter()
            .flat_map(|(_, layer)| layer.names())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        self.write(|layer| layer.set(name, value))
    }

    fn remove(&self, name: &str) -> Result<(), SecretError> {
        self.write(|layer| layer.remove(name))
    }

    fn source_of(&self, name: &str) -> Option<&'static str> {
        self.layers
            .iter()
            .find(|(_, layer)| layer.get(name).is_some())
            .map(|(label, _)| *label)
    }
}

/// The host's source: the environment, then the files under `secrets_dir`,
/// then the keychain when `keychain` is set.
pub fn host_source(secrets_dir: PathBuf, keychain: bool) -> Arc<dyn SecretSource> {
    let mut layers: Vec<(&'static str, Arc<dyn SecretSource>)> = vec![
        ("env", Arc::new(EnvSource)),
        ("file", Arc::new(FileSource::new(secrets_dir))),
    ];
    if keychain {
        layers.push(("keychain", Arc::new(KeychainSource::default())));
    }
    Arc::new(LayeredSource::new(layers))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// An in-memory layer, so the layering tests touch no process
    /// environment. `writable: false` makes it read-only like the real env.
    struct Map {
        label: &'static str,
        writable: bool,
        values: Mutex<BTreeMap<String, String>>,
    }

    impl Map {
        fn new(label: &'static str, writable: bool, pairs: &[(&str, &str)]) -> Arc<Self> {
            Arc::new(Self {
                label,
                writable,
                values: Mutex::new(
                    pairs
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect(),
                ),
            })
        }
    }

    impl SecretSource for Map {
        fn get(&self, name: &str) -> Option<Secret> {
            self.values.lock().unwrap().get(name).map(Secret::new)
        }
        fn names(&self) -> Vec<String> {
            self.values.lock().unwrap().keys().cloned().collect()
        }
        fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
            if !self.writable {
                return Err(SecretError::ReadOnly);
            }
            self.values
                .lock()
                .unwrap()
                .insert(name.into(), value.into());
            Ok(())
        }
        fn remove(&self, name: &str) -> Result<(), SecretError> {
            if !self.writable {
                return Err(SecretError::ReadOnly);
            }
            self.values.lock().unwrap().remove(name);
            Ok(())
        }
        fn source_of(&self, name: &str) -> Option<&'static str> {
            self.get(name).map(|_| self.label)
        }
    }

    #[test]
    fn a_file_source_sets_gets_lists_and_removes() {
        let dir = tempfile::tempdir().unwrap();
        let source = FileSource::new(dir.path().join("secrets"));
        assert!(source.get("tok").is_none());
        assert!(source.names().is_empty());

        source.set("tok", "hunter2").unwrap();
        source.set("other-tok", "x").unwrap();
        assert_eq!(source.get("tok").unwrap().expose(), "hunter2");
        assert_eq!(source.source_of("tok"), Some("file"));
        assert_eq!(source.names(), vec!["other-tok", "tok"]);
        // One line on disk; reading trims the one newline.
        assert_eq!(
            std::fs::read_to_string(source.dir().join("tok")).unwrap(),
            "hunter2\n"
        );

        // A second write replaces; no temp file is left behind.
        source.set("tok", "two").unwrap();
        assert_eq!(source.get("tok").unwrap().expose(), "two");
        let leftovers = std::fs::read_dir(source.dir()).unwrap().count();
        assert_eq!(leftovers, 2);

        source.remove("tok").unwrap();
        assert!(source.get("tok").is_none());
        source.remove("tok").unwrap(); // absent is Ok
        assert_eq!(source.names(), vec!["other-tok"]);
    }

    #[cfg(unix)]
    #[test]
    fn the_directory_is_0700_and_the_file_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let secrets = dir.path().join("a").join("secrets");
        let source = FileSource::new(&secrets);
        source.set("tok", "v").unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&secrets), 0o700);
        assert_eq!(mode(&secrets.join("tok")), 0o600);
        // A replace keeps the mode.
        source.set("tok", "w").unwrap();
        assert_eq!(mode(&secrets.join("tok")), 0o600);
    }

    #[test]
    fn a_bad_name_is_refused_and_never_touches_the_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        let source = FileSource::new(dir.path());
        for bad in ["", "../escape", "A", "a/b", "1a", "a b", ".hidden"] {
            assert!(
                matches!(source.set(bad, "v"), Err(SecretError::InvalidName(_))),
                "{bad}"
            );
            assert!(
                matches!(source.remove(bad), Err(SecretError::InvalidName(_))),
                "{bad}"
            );
            assert!(source.get(bad).is_none(), "{bad}");
        }
        // A file that is not a secret name is not listed.
        std::fs::write(dir.path().join("Not-A-Name"), "x").unwrap();
        std::fs::write(dir.path().join("fine"), "x").unwrap();
        assert_eq!(source.names(), vec!["fine"]);
    }

    #[test]
    fn an_empty_file_is_not_a_secret() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blank"), "\n").unwrap();
        assert!(FileSource::new(dir.path()).get("blank").is_none());
    }

    #[test]
    fn layers_answer_in_order_and_name_the_one_that_did() {
        let env = Map::new("env", false, &[("shared", "from-env")]);
        let file = Map::new("file", true, &[("shared", "from-file"), ("only-file", "f")]);
        let layered = LayeredSource::new(vec![("env", env), ("file", file)]);
        assert_eq!(layered.get("shared").unwrap().expose(), "from-env");
        assert_eq!(layered.source_of("shared"), Some("env"));
        assert_eq!(layered.get("only-file").unwrap().expose(), "f");
        assert_eq!(layered.source_of("only-file"), Some("file"));
        assert!(layered.get("nowhere").is_none());
        assert_eq!(layered.source_of("nowhere"), None);
        assert_eq!(layered.names(), vec!["only-file", "shared"]);
    }

    #[test]
    fn writes_skip_read_only_layers() {
        let env = Map::new("env", false, &[]);
        let file = Map::new("file", true, &[]);
        let layered = LayeredSource::new(vec![("env", env), ("file", file.clone())]);
        layered.set("tok", "v").unwrap();
        assert!(file.get("tok").is_some());
        assert_eq!(layered.source_of("tok"), Some("file"));
        layered.remove("tok").unwrap();
        assert!(layered.get("tok").is_none());

        let only_env = LayeredSource::new(vec![("env", Map::new("env", false, &[]))]);
        assert_eq!(only_env.set("tok", "v"), Err(SecretError::ReadOnly));
        assert_eq!(only_env.remove("tok"), Err(SecretError::ReadOnly));
    }

    #[test]
    fn an_error_that_is_not_read_only_stops_the_walk() {
        struct Broken;
        impl SecretSource for Broken {
            fn get(&self, _: &str) -> Option<Secret> {
                None
            }
            fn names(&self) -> Vec<String> {
                Vec::new()
            }
            fn set(&self, _: &str, _: &str) -> Result<(), SecretError> {
                Err(SecretError::Io("disk full".into()))
            }
        }
        let file = Map::new("file", true, &[]);
        let layered = LayeredSource::new(vec![("a", Arc::new(Broken)), ("b", file.clone())]);
        assert_eq!(
            layered.set("tok", "v"),
            Err(SecretError::Io("disk full".into()))
        );
        assert!(file.get("tok").is_none());
    }

    #[test]
    fn the_host_source_has_env_then_file_and_keychain_only_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let source = host_source(dir.path().to_path_buf(), false);
        source.set("host-source-probe", "v").unwrap();
        assert_eq!(source.source_of("host-source-probe"), Some("file"));
        assert!(dir.path().join("host-source-probe").exists());
        // With the keychain layer the file layer still takes the write.
        let with = host_source(dir.path().to_path_buf(), true);
        assert_eq!(with.get("host-source-probe").unwrap().expose(), "v");
    }

    #[test]
    fn the_keychain_argv_is_security_s_own() {
        assert_eq!(
            find_args("koine", "scix-api-token"),
            [
                "find-generic-password",
                "-s",
                "koine",
                "-a",
                "scix-api-token",
                "-w"
            ]
        );
        assert_eq!(
            add_args("koine", "tok", "v"),
            [
                "add-generic-password",
                "-U",
                "-s",
                "koine",
                "-a",
                "tok",
                "-w",
                "v"
            ]
        );
        assert_eq!(
            delete_args("svc", "tok"),
            ["delete-generic-password", "-s", "svc", "-a", "tok"]
        );
        assert_eq!(KeychainSource::default().service, "koine");
        assert!(KeychainSource::default().names().is_empty());
        assert!(matches!(
            KeychainSource::default().set("Bad Name", "v"),
            Err(SecretError::InvalidName(_))
        ));
    }
}
