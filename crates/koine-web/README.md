# koine-web

The koine web frontend as a page. It draws any surface of a running impress
app, live, in a browser, and it hosts the style editor for its own sheet. The
mapping from a `RenderTree` to elements is [`koine-dioxus`](../koine-dioxus);
this crate is the part around it: the HTTP transport, the loopback token, the
keys, the router and the pages. The plan is
[`docs/plan-koine-web.md`](../../docs/plan-koine-web.md) (D6 to D9, D12).

| Route | Page |
|---|---|
| `/` | the app's surfaces (`GET /api/surface`), as links, listed again every three seconds while the tab is visible |
| `/surface/:id` | one surface, live: render, then a long-poll on `…/wait` that re-renders on any change; events are posted to `…/dispatch` |
| `/layout` | the window tree with pane frames (below) |
| `/style` | the style editor, drawn by the web frontend and applied to the same sheet the surface page reads |
| `/host` | where the page's host is: with a host link, the app id, the origin and the bearer (with a Copy button); in a browser, the window's origin, where the token came from, and the paste field |

(In a browser the router's prefix is `/ui`, so `/host` is `/ui/host`.)

Every page has a status line: the app origin, whether a token is held
("read-only: no token" when not), and the last refusal as `code: message`.

## The layout page

`/layout` fetches `GET /api/layout/tree` and draws every window of the layout,
the current one first, each titled by its id and its root container's kind.
Each window is drawn twice over (docs/plan-koine-affordances.md A-D3):

- **Its outline**: the window's containers and panes as rows of the tree
  widget (`koine_dioxus::Surface`), in container order, a pane labelled
  `view kind · role`, a container `split-h`, `split-v`, `tabs` or `grid`. The
  root container's children are the roots. The focused tile is the active
  row. The tree is `movable`: dragging a row (or `x` on it, then `p`/`P` on
  another) sends the widget's `move`, and `koine::gesture::tree_move_verb`
  names the `move-tile` beside the tile at that place, by the container's
  axis, or into a tab strip; a drop on a pane's row tabs the two. A click on
  a row focuses that tile (a container's first pane); a disclosure folds a
  container in the page only. The gesture is the widget's, so whatever moves a
  row of any surface's tree on a pointer moves a tile here.
- **Its panes**: a `Linear` is a flex row or column whose children grow by
  their share; a `Tabs` container is a strip on the top edge of the pane it
  shows (one button per child, the active one marked, only the active child
  drawn); a `Grid` is a CSS grid. Each pane's top edge carries its title
  (`role · view kind`). The focused tile is outlined in the sheet's `focus`
  colour. A pane whose view kind is `surface` draws that surface live, with
  its own wait loop; any other kind is a "View Kind Unavailable" placeholder
  naming its query kinds, role and tile, because imbib's list and detail are
  Swift views and another tool's domain is that tool's surface (ADR-0032).
  When every pane is one, the page says there is nothing to lay out.

A click on a pane or a tab is a focus. With a fine pointer on a page wide
enough to lay panes side by side, a divider sits between siblings: pressing it
and releasing resizes (`koine::gesture::resize_verb`, over the lengths the
browser measured; posted on release). A phone stacks its panes and draws no
divider. `h` and `l` post `focus-direction`. A refusal goes to the status line;
a success fetches the tree again. The layout has no wait route, so the page
also fetches every five seconds while the tab is visible.

The nav shows the layout tab only when the host serves the layout verbs and
its layout has a pane the page can draw (a `surface` pane), A-D4; the route is
there either way.

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

`koine-host --window` (plan-koine-live L-D9) is still the desktop window for a
browser bundle; the `desktop` and `mobile` features below are for an app that
links its own host.

## As a library

The crate is a library with a thin browser binary (`src/main.rs` is
`koine_web::launch()`). An app that runs its host in-process
(`docs/plan-koine-mobile.md` M-D1, M-D2) hands the page its host instead of
letting it ask the window:

```rust
use koine_web::{install_link, launch, HostLink};

install_link(HostLink {
    origin: "http://127.0.0.1:49152".into(),   // where the host bound
    token: Some(bearer),                        // its loopback bearer
    app_id: "my-app".into(),
});
launch();
```

- `install_link(HostLink)` installs the link once, before `launch`; a second
  call replaces the first. `host_link()` answers the installed link.
- `launch()` runs `dioxus::launch` under the `desktop` or `mobile` feature (and
  in a browser); with none of them it prints a hint and returns.
- With a link installed, every request dials `link.origin`, and the token is
  the link's from the first paint (read-write, nothing to paste). The link's
  token lives in memory only: it is never written to `sessionStorage` or a URL.
  With no link, the window's origin and the meta tag / `sessionStorage` / field
  apply as above.

`desktop` and `mobile` forward to `dioxus/desktop` and `dioxus/mobile` (a
WKWebView on macOS and iOS) and are built with `--no-default-features`, which
`dx` does for you: `dx build --platform ios` (or `macos`) from a crate that
links a host and calls `launch`, such as `crates/koine-demo-mobile`. This crate
itself is not what `dx` builds for a phone. Those features need the platform's
WebView (Xcode on a Mac); the Linux lane keeps the browser build and the native
tests. On a native target the key shortcuts are off (the editable-element
guard cannot read the DOM synchronously, so it answers "editable"): touch and
pointer input, Tab and Enter work. The native build adds a viewport meta
(`width=device-width, initial-scale=1, viewport-fit=cover`) to the index the
WebView serves (`Config::with_custom_head`), and the page is laid out for a
phone at or below 700px wide: the nav wraps, controls are full width and
finger-sized, a row's menu has a visible "…" button, tables scroll inside
their box, and `/layout` stacks its panes (plan-koine-tour T-D4).

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
