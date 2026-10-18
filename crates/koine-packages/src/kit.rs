//! The kit: the surfaces every koine host ships, as a package built into the
//! binary and installed at start through the same three-way path a package
//! from disk takes (plan-koine-research-os B-D4). A person's edits to them
//! survive an upgrade.

use std::collections::BTreeMap;

use serde_json::json;

use crate::package::{self, Package};
use crate::shipped::ShippedError;

/// The kit's package name.
pub const KIT: &str = "koine-kit";

/// The Packages surface: what is installed, each shipped surface's status,
/// diff, reset and take.
pub const PACKAGES_SURFACE: &str = include_str!("../surfaces/packages.surface.json");
pub const PACKAGES_SURFACE_NAME: &str = "Packages";

/// The Review surface (plan-koine-trust TR-D8): the calls waiting for the
/// person, their targets and reasons, approve or reject (once or for good),
/// what was decided, and the standing rules. Shipped only where the review
/// verbs are linked.
pub const REVIEW_SURFACE: &str = include_str!("../surfaces/review.surface.json");
pub const REVIEW_SURFACE_NAME: &str = "Review";

/// The kit as a package: `(name, file, spec)` of each surface.
pub fn surfaces() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            koine_notebook::NOTEBOOK_SURFACE_NAME,
            "surfaces/notebook.surface.json",
            koine_notebook::NOTEBOOK_SURFACE,
        ),
        (
            koine_agent::AGENT_DESK_SURFACE_NAME,
            "surfaces/agent-desk.surface.json",
            koine_agent::AGENT_DESK_SURFACE,
        ),
        (
            PACKAGES_SURFACE_NAME,
            "surfaces/packages.surface.json",
            PACKAGES_SURFACE,
        ),
        (
            REVIEW_SURFACE_NAME,
            "surfaces/review.surface.json",
            REVIEW_SURFACE,
        ),
    ]
}

/// The kit's surfaces whose every verb this host links (H-D10): a host
/// that does not link a part of the kit is not handed its surface, which
/// would render with nothing to call.
pub fn linked_surfaces() -> Vec<(&'static str, &'static str, &'static str)> {
    linked(surfaces())
}

/// The entries of `surfaces` whose spec parses and every verb it calls is
/// linked.
pub fn linked(
    surfaces: Vec<(&'static str, &'static str, &'static str)>,
) -> Vec<(&'static str, &'static str, &'static str)> {
    surfaces
        .into_iter()
        .filter(|(_, _, spec)| {
            serde_json::from_str::<serde_json::Value>(spec)
                .map(|spec| {
                    package::verbs_called(&spec)
                        .iter()
                        .all(|verb| impress_service_core::VerbDescriptor::find(verb).is_some())
                })
                .unwrap_or(false)
        })
        .collect()
}

/// The kit package: the surfaces of [`linked_surfaces`].
pub fn package() -> Package {
    let surfaces = linked_surfaces();
    let manifest = json!({
        "package": package::FORMAT,
        "name": KIT,
        "version": env!("CARGO_PKG_VERSION"),
        "description": "The surfaces every koine host ships: the lab notebook, the agent desk \
                        and the package manager.",
        "surfaces": surfaces.iter()
            .map(|(name, file, _)| json!({"name": name, "file": file}))
            .collect::<Vec<_>>(),
    });
    let files: BTreeMap<String, Vec<u8>> = surfaces
        .iter()
        .map(|(_, file, spec)| (file.to_string(), spec.as_bytes().to_vec()))
        .collect();
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).expect("the kit manifest");
    Package::from_parts(manifest_bytes, files, format!("builtin:{KIT}"))
        .expect("the kit is a well-formed package")
}

/// Install (or upgrade) the kit. A host calls this once the store, the
/// manifest and the surface runtime are in place.
pub async fn install() -> Result<package::Installed, ShippedError> {
    package::install(&package()).await
}

/// [`install`] from a thread that is not inside a Tokio runtime.
pub fn install_blocking() -> Result<package::Installed, ShippedError> {
    impress_service_core::runtime::block_on(install())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_surface_whose_verbs_are_not_linked_is_left_out() {
        crate::ensure_linked();
        let lonely = r#"{"surface": "1.0", "name": "Lonely", "state": {},
            "sources": {"x": {"verb": "no-such-service_nothing", "args": {}}},
            "root": {"text": "x"}}"#;
        let packages = r#"{"surface": "1.0", "name": "Mine", "state": {},
            "sources": {"x": {"verb": "package-service_list", "args": {}}},
            "root": {"text": "x"}}"#;
        let kept = linked(vec![
            ("Lonely", "lonely.json", lonely),
            ("Mine", "mine.json", packages),
            ("Broken", "broken.json", "not json"),
        ]);
        let names: Vec<&str> = kept.iter().map(|(name, _, _)| *name).collect();
        assert_eq!(names, ["Mine"]);
    }
}
