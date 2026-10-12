# Koine live: pages that move, rows that last, forms that take files

Five libraries were held against koine after the follow-ups wave
(`plan-koine-followups.md`): a system monitor over `sysinfo`, a repository
browser over `git2`, a database browser over `rusqlite`, a spreadsheet
reader over `calamine`, a GitHub desk over `octocrab`. Each would build; each
would stop at one of nine gaps. This plan closes them, grouped by what they
stop an agent from building.

| Gap | Decision |
|---|---|
| A verb runs in the background once; nothing repeats | L-D1 `every` |
| A store write wakes no page | L-D2 the store feed |
| A job reports no progress | L-D3 |
| A verb's rows vanish with the surface that called it | L-D4 materialised rows |
| Results come in pages; a table draws what it is handed | L-D5 the `{rows, next}` envelope |
| A query's columns are known only at run time | L-D5 (the same envelope) |
| The form cannot take a file | L-D6 the `file` field |
| A diff lands in `text` | L-D7 the `code` node |
| A plot needs a verb per plot | L-D8 the bound plot |
| "Desktop" is a browser pointed at a port | L-D9 `--window` |
| The token is not where the suite's clients look | L-D10 |

## Decisions

### L-D1. `every`: a verb that repeats

A seventh adverb, `every`, declared like `in_background`
(`supports(every)`), taking a duration (`"2s"`, `"10m"`, `"1h"`; seconds at
least 1, days at most). The pipeline checks it where it checks
`in_background` and hands the call to the background runner's `repeat`.
The host's `JobRunner` writes one `task@1.0.0` row with `kind: "routine"`,
`every_ms`, `runs`, `last_result`, `last_finished`, `state: running`, and
spawns a loop: run the verb through the pipeline (so policy and audit see
every run), record the outcome, sleep the interval, repeat; `cancel` stops
it. A routine row that says `running` when the host starts is re-armed from
the row's own verb, arguments and interval, so a restart does not forget
it; one that cannot be re-armed (the verb is no longer linked) is written
`interrupted`. `GET /api/job/<id>` shows the routine's runs; `GET /api/job`
lists what is running. The sampling case is then `sample` with
`every: "2s"` from one form button.

### L-D2. The store feed: a write wakes the surfaces that read it

The host subscribes to its store once (`ItemStore::subscribe` with an empty
query) on a thread, keeps `id → schema` for what it has seen so a `Deleted`
and an `Updated` can be named, and for each event hands the schema ref to
the surface registry (`SessionRegistry::invalidate_refs`) and, for a layout
row, to the layout service (`forget_session`). `surface_wait` then returns
early for a surface the feed marked: the registry keeps a wake counter per
surface that `invalidate_refs` advances, `surface_wait` reads it on entry
and at every poll, and answers `{events: [], next_seq: <unchanged>,
invalidated: true}` when it moved, which the page already treats as "render
again". A `dir:` store's watcher delivers another process's writes into the
same feed, so two apps on one directory redraw each other; the memory and
SQLite stores deliver this process's writes, which is what a background
job's sample needs.

### L-D3. Progress

`impress_service_core::job::progress(fraction: Option<f32>, message: &str)`
is a verb's channel: a task-local the runner sets while it runs a job, a
no-op anywhere else. The runner writes `progress` and `progress_message`
to the task row at most twice a second; `GET /api/job/<id>` shows them. A
routine's progress is its current run's.

### L-D4. Materialised rows: a verb's rows become records

After a linked verb with `returns_kind` and `rows(…)` answers `ok`, the
pipeline hands its rows to an installed materialiser
(`pipeline::materialize::install`; a process seam like the runner's). The
host's materialiser upserts each row into the store as an item of schema
`<kind>` (the manifest maps the kind to that one ref;
`koine_host::manifest::from_inventory` adds every `returns_kind` the
inventory declares), with id `uuid5(koine-rows, "<kind>:<row id>")`, the
row as payload, and the verb's name in `payload["_verb"]`. A row that
exists is updated field by field, so tags, flags and references put on it
earlier survive; a row that is gone is not deleted (a verb's page is not the
whole set). Materialisation runs after the handler and outside the effects
check, in the host only; a bare CLI or MCP process installs no materialiser
and nothing changes for it. The GitHub desk's issues are then rows a layout
pane can query, tag, star and select, and they are there offline.

### L-D5. The `{rows, next}` envelope

A result that is an object with a `rows` array is rows: a table whose
`rows` resolves to such an object reads `rows` from it, `columns` from it
when the node declares none, `id_field` likewise, and the envelope's
`next` (a cursor string, or null) is the page's. The generated form of a
verb whose arguments include `cursor` and whose result carries `next` gets
a More button: it calls the verb again with the form's arguments plus
`cursor: {{state.result.next}}` into `state.more`, then `append`s
`state.more.rows` to `state.result.rows` and sets `state.result.next` from
`state.more.next`. `append` is a new action (`{"append": {"path", "value"}}`:
the value's rows, or the value itself when it is an array, onto the array
at the path). A query result that knows its columns only at run time
answers the envelope with `columns` and `id_field` filled in; the row-schema
check still refuses a static `rows(…)` that the schema cannot honour, and
the envelope is how a verb says it at run time instead.

### L-D6. The `file` field

`field: {file: {accept: ".csv,.xlsx"}}` is a seventh field kind. Its value
is a blob reference, `{"blob": {"item": "<surface id>", "name":
"<file name>"}}`. The web page uploads the chosen file with `PUT
/api/blob/<surface id>/<name>` (the loopback token it already holds) and
then sends the `change` event with that value; the mapping stays logic-free
by handing the file to the page through one more callback beside
`on_event`. A verb takes the argument as `impress_service_core::BlobRef
{item, name}`, and `BlobRef::read()` answers the bytes through the process's
blob slot (`koine_store::process::blobs`, which the host fills from
`open_with_blobs`). The TUI shows the field as the file name; the Swift view
is a later edit (an unknown field kind draws as a labelled text input).

### L-D7. The `code` node

`code{language, text}`: `pre.koine-code[data-language]` on the web, with
each line of a `diff` prefixed `+`/`-`/`@@` given a class so a sheet can
colour it; one line per source line under a header in the TUI. No syntax
highlighter in this plan: the node names the language so a frontend may
add one.

### L-D8. The bound plot

`plot{rows, x, y}` beside `plot{spec}` (exactly one of the two): `rows` is a
template to an array of objects (or an envelope), `x` a field name, `y` a
field name or a list of them; the resolver builds the `koine-plot` spec
(one series per `y`, labelled by the field, the x axis labelled by `x`)
and the renderers see the same `RenderKind::Plot { spec }` as before. A
non-numeric value is skipped, so a table with a text column still plots.

### L-D9. One binary: `--window`

`koine-window` opens a URL in a native WebView when built with its
`webview` feature (`wry` + `tao`: macOS and Windows out of the box, Linux
with webkit2gtk) and in the system browser otherwise (`webbrowser`), so the
flag works everywhere and the native window is a build choice. The host's
`--window` serves, opens `http://127.0.0.1:<port>/ui/` (the page gets the
token: the WebView is a loopback peer), and stops when the window closes.
`koine-web`'s own `desktop` feature goes; the scaffold's README says
`cargo run -p <name>-host -- --window`. CI builds the browser fallback only.

### L-D10. The token where the clients look

`koine_host::cli` defaults the workspace to the suite's app-group container
when it exists on this machine (so the suite's Rust clients find the
token where they already look), else `$KOINE_WORKSPACE`, else `~/.koine`;
and `loopback_token::client_token` looks under `~/.koine/automation` after
the container, so a koine client finds a koine host's token either way.

## Work packages

| # | Package | Where |
|---|---|---|
| P1 | `every` (adverb, pipeline, runner), the store feed and `surface_wait`'s wake, progress, the token default, `GET /api/job` | impress-service-core (adverb, pipeline, job), koine-host (runner, routes, lib, cli), impress-surface-service (runtime, service) |
| P2 | the envelope and `append`, the `file` field, the `code` node, the bound plot; the three renderers; the generated form's More button | impress-surface, impress-verb-surface, koine-dioxus, koine-tui, koine-web |
| P3 | materialised rows: the seam, the host's materialiser, the inventory manifest, `BlobRef` and the blob slot | impress-service-core (materialize, blob_ref), koine-store (process blobs), koine-host (materialize, manifest) |
| P4 | `koine-window`, the host's `window.rs`, the scaffold line | new crate, koine-host (window.rs, Cargo.toml), scripts/new-app.sh |
| P5 | wiring (`--window`, the materialiser's install), docs, the gate, the e2e assertions, the rev bump | orchestrator |

## Status

P1–P5 are built on koine main. What landed, and where it differs from the
decisions above:

- **L-D1.** `every` is the seventh adverb; the interval is at least one
  second and at most 365 days. A routine row also keeps the call's other
  adverbs, so a re-arm keeps `within` and `reversibly`; a re-armed routine
  runs at once, then on its interval. Stopping the host leaves routine rows
  `running` so the next launch re-arms them; a plain job is written
  `cancelled`. The CLI takes `--every <duration>`.
- **L-D2.** As decided. The feed keeps its id-to-schema map for the host's
  life; a `Deleted` for an id it never saw invalidates every ref a live
  surface reads. A write that lands between a page's render and its next
  `surface_wait` is seen at the following event, which is the counter's
  design. The feed reads `SessionRegistry::shared()` and
  `DefaultLayoutService::new()`, the same objects the inventory verbs use.
- **L-D3.** As decided. A verb that does its work on a task it spawns itself
  does not carry the progress scope.
- **L-D4.** As decided. A row's id may be a string, a number or a bool; a
  row without one is skipped and counted in a warning. The uuid5 is
  hand-written (the workspace `uuid` carries no `v5` feature, and enabling
  it would move the workspace-hack).
- **L-D5.** As decided, plus a rule the first press needed: a handler's
  actions after a `call` are carried on the `Effect::Call` and run by the
  runtime after the call's `into` is written, because `reduce` is pure and
  ran the whole list before any call was made (the catalogue's own
  three-call chain had the same ordering). The More button is hidden
  when `state.result.next` is null.
- **L-D6.** As decided. The TUI shows the file's name; the Swift view is a
  later edit.
- **L-D7.** As decided. There is no success colour in the palette, so an
  added diff line uses `--koine-color-success` with info as its fallback.
  The TUI prints the block under `with_blocks(true)`; `with_plots` is a
  deprecated alias of it.
- **L-D8.** As decided, with a colour per series so several are told
  apart; the y axis is unlabelled.
- **L-D9.** As decided. `--window` is `cli::main`'s alone (the native window
  needs the main thread, outside the runtime's `block_on`); `cli::run`
  warns and serves without it. The `webview` feature cannot be built on this
  container (no webkit2gtk) and is unverified beyond reading the wry and
  tao sources; CI builds the browser fallback.
- **L-D10.** As decided; the suite's container gives `<container>/workspace`,
  where its clients look.
