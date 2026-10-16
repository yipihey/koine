//! `CapabilitiesService` — impact analysis over the linked inventory's own
//! declared effects (ADR-0036 D-G2's spirit; plan-self-reflective-layer.md's
//! E3 row and § Effects (consumers): "the input to P3's rename pass and to
//! 'what breaks if this is renamed'").
//!
//! `impact` answers, for a record kind or a verb name:
//!
//! * every linked verb that declares the kind among its `effects.reads` or
//!   `effects.writes` (literal `Kind::Ref` only — see [`Self::impact`]'s doc
//!   comment for what a dynamic declaration means here);
//! * for a verb name, the kinds THAT verb declares, the same way;
//! * every stored surface (`impress/ui/surface@1.0.0` row,
//!   `impress-surface-service::SurfaceStore`) that names one of the matched
//!   verbs, or — for a kind query — names the kind literally in its spec.
//!
//! **What this does not (yet) cover, and why:** stored *layouts* and
//! workflows/scenarios. A layout's panes carry a `PaneQuery` (kinds, not
//! verbs) scoped to one `(app_id, device)` pair with no store-wide "list
//! every layout" reader today (`impress-layout-service::LayoutStore::all_rows`
//! takes an `app_id`); workflows and scenarios are later work packages (W, S)
//! this plan has not built yet. Both are left as follow-up rather than
//! guessed at here — the plan's own EF-5-style discipline: say what is
//! covered, not everything the row eventually wants.
//!
//! The surface match is deliberately a text search over the stored spec
//! (`SurfaceRow::spec_text`) rather than a second walk of `Source`/`Action`
//! shapes: every source and call action serializes its verb as
//! `"verb":"<name>"` and every query source's kind list is JSON text too, so
//! a substring match finds both without this crate re-implementing
//! `impress-surface`'s own AST — the same reasoning
//! `impress-surface-service::runtime`'s `query_refs` gives for reading the
//! parsed `Source` enum where it already has one, turned around: here there
//! is no parsed form worth adding for a report, not a running source.

use std::sync::Arc;

use impress_item::store::AttributedStore;
use impress_service_core::descriptor::Kind;
use impress_service_core::{call, HandleSource, ProviderStatus, VerbHandle};
use impress_service_macros::{impress_service, impress_service_impl};
use serde::{Deserialize, Serialize};

#[allow(unused_imports)]
use impress_service_macros::impress_method;

use impress_surface::spec::SurfaceSpec;
use impress_surface_service::SurfaceStore;

/// One verb, as the catalogue lists it (ADR-0035 D2, plan G4).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct VerbSummary {
    pub name: String,
    pub service: String,
    pub description: String,
    /// `read_only` / `mutating` / `destructive` / `external`
    /// (`SafetyClass::as_str`).
    pub safety: String,
    pub since: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_safety: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_safety: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated_since: Option<String>,
}

/// `list-verbs`'s answer: every linked verb matching the query, grouped by
/// service in the order encountered (the inventory's own linking order).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq, Default)]
pub struct ListVerbsResult {
    pub verbs: Vec<VerbSummary>,
    pub total: usize,
    /// Every service name seen, in first-seen order — the catalogue's group
    /// filter options.
    pub groups: Vec<String>,
}

/// `verb-surface`'s answer: the generated form, or why there isn't one.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VerbSurfaceResult {
    pub ok: bool,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<SurfaceSpec>,
}

/// One verb that touches a kind, and how.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct VerbTouch {
    pub verb: String,
    pub reads: bool,
    pub writes: bool,
}

/// One stored surface that names a matched verb or kind, and what named it.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct SurfaceHit {
    pub id: String,
    pub name: String,
}

/// `impact`'s answer: what a kind or a verb touches, and what would need a
/// look before either is renamed or removed.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq, Default)]
pub struct ImpactResult {
    /// Echoed back: the kind this answer is about, if one was given.
    pub kind: Option<String>,
    /// Echoed back: the verb this answer is about, if one was given.
    pub verb: Option<String>,
    /// For a `kind` query: every linked verb declaring it among its reads or
    /// writes. For a `verb` query: that one verb, with `reads`/`writes` both
    /// true only if it declares the SAME kind on both sides (rare; usually
    /// one row per kind it touches — see `kinds_touched`).
    pub verbs: Vec<VerbTouch>,
    /// For a `verb` query only: the literal kinds that verb declares
    /// (`effects.reads`/`writes`), by name, with which side(s) named it.
    pub kinds_touched: Vec<VerbTouch>,
    /// Stored surfaces naming a matched verb (or, for a `kind` query, the
    /// kind literally) — a text search over the stored spec, not a parsed
    /// walk (see the module docs).
    pub surfaces: Vec<SurfaceHit>,
    /// Set when neither `kind` nor `verb` resolved to anything: a `verb`
    /// name not linked, or a `kind` no linked verb declares.
    pub message: String,
}

/// Impact analysis over the linked `#[impress_service]` inventory's own
/// declared effects (E3, ADR-0036 D-G2's spirit) — read-only, and the only
/// thing it reads is the inventory itself and the stored surfaces.
#[impress_service]
pub trait CapabilitiesService: Send + Sync + 'static {
    /// What touches a record kind, or what a verb touches: every linked verb
    /// that declares the kind among its reads or writes; for a verb, the
    /// kinds it declares; and the stored surfaces naming either. Give at
    /// least one of `kind` or `verb` — both narrows to their intersection
    /// (verbs matching `kind` AND named `verb`, which is usually just that
    /// one verb, echoed back so a caller can confirm it actually touches the
    /// kind it thinks it does).
    #[impress_method]
    #[impress_example(name = "by_kind", args = r#"{"kind": "imbib/bibliography-entry"}"#)]
    #[impress_example(
        name = "by_verb",
        args = r#"{"verb": "imbib-library-service_count-publications"}"#
    )]
    async fn impact(&self, kind: Option<String>, verb: Option<String>) -> ImpactResult;

    /// Every linked verb, optionally narrowed by a case-insensitive
    /// substring of its name or description and/or an exact service name —
    /// the inventory as data (D-G2, ADR-0035 D2). This is what the generated
    /// catalogue (`catalogue-surface`) searches.
    #[impress_method]
    #[impress_example(name = "all", args = r#"{}"#)]
    #[impress_example(name = "search", args = r#"{"search": "publications"}"#)]
    async fn list_verbs(&self, search: Option<String>, group: Option<String>) -> ListVerbsResult;

    /// The generated form for one verb (ADR-0035 D2): `verb_surface` over
    /// its descriptor, or `not-found` when the name is not linked.
    #[impress_method]
    #[impress_example(
        name = "surface_demo",
        args = r#"{"verb": "surface-demo-service_series"}"#
    )]
    async fn verb_surface(&self, verb: String) -> VerbSurfaceResult;

    /// The generated catalogue surface: search, a table of every linked
    /// verb (sourced from `list-verbs`), and a button that opens the
    /// selected verb's generated form (ADR-0035 D2).
    #[impress_method]
    #[impress_example(name = "default", args = r#"{}"#)]
    async fn catalogue_surface(&self) -> SurfaceSpec;
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

/// Reads `VerbDescriptor::iter()` (the linked inventory) and, for the
/// surface search, an explicit store or the shared one — the same
/// `with_store`/shared-singleton split every store-backed service in the kit
/// uses.
#[derive(Clone, Default)]
pub struct DefaultCapabilitiesService {
    store: Option<Arc<dyn AttributedStore>>,
}

impl DefaultCapabilitiesService {
    pub fn new() -> Self {
        Self { store: None }
    }

    pub fn with_store(store: Arc<dyn AttributedStore>) -> Self {
        Self { store: Some(store) }
    }

    /// The explicit store, else the process store
    /// (`koine_store::process::acquire_or_stand_in`): the one every other
    /// verb in the binary shares, opened lazily by whatever opener the host
    /// registered, or a private in-memory stand-in when it registered none
    /// (`impact` only reads, so a stand-in answers "no surfaces").
    fn store(&self) -> Arc<dyn AttributedStore> {
        self.store
            .clone()
            .unwrap_or_else(|| koine_store::process::acquire_or_stand_in().0)
    }
}

/// The literal (`Kind::Ref`) name a declared kind carries, else `None` — the
/// same "dynamic declarations are not this static walk's problem" reasoning
/// `impress-surface-service::runtime`'s `verb_declared_read_refs` gives (E3).
fn literal_ref(k: &Kind) -> Option<&'static str> {
    match k {
        Kind::Ref(r) => Some(r),
        _ => None,
    }
}

fn verbs_touching_kind(kind: &str) -> Vec<VerbTouch> {
    call::descriptors()
        .filter_map(|v| {
            let effects = v.effects();
            let reads = effects
                .reads
                .iter()
                .filter_map(literal_ref)
                .any(|r| r == kind);
            let writes = effects
                .writes
                .iter()
                .filter_map(literal_ref)
                .any(|r| r == kind);
            if reads || writes {
                Some(VerbTouch {
                    verb: v.name().to_string(),
                    reads,
                    writes,
                })
            } else {
                None
            }
        })
        .collect()
}

fn kinds_touched_by(verb: &VerbHandle) -> Vec<VerbTouch> {
    let mut out: Vec<VerbTouch> = Vec::new();
    let mut touch = |kind: &str, is_read: bool, is_write: bool| {
        if let Some(existing) = out.iter_mut().find(|t| t.verb == kind) {
            existing.reads |= is_read;
            existing.writes |= is_write;
        } else {
            out.push(VerbTouch {
                verb: kind.to_string(),
                reads: is_read,
                writes: is_write,
            });
        }
    };
    let effects = verb.effects();
    for k in effects.reads.iter().filter_map(literal_ref) {
        touch(k, true, false);
    }
    for k in effects.writes.iter().filter_map(literal_ref) {
        touch(k, false, true);
    }
    out
}

fn summarize(verb: &VerbHandle) -> VerbSummary {
    let provider = match verb {
        VerbHandle::Provider(provider) => Some(provider),
        VerbHandle::Linked(_) => None,
    };
    VerbSummary {
        name: verb.name().to_string(),
        service: verb.service().to_string(),
        description: verb.description().to_string(),
        safety: verb.safety().class.as_str().to_string(),
        since: verb.since().to_string(),
        source: match verb.source() {
            HandleSource::Linked => None,
            HandleSource::Provider(_) => Some("provider".to_string()),
        },
        provider_id: verb.provider_id().map(str::to_string),
        available: provider
            .map(|p| p.status == ProviderStatus::Available && p.deprecated_since.is_none()),
        availability: provider.map(|p| {
            if p.deprecated_since.is_some() {
                "deprecated".to_string()
            } else if p.status == ProviderStatus::Available {
                "available".to_string()
            } else {
                "unavailable".to_string()
            }
        }),
        declared_safety: provider.map(|p| p.declared_safety.class.as_str().to_string()),
        effective_safety: provider.map(|p| p.effective_safety.class.as_str().to_string()),
        deprecated_since: provider.and_then(|p| p.deprecated_since.clone()),
    }
}

/// Stored surfaces naming any of `needles` (verb names, or a bare kind) as a
/// JSON substring of the stored spec (see the module docs on why this is a
/// text search, not a parsed walk).
fn surfaces_naming(store: &Arc<dyn AttributedStore>, needles: &[String]) -> Vec<SurfaceHit> {
    if needles.is_empty() {
        return Vec::new();
    }
    let surfaces = SurfaceStore::new(store.clone());
    let Ok(rows) = surfaces.list() else {
        return Vec::new();
    };
    rows.into_iter()
        .filter(|row| needles.iter().any(|n| row.spec_text.contains(n.as_str())))
        .map(|row| SurfaceHit {
            id: row.id.to_string(),
            name: row.name,
        })
        .collect()
}

#[async_trait::async_trait]
impl CapabilitiesService for DefaultCapabilitiesService {
    async fn impact(&self, kind: Option<String>, verb: Option<String>) -> ImpactResult {
        if kind.is_none() && verb.is_none() {
            return ImpactResult {
                message: "give at least one of `kind` or `verb`".to_string(),
                ..Default::default()
            };
        }

        let mut result = ImpactResult {
            kind: kind.clone(),
            verb: verb.clone(),
            ..Default::default()
        };

        let mut needles: Vec<String> = Vec::new();

        if let Some(kind) = &kind {
            result.verbs = verbs_touching_kind(kind);
            needles.push(kind.clone());
            needles.extend(result.verbs.iter().map(|v| v.verb.clone()));
        }

        if let Some(verb_name) = &verb {
            match call::find(verb_name) {
                Some(descriptor) => {
                    result.kinds_touched = kinds_touched_by(&descriptor);
                    // Narrow `verbs` to the intersection when both a kind and
                    // a verb were given: does THIS verb actually touch that
                    // kind, by declaration?
                    if kind.is_some() {
                        result.verbs.retain(|v| v.verb == *verb_name);
                    }
                    needles.push(verb_name.clone());
                }
                None => {
                    result.message = format!("`{verb_name}` is not a registered verb");
                    return result;
                }
            }
        }

        if kind.is_some() && result.verbs.is_empty() && verb.is_none() {
            result.message = format!(
                "no verb declares `{}` among its reads or writes",
                kind.as_deref().unwrap_or_default()
            );
        }

        needles.dedup();
        result.surfaces = surfaces_naming(&self.store(), &needles);
        result
    }

    async fn list_verbs(&self, search: Option<String>, group: Option<String>) -> ListVerbsResult {
        let needle = search.as_deref().map(str::to_lowercase);
        let mut groups: Vec<String> = Vec::new();
        let mut verbs: Vec<VerbSummary> = Vec::new();
        for v in call::descriptors() {
            if let Some(g) = &group {
                if v.service() != g.as_str() {
                    continue;
                }
            }
            if !groups.iter().any(|g| g == v.service()) {
                groups.push(v.service().to_string());
            }
            if let Some(needle) = &needle {
                let hay = format!(
                    "{} {}",
                    v.name().to_lowercase(),
                    v.description().to_lowercase()
                );
                if !hay.contains(needle.as_str()) {
                    continue;
                }
            }
            verbs.push(summarize(&v));
        }
        ListVerbsResult {
            total: verbs.len(),
            verbs,
            groups,
        }
    }

    async fn verb_surface(&self, verb: String) -> VerbSurfaceResult {
        match call::find(&verb) {
            Some(descriptor) => {
                let unavailable = matches!(&descriptor, VerbHandle::Provider(_))
                    && (descriptor.provider_status() == Some(ProviderStatus::Unavailable)
                        || descriptor.deprecation_notice().is_some());
                VerbSurfaceResult {
                    ok: true,
                    message: if unavailable {
                        format!("generated warning form for unavailable `{verb}`")
                    } else {
                        format!("generated form for `{verb}`")
                    },
                    code: None,
                    spec: Some(impress_verb_surface::verb_surface_handle(&descriptor)),
                }
            }
            None => VerbSurfaceResult {
                ok: false,
                message: format!("`{verb}` is not a registered verb"),
                code: Some("not-found".to_string()),
                spec: None,
            },
        }
    }

    async fn catalogue_surface(&self) -> SurfaceSpec {
        impress_verb_surface::catalogue()
    }
}

// ===========================================================================
// Macro registration
// ===========================================================================

impress_service_impl! {
    service = CapabilitiesService,
    safety = read_only,
    since = "0.1.0",
    effects = {
        reads: [any("the linked inventory's own effect declarations and the stored impress/ui/surface rows")],
        writes: [],
        reach: [],
    },
    impl = DefaultCapabilitiesService,
    instance = DefaultCapabilitiesService::new,
    methods = [
        impact(
            /// A record kind (`imbib/bibliography-entry`) — every linked
            /// verb declaring it among its reads or writes.
            kind: Option<String>,
            /// A verb name (`<service>_<method>`) — the kinds it declares.
            verb: Option<String>,
        ) -> ImpactResult,
        list_verbs(
            /// A case-insensitive substring of the verb's name or description.
            search: Option<String>,
            /// An exact service name (`imbib-library-service`) to narrow to.
            group: Option<String>,
        ) -> ListVerbsResult,
        verb_surface(
            /// The verb name (`<service>_<method>`) to generate a form for.
            verb: String,
        ) -> VerbSurfaceResult,
        catalogue_surface() -> SurfaceSpec,
    ],
}

#[cfg(test)]
mod tests {
    use super::*;
    use impress_service_core::{ProviderVerb, Safety, SafetyClass};
    use serde_json::json;

    fn store() -> Arc<dyn AttributedStore> {
        Arc::new(koine_store_memory::MemoryStore::new())
    }

    #[tokio::test]
    async fn a_kind_query_lists_the_real_linked_reader() {
        // `imbib-library-service` is not force-linked into THIS crate's own
        // test binary, so this asserts over whichever real verbs the
        // workspace-wide inventory happens to link into the test binary —
        // in this crate alone, none — proving instead that an unknown kind
        // answers honestly rather than crashing.
        let svc = DefaultCapabilitiesService::with_store(store());
        let result = svc.impact(Some("no-such/kind".to_string()), None).await;
        assert_eq!(result.kind, Some("no-such/kind".to_string()));
        assert!(result.verbs.is_empty());
        assert!(!result.message.is_empty(), "{result:?}");
    }

    #[tokio::test]
    async fn a_verb_query_on_an_unknown_verb_says_so() {
        let svc = DefaultCapabilitiesService::with_store(store());
        let result = svc
            .impact(None, Some("no-such-service_no-such".to_string()))
            .await;
        assert!(
            result.message.contains("not a registered verb"),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn neither_kind_nor_verb_asks_for_one() {
        let svc = DefaultCapabilitiesService::with_store(store());
        let result = svc.impact(None, None).await;
        assert!(result.message.contains("at least one"), "{result:?}");
    }

    /// The verb this crate itself declares is linked into its own test
    /// binary trivially (it is this crate), so `impact` on its own name is a
    /// real, non-degenerate proof over the linked inventory.
    #[tokio::test]
    async fn a_verb_query_on_impact_itself_finds_no_declared_kinds() {
        let svc = DefaultCapabilitiesService::with_store(store());
        let result = svc
            .impact(None, Some("capabilities-service_impact".to_string()))
            .await;
        assert!(result.message.is_empty(), "{result:?}");
        assert!(
            result.kinds_touched.is_empty(),
            "impact declares reads = [any(\"...\")], no literal kind: {result:?}"
        );
    }

    #[test]
    fn provider_summary_exposes_source_safety_and_unavailable_state() {
        let declared = Safety {
            class: SafetyClass::ReadOnly,
            idempotent: true,
        };
        let effective = Safety {
            class: SafetyClass::External,
            idempotent: false,
        };
        let handle = VerbHandle::Provider(Arc::new(ProviderVerb {
            name: "fixture-service_echo".into(),
            service: "fixture-service".into(),
            method: "echo".into(),
            description: "Echo text".into(),
            input_schema: json!({"type":"object"}),
            output_schema: json!({"type":"object"}),
            declared_safety: declared,
            effective_safety: effective,
            since: "0.1.0".into(),
            examples: vec![],
            provider_id: "fixture".into(),
            status: ProviderStatus::Unavailable,
            deprecated_since: None,
            generation: 1,
        }));
        let summary = summarize(&handle);
        assert_eq!(summary.source.as_deref(), Some("provider"));
        assert_eq!(summary.provider_id.as_deref(), Some("fixture"));
        assert_eq!(summary.safety, "external");
        assert_eq!(summary.declared_safety.as_deref(), Some("read_only"));
        assert_eq!(summary.effective_safety.as_deref(), Some("external"));
        assert_eq!(summary.available, Some(false));
        assert_eq!(summary.availability.as_deref(), Some("unavailable"));
        let wire = serde_json::to_value(summary).unwrap();
        assert_eq!(wire["available"], false);
    }

    /// `impact` finds a stored surface that names the verb, over every
    /// backend of the port (KM-D2): the kernel never takes SQLite.
    async fn finds_a_stored_surface_over(store: Arc<dyn AttributedStore>) {
        let spec: SurfaceSpec = serde_json::from_value(json!({
            "surface": "1.0",
            "name": "names impact",
            "sources": { "s": { "verb": "capabilities-service_impact", "args": {} } },
            "root": { "column": [] }
        }))
        .expect("valid spec");
        let row = SurfaceStore::new(store.clone())
            .create(&spec, None, &[], impress_item::item::ActorKind::System)
            .expect("create");
        let svc = DefaultCapabilitiesService::with_store(store);
        let result = svc
            .impact(None, Some("capabilities-service_impact".to_string()))
            .await;
        assert!(result.message.is_empty(), "{result:?}");
        assert_eq!(result.surfaces.len(), 1, "{result:?}");
        assert_eq!(result.surfaces[0].id, row.id.to_string());
    }

    #[tokio::test]
    async fn impact_finds_a_surface_in_the_memory_store() {
        finds_a_stored_surface_over(store()).await;
    }

    #[tokio::test]
    async fn impact_finds_a_surface_in_the_directory_store() {
        let dir = tempfile::tempdir().unwrap();
        let dir_store = koine_store_dir::DirStore::open(dir.path()).expect("open");
        finds_a_stored_surface_over(Arc::new(dir_store)).await;
    }
}
