//! regex-lab on an iPhone: one process, the host in-process (loopback, an
//! ephemeral port, a `dir:` store under the app's Documents) and the
//! `koine-web` page in a WebView, handed the host's URL and bearer directly.
//! `dx build --platform ios` builds it with the `mobile` feature on; the
//! release workflow is `.github/workflows/release-regex-lab-ios.yml`.

const APP_ID: &str = "regex-lab";

fn main() {
    // Each keeps its crate's verbs through the link.
    koine_host::ensure_linked();
    regex_lab_service::ensure_linked();

    // A crash writes its cause to the workspace; the next launch shows it on
    // the Host tab (a phone has no terminal to read a panic from).
    let mut config = koine_host::embed::sandbox_config(APP_ID);
    if let Some(crash) = koine_host::embed::record_crashes(&config.workspace) {
        eprintln!("regex-lab-mobile: the last run crashed:\n{crash}");
        koine_web::add_note(format!("the last run crashed: {crash}"));
    }
    // The tester and the library, as the host binary installs them.
    config.packages.push(regex_lab_host::package::package());
    let host = koine_host::embed::start(config).expect("the host starts");

    let url = host.url();
    eprintln!("regex-lab-mobile: host {url}");
    eprintln!(
        "regex-lab-mobile: loopback token {}",
        host.token_path().display()
    );

    koine_web::install_link(koine_web::HostLink {
        origin: url,
        token: Some(host.token().to_string()),
        app_id: APP_ID.into(),
    });
    koine_web::launch();
    host.shutdown();
}
