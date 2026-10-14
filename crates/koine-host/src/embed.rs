//! The host inside an app (plan-koine-mobile M-D1, M-D3).
//!
//! A program whose main thread belongs to a window toolkit (a Dioxus
//! `mobile` app, a Tauri shell) cannot `block_on` the host. [`start`] gives
//! the host a multi-thread tokio runtime of its own on a background thread
//! and returns once the socket is bound, so [`Embedded::url`] and
//! [`Embedded::token`] are real the moment the caller has them. Dropping the
//! [`Embedded`] (or [`Embedded::shutdown`]) stops the host and joins the
//! thread.
//!
//! ```no_run
//! let host = koine_host::embed::start(koine_host::embed::sandbox_config("my-app"))?;
//! println!("{} (token in {})", host.url(), host.token_path().display());
//! // ... hand the main thread to the window toolkit ...
//! host.shutdown();
//! # Ok::<(), koine_host::HostError>(())
//! ```
//!
//! The caller links its own services (and calls their `ensure_linked()`)
//! before [`start`], exactly as a `<name>-host` binary does before
//! [`crate::cli::main`]; [`start`] anchors the two runtimes the library owns.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;

use tokio::sync::oneshot;

use crate::{ensure_linked, serve, Config, HostError};

/// A host serving on its own runtime thread.
pub struct Embedded {
    addr: SocketAddr,
    url: String,
    token: String,
    token_path: PathBuf,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Embedded {
    /// `http://<addr>`.
    pub fn url(&self) -> String {
        self.url.clone()
    }

    /// This launch's loopback token (see [`crate::Served::token`]).
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The address actually bound.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Where the token was written.
    pub fn token_path(&self) -> &Path {
        &self.token_path
    }

    /// Stop the host and join its runtime thread. Dropping does the same.
    pub fn shutdown(self) {}
}

impl Drop for Embedded {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What the runtime thread tells the caller once the host is bound.
struct Bound {
    addr: SocketAddr,
    url: String,
    token: String,
    token_path: PathBuf,
}

/// Serve `config` on a new multi-thread tokio runtime owned by a background
/// thread; returns once the socket is bound, or with the serve error.
pub fn start(config: Config) -> Result<Embedded, HostError> {
    ensure_linked();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<Bound, HostError>>();
    let (stop, stopped) = oneshot::channel::<()>();
    let thread = std::thread::Builder::new()
        .name(format!("koine-host:{}", config.app_id))
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = ready_tx.send(Err(HostError::Config(format!(
                        "cannot start the async runtime: {error}"
                    ))));
                    return;
                }
            };
            let mut served = match runtime.block_on(serve(config)) {
                Ok(served) => served,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(Bound {
                addr: served.addr(),
                url: served.url(),
                token: served.token().to_string(),
                token_path: served.token_path().to_path_buf(),
            }));
            runtime.block_on(async {
                tokio::select! {
                    _ = stopped => {}
                    _ = served.wait() => {}
                }
                served.shutdown().await;
            });
        })
        .map_err(|error| HostError::Config(format!("cannot start the host thread: {error}")))?;

    match ready_rx.recv() {
        Ok(Ok(bound)) => Ok(Embedded {
            addr: bound.addr,
            url: bound.url,
            token: bound.token,
            token_path: bound.token_path,
            stop: Some(stop),
            thread: Some(thread),
        }),
        Ok(Err(error)) => {
            let _ = thread.join();
            Err(error)
        }
        Err(_) => {
            let _ = thread.join();
            Err(HostError::Config(
                "the host thread ended before it was bound".into(),
            ))
        }
    }
}

/// The config an app on a phone uses (M-D3): workspace
/// `<home>/Documents/<app_id>`, store `dir:<workspace>/store`, loopback, an
/// ephemeral port. `home` is `$HOME` (the app's sandbox on iOS; the system
/// temp directory if it is unset).
pub fn sandbox_config(app_id: &str) -> Config {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    sandbox_config_in(app_id, &home)
}

/// [`sandbox_config`] under an explicit `home`, for tests.
pub fn sandbox_config_in(app_id: &str, home: &Path) -> Config {
    let workspace = home.join("Documents").join(app_id);
    let mut config = Config::new(
        format!("dir:{}", workspace.join("store").display()),
        workspace,
    );
    config.app_id = app_id.to_string();
    config
}

/// The file a crash is written to, under the app's workspace.
pub const CRASH_FILE: &str = "last-crash.txt";

/// Make a crash report itself on the next launch. Installs a panic hook that
/// writes the panic's message, where it happened, the thread and a backtrace
/// to `<workspace>/last-crash.txt` (then runs the default hook), and answers
/// the report a previous run left there, removing it. A phone shows no
/// terminal, and a crash before the first paint shows nothing at all; with
/// this the next launch can put the cause on the page.
pub fn record_crashes(workspace: &Path) -> Option<String> {
    let path = workspace.join(CRASH_FILE);
    let previous = std::fs::read_to_string(&path).ok();
    if previous.is_some() {
        let _ = std::fs::remove_file(&path);
    }
    let _ = std::fs::create_dir_all(workspace);
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let report = format!(
            "{}\nat {}\nthread {}\n\n{}",
            info.payload()
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "(no message)".to_string()),
            info.location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                .unwrap_or_else(|| "(unknown)".to_string()),
            thread.name().unwrap_or("(unnamed)"),
            std::backtrace::Backtrace::force_capture(),
        );
        let _ = std::fs::write(&path, report);
        default(info);
    }));
    previous
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_crash_report_left_by_the_last_run_is_answered_once() {
        let dir = std::env::temp_dir().join(format!("koine-crash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(super::CRASH_FILE), "boom\nat x.rs:1:1").unwrap();
        let hook = std::panic::take_hook();
        assert_eq!(
            super::record_crashes(&dir).as_deref(),
            Some("boom\nat x.rs:1:1")
        );
        assert!(!dir.join(super::CRASH_FILE).exists());
        let _ = std::panic::catch_unwind(|| panic!("second"));
        let written = std::fs::read_to_string(dir.join(super::CRASH_FILE)).unwrap();
        assert!(written.starts_with("second\nat "), "{written}");
        std::panic::set_hook(hook);
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::*;

    fn memory(dir: &Path, app: &str) -> Config {
        let mut config = Config::new("memory:", dir);
        config.app_id = app.to_string();
        config
    }

    // Embedded hosts install process-wide seams; take turns, like the
    // integration tests.
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn turn() -> std::sync::MutexGuard<'static, ()> {
        TURN.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn answers(url: &str) -> bool {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            match reqwest::get(format!("{url}/api/status")).await {
                Ok(response) => response.status() == 200,
                Err(_) => false,
            }
        })
    }

    #[test]
    fn a_started_host_answers_with_a_token() {
        let _turn = turn();
        let dir = tempfile::tempdir().unwrap();
        let host = start(memory(dir.path(), "embed-answers")).expect("starts");
        assert!(host.url().starts_with("http://127.0.0.1:"));
        assert_eq!(host.url(), format!("http://{}", host.addr()));
        assert!(!host.token().is_empty());
        assert!(host.token_path().exists());
        assert_eq!(
            std::fs::read_to_string(host.token_path()).unwrap().trim(),
            host.token()
        );
        assert!(answers(&host.url()));
        host.shutdown();
    }

    #[test]
    fn shutdown_stops_the_port_and_removes_the_token() {
        let _turn = turn();
        let dir = tempfile::tempdir().unwrap();
        let host = start(memory(dir.path(), "embed-shutdown")).expect("starts");
        let (url, token_path) = (host.url(), host.token_path().to_path_buf());
        assert!(answers(&url));
        host.shutdown();
        assert!(!answers(&url));
        assert!(!token_path.exists());
    }

    #[test]
    fn dropping_stops_the_host_too() {
        let _turn = turn();
        let dir = tempfile::tempdir().unwrap();
        let host = start(memory(dir.path(), "embed-drop")).expect("starts");
        let url = host.url();
        assert!(answers(&url));
        drop(host);
        assert!(!answers(&url));
    }

    #[test]
    fn a_serve_error_comes_back_from_start() {
        let _turn = turn();
        let dir = tempfile::tempdir().unwrap();
        let mut config = memory(dir.path(), "embed-error");
        config.store_url = "nonsense:".into();
        assert!(start(config).is_err());
    }

    #[test]
    fn the_sandbox_config_lives_under_documents() {
        let config = sandbox_config_in("demo", Path::new("/x"));
        assert_eq!(config.workspace, Path::new("/x/Documents/demo"));
        assert_eq!(config.store_url, "dir:/x/Documents/demo/store");
        assert_eq!(config.app_id, "demo");
        assert!(config.bind.is_loopback());
        assert_eq!(config.port, 0);
        assert!(!config.allow_network);
        assert!(config.network_bearer.is_none());
    }
}
