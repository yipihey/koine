# A tree that is a sidebar

An app whose sidebar is a list of things that nest (collections, an outline
of sections, folders) can write that sidebar as a surface: one `tree` node,
a `query` source, and a verb name per thing the person may do to a row. The
tree reports what the person did as an event; your handler turns the event
into a verb call. The tree never changes your data itself.

This guide is what a `tree` can do, the keys, the exact event values, and
the worked sidebar. The decisions are in
[plan-koine-outline.md](plan-koine-outline.md).

## 1. What a tree can do

impress's chassis capability matrix
(`impress-apps/docs/chassis-capability-matrix.md`) lists, per sidebar node
kind, seven cells. Each has one field or one event on the tree:

| Cell | The tree's field or event | Key |
|---|---|---|
| select, then the panes beside it show the row | `on_select: [{"publish": {}}]` | `Enter` |
| context menu | `actions: [{label, on_click}]`, a `menu` event | `m`, or right-click |
| rename | `on_rename`, a `rename` event | `r` |
| delete | `on_delete`, a `delete` event | `d` |
| drag | `on_move`, a `move` event | `x` (cut) in the terminal |
| drop | the same `move` event | `p` under the row, `P` beside it |
| counts | `count_field`, a row field drawn after the label | |
| open (a double-click) | `on_activate`, an `activate` event | `o` |

A tree with no handler for an event ignores it, so a read-only tree declares
nothing. `actions` and `count_field` are also on `table` and `list`.

Drag and drop is only turned on for a tree that declares `on_move`. A drop
from outside the surface (a file, a paper dragged from a list in another
pane) is not a `move` of this tree's rows; it is the host's and is not
covered here.

## 2. The keys

The row cursor is kept by row id, so a row inserted above it does not move
it. `j` and `k` stop at the ends; `Tab` still leaves the tree.

| Key | Does |
|---|---|
| `j` / `k` | the active row down / up |
| `Enter` | `select` with the active row's id |
| `Space` | toggle the active branch (`change`) |
| `l` | expand the branch; on an expanded one, go to its first child |
| `h` | collapse the branch; on a collapsed one or a leaf, go to its parent; at a root, fall through to the page (the layout verb) |
| `r` | rename the active row inline: Enter commits, Escape cancels |
| `d` | `delete` the active row |
| `o` | `activate` the active row |
| `m` | the row menu |
| `x` | cut the active row for a move |
| `p` / `P` | paste the cut rows under / beside the active row (`move`) |

The web draws the same events from the pointer: a click selects, the
disclosure triangle toggles, a double-click activates, right-click opens the
menu, and dragging a row onto a row makes it the parent, between rows makes
them siblings.

## 3. The events

Each event is `{"widget": "<tree id>", "kind": ..., "value": ...}`. The
handler's actions see `value` as `{{event.value}}`. These are the values
exactly as the tests in `crates/impress-surface/tests/outline.rs` send them.

| `kind` | `value` | Handler |
|---|---|---|
| `select` | `["c2"]` | `on_select` |
| `change` | `["c1", "c2"]`, the whole expanded list | writes `bind` |
| `rename` | `{"id": "b", "label": "library"}` | `on_rename` |
| `delete` | `["c"]` | `on_delete` |
| `activate` | `["d"]` | `on_activate` |
| `move` | `{"ids": ["c", "d"], "to": "a", "index": 1}` | `on_move` |
| `move` to the root | `{"ids": ["c"], "to": null, "index": 0}` | `on_move` |
| `menu` | `{"id": "b", "action": "Delete"}` | that entry's `on_click` |

- `to` is the new parent's id, or `null` for the root. `index` is the
  position among the new siblings. A `move` whose value is not an object is
  refused `invalid-argument`.
- `menu` names an entry by label. A label the tree does not have, or none,
  is refused `invalid-argument` and the message lists the labels.
- `change` carries the whole expanded list, including branches a collapsed
  ancestor hides, so collapsing a branch keeps its children's state for when
  it opens again. The render tree hands the renderer that list as `expanded`.
- A row's `count_field` value reaches the render tree as the node's `count`.
  Zero is a count. Absent or `null` draws nothing.

## 4. The worked sidebar

`koine::surface::example_collection_sidebar(kind, &SidebarVerbs)` builds the
spec:

```rust
use koine::surface::{example_collection_sidebar, SidebarVerbs};

let spec = example_collection_sidebar("collection", &SidebarVerbs {
    rename:   Some("imbib-service_rename-collection".into()),
    delete:   Some("imbib-service_delete-collection".into()),
    reparent: Some("imbib-service_reparent-collection".into()),
    create:   Some("imbib-service_create-collection".into()),
});
```

The tree has id `outline`, rows from `{{source.collections}}` (a `query`
source for `kind`, sorted by `name`), nested by each record's envelope
`parent`, labelled by `name`, counted by `count`, open branches in
`state.open`. A select publishes. Each verb you name wires its cells:

| `SidebarVerbs` | Wires | Call |
|---|---|---|
| `rename` | `on_rename` | `{"id": "{{event.value.id}}", "name": "{{event.value.label}}"}` |
| `delete` | `on_delete`, the "Delete" menu entry | `{"ids": "{{event.value}}"}`; from the menu `{"ids": ["{{event.value.id}}"]}` |
| `reparent` | `on_move` | `{"ids": "{{event.value.ids}}", "parent": "{{event.value.to}}", "index": "{{event.value.index}}"}` |
| `create` | the "New collection" menu entry | `{"parent": "{{event.value.id}}"}` |

Every call is followed by `{"refresh": {"source": "collections"}}`, so the
tree shows what the verb did. A verb left `None` leaves its handler out.
Rename is not a menu entry: it needs the person's text, which is the
renderer's inline edit.

The verb names are names only. `validate` checks that a call is well formed,
not that the verb exists; the host's `surface_validate` checks that the
verbs are linked. The query reads rows the way the runtime hands them over:
the record's `payload` lifted to the top of the row, the envelope `parent`
beside it.

## 5. How impress uses it

The Swift renderer that draws a `tree` (rows, the active row, inline rename,
drag and drop, the menu, counts) and the imbib sidebar leaf that calls
`example_collection_sidebar` with imbib's own verbs are impress-apps' work.
The checklist for that work is the matrix: a node kind is done when each of
its cells is either wired to the field above or marked not applicable, and
the row for it in `docs/chassis-capability-matrix.md` says which.
