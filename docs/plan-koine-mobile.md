# Koine mobile: an app on an iPhone, with its agent inside

`plan-koine-web.md` D6 said "mobile: not now (the Apple apps are native)".
The suite's own apps are native, but an agent that writes a capability
with koine and wants it in a pocket has nothing: the host is a binary on a
laptop, the page is a browser tab, and `--window` opens a desktop WebView.
This plan makes `cargo`, `dx` and a Mac enough to put a koine app on an
iPhone, with the host inside the app, so the agent that drives it can be
inside too.

| Question | Decision |
|---|---|
| what an iOS app is | M-D1 one binary: the host in-process, the page in a WebView |
| how the page finds its host | M-D2 a host link, not `window.location` |
| where the data lives | M-D3 the app's Documents, a `dir:` store |
| who the agent is | M-D4 loopback for anything on the phone; a network bearer for a Mac |
| how it is built and proven | M-D5 `dx build --platform ios`, a macOS lane that drives the simulator |
| the scaffold | M-D6 `new-app.sh --mobile` |
| onto a real phone | M-D8 TestFlight from a tag |
| not now | M-D7 |

## Decisions

### M-D1. One binary: the host in-process, the page in a WebView

A koine iOS app is one process. At launch it starts `koine_host::serve`
on a background tokio runtime, bound to loopback on an ephemeral port, with
the app's service linked exactly as `<name>-host` links it; then it hands
the main thread to Dioxus `mobile` (`dioxus-desktop` over WKWebView), which
performs the same `koine-web` page the browser performs. Nothing in the
page or the host is mobile-specific: the page's transport stays HTTP to
`/api/*`, the long-poll wait loop stays, the surface and layout routes
stay, so a surface an agent wrote for the laptop draws on the phone
unchanged. `koine_host::embed::start(config) -> Embedded` is the seam: it
owns the runtime thread, answers the URL and the token once the socket is
bound, and shuts the host down when dropped. `koine-window` is not used:
Dioxus is the window.

### M-D2. The page learns its host from a link, not from `window.location`

`koine-web` becomes a library with a thin binary. `koine_web::HostLink
{origin, token}` is installed once before `koine_web::launch()`; the
page's `api::origin()` answers the link when one is installed and the
window's origin otherwise, and `token::load()` answers the link's token
before it looks at the meta tag or `sessionStorage`. The browser build is
unchanged (no link is ever installed there); a native build is read-write
from its first paint, because the token was handed over in-process, which
D8's "never guess" allows: the host and the page are one program. A new
route, `/host`, shows the origin, the bearer (with a copy button) and the
app id, so a person can hand them to another app on the phone.

### M-D3. Data lives in the app's Documents, as a `dir:` store

`koine_host::embed::sandbox_config(app_id)` is `Config::new("dir:<home>/
Documents/<app_id>/store", "<home>/Documents/<app_id>")`, `$HOME` being the
app's sandbox on iOS: the workspace (settings, secrets, the token file) and
the store are plain files the Files app can show when the bundle declares
`UIFileSharingEnabled` and `LSSupportsOpeningDocumentsInPlace`, and a
backup, an AirDrop or an `rsync` from a Mac carries them whole
(plan-koine-store D2 is why the dir store exists). The memory store is the
`--check` of a phone: `sandbox_config` takes an override for tests.

### M-D4. The agent on the phone

Three callers reach the app, all through the same `/api/*` and `/mcp`
the laptop host serves:

1. **The person**, through the page.
2. **Anything on the phone**, over loopback with the bearer: iOS lets one
   app's socket on `127.0.0.1` be dialled by another, so an agent app that
   speaks MCP over HTTP drives this app with the URL and bearer `/host`
   shows (or the token file under Documents). This is the "agent running
   on an iPhone" case, and it needs nothing new: `koine-mcp-host` already
   mounts `/mcp`.
3. **A Mac on the same network**, when the person turns on
   `allow_network` (the host's existing `--allow-network` + bearer), so
   Claude Code on the laptop drives the phone the way Tier B drives an
   app. Off by default; the toggle is a host verb (`koine-host-service_
   network {allow}`), not a page switch, so it is audited like every other
   mutation.

iOS suspends a backgrounded app: the host stops answering while the app
is not on screen and resumes with it. A wait loop that times out while
suspended is the page's normal "render again" path; a Mac driving the
phone must keep the app in front. This is recorded, not worked around.

### M-D5. Built by `dx`, proven on a macOS lane

`dx build --platform ios` (the simulator by default; `--device` for a
phone) bundles the binary with the `Dioxus.toml` `[bundle]` identifier.
The build needs a Mac with Xcode; the Linux lane keeps `cargo test` and the
wasm check and never tries. A new CI job on `macos-15` installs the pinned
`dioxus-cli`, builds the demo app (`crates/koine-demo-mobile`, the
scaffold's template: `surface-demo-service` + the host + the page) for
the simulator, boots a simulator, installs and launches it, reads the
token file from the app's data container (`xcrun simctl
get_app_container … data`), and drives the host from the Mac through the
simulator's shared loopback: `GET /api/health`, then the five-verb loop
(`surface_create` → `surface_show` → `surface_render` → `surface_dispatch`
→ `surface_wait`) with the bearer, so "an agent drives a koine app on
iOS" is a green check, not a sentence. The lane runs on pull requests and
main; a push to a `mobile*` branch runs it too, so the lane can be watched
before main moves.

### M-D6. The scaffold

`scripts/new-app.sh … --mobile` adds `crates/<name>-mobile/` (a `main.rs`
of six lines: link the service, `embed::start(sandbox_config(name))`,
`HostLink`, `launch()`), its `Dioxus.toml` with a bundle identifier and the
two Files-app keys, and a README section "On an iPhone": `dx serve
--platform ios` from that crate, where to find the token, how an agent
drives it. The e2e proof checks the scaffold writes the crate and that the
workspace resolves it; compiling it is the macOS lane's.

### M-D8. Onto a real phone: TestFlight from a tag

A phone runs only a signed app, and the only phone-only loop (no Mac
beside the phone) is a signed build delivered by TestFlight. One
reusable workflow, `release-ios.yml`, takes a crate, a bundle id and a
version: `fastlane match` makes a distribution certificate and an App
Store profile exist for the bundle id through the App Store Connect API
key, keeping them encrypted on the repository's `signing` branch so every
later run reuses them (the team's certificate count stays at one, no Mac
holds the key); `dx build --device` signs with that identity;
`scripts/release/sign-ipa.sh` stamps the version and the run number as the
build number, re-seals and packs; `fastlane pilot` uploads; the tag's
GitHub Release gets the `.ipa`. The App Store Connect API cannot create an
App record, so that one form, and the bundle id's registration, stay with
the person, once per app. Five repository secrets, set once for the team:
the key's id, issuer and `.p8`, the team id, and the passphrase for the
signing store. External testers, review and the App Store are out of
scope: internal TestFlight needs neither.

### M-D7. Not in this plan

- **Android.** `dx build --platform android` takes the same crate; nothing
  here is iOS-only except M-D3's Files keys and M-D5's lane. Not proven.
- **An LLM client in koine.** The agent on the phone is whatever speaks
  MCP to loopback; koine ships no model client (ADR-0029's providers are
  impress-apps').
- **A relay** that lets an agent off the network reach the phone.
- **App Store review and external testers.** M-D8 stops at internal
  TestFlight; the store is the person's.
- **A native (SwiftUI) renderer for a standalone app.** The suite's
  `ImpressSurface` draws surfaces natively inside impress-apps' own iOS
  apps already; a standalone koine app draws the web page.

## Work packages

| # | Package | Where |
|---|---|---|
| I1 | `koine-web` as a library: `HostLink`, `launch`, the `mobile` feature, `/host` | koine-web |
| I2 | `koine_host::embed` (`start`, `Embedded`, `sandbox_config`) and `koine-host-service_network` | koine-host |
| I3 | `crates/koine-demo-mobile`; `new-app.sh --mobile`; the e2e scaffold check | koine-demo-mobile, scripts |
| I4 | the macOS lane; `docs/mobile.md`; README and plan status | .github, docs |
| I5 | gate, main | orchestrator |
| R1 | `release-ios.yml` (M-D8), `fastlane/Fastfile`, `scripts/release/sign-ipa.sh`, the demo caller, docs | .github, fastlane, scripts, docs |

## Status

Planned 2026-10-12. I1 to I4 landed on branch `mobile` the same day and are
not yet committed to main (I5). The guide for an agent is
[mobile.md](mobile.md).

**Landed and verified on Linux** (`cargo test --workspace`, clippy, fmt,
`scripts/tests/e2e/run.sh` 75/75 including the new section 8):

- I1 `koine-web` is a library with a thin binary: `HostLink {origin, token,
  app_id}`, `install_link`, `host_link`, `launch`, the non-default `desktop`
  and `mobile` features, and the `/host` route. `launch` prints a hint and
  returns on a native build with neither feature, which is what lets the
  mobile crate compile everywhere.
- I2 `koine_host::embed`: `start`, `Embedded {url, token, token_path, addr,
  shutdown}`, `sandbox_config`, `sandbox_config_in`.
- I3 `crates/koine-demo-mobile` (workspace member; `src/main.rs`, `Dioxus.toml`,
  `README.md`), `new-app.sh --mobile` (the crate, its `Dioxus.toml`, the
  workspace member and dependency lines, an "On an iPhone" README section; the
  output without the flag is byte-identical to before), and the e2e's section 8
  (three files exist, `cargo metadata` lists `<name>-mobile`, `cargo check -p
  <name>-mobile` passes).
- I4 `docs/mobile.md`, the root README paragraph, `scripts/tests/ios/run.sh`
  (`bash -n` only), and the `ios` job in `.github/workflows/ci.yml` (parsed as
  YAML only).

**Where the build differs from the text above**

- M-D1/M-D5: the demo crate's `main` is not `cfg`-gated. `mobile` is a
  non-default feature (`mobile = ["koine-web/mobile", "dioxus/mobile"]`) and
  `koine_web::launch` is the only thing it changes, so `cargo test
  --workspace` type-checks the whole `main` (every name used from `koine-host`
  and `koine-web`), not an empty one. The crate names `dioxus` as a direct
  dependency (no features of its own) so `dx` recognises the crate.
- M-D4: the network door is `GET/POST /api/host/network` (loopback and the
  token only), documented in plan-koine-host.md, not a
  `koine-host-service_network` verb. It is therefore not audited like a verb
  call, and nothing on the page flips it yet.
- M-D5: the host has no `GET /api/health`; the lane calls `/api/status`.
- M-D3: the Files-app keys are supported by dx 0.7.10 as `[ios.plist]` in
  `Dioxus.toml` (its `dx config schema`, and `build/apple.rs` in the dioxus-cli
  source); `Dioxus.toml` sets `UIFileSharingEnabled` and
  `LSSupportsOpeningDocumentsInPlace` there.

**Verified on the lane (2026-10-12, `mobile` at 076d99f): 43 of 43.** The
first run on `macos-15` answered the eight assumptions below in one pass:
the dependency tree built for `aarch64-apple-ios-sim` (504 crates, 330 s
cold); `dx build --platform ios --release --features mobile` enabled the
feature and bundled `target/dx/koine-demo-mobile/release/ios/KoineDemoMobile.app`;
the plist keys and `com.koine.demo` landed; the simulator ran the unsigned
bundle; the app's `$HOME` was its data container and the token appeared
under `Documents/koine-demo/automation/loopback-<port>.token`; the Mac
reached the app's loopback and drove `/api/status`, the histogram verb,
`/api/surface` create, render, three dispatches, state, wait, the list and
`/mcp tools/list`. Two things the run taught: `xcrun simctl
get_app_container` took eight minutes on the cold runner (the proof is
patient), and `GET /ui/` answers 503 because the page lives in the WebView,
not on the host (the browser bundle is not shipped in the app). The one
assertion that failed on the first run was the script's: a dispatch is an
agent event only through an `emit`, and the generated form has none.

**Unverified before that run: everything behind the `mobile` feature and the lane.** There was
no Xcode, no WKWebView and no `x86_64`/`aarch64-apple-ios*` toolchain, so none
of this has run. A Mac run (the first push to a `mobile*` branch) is what
confirms:

1. the dependency tree builds for `aarch64-apple-ios-sim` (`koine-host`,
   tokio, axum, `notify`, `webbrowser`, `dioxus-desktop`, wry/tao);
2. `dx build --platform ios --release --features mobile` run in
   `crates/koine-demo-mobile` enables the `mobile` feature, produces
   `target/dx/koine-demo-mobile/release/ios/KoineDemoMobile.app` (the
   directory is `<target>/dx/<bin>/release/ios/`, the name the pascal-cased
   binary), and the simulator runs it unsigned;
3. `[ios.plist]` lands in the bundle's `Info.plist` and the bundle identifier
   is `com.koine.demo`;
4. a process in the simulator has `$HOME` equal to the app's data container,
   so `sandbox_config` writes under `<container>/Documents/koine-demo/` and
   `xcrun simctl get_app_container <udid> com.koine.demo data` names that
   container;
5. the app's socket on `127.0.0.1` is reachable from the Mac, and the
   `Host: 127.0.0.1:<port>` check and the token rule behave as on Linux;
6. `xcrun simctl launch --console --terminate-running-process` relays the
   app's stderr (the script treats the console lines as informational and
   the token file as the authority);
7. the WKWebView draws the page and the page's calls to the in-process host
   succeed. They are `reqwest` calls on the native target (Rust's HTTP client,
   so neither CORS nor App Transport Security applies, and the host sends no
   CORS headers); what a Mac run checks is that dioxus-desktop's runtime
   serves them, and that `document::eval` (the DOM reads) works on iOS;
8. macos-15 is arm64, so `dx` picks `aarch64-apple-ios-sim`; the job's runtime
   (60 minutes) is enough for a cold build.

**M-D8 proven 2026-10-13.** The fifth `release-demo-ios` run on main
(build 5 of 0.1.0) uploaded to TestFlight; the four before it each
stopped one step later than the last, and every stop was a fact about
the gap between a dx bundle and what Apple's validator reads, now closed
in `scripts/release/sign-ipa.sh` and the workflow:

1. the identity `match` returns is `Apple Distribution: NAME (TEAM)`, which
   needs quoting in a shell env file;
2. a dx-built `Info.plist` has none of the `DT*` keys, so `pilot` cannot
   infer the platform (`app_platform: "ios"`) and validation refuses the
   bundle; the script writes them from the runner's Xcode and
   `MinimumOSVersion` from the binary's `LC_BUILD_VERSION`;
3. validation refuses an iOS bundle without an icon catalog
   (`CFBundleIconName`): the script compiles `crates/<crate>/icon.png`
   with `actool`;
4. dx names a `LaunchScreen` storyboard it does not ship, and an
   iPad-multitasking app must have a launch screen: an empty
   `UILaunchScreen` dict is the storyboard-free form for a floor of 14+;
5. App Store Connect takes only builds made with the iOS 26 SDK, and the
   `macos-15` image defaults to Xcode 16.4: the job selects the newest
   `Xcode_26*` on the image first.

`match` created the certificate and profile on the first run with the
API key alone and reused them on every later run (the `signing` branch
exists); no Mac touched a keychain. Apple processed build 5 within
minutes of the upload, the internal group received it, and the demo
installed from TestFlight and ran on an iPhone the same morning: the
whole path, tag to phone, with no Mac in it.
