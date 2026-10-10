//! The native writer for `<workspace>/settings/koine-style.json`.
//!
//! The document — `wire_version`, the `frontends` map, and the refusal of a
//! bad slot — is [`koine::style_file`]. This crate locks that path with
//! `impress-fs-lock` and, with no `IMPRESS_WORKSPACE`, defaults to the suite's
//! group container. A host with no filesystem parses the bytes with `koine`
//! and does not link this crate.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use impress_fs_lock::FileLock;
use koine::style::StyleBook;
use koine::style_file;

pub use koine::style_file::{
    book_from_slice, book_to_value, style_path, StyleFileError as DocumentError, STYLE_FILE_NAME,
    WIRE_VERSION,
};

/// Why the style file could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum StyleFileError {
    #[error("{0}")]
    Document(#[from] style_file::StyleFileError),
    #[error("style file: {0}")]
    Io(#[from] io::Error),
}

/// `IMPRESS_WORKSPACE`, or the suite group container's `workspace` directory.
pub fn default_workspace() -> PathBuf {
    if let Some(over) = std::env::var_os("IMPRESS_WORKSPACE") {
        return PathBuf::from(over);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("Library/Group Containers/QG3MEYVHMS.com.impress.suite/workspace")
}

/// Read the book at `style_path(workspace)`. A missing file is an empty book,
/// which opens as the defaults.
pub fn load_book(workspace: impl AsRef<Path>) -> Result<StyleBook, StyleFileError> {
    let path = style_path(workspace);
    let _lock = lock(&path)?;
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(StyleBook::new()),
        Err(error) => return Err(error.into()),
    };
    Ok(style_file::book_from_slice(&bytes)?)
}

/// Write every declared frontend. Slots the book has not touched stay at
/// their defaults, so the file is the whole sheet.
pub fn save_book(workspace: impl AsRef<Path>, book: &StyleBook) -> Result<(), StyleFileError> {
    let path = style_path(workspace);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let _lock = lock(&path)?;
    let value = style_file::book_to_value(book)?;
    let mut bytes = serde_json::to_vec_pretty(&value).map_err(style_file::StyleFileError::Json)?;
    bytes.push(b'\n');
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    {
        let mut file = File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, &path)?;
    Ok(())
}

fn lock(path: &Path) -> io::Result<FileLock> {
    let lock_path = PathBuf::from(format!("{}.lock", path.display()));
    FileLock::exclusive(lock_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_the_defaults_and_a_saved_slot_comes_back() {
        let dir =
            std::env::temp_dir().join(format!("koine-style-file-roundtrip-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let loaded = load_book(&dir).unwrap();
        assert!(loaded.sheet("swift").is_none());

        let mut book = StyleBook::new();
        book.set("swift", "button", "accent").unwrap();
        book.set("swift", "text", "title").unwrap();
        book.set("tui", "button", "[{label}]").unwrap();
        save_book(&dir, &book).unwrap();

        let again = load_book(&dir).unwrap();
        assert_eq!(again.sheet("swift").unwrap().value("button"), "accent");
        assert_eq!(again.sheet("swift").unwrap().value("text"), "title");
        assert_eq!(again.sheet("swift").unwrap().value("column"), "8");
        assert_eq!(again.sheet("tui").unwrap().value("button"), "[{label}]");
        let _ = fs::remove_dir_all(&dir);
    }
}
