# koine

Koine is the layout and surface grammar for [impress](https://github.com/yipihey/impress-apps).

A window is a tree of panes. A pane shows a query. A gesture (focus, activate, move, drag, resize) is data, and the grammar decides which layout verb that gesture already is. A surface is a spec reduced to a render tree. A style sheet names shared slots; each frontend supplies its own value kind. None of this opens a database or draws pixels.

The store, the Apple frontends, and the service crates that construct `SqliteItemStore` stay in impress-apps. That repository depends on this one.

The parts of speech — nouns, pointing, verbs, adjectives, adverbs, mood and consent — and the rules each one follows are [docs/grammar.md](docs/grammar.md). An adjective is a named predicate over records that any query may use; an adverb is a modifier on one call (`tentatively`, `with_review`, `once`, `reversibly`, `within`, `in_background`) that every verb accepts through one envelope the invoker pipeline checks.

The records every part of this speaks — surfaces, layouts, settings, the call log, an app's own kinds — are items in a store, and the store is a port with three backends: `SqliteItemStore` in impress-apps, `koine-store-memory` and `koine-store-dir` here (one JSON file per item, blobs beside it, rsync-able). `koine-store-conformance` is the suite that keeps them the same store, and `koine-store` opens one from a URL (`memory:`, `dir:<path>`). The design is [docs/plan-koine-store.md](docs/plan-koine-store.md).

The document has three performances: SwiftUI in impress-apps' `ImpressSurface`, plain text in `koine-tui`, and the browser. The browser one is `koine-dioxus` (one Dioxus mapping from `RenderTree` to HTML, performed as a string on the server, as the DOM in a browser and as a native window) under the page `koine-web`, which any impress app serves at `/ui/`. The design and its decisions are [docs/plan-koine-web.md](docs/plan-koine-web.md); `crates/koine/tests/frontends.rs` is the conformance suite that performs each tree twice and compares.

```bash
cargo test --workspace --all-targets
cargo check -p koine-dioxus -p koine-web --target wasm32-unknown-unknown
cd crates/koine-web && dx serve --hotpatch     # the page, live against a running app
```

`koine-host` runs it: one process that serves every linked `#[impress_service]` verb (`POST /api/verb/<name>`), the surface and layout runtimes (`/api/surface/*`, `/api/layout/*`), the `koine-web` bundle (`/ui/`), the store's blobs and MCP (`/mcp`, the `koine-mcp-host` crate) over a store opened from a URL, behind the suite's Host, loopback-token and no-CORS checks, with a default job runner for `in_background`. `cargo run -p koine-host -- --store dir:~/koine-demo --port 23130 --web-dir <bundle>` is a working app with the demo verbs linked; the design is [docs/plan-koine-host.md](docs/plan-koine-host.md).

Every host also links the research kit ([docs/plan-koine-research-os.md](docs/plan-koine-research-os.md)):

- **The lab notebook** (`koine-notebook`, [docs/plan-koine-notebook.md](docs/plan-koine-notebook.md)). The call log is kept in the store. Each record carries the tree of calls made inside it, with total and self time, the records it read and wrote, the run it belonged to, and the host and build. The `notebook-service_*` verbs and the Notebook surface cover runs, the timeline, step timings, notes, comparing runs, replay and a Markdown export.
- **The agent desk** (`koine-agent`). A person leaves work for whichever agent is connected: a question that carries what they are pointing at, a logged call to diagnose, or a view to build. The agent picks it up with `agent-service_next` and answers it. The host also serves the app's generated skill at `GET /api/skill` and as the MCP resource `koine://skill`, and `<host> skill install` writes it into `~/.claude/skills`, `~/.codex/skills` and `~/.agents/skills`.
- **Packages and shipped surfaces** (`koine-packages`). A shipped surface keeps its baseline beside the person's copy. An upgrade is three-way, `package-service_shipped-reset` puts the shipped version back, and `_shipped-diff` shows what changed. A package directory is reviewed (`_inspect`) and then installed only at the digest that was reviewed. The kit's own three surfaces are a package built into the binary.

An app from a library: `scripts/new-app.sh stats --lib mini-stats --lib-path ../mini-stats` writes a cargo workspace with a service crate and a host crate over it. An agent turns the library's public functions into `#[impress_method]` verbs (the guide below, then `scripts/check-verb-coverage.sh` for what it missed), and `cargo run -p stats-host` serves them over HTTP and MCP, with a generated form per verb (`GET /api/verb/<name>/surface`) and the page at `/ui/`.
`scripts/tests/e2e/run.sh` does exactly that over a scratch library and asserts each step, about 45 seconds once the dependencies are compiled.

On an iPhone: `scripts/new-app.sh stats --lib mini-stats --lib-path ../mini-stats --mobile` also writes `crates/stats-mobile`, the same app as one iOS binary with the host in-process and the `koine-web` page in a WebView (`dx build --platform ios`, a Mac with Xcode). Anything on the phone, or a Mac on the same network, drives it through the same `/api/*` and `/mcp` with a bearer. `crates/koine-demo-mobile` is the template and `scripts/tests/ios/run.sh` the proof that an agent drives it on a simulator; the guide is [docs/mobile.md](docs/mobile.md) and the design [docs/plan-koine-mobile.md](docs/plan-koine-mobile.md).

To give a Rust library verbs, generated surfaces and a page, follow [docs/capability.md](docs/capability.md); `scripts/new-app.sh <name> --lib <crate>` writes the crate it describes.

`schema-refs.json` is the snapshot this workspace builds against. An impress-apps build sets `IMPRESS_SCHEMA_REFS` to the live manifest, so suite schema names are generated from the suite, not from this snapshot.
