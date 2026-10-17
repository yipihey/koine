//! The memory capability as a built-in package (KM-D5).

use std::collections::BTreeMap;

use koine_package_format::Package;
use serde_json::json;

/// The package's name; its source is `builtin:memory`.
pub const PACKAGE: &str = "memory";

/// The skill text the package carries.
pub const SKILL: &str = include_str!("../SKILL.md");

/// The verbs this capability registers, in the order the trait declares them.
pub const VERBS: [&str; 7] = [
    "memory-service_remember",
    "memory-service_recall",
    "memory-service_memory-brief",
    "memory-service_confirm-claim",
    "memory-service_supersede-claim",
    "memory-service_forget",
    "memory-service_memory-status",
];

/// The memory capability as a built-in package: one skill, and
/// `requires.verbs` naming its own verbs.
pub fn package() -> Package {
    let manifest = json!({
        "package": koine_package_format::FORMAT,
        "name": PACKAGE,
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Remember and recall claims, episodes and standing instructions across sessions.",
        "skills": [{"name": PACKAGE, "file": "SKILL.md"}],
        "requires": {"verbs": VERBS},
    });
    let files: BTreeMap<String, Vec<u8>> =
        BTreeMap::from([("SKILL.md".to_string(), SKILL.as_bytes().to_vec())]);
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).expect("the memory manifest");
    Package::from_parts(manifest_bytes, files, format!("builtin:{PACKAGE}"))
        .expect("the memory package is well-formed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use impress_service_core::VerbDescriptor;

    #[test]
    fn the_package_is_valid_and_its_digest_is_stable() {
        let a = package();
        assert_eq!(a.digest(), package().digest());
        assert_eq!(a.source, "builtin:memory");
        assert_eq!(a.manifest.skills.len(), 1);
        assert!(a
            .skill_text(&a.manifest.skills[0])
            .contains("memory-service_recall"));
        assert_eq!(a.manifest.requires.verbs, VERBS);
    }

    /// Every verb the package requires is the one this crate links, and
    /// every verb it links is required.
    #[test]
    fn it_requires_exactly_the_linked_memory_verbs() {
        crate::ensure_linked();
        let linked: Vec<&str> = VerbDescriptor::iter()
            .map(|d| d.name)
            .filter(|n| n.starts_with("memory-service_"))
            .collect();
        let mut linked_sorted = linked.clone();
        linked_sorted.sort_unstable();
        let mut required = VERBS.to_vec();
        required.sort_unstable();
        assert_eq!(linked_sorted, required);
    }

    #[tokio::test]
    async fn the_package_reviews_clean() {
        crate::ensure_linked();
        let review = koine_packages::package::review(&package()).await;
        assert_eq!(review["ok"], json!(true), "{review}");
        assert_eq!(review["missing_verbs"], json!([]));
    }
}
