//! `koine-host`: one process that serves a verb inventory, the surfaces and
//! layout built on it, the `koine-web` page and MCP, over a store opened from
//! a URL (docs/plan-koine-host.md H-D4).
//!
//! ```no_run
//! # async fn run() -> Result<(), koine_host::HostError> {
//! let mut config = koine_host::Config::new("dir:/tmp/koine/store", "/tmp/koine");
//! config.port = 23130;
//! let served = koine_host::serve(config).await?;
//! println!("listening on {}", served.url());
//! served.shutdown().await;
//! # Ok(()) }
//! ```
//!
//! # What it is not
//!
//! It owns no domain. The verbs are whatever the binary linked
//! ([`ensure_linked`] anchors the two runtimes; the `koine-host` binary adds
//! `surface-demo-service`), the record kinds are the [`manifest`] in
//! [`Config`], and the store is whatever the URL names. It is not an
//! impress-apps app: nothing here is looking at a window, so a layout tree
//! is the stored row, not a rendering (see `routes::layout`).
//!
//! # The process seams
//!
//! [`serve`] installs, once per call and process-wide: the store
//! (`koine_store::process::install`), the kind manifest
//! (`impress_pane_query::process_manifest::install`) and the background
//! runner (`impress_service_core::pipeline::adverbs`). Two hosts in one
//! process therefore share them — the last to start wins — which is why
//! the integration tests take turns.
//!
//! # Who may call
//!
//! The checks every automation server in the suite applies run before any
//! route (`guard`): a `Host` that is not this machine is 400; on loopback a
//! `GET`/`HEAD`/`OPTIONS` needs nothing and every other method needs this
//! launch's token; a non-loopback peer needs [`Config::allow_network`] and
//! the network bearer; no response carries an `Access-Control-*` header.

pub mod cli;
mod guard;
pub mod manifest;
mod routes;
pub mod runner;

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use impress_item::{AttributedStore, BlobStore, StoreCapabilities};
use impress_pane_query::{process_manifest, KindManifest};
use impress_service_core::loopback_token;
use impress_service_core::pipeline::adverbs::install_background_runner;
use koine_mcp_host::HostConfig;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::runner::JobRunner;

/// Everything a host needs to start.
#[derive(Clone, Debug)]
pub struct Config {
    /// Where the records live: `memory:`, `dir:<path>`, or another scheme
    /// registered with `koine_store::register`.
    pub store_url: String,
    /// The port to bind; `0` asks the OS (the bound port is
    /// [`Served::addr`]).
    pub port: u16,
    /// The address to bind. Loopback unless [`Self::allow_network`].
    pub bind: IpAddr,
    /// Who the host says it is: the layout's `app_id`, `GET /api/status`'s
    /// `app`, the MCP server name.
    pub app_id: String,
    /// The device tag the layout and surface state are scoped to.
    pub device: String,
    /// The root the loopback token is written under:
    /// `<workspace>/automation/loopback-<port>.token`.
    pub workspace: PathBuf,
    /// The built `koine-web` bundle (`index.html` at its top), served at
    /// `/ui`. `None` answers 503 there, naming the build.
    pub web_dir: Option<PathBuf>,
    /// The record kinds the runtimes' queries compile against.
    pub manifest: Arc<KindManifest>,
    /// Admit peers that are not loopback. Needs [`Self::network_bearer`] and
    /// a concrete, non-loopback [`Self::bind`].
    pub allow_network: bool,
    /// The bearer a non-loopback peer presents on every request.
    pub network_bearer: Option<String>,
}

impl Config {
    /// A loopback host on an OS-chosen port with the [`manifest::minimal`]
    /// kinds, no page and no network access.
    pub fn new(store_url: impl Into<String>, workspace: impl Into<PathBuf>) -> Self {
        Self {
            store_url: store_url.into(),
            port: 0,
            bind: IpAddr::from([127, 0, 0, 1]),
            app_id: "koine-demo".into(),
            device: impress_layout_service::current_device(),
            workspace: workspace.into(),
            web_dir: None,
            manifest: Arc::new(manifest::minimal()),
            allow_network: false,
            network_bearer: None,
        }
    }
}

/// Why [`serve`] did not start.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// The configuration cannot be served safely or at all.
    #[error("{0}")]
    Config(String),
    /// The store URL did not open.
    #[error(transparent)]
    Store(#[from] koine_store::OpenError),
    /// The address could not be bound.
    #[error("bind {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    /// The loopback token could not be written.
    #[error("write the loopback token under {path}: {source}")]
    Token {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// A running host. Stop it with [`Self::shutdown`]; dropping it stops it too,
/// without waiting.
pub struct Served {
    addr: SocketAddr,
    token: String,
    token_path: PathBuf,
    runner: Arc<JobRunner>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl Served {
    /// The address actually bound (the port is the OS's when `Config::port`
    /// was `0`).
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// `http://<addr>`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// This launch's loopback token: what a mutating request from loopback
    /// presents as `Authorization: Bearer <token>`.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Where the token was written.
    pub fn token_path(&self) -> &Path {
        &self.token_path
    }

    /// The job runner `in_background` calls start on.
    pub fn runner(&self) -> &Arc<JobRunner> {
        &self.runner
    }

    /// Resolves when the server task ends (it only does so after
    /// [`Self::shutdown`] or a fatal accept error). The binary waits on this
    /// beside the interrupt signal.
    pub async fn wait(&mut self) {
        if let Some(task) = self.task.as_mut() {
            let _ = task.await;
            self.task = None;
        }
    }

    /// Stop accepting, let in-flight requests finish, cancel running jobs and
    /// remove the token file.
    pub async fn shutdown(mut self) {
        self.stop_now();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }

    fn stop_now(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.runner.cancel_all();
        let _ = std::fs::remove_file(&self.token_path);
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        self.stop_now();
    }
}

/// What every route reads. Built once by [`serve`].
pub(crate) struct HostState {
    pub app_id: String,
    pub device: String,
    pub store_url: String,
    pub capabilities: StoreCapabilities,
    pub port: u16,
    pub bind: IpAddr,
    pub token: String,
    pub allow_network: bool,
    pub network_bearer: Option<String>,
    pub web_dir: Option<PathBuf>,
    pub blobs: Option<Arc<dyn BlobStore>>,
    pub runner: Arc<JobRunner>,
    pub mcp: Arc<HostConfig>,
}

/// Open the store, install the process seams, write the loopback token and
/// start serving. Returns once the listener is bound.
pub async fn serve(config: Config) -> Result<Served, HostError> {
    check_network_config(&config)?;
    let opened = koine_store::open_with_blobs(&config.store_url)?;
    let store: Arc<dyn AttributedStore> = opened.store.clone();
    let capabilities = store.capabilities();

    let listener = tokio::net::TcpListener::bind(SocketAddr::new(config.bind, config.port))
        .await
        .map_err(|source| HostError::Bind {
            addr: SocketAddr::new(config.bind, config.port),
            source,
        })?;
    let addr = listener.local_addr().map_err(|source| HostError::Bind {
        addr: SocketAddr::new(config.bind, config.port),
        source,
    })?;

    let token = loopback_token::generate();
    let token_path = config
        .workspace
        .join("automation")
        .join(loopback_token::token_file_name(addr.port()));
    loopback_token::write(&token_path, &token).map_err(|source| HostError::Token {
        path: token_path.clone(),
        source,
    })?;

    koine_store::process::install(store.clone());
    process_manifest::install(config.manifest.clone());
    let runner = Arc::new(JobRunner::new(store, tokio::runtime::Handle::current()));
    install_background_runner(runner.clone());

    let state = Arc::new(HostState {
        app_id: config.app_id.clone(),
        device: config.device.clone(),
        store_url: config.store_url.clone(),
        capabilities,
        port: addr.port(),
        bind: config.bind,
        token: token.clone(),
        allow_network: config.allow_network,
        network_bearer: config.network_bearer.clone(),
        web_dir: config.web_dir.clone(),
        blobs: opened.blobs,
        runner: runner.clone(),
        mcp: Arc::new(HostConfig {
            server_name: config.app_id.clone(),
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            instructions: format!(
                "{}: every linked verb is a tool. Surfaces are authored with the surface_* \
                 verbs and shown with surface_show; the layout is the layout-service_* verbs.",
                config.app_id
            ),
            allowed_tool_prefixes: Vec::new(),
            resources: Vec::new(),
            tool_ui: Vec::new(),
            tool_file_params: Vec::new(),
        }),
    });

    let app = routes::router(state.clone())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            guard::guard,
        ))
        .into_make_service_with_connect_info::<SocketAddr>();
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let served = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = stopped.await;
        });
        if let Err(error) = served.await {
            tracing::error!(target: "koine-host", "server stopped: {error}");
        }
    });

    Ok(Served {
        addr,
        token,
        token_path,
        runner,
        stop: Some(stop),
        task: Some(task),
    })
}

/// Network mode binds the one configured address and refuses to start
/// without a bearer; a non-loopback bind without network mode is refused
/// rather than served as if it were local.
fn check_network_config(config: &Config) -> Result<(), HostError> {
    if config.allow_network {
        if config.network_bearer.as_deref().is_none_or(str::is_empty) {
            return Err(HostError::Config(
                "network access needs a bearer: set Config::network_bearer \
                 (--network-bearer), or turn network access off"
                    .into(),
            ));
        }
        if config.bind.is_unspecified() || config.bind.is_loopback() {
            return Err(HostError::Config(format!(
                "network access binds one concrete non-loopback address; {} is {}",
                config.bind,
                if config.bind.is_loopback() {
                    "loopback"
                } else {
                    "a wildcard"
                }
            )));
        }
    } else if !config.bind.is_loopback() {
        return Err(HostError::Config(format!(
            "{} is not a loopback address: pass allow_network (--allow-network) with a \
             bearer to serve beyond this machine",
            config.bind
        )));
    }
    Ok(())
}

/// Reference the two runtime crates so the linker keeps their
/// `#[impress_service]` registrations: an unreferenced crate that only
/// contains `inventory::submit!` statics is dropped from the final binary.
/// Call this from `main` (and from any test that expects their verbs).
/// `surface-demo-service` is the binary's, not the library's.
pub fn ensure_linked() {
    let anchors: [*const (); 2] = [
        impress_layout_service::DefaultLayoutService::new as *const (),
        impress_surface_service::DefaultImpressSurfaceService::new as *const (),
    ];
    std::hint::black_box(anchors);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(bind: [u8; 4], network: bool, bearer: Option<&str>) -> Config {
        let mut c = Config::new("memory:", std::env::temp_dir());
        c.bind = IpAddr::from(bind);
        c.allow_network = network;
        c.network_bearer = bearer.map(str::to_string);
        c
    }

    #[test]
    fn network_mode_needs_a_bearer_and_one_concrete_address() {
        assert!(check_network_config(&config([127, 0, 0, 1], false, None)).is_ok());
        assert!(check_network_config(&config([192, 168, 1, 5], false, None)).is_err());
        assert!(check_network_config(&config([192, 168, 1, 5], true, None)).is_err());
        assert!(check_network_config(&config([192, 168, 1, 5], true, Some(""))).is_err());
        assert!(check_network_config(&config([0, 0, 0, 0], true, Some("x"))).is_err());
        assert!(check_network_config(&config([127, 0, 0, 1], true, Some("x"))).is_err());
        assert!(check_network_config(&config([192, 168, 1, 5], true, Some("x"))).is_ok());
    }
}
