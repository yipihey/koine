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
`--mobile` also writes `crates/<name>-mobile` (the host in-process and the
`koine-web` page in a WebView, bundle id `com.<name>.koine`; docs/mobile.md) and
an "On an iPhone" section in the README; without it the output is unchanged.

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

**`tests/ios/run.sh`** is the proof of docs/plan-koine-mobile.md M-D5: it boots an
iOS simulator, installs the app `dx build --platform ios` made from
`crates/koine-demo-mobile`, launches it, reads the loopback token from the app's
data container and drives the host inside it from the Mac (status, a verb, the
surface loop, MCP). PASS/FAIL per assertion. Needs macOS with Xcode, curl and jq;
the `ios` job of `.github/workflows/ci.yml` runs it, and so can a person.

- `release/sign-ipa.sh`: from a `dx`-built iOS `.app` to a TestFlight-ready `.ipa` (entitlements from the profile, version and build stamped, re-sealed, packed); run by `.github/workflows/release-ios.yml`.

## `tests/gestures/run.sh`

Every gesture the tour offers, fired with a mouse and then with a finger
against a fresh demo host, each judged by what the host answers
(docs/plan-koine-affordances.md A-D5). `KOINE_SKIP_BUILD=1` reuses the page
bundle, `GESTURE_POINTERS=touch` runs one pointer, `GESTURE_DEBUG=1` prints
every event the page posts. CI runs it as the `gestures` job.
