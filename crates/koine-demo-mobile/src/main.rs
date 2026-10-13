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

    let host = koine_host::embed::start(koine_host::embed::sandbox_config(APP_ID))
        .expect("the host starts");

    // The lane (scripts/tests/ios/run.sh) and a person with `dx serve` read
    // these two lines: where the host is, and where its token is.
    let url = host.url();
    eprintln!("koine-demo-mobile: host {url}");
    eprintln!(
        "koine-demo-mobile: loopback token {}",
        host.token_path().display()
    );

    // The tour (docs/plan-koine-tour.md): the five surfaces and the notes,
    // seeded once, then the concierge on its own thread, speaking the same
    // verbs an agent off the phone would.
    let report = surface_demo_service::tour::seed_blocking();
    eprintln!(
        "koine-demo-mobile: tour {} created, {} existing, {} notes seeded",
        report.created.len(),
        report.existing.len(),
        report.notes_seeded
    );
    for error in &report.errors {
        eprintln!("koine-demo-mobile: tour: {error}");
    }
    let concierge = surface_demo_service::concierge::spawn_thread(
        surface_demo_service::concierge::ConciergeConfig::new(APP_ID),
    );

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
