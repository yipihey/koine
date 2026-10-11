//! `koine-store-dir`: the directory koine store (`docs/plan-koine-store.md` D4).
//!
//! One JSON file per item under `items/<schema ref as a path>/<id>.json`,
//! operations under `operations/`, blobs under `blobs/<item id>/<name>`, a
//! `clock` file, a `lock` file and a `journal/`. A root is legible with `cat`,
//! diffable with git and syncable with rsync.
//!
//! The semantics are `SqliteItemStore`'s (D2); the layout and the guarantees
//! are the plan's:
//!
//! - **D4.1** every mutating call holds the exclusive [`FileLock`] on
//!   `<root>/lock` for its duration; reads take none and see whole files only
//!   (every write is temp-and-rename in the same directory).
//! - **D4.2** a write that touches more than one file (a batch, an operation
//!   and its target, a delete) is written whole to `journal/<id>.json` first,
//!   applied file by file, and the journal removed. [`DirStore::open`] (and
//!   the next mutating call) replays a journal it finds, so such a write is
//!   all-or-nothing across a crash. A *reader* can still observe a batch
//!   half-applied while it runs; the journal is crash atomicity, not
//!   isolation.
//! - **D4.3** events are in-process; [`DirStore::clock_file`] is the cheap
//!   "did another process write" probe (SQLite's `data_version`).
//! - **D4.4** two roots synced by a file tool are two stores with the same
//!   ids. A `<id>.conflict-<n>.json` copy that carries a higher clock than
//!   the file it sits beside is reported by [`DirStore::conflicts`]; both
//!   files are left alone.
//!
//! Nothing is `fsync`ed: the store is durable across process exit, not across
//! power loss.

pub mod eval;
mod fsio;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use fsio::{record_id, remove_if_present, storage, write_atomic};
pub use impress_fs_lock::FileLock;
use impress_item::{
    build_operation_payload, routine_retention_for_schema, ActorKind, AttributedStore, BlobStore,
    EdgeType, FieldMutation, GuardedBatch, GuardedWrite, Item, ItemEvent, ItemId, ItemQuery,
    ItemStore, OperationIntent, OperationSpec, OperationType, Priority, RetentionTier, SchemaRef,
    StoreCapabilities, StoreError, Value, Visibility,
};

/// The on-disk format this build reads and writes.
pub const FORMAT: u32 = 1;

/// The `schema` of operation rows (`core/operation`).
const OPERATION_SCHEMA: &str = "core/operation";

/// The immutable-kind hook (`docs/plan-koine-store.md` D2): called with the
/// target and the operation before anything is written, so the suite's own
/// kinds (impress's manuscript revisions) stay out of the store. The default
/// store has no policy.
pub trait WritePolicy: Send + Sync {
    /// `Err` refuses `op` on `target`; nothing is written.
    fn check(&self, target: &Item, op: &OperationType) -> Result<(), StoreError>;
}

/// Allows everything.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoPolicy;

impl WritePolicy for NoPolicy {
    fn check(&self, _target: &Item, _op: &OperationType) -> Result<(), StoreError> {
        Ok(())
    }
}

/// `koine-store.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoreMeta {
    format: u32,
    store_id: String,
    created: DateTime<Utc>,
}

/// An item file that is behind a sibling conflict copy (D4.4). Reported, never
/// merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub id: ItemId,
    /// `<id>.json`.
    pub file: PathBuf,
    /// Its `logical_clock`; `None` when the file is missing.
    pub file_clock: Option<u64>,
    /// `<id>.conflict-<n>.json`.
    pub copy: PathBuf,
    pub copy_clock: u64,
}

// ─────────────────────────────────────────────────────────────────────────────
// Rows and documents
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
struct OpMeta {
    target: ItemId,
    retention: RetentionTier,
}

/// One record: an item, or an operation (an item plus what the SQLite
/// store keeps in the `op_target_id` and `retention` columns).
#[derive(Debug, Clone)]
struct Row {
    item: Item,
    op: Option<OpMeta>,
}

#[derive(Serialize)]
struct OpDocRef<'a> {
    #[serde(flatten)]
    item: &'a Item,
    op_target_id: ItemId,
    retention: RetentionTier,
}

#[derive(Deserialize)]
struct OpDoc {
    #[serde(flatten)]
    item: Item,
    op_target_id: ItemId,
    retention: RetentionTier,
}

#[derive(Deserialize)]
struct OpTargetOnly {
    op_target_id: ItemId,
}

#[derive(Deserialize)]
struct ClockOnly {
    logical_clock: u64,
}

impl Row {
    fn rel_path(&self) -> Result<String, StoreError> {
        match self.op {
            Some(_) => Ok(op_rel(self.item.id)),
            None => item_rel(self.item.schema.as_str(), self.item.id),
        }
    }

    fn doc(&self) -> Result<serde_json::Value, StoreError> {
        let doc = match self.op {
            Some(meta) => serde_json::to_value(OpDocRef {
                item: &self.item,
                op_target_id: meta.target,
                retention: meta.retention,
            }),
            None => serde_json::to_value(&self.item),
        };
        doc.map_err(|e| storage("serialize record", e))
    }

    fn parse(bytes: &[u8], is_op: bool, path: &Path) -> Result<Row, StoreError> {
        let context = || format!("parse {}", path.display());
        if is_op {
            let doc: OpDoc = serde_json::from_slice(bytes).map_err(|e| storage(context(), e))?;
            Ok(Row {
                item: doc.item,
                op: Some(OpMeta {
                    target: doc.op_target_id,
                    retention: doc.retention,
                }),
            })
        } else {
            let item: Item = serde_json::from_slice(bytes).map_err(|e| storage(context(), e))?;
            Ok(Row { item, op: None })
        }
    }

    fn eval_row(&self) -> eval::Row<'_> {
        eval::Row {
            item: &self.item,
            op_target: self.op.map(|o| o.target),
        }
    }
}

fn op_rel(id: ItemId) -> String {
    format!("operations/{id}.json")
}

/// `items/<schema ref as a path>/<id>.json`: the ref's `/` are directory
/// separators, `@` stays.
fn schema_dir(schema: &str) -> Result<String, StoreError> {
    let mut out = String::from("items");
    for segment in schema.split('/') {
        if segment.is_empty()
            || segment == "."
            || segment == ".."
            || segment.contains('\\')
            || segment.contains('\0')
        {
            return Err(StoreError::Validation(format!(
                "schema ref {schema:?} cannot be a directory path"
            )));
        }
        out.push('/');
        out.push_str(segment);
    }
    Ok(out)
}

fn item_rel(schema: &str, id: ItemId) -> Result<String, StoreError> {
    Ok(format!("{}/{id}.json", schema_dir(schema)?))
}

fn to_path(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, s| p.join(s))
}

fn trunc_ms(t: DateTime<Utc>) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(t.timestamp_millis())
        .single()
        .unwrap_or(t)
}

fn pretty(doc: &serde_json::Value) -> Result<Vec<u8>, StoreError> {
    let mut bytes = serde_json::to_vec_pretty(doc).map_err(|e| storage("serialize", e))?;
    bytes.push(b'\n');
    Ok(bytes)
}

// ─────────────────────────────────────────────────────────────────────────────
// The hybrid logical clock
// ─────────────────────────────────────────────────────────────────────────────

/// The last `(wall_ms, counter)` handed out; the `clock` file holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Hlc {
    wall_ms: u64,
    counter: u64,
}

impl Hlc {
    fn pack(self) -> u64 {
        (self.wall_ms << 16) | (self.counter & 0xFFFF)
    }

    /// The next clock: the counter counts within one millisecond and resets
    /// when the wall clock moves. (A 65536th tick in one millisecond borrows
    /// the next millisecond rather than repeat a value.)
    fn next(&mut self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        if now > self.wall_ms {
            self.wall_ms = now;
            self.counter = 0;
        } else {
            self.counter += 1;
            if self.counter > 0xFFFF {
                self.wall_ms += 1;
                self.counter = 0;
            }
        }
        self.pack()
    }

    /// Raise the state so every later local clock sorts after `remote`.
    fn observe(&mut self, remote: u64) {
        let (wall, counter) = (remote >> 16, remote & 0xFFFF);
        if wall > self.wall_ms {
            self.wall_ms = wall;
            self.counter = counter;
        } else if wall == self.wall_ms {
            self.counter = self.counter.max(counter);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The journal (D4.2)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JournalWrite {
    /// Relative to the root, `/`-separated.
    path: String,
    /// The whole new document.
    doc: serde_json::Value,
}

/// A pending multi-file write: the full documents to write and the files to
/// remove. Replaying it is idempotent.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Journal {
    batch: String,
    created: DateTime<Utc>,
    /// The ids this write touches (for a human reading the file).
    ids: Vec<ItemId>,
    writes: Vec<JournalWrite>,
    removes: Vec<String>,
}

impl Journal {
    fn path(&self, root: &Path) -> PathBuf {
        root.join("journal").join(format!("{}.json", self.batch))
    }

    fn write(&self, root: &Path) -> Result<(), StoreError> {
        let bytes =
            pretty(&serde_json::to_value(self).map_err(|e| storage("serialize journal", e))?)?;
        write_atomic(&self.path(root), &bytes).map_err(|e| storage("write journal", e))
    }

    fn apply_write(root: &Path, write: &JournalWrite) -> Result<(), StoreError> {
        let path = root.join(fsio::safe_relative(&write.path)?);
        write_atomic(&path, &pretty(&write.doc)?)
            .map_err(|e| storage(format!("write {}", path.display()), e))
    }

    fn apply(&self, root: &Path) -> Result<(), StoreError> {
        for write in &self.writes {
            Self::apply_write(root, write)?;
        }
        for rel in &self.removes {
            let path = root.join(fsio::safe_relative(rel)?);
            remove_if_present(&path)
                .map_err(|e| storage(format!("remove {}", path.display()), e))?;
        }
        Ok(())
    }

    fn finish(&self, root: &Path) -> Result<(), StoreError> {
        remove_if_present(&self.path(root)).map_err(|e| storage("remove journal", e))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The store
// ─────────────────────────────────────────────────────────────────────────────

/// Events a call produced, with the schema each is filtered by.
type Events = Vec<(Option<String>, ItemEvent)>;

type Subscribers = Mutex<Vec<(Option<String>, Sender<ItemEvent>)>>;

/// The directory store. Cheap to hold; wrap it in an `Arc` to share it.
pub struct DirStore {
    root: PathBuf,
    store_id: String,
    policy: Arc<dyn WritePolicy>,
    author: String,
    author_kind: ActorKind,
    subscribers: Subscribers,
    conflicts: Mutex<Vec<Conflict>>,
}

impl fmt::Debug for DirStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DirStore")
            .field("root", &self.root)
            .field("store_id", &self.store_id)
            .finish_non_exhaustive()
    }
}

impl DirStore {
    /// Open the store at `root`, creating the layout when absent. Refuses a
    /// root whose `format` is newer than [`FORMAT`]; completes any journal a
    /// crashed writer left; collects [`Self::conflicts`].
    pub fn open(root: impl AsRef<Path>) -> Result<DirStore, StoreError> {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root)
            .map_err(|e| storage(format!("create {}", root.display()), e))?;
        let _lock = FileLock::exclusive(root.join("lock"))
            .map_err(|e| storage("take the writer lock", e))?;

        let meta_path = root.join("koine-store.json");
        let meta = match std::fs::read(&meta_path) {
            Ok(bytes) => {
                let meta: StoreMeta = serde_json::from_slice(&bytes)
                    .map_err(|e| storage(format!("parse {}", meta_path.display()), e))?;
                if meta.format > FORMAT {
                    return Err(StoreError::Storage(format!(
                        "{} is format {}, this build reads up to {FORMAT}",
                        meta_path.display(),
                        meta.format
                    )));
                }
                meta
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let meta = StoreMeta {
                    format: FORMAT,
                    store_id: Uuid::new_v4().to_string(),
                    created: Utc::now(),
                };
                let doc = serde_json::to_value(&meta).map_err(|e| storage("serialize meta", e))?;
                write_atomic(&meta_path, &pretty(&doc)?)
                    .map_err(|e| storage(format!("write {}", meta_path.display()), e))?;
                meta
            }
            Err(e) => return Err(storage(format!("read {}", meta_path.display()), e)),
        };
        for dir in ["items", "operations", "blobs", "journal"] {
            std::fs::create_dir_all(root.join(dir))
                .map_err(|e| storage(format!("create {dir}/"), e))?;
        }

        let store = DirStore {
            root,
            store_id: meta.store_id,
            policy: Arc::new(NoPolicy),
            author: "system:local".into(),
            author_kind: ActorKind::System,
            subscribers: Mutex::new(Vec::new()),
            conflicts: Mutex::new(Vec::new()),
        };
        store.replay_journals()?;
        *store.conflicts.lock().map_err(poisoned)? = store.scan_conflicts()?;
        Ok(store)
    }

    /// Install the immutable-kind hook.
    pub fn with_policy(mut self, policy: Arc<dyn WritePolicy>) -> Self {
        self.policy = policy;
        self
    }

    /// The author `update` stamps on the operations it mints
    /// (default `system:local`, a system actor, as the SQLite store does).
    pub fn with_author(mut self, author: impl Into<String>, kind: ActorKind) -> Self {
        self.author = author.into();
        self.author_kind = kind;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The UUID in `koine-store.json`; stamped as `origin` on rows that have
    /// none.
    pub fn store_id(&self) -> &str {
        &self.store_id
    }

    /// Hold the writer lock (D4.1) — what a sync tool does while it copies the
    /// root (D4.4). Do not call a mutating method on any handle of this
    /// store, in this thread, while the guard lives: that waits for itself.
    pub fn lock(&self) -> Result<FileLock, StoreError> {
        FileLock::exclusive(self.root.join("lock")).map_err(|e| storage("take the writer lock", e))
    }

    /// The packed clock in the `clock` file: the directory store's
    /// `data_version`. It moves whenever any process writes, so a reader that
    /// remembers the last value knows whether to look again. `0` for a store
    /// that has never advanced it.
    pub fn clock_file(&self) -> Result<u64, StoreError> {
        Ok(self.read_hlc()?.pack())
    }

    /// Raise the clock so every later write sorts after a `remote` clock that
    /// arrived by sync (the oracle's `hlc_observe_remote`).
    pub fn observe_clock(&self, remote: u64) -> Result<(), StoreError> {
        let _lock = self.lock()?;
        let mut hlc = self.read_hlc()?;
        let before = hlc;
        hlc.observe(remote);
        if hlc != before {
            self.write_hlc(&hlc)?;
        }
        Ok(())
    }

    /// The conflicts found when the store was opened (or last rescanned).
    pub fn conflicts(&self) -> Vec<Conflict> {
        self.conflicts.lock().map(|c| c.clone()).unwrap_or_default()
    }

    /// Look for conflict copies again, after a sync.
    pub fn rescan_conflicts(&self) -> Result<Vec<Conflict>, StoreError> {
        let found = self.scan_conflicts()?;
        *self.conflicts.lock().map_err(poisoned)? = found.clone();
        Ok(found)
    }

    // ── clock file ──────────────────────────────────────────────────────────

    fn read_hlc(&self) -> Result<Hlc, StoreError> {
        let path = self.root.join("clock");
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let mut parts = text.split_whitespace();
                let parsed = (|| {
                    let wall = parts.next()?.parse::<u64>().ok()?;
                    let counter = parts.next()?.parse::<u64>().ok()?;
                    parts.next().is_none().then_some(Hlc {
                        wall_ms: wall,
                        counter,
                    })
                })();
                parsed.ok_or_else(|| {
                    StoreError::Storage(format!(
                        "{} is not \"<wall_ms> <counter>\": {text:?}",
                        path.display()
                    ))
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Hlc::default()),
            Err(e) => Err(storage(format!("read {}", path.display()), e)),
        }
    }

    fn write_hlc(&self, hlc: &Hlc) -> Result<(), StoreError> {
        let path = self.root.join("clock");
        write_atomic(
            &path,
            format!("{} {}\n", hlc.wall_ms, hlc.counter).as_bytes(),
        )
        .map_err(|e| storage(format!("write {}", path.display()), e))
    }

    // ── journal ─────────────────────────────────────────────────────────────

    /// Complete every journal found. The caller holds the lock (or is `open`),
    /// so a journal found here belongs to a writer that is gone.
    fn replay_journals(&self) -> Result<(), StoreError> {
        let dir = self.root.join("journal");
        for (name, is_dir) in fsio::list_dir(&dir)? {
            if is_dir {
                continue;
            }
            let path = dir.join(&name);
            if name.starts_with('.') {
                // A temporary from a writer that died mid-journal; the
                // journal it was becoming never existed, so nothing to do.
                let _ = std::fs::remove_file(&path);
                continue;
            }
            if !name.ends_with(".json") {
                continue;
            }
            let bytes =
                std::fs::read(&path).map_err(|e| storage(format!("read {}", path.display()), e))?;
            let journal: Journal = serde_json::from_slice(&bytes)
                .map_err(|e| storage(format!("parse {}", path.display()), e))?;
            journal.apply(&self.root)?;
            journal.finish(&self.root)?;
        }
        Ok(())
    }

    /// Write `plan`: one file directly, several through the journal.
    fn commit_plan(&self, plan: &Journal) -> Result<(), StoreError> {
        match plan.writes.len() + plan.removes.len() {
            0 => Ok(()),
            1 => plan.apply(&self.root),
            _ => {
                plan.write(&self.root)?;
                plan.apply(&self.root)?;
                plan.finish(&self.root)
            }
        }
    }

    // ── reading ─────────────────────────────────────────────────────────────

    /// The directories that may hold item files, schema folders at any depth.
    fn item_dirs(&self) -> Result<Vec<PathBuf>, StoreError> {
        fsio::dirs_below(&self.root.join("items"))
    }

    /// Where the record `id` lives, and whether it is an operation.
    fn locate(&self, id: ItemId) -> Result<Option<(PathBuf, bool)>, StoreError> {
        let op = self.root.join(op_rel(id));
        if op.is_file() {
            return Ok(Some((op, true)));
        }
        let name = format!("{id}.json");
        for dir in self.item_dirs()? {
            let candidate = dir.join(&name);
            if candidate.is_file() {
                return Ok(Some((candidate, false)));
            }
        }
        Ok(None)
    }

    fn read_row_at(path: &Path, is_op: bool) -> Result<Option<Row>, StoreError> {
        match std::fs::read(path) {
            Ok(bytes) => Row::parse(&bytes, is_op, path).map(Some),
            // Deleted between the listing and the read.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(storage(format!("read {}", path.display()), e)),
        }
    }

    fn read_row(&self, id: ItemId) -> Result<Option<Row>, StoreError> {
        match self.locate(id)? {
            Some((path, is_op)) => Self::read_row_at(&path, is_op),
            None => Ok(None),
        }
    }

    /// The record files of one directory.
    fn files_in(dir: &Path) -> Result<Vec<(ItemId, PathBuf)>, StoreError> {
        Ok(fsio::list_dir(dir)?
            .into_iter()
            .filter(|(_, is_dir)| !is_dir)
            .filter_map(|(name, _)| record_id(&name).map(|id| (id, dir.join(name))))
            .collect())
    }

    /// Every record of the kind (exactly equal `schema`), or every record.
    /// Rows come in `(created, logical_clock, id)` order — the closest a
    /// directory has to SQLite's insertion order (items made in the same
    /// millisecond are told apart by the clock they were stamped with) — so
    /// an unsorted query is deterministic.
    fn scan_rows(&self, schema: Option<&SchemaRef>) -> Result<Vec<Row>, StoreError> {
        let mut files: Vec<(PathBuf, bool)> = Vec::new();
        let ops_dir = self.root.join("operations");
        match schema {
            Some(schema) => {
                if let Ok(rel) = schema_dir(schema.as_str()) {
                    files.extend(
                        Self::files_in(&to_path(&self.root, &rel))?
                            .into_iter()
                            .map(|(_, p)| (p, false)),
                    );
                }
                if schema.as_str() == OPERATION_SCHEMA {
                    files.extend(
                        Self::files_in(&ops_dir)?
                            .into_iter()
                            .map(|(_, p)| (p, true)),
                    );
                }
            }
            None => {
                for dir in self.item_dirs()? {
                    files.extend(Self::files_in(&dir)?.into_iter().map(|(_, p)| (p, false)));
                }
                files.extend(
                    Self::files_in(&ops_dir)?
                        .into_iter()
                        .map(|(_, p)| (p, true)),
                );
            }
        }
        let mut rows = Vec::with_capacity(files.len());
        for (path, is_op) in files {
            if let Some(row) = Self::read_row_at(&path, is_op)? {
                rows.push(row);
            }
        }
        rows.sort_by_key(|r| (r.item.created, r.item.logical_clock, r.item.id));
        Ok(rows)
    }

    /// Every item row (no operations), unordered.
    fn scan_items(&self) -> Result<Vec<Row>, StoreError> {
        let mut rows = Vec::new();
        for dir in self.item_dirs()? {
            for (_, path) in Self::files_in(&dir)? {
                if let Some(row) = Self::read_row_at(&path, false)? {
                    rows.push(row);
                }
            }
        }
        Ok(rows)
    }

    fn scan_conflicts(&self) -> Result<Vec<Conflict>, StoreError> {
        let mut dirs = self.item_dirs()?;
        dirs.push(self.root.join("operations"));
        let mut found = Vec::new();
        for dir in dirs {
            for (name, is_dir) in fsio::list_dir(&dir)? {
                if is_dir {
                    continue;
                }
                let Some(id) = fsio::conflict_copy_id(&name) else {
                    continue;
                };
                let copy = dir.join(&name);
                let file = dir.join(format!("{id}.json"));
                let clock_of = |p: &Path| -> Result<Option<u64>, StoreError> {
                    match std::fs::read(p) {
                        Ok(bytes) => serde_json::from_slice::<ClockOnly>(&bytes)
                            .map(|c| Some(c.logical_clock))
                            .map_err(|e| storage(format!("parse {}", p.display()), e)),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                        Err(e) => Err(storage(format!("read {}", p.display()), e)),
                    }
                };
                let Some(copy_clock) = clock_of(&copy)? else {
                    continue;
                };
                let file_clock = clock_of(&file)?;
                if file_clock.is_none_or(|c| c < copy_clock) {
                    found.push(Conflict {
                        id,
                        file,
                        file_clock,
                        copy,
                        copy_clock,
                    });
                }
            }
        }
        found.sort_by(|a, b| a.copy.cmp(&b.copy));
        Ok(found)
    }

    // ── events ──────────────────────────────────────────────────────────────

    /// Fan an event out to subscribers whose schema prefix matches `schema`
    /// (`None`, either side, matches everything), pruning dead ones.
    fn emit(&self, schema: Option<&str>, event: ItemEvent) {
        let Ok(mut subs) = self.subscribers.lock() else {
            return;
        };
        subs.retain(|(prefix, tx)| {
            let matches = match (prefix, schema) {
                (None, _) | (_, None) => true,
                (Some(prefix), Some(schema)) => schema.starts_with(prefix.as_str()),
            };
            !matches || tx.send(event.clone()).is_ok()
        });
    }

    fn emit_all(&self, events: Events) {
        for (schema, event) in events {
            self.emit(schema.as_deref(), event);
        }
    }

    // ── transactions ────────────────────────────────────────────────────────

    /// Take the lock, complete any journal a dead writer left, read the clock.
    fn begin(&self) -> Result<Txn<'_>, StoreError> {
        let lock = self.lock()?;
        self.replay_journals()?;
        let hlc = self.read_hlc()?;
        Ok(Txn {
            store: self,
            _lock: lock,
            hlc,
            hlc_before: hlc,
            rows: HashMap::new(),
            dirty: Vec::new(),
            removes: Vec::new(),
            blob_removals: Vec::new(),
            known_ids: None,
            events: Vec::new(),
        })
    }

    fn blob_dir(&self, item: ItemId) -> PathBuf {
        self.root.join("blobs").join(item.to_string())
    }
}

fn poisoned<T>(_: std::sync::PoisonError<T>) -> StoreError {
    StoreError::Storage("a store mutex was poisoned".into())
}

// ─────────────────────────────────────────────────────────────────────────────
// A transaction: everything one mutating call does, held in memory
// ─────────────────────────────────────────────────────────────────────────────

/// What `Txn::apply_op` did.
struct Applied {
    operation_id: ItemId,
    clock: u64,
}

/// The lock, the clock and an overlay of the rows a call has touched. Nothing
/// reaches the disk until [`Txn::commit`], so a call that fails part-way
/// (a missing target in the middle of a batch) has written nothing.
struct Txn<'a> {
    store: &'a DirStore,
    _lock: FileLock,
    hlc: Hlc,
    hlc_before: Hlc,
    rows: HashMap<ItemId, Option<Row>>,
    dirty: Vec<ItemId>,
    removes: Vec<String>,
    blob_removals: Vec<ItemId>,
    known_ids: Option<HashSet<ItemId>>,
    events: Events,
}

impl Txn<'_> {
    /// Know every id on disk up front (one listing instead of one search per
    /// id), for a batch.
    fn preload_ids(&mut self) -> Result<(), StoreError> {
        let mut ids = HashSet::new();
        for dir in self.store.item_dirs()? {
            ids.extend(DirStore::files_in(&dir)?.into_iter().map(|(id, _)| id));
        }
        ids.extend(
            DirStore::files_in(&self.store.root.join("operations"))?
                .into_iter()
                .map(|(id, _)| id),
        );
        self.known_ids = Some(ids);
        Ok(())
    }

    fn exists(&self, id: ItemId) -> Result<bool, StoreError> {
        if let Some(entry) = self.rows.get(&id) {
            return Ok(entry.is_some());
        }
        match &self.known_ids {
            Some(ids) => Ok(ids.contains(&id)),
            None => Ok(self.store.locate(id)?.is_some()),
        }
    }

    /// Bring `id` into the overlay; `false` when there is no such record.
    fn load(&mut self, id: ItemId) -> Result<bool, StoreError> {
        if !self.rows.contains_key(&id) {
            let row = self.store.read_row(id)?;
            self.rows.insert(id, row);
        }
        Ok(self.rows.get(&id).is_some_and(Option::is_some))
    }

    fn row(&self, id: ItemId) -> Option<&Row> {
        self.rows.get(&id).and_then(Option::as_ref)
    }

    fn mark_dirty(&mut self, id: ItemId) {
        if !self.dirty.contains(&id) {
            self.dirty.push(id);
        }
    }

    fn clock_of(&mut self, id: ItemId) -> Result<Option<u64>, StoreError> {
        Ok(if self.load(id)? {
            self.row(id).map(|r| r.item.logical_clock)
        } else {
            None
        })
    }

    /// The oracle's `insert_item`: a zero clock is stamped, a nonzero one
    /// kept verbatim (sync and undo restore keep their causal position), a
    /// missing origin is this store's, instants are millisecond-precise, tags
    /// and references are sets.
    fn insert(&mut self, mut item: Item) -> Result<ItemId, StoreError> {
        let id = item.id;
        if self.exists(id)? {
            return Err(StoreError::AlreadyExists(id));
        }
        schema_dir(item.schema.as_str())?;
        item.created = trunc_ms(item.created);
        item.modified = trunc_ms(item.modified);
        item.tags.sort();
        item.tags.dedup();
        let mut seen: Vec<(ItemId, EdgeType)> = Vec::new();
        item.references.retain(|r| {
            let key = (r.target, r.edge_type.clone());
            if seen.contains(&key) {
                false
            } else {
                seen.push(key);
                true
            }
        });
        if item.logical_clock == 0 {
            item.logical_clock = self.hlc.next();
        }
        if item.origin.is_none() {
            item.origin = Some(self.store.store_id.clone());
        }
        let schema = item.schema.to_string();
        let stored = item.clone();
        // Visible to its own foreign-key checks, as a SQLite row is.
        self.rows.insert(id, Some(Row { item, op: None }));
        if let Some(parent) = stored.parent {
            if !self.exists(parent)? {
                return Err(missing_reference("parent", id, parent));
            }
        }
        for reference in &stored.references {
            if !self.exists(reference.target)? {
                return Err(missing_reference("reference", id, reference.target));
            }
        }
        self.mark_dirty(id);
        self.events
            .push((Some(schema), ItemEvent::Created(Box::new(stored))));
        Ok(id)
    }

    /// The oracle's `apply_operation_on`: mint the operation row and
    /// materialize it on the target, stamping both with one clock.
    fn apply_op(&mut self, spec: &OperationSpec) -> Result<Applied, StoreError> {
        let target_id = spec.target_id;
        if !self.load(target_id)? {
            return Err(StoreError::NotFound(target_id));
        }
        let (target_schema, prev) = {
            let row = self.row(target_id).ok_or(StoreError::NotFound(target_id))?;
            self.store.policy.check(&row.item, &spec.op_type)?;
            (
                row.item.schema.to_string(),
                capture_previous(&row.item, &spec.op_type),
            )
        };
        match &spec.op_type {
            OperationType::AddReference(r) if !self.exists(r.target)? => {
                return Err(missing_reference("reference", target_id, r.target));
            }
            OperationType::SetParent(Some(parent)) if !self.exists(*parent)? => {
                return Err(missing_reference("parent", target_id, *parent));
            }
            _ => {}
        }

        let clock = self.hlc.next();
        let now = trunc_ms(Utc::now());
        let operation_id = Uuid::new_v4();
        let retention = if spec.intent == OperationIntent::Routine
            && spec.retention == RetentionTier::Compactable
        {
            routine_retention_for_schema(&target_schema)
        } else {
            spec.retention
        };
        let op_item = Item {
            id: operation_id,
            schema: SchemaRef::from_stored(OPERATION_SCHEMA.into()),
            payload: build_operation_payload(
                target_id,
                &spec.op_type,
                spec.intent,
                spec.reason.as_deref(),
                prev,
            ),
            created: now,
            modified: now,
            author: spec.author.clone(),
            author_kind: spec.author_kind,
            logical_clock: clock,
            origin: Some(self.store.store_id.clone()),
            canonical_id: None,
            tags: vec![],
            flag: None,
            is_read: false,
            is_starred: false,
            priority: Priority::Normal,
            visibility: Visibility::Private,
            message_type: None,
            produced_by: None,
            version: None,
            batch_id: spec.batch_id.clone(),
            references: vec![],
            parent: None,
        };
        self.rows.insert(
            operation_id,
            Some(Row {
                item: op_item,
                op: Some(OpMeta {
                    target: target_id,
                    retention,
                }),
            }),
        );
        self.mark_dirty(operation_id);

        let row = self
            .rows
            .get_mut(&target_id)
            .and_then(Option::as_mut)
            .ok_or(StoreError::NotFound(target_id))?;
        materialize(&mut row.item, &spec.op_type, now, clock);
        self.mark_dirty(target_id);
        self.events.push((
            Some(target_schema),
            ItemEvent::OperationApplied {
                operation_id,
                target_id,
            },
        ));
        Ok(Applied {
            operation_id,
            clock,
        })
    }

    /// The oracle's `delete`, with the cascades its foreign keys give: the
    /// operations that target the row go, its children lose their parent, and
    /// every reference to it (from any item) is dropped.
    fn delete(&mut self, id: ItemId) -> Result<(), StoreError> {
        let Some((path, is_op)) = self.store.locate(id)? else {
            return Err(StoreError::NotFound(id));
        };
        let row = DirStore::read_row_at(&path, is_op)?.ok_or(StoreError::NotFound(id))?;
        let schema = row.item.schema.to_string();
        let rel = row.rel_path()?;

        for (op_id, op_path) in DirStore::files_in(&self.store.root.join("operations"))? {
            if op_id == id {
                continue;
            }
            let Ok(bytes) = std::fs::read(&op_path) else {
                continue;
            };
            let target = serde_json::from_slice::<OpTargetOnly>(&bytes)
                .map_err(|e| storage(format!("parse {}", op_path.display()), e))?;
            if target.op_target_id == id {
                self.removes.push(op_rel(op_id));
            }
        }
        for mut other in self.store.scan_items()? {
            if other.item.id == id {
                continue;
            }
            let before = (other.item.parent, other.item.references.len());
            if other.item.parent == Some(id) {
                other.item.parent = None;
            }
            other.item.references.retain(|r| r.target != id);
            if before != (other.item.parent, other.item.references.len()) {
                let other_id = other.item.id;
                self.rows.insert(other_id, Some(other));
                self.mark_dirty(other_id);
            }
        }
        self.rows.insert(id, None);
        self.removes.push(rel);
        self.blob_removals.push(id);
        self.events.push((Some(schema), ItemEvent::Deleted(id)));
        Ok(())
    }

    /// The documents and removals this call amounts to, with the clock file
    /// brought up to date. The lock is still held.
    fn plan(&mut self) -> Result<(Journal, Events), StoreError> {
        let mut writes = Vec::new();
        let mut ids = Vec::new();
        for id in &self.dirty {
            if let Some(Some(row)) = self.rows.get(id) {
                writes.push(JournalWrite {
                    path: row.rel_path()?,
                    doc: row.doc()?,
                });
                ids.push(*id);
            }
        }
        ids.extend(self.blob_removals.iter().copied());
        if self.hlc != self.hlc_before {
            // Before the journal: a clock that is ahead of the documents is a
            // harmless gap; one that is behind them would repeat a value.
            self.store.write_hlc(&self.hlc)?;
            self.hlc_before = self.hlc;
        }
        Ok((
            Journal {
                batch: Uuid::new_v4().to_string(),
                created: Utc::now(),
                ids,
                writes,
                removes: std::mem::take(&mut self.removes),
            },
            std::mem::take(&mut self.events),
        ))
    }

    /// Write everything, release the lock, then announce.
    fn commit(mut self) -> Result<(), StoreError> {
        let (plan, events) = self.plan()?;
        self.store.commit_plan(&plan)?;
        for id in std::mem::take(&mut self.blob_removals) {
            let _ = std::fs::remove_dir_all(self.store.blob_dir(id));
        }
        let store = self.store;
        drop(self);
        store.emit_all(events);
        Ok(())
    }
}

fn missing_reference(kind: &str, from: ItemId, to: ItemId) -> StoreError {
    StoreError::Validation(format!("{from} has a {kind} to {to}, which does not exist"))
}

/// The value an operation overwrites, kept in the operation row so it can be
/// undone (`capture_previous_value`). Read from the typed payload, so a
/// boolean stays a boolean (SQLite's `json_extract` flattens it to 0/1).
fn capture_previous(item: &Item, op: &OperationType) -> Option<Value> {
    let field = |k: &str| item.payload.get(k).cloned().unwrap_or(Value::Null);
    match op {
        OperationType::SetRead(_) => Some(Value::Bool(item.is_read)),
        OperationType::SetStarred(_) => Some(Value::Bool(item.is_starred)),
        OperationType::SetFlag(_) => Some(match &item.flag {
            Some(f) => {
                let mut m = BTreeMap::new();
                m.insert("color".into(), Value::String(f.color.clone()));
                if let Some(s) = &f.style {
                    m.insert("style".into(), Value::String(s.clone()));
                }
                if let Some(l) = &f.length {
                    m.insert("length".into(), Value::String(l.clone()));
                }
                Value::Object(m)
            }
            None => Value::Null,
        }),
        OperationType::SetPriority(_) => Some(Value::String(item.priority.to_string())),
        OperationType::SetVisibility(_) => Some(Value::String(item.visibility.to_string())),
        OperationType::SetPayload(k, _) | OperationType::RemovePayload(k) => Some(field(k)),
        OperationType::PatchPayload(fields) => Some(Value::Object(
            fields.keys().map(|k| (k.clone(), field(k))).collect(),
        )),
        OperationType::SetParent(_) => Some(match item.parent {
            Some(p) => Value::String(p.to_string()),
            None => Value::Null,
        }),
        OperationType::AddTag(_)
        | OperationType::RemoveTag(_)
        | OperationType::AddReference(_)
        | OperationType::RemoveReference(..)
        | OperationType::Custom(..) => None,
    }
}

/// The oracle's `materialize_operation`: the target takes the operation's
/// clock, `modified` moves, and the change lands on the envelope or payload.
/// A payload key is a literal key (SQLite's `$.a.b` path would nest it).
fn materialize(item: &mut Item, op: &OperationType, now: DateTime<Utc>, clock: u64) {
    item.logical_clock = clock;
    item.modified = now;
    match op {
        OperationType::SetPayload(k, v) => {
            item.payload.insert(k.clone(), v.clone());
        }
        OperationType::RemovePayload(k) => {
            item.payload.remove(k);
        }
        OperationType::PatchPayload(fields) => {
            for (k, v) in fields {
                item.payload.insert(k.clone(), v.clone());
            }
        }
        OperationType::SetRead(v) => item.is_read = *v,
        OperationType::SetStarred(v) => item.is_starred = *v,
        OperationType::SetFlag(f) => item.flag = f.clone(),
        OperationType::AddTag(tag) => {
            if let Err(at) = item.tags.binary_search(tag) {
                item.tags.insert(at, tag.clone());
            }
        }
        OperationType::RemoveTag(tag) => item.tags.retain(|t| t != tag),
        OperationType::SetPriority(p) => item.priority = *p,
        OperationType::SetVisibility(v) => item.visibility = *v,
        // A second edge to the same target of the same type is ignored, its
        // metadata included (`INSERT OR IGNORE`).
        OperationType::AddReference(r) => {
            if !item
                .references
                .iter()
                .any(|e| e.target == r.target && e.edge_type == r.edge_type)
            {
                item.references.push(r.clone());
            }
        }
        OperationType::RemoveReference(target, edge) => item
            .references
            .retain(|e| !(e.target == *target && e.edge_type == *edge)),
        OperationType::SetParent(p) => item.parent = *p,
        OperationType::Custom(..) => {}
    }
}

/// A mutation that changes nothing mints no operation (`filter_noop_mutations`).
fn is_noop(item: &Item, m: &FieldMutation) -> bool {
    match m {
        FieldMutation::SetPayload(k, v) => item.payload.get(k) == Some(v),
        FieldMutation::RemovePayload(k) => !item.payload.contains_key(k),
        FieldMutation::SetRead(v) => item.is_read == *v,
        FieldMutation::SetStarred(v) => item.is_starred == *v,
        _ => false,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The traits
// ─────────────────────────────────────────────────────────────────────────────

impl ItemStore for DirStore {
    fn capabilities(&self) -> StoreCapabilities {
        StoreCapabilities {
            indexed_query: false,
            atomic_batch: true,
            subscribe: true,
            cross_process_events: false,
            durable: true,
            shared: true,
            blobs: true,
        }
    }

    fn insert(&self, item: Item) -> Result<ItemId, StoreError> {
        let mut tx = self.begin()?;
        let id = tx.insert(item)?;
        tx.commit()?;
        Ok(id)
    }

    fn insert_batch(&self, items: Vec<Item>) -> Result<Vec<ItemId>, StoreError> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let mut tx = self.begin()?;
        tx.preload_ids()?;
        let mut ids = Vec::with_capacity(items.len());
        for item in items {
            ids.push(tx.insert(item)?);
        }
        tx.commit()?;
        Ok(ids)
    }

    fn get(&self, id: ItemId) -> Result<Option<Item>, StoreError> {
        Ok(self.read_row(id)?.map(|r| r.item))
    }

    fn update(&self, id: ItemId, mutations: Vec<FieldMutation>) -> Result<(), StoreError> {
        // The oracle returns before looking the row up when there is nothing
        // to apply, so an update with no mutations succeeds on a missing id.
        if mutations.is_empty() {
            return Ok(());
        }
        let mut tx = self.begin()?;
        if !tx.load(id)? {
            return Err(StoreError::NotFound(id));
        }
        // Drop what changes nothing, against the state each mutation will see.
        let mut scratch = tx
            .row(id)
            .map(|r| r.item.clone())
            .ok_or(StoreError::NotFound(id))?;
        let now = trunc_ms(Utc::now());
        let mut kept = Vec::new();
        for m in mutations {
            if is_noop(&scratch, &m) {
                continue;
            }
            materialize(&mut scratch, &OperationType::from(m.clone()), now, 0);
            kept.push(m);
        }
        let batch_id = (kept.len() > 1).then(|| Uuid::new_v4().to_string());
        for m in kept {
            tx.apply_op(&OperationSpec {
                target_id: id,
                op_type: m.into(),
                intent: OperationIntent::Routine,
                reason: None,
                batch_id: batch_id.clone(),
                author: self.author.clone(),
                author_kind: self.author_kind,
                retention: RetentionTier::Compactable,
            })?;
        }
        tx.commit()
    }

    fn delete(&self, id: ItemId) -> Result<(), StoreError> {
        let mut tx = self.begin()?;
        tx.delete(id)?;
        tx.commit()
    }

    fn query(&self, q: &ItemQuery) -> Result<Vec<Item>, StoreError> {
        let mut rows = self.matching_rows(q)?;
        eval::sort_rows(&mut rows, &q.sort, Row::eval_row);
        Ok(eval::page(rows, q)
            .into_iter()
            .map(|mut r| {
                if !q.include_tags {
                    r.item.tags.clear();
                }
                if !q.include_references {
                    r.item.references.clear();
                }
                r.item
            })
            .collect())
    }

    fn count(&self, q: &ItemQuery) -> Result<usize, StoreError> {
        Ok(self.matching_rows(q)?.len())
    }

    fn neighbors(
        &self,
        id: ItemId,
        edge_types: &[EdgeType],
        depth: u32,
    ) -> Result<Vec<Item>, StoreError> {
        if depth == 0 {
            return Ok(Vec::new());
        }
        let items: HashMap<ItemId, Item> = self
            .scan_items()?
            .into_iter()
            .map(|r| (r.item.id, r.item))
            .collect();
        let mut adjacent: HashMap<ItemId, Vec<ItemId>> = HashMap::new();
        // Outgoing edges first, then incoming, as the oracle walks them.
        let mut incoming: HashMap<ItemId, Vec<ItemId>> = HashMap::new();
        let mut sources: Vec<&Item> = items.values().collect();
        sources.sort_by_key(|i| (i.created, i.id));
        for item in sources {
            for r in &item.references {
                if eval::edge_admitted(edge_types, &r.edge_type) {
                    adjacent.entry(item.id).or_default().push(r.target);
                    incoming.entry(r.target).or_default().push(item.id);
                }
            }
        }
        let mut visited = HashSet::from([id]);
        let mut frontier = vec![id];
        let mut out = Vec::new();
        for _ in 0..depth {
            let mut next = Vec::new();
            for node in &frontier {
                let around = adjacent
                    .get(node)
                    .into_iter()
                    .chain(incoming.get(node))
                    .flatten();
                for other in around {
                    if items.contains_key(other) && visited.insert(*other) {
                        next.push(*other);
                    }
                }
            }
            out.extend(next.iter().filter_map(|n| items.get(n).cloned()));
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        Ok(out)
    }

    fn subscribe(&self, q: ItemQuery) -> Result<Receiver<ItemEvent>, StoreError> {
        let (tx, rx) = mpsc::channel();
        self.subscribers
            .lock()
            .map_err(poisoned)?
            .push((q.schema.map(|s| s.to_string()), tx));
        Ok(rx)
    }
}

impl DirStore {
    fn matching_rows(&self, q: &ItemQuery) -> Result<Vec<Row>, StoreError> {
        let mut ctx = eval::Context::default();
        for source in eval::needed_sources(q) {
            let refs = self
                .read_row(source)?
                .map(|r| r.item.references)
                .unwrap_or_default();
            ctx.sources.insert(source, refs);
        }
        let mut rows = self.scan_rows(q.schema.as_ref())?;
        rows.retain(|r| eval::matches(q, &r.eval_row(), &ctx));
        Ok(rows)
    }
}

impl AttributedStore for DirStore {
    fn apply_operation(&self, spec: OperationSpec) -> Result<ItemId, StoreError> {
        let mut tx = self.begin()?;
        let applied = tx.apply_op(&spec)?;
        tx.commit()?;
        Ok(applied.operation_id)
    }

    fn apply_operation_if_clock(
        &self,
        spec: OperationSpec,
        expected_clock: u64,
    ) -> Result<GuardedWrite, StoreError> {
        let mut tx = self.begin()?;
        match tx.clock_of(spec.target_id)? {
            Some(clock) if clock == expected_clock => {}
            other => return Ok(GuardedWrite::Moved { clock: other }),
        }
        let applied = tx.apply_op(&spec)?;
        tx.commit()?;
        Ok(GuardedWrite::Applied {
            operation_id: applied.operation_id,
            clock: applied.clock,
        })
    }

    fn apply_operations_if_clock(
        &self,
        specs: Vec<OperationSpec>,
        guard: ItemId,
        expected_clock: u64,
    ) -> Result<GuardedBatch, StoreError> {
        if specs.is_empty() {
            return Err(StoreError::Validation(
                "a guarded batch needs at least one operation".into(),
            ));
        }
        let mut tx = self.begin()?;
        match tx.clock_of(guard)? {
            Some(clock) if clock == expected_clock => {}
            other => return Ok(GuardedBatch::Moved { clock: other }),
        }
        let batch_id = Uuid::new_v4().to_string();
        let mut operation_ids = Vec::with_capacity(specs.len());
        for mut spec in specs {
            spec.batch_id = Some(batch_id.clone());
            operation_ids.push(tx.apply_op(&spec)?.operation_id);
        }
        let clock = tx.clock_of(guard)?.ok_or(StoreError::NotFound(guard))?;
        tx.commit()?;
        Ok(GuardedBatch::Applied {
            operation_ids,
            clock,
        })
    }

    fn logical_clock_of(&self, id: ItemId) -> Result<Option<u64>, StoreError> {
        Ok(self.read_row(id)?.map(|r| r.item.logical_clock))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Blobs (D5)
// ─────────────────────────────────────────────────────────────────────────────

/// A blob name is one plain file name: not empty, no `/` or `\`, no `..`, no
/// leading `.` (which is also what keeps a half-written temporary out of
/// [`BlobStore::list_blobs`]).
fn check_blob_name(name: &str) -> Result<(), StoreError> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name.contains("..")
        || name.starts_with('.')
    {
        return Err(StoreError::Validation(format!(
            "invalid blob name {name:?}"
        )));
    }
    Ok(())
}

impl BlobStore for DirStore {
    fn put_blob(&self, item: ItemId, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
        check_blob_name(name)?;
        let _lock = self.lock()?;
        let path = self.blob_dir(item).join(name);
        write_atomic(&path, bytes).map_err(|e| storage(format!("write {}", path.display()), e))
    }

    fn get_blob(&self, item: ItemId, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        check_blob_name(name)?;
        let path = self.blob_dir(item).join(name);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(storage(format!("read {}", path.display()), e)),
        }
    }

    fn delete_blob(&self, item: ItemId, name: &str) -> Result<(), StoreError> {
        check_blob_name(name)?;
        let _lock = self.lock()?;
        let path = self.blob_dir(item).join(name);
        remove_if_present(&path).map_err(|e| storage(format!("remove {}", path.display()), e))?;
        // Tidy the item's folder once it holds nothing (fails harmlessly if not).
        let _ = std::fs::remove_dir(self.blob_dir(item));
        Ok(())
    }

    fn list_blobs(&self, item: ItemId) -> Result<Vec<String>, StoreError> {
        Ok(fsio::list_dir(&self.blob_dir(item))?
            .into_iter()
            .filter(|(name, is_dir)| !is_dir && !name.starts_with('.'))
            .map(|(name, _)| name)
            .collect())
    }
}
