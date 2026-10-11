# koine-web

The koine web frontend as a page. It draws any surface of a running impress
app, live, in a browser, and it hosts the style editor for its own sheet. The
mapping from a `RenderTree` to elements is [`koine-dioxus`](../koine-dioxus);
this crate is the part around it: the HTTP transport, the loopback token, the
keys, the router and the pages. The plan is
[`docs/plan-koine-web.md`](../../docs/plan-koine-web.md) (D6 to D9, D12).

| Route | Page |
|---|---|
| `/` | the app's surfaces (`GET /api/surface`), as links |
| `/surface/:id` | one surface, live: render, then a long-poll on `…/wait` that re-renders on any change; events are posted to `…/dispatch` |
| `/layout` | the window tree with pane frames (below) |
| `/style` | the style editor, drawn by the web frontend and applied to the same sheet the surface page reads |

Every page has a status line: the app origin, whether a token is held
("read-only: no token" when not), and the last refusal as `code: message`.

## The layout page

`/layout` fetches `GET /api/layout/tree` and draws every window of the layout,
the current one first, each titled by its id, as its tile tree. A `Linear` is a
flex row or column whose children grow by their share, with a divider between
siblings; a `Tabs` container is a strip on the top edge of the pane it shows
(one button per child, the active one marked, only the active child drawn); a
`Grid` is a CSS grid. Every pane has the frame `koine-tui` draws as cells: a
`≡` grip and the title (`role · view kind`) on the top edge, and four edges.
The focused tile is outlined in the sheet's `focus` colour. A pane whose view
kind is `surface` draws that surface live, with its own wait loop; any other
kind is a "View Kind Unavailable" placeholder naming its query kinds, role and
tile, because imbib's list and detail are Swift views and another tool's
domain is that tool's surface (ADR-0032).

Pointer events become the shared gestures; the page decides none of them. A
click on a pane or a tab is a focus. Pressing the grip (or the title) and
releasing over another pane's edge moves it there, over its middle makes it a
tab (`koine::gesture::layout_verb` names the verb). Pressing a divider and
releasing resizes (`koine::gesture::resize_verb`, over the lengths the browser
measured; the verb is posted on release only). `h` and `l` post
`focus-direction`. A refusal goes to the status line; a success fetches the
tree again. The layout has no wait route, so the page also fetches every five
seconds while the tab is visible. Dragging a tab out of its strip, reordering
tabs, and the secondary-click menu are not drawn; touch screens keep the
pointer on the element they pressed, so drag-and-drop is for a mouse or a pen.

## The page is same-origin with the app

The app sends no CORS headers, so the page must be served from the app itself
(`/ui/`, plan D9) or by `dx serve`, which proxies `/api` to it. Requests go to
the page's own origin.

A `GET` needs no token. A dispatch and a layout verb need the launch's
loopback token (plan D8). The page takes it, in order, from
`<meta name="koine-token" content="…">` (the app injects it into the
`index.html` it serves on loopback), from `sessionStorage["koine-token"]`, or
from the field in the status line, whose value is then kept in
`sessionStorage` for this tab. For a `dx serve` session, paste it from
`~/Library/Group Containers/QG3MEYVHMS.com.impress.suite/workspace/automation/loopback-<port>.token`
(it changes at every launch). Without one the page is read-only and a
dispatch is answered `unauthorized`.

## Development loop

```bash
cd crates/koine-web
dx serve --hotpatch
```

`Dioxus.toml` serves the page under `/ui/` (`base_path = "ui"`, so open
`http://localhost:8080/ui/`) and proxies `/api` to the impress app on
`http://localhost:23125` (`[[web.proxy]] backend = "http://localhost:23125/api"`;
the backend URL must carry a path, which is the prefix forwarded). To point at
another app edit the port: imbib is 23120, and `SiblingApp.descriptors` in
impress-apps is the table.

Edit `crates/koine-dioxus/src/map.rs` and the open surface redraws; edit
`koine.css` and it redraws without a rebuild.

`koine.css` is not a bundled file. The page puts `koine_dioxus::STYLESHEET`
in a `<style>` element at its root, so `Dioxus.toml` lists no resources.

## Build

```bash
cd crates/koine-web
dx build --platform web --release
```

The bundle lands in `target/dx/koine-web/release/web/public/`:
`index.html`, `wasm/koine-web.js`, `wasm/koine-web_bg.wasm` (plus
`wasm/snippets/`), and `assets/` when there are any (there are none yet).
`dx build --platform web` without `--release` writes the same layout under
`target/dx/koine-web/debug/web/public/`. The release build downloads
`wasm-opt` on first use. Copying the bundle into each app's
`Resources/koine-web/` and serving `GET /ui/*` is impress-apps work (plan W6).

The crate also checks and lints for the browser target without `dx`:

```bash
cargo check -p koine-web --target wasm32-unknown-unknown
cargo test -p koine-web      # the pure modules, natively
```

`--features desktop` builds the same binary under `dioxus/desktop` (a wry
window). It needs webkit2gtk on Linux, is built on the Mac only, and is not
in CI.

## Keys

`onkeydown` on the page root feeds `koine::cursor`: `j`/`k` and the up and
down arrows walk the focus order, Enter clicks the focused widget, `h`/`l`
and the left and right arrows post the `focus-direction` layout verb. Tab and
Shift+Tab are the browser's (the DOM's tab order is the tree's focus order);
the page reads the focused `data-koine-id` back into the cursor. A key does
nothing while an `input`, `textarea`, `select` or content-editable element has
focus, and nothing with Control, Alt or Meta held.

## Not done

- Tab drags (reorder, split out) and the pane menu on `/layout`.
- Persisting a style edit. The sheet lives in the page and a reload returns to
  the defaults (plan D12); the style file is written natively by
  `koine-style-file`.
- Drawing `plot` nodes, and serving `image{blob}` (plan D12).
- The menu key (`m`): the web has no menu yet.
- Any non-koine view kind: those are the apps' own surfaces.
