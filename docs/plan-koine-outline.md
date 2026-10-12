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

Q1–Q3 are built on koine main. What landed, and where it differs from
the decisions above:

- **O-D1.** As decided, with `Cursor::reply_for` answering `Event`,
  `Moved` or `Unhandled` so a page knows whether to redraw or to hand the
  key on; `event_for` is unchanged for every caller. `j`/`k` no longer
  leave a table, list or tree: `Tab` and the next/previous keys do, and the
  frontends suite walks with those. `h` on a collapsed branch or a leaf
  moves to the nearest shallower node above. `d` deletes and `o` opens
  without any renderer UI; `r`, `m`, `x`, `p` and `P` are spelled in the
  cursor and answered by the renderers.
- **O-D2 to O-D6.** As decided. A `move` with a non-object value is refused
  only by a tree that declares `on_move`; a `menu` whose label the node
  does not have is refused naming the labels; every other unhandled event
  is ignored. `RenderKind::Tree` also says `movable` (the tree declared
  `on_move`), so a renderer offers drag and cut/paste only where they can
  land. The move arithmetic (drop on a row, drop in a gap, paste into,
  paste beside, cycle and self-drop refusal) is one set of helpers in
  `impress_surface::tree` that the web and the TUI both call, so a drop and
  a paste send the same value. `P` places the row after the active row's
  position among its siblings, counted before the move. The TUI shows an
  active row and counts in trees only (it draws no table or list rows), and
  its rename and menu are prompt lines the binary does not yet feed, so
  they are proven at the `key()` level.
- **O-D7.** As decided, plus a `refresh` of the query source after every
  verb call, so a rename or a delete shows at once; the menu's Delete passes
  `[event.value.id]` since a menu's value is `{id, action}`. The worked
  sidebar is a golden of the frontends suite in both renderers.
- **Not in koine.** The Swift renderer of a tree (the `tree` case of
  `ImpressSurface`, with the matrix's cells) and the imbib leaf that uses
  `example_collection_sidebar` with imbib's verbs are impress-apps' work;
  `docs/outline.md` is their brief. The web's drag and drop has no browser
  test: SSR structure and the pure helpers are tested, a real drag is not.

**The chassis sidebar (impress-apps L8 leaf 4b).** The imbib sidebar became
this tree. What that wave adds on koine's side, in `impress-layout-service`:
a row's id is its `OutlineNode` written as one string (`node_key` /
`parse_node_key`: `section:inbox`, `library:<uuid>`, `collection:<uuid>`, …),
which is what every tree event carries and what `state.open` holds;
`layout-service_outline-rows {app_id, bindings}` answers the rows as a flat
`{id, parent, label, count, …}` list, each count being the count of the very
query the row selects, so a badge is the list's row count by construction;
`layout-service_outline-select` parses the key and applies the outline's
verbs (`outline_target` + `outline_verbs`) as one all-or-none, one-undo step;
`outline-rename`, `outline-delete`, `outline-move` and `outline-create`
resolve the keys and forward to the collection kernel
(`collection-service_{rename,delete,reparent,create}`); and
`outline_surface_spec(app_id, bindings)` is the `SurfaceSpec` that wires all
of it to a `tree` (`rows` read at `{{source.rows.rows}}`, `on_select` at
`{{event.value.0}}`, every mutating call followed by a `refresh` of `rows`).
The `impress.outline.surface` setting (bool, device scope, default off,
section `impress.layout`, in `impress-settings`) is what turns the surface on
in the host. The Swift host (the `outline` view kind drawing the surface, the
installer and the FFI) and the Tier A proof over SQLite are impress-apps':
`docs/plan-l8-sidebar-surface.md` there.
