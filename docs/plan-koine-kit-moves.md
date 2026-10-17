# Plan: the general-purpose kernels leave impress-apps

**Builds on:** plan-koine-ai-tasks.md (the pattern: a kernel runs over the
store port, impress-apps re-exports it and keeps SQLite's fast path),
plan-koine-research-os.md P-D (packages), plan-koine-local-models.md (the
desk agent these kernels feed), impress-apps `docs/plan-koine-separation.md`.

## Why

Koine is the set of capabilities apps and whole systems are built from.
impress-apps still holds kernels that know nothing about publications,
manuscripts or mail: triage, collections, agent memory and embeddings,
scenario and workflow verbs, the verb transport's client, the Python
binding, the capability and performance verbs, modal text editing. A
`scripts/new-app.sh` app cannot have any of them, and the desk agent that
now runs on local models has no memory to recall from.

An app must not compile all of koine to use part of it. Two mechanisms keep
it modular:

- **Code is a crate per capability.** An app links the crates whose verbs it
  wants (`#[impress_service]` verbs register when linked); heavy dependencies
  (fastembed, HNSW, pyo3) are features that are off by default.
- **Content is a koine package.** A capability's surfaces, skills, workflows
  and AI categories are a built-in package (`builtin:<name>`, the kit's own
  mechanism, B-D4) that the crate carries. A host installs the packages of
  the capabilities it links, reviewed and proposed like any other, so the
  data an app ships follows the code it chose.

## What moves

Found by the test "depends on no app's domain crate; needs only the store
port, or SQLite only as a fast path" over impress-apps' 70 crates.

| impress-apps today | Lines | Goes to | Stays in impress-apps |
|---|---|---|---|
| `perf-service` | 411 | `perf-service` (same name) | — |
| `impress-app-transport` | 1.6k | `impress-app-transport` (same name) | — |
| `capabilities-service` | 0.6k | `capabilities-service`, deployment over the port | — |
| `impress-py` | 0.3k | `koine-py`: the module builder over whatever inventory is linked, behind `python` | `impress-py`, a thin cdylib that links impress's inventory |
| `impress-tags`, `impress-flags` | 2.4k | same names; `uniffi` behind an `ffi` feature | — |
| `impress-core::triage_ops`, `impress-store-service::triage_service` | 0.6k | `koine-triage` over `AttributedStore` | re-exports; the SQLite deployment |
| `impress-core::collection_ops` (generic kernel + `collection@1.0.0`), `collection_service` | 4.4k | `koine-collections` | the app schema bindings (`imbib/collection`, `manuscript-collection`, `figure-collection`) |
| `settings_service` | 0.8k | `koine-settings-service` over `impress-settings` (already koine's) | re-export |
| `impress-scenario-service` | 2.6k | `impress-scenario-service` (same name) over the port | — |
| `impress-workflow-service` | 1.7k | `impress-workflow-service` over the port's arrival feed | — |
| `impress-core::memory_ops`, `impress-memory-service`, `impel-memory` | 10.3k | `koine-memory` (kernel, verbs, executors) | re-exports; SQLite's FTS as the text-search fast path |
| `impress-embeddings` | 2.5k | `koine-embeddings`, used by `koine-memory` behind `embeddings` | — |
| `impress-helix` | 5.9k | `impress-helix` (same name), `uniffi` behind `ffi` | the xcframework build, now of the koine crate |

Staying, and why: the academic libraries (`im-bibtex`, `im-identifiers`,
`impress-sources`, `impress-smart-search`) are a domain, and koine owns
none; `impress-mcp`, `impress-bridges-service`, `impress-capabilities` and the
FFI crates are impress's assembly; `impress-ai-http` and `impel-core`'s
threads and personas are plan-koine-ai-tasks' "Not yet"; `sync` and `backup`
are SQLite's own; `docs_import_service`, `source_service`,
`manuscript_collab_service` and `figure_detection` in `impress-store-service`
are domain. `history_service` and `audit` overlap koine-notebook's call log;
reconciling them is its own decision (Open).

## Decisions

### KM-D1. Names, verbs and schema refs do not change

A crate that moves unchanged keeps its name (K0: renames are a repository
cut, not a precondition), so impress-apps swaps a path dependency for the
koine git dependency and no `use` changes. A module that leaves
`impress-core` is re-exported there (`impress_core::triage_ops` is
`koine_triage::ops`), as `task` and `job` were. Verb names
(`triage-service_*`, `collection-service_*`, `memory-service_*`, …) and schema
refs (`collection@1.0.0`, the memory schemas, `core/verb-call@1.0.0`) keep
their spelling: agents, skills and stored rows know them.

### KM-D2. A kernel is written against the port; SQLite keeps its fast path

As Q-D2: the kernel runs over `AttributedStore` and is tested over the
memory and directory backends with the conformance suite; impress-core keeps
any SQL that is faster as its implementation of the port or of an extension.
No kernel takes `SqliteItemStore`.

### KM-D3. The port gains two optional extensions

- **Text search.** `TextSearch::search_text(&self, schema, text, limit) ->
  Vec<(ItemId, f32)>`, ranked. The default, for any store, scores token
  overlap over a `Contains`-narrowed query; `SqliteItemStore` answers from
  FTS. The memory kernel's dedup gate and recall, and grouped global search,
  use it.
- **Arrival feed.** `ArrivalFeed::arrivals_after(cursor, schema, limit) ->
  (Vec<Item>, cursor)`: rows in the order this store received them, including
  rows sync applied with old timestamps (the reason a `created` query is not
  enough). Memory and directory backends keep a monotonic arrival counter
  (the directory one in a file beside the items); SQLite uses its rowid. The
  workflow runner's `store`/`message`/`job`/`call` triggers read it.

Both are conformance cases every backend passes.

### KM-D4. Heavy dependencies are features

`koine-memory` without `embeddings` recalls by text search alone;
`embeddings` adds `koine-embeddings` (fastembed, HNSW, its sidecar index).
`koine-py` needs `python` for pyo3. `impress-helix` and the tag/flag crates
need `ffi` for UniFFI. A host turns on what it serves.

### KM-D5. A capability carries its package

Each moved capability crate with content exposes `pub fn package() ->
koine_packages::Package` (built-in, `builtin:<name>`): its skill text, its
surfaces, its workflows (installed `proposed`) and its AI categories, with
`requires.verbs` naming its own verbs. `koine-host` installs the packages of
the capabilities it links at first start, through the same review rule as
the kit's. impress-apps installs the same packages, so its agents and
surfaces get the same content.

### KM-D6. The desk agent remembers

With `koine-memory` linked, the memory verbs are in the inventory and so in
the model's tools; the flat Apple catalog's default allowlist gains
`memory-service_recall`. koine-host links `koine-memory` (without
`embeddings`) by default.

### KM-D7. Both repositories stay green at every step

Each package lands in koine with its tests, then impress-apps adopts it on
branch `koine-kit-moves` through a temporary `[patch]` to the local koine
checkout, and the affected crates build and test. At the end koine `main` is
pushed, impress-apps pins that revision, the `[patch]` is removed, and the
full impress-apps Rust gate runs.

## How a move lands

The same steps for every package, so each one is reviewable and both
repositories stay green.

**In koine** (`/Users/tabel/Projects/koine`, branch `kit-moves`):

1. Copy the crate into `crates/<name>` with `git mv`-like fidelity (keep its
   module layout, docs and tests), add it to the root `members` and
   `[workspace.dependencies]`, and add `koine-workspace-hack.workspace = true`
   to its dependencies like every other member.
2. Replace every `impress-core` / `SqliteItemStore` use with the port
   (`impress_item::store::{ItemStore, AttributedStore}`) per KM-D2; anything
   that has no port equivalent yet is a KM3 extension, not a reason to keep
   SQLite.
3. Move its tests and add backend tests over `koine-store-memory` and
   `koine-store-dir`. Heavy dependencies go behind features (KM-D4).
4. `cargo test -p <crate>` (with and without its features),
   `cargo clippy --workspace --all-targets -- -D warnings`,
   `cargo fmt --all --check`; commit.

**In impress-apps** (`/Users/tabel/Projects/impress-apps`, branch
`koine-kit-moves`, which builds koine from `../koine` by path):

1. Remove the moved crate from `members` and delete its directory; point its
   `[workspace.dependencies]` entry at `{ path = "../koine/crates/<name>" }`
   (KM8 turns every such path into the pushed git rev), and add the same
   path entry to `.config/hakari.toml`'s `third-party` exclusions.
2. A module that left `impress-core` is re-exported there with the same path
   (`pub use koine_triage::ops as triage_ops;`), and SQLite's faster
   implementation stays in impress-core as the port impl.
3. `cargo hakari generate`, `cargo check --workspace --all-targets`, the
   tests of every crate that used the moved one, `./scripts/rust-gate.sh fmt`,
   `./scripts/rust-gate.sh clippy auto`, `./scripts/check-kit-deps.sh --strict`,
   `./scripts/check-uniffi-bindings.sh`; update verb docs/tests that count
   verbs (`docs/verb-coverage.md`, `docs/verb-safety.md`, `docs/verb-effects.md`,
   `crates/impress-capabilities/tests/effects.rs`) only if the inventory
   changed; commit. Never set a private `CARGO_TARGET_DIR`; never bypass hooks.
   Run `cargo test -p impress-capabilities` alone (feature unification with
   other crates changes its verb census).

## Work packages

| # | Package | Repos |
|---|---|---|
| KM0 | impress-apps adopts koine `main` (the local-models work): pin bump, struct literals, typed callers | impress-apps |
| KM1 | `perf-service`, `impress-app-transport`, `capabilities-service`, `koine-py` | both |
| KM2 | `impress-tags`, `impress-flags`, `koine-triage` | both |
| KM3 | The port's `TextSearch` and `ArrivalFeed`, in every backend and the conformance suite; SQLite's implementations in impress-core | both |
| KM4 | `koine-collections`, `koine-settings-service`, `impress-scenario-service`, `impress-workflow-service` (with its runner) | both |
| KM5 | `koine-memory`, `koine-embeddings`; the desk agent's memory (KM-D6) | both |
| KM6 | `impress-helix` | both |
| KM7 | Built-in packages for the moved capabilities (KM-D5); koine-host installs them. The package format (manifest, digest, `Package::from_parts`) moves out of `koine-packages` into a dependency-free `koine-package-format`, so carrying a package does not link the notebook and agent verbs (found in KM2) | koine |
| KM8 | impress-apps pins the pushed koine `main`, removes the moved crates and the `[patch]`; the full gate | both |

## What pins this

| Rule | Test |
|---|---|
| A moved kernel behaves the same on every backend | its tests over `MemoryStore` and `DirStore`, and impress-core's over SQLite |
| Text search ranks and the arrival feed sees late-synced rows | `koine-store-conformance` cases, every backend |
| A verb keeps its name and schema | each crate's inventory test; impress-apps' existing verb tests unchanged |
| An app links only what it uses | `new-app.sh`'s e2e builds without memory, embeddings or Python |
| impress-apps builds with everything sourced from koine | the impress-apps Rust gate on `koine-kit-moves` |

## Open

- `history_service` and `audit` against koine-notebook's call log: one of the
  two should own `core/verb-call@1.0.0`.
- `scripts/new-app.sh --python` (a stub cdylib calling `koine_py::register`) is
  not done: it needs a pyo3 `extension-module` build and a maturin or
  setup step the script's e2e does not have yet.
- koine has no `.config/hakari.toml`, so `koine-workspace-hack` was not
  regenerated for the new crates; the member lines are in place.

## Status

- **KM1 done** on branch `kit-moves` (koine) and `koine-kit-moves`
  (impress-apps). `perf-service` (4 verbs), `impress-app-transport` and
  `capabilities-service` (4 verbs) moved under the same names; `koine-py` is
  new. API changes: `impress-app-transport` takes
  `loopback_token::client_token_for_url` from `impress-service-core` (already
  there; impress-core still re-exports it); `DefaultCapabilitiesService` holds
  an `Arc<dyn AttributedStore>` (`with_store` takes the port) and defaults to
  `koine_store::process::acquire_or_stand_in`, so a host that registers the
  process ports gets its own store (impress-capabilities' `force_link` now
  calls `register_process_ports`). `koine-py` exposes `list_verb_infos`,
  `call_local`, `call_remote`, `reload_provider_registry` and, behind
  `python` (pyo3 0.23, pythonize 0.23), `register(m, Hooks)`; `Hooks` carries
  what only the binary knows (`force_link`, `reload_providers`).
  impress-apps' `impress-py` stays the `impress` cdylib with identical module
  name and behaviour. Two tests that need impress left the transport crate for
  `implore-verbs-ffi`: the implore parity stub and the Swift port-table pin.
  koine tests: perf-service 6, impress-app-transport 20 + 2, capabilities-service
  7 (two over the memory and directory stores), koine-py 4 (+1 interpreter test,
  `--ignored`, needs `PYTHONHOME`), koine-host 84; impress-apps: impress-py 3,
  implore-verbs-ffi 12, impress-capabilities 56 (with `KOINE_CHECKOUT`, while
  the lock has no koine git pin), impel-tools 14, impress-ai-tools 6,
  impress-mcp 49. `docs/verb-coverage.md` gained the two service rows (and the
  impress-ai-service count is corrected to 17).
- **KM2 done.** `impress-tags` (14 + 48 + 27 + 3 tests, the rest doc tests) and
  `impress-flags` moved under the same names; their UniFFI feature is now `ffi`
  (was `native`, which nothing in impress-apps enabled on them but a
  workspace-wide `--features native` clippy run). No UniFFI type of either
  crosses into `imbib-core`'s interface (it calls plain functions), so no
  dependent enables `ffi` and imbib-core's binding is unchanged. The tags
  BibTeX `keywords` export is not in the crate (it is only the doc comment's
  intent), nothing to leave behind. `koine-triage` is new: `ops` (the kernel,
  generic over `S: ItemStore + ?Sized`, so a caller passes `&SqliteItemStore`,
  `&dyn AttributedStore` or a `MemoryStore` alike), `service` (the five
  `triage-service_set-starred|set-flag|add-tag|remove-tag|set-status` verbs,
  names unchanged; `DefaultTriageService::with_store(Arc<dyn AttributedStore>)`,
  default `koine_store::process::acquire_or_stand_in`) and `package()`
  (`builtin:triage`, one `SKILL.md`, `requires.verbs` the five verbs). Features
  `service` and `package` are on by default; impress-core takes
  `default-features = false` so it links only the kernel, not the verb
  machinery or the package manager. Not yet wired into koine-host (KM7).
  Tests: koine-triage 15 (kernel and service over `MemoryStore` and `DirStore`,
  the process-store default, package validity, digest stability, required
  verbs linked, review clean).
- **KM3 done.** Both extensions are default methods on `ItemStore`:
  `search_text(&self, schema: Option<&str>, text: &str, limit: usize) ->
  Result<Vec<TextHit>, StoreError>` (`TextHit { id, score: f32 }`; default
  tokenizes, narrows with `Contains` over the FTS field list
  `impress_item::TEXT_FIELDS`, scores the fraction of query tokens present,
  ties by occurrences then recency) and `arrivals_after(&self, cursor:
  ArrivalCursor, schema: Option<&str>, limit: usize) -> Result<(Vec<Item>,
  ArrivalCursor), StoreError>` (default `StoreError::Unsupported`, a new
  variant). `ArrivalCursor` is a serde-transparent `u64` newtype
  (`START`, `to_u64`, `from_u64`). `StoreCapabilities` gained
  `text_search_indexed` and `arrival_feed`. Memory: the row's insertion
  number; dir: an append-only `arrivals` log (`<id>\t<schema>`) appended under
  the writer lock in `Txn::plan`, which also adopts files a sync tool copied
  in; SQLite: bm25 over `items_fts` and the rowid keyset (`arrival_page`).
  Conformance: 4 text-search cases, 7 arrival cases, `reopen_keeps_the_arrival_feed`;
  memory, dir (koine) and SQLite memory + file (impress-core) pass all.
  Known, not from KM3: `koine-host`'s `tests/host.rs` hangs under
  `cargo test --workspace` (feature unification; reproduced on the commit
  before KM3) and passes with `-p koine-host`; imbib-core's
  `tag_count_effects_are_the_same_on_cold_and_warm_cache_reads` is flaky on the
  global effects spy.
- **KM4 done.** `koine-collections` (kernel `ops` over `&dyn AttributedStore`,
  the generic `collection@1.0.0` binding, the marker `is_migrated_on`; the
  twelve `collection-service_*` verbs behind `service`; `builtin:collections`
  behind `package`) and `koine-settings-service` (the six `settings-service_*`
  verbs over `impress-settings`; settings files in the process workspace,
  `koine_store::process::set_workspace`). An app registers its bindings by
  name (`register_binding`) and may offer the convergence verbs
  (`set_migration`, a `CollectionMigration`); impress registers its three
  bindings and `SqliteCollectionMigration` from `register_process_ports`. The
  port gained `ItemStore::default_author` (the author a kernel stamps when the
  caller names none). `impress-scenario-service` and `impress-workflow-service`
  moved under the same names, verbs unchanged. Two port-level pieces came with
  them: `koine_store::process::CallStore` (a store as the port and as the
  concrete-typed call override, so a verb that downcasts the call's store to
  its backend still finds it) and `Sink::flush` / `audit::flush` on the
  pipeline's audit sink (both sinks answer it; scenario recording drains the
  call log through it). A Tier A scenario runs on a scratch `CallStore`, a
  `MemoryStore` unless the host calls `tier_a::set_scratch_store_factory`;
  impress-capabilities' `scenario` feature points it at an in-memory SQLite
  store (`impress_store_service::scratch_call_store`). The workflow runner's
  `store`/`message`/`job`/`call` triggers read `arrivals_after`;
  `WorkflowEngine::run_once` takes a `CallStore` (re-exported from the crate),
  and its three hosts (impress-store-ffi's tick, impel-taskd, imbib-service's
  retention test) wrap their `Arc<SqliteItemStore>`. Stayed in impress-apps:
  `interpreter.gaps.json` (it drives the store-query verbs; now under
  `impress-store-service/scenarios`, run by impress-capabilities'
  `scenario_catalogue` test with the whole inventory) and the history-miner
  half of "a proposed workflow never runs" (impress-store-service's
  `history_service` tests). Callers that held `&Arc<SqliteItemStore>` pass
  `&*store` to the port-typed collection kernel. Tests (koine):
  koine-collections 60 + 10 kernel (+14 with `package`), koine-settings-service
  7, impress-scenario-service 18 + 10 (memory and dir backends),
  impress-workflow-service 3 + 9 (the engine over memory and dir).
  impress-apps: impress-core 525, impress-store-service 132 (+2), imbib-core
  988, impress-capabilities all green with `KOINE_CHECKOUT`. `check-kit-deps
  --strict` reports only the expected path-dependency entries until KM8.
- **KM5 done.** `koine-embeddings` is `impress-embeddings` unchanged (`store`,
  `index`, `embedder` features). `koine-memory` carries the kernel (`ops`,
  over `&dyn AttributedStore`: near-duplicate and query retrieval are the
  port's `search_text`; heads are computed from the memory rows' own
  `Supersedes` references instead of a `NOT EXISTS` over SQLite's edge table;
  `memory_status` counts through `ItemStore::count`), the seven
  `memory-service_*` verbs (`service`, default), the vector tier in its own
  module behind `embeddings` (`embedder` adds the local model; without it
  `memory_status` reports the tier `off` and recall keeps the text order), and
  impel-memory's executors and spawner behind `executors` (koine-tasks
  `runtime`, koine-ai; the candidate keyset scan is one port query per
  embeddable schema, merged; tasks are created through
  `koine_tasks::store::kernel::create_item`). koine-host links koine-memory
  without embeddings, and `DEFAULT_NATIVE_TOOLS` gains
  `memory-service_recall` (still under the cap). impress-apps keeps the old
  crate names as workspace aliases (`impress-memory-service` and
  `impel-memory` are `koine-memory`, the latter with `executors`;
  `impress-embeddings` is `koine-embeddings`), so no `use` changed except
  impel-taskd's `impel_memory::executors::` paths; `vector-embedder` is now
  `embedder`. impress-core re-exports the kernel as `memory_ops` and keeps
  `tests/prop_memory_ops.rs` over SQLite. The memory verbs read the process
  store, so impress-capabilities' `memory` feature and the MCP brief resource
  register the process ports. `memory-service_memory-status` is now visible
  to the effects spy (its exception row is gone). Found on the way: the
  default `search_text` retrieves by any token where SQLite's FTS ANDs them,
  so a gate fixture whose bodies differed by one token that the fixture also
  used (`Tokens: 42` vs run 42) confirmed on the memory store; the fixture now
  names runs `r{i}`. Tests (koine): koine-embeddings 24, koine-memory 67
  (75 with `embeddings`, 86 + 37 executors with `executors`) + 2 backends
  (memory, dir), koine-host 43 + 34. impress-apps: impress-core with the
  SQLite memory property tests, impress-mcp, impel-taskd, impel-tools,
  impress-cli, imbib-semantic-service, imbib-core 988, impress-capabilities.
  Not carried: the executors' 2k-line integration test now runs on
  `MemoryStore` only; impress-apps has no SQLite run of it.
- **KM6 done.** `impress-helix` moved unchanged (UniFFI behind `ffi`).
  impress-apps keeps `crates/impress-helix` as the `impress-helix-ffi`
  package (`pub use impress_helix::*`, `ffi` turns on the koine crate's), so
  the xcframework build, CI path filters, the xcframework action and
  `Package.swift` did not move. Cargo writes `libimpress_helix_ffi.a`; the
  xcframework keeps `libimpress_helix.a` in every slice. A macOS build was
  run: 34 `uniffi_impress_helix` exports in the archive and the generated
  Swift byte-identical to the committed binding. `check-uniffi-bindings.sh`
  reads helix's exports from the source cargo resolved.
- **KM7 done.** `koine-package-format` (serde, serde_json, sha2) holds
  `Manifest`, `Package` (`from_parts`, `load`, `load_path`, bundles,
  `digest`), the entry types, `category_errors`, `sha256` and
  `ShippedError`; koine-packages re-exports them under the old paths, and
  `Package::workflow_spec` became `package::workflow_spec` (it needs
  impress-workflow). koine-triage, koine-collections and koine-memory carry
  their packages against the format alone (`cargo tree -p koine-triage
  --features package` reaches no koine-packages, notebook or agent);
  koine-packages is their dev-dependency for the review test. koine-memory
  ships `builtin:memory` (`SKILL.md`, `requires.verbs` its seven verbs), and
  koine-host installs `builtin_packages()` beside the kit at start
  (`the_kit_the_call_log_and_the_skill_are_served` checks it). koine-host
  links no rusqlite, fastembed, HNSW or pyo3. The scenario, workflow,
  settings, capabilities, perf and transport crates carry no content, so no
  package.
