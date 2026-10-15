//! `package-service`: packages and shipped surfaces
//! (plan-koine-research-os B-D, P-D).

use std::path::PathBuf;

use impress_service_core::async_trait;
#[allow(unused_imports)]
use impress_service_macros::impress_method;
use impress_service_macros::{impress_service, impress_service_impl};
use serde_json::Value;

use crate::dto::*;
use crate::package::{self, Package};
use crate::registry;
use crate::shipped::{self, Baseline, ShippedError};

/// Characters of a value a diff row shows.
pub const DIFF_VALUE_MAX: usize = 200;

#[impress_service]
pub trait PackageService: Send + Sync + 'static {
    /// The installed packages, oldest first: name, version, digest, where
    /// each came from, and how many surfaces it shipped.
    #[impress_method(effects(reads = [any("koine/package@1.0.0")]))]
    #[impress_example(name = "default", args = r#"{}"#, expect = r#"{"ok": true}"#)]
    async fn list(&self) -> PackagesResult;

    /// Review a package directory before installing it: its digest (pass it
    /// to `install`), each surface validated with the verbs it calls and what
    /// installing would do to it, every verb with its safety and whether this
    /// host links it, and the verbs and secrets that are missing. Changes
    /// nothing. Refuses a directory with no `koine-package.json`.
    #[impress_method(effects(reads = [any("koine/shipped@1.0.0")], reach = [fs]))]
    #[impress_example(
        name = "missing",
        args = r#"{"path": "/nonexistent/koine-package"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn inspect(&self, path: String) -> ReviewResult;

    /// Install the package at `path`, exactly as reviewed: `digest` must be
    /// the one `inspect` answered (`digest-mismatch` otherwise, and nothing
    /// is written). Each surface is created, upgraded when the person's copy
    /// is the old shipped one, or kept with the new one pending when the
    /// person changed it. Refuses a package with an invalid surface.
    #[impress_method(
        safety = mutating,
        effects(
            reads = ["impress/ui/surface@1.0.0"],
            writes = ["impress/ui/surface@1.0.0", any("koine/shipped@1.0.0"), any("koine/package@1.0.0")],
            reach = [fs]
        )
    )]
    #[impress_example(
        name = "missing",
        args = r#"{"path": "/nonexistent/koine-package", "digest": "sha256:00"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn install(&self, path: String, digest: String) -> InstallResult;

    /// Remove an installed package: the surfaces it shipped that are as
    /// shipped are deleted; those the person changed are kept (without a
    /// baseline). Refuses a package that is not installed.
    #[impress_method(
        safety = destructive,
        effects(writes = ["impress/ui/surface@1.0.0", any("koine/shipped@1.0.0"), any("koine/package@1.0.0")])
    )]
    #[impress_example(
        name = "missing",
        args = r#"{"name": "no-such-package"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn remove(&self, name: String) -> RemoveResult;

    /// Every shipped surface and how the person's copy stands against what
    /// was shipped: `pristine`, `modified`, `diverged` (a newer shipped
    /// version is waiting) or `missing` (deleted).
    #[impress_method(effects(reads = ["impress/ui/surface@1.0.0", any("koine/shipped@1.0.0")]))]
    #[impress_example(name = "default", args = r#"{}"#, expect = r#"{"ok": true}"#)]
    async fn shipped_status(&self) -> ShippedStatusResult;

    /// What the person changed in a shipped surface (`id`: the surface's),
    /// as JSON pointers from the shipped spec to their copy.
    #[impress_method(effects(reads = ["impress/ui/surface@1.0.0", any("koine/shipped@1.0.0")]))]
    #[impress_example(
        name = "unknown",
        args = r#"{"id": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn shipped_diff(&self, id: String) -> DiffResult;

    /// Put the shipped spec back in place of the person's copy (recreating
    /// it if they deleted it). Supports `reversibly`: the undo is the surface
    /// verb that restores their copy.
    #[impress_method(
        safety = mutating,
        supports(reversibly),
        effects(writes = ["impress/ui/surface@1.0.0", any("koine/shipped@1.0.0")])
    )]
    #[impress_example(
        name = "unknown",
        args = r#"{"id": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn shipped_reset(&self, id: String) -> ShippedResult;

    /// Take the newer shipped version waiting for a diverged surface in place
    /// of the person's copy. Supports `reversibly`. Refuses a surface with
    /// nothing waiting (`conflict`).
    #[impress_method(
        safety = mutating,
        supports(reversibly),
        effects(writes = ["impress/ui/surface@1.0.0", any("koine/shipped@1.0.0")])
    )]
    #[impress_example(
        name = "unknown",
        args = r#"{"id": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn shipped_take(&self, id: String) -> ShippedResult;

    /// Keep the person's copy of a diverged surface; the waiting shipped
    /// version becomes the one a reset restores. Refuses a surface with
    /// nothing waiting (`conflict`).
    #[impress_method(
        safety = mutating,
        effects(writes = [any("koine/shipped@1.0.0")])
    )]
    #[impress_example(
        name = "unknown",
        args = r#"{"id": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn shipped_keep(&self, id: String) -> ShippedResult;

    /// The package registries this host knows: where `search` and `fetch`
    /// look (P-D5).
    #[impress_method(effects(reads = [any("koine/registry@1.0.0")]))]
    #[impress_example(name = "default", args = r#"{}"#, expect = r#"{"ok": true}"#)]
    async fn registries(&self) -> RegistriesResult;

    /// Add a package registry: an `index.json` (or a directory holding one)
    /// at an `https://`, `http://` or `file://` URL or a path. Its index is
    /// read and checked first; a registry that cannot be read is refused.
    #[impress_method(
        safety = mutating,
        effects(writes = [any("koine/registry@1.0.0")], reach = [network, fs])
    )]
    #[impress_example(
        name = "missing",
        args = r#"{"url": "/nonexistent/koine-registry"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn registry_add(&self, url: String) -> RegistriesResult;

    /// Forget a registry (by URL or name). Packages installed from it stay.
    #[impress_method(
        safety = mutating,
        effects(writes = [any("koine/registry@1.0.0")])
    )]
    #[impress_example(
        name = "missing",
        args = r#"{"url": "no-such-registry"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn registry_remove(&self, url: String) -> RegistriesResult;

    /// Every package version the known registries list whose name or
    /// description contains `query` (all when absent), newest first per name,
    /// with the version installed now and whether this one would install,
    /// update or is older. Registries that cannot be read are listed in
    /// `unreadable`, not refused.
    #[impress_method(effects(reads = [any("koine/registry@1.0.0"), any("koine/package@1.0.0")], reach = [network, fs]))]
    #[impress_example(name = "default", args = r#"{}"#, expect = r#"{"ok": true}"#)]
    async fn search(&self, query: Option<String>) -> SearchResult;

    /// Download a package from a registry (`version` absent: the newest) and
    /// review it: the bundle must be the package the index pins (its digest,
    /// name and version; `digest-mismatch` otherwise), and the answer is the
    /// review `inspect` gives, with `path` and `digest` to pass to `install`.
    #[impress_method(effects(reads = [any("koine/registry@1.0.0"), any("koine/shipped@1.0.0")], reach = [network, fs]))]
    #[impress_example(
        name = "unknown",
        args = r#"{"name": "no-such-package"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn fetch(&self, name: String, version: Option<String>) -> ReviewResult;

    /// The installed packages a known registry has a newer version of.
    #[impress_method(effects(reads = [any("koine/registry@1.0.0"), any("koine/package@1.0.0")], reach = [network, fs]))]
    #[impress_example(name = "default", args = r#"{}"#, expect = r#"{"ok": true}"#)]
    async fn updates(&self) -> SearchResult;
}

#[derive(Clone, Default)]
pub struct DefaultPackageService;

impl DefaultPackageService {
    pub fn new() -> Self {
        Self
    }
}

fn path_of(path: &str) -> PathBuf {
    PathBuf::from(path.trim())
}

async fn row(baseline: &Baseline) -> ShippedRow {
    let status = shipped::status(baseline)
        .await
        .map(|s| s.word().to_string())
        .unwrap_or_else(|e| format!("unknown: {}", e.message));
    ShippedRow {
        surface_id: baseline.surface_id.clone(),
        package: baseline.package.clone(),
        name: baseline.name.clone(),
        version: baseline.version.clone(),
        status,
        pending_version: baseline.pending_version.clone(),
    }
}

macro_rules! refuse {
    ($ty:ident, $error:expr) => {{
        let error: ShippedError = $error;
        $ty {
            ok: false,
            code: Some(error.code.to_string()),
            message: Some(error.message),
            ..Default::default()
        }
    }};
}

async fn shipped_result(outcome: Result<Baseline, ShippedError>) -> ShippedResult {
    match outcome {
        Ok(baseline) => ShippedResult {
            ok: true,
            row: Some(row(&baseline).await),
            ..Default::default()
        },
        Err(e) => refuse!(ShippedResult, e),
    }
}

#[async_trait::async_trait]
impl PackageService for DefaultPackageService {
    async fn list(&self) -> PackagesResult {
        match package::installed() {
            Ok(all) => PackagesResult {
                ok: true,
                packages: all
                    .iter()
                    .map(|p| {
                        let text =
                            |f: &str| p.get(f).and_then(Value::as_str).unwrap_or("").to_string();
                        PackageRow {
                            name: text("name"),
                            version: text("version"),
                            digest: text("digest"),
                            source: text("source"),
                            installed_at: text("installed_at"),
                            description: text("description"),
                            surfaces: p
                                .get("surfaces")
                                .and_then(Value::as_array)
                                .map_or(0, |a| a.len() as u64),
                        }
                    })
                    .collect(),
                ..Default::default()
            },
            Err(e) => refuse!(PackagesResult, e),
        }
    }

    async fn inspect(&self, path: String) -> ReviewResult {
        let package = match Package::load_path(&path_of(&path)) {
            Ok(package) => package,
            Err(e) => return refuse!(ReviewResult, e),
        };
        review_of(&package, path.trim()).await
    }

    async fn install(&self, path: String, digest: String) -> InstallResult {
        let package = match Package::load_path(&path_of(&path)) {
            Ok(package) => package,
            Err(e) => return refuse!(InstallResult, e),
        };
        let actual = package.digest();
        if actual != digest.trim() {
            return refuse!(
                InstallResult,
                ShippedError::new(
                    "digest-mismatch",
                    format!(
                        "the package at {path} is {actual}, not the {digest} you reviewed; \
                         inspect it again"
                    )
                )
            );
        }
        match package::install(&package).await {
            Ok(installed) => InstallResult {
                ok: true,
                name: installed.name,
                version: installed.version,
                digest: installed.digest,
                surfaces: installed
                    .surfaces
                    .into_iter()
                    .map(|(name, surface_id, outcome)| InstallRow {
                        name,
                        surface_id,
                        outcome: outcome.word().into(),
                    })
                    .collect(),
                ..Default::default()
            },
            Err(e) => refuse!(InstallResult, e),
        }
    }

    async fn remove(&self, name: String) -> RemoveResult {
        match package::remove(name.trim()).await {
            Ok((deleted, kept)) => RemoveResult {
                ok: true,
                deleted,
                kept,
                ..Default::default()
            },
            Err(e) => refuse!(RemoveResult, e),
        }
    }

    async fn shipped_status(&self) -> ShippedStatusResult {
        match shipped::list(None) {
            Ok(all) => {
                let mut rows = Vec::with_capacity(all.len());
                for baseline in &all {
                    rows.push(row(baseline).await);
                }
                ShippedStatusResult {
                    ok: true,
                    rows,
                    ..Default::default()
                }
            }
            Err(e) => refuse!(ShippedStatusResult, e),
        }
    }

    async fn shipped_diff(&self, id: String) -> DiffResult {
        let baseline = match shipped::find(&id) {
            Ok(baseline) => baseline,
            Err(e) => return refuse!(DiffResult, e),
        };
        match shipped::changes(&baseline).await {
            Ok(changes) => DiffResult {
                ok: true,
                surface_id: baseline.surface_id,
                changes: changes
                    .into_iter()
                    .map(|c| ChangeRow {
                        shipped: crate::digest::shown(&c.before, DIFF_VALUE_MAX),
                        yours: crate::digest::shown(&c.after, DIFF_VALUE_MAX),
                        pointer: c.pointer,
                        op: c.op.into(),
                    })
                    .collect(),
                ..Default::default()
            },
            Err(e) => refuse!(DiffResult, e),
        }
    }

    async fn shipped_reset(&self, id: String) -> ShippedResult {
        shipped_result(shipped::reset(&id).await).await
    }

    async fn shipped_take(&self, id: String) -> ShippedResult {
        shipped_result(shipped::take(&id).await).await
    }

    async fn shipped_keep(&self, id: String) -> ShippedResult {
        shipped_result(shipped::keep(&id)).await
    }

    async fn registries(&self) -> RegistriesResult {
        match registry::known() {
            Ok(known) => RegistriesResult {
                ok: true,
                registries: known.iter().map(registry_row).collect(),
                ..Default::default()
            },
            Err(e) => refuse!(RegistriesResult, e),
        }
    }

    async fn registry_add(&self, url: String) -> RegistriesResult {
        match registry::add(&url).await {
            Ok((_, index)) => RegistriesResult {
                packages: Some(index.packages.len() as u64),
                ..self.registries().await
            },
            Err(e) => refuse!(RegistriesResult, e),
        }
    }

    async fn registry_remove(&self, url: String) -> RegistriesResult {
        match registry::remove(&url) {
            Ok(_) => self.registries().await,
            Err(e) => refuse!(RegistriesResult, e),
        }
    }

    async fn search(&self, query: Option<String>) -> SearchResult {
        match registry::search(query.as_deref().unwrap_or("")).await {
            Ok((found, unreadable)) => SearchResult {
                ok: true,
                rows: found.iter().map(found_row).collect(),
                unreadable,
                ..Default::default()
            },
            Err(e) => refuse!(SearchResult, e),
        }
    }

    async fn fetch(&self, name: String, version: Option<String>) -> ReviewResult {
        let version = version.filter(|v| !v.trim().is_empty());
        let found = match registry::pick(&name, version.as_deref()).await {
            Ok(found) => found,
            Err(e) => return refuse!(ReviewResult, e),
        };
        match registry::fetch(&found).await {
            Ok((package, path)) => review_of(&package, &path.display().to_string()).await,
            Err(e) => refuse!(ReviewResult, e),
        }
    }

    async fn updates(&self) -> SearchResult {
        match registry::updates().await {
            Ok((updates, unreadable)) => SearchResult {
                ok: true,
                rows: updates
                    .iter()
                    .map(|(_, _, found)| found_row(found))
                    .collect(),
                unreadable,
                ..Default::default()
            },
            Err(e) => refuse!(SearchResult, e),
        }
    }
}

/// The review of `package`, kept at `path`.
async fn review_of(package: &Package, path: &str) -> ReviewResult {
    let review = package::review(package).await;
    match serde_json::from_value::<ReviewResult>(review) {
        Ok(mut review) => {
            review.path = Some(path.to_string());
            review
        }
        Err(e) => refuse!(
            ReviewResult,
            ShippedError::new("internal", format!("the review did not fit its shape: {e}"))
        ),
    }
}

fn registry_row(known: &registry::Known) -> RegistryRow {
    RegistryRow {
        url: known.url.clone(),
        name: known.name.clone(),
        added_at: known.added_at.clone(),
    }
}

fn found_row(found: &registry::Found) -> FoundRow {
    use std::cmp::Ordering;
    let state = match &found.installed {
        None => "install",
        Some(installed) => match registry::compare_versions(&found.entry.version, installed) {
            Ordering::Greater => "update",
            Ordering::Equal => "installed",
            Ordering::Less => "older",
        },
    };
    FoundRow {
        name: found.entry.name.clone(),
        version: found.entry.version.clone(),
        description: found.entry.description.clone(),
        registry: found.registry.clone(),
        digest: found.entry.digest.clone(),
        installed: found.installed.clone(),
        state: state.into(),
    }
}

impress_service_impl! {
    service = PackageService,
    safety = read_only,
    since = "0.2.0",
    effects = {
        reads: [any("koine/package@1.0.0"), any("koine/shipped@1.0.0")],
        writes: [],
        reach: [],
    },
    impl = DefaultPackageService,
    instance = DefaultPackageService::new,
    methods = [
        /// The installed packages.
        list() -> PackagesResult,
        /// Review a package directory before installing it.
        inspect(
            /// The package directory (holding koine-package.json).
            path: String
        ) -> ReviewResult,
        /// Install a reviewed package.
        install(
            /// The package directory.
            path: String,
            /// The digest `inspect` answered.
            digest: String
        ) -> InstallResult,
        /// Remove an installed package; keep the surfaces the person changed.
        remove(
            /// The package's name.
            name: String
        ) -> RemoveResult,
        /// Every shipped surface and its status.
        shipped_status() -> ShippedStatusResult,
        /// What the person changed in a shipped surface.
        shipped_diff(
            /// The surface's id.
            id: String
        ) -> DiffResult,
        /// Put the shipped spec back.
        shipped_reset(
            /// The surface's id.
            id: String
        ) -> ShippedResult,
        /// Take the newer shipped version waiting.
        shipped_take(
            /// The surface's id.
            id: String
        ) -> ShippedResult,
        /// Keep the person's copy over the newer shipped version.
        shipped_keep(
            /// The surface's id.
            id: String
        ) -> ShippedResult,
        /// The package registries this host knows.
        registries() -> RegistriesResult,
        /// Add a package registry after reading its index.
        registry_add(
            /// The registry: an index.json, or a directory holding one; a URL or a path.
            url: String
        ) -> RegistriesResult,
        /// Forget a registry.
        registry_remove(
            /// The registry's URL or name.
            url: String
        ) -> RegistriesResult,
        /// Search the known registries.
        search(
            /// Words in a package's name or description; all packages when absent.
            query: Option<String>
        ) -> SearchResult,
        /// Download a package from a registry and review it.
        fetch(
            /// The package's name.
            name: String,
            /// The version; the newest when absent.
            version: Option<String>
        ) -> ReviewResult,
        /// Installed packages with a newer version in a registry.
        updates() -> SearchResult,
    ],
}
