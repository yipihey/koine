//! The koine demo as an iPhone app (`docs/plan-koine-mobile.md` M-D1).
//!
//! One process: the host (`koine_host::embed`, on its own runtime thread,
//! bound to loopback on an ephemeral port, over a `dir:` store in the app's
//! Documents) and the `koine-web` page in a WebView on the main thread. The
//! page is handed the host's URL and bearer in-process (`HostLink`), so it
//! is read-write from its first paint.
//!
//! With the `mobile` feature (what `dx build --platform ios` turns on) this
//! opens the window. Without it `koine_web::launch` prints a hint and
//! returns, so the same `main` compiles and type-checks in the Linux lane.
//! A bespoke app is this file with its own service crate linked instead
//! (`scripts/new-app.sh <name> --lib <lib> --mobile`).

/// The id the host says it is, and the folder under Documents.
const APP_ID: &str = "koine-demo";

fn main() {
    // The runtimes' registrations and the demo's must survive the link: an
    // unreferenced crate that only holds `inventory::submit!` statics is
    // dropped from the final binary.
    koine_host::ensure_linked();
    std::hint::black_box(surface_demo_service::DefaultSurfaceDemoService::new as *const ());

    // A crash writes its cause to the workspace; the next launch shows it on
    // the Host tab (a phone has no terminal to read a panic from).
    let config = koine_host::embed::sandbox_config(APP_ID);
    if let Some(crash) = koine_host::embed::record_crashes(&config.workspace) {
        eprintln!("koine-demo-mobile: the last run crashed:\n{crash}");
        koine_web::add_note(format!("the last run crashed: {crash}"));
    }
    let host = koine_host::embed::start(config).expect("the host starts");

    // The lane (scripts/tests/ios/run.sh) and a person with `dx serve` read
    // these two lines: where the host is, and where its token is.
    let url = host.url();
    eprintln!("koine-demo-mobile: host {url}");
    eprintln!(
        "koine-demo-mobile: loopback token {}",
        host.token_path().display()
    );

    // The tour (docs/plan-koine-tour.md): the five surfaces and the notes,
    // seeded once (a surface whose spec changed since the last release is
    // replaced, A-D8), three of them side by side in the layout once (A-D4),
    // then the concierge on its own thread, speaking the same verbs an agent
    // off the phone would.
    // A tour that fails to seed must not keep the app from opening: the
    // panic is caught (and recorded by the hook above), the page opens
    // without the tour, and the Host tab says why.
    let report = std::panic::catch_unwind(|| {
        surface_demo_service::tour::seed_with_layout_blocking(APP_ID, None)
    })
    .unwrap_or_else(|_| {
        koine_web::add_note("the tour could not be seeded; the crash note above says why");
        surface_demo_service::tour::SeedReport::default()
    });
    eprintln!(
        "koine-demo-mobile: tour {} created, {} updated, {} existing, {} notes seeded, {} tiles laid out",
        report.created.len(),
        report.updated.len(),
        report.existing.len(),
        report.notes_seeded,
        report.layout.len()
    );
    for error in &report.errors {
        eprintln!("koine-demo-mobile: tour: {error}");
    }
    let concierge = surface_demo_service::concierge::spawn_thread(
        surface_demo_service::concierge::ConciergeConfig::new(APP_ID),
    );

    // Probe the host from this thread with plain sockets, before the page
    // does with its own transport: the two answers, side by side on the
    // page's Host tab, say which layer a phone fails at.
    for note in probe(&url) {
        eprintln!("koine-demo-mobile: {note}");
        koine_web::add_note(note);
    }

    koine_web::install_link(koine_web::HostLink {
        origin: url,
        token: Some(host.token().to_string()),
        app_id: APP_ID.into(),
    });

    // Returns when the window closes (or at once without a renderer).
    koine_web::launch();

    concierge.shutdown();
    host.shutdown();
}

/// `GET /api/status` over `std::net` with a 3 s limit: a TCP connect line and
/// the first line of the answer, or the error, as notes for the Host tab.
fn probe(url: &str) -> Vec<String> {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::time::Duration;

    let mut notes = Vec::new();
    let authority = url.trim_start_matches("http://").trim_end_matches('/');
    let addr: SocketAddr = match authority.parse() {
        Ok(addr) => addr,
        Err(error) => {
            notes.push(format!("probe: {authority} is not an address: {error}"));
            return notes;
        }
    };
    let mut stream = match TcpStream::connect_timeout(&addr, Duration::from_secs(3)) {
        Ok(stream) => {
            notes.push(format!("tcp connect {addr}: ok"));
            stream
        }
        Err(error) => {
            notes.push(format!("tcp connect {addr}: {error}"));
            return notes;
        }
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
    let request =
        format!("GET /api/status HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    if let Err(error) = stream.write_all(request.as_bytes()) {
        notes.push(format!("GET /api/status: write: {error}"));
        return notes;
    }
    let mut answer = Vec::new();
    match stream.read_to_end(&mut answer) {
        Ok(_) => {
            let text = String::from_utf8_lossy(&answer);
            let first = text.lines().next().unwrap_or("").to_string();
            let body = text.split("\r\n\r\n").nth(1).unwrap_or("").trim();
            let body: String = body.chars().take(160).collect();
            notes.push(format!("GET /api/status: {first} {body}"));
        }
        Err(error) => notes.push(format!("GET /api/status: read: {error}")),
    }
    notes
}
