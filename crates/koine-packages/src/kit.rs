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
    ]
}

/// The kit package.
pub fn package() -> Package {
    let surfaces = surfaces();
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
