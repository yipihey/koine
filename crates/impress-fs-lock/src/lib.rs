//! Advisory file locks for the device-local files the suite owns outside the
//! store: `impress-ai`'s worker lease and preferences file, and
//! `impress-settings`'s per-scope settings files (ADR-0036 D5).
//!
//! Lifted out of `impress-ai` by plan-self-reflective-layer R1 (decision
//! D-R8): `impress-settings` is a pure kit crate and may not reach a
//! store-tier crate for sixty lines of `flock`, and copying them would be a
//! second definition. This crate depends on nothing.

#![forbid(unsafe_op_in_unsafe_fn)]

use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::Path;

/// An exclusive `flock` held for the lifetime of the value. The lock file
/// itself stays on disk: the kernel releases the lock on crash, which avoids
/// stale-file deletion races.
#[derive(Debug)]
pub struct FileLock {
    file: File,
}

impl FileLock {
    /// Block until the exclusive lock is acquired.
    pub fn exclusive(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = open(path.as_ref())?;
        lock_exclusive(&file, true)?;
        Ok(Self { file })
    }

    /// Acquire the exclusive lock or fail with `WouldBlock`.
    pub fn try_exclusive(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = open(path.as_ref())?;
        lock_exclusive(&file, false)?;
        Ok(Self { file })
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = unlock(&self.file);
    }
}

fn open(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
}

#[cfg(unix)]
pub fn lock_exclusive(file: &File, blocking: bool) -> io::Result<()> {
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    let operation = if blocking { LOCK_EX } else { LOCK_EX | LOCK_NB };
    // SAFETY: `file` owns a valid descriptor for the duration of the call.
    let result = unsafe { flock(file.as_raw_fd(), operation) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(unix)]
pub fn unlock(file: &File) -> io::Result<()> {
    const LOCK_UN: i32 = 8;
    // SAFETY: `file` owns a valid descriptor for the duration of the call.
    let result = unsafe { flock(file.as_raw_fd(), LOCK_UN) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(unix)]
unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

#[cfg(not(unix))]
pub fn lock_exclusive(_file: &File, _blocking: bool) -> io::Result<()> {
    Err(io::Error::new(
        ErrorKind::Unsupported,
        "advisory file locks require a Unix host",
    ))
}

#[cfg(not(unix))]
pub fn unlock(_file: &File) -> io::Result<()> {
    Ok(())
}

/// Map a `WouldBlock` from `try_exclusive` to a clearer error for callers
/// that report who holds the lock.
pub fn already_locked<'a>(
    path: &'a Path,
    holder: &'a str,
) -> impl FnOnce(io::Error) -> io::Error + 'a {
    move |error| {
        if error.kind() == ErrorKind::WouldBlock {
            io::Error::new(
                ErrorKind::AlreadyExists,
                format!("another {holder} holds {}", path.display()),
            )
        } else {
            error
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_exclusive_is_exclusive_until_dropped() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested").join("x.lock");
        let first = FileLock::try_exclusive(&path).unwrap();
        let error = FileLock::try_exclusive(&path).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::WouldBlock);
        drop(first);
        FileLock::try_exclusive(&path).unwrap();
    }
}
