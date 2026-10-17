//! `CollectionService` — the agent-facing twin of the collection kernel
//! (ADR-0022 D1/D5).
//!
//! One verb set over every collection hierarchy in the suite, parameterized by
//! a `binding` string that names which schema to operate on. The method names,
//! argument names and argument order mirror
//! `impress_store_ffi::SharedStore::collection_*` on purpose: Swift, the CLI
//! and an agent should say the same words for the same operation, because the
//! moment they diverge somebody has to keep a translation table in their head.
//!
//! Everything here delegates to `impress_core::collection_ops`. No logic lives
//! in this layer — it converts strings to bindings, kernel results to result
//! DTOs, and nothing else.

use std::sync::{Arc, RwLock};

use impress_item::{AttributedStore, StoreError};
use impress_service_core::async_trait;
use impress_service_macros::{impress_service, impress_service_impl};
use serde::{Deserialize, Serialize};

#[allow(unused_imports)]
use impress_service_macros::impress_method;

use crate::ops::{self, CollectionRow, CollectionSchemaBinding, GENERIC_COLLECTION};

/// One hierarchy an application registered under a name (and aliases).
struct Registered {
    names: Vec<String>,
    binding: CollectionSchemaBinding,
}

static BINDINGS: RwLock<Vec<Registered>> = RwLock::new(Vec::new());

/// Register an application's collection hierarchy under `names`: the first is
/// its primary name (listed in the error a wrong guess gets), the rest are
/// aliases. Names are matched trimmed and lowercased. Registering a name again
/// replaces the earlier registration, so the call is idempotent.
///
/// `generic` (the `collection@1.0.0` kernel schema) is built in; an app
/// registers the schemas it keeps its own rows in (`imbib`, `manuscript`,
/// `figure` in impress).
pub fn register_binding(names: &[&str], binding: CollectionSchemaBinding) {
    let names: Vec<String> = names
        .iter()
        .map(|n| n.trim().to_ascii_lowercase())
        .collect();
    let Some(primary) = names.first().cloned() else {
        return;
    };
    let mut all = BINDINGS.write().unwrap_or_else(|e| e.into_inner());
    all.retain(|r| r.names.first() != Some(&primary));
    all.push(Registered { names, binding });
}

/// Every accepted primary `binding` name, registered ones first and `generic`
/// last. Listed in error messages so a wrong guess is self-correcting.
pub fn binding_names() -> Vec<String> {
    let all = BINDINGS.read().unwrap_or_else(|e| e.into_inner());
    all.iter()
        .filter_map(|r| r.names.first().cloned())
        .chain(["generic".to_string()])
        .collect()
}

/// Resolve a `binding` argument to a kernel binding.
///
/// Aliases are deliberate: an agent that knows the record kind (`publication`)
/// or the stored schema (`imbib/collection`) should not have to learn a third
/// vocabulary to use the tool.
pub fn binding_for(name: &str) -> Result<CollectionSchemaBinding, String> {
    let key = name.trim().to_ascii_lowercase();
    if matches!(
        key.as_str(),
        "" | "generic" | "any" | "collection" | "impress"
    ) {
        return Ok(GENERIC_COLLECTION);
    }
    let all = BINDINGS.read().unwrap_or_else(|e| e.into_inner());
    if let Some(found) = all.iter().find(|r| r.names.contains(&key)) {
        return Ok(found.binding);
    }
    Err(format!(
        "unknown collection binding '{key}'. Use one of: {}.",
        binding_names().join(", ")
    ))
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// One flat collection row. Build the tree yourself from `parent_id`
/// (`null` = root); the kernel returns a flat list so callers can group,
/// filter and sort it their own way.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CollectionRowDto {
    /// Lowercase UUID string.
    pub id: String,
    pub name: String,
    /// Lowercase UUID of the PARENT COLLECTION, or null for a root. Never the
    /// owning library — that is the envelope parent and a different thing.
    pub parent_id: Option<String>,
    pub sort_order: i64,
    /// Record kind this collection organises ("publication", "manuscript",
    /// "any", …) for schemas that carry one; null for the per-kind schemas.
    pub kind_scope: Option<String>,
}

impl From<CollectionRow> for CollectionRowDto {
    fn from(row: CollectionRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            parent_id: row.parent_id,
            sort_order: row.sort_order,
            kind_scope: row.kind_scope,
        }
    }
}

/// Result of an operation that returns a single collection.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CollectionResult {
    pub ok: bool,
    pub collection: Option<CollectionRowDto>,
    pub message: String,
}

/// Result of a whole-tree read.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CollectionListResult {
    pub ok: bool,
    pub collections: Vec<CollectionRowDto>,
    pub message: String,
}

/// Result of a membership or delete operation.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CollectionMutationResult {
    pub ok: bool,
    /// How many members were actually added or removed. Zero for `delete`.
    pub applied: u32,
    pub message: String,
}

/// Member counts, aligned index-for-index with the requested ids.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MemberCountsResult {
    pub ok: bool,
    pub counts: Vec<u32>,
    pub message: String,
}

// --- Schema convergence (ADR-0022 WP G7) -----------------------------------

/// Rows still stored under one legacy collection schema.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LegacyCountDto {
    /// The legacy `schema_ref` (`imbib/collection`, `manuscript-collection`,
    /// `figure-collection`).
    pub schema_ref: String,
    /// The `kind_scope` these rows take on once migrated.
    pub kind_scope: String,
    pub rows: u64,
}

/// Generic `collection@1.0.0` rows sharing a `kind_scope`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct KindScopeCountDto {
    /// `"publication"`, `"manuscript"`, `"figure"`, `"any"`, …
    pub kind_scope: String,
    pub rows: u64,
    /// How many of them were produced by a migration. `rows - migrated` were
    /// created natively as `collection@1.0.0` and no rollback will touch them.
    pub migrated: u64,
}

/// Where a store sits on the convergence: the flag, and the actual row counts
/// on both sides of it.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MigrationStatusResult {
    pub ok: bool,
    /// The `collections.unified` flag. False on every store that has not been
    /// deliberately migrated — which is all of them by default.
    pub migrated: bool,
    pub legacy: Vec<LegacyCountDto>,
    pub generic: Vec<KindScopeCountDto>,
    pub message: String,
}

/// One legacy binding's share of a migration run.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BindingMigrationDto {
    pub schema_ref: String,
    pub kind_scope: String,
    /// Legacy rows found.
    pub found: u64,
    /// Rows rewritten — equal to `found`, or equal to what the real run WOULD
    /// rewrite when this was a dry run.
    pub rewritten: u64,
    /// Rows an earlier run already converged. Makes a re-run's zero legible.
    pub skipped_already_generic: u64,
}

/// What `migrate` did, or (dry run) would do.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MigrationReportResult {
    pub ok: bool,
    /// True when nothing was written.
    pub dry_run: bool,
    /// The flag's state BEFORE this run.
    pub was_migrated: bool,
    pub bindings: Vec<BindingMigrationDto>,
    /// Always true: the migration rewrites `schema_ref` and `payload` only.
    /// `Contains` edges and envelope parents are not part of it.
    pub membership_edges_untouched: bool,
    pub message: String,
}

/// Rows restored to one legacy `schema_ref`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RollbackCountDto {
    pub schema_ref: String,
    pub restored: u64,
}

/// What `rollback` restored.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RollbackReportResult {
    pub ok: bool,
    /// Rows restored, per originating legacy `schema_ref`.
    pub restored: Vec<RollbackCountDto>,
    /// Generic rows with no migration provenance: created natively as
    /// `collection@1.0.0`, counted and left alone.
    pub native_generic_untouched: u64,
    pub membership_edges_untouched: bool,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Collections and folders across the whole suite, through one verb set.
///
/// Every method takes `binding`, which selects the hierarchy:
/// `"imbib"` (publication collections), `"manuscript"` (imprint folders),
/// `"figure"` (implore folders) or `"generic"` (the mixed-kind
/// `collection@1.0.0` kernel schema, which accepts records of ANY kind).
#[impress_service]
pub trait CollectionService: Send + Sync + 'static {
    /// Every collection in one hierarchy, flat, ordered by `sort_order`.
    ///
    /// `binding` is `imbib` | `manuscript` | `figure` | `generic`. Build the
    /// tree from each row's `parent_id` (null = root). Start here: every other
    /// method takes ids this returns.
    #[impress_method(safety = read_only, effects(reads = ["collection", "imbib/collection", "manuscript-collection", "figure-collection", any("envelope membership counts can query children of any record kind")]))]
    #[impress_example(name = "default", args = r#"{"binding": "generic"}"#)]
    #[impress_example(
        name = "figure-folder",
        args = r#"{"binding":"figure"}"#,
        expect = r#"{"ok":true,"collections":[{"id":"59000000-0000-4000-8000-000000000001","name":"G3 figures","parent_id":null}]}"#
    )]
    async fn tree(&self, binding: String) -> CollectionListResult;

    /// Create a collection under `parent_id` (null = a new root).
    ///
    /// `kind_scope` is honoured only by the `generic` binding, where it names
    /// the record kind the collection organises ("publication", "manuscript",
    /// "figure", "message", "task") or "any" for a mixed-kind collection; it
    /// defaults to "any". The other bindings ignore it — their schema has no
    /// such field.
    #[impress_method]
    #[impress_example(
        name = "default",
        args = r#"{"binding": "generic", "name": "G3 research notes", "parent_id": null, "kind_scope": "any"}"#,
        expect = r#"{"ok":true,"collection":{"name":"G3 research notes","kind_scope":"any","parent_id":null}}"#
    )]
    async fn create(
        &self,
        binding: String,
        name: String,
        parent_id: Option<String>,
        kind_scope: Option<String>,
    ) -> CollectionResult;

    /// Rename a collection.
    #[impress_method]
    #[impress_example(
        name = "missing-collection",
        args = r#"{"binding": "generic", "id": "00000000-0000-0000-0000-000000000000", "name": "renamed"}"#,
        expect = r#"{"ok":false}"#
    )]
    #[impress_example(
        name = "rename-folder",
        args = r#"{"binding":"generic","id":"59000000-0000-4000-8000-000000000002","name":"Renamed G3 notes"}"#,
        expect = r#"{"ok":true,"collection":{"id":"59000000-0000-4000-8000-000000000002","name":"Renamed G3 notes"}}"#
    )]
    async fn rename(&self, binding: String, id: String, name: String) -> CollectionResult;

    /// Move a collection under `new_parent_id` (null = make it a root).
    ///
    /// Refuses self-parenting and any move under one of the collection's own
    /// descendants — the cycle check lives in the Rust kernel, so it holds for
    /// every caller rather than only the ones that remembered it.
    #[impress_method]
    #[impress_example(
        name = "missing-collection",
        args = r#"{"binding": "generic", "id": "00000000-0000-0000-0000-000000000000", "new_parent_id": null}"#,
        expect = r#"{"ok":false}"#
    )]
    #[impress_example(
        name = "nest-folder",
        args = r#"{"binding":"generic","id":"59000000-0000-4000-8000-000000000003","new_parent_id":"59000000-0000-4000-8000-000000000004"}"#,
        expect = r#"{"ok":true,"collection":{"id":"59000000-0000-4000-8000-000000000003","parent_id":"59000000-0000-4000-8000-000000000004"}}"#
    )]
    async fn reparent(
        &self,
        binding: String,
        id: String,
        new_parent_id: Option<String>,
    ) -> CollectionResult;

    /// Set a collection's position among its siblings. Lower sorts first.
    #[impress_method]
    #[impress_example(
        name = "missing-collection",
        args = r#"{"binding": "generic", "id": "00000000-0000-0000-0000-000000000000", "sort_order": 0}"#,
        expect = r#"{"ok":false}"#
    )]
    #[impress_example(
        name = "position-folder",
        args = r#"{"binding":"generic","id":"59000000-0000-4000-8000-000000000005","sort_order":7}"#,
        expect = r#"{"ok":true,"collection":{"id":"59000000-0000-4000-8000-000000000005","sort_order":7}}"#
    )]
    async fn reorder(&self, binding: String, id: String, sort_order: i64) -> CollectionResult;

    /// Delete a collection. Members are NEVER deleted — only the membership
    /// goes away, so the papers/manuscripts/figures survive. Deleting a
    /// collection that does not exist is an error, not a no-op.
    #[impress_method(safety = destructive)]
    #[impress_example(
        name = "missing-collection",
        args = r#"{"binding": "generic", "id": "00000000-0000-0000-0000-000000000000"}"#,
        expect = r#"{"ok":false}"#
    )]
    #[impress_example(
        name = "delete-empty-folder",
        args = r#"{"binding":"generic","id":"59000000-0000-4000-8000-000000000006"}"#,
        expect = r#"{"ok":true,"applied":0}"#
    )]
    async fn delete(&self, binding: String, id: String) -> CollectionMutationResult;

    /// File items into a collection. Idempotent per item; returns how many
    /// were applied. A `generic` collection with `kind_scope: "any"` accepts
    /// items of every kind at once.
    #[impress_method(effects(reads = ["collection", "imbib/collection", "manuscript-collection", "figure-collection", target(item_ids)], writes = ["collection", "imbib/collection", "manuscript-collection", "figure-collection"]))]
    #[impress_example(
        name = "missing-collection",
        args = r#"{"binding": "generic", "collection_id": "00000000-0000-0000-0000-000000000000", "item_ids": []}"#,
        expect = r#"{"ok":false}"#
    )]
    #[impress_example(
        name = "file-manuscript",
        args = r#"{"binding":"generic","collection_id":"59000000-0000-4000-8000-000000000007","item_ids":["59000000-0000-4000-8000-000000000020"]}"#,
        expect = r#"{"ok":true,"applied":1}"#
    )]
    async fn add_members(
        &self,
        binding: String,
        collection_id: String,
        item_ids: Vec<String>,
    ) -> CollectionMutationResult;

    /// Remove items from a collection. The items themselves are untouched;
    /// items filed in a DIFFERENT collection are left alone rather than
    /// unfiled. Returns how many were actually removed.
    #[impress_method]
    #[impress_example(
        name = "missing-collection",
        args = r#"{"binding": "generic", "collection_id": "00000000-0000-0000-0000-000000000000", "item_ids": []}"#,
        expect = r#"{"ok":false}"#
    )]
    #[impress_example(
        name = "unfile-manuscript",
        args = r#"{"binding":"generic","collection_id":"59000000-0000-4000-8000-000000000008","item_ids":["59000000-0000-4000-8000-000000000021"]}"#,
        expect = r#"{"ok":true,"applied":1}"#
    )]
    async fn remove_members(
        &self,
        binding: String,
        collection_id: String,
        item_ids: Vec<String>,
    ) -> CollectionMutationResult;

    /// Member count per collection, aligned index-for-index with
    /// `collection_ids`. Unknown ids count 0 rather than failing the batch, so
    /// a stale id cannot break a whole sidebar refresh.
    #[impress_method(safety = read_only, effects(reads = ["collection", "imbib/collection", "manuscript-collection", "figure-collection"]))]
    #[impress_example(
        name = "default",
        args = r#"{"binding": "generic", "collection_ids": []}"#
    )]
    #[impress_example(
        name = "one-member",
        args = r#"{"binding":"generic","collection_ids":["59000000-0000-4000-8000-000000000009"]}"#,
        expect = r#"{"ok":true,"counts":[1]}"#
    )]
    async fn member_counts(
        &self,
        binding: String,
        collection_ids: Vec<String>,
    ) -> MemberCountsResult;

    /// Where this store sits on the `collection@1.0.0` convergence
    /// (ADR-0022 WP G7): the `collections.unified` flag, how many rows are
    /// still under each legacy schema, and how many generic rows exist per
    /// `kind_scope`. Read-only and always safe. Start here before `migrate`.
    #[impress_method(safety = read_only, effects(reads = ["collection", "imbib/collection", "manuscript-collection", "figure-collection"]))]
    #[impress_example(name = "default", args = r#"{}"#)]
    async fn migration_status(&self) -> MigrationStatusResult;

    /// Converge the three legacy collection schemas onto `collection@1.0.0`.
    ///
    /// **Run with `dry_run: true` first.** A dry run writes NOTHING — not the
    /// rows, not the flag — and reports exactly the counts the real run will
    /// report, so the numbers you see are the numbers you will get.
    ///
    /// The real run rewrites every legacy collection row IN PLACE, keeping its
    /// id, its `Contains` edges and its envelope parent, and sets the flag in
    /// the same transaction. Membership is not rewritten. Every rewritten row
    /// carries its original schema and payload verbatim, so `rollback` is
    /// byte-faithful. Re-running is safe: the second run rewrites zero rows.
    ///
    /// This is a deliberate, human-invoked operation on data users live in.
    #[impress_method(effects(reads = ["collection", "imbib/collection", "manuscript-collection", "figure-collection"], writes = ["collection"]))]
    #[impress_example(
        name = "preview-convergence",
        args = r#"{"dry_run":true}"#,
        expect = r#"{"ok":true,"dry_run":true,"membership_edges_untouched":true}"#
    )]
    async fn migrate(&self, dry_run: bool) -> MigrationReportResult;

    /// Undo `migrate`: restore every migrated collection's original schema and
    /// payload byte-for-byte and clear the flag.
    ///
    /// A REWIND, not a merge. The payload restored is the one the migration
    /// froze, so renames/reorders/moves performed on a migrated collection
    /// AFTER the migration are discarded along with it. Collections created
    /// after the migration carry no provenance, are left strictly alone, and
    /// are reported separately — nothing is deleted, but post-migration edits
    /// to pre-migration rows do not survive. Check `migration_status` first.
    #[impress_method(safety = destructive)]
    #[impress_example(
        name = "restore-legacy-folders",
        args = r#"{}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn rollback(&self) -> RollbackReportResult;
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

/// The convergence of an application's legacy collection schemas onto
/// `collection@1.0.0` (ADR-0022 WP G7). A deployment whose legacy rows live in
/// a store with its own transactional rewrite (impress: SQLite) registers one
/// with [`set_migration`]; the `migration-status`, `migrate` and `rollback`
/// verbs call it. A deployment with none has no legacy schemas, so those verbs
/// answer that there is nothing to converge.
pub trait CollectionMigration: Send + Sync {
    /// The flag and the row counts on both sides of it.
    fn status(&self) -> MigrationStatusResult;
    /// Converge (or, `dry_run`, report what would change).
    fn migrate(&self, dry_run: bool) -> MigrationReportResult;
    /// Undo `migrate`.
    fn rollback(&self) -> RollbackReportResult;
}

static MIGRATION: RwLock<Option<Arc<dyn CollectionMigration>>> = RwLock::new(None);

/// Install the process's collection migration (see [`CollectionMigration`]).
pub fn set_migration(migration: Arc<dyn CollectionMigration>) {
    *MIGRATION.write().unwrap_or_else(|e| e.into_inner()) = Some(migration);
}

fn process_migration() -> Option<Arc<dyn CollectionMigration>> {
    MIGRATION.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Store-backed `CollectionService`. `new()` uses the process store
/// (`koine_store::process::acquire_or_stand_in`); `with_store` takes an
/// explicit one over the port, as the tests do.
#[derive(Clone, Default)]
pub struct DefaultCollectionService {
    store: Option<Arc<dyn AttributedStore>>,
    migration: Option<Arc<dyn CollectionMigration>>,
}

impl DefaultCollectionService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_store(store: Arc<dyn AttributedStore>) -> Self {
        Self {
            store: Some(store),
            migration: None,
        }
    }

    /// Use `migration` for the migration verbs instead of the process's.
    pub fn with_migration(mut self, migration: Arc<dyn CollectionMigration>) -> Self {
        self.migration = Some(migration);
        self
    }

    fn store(&self) -> Arc<dyn AttributedStore> {
        self.store
            .clone()
            .unwrap_or_else(|| koine_store::process::acquire_or_stand_in().0)
    }

    fn migration(&self) -> Option<Arc<dyn CollectionMigration>> {
        self.migration.clone().or_else(process_migration)
    }
}

/// What the migration verbs say when no migration is installed: this
/// deployment keeps no legacy collection schemas, so there is nothing to
/// converge. The flag is still the store's.
const NOTHING_TO_MIGRATE: &str =
    "This deployment has no legacy collection schemas, so there is nothing to converge.";

/// Kernel errors carry the diagnosis (`invalid UUID`, `would create a cycle`,
/// `has schema 'x', expected 'y'`); the tool surface just has to not swallow
/// them.
fn describe(err: StoreError) -> String {
    err.to_string()
}

fn row_result(result: Result<CollectionRow, StoreError>, verb: &str) -> CollectionResult {
    match result {
        Ok(row) => CollectionResult {
            ok: true,
            message: format!("{verb} '{}' ({}).", row.name, row.id),
            collection: Some(row.into()),
        },
        Err(e) => CollectionResult {
            ok: false,
            collection: None,
            message: describe(e),
        },
    }
}

/// The structural verbs return the row plus the prior value an undo stack
/// needs (ADR-0022 G2). The agent surface has no undo stack — it reports the
/// row and drops the prior value on the floor, deliberately.
fn mutation_result(
    result: Result<ops::CollectionMutation, StoreError>,
    verb: &str,
) -> CollectionResult {
    row_result(result.map(|m| m.row), verb)
}

/// The membership verbs return the ids they ACTUALLY changed; the tool surface
/// reports how many that was.
fn member_result(result: Result<Vec<String>, StoreError>, verb: &str) -> CollectionMutationResult {
    match result {
        Ok(changed) => CollectionMutationResult {
            ok: true,
            applied: changed.len() as u32,
            message: format!("{verb} {} item(s).", changed.len()),
        },
        Err(e) => CollectionMutationResult {
            ok: false,
            applied: 0,
            message: describe(e),
        },
    }
}

#[async_trait::async_trait]
impl CollectionService for DefaultCollectionService {
    async fn tree(&self, binding: String) -> CollectionListResult {
        let binding = match binding_for(&binding) {
            Ok(b) => b,
            Err(message) => {
                return CollectionListResult {
                    ok: false,
                    collections: vec![],
                    message,
                }
            }
        };
        match ops::list_tree(&*self.store(), &binding) {
            Ok(rows) => CollectionListResult {
                ok: true,
                message: format!("{} collection(s).", rows.len()),
                collections: rows.into_iter().map(CollectionRowDto::from).collect(),
            },
            Err(e) => CollectionListResult {
                ok: false,
                collections: vec![],
                message: describe(e),
            },
        }
    }

    async fn create(
        &self,
        binding: String,
        name: String,
        parent_id: Option<String>,
        kind_scope: Option<String>,
    ) -> CollectionResult {
        let binding = match binding_for(&binding) {
            Ok(b) => b,
            Err(message) => {
                return CollectionResult {
                    ok: false,
                    collection: None,
                    message,
                }
            }
        };
        row_result(
            ops::create(
                &*self.store(),
                &binding,
                &name,
                parent_id.as_deref(),
                kind_scope.as_deref(),
                // The agent surface always appends with the schema default;
                // explicit positioning is `reorder`'s job here.
                None,
            ),
            "Created",
        )
    }

    async fn rename(&self, binding: String, id: String, name: String) -> CollectionResult {
        let binding = match binding_for(&binding) {
            Ok(b) => b,
            Err(message) => {
                return CollectionResult {
                    ok: false,
                    collection: None,
                    message,
                }
            }
        };
        mutation_result(
            ops::rename(&*self.store(), &binding, &id, &name),
            "Renamed to",
        )
    }

    async fn reparent(
        &self,
        binding: String,
        id: String,
        new_parent_id: Option<String>,
    ) -> CollectionResult {
        let binding = match binding_for(&binding) {
            Ok(b) => b,
            Err(message) => {
                return CollectionResult {
                    ok: false,
                    collection: None,
                    message,
                }
            }
        };
        mutation_result(
            ops::reparent(&*self.store(), &binding, &id, new_parent_id.as_deref()),
            "Moved",
        )
    }

    async fn reorder(&self, binding: String, id: String, sort_order: i64) -> CollectionResult {
        let binding = match binding_for(&binding) {
            Ok(b) => b,
            Err(message) => {
                return CollectionResult {
                    ok: false,
                    collection: None,
                    message,
                }
            }
        };
        mutation_result(
            ops::reorder(&*self.store(), &binding, &id, sort_order),
            "Reordered",
        )
    }

    async fn delete(&self, binding: String, id: String) -> CollectionMutationResult {
        let binding = match binding_for(&binding) {
            Ok(b) => b,
            Err(message) => {
                return CollectionMutationResult {
                    ok: false,
                    applied: 0,
                    message,
                }
            }
        };
        match ops::delete(&*self.store(), &binding, &id) {
            Ok(_snapshot) => CollectionMutationResult {
                ok: true,
                applied: 0,
                message: format!("Deleted collection {id}. Its members were not deleted."),
            },
            Err(e) => CollectionMutationResult {
                ok: false,
                applied: 0,
                message: describe(e),
            },
        }
    }

    async fn add_members(
        &self,
        binding: String,
        collection_id: String,
        item_ids: Vec<String>,
    ) -> CollectionMutationResult {
        let binding = match binding_for(&binding) {
            Ok(b) => b,
            Err(message) => {
                return CollectionMutationResult {
                    ok: false,
                    applied: 0,
                    message,
                }
            }
        };
        member_result(
            ops::add_members(&*self.store(), &binding, &collection_id, &item_ids),
            "Filed",
        )
    }

    async fn remove_members(
        &self,
        binding: String,
        collection_id: String,
        item_ids: Vec<String>,
    ) -> CollectionMutationResult {
        let binding = match binding_for(&binding) {
            Ok(b) => b,
            Err(message) => {
                return CollectionMutationResult {
                    ok: false,
                    applied: 0,
                    message,
                }
            }
        };
        member_result(
            ops::remove_members(&*self.store(), &binding, &collection_id, &item_ids),
            "Removed",
        )
    }

    async fn member_counts(
        &self,
        binding: String,
        collection_ids: Vec<String>,
    ) -> MemberCountsResult {
        let binding = match binding_for(&binding) {
            Ok(b) => b,
            Err(message) => {
                return MemberCountsResult {
                    ok: false,
                    counts: vec![],
                    message,
                }
            }
        };
        match ops::member_counts(&*self.store(), &binding, &collection_ids) {
            Ok(counts) => MemberCountsResult {
                ok: true,
                message: format!("Counted {} collection(s).", counts.len()),
                counts,
            },
            Err(e) => MemberCountsResult {
                ok: false,
                counts: vec![],
                message: describe(e),
            },
        }
    }

    async fn migration_status(&self) -> MigrationStatusResult {
        if let Some(migration) = self.migration() {
            return migration.status();
        }
        match ops::is_migrated_on(&*self.store()) {
            Ok(migrated) => MigrationStatusResult {
                ok: true,
                migrated,
                legacy: vec![],
                generic: vec![],
                message: NOTHING_TO_MIGRATE.to_string(),
            },
            Err(e) => MigrationStatusResult {
                ok: false,
                migrated: false,
                legacy: vec![],
                generic: vec![],
                message: describe(e),
            },
        }
    }

    async fn migrate(&self, dry_run: bool) -> MigrationReportResult {
        if let Some(migration) = self.migration() {
            return migration.migrate(dry_run);
        }
        let was_migrated = ops::is_migrated_on(&*self.store()).unwrap_or(false);
        MigrationReportResult {
            ok: true,
            dry_run,
            was_migrated,
            bindings: vec![],
            membership_edges_untouched: true,
            message: NOTHING_TO_MIGRATE.to_string(),
        }
    }

    async fn rollback(&self) -> RollbackReportResult {
        if let Some(migration) = self.migration() {
            return migration.rollback();
        }
        RollbackReportResult {
            ok: true,
            restored: vec![],
            native_generic_untouched: 0,
            membership_edges_untouched: true,
            message: NOTHING_TO_MIGRATE.to_string(),
        }
    }
}

impress_service_impl! {
    service = CollectionService,
    safety = mutating,
    since = "0.1.0",
    effects = {
        reads: ["collection", "imbib/collection", "manuscript-collection", "figure-collection"],
        writes: ["collection", "imbib/collection", "manuscript-collection", "figure-collection"],
        reach: [],
    },
    impl = DefaultCollectionService,
    instance = DefaultCollectionService::new,
    methods = [
        tree(
            /// Collection hierarchy: `imbib`, `manuscript`, `figure`, or `generic`.
            binding: String
        ) -> CollectionListResult,
        create(
            /// Collection hierarchy to create within.
            binding: String,
            /// Visible name of the new collection.
            name: String,
            /// Parent collection UUID, or null for a root collection.
            parent_id: Option<String>,
            /// Record kind for a generic collection; null defaults to `any`.
            kind_scope: Option<String>
        ) -> CollectionResult,
        rename(
            /// Collection hierarchy containing the row.
            binding: String,
            /// UUID of the collection to rename.
            id: String,
            /// New visible collection name.
            name: String
        ) -> CollectionResult,
        reparent(
            /// Collection hierarchy containing both rows.
            binding: String,
            /// UUID of the collection to move.
            id: String,
            /// New parent collection UUID, or null to make it a root.
            new_parent_id: Option<String>
        ) -> CollectionResult,
        reorder(
            /// Collection hierarchy containing the row.
            binding: String,
            /// UUID of the collection to reposition.
            id: String,
            /// Numeric sibling position; lower values sort first.
            sort_order: i64
        ) -> CollectionResult,
        delete(
            /// Collection hierarchy containing the row.
            binding: String,
            /// UUID of the collection to delete; members survive.
            id: String
        ) -> CollectionMutationResult,
        add_members(
            /// Collection hierarchy containing the target collection.
            binding: String,
            /// UUID of the collection receiving members.
            collection_id: String,
            /// UUIDs of existing items to file into the collection.
            item_ids: Vec<String>
        ) -> CollectionMutationResult,
        remove_members(
            /// Collection hierarchy containing the target collection.
            binding: String,
            /// UUID of the collection losing members.
            collection_id: String,
            /// UUIDs of items to unfile; the items themselves survive.
            item_ids: Vec<String>
        ) -> CollectionMutationResult,
        member_counts(
            /// Collection hierarchy containing the requested rows.
            binding: String,
            /// Collection UUIDs; counts return in the same order.
            collection_ids: Vec<String>
        ) -> MemberCountsResult,
        migration_status() -> MigrationStatusResult,
        migrate(
            /// True to report what would change without writing anything.
            dry_run: bool
        ) -> MigrationReportResult,
        rollback() -> RollbackReportResult,
    ],
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{new_item, EnvelopeParent, Membership, ParentField};
    use std::collections::BTreeMap;
    use std::future::Future;

    fn make_item(store: &dyn AttributedStore, schema: &str) -> String {
        let mut payload = BTreeMap::new();
        payload.insert(
            "title".to_string(),
            impress_item::Value::String("an item".into()),
        );
        let item = new_item(
            store,
            &impress_item::SchemaRef::from_stored(schema.to_owned()),
            payload,
        );
        store.insert(item).unwrap().to_string()
    }

    /// The figure-folder shape: no parent field, nesting and membership both
    /// through the envelope. An app registers a binding like it.
    fn folder_binding() -> CollectionSchemaBinding {
        CollectionSchemaBinding {
            schema_ref: &FOLDERS,
            parent_field: ParentField::Envelope,
            membership: Membership::EnvelopeParent,
            kind_scope_field: None,
            envelope_parent: EnvelopeParent::TreeParent,
            unified_kind_scope: None,
            container_field: None,
            smart_field: None,
        }
    }

    static FOLDERS: impress_item::SchemaRef = impress_item::schema::refs::FIGURE_COLLECTION;

    /// Run `case` once over each backend: the service behaves the same.
    async fn on_each_backend<F, Fut>(case: F)
    where
        F: Fn(Arc<dyn AttributedStore>) -> Fut,
        Fut: Future<Output = ()>,
    {
        case(Arc::new(koine_store_memory::MemoryStore::new())).await;
        let dir = tempfile::tempdir().expect("tempdir");
        case(Arc::new(
            koine_store_dir::DirStore::open(dir.path()).expect("open dir store"),
        ))
        .await;
    }

    #[tokio::test]
    async fn binding_names_and_aliases_resolve() {
        register_binding(&["folders", "folder-collection"], folder_binding());
        for name in binding_names() {
            assert!(binding_for(&name).is_ok(), "{name} must resolve");
        }
        assert_eq!(
            binding_for("FOLDER-COLLECTION")
                .unwrap()
                .schema_ref
                .as_str(),
            "figure-collection"
        );
        for alias in ["", "generic", "any", "collection", "Impress"] {
            assert_eq!(
                binding_for(alias).unwrap().schema_ref.as_str(),
                "collection"
            );
        }
        let err = binding_for("mailbox").unwrap_err();
        assert!(err.contains("generic") && err.contains("folders"), "{err}");
        // Registering again replaces, it does not duplicate.
        register_binding(&["folders", "folder-collection"], folder_binding());
        assert_eq!(
            binding_names().iter().filter(|n| *n == "folders").count(),
            1
        );
    }

    #[tokio::test]
    async fn unknown_binding_fails_every_method_loudly() {
        let s =
            DefaultCollectionService::with_store(Arc::new(koine_store_memory::MemoryStore::new()));
        assert!(!s.tree("mailbox".into()).await.ok);
        assert!(!s.create("mailbox".into(), "X".into(), None, None).await.ok);
        assert!(!s.delete("mailbox".into(), "id".into()).await.ok);
        assert!(!s.member_counts("mailbox".into(), vec![]).await.ok);
    }

    #[tokio::test]
    async fn create_tree_rename_reorder_reparent_delete_round_trip() {
        on_each_backend(|store| async move {
            let s = DefaultCollectionService::with_store(store);

            let root = s
                .create("generic".into(), "Root".into(), None, Some("any".into()))
                .await;
            assert!(root.ok, "{}", root.message);
            let root_id = root.collection.unwrap().id;

            let child = s
                .create(
                    "generic".into(),
                    "Child".into(),
                    Some(root_id.clone()),
                    Some("publication".into()),
                )
                .await;
            let child_row = child.collection.expect("child created");
            assert_eq!(child_row.parent_id.as_deref(), Some(root_id.as_str()));
            assert_eq!(child_row.kind_scope.as_deref(), Some("publication"));
            let child_id = child_row.id;

            let tree = s.tree("generic".into()).await;
            assert!(tree.ok);
            assert_eq!(tree.collections.len(), 2);

            let renamed = s
                .rename("generic".into(), child_id.clone(), "Renamed".into())
                .await;
            assert_eq!(renamed.collection.unwrap().name, "Renamed");

            let reordered = s.reorder("generic".into(), child_id.clone(), 7).await;
            assert_eq!(reordered.collection.unwrap().sort_order, 7);

            let unparented = s.reparent("generic".into(), child_id.clone(), None).await;
            assert_eq!(unparented.collection.unwrap().parent_id, None);
            let reparented = s
                .reparent("generic".into(), child_id.clone(), Some(root_id.clone()))
                .await;
            assert_eq!(
                reparented.collection.unwrap().parent_id.as_deref(),
                Some(root_id.as_str())
            );

            // The cycle check reaches the tool surface.
            let cycle = s
                .reparent("generic".into(), root_id.clone(), Some(child_id.clone()))
                .await;
            assert!(!cycle.ok);
            assert!(cycle.message.contains("cycle"), "{}", cycle.message);

            let deleted = s.delete("generic".into(), child_id.clone()).await;
            assert!(deleted.ok, "{}", deleted.message);
            assert_eq!(s.tree("generic".into()).await.collections.len(), 1);
            assert!(!s.delete("generic".into(), child_id).await.ok);
        })
        .await;
    }

    #[tokio::test]
    async fn members_are_added_counted_and_removed() {
        on_each_backend(|store| async move {
            let s = DefaultCollectionService::with_store(store.clone());
            let mixed = s
                .create(
                    "generic".into(),
                    "Grant renewal".into(),
                    None,
                    Some("any".into()),
                )
                .await
                .collection
                .unwrap()
                .id;
            let empty = s
                .create("generic".into(), "Empty".into(), None, None)
                .await
                .collection
                .unwrap()
                .id;
            let manuscript = make_item(&*store, "manuscript");
            let figure = make_item(&*store, "figure");

            let added = s
                .add_members(
                    "generic".into(),
                    mixed.clone(),
                    vec![manuscript.clone(), figure.clone()],
                )
                .await;
            assert!(added.ok, "{}", added.message);
            assert_eq!(added.applied, 2);

            let counts = s
                .member_counts("generic".into(), vec![mixed.clone(), empty.clone()])
                .await;
            assert_eq!(counts.counts, vec![2, 0], "counts align with the ids given");

            let removed = s
                .remove_members("generic".into(), mixed.clone(), vec![figure])
                .await;
            assert_eq!(removed.applied, 1);
            assert_eq!(
                s.member_counts("generic".into(), vec![mixed]).await.counts,
                vec![1]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_registered_binding_round_trips_through_the_service() {
        on_each_backend(|store| async move {
            register_binding(&["folders", "folder-collection"], folder_binding());
            let s = DefaultCollectionService::with_store(store.clone());
            let folder = s
                .create("folders".into(), "Plots".into(), None, None)
                .await
                .collection
                .expect("folder created")
                .id;
            let figure = make_item(&*store, "figure");
            let filed = s
                .add_members("folders".into(), folder.clone(), vec![figure])
                .await;
            assert_eq!(filed.applied, 1, "{}", filed.message);
            assert_eq!(
                s.member_counts("folders".into(), vec![folder]).await.counts,
                vec![1]
            );
            // The generic hierarchy does not see the folder.
            assert!(s.tree("generic".into()).await.collections.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn a_binding_refuses_another_schemas_collections() {
        on_each_backend(|store| async move {
            register_binding(&["folders", "folder-collection"], folder_binding());
            let s = DefaultCollectionService::with_store(store);
            let generic = s
                .create("generic".into(), "G".into(), None, None)
                .await
                .collection
                .unwrap()
                .id;
            let renamed = s.rename("folders".into(), generic, "x".into()).await;
            assert!(!renamed.ok);
            assert!(renamed.message.contains("expected"), "{}", renamed.message);
        })
        .await;
    }

    #[tokio::test]
    async fn without_a_migration_the_verbs_say_there_is_nothing_to_converge() {
        on_each_backend(|store| async move {
            let s = DefaultCollectionService::with_store(store.clone());
            let status = s.migration_status().await;
            assert!(status.ok && !status.migrated && status.legacy.is_empty());
            let dry = s.migrate(true).await;
            assert!(dry.ok && dry.dry_run && dry.bindings.is_empty());
            let rolled = s.rollback().await;
            assert!(rolled.ok && rolled.restored.is_empty());
            // The flag is still the store's.
            store
                .metadata_set(ops::MARKER_KEY, ops::MARKER_VALUE)
                .unwrap();
            assert!(s.migration_status().await.migrated);
            assert!(s.migrate(false).await.was_migrated);
        })
        .await;
    }

    struct Recording;

    impl CollectionMigration for Recording {
        fn status(&self) -> MigrationStatusResult {
            MigrationStatusResult {
                ok: true,
                migrated: true,
                legacy: vec![],
                generic: vec![],
                message: "from the hook".into(),
            }
        }
        fn migrate(&self, dry_run: bool) -> MigrationReportResult {
            MigrationReportResult {
                ok: true,
                dry_run,
                was_migrated: false,
                bindings: vec![],
                membership_edges_untouched: true,
                message: "from the hook".into(),
            }
        }
        fn rollback(&self) -> RollbackReportResult {
            RollbackReportResult {
                ok: true,
                restored: vec![],
                native_generic_untouched: 0,
                membership_edges_untouched: true,
                message: "from the hook".into(),
            }
        }
    }

    #[tokio::test]
    async fn an_installed_migration_answers_the_migration_verbs() {
        let store: Arc<dyn AttributedStore> = Arc::new(koine_store_memory::MemoryStore::new());
        let s = DefaultCollectionService::with_store(store).with_migration(Arc::new(Recording));
        assert_eq!(s.migration_status().await.message, "from the hook");
        assert!(s.migrate(true).await.dry_run);
        assert_eq!(s.rollback().await.message, "from the hook");
    }

    /// Without `with_store` the service reads the process store.
    #[tokio::test]
    async fn the_default_deployment_uses_the_process_store() {
        let (store, _) = koine_store::process::acquire_or_stand_in();
        let s = DefaultCollectionService::new();
        let made = s
            .create("generic".into(), "Shared".into(), None, None)
            .await;
        assert!(made.ok, "{}", made.message);
        let id = made.collection.unwrap().id;
        assert!(store.get(id.parse().unwrap()).unwrap().is_some());
    }

    #[test]
    fn the_inventory_registers_the_twelve_verbs() {
        for verb in [
            "tree",
            "create",
            "rename",
            "reparent",
            "reorder",
            "delete",
            "add-members",
            "remove-members",
            "member-counts",
            "migration-status",
            "migrate",
            "rollback",
        ] {
            let name = format!("collection-service_{verb}");
            assert!(
                impress_service_core::VerbDescriptor::find(&name).is_some(),
                "{name} is not linked"
            );
        }
    }
}
