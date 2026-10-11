# koine

Koine is the layout and surface grammar for [impress](https://github.com/yipihey/impress-apps).

A window is a tree of panes. A pane shows a query. A gesture (focus, activate, move, drag, resize) is data, and the grammar decides which layout verb that gesture already is. A surface is a spec reduced to a render tree. A style sheet names shared slots; each frontend supplies its own value kind. None of this opens a database or draws pixels.

The store, the Apple frontends, and the service crates that construct `SqliteItemStore` stay in impress-apps. That repository depends on this one.

The parts of speech — nouns, pointing, verbs, adjectives, adverbs, mood and consent — and the rules each one follows are [docs/grammar.md](docs/grammar.md). An adjective is a named predicate over records that any query may use; an adverb is a modifier on one call (`tentatively`, `with_review`, `once`, `reversibly`, `within`, `in_background`) that every verb accepts through one envelope the invoker pipeline checks.

The document has three performances: SwiftUI in impress-apps' `ImpressSurface`, plain text in `koine-tui`, and the browser. The browser one is `koine-dioxus` (one Dioxus mapping from `RenderTree` to HTML, performed as a string on the server, as the DOM in a browser and as a native window) under the page `koine-web`, which any impress app serves at `/ui/`. The design and its decisions are [docs/plan-koine-web.md](docs/plan-koine-web.md); `crates/koine/tests/frontends.rs` is the conformance suite that performs each tree twice and compares.

```bash
cargo test --workspace --all-targets
cargo check -p koine-dioxus -p koine-web --target wasm32-unknown-unknown
cd crates/koine-web && dx serve --hotpatch     # the page, live against a running app
```

`schema-refs.json` is the snapshot this workspace builds against. An impress-apps build sets `IMPRESS_SCHEMA_REFS` to the live manifest, so suite schema names are generated from the suite, not from this snapshot.
