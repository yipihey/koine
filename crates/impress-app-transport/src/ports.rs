//! One port table, replacing the four copies the plan found (`impress-mcp`
//! `main.rs`, impel-tools, impress-ai-tools, impress-cli — TR-3) and every
//! `*-service-http` crate's own hard-coded `DEFAULT_BASE_URL` const (TR-1).
//!
//! The authoritative table is Swift's — `SiblingApp.descriptors` in
//! `packages/ImpressKit/Sources/ImpressKit/SiblingApp.swift` (CLAUDE.md:
//! "There is exactly ONE authoritative port table"). This is a second,
//! necessary copy for the Rust side of the transport (Rust cannot read a
//! Swift source file at call time), pinned to it by
//! impress-apps' `implore-verbs-ffi/tests/transport_ports.rs` (the Swift
//! source is not in this repository) so the two cannot drift silently.

/// `app` (`"imbib"`, `"imprint"`, …) → its automation server's default port.
pub const PORTS: &[(&str, u16)] = &[
    ("imbib", 23120),
    ("imprint", 23121),
    ("impart", 23122),
    ("implore", 23123),
    ("impel", 23124),
    ("impress", 23125),
];

/// The default port for `app`, or `None` for an app not in [`PORTS`].
pub fn port_of(app: &str) -> Option<u16> {
    PORTS.iter().find(|(name, _)| *name == app).map(|(_, p)| *p)
}

/// `http://127.0.0.1:<port>` for `app`, honouring `IMPRESS_<APP>_HTTP_URL`
/// (uppercased, `-` → `_`) for a Tier B runner pointed at a non-default
/// port, the same override shape every `*-service-http` crate already
/// accepts (`IMBIB_HTTP_URL`, …).
pub fn base_url(app: &str) -> Option<String> {
    let env_key = format!("IMPRESS_{}_HTTP_URL", app.to_uppercase().replace('-', "_"));
    for key in [
        env_key,
        format!("{}_HTTP_URL", app.to_uppercase()),
        format!("{}_BASE_URL", app.to_uppercase()),
    ] {
        if let Ok(url) = std::env::var(&key) {
            if !url.trim().is_empty() {
                return Some(url);
            }
        }
    }
    port_of(app).map(|port| format!("http://127.0.0.1:{port}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_app_has_a_port() {
        for app in ["imbib", "imprint", "impart", "implore", "impel", "impress"] {
            assert!(port_of(app).is_some(), "{app} should have a port");
        }
    }

    #[test]
    fn an_unknown_app_has_no_port() {
        assert_eq!(port_of("not-an-app"), None);
    }

    #[test]
    fn the_env_override_wins() {
        std::env::set_var("IMPRESS_IMPLORE_HTTP_URL", "http://127.0.0.1:23181");
        assert_eq!(
            base_url("implore"),
            Some("http://127.0.0.1:23181".to_string())
        );
        std::env::remove_var("IMPRESS_IMPLORE_HTTP_URL");
        assert_eq!(
            base_url("implore"),
            Some("http://127.0.0.1:23123".to_string())
        );
    }
}
