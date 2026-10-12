# Koine on the web: a Dioxus performance of the same document

Koine already has two performances of one `RenderTree`: SwiftUI in
`ImpressSurface` and plain text in `koine-tui`. This plan adds the third,
built on [Dioxus](https://crates.io/crates/dioxus) 0.7, and it is the first
frontend that runs where no impress app is installed: a browser, a reMarkable's
web view, a colleague's laptop pointed at a port, an agent's headless check.

The rules of the first two frontends hold unchanged. The mapping holds no
logic. Anything that computes is a verb. The sheet names the slots and the
frontend chooses the value kind. A gesture is data and the grammar decides
which layout verb it already is. The conformance suite is what proves the
third frontend is a koine frontend and not a private paint table beside it.

## Why Dioxus, in one paragraph

Dioxus is a React-shaped Rust UI library with one component model and
several renderers: a browser (wasm, `dioxus/web`), a server string
(`dioxus-ssr`), a native window around a system web view (`dioxus/desktop`,
wry), a server-driven view over a socket (`liveview`), a combined
server + client build (`fullstack`), and an in-progress HTML renderer with
no web view at all (Blitz, `dioxus/native`). All of them take the same
`rsx!` tree, so one mapping from `RenderTree` to `Element` is written once
and performed anywhere. Its CLI, `dx`, serves a build with hot-reload of
`rsx!` markup and, with `--hotpatch`, of Rust function bodies, which is the
iteration loop a design wants: edit the mapping, see the surface change,
no restart of the app that owns the store. The alternatives considered were
Leptos (fine-grained signals, strong SSR, but no desktop or native renderer
and no hot-patch), Yew (web only, slower cadence), egui (immediate mode;
not HTML, so no document semantics, no accessibility tree, no CSS sheet),
and Slint (its own markup language, which would be a second grammar).

## What the suite gains

- **A frontend with no install.** Every surface an agent creates is
  reachable at `http://localhost:<port>/ui/surface/<id>` from any browser on
  the machine (and, with the network bearer, from another). The verb
  surfaces, the settings panes, the paper-triage board, the style editor:
  all of them, with nothing new written per surface.
- **One mapping, several hosts.** The same `koine-dioxus` crate renders to a
  string on the server (conformance tests, first paint), to the DOM in a
  browser, and to a native window on the Mac for a developer who wants a
  window without Xcode. Mobile and Blitz come later from the same crate.
- **Hot reload of the design.** `dx serve --hotpatch` against a running app
  (the automation port proxied) redraws a surface as the mapping or the
  stylesheet is edited. A style edit is a CSS variable, so it never
  rebuilds anything.
- **The third conformance point.** With three frontends the suite can say
  what "the same" means: the same focus order, the same event for Enter and
  for a click, the same placeholder for an unknown kind, every required
  slot applied, and golden renders for every `RenderKind`. Two points were a
  line; three are a plane.
- **Nothing moves out of Rust.** No TypeScript, no JSON-to-HTML templates,
  no hand-written client. The browser runs the same `impress-surface` and
  `impress-layout` types the app does, compiled to wasm (verified: both
  crates check on `wasm32-unknown-unknown`, with `uuid`'s `js` feature).

## Decisions

### D1. One mapping crate, pure: `koine-dioxus`

`koine-dioxus` is a library with one job: a `RenderTree`, a `StyleSheet`
tagged `web`, and the focused widget id become a Dioxus `Element`; every
widget interaction becomes an `impress_surface::Event` handed to a callback.
It fetches nothing, stores nothing and decides nothing.

```rust
#[component]
pub fn Surface(
    tree: ReadOnlySignal<RenderTree>,
    sheet: ReadOnlySignal<StyleSheet>,     // tagged "web"; another tag is refused
    focused: ReadOnlySignal<Option<String>>,
    on_event: EventHandler<Event>,
) -> Element
```

It compiles for the browser and for the server: its Dioxus dependency is
`default-features = false, features = ["macro", "html", "signals", "hooks"]`
(verified), and an `ssr` feature adds `dioxus-ssr` and
`pub fn render_html(tree, sheet, focused) -> String`. It depends on
`impress-surface`, `impress-layout` and `koine` with `default-features =
false` (D5), never on a store, a transport or `impress-core`. It is
kit-grade and `scripts/check-kit-packages.sh`'s Rust counterpart in this
repository (`koine-dioxus` may depend only on kit crates and on Dioxus)
keeps it so.

### D2. Elements are chosen by kind, ids ride on the DOM

The HTML element for each `RenderKind` is fixed by this plan and pinned by
the SSR goldens:

| RenderKind | Element | Notes |
|---|---|---|
| `column`, `row` | `div.koine-column` / `div.koine-row` | flex column / row; gap from the `column` / `row` slot |
| `grid{columns}` | `div.koine-grid` with `grid-template-columns: repeat(N, …)` | |
| `section{title,collapsed,body}` | `section` + `h2` + `details`-like body | collapsed is `hidden` on the body, not a DOM removal, so ids stay addressable |
| `tabs{tabs}` | `div[role=tablist]` + `button[role=tab]` + `div[role=tabpanel]` | the active tab is the page's memory of a gesture (the grammar's `tabs` has no handler); a tab click still sends `select` with the tab index |
| `text{text}` | `p` | font class from the `text` slot |
| `table{rows,columns}` | `table` with `thead`/`tbody` | one `tr` per row; a row click is `select` with `[row.id]` (the suite's convention: a selection is an array of ids, `[index]` when a row has none). `rows` may resolve to a `{rows, next}` envelope: the resolver reads its rows, and its `columns` / `id_field` when the node declares none (plan-koine-live L-D5); the mapping sees the array |
| `list{rows}` | `ul` + `li` | same `select` rule; the same envelope |
| `field{field,bind,value}` | `input`/`select`/`textarea`/`input[type=date]`/`input[type=range]`/`input[type=checkbox]` by `FieldKind` | `change` carries the value in the field's JSON type |
| `field{file}` | `input[type=file][accept]`, `data-koine-file` the name last sent | no fetch in the mapping: the chosen file's bytes go to the page through `Surface`'s `on_file` (`FileChosen{widget,name,bytes}`); the page `PUT`s `/api/blob/<surface id>/<name>` and sends `change` with `{"blob": {"item": "<surface id>", "name": "<name>"}}`. With no `on_file` (SSR, tests) the input is drawn and nothing is wired (L-D6) |
| `button{label}` | `button` | `click` |
| `status{level,message}` | `div[role=status]`, `role=alert` for `error` | color from the `status.<level>` slot |
| `log{lines}` | `pre.koine-log` | |
| `kv{pairs}` | `dl` | |
| `plot{spec}` | `div.koine-plot` holding the SVG `koine_plot::render_svg_json` draws, the spec on `data-koine-spec`; `div.koine-plot-error` naming the error when the spec does not parse | the drawing is `koine-plot`'s; the mapping only places it |
| `plot{rows,x,y}` | the same `div.koine-plot` | the bound form (L-D8): the resolver builds the spec (one `Line` series per `y`, labelled by the field, the x axis labelled by `x`; a row whose `x` or `y` is not a JSON number is skipped), so the mapping sees `RenderKind::Plot{spec}` as before |
| `code{language,text}` | `pre.koine-code[data-language]` holding `code`; for `diff`, one `span.koine-diff-add` / `-del` / `-hunk` / `-ctx` per line (`+`, `-`, `@@`, else) with its newline | colours from the status palette (`--koine-color-success`, else info, for an added line); no syntax highlighter (L-D7). The tui prints `code <language>` and, with `with_blocks`, the source indented by two |
| `image{blob,url}` | `img` | `blob` is served by the host as `/api/blob/<id>` (not in this plan; placeholder until then) |
| `divider`, `spacer` | `hr`, `div.koine-spacer` | |
| `placeholder{unknown_kind,reason}` | `div.koine-placeholder` naming both | the same words the tui prints |

Every element carries `data-koine-id` = the node id, and every focusable one
is in `focus_order` order with a real `tabindex`, so the DOM's tab order *is*
the tree's focus order. The focused id gets `data-koine-focused` and the
`focus` slot colour as its outline. Labels become `<label for>` or
`aria-label`; `help` becomes `title`. An assistive technology reads the
same tree the tui prints.

### D3. The web is a style frontend: `FRONTENDS = ["swift", "tui", "web"]`

The twelve required slots, with the web's value kinds: `column`, `row`,
`list`, `table` are spacing (a CSS length in px); `text`, `section`, `field`
are font (a token mapped to a class `koine-font-body|title|mono`, and
`automatic` applies no class); `button`, `status.error`, `status.warning`,
`status.info`, `focus` are color (a token mapped to a CSS variable
`--koine-color-accent|primary|…`, and `automatic` leaves the browser's
own). The mapping writes the sheet as custom properties on the root
element, and one stylesheet, `koine.css`, shipped inside `koine-dioxus` as
`include_str!`, reads them. Changing a slot changes one attribute; nothing
re-renders but the browser's style pass. The editor surface
(`koine::style::editor_spec("web")`) is a surface like any other and the
web frontend renders its own editor with itself.

### D4. The focus cursor and the key vocabulary move up into `koine`

`koine-tui` owns today the cursor into `focus_order` and the rule that `j`,
`k`, Enter and the menu key produce a surface `Event` or a `Gesture`. The
web needs exactly that rule, so it moves to `koine::cursor` (`Key`,
`Cursor`, `event_for`, `gesture_for`), and `koine-tui`'s `TextFrontend`
becomes a user of it. The browser maps `keydown` to the same `Key` (and
only outside an editable element, the rule `ImpressKeyboard` applies on the
Mac), so a conformance test can type `j j Enter` into both frontends and
compare the two event lists.

### D5. `koine` compiles on wasm: the verb re-exports become a feature

The `koine` umbrella re-exports `impress-service-core` (tokio `full`,
`inventory`), which the browser can neither build nor needs. A `verbs`
feature, on by default, gates `koine::verbs` and `koine::verb_surface`;
`koine::style`, `koine::gesture`, `koine::surface`, `koine::layout` and
`koine::pane_query` stay unconditional. The web crates use `koine` with
`default-features = false`. No existing caller changes.

### D6. Hosts: browser, server string, desktop

- **`koine-web`** (binary, `dioxus/web` + `router`): the page. Routes:
  `/surface/:id` renders one surface live; `/layout` renders the app's
  window tree (D7); `/style` is the editor. It talks to the app on the
  same origin: `GET /api/surface/<id>/render` for the tree, `POST
  …/dispatch` with the event, `GET …/wait?after_seq=&timeout_ms=` in a loop
  so another process's change (an agent's `surface_state_set`, a Swift
  pane's click) redraws the page, and `GET /api/layout/tree`. The client is
  `reqwest` on wasm (the browser's `fetch` under the same crate name the
  native transport uses). A failed source or a refused dispatch is drawn as
  the status node the tree already carries; the page invents no error UI.
- **`koine-dioxus` with `ssr`**: `render_html` for the conformance suite and
  for an app that wants the first paint served as HTML before the wasm
  loads. No separate crate.
- **`koine-web --features desktop`** (Mac): the same binary under
  `dioxus/desktop`, a native window with the same page, for `dx serve
  --platform desktop`. It cannot build on Linux without webkit2gtk and is
  not in CI; it is the developer's window, not a shipped app.
- **Not now**: `fullstack` and `liveview` (the app already is the server;
  a second server beside it is the TypeScript-MCP mistake in a new coat),
  mobile (the Apple apps are native), Blitz (experimental in 0.7). Each is
  a feature flag on the same crate when wanted, not a redesign.

### D7. The layout page performs the window tree, with pane frames

`/layout` fetches `Layout` and renders every window's tile tree:
`Linear` is a flex container with `shares` as `flex-grow`, `Tabs` is a
tab strip, `Grid` is a CSS grid. A pane whose view kind is `surface`
renders that surface live through D1; any other kind is a placeholder
naming the query kind, role and tile, exactly as `kit-demo` draws it,
because imbib's list and detail are Swift views and the web shows what
koine can show (ADR-0032: another tool's domain is that tool's surface).
Every pane has the frame `koine-tui` draws as cells: a grip on the top
edge, tab labels on that edge, dividers between siblings. Pointer down,
move and up on those elements become `koine::gesture::Pointer` and `Edge`
values, `gesture::layout_verb` names the verb, and the page posts it to
`/api/layout/verb`. The page decides nothing about what a drag means: the
decisions are `koine::frame`, shared with `koine-tui`.

### D8. Who may call: the loopback token

A `GET` needs no token; a dispatch and a layout verb need the launch's
loopback bearer, which a page cannot read from the file. The page takes it
one of two ways and never guesses: the app's static route (D9) injects it
as `<meta name="koine-token">` into the `index.html` it serves on loopback;
or the person pastes it into the page's token field (kept in
`sessionStorage`, never in the URL after the first load). Without one the
page is read-only and says so in its status line, which is the honest
state, not a bug.

### D9. Where the bundle lives, and the development loop

`dx build --platform web --release` produces `index.html`, the wasm and
`koine.css`. In impress-apps, `scripts/build-koine-web.sh` runs it and
copies the output into each app's `Resources/koine-web/`, and
`ImpressAutomation` gains one route group, `GET /ui/*`, that serves those
files with the right content types (the first static route the server has;
today it serves only JSON). That is Mac work, briefed in impress-apps, and
it is also the only line of Swift this plan adds. Until it lands, and for
every day of design iteration after, the loop is:

```bash
cd crates/koine-web && dx serve --hotpatch     # Dioxus.toml proxies /api to localhost:23125
```

Edit `koine-dioxus/src/map.rs`; the open surface redraws. Edit
`koine.css`; it redraws without a rebuild.

### D10. Conformance is a suite, not a review

`crates/koine/tests/frontends.rs` runs over a fixed set of trees: the
examples `impress-surface` ships, the style editor spec, and one synthetic
tree holding every `RenderKind`. For each tree the suite renders with
`koine-tui` and with `koine-dioxus`'s `render_html` and asserts:

1. every node id appears exactly once in both performances;
2. the focusable elements in DOM order equal `focus_order`;
3. `j j Enter` through `koine::cursor` gives the same `Event` on both;
4. every required slot changes the output when its value changes (a slot
   nobody applies is not a slot);
5. a tree with an unknown kind draws a placeholder naming it, in both;
6. the HTML goldens under `tests/golden/web/*.html` are byte-stable (a
   change to the mapping is a reviewed golden change).

### D11. CI and the toolchain

`rust-toolchain.toml` adds `wasm32-unknown-unknown` to its targets. `ci.yml`
keeps the one job and adds `cargo check -p koine-web --target
wasm32-unknown-unknown`; the SSR tests run in the ordinary `cargo test`.
`dx` is not a CI dependency: the bundle is built where it is shipped (the
Mac) and the checks that matter (it compiles for wasm, the HTML is right)
need no browser. A later job may `cargo binstall dioxus-cli` and `dx build`
to publish the bundle as an artifact.

### D12. What this plan does not decide

- A plot renderer in the browser (`plot{spec}` is placed, not drawn).
- Serving blobs (`image{blob}`).
- Persisting a style edit from the browser (the style file is written by
  `koine-style-file` natively; a `PUT /api/style` route is a later verb).
- Any non-koine view kind on the web. Those are the apps' own surfaces.

## Work packages

| # | Package | Where | Depends on |
|---|---|---|---|
| W0 | This document; `koine` feature `verbs`; `web` in `FRONTENDS` with its slot table and tests; wasm target in the toolchain | `crates/koine`, `rust-toolchain.toml` | — |
| W1 | `koine-dioxus`: the mapping (D2), the sheet as CSS variables and `koine.css` (D3), the `ssr` feature with `render_html`, unit tests over every kind | new `crates/koine-dioxus` | W0 |
| W2 | `koine::cursor`: lift `Key`, the focus cursor, `event_for` and `gesture_for` out of `koine-tui`; `TextFrontend` uses it; tests move with it | `crates/koine/src/cursor.rs`, `crates/koine-tui` | W0 |
| W3 | `koine-web`: router, transport, the surface page with the wait loop, keys through `koine::cursor`, the token (D8), `Dioxus.toml` with the proxy, README | new `crates/koine-web` | W1, W2 |
| W4 | The layout page and pane frames (D7) | `crates/koine-web` | W3 |
| W5 | The conformance suite and goldens (D10); CI (D11) | `crates/koine/tests`, `.github/workflows/ci.yml` | W1, W2 |
| W6 | impress-apps: `scripts/build-koine-web.sh`, the `GET /ui/*` route in `ImpressAutomation`, the token meta (D8, D9); a Mac brief | impress-apps | W3 |

## Status (2026-10-11)

W0–W6 are built. What landed differs from the text above in these ways,
each on purpose:

- **A plot is drawn.** `plot{spec}` is drawn by `koine-plot` (plan-koine-followups F-D3, F3), not just placed; the mapping table above is current, and the D12 line "a plot renderer in the browser" is done.
- **A selection is an array of ids.** A table or list row click sends
  `select` with `[row.id]` (`[index]` when the row has none), the suite's
  convention, not the row index D2 first said.
- **Tab is the browser's.** The DOM tab order equals `focus_order`, so Tab
  and Shift+Tab move focus natively and the page reads the focused id back
  into the cursor; `j`/`k`/Enter/`h`/`l` go through `koine::cursor`.
- **The frame decisions are `koine::frame`.** `Hit`, `pointer_gesture`,
  `drag_gesture`, `verb_for_pointer` and `verb_for_drag` moved up from
  `koine-tui`; the terminal keeps only the cells. A first draft of the web
  restated them, and the restatement is gone.
- **The layout page polls** every five seconds while the tab is visible,
  since the layout has no wait route; a drag posts on release with no live
  preview; drag-to-move is mouse or pen (touch keeps the pointer on the
  element it pressed); no `expected_revision` is sent yet.
- **Release bundle**: `index.html` plus `assets/koine-web-<hash>.js` and
  `assets/koine-web_bg-<hash>.wasm`, 2.9 MB; a debug build writes `wasm/`.
  `dx` fetches esbuild, wasm-bindgen and binaryen itself on first use; a
  machine whose TLS it does not trust seeds `$DX_HOME/tools/` by hand.
- **Not run in a browser yet.** Every handler is compiled, SSR-rendered and
  unit-tested; the first live run is the Mac's, against a running app
  through `dx serve --hotpatch` (the token pasted) and then through
  `GET /ui/` once `scripts/build-koine-web.sh` has put the bundle in
  `ImpressAutomation`.

## Known limits at the end of this plan

- Field kinds the browser has no native control for render as text inputs
  with the kind named in `data-koine-field-kind`.
- The wait loop is long-polling, as the Swift host does; no WebSocket.
- `desktop` is verified on the Mac only.
