# Koine follow-ups: secrets, row schemas, drawn plots, a watched directory, merged conflicts

The host plan (`plan-koine-host.md`) ended with a list of what it did not
do (H-D9) and what the end-to-end run showed missing. This plan does those,
in the order an agent building an app over an API library meets them: the
app needs a token before its first verb runs (F-D1); its tables need to know
which field is the id (F-D2); its results want drawing, not placing (F-D3);
two processes over one directory want to hear each other (F-D4); two synced
roots want their conflicts closed (F-D5). F-D6 is housekeeping from the move
of the MCP host.

## Decisions

### F-D1. A secret is declared by the verb and held by the host

A verb that calls a remote service needs a bearer. Passing it as an
argument puts it in every form, every CLI line and every audit row
(`#[impress_private]` redacts the log, not the form). The slot:

- `#[impress_method(secrets = ["scix-api-token"])]` declares what the verb
  needs. A name is a kind-id-shaped string (lowercase, digits, `-`/`_`,
  letter first); the macro refuses anything else, naming the rule. The
  descriptor carries `secrets: &'static [&'static str]`; `tools/list` shows
  it as `_meta.secrets`, the CLI listing prints it.
- `impress_service_core::secrets` is the slot: `Secret` (a `String` whose
  `Debug` and `Display` are `<redacted>`; `expose()` is the one way out),
  `trait SecretSource { get(name) -> Option<Secret>; names() -> Vec<String>;
  set(name, value) -> Result<(), SecretError>; remove(name) -> Result<(), SecretError> }`
  (`set`/`remove` default to `Err(SecretError::ReadOnly)`), `install(Arc<dyn
  SecretSource>)`, `get(name)`, `declared()` (every name any linked verb
  declares, with the verbs that declare it) and `status()` (declared ×
  present). With nothing installed, `get` reads the environment:
  `KOINE_SECRET_<NAME>` with the name upper-cased and `-` as `_`
  (`scix-api-token` → `KOINE_SECRET_SCIX_API_TOKEN`), so a bare CLI or MCP
  process needs no host.
- The pipeline refuses a verb whose declared secret is absent, before the
  policy layer, as `missing-secret` (HTTP 412), naming the secret and both
  ways to set it. A verb handler reads `secrets::get("scix-api-token")` and
  never sees `None` for a declared name.
- The generated form of a verb whose secret is missing opens with a
  `status` node of level `warning` naming it; the Run button stays, and the
  pipeline's refusal is what it shows.
- `koine-host` installs a layered source: the environment first, then
  `<workspace>/secrets/<name>` (directory 0700, file 0600, one line), then,
  on macOS with `--keychain`, `security find-generic-password -s koine -a
  <name> -w`. Routes: `GET /api/secret` lists `{name, present, source,
  verbs}` and never a value; `PUT /api/secret/<name>` with `{"value": "…"}`
  writes the file (a non-GET, so the guard already demands the loopback
  token); `DELETE /api/secret/<name>` removes it. A name no linked verb
  declares is refused `not-found` listing the declared ones: the slot is
  explicit or it is not a slot. `GET /api/status` gains
  `secrets: {declared, missing}`. Flags: `--secrets-dir <dir>`, `--keychain`.
- `impress-ai` in impress-apps keeps its own `Secret` for now; moving it
  onto this one is a later edit, noted there.

### F-D2. A verb says which field is the id of its rows

`returns_kind` names the noun; nothing said which column is its id, so a
generated table selected by a field called `id` and showed every property
in schema order. Now:

- `#[impress_method(returns_kind = "paper", rows(id = "bibcode", title =
  "title", columns = ["bibcode", "title", "year"]))]`. `rows(…)` needs
  `returns_kind` on the same method (rows are of a kind); `id` is required;
  `title` defaults to `id`; `columns` defaults to the row object's
  properties in schema order. The descriptor carries `rows: Option<RowSchema
  { kind, id, title, columns }>`.
- `VerbDescriptor::row_schema_check()` reads the output schema and refuses a
  field that is not a property of the row object (the `items` of an array
  result, or the one array-of-objects property of an object result). The
  Tier A report runs it for every linked verb that declares `rows`, so a
  renamed DTO field fails `cargo test`, not a form.
- `impress_surface::spec::Table` gains `id_field: Option<String>` (absent
  from the wire when `None`, so every existing surface serialises as
  before). A `select` on a table whose `id_field` is set carries that field
  of the row; a table without one selects by `id`, then by index, as today.
  The generated form's result table sets `id_field` and `columns` from
  `rows`, and its `on_select` writes the selection to `state.selected`, so a
  second button on the same form can pass `{{state.selected}}` to the next
  verb.
- `tools/list` shows `_meta.rows`; the CLI listing prints it.

### F-D3. Plots are drawn, in every frontend, by one crate

`plot{spec}` carries a `plot-spec@1.0.0` payload (ADR-0033) that implore
already draws as SVG with pure Rust (`implore-core::plot::{types,
svg_render}` and the `axis` ticks it uses, no dependency beyond serde). That
code becomes `koine-plot`, kit-grade and wasm-clean:

- `koine_plot::spec` is implore's `PlotSpec` and its parts, moved, with the
  serde shape kept bit for bit (it is the wire); `koine_plot::axis` is the
  tick calculator; `koine_plot::svg::render_svg(&PlotSpec) -> String` is
  the renderer, moved; `koine_plot::text::render_text(&PlotSpec, cols,
  rows) -> String` is new, an ASCII sketch for a terminal (axes, ticks, one
  glyph per series); `koine_plot::render_svg_json(&Value)` and
  `render_text_json` parse a spec held as JSON and answer a typed error.
- `koine-dioxus` draws the SVG inside `div.koine-plot` (the spec stays on
  `data-koine-spec`); a spec that does not parse becomes
  `div.koine-plot-error` naming the error. `koine-tui` prints the sketch.
  Both are conformance cases.
- In impress-apps, `implore-core::plot::types`, `::svg_render` and `::axis`
  become re-exports of `koine-plot`; every caller keeps its path. Two
  definitions of a plot would drift.
- The Swift renderer keeps its hook (`renderPlot` through imprint-core's
  Typst pipeline): it draws more than this crate does, and it is not a
  second definition of the spec once the types are koine's.

### F-D4. The directory store hears other processes

`cross_process_events` was false for `dir:` because nothing watched. Now:

- `koine-store-dir` gains a `watch` feature (default on) over `notify`.
  `DirStore::watch()` starts one thread watching `items/`, `operations/`
  and `clock`. It keeps a cache of every item it has seen (id → item), built
  by one scan when it starts, and on a change to a record file reads the
  file and emits what changed: a new id is `Created`, a missing file is
  `Deleted`, a higher clock is `Updated` with the mutations that take the
  cached item to the new one (payload set/remove, tag add/remove, reference
  add/remove, the flag fields, the parent), a new operation file is
  `OperationApplied`. Events are coalesced over a 50 ms quiet window, so a
  temp-and-rename is one event; a `Rescan` from the OS is a full reconcile.
- This handle's own writes are not echoed: the store records the `(id,
  clock)` of every record it wrote and the watcher skips a file carrying
  one. Two handles of one process on one root hear each other, which is
  what two processes do.
- `capabilities().cross_process_events` is true while the watcher runs;
  `koine_store::open("dir:<path>")` starts it, `dir:<path>?watch=0` does
  not. The conformance suite gains a case for a backend that claims the
  capability (a write through one handle reaches a subscriber on another).

### F-D5. Conflicts are resolved by a policy the app chooses

`open` reported a `<id>.conflict-<n>.json` copy with a higher clock than its
file and left both. Now the store takes a `MergePolicy`:

- `Report` (the default, today's behaviour), `TakeCopy`, `KeepFile`, `Union`,
  or `Custom(Arc<dyn Merge>)` with `fn merge(&self, file: &Item, copy:
  &Item) -> Item`. `Union` is the policy for two roots that each added
  things: tags and references are unioned, payload keys are unioned with the
  higher-clocked side winning a key both carry with different values, the
  flag fields and the parent come from the higher clock, `modified` is the
  later, and the result gets a fresh clock after observing both.
- `resolve_conflicts()` applies the policy under the writer lock: the
  winner is written as `<id>.json`, the copy moves to
  `conflicts/<id>.conflict-<n>.json` (kept: a merge is reversible by hand),
  a `core/operation` of type `Custom("merge-conflict", {policy, file_clock,
  copy_clock, copy})` is recorded on the item, and `Updated` is emitted with
  the mutations. A stale copy (a lower clock, no conflict) is moved to
  `conflicts/` by any policy but `Report`. `open` with a policy other than
  `Report` resolves after its scan, so a synced root heals when the app said
  so. `dir:<path>?merge=union|take-copy|keep-file` names the policy by URL.

### F-D6. The MCP host's old name

`impress-mcp-host` became `koine-mcp-host` (H-D5). The two places code still
said the old name (a comment in `impress-service-core`, the HTTP banner in
`koine-mcp-host`) say the new one; the historical plan documents in
impress-apps that cite it get one line saying where it went, and keep their
history.

## Work packages

| # | Package | Where |
|---|---|---|
| F1 | The secret slot in `impress-service-core` and the macro (F-D1 without the host), `rows(…)` end to end (F-D2): macro, descriptor, check, `_meta`, CLI listing, `Table.id_field`, the generated form | koine |
| F2 | `koine-host` secrets: the layered source, the routes, status, flags, the e2e assertions, the capability guide section | koine, after F1 |
| F3 | `koine-plot`; `koine-dioxus` and `koine-tui` draw; conformance cases; wasm check | koine |
| F4 | The watcher and the merge policy in `koine-store-dir`; the URL parameters in `koine-store`; the suite case | koine |
| F5 | F-D6; this plan's status; the rev bump | both |
| F6 | impress-apps: `implore-core` over `koine-plot`; the descriptor literals; the Swift table's `idField` | impress-apps, after F1–F4 |

## Status

F1–F4 are built on koine main. What landed, and where it differs from the
decisions above:

- **F-D1 (core).** `impress_service_core::secrets` is as decided. The
  refusal is the pipeline's envelope `{ok: false, code: "missing-secret",
  message}` from `prepare`, the same path as `unsupported-adverb` and strict
  arguments, not a new `PipelineError` variant (every transport matches that
  enum exhaustively). The CLI shows `Secrets:` and `Returns:` as a verb's
  `--help` after-text; there was no listing that printed `returns_kind`
  before.
- **F-D1 (host).** As decided. Two behaviours worth knowing: `DELETE`
  answers `removed: true` only when the name was present before and absent
  after, so a value the environment still supplies reports `false` though
  the file is gone; and a `PUT` with `--keychain` still writes the file
  layer, because the file layer is asked first and the environment is
  read-only — the keychain is read by the host and written with `security`
  by hand. The e2e script asserts the list, the status field and the 404
  for an undeclared name; the scaffolded app declares no secret.
- **F-D2.** As decided, with two details: a select on a table carries
  `{{event.value}}` (the array of ids every select sends), so the generated
  form's `on_select` is `set state.selected = {{event.value}}`; and the
  default column order is the output schema's property order as
  `serde_json` holds it, which is alphabetical. `row_layout()` also reports
  which property wraps the rows when the result is an object, so the form
  reads `state.result.<that>`.
- **F-D3.** `koine-plot` is `implore-core`'s `plot::types`, `plot::svg_render`
  and `axis` moved verbatim (29 tests kept or added), plus `text`. The
  original fixture `{"x": [1,2], "y": [3,4]}` parses: `PlotSpec` takes
  defaults for every missing field and refuses no unknown one, so it is an
  empty chart, not an error; a spec that fails is one with a series missing
  `y`. The parser was left as it was: stricter would change what the wire
  accepts. The TUI's sketch is an opt-in (`TextFrontend::with_plots`) the
  binary turns on, because `render` promises one line per node and the
  cursor and the frontends suite rely on it. The text sketch ignores
  annotations and error bars. Two things in impress-apps are not what the
  decision's last bullet assumed: the store record `impress-core` registers
  as `plot-spec@1.0.0` wraps imprint's `impress-plot` FFI spec under
  `spec_json` (`spec_kind` series/grid), a different grammar from the
  surface node's (`surface-demo-service` and this crate speak implore's);
  and the Swift `renderPlot` hook's default is a placeholder that no app
  replaces yet. `implore-core`'s `render_plot_svg` FFI, over `koine-plot`
  after F6, is the function an app installs that hook with. Reconciling the
  two record grammars is not this plan's.
- **F-D4.** As decided. `FieldMutation` has no variant for priority or
  visibility, so a change to only those is `Updated` with an empty mutation
  list. `wait_until_watching(Duration)` lets a caller block until the first
  scan is done. Dropping the store sets a flag and never joins the thread.
- **F-D5.** As decided. A conflict copy of an operation row is reported and
  not merged (an operation has nothing to merge); `Resolved` lists merges,
  not the stale copies quietly filed; `with_merge_policy` swallows a resolve
  error (the conflicts stay reported; `resolve_conflicts()` shows it).
