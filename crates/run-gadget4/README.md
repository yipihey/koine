# run-gadget4

The GADGET-4 adapter of the run convention (`docs/plan-run-convention.md`,
R-D9.1): GADGET-4 does not know the convention, and a run launched through
this crate is a directory `run-check` cannot tell from a native run.

```sh
gadget4-adapt schema <startup-echo|parameters-usedvalues|param.txt> [-o schema.json]
gadget4-adapt render <params.json> [--schema schema.json] [-o param.txt]
gadget4-adapt launch <run-dir> --params params.json [--schema schema.json] [--mpi N]
                     [--name NAME] [--problem NAME] [--poll-ms N] [--progress-every N]
                     -- <Gadget4 binary> [args after param.txt, e.g. 0]
gadget4-adapt adopt  <gadget4-output-dir> [--name NAME] [--problem NAME] [--log stdout.log]
gadget4-adapt --schema
gadget4-adapt run --dir <run-dir> --params <koine-input.json> [--from <ckpt>] [--problem <json>]
                  [--schema schema.json] [--poll-ms N] [-- <Gadget4 binary> [--mpi N] [args]]
```

Exit status of the tool: 0 ok (or suspended); 1 the run failed (the labelled
exit is on stderr); 2 usage; 3 input refused before anything was written.

## What was recorded, what was reconstructed

A real GADGET-4 was on this machine (`~/Projects/gadget4`, commit `6e4f112`,
the EnzoNG.jl `Config.capi.sh` build, `Gadget4-capi`, OpenMPI). Every fixture
that looks like GADGET-4 output is **recorded from a real run** of it
(DM-L50-N128 parameters, 16^3 particles, `a = 0.1 -> 0.11`, 33 sync points,
one MPI rank), not reconstructed:

- `fixtures/startup-echo.txt` -- the head of the real stdout (banner, compile
  settings, the `Obtaining parameters` block);
- `fixtures/parameters-usedvalues`, `adopt-dir/info.txt` -- real files;
- `fixtures/h5dump-{config,parameters,header}.txt` -- real `h5dump -g` output;
- `fixtures/small.hdf5`, `adopt-dir/snapshot_000.hdf5` -- the real snapshot's
  `Config`, `Parameters` and `Header` groups copied out with `h5copy` (the
  particle data is dropped);
- `fixtures/param.real.txt` -- the parameter file that run used.

Stand-ins, marked as such: `fixtures/fake-gadget4.sh` (a shell script that
writes the shapes above and honours `stop`/`restart` files) and
`adopt-dir/restartfiles/restart.0` (a text stub, not a real restart file).
The parameter table's **descriptions and units are written by hand** from
`documentation/05_parameterfile.md`; its **names, types and mutability are
read from the code** (`add_param` in `src/data/allvars.cc`).

## The schema (`schema`)

`data/parameters.tsv` has one row per `add_param`: 79 rows (the per-class
softening tags as three `#` patterns), 42 `never` and 37 `restart`. The
harvester reads a startup echo, a `parameters-usedvalues` or a `param.txt`
(the same first-two-tokens rule as `parameters::read_parameter_file`), types
each tag from the table (the echo prints `Hubble 100`, which the code reads as
a double), and falls back to inference for a tag the table does not know
(group `other`, type from the value, said so in the schema's description). The
result passes `run_convention::check_parameter_schema`. From the recorded run:
60 parameters (this build's set -- some tags exist only under `#ifdef`),
30 `restart` and 30 `never`, none `always`.

Names are `group.Tag` (the convention requires a namespace): the groups follow
the manual's sections (`io`, `restart`, `memory`, `time`, `cosmology`, `units`,
`gravity`, `integration`, `domain`, `output`, `sph`, `softening`, `subfind`,
`ngenic`, `lightcone`, `cooling_sf`, `special`). The group is dropped when
rendering.

**Mutability is GADGET-4's own flag**: `PARAM_FIXED` is `never`,
`PARAM_CHANGEABLE` is `restart`. This is more permissive than "only the
cadence and limits", because the code does accept e.g. a new `ErrTolTheta` or
`CourantFac` on `RestartFlag 1`. `TimeMax` is `FIXED` in the table with the
comment "can be changed nevertheless through special function", and the manual
says a restart may extend it, so it is `restart`. Nothing is `always`.

Units: GADGET-4's lengths, masses and velocities are in code units defined by
the three `units.*` parameters, so those quantities carry `code_length`,
`code_mass`, `code_velocity`, `code_time` (defined in the schema's
description); physical ones carry `cm`, `g`, `cm/s`, `s`, `K`, `MBy`.

`required` lists every tag: GADGET-4 refuses a parameter file that misses or
repeats one, so the schema says so.

## The renderer (`render`)

Validates, then writes `Tag value` lines in table order. Unknown names,
a name in the wrong group, a type mismatch, an enum/range violation, a string
GADGET-4 would read wrongly (empty, or containing whitespace: it reads only the
second token) and a missing required tag are all errors with a `2xx` code
(201 unknown-parameter, 202 type-mismatch, 203 missing-parameter, 204
bad-value, 205 output-dir-outside-run, 206 bad-schema), never warnings; every
problem is reported, not the first. Floats print so that `atof` reads the
same number (`3.085678e24`, never twenty-five digits).

## The launcher (`launch`)

Creates `<run-dir>` (refuses one that has a `run.json`), writes `param.txt`,
`params.json` (each value with its `source`: the input's own, else
`file:<name>`), `run.json` (`created`, the key of R-D4.3), spawns
`[mpirun -np N] <binary> param.txt [args]` in it with stdout/stderr to
`stdout.log`, and once the banner appears writes the code block (version,
commit, compile settings from the startup echo) and `running`. Then, every
`--poll-ms`:

| GADGET-4 | convention |
|---|---|
| `<OutputDir>/info.txt` `Sync-Point` lines | `progress` (step, `a`, `Systemstep` as `dt`, redshift, `fraction` in `ln a`) and a `metric` (`wall_s`, `memory_largest_mb` from `memory.txt`), every `--progress-every` points |
| `snapshot_NNN.hdf5` (size and mtime stable for one poll) | `output` event + `outputs.json` entry, kind `snapshot` |
| `restartfiles/restart.N` (same) | `checkpoint` event (sha256) + `checkpoint` entry |
| control `stop` | `touch <OutputDir>/stop` |
| control `checkpoint` | `touch <OutputDir>/restart` (GADGET-4 writes restart files and carries on); the next `checkpoint` event names the request |
| control `set` | refused with a reason; for a `restart`-class parameter it is *recorded* in `restart-pending.json` and moved to `control/done/` |
| control `pause`/`resume`/`step`/`until` | refused: "GADGET-4 has no `x`" |

The stop file goes in the **output directory**, not the working directory
(`check_for_interruption_of_run` builds `"%sstop"` from `All.OutputDir`).

Ending, from the log first and the status second (GADGET-4 exits 0 on a
parameter error, and `Terminate` is `MPI_Abort`): 0 `ok` (done); 110
`walltime` (`TimeLimitCPU`, suspended); 120 `out-of-memory`; 130 `stopped`
(suspended); 140 `mpi-failure`; 150 `io-failure`; 160 `spawn-failed`; 201
`invalid-parameters`; 310 `nan-in-state`; 320 `timestep-collapse`; 400
`terminated`; 410 `nonzero-exit`. A graceful stop is state `suspended` with a
`1xx` exit; failures are `failed`, with `invalidates_reuse` for 2xx-4xx
except the environmental ones.

After the run: `parameters-usedvalues` is compared with `params.json` and the
first snapshot's `Parameters` group likewise (`h5dump`), each difference a
`warning` event; the snapshot's `Config` group becomes `code.build` (an object)
and `code.build_flags` (`["PERIODIC", "PMGRID=256"]`), cross-checked against
the startup echo's list. Diagnostics, the log and the parameter files are
indexed as outputs.

**Deliberate deviations from the brief**

- `params.json` is **not rewritten** with the used values. R-D3/R-D5 freeze it
  once running and the key hashes it; rewriting would change the key. The used
  values are compared and any difference is a warning. (`adopt`, which has no
  other source, does write them: source `usedvalues:parameters-usedvalues`.
  The convention's source pattern is `[a-z]+:.+`, which `used-values` fails.)
- The `code.build` block is written at the end if the startup echo did not
  carry it -- the one late write to `run.json`, to a block that is not hashed.
- HDF5 is read with **`h5dump -g` as a child process** (override with
  `$H5DUMP`), not the `hdf5` crate, which needs libhdf5 at build time and this
  workspace has none. Missing `h5dump` degrades to a `warning` event.
  `h5dump` prints with `%g`, so snapshot/used-value comparisons use a relative
  tolerance of 1e-5 (six significant digits).

## Adopt

`adopt <output-dir>` reads `parameters-usedvalues` (all of the run's tags, so a
key is possible) into `params.json`, `info.txt` into `progress` events,
snapshots and restart files into `output`/`checkpoint` events and
`outputs.json`, and writes `run.json` with `provenance: imported`, beside the
existing files. Event times are interpolated between the first and last file
mtime (each such event carries `t_source: "interpolated"`). Without the run's
stdout the ending is *inferred* (last sync point at `TimeMax` is `ok`; a `cont`
file is `walltime`; restart files without completion is `stopped`; else
`incomplete`) and the exit message says so; `--log` classifies the real log.

## Through koine's `run-service` (R-D13): `--schema`, `run`, `--from`

A GADGET-4 run is started, watched and steered through the generic
`run-service_*` verbs with no wrapper: `gadget4-adapt` is a code binary in the
sense of `run-service`'s `CodeProvider`. `run_gadget4::provider(adapt, gadget4,
mpi)` returns the arguments of `run-service_register-code` (this crate does not
depend on `run-service`, and `koine-host` does not link GADGET-4; a host
registers it with one call, a person pastes the JSON):

```json
{"name": "GADGET-4", "version": "4.0+6e4f112",
 "launch": {"argv": ["/path/gadget4-adapt", "run"], "params": "file",
            "params_flag": "--params", "dir": "flag", "dir_name": "--dir",
            "problem_flag": "--problem", "resume_flag": "--from",
            "env": {"GADGET4_BIN": "/path/Gadget4", "GADGET4_MPI": "1"}},
 "default_problem": {"name": "gadget4", "version": "4.0+6e4f112"}}
```

- `gadget4-adapt run --schema` (and `gadget4-adapt --schema`) prints the schema
  harvested from the committed `data/parameters.tsv` and the recorded echo
  (`fixtures/startup-echo.txt`); no GADGET-4 is needed. A different build has
  different tags: set `GADGET4_ECHO` to its startup echo or
  `parameters-usedvalues`.
- **The binary and the rank count are environment, not arguments.** run-service
  runs `argv + [--dir D, --params P, --problem J, --from C]` and appends nothing
  else, so there is no place after a `--` for the GADGET-4 executable.
  `launch.env` is the contract's slot for what is fixed per host: `GADGET4_BIN`,
  `GADGET4_MPI` (run as `mpirun -np N`), `GADGET4_ARGS` (extra arguments after
  the parameter file). `-- <bin> [--mpi N]` after the flags still works by hand
  and wins.
- `--params` is the `koine-input.json` run-service writes
  (`{name: {value, source}}`, every tag, the schema's defaults under the
  caller's values). `--problem` is the problem object, used verbatim (the key
  run-service predicted is computed over it, so the adapter adds nothing to it).
- Inputs the code reads from the working directory (`PowerSpectrumFile`,
  `OutputListFilename`, `InitCondFile`) must exist before the run directory
  does, so name them by absolute path: the launch has no staging hook.

### Resume (`--from`)

`run-service_resume {id[, from, params]}` passes the checkpoint (the last
`checkpoint` event's `restartfiles/restart.N`, or a snapshot named in `from`).
**`RestartFlag` is a command-line argument of GADGET-4, never a tag in
`param.txt`** (`src/main/main.cc`: `Gadget4 <param.txt> [<RestartFlag>
[<snapnum>]]`; an unknown tag is a fatal error), so the child's `param.txt` has
no `RestartFlag` line and its `host.argv` is `[..., "param.txt", "1"]`:

| `--from` | flag | staged into the child's `OutputDir` |
|---|---|---|
| `restartfiles/restart.N`, or the `restartfiles` directory | `1` (`RST_RESUME`) | every `restart.N`, copied (the code overwrites them in place) |
| a snapshot `<base>_NNN.hdf5` | `2` (`RST_STARTFROMSNAP`) then `NNN` | the snapshot, copied |

The parent is the nearest ancestor run directory of the checkpoint, and the
child asserts it: `lineage.parent = {run, relation: "resume", checkpoint:
{path, sha256}}`. Parameters are the parent's `params.json`, then its
`restart-pending.json` (what `set` recorded), then the values supplied; a
difference from the parent is a `changed{param, from, to, by: "restart", step}`
event after `started`. A difference in a `never` parameter is refused before
anything is written: exit status 3, `207 [never-changed]` on stderr naming the
parameter (run-service reports it as `process-failed` with that text). The restart
file count must equal the rank count (`208 [bad-restart-source]`). Verified
against the real code: it also refuses to *reduce* `TimeMax` on a restart
(`readjust_timebase`), which the schema cannot express.

## Not built

- Multi-rank restart-file bookkeeping beyond one event per `restart.N`.

## Verified live, and not

Run through the adapter against the real `Gadget4-capi` with `--mpi 1`
(`mpirun` OpenMPI): a full run (33 progress, 2 snapshots, 1 restart file, no
warnings: the used values and the snapshot's `Parameters` agree with
`params.json`, `run-check` clean); a `stop` request (`touch <OutputDir>/stop`,
GADGET-4 wrote restart files and ended: `suspended`, exit 130 `stopped`); a
`checkpoint` request (`touch <OutputDir>/restart`: restart files written, run
continued, the `checkpoint` event names the request).

Through `run-service` (the `#[ignore]` test `live_gadget4_start_stop_and_resume_through_run_service`
in `tests/provider.rs`, ~60 s): `start` -> `stop` (`suspended`, 130) -> `resume`
with `time.TimeMax` 0.11 -> 0.112 (`RestartFlag 1`, the real code read the
staged `restart.0`; `done`, `check_dir` clean, `lineage.parent` and a
`changed{by: "restart"}` event).

Read from the source, never seen live: the `cont` file and `reaching
time-limit` ending (110); the log text of the NaN, timestep-collapse and I/O
`Terminate` calls (320/310/150 match substrings of the `Terminate(...)`
strings in the source; only the out-of-memory one, 120, was seen, by accident
on a first run with `MaxMemSize 200`); multi-rank runs (rank-numbered restart
files, `bak-restart.N`); `RestartFlag` 2 against the real code (the fake binary covers it); `cpu.csv` (its header has one more column
than its rows, so no CPU metric is taken from it).
