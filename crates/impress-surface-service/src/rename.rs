//! The rename pass (plan-verb-pipeline-and-transport.md P3b): rewrites verb
//! names in every stored `impress/ui/surface@1.0.0` spec against a
//! [`RenameTable`](impress_service_core::lifecycle::RenameTable), once per
//! table version.
//!
//! # Where a verb or view kind is named in a spec
//!
//! Table LC (plan-verb-pipeline-and-transport.md § Lifecycle measurements)
//! names four sites: `sources.*.verb` ([`impress_surface::spec::Source::Verb`]),
//! `call.verb` ([`impress_surface::spec::Action::Call`]), `open.view_kind`
//! ([`impress_surface::spec::Action::Open`]) — all rewritten here — and
//! `params[].kind`, which is a **record schema** kind (`"publication"`), not
//! a verb or a view kind, and is out of this pass's scope.
//!
//! This pass rewrites the spec's own JSON rather than walking the typed
//! [`impress_surface::spec::SurfaceSpec`] tree: `verb` and `view_kind` are
//! keys that appear only at those three sites in the vocabulary
//! (`docs/plan-agent-surfaces.md`, "The vocabulary (normative)"), so a plain
//! recursive walk of the stored JSON finds every one of them without a
//! second, mutable tree-walker alongside `impress_surface::spec`'s existing
//! read-only ones (`handler_pointers`, `walk_with_pointers`) — which return
//! `&Action`/`&Node`, not `&mut`, because nothing before this pass ever
//! needed to rewrite a spec in place.
//!
//! # System rows and user rows, the same way
//!
//! Exactly [`impress_layout_service::rename`]'s reasoning: a rename rewrites
//! every row that names the old verb, however it was authored, and nothing
//! is lost because [`crate::store::SurfaceStore::update`] is the same
//! attributed, `Durable` + `Editorial` write path a real `surface_update`
//! call makes — the previous spec text stays in the operation log.
//!
//! # Running once per version
//!
//! Same mechanism as the layout-service pass: the highest applied
//! [`RenameTable::version`](impress_service_core::lifecycle::RenameTable::version)
//! is recorded in `store_metadata` under [`MARKER_KEY`], via
//! [`AttributedStore::metadata_get`] /
//! `metadata_set`, so a second run of an already-applied table is a
//! no-op.

use std::sync::Arc;

use impress_item::item::ActorKind;
use impress_item::store::AttributedStore;
use impress_service_core::lifecycle::{
    DeprecatedProviderNames, DeprecatedReference, RenameTable, RenameVisitor,
};
use impress_service_core::Refusal;
use impress_surface::spec::{walk_actions, Action, Source, SurfaceSpec};

use crate::store::{Result, SurfaceStore};

/// The `store_metadata` key this pass records its applied version under.
pub const MARKER_KEY: &str = "lifecycle.rename.surface-service";

/// What one run of [`RenamePass::run`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenameReport {
    pub surfaces_visited: usize,
    pub surfaces_rewritten: usize,
    /// Read-only findings, including when no rename table has shipped or its
    /// version was applied earlier. No provider reference is rewritten.
    pub deprecated_provider_references: Vec<DeprecatedReference>,
}

/// The surface-service rename pass — the
/// [`RenameVisitor`](impress_service_core::lifecycle::RenameVisitor)
/// implementation this crate's one stored document kind (a surface spec)
/// provides. See `impress_service_core::lifecycle`'s module docs for the
/// H-P3-1 note this mirrors: a later workflow or scenario document kind adds
/// its own implementation elsewhere, without this one changing.
pub struct RenamePass {
    store: SurfaceStore,
}

impl RenameVisitor for RenamePass {
    fn kind(&self) -> &'static str {
        "surface"
    }
}

impl RenamePass {
    pub fn new(store: Arc<dyn AttributedStore>) -> Self {
        Self {
            store: SurfaceStore::new(store),
        }
    }

    /// Run `table` against this store if it has not already been applied at
    /// this version or later. Call once, at store open or service start.
    pub fn run_if_needed(&self, table: &RenameTable) -> Result<RenameReport> {
        let names =
            DeprecatedProviderNames::from_handles(impress_service_core::call::descriptors());
        self.run_if_needed_with(table, &names)
    }

    /// Same pass with an explicit inventory snapshot, useful for a host that
    /// already hydrated its registry and for isolated tests.
    pub fn run_if_needed_with(
        &self,
        table: &RenameTable,
        names: &DeprecatedProviderNames,
    ) -> Result<RenameReport> {
        if table.is_empty() {
            return Ok(RenameReport {
                deprecated_provider_references: self.report_deprecated_provider_refs_with(names)?,
                ..RenameReport::default()
            });
        }
        if self.applied_version()?.is_some_and(|v| v >= table.version) {
            return Ok(RenameReport {
                deprecated_provider_references: self.report_deprecated_provider_refs_with(names)?,
                ..RenameReport::default()
            });
        }
        let report = self.run_with(table, names)?;
        self.record_version(table.version)?;
        Ok(report)
    }

    /// Run `table` unconditionally, ignoring the recorded version. What the
    /// tests call directly, and what [`Self::run_if_needed`] calls once it
    /// has decided the table is not already applied.
    pub fn run(&self, table: &RenameTable) -> Result<RenameReport> {
        let names =
            DeprecatedProviderNames::from_handles(impress_service_core::call::descriptors());
        self.run_with(table, &names)
    }

    fn run_with(
        &self,
        table: &RenameTable,
        names: &DeprecatedProviderNames,
    ) -> Result<RenameReport> {
        let mut report = RenameReport::default();
        for row in self.store.list()? {
            report.surfaces_visited += 1;
            let mut json: serde_json::Value = serde_json::from_str(&row.spec_text)
                .map_err(|e| Refusal::store(format!("surface {} spec: {e}", row.id)))?;
            let mut changed = false;
            rewrite_json(&mut json, table, row.id, &mut changed);
            if !changed {
                continue;
            }
            let spec: SurfaceSpec = serde_json::from_value(json).map_err(|e| {
                Refusal::store(format!("surface {} spec after rename: {e}", row.id))
            })?;
            self.store
                .update(row.id, &spec, None, None, ActorKind::System)?;
            report.surfaces_rewritten += 1;
        }
        if report.surfaces_rewritten > 0 {
            tracing::info!(
                target: "surface",
                "rename pass v{}: rewrote {} surface row(s)",
                table.version,
                report.surfaces_rewritten
            );
        }
        report.deprecated_provider_references = self.report_deprecated_provider_refs_with(names)?;
        Ok(report)
    }

    /// Recheck current stored references after runtime provider changes,
    /// independently of the once-per-version rewrite marker. A dropped verb
    /// may appear long after the shipped table was marked applied.
    pub fn report_deprecated_provider_refs(&self) -> Result<Vec<DeprecatedReference>> {
        let names =
            DeprecatedProviderNames::from_handles(impress_service_core::call::descriptors());
        self.report_deprecated_provider_refs_with(&names)
    }

    pub fn report_deprecated_provider_refs_with(
        &self,
        names: &DeprecatedProviderNames,
    ) -> Result<Vec<DeprecatedReference>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let mut references = Vec::new();
        for row in self.store.list()? {
            let document_id = row.id.to_string();
            for (source_name, source) in &row.spec.sources {
                if let Source::Verb { verb, .. } = source {
                    if names.contains(verb) {
                        references.push(DeprecatedReference {
                            document_kind: "surface",
                            document_id: document_id.clone(),
                            pointer: format!("/sources/{}/verb", escape_pointer(source_name)),
                            verb: verb.clone(),
                        });
                    }
                }
            }
            for (pointer, action) in walk_actions(&row.spec.root) {
                if let Action::Call { verb, .. } = action {
                    if names.contains(verb) {
                        references.push(DeprecatedReference {
                            document_kind: "surface",
                            document_id: document_id.clone(),
                            pointer: format!("{pointer}/call/verb"),
                            verb: verb.clone(),
                        });
                    }
                }
            }
        }
        for reference in &references {
            tracing::warn!(
                target: "surface",
                "deprecated provider reference: {} {} {} -> {}",
                reference.document_kind,
                reference.document_id,
                reference.pointer,
                reference.verb,
            );
        }
        Ok(references)
    }

    fn applied_version(&self) -> Result<Option<u32>> {
        Ok(self
            .store
            .store()
            .metadata_get(MARKER_KEY)
            .map_err(|e| Refusal::store(format!("read rename marker: {e}")))?
            .and_then(|v| v.parse::<u32>().ok()))
    }

    fn record_version(&self, version: u32) -> Result<()> {
        self.store
            .store()
            .metadata_set(MARKER_KEY, &version.to_string())
            .map_err(|e| Refusal::store(format!("write rename marker: {e}")))
    }
}

fn escape_pointer(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

/// Recursively rewrite every `verb` and `view_kind` string key against
/// `table`. `verb` covers both `sources.*.verb` (a [`Source::Verb`]) and
/// `call.verb` (an [`Action::Call`]) — both spell the field the same way,
/// which is exactly why one walk finds both. `view_kind` covers `open`
/// actions. See the module docs for why a blind key match is the right
/// amount of machinery for this vocabulary.
///
/// [`Source::Verb`]: impress_surface::spec::Source::Verb
/// [`Action::Call`]: impress_surface::spec::Action::Call
fn rewrite_json(
    value: &mut serde_json::Value,
    table: &RenameTable,
    surface: impress_item::item::ItemId,
    changed: &mut bool,
) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(new_verb) = map
                .get("verb")
                .and_then(|v| v.as_str())
                .and_then(|old| table.rename_verb(old))
            {
                tracing::info!(
                    target: "surface",
                    "rename pass v{}: surface {} verb '{}' -> '{new_verb}'",
                    table.version,
                    surface,
                    map.get("verb").and_then(|v| v.as_str()).unwrap_or_default(),
                );
                map.insert(
                    "verb".to_string(),
                    serde_json::Value::String(new_verb.to_string()),
                );
                *changed = true;
            }
            if let Some(new_kind) = map
                .get("view_kind")
                .and_then(|v| v.as_str())
                .and_then(|old| table.rename_view_kind(old))
            {
                tracing::info!(
                    target: "surface",
                    "rename pass v{}: surface {} view kind '{}' -> '{new_kind}'",
                    table.version,
                    surface,
                    map.get("view_kind").and_then(|v| v.as_str()).unwrap_or_default(),
                );
                map.insert(
                    "view_kind".to_string(),
                    serde_json::Value::String(new_kind.to_string()),
                );
                *changed = true;
            }
            for v in map.values_mut() {
                rewrite_json(v, table, surface, changed);
            }
        }
        serde_json::Value::Array(items) => {
            for v in items.iter_mut() {
                rewrite_json(v, table, surface, changed);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use impress_item::item::ActorKind;
    use impress_service_core::lifecycle::RenameTable;
    use impress_surface::spec::SurfaceSpec;
    use koine_store_memory::MemoryStore;

    use super::*;

    /// A rename table used only by these tests — the shipped one stays
    /// empty (docs/plan-verb-pipeline-and-transport.md P3b).
    const TEST_TABLE: RenameTable = RenameTable {
        version: 1,
        verbs: &[("triage-service_a", "triage-service_b")],
        view_kinds: &[],
    };

    fn scratch_store() -> Arc<dyn AttributedStore> {
        Arc::new(MemoryStore::new())
    }

    fn spec_naming(verb: &str) -> SurfaceSpec {
        let json = format!(
            r#"{{
                "surface": "1.0",
                "name": "test surface",
                "sources": {{
                    "s": {{ "verb": "{verb}", "args": {{}} }}
                }},
                "root": {{ "column": [] }}
            }}"#
        );
        serde_json::from_str(&json).expect("valid spec")
    }

    #[test]
    fn a_seeded_surface_naming_verb_a_is_rewritten_to_b() {
        let store = scratch_store();
        let surfaces = SurfaceStore::new(store.clone());
        let spec = spec_naming("triage-service_a");
        let row = surfaces
            .create(&spec, None, &[], ActorKind::System)
            .expect("create");

        let pass = RenamePass::new(store);
        let report = pass.run(&TEST_TABLE).expect("run");
        assert_eq!(report.surfaces_rewritten, 1);

        let reloaded = surfaces.get(row.id).expect("get").expect("row exists");
        match reloaded.spec.sources.get("s") {
            Some(impress_surface::spec::Source::Verb { verb, .. }) => {
                assert_eq!(verb, "triage-service_b");
            }
            other => panic!("expected a Verb source, got {other:?}"),
        }
    }

    #[test]
    fn running_twice_is_a_no_op() {
        let store = scratch_store();
        let surfaces = SurfaceStore::new(store.clone());
        let spec = spec_naming("triage-service_a");
        surfaces
            .create(&spec, None, &[], ActorKind::System)
            .expect("create");

        let pass = RenamePass::new(store);
        let first = pass.run_if_needed(&TEST_TABLE).expect("first run");
        assert_eq!(first.surfaces_rewritten, 1);

        let names = DeprecatedProviderNames::from_names(["triage-service_b".into()]);
        let second = pass
            .run_if_needed_with(&TEST_TABLE, &names)
            .expect("second run");
        assert_eq!(second.surfaces_visited, 0);
        assert_eq!(second.surfaces_rewritten, 0);
        assert_eq!(second.deprecated_provider_references.len(), 1);
        assert_eq!(
            second.deprecated_provider_references[0].pointer,
            "/sources/s/verb"
        );
    }

    #[test]
    fn an_empty_table_is_a_no_op() {
        let store = scratch_store();
        let pass = RenamePass::new(store);
        let empty = RenameTable {
            version: 1,
            verbs: &[],
            view_kinds: &[],
        };
        let report = pass.run_if_needed(&empty).expect("run");
        assert_eq!(report, RenameReport::default());
    }

    #[test]
    fn a_surface_naming_neither_old_verb_is_left_alone() {
        let store = scratch_store();
        let surfaces = SurfaceStore::new(store.clone());
        let spec = spec_naming("unrelated-service_verb");
        surfaces
            .create(&spec, None, &[], ActorKind::System)
            .expect("create");

        let pass = RenamePass::new(store);
        let report = pass.run(&TEST_TABLE).expect("run");
        assert_eq!(report.surfaces_visited, 1);
        assert_eq!(report.surfaces_rewritten, 0);
    }

    #[test]
    fn dropped_provider_references_are_reported_without_rewriting_empty_table() {
        let store = scratch_store();
        let surfaces = SurfaceStore::new(store.clone());
        let spec: SurfaceSpec = serde_json::from_value(serde_json::json!({
            "surface": "1.0",
            "name": "dropped provider",
            "sources": {
                "data/name~": {
                    "verb": "julia-service_dropped",
                    "args": {"verb": "julia-service_dropped"}
                }
            },
            "root": {
                "button": {
                    "label": "Run",
                    "on_click": [{"call": {
                        "verb": "julia-service_dropped",
                        "args": {"verb": "julia-service_dropped"}
                    }}]
                }
            }
        }))
        .expect("valid surface");
        let row = surfaces
            .create(&spec, None, &[], ActorKind::System)
            .expect("create");
        let before = surfaces.get(row.id).unwrap().unwrap();
        let before_ops = store.operations_for(row.id, None).unwrap().len();
        let names = DeprecatedProviderNames::from_names(["julia-service_dropped".into()]);
        let empty = RenameTable {
            version: 0,
            verbs: &[],
            view_kinds: &[],
        };
        let pass = RenamePass::new(store.clone());
        let report = pass.run_if_needed_with(&empty, &names).expect("report");
        assert_eq!(report.surfaces_visited, 0);
        assert_eq!(report.surfaces_rewritten, 0);
        assert_eq!(report.deprecated_provider_references.len(), 2);
        assert_eq!(
            report.deprecated_provider_references[0],
            DeprecatedReference {
                document_kind: "surface",
                document_id: row.id.to_string(),
                pointer: "/sources/data~1name~0/verb".into(),
                verb: "julia-service_dropped".into(),
            }
        );
        assert_eq!(
            report.deprecated_provider_references[1].pointer,
            "/root/button/on_click/0/call/verb"
        );
        assert_eq!(
            pass.report_deprecated_provider_refs_with(&names).unwrap(),
            report.deprecated_provider_references,
        );
        let after = surfaces.get(row.id).unwrap().unwrap();
        assert_eq!(after.spec_text, before.spec_text);
        assert_eq!(after.revision, before.revision);
        assert_eq!(
            store.operations_for(row.id, None).unwrap().len(),
            before_ops
        );
    }
}
