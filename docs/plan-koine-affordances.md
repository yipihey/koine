# Plan: no dead affordances

The 0.3.0 phone build drew five things a finger could not do: a layout
whose panes could neither move nor resize and whose three tiles were "View
Kind Unavailable"; a notes outline whose rows could not be renamed or
dragged, and whose "Add child" made a row that could not be renamed either;
a style editor that took a typed word and answered
`invalid-value: slot 'button' is color, and '"red"' is not a color token`;
a map whose labels overlapped into a smear; and a text that began with a
literal `# Map of this app`. Each has a one-line fix. This plan is about
the shape that produced them, so the next frontend, widget and tour do not
produce them again.

## Why they happened

Every one is the same failure in a different coat: **an affordance was
drawn without a path that works where it was drawn.**

| What was drawn | The path that existed | Where it failed |
|---|---|---|
| a draggable row | HTML5 drag and drop (`draggable`, `ondragstart`) | a touch screen in a web view: no drag ever starts |
| a renamable row | the `r` key (keys.rs) and the TUI's prompt | a phone has no key; the row menu did not list it |
| a pane handle | `onpointerup` on the pane it lands on | a touch pointer's `pointerup` fires on the element it started on, never the one under it |
| a layout tab | impress's navigator/list/detail preset | a host that registers no view but `surface` |
| a colour slot | a free text field checked after typing | a person who does not know the token list |
| a text node | markdown written for a reader | a renderer that prints the markdown |
| a seeded spec | create when the name is new | a store that already had the previous release's spec |

The common cause: the gesture was implemented in the place it was first
needed, for the pointer that was at hand, and proven by looking at a
picture. Nothing checked that the gesture *fires* on each pointer, and
nothing checked that what the page shows is what the host can do.

## Decisions

**A-D1 One pointer-agnostic implementation per gesture, in the widget.**
The web tree moves a row with Pointer Events from a grip (`≡`,
`touch-action: none`), never HTML5 drag and drop: `pointerdown` on the grip
arms the move, `setPointerCapture` keeps the stream, `pointermove` paints
the drop target found by `elementFromPoint` (a row, or a gap between rows),
`pointerup` sends `move` with the value `outline::drop_on`/`drop_gap`
already compute. The same handlers serve a mouse and a finger; no
`(pointer: coarse)` branch chooses an implementation, only sizes. The
layout page's own handle-drag code is deleted (A-D3). No widget may add a
second way to move a row.

**A-D2 Every row verb has a visible path on every pointer.** A tree whose
spec handles `on_rename` lists "Rename" first in its row menu; one that
handles `on_delete` lists "Delete" last unless the spec's `actions` already
has that label. The rule is one function in koine (`koine::outline::
menu_entries`), used by the web menu and the TUI's, so the entries are the
same list the keyboard grammar names. The frontend handles "Rename" itself
(it opens the inline rename, as the `r` key does); the rest go to the
surface as `menu` events. The `…` button is the finger's path, the
secondary click the mouse's, the key the keyboard's: three paths, one
list.

**A-D3 The layout page's tree is the tree widget.** The page draws each
window's tiles as a `movable` tree (its rows are the tiles, in container
order, with the view kind and role as the label), and a drop sends the
layout verb `koine::gesture::tree_move_verb` derives from `(ids, to,
index)` and the tree: `MoveTile` next to the tile now at that index, with
the placement the container's axis gives. The pane handles and the
edge-drop targets are gone, with their `onpointerup` on the target. The
dividers stay for a fine pointer only (`@media (pointer: fine)`): a phone
stacks panes and has nothing to resize; the keyboard's resize verbs serve
the rest. Below the tree, the pane bodies draw as they do today.

**A-D4 A tab is shown only when the host can serve it.** The nav derives
its tabs from what the host answers: "layout" when the inventory links the
layout verbs *and* the current layout has at least one tile the page can
draw (a `surface` tile; `pane_body` says); "style" when the style verbs
are linked. A page that would show only placeholders is not a page. The
demo host seeds a layout of three `surface` tiles over tour surfaces, so
its layout tab shows three surfaces side by side (stacked on a phone) that
can be reordered by the tree.

**A-D5 A gesture is proven by firing it, on each pointer.** `scripts/tests/
e2e` gains a gesture suite that runs twice, under a mouse context and a
touch context (`hasTouch`, `isMobile`), against the demo host: move a
note by its grip, rename one by the menu, delete one, add a collection and
a note, write a note's body, pick a colour token, reorder a layout tile.
Each case asserts the verb's effect on the host (a `GET` after), not a
DOM state. The screenshot script keeps proving looks; this suite proves
function, and CI runs it on every change under `crates/koine-dioxus`,
`crates/koine-web`, `crates/surface-demo-service` and `crates/koine`.

**A-D6 A field's kind follows the value's kind.** The style editor
declares a colour slot as a `select` over `COLOR_TOKENS`, a font slot as
a `select` over `FONT_TOKENS`, a spacing slot as a `number` with the
registry's range, and a line slot as text. A refusal, which only an agent
or the CLI can now provoke, reads as a sentence: `button takes a colour
token: accent, label, secondary, …`. The quoted-quote form (`'"red"'`)
came from the message quoting a value that was already quoted; the message
names the slot, the kind and the choices, never the raw value in quotes.

**A-D7 A text node renders markdown.** The web renderer passes a text
node through `pulldown-cmark` (headings, emphasis, inline code, lists,
links; raw HTML escaped, never passed through), the TUI prints the source
as it does today, since markdown reads as text. The tour's descriptions
get their backticks back. A title is the surface's name (0.3.0), never a
heading in the first text node.

**A-D8 Seeding replaces a spec that changed.** The tour tags each seeded
surface with a hash of its spec (`tour-spec:<fnv1a>`); `seed` updates a
surface whose stored hash differs from the file's, so a release's spec is
what the phone shows, and a person's own surfaces (no tag) are never
touched. Notes keep their records.

**A-D9 The notes tour is collections of notes.** A root is a collection; a
child is a note with a body. The tree sorts notes into collections by
drag; the menu renames and deletes; "Add collection" adds a root, "Add
note" adds into the selected collection (or the root when none is
selected); selecting a note shows its body in a text field, and "Save"
writes it through `note-write {id, body}`. Nothing else is new: the
surface still only calls verbs.

**A-D10 The graph draws what fits.** `koine-plot::graph` lays out with
label-aware repulsion (a node's radius includes its label's width at the
font size) and draws a label only where it does not overlap one already
drawn, highest degree first; a node whose label was skipped shows it on
select, as the highlight does today. The `<svg>` carries a `viewBox` and
its card allows pinch-zoom (`touch-action: pinch-zoom`) so a phone can
read a dense map.

## Units

- **G1** (koine, koine-dioxus): A-D1, A-D2. Grip and Pointer Events in
  `map.rs`; `menu_entries` in koine; the TUI's menu through it; goldens.
- **G2** (koine, koine-web): A-D3, A-D4, and the safe-area top (the title
  under the clock: `body`'s `env(safe-area-inset-top)` is lost on the
  phone; find why and fix it where the nav is).
- **G3** (surface-demo-service, koine-host): A-D8, A-D9, and the demo's
  seeded three-surface layout for A-D4.
- **G4** (koine-dioxus, koine-plot): A-D7, A-D10.
- **G5** (koine): A-D6.
- **G6** (scripts/tests/e2e, CI): A-D5, after G1-G5 land.

## Status

Planned 2026-10-14.
