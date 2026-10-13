# A koine app on an iPhone

This is the guide for an agent that was asked to put a koine app in a pocket.
The design and its reasons are [plan-koine-mobile.md](plan-koine-mobile.md); this
page is what to run and what to expect. Everything below that needs Xcode was
written on a machine without it and has not been run: the status of each claim
is in "What is verified".

## What it is (M-D1)

One iOS process. The host (`koine_host::embed::start`) runs on a background
tokio runtime, bound to loopback on an ephemeral port, with the app's service
linked exactly as `<name>-host` links it. The main thread belongs to Dioxus
`mobile` (`dioxus-desktop` over WKWebView), which performs the same `koine-web`
page the browser performs. The page's transport is still HTTP to `/api/*`, so a
surface an agent wrote for the laptop draws on the phone unchanged.

The program is `crates/koine-demo-mobile/src/main.rs`, a dozen lines: link the
verbs, `embed::start(embed::sandbox_config(app_id))`, `koine_web::install_link(
HostLink { origin, token, app_id })`, `koine_web::launch()`, `host.shutdown()`.
For your own app, `scripts/new-app.sh <name> --lib <crate> --mobile` writes the
same thing as `crates/<name>-mobile` (service linked is `<name>-service`, app id
`<name>`, bundle id `com.<name>.koine`) and an "On an iPhone" section in the
generated README.

`mobile` is a feature of that crate, off by default. `cargo test --workspace`
and `cargo clippy --workspace` therefore compile all of `main` with no window
(`koine_web::launch` prints a hint and returns), which type-checks every name it
uses from `koine-host` and `koine-web` on Linux. `dx` turns the feature on.

## Build and run

A Mac with Xcode, and:

```bash
rustup target add aarch64-apple-ios-sim aarch64-apple-ios
cargo install dioxus-cli --version 0.7.10 --locked

cd crates/koine-demo-mobile            # or crates/<name>-mobile
dx serve --platform ios                # the simulator (boots one); hot reload
dx build --platform ios --release      # a simulator .app, no device
dx build --platform ios --device --release   # a phone (aarch64-apple-ios)
```

The bundle is `target/dx/koine-demo-mobile/release/ios/KoineDemoMobile.app` (dx
names it by pascal-casing the binary). The bundle identifier is the `Dioxus.toml`
`[bundle] identifier` (`com.yipihey.koine`). Putting a build on a phone needs your
signing identity and provisioning profile; `dx build --help` lists `--codesign`,
`--apple-team-id` and `--apple-entitlements`. Nothing here automates signing or
the App Store (M-D7).

## Where data and the token live (M-D3)

`sandbox_config(app_id)` is `Config::new("dir:<home>/Documents/<app_id>/store",
"<home>/Documents/<app_id>")`, `$HOME` being the app's sandbox. So:

```text
Documents/<app_id>/
    store/                                  the dir: store (plan-koine-store D2)
    settings/ secrets/ ...                  the workspace
    automation/loopback-<port>.token        this launch's bearer, one line
```

The port is in the file name and changes every launch. `Dioxus.toml` carries
`[ios.plist] UIFileSharingEnabled = true` and `LSSupportsOpeningDocumentsInPlace
= true`, which dx merges into the bundle's `Info.plist` (dioxus-cli 0.7.10
supports `[ios.plist]`; its source writes a TOML `true` as `<true/>`). With both,
the Files app shows the app's `Documents/` folder, and a backup, an AirDrop or
an `rsync` from a Mac carries the store whole.

The app logs two lines to stderr at start:

```text
koine-demo-mobile: host http://127.0.0.1:49152
koine-demo-mobile: loopback token <Documents>/koine-demo/automation/loopback-49152.token
```

On the simulator, the Mac reads the file from the app's data container:

```bash
UDID=$(xcrun simctl list devices booted -j | jq -r '[.devices[][]][0].udid')
DATA=$(xcrun simctl get_app_container "$UDID" com.yipihey.koine data)
TOKEN_FILE=$(ls -t "$DATA"/Documents/koine-demo/automation/loopback-*.token | head -n 1)
TOKEN=$(cat "$TOKEN_FILE")
PORT=$(basename "$TOKEN_FILE" .token); PORT=${PORT#loopback-}
```

On a phone, the page's `/host` route shows the app id, the origin and the bearer
(masked, with a copy button) so a person can hand them to another app. The
bearer is held in memory by the page; it is never written to `sessionStorage` or
a URL.

## The three callers (M-D4)

All three reach the same `/api/*` and `/mcp` the laptop host serves. A GET needs
no token; every other method needs `Authorization: Bearer <token>`.

**1. The person**, through the page: surfaces, layout, style and host.

**2. Anything on the phone**, over loopback. iOS lets one app dial another's
`127.0.0.1` socket, so an agent app that speaks MCP over HTTP drives this app
with the URL and bearer `/host` shows (or the token file):

```bash
BASE=http://127.0.0.1:$PORT
AUTH="Authorization: Bearer $TOKEN"

curl -s  "$BASE/api/status"
curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
     -d '{"values":[1,2,3,4,5,6],"bins":3}' "$BASE/api/verb/surface-demo-service_histogram"
curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
     -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' "$BASE/mcp"
```

From a Mac, the simulator shares the host's loopback, so the same commands work
unchanged against `$BASE` above. That is what `scripts/tests/ios/run.sh` does.

The five-verb loop (docs/agent-surfaces.md in impress-apps; the host's routes
are `crates/koine-host/src/routes/surface.rs`):

```bash
SPEC=$(curl -s "$BASE/api/verb/surface-demo-service_histogram/surface" | jq -c .spec)
SID=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' -d "$SPEC" "$BASE/api/surface" | jq -r .id)
curl -s "$BASE/api/surface/$SID/render"
curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
     -d '{"widget":"arg-values","kind":"change","value":[1,2,3,4,5,6]}' "$BASE/api/surface/$SID/dispatch"
curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
     -d '{"widget":"arg-bins","kind":"change","value":3}' "$BASE/api/surface/$SID/dispatch"
curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
     -d '{"widget":"run","kind":"click"}' "$BASE/api/surface/$SID/dispatch"
curl -s "$BASE/api/surface/$SID/state" | jq -c '(.state // .) | .result.counts'     # [2,2,2]
curl -s "$BASE/api/surface/$SID/wait?after_seq=0&timeout_ms=1000"
```

**3. A Mac on the same network**, when the network door is opened. It is off by
default. The host started on loopback; `POST /api/host/network` (from loopback
only, with the loopback token) opens a second listener on `0.0.0.0` behind the
same guard, and answers the network bearer:

```bash
curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
     -d '{"allow":true}' "$BASE/api/host/network"
# {"ok":true,"allowed":true,"addr":"0.0.0.0:49152","port":49152,"bearer":"..."}
# then, from the Mac:
curl -s -H "Authorization: Bearer <network bearer>" http://<phone-ip>:49152/api/status
curl -s -X POST -d '{"allow":false}' -H "$AUTH" -H 'Content-Type: application/json' "$BASE/api/host/network"
```

A non-loopback peer presents the network bearer on every request, even a read;
the loopback token does not work from outside. `GET /api/host/network` reports
the state and never the bearer. The toggle is a route, not a verb or a page
switch, in this tree, so it is not in the audit log the way a verb call is (the
plan names a `koine-host-service_network` verb; see plan status).

## The suspension note

iOS suspends a backgrounded app. The host stops answering while the app is not on
screen and resumes with it, and a wait loop that times out while suspended is the
page's normal "render again" path. A Mac or an agent driving a phone must keep
the app in front (on the simulator, keep it launched; on a phone, keep the screen
on with the app foregrounded). This is recorded, not worked around.

## The lane and the local proof (M-D5)

`.github/workflows/ci.yml`, job `ios` (`macos-15`, 60 minutes; it runs on pull
requests, on main and on a push to a `mobile*` branch): install `dioxus-cli`
0.7.10 (cached), `rustup target add aarch64-apple-ios-sim`, `dx build --platform
ios --release --features mobile` in `crates/koine-demo-mobile`, then
`scripts/tests/ios/run.sh`.

`scripts/tests/ios/run.sh` is the same script a person with a Mac runs after
`dx build`:

```bash
scripts/tests/ios/run.sh                              # the built app, a booted or first iPhone
scripts/tests/ios/run.sh --udid <udid> --app path/to/KoineDemoMobile.app
```

It boots a simulator (and shuts it down again if it booted it), installs the
bundle, launches `com.yipihey.koine`, finds the token in the data container,
and from the Mac asserts `/api/status`, a 401 without the token, the histogram
verb, the surface loop above (create, render, dispatch, state, wait), `/mcp`
`tools/list` and the Info.plist keys. PASS/FAIL per line, non-zero on any FAIL.

The Linux job `test` is unchanged and also covers the mobile crate's type-check
(without the feature). `scripts/tests/e2e/run.sh` checks `new-app.sh --mobile`
writes the crate, that the workspace resolves it, and that `cargo check -p
<name>-mobile` passes.

## What is verified

- Compiled and tested on Linux: `koine_host::embed`, `koine-web` as a library
  (`HostLink`, `/host` page logic), the demo crate and a scaffolded `--mobile`
  crate without the `mobile` feature, `scripts/new-app.sh --mobile`.
- Not run anywhere yet: everything behind the `mobile` feature (dioxus-desktop
  on iOS, the WebView drawing the page), `dx build --platform ios`, the
  `[ios.plist]` keys landing in the bundle, `scripts/tests/ios/run.sh`, the `ios`
  job, a device. The first run of the lane is the proof; expect to fix paths.
- Assumptions a Mac run confirms are listed in plan-koine-mobile.md § Status.

## What is not done (M-D7)

- **Android.** `dx build --platform android` takes the same crate; nothing here is
  iOS-only except the Files keys and the lane. Not proven.
- **An LLM client in koine.** The agent on the phone is whatever speaks MCP to
  loopback; koine ships no model client.
- **A relay** that lets an agent off the network reach the phone.
- **Signing and the App Store.** Yours.
- **A native (SwiftUI) renderer for a standalone app.** A standalone koine app
  draws the web page; impress-apps' own iOS apps draw surfaces natively already.
- Not in this tree either: a page switch for the network door, and the door as an
  audited verb.

## Onto a phone: TestFlight from a tag

A simulator runs an unsigned bundle; an iPhone runs only a signed one, and
the signing needs an Apple Developer Program membership. With that, the
loop is phone-only after a one-time setup (docs/plan-koine-mobile.md M-D8):

**Once, for the team**

1. In App Store Connect (Users and Access, Integrations, App Store Connect
   API, Team Keys) generate a key with the **Admin** role; note its Key ID,
   the page's Issuer ID, and download the `.p8` (once only).
2. Repository secrets on this repository: `APP_STORE_CONNECT_KEY_ID`,
   `APP_STORE_CONNECT_ISSUER_ID`, `APP_STORE_CONNECT_API_KEY` (the `.p8`
   text, BEGIN and END lines included), `APPLE_TEAM_ID` (Membership details
   on developer.apple.com), and `MATCH_PASSWORD` (any long random
   passphrase: it encrypts the certificate and profile `fastlane match`
   keeps on this repository's `signing` branch, so the second run reuses
   what the first run minted and no Mac ever holds the key).
3. TestFlight installed on the phone.

**Once per app**

1. Register the bundle id (developer.apple.com, Identifiers, App IDs) and
   create the App record (App Store Connect, My Apps, New App) with that id.
2. In the app's TestFlight tab, an internal group with you in it and
   automatic distribution on.
3. A caller workflow like `.github/workflows/release-demo-ios.yml` naming
   the crate, the bundle id and a tag pattern (copy the demo's and edit the
   three).
4. `crates/<crate>/icon.png`, a 1024x1024 opaque PNG: App Store validation
   refuses an iOS bundle without an icon catalog, and `sign-ipa.sh` compiles
   this one into it with `actool`.

**Every release**

```bash
git tag demo-v0.1.1 && git push origin demo-v0.1.1   # or Actions, Run workflow
```

`release-ios.yml` then runs on a macOS runner: `fastlane ios signing`
(`fastlane/Fastfile`: `match` makes the certificate and the App Store
profile exist and installs them), `dx build --platform ios --device
--release --features mobile` signed with that identity,
`scripts/release/sign-ipa.sh` (stamps `CFBundleShortVersionString` from the
tag and `CFBundleVersion` from the run number, which TestFlight needs
unique, adds the Xcode keys validation reads and the icon catalog, re-seals
the bundle, packs the `.ipa`), `fastlane ios upload`
(uploads), and the `.ipa` lands on the tag's GitHub Release. TestFlight
notifies the phone a few minutes after Apple processes the build; tap
Install.

What this does not do: App Store review or external testers (internal
testing needs neither), Android, a build on a machine without Xcode.
