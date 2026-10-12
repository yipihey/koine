//! A file a verb is handed: [`BlobRef`], [`BlobArg`] and the process's blob
//! slot (plan-koine-live L-D6).
//!
//! A surface's `file` field uploads the chosen file to the host's blob store
//! (`PUT /api/blob/<surface id>/<name>`) and then sends the field's value, a
//! blob reference. A verb that takes a file declares that argument as
//! [`BlobArg`] (the wire shape the form sends, `{"blob": {"item", "name"}}`)
//! and calls [`BlobRef::read`] for the bytes:
//!
//! ```ignore
//! async fn import(&self, file: BlobArg) -> Result<Imported, Refusal> {
//!     let bytes = file.read().map_err(|e| Refusal::new("blob-unavailable", e.to_string()))?;
//!     // …
//! }
//! ```
//!
//! This crate sits below the store, so it cannot name one: the host installs a
//! [`BlobReader`] ([`install_blob_reader`]; `koine_store::process::install_blobs`
//! does it from a `BlobStore`), the same process-seam shape as the runner's
//! and the materialiser's. A bare CLI or MCP process installs none, and
//! [`BlobRef::read`] says [`BlobError::NotInstalled`].

use std::sync::{Arc, RwLock};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Which blob: the item it is kept beside (a uuid, as text) and its name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct BlobRef {
    /// The item the blob is kept beside, as uuid text. For a `file` field this
    /// is the surface's id.
    pub item: String,
    /// The blob's name under that item: the file name the page uploaded.
    pub name: String,
}

/// The argument a verb writes for a `file` field: `{"blob": {"item", "name"}}`,
/// which is the value the form sends. Use this as the argument type; take
/// [`BlobRef`] out of it with `.blob` or `BlobRef::from`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct BlobArg {
    /// The reference to the uploaded file.
    pub blob: BlobRef,
}

impl From<BlobArg> for BlobRef {
    fn from(arg: BlobArg) -> Self {
        arg.blob
    }
}

impl From<BlobRef> for BlobArg {
    fn from(blob: BlobRef) -> Self {
        BlobArg { blob }
    }
}

impl BlobArg {
    /// [`BlobRef::read`] on the wrapped reference.
    pub fn read(&self) -> Result<Vec<u8>, BlobError> {
        self.blob.read()
    }
}

/// Why a blob could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BlobError {
    /// No host installed a blob reader: this process is not a koine host (a
    /// bare CLI or MCP process) or its store holds no blobs.
    #[error(
        "no blob store is installed in this process; a verb that takes a file runs inside a \
         koine host over a store that holds blobs (memory: or dir:), not from a bare CLI or MCP \
         process"
    )]
    NotInstalled,
    /// The store holds no blob of that name under that item.
    #[error(
        "no blob `{name}` under item {item}; upload the file again (PUT /api/blob/{item}/{name})"
    )]
    NotFound {
        /// The item.
        item: String,
        /// The blob name.
        name: String,
    },
    /// The store failed.
    #[error("the blob store failed: {0}")]
    Store(String),
}

/// How the process reads a blob. The host installs one over its store.
pub trait BlobReader: Send + Sync {
    /// The bytes of blob `name` under `item` (uuid text); `Ok(None)` when
    /// there is no such blob, `Err` when the store failed.
    fn read(&self, item: &str, name: &str) -> Result<Option<Vec<u8>>, String>;
}

static READER: RwLock<Option<Arc<dyn BlobReader>>> = RwLock::new(None);

/// Install the process's blob reader. A later install replaces the earlier.
pub fn install_blob_reader(reader: Arc<dyn BlobReader>) {
    *READER.write().unwrap_or_else(|e| e.into_inner()) = Some(reader);
}

/// The installed blob reader, if any.
pub fn installed_blob_reader() -> Option<Arc<dyn BlobReader>> {
    READER.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Remove the installed reader (tests).
pub fn clear_blob_reader() {
    *READER.write().unwrap_or_else(|e| e.into_inner()) = None;
}

impl BlobRef {
    /// The blob's bytes, through the process's blob reader.
    pub fn read(&self) -> Result<Vec<u8>, BlobError> {
        let reader = installed_blob_reader().ok_or(BlobError::NotInstalled)?;
        match reader.read(&self.item, &self.name) {
            Ok(Some(bytes)) => Ok(bytes),
            Ok(None) => Err(BlobError::NotFound {
                item: self.item.clone(),
                name: self.name.clone(),
            }),
            Err(message) => Err(BlobError::Store(message)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    // The slot is process-wide; these cases take turns.
    static TURN: Mutex<()> = Mutex::new(());

    struct Fixed;
    impl BlobReader for Fixed {
        fn read(&self, item: &str, name: &str) -> Result<Option<Vec<u8>>, String> {
            match (item, name) {
                ("i1", "a.csv") => Ok(Some(b"x,y\n1,2\n".to_vec())),
                ("i1", "boom") => Err("disk on fire".into()),
                _ => Ok(None),
            }
        }
    }

    fn blob(item: &str, name: &str) -> BlobRef {
        BlobRef {
            item: item.into(),
            name: name.into(),
        }
    }

    #[test]
    fn the_form_value_deserialises_as_a_blob_arg() {
        let arg: BlobArg =
            serde_json::from_value(json!({ "blob": { "item": "i1", "name": "a.csv" } })).unwrap();
        assert_eq!(arg.blob, blob("i1", "a.csv"));
        assert_eq!(
            serde_json::to_value(&arg).unwrap(),
            json!({ "blob": { "item": "i1", "name": "a.csv" } })
        );
        assert_eq!(BlobRef::from(arg), blob("i1", "a.csv"));
    }

    #[test]
    fn a_bare_blob_ref_is_item_and_name() {
        let r: BlobRef = serde_json::from_value(json!({ "item": "i1", "name": "n" })).unwrap();
        assert_eq!(r, blob("i1", "n"));
        assert!(serde_json::from_value::<BlobRef>(json!({ "item": "i1" })).is_err());
    }

    #[test]
    fn the_schemas_name_the_fields() {
        let schema = serde_json::to_value(schemars::schema_for!(BlobArg)).unwrap();
        let text = schema.to_string();
        assert!(
            text.contains("\"blob\"") && text.contains("\"item\"") && text.contains("\"name\"")
        );
    }

    #[test]
    fn read_answers_the_bytes_or_says_what_is_wrong() {
        let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        clear_blob_reader();
        let err = blob("i1", "a.csv").read().unwrap_err();
        assert_eq!(err, BlobError::NotInstalled);
        assert!(err.to_string().contains("koine host"));

        install_blob_reader(Arc::new(Fixed));
        assert_eq!(blob("i1", "a.csv").read().unwrap(), b"x,y\n1,2\n");
        assert_eq!(
            BlobArg::from(blob("i1", "a.csv")).read().unwrap(),
            b"x,y\n1,2\n"
        );
        let missing = blob("i1", "gone.csv").read().unwrap_err();
        assert!(matches!(missing, BlobError::NotFound { .. }));
        assert!(missing.to_string().contains("upload the file again"));
        assert_eq!(
            blob("i1", "boom").read().unwrap_err(),
            BlobError::Store("disk on fire".into())
        );
        clear_blob_reader();
    }
}
