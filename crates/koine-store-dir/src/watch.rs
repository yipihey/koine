//! The watcher (`docs/plan-koine-followups.md` F-D4): other processes' writes
//! become events.
//!
//! [`DirStore::watch`] starts one thread, `koine-store-dir-watch`, over
//! `notify` (inotify on Linux, FSEvents on macOS) watching `items/` and
//! `operations/`. The thread keeps a cache of every record it has seen,
//! built by one scan when it starts; a change to a record file is read back
//! and compared with the cache, and what differs is announced through the
//! same subscriber list the in-process writes use:
//!
//! | the file | the cache | event |
//! |----------|-----------|-------|
//! | item, new | no | `Created` |
//! | item, higher `logical_clock` | yes | `Updated` with [`diff_items`] |
//! | item, gone | yes | `Deleted` |
//! | operation, new | no | `OperationApplied` |
//!
//! Events are gathered over a quiet window ([`QUIET`]) so the creates,
//! modifies and renames of one temp-and-rename are one look at the file, and
//! an OS `Rescan` (or an error) is a full reconcile: the cache against a fresh
//! scan, announcing the same things.
//!
//! **Own writes are not echoed.** Every commit records the `(id, clock)` of
//! the files it writes (a removal is `(id, 0)`) *before* they reach the disk;
//! the watcher, finding a file carrying one, updates its cache and says
//! nothing, because the in-process write already did. Two handles of one
//! process on one root hear each other, as two processes do.
//!
//! The thread ends within a few tens of milliseconds of the store being
//! dropped (`DirStore::drop` sets a flag the thread checks every wake-up);
//! dropping never waits for it.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify::{EventKind, RecursiveMode, Watcher};

use super::*;

/// How long the file system must be quiet before the changes are read.
const QUIET: Duration = Duration::from_millis(50);

/// A stream that never goes quiet is read at least this often.
const LONGEST_WAIT: Duration = Duration::from_millis(500);

/// Every record seen: items by id, operations by id with their target.
#[derive(Default)]
struct Cache {
    items: HashMap<ItemId, Item>,
    /// Operation id -> (target, clock).
    ops: HashMap<ItemId, (ItemId, u64)>,
}

impl Cache {
    /// Read every record under `root`; a file that does not parse is skipped.
    fn scan(root: &Path) -> Cache {
        let mut cache = Cache::default();
        let dirs = fsio::dirs_below(&root.join("items")).unwrap_or_default();
        for dir in dirs {
            for (_, path) in DirStore::files_in(&dir).unwrap_or_default() {
                if let Ok(Some(row)) = DirStore::read_row_at(&path, false) {
                    cache.items.insert(row.item.id, row.item);
                }
            }
        }
        for (_, path) in DirStore::files_in(&root.join("operations")).unwrap_or_default() {
            if let Ok(Some(row)) = DirStore::read_row_at(&path, true) {
                if let Some(op) = row.op {
                    cache
                        .ops
                        .insert(row.item.id, (op.target, row.item.logical_clock));
                }
            }
        }
        cache
    }
}

/// What the thread needs of the store, all shared.
pub(crate) struct Worker {
    root: PathBuf,
    subscribers: Arc<Subscribers>,
    own: Arc<OwnWrites>,
    cache: Cache,
}

/// Which kind of record a path is, and its id.
fn classify(root: &Path, path: &Path) -> Option<(bool, ItemId)> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Vec<&str> = rel
        .components()
        .map(|c| c.as_os_str().to_str())
        .collect::<Option<_>>()?;
    let id = record_id(parts.last()?)?;
    match *parts.first()? {
        "items" if parts.len() >= 3 => Some((false, id)),
        "operations" if parts.len() == 2 => Some((true, id)),
        _ => None,
    }
}

/// `path` if it is a record file, or every record file below it if it is a
/// directory (a schema folder that appeared with its files already in it).
fn expand(root: &Path, path: &Path, out: &mut BTreeSet<PathBuf>) {
    if path.is_dir() {
        for (name, is_dir) in fsio::list_dir(path).unwrap_or_default() {
            if name.starts_with('.') {
                continue;
            }
            let child = path.join(name);
            if is_dir {
                expand(root, &child, out);
            } else if classify(root, &child).is_some() {
                out.insert(child);
            }
        }
    } else if classify(root, path).is_some() {
        // A file, or one that is gone (a delete is a path that does not exist).
        out.insert(path.to_path_buf());
    }
}

impl Worker {
    fn item_present(&mut self, item: Item) {
        let own = self.own.take((item.id, item.logical_clock));
        match self.cache.items.get(&item.id) {
            None => {
                if !own {
                    self.subscribers.emit(
                        Some(item.schema.as_str()),
                        ItemEvent::Created(Box::new(item.clone())),
                    );
                }
            }
            Some(old) if !own && item.logical_clock > old.logical_clock => {
                let event = ItemEvent::Updated {
                    id: item.id,
                    mutations: diff_items(old, &item),
                };
                self.subscribers.emit(Some(item.schema.as_str()), event);
            }
            // Ours, or not newer: the cache just follows the disk.
            Some(_) => {}
        }
        self.cache.items.insert(item.id, item);
    }

    fn item_absent(&mut self, id: ItemId) {
        if let Some(old) = self.cache.items.remove(&id) {
            if !self.own.take((id, 0)) {
                self.subscribers
                    .emit(Some(old.schema.as_str()), ItemEvent::Deleted(id));
            }
        }
    }

    fn op_present(&mut self, id: ItemId, target: ItemId, clock: u64) {
        let own = self.own.take((id, clock));
        if self.cache.ops.insert(id, (target, clock)).is_none() && !own {
            let schema = self.cache.items.get(&target).map(|t| t.schema.to_string());
            self.subscribers.emit(
                schema.as_deref(),
                ItemEvent::OperationApplied {
                    operation_id: id,
                    target_id: target,
                },
            );
        }
    }

    fn op_absent(&mut self, id: ItemId) {
        // An operation going is the cascade of its target's delete (or a
        // compaction): the in-process store says nothing either.
        self.cache.ops.remove(&id);
        self.own.take((id, 0));
    }

    /// Look at one record file and announce what changed.
    fn look(&mut self, is_op: bool, id: ItemId, path: &Path) {
        match DirStore::read_row_at(path, is_op) {
            Ok(Some(row)) => match row.op {
                Some(op) => self.op_present(id, op.target, row.item.logical_clock),
                None => self.item_present(row.item),
            },
            Ok(None) if is_op => self.op_absent(id),
            Ok(None) => self.item_absent(id),
            // Half-synced or corrupt: the next event looks again.
            Err(_) => {}
        }
    }

    /// Look at the records a batch of paths names: items first (so an
    /// operation's target is already cached), then operations.
    fn process(&mut self, paths: BTreeSet<PathBuf>) {
        let mut expanded = BTreeSet::new();
        for path in &paths {
            expand(&self.root, path, &mut expanded);
        }
        let mut records: Vec<(bool, ItemId, PathBuf)> = expanded
            .into_iter()
            .filter_map(|p| classify(&self.root, &p).map(|(is_op, id)| (is_op, id, p)))
            .collect();
        records.sort();
        records.dedup();
        for (is_op, id, path) in records {
            self.look(is_op, id, &path);
        }
    }

    /// The cache against a fresh scan.
    pub(crate) fn reconcile(&mut self) {
        let fresh = Cache::scan(&self.root);
        let gone: Vec<ItemId> = self
            .cache
            .items
            .keys()
            .filter(|id| !fresh.items.contains_key(id))
            .copied()
            .collect();
        for id in gone {
            self.item_absent(id);
        }
        for (_, item) in fresh.items {
            self.item_present(item);
        }
        let gone: Vec<ItemId> = self
            .cache
            .ops
            .keys()
            .filter(|id| !fresh.ops.contains_key(id))
            .copied()
            .collect();
        for id in gone {
            self.op_absent(id);
        }
        for (id, (target, clock)) in fresh.ops {
            self.op_present(id, target, clock);
        }
    }
}

/// What a batch of notify events asks for.
#[derive(Default)]
struct Batch {
    paths: BTreeSet<PathBuf>,
    rescan: bool,
}

impl Batch {
    fn add(&mut self, event: notify::Result<notify::Event>) {
        match event {
            Err(_) => self.rescan = true,
            Ok(event) => {
                if event.need_rescan() {
                    self.rescan = true;
                }
                if !matches!(event.kind, EventKind::Access(_)) {
                    self.paths.extend(event.paths);
                }
            }
        }
    }
}

fn run(
    mut worker: Worker,
    rx: Receiver<notify::Result<notify::Event>>,
    watching: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) {
    worker.cache = Cache::scan(&worker.root);
    watching.store(true, Ordering::SeqCst);
    'outer: while !stop.load(Ordering::SeqCst) {
        let mut batch = Batch::default();
        match rx.recv_timeout(QUIET) {
            Ok(event) => batch.add(event),
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let started = Instant::now();
        loop {
            if stop.load(Ordering::SeqCst) {
                break 'outer;
            }
            if started.elapsed() >= LONGEST_WAIT {
                break;
            }
            match rx.recv_timeout(QUIET) {
                Ok(event) => batch.add(event),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        if batch.rescan {
            worker.reconcile();
        } else {
            worker.process(batch.paths);
        }
    }
    watching.store(false, Ordering::SeqCst);
}

impl DirStore {
    /// Start the watcher (once; calling it again is a no-op). From here on a
    /// record another handle or process writes under this root reaches
    /// [`ItemStore::subscribe`]rs as `Created`, `Updated`, `Deleted` or
    /// `OperationApplied`, and `capabilities().cross_process_events` is true
    /// — as soon as the thread has made its first scan of the root, which
    /// [`DirStore::wait_until_watching`] waits for. `Err` when the OS
    /// refuses to watch (inotify's limits, a root on a file system that has
    /// no notifications).
    pub fn watch(&self) -> Result<(), StoreError> {
        if self.watch_started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.start_watcher().inspect_err(|_| {
            self.watch_started.store(false, Ordering::SeqCst);
        })
    }

    fn start_watcher(&self) -> Result<(), StoreError> {
        // FSEvents reports resolved paths; so is the root the paths are
        // classified against.
        let root = std::fs::canonicalize(&self.root)
            .map_err(|e| storage(format!("resolve {}", self.root.display()), e))?;
        let (tx, rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = tx.send(event);
        })
        .map_err(|e| storage("start the file watcher", e))?;
        // Both paths in one commit: FSEvents restarts its stream for every
        // `watch` call, which cost about 1.5 s each on macOS.
        let mut paths = watcher.paths_mut();
        for dir in ["items", "operations"] {
            let path = root.join(dir);
            std::fs::create_dir_all(&path).map_err(|e| storage(format!("create {dir}/"), e))?;
            paths
                .add(&path, RecursiveMode::Recursive)
                .map_err(|e| storage(format!("watch {}", path.display()), e))?;
        }
        paths
            .commit()
            .map_err(|e| storage("start watching the root", e))?;
        let worker = Worker {
            root,
            subscribers: self.subscribers.clone(),
            own: self.own_writes.clone(),
            cache: Cache::default(),
        };
        let (watching, stop) = (self.watching.clone(), self.stop.clone());
        std::thread::Builder::new()
            .name("koine-store-dir-watch".into())
            .spawn(move || {
                // The watcher lives as long as the thread does.
                let _watcher = watcher;
                run(worker, rx, watching, stop);
            })
            .map_err(|e| storage("start the watcher thread", e))?;
        Ok(())
    }

    /// Wait until the watcher has made its first scan and is delivering
    /// (`capabilities().cross_process_events`). `false` on timeout, or when
    /// [`DirStore::watch`] was never called.
    pub fn wait_until_watching(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while !self.watching.load(Ordering::SeqCst) {
            if Instant::now() >= deadline || !self.watch_started.load(Ordering::SeqCst) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }
}

/// A worker with a cache of what is on disk now, for the tests to drive.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    pub(crate) fn worker(
        root: &Path,
        subscribers: Arc<Subscribers>,
        own: Arc<OwnWrites>,
    ) -> Worker {
        let root = std::fs::canonicalize(root).unwrap();
        Worker {
            cache: Cache::scan(&root),
            root,
            subscribers,
            own,
        }
    }
}
