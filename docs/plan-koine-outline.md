# Koine outline: a tree that can be a sidebar

`plan-koine-tree-stream.md` gave the vocabulary a `tree` that nests rows by
a parent field, expands and selects. impress's sidebars (imbib's
collections, imprint's outline) do more: the keys move inside the outline,
a node is renamed, deleted, dragged onto another, shows a count, has a
menu, and the selection drives the panes beside it. The chassis capability
matrix in impress-apps (`docs/chassis-capability-matrix.md`) lists exactly
those cells per node kind: context menu, rename, delete, drag, drop,
counts, select-to-detail. This plan gives the koine tree every cell, as a
general vocabulary any app can use, so a sidebar can be a surface.

| Cell | Decision |
|---|---|
| keys move inside the outline | O-D1 a row cursor |
| what is open stays open | O-D2 the full expanded list |
| rename, delete, open | O-D3 row events with handlers |
| drag and drop | O-D4 `move` |
| context menu | O-D5 `actions` |
| counts | O-D6 `count_field` |
| select-to-detail | already `publish` |
| the sidebar itself | O-D7 a worked example |

## Decisions

### O-D1. A row cursor: `j`/`k`/`h`/`l` inside a tree, a table, a list

`koine::cursor::Cursor` gains an active row per widget: for the focused
`tree`, `table` or `list`, `j` and `k` move the active row and stop at
the ends (they do not leave the widget; `Tab` and the next/previous keys
still do); `Enter` sends `select` with the active row's id, as a click
does; `Space` on a tree toggles the active branch (a `change` with the
new expanded list). `h` on a tree collapses the active branch, and on a
collapsed branch or a leaf moves to the parent; `l` expands, and on an
expanded branch moves to the first child; at a root with nothing to
collapse `h` falls through to whatever the page does with it (the layout
verb, when the surface fills a pane), which is the rule `keys.rs` already
applies. The active row is kept by id across re-renders, so a row
inserted above it does not move it. Renderers draw it: `data-koine-active`
on the row, and the TUI's row mark. `Cursor::active_row(widget) ->
Option<&Value>` and `set_active_row` are the API; `event_for` answers the
events above.

### O-D2. The full expanded list

`RenderKind::Tree` carries `expanded: Vec<Value>`, the whole list from
the bind path, not only the visible branches; a disclosure toggle sends
the whole list with one id added or removed, so collapsing a branch keeps
its descendants' state for when it opens again.

### O-D3. Rename, delete, open

Three more handlers on `tree`: `on_rename` (a `rename` event, value
`{"id": …, "label": "<the new text>"}`), `on_delete` (a `delete` event,
value `[id]`), `on_activate` (an `activate` event, value `[id]`: a
double-click, or `o` on the active row). `EventKind` gains `rename`,
`delete`, `activate`, `move`, `menu`. `r` on the active row starts an
inline rename on the web (an input in place of the label; Enter commits,
Escape cancels) and a prompt line in the TUI; `d` sends `delete`. A tree
without a handler for an event ignores it, so a read-only tree needs no
declaration. The reducer runs each list with `{{event.value}}`, as it
does for `select`.

### O-D4. `move`: drag and drop

`on_move` runs on a `move` event, value `{"ids": [...], "to": <parent id
or null>, "index": <position among the new siblings>}`. The web uses HTML
drag and drop on the rows (`draggable`, a drop target per row and a drop
zone between rows for ordering; dropping on a row makes it the parent,
between rows makes them siblings), only for a tree that declares
`on_move`. The TUI cuts with `x` and pastes under the active row with
`p`, or beside it with `P`. The handler decides what a move means (a
`SetParent` through a verb, an order field through another); the tree
only reports it.

### O-D5. `actions`: a context menu

`tree{actions: [{label, on_click}]}` is a menu per row: the web opens it
on right-click and on `m`, the TUI on `m` as a numbered prompt; choosing
one sends `menu` with value `{"id": <row id>, "action": <label>}` and the
reducer runs that entry's `on_click` with `{{event.value}}`. The same
field on `table` and `list`, since a row of any kind may have a menu.

### O-D6. `count_field`

`tree{count_field}` (and `table`/`list`) names a row field drawn after the
label, right-aligned on the web and in parentheses in the TUI; absent or
null draws nothing. The row's other fields stay available to a renderer
through `row`.

### O-D7. A worked sidebar

`koine::surface::example_collection_sidebar(kind, verbs)` is a spec
builder: a `tree` over a `query` source for `kind` (the parent field is
the record's `parent`), `publish` on select, `on_rename` and `on_move`
and `on_delete` wired to the verb names the caller passes, a count from
`count_field`, and a menu with those three. It validates and resolves in
koine's tests over the memory store with a `collection`-shaped kind; in
impress-apps it is what the imbib sidebar leaf becomes, with imbib's own
verbs named, once the Swift renderer draws a tree (that renderer is
impress-apps' work and follows this plan).

The guide is [outline.md](outline.md).

## Work packages

| # | Package | Where |
|---|---|---|
| Q1a | the spec, the events, the reducer, the full expanded list, `count_field`, `actions` (O-D2–O-D6 in `impress-surface`) | impress-surface |
| Q1b | the row cursor (O-D1) | koine (`cursor`) |
| Q2 | the three renderers: active row, inline rename, drag and drop, the menu, counts; the web keys; goldens | koine-dioxus, koine-web, koine-tui, koine goldens; after Q1a and Q1b |
| Q3 | the worked sidebar, docs, the matrix mapping | koine (`surface`), docs; after Q1a |
| Q4 | gate, rev bump, the Swift renderer brief | orchestrator |

## Status

Filled in as the packages land.
