//! The collections capability as a built-in package (KM-D5).

use std::collections::BTreeMap;

use koine_package_format::Package;
use serde_json::json;

/// The package's name; its source is `builtin:collections`.
pub const PACKAGE: &str = "collections";

/// The skill text the package carries.
pub const SKILL: &str = include_str!("../SKILL.md");

/// The verbs this capability registers, in the order the trait declares them.
pub const VERBS: [&str; 12] = [
    "collection-service_tree",
    "collection-service_create",
    "collection-service_rename",
    "collection-service_reparent",
    "collection-service_reorder",
    "collection-service_delete",
    "collection-service_add-members",
    "collection-service_remove-members",
    "collection-service_member-counts",
    "collection-service_migration-status",
    "collection-service_migrate",
    "collection-service_rollback",
];

/// The collections capability as a built-in package: one skill, and
/// `requires.verbs` naming its own verbs.
pub fn package() -> Package {
    let manifest = json!({
        "package": koine_package_format::FORMAT,
        "name": PACKAGE,
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Nested collections of any items: create, move, file and count.",
        "skills": [{"name": PACKAGE, "file": "SKILL.md"}],
        "requires": {"verbs": VERBS},
    });
    let files: BTreeMap<String, Vec<u8>> =
        BTreeMap::from([("SKILL.md".to_string(), SKILL.as_bytes().to_vec())]);
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).expect("the collections manifest");
    Package::from_parts(manifest_bytes, files, format!("builtin:{PACKAGE}"))
        .expect("the collections package is well-formed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use impress_service_core::VerbDescriptor;

    #[test]
    fn the_package_is_valid_and_its_digest_is_stable() {
        let a = package();
        let b = package();
        assert_eq!(a.digest(), b.digest());
        assert_eq!(a.source, "builtin:collections");
        assert_eq!(a.manifest.name, "collections");
        assert_eq!(a.manifest.skills.len(), 1);
        assert!(a
            .skill_text(&a.manifest.skills[0])
            .contains("collection-service_tree"));
        assert_eq!(a.manifest.requires.verbs, VERBS);
    }

    #[test]
    fn every_required_verb_is_linked_here() {
        for verb in package().manifest.requires.verbs {
            assert!(
                VerbDescriptor::find(&verb).is_some(),
                "{verb} is not linked"
            );
        }
    }

    #[test]
    fn the_skill_names_every_verb() {
        for verb in VERBS {
            assert!(SKILL.contains(verb), "SKILL.md omits {verb}");
        }
    }

    #[tokio::test]
    async fn the_package_reviews_clean() {
        let review = koine_packages::package::review(&package()).await;
        assert_eq!(review["ok"], json!(true));
        assert_eq!(review["missing_verbs"], json!([]));
    }
}
