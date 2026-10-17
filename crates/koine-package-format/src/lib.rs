//! The koine package format (plan-koine-research-os P-D1..P-D3): a
//! directory with a `koine-package.json`, or the same as one bundle file, and
//! its digest — and nothing else. Reading, reviewing and installing a package
//! is `koine-packages`; a capability that only *carries* its package
//! (`pub fn package() -> Package`, docs/plan-koine-kit-moves.md KM-D5)
//! depends on this crate alone, so it does not link the package manager's
//! verbs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Why a package operation did not happen: a refusal code
/// (`invalid-argument`, `not-found`, …) and a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShippedError {
    pub code: &'static str,
    pub message: String,
}

impl ShippedError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ShippedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ShippedError {}

/// `sha256:<lowercase hex>` of `bytes`, the form every digest takes.
pub fn sha256(bytes: &[u8]) -> String {
    let hash = Sha256::digest(bytes);
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

/// A workflow the package ships.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowEntry {
    pub name: String,
    pub file: String,
}

/// An AI task category the package declares: an id the preferences file
/// assigns a model to (`<parent>.<leaf>` under a parent).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CategoryEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// A symbol name a GUI may show.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// The apps that offer it; empty is every app.
    #[serde(default)]
    pub apps: Vec<String>,
}

/// A category entry's problems: a malformed id, a leaf not under its parent.
pub fn category_errors(entry: &CategoryEntry) -> Vec<String> {
    let mut errors = Vec::new();
    let well_formed = !entry.id.is_empty()
        && entry
            .id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.');
    if !well_formed {
        errors.push(format!(
            "category id '{}' must be lowercase letters, digits, '-' and '.'",
            entry.id
        ));
    }
    if let Some(parent) = &entry.parent {
        if !entry.id.starts_with(&format!("{parent}.")) {
            errors.push(format!(
                "category '{}' is under '{parent}', so its id starts '{parent}.'",
                entry.id
            ));
        }
    }
    if entry.name.trim().is_empty() {
        errors.push(format!("category '{}' has no name", entry.id));
    }
    errors
}

pub const MANIFEST: &str = "koine-package.json";
/// An installed package.
pub const PACKAGE: &str = "koine/package@1.0.0";
/// The manifest format this build reads.
pub const FORMAT: &str = "1.0";
/// A package as one file (plan-koine-research-os P-D5): what a registry
/// serves. `<name>-<version>.koinepkg.json`.
pub const BUNDLE_SUFFIX: &str = ".koinepkg.json";
/// The bundle format this build reads.
pub const BUNDLE_FORMAT: &str = "1.0";

/// A surface the package ships.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceEntry {
    pub name: String,
    pub file: String,
}

/// A skill the package ships.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillEntry {
    pub name: String,
    pub file: String,
}

/// What the package needs from the host.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requires {
    #[serde(default)]
    pub verbs: Vec<String>,
    #[serde(default)]
    pub secrets: Vec<String>,
}

/// `koine-package.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub package: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub surfaces: Vec<SurfaceEntry>,
    #[serde(default)]
    pub skills: Vec<SkillEntry>,
    #[serde(default)]
    pub requires: Requires,
    /// Workflows it ships (P-D9), installed `proposed`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workflows: Vec<WorkflowEntry>,
    /// AI task categories it declares (P-D9).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ai_categories: Vec<CategoryEntry>,
}

impl Manifest {
    /// Every file the manifest names: surfaces, skills, workflows.
    pub fn named_files(&self) -> Vec<&String> {
        self.surfaces
            .iter()
            .map(|s| &s.file)
            .chain(self.skills.iter().map(|s| &s.file))
            .chain(self.workflows.iter().map(|w| &w.file))
            .collect()
    }
}

/// A package read whole: the manifest's bytes, the manifest, and every file
/// it names.
#[derive(Debug, Clone, PartialEq)]
pub struct Package {
    pub manifest_bytes: Vec<u8>,
    pub manifest: Manifest,
    pub files: BTreeMap<String, Vec<u8>>,
    /// Where it was read from (`builtin:<name>` for one built into the binary).
    pub source: String,
}

fn fail(code: &'static str, message: impl Into<String>) -> ShippedError {
    ShippedError::new(code, message)
}

/// A relative path inside the package: no root, no `..`, not empty.
fn checked(path: &str) -> Result<(), ShippedError> {
    let p = Path::new(path);
    let inside = !path.is_empty()
        && p.components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if inside {
        Ok(())
    } else {
        Err(fail(
            "invalid-argument",
            format!("'{path}' is not a relative path inside the package"),
        ))
    }
}

fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

impl Package {
    /// A package from its manifest and files (a built-in one, or a test's).
    pub fn from_parts(
        manifest_bytes: Vec<u8>,
        files: BTreeMap<String, Vec<u8>>,
        source: impl Into<String>,
    ) -> Result<Self, ShippedError> {
        let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| fail("invalid-argument", format!("{MANIFEST}: {e}")))?;
        if manifest.package != FORMAT {
            return Err(fail(
                "invalid-argument",
                format!(
                    "{MANIFEST}: format {} is not {FORMAT}, the one this build reads",
                    manifest.package
                ),
            ));
        }
        if !is_name(&manifest.name) {
            return Err(fail(
                "invalid-argument",
                format!(
                    "{MANIFEST}: name '{}' must be lowercase letters, digits and '-', \
                     starting with a letter",
                    manifest.name
                ),
            ));
        }
        let mut names = BTreeSet::new();
        for surface in &manifest.surfaces {
            if !names.insert(surface.name.clone()) {
                return Err(fail(
                    "invalid-argument",
                    format!("{MANIFEST}: two surfaces are named '{}'", surface.name),
                ));
            }
        }
        let mut workflows = BTreeSet::new();
        for workflow in &manifest.workflows {
            if !workflows.insert(workflow.name.clone()) {
                return Err(fail(
                    "invalid-argument",
                    format!("{MANIFEST}: two workflows are named '{}'", workflow.name),
                ));
            }
        }
        let mut categories = BTreeSet::new();
        for category in &manifest.ai_categories {
            if let Some(problem) = category_errors(category).into_iter().next() {
                return Err(fail("invalid-argument", format!("{MANIFEST}: {problem}")));
            }
            if !categories.insert(category.id.clone()) {
                return Err(fail(
                    "invalid-argument",
                    format!("{MANIFEST}: two categories have the id '{}'", category.id),
                ));
            }
        }
        let named = manifest.named_files();
        for file in &named {
            checked(file)?;
            if !files.contains_key(file.as_str()) {
                return Err(fail(
                    "not-found",
                    format!("{MANIFEST} names '{file}', which the package does not have"),
                ));
            }
        }
        Ok(Self {
            manifest_bytes,
            manifest,
            files,
            source: source.into(),
        })
    }

    /// Read a package directory: the manifest and the files it names.
    pub fn load(dir: &Path) -> Result<Self, ShippedError> {
        let manifest_bytes = std::fs::read(dir.join(MANIFEST)).map_err(|e| {
            fail(
                "not-found",
                format!("{}: {e}", dir.join(MANIFEST).display()),
            )
        })?;
        let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| fail("invalid-argument", format!("{MANIFEST}: {e}")))?;
        let mut files = BTreeMap::new();
        for file in manifest.named_files() {
            checked(file)?;
            let path = dir.join(file);
            // A symlink inside the package must not reach outside it.
            let real = path
                .canonicalize()
                .map_err(|e| fail("not-found", format!("{}: {e}", path.display())))?;
            let root = dir
                .canonicalize()
                .map_err(|e| fail("not-found", format!("{}: {e}", dir.display())))?;
            if !real.starts_with(&root) {
                return Err(fail(
                    "invalid-argument",
                    format!("'{file}' resolves outside the package ({})", real.display()),
                ));
            }
            let bytes = std::fs::read(&real)
                .map_err(|e| fail("not-found", format!("{}: {e}", path.display())))?;
            files.insert(file.clone(), bytes);
        }
        Self::from_parts(manifest_bytes, files, dir.display().to_string())
    }

    /// A package directory or a bundle file, whichever `path` is.
    pub fn load_path(path: &Path) -> Result<Self, ShippedError> {
        if path.is_file() {
            let bytes = std::fs::read(path)
                .map_err(|e| fail("not-found", format!("{}: {e}", path.display())))?;
            Self::from_bundle(&bytes, path.display().to_string())
        } else {
            Self::load(path)
        }
    }

    /// The package as one JSON file: `{"koinepkg": "1.0", "manifest": <the
    /// manifest's text>, "files": {<path>: <text>}}`. The manifest and files
    /// are carried as their exact text, so the bundle's package has the
    /// directory's digest. Refuses a file that is not UTF-8.
    pub fn to_bundle(&self) -> Result<Vec<u8>, ShippedError> {
        let text = |bytes: &[u8], what: &str| {
            String::from_utf8(bytes.to_vec())
                .map_err(|_| fail("invalid-argument", format!("{what} is not UTF-8 text")))
        };
        let mut files = serde_json::Map::new();
        for (path, bytes) in &self.files {
            files.insert(path.clone(), Value::String(text(bytes, path)?));
        }
        let bundle = json!({
            "koinepkg": BUNDLE_FORMAT,
            "manifest": text(&self.manifest_bytes, MANIFEST)?,
            "files": files,
        });
        Ok(serde_json::to_vec_pretty(&bundle).expect("a bundle serializes"))
    }

    /// A package from a bundle's bytes.
    pub fn from_bundle(bytes: &[u8], source: impl Into<String>) -> Result<Self, ShippedError> {
        let bundle: Value = serde_json::from_slice(bytes)
            .map_err(|e| fail("invalid-argument", format!("not a package bundle: {e}")))?;
        if bundle["koinepkg"] != json!(BUNDLE_FORMAT) {
            return Err(fail(
                "invalid-argument",
                format!(
                    "not a package bundle of format {BUNDLE_FORMAT} (koinepkg is {})",
                    bundle["koinepkg"]
                ),
            ));
        }
        let manifest = bundle["manifest"]
            .as_str()
            .ok_or_else(|| fail("invalid-argument", "the bundle has no manifest text"))?;
        let mut files = BTreeMap::new();
        if let Some(entries) = bundle["files"].as_object() {
            for (path, text) in entries {
                checked(path)?;
                let text = text.as_str().ok_or_else(|| {
                    fail(
                        "invalid-argument",
                        format!("bundle file {path} is not text"),
                    )
                })?;
                files.insert(path.clone(), text.as_bytes().to_vec());
            }
        }
        let package = Self::from_parts(manifest.as_bytes().to_vec(), files, source)?;
        // Only what the manifest names counts toward the digest.
        let named: BTreeSet<&String> = package.manifest.named_files().into_iter().collect();
        if let Some(extra) = package.files.keys().find(|p| !named.contains(p)) {
            return Err(fail(
                "invalid-argument",
                format!("the bundle carries {extra}, which its manifest does not name"),
            ));
        }
        Ok(package)
    }

    /// The file name a registry serves this package under.
    pub fn bundle_name(&self) -> String {
        format!(
            "{}-{}{BUNDLE_SUFFIX}",
            self.manifest.name, self.manifest.version
        )
    }

    /// SHA-256 over the manifest and every file it names, in path order:
    /// `koine-package.json\0<bytes>\0<path>\0<bytes>\0…` (P-D1).
    pub fn digest(&self) -> String {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MANIFEST.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&self.manifest_bytes);
        bytes.push(0);
        for (path, content) in &self.files {
            bytes.extend_from_slice(path.as_bytes());
            bytes.push(0);
            bytes.extend_from_slice(content);
            bytes.push(0);
        }
        sha256(&bytes)
    }

    /// One shipped surface's spec.
    pub fn surface_spec(&self, entry: &SurfaceEntry) -> Result<Value, ShippedError> {
        let bytes = &self.files[&entry.file];
        serde_json::from_slice(bytes)
            .map_err(|e| fail("invalid-spec", format!("{}: not JSON: {e}", entry.file)))
    }

    /// One shipped skill's text.
    pub fn skill_text(&self, entry: &SkillEntry) -> String {
        String::from_utf8_lossy(&self.files[&entry.file]).into_owned()
    }
}
