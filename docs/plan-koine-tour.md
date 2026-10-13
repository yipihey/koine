# The tour: a substantial demo on a phone

Planned 2026-10-13, after M-D8 put the demo on an iPhone through
TestFlight. The demo app then opened on "the app has no surfaces yet":
every piece the grammar has (tree, table, graph, plot, log, fields,
background jobs that wake surfaces, the five-verb loop) was linked and
none was on screen. The tour is the demo showing all of it, offline, with
no agent off the phone: the pieces are seeded at launch, and one small
agent runs in-process and speaks the same verbs an agent off the phone
would.

## Decisions

### T-D1. Seeded at launch, idempotently, from spec files in the crate

`surface_demo_service::tour::seed()` creates the tour's surfaces through
the in-process verb pipeline (`surface_validate`, then `surface_create`)
when the store has none with the tour's tag, and seeds the note records
the Notes surface shows. Each spec is a JSON file under
`crates/surface-demo-service/surfaces/`, included at compile time, so the
same spec is what Tier A validates, what the simulator lane sees and what
the phone draws. Seeding is a function the demo binary and
`koine-host --tour` call after the host is bound; nothing in
`koine-host` itself knows the tour.

### T-D2. The five surfaces

| Surface | Shows | Over |
|---|---|---|
| Signal explorer | sliders, a verb-driven plot, a table | `series`, `histogram` (plot payload in koine-plot's shape) |
| Live job | a background job's emitted lines arriving while you watch | `tick` with `in_background`, a `query` over `task`, `log` |
| Notes | an outline you rename, move, delete and add to, by touch | `note` records; `note-add/rename/move/delete` write them |
| Map of this app | a graph of the linked verbs, the kinds they read and write, and the surfaces | `inventory` (nodes and edges from the descriptors) |
| Concierge | an agent in the loop: ask in words, it answers by making a surface | `surface_wait` → react → `surface_create` → `surface_show` |

### T-D3. The concierge is an in-process agent speaking the public verbs

`surface_demo_service::concierge::spawn()` runs on the host's runtime: it
waits on its surface (`surface_wait`), reads the question from state,
answers with a few rules (a plot, the notes, the map, a job, help), appends
the exchange to a transcript in state, and creates and shows the surface
its answer names. It never reaches into the runtime: everything it does is
a verb call an agent on a Mac could make over `/api/verb/*`. Rules, not a
model: the point is the loop, and the phone has no model client (M-D7).

### T-D4. The page fits a phone

`koine-web` sets the viewport for the native build, inputs and buttons are
touch-sized, the layout page stacks panes on a narrow screen, the home
list refreshes itself, and a state write by an agent wakes a waiting page
(`surface_state_set` bumps the surface's wake counter; before this, the
page noticed at its next 25 s timeout).

### T-D5. Proven without the phone

Tier A: every tour spec validates against the schema; seeding into a
memory host makes exactly the tour's surfaces and renders each with no
error node; the concierge, given a dispatched question, creates a surface
and writes the transcript. The simulator lane asserts the seeded list and
one concierge round over HTTP. Then a release (`demo-v0.2.0`) and the
phone.

## Status

Planned. Units: T1 page fit (koine-web, koine-dioxus), T2 tour content
(surface-demo-service: plot fix, notes, inventory, specs, seed), T3 the
concierge and the wake fix (surface-demo-service, impress-surface-service),
T4 wiring, lane, docs, release.
