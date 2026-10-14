#!/usr/bin/env bash
# scripts/tests/gestures/run.sh - fire every gesture the tour offers with a
# mouse and then with a finger, each against a fresh demo host, and judge each
# by what the host answers (docs/plan-koine-affordances.md A-D5).
#
#   scripts/tests/gestures/run.sh                 # build the bundle, both pointers
#   KOINE_SKIP_BUILD=1 scripts/tests/gestures/run.sh
#   GESTURE_POINTERS=touch scripts/tests/gestures/run.sh
#
# Needs dx (dioxus-cli 0.7), node with playwright, and its Chromium under
# PLAYWRIGHT_BROWSERS_PATH. Exits non-zero when any gesture fails.
set -uo pipefail

here=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$here"
# dx is a cargo install: the user's bin and the repo owner's both count.
export PATH="$HOME/.cargo/bin:/home/user/.cargo/bin:$PATH"
export DX_HOME="${DX_HOME:-$HOME/.dx}"
# A machine with a shared Chromium (this repository's cloud sessions) says
# where; CI installs Playwright's own and leaves this unset.
[ -z "${PLAYWRIGHT_BROWSERS_PATH:-}" ] && [ -d /opt/pw-browsers ] && export PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers
target="${CARGO_TARGET_DIR:-$here/target}"
web_dir="$target/dx/koine-web/release/web/public"
[ -d "$here/target/dx/koine-web/release/web/public" ] && web_dir="$here/target/dx/koine-web/release/web/public"
port="${KOINE_PORT:-23196}"
scratch=$(mktemp -d)
host=""
stop() { [ -n "$host" ] && kill "$host" 2>/dev/null && wait "$host" 2>/dev/null; host=""; }
trap 'stop; rm -rf "$scratch"' EXIT

if [ -z "${KOINE_SKIP_BUILD:-}" ]; then
  (cd crates/koine-web && dx build --platform web --release) || exit 1
fi
[ -f "$web_dir/index.html" ] || { echo "no bundle at $web_dir" >&2; exit 1; }
cargo build -q -p koine-host || exit 1

status=0
for pointer in ${GESTURE_POINTERS:-mouse touch}; do
  ws="$scratch/$pointer"
  "$target/debug/koine-host" --store memory: --port "$port" --workspace "$ws" \
    --web-dir "$web_dir" > "$scratch/$pointer.log" 2>&1 &
  host=$!
  # The tour seeds after the listener is up: wait for the laid-out tiles.
  for _ in $(seq 1 150); do
    grep -q "tiles laid out" "$scratch/$pointer.log" 2>/dev/null && break
    sleep 0.2
  done
  grep -q "tiles laid out" "$scratch/$pointer.log" || { cat "$scratch/$pointer.log" >&2; exit 1; }
  KOINE_URL="http://127.0.0.1:$port" KOINE_TOKEN_FILE="$ws/automation/loopback-$port.token" \
    GESTURE_POINTER="$pointer" node scripts/tests/gestures/gestures.mjs || {
      status=1
      echo "---- host log ($pointer), warnings and refusals:"
      grep -E "WARN|refus|failed" "$scratch/$pointer.log" | tail -20
    }
  stop
done
exit $status
