use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobDescriptor {
    pub sha256: String,
    pub byte_length: u64,
    pub storage_kind: String,
    pub locator: String,
}

/// Byte transport behind `content-blob@1.0.0` metadata items.
///
/// This port is intentionally independent of SQLite/CloudKit. The macOS
/// service uses [`FileBlobStore`]; an iOS/CloudKit asset adapter can implement
/// the same contract without changing message or provenance records.
pub trait BlobStore: Send + Sync {
    fn put(&self, bytes: &[u8]) -> Result<BlobDescriptor>;
    fn read(&self, descriptor: &BlobDescriptor) -> Result<Vec<u8>>;
    fn contains(&self, descriptor: &BlobDescriptor) -> Result<bool>;
}

/// Immutable content-addressed files, sharded by the first two hash bytes.
pub struct FileBlobStore {
    root: PathBuf,
}

impl FileBlobStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_for_hash(&self, hash: &str) -> Result<PathBuf> {
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::Blob("invalid SHA-256 locator".into()));
        }
        Ok(self.root.join(&hash[..2]).join(hash))
    }

    fn path_for(&self, descriptor: &BlobDescriptor) -> Result<PathBuf> {
        if descriptor.storage_kind != "local-cas" {
            return Err(Error::Blob(format!(
                "FileBlobStore cannot read storage kind '{}'",
                descriptor.storage_kind
            )));
        }
        self.path_for_hash(&descriptor.sha256)
    }

    /// Stable local path suitable for a `CKAsset`, after full hash and length
    /// verification. Callers never construct CAS paths from synced locators.
    pub fn verified_path(&self, descriptor: &BlobDescriptor) -> Result<PathBuf> {
        self.read(descriptor)?;
        self.path_for(descriptor)
    }

    /// Import bytes received through another transport while enforcing the
    /// immutable descriptor already stored in the graph.
    pub fn import_expected(
        &self,
        expected: &BlobDescriptor,
        bytes: &[u8],
    ) -> Result<BlobDescriptor> {
        let actual_hash = format!("{:x}", Sha256::digest(bytes));
        let actual_length = bytes.len() as u64;
        if actual_hash != expected.sha256 || actual_length != expected.byte_length {
            return Err(Error::Blob(format!(
                "received blob does not match metadata: expected {} ({} bytes), found {} ({} bytes)",
                expected.sha256, expected.byte_length, actual_hash, actual_length
            )));
        }
        self.put(bytes)
    }
}

impl BlobStore for FileBlobStore {
    fn put(&self, bytes: &[u8]) -> Result<BlobDescriptor> {
        let sha256 = format!("{:x}", Sha256::digest(bytes));
        let target = self.path_for_hash(&sha256)?;
        let parent = target
            .parent()
            .ok_or_else(|| Error::Blob("content path has no parent".into()))?;
        std::fs::create_dir_all(parent)?;
        if !target.exists() {
            let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
            std::fs::write(&temporary, bytes)?;
            match std::fs::rename(&temporary, &target) {
                Ok(()) => {}
                Err(_error) if target.exists() => {
                    let _ = std::fs::remove_file(&temporary);
                }
                Err(error) => {
                    let _ = std::fs::remove_file(&temporary);
                    return Err(error.into());
                }
            }
        }
        Ok(BlobDescriptor {
            sha256: sha256.clone(),
            byte_length: bytes.len() as u64,
            storage_kind: "local-cas".into(),
            // Portable, root-relative locator. Never persist an absolute path
            // from one device in the synced graph.
            locator: format!("{}/{}", &sha256[..2], sha256),
        })
    }

    fn read(&self, descriptor: &BlobDescriptor) -> Result<Vec<u8>> {
        let bytes = std::fs::read(self.path_for(descriptor)?)?;
        if bytes.len() as u64 != descriptor.byte_length {
            return Err(Error::Blob(format!(
                "blob {} length mismatch: expected {}, found {}",
                descriptor.sha256,
                descriptor.byte_length,
                bytes.len()
            )));
        }
        let actual = format!("{:x}", Sha256::digest(&bytes));
        if actual != descriptor.sha256 {
            return Err(Error::Blob(format!(
                "blob {} failed hash verification",
                descriptor.sha256
            )));
        }
        Ok(bytes)
    }

    fn contains(&self, descriptor: &BlobDescriptor) -> Result<bool> {
        Ok(self.path_for(descriptor)?.is_file())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_content_addressed_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileBlobStore::open(directory.path()).unwrap();
        let first = store.put(b"same bytes").unwrap();
        let second = store.put(b"same bytes").unwrap();
        assert_eq!(first, second);
        assert!(store.contains(&first).unwrap());
        assert_eq!(store.read(&first).unwrap(), b"same bytes");
        assert!(!first.locator.starts_with('/'));
    }

    #[test]
    fn rejects_a_tampered_blob() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileBlobStore::open(directory.path()).unwrap();
        let descriptor = store.put(b"original").unwrap();
        std::fs::write(store.path_for(&descriptor).unwrap(), b"tampered").unwrap();
        assert!(store.read(&descriptor).is_err());
    }

    #[test]
    fn rejects_import_that_does_not_match_expected_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileBlobStore::open(directory.path()).unwrap();
        let expected = BlobDescriptor {
            sha256: format!("{:x}", Sha256::digest(b"expected")),
            byte_length: 8,
            storage_kind: "local-cas".into(),
            locator: "unused".into(),
        };
        assert!(store.import_expected(&expected, b"different").is_err());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }
}
