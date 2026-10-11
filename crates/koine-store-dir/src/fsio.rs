//! The few filesystem primitives the store is built from.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use impress_item::{ItemId, StoreError};
use uuid::Uuid;

/// A `StoreError::Storage` naming what was being done.
pub(crate) fn storage(
    context: impl std::fmt::Display,
    error: impl std::fmt::Display,
) -> StoreError {
    StoreError::Storage(format!("{context}: {error}"))
}

/// Write `bytes` to `path` as temp-and-rename in the same directory, so a
/// reader (or a crash) sees the old file or the whole new one, never half.
/// The temporary starts with `.`, which every listing here skips.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temporary = parent.join(format!(".{name}.tmp-{}", Uuid::new_v4().simple()));
    if let Err(error) = fs::write(&temporary, bytes) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    fs::rename(&temporary, path).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })
}

/// `remove_file` where a file that is already gone is success (replaying a
/// journal removes what a crashed run may already have removed).
pub(crate) fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// The entries of `dir` as `(file name, is a directory)`; a missing
/// directory is an empty one.
pub(crate) fn list_dir(dir: &Path) -> Result<Vec<(String, bool)>, StoreError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(storage(format!("list {}", dir.display()), error)),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| storage(format!("list {}", dir.display()), e))?;
        let is_dir = entry
            .file_type()
            .map_err(|e| storage(format!("stat {}", entry.path().display()), e))?
            .is_dir();
        out.push((entry.file_name().to_string_lossy().into_owned(), is_dir));
    }
    out.sort();
    Ok(out)
}

/// `<uuid>.json` -> the id. Anything else (a temporary, a conflict copy, a
/// stray file) is not a record.
pub(crate) fn record_id(file_name: &str) -> Option<ItemId> {
    let stem = file_name.strip_suffix(".json")?;
    if stem.len() != 36 {
        return None;
    }
    let id = Uuid::parse_str(stem).ok()?;
    (id.to_string() == stem).then_some(id)
}

/// `<uuid>.conflict-<n>.json` -> the id the copy belongs to.
pub(crate) fn conflict_copy_id(file_name: &str) -> Option<ItemId> {
    let stem = file_name.strip_suffix(".json")?;
    let (id, n) = stem.split_once(".conflict-")?;
    if n.is_empty() || !n.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    record_id(&format!("{id}.json"))
}

/// Every directory under `root` (not `root` itself), depth first.
pub(crate) fn dirs_below(root: &Path) -> Result<Vec<PathBuf>, StoreError> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for (name, is_dir) in list_dir(&dir)? {
            if is_dir && !name.starts_with('.') {
                let child = dir.join(name);
                stack.push(child.clone());
                out.push(child);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// A relative `/`-separated path made only of plain components: the shape of
/// every path a journal may name. A journal arrives by rsync like any other
/// file, so its paths are not trusted.
pub(crate) fn safe_relative(path: &str) -> Result<PathBuf, StoreError> {
    let mut out = PathBuf::new();
    for segment in path.split('/') {
        if segment.is_empty()
            || segment == "."
            || segment == ".."
            || segment.contains('\\')
            || segment.contains('\0')
        {
            return Err(StoreError::Storage(format!(
                "unsafe path in journal: {path:?}"
            )));
        }
        out.push(segment);
    }
    match path.split('/').next() {
        Some("items") | Some("operations") => Ok(out),
        _ => Err(StoreError::Storage(format!(
            "journal path outside items/ and operations/: {path:?}"
        ))),
    }
}
