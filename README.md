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

An app from a library: `scripts/new-app.sh stats --lib mini-stats --lib-path ../mini-stats` writes a cargo workspace with a service crate and a host crate over it. An agent turns the library's public functions into `#[impress_method]` verbs (the guide below, then `scripts/check-verb-coverage.sh` for what it missed), and `cargo run -p stats-host` serves them over HTTP and MCP, with a generated form per verb (`GET /api/verb/<name>/surface`) and the page at `/ui/`.
`scripts/tests/e2e/run.sh` does exactly that over a scratch library and asserts each step, about 45 seconds once the dependencies are compiled.

To give a Rust library verbs, generated surfaces and a page, follow [docs/capability.md](docs/capability.md); `scripts/new-app.sh <name> --lib <crate>` writes the crate it describes.

`schema-refs.json` is the snapshot this workspace builds against. An impress-apps build sets `IMPRESS_SCHEMA_REFS` to the live manifest, so suite schema names are generated from the suite, not from this snapshot.
