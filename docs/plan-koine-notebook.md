# Koine notebook: the call log as a lab notebook

Every impress app ships a console window. It shows a log filtered by search
and level (the `console` view kind, `impress-layout-service/scenarios/
layout.console_pane.json`) and a Performance tab fed by `pipeline::perf`:
per-verb counts, p50/p95, self against total time and budget breaches,
plus a ring of the last 4096 spans that exports as Chrome trace JSON or
folded stacks. Beside it, the pipeline's audit layer builds one
`core/verb-call@1.0.0` record per mutating call: who called, the trace and
parent, a privacy-filtered argument summary, whether that summary can be
replayed, the ids written, the adverbs, the undo and the duration.

That is a lab notebook's raw material. A researcher's question is not "what
did the process log" but "what did we do, in what order, with what, how
long did each step take, and can we do it again". The console answers the
first and forgets; the notebook answers the second and keeps. This plan
joins the two on `call_id`/`trace_id` and closes the gaps between them.

| Gap | Decision |
|---|---|
| A koine host installs no audit sink, so its call records are dropped | N-D1 the store sink |
| Step timing lives in an in-memory ring and is gone by next month | N-D2 the step tree on the record |
| Which records a call *read* is never recorded | N-D3 the read set |
| Read-only calls are recorded only under `IMPRESS_CALL_LOG=all` | N-D4 recording reads is a switch |
| Nothing groups calls above one trace | N-D5 runs |
| Two timings from two machines look comparable | N-D6 host and build on the record |
| The log is a developer's view | N-D7 the notebook verbs and surface |
| A failed step is a dead end | N-D8 diagnose, replay, export |
| The log grows without bound | N-D9 retention |

## Decisions

### N-D1. A koine host keeps its call log

`koine-host` installs an audit sink at start (`koine_notebook::sink::
StoreSink`): a bounded channel (1024 records) and one writer thread. The
writer stores each `VerbCallRecord` as an item of schema
`core/verb-call@1.0.0` whose id is the call id (a uuid, `pipeline::prepare`)
and whose payload is `VerbCallRecord::payload()` plus the fields this plan
adds. The author is the caller (`CallerIdentity::author`), the author kind
`Human` for the person, `Agent` for an agent and `System` otherwise. A
record whose call ran on a per-call store override (a Tier A scenario) is
not written to the host's store; the sink counts it as skipped. A full
channel counts the record in `audit::count_dropped()` rather than blocking
the call, as the audit layer's contract requires.

The writer runs outside any call, so its own writes are never audited and
never recorded as a call's mutations. A successful read-only call of the
notebook's own plumbing (the surface wait loop and render, the notebook's
read verbs) is not written: while a run records reads it would otherwise be
most of the log. `GET /api/status` answers `call_log: {written, dropped,
run}`.

### N-D2. Every record carries its step tree

`CallContext` gains `notes: CallNotes`, shared by every clone of the
context. When a call finishes, the pipeline pushes a `StepRecord` into the
`notes` of the call it ran inside (the context current at `finish`, which
is the parent's, since `finish` runs after the child's own scope has
ended):

```json
{ "verb": "…", "call_id": "…", "duration_us": 1840, "self_us": 210,
  "ok": true, "code": null, "steps": [ … ], "steps_truncated": 0 }
```

`self_us` is the duration less the children's, floored at zero (concurrent
children may overlap). Every nested call contributes, read-only ones
included, so the record of a surface click shows each verb its actions ran
even when none of them is audited on its own. Bounds: at most 64 steps per
level and 8 levels; what is cut is counted in `steps_truncated`. The
record also carries `duration_us`, the microsecond figure the span already
has. Nothing here reads the tracing layer, so a host that installs no
`perf` layer still records steps, and the timings on the record are the
pipeline's own `Instant`s.

### N-D3. The read set

`CallNotes` also holds the ids a call read: `context::note_read(id)`,
deduplicated, at most 256 ids, the rest counted in `reads_truncated`. The
record carries `read_ids` and `reads_truncated`, and `updated_ids`: what it
changed in place (`context::note_update`), beside the `inserted_ids` and
`deleted_ids` it always had. A store does not know it
is inside a call, so koine-host wraps the store it opens in
`koine_store::RecordingStore`, which delegates every method and, after a
successful `get` or `query`, notes the ids it returned; after `insert`,
`insert_batch`, `delete` and `apply_operation`, it notes the mutation
(`note_mutation`), which the memory and directory backends never did, so
`inserted_ids`/`deleted_ids` are filled on a koine host for the first time.
Rows written by the materialiser after a handler (L-D4) are noted on the
enclosing call, if any: the materialiser runs after the child's scope.

### N-D4. Recording reads is a switch, not an environment variable

`audit::log_all()` reads `IMPRESS_CALL_LOG=all` once. It becomes
`audit::set_log_all(bool)` over an atomic initialised from the variable, so
a host or a run can turn it on and off. A run that records reads turns it on
while it is open (N-D5).

### N-D5. Runs

A run is a named, open-ended grouping of calls: an item of schema
`koine/run@1.0.0` with `name`, `started_at`, `ended_at`, `record_reads`,
`host`, `build` and `status` (`open`, `closed`). While a run is open, every
record the sink writes carries `run: <run id>`. One run is open at a time
in a process (`audit::set_current_run`); starting a second closes the
first. This is the lab notebook's "recording" switch, not a scheduler: it
groups whatever the person and the agents did while it was on, and the
timeline can still filter by caller. A host that restarts with a run open
re-opens it (`koine_notebook::install` reads the store).

Calls do not opt into a run, so no new adverb is added to the grammar; a
per-call run is a follow-up if two agents must record into two runs at
once.

### N-D6. Host and build on the record

The sink stamps every record with `host` (the host's device name) and
`build` (`{app, version}`), so a timing is always read with the machine and
the build that produced it. `notebook_compare` (N-D7) groups by them.

### N-D7. The notebook: verbs and a surface

`koine-notebook` is a service crate, `notebook-service`, linked by every
koine host:

| Verb | Safety | Does |
|---|---|---|
| `runs` | read-only | the runs, newest first, with call counts |
| `run-start {name, record_reads?}` | mutating | open a run (closing any open one) |
| `run-stop` | mutating | close the open run |
| `timeline {run?, caller?, verb?, failed_only?, limit?}` | read-only | top-level records, newest first: time, verb, caller, ok/code, duration, self time, step count, writes, reads |
| `entry {call_id}` | read-only | one record in full, its steps flattened for a table (depth-indented), the slowest steps, folded stacks, a `plot-spec@1.0.0` bar chart of self time per step, and its notes |
| `annotate {call_id, text}` | mutating | attach a note (`koine/notebook-note@1.0.0`, parent: the record) |
| `compare {verb, run_a?, run_b?}` | read-only | per step verb: count, mean, p50, p95 of total and self time in each set, and the ratio |
| `replay {call_id}` | mutating | run the recorded call again when its arguments are replayable; the answer names the new call |
| `export {run}` | read-only | the run as Markdown: what was done, by whom, how long each step took, what was written, the notes, the host and build |
| `prune {keep}` | destructive | delete the oldest records outside any run beyond `keep` |

The Notebook surface (`notebook.surface.json`, shipped by the kit package,
see plan-koine-research-os B-D3) puts them together: the runs with start
and stop, the timeline as a table, and for the selected entry its summary,
its steps table, its self-time plot, its notes with an annotate field, and
Replay, Diagnose and Export buttons. A top-level record is one whose
`parent_call` is absent or names a call outside the log.

`replay` re-enters the pipeline as the replaying caller, so policy decides
again: an agent replaying a destructive call is queued for review like any
other agent write. Its answer carries `replay_of` and the new `call_id`.

### N-D8. Diagnose and export

A failed entry's Diagnose button calls `agent-service_diagnose {call_id}`
(plan-koine-research-os G-D3): the request an agent picks up carries the
record, its step tree, the verb's descriptor and the run. `export` is what
goes into a paper's methods section or a collaborator's inbox; it holds no
argument value the audit layer did not already keep.

### N-D9. Retention

A record inside a run is evidence and is kept. A record outside any run is
telemetry: `prune {keep}` deletes the oldest of those beyond `keep`, and the
sink prunes to 5 000 (`SinkConfig::keep`) every 512 writes. Notes are kept
with their records and deleted with them. (5 000 rather than more: the
directory store reads one file per record to answer a timeline.)

## What this does not do

- CPU time, peak memory, bytes read and GPU time. A handler is an async
  future that moves between threads, so per-thread `getrusage` would
  misattribute; the honest measure is a job-scoped sampler in the runner,
  a follow-up.
- Token and money cost of agent steps: the host does not run the model
  (plan-koine-mobile M-D7); an agent may `annotate` its own cost.
- The Swift console's log lines: they live in impress-apps. The notebook
  surface links to a call's trace id, which the console can filter on.

## Work packages

1. `impress-service-core`: `CallNotes`, `StepRecord`, `note_read`, the
   step push in `finish`, `duration_us`/`steps`/`read_ids`/`run` on the
   record, `set_log_all`, `set_current_run`.
2. `koine-store`: `RecordingStore`.
3. `koine-notebook`: the sink, the service, the surface, `install`.
4. `koine-host`: wrap the store, install the sink, link the service.

## What pins this

| Rule | Test |
|---|---|
| A nested call's step lands on its parent's record with self time | `impress-service-core` pipeline test |
| Steps are bounded and the cut is counted | `impress-service-core` pipeline test |
| A read through `RecordingStore` inside a call is on the record; a write fills `inserted_ids` | `koine-store` recording tests |
| `set_log_all` turns read-only recording on and off | `impress-service-core` audit test |
| The sink writes a record per call with run, host, build | `koine-notebook` sink test |
| A run groups calls; stop ends the grouping; restart re-opens | `koine-notebook` service tests |
| Timeline, entry, compare, export answer from stored records | `koine-notebook` service tests |
| A hosted call is in the log over HTTP and the notebook surface validates | `koine-host` `tests/host.rs` |

## Status

Landed 2026-10-15 (branch `research-os`), N-D1 to N-D9 as written. The
pipeline's step tree and read set are in `impress-service-core`
(`pipeline::context::{StepRecord, CallNotes}`, `audit::{set_log_all,
set_current_run}`); `koine_store::RecordingStore`; `koine-notebook`; and
`koine-host` wraps its store, installs the sink and links the service.

Note for impress-apps: `CallContext` gained a `notes` field and
`VerbCallRecord` gained `duration_us`, `steps`, `steps_truncated`,
`read_ids`, `reads_truncated`, `updated_ids` and `run`. A struct literal of
either needs the new fields (`notes: Default::default()`); the payload
change is additive.
