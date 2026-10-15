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
//! runner (`impress_service_core::pipeline::adverbs`) and the secret source
//! (`impress_service_core::secrets`, layered by [`secrets::host_source`]),
//! then starts the [`feed`] (a store write wakes the surfaces that read it)
//! and re-arms the routines a previous launch left running
//! ([`runner::JobRunner::rearm_routines`]).
//! Two hosts in one
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
//!
//! # Network access at runtime
//!
//! A host binds one address at start. Network mode at start (`allow_network`
//! with a concrete bind) is that address. A host that started on loopback —
//! the shape of an app on a phone — can open a second listener on the same
//! router with `POST /api/host/network {"allow": true, "bearer": "<optional>"}`
//! (loopback and the loopback token only): it binds `0.0.0.0` on the host's
//! port (any port when that one is taken), a non-loopback peer must present
//! the bearer on every request, and the answer is
//! `{"ok", "wire_version", "allowed": true, "addr", "port", "bearer"}`.
//! `{"allow": false}` aborts that listener and the guard refuses what is left
//! of its connections. `GET /api/host/network` answers the state without the
//! bearer. Both listeners go through one guard reading one
//! [`NetworkAccess`], so the policy cannot differ between them.

pub mod cli;
pub mod embed;
pub mod feed;
mod guard;
pub mod manifest;
pub mod materialize;
mod routes;
pub mod runner;
pub mod secrets;
pub mod window;

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use impress_item::{AttributedStore, BlobStore, StoreCapabilities};
use impress_pane_query::{process_manifest, KindManifest};
use impress_service_core::loopback_token;
use impress_service_core::pipeline::adverbs::install_background_runner;
use impress_service_core::secrets::SecretSource;
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
    /// Where `PUT /api/secret/<name>` keeps the secrets linked verbs
    /// declare, one file each. `None` is `<workspace>/secrets`.
    pub secrets_dir: Option<PathBuf>,
    /// Also read and write secrets in the macOS keychain (service `koine`).
    pub keychain: bool,
    /// The app's version: stamped on every call record and run
    /// (plan-koine-notebook N-D6) and named in the skill.
    pub app_version: String,
    /// Install the kit package at start: the Notebook, Agent desk and
    /// Packages surfaces (plan-koine-research-os B-D4).
    pub kit: bool,
    /// Package registries to add at start (`--registry`, P-D5); one already
    /// known is left as it is, one that cannot be read is logged.
    pub registries: Vec<String>,
}

impl Config {
    /// A loopback host on an OS-chosen port with the [`manifest::from_inventory`]
    /// kinds (the minimal ui kinds plus every `returns_kind` a linked verb
    /// declares), no page and no network access.
    pub fn new(store_url: impl Into<String>, workspace: impl Into<PathBuf>) -> Self {
        Self {
            store_url: store_url.into(),
            port: 0,
            bind: IpAddr::from([127, 0, 0, 1]),
            app_id: "koine-demo".into(),
            device: impress_layout_service::current_device(),
            workspace: workspace.into(),
            web_dir: None,
            manifest: Arc::new(manifest::from_inventory()),
            allow_network: false,
            network_bearer: None,
            secrets_dir: None,
            keychain: false,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            kit: true,
            registries: Vec::new(),
        }
    }

    /// The directory the file layer of the secret source uses.
    pub fn secrets_path(&self) -> PathBuf {
        self.secrets_dir
            .clone()
            .unwrap_or_else(|| self.workspace.join("secrets"))
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
    state: Arc<HostState>,
    feed: Option<feed::FeedHandle>,
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

    /// The job runner `in_background` and `every` calls start on.
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
        self.state.close_network();
        self.feed.take();
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
    /// Who may reach this host beyond loopback. Fixed at start when the
    /// config asked for network mode; otherwise `POST /api/host/network`
    /// opens and closes a second listener and updates it.
    pub network: RwLock<NetworkAccess>,
    pub web_dir: Option<PathBuf>,
    pub blobs: Option<Arc<dyn BlobStore>>,
    pub runner: Arc<JobRunner>,
    pub mcp: Arc<HostConfig>,
    pub secrets: Arc<dyn SecretSource>,
}

/// Open the store, install the process seams, write the loopback token and
/// start serving. Returns once the listener is bound.
pub async fn serve(config: Config) -> Result<Served, HostError> {
    check_network_config(&config)?;
    let opened = koine_store::open_with_blobs(&config.store_url)?;
    // plan-koine-notebook N-D3: every read and write through the process
    // store is noted on the call it ran in.
    let store: Arc<dyn AttributedStore> =
        Arc::new(koine_store::RecordingStore::new(opened.store.clone()));
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
    if let Some(blobs) = opened.blobs.clone() {
        koine_store::process::install_blobs(blobs);
    }
    process_manifest::install(config.manifest.clone());
    // L-D4: a linked verb's rows become records of this store.
    materialize::install(store.clone(), config.manifest.clone());
    let runner = Arc::new(JobRunner::new(
        store.clone(),
        tokio::runtime::Handle::current(),
    ));
    install_background_runner(runner.clone());
    let secret_source = secrets::host_source(config.secrets_path(), config.keychain);
    impress_service_core::secrets::install(secret_source.clone());
    // plan-koine-notebook N-D1: the call log is kept in the store; a run left
    // open by the last launch is open again.
    koine_notebook::install(
        store.clone(),
        koine_notebook::SinkConfig::new(&config.device, &config.app_id, &config.app_version),
    );
    // plan-koine-research-os S-D1: who the skill and the desk speak for.
    koine_agent::set_app(koine_agent::AppInfo {
        app_id: config.app_id.clone(),
        version: config.app_version.clone(),
        kinds: config
            .manifest
            .kinds
            .keys()
            .map(|k| k.to_string())
            .collect(),
    });
    // After the seams a routine's verb may read: the store, the manifest and
    // the secrets.
    let rearmed = runner.rearm_routines();
    if rearmed > 0 {
        tracing::info!(target: "koine-host", "re-armed {rearmed} routine(s)");
    }
    // P-D5: fetched bundles are kept under the workspace; registries named
    // on the command line are added.
    koine_packages::registry::set_cache_dir(config.workspace.join("packages"));
    for url in &config.registries {
        match koine_packages::registry::add(url).await {
            Ok((known, index)) => tracing::info!(target: "koine-host",
                "registry {} ({}): {} package version(s)", known.name, known.url, index.packages.len()),
            Err(error) => tracing::warn!(target: "koine-host",
                "registry {url} not added: {}", error.message),
        }
    }
    // B-D4: the kit's surfaces, three-way against the person's copies.
    if config.kit {
        match koine_packages::kit::install().await {
            Ok(installed) => {
                let changed: Vec<String> = installed
                    .surfaces
                    .iter()
                    .filter(|(_, _, outcome)| {
                        *outcome != koine_packages::shipped::Outcome::Unchanged
                    })
                    .map(|(name, _, outcome)| format!("{name} {}", outcome.word()))
                    .collect();
                if !changed.is_empty() {
                    tracing::info!(target: "koine-host", "kit {}: {}", installed.version, changed.join(", "));
                }
            }
            Err(error) => {
                tracing::warn!(target: "koine-host", "kit not installed: {}", error.message)
            }
        }
    }
    let skill = koine_agent::skill::render(&koine_agent::app());
    let feed = feed::start(
        store,
        impress_surface_service::SessionRegistry::shared(),
        impress_layout_service::DefaultLayoutService::new(),
        config.app_id.clone(),
        config.device.clone(),
    );

    let state = Arc::new(HostState {
        app_id: config.app_id.clone(),
        device: config.device.clone(),
        store_url: config.store_url.clone(),
        capabilities,
        port: addr.port(),
        bind: config.bind,
        token: token.clone(),
        network: RwLock::new(NetworkAccess {
            allow: config.allow_network,
            bearer: config.network_bearer.clone(),
            started_in_network_mode: config.allow_network,
            listener: None,
        }),
        web_dir: config.web_dir.clone(),
        blobs: opened.blobs,
        runner: runner.clone(),
        mcp: Arc::new(HostConfig {
            server_name: config.app_id.clone(),
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            instructions: format!(
                "{}: every linked verb is a tool. Surfaces are authored with the surface_* \
                 verbs and shown with surface_show; the layout is the layout-service_* verbs. \
                 Read the resource koine://skill first; the person leaves you work on the \
                 agent desk (agent-service_next).",
                config.app_id
            ),
            allowed_tool_prefixes: Vec::new(),
            // S-D2: the skill, for an agent that reads resources.
            resources: vec![koine_mcp_host::Resource {
                uri: "koine://skill".into(),
                name: "skill".into(),
                description: format!(
                    "How to work in {}: reaching the host, the rules, the adverbs, the record \
                     kinds and every verb",
                    config.app_id
                ),
                mime_type: "text/markdown".into(),
                text: skill,
                meta: None,
            }],
            tool_ui: Vec::new(),
            tool_file_params: Vec::new(),
        }),
        secrets: secret_source,
    });

    let app = app(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
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
        state,
        feed: Some(feed),
        stop: Some(stop),
        task: Some(task),
    })
}

/// The routes behind the guard: what every listener of one host serves, the
/// one it was started with and the one `POST /api/host/network` opens.
pub(crate) fn app(state: Arc<HostState>) -> axum::Router {
    routes::router(state.clone()).layer(axum::middleware::from_fn_with_state(state, guard::guard))
}

/// Whether, and with which bearer, a peer that is not loopback is admitted.
/// The guard reads this on every request, so changing it changes every
/// listener at once (a keep-alive connection from a closed listener is
/// refused with the next request).
pub(crate) struct NetworkAccess {
    pub allow: bool,
    pub bearer: Option<String>,
    /// The config bound a concrete non-loopback address at start: network
    /// mode is the host's whole shape and the route cannot change it.
    pub started_in_network_mode: bool,
    /// The second listener `POST /api/host/network` opened.
    pub listener: Option<routes::host::NetworkListener>,
}

impl HostState {
    /// Close the listener `POST /api/host/network` opened, if any, and stop
    /// admitting remote peers. A no-op for a host started in network mode.
    pub(crate) fn close_network(&self) {
        let mut network = self.network.write().unwrap_or_else(|e| e.into_inner());
        if network.started_in_network_mode {
            return;
        }
        network.allow = false;
        network.bearer = None;
        if let Some(listener) = network.listener.take() {
            listener.task.abort();
        }
    }
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

/// Reference the two runtime crates and the kit's three (the notebook, the
/// agent desk, packages) so the linker keeps their `#[impress_service]`
/// registrations: an unreferenced crate that only
/// contains `inventory::submit!` statics is dropped from the final binary.
/// Call this from `main` (and from any test that expects their verbs).
/// `surface-demo-service` is the binary's, not the library's.
pub fn ensure_linked() {
    let anchors: [*const (); 2] = [
        impress_layout_service::DefaultLayoutService::new as *const (),
        impress_surface_service::DefaultImpressSurfaceService::new as *const (),
    ];
    std::hint::black_box(anchors);
    koine_notebook::ensure_linked();
    koine_agent::ensure_linked();
    koine_packages::ensure_linked();
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
