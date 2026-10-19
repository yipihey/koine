# run-convention

The schemas and the conformance checker of the run convention
(`docs/plan-run-convention.md`): how a scientific code declares its
parameters, records a run, logs events, indexes outputs and takes steering, as
plain files any tool can read. The schemas in `schemas/` are the contract
(JSON Schema draft 2020-12); the Rust crate embeds them (`run_convention::schemas`).

## Running the checker

```sh
cargo run -p run-convention --bin run-check -- <run-dir> [<run-dir>...]
cargo run -p run-convention --bin run-check -- --print-key <run-dir>   # the key run.json should hold
```

Each finding prints as `file: [rule] message`. Exit status is 0 when every
directory conforms, 1 on any finding, 2 on bad usage. Rules: `parse`, `schema`,
`exit-band`, `key`, `seq`, `output-missing`, `output-index`, `path`,
`missing-file`, `control`. The checker validates `run.json`, `params.json`,
every line of `events.jsonl`, `outputs.json` and the files under `control/`;
checks `key == sha256(canonical({problem, params-values}))` (RFC 8785, values
only); checks `seq` is 1..n gap-free and ignores a trailing partial line;
checks every indexed output exists inside the run and that every `output`
event is indexed; and enforces the exit bands. As a library: `check_dir`,
`canonical_json`, `key_of`, `values_of`, `check_parameter_schema`.

## The schemas

- `run.schema.json` — `run.json` (R-D4): `convention: "run@1"`, a UUID `id`, the
  `key`, `code`, `problem`, `host`, `lineage`, and the `status` block with its
  state and labelled exit (bands 0; 100-499). Terminal states require
  `ended_at` and `exit`; `key` is required unless the record is an import.
- `params.schema.json` — `params.json` (R-D3): one entry per parameter,
  `{value, source}`, names namespaced `component.name`.
- `event.schema.json` — one line of `events.jsonl` (R-D6): `seq`, `t`, and a
  `kind` from the fixed list, each kind with its required fields.
- `outputs.schema.json` — `outputs.json` (R-D7): `{"outputs": [...]}`, each
  with `path`, `kind`, and optional format, step, time, sha256, bytes, schema.
- `sweep.schema.json` — `sweep.json` (R-D10): base values, the varied
  parameters (explicit values or a generator with its seed), the runs spawned.
- `control.schema.json` — a request in `control/` (R-D8): set, checkpoint,
  stop, pause, resume, step, until.
- `parameter-schema.schema.json` — a meta-schema for the schema a code emits
  on `--schema` (R-D2): standard keywords plus the closed `x-run` vocabulary
  (unit, group, mutable, env, advanced, requires, open, and top-level metrics
  and groups).

Where the plan is silent, the choice is written in the schema's `description`.

## Fixtures

`fixtures/good` conforms; `gapped-seq`, `bad-key`, `missing-output` and
`bad-exit-band` each break one rule. `tests/conformance.rs` asserts each verdict.
