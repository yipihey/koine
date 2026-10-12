//! The per-launch loopback token every automation server requires on a
//! mutating request (plan verb-pipeline P0, SEC-2).
//!
//! Loopback used to mean trusted: any process on the machine — a browser tab
//! after DNS rebinding, a script the user never wrote — could `POST` to every
//! app's automation port with no credential at all. Now each app mints a
//! random token at launch, writes it where only this user can read it, and
//! demands it as `Authorization: Bearer <token>` on every request that is not
//! `GET`/`HEAD`/`OPTIONS` from loopback. `GET` stays token-free on loopback
//! (the read-only routes), so `curl http://localhost:23120/api/logs` still
//! works exactly as before.
//!
//! **This module is the contract.** Both halves of the suite map onto it:
//! Swift (`ImpressAutomation`) calls it through the UniFFI export in
//! `impress-store-ffi` to decide where to write and what to write; the shared
//! `impress-app-transport` client and Tier B runners call [`client_token`]
//! to find what to send. Neither side
//! spells the path, the file name or the format on its own.
//!
//! ## The convention
//!
//! * **Directory:** `<suite app-group container>/workspace/automation/`, the
//!   one directory every suite app and every headless process of this user
//!   can read — the same container `workspace/impress.sqlite` lives in. The
//!   directory is created `0700`.
//! * **File name:** `loopback-<port>.token`, keyed by the **port the server
//!   bound**, not the app's name. A second instance of an app under test
//!   (`-httpAutomationPort 23181`, the documented way to run a branch build
//!   beside the user's) must not overwrite the running app's token, and a
//!   client that knows only a base URL — the Tier B runners take one from
//!   `IMPRESS_LAYOUT_SELFTEST_BASE_URL` — must be able to find the file
//!   without a second lookup. The port is the one thing both sides know.
//! * **Contents:** the token, one line, UTF-8, trailing newline. The token is
//!   64 lowercase hex characters (256 bits). Readers trim whitespace.
//! * **Mode:** `0600`, written to a sibling temp file and renamed into place so
//!   a reader never sees a partial token.
//! * **Override:** `IMPRESS_APP_TOKEN`, when set and non-empty, wins over the
//!   file in every client. It is the same variable a remote (non-loopback)
//!   client sets to the app's network bearer, so one name covers both cases.
//!
//! The file is per launch: a fresh token on every start, and a caller that
//! cached the old one gets 401 and re-reads. The app removes the file when it
//! stops its server; a stale file from a crash is harmless (the port is
//! closed, or a new launch overwrote it).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The environment variable a client may set instead of reading the file.
pub const TOKEN_ENV: &str = "IMPRESS_APP_TOKEN";

/// The directory under the app-group container that holds the token files.
pub const TOKEN_DIR: &str = "workspace/automation";

/// The suite app group's container id on macOS (team-prefixed; the same
/// literal `impress-store-service`, `imbib-service` and Swift's
/// `SiblingDiscovery.suiteGroupID` use). Only [`default_container_root`]
/// spells it; a sandboxed app passes its own container URL instead.
const SUITE_GROUP: &str = "QG3MEYVHMS.com.impress.suite";

/// The file name for the server bound to `port`.
pub fn token_file_name(port: u16) -> String {
    format!("loopback-{port}.token")
}

/// `<container_root>/workspace/automation/loopback-<port>.token`.
pub fn token_path_under(container_root: &Path, port: u16) -> PathBuf {
    container_root.join(TOKEN_DIR).join(token_file_name(port))
}

/// The suite app-group container as a headless process finds it:
/// `~/Library/Group Containers/QG3MEYVHMS.com.impress.suite`, with `HOME`
/// unwrapped from a sandbox container path when it points into one. `None`
/// when there is no home directory at all.
pub fn default_container_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)?;
    Some(
        real_home(&home)
            .join("Library")
            .join("Group Containers")
            .join(SUITE_GROUP),
    )
}

/// The token file for `port` under [`default_container_root`].
pub fn default_token_path(port: u16) -> Option<PathBuf> {
    default_container_root().map(|root| token_path_under(&root, port))
}

/// A fresh 256-bit token as 64 lowercase hex characters.
pub fn generate() -> String {
    // Two v4 UUIDs are 32 random bytes from the OS CSPRNG; `simple()` renders
    // each as 32 hex digits with no separators.
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Write `token` to `path` in the contract's format and mode, creating the
/// directory (`0700`) when needed. Atomic: written to a sibling temp file and
/// renamed over `path`.
pub fn write(path: &Path, token: &str) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "token path has no parent"))?;
    fs::create_dir_all(dir)?;
    set_mode(dir, 0o700)?;
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("loopback"),
        std::process::id()
    ));
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        use io::Write as _;
        file.write_all(token.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    // The mode above applies only on creation; an existing temp file from an
    // earlier crash keeps its old mode, so set it again.
    set_mode(&tmp, 0o600)?;
    fs::rename(&tmp, path)
}

/// Mint a token, write it for `port` under `container_root`, and return the
/// token with the path it was written to.
pub fn install(container_root: &Path, port: u16) -> io::Result<Installed> {
    let path = token_path_under(container_root, port);
    let token = generate();
    write(&path, &token)?;
    Ok(Installed { path, token })
}

/// What [`install`] produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub path: PathBuf,
    pub token: String,
}

/// Remove the token file for `port` under `container_root`. A missing file
/// is not an error: the server is stopping and the goal is "no file".
pub fn remove(container_root: &Path, port: u16) -> io::Result<()> {
    match fs::remove_file(token_path_under(container_root, port)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Read the token at `path`: trimmed, `None` when the file is missing or
/// empty. Any other I/O error is returned.
pub fn read(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            let token = contents.trim();
            Ok((!token.is_empty()).then(|| token.to_string()))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// The token a client should send to the server on `port`: `IMPRESS_APP_TOKEN`
/// when set, else the token file under the default container, else `None`
/// (the client sends nothing and a mutating request will be refused 401 —
/// which is the right failure, not a silent one).
pub fn client_token(port: u16) -> Option<String> {
    if let Some(token) = std::env::var(TOKEN_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return Some(token);
    }
    let path = default_token_path(port)?;
    read(&path).ok().flatten()
}

/// [`client_token`] for a base URL: the URL's port, or the scheme's default
/// when it names none. Malformed input yields the env override only.
pub fn client_token_for_url(base_url: &str) -> Option<String> {
    match port_of_url(base_url) {
        Some(port) => client_token(port),
        None => std::env::var(TOKEN_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty()),
    }
}

/// The port a base URL names, with `http` → 80 and `https` → 443 when none
/// is spelled. Handles `[::1]:port` and a trailing path.
pub fn port_of_url(base_url: &str) -> Option<u16> {
    let (scheme, rest) = base_url.trim().split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit('@').next()?;
    let port_text = if let Some(bracket_end) = host_port.find(']') {
        host_port[bracket_end + 1..].strip_prefix(':')
    } else {
        host_port.rsplit_once(':').map(|(_, port)| port)
    };
    match port_text {
        Some(port) => port.parse().ok(),
        None => match scheme {
            "http" => Some(80),
            "https" => Some(443),
            _ => None,
        },
    }
}

/// Strip a `/Library/Containers/<bundle>/Data` suffix, so a sandboxed
/// process's `HOME` yields the user's real home (the same rule
/// `imbib_core::eink::paths::real_home` applies).
fn real_home(home: &Path) -> PathBuf {
    let components: Vec<&std::ffi::OsStr> = home.iter().collect();
    if let Some(index) = components.windows(4).position(|window| {
        window[0] == "Library" && window[1] == "Containers" && window[3] == "Data"
    }) {
        let mut trimmed = PathBuf::new();
        for component in &components[..index] {
            trimmed.push(component);
        }
        return trimmed;
    }
    home.to_path_buf()
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "impress-loopback-token-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn path_is_the_documented_convention() {
        let root = Path::new("/tmp/container");
        assert_eq!(
            token_path_under(root, 23120),
            PathBuf::from("/tmp/container/workspace/automation/loopback-23120.token")
        );
    }

    #[test]
    fn generated_tokens_are_64_hex_and_unique() {
        let a = generate();
        let b = generate();
        assert_eq!(a.len(), 64);
        assert!(a
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_ne!(a, b);
    }

    #[test]
    fn install_writes_0600_one_line_and_read_trims() {
        let root = scratch();
        let installed = install(&root, 23999).unwrap();
        assert_eq!(installed.path, token_path_under(&root, 23999));
        let raw = fs::read_to_string(&installed.path).unwrap();
        assert_eq!(raw, format!("{}\n", installed.token));
        assert_eq!(
            read(&installed.path).unwrap(),
            Some(installed.token.clone())
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&installed.path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
            let dir_mode = fs::metadata(installed.path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dir_mode, 0o700);
        }
        // A second install replaces the token; a remove leaves no file and a
        // second remove is not an error.
        let again = install(&root, 23999).unwrap();
        assert_ne!(again.token, installed.token);
        remove(&root, 23999).unwrap();
        assert_eq!(read(&installed.path).unwrap(), None);
        remove(&root, 23999).unwrap();
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn read_missing_is_none_and_empty_is_none() {
        let root = scratch();
        assert_eq!(read(&root.join("nope")).unwrap(), None);
        let empty = root.join("empty");
        fs::write(&empty, "  \n").unwrap();
        assert_eq!(read(&empty).unwrap(), None);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn port_of_url_covers_the_spellings_clients_use() {
        assert_eq!(port_of_url("http://127.0.0.1:23261"), Some(23261));
        assert_eq!(port_of_url("http://localhost:23120/"), Some(23120));
        assert_eq!(port_of_url("http://[::1]:23125/api/status"), Some(23125));
        assert_eq!(port_of_url("http://localhost"), Some(80));
        assert_eq!(port_of_url("https://example.test/x"), Some(443));
        assert_eq!(port_of_url("not a url"), None);
    }

    #[test]
    fn real_home_unwraps_a_sandbox_container() {
        assert_eq!(
            real_home(Path::new(
                "/Users/me/Library/Containers/com.impress.imbib/Data"
            )),
            PathBuf::from("/Users/me")
        );
        assert_eq!(
            real_home(Path::new("/Users/me")),
            PathBuf::from("/Users/me")
        );
    }
}
