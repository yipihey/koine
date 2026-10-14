#!/usr/bin/env bash
# The theme's proof (docs/plan-koine-theme.md TH-D5): build the browser
# bundle, serve it from a demo host over the tour's surfaces, screenshot every
# page at an iPhone and a Mac viewport, stop the host. The images land in
# docs/screenshots/<page>-<viewport>-<scheme>.png.
#
#   scripts/screenshots.sh            # build, serve, shoot
#   KOINE_SKIP_BUILD=1 scripts/screenshots.sh   # reuse the built bundle
#
# Needs `dx` (dioxus-cli 0.7), node with playwright installed globally and
# its Chromium under PLAYWRIGHT_BROWSERS_PATH (nothing is downloaded).
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
cd "$here"

# dx is a cargo install; the user's and the repo owner's bins both count.
export PATH="$HOME/.cargo/bin:/home/user/.cargo/bin:$PATH"
# dx caches its tools (esbuild, wasm-bindgen) under DX_HOME; without it an
# offline machine fails with "Failed to download esbuild".
export DX_HOME="${DX_HOME:-$HOME/.dx}"
export PLAYWRIGHT_BROWSERS_PATH="${PLAYWRIGHT_BROWSERS_PATH:-/opt/pw-browsers}"

port="${KOINE_PORT:-23188}"
web_dir="$here/target/dx/koine-web/release/web/public"
out="${KOINE_OUT:-$here/docs/screenshots}"
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

if [ -z "${KOINE_SKIP_BUILD:-}" ]; then
  echo "== building the browser bundle"
  (cd crates/koine-web && dx build --platform web --release)
fi
[ -f "$web_dir/index.html" ] || { echo "no bundle at $web_dir" >&2; exit 1; }

echo "== starting koine-host on :$port"
cargo build -q -p koine-host
./target/debug/koine-host --store memory: --port "$port" --workspace "$scratch/workspace" \
  --web-dir "$web_dir" > "$scratch/host.log" 2>&1 &
host=$!
trap 'kill $host 2>/dev/null; wait $host 2>/dev/null; rm -rf "$scratch"' EXIT

for _ in $(seq 1 100); do
  if curl -fsS "http://127.0.0.1:$port/api/status" > /dev/null 2>&1; then break; fi
  sleep 0.2
done
curl -fsS "http://127.0.0.1:$port/api/status" > /dev/null || { cat "$scratch/host.log" >&2; exit 1; }

echo "== screenshots into $out"
KOINE_URL="http://127.0.0.1:$port" KOINE_OUT="$out" node "$here/scripts/screenshots.mjs"
echo "== done"
