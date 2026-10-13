# koine-demo-mobile

The koine demo as an iPhone app: `docs/plan-koine-mobile.md` M-D1. One process,
the host in-process (`koine_host::embed`, loopback, an ephemeral port, a `dir:`
store under the app's Documents) and the `koine-web` page in a WebView. It links
`surface-demo-service` (`series`, `histogram`, `tick`) and the two runtimes, as
the `koine-host` binary does. It is also the template `scripts/new-app.sh
<name> --lib <lib> --mobile` writes for a bespoke app. The guide is
[`docs/mobile.md`](../../docs/mobile.md).

`src/main.rs` is the whole program: link the verbs, `embed::start(sandbox_config(..))`,
`koine_web::install_link(..)`, `koine_web::launch()`, `host.shutdown()`.

## Build and run (a Mac with Xcode)

```bash
rustup target add aarch64-apple-ios-sim aarch64-apple-ios
cargo install dioxus-cli --version 0.7.10 --locked
cd crates/koine-demo-mobile

dx serve --platform ios            # the simulator; boots one if none is running
dx build --platform ios --release  # a simulator .app, for scripts/tests/ios/run.sh
dx build --platform ios --device --release   # a phone (aarch64-apple-ios)
```

`dx` turns on this crate's `mobile` feature for the iOS platform. The `.app` is
`target/dx/koine-demo-mobile/release/ios/KoineDemoMobile.app`. Putting it on a
phone needs signing: `.github/workflows/release-demo-ios.yml` does it on a tag
(`demo-v0.1.0`) through TestFlight, with the certificate and profile made from
an App Store Connect API key (docs/mobile.md "Onto a phone").

## Without `mobile`

The feature is off by default and the crate is a workspace member, so
`cargo test --workspace` and `cargo clippy --workspace` compile all of `main`
with no window: `koine_web::launch` prints a hint and returns when no renderer
is on, and `main` then shuts the host down. That is a stronger Linux-lane check
than an empty `main` would be (every name `main` uses from `koine-host` and
`koine-web` is type-checked) and costs nothing, because `dioxus/mobile` is the
only thing the feature adds. Do not run the binary on a laptop expecting a
window: it starts a host over `~/Documents/koine-demo/` and exits. The
macOS lane (`.github/workflows/ci.yml`, job `ios`) builds with `dx`, which
enables `mobile`.

## Where the token lands

The app logs two lines to stderr at start:

```
koine-demo-mobile: host http://127.0.0.1:49152
koine-demo-mobile: loopback token <Documents>/koine-demo/automation/loopback-49152.token
```

On the simulator, `xcrun simctl launch --console-pty <udid> com.yipihey.koine`
prints them, and the token file is under the app's data container:

```bash
DATA=$(xcrun simctl get_app_container <udid> com.yipihey.koine data)
cat "$DATA"/Documents/koine-demo/automation/loopback-*.token
```

On a phone, `Dioxus.toml` sets `UIFileSharingEnabled` and
`LSSupportsOpeningDocumentsInPlace`, so the Files app shows `koine-demo-mobile`
with the `koine-demo/` folder (store, settings, token) in it. The page's
`/host` route shows the origin and the bearer with a copy button.

## How an agent drives it

The same `/api/*` and `/mcp` the laptop host serves, with
`Authorization: Bearer <token>` on every request but a GET:

1. **The person**, through the page.
2. **Anything on the phone**, over loopback: an agent app that speaks MCP over
   HTTP dials `http://127.0.0.1:<port>/mcp` with the bearer from `/host`.
3. **A Mac on the same network**, after the `koine-host-service_network` verb
   turns on `allow_network` (off by default).
   On the simulator the Mac and the app share loopback, so
   `scripts/tests/ios/run.sh` drives it with plain `curl`.

The exact calls are in [`docs/mobile.md`](../../docs/mobile.md).

## Suspension

iOS suspends a backgrounded app and the host stops answering until it is on
screen again. A wait loop that times out while suspended is the page's normal
"render again" path; an agent driving a phone from a Mac keeps the app in front.
This is recorded, not worked around (M-D4).
