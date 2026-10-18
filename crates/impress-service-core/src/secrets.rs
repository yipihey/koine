//! The secret slot (plan-koine-followups F-D1): what a verb that calls a
//! remote service needs, declared by the verb and held by the host.
//!
//! A bearer passed as an argument lands in every form, every CLI line and
//! every audit row. So a verb *declares* it —
//! `#[impress_method(secrets = ["scix-api-token"])]` — and reads it at run
//! time with [`get`]; the value never travels through the call.
//!
//! * [`Secret`] is a `String` that prints as `Secret(<redacted>)`; `expose()`
//!   is the one way out and it has no serde derive, so a secret cannot be
//!   serialised by accident.
//! * [`SecretSource`] is where values come from. Nothing installed means
//!   [`EnvSource`]: `KOINE_SECRET_<NAME>` with the name upper-cased and `-` as
//!   `_`, so a bare CLI or MCP process needs no host. `koine-host` installs a
//!   layered source (environment, a directory, optionally the keychain).
//! * The pipeline refuses a linked verb whose declared secret is absent,
//!   before the policy layer, as `missing-secret` ([`how_to_set`] is the
//!   second half of that message). A handler therefore never sees `None` for
//!   a name it declared.
//! * [`declared`] and [`status`] are what a status line shows: names, whether
//!   each is present and from which layer, never a value.

use std::fmt;
use std::sync::{Arc, RwLock};

use crate::descriptor::{is_kind_id, VerbDescriptor};

/// The environment prefix [`EnvSource`] reads.
pub const ENV_PREFIX: &str = "KOINE_SECRET_";

/// A secret value. `Debug` and `Display` print `Secret(<redacted>)`; there is
/// deliberately no `Serialize`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value. The one way out: a call site that writes it into a request
    /// header says so by name.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Why a [`SecretSource`] could not change a secret.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    /// The installed source cannot be written to (the environment layer).
    #[error(
        "this secret source is read-only: set the secret in the environment \
         (KOINE_SECRET_<NAME>) or start the host with --secrets-dir"
    )]
    ReadOnly,
    /// The name is not a secret name.
    #[error(
        "`{0}` is not a secret name: lowercase letters, digits, `_` or `-`, \
         starting with a letter (for example `scix-api-token`)"
    )]
    InvalidName(String),
    /// The source tried and the filesystem or keychain said no.
    #[error("the secret store failed: {0}")]
    Io(String),
}

/// Where secret values come from.
pub trait SecretSource: Send + Sync {
    /// The value, when this source holds one.
    fn get(&self, name: &str) -> Option<Secret>;

    /// Every name this source holds a value for.
    fn names(&self) -> Vec<String>;

    /// Store a value. Read-only by default.
    fn set(&self, _name: &str, _value: &str) -> Result<(), SecretError> {
        Err(SecretError::ReadOnly)
    }

    /// Remove a value. Read-only by default.
    fn remove(&self, _name: &str) -> Result<(), SecretError> {
        Err(SecretError::ReadOnly)
    }

    /// The diagnostic label of the layer that answers for `name` (`env`,
    /// `file`, `keychain`): what a status line shows, never a value.
    fn source_of(&self, name: &str) -> Option<&'static str> {
        self.get(name).map(|_| "installed")
    }
}

/// The environment variable that holds secret `name`: upper-cased, `-` as
/// `_`, behind `KOINE_SECRET_`.
pub fn env_var_name(name: &str) -> String {
    format!(
        "{ENV_PREFIX}{}",
        name.to_ascii_uppercase().replace('-', "_")
    )
}

/// The environment layer, and the default when nothing is installed.
#[derive(Debug, Clone, Copy, Default)]
pub struct EnvSource;

impl SecretSource for EnvSource {
    fn get(&self, name: &str) -> Option<Secret> {
        if !is_secret_name(name) {
            return None;
        }
        match std::env::var(env_var_name(name)) {
            Ok(value) if !value.is_empty() => Some(Secret::new(value)),
            _ => None,
        }
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::env::vars()
            .filter_map(|(key, value)| {
                let rest = key.strip_prefix(ENV_PREFIX)?;
                (!rest.is_empty() && !value.is_empty())
                    .then(|| rest.to_ascii_lowercase().replace('_', "-"))
            })
            .collect();
        names.sort();
        names.dedup();
        names
    }

    fn source_of(&self, name: &str) -> Option<&'static str> {
        self.get(name).map(|_| "env")
    }
}

static INSTALLED: RwLock<Option<Arc<dyn SecretSource>>> = RwLock::new(None);

/// Install the process-wide source; a later install replaces it.
pub fn install(source: Arc<dyn SecretSource>) {
    let mut slot = INSTALLED.write().unwrap_or_else(|e| e.into_inner());
    *slot = Some(source);
}

/// The installed source, else [`EnvSource`].
pub fn installed() -> Arc<dyn SecretSource> {
    INSTALLED
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| Arc::new(EnvSource))
}

/// The value of secret `name` from the installed source.
pub fn get(name: &str) -> Option<Secret> {
    installed().get(name)
}

/// Whether `name` is a well-formed secret name: the kind-id rule (lowercase
/// letter first, then lowercase letters, digits, `_` or `-`).
pub fn is_secret_name(name: &str) -> bool {
    is_kind_id(name)
}

/// One declared secret and the verbs that declare it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretDecl {
    pub name: &'static str,
    pub verbs: Vec<&'static str>,
}

/// Every secret any linked verb declares, with the verbs that declare it;
/// sorted by name, and each name's verbs sorted.
pub fn declared() -> Vec<SecretDecl> {
    declared_in(VerbDescriptor::iter())
}

fn declared_in<'a>(verbs: impl Iterator<Item = &'a VerbDescriptor>) -> Vec<SecretDecl> {
    let mut out: Vec<SecretDecl> = Vec::new();
    for verb in verbs {
        for &name in verb.secrets {
            match out.iter_mut().find(|decl| decl.name == name) {
                Some(decl) => {
                    if !decl.verbs.contains(&verb.name) {
                        decl.verbs.push(verb.name);
                    }
                }
                None => out.push(SecretDecl {
                    name,
                    verbs: vec![verb.name],
                }),
            }
        }
    }
    out.sort_by_key(|decl| decl.name);
    for decl in &mut out {
        decl.verbs.sort_unstable();
    }
    out
}

/// A declared secret, whether it is present and which layer answers for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretStatus {
    pub name: String,
    pub present: bool,
    pub source: Option<&'static str>,
    pub verbs: Vec<&'static str>,
}

/// [`declared`] × present, against the installed source. No value.
pub fn status() -> Vec<SecretStatus> {
    status_of(&*installed(), declared())
}

fn status_of(source: &dyn SecretSource, declared: Vec<SecretDecl>) -> Vec<SecretStatus> {
    declared
        .into_iter()
        .map(|decl| {
            let present = source.get(decl.name).is_some();
            SecretStatus {
                name: decl.name.to_string(),
                present,
                source: if present {
                    source.source_of(decl.name)
                } else {
                    None
                },
                verbs: decl.verbs,
            }
        })
        .collect()
}

/// The secrets `verb` declares that the installed source does not hold.
pub fn missing_for(verb: &VerbDescriptor) -> Vec<&'static str> {
    if verb.secrets.is_empty() {
        return Vec::new();
    }
    let source = installed();
    verb.secrets
        .iter()
        .copied()
        .filter(|name| source.get(name).is_none())
        .collect()
}

/// How to supply secret `name`: one string, the second half of the
/// `missing-secret` refusal.
pub fn how_to_set(name: &str) -> String {
    format!(
        "set {} in the environment, or PUT /api/secret/{name} on the host",
        env_var_name(name)
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::descriptor::{Effects, Safety, SafetyClass, Source};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// A source that holds values in memory, for tests that must not touch
    /// the process environment.
    #[derive(Default)]
    pub(crate) struct MapSource(pub(crate) Mutex<BTreeMap<String, String>>);

    impl MapSource {
        pub(crate) fn with(pairs: &[(&str, &str)]) -> Arc<Self> {
            let me = Self::default();
            for (k, v) in pairs {
                me.0.lock().unwrap().insert(k.to_string(), v.to_string());
            }
            Arc::new(me)
        }
    }

    impl SecretSource for MapSource {
        fn get(&self, name: &str) -> Option<Secret> {
            self.0.lock().unwrap().get(name).cloned().map(Secret::new)
        }
        fn names(&self) -> Vec<String> {
            self.0.lock().unwrap().keys().cloned().collect()
        }
        fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
            self.0.lock().unwrap().insert(name.into(), value.into());
            Ok(())
        }
        fn source_of(&self, name: &str) -> Option<&'static str> {
            self.get(name).map(|_| "file")
        }
    }

    /// Serialises the tests that install a process-wide source.
    pub(crate) static INSTALL_LOCK: Mutex<()> = Mutex::new(());

    fn verb(name: &'static str, secrets: &'static [&'static str]) -> VerbDescriptor {
        VerbDescriptor {
            name,
            service: "t-service",
            method: "m",
            description: "d",
            input_schema: || serde_json::json!({}),
            output_schema: || serde_json::json!({}),
            safety: Safety {
                class: SafetyClass::ReadOnly,
                idempotent: true,
            },
            effects: Effects {
                reads: &[],
                writes: &[],
                reach: &[],
            },
            since: "0.1.0",
            deprecated: None,
            aliases: &[],
            examples: &[],
            strict: false,
            budget_ms: None,
            replay_full: false,
            supports: &[],
            adjective: None,
            returns_kind: None,
            rows: None,
            secrets,
            source: Source::Linked,
            handler: |_| Box::pin(async { Ok(serde_json::json!({})) }),
            consent: crate::Consent::DEFAULT,
        }
    }

    #[test]
    fn a_secret_never_prints_its_value() {
        let s = Secret::new("hunter2");
        assert_eq!(format!("{s:?}"), "Secret(<redacted>)");
        assert_eq!(format!("{s}"), "Secret(<redacted>)");
        assert_eq!(s.expose(), "hunter2");
        assert!(!s.is_empty());
        assert!(Secret::new("").is_empty());
        assert_eq!(s.clone(), s);
    }

    #[test]
    fn env_var_names_go_both_ways() {
        assert_eq!(
            env_var_name("scix-api-token"),
            "KOINE_SECRET_SCIX_API_TOKEN"
        );
        assert_eq!(env_var_name("x"), "KOINE_SECRET_X");
        assert_eq!(
            how_to_set("scix-api-token"),
            "set KOINE_SECRET_SCIX_API_TOKEN in the environment, or PUT \
             /api/secret/scix-api-token on the host"
        );
    }

    #[test]
    fn the_env_source_reads_and_lists_through_the_environment() {
        // A name no other test uses, set and removed inside this one.
        let var = env_var_name("secrets-unit-probe");
        assert_eq!(var, "KOINE_SECRET_SECRETS_UNIT_PROBE");
        std::env::set_var(&var, "v1");
        let src = EnvSource;
        assert_eq!(src.get("secrets-unit-probe").unwrap().expose(), "v1");
        assert_eq!(src.source_of("secrets-unit-probe"), Some("env"));
        assert!(src.names().contains(&"secrets-unit-probe".to_string()));
        assert_eq!(src.set("a", "b"), Err(SecretError::ReadOnly));
        assert_eq!(src.remove("a"), Err(SecretError::ReadOnly));
        std::env::remove_var(&var);
        assert!(src.get("secrets-unit-probe").is_none());
        assert_eq!(src.source_of("secrets-unit-probe"), None);
        // Not a name: never reaches the environment.
        assert!(src.get("Not A Name").is_none());
    }

    #[test]
    fn a_later_install_replaces_the_earlier_one() {
        let _guard = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        install(MapSource::with(&[("a-b", "one")]));
        assert_eq!(get("a-b").unwrap().expose(), "one");
        install(MapSource::with(&[("a-b", "two")]));
        assert_eq!(get("a-b").unwrap().expose(), "two");
        assert_eq!(installed().source_of("a-b"), Some("file"));
        installed().set("c", "3").unwrap();
        assert_eq!(get("c").unwrap().expose(), "3");
    }

    #[test]
    fn secret_names_follow_the_kind_id_rule() {
        assert!(is_secret_name("scix-api-token"));
        assert!(is_secret_name("a1_b"));
        assert!(!is_secret_name(""));
        assert!(!is_secret_name("1a"));
        assert!(!is_secret_name("Token"));
        assert!(!is_secret_name("a b"));
    }

    #[test]
    fn declared_groups_by_name_and_sorts() {
        let a = verb("z-one", &["tok-b", "tok-a"]);
        let b = verb("a-two", &["tok-a"]);
        let c = verb("m-none", &[]);
        let decls = declared_in([&a, &b, &c].into_iter());
        assert_eq!(
            decls,
            vec![
                SecretDecl {
                    name: "tok-a",
                    verbs: vec!["a-two", "z-one"]
                },
                SecretDecl {
                    name: "tok-b",
                    verbs: vec!["z-one"]
                },
            ]
        );
    }

    #[test]
    fn status_is_declared_times_present_and_carries_no_value() {
        let a = verb("v-one", &["tok-a", "tok-b"]);
        let src = MapSource::with(&[("tok-a", "SECRETVALUE")]);
        let rows = status_of(&*src, declared_in([&a].into_iter()));
        assert_eq!(rows.len(), 2);
        assert!(rows[0].present);
        assert_eq!(rows[0].source, Some("file"));
        assert!(!rows[1].present);
        assert_eq!(rows[1].source, None);
        assert!(!format!("{rows:?}").contains("SECRETVALUE"));
    }

    #[test]
    fn missing_for_lists_only_what_the_installed_source_lacks() {
        let _guard = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let v = verb("v-one", &["tok-a", "tok-b"]);
        install(MapSource::with(&[("tok-a", "x")]));
        assert_eq!(missing_for(&v), vec!["tok-b"]);
        install(MapSource::with(&[("tok-a", "x"), ("tok-b", "y")]));
        assert!(missing_for(&v).is_empty());
        assert!(missing_for(&verb("v-none", &[])).is_empty());
    }
}
