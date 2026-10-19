# The run convention: how a scientific code declares itself

**Status:** design, 2026-10-19. Nothing is built. **Builds on:** plan-koine-store.md
(the directory store's layout and its port), plan-koine-notebook.md (runs and the
call log), plan-koine-ai-tasks.md (jobs, events, workflows), plan-koine-trust.md
(TR-D12 kind evolution, TR-D13 keeping the unknown), plan-koine-live.md (L-D3
progress, L-D6 files, L-D8 the bound plot), plan-koine-tree-stream.md (T-D2
streams). **Independent of** all of them: a code that follows this convention
needs none of koine to be useful, and koine needs no per-code wrapper to use it.

## Why

Two of our codes were surveyed on 2026-10-19: titan.jl (1D radiation
hydrodynamics, a Fortran port) and Vespa.jl (AMR cosmology on GPUs). They are
different in every way except how they present themselves to the person who
runs them, which is the same five gaps:

| Gap | titan.jl | Vespa.jl |
|---|---|---|
| Parameters are scattered environment variables, with units and defaults only in comments | ~60 `TITAN_*`/`QS_*` plus ~55 global `Ref` switches | ~470 `MHD_*`/`CIC_*`/`BAM_*`, most read in one 7.2k-line driver |
| The run's own parameters are not recorded with its outputs | only `setup_meta_<name>.jls` (M, L, R, r_in) | `vrun`'s manifest greps `CIC_*` only; the `MHD_*` and `BAM_*` values of the production runs are gone |
| Monitoring is `@printf` and `grep` for sentinel lines; stopping is `kill` | `QS_EVERY` printf; restart via `QS_FROM=` | `LEVEL 10 POPULATED` grepped by a bash supervisor that polls `nvidia-smi` |
| Each GUI is hand-built | the "laboratory": vanilla JS, its own plot library, a hand-kept state schema | EnzoViz: a hand-rolled HTTP server, works only for the core driver |
| Sweeps and analysis are copies | bash loops over env vars; three `snap()` extractors | 112 config files; 60+ near-duplicate `summarize_*`/`compare_*` scripts; 526 run folders in three metadata styles |

Every code we write next will have the same five gaps, because nothing tells
it otherwise. koine can take a code's parameters, runs and events and give
back a form, a run browser, live plots, agent tools and a provenance log —
but only if the code presents them in a shape koine can read. That shape is
this document. It is written to be followed by a code that has never heard
of koine (a Python code, a Fortran code behind a shell script) and to lose
nothing when it is.

### What the prior art settled

The design below copies what has worked for decades and declines what has
been walked back. The survey (2026-10-19) is summarised here because each
decision cites it.

- **FLASH** declares every runtime parameter in a `Config` file with type,
  default, range and a `D` doc line; `setup` collects them into one generated
  list. Its one documented trap: an unknown name in `flash.par` is a
  *warning* ("Ignored Parameters"), which misconfigured runs for years.
- **Cactus / Einstein Toolkit** declares parameters in `param.ccl` with
  ranges-with-descriptions and a steerability level, `NEVER | RECOVER |
  ALWAYS`; unknown parameters are errors; the generated `.par` of the
  effective values is written into the output directory; its HTTPD thorn
  steers a running code over a web form. Its gaps: steered values are
  re-dumped as a snapshot every N iterations, so nobody can later say *when*
  a value changed; and `autoprobe` recovery starts fresh if the checkpoint
  path is wrong, silently.
- **PETSc** makes options self-describing at runtime (`-help`) and catches the
  misspelt one (`-options_left`).
- **signac** keys each run by a hash of a canonical serialization of its
  parameters, keeps that serialization beside the hash as a self-check,
  separates the immutable key from a mutable document, avoids file locks
  because they fail on cluster filesystems, and treats its index as a
  rebuildable cache. Its 2.0 removed the database layer, the crawlers and
  project names: "in practice most of our users stay with the job-project
  data model". Measured: ~1 ms per directory on NFS, 10⁴ runs interactive,
  10⁵ not.
- **DrWatson.jl** stamps every saved file with the git commit (`-dirty` with
  the patch), the script and line; is "entirely non-invasive"; and says
  plainly it "is not a data management system".
- **AiiDA** has the most complete provenance graph in the field and the
  highest adoption cost: PostgreSQL and RabbitMQ were mandatory until 2.6
  made them optional (`verdi presto`), each code needs a plugin, its typed
  ports stop at an opaque `Dict` so the real parameter schema lives in
  Python, software version strings in its cache hash invalidated every cache
  on every upgrade (replaced by an explicit counter), and AiiDAlab generates
  no GUI from the spec — every panel is hand-written.
- **ParaView Catalyst** and **Ascent** steer running codes through declared
  parameters; **openPMD** is the field's metadata standard for mesh and
  particle data; **Workflow Run RO-Crate** is the archival export for a run's
  provenance.

The pattern across all of it: the pieces that lasted are declarative files a
code writes and any tool reads; the pieces that were removed are services,
databases and per-code plugins.

## Decisions

### R-D1. The unit is a run, and a run is a directory

A run is one execution of one code on one problem, from start to a terminal
state. It is a directory that holds, and holds only, what this convention
names plus the code's own outputs:

```
<run>/
  run.json          the record: identity, code, problem, parameters, host (R-D4)
  params.json       the effective parameter values, one object (R-D3)
  events.jsonl      append-only: progress, metrics, checkpoints, outputs, changes (R-D6)
  outputs.json      the index of artifacts the code produced (R-D7)
  notes.json        mutable annotation, never hashed (R-D5)
  control/          requests to a running code, one file each (R-D8)
  <the code's own files: checkpoints, dumps, logs, …>
```

Everything else about the run — its name, its place in a sweep, who ran it,
what it was for — is *in* these files, never *in the path*. A run directory
can be moved, copied, rsynced, archived to HDD and restored without changing
meaning, which is the test vrun's `archive` already has to pass by hand.

Why a directory and not a database row: the run happens on a cluster node
with a batch scheduler, no daemon and a filesystem that drops locks (signac,
AiiDA 2.6). A directory needs nothing installed, survives the code crashing,
and is readable by `cat`, `jq`, Python and git. koine's own directory store
is one JSON file per item for the same reasons (plan-koine-store D4), and a
run directory is designed to *be* such an item: `run.json` is the item, the
rest are its blobs and its event ring.

### R-D2. Parameters are declared once, in a schema the code emits

A code declares every parameter it reads: name, type, default, unit,
constraints, documentation, group, and **when it may change** (R-D2.3). The
declaration is the code's, in the code's language (a Julia macro, a Python
decorator, a Fortran namelist plus a sidecar), and the code **emits** it as
JSON Schema on request (`<code> --schema`, or `Code.schema()`), the way
PETSc answers `-help` and FLASH generates `setup_params`. The emitted form is
the contract; the declaration syntax is the code's business.

**R-D2.0 Declared once, where it is read.** The declaration is the one place
a parameter exists, and it stands where the code reads the value:

```julia
@param sink.tau::Float64 = 0.0  unit="s" group="sink" mutable=:never env="QS_SINK_TAU" \
       doc="inner sink relaxation time; 0 = closed wall"
```

From that line and nothing else: the value the code reads (`params.sink.tau`),
the emitted schema, the entry in `params.json` with its source, the
environment variable that is read because the declaration names it, and —
downstream, from the schema — the form, the MCP tool, the CLI flag and the
App Intent. There is no table of parameters, no table of environment
variables, no documentation page that lists them: each of those is a view of
the registry, generated on request, and a person keeps nothing consistent
by hand. The same holds for metrics (`@metric`, R-D6.4) and for the open
group (R-D2.5), which the code's own tables generate.

The one drift this leaves — a declaration that outlives the read it was
written for — is caught at exit (R-D2.5, PETSc's `-options_left`).

**R-D2.1 The form.** JSON Schema (draft 2020-12), one object whose
properties are the parameters, with these vocabulary extensions under an
`x-run` key where JSON Schema has no word for it:

| Field | JSON Schema | Why |
|---|---|---|
| type, default, enum, minimum/maximum, pattern, description | standard | tools and koine already read these (`impress-verb-surface` builds a form from exactly this) |
| `x-run.unit` | `"Msun/yr"`, `"s"`, `"cm"` — a UCUM or astropy-parseable string | titan's units live in comments today |
| `x-run.group` | `"sink"`, `"grid"`, `"io"` | 470 parameters need grouping before a form is usable (AiiDAlab: a GUI exposing all features "can quickly overwhelm") |
| `x-run.mutable` | `"never"` \| `"restart"` \| `"always"` | R-D2.3 |
| `x-run.env` | the environment variable this parameter replaces | the migration path (R-D11) |
| `x-run.advanced` | `true` to hide by default | FLASH/Cactus hide nothing and their par files are hundreds of lines |

JSON Schema rather than a new format because every consumer we have (koine's
verbs, MCP tools, App Intents, the CLI, the form generator) already speaks
it, and because its `$ref` and `allOf` let a problem's schema be composed
with the code's (R-D2.4).

**R-D2.2 Namespaced, not global.** Parameter names are `component.name`
(`sink.tau`, `grid.ncell`, `io.every`), never a flat global list. Cactus's
`thorn::param` with explicit `SHARES`/`USES` makes dependencies visible;
FLASH's global list lets any unit read any parameter and the coupling is
invisible. A component declares its own parameters and names the
components it reads.

**R-D2.3 Three mutability classes.** Cactus's `STEERABLE = NEVER | RECOVER |
ALWAYS`, with its exact semantics and one rename:

- `never` (the default): fixed once the run starts; a restart carries the
  checkpoint's value and refuses a different one.
- `restart`: may differ when resuming from a checkpoint (`zend`, CFL, output
  cadence). Vespa's PMF restore already does this by hand: its checkpoint
  carries a config fingerprint, a mismatch refuses the resume, and
  `MHD_CHECKPOINT_ALLOW_CFL_CHANGE=1` waives one field at a time. The class
  on the parameter replaces the per-field waiver variables, and the
  fingerprint becomes the key of the `never` subset.
- `always`: may change while running, through the control inbox (R-D8).

The class is enforced *in the setter*, matched exactly (Cactus once matched
`RECOVERY` by substring), and tested per code. Nothing else in this
convention needs the code to be re-entrant: a code with global state, like
titan, is `never` for everything and still conforms.

**R-D2.4 A problem is a named generator with its own schema.** Vespa's rule
that "a problem is source code, not a parameter file" stands: initial
conditions are a closure, and no convention will serialize a closure. What
the convention asks is that the generator have a *name*, a *version* and a
*parameter schema* of its own (`problem.name = "sod"`, `problem.version =
"2"`, `problem.params = {…}`), so a run records which generator produced its
initial state and with what. An input file (a MUSIC IC, a reference profile)
is a parameter of type `file` and is recorded by content hash (R-D4.2).

**R-D2.5 Unknown is an error.** A value for a name the schema does not
declare refuses the run (Cactus's `--parameter-level strict`), never warns
(FLASH's "Ignored Parameters"). A relaxed mode exists and must be asked for
by name. A declared parameter the code never reads is reported at exit
(PETSc's `-options_left`), because it is the other half of the same bug.

A code with a large inherited parameter space it cannot enumerate by hand —
titan's Fortran COMMON blocks, hundreds of scalars reached today through
`QS_SET=ql1=1e-4,cq2=1` — declares that space as one **open group** with
a generator: `x-run.open: true` on a group whose names and types the code
answers at schema time from its own tables (`TitanFortran.parse_commons`
already knows every name, type and dimension). The group is still declared,
still typed, still strict about names; what it is not is written out by a
person. An open group with no generator is a schema error, not a loophole.

**R-D2.6 Validation that sees only parameters runs before anything is
allocated** — Cactus's `PARAMCHECK` bin. Cross-parameter constraints
(`QS_SINK_TAU needs QS_GASROW=1`) are declared as `x-run.requires` on the
parameter where tools can read them, and checked in that bin.

### R-D3. The effective values are one object, with their sources

`params.json` is the complete resolved parameter set — every declared
parameter, every value, after defaults, files, environment and command line
have been applied — plus, per parameter, where its value came from:

```json
{"sink.tau": {"value": 3.0e6, "source": "env:QS_SINK_TAU"},
 "sink.m0":  {"value": 1.2e34, "source": "default"},
 "grid.ncell": {"value": 400, "source": "file:qstar.toml"}}
```

Written **before the first step** and never rewritten (a steered change is
an event, R-D6.4). This is Cactus's generated `.par` and FLASH's runtime
parameter datasets with one addition, the source, which is the field a
person needs when a run did something unexpected ("was that the default or
did I set it?"). The *same file* is a valid input: `code --params
params.json` reproduces the run's parameters exactly (Cactus: "the same
format the user writes").

### R-D4. The record

`run.json` is the run's identity and provenance. It is written at start,
updated only in its `status` block, and is the item koine ingests.

**R-D4.1 Identity.** `id` is a UUID minted at start (not a hash: two runs
with identical parameters on different days are two runs). `key` is the
hash of the canonical serialization of the *parameters and problem* (R-D4.3)
— signac's statepoint id, which finds "the run with these settings" and
detects a duplicate. `name` is a human alias, free text, not unique (DrWatson's
`savename` is readable but lossy at three significant digits; signac's hash
is exact but opaque; keep both, make neither the path).

**R-D4.2 What is recorded.**

| Block | Fields | Precedent |
|---|---|---|
| `code` | name, version, git commit (full hash), `dirty: true/false`, the diff's hash when dirty (the diff itself as a blob), build flags, the environment lock file's hash (`Manifest.toml`) | DrWatson `tag!`; FLASH `sim info`; vrun records a 7-char hash and `julia_ver = "unknown"` |
| `problem` | name, version, params, input files by content hash | R-D2.4 |
| `params` | the key (R-D4.3); the values are in `params.json` | signac |
| `host` | hostname, OS, CPU, accelerator, scheduler job id, `argv`, the launching environment's relevant variables | FLASH `sim info` (build machine, flags) |
| `lineage` | `parent`: the run this one resumed from (and the checkpoint, by path and hash) or forked from; `sweep`: the sweep record this run belongs to (R-D10) | Cactus `autoprobe` starts fresh silently — a restart **asserts** what it resumed from |
| `status` | `state` (R-D4.4), `started_at`, `ended_at`, `exit` (R-D4.5), `last_event_seq` | the only block that changes after start |
| `convention` | `"run@1"`, the version of this document the record follows | signac `schema_version`; TR-D12 |

**R-D4.3 The key is a hash of a canonical form, kept beside the hash.** SHA-256
over the canonical JSON (RFC 8785: sorted keys, no whitespace, shortest
round-trip floats, UTF-8) of `{problem, params-values}` — values only,
not sources. The canonical form is what `params.json` holds, so `key ==
sha256(canonical(params))` is a self-check any tool can run (signac's
`calc_id(manifest) == dirname`, which powers its `repair()`). Floats are not
rounded: the key is exact. A readable, rounded `name` is for people.

**R-D4.4 States.** `created → running → {done, failed, cancelled}`, plus
`suspended` (checkpointed and stopped on purpose, resumable). A terminal
state is terminal. A run found `running` by a tool whose process is gone is
`orphaned`, a *derived* state the tool reports and never writes.

**R-D4.5 Exit is a labelled code.** `exit: {code: 0, label: "ok"}`,
`{code: 110, label: "walltime"}`, `{code: 320, label: "nan-in-state", message:
"…"}`. The code bands are part of the convention (0 ok; 1xx stopped by
environment; 2xx invalid input; 3xx numerical failure; 4xx code-specific),
and the conformance checker enforces the bands, which AiiDA's exit codes
never did. `invalidates_reuse: true` on a failure says the run must not be
treated as the answer for its key.

### R-D5. Frozen content, open annotation

Once `status.state` is `running`, `run.json` (outside `status`),
`params.json` and the problem are frozen. Everything a person adds later —
tags, a rating, a note, "this is the one in figure 3", links to a paper —
goes in `notes.json`, which is mutable, mergeable and never part of the key.
AiiDA's attributes versus extras and signac's statepoint versus document
converge on this split; both ecosystems call it their core idea.

Changing a frozen field is not an edit. It is a new run with `lineage.parent`
set (R-D4.2), or an explicit, logged `relabel` that moves the run (signac
rehashes and `os.replace`s, and still documents it as a footgun).

### R-D6. Events are an append-only log with a gap-free sequence

`events.jsonl` is one JSON object per line, appended, never rewritten:

```json
{"seq": 1, "t": "2026-10-19T14:02:11.503Z", "kind": "started"}
{"seq": 2, "t": "…", "kind": "progress", "step": 100, "time": 3.1e5, "dt": 12.0, "fraction": 0.03}
{"seq": 3, "t": "…", "kind": "metric", "step": 100, "values": {"mach": 1.42, "L_over_L0": 0.98}}
{"seq": 4, "t": "…", "kind": "checkpoint", "path": "ckpt_000100.jls", "sha256": "…", "step": 100}
{"seq": 5, "t": "…", "kind": "output", "path": "snap_000100.h5", "kind_of": "snapshot", "step": 100}
{"seq": 6, "t": "…", "kind": "changed", "param": "io.every", "from": 100, "to": 10, "by": "control:7f3a", "step": 100}
{"seq": 7, "t": "…", "kind": "warning", "message": "dt below floor", "step": 211}
{"seq": 8, "t": "…", "kind": "finished", "exit": {"code": 0, "label": "ok"}}
```

**R-D6.1 The kinds are fixed:** `started`, `progress`, `metric`,
`checkpoint`, `output`, `changed`, `warning`, `finished`, and `note` for
anything else, with free fields under `data`. A tool that meets a kind it
does not know keeps the line (TR-D13).

**R-D6.2 `seq` is gap-free and the cursor.** A reader that saw `seq` 40
asks for what follows; a gap means the file was truncated and the reader
says so. This is exactly koine's `task-event` ring (plan-koine-ai-tasks
Q-D4, "a gap-free `seq` derived into the row id"), so an ingested run's
events need no translation.

**R-D6.3 Progress replaces sentinel lines.** `progress` carries the step,
the simulation time, `dt` and a `fraction` when the end is known; it is the
line vrun's supervisor greps `LEVEL 10 POPULATED` for, and koine's
`job::progress` (L-D3). It is throttled by the code (`io.every`, already a
parameter in both codes) and costs nothing in the inner loop.

**R-D6.4 Declared metrics.** The metric names a code emits are declared
where they are emitted (`@metric mach unit="" doc="shock Mach number"`),
the same way as parameters (R-D2.0), and appear in the emitted schema under
`x-run.metrics` (name, unit, description). Vespa's `MHD_HISTORY_TSV`
columns exist today with no declaration anywhere; koine's bound plot (L-D8)
needs the names to draw them.

**R-D6.5 A steered change is an event, with its source.** `changed` names
the parameter, both values, the step, and who asked (`control:<request
id>`, `restart`, `agent:<name>`). Cactus re-dumps a snapshot every N
iterations and loses the when; this keeps it.

**R-D6.6 Append-only is the concurrency model.** One writer (the run)
appends; any number of readers tail. No locks (signac: "do not work
reliably on the network file systems commonly employed in HPC"). A line is
written whole with one `write` call, under 4 KiB, which is atomic on every
filesystem we run on; a reader ignores a trailing partial line.

### R-D7. Outputs are indexed, not discovered

`outputs.json` lists each artifact the code wrote: `path` (relative to the
run), `kind` (`snapshot`, `checkpoint`, `series`, `spectrum`, `image`,
`log`, `other`), `format` (`hdf5`, `openpmd`, `jls`, `csv`, `bin+toml`),
`step`, `time`, `sha256`, `bytes`, and a `schema` sidecar path when the
format has one (Vespa's `fields.toml` is exactly this sidecar and is kept
as-is). Every `output` event (R-D6) is also an entry here; the file is the
index, the event is the notification. A tool that wants "the last snapshot"
reads one file instead of globbing a directory of thousands.

New field and particle outputs use **openPMD** over HDF5. Existing formats
are not converted: `bin+toml` and `jls` are kinds the index names, and a
reader library per format is the code's own (`vespa_io.py` already is).
Checkpoints are the code's format; the convention asks only that they be
indexed, hashed, and named in `lineage.parent` when resumed from.

### R-D8. Steering is a control inbox, served by the code

A running code that supports steering reads `control/` once per
`io.every` steps (from the callback both codes already have:
`evolve!(…; callback)`, `run_julia!(; output)`): one JSON file per request,
`control/<request id>.json`, `{"request": "set", "param": "io.every",
"value": 10}` | `{"request": "checkpoint"}` | `{"request": "stop", "after":
"checkpoint"}` | `{"request": "pause"}`. The code acts, appends a `changed`
or `checkpoint` event naming the request id, and renames the file to
`control/done/<id>.json` (or `control/refused/<id>.json` with a reason when
the parameter is not `always`). A requester polls `events.jsonl`.

Files, not a socket, because a cluster node has a filesystem and seldom has
a reachable port; because a request survives the requester disconnecting;
and because `ls control/` is the whole protocol. Cactus's HTTPD and
Catalyst's live connection are richer and need the code to serve HTTP; a
code that wants that can, and the inbox stays the fallback. Pause, single
step and "until step N" are Cactus HTTPD's request set and are the v1
vocabulary.

### R-D9. The convention is files; the library is a convenience

Everything above can be satisfied by a code that writes the files itself.
The reference library (R-D12) exists so that a Julia code satisfies it in an
afternoon, not so that a code must link it. This is the lesson of AiiDA's
plugins ("an additional overhead if the application code changes
frequently") and of DrWatson's "entirely non-invasive": the moment
conformance needs a dependency, the Fortran code, the shell-scripted code
and the collaborator's code are out.

A **conformance checker** (`run-check <dir>`) is the other half of this
decision: it validates the files against the schemas in this document,
checks the key against `params.json`, the event sequence for gaps, the
output index against the files present, and the exit bands. It is what CI
runs and what an importer trusts.

### R-D10. A sweep is a record of its own

A sweep is a directory with `sweep.json`: the parameters varied and their
values or generator (a grid, a list, a sampler with its seed), the base
parameters, and the runs it spawned by id. Each run's `lineage.sweep` points
back. DrWatson's `dict_list`/`@onlyif` and Vespa's 112 config files both
leave the sweep's definition in memory or in bash; this writes it down so a
sweep can be regenerated, extended ("add two more seeds"), and diffed.

### R-D11. Migration keeps the old doors open

- **Environment variables keep working.** `x-run.env` on a parameter names
  its old variable; the library reads it as one more source (`source:
  "env:QS_SINK_TAU"`) and records it. Existing configs run unchanged on day
  one, and their values are finally in `params.json`.
- **Old run folders are imported best-effort.** An importer for vrun's
  `manifest.toml`, Vespa's `metadata.toml` and `command.sh`, and titan's
  `setup_meta_*.jls` produces a `run.json` with `convention: "run@1"` and a
  `provenance: "imported"` marker, a `key` only when the full parameter set
  can be recovered (for most PMF runs it cannot: the manifest never captured
  `MHD_*`), and `notes.json` seeded from `NOTES.md`/`campaigns/*.md`
  mentions. Nothing is moved; the import writes beside what is there. The
  real tree is the fixture: 526 run folders, of which 388 have
  `metadata.toml`, 166 `command.sh` and 5 a vrun manifest.
- **The schema can be drafted mechanically.** A script scans
  `get(ENV, "X", default)` sites and emits a draft schema with the name,
  the default and the comment on the line as the description. The units,
  groups and mutability are filled in by a person, once.

### R-D12. The Julia library

`RunConvention.jl` (name open), small, no dependencies beyond JSON and SHA:

- `@param module.name::T = default unit="…" group="…" mutable=:restart doc="…" env="…"`
  registers a parameter; `params()` resolves defaults → file → env → CLI
  and refuses unknown names (R-D2.5); `schema()` emits the JSON Schema.
- `start_run(dir; problem, name)` writes `run.json` and `params.json`, stamps
  git and the Manifest hash (DrWatson's `tag!`, reused if present), and
  returns a handle.
- `progress!(run; step, time, dt, fraction)`, `metric!`, `checkpoint!`,
  `output!`, `warn!`, `finish!(run, exit)` append events and keep
  `outputs.json`; each is one `write`, throttled by the caller.
- `poll_control!(run)` reads the inbox and applies `always` parameters.
- `resume(dir; from)` opens a run as a child with `lineage.parent` set and
  the `restart` parameters applied.

What it does not do: schedule, sweep, plot, serve HTTP, or know koine.
Those are koine's (R-D13) or the person's.

### R-D13. What koine does with a conforming run

This is where the convention pays, and none of it is per-code:

| Convention piece | koine |
|---|---|
| the emitted schema | a `run-service_start` verb per code, registered by the code's process as a runtime provider; its form generated by `impress-verb-surface`, grouped by `x-run.group`; the same schema makes the MCP tool, the CLI and the App Intent |
| `run.json` | a `run@1` item in the store (the directory store ingests it as the item it already is); the run browser is a surface over `query` of that kind, with `key` for "same settings", `lineage` for restart chains, `notes` for tags |
| `events.jsonl` | the run's task-event ring; `progress` feeds L-D3; `metric` feeds the bound plot (L-D8) with the declared metric names; `changed` and `warning` are the live log |
| `outputs.json` | artifacts, linked from the run; a `snapshot` of a known format opens in implore's figure panel; `openpmd` gets a reader once |
| `control/` | `run-service_set`, `_checkpoint`, `_stop` verbs, each `mutating`, each routed through trust (TR-D4: a 7-hour GPU job's parameter change is a review request) |
| `sweep.json` | a koine-tasks workflow: one task per run with dependencies and retries; the bash supervisor's "relaunch from the newest checkpoint" is a retry policy |
| `exit` bands | `failed` with a `3xx` files a `diagnose` request on the agent desk with the last 50 events as context |
| `key` | `once`-style reuse: a `start` whose key matches a `done` run with `invalidates_reuse: false` answers that run instead (AiiDA caching, keyed by content and an explicit counter, never a version string) |
| the whole run | `notebook_export` to Workflow Run RO-Crate (Process Run Crate profile: one tool, one `CreateAction`) for archiving beside a paper |

The one new koine kind is `run@1.0.0`, declared by TR-D12's `KindSchema`,
and the one new koine capability is `run-service`, generic over any
conforming code. The host that runs it is `koine-host --tasks` on the
machine that has the runs' filesystem; the GPU box serves the laptop through
the network listener it already has.

### R-D14. Not in this plan

- Field and particle data formats beyond naming them: openPMD is adopted,
  not redesigned.
- Scheduler submission (SLURM, PBS): a run's `host.scheduler_job` records
  it; submitting is koine-tasks' or vrun's.
- Multi-code federation (Vespa's MultiCode): each engine is a code with its
  own schema; a comparison is a sweep over the `code` parameter.
- Richer visualisation than koine-plot: titan's time slider and EnzoViz stay
  embedded custom panels until koine's surfaces catch up.
- A Python or Fortran library: the files are the contract; those come when
  a code needs them.

## What pins this

| Rule | Test |
|---|---|
| A run directory round-trips through the checker: every file validates, `key == sha256(canonical(params))`, `seq` has no gaps, every indexed output exists | `run-check` on the fixture runs, in CI for both codes |
| An unknown parameter name refuses the run; a declared one never read is reported at exit | library unit tests; one per code |
| `never` refuses a different value on restart, `restart` accepts it and logs `changed{by: "restart"}`, `always` accepts it from the inbox | library tests, exact-match on the class |
| `params.json` fed back as input reproduces the same key | library round-trip test |
| A line appended to `events.jsonl` is readable by a concurrent tail without a lock; a partial trailing line is ignored | library test with a reader process |
| The importer produces a valid `run.json` for every one of the 526 `vespa-runs` folders, with `key` present exactly where the parameters are complete, and changes nothing in place | importer test over the real tree (read-only) |
| koine ingests a conforming run as a `run@1.0.0` item and its events as the task-event ring, and the generated form for the emitted schema validates with zero errors | koine `run-service` tests, Tier A |

## Rollout

1. **The spec and the checker** (this document's schemas as JSON Schema files;
   `run-check`). Two weeks.
2. **`RunConvention.jl`** and the mechanical schema drafter. One week.
3. **titan's `qstar_run.jl`** adopts it: 27 `QS_*` parameters become a
   registry, the COMMON scalars an open group (R-D2.5), the `output`
   callback emits progress and checkpoints (it already writes one every
   `10·QS_EVERY` steps), `QS_FROM` becomes `resume`. Half a day once the
   library exists; proves the spec on a global-state Fortran port, which is
   the hardest case for everything except steering.
4. **One Vespa production driver** (`pmf_dm_lattice_bench.jl`) adopts it:
   211 `get(ENV, "MHD_…")` sites become a registry, and the provenance gap
   closes the day it lands (today's manifests record none of them). Its
   checkpoint fingerprint and `ALLOW_*_CHANGE` waivers become `never` and
   `restart` classes. Harder: a 7.2k-line driver. Two weeks, mostly the
   person's time on units and groups.
5. **The importer** over `vespa-runs`. One week.
6. **koine `run-service`** and the `run@1.0.0` kind. Two weeks; after this,
   the next code pays nothing.

Steps 1–5 need no koine change and are worth doing alone.

## Open

- **Hash canonicalisation of floats.** RFC 8785 shortest round-trip is
  exact and unambiguous, but two codes that compute the same `dt0` with
  different arithmetic get different keys. That is correct (they are not
  the same run) and surprising. The `name` alias absorbs the surprise.
- **Very long runs and `events.jsonl` size.** At one `progress` line per
  100 steps a million-step run is 10⁴ lines, fine; a `metric` line per step
  is not. The schema's `x-run.metrics` could carry a cadence, or metrics
  could go to a columnar file with only a pointer event. Decide on the
  first run that hits 10⁶ lines.
- **Who writes `outputs.json` for a code that cannot** (a Fortran binary
  writing dumps by itself): a sidecar watcher in the launcher, or the
  importer after the fact. Both conform; neither is specified here.
- **Secrets and paths in `host`.** `argv` and environment can hold tokens
  and home directories. The library redacts by a declared list; the checker
  flags anything that looks like a credential.
