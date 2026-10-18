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
//! `memory:` and `dir:<path>` are known here.
//!
//! # `dir:` parameters
//!
//! `dir:<path>?k=v&k=v` takes parameters after the **last** `?` of the rest
//! (a path may itself contain a `?`; one that ends in what looks like
//! `?x=y` must be written with a trailing `?`, as `dir:/a/b?x=y?`). Unknown
//! keys are refused, naming the known ones:
//!
//! | key | values | default | meaning |
//! |-----|--------|---------|---------|
//! | `watch` | `1`/`true`/`on`, `0`/`false`/`off` | `1` | start the watcher, so another process's writes reach subscribers and `cross_process_events` is true |
//! | `merge` | `report`, `take-copy`, `keep-file`, `union` | `report` | what to do with a sync-conflict copy of an item (`koine_store_dir::MergePolicy`) |
//!
//! ```
//! # let root = std::env::temp_dir().join(format!("koine-store-doc-{}", std::process::id()));
//! let url = format!("dir:{}?watch=0&merge=union", root.display());
//! let store = koine_store::open(&url).unwrap();
//! assert!(!store.capabilities().cross_process_events);
//! # drop(store); let _ = std::fs::remove_dir_all(root);
//! ```
//!
//! Another backend registers its
//! scheme with [`register`] before the host opens — `sqlite:` is
//! impress-core's to register — and [`schemes`] lists what this process can
//! open.

pub mod once;
pub mod process;
pub mod recording;
pub mod rehearse;

pub use recording::RecordingStore;

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

/// The parameters `dir:` knows, in the order the error names them.
const DIR_PARAMETERS: [&str; 2] = ["watch", "merge"];

/// What `dir:<path>[?k=v&k=v]` asks for.
#[derive(Debug)]
struct DirUrl {
    path: String,
    watch: bool,
    merge: koine_store_dir::MergePolicy,
}

fn parse_flag(key: &str, value: &str) -> Result<bool, StoreError> {
    match value {
        "1" | "true" | "on" | "yes" => Ok(true),
        "0" | "false" | "off" | "no" => Ok(false),
        other => Err(StoreError::Validation(format!(
            "dir: parameter {key}={other}: expected 1 or 0"
        ))),
    }
}

/// Split the part after `dir:` into a path and its parameters: everything
/// after the LAST `?` is `k=v&k=v` (an empty tail, `dir:/a?`, is no parameters).
fn parse_dir(rest: &str) -> Result<DirUrl, StoreError> {
    let (path, query) = match rest.rsplit_once('?') {
        Some((path, query)) => (path, query),
        None => (rest, ""),
    };
    if path.is_empty() {
        return Err(StoreError::Validation(
            "dir: needs a path, as in dir:/path/to/root".into(),
        ));
    }
    let mut url = DirUrl {
        path: path.to_string(),
        watch: true,
        merge: koine_store_dir::MergePolicy::Report,
    };
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "watch" => {
                url.watch = parse_flag(key, value)?;
            }
            "merge" => {
                url.merge = match value {
                    "report" => koine_store_dir::MergePolicy::Report,
                    "take-copy" => koine_store_dir::MergePolicy::TakeCopy,
                    "keep-file" => koine_store_dir::MergePolicy::KeepFile,
                    "union" => koine_store_dir::MergePolicy::Union,
                    other => {
                        return Err(StoreError::Validation(format!(
                            "dir: parameter merge={other}: expected report, take-copy, keep-file or union"
                        )))
                    }
                };
            }
            other => {
                return Err(StoreError::Validation(format!(
                    "dir: unknown parameter '{other}'; known: {}",
                    DIR_PARAMETERS.join(", ")
                )))
            }
        }
    }
    Ok(url)
}

/// Open the `dir:` store a URL names: the one function `open` and
/// `open_with_blobs` both use.
fn open_dir_store(rest: &str) -> Result<koine_store_dir::DirStore, StoreError> {
    let url = parse_dir(rest)?;
    let store = koine_store_dir::DirStore::open_with(&url.path, url.merge)?;
    if url.watch {
        store.watch()?;
    }
    Ok(store)
}

fn open_dir(rest: &str) -> Result<Box<dyn AttributedStore>, StoreError> {
    Ok(Box::new(open_dir_store(rest)?))
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
            let store = Arc::new(open_dir_store(rest).map_err(backend)?);
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

    fn scratch(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("koine-store-{tag}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn dir_urls_start_the_watcher_unless_told_not_to() {
        let root = scratch("watch");
        let store = open(&format!("dir:{}", root.display())).unwrap();
        // the watcher's first scan is on its own thread
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !store.capabilities().cross_process_events {
            assert!(std::time::Instant::now() < deadline, "the watcher started");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        drop(store);

        let off = open(&format!("dir:{}?watch=0", root.display())).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(!off.capabilities().cross_process_events);
        drop(off);
        let blobs =
            open_with_blobs(&format!("dir:{}?watch=off&merge=union", root.display())).unwrap();
        assert!(blobs.blobs.is_some() && !blobs.store.capabilities().cross_process_events);
        drop(blobs);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn dir_parameters_are_parsed_after_the_last_question_mark() {
        let url = parse_dir("/a/b?c/d?watch=0&merge=take-copy").unwrap();
        assert_eq!(url.path, "/a/b?c/d");
        assert!(!url.watch);
        assert_eq!(url.merge.name(), "take-copy");
        // a path that ends in what looks like parameters: end it with `?`
        let url = parse_dir("/a/b?x=y?").unwrap();
        assert_eq!(url.path, "/a/b?x=y");
        assert!(url.watch);
        assert_eq!(
            parse_dir("/a?merge=keep-file&watch=1")
                .unwrap()
                .merge
                .name(),
            "keep-file"
        );
        assert_eq!(parse_dir("/a?merge=report").unwrap().merge.name(), "report");
        assert_eq!(parse_dir("/a?merge=union").unwrap().merge.name(), "union");
    }

    #[test]
    fn unknown_or_malformed_dir_parameters_are_validation_errors() {
        for (rest, expect) in [
            ("/a?bogus=1", "known: watch, merge"),
            ("/a?watch=maybe", "expected 1 or 0"),
            ("/a?merge=sideways", "expected report, take-copy"),
            ("?watch=0", "needs a path"),
        ] {
            match parse_dir(rest) {
                Err(StoreError::Validation(message)) => {
                    assert!(message.contains(expect), "{rest}: {message}")
                }
                other => panic!("{rest}: {other:?}"),
            }
        }
        assert!(matches!(
            open("dir:/tmp/never-created?nope=1"),
            Err(OpenError::Backend { .. })
        ));
        assert!(!std::path::Path::new("/tmp/never-created").exists());
    }

    #[test]
    fn a_merge_parameter_resolves_a_conflict_at_open() {
        use impress_item::ItemStore;
        let root = scratch("merge");
        let id = {
            let store = koine_store_dir::DirStore::open(&root).unwrap();
            let id = uuid::Uuid::new_v4();
            let now = chrono_free_item(id);
            store.insert(now).unwrap();
            id
        };
        let dir = root.join("items").join("paper");
        let file = dir.join(format!("{id}.json"));
        let mut doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        doc["logical_clock"] = (doc["logical_clock"].as_u64().unwrap() + 1000).into();
        doc["payload"]["title"] = "theirs".into();
        std::fs::write(
            dir.join(format!("{id}.conflict-1.json")),
            serde_json::to_vec(&doc).unwrap(),
        )
        .unwrap();

        let kept = open(&format!("dir:{}?watch=0", root.display())).unwrap();
        let title =
            |store: &dyn AttributedStore| store.get(id).unwrap().unwrap().payload["title"].clone();
        assert_eq!(title(kept.as_ref()), port::Value::String("mine".into()));
        drop(kept);
        let merged = open(&format!("dir:{}?watch=0&merge=take-copy", root.display())).unwrap();
        assert_eq!(title(merged.as_ref()), port::Value::String("theirs".into()));
        assert!(root
            .join("conflicts")
            .join(format!("{id}.conflict-1.json"))
            .is_file());
        drop(merged);
        let _ = std::fs::remove_dir_all(root);
    }

    fn chrono_free_item(id: uuid::Uuid) -> port::Item {
        let doc = serde_json::json!({
            "id": id, "schema": "paper", "payload": {"title": "mine"},
            "created": "2026-01-01T00:00:00Z", "modified": "2026-01-01T00:00:00Z",
            "author": "t", "author_kind": "Human", "logical_clock": 0,
            "origin": null, "canonical_id": null, "tags": [], "flag": null,
            "is_read": false, "is_starred": false, "priority": "normal",
            "visibility": "private", "message_type": null, "produced_by": null,
            "version": null, "batch_id": null, "references": [], "parent": null
        });
        serde_json::from_value(doc).unwrap()
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
