//! Which device this is (ADR-0019 D2).
//!
//! The live arrangement is **device-scoped**: one `impress/ui/layout@1.0.0`
//! row per `(app_id, device)`, so a 27" iMac's arrangement never overwrites
//! the laptop's. That makes "which device am I" a value this crate has to be
//! able to produce, and the codebase does not yet have one to borrow.
//!
//! # What was considered, and why this is a placeholder
//!
//! The principled answer is `store_metadata.origin_id` — a UUID minted once
//! per store file, which `impress_core::backup` documents in as many words as
//! "which device/store this came from". It is exactly the right identity: it
//! survives a rename, it is already what sync stamps rows with, and there is
//! one per machine because there is one store file per machine.
//!
//! It is also `pub(crate)` on `SqliteItemStore` with no accessor, and widening
//! it is an `impress-core` edit that belongs with L4's owner, not here. So
//! this module resolves, in order:
//!
//! 1. `IMPRESS_DEVICE_ID` — the explicit override, honoured on every call so a
//!    test (or a headless daemon acting for a named device) can set it;
//! 2. the host name, looked up once and cached;
//! 3. `unknown-device`, so a machine with no resolvable name still gets ONE
//!    stable scope rather than a new one per launch.
//!
//! **When `origin_id` gains an accessor, this should become it**, with the
//! host name kept only as the human-readable label. Until then the tag is a
//! host name, and two machines that genuinely share one are one device as far
//! as the live row is concerned — which is a naming collision, not data loss:
//! the logical tree is portable, and the only thing that travels wrongly is
//! window geometry, which the projection filters anyway (ADR-0019 D2).

use std::sync::OnceLock;

/// The environment variable that overrides the device tag.
pub const DEVICE_ENV: &str = "IMPRESS_DEVICE_ID";

/// What is used when no name can be resolved at all.
pub const UNKNOWN_DEVICE: &str = "unknown-device";

/// The device tag for the live-layout scope. See the module docs.
pub fn current_device() -> String {
    if let Ok(explicit) = std::env::var(DEVICE_ENV) {
        let explicit = sanitize(&explicit);
        if !explicit.is_empty() {
            return explicit;
        }
    }
    host_name().to_string()
}

/// `device` as given, or [`current_device`] when it is absent or blank —
/// the rule every verb's `device: Option<String>` argument follows.
pub fn resolve_device(device: Option<&str>) -> String {
    match device.map(sanitize) {
        Some(d) if !d.is_empty() => d,
        _ => current_device(),
    }
}

fn host_name() -> &'static str {
    static HOST: OnceLock<String> = OnceLock::new();
    HOST.get_or_init(|| {
        // `hostname(1)` rather than a crate: it exists on macOS and Linux, the
        // answer is cached for the life of the process, and a device tag is
        // not worth a new dependency in a crate this close to the store.
        let resolved = std::process::Command::new("hostname")
            .output()
            .ok()
            .filter(|out| out.status.success())
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .map(|name| sanitize(&name))
            .filter(|name| !name.is_empty());
        resolved.unwrap_or_else(|| UNKNOWN_DEVICE.to_string())
    })
}

/// Trim, collapse whitespace, and keep the tag to something that reads well in
/// a payload field and a log line.
fn sanitize(raw: &str) -> String {
    raw.trim()
        .chars()
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .filter(|c| !c.is_control())
        .take(128)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, not three: these touch a process-wide environment variable,
    /// and `cargo test` runs test functions on parallel threads — three tests
    /// setting and clearing `IMPRESS_DEVICE_ID` would race each other into
    /// intermittent failures that look like a bug in the device rule.
    #[test]
    fn the_device_tag_resolves_override_then_argument_then_host() {
        std::env::set_var(DEVICE_ENV, "  studio imac  ");
        assert_eq!(current_device(), "studio-imac");
        // An explicit argument still wins over the ambient device.
        assert_eq!(resolve_device(Some("laptop")), "laptop");
        std::env::remove_var(DEVICE_ENV);

        // Blank is not a device: it falls back rather than writing a row whose
        // scope is the empty string, which nothing could ever find again.
        assert_eq!(resolve_device(Some("   ")), current_device());
        assert_eq!(resolve_device(None), current_device());

        // And whatever it resolved to is stable and non-empty.
        assert!(!current_device().is_empty());
        assert_eq!(current_device(), current_device());
    }
}
