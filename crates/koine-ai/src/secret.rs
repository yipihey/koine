use std::fmt;

use sha2::{Digest, Sha256};

/// An API key or bearer token. It never appears in `Debug` output, logs,
/// provenance or the preferences file; callers must `expose()` it
/// deliberately at the one place the wire needs it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw value, for building an `Authorization` header.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }

    /// Short, stable, non-reversible identity used to key client caches so a
    /// rotated credential produces a fresh client without the value itself
    /// being stored anywhere.
    pub fn fingerprint(&self) -> String {
        let digest = Sha256::digest(self.0.as_bytes());
        digest
            .iter()
            .take(8)
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_never_contains_the_value() {
        let secret = Secret::new("sk-ant-very-secret");
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
        assert_eq!(secret.expose(), "sk-ant-very-secret");
    }

    #[test]
    fn fingerprints_are_stable_short_and_distinct() {
        let first = Secret::new("a").fingerprint();
        assert_eq!(first.len(), 16);
        assert_eq!(first, Secret::new("a").fingerprint());
        assert_ne!(first, Secret::new("b").fingerprint());
        assert!(Secret::new("  ").is_empty());
    }
}
