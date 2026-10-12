//! `koine-host`: serve the linked verbs, generated surfaces, the layout, the
//! `koine-web` page and MCP from one process.
//!
//! ```text
//! koine-host --store dir:~/koine-demo/store --port 23130 --web-dir <public>
//! koine-host --store memory: --port 0 --check      # start, print, exit 0
//! ```
//!
//! It links `surface-demo-service` (two verbs, `series` and `histogram`) and
//! the two runtime crates; a bespoke app is this binary with its own service
//! crate linked beside them (`scripts/new-app.sh`).

use std::net::IpAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use koine_host::{ensure_linked, serve, Config};

const USAGE: &str = "\
koine-host: serve the linked verbs, surfaces, layout, page and MCP

USAGE:
    koine-host [OPTIONS]

OPTIONS:
    --store <url>            memory:, dir:<path> (default: dir:<workspace>/store)
    --port <n>               port to bind; 0 lets the OS choose (default: 23130)
    --bind <addr>            address to bind (default: 127.0.0.1)
    --workspace <dir>        where the loopback token is written, as
                             <dir>/automation/loopback-<port>.token
                             (default: $KOINE_WORKSPACE, else ~/.koine)
    --web-dir <dir>          the built koine-web bundle (holds index.html), served at /ui
    --app-id <id>            who the host says it is (default: koine-demo)
    --device <name>          the device the layout and surface state are scoped to
                             (default: this machine's name)
    --allow-network          admit peers that are not loopback; needs --network-bearer
                             (or $KOINE_NETWORK_BEARER) and a concrete --bind
    --network-bearer <tok>   the bearer a non-loopback peer presents
    --check                  start, print the URL and verb count, stop, exit 0
    -h, --help               this text
";

#[derive(Debug)]
struct Cli {
    config: Config,
    check: bool,
}

fn parse(args: impl IntoIterator<Item = String>) -> Result<Option<Cli>, String> {
    let mut args = args.into_iter();
    let mut store: Option<String> = None;
    let mut port: u16 = 23130;
    let mut bind: Option<IpAddr> = None;
    let mut workspace: Option<PathBuf> = None;
    let mut web_dir: Option<PathBuf> = None;
    let mut app_id: Option<String> = None;
    let mut device: Option<String> = None;
    let mut allow_network = false;
    let mut bearer = std::env::var("KOINE_NETWORK_BEARER").ok();
    let mut check = false;
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
            "--app-id" => app_id = Some(value("--app-id")?),
            "--device" => device = Some(value("--device")?),
            "--network-bearer" => bearer = Some(value("--network-bearer")?),
            "--allow-network" => allow_network = true,
            "--check" => check = true,
            other => return Err(format!("unknown option {other} (see --help)")),
        }
    }
    let workspace = workspace
        .or_else(|| {
            std::env::var_os("KOINE_WORKSPACE")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|home| PathBuf::from(home).join(".koine"))
        })
        .ok_or("no --workspace, $KOINE_WORKSPACE or $HOME to put the token under")?;
    let store = store.unwrap_or_else(|| format!("dir:{}", workspace.join("store").display()));
    let mut config = Config::new(store, workspace);
    config.port = port;
    if let Some(bind) = bind {
        config.bind = bind;
    }
    config.web_dir = web_dir;
    if let Some(app_id) = app_id {
        config.app_id = app_id;
    } else {
        config.app_id = "koine-demo".into();
    }
    if let Some(device) = device {
        config.device = device;
    }
    config.allow_network = allow_network;
    config.network_bearer = bearer;
    Ok(Some(Cli { config, check }))
}

#[tokio::main]
async fn main() -> ExitCode {
    // The runtimes' registrations, and the demo's, must survive the link.
    ensure_linked();
    std::hint::black_box(surface_demo_service::DefaultSurfaceDemoService::new as *const ());

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,koine-host=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = match parse(std::env::args().skip(1)) {
        Ok(Some(cli)) => cli,
        Ok(None) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("koine-host: {message}");
            return ExitCode::from(2);
        }
    };
    let mut served = match serve(cli.config.clone()).await {
        Ok(served) => served,
        Err(error) => {
            eprintln!("koine-host: {error}");
            return ExitCode::FAILURE;
        }
    };
    let verbs = impress_service_core::call::descriptors().count();
    println!("koine-host: {}", served.url());
    println!(
        "  app {}, device {}, store {}, {verbs} verbs",
        cli.config.app_id, cli.config.device, cli.config.store_url
    );
    println!("  loopback token: {}", served.token_path().display());
    match &cli.config.web_dir {
        Some(dir) => println!("  page: {}/ui/ (from {})", served.url(), dir.display()),
        None => println!("  page: none (pass --web-dir to serve /ui)"),
    }
    if cli.check {
        served.shutdown().await;
        return ExitCode::SUCCESS;
    }
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = served.wait() => {}
    }
    served.shutdown().await;
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn flags_become_a_config() {
        let cli = parse(args(&[
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
    fn the_store_defaults_under_the_workspace() {
        let cli = parse(args(&["--workspace", "/tmp/ws"])).unwrap().unwrap();
        assert_eq!(cli.config.store_url, "dir:/tmp/ws/store");
        assert_eq!(cli.config.port, 23130);
    }

    #[test]
    fn a_bad_flag_is_named() {
        assert!(parse(args(&["--bogus"])).unwrap_err().contains("--bogus"));
        assert!(parse(args(&["--port"])).unwrap_err().contains("--port"));
        assert!(parse(args(&["--port", "x"])).unwrap_err().contains("port"));
        assert!(parse(args(&["--help"])).unwrap().is_none());
    }
}
