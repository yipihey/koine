//! Registries (plan-koine-research-os P-D5..P-D8): where packages come from.
//!
//! A registry is an index file, `index.json`, beside the bundles it lists:
//!
//! ```json
//! { "registry": "1.0", "name": "koine-examples", "description": "…",
//!   "packages": [ { "name": "run-compare", "version": "0.1.0",
//!                   "description": "…", "bundle": "run-compare-0.1.0.koinepkg.json",
//!                   "digest": "sha256:…", "requires": { "verbs": [], "secrets": [] } } ] }
//! ```
//!
//! `bundle` is relative to the index's own location, or absolute. The index
//! pins each bundle's digest, so what [`fetch`] hands to the review is what
//! the registry's author published, and `package-service_install` pins it
//! again to what the person reviewed: index → review → install is one digest
//! end to end. An index is read from `https://`, `http://`, `file://` or a
//! plain path. The registries a host knows are `koine/registry@1.0.0` items.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::Duration;

use chrono::Utc;
use impress_item::{
    ActorKind, Item, ItemId, ItemQuery, Priority, SchemaRef, SortDescriptor, Value as ItemValue,
    Visibility,
};
use serde::{Deserialize, Serialize};

use crate::package::{self, Package, Requires};
use crate::shipped::{self, ShippedError};

/// A registry the host knows.
pub const REGISTRY: &str = "koine/registry@1.0.0";
/// The index format this build reads.
pub const INDEX_FORMAT: &str = "1.0";
/// The index's file name in a registry directory.
pub const INDEX: &str = "index.json";
/// The most a fetched index or bundle may be.
pub const MAX_BYTES: usize = 8 * 1024 * 1024;
/// How long a fetch may take.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

fn fail(code: &'static str, message: impl Into<String>) -> ShippedError {
    ShippedError::new(code, message)
}

/// One package version an index lists.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexEntry {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// Where the bundle is: relative to the index, or absolute.
    pub bundle: String,
    /// `sha256:<hex>` of the package (`Package::digest`).
    pub digest: String,
    #[serde(default)]
    pub requires: Requires,
}

/// `index.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub registry: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub packages: Vec<IndexEntry>,
}

impl Index {
    pub fn parse(bytes: &[u8]) -> Result<Self, ShippedError> {
        let index: Index = serde_json::from_slice(bytes)
            .map_err(|e| fail("invalid-argument", format!("not a registry index: {e}")))?;
        if index.registry != INDEX_FORMAT {
            return Err(fail(
                "invalid-argument",
                format!(
                    "registry index format {} is not {INDEX_FORMAT}, the one this build reads",
                    index.registry
                ),
            ));
        }
        Ok(index)
    }
}

/// Order two versions: numeric dot-separated parts compared as numbers, a
/// part that is not a number compared as text, a missing part as zero.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let parts = |v: &str| -> Vec<String> { v.split(['.', '-', '+']).map(str::to_string).collect() };
    let (pa, pb) = (parts(a), parts(b));
    for i in 0..pa.len().max(pb.len()) {
        let x = pa.get(i).map(String::as_str).unwrap_or("0");
        let y = pb.get(i).map(String::as_str).unwrap_or("0");
        let order = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            _ => x.cmp(y),
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    Ordering::Equal
}

fn has_scheme(location: &str) -> bool {
    location.starts_with("http://")
        || location.starts_with("https://")
        || location.starts_with("file://")
}

/// Where `bundle` is, read against the index at `index_url`.
pub fn resolve(index_url: &str, bundle: &str) -> String {
    if has_scheme(bundle) || bundle.starts_with('/') {
        return bundle.to_string();
    }
    match index_url.rfind('/') {
        Some(slash) => format!("{}/{bundle}", &index_url[..slash]),
        None => bundle.to_string(),
    }
}

/// The index location a registry URL names: the URL itself when it names a
/// `.json` file, else `<url>/index.json` (a registry directory).
pub fn index_location(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    if url.ends_with(".json") {
        url.to_string()
    } else {
        format!("{url}/{INDEX}")
    }
}

/// Read `location`: a URL over HTTP(S), a `file://` URL or a path.
pub async fn read(location: &str) -> Result<Vec<u8>, ShippedError> {
    if location.starts_with("http://") || location.starts_with("https://") {
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .build()
            .map_err(|e| fail("unavailable", e.to_string()))?;
        let response = client
            .get(location)
            .send()
            .await
            .map_err(|e| fail("unavailable", format!("{location}: {e}")))?;
        if !response.status().is_success() {
            return Err(fail(
                if response.status().as_u16() == 404 {
                    "not-found"
                } else {
                    "unavailable"
                },
                format!("{location}: HTTP {}", response.status()),
            ));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|e| fail("unavailable", format!("{location}: {e}")))?;
        if bytes.len() > MAX_BYTES {
            return Err(fail(
                "invalid-argument",
                format!("{location} is over {MAX_BYTES} bytes"),
            ));
        }
        return Ok(bytes.to_vec());
    }
    let path = location.strip_prefix("file://").unwrap_or(location);
    let bytes = std::fs::read(path).map_err(|e| fail("not-found", format!("{path}: {e}")))?;
    if bytes.len() > MAX_BYTES {
        return Err(fail(
            "invalid-argument",
            format!("{path} is over {MAX_BYTES} bytes"),
        ));
    }
    Ok(bytes)
}

/// A registry the host knows.
#[derive(Debug, Clone, PartialEq)]
pub struct Known {
    pub id: ItemId,
    pub url: String,
    pub name: String,
    pub added_at: String,
}

fn registry_id(url: &str) -> ItemId {
    let namespace = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"koine-registry");
    uuid::Uuid::new_v5(&namespace, index_location(url).as_bytes())
}

fn text(item: &Item, field: &str) -> String {
    match item.payload.get(field) {
        Some(ItemValue::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// The registries the host knows, oldest first.
pub fn known() -> Result<Vec<Known>, ShippedError> {
    let store = shipped::store(false)?;
    let items = store
        .query(&ItemQuery {
            schema: Some(SchemaRef::from_stored(REGISTRY.to_owned())),
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
        .map(|item| Known {
            id: item.id,
            url: text(item, "url"),
            name: text(item, "name"),
            added_at: text(item, "added_at"),
        })
        .collect())
}

/// Read and check the index at `url`.
pub async fn index(url: &str) -> Result<Index, ShippedError> {
    Index::parse(&read(&index_location(url)).await?)
}

/// Add the registry at `url` after reading its index. Adding one twice
/// answers the one already known.
pub async fn add(url: &str) -> Result<(Known, Index), ShippedError> {
    let url = url.trim();
    if url.is_empty() {
        return Err(fail("invalid-argument", "a registry needs a URL or a path"));
    }
    let index = index(url).await?;
    let store = shipped::store(true)?;
    let id = registry_id(url);
    if let Some(found) = known()?.into_iter().find(|k| k.id == id) {
        return Ok((found, index));
    }
    let now = Utc::now();
    let mut payload = std::collections::BTreeMap::new();
    payload.insert("url".to_string(), ItemValue::String(url.to_string()));
    payload.insert("name".to_string(), ItemValue::String(index.name.clone()));
    payload.insert(
        "added_at".to_string(),
        ItemValue::String(now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
    );
    store
        .insert(Item {
            id,
            schema: SchemaRef::from_stored(REGISTRY.to_owned()),
            payload,
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
        .map_err(|e| fail("store-error", e.to_string()))?;
    let found = known()?
        .into_iter()
        .find(|k| k.id == id)
        .ok_or_else(|| fail("store-error", "the registry was written but cannot be read"))?;
    Ok((found, index))
}

/// Forget the registry with this URL or name. Installed packages stay.
pub fn remove(url_or_name: &str) -> Result<Known, ShippedError> {
    let wanted = url_or_name.trim();
    let found = known()?
        .into_iter()
        .find(|k| k.url == wanted || k.name == wanted || k.id == registry_id(wanted))
        .ok_or_else(|| fail("not-found", format!("no registry {wanted}")))?;
    shipped::store(true)?
        .delete(found.id)
        .map_err(|e| fail("store-error", e.to_string()))?;
    Ok(found)
}

/// One package version found in a registry.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub registry: String,
    pub registry_url: String,
    pub entry: IndexEntry,
    /// The version installed now, if any.
    pub installed: Option<String>,
}

/// Every package version in every known registry whose name or description
/// contains `query` (all when empty), newest version first per name. A
/// registry that cannot be read is reported in the second list, not fatal.
pub async fn search(query: &str) -> Result<(Vec<Found>, Vec<String>), ShippedError> {
    let query = query.trim().to_lowercase();
    let installed: std::collections::BTreeMap<String, String> = package::installed()?
        .iter()
        .filter_map(|p| {
            Some((
                p.get("name")?.as_str()?.to_string(),
                p.get("version")?.as_str()?.to_string(),
            ))
        })
        .collect();
    let mut found = Vec::new();
    let mut errors = Vec::new();
    for registry in known()? {
        match index(&registry.url).await {
            Ok(index) => {
                for entry in index.packages {
                    let hay = format!("{} {}", entry.name, entry.description).to_lowercase();
                    if query.is_empty() || hay.contains(&query) {
                        found.push(Found {
                            registry: registry.name.clone(),
                            registry_url: registry.url.clone(),
                            installed: installed.get(&entry.name).cloned(),
                            entry,
                        });
                    }
                }
            }
            Err(e) => errors.push(format!("{}: {}", registry.url, e.message)),
        }
    }
    found.sort_by(|a, b| {
        a.entry
            .name
            .cmp(&b.entry.name)
            .then(compare_versions(&b.entry.version, &a.entry.version))
    });
    Ok((found, errors))
}

/// The newest version of `name` (or exactly `version`) across the known
/// registries.
pub async fn pick(name: &str, version: Option<&str>) -> Result<Found, ShippedError> {
    let (found, errors) = search("").await?;
    found
        .into_iter()
        .filter(|f| f.entry.name == name.trim())
        .find(|f| version.is_none_or(|v| f.entry.version == v.trim()))
        .ok_or_else(|| {
            let mut message = match version {
                Some(v) => format!("no registry lists {name} {v}"),
                None => format!("no registry lists {name}"),
            };
            if !errors.is_empty() {
                message.push_str(&format!(" (unreadable: {})", errors.join("; ")));
            }
            fail("not-found", message)
        })
}

static CACHE: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Where fetched bundles are kept (a host sets `<workspace>/packages`).
pub fn set_cache_dir(dir: PathBuf) {
    if let Ok(mut slot) = CACHE.write() {
        *slot = Some(dir);
    }
}

/// The bundle cache: what [`set_cache_dir`] said, else a temporary directory.
pub fn cache_dir() -> PathBuf {
    CACHE
        .read()
        .ok()
        .and_then(|slot| slot.clone())
        .unwrap_or_else(|| std::env::temp_dir().join("koine-packages"))
}

/// Download `found`'s bundle, check it is the package the index pins (its
/// name, version and digest), and keep it in the cache. Answers the package
/// and where it was kept, for the review and then `install`.
pub async fn fetch(found: &Found) -> Result<(Package, PathBuf), ShippedError> {
    let location = resolve(&index_location(&found.registry_url), &found.entry.bundle);
    let bytes = read(&location).await?;
    let package = Package::from_bundle(&bytes, location.clone())?;
    let digest = package.digest();
    if digest != found.entry.digest {
        return Err(fail(
            "digest-mismatch",
            format!(
                "{location} is {digest}, but the registry {} lists {}; not fetched",
                found.registry, found.entry.digest
            ),
        ));
    }
    if package.manifest.name != found.entry.name || package.manifest.version != found.entry.version
    {
        return Err(fail(
            "digest-mismatch",
            format!(
                "{location} is {} {}, but the registry lists it as {} {}",
                package.manifest.name,
                package.manifest.version,
                found.entry.name,
                found.entry.version
            ),
        ));
    }
    let dir = cache_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| fail("store-error", format!("{}: {e}", dir.display())))?;
    let path = dir.join(package.bundle_name());
    std::fs::write(&path, &bytes)
        .map_err(|e| fail("store-error", format!("{}: {e}", path.display())))?;
    Ok((package, path))
}

/// What publishing wrote.
#[derive(Debug, Clone, PartialEq)]
pub struct Published {
    pub bundle: PathBuf,
    pub entry: IndexEntry,
    pub index: Option<PathBuf>,
}

/// Bundle the package directory `dir` into `out` and, with `index`, add or
/// replace its entry there (creating the index named `registry_name` when
/// missing). `base_url` makes the entry's `bundle` absolute; without it the
/// entry names the bundle relative to the index.
pub fn publish(
    dir: &Path,
    out: &Path,
    index: Option<&Path>,
    base_url: Option<&str>,
    registry_name: &str,
) -> Result<Published, ShippedError> {
    let package = Package::load(dir)?;
    let bytes = package.to_bundle()?;
    std::fs::create_dir_all(out)
        .map_err(|e| fail("store-error", format!("{}: {e}", out.display())))?;
    let bundle = out.join(package.bundle_name());
    std::fs::write(&bundle, &bytes)
        .map_err(|e| fail("store-error", format!("{}: {e}", bundle.display())))?;
    let entry = IndexEntry {
        name: package.manifest.name.clone(),
        version: package.manifest.version.clone(),
        description: package.manifest.description.clone(),
        bundle: match base_url {
            Some(base) => format!("{}/{}", base.trim_end_matches('/'), package.bundle_name()),
            None => package.bundle_name(),
        },
        digest: package.digest(),
        requires: package.manifest.requires.clone(),
    };
    if let Some(index_path) = index {
        let mut index = match std::fs::read(index_path) {
            Ok(bytes) => Index::parse(&bytes)?,
            Err(_) => Index {
                registry: INDEX_FORMAT.into(),
                name: registry_name.into(),
                description: String::new(),
                packages: Vec::new(),
            },
        };
        index
            .packages
            .retain(|e| !(e.name == entry.name && e.version == entry.version));
        index.packages.push(entry.clone());
        index.packages.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then(compare_versions(&a.version, &b.version))
        });
        let mut text = serde_json::to_string_pretty(&index).expect("an index serializes");
        text.push('\n');
        std::fs::write(index_path, text)
            .map_err(|e| fail("store-error", format!("{}: {e}", index_path.display())))?;
    }
    Ok(Published {
        bundle,
        entry,
        index: index.map(Path::to_path_buf),
    })
}

/// The newest version each installed package has in a known registry, when
/// it is newer than the one installed: `(name, installed, newest)`.
pub async fn updates() -> Result<(Vec<(String, String, Found)>, Vec<String>), ShippedError> {
    let (found, errors) = search("").await?;
    let mut out = Vec::new();
    for f in &found {
        if let Some(installed) = &f.installed {
            let newest = found
                .iter()
                .filter(|g| g.entry.name == f.entry.name)
                .max_by(|a, b| compare_versions(&a.entry.version, &b.entry.version))
                .expect("f itself");
            if newest == f
                && compare_versions(&f.entry.version, installed) == Ordering::Greater
                && !out
                    .iter()
                    .any(|(n, _, _): &(String, String, Found)| *n == f.entry.name)
            {
                out.push((f.entry.name.clone(), installed.clone(), f.clone()));
            }
        }
    }
    Ok((out, errors))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_order_by_number_not_text() {
        assert_eq!(compare_versions("0.10.0", "0.9.1"), Ordering::Greater);
        assert_eq!(compare_versions("1.0", "1.0.0"), Ordering::Equal);
        assert_eq!(compare_versions("1.0.0-rc1", "1.0.0"), Ordering::Greater);
        assert_eq!(compare_versions("2", "10"), Ordering::Less);
    }

    #[test]
    fn a_bundle_is_read_against_its_index() {
        assert_eq!(
            resolve("https://x.org/reg/index.json", "a-1.koinepkg.json"),
            "https://x.org/reg/a-1.koinepkg.json"
        );
        assert_eq!(
            resolve("/srv/reg/index.json", "https://cdn/a.json"),
            "https://cdn/a.json"
        );
        assert_eq!(resolve("/srv/reg/index.json", "/abs/a.json"), "/abs/a.json");
        assert_eq!(
            index_location("https://x.org/reg/"),
            "https://x.org/reg/index.json"
        );
        assert_eq!(index_location("/srv/reg/mine.json"), "/srv/reg/mine.json");
    }
}
