# scripts

**`new-app.sh <name> --lib <crate> [--lib-path <path>]`** scaffolds a bespoke
app over a Rust library (docs/plan-koine-host.md H-D6; the guide is
[docs/capability.md](../docs/capability.md)). It writes a cargo workspace
`<name>/` in the current directory: a service crate (`dto.rs`, `service.rs`
with one `echo` verb whose `effects` are filled and a list of the library's
public functions to turn into verbs, `selftest.rs` running every
`#[impress_example]` through the inventory), a host crate that links it, a
`Dioxus.toml` for the page proxy, a copy of `schema-refs.json` the macros are
pointed at, and a README that is the checklist. koine is a git dependency at
`--koine-rev` (default: this checkout's HEAD, which must be pushed) or a path
dependency with `--koine-path <checkout>`. It refuses a `<name>/` that exists
and writes nothing in that case.

**`check-verb-coverage.sh`** answers "did I expose everything"
(docs/plan-koine-host.md H-D7). Without arguments it is a census of this
repository's `impress_service_impl!` blocks against `docs/verb-coverage.md`.
With `--lib <crate-dir>` it lists the library's public functions and types;
with `--service <service-crate-dir>` as well it matches each to a verb or to a
reasoned `[[exclude]]` line in `verb-exclusions.toml`, and exits 1 on any item
that is neither. Read-only; needs bash and python3. `scripts/tests/coverage/run.sh`
runs it over fixtures.

**`tests/e2e/run.sh`** is the proof of docs/plan-koine-host.md K7: it writes a
scratch library (`mini-stats`), scaffolds `stats-app` over it, makes the edits
an agent makes (two verbs, their DTOs and selftest), checks coverage, runs
`cargo test`, starts the app's host and drives it with curl (a verb, a verb's
generated surface created, rendered and dispatched, a restart, MCP), and, when
`dx` is installed, builds `koine-web` and checks `/ui/`. PASS/FAIL per
assertion, non-zero exit on any FAIL, scratch directory removed. Compiled
dependencies are kept in `target/e2e-app`, so the first run is the long one.
Needs bash, cargo, curl, jq and python3.
