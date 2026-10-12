//! `koine-store`: the store port by name, and a backend from a URL.
//!
//! The port is [`impress_item`] (re-exported here as [`port`]): `ItemStore`,
//! `AttributedStore`, `BlobStore`, the item, query, operation and event
//! types. The backends are crates of their own: `koine-store-memory`,
//! `koine-store-dir`, and `SqliteItemStore` in impress-apps. A host takes one
//! string and never names a backend (`docs/plan-koine-store.md` D7):
//!
//! ```
//! let store = koine_store::open("memory:").unwrap();
//! assert!(!store.capabilities().durable);
//! ```
//!
//! `memory:` and `dir:<path>` are known here. Another backend registers its
//! scheme with [`register`] before the host opens — `sqlite:` is
//! impress-core's to register — and [`schemes`] lists what this process can
//! open.

pub mod process;

use std::collections::BTreeMap;
use std::sync::RwLock;

pub use impress_item as port;
use impress_item::{AttributedStore, BlobStore, StoreError};
use std::sync::Arc;

/// A function that opens a backend from the part of the URL after its
/// scheme (`dir:/a/b` hands `/a/b`; `memory:` hands `""`).
pub type Opener = fn(&str) -> Result<Box<dyn AttributedStore>, StoreError>;

/// Why [`open`] could not.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// No `scheme:` prefix, or an empty scheme.
    #[error("store url '{url}' has no scheme; expected '<scheme>:<rest>', one of {known}")]
    NoScheme { url: String, known: String },
    /// A scheme no backend registered in this process.
    #[error("no store backend for scheme '{scheme}' (known: {known})")]
    UnknownScheme { scheme: String, known: String },
    /// The backend refused.
    #[error("open '{url}': {source}")]
    Backend {
        url: String,
        #[source]
        source: StoreError,
    },
}

static OPENERS: RwLock<BTreeMap<&'static str, Opener>> = RwLock::new(BTreeMap::new());

fn open_memory(_rest: &str) -> Result<Box<dyn AttributedStore>, StoreError> {
    Ok(Box::new(koine_store_memory::MemoryStore::new()))
}

fn open_dir(rest: &str) -> Result<Box<dyn AttributedStore>, StoreError> {
    if rest.is_empty() {
        return Err(StoreError::Validation(
            "dir: needs a path, as in dir:/path/to/root".into(),
        ));
    }
    Ok(Box::new(koine_store_dir::DirStore::open(rest)?))
}

fn with_builtins<R>(f: impl FnOnce(&mut BTreeMap<&'static str, Opener>) -> R) -> R {
    let mut map = OPENERS.write().unwrap_or_else(|e| e.into_inner());
    map.entry("memory").or_insert(open_memory as Opener);
    map.entry("dir").or_insert(open_dir as Opener);
    f(&mut map)
}

/// Register a backend for `scheme` (without the colon). A later
/// registration of the same scheme replaces the earlier one, so a host can
/// substitute a backend in a test.
pub fn register(scheme: &'static str, opener: Opener) {
    with_builtins(|map| {
        map.insert(scheme, opener);
    });
}

/// The schemes this process can open, sorted.
pub fn schemes() -> Vec<&'static str> {
    with_builtins(|map| map.keys().copied().collect())
}

/// Open the backend `url` names: `memory:`, `dir:<path>`, or a registered
/// scheme.
pub fn open(url: &str) -> Result<Box<dyn AttributedStore>, OpenError> {
    let known = || schemes().join(", ");
    let Some((scheme, rest)) = url.split_once(':') else {
        return Err(OpenError::NoScheme {
            url: url.to_string(),
            known: known(),
        });
    };
    if scheme.is_empty() {
        return Err(OpenError::NoScheme {
            url: url.to_string(),
            known: known(),
        });
    }
    let opener = with_builtins(|map| map.get(scheme).copied());
    let Some(opener) = opener else {
        return Err(OpenError::UnknownScheme {
            scheme: scheme.to_string(),
            known: known(),
        });
    };
    opener(rest).map_err(|source| OpenError::Backend {
        url: url.to_string(),
        source,
    })
}

/// A store opened for a host that serves blobs: the same store as two views.
///
/// [`open`] answers `Box<dyn AttributedStore>`, and a trait object cannot be
/// downcast to another trait, so a host that wants to reach
/// [`BlobStore`] could not. `blobs` is `Some` exactly when the backend holds
/// blobs and this crate knows how to hand them out (`memory:` and `dir:`);
/// `store` and `blobs` are the same allocation.
#[derive(Clone)]
pub struct Opened {
    pub store: Arc<dyn AttributedStore>,
    pub blobs: Option<Arc<dyn BlobStore>>,
}

/// [`open`], keeping the blob view. `memory:` and `dir:<path>` open here with
/// both views over one handle; any other registered scheme opens through its
/// [`Opener`] with `blobs: None` (an opener returns a `Box<dyn
/// AttributedStore>`, which has no blob view to recover), so a host asking a
/// registered backend for blobs gets `None` even when the backend's
/// capabilities say `blobs`.
pub fn open_with_blobs(url: &str) -> Result<Opened, OpenError> {
    let backend = |source| OpenError::Backend {
        url: url.to_string(),
        source,
    };
    match url.split_once(':') {
        Some(("memory", _)) => {
            let store = Arc::new(koine_store_memory::MemoryStore::new());
            Ok(Opened {
                store: store.clone(),
                blobs: Some(store),
            })
        }
        Some(("dir", rest)) => {
            if rest.is_empty() {
                return Err(backend(StoreError::Validation(
                    "dir: needs a path, as in dir:/path/to/root".into(),
                )));
            }
            let store = Arc::new(koine_store_dir::DirStore::open(rest).map_err(backend)?);
            Ok(Opened {
                store: store.clone(),
                blobs: Some(store),
            })
        }
        _ => {
            let store: Box<dyn AttributedStore> = open(url)?;
            Ok(Opened {
                store: Arc::from(store),
                blobs: None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_with_blobs_keeps_the_blob_view_of_memory_and_dir() {
        let memory = open_with_blobs("memory:").unwrap();
        let blobs = memory.blobs.clone().expect("memory holds blobs");
        assert!(memory.store.capabilities().blobs);
        // One allocation, two views: a blob is for a row of this store, and
        // a row that is not there is refused by the store, not by the view.
        assert_eq!(
            Arc::as_ptr(&memory.store) as *const (),
            Arc::as_ptr(&blobs) as *const ()
        );
        assert!(matches!(
            blobs.put_blob(uuid::Uuid::new_v4(), "a.txt", b"hi"),
            Err(StoreError::NotFound(_))
        ));

        let root = std::env::temp_dir().join(format!("koine-store-b-{}", uuid::Uuid::new_v4()));
        let dir = open_with_blobs(&format!("dir:{}", root.display())).unwrap();
        assert!(dir.blobs.is_some());
        drop(dir);
        let _ = std::fs::remove_dir_all(root);

        assert!(matches!(
            open_with_blobs("dir:"),
            Err(OpenError::Backend { .. })
        ));
        assert!(matches!(
            open_with_blobs("s3:b"),
            Err(OpenError::UnknownScheme { .. })
        ));
    }

    #[test]
    fn memory_and_dir_are_known_and_open() {
        assert!(schemes().contains(&"memory"));
        assert!(schemes().contains(&"dir"));
        let memory = open("memory:").unwrap();
        assert!(!memory.capabilities().durable);
        let root = std::env::temp_dir().join(format!("koine-store-{}", uuid::Uuid::new_v4()));
        let dir = open(&format!("dir:{}", root.display())).unwrap();
        assert!(dir.capabilities().durable);
        drop(dir);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_missing_or_unknown_scheme_is_named() {
        assert!(matches!(
            open("/just/a/path"),
            Err(OpenError::NoScheme { .. })
        ));
        assert!(matches!(open(":x"), Err(OpenError::NoScheme { .. })));
        let err = open("s3:bucket").err().expect("s3 is unknown");
        assert!(err.to_string().contains("s3"), "{err}");
        assert!(err.to_string().contains("memory"), "{err}");
    }

    #[test]
    fn dir_without_a_path_is_refused_by_the_backend() {
        assert!(matches!(open("dir:"), Err(OpenError::Backend { .. })));
    }

    #[test]
    fn a_registered_scheme_opens_and_a_later_one_replaces_it() {
        fn opener(_: &str) -> Result<Box<dyn AttributedStore>, StoreError> {
            Ok(Box::new(koine_store_memory::MemoryStore::new()))
        }
        register("test-scheme", opener);
        assert!(open("test-scheme:anything").is_ok());
        assert!(schemes().contains(&"test-scheme"));
    }
}
