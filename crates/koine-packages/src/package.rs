//! Packages (plan-koine-research-os P-D1..P-D3): a directory with a
//! `koine-package.json`, reviewed by its digest before it is installed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use chrono::Utc;
use impress_item::{
    ActorKind, Item, ItemId, ItemQuery, Priority, SchemaRef, SortDescriptor, Value as ItemValue,
    Visibility,
};
use impress_service_core::{SafetyClass, VerbDescriptor};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::digest::sha256;
use crate::shipped::{self, Outcome, ShippedError};

/// The manifest's file name.
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
        let named: Vec<&String> = manifest
            .surfaces
            .iter()
            .map(|s| &s.file)
            .chain(manifest.skills.iter().map(|s| &s.file))
            .collect();
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
        for file in manifest
            .surfaces
            .iter()
            .map(|s| &s.file)
            .chain(manifest.skills.iter().map(|s| &s.file))
        {
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
        let named: BTreeSet<&String> = package
            .manifest
            .surfaces
            .iter()
            .map(|s| &s.file)
            .chain(package.manifest.skills.iter().map(|s| &s.file))
            .collect();
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

/// Every verb name a spec calls: `"verb": "<name>"` in a source or an action.
pub fn verbs_called(spec: &Value) -> BTreeSet<String> {
    fn walk(value: &Value, out: &mut BTreeSet<String>) {
        match value {
            Value::Object(map) => {
                // A verb name is `<service>_<method>`; a `verb` key in a
                // surface's own state (an empty field, a template) is not.
                if let Some(Value::String(name)) = map.get("verb") {
                    if name.contains('_') && !name.contains("{{") && !name.contains(' ') {
                        out.insert(name.clone());
                    }
                }
                for child in map.values() {
                    walk(child, out);
                }
            }
            Value::Array(items) => items.iter().for_each(|i| walk(i, out)),
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    walk(spec, &mut out);
    out
}

fn safety_word(class: SafetyClass) -> &'static str {
    match class {
        SafetyClass::ReadOnly => "read-only",
        SafetyClass::Mutating => "mutating",
        SafetyClass::Destructive => "destructive",
        SafetyClass::External => "external",
    }
}

/// What a person reviews before installing (P-D2).
pub async fn review(package: &Package) -> Value {
    let manifest = &package.manifest;
    let mut surfaces = Vec::new();
    let mut called: BTreeSet<String> = manifest.requires.verbs.iter().cloned().collect();
    let mut all_valid = true;
    for entry in &manifest.surfaces {
        let spec = match package.surface_spec(entry) {
            Ok(spec) => spec,
            Err(e) => {
                all_valid = false;
                surfaces.push(
                    json!({"name": entry.name, "file": entry.file, "valid": false,
                                     "problems": [e.message], "verbs": [], "would": "refuse"}),
                );
                continue;
            }
        };
        let (_, problems) = impress_surface::validate_json(&spec);
        let errors: Vec<String> = problems
            .iter()
            .filter(|p| p.severity == impress_surface::Severity::Error)
            .map(|p| format!("{}: {}", p.path, p.message))
            .collect();
        all_valid &= errors.is_empty();
        let verbs = verbs_called(&spec);
        called.extend(verbs.iter().cloned());
        let would = would(&manifest.name, &entry.name, &spec).await;
        surfaces.push(json!({
            "name": entry.name, "file": entry.file, "valid": errors.is_empty(),
            "problems": errors, "verbs": verbs, "would": would,
        }));
    }
    let mut secrets: BTreeSet<String> = manifest.requires.secrets.iter().cloned().collect();
    let verbs: Vec<Value> = called
        .iter()
        .map(|name| match VerbDescriptor::find(name) {
            Some(verb) => {
                secrets.extend(verb.secrets.iter().map(|s| s.to_string()));
                json!({"name": name, "linked": true, "safety": safety_word(verb.safety.class),
                       "secrets": verb.secrets})
            }
            None => json!({"name": name, "linked": false, "safety": null, "secrets": []}),
        })
        .collect();
    let missing_verbs: Vec<&String> = called
        .iter()
        .filter(|name| VerbDescriptor::find(name).is_none())
        .collect();
    let missing_secrets: Vec<&String> = secrets
        .iter()
        .filter(|name| impress_service_core::secrets::get(name).is_none())
        .collect();
    json!({
        "ok": true,
        "name": manifest.name,
        "version": manifest.version,
        "description": manifest.description,
        "source": package.source,
        "digest": package.digest(),
        "valid": all_valid,
        "surfaces": surfaces,
        "skills": manifest.skills.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
        "verbs": verbs,
        "missing_verbs": missing_verbs,
        "missing_secrets": missing_secrets,
    })
}

/// What installing `spec` as `package`'s surface `name` would do now.
async fn would(package: &str, name: &str, spec: &Value) -> &'static str {
    let Ok(store) = shipped::store(false) else {
        return "create";
    };
    let Ok(Some(item)) = store.get(shipped::baseline_id(package, name)) else {
        return "create";
    };
    let baseline = shipped::Baseline::of(&item);
    if baseline.digest == crate::digest::spec_digest(spec) {
        return "keep (unchanged)";
    }
    match shipped::current(&baseline.surface_id).await {
        Ok(None) => "leave deleted",
        Ok(Some(copy)) if copy == baseline.spec => "update",
        Ok(Some(_)) => "keep yours; hold the new one as pending",
        Err(_) => "update",
    }
}

/// What one install did.
#[derive(Debug, Clone, PartialEq)]
pub struct Installed {
    pub name: String,
    pub version: String,
    pub digest: String,
    /// `(surface name, surface id, outcome)`.
    pub surfaces: Vec<(String, String, Outcome)>,
}

fn package_id(name: &str) -> ItemId {
    let namespace = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"koine-package");
    uuid::Uuid::new_v5(&namespace, name.as_bytes())
}

/// Install `package`. Every surface must validate, or nothing is written.
pub async fn install(package: &Package) -> Result<Installed, ShippedError> {
    let manifest = &package.manifest;
    let mut specs = Vec::new();
    for entry in &manifest.surfaces {
        let spec = package.surface_spec(entry)?;
        let (_, problems) = impress_surface::validate_json(&spec);
        if let Some(problem) = problems
            .iter()
            .find(|p| p.severity == impress_surface::Severity::Error)
        {
            return Err(fail(
                "invalid-spec",
                format!(
                    "{} ({}): {}: {}",
                    entry.name, entry.file, problem.path, problem.message
                ),
            ));
        }
        specs.push((entry, spec));
    }
    let mut surfaces = Vec::new();
    for (entry, spec) in specs {
        let (outcome, id) =
            shipped::install(&manifest.name, &manifest.version, &entry.name, &spec).await?;
        surfaces.push((entry.name.clone(), id, outcome));
    }
    let installed = Installed {
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        digest: package.digest(),
        surfaces,
    };
    record(package, &installed)?;
    Ok(installed)
}

fn record(package: &Package, installed: &Installed) -> Result<(), ShippedError> {
    let store = shipped::store(true)?;
    let manifest = &package.manifest;
    let payload: Value = json!({
        "name": manifest.name,
        "version": manifest.version,
        "description": manifest.description,
        "digest": installed.digest,
        "source": package.source,
        "installed_at": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "surfaces": installed.surfaces.iter()
            .map(|(name, id, _)| json!({"name": name, "surface_id": id}))
            .collect::<Vec<_>>(),
        "skills": manifest.skills.iter()
            .map(|s| json!({"name": s.name, "text": package.skill_text(s)}))
            .collect::<Vec<_>>(),
        "requires": manifest.requires,
    });
    let fields: BTreeMap<String, ItemValue> = payload
        .as_object()
        .expect("an object")
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                serde_json::from_value(v.clone()).unwrap_or(ItemValue::Null),
            )
        })
        .collect();
    let id = package_id(&manifest.name);
    let fail_store = |e: impress_item::StoreError| fail("store-error", e.to_string());
    if store.get(id).map_err(fail_store)?.is_some() {
        store.delete(id).map_err(fail_store)?;
    }
    let now = Utc::now();
    store
        .insert(Item {
            id,
            schema: SchemaRef::from_stored(PACKAGE.to_owned()),
            payload: fields,
            created: now,
            modified: now,
            author: shipped::AUTHOR.into(),
            author_kind: ActorKind::System,
            logical_clock: 0,
            origin: None,
            canonical_id: None,
            tags: Vec::new(),
            flag: None,
            is_read: false,
            is_starred: false,
            priority: Priority::Normal,
            visibility: Visibility::Private,
            message_type: None,
            produced_by: None,
            version: None,
            batch_id: None,
            references: Vec::new(),
            parent: None,
        })
        .map(|_| ())
        .map_err(fail_store)
}

/// Every installed package's record, oldest first.
pub fn installed() -> Result<Vec<serde_json::Map<String, Value>>, ShippedError> {
    let store = shipped::store(false)?;
    let items = store
        .query(&ItemQuery {
            schema: Some(SchemaRef::from_stored(PACKAGE.to_owned())),
            sort: vec![SortDescriptor {
                field: "created".into(),
                ascending: true,
            }],
            include_tags: false,
            include_references: false,
            ..Default::default()
        })
        .map_err(|e| fail("store-error", e.to_string()))?;
    Ok(items
        .iter()
        .map(|item| {
            item.payload
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::to_value(v).unwrap_or(Value::Null)))
                .collect()
        })
        .collect())
}

/// Remove `name`: its pristine surfaces are deleted; those the person
/// changed are kept, without a baseline (P-D3). Answers `(deleted, kept)`
/// surface ids.
pub async fn remove(name: &str) -> Result<(Vec<String>, Vec<String>), ShippedError> {
    let store = shipped::store(true)?;
    let id = package_id(name);
    if store
        .get(id)
        .map_err(|e| fail("store-error", e.to_string()))?
        .is_none()
    {
        return Err(fail("not-found", format!("no package {name} is installed")));
    }
    let mut deleted = Vec::new();
    let mut kept = Vec::new();
    for baseline in shipped::list(Some(name))? {
        match shipped::status(&baseline).await? {
            shipped::Status::Pristine => {
                shipped::delete(&baseline.surface_id).await?;
                deleted.push(baseline.surface_id.clone());
            }
            shipped::Status::Missing => {}
            _ => kept.push(baseline.surface_id.clone()),
        }
        shipped::forget(&baseline)?;
    }
    store
        .delete(id)
        .map_err(|e| fail("store-error", e.to_string()))?;
    Ok((deleted, kept))
}

/// The skills of every installed package, `(name, text)`.
pub fn installed_skills() -> Vec<(String, String)> {
    installed()
        .unwrap_or_default()
        .iter()
        .flat_map(|p| {
            p.get("skills")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .filter_map(|s| {
            Some((
                s.get("name")?.as_str()?.to_string(),
                s.get("text")?.as_str()?.to_string(),
            ))
        })
        .collect()
}
