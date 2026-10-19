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

## Importing old run folders (`run-import`, R-D11)

```sh
cargo run -p run-convention --bin run-import -- <folder>... [--dry-run] [--force]
```

The best-effort importer for the run folders that exist before the
convention. It writes `run.json`, `params.json`, `outputs.json`, an
`events.jsonl` (the checker requires one for any started run; the importer
writes the `started` and `finished` events its facts support) and, when there
is anything to seed it with, `notes.json` **beside** what is there. Nothing is
moved, renamed or edited. An existing `run.json` (or `params.json`,
`outputs.json`, `events.jsonl`) makes the folder `skipped`; `--force`
overwrites. `--dry-run` prints what it would write (and, for one folder, the
`run.json`) and hashes nothing. Every record it writes passes `check_dir`
(asserted in `tests/import.rs`). Library: `run_convention::import`
(`import_dir`, `plan_import`, `detect`).

Common to all styles: `id` is a UUID v5 of the folder name (re-import is
stable; the same folder name under `vespa-runs/` and `vespa-bulk/` is one
run); `name` is the folder name; `provenance: "imported"` with
`imported: {from: [...], at}`; absent fields are omitted, never `null`.
Values are typed when plainly numeric (`"32"` -> `32`, `"007"` stays text).
Environment variables become `mhd.pmf_n`, `bam.ngrid`, `cic.zstart` (family
prefix -> namespace; anything else `env.<name>`); `metadata.toml` keys become
`meta.<key>` (or `<section>.<key>`). Each carries `source: "imported:<file>"`.
Output paths and `JULIA_*` thread settings are the launching environment, not
parameters: they go to `host.env` and stay out of the key.

| Style | Detected by | Yields |
|---|---|---|
| vrun manifest | `manifest.toml` with `[run]`/`[provenance]`/`[config.env]` | code (7-char `vespa_sha` kept as `code.version` with `commit_short: true`, never padded to a `commit`), `dirty`, `julia` (omitted if `unknown`), `host.hostname`, `host.accelerator` (gpu), `status` from `[run] status`, `ended_at` = `date_utc + wall_sec`, `[config.env]` as params, `tag` as a tag. Two hand-written variants (`[suite]`, and flat `id/machine/vespa_commit` with `[run]` settings) are read too |
| command.sh | `env KEY=value ... julia ... driver.jl` | every `MHD_*` value as a param, `host.argv`, `julia` from the binary path, problem name = driver stem (`pmf_dm_lattice_bench`) |
| metadata.toml | flat `key = value` (duplicate keys tolerated, last wins, reported in `imported.warnings`) | `machine` -> hostname, `created_utc` -> `started_at`, `vespa_sha` -> code, `campaign` -> tag, `note` -> notes, the rest as `meta.*` params |

A folder may have several; precedence is manifest > command.sh > metadata.toml
and the lower ones contribute what the higher ones lack. Combined with
`NOTES.md` (seeds `notes.json`) and `run.log`.

**The key.** `params.key` is written only when the driver's known parameter
family is present: a run whose command/config/folder names the PMF driver
(`pmf`, `mhd`, `pmf_dm_lattice`) must have `MHD_*` values, a `bamr` run
`BAM_*`, a `cicass` run `CIC_*`. Otherwise `run.json` has no `key` and says
why in `imported.key_withheld`. `problem.params` repeats the recovered values
under their original names (`MHD_PMF_N`, `n`), as the brief asks; `params.json`
has them namespaced with their sources, and the key hashes that.

**Status.** From the manifest's `status`; else a `===== EXIT n =====` sentinel
in `run.log` (0 -> done/ok; n -> failed, `400+n`); else a Julia `ERROR:` or
`Stacktrace:` (failed, banded: Metal unavailable 110, NaN/Inf 300, `must be`
200, else 400); else a `COMPLETE` line (done/ok); else **done with
`exit: {code: 0, label: "unknown"}` and a message saying the outcome was not
recorded**. `ended_at` is `started_at + wall_sec`, else the mtime of
`run.log` (named in `imported.ended_at_source`).

**Outputs.** Files present (depth 3, hidden files and symlinks skipped, at most
2000) with kind guessed from the name (`ckpt`/`.jls` checkpoint, `.bin`/`.h5`
snapshot, `pk*.tsv` spectrum, other `.tsv` series, images, `.log`), `bytes`
always, `sha256` only under 64 MiB (larger: a `note`), and `fields.toml` as
`schema` of the `.bin` files beside it (`format: "bin+toml"`).

**What is lost.** vrun manifests captured only `CIC_*`/`BAM_*`/`BACKEND`, never
the `MHD_*` parameters, so a PMF run known only by manifest or `metadata.toml`
has no key, and `metadata.toml` holds derived physics (`kcut`, `omega_r`, ...)
rather than what the code read, so those runs are reachable but not
deduplicable. Defaults that were never set in the environment are not recorded
by anything. Git hashes are 7 characters. Many runs have no recorded outcome.
`diff_sha256`, `lock_sha256`, `lineage` and `host.os/cpu` are absent: nothing
captured them.

**Real tree** (`cargo test -p run-convention --test import -- --ignored
--nocapture`, read-only dry run of `/Users/tabel/Projects/vespa-runs/runs`,
2026-10-19): 513 folders; 120 unrecognised (a `README.md`/`summary.tsv` and no
manifest, `command.sh` or `metadata.toml`; nothing to import); command 166
(166 with a key), manifest 5 (1 with a key; 3 are CIC runs missing `CIC_*`, 1
has no identifiable driver), metadata-only 222 (0 with a key: 136 name the PMF
driver but never recorded `MHD_*`, 86 name no driver). 167 keyed in all.

**Follow-up.** titan's `setup_meta_*.jls` are Julia `Serialization` files and
are out of scope for this Rust importer; a Julia-side importer (reading them
with `Serialization.deserialize` and writing the same records) is the
remaining piece of R-D11.
