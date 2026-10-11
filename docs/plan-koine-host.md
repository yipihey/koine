# Koine host: an agent, a library, a bespoke app

The aim: point an agent at koine and a Rust library and get a bespoke app
that exposes the whole library — verbs, generated surfaces, a layout, a
page in a browser, a terminal, MCP and a CLI — with nothing installed and
nothing of impress-apps checked out. Six stumbling blocks stood in the way
when this was first assessed; this plan removes them in order of how hard
each would stop the agent. The stores (`docs/plan-koine-store.md`) were the
first step and are done.

| Block | Decision |
|---|---|
| Koine has no host: nothing in it runs a verb over HTTP or holds a surface | H-D1–H-D5: the runtimes come to koine, generic over the store port, and `koine-host` serves them |
| The scaffold and the capability guide are in impress-apps | H-D6 |
| No mechanical "did I expose everything" | H-D7 |
| Long work and big results have no default path | H-D4 (a default job runner, a blob route); plots stay placed, not drawn (H-D9) |
| Type mapping is undocumented and the macro is strict | H-D6 (DTO rules, compile-fail tests whose messages name the fix) |
| Nouns without a store | H-D8 |

## Decisions

### H-D1. The runtimes are generic over the port

`impress-surface-service` and `impress-layout-service` already wrap their
rows in `SurfaceStore<S: AttributedStore>` and `LayoutStore<S>`, and then
pin `S = SqliteItemStore` at about fifty sites in their services, runtimes,
presets and rename passes. They become `Arc<dyn AttributedStore>` through
and through. Three things they take from SQLite beyond the port become
port methods, with defaults every backend can answer and SQLite overriding
with its tables:

```rust
// on AttributedStore
fn operations_for(&self, target: ItemId, limit: Option<usize>) -> Result<Vec<Item>, StoreError>;
    // default: query schema `core/operation`, Eq("op_target_id", target), oldest first (logical_clock asc, as SQLite answers)
fn metadata_get(&self, key: &str) -> Result<Option<String>, StoreError>;
fn metadata_set(&self, key: &str, value: &str) -> Result<(), StoreError>;
    // default: one `core/store-metadata` item per key, payload {key, value}
// on ItemStore
fn change_token(&self) -> Result<u64, StoreError>;
    // "something committed": SQLite's data_version, the directory's clock file,
    // the memory store's clock. Monotonic within a handle; not comparable across backends.
```

The conformance suite gains a case for each. The builtin kind manifest is
a value passed to a runtime at construction, never a global the runtime
calls: a bespoke app passes its own manifest.

### H-D2. The pane-query compiler moves to koine

`impress_core::pane_query::{compile, compile_with, CompiledQuery, SubtreeResolver}`
and the `invalidation` module use only the item, query and reference types
koine already owns. They move to `impress-pane-query::compile`, tested on
the memory store; impress-core keeps `builtin_manifest()` and re-exports the
compiler so no caller changes.

### H-D3. The two runtime crates move to koine

After H-D1 and H-D2 they depend on impress-core for nothing a parameter
cannot carry. They move as crates, Tier A tests on the memory store, Tier B
(HTTP against a running app) unchanged. impress-apps depends on them by
git rev like every other kit crate. The pure route tables that
`impress-store-ffi` keeps for the surface and verb routes (`route_of`:
method and path to verb name, id and arguments) move into the service
crates as `http::route`, so the Swift FFI and the headless host call one
table.

### H-D4. `koine-host`

An axum server, as a library (`koine_host::serve(Config)`) and a binary.
Routes, each a call into the inventory through the pipeline:

| Route | Answers |
|---|---|
| `GET /api/status` | app id, port, store URL and capabilities, the verb count |
| `/api/surface/*` | the surface route table (H-D3) |
| `/api/layout/{tree,verb,op,layouts}` | the layout route table |
| `POST /api/verb/<name>` | one verb by name, strict, through the pipeline |
| `GET /ui/*` | the `koine-web` bundle from a directory (`Config.web_dir`), `index.html` with the loopback token for a loopback peer, exactly as `KoineWebRoutes` in impress-apps does |
| `GET/PUT/DELETE /api/blob/<item>/<name>` | `BlobStore`, when the store declares `blobs` |
| `/mcp` | the MCP host (H-D5) |

The checks every impress server applies, applied here: a `Host` header that
is not this machine is 400; a non-GET from loopback needs this launch's
loopback token, written to `<workspace>/automation/loopback-<port>.token`
by the same code (`impress_core::loopback_token` is pure std and moves to
`koine_host::token`); no CORS header ever. The host installs a default
`BackgroundRunner` (tokio tasks with job rows in the store) so `in_background`
works with no app code, and opens its store from a URL (`koine-store`).
The binary links `surface-demo-service`, the settings service and the two
runtimes, so `koine-host --store dir:~/Library/koine-demo --port 23130`
runs on any machine and `koine-web` talks to it.

### H-D5. `koine-mcp-host`

`impress-mcp-host` is already product-neutral (JSON-RPC and MCP mechanics
over the inventory, axum). It moves to koine unchanged in behaviour;
impress-mcp depends on it by rev.

### H-D6. The agent's path lives where the grammar lives

- `docs/capability.md`: "How to add a Rust capability" moves here from
  impress-apps' `docs/agent-surfaces.md` with the DTO rules: the five
  argument and result shapes that work (serde + schemars structs, enums of
  unit variants or tagged structs, `Option`, `Vec`, maps of strings) and
  the three that do not (lifetimes, generics the macro cannot name,
  foreign types without `JsonSchema`), each with the wrapper to write.
- `impress-service-macros` gains compile-fail tests (`trybuild`) for the
  refusals an agent meets first: a missing `effects`, an unknown adverb in
  `supports`, an argument without `JsonSchema`, a method that is not
  `async`; each message says what to write.
- `scripts/new-app.sh <name> --lib <crate>`: a service crate with `dto.rs`,
  a `service.rs` whose every method has an `effects` template, a Tier A
  selftest, a host binary that links it, a `Dioxus.toml` for the page, a
  `schema-refs.json`, and a README that is the agent's checklist.

### H-D7. "Did I expose everything" is a script

`scripts/check-verb-coverage.sh` comes to koine with a library mode:
`--lib <crate>` lists the library's public functions and types
(`cargo doc --output-format json` or `rustdoc-json`), matches each to a verb
in the linked inventory or to a line in `verb-exclusions.toml` with a
reason, and fails on silence. The same listing answers the agent's first
question, what is there to expose.

### H-D8. Nouns without a store: a verb says what it returns

`#[impress_method(returns_kind = "paper")]` puts `returns_kind` on the
descriptor; the inventory, the verb docs and a surface's table can then
agree on columns, ids and selection for rows a verb produces. A bespoke
app is surfaces over verbs: its layout's panes are `surface` panes, and
no view kind is written in Swift or Dioxus for it.

### H-D9. Not in this plan

Plots drawn in the browser (`plot{spec}` stays placed), conflict merge for
synced directory stores, S3, mobile, moving impress's domain services
(`impress-store-service` and the app services stay in impress-apps).

## Work packages

| # | Package | Where | Depends on |
|---|---|---|---|
| K1 | Port methods (H-D1) with defaults, memory and dir implementations, suite cases; the compiler and invalidation moved to `impress-pane-query::compile` with tests on the memory store (H-D2) | koine | — |
| K2 | impress-apps: SQLite implements the new methods; impress-core re-exports the compiler; the two runtimes generic over `dyn AttributedStore`; route tables into the service crates; the rev bump | impress-apps | K1 |
| K3 | The two runtime crates move to koine; impress-apps depends by rev; kit manifest | both | K2 |
| K4 | `koine-host` and `koine-mcp-host`, the token, the job runner, the demo binary; `koine-web` against it | koine | K3 |
| K5 | `docs/capability.md`, compile-fail tests, `scripts/new-app.sh` (H-D6) | koine | — |
| K6 | `returns_kind` (H-D8); `scripts/check-verb-coverage.sh --lib` (H-D7) | koine | — |
| K7 | End to end: an agent-shaped run of `new-app.sh` over a small library, served by `koine-host`, driven by a scenario over HTTP | koine | K4, K5, K6 |
