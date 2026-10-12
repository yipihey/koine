#!/usr/bin/env bash
# scripts/tests/ios/run.sh - the proof of docs/plan-koine-mobile.md M-D5:
# an agent drives a koine app that is running on iOS.
#
#   boot a simulator -> install the .app that `dx build --platform ios` made ->
#   launch it -> read the loopback token from the app's data container ->
#   from the Mac (the simulator shares its loopback) drive the host the way
#   scripts/tests/e2e/run.sh drives a scaffolded app's: /api/status, a verb,
#   and the five-verb loop (surface create -> render -> dispatch -> state ->
#   wait) with the bearer.
#
# This is what the `ios` job of .github/workflows/ci.yml runs, and what a
# person with a Mac runs by hand after
#
#   cd crates/koine-demo-mobile && dx build --platform ios --release
#
# Prints PASS/FAIL per assertion and exits non-zero on any FAIL. The app is
# terminated, and a simulator this script booted is shut down, on the way out.
#
# Usage:  scripts/tests/ios/run.sh [--udid <udid>] [--app <path/to/X.app>]
#                                  [--bundle-id <id>] [--app-id <id>]
#                                  [--keep-booted]
#
# Each flag has an environment variable of the same name in capitals
# (UDID, APP_PATH, BUNDLE_ID, APP_ID, KEEP_BOOTED=1):
#   --udid       the simulator (default: a booted iPhone, else the first
#                available iPhone)
#   --app        the built bundle (default: the first *.app under
#                <target>/dx/koine-demo-mobile/release/ios/, <target> being
#                $CARGO_TARGET_DIR or <koine>/target)
#   --bundle-id  the app's CFBundleIdentifier (default com.koine.demo, the
#                Dioxus.toml [bundle] identifier of crates/koine-demo-mobile)
#   --app-id     the folder under Documents the host's workspace lives in
#                (default koine-demo, `sandbox_config("koine-demo")`)
#   --keep-booted  leave the simulator running (it is left alone anyway when
#                it was already booted)
#
# Needs macOS with Xcode (xcrun simctl), curl and jq.

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KOINE_PATH="${KOINE_PATH:-$(cd "$HERE/../../.." && pwd)}"

UDID="${UDID:-}"
APP_PATH="${APP_PATH:-}"
BUNDLE_ID="${BUNDLE_ID:-com.koine.demo}"
APP_ID="${APP_ID:-koine-demo}"
KEEP_BOOTED="${KEEP_BOOTED:-}"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --udid) UDID="${2:-}"; shift 2 ;;
        --app) APP_PATH="${2:-}"; shift 2 ;;
        --bundle-id) BUNDLE_ID="${2:-}"; shift 2 ;;
        --app-id) APP_ID="${2:-}"; shift 2 ;;
        --keep-booted) KEEP_BOOTED=1; shift ;;
        -h | --help) sed -n '2,/^set -uo/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "ios: unknown argument '$1' (try --help)" >&2; exit 2 ;;
    esac
done

for tool in xcrun curl jq; do
    command -v "$tool" >/dev/null 2>&1 || { echo "ios: $tool is required (this needs macOS with Xcode)" >&2; exit 2; }
done

TMP="$(mktemp -d)"
LOG="$TMP/app-console.log"
LAUNCHER_PID=""
BOOTED_BY_US=""
FAILS=0
PASSES=0
START="$(date +%s)"

cleanup() {
    if [[ -n "$UDID" ]]; then
        xcrun simctl terminate "$UDID" "$BUNDLE_ID" >/dev/null 2>&1
    fi
    if [[ -n "$LAUNCHER_PID" ]]; then
        kill "$LAUNCHER_PID" 2>/dev/null
        wait "$LAUNCHER_PID" 2>/dev/null
    fi
    if [[ -n "$BOOTED_BY_US" && -z "$KEEP_BOOTED" ]]; then
        xcrun simctl shutdown "$UDID" >/dev/null 2>&1
    fi
    rm -rf "$TMP"
}
trap cleanup EXIT

pass() { echo "PASS  $1"; PASSES=$((PASSES + 1)); }
fail() { echo "FAIL  $1${2:+  ($2)}"; FAILS=$((FAILS + 1)); }
skip() { echo "SKIP  $1"; }
check() {
    local label="$1"
    shift
    if "$@" >/dev/null 2>&1; then pass "$label"; else fail "$label"; fi
}
expect_eq() {
    if [[ "$2" == "$3" ]]; then pass "$1"; else fail "$1" "got '$2', want '$3'"; fi
}
summary() {
    echo "ios: $PASSES passed, $FAILS failed in $(( $(date +%s) - START ))s"
}
# must <label> <command...>: a step nothing after it can do without.
must() {
    local label="$1" log="$TMP/step-$((++STEP)).log"
    shift
    if "$@" >"$log" 2>&1; then
        pass "$label"
    else
        fail "$label"
        tail -n 40 "$log" | sed 's/^/        | /'
        dump_console
        summary
        exit 1
    fi
}
STEP=0

dump_console() {
    if [[ -s "$LOG" ]]; then
        echo "        | app console (last 30 lines):"
        tail -n 30 "$LOG" | sed 's/^/        | /'
    fi
}

# req <METHOD> <path> [json-body]: sets STATUS and BODY. The bearer rides on
# every request, as the e2e's does.
req() {
    local method="$1" path="$2" data="${3:-}"
    local args=(-sS -X "$method" -o "$TMP/body" -w '%{http_code}' -H "Authorization: Bearer $TOKEN")
    [[ -n "$data" ]] && args+=(-H 'Content-Type: application/json' --data "$data")
    STATUS="$(curl "${args[@]}" "$BASE$path" 2>"$TMP/curl.err")" || STATUS="000"
    BODY="$(cat "$TMP/body" 2>/dev/null)"
}

# ================================================================ 1. the app
if [[ -z "$APP_PATH" ]]; then
    TARGET="${CARGO_TARGET_DIR:-$KOINE_PATH/target}"
    APP_PATH="$(ls -d "$TARGET"/dx/koine-demo-mobile/release/ios/*.app 2>/dev/null | head -n 1)"
fi
if [[ -z "$APP_PATH" || ! -d "$APP_PATH" ]]; then
    fail "the built app exists" "no .app: run \`dx build --platform ios --release\` in crates/koine-demo-mobile, or pass --app"
    summary
    exit 1
fi
pass "the built app: $APP_PATH"
check "the bundle has an Info.plist" test -f "$APP_PATH/Info.plist"
if command -v plutil >/dev/null 2>&1; then
    expect_eq "the bundle's CFBundleIdentifier is $BUNDLE_ID" \
        "$(plutil -extract CFBundleIdentifier raw -o - "$APP_PATH/Info.plist" 2>/dev/null)" "$BUNDLE_ID"
    expect_eq "the bundle asks for Files-app sharing (UIFileSharingEnabled)" \
        "$(plutil -extract UIFileSharingEnabled raw -o - "$APP_PATH/Info.plist" 2>/dev/null)" "true"
    expect_eq "the bundle opens documents in place (LSSupportsOpeningDocumentsInPlace)" \
        "$(plutil -extract LSSupportsOpeningDocumentsInPlace raw -o - "$APP_PATH/Info.plist" 2>/dev/null)" "true"
fi

# ================================================================ 2. the simulator
device_state() {
    xcrun simctl list devices -j | jq -r --arg u "$1" '[.devices[][] | select(.udid == $u)][0].state // empty'
}

if [[ -z "$UDID" ]]; then
    # A booted iPhone first (no boot to wait for), else the first available.
    UDID="$(xcrun simctl list devices available -j | jq -r '
        [.devices | to_entries[] | select(.key | test("iOS")) | .value[] | select(.name | startswith("iPhone"))]
        | ((map(select(.state == "Booted")) + .) | first | .udid) // empty')"
fi
if [[ -z "$UDID" ]]; then
    fail "an iPhone simulator is available" "xcrun simctl list devices available shows none: install an iOS runtime in Xcode"
    summary
    exit 1
fi
pass "simulator $UDID ($(xcrun simctl list devices -j | jq -r --arg u "$UDID" '[.devices[][] | select(.udid == $u)][0].name'))"

if [[ "$(device_state "$UDID")" != "Booted" ]]; then
    must "xcrun simctl boot" xcrun simctl boot "$UDID"
    BOOTED_BY_US=1
fi
must "the simulator finishes booting (bootstatus)" xcrun simctl bootstatus "$UDID"

# A clean data container: a token file from an earlier run would be read as
# this run's.
xcrun simctl uninstall "$UDID" "$BUNDLE_ID" >/dev/null 2>&1
must "xcrun simctl install" xcrun simctl install "$UDID" "$APP_PATH"

# ================================================================ 3. launch, find the token
# `--console` keeps the launcher attached and relays the app's stdout and
# stderr, which is where `main` logs its URL and token path.
xcrun simctl launch --console --terminate-running-process "$UDID" "$BUNDLE_ID" >"$LOG" 2>&1 &
LAUNCHER_PID=$!
sleep 1
if kill -0 "$LAUNCHER_PID" 2>/dev/null || grep -q "$BUNDLE_ID" "$LOG"; then
    pass "xcrun simctl launch $BUNDLE_ID"
else
    fail "xcrun simctl launch $BUNDLE_ID"
    dump_console
    summary
    exit 1
fi

DATA="$(xcrun simctl get_app_container "$UDID" "$BUNDLE_ID" data 2>"$TMP/container.err")"
if [[ -n "$DATA" && -d "$DATA" ]]; then
    pass "the app's data container: $DATA"
else
    fail "xcrun simctl get_app_container $BUNDLE_ID data" "$(cat "$TMP/container.err")"
    summary
    exit 1
fi

# The host writes <workspace>/automation/loopback-<port>.token at start
# (impress_service_core::loopback_token::token_file_name); the workspace is
# <HOME>/Documents/<app id>, HOME being the data container.
AUTOMATION="$DATA/Documents/$APP_ID/automation"
TOKEN_FILE=""
for _ in $(seq 1 90); do
    TOKEN_FILE="$(ls -t "$AUTOMATION"/loopback-*.token 2>/dev/null | head -n 1)"
    [[ -n "$TOKEN_FILE" && -s "$TOKEN_FILE" ]] && break
    TOKEN_FILE=""
    kill -0 "$LAUNCHER_PID" 2>/dev/null || break
    sleep 1
done
if [[ -z "$TOKEN_FILE" ]]; then
    fail "the app wrote its loopback token under Documents/$APP_ID/automation" "none in $AUTOMATION"
    ls -R "$DATA/Documents" 2>/dev/null | head -n 30 | sed 's/^/        | /'
    dump_console
    summary
    exit 1
fi
pass "the app wrote its loopback token: ${TOKEN_FILE#"$DATA"/}"
TOKEN="$(cat "$TOKEN_FILE")"
PORT="$(basename "$TOKEN_FILE" .token)"
PORT="${PORT#loopback-}"
check "the port in the token file's name is a number ($PORT)" test "$PORT" -gt 0
BASE="http://127.0.0.1:$PORT"

logged="$(sed -n 's/.*koine-demo-mobile: host \(http:[^ ]*\).*/\1/p' "$LOG" | head -n 1)"
if [[ -n "$logged" ]]; then
    expect_eq "the console's URL is the token file's port" "$logged" "$BASE"
else
    skip "the console relayed no URL line (the token file's name is the authority)"
fi

# ================================================================ 4. the host answers
UP=""
for _ in $(seq 1 60); do
    if curl -sf -o "$TMP/status.json" "$BASE/api/status" 2>/dev/null; then UP=1; break; fi
    kill -0 "$LAUNCHER_PID" 2>/dev/null || break
    sleep 1
done
if [[ -z "$UP" ]]; then
    fail "GET $BASE/api/status answers from the Mac" "$(cat "$TMP/curl.err" 2>/dev/null)"
    dump_console
    summary
    exit 1
fi
pass "GET /api/status answers from the Mac (the simulator shares loopback)"

req GET /api/status
expect_eq "GET /api/status is 200" "$STATUS" "200"
expect_eq "/api/status names the app" "$(jq -r .app <<<"$BODY")" "$APP_ID"
expect_eq "/api/status reports the port the token file named" "$(jq -r .port <<<"$BODY")" "$PORT"
verbs="$(jq -r .verbs <<<"$BODY")"
check "/api/status counts >= 3 verbs ($verbs)" test "${verbs:-0}" -ge 3
check "the store is a dir: store under the app's Documents" jq -e '.store.url | startswith("dir:") and contains("/Documents/")' <<<"$BODY"

# A GET needs no token, a POST does.
anon="$(curl -sS -o /dev/null -w '%{http_code}' -X POST --data '{"values":[1],"bins":1}' "$BASE/api/verb/surface-demo-service_histogram")"
expect_eq "a POST without the token is 401" "$anon" "401"

# ================================================================ 5. a verb
req POST /api/verb/surface-demo-service_histogram '{"values":[1,2,3,4,5,6],"bins":3}'
expect_eq "POST histogram is 200" "$STATUS" "200"
expect_eq "histogram of 1..6 in 3 bins" "$(jq -c '.counts' <<<"$BODY")" "[2,2,2]"

# ================================================================ 6. the five-verb loop
# create -> show/render -> dispatch -> state -> wait, with the bearer.
req GET /api/verb/surface-demo-service_histogram/surface
expect_eq "GET .../histogram/surface is 200" "$STATUS" "200"
SPEC="$(jq -c .spec <<<"$BODY")"
check "the answer carries a spec with a Run button" jq -e '.. | objects | select(.id? == "run")' <<<"$SPEC"

req POST /api/surface "$SPEC"
expect_eq "POST /api/surface creates it" "$STATUS" "200"
check "the create answer is ok" jq -e '.ok != false' <<<"$BODY"
SID="$(jq -r .id <<<"$BODY")"
check "the surface has an id" test -n "$SID" -a "$SID" != null

req GET "/api/surface/$SID/render"
expect_eq "render is 200" "$STATUS" "200"
check "the rendered tree has the Run button" grep -q '"run"' <<<"$BODY"

req POST "/api/surface/$SID/dispatch" '{"widget":"arg-values","kind":"change","value":[1,2,3,4,5,6]}'
expect_eq "dispatch change on the values field" "$STATUS" "200"
req POST "/api/surface/$SID/dispatch" '{"widget":"arg-bins","kind":"change","value":3}'
expect_eq "dispatch change on the bins field" "$STATUS" "200"
req POST "/api/surface/$SID/dispatch" '{"widget":"run","kind":"click"}'
expect_eq "dispatch click on Run" "$STATUS" "200"
check "the click's answer is ok" jq -e '.ok == true' <<<"$BODY"

req GET "/api/surface/$SID/state"
expect_eq "GET .../state is 200" "$STATUS" "200"
expect_eq "the result landed in state.result" "$(jq -c '(.state // .) | .result.counts' <<<"$BODY")" "[2,2,2]"

req GET "/api/surface/$SID/wait?after_seq=0&timeout_ms=1000"
expect_eq "GET .../wait is 200" "$STATUS" "200"
check "the wait's answer is ok" jq -e '.ok == true' <<<"$BODY"
# A dispatch becomes an agent event only through an `emit` action, and the
# generated verb form has none: the wait answers at once with no events and
# the cursor it was given back. The e2e asserts the same shape.
check "the wait answers a cursor and an events list" jq -e '(.next_seq | type) == "number" and (.events | type) == "array"' <<<"$BODY"

req GET /api/surface
check "GET /api/surface lists it" jq -e --arg id "$SID" '.surfaces | map(.id) | index($id) != null' <<<"$BODY"

# MCP: what an agent app on the phone dials.
req POST /mcp '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
expect_eq "MCP tools/list is 200" "$STATUS" "200"
check "MCP lists surface-demo-service_histogram" jq -e '.result.tools | map(.name) | index("surface-demo-service_histogram") != null' <<<"$BODY"

# The page is a client of the same host: /ui answers 503 (the bundle is the
# WebView's, not served here) or the page, never a connection error.
ui="$(curl -sS -o /dev/null -w '%{http_code}' "$BASE/ui/")"
check "GET /ui/ answers ($ui)" test "$ui" != "000"

[[ $FAILS -eq 0 ]] || dump_console

summary
[[ $FAILS -eq 0 ]]
