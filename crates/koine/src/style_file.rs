//! The style document: `<workspace>/settings/koine-style.json`.
//!
//! This module names the file and reads its bytes. It does not open them.
//! A missing file is the defaults, decided by the host that finds nothing
//! to read. A document `set` would refuse is refused whole. Writing the
//! file, and locking it, is `koine-style-file`: a native host. A frontend
//! with no filesystem parses the same bytes here.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::style::{StyleBook, StyleError, FRONTENDS};

pub const STYLE_FILE_NAME: &str = "koine-style.json";
pub const WIRE_VERSION: u32 = 1;

/// `<workspace>/settings/koine-style.json`.
pub fn style_path(workspace: impl AsRef<Path>) -> PathBuf {
    workspace.as_ref().join("settings").join(STYLE_FILE_NAME)
}

/// Why a style document could not be read.
#[derive(Debug, thiserror::Error)]
pub enum StyleFileError {
    #[error("{0}")]
    Style(#[from] StyleError),
    #[error("style file wire_version is {found}, and this build reads {WIRE_VERSION}")]
    Version { found: u32 },
    #[error("style file: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Serialize, Deserialize)]
struct StyleFile {
    wire_version: u32,
    frontends: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
}

/// The book in `bytes`. A document that names an unknown slot, the wrong
/// kind, or another `wire_version` is refused whole.
pub fn book_from_slice(bytes: &[u8]) -> Result<StyleBook, StyleFileError> {
    let file: StyleFile = serde_json::from_slice(bytes)?;
    if file.wire_version != WIRE_VERSION {
        return Err(StyleFileError::Version {
            found: file.wire_version,
        });
    }
    let mut book = StyleBook::new();
    for (frontend, values) in file.frontends {
        for (slot, value) in values {
            book.set(&frontend, &slot, &value)?;
        }
    }
    Ok(book)
}

/// Pretty JSON of a book, for a host that wants the bytes without writing.
pub fn book_to_value(book: &StyleBook) -> Result<Value, StyleFileError> {
    Ok(serde_json::to_value(file_from_book(book)?)?)
}

fn file_from_book(book: &StyleBook) -> Result<StyleFile, StyleFileError> {
    let mut frontends = std::collections::BTreeMap::new();
    for frontend in FRONTENDS {
        let sheet = match book.sheet(frontend) {
            Some(sheet) => sheet.clone(),
            None => crate::style::StyleSheet::default_for(frontend)?,
        };
        frontends.insert(frontend.to_string(), sheet.values().clone());
    }
    Ok(StyleFile {
        wire_version: WIRE_VERSION,
        frontends,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bad_slot_refuses_the_whole_file() {
        let bytes = br#"{ "wire_version": 1, "frontends": { "swift": { "grid": "8" } } }"#;
        assert!(book_from_slice(bytes).is_err());
        let wrong =
            br#"{ "wire_version": 1, "frontends": { "swift": { "button": "not-a-token" } } }"#;
        assert!(book_from_slice(wrong).is_err());
        let version = br#"{ "wire_version": 2, "frontends": {} }"#;
        assert!(matches!(
            book_from_slice(version),
            Err(StyleFileError::Version { found: 2 })
        ));
    }

    #[test]
    fn the_committed_partial_file_fills_the_defaults() {
        let bytes = include_bytes!("../tests/fixtures/style-file.json");
        let book = book_from_slice(bytes).unwrap();
        let swift = book.sheet("swift").unwrap();
        assert_eq!(swift.value("button"), "accent");
        assert_eq!(swift.value("text"), "title");
        assert_eq!(swift.value("column"), "8");
        assert_eq!(swift.value("focus"), "automatic");
        assert_eq!(book.sheet("tui").unwrap().value("button"), "[{label}]");
        assert_eq!(book.sheet("tui").unwrap().value("focus"), ">");
    }

    #[test]
    fn a_book_round_trips_through_the_document() {
        let mut book = StyleBook::new();
        book.set("swift", "button", "accent").unwrap();
        let value = book_to_value(&book).unwrap();
        let bytes = serde_json::to_vec(&value).unwrap();
        let again = book_from_slice(&bytes).unwrap();
        assert_eq!(again.sheet("swift").unwrap().value("button"), "accent");
        assert_eq!(again.sheet("swift").unwrap().value("column"), "8");
        assert_eq!(again.sheet("tui").unwrap().value("focus"), ">");
    }
}
