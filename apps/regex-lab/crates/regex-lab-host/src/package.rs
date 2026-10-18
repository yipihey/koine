//! regex-lab's own package: the pages in `package/surfaces/` and the skill
//! in `package/SKILL.md`, built into the binary. The host installs it at
//! start (`Config::packages`), three-way like any package, so a person's
//! edit to a page survives an upgrade of the app.

use std::collections::BTreeMap;

use koine_packages::package::Package;

/// The manifest and every file it names, as `include_bytes!` read them.
const FILES: &[(&str, &[u8])] = &[
    (
        "surfaces/tester.surface.json",
        include_bytes!("../../../package/surfaces/tester.surface.json"),
    ),
    (
        "surfaces/library.surface.json",
        include_bytes!("../../../package/surfaces/library.surface.json"),
    ),
    ("SKILL.md", include_bytes!("../../../package/SKILL.md")),
];

const MANIFEST: &[u8] = include_bytes!("../../../package/koine-package.json");

/// The package. It is checked by this crate's tests, so a build that
/// compiles has one that loads.
pub fn package() -> Package {
    let files: BTreeMap<String, Vec<u8>> = FILES
        .iter()
        .map(|(name, bytes)| ((*name).to_string(), bytes.to_vec()))
        .collect();
    Package::from_parts(MANIFEST.to_vec(), files, "builtin:regex-lab")
        .expect("package/ is a valid package (see the tests)")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linked() {
        koine_host::ensure_linked();
        regex_lab_service::ensure_linked();
    }

    #[test]
    fn the_package_loads_and_names_its_two_pages() {
        let package = package();
        let names: Vec<&str> = package
            .manifest
            .surfaces
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(names, ["Regex tester", "Pattern library"]);
        assert_eq!(package.manifest.skills.len(), 1);
    }

    #[test]
    fn every_page_validates_clean() {
        let package = package();
        for entry in &package.manifest.surfaces {
            let spec = package.surface_spec(entry).unwrap();
            let (parsed, problems) = impress_surface::validate_json(&spec);
            assert!(
                parsed.is_some(),
                "{} does not parse: {problems:?}",
                entry.name
            );
            assert!(problems.is_empty(), "{}: {problems:#?}", entry.name);
        }
    }

    /// A page may only call verbs this binary links, and the manifest's
    /// `requires.verbs` is exactly what the pages call.
    #[test]
    fn every_verb_a_page_calls_is_linked_and_required() {
        linked();
        let package = package();
        let mut called = std::collections::BTreeSet::new();
        for entry in &package.manifest.surfaces {
            let spec = package.surface_spec(entry).unwrap();
            for verb in koine_packages::package::verbs_called(&spec) {
                assert!(
                    impress_service_core::VerbDescriptor::find(&verb).is_some(),
                    "{} calls {verb}, which is not linked",
                    entry.name
                );
                called.insert(verb);
            }
        }
        let required: std::collections::BTreeSet<String> =
            package.manifest.requires.verbs.iter().cloned().collect();
        assert_eq!(called, required);
    }
}
