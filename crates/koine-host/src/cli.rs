//! The flags and the run loop of a host binary, once.
//!
//! `koine-host` and every scaffolded `<name>-host` (`scripts/new-app.sh`) are
//! the same `main`: link the app's service crate, call [`main`]. The flags:
//!
//! ```text
//! <host> --store dir:~/app/store --port 23130 --web-dir <public>
//! <host> --store memory: --port 0 --check      # start, print, exit 0
//! ```

use std::net::IpAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use impress_service_core::loopback_token;

use crate::{serve, Config};

/// The `--help` text for a binary called `program`.
pub fn usage(program: &str) -> String {
    USAGE.replace("{program}", program)
}

const USAGE: &str = "\
{program}: serve the linked verbs, surfaces, layout, page and MCP

USAGE:
    {program} [OPTIONS]

OPTIONS:
    --store <url>            memory:, dir:<path> (default: dir:<workspace>/store)
    --port <n>               port to bind; 0 lets the OS choose (default: 23130)
    --bind <addr>            address to bind (default: 127.0.0.1)
    --workspace <dir>        where the loopback token is written, as
                             <dir>/automation/loopback-<port>.token
                             (default: $KOINE_WORKSPACE; else the impress suite's
                             workspace, <app-group container>/workspace, when that
                             container exists on this machine, so the suite's own
                             clients find the token; else ~/.koine)
    --web-dir <dir>          the built koine-web bundle (holds index.html), served at /ui
    --app-id <id>            who the host says it is (default: koine-demo)
    --device <name>          the device the layout and surface state are scoped to
                             (default: this machine's name)
    --allow-network          admit peers that are not loopback; needs --network-bearer
                             (or $KOINE_NETWORK_BEARER) and a concrete --bind
    --network-bearer <tok>   the bearer a non-loopback peer presents
    --secrets-dir <dir>      where PUT /api/secret/<name> keeps the secrets linked verbs
                             declare, one 0600 file each
                             (default: <workspace>/secrets)
    --keychain               also read and write secrets in the macOS keychain
                             (service koine); the environment and --secrets-dir win
    --window                 open the page at /ui/ in a window: a native WebView when
                             built with --features window-webview, else the browser;
                             the host stops when the native window closes
    --check                  start, print the URL and verb count, stop, exit 0
    -h, --help               this text
";

#[derive(Debug)]
pub struct Cli {
    /// What [`crate::serve`] takes.
    pub config: Config,
    /// Start, print, stop, exit 0 (`--check`).
    pub check: bool,
    /// Open the page in a window after serving (`--window`, L-D9).
    pub window: bool,
}

/// What the flags default from outside the command line. [`Env::process`] is
/// this process's; a test builds its own instead of setting variables.
#[derive(Debug, Clone, Default)]
pub struct Env {
    /// `$KOINE_WORKSPACE`.
    pub koine_workspace: Option<PathBuf>,
    /// The suite's app-group container
    /// ([`loopback_token::default_container_root`]), whether or not it
    /// exists.
    pub container_root: Option<PathBuf>,
    /// `$HOME`.
    pub home: Option<PathBuf>,
    /// `$KOINE_NETWORK_BEARER`.
    pub network_bearer: Option<String>,
}

impl Env {
    /// This process's environment.
    pub fn process() -> Self {
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
        Env {
            koine_workspace: var("KOINE_WORKSPACE").map(PathBuf::from),
            container_root: loopback_token::default_container_root(),
            home: var("HOME").map(PathBuf::from),
            network_bearer: std::env::var("KOINE_NETWORK_BEARER").ok(),
        }
    }

    /// The workspace when `--workspace` is not given (plan-koine-live L-D10):
    /// `$KOINE_WORKSPACE`; else `<container>/workspace` when the suite's
    /// container is an existing directory (the token then lands where the
    /// suite's Rust clients look first); else `~/.koine`. `None` when there is
    /// no home to fall back on.
    pub fn default_workspace(&self) -> Option<PathBuf> {
        self.koine_workspace
            .clone()
            .or_else(|| {
                self.container_root
                    .as_ref()
                    .filter(|root| root.is_dir())
                    .map(|root| root.join("workspace"))
            })
            .or_else(|| {
                self.home
                    .as_ref()
                    .map(|home| home.join(loopback_token::KOINE_HOME_DIR))
            })
    }
}

/// Parse the flags of a host binary (`--help` text: [`usage`]). `Ok(None)` is
/// `--help`; `app_id` is who the host says it is unless `--app-id` overrides.
pub fn parse(args: impl IntoIterator<Item = String>, app_id: &str) -> Result<Option<Cli>, String> {
    parse_with_env(args, app_id, &Env::process())
}

/// [`parse`] over an explicit [`Env`].
pub fn parse_with_env(
    args: impl IntoIterator<Item = String>,
    app_id: &str,
    env: &Env,
) -> Result<Option<Cli>, String> {
    let mut args = args.into_iter();
    let mut store: Option<String> = None;
    let mut port: u16 = 23130;
    let mut bind: Option<IpAddr> = None;
    let mut workspace: Option<PathBuf> = None;
    let mut web_dir: Option<PathBuf> = None;
    let mut app_id_flag: Option<String> = None;
    let mut device: Option<String> = None;
    let mut allow_network = false;
    let mut bearer = env.network_bearer.clone();
    let mut secrets_dir: Option<PathBuf> = None;
    let mut keychain = false;
    let mut check = false;
    let mut window = false;
    while let Some(flag) = args.next() {
        let (flag, inline) = match flag.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => {
                (flag.to_string(), Some(value.to_string()))
            }
            _ => (flag, None),
        };
        let mut value = |name: &str| {
            inline
                .clone()
                .or_else(|| args.next())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag.as_str() {
            "-h" | "--help" => return Ok(None),
            "--store" => store = Some(value("--store")?),
            "--port" => {
                let text = value("--port")?;
                port = text
                    .parse()
                    .map_err(|_| format!("--port {text}: not a port number"))?;
            }
            "--bind" => {
                let text = value("--bind")?;
                bind = Some(
                    text.parse()
                        .map_err(|_| format!("--bind {text}: not an IP address"))?,
                );
            }
            "--workspace" => workspace = Some(PathBuf::from(value("--workspace")?)),
            "--web-dir" => web_dir = Some(PathBuf::from(value("--web-dir")?)),
            "--app-id" => app_id_flag = Some(value("--app-id")?),
            "--device" => device = Some(value("--device")?),
            "--network-bearer" => bearer = Some(value("--network-bearer")?),
            "--allow-network" => allow_network = true,
            "--secrets-dir" => secrets_dir = Some(PathBuf::from(value("--secrets-dir")?)),
            "--keychain" => keychain = true,
            "--check" => check = true,
            "--window" => window = true,
            other => return Err(format!("unknown option {other} (see --help)")),
        }
    }
    let workspace = workspace
        .or_else(|| env.default_workspace())
        .ok_or("no --workspace, $KOINE_WORKSPACE or $HOME to put the token under")?;
    let store = store.unwrap_or_else(|| format!("dir:{}", workspace.join("store").display()));
    let mut config = Config::new(store, workspace);
    config.port = port;
    if let Some(bind) = bind {
        config.bind = bind;
    }
    config.web_dir = web_dir;
    config.app_id = app_id_flag.unwrap_or_else(|| app_id.to_string());
    if let Some(device) = device {
        config.device = device;
    }
    config.allow_network = allow_network;
    config.network_bearer = bearer;
    config.secrets_dir = secrets_dir;
    config.keychain = keychain;
    Ok(Some(Cli {
        config,
        check,
        window,
    }))
}

/// Everything a host binary's `main` does: logging, the flags, [`serve`], the
/// banner, then wait for Ctrl-C (or return at once for `--check`). `app_id` is
/// the default for `--app-id`. Builds its own tokio runtime, so the caller's
/// `main` is a plain `fn` and needs no async dependency.
///
/// Call the app's `ensure_linked()` and [`crate::ensure_linked`] first: the
/// linker drops a crate nothing refers to, and its verbs with it.
pub fn main(app_id: &str) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("cannot start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,koine-host=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let program = std::env::args()
        .next()
        .and_then(|p| {
            std::path::Path::new(&p)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "koine-host".into());
    let cli = match parse(std::env::args().skip(1), app_id) {
        Ok(Some(cli)) => cli,
        Ok(None) => {
            print!("{}", usage(&program));
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("{program}: {message}");
            return ExitCode::from(2);
        }
    };
    let served = match runtime.block_on(start(&cli, &program)) {
        Ok(served) => served,
        Err(code) => return code,
    };
    if cli.check {
        runtime.block_on(served.shutdown());
        return ExitCode::SUCCESS;
    }
    // `--window` (L-D9): the native WebView's event loop must own the main
    // thread, which is this one, outside `block_on`; the server keeps
    // answering on the runtime's worker threads meanwhile. The browser
    // fallback returns at once and the host then waits as without the flag.
    if cli.window {
        if cli.config.web_dir.is_none() {
            eprintln!("{program}: --window without --web-dir: the page will answer 503");
        }
        let url = format!("{}/ui/", served.url());
        match crate::window::open_after_serve(&url, &cli.config.app_id) {
            Ok(koine_window::Opened::WebView) => {
                runtime.block_on(served.shutdown());
                return ExitCode::SUCCESS;
            }
            Ok(koine_window::Opened::Browser) => println!("  opened {url} in the browser"),
            Err(error) => eprintln!("{program}: {error}"),
        }
    }
    runtime.block_on(wait_then_shutdown(served))
}

/// Serve `cli.config` and print where. The banner names the page and the
/// token path; `Err` is the exit code to return.
async fn start(cli: &Cli, program: &str) -> Result<crate::Served, ExitCode> {
    let served = match serve(cli.config.clone()).await {
        Ok(served) => served,
        Err(error) => {
            eprintln!("{program}: {error}");
            return Err(ExitCode::FAILURE);
        }
    };
    let verbs = impress_service_core::call::descriptors().count();
    println!("{program}: {}", served.url());
    println!(
        "  app {}, device {}, store {}, {verbs} verbs",
        cli.config.app_id, cli.config.device, cli.config.store_url
    );
    println!("  loopback token: {}", served.token_path().display());
    match &cli.config.web_dir {
        Some(dir) => println!("  page: {}/ui/ (from {})", served.url(), dir.display()),
        None => println!("  page: none (pass --web-dir to serve /ui)"),
    }
    Ok(served)
}

/// Wait for Ctrl-C or the server's end, then stop.
async fn wait_then_shutdown(mut served: crate::Served) -> ExitCode {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = served.wait() => {}
    }
    served.shutdown().await;
    ExitCode::SUCCESS
}

/// Serve `cli.config`, print where, and wait (or stop at once for `--check`).
/// `--window` is [`main`]'s alone: a native window needs the main thread.
pub async fn run(cli: Cli, program: &str) -> ExitCode {
    let served = match start(&cli, program).await {
        Ok(served) => served,
        Err(code) => return code,
    };
    if cli.check {
        served.shutdown().await;
        return ExitCode::SUCCESS;
    }
    if cli.window {
        eprintln!(
            "{program}: --window is only honoured by cli::main (the window needs the main thread)"
        );
    }
    wait_then_shutdown(served).await
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn parse_args(a: Vec<String>) -> Result<Option<Cli>, String> {
        parse(a, "koine-demo")
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn flags_become_a_config() {
        let cli = parse_args(args(&[
            "--store",
            "memory:",
            "--port=0",
            "--workspace",
            "/tmp/ws",
            "--app-id",
            "mine",
            "--check",
        ]))
        .unwrap()
        .unwrap();
        assert!(cli.check);
        assert_eq!(cli.config.store_url, "memory:");
        assert_eq!(cli.config.port, 0);
        assert_eq!(cli.config.app_id, "mine");
        assert!(!cli.config.allow_network);
    }

    #[test]
    fn secret_flags_become_config() {
        let cli = parse_args(args(&["--workspace", "/tmp/ws"]))
            .unwrap()
            .unwrap();
        assert_eq!(cli.config.secrets_dir, None);
        assert!(!cli.config.keychain);
        assert_eq!(
            cli.config.secrets_path(),
            PathBuf::from("/tmp/ws").join("secrets")
        );
        let cli = parse_args(args(&[
            "--workspace",
            "/tmp/ws",
            "--secrets-dir=/tmp/s",
            "--keychain",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(cli.config.secrets_dir, Some(PathBuf::from("/tmp/s")));
        assert!(cli.config.keychain);
        assert_eq!(cli.config.secrets_path(), PathBuf::from("/tmp/s"));
    }

    #[test]
    fn the_store_defaults_under_the_workspace() {
        let cli = parse_args(args(&["--workspace", "/tmp/ws"]))
            .unwrap()
            .unwrap();
        assert_eq!(cli.config.store_url, "dir:/tmp/ws/store");
        assert_eq!(cli.config.port, 23130);
    }

    fn env(workspace: Option<&str>, container: Option<&Path>, home: Option<&str>) -> Env {
        Env {
            koine_workspace: workspace.map(PathBuf::from),
            container_root: container.map(Path::to_path_buf),
            home: home.map(PathBuf::from),
            network_bearer: None,
        }
    }

    #[test]
    fn the_workspace_defaults_to_koine_workspace_then_the_container_then_home() {
        let container = tempfile::tempdir().unwrap();
        // $KOINE_WORKSPACE wins over everything.
        let cli = parse_with_env(
            args(&["--port=0"]),
            "x",
            &env(Some("/tmp/kw"), Some(container.path()), Some("/home/u")),
        )
        .unwrap()
        .unwrap();
        assert_eq!(cli.config.workspace, PathBuf::from("/tmp/kw"));
        assert_eq!(cli.config.store_url, "dir:/tmp/kw/store");
        // The token lands where a client's second candidate looks.
        let port = 23199;
        let token = cli
            .config
            .workspace
            .join("automation")
            .join(loopback_token::token_file_name(port));
        assert_eq!(
            loopback_token::candidate_paths(port, None, Some(&cli.config.workspace), None),
            vec![token]
        );
        // Else the suite's container, when it exists: under its workspace/.
        let cli = parse_with_env(
            args(&[]),
            "x",
            &env(None, Some(container.path()), Some("/home/u")),
        )
        .unwrap()
        .unwrap();
        assert_eq!(cli.config.workspace, container.path().join("workspace"));
        assert_eq!(
            loopback_token::token_path_under(container.path(), port),
            cli.config
                .workspace
                .join("automation")
                .join(loopback_token::token_file_name(port))
        );
        // A container that is not on this machine is skipped.
        let absent = container.path().join("absent");
        let cli = parse_with_env(args(&[]), "x", &env(None, Some(&absent), Some("/home/u")))
            .unwrap()
            .unwrap();
        assert_eq!(cli.config.workspace, PathBuf::from("/home/u/.koine"));
        // --workspace wins over all of it; nothing at all is an error.
        let cli = parse_with_env(
            args(&["--workspace", "/tmp/flag"]),
            "x",
            &env(Some("/tmp/kw"), None, None),
        )
        .unwrap()
        .unwrap();
        assert_eq!(cli.config.workspace, PathBuf::from("/tmp/flag"));
        assert!(parse_with_env(args(&[]), "x", &env(None, None, None)).is_err());
    }

    #[test]
    fn a_bad_flag_is_named() {
        assert!(parse_args(args(&["--bogus"]))
            .unwrap_err()
            .contains("--bogus"));
        assert!(parse_args(args(&["--port"]))
            .unwrap_err()
            .contains("--port"));
        assert!(parse_args(args(&["--port", "x"]))
            .unwrap_err()
            .contains("port"));
        assert!(parse_args(args(&["--help"])).unwrap().is_none());
    }
}
