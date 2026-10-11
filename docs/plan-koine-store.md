# Koine stores: one port, several backends

Koine's records — surfaces, layouts, settings, the call log, and every
kind an app declares — are items in a store. The store has been one
implementation, `SqliteItemStore` in impress-apps, behind a port that
already lives here: `impress-item`'s `ItemStore` (the item CRUD, queries,
neighbours, events) and `AttributedStore` (attributed operations and the
compare-and-swap on a logical clock). This plan adds the second and third
implementations, the suite that proves every implementation is the same
store, and the one addition the port needs so a host can tell them apart.

The reasons, in the order they matter:

- **A bespoke app needs a store it can have by default.** An agent pointed
  at koine and a library should get persistence from a directory in the
  app's own Library folder, with nothing to install and nothing to
  configure.
- **Legible records.** One JSON file per item is readable by `cat`,
  diffable by git, repairable by hand, and syncable by rsync, iCloud, git
  or a mounted remote folder — the suite's "user-controlled sync"
  principle made literal.
- **Blobs beside their items.** A PDF next to its record instead of a path
  in a payload field pointing elsewhere.
- **The port stays honest.** A second backend is what proves the port is a
  port. Every method SQLite answers one way and a directory answers
  another way is a decision this plan writes down.

What this plan is not: a database. The directory store is for an app's own
records — tens of thousands of items, scan-based queries — and large data
stays in files that items reference.

## Decisions

### D1. The port is `impress-item`, plus a capabilities declaration

`ItemStore` and `AttributedStore` are the port, unchanged in every method.
One method is added to `ItemStore`:

```rust
fn capabilities(&self) -> StoreCapabilities;

pub struct StoreCapabilities {
    /// Predicates and sorts are served by an index; a query's cost does not
    /// grow with the number of items of other kinds.
    pub indexed_query: bool,
    /// `insert_batch` and `apply_operations_if_clock` are all-or-nothing
    /// even if the process dies mid-way.
    pub atomic_batch: bool,
    /// `subscribe` delivers events for writes made by THIS process.
    pub subscribe: bool,
    /// `subscribe` also delivers events for writes made by other processes.
    pub cross_process_events: bool,
    /// Records survive the process.
    pub durable: bool,
    /// Two processes may open the same store at once and each sees the
    /// other's committed writes.
    pub shared: bool,
    /// The store holds blobs beside items (`BlobStore`, D5).
    pub blobs: bool,
}
```

A host reads it and refuses or degrades explicitly: a layout host that
needs `shared` says so at open, a query planner that sees
`indexed_query: false` warns past a size, a job runner that needs
`cross_process_events` polls instead. Nothing silently slows.

### D2. The semantics every backend shares

Spelled from `SqliteItemStore`, which is the oracle for the suite (D6):

- **Ids** are the item's own (`Item::new` assigns a v4 UUID); inserting an
  existing id is `AlreadyExists`.
- **The logical clock is a hybrid logical clock** packed as
  `(wall_ms << 16) | counter`, store-wide, monotonic, advanced on every
  insert and every operation; the target row is stamped with it. A
  backend keeps the last `(wall_ms, counter)` and never answers a clock
  twice. Clocks from two backends therefore sort together, which is what
  sync needs.
- **`update` and `apply_operation` both materialize** onto the target
  (tags, flag, read, starred, priority, visibility, payload set/remove/
  patch, references, parent) and bump the clock and `modified` (the two
  reference operations stamp the clock but leave `modified` alone);
  `apply_operation` also records the operation row with its author,
  intent, reason, batch and retention, and `OperationApplied` is emitted
  (`update` emits one per applied mutation).
- **Guarded writes** compare the target's clock inside the same atomic
  step as the write; `Moved { clock }` writes nothing. The batch form
  guards on one row and applies every spec or none.
- **Queries** evaluate every `Predicate` (field comparisons on `payload.*`
  and the item's own columns, `Contains`, `In`, tags, flag, read, starred,
  parent, references in both directions, `And`/`Or`/`Not`), sort by the
  named fields (`created`, `modified`, `logical_clock`, `payload.<key>`),
  then `offset` and `limit`; `count` is the same without the fetch.
  A `schema` of `None` means every kind. The schema ref matches by exact
  equality, as the suite's rule says.
- **`neighbors`** is a breadth-first walk over references up to `depth`,
  filtered by edge type, never returning the start item.
- **Events** are `Created`, `Deleted` and `OperationApplied` (`update` sends
  one `OperationApplied` per applied mutation, never `Updated { mutations }`:
  the variant exists in `impress-item` and no backend sends it), delivered
  to subscribers whose query's schema matches by prefix (the SQLite rule),
  from this process.
- **Immutable kinds** (manuscript revisions and changes) refuse payload
  mutation. That is a rule of the suite's schema, not of the store; it is
  applied through a `Policy` hook a backend takes at open (default: none)
  so koine's backends do not learn impress's kinds.

### D3. `koine-store-memory`

`BTreeMap<ItemId, Item>` and an operation map behind one `RwLock`, the
clock beside them, subscribers as `mpsc::Sender`s pruned on send failure.
Capabilities: everything but `durable`, `shared`, `cross_process_events`
and `blobs`. It is the suite's reference backend, the host's default when
no store is named, and the test double every koine crate uses.

### D4. `koine-store-dir`

The layout, under one root:

```
<root>/koine-store.json          format version, created, store id
<root>/clock                     "wall_ms counter", rewritten on every advance
<root>/lock                      the writer lock (impress-fs-lock)
<root>/items/<schema ref as path>/<id>.json
<root>/operations/<id>.json
<root>/journal/<batch id>.json   pending atomic batch (D4.2)
<root>/blobs/<item id>/<name>    D5
```

A schema ref is a path: `imbib/library` is `items/imbib/library/`,
`task@1.0.0` is `items/task@1.0.0/`. One file per item, written as
temp-and-rename. A query lists one schema folder (or every folder when the
schema is `None`), parses, filters, sorts in memory.

**D4.1 Writers take the lock.** Every mutating call takes the exclusive
lock for its duration; readers do not. Two processes may open the root;
one writes at a time; readers see whole files only (rename is atomic).

**D4.2 Batches are journaled.** `insert_batch` and
`apply_operations_if_clock` write the whole batch to `journal/<id>.json`
first, then apply file by file, then delete the journal. `open` replays
any journal it finds, so a batch is all-or-nothing across a crash.
`atomic_batch: true` is earned, not assumed.

**D4.3 Events are in-process.** `subscribe` works for this process's
writes; `cross_process_events: false`. A watcher is a later change and a
capability flip, not a redesign. A reader that wants to know whether
another process wrote reads `clock` (one small file), which is the role
`data_version` plays for SQLite.

**D4.4 Sync is rsync of the root while no writer holds the lock.** Two
roots synced by a file tool are two stores with the same ids; the HLC
keeps their clocks comparable. A file that arrives with a lower clock
than the local copy is a conflict, and this plan does not resolve it:
`open` reports such files as `conflicts` and leaves both. Conflict copies
and a merge policy are a later plan.

Capabilities: `durable`, `shared`, `atomic_batch`, `subscribe`, `blobs`;
not `indexed_query`, not `cross_process_events`.

### D5. Blobs

A small trait beside the port, `BlobStore`, with `put(item, name, bytes)`,
`get`, `delete`, `list(item)`. The directory store implements it on
`blobs/`; the memory store holds bytes; SQLite does not implement it (its
`blobs: false`), which is honest: the suite's PDFs live outside its file.
The host's `/api/blob/<item>/<name>` route (plan-koine-web D12) is served
from this trait.

### D6. The conformance suite: `koine-store-conformance`

A library crate of named cases over `&dyn AttributedStore` (and
`&dyn BlobStore` when `blobs`), each a function, with one macro that
instantiates them as `#[test]`s for a backend:

```rust
koine_store_conformance::suite! {
    memory => || koine_store_memory::MemoryStore::new()
}
```

The cases are the semantics of D2 written as assertions, and the oracle is
`SqliteItemStore`'s own ninety tests in impress-apps: every behaviour they
pin that the port exposes is a case here. A case consults
`capabilities()` and skips with a reason, never silently, when the backend
declares it cannot (an in-memory store is not asked to survive a reopen).
Koine runs the suite against memory and dir in CI; impress-apps runs the
same crate against SQLite (its dev-dependency), so the three can only
drift into a failing test.

### D7. `koine-store`: the name and the URL

A facade crate that re-exports the port and opens a backend from a URL:
`memory:`, `dir:<path>`. Another backend registers its scheme with the
facade (`sqlite:` is impress-core's to register), so a host takes one
string and never names a backend.

### D8. Not in this plan

- Moving the surface and layout runtimes in impress-apps onto
  `dyn AttributedStore`. They also use the pane-query compiler and the
  builtin manifest, which are impress-core's today; that move is its own
  plan and the headless host's first dependency.
- Cross-process events for the directory store (a watcher), conflict
  copies and merge, an S3 or WebDAV backend, an index for the directory
  store.

## Work packages

| # | Package | Where |
|---|---|---|
| S0 | This document; `StoreCapabilities` and `capabilities()` on the port; `BlobStore`; crate skeletons and workspace members | `crates/impress-item`, root `Cargo.toml` |
| S1 | `koine-store-memory` and `koine-store-conformance` (the suite over the reference backend) | new crates |
| S2 | `koine-store-dir` with its own tests, then the suite | new crate |
| S3 | `koine-store` facade; CI; README | new crate, `.github` |
| S4 | impress-apps: `SqliteItemStore::capabilities`, the suite as a dev-dependency, the rev bump | impress-apps |

## Status (2026-10-11)

S0–S3 are built on koine main. What the suite established:

- **The suite is the oracle's behaviour, not a reading of it.** Its eighty
  cases were run against a scratch build of `SqliteItemStore` itself (144
  passed, in-memory and file-backed) before either new backend passed
  them. Where D2 above had paraphrased SQLite wrongly (an `Updated` event
  that is never sent; `modified` on reference operations), the text moved
  to the oracle.
- **Two blind implementations disagreed exactly where expected.** The
  directory store, written without seeing the memory store, failed three
  cases (a blob for a missing item, the blob-name rules, `Not` over a
  missing field) and differed on four more the suite does not pin
  (no-op filtering judged against the initial row, `modified` on
  reference operations, undo values for booleans, reference order). All
  seven moved to the oracle. This is the drift the suite exists to catch,
  seen once on purpose.
- **One evaluation.** `koine-store-memory::eval` is the predicate, sort
  and paging evaluation for every scanning backend; the directory store
  depends on it for that and nothing else.
- **Capabilities as declared:** memory `atomic_batch, subscribe, blobs`;
  dir `durable, shared, atomic_batch, subscribe, blobs`; SQLite (S4)
  `indexed_query, atomic_batch, subscribe`, and `durable, shared` when
  file-backed, `blobs: false`.
- **Known limits:** the suite takes a store by value and has no cleanup
  hook, so each directory case leaves a small root under the OS temp dir;
  the directory store's `delete` is linear in the store (parent and
  reference cleanup) and nothing is fsynced, so it survives a process
  exit and not a power loss; cross-process events, conflict copies and
  an index remain D8.

