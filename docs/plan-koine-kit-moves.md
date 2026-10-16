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
| KM7 | Built-in packages for the moved capabilities (KM-D5); koine-host installs them | koine |
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
