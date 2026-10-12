//! The rename pass (plan-verb-pipeline-and-transport.md P3b): rewrites
//! `pane.view_kind` in every stored `impress/ui/layout@1.0.0` row and
//! `impress/ui/preset@1.0.0` row against a
//! [`RenameTable`](impress_service_core::lifecycle::RenameTable), once per
//! table version.
//!
//! # Why only view kinds
//!
//! Table LC (plan-verb-pipeline-and-transport.md § Lifecycle measurements)
//! found no verb name anywhere in a stored layout or preset: a pane's
//! `query.kinds` names a record schema (`"publication"`), not a verb, and
//! the tree crate's own "verb" (`impress_layout::Verb`) is an in-memory
//! gesture that is never persisted as a string. `impress-surface-service`'s
//! pass is the one that rewrites verb names, against `sources.*.verb` and
//! `call.verb` in a stored surface spec.
//!
//! # System rows and user rows, the same way
//!
//! Unlike [`crate::presets::PresetStore::upgrade_if_untouched`], this pass
//! does not ask whether a row still matches what the suite shipped: a rename
//! rewrites the string wherever it finds it, system-seeded or user-edited,
//! because an old view-kind name must stop appearing in a user's own layout
//! too. Nothing is lost by doing this to a user's row: every write goes
//! through [`impress_core::sqlite_store::SqliteItemStore::apply_operation`],
//! the same path [`crate::store::LayoutStore::save_named`] and
//! [`crate::presets::PresetStore::save`] use for a durable, attributed
//! commit — so the previous spelling stays exactly where every other edit's
//! previous value already lives, the operation log, and a rewrite is logged
//! at `info` same as `upgrade_if_untouched` logs its own rewrites.
//!
//! # Running once per version
//!
//! [`RenamePass::run_if_needed`] records the highest table
//! [`RenameTable::version`](impress_service_core::lifecycle::RenameTable::version)
//! it has applied in `store_metadata` (`MARKER_KEY`), via the store's new
//! [`AttributedStore::metadata_get`]/[`AttributedStore::metadata_set`]
//! — the same table `origin_id` and `impress_core::task_schema_migration`'s
//! marker already live in — so a second run of an already-applied table is a
//! no-op, and the pass is meant to be called at store open or service start.

use std::sync::Arc;

use impress_item::item::{ActorKind, Item};
use impress_item::operation::{OperationIntent, OperationSpec, OperationType, RetentionTier};
use impress_item::query::ItemQuery;
use impress_item::schema::refs::{
    IMPRESS_UI_LAYOUT as LAYOUT_SCHEMA_REF, IMPRESS_UI_PRESET as PRESET_SCHEMA_REF,
};
use impress_item::store::AttributedStore;
use impress_layout::{Layout, ViewKindId};
use impress_service_core::lifecycle::{RenameTable, RenameVisitor};
use impress_service_core::Refusal;

use crate::store::{author_for, field, layout_of, layout_value};

/// The `store_metadata` key this pass records its applied version under.
pub const MARKER_KEY: &str = "lifecycle.rename.layout-service";

/// What one run of [`RenamePass::run`] did — visited and rewritten counts per
/// document kind, for the caller to report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenameReport {
    pub layouts_visited: usize,
    pub layouts_rewritten: usize,
    pub presets_visited: usize,
    pub presets_rewritten: usize,
}

impl RenameReport {
    pub fn total_rewritten(&self) -> usize {
        self.layouts_rewritten + self.presets_rewritten
    }
}

/// The layout-service rename pass — the [`RenameVisitor`] implementation
/// this crate's stored document kinds (layouts, presets) provide. See the
/// module docs for `impress_service_core::lifecycle`'s H-P3-1 note: a later
/// workflow or scenario document kind adds its own implementation elsewhere,
/// without this one changing.
pub struct RenamePass {
    store: Arc<dyn AttributedStore>,
}

impl RenameVisitor for RenamePass {
    fn kind(&self) -> &'static str {
        "layout"
    }
}

impl RenamePass {
    pub fn new(store: Arc<dyn AttributedStore>) -> Self {
        Self { store }
    }

    /// Run `table` against this store if it has not already been applied at
    /// this version or later. Call once, at store open or service start.
    pub fn run_if_needed(&self, table: &RenameTable) -> crate::store::Result<RenameReport> {
        if table.is_empty() {
            return Ok(RenameReport::default());
        }
        if self.applied_version()?.is_some_and(|v| v >= table.version) {
            return Ok(RenameReport::default());
        }
        let report = self.run(table)?;
        self.record_version(table.version)?;
        Ok(report)
    }

    /// Run `table` unconditionally, ignoring the recorded version. What the
    /// tests call directly, and what [`Self::run_if_needed`] calls once it
    /// has decided the table is not already applied.
    pub fn run(&self, table: &RenameTable) -> crate::store::Result<RenameReport> {
        let mut report = RenameReport::default();
        let (visited, rewritten) = self.rewrite_kind(LAYOUT_SCHEMA_REF, table)?;
        report.layouts_visited = visited;
        report.layouts_rewritten = rewritten;
        let (visited, rewritten) = self.rewrite_kind(PRESET_SCHEMA_REF, table)?;
        report.presets_visited = visited;
        report.presets_rewritten = rewritten;
        if report.total_rewritten() > 0 {
            tracing::info!(
                target: "layout",
                "rename pass v{}: rewrote {} layout row(s) and {} preset row(s)",
                table.version,
                report.layouts_rewritten,
                report.presets_rewritten
            );
        }
        Ok(report)
    }

    fn applied_version(&self) -> crate::store::Result<Option<u32>> {
        Ok(self
            .store
            .metadata_get(MARKER_KEY)
            .map_err(|e| Refusal::store(format!("read rename marker: {e}")))?
            .and_then(|v| v.parse::<u32>().ok()))
    }

    fn record_version(&self, version: u32) -> crate::store::Result<()> {
        self.store
            .metadata_set(MARKER_KEY, &version.to_string())
            .map_err(|e| Refusal::store(format!("write rename marker: {e}")))
    }

    /// Rewrite every row of one schema (`impress/ui/layout@1.0.0` or
    /// `impress/ui/preset@1.0.0`): both store their tree under the same
    /// `layout` payload field, so one code path visits either. A preset row
    /// that inherits its tree (no `layout` field) fails to decode and is
    /// skipped — it has no view-kind names of its own to rewrite; the
    /// shipped preset it derives from carries its own row, visited on its
    /// own turn.
    fn rewrite_kind(
        &self,
        schema_ref: impress_item::SchemaRef,
        table: &RenameTable,
    ) -> crate::store::Result<(usize, usize)> {
        let items = self
            .store
            .query(&ItemQuery {
                schema: Some(schema_ref.clone()),
                include_tags: false,
                include_references: false,
                ..Default::default()
            })
            .map_err(|e| Refusal::store(format!("read {schema_ref} rows: {e}")))?;
        let mut visited = 0usize;
        let mut rewritten = 0usize;
        for item in items {
            visited += 1;
            let Ok(mut layout) = layout_of(&item) else {
                continue;
            };
            if !rewrite_view_kinds(&mut layout, table, &item) {
                continue;
            }
            self.write_tree(&item, &layout, table)?;
            rewritten += 1;
        }
        Ok((visited, rewritten))
    }

    fn write_tree(
        &self,
        item: &Item,
        layout: &Layout,
        table: &RenameTable,
    ) -> crate::store::Result<()> {
        let value = layout_value(layout)?;
        self.store
            .apply_operation(OperationSpec {
                target_id: item.id,
                op_type: OperationType::SetPayload(field::LAYOUT.to_string(), value),
                intent: OperationIntent::Editorial,
                reason: Some(format!(
                    "rename pass v{}: rewrite stored view-kind name(s)",
                    table.version
                )),
                batch_id: None,
                author: author_for(ActorKind::System),
                author_kind: ActorKind::System,
                retention: RetentionTier::Durable,
            })
            .map(|_| ())
            .map_err(|e| Refusal::store(format!("rename pass write {}: {e}", item.id)))
    }
}

/// Rewrite every pane's `view_kind` in `layout` against `table`. Returns
/// whether anything changed. Logged per pane, at `info`, so the rewrite of a
/// user's own row is visible the same way `upgrade_if_untouched` logs a
/// preset upgrade.
fn rewrite_view_kinds(layout: &mut Layout, table: &RenameTable, item: &Item) -> bool {
    let mut changed = false;
    for id in layout.panes() {
        let Some(pane) = layout.pane_mut(id) else {
            continue;
        };
        let Some(new_kind) = table.rename_view_kind(pane.view_kind.as_str()) else {
            continue;
        };
        tracing::info!(
            target: "layout",
            "rename pass v{}: row {} pane {:?} view kind '{}' -> '{}'",
            table.version,
            item.id,
            id,
            pane.view_kind,
            new_kind
        );
        pane.view_kind = ViewKindId::from(new_kind.to_string());
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use impress_item::item::ActorKind;
    use impress_layout::preset;
    use impress_service_core::lifecycle::RenameTable;

    use super::*;
    use crate::presets::PresetStore;
    use crate::store::{default_list_query, LayoutStore};

    /// A rename table used only by these tests — the shipped one stays
    /// empty (docs/plan-verb-pipeline-and-transport.md P3b).
    const TEST_TABLE: RenameTable = RenameTable {
        version: 1,
        verbs: &[],
        view_kinds: &[("a", "b")],
    };

    fn scratch_store() -> Arc<dyn AttributedStore> {
        Arc::new(koine_store_memory::MemoryStore::new())
    }

    fn layout_with_kind(kind: &str) -> Layout {
        preset::three_column(default_list_query(), ViewKindId::from(kind.to_string()))
    }

    #[test]
    fn a_seeded_row_naming_the_old_kind_is_rewritten() {
        let store = scratch_store();
        let layout_store = LayoutStore::new(store.clone());
        let seeded = layout_with_kind("a");
        let row = layout_store
            .save_named(
                "test-app",
                "Seeded",
                None,
                &seeded,
                ActorKind::System,
                "seed",
            )
            .expect("save");

        let pass = RenamePass::new(store);
        let report = pass.run(&TEST_TABLE).expect("run");
        assert_eq!(report.layouts_rewritten, 1);

        let (_, reloaded) = layout_store
            .load_named("test-app", &row.id.to_string())
            .expect("load")
            .expect("row exists");
        assert!(reloaded
            .panes()
            .iter()
            .all(|id| reloaded.pane(*id).unwrap().view_kind.as_str() != "a"));
        assert!(reloaded
            .panes()
            .iter()
            .any(|id| reloaded.pane(*id).unwrap().view_kind.as_str() == "b"));
    }

    #[test]
    fn an_edited_row_naming_the_old_kind_is_rewritten_and_logged() {
        let store = scratch_store();
        let layout_store = LayoutStore::new(store.clone());
        let edited = layout_with_kind("a");
        layout_store
            .save_named(
                "test-app",
                "Mine",
                None,
                &edited,
                ActorKind::Human,
                "the user's own",
            )
            .expect("save");

        let pass = RenamePass::new(store);
        let report = pass.run(&TEST_TABLE).expect("run");
        assert_eq!(report.layouts_rewritten, 1);

        let (_, reloaded) = layout_store
            .load_named("test-app", "Mine")
            .expect("load")
            .expect("row exists");
        assert!(reloaded
            .panes()
            .iter()
            .any(|id| reloaded.pane(*id).unwrap().view_kind.as_str() == "b"));
    }

    #[test]
    fn an_untouched_seeded_preset_is_upgraded() {
        let store = scratch_store();
        let preset_store = PresetStore::new(store.clone());
        let layout = layout_with_kind("a");
        preset_store
            .save(
                "test-app",
                "Seeded Preset",
                Some("a shipped preset"),
                &layout,
                &Default::default(),
                &Default::default(),
                Some(1),
                ActorKind::System,
                "seed",
            )
            .expect("save preset");

        let pass = RenamePass::new(store);
        let report = pass.run(&TEST_TABLE).expect("run");
        assert_eq!(report.presets_rewritten, 1);

        let (_, stored) = preset_store
            .load("test-app", "Seeded Preset")
            .expect("load")
            .expect("row exists");
        let layout = stored.layout.expect("layout present");
        assert!(layout
            .panes()
            .iter()
            .any(|id| layout.pane(*id).unwrap().view_kind.as_str() == "b"));
    }

    #[test]
    fn running_twice_is_a_no_op() {
        let store = scratch_store();
        let layout_store = LayoutStore::new(store.clone());
        let seeded = layout_with_kind("a");
        layout_store
            .save_named(
                "test-app",
                "Seeded",
                None,
                &seeded,
                ActorKind::System,
                "seed",
            )
            .expect("save");

        let pass = RenamePass::new(store);
        let first = pass.run_if_needed(&TEST_TABLE).expect("first run");
        assert_eq!(first.layouts_rewritten, 1);

        let second = pass.run_if_needed(&TEST_TABLE).expect("second run");
        assert_eq!(second, RenameReport::default());
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
}
