#!/usr/bin/env bash
# scripts/tests/e2e/run.sh - the end-to-end proof of docs/plan-koine-host.md K7:
# an agent with koine and a library gets a bespoke app.
#
#   scratch library `mini-stats` -> scripts/new-app.sh stats-app -> the edits an
#   agent makes (two verbs and their selftest) -> check-verb-coverage.sh ->
#   cargo test -> the host (--check, then served over HTTP, MCP, restarted) ->
#   the page bundle (when `dx` is on this machine) -> `new-app.sh --mobile`
#   writes the iPhone crate and it type-checks (the iOS build itself is the
#   macOS lane's: scripts/tests/ios/run.sh).
#
# Prints PASS/FAIL per assertion, exits non-zero on any FAIL, and cleans up
# (kills the host, removes the scratch directory). The compiled dependencies
# are kept between runs in $E2E_TARGET_DIR (default: <koine>/target/e2e-app),
# so the first run builds the host's whole dependency tree and later ones do not.
#
# Environment:
#   KOINE_PATH       the koine checkout (default: the one this script is in)
#   E2E_TARGET_DIR   cargo's target directory for the scaffolded app
#   E2E_KEEP=1       keep the scratch directory (its path is printed)
#   DX               the dioxus CLI (default: dx on PATH, else ~/.cargo/bin/dx)
#   DX_HOME          dx's home (default: /root/.dx when it exists)
#
# Needs bash, cargo, curl, jq and python3.

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KOINE_PATH="${KOINE_PATH:-$(cd "$HERE/../../.." && pwd)}"
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR="${E2E_TARGET_DIR:-$KOINE_PATH/target/e2e-app}"

for tool in cargo curl jq python3; do
    command -v "$tool" >/dev/null 2>&1 || { echo "e2e: $tool is required" >&2; exit 2; }
done

TMP="$(mktemp -d)"
HOST_PID=""
FAILS=0
PASSES=0
START="$(date +%s)"

cleanup() {
    stop_host
    if [[ "${E2E_KEEP:-}" == 1 ]]; then
        echo "e2e: kept $TMP"
    else
        rm -rf "$TMP"
    fi
}
trap cleanup EXIT

pass() { echo "PASS  $1"; PASSES=$((PASSES + 1)); }
fail() { echo "FAIL  $1${2:+  ($2)}"; FAILS=$((FAILS + 1)); }
skip() { echo "SKIP  $1"; }
# check <label> <command...>: PASS when it succeeds.
check() {
    local label="$1"
    shift
    if "$@" >/dev/null 2>&1; then pass "$label"; else fail "$label"; fi
}
# expect_eq <label> <got> <want>
expect_eq() {
    if [[ "$2" == "$3" ]]; then pass "$1"; else fail "$1" "got '$2', want '$3'"; fi
}
# must <label> <command...>: run a build step with its output in a log; on
# failure print the tail of the log and stop (nothing after it can hold).
must() {
    local label="$1" log="$TMP/step-$((++STEP)).log"
    shift
    if "$@" >"$log" 2>&1; then
        pass "$label"
    else
        fail "$label"
        tail -n 40 "$log" | sed 's/^/        | /'
        summary
        exit 1
    fi
}
STEP=0

summary() {
    echo "e2e: $PASSES passed, $FAILS failed in $(( $(date +%s) - START ))s"
}

# ----------------------------------------------------------------- host
stop_host() {
    if [[ -n "$HOST_PID" ]]; then
        kill "$HOST_PID" 2>/dev/null
        wait "$HOST_PID" 2>/dev/null
        HOST_PID=""
    fi
}

# start_host <log> [flags...]: the app's host on a free port over the same
# store and workspace. Sets BASE, TOKEN; returns 1 when it does not come up.
start_host() {
    local log="$1"
    shift
    "$HOST_BIN" --store "dir:$TMP/store" --port 0 --workspace "$TMP/ws" "$@" >"$log" 2>&1 &
    HOST_PID=$!
    BASE=""
    for _ in $(seq 1 100); do
        BASE="$(grep -o -m1 'http://127\.0\.0\.1:[0-9]*' "$log" || true)"
        [[ -n "$BASE" ]] && break
        kill -0 "$HOST_PID" 2>/dev/null || break
        sleep 0.1
    done
    [[ -n "$BASE" ]] || { tail -n 20 "$log" | sed 's/^/        | /'; return 1; }
    local token_file
    token_file="$(sed -n 's/^ *loopback token: //p' "$log" | head -n 1)"
    TOKEN="$(cat "$token_file" 2>/dev/null || true)"
    [[ -n "$TOKEN" ]]
}

# req <METHOD> <path> [json-body]: sets STATUS and BODY.
req() {
    local method="$1" path="$2" data="${3:-}"
    local args=(-sS -X "$method" -o "$TMP/body" -w '%{http_code}' -H "Authorization: Bearer $TOKEN")
    [[ -n "$data" ]] && args+=(-H 'Content-Type: application/json' --data "$data")
    STATUS="$(curl "${args[@]}" "$BASE$path" 2>"$TMP/curl.err")" || STATUS="000"
    BODY="$(cat "$TMP/body" 2>/dev/null)"
}

# ================================================================ 1. the library
LIB="$TMP/mini-stats"
mkdir -p "$LIB/src"
cat >"$LIB/Cargo.toml" <<'EOF'
[package]
name = "mini-stats"
version = "0.1.0"
edition = "2021"
EOF
cat >"$LIB/src/lib.rs" <<'EOF'
//! A small statistics library: the thing an agent is asked to make an app of.

/// The arithmetic mean. `NaN` for an empty slice.
pub fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

/// Counts of `values` in `bins` equal-width bins over their range.
pub fn histogram(values: &[f64], bins: usize) -> Vec<usize> {
    let mut counts = vec![0usize; bins];
    if values.is_empty() || bins == 0 {
        return counts;
    }
    let lo = values.iter().cloned().fold(f64::INFINITY, f64::min);
    let hi = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let width = (hi - lo) / bins as f64;
    for v in values {
        let i = if width == 0.0 { 0 } else { ((v - lo) / width) as usize };
        counts[i.min(bins - 1)] += 1;
    }
    counts
}
EOF
pass "scratch library mini-stats written"

# ================================================================ 2. the scaffold
cd "$TMP" || exit 1
must "new-app.sh stats-app --lib mini-stats" \
    bash "$KOINE_PATH/scripts/new-app.sh" stats-app --lib mini-stats --lib-path "$LIB" --koine-path "$KOINE_PATH"
APP="$TMP/stats-app"
SVC="$APP/crates/stats-app-service"
check "the scaffold's host links koine_host::cli" grep -q 'koine_host::cli::main' "$APP/crates/stats-app-host/src/main.rs"

# ================================================================ 3. what an agent edits
# dto.rs: replace the TODO with the DTOs of the two verbs.
python3 - "$SVC/src/dto.rs" <<'EOF'
import sys
path = sys.argv[1]
s = open(path).read()
todo = "// TODO: one DTO per type of `mini-stats` that a verb takes or returns.\n"
assert todo in s, "dto.rs TODO marker not found"
dtos = '''/// The result of `mean`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Estimate {
    /// False when the call was refused; then `code` and `message` say why.
    pub ok: bool,
    /// The mean, when `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    /// A stable kebab-case reason, when refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The reason in a sentence, when refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl Estimate {
    pub fn value(value: f64) -> Self {
        Self { ok: true, value: Some(value), code: None, message: None }
    }
    pub fn refused(code: &str, message: &str) -> Self {
        Self { ok: false, value: None, code: Some(code.into()), message: Some(message.into()) }
    }
}

/// The result of `histogram`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Counts {
    /// False when the call was refused; then `code` and `message` say why.
    pub ok: bool,
    /// How many values fell in each bin, lowest bin first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counts: Option<Vec<u64>>,
    /// A stable kebab-case reason, when refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The reason in a sentence, when refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl Counts {
    pub fn counts(counts: Vec<u64>) -> Self {
        Self { ok: true, counts: Some(counts), code: None, message: None }
    }
    pub fn refused(code: &str, message: &str) -> Self {
        Self { ok: false, counts: None, code: Some(code.into()), message: Some(message.into()) }
    }
}
'''
open(path, "w").write(s.replace(todo, dtos))
EOF

# service.rs: the two methods on the trait, in the implementation, in methods = [...].
python3 - "$SVC/src/service.rs" <<'EOF'
import sys
path = sys.argv[1]
s = open(path).read()

def replace_once(s, old, new):
    assert s.count(old) == 1, f"anchor not found exactly once: {old[:60]!r}"
    return s.replace(old, new)

s = replace_once(s, "use crate::dto::EchoReply;", "use crate::dto::{Counts, EchoReply, Estimate};")
s = replace_once(s, "    async fn echo(&self, message: String, shout: Option<bool>) -> EchoReply;\n}", '''    async fn echo(&self, message: String, shout: Option<bool>) -> EchoReply;

    /// The arithmetic mean of a list of numbers. Refuses an empty list.
    #[impress_method]
    #[impress_example(
        name = "three",
        args = r#"{"values": [1, 2, 6]}"#,
        expect = r#"{"ok": true, "value": 3.0}"#
    )]
    async fn mean(&self, values: Vec<f64>) -> Estimate;

    /// Count a list of numbers into equal-width bins over their range. Refuses
    /// an empty list and zero bins.
    #[impress_method]
    #[impress_example(
        name = "two-bins",
        args = r#"{"values": [1, 2, 3, 4], "bins": 2}"#,
        expect = r#"{"ok": true, "counts": [2, 2]}"#
    )]
    async fn histogram(&self, values: Vec<f64>, bins: u64) -> Counts;
}''')
s = replace_once(s, "        EchoReply {\n            length: reply.chars().count() as u64,\n            reply,\n        }\n    }\n", '''        EchoReply {
            length: reply.chars().count() as u64,
            reply,
        }
    }

    async fn mean(&self, values: Vec<f64>) -> Estimate {
        if values.is_empty() {
            return Estimate::refused("invalid-argument", "the mean of no numbers is undefined");
        }
        Estimate::value(mini_stats::mean(&values))
    }

    async fn histogram(&self, values: Vec<f64>, bins: u64) -> Counts {
        if values.is_empty() || bins == 0 {
            return Counts::refused("invalid-argument", "histogram needs at least one value and one bin");
        }
        Counts::counts(
            mini_stats::histogram(&values, bins as usize)
                .into_iter()
                .map(|n| n as u64)
                .collect(),
        )
    }
''')
s = replace_once(s, "        ) -> EchoReply,\n    ],", '''        ) -> EchoReply,
        /// The arithmetic mean of a list of numbers.
        mean(
            /// The numbers to average. At least one.
            values: Vec<f64>,
        ) -> Estimate,
        /// Count numbers into equal-width bins over their range.
        histogram(
            /// The numbers to count. At least one.
            values: Vec<f64>,
            /// How many bins. At least one.
            bins: u64,
        ) -> Counts,
    ],''')
open(path, "w").write(s)
EOF

# selftest.rs: the new verbs, called through the inventory.
cat >>"$SVC/src/selftest.rs" <<'EOF'

#[test]
fn mean_averages_and_refuses_nothing() {
    let result = call::call("stats-app-service_mean", json!({ "values": [1, 2, 3] })).unwrap();
    assert_eq!(result["ok"], json!(true), "{result}");
    assert_eq!(result["value"], json!(2.0));
    let result = call::call("stats-app-service_mean", json!({ "values": [] })).unwrap();
    assert_eq!(result["ok"], json!(false), "{result}");
    assert_eq!(result["code"], json!("invalid-argument"));
}

#[test]
fn histogram_counts_into_bins() {
    let result = call::call(
        "stats-app-service_histogram",
        json!({ "values": [1, 2, 3, 4, 5, 6], "bins": 3 }),
    )
    .unwrap();
    assert_eq!(result["counts"], json!([2, 2, 2]), "{result}");
    let result = call::call("stats-app-service_histogram", json!({ "values": [1], "bins": 0 })).unwrap();
    assert_eq!(result["ok"], json!(false), "{result}");
}
EOF
pass "service.rs, dto.rs and selftest.rs edited (mean, histogram)"

# ================================================================ 4. coverage
cov="$(bash "$KOINE_PATH/scripts/check-verb-coverage.sh" --lib "$LIB" --service "$SVC" 2>&1)"
cov_rc=$?
echo "$cov" | sed 's/^/        | /'
expect_eq "check-verb-coverage exits 0" "$cov_rc" "0"
check "coverage: covered (2)" grep -qF 'covered (2):' <<<"$cov"
check "coverage: UNCOVERED (0)" grep -qF 'UNCOVERED (0):' <<<"$cov"

# ================================================================ 5. build, test, check
cd "$APP" || exit 1
must "cargo test (selftest)" cargo test
check "selftest ran the new verbs" grep -q 'histogram_counts_into_bins ... ok' "$TMP/step-$STEP.log"
must "cargo build -p stats-app-host" cargo build -p stats-app-host
HOST_BIN="$CARGO_TARGET_DIR/debug/stats-app-host"
check "host binary exists" test -x "$HOST_BIN"

cargo run -q -p stats-app-host -- --store memory: --port 0 --workspace "$TMP/ws-check" --check >"$TMP/check.out" 2>&1
expect_eq "cargo run -p stats-app-host -- --check exits 0" "$?" "0"
check "--check names the app and counts verbs" grep -Eq 'app stats-app, .*, [0-9]+ verbs' "$TMP/check.out"

# ================================================================ 6. serve it
if start_host "$TMP/host1.log"; then
    pass "host starts on a free port ($BASE) with dir:<tmp>/store"
else
    fail "host starts"
    summary
    exit 1
fi
check "the token file is under the workspace" test -s "$TMP/ws/automation/loopback-${BASE##*:}.token"

req GET /api/status
expect_eq "GET /api/status is 200" "$STATUS" "200"
expect_eq "/api/status names the app" "$(jq -r .app <<<"$BODY")" "stats-app"
verbs="$(jq -r .verbs <<<"$BODY")"
check "/api/status counts >= 3 verbs ($verbs)" test "${verbs:-0}" -ge 3

check "/api/status has .secrets.declared as a number" jq -e '.secrets.declared | type == "number"' <<<"$BODY"

# The secret slot (F-D1): this app declares none, so the list is empty and no
# name can be set.
req GET /api/secret
expect_eq "GET /api/secret is 200" "$STATUS" "200"
check "GET /api/secret answers ok with an array" jq -e '.ok == true and (.secrets | type == "array")' <<<"$BODY"
req PUT /api/secret/not-declared '{"value":"x"}'
expect_eq "PUT /api/secret/not-declared is 404" "$STATUS" "404"

# The job list (L-D1): the scaffolded app declares no `every` verb, so only
# the list itself is asserted.
req GET /api/job
expect_eq "GET /api/job is 200" "$STATUS" "200"
check "GET /api/job answers ok with an array" jq -e '.ok == true and (.jobs | type == "array")' <<<"$BODY"

req POST /api/verb/stats-app-service_mean '{"values":[1,2,3]}'
expect_eq "POST mean is 200" "$STATUS" "200"
check "mean of 1,2,3 is 2" jq -e '.ok == true and .value == 2' <<<"$BODY"
req POST /api/verb/stats-app-service_mean '{"values":[]}'
expect_eq "mean of nothing is refused invalid-argument (422/400)" "$(jq -r '.code' <<<"$BODY")/$([[ "$STATUS" == 4?? ]] && echo 4xx)" "invalid-argument/4xx"
req POST /api/verb/stats-app-service_mean '{"vals":[1]}'
expect_eq "a misspelt argument is refused" "$(jq -r '.code' <<<"$BODY")" "invalid-argument"
req POST /api/verb/stats-app-service_histogram '{"values":[1,2,3,4,5,6],"bins":3}'
expect_eq "histogram of 1..6 in 3 bins" "$(jq -c '.counts' <<<"$BODY")" "[2,2,2]"

# The generated surface of a verb, as a surface.
req GET /api/verb/stats-app-service_histogram/surface
expect_eq "GET .../histogram/surface is 200" "$STATUS" "200"
SPEC="$(jq -c .spec <<<"$BODY")"
check "the answer carries a spec with a Run button" jq -e '.. | objects | select(.id? == "run")' <<<"$SPEC"
req GET /api/verb/nothing_here/surface
expect_eq "an unknown verb's surface is 404" "$STATUS" "404"

req POST /api/surface "$SPEC"
expect_eq "POST /api/surface creates it" "$STATUS" "200"
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
RESULT="$(jq -c '(.state // .) | .result' <<<"$BODY")"
expect_eq "the result landed in state.result" "$(jq -c '.counts' <<<"$RESULT" 2>/dev/null)" "[2,2,2]"

# A browser's text field sends a string: the raw-JSON field takes "[1,2,3,4]" too.
req POST "/api/surface/$SID/dispatch" '{"widget":"arg-values","kind":"change","value":"[1,2,3,4]"}'
req POST "/api/surface/$SID/dispatch" '{"widget":"arg-bins","kind":"change","value":2}'
req POST "/api/surface/$SID/dispatch" '{"widget":"run","kind":"click"}'
req GET "/api/surface/$SID/state"
expect_eq "a string typed into the values field runs too" "$(jq -c '(.state // .) | .result.counts' <<<"$BODY")" "[2,2]"
# The state the restart check compares with is the last run's.

req GET /api/surface
check "GET /api/surface lists it" jq -e --arg id "$SID" '.surfaces | map(.id) | index($id) != null' <<<"$BODY"

# MCP.
req POST /mcp '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
expect_eq "MCP tools/list is 200" "$STATUS" "200"
for tool in stats-app-service_echo stats-app-service_mean stats-app-service_histogram; do
    check "MCP lists $tool" jq -e --arg t "$tool" '.result.tools | map(.name) | index($t) != null' <<<"$BODY"
done
req POST /mcp '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"stats-app-service_mean","arguments":{"values":[2,4]}}}'
check "MCP tools/call mean([2,4]) answers 3" grep -q '3' <<<"$BODY"

# A GET needs no token, a POST does.
anon="$(curl -sS -o /dev/null -w '%{http_code}' -X POST --data '{"values":[1]}' "$BASE/api/verb/stats-app-service_mean")"
expect_eq "a POST without the token is 401" "$anon" "401"

# Restart on the same root: the surface is still there.
stop_host
if start_host "$TMP/host2.log"; then
    req GET /api/surface
    check "after a restart the surface is still listed" jq -e --arg id "$SID" '.surfaces | map(.id) | index($id) != null' <<<"$BODY"
    req GET "/api/surface/$SID/state"
    expect_eq "after a restart its state still holds the result" "$(jq -c '(.state // .) | .result.counts' <<<"$BODY")" "[2,2]"
else
    fail "host restarts on the same store"
fi
stop_host

# A stream is a job that emits (plan-koine-tree-stream T-D2). The scaffolded
# app links no demo verbs, so this is the stock koine-host binary, which links
# `surface-demo-service_tick`, started in_background through the same route.
must "cargo build -p koine-host" cargo build -p koine-host --manifest-path "$KOINE_PATH/Cargo.toml"
STREAM_HOST_BIN="$CARGO_TARGET_DIR/debug/koine-host"
check "koine-host binary exists" test -x "$STREAM_HOST_BIN"
STATS_HOST_BIN="$HOST_BIN"
HOST_BIN="$STREAM_HOST_BIN"
if start_host "$TMP/host-stream.log"; then
    req POST /api/verb/surface-demo-service_tick '{"n":3,"every_ms":50,"adverbs":{"in_background":true}}'
    expect_eq "tick in_background is 200" "$STATUS" "200"
    JOB="$(jq -r '.job.id' <<<"$BODY")"
    check "tick answers a job handle" test -n "$JOB" -a "$JOB" != null
    TOTAL=0
    for _ in $(seq 1 100); do
        req GET "/api/job/$JOB/lines?after=0"
        TOTAL="$(jq -r '.total // 0' <<<"$BODY")"
        [[ "$TOTAL" == 3 ]] && break
        sleep 0.1
    done
    expect_eq "the job's lines reach total == 3" "$TOTAL" "3"
    check "the lines are the emitted objects in order" jq -e '.ok == true and ([.lines[].i] == [1,2,3]) and .next == 3 and .dropped == 0' <<<"$BODY"
    req GET "/api/job/$JOB/lines?after=2"
    check "after=2 pages to the last line" jq -e '(.lines | length) == 1 and .lines[0].i == 3' <<<"$BODY"
    req GET "/api/job/$JOB"
    expect_eq "GET /api/job/<id> shows line_count" "$(jq -r '.line_count' <<<"$BODY")" "3"
    req GET "/api/job/00000000-0000-4000-8000-000000000000/lines"
    expect_eq "lines of an unknown job is 404" "$STATUS" "404"
else
    fail "koine-host starts for the stream check"
fi
stop_host
HOST_BIN="$STATS_HOST_BIN"

# ================================================================ 7. the page
DX="${DX:-$(command -v dx || echo "$HOME/.cargo/bin/dx")}"
[[ -x "$DX" ]] || DX="/home/user/.cargo/bin/dx"
if [[ -x "$DX" ]]; then
    [[ -z "${DX_HOME:-}" && -d /root/.dx ]] && export DX_HOME=/root/.dx
    # dx writes under the invoking cargo target directory; keep it with koine's own.
    ( cd "$KOINE_PATH/crates/koine-web" && CARGO_TARGET_DIR="$KOINE_PATH/target" "$DX" build --platform web --release ) >"$TMP/dx.log" 2>&1
    dx_rc=$?
    PUBLIC="$KOINE_PATH/target/dx/koine-web/release/web/public"
    if [[ $dx_rc -eq 0 && -f "$PUBLIC/index.html" ]]; then
        pass "dx build --platform web --release (koine-web)"
        if start_host "$TMP/host3.log" --web-dir "$PUBLIC"; then
            page="$(curl -sS "$BASE/ui/")"
            check "/ui/ carries <meta name=\"koine-token\"" grep -qF '<meta name="koine-token"' <<<"$page"
            js="$(ls "$PUBLIC/assets" 2>/dev/null | grep -E '\.js$' | head -n 1)"
            if [[ -n "$js" ]]; then
                ctype="$(curl -sS -o /dev/null -w '%{content_type}' "$BASE/ui/assets/$js")"
                check "/ui/assets/$js is served as JavaScript ($ctype)" grep -q 'javascript' <<<"$ctype"
            else
                fail "the bundle has a .js asset" "none under $PUBLIC/assets"
            fi
        else
            fail "host starts with --web-dir"
        fi
        stop_host
    else
        fail "dx build --platform web --release (koine-web)" "see $TMP/dx.log"
        tail -n 20 "$TMP/dx.log" | sed 's/^/        | /'
    fi
else
    skip "the page: dx is not on this machine (build koine-web and pass --web-dir to check /ui/)"
fi

# ================================================================ 8. the app on an iPhone
# `new-app.sh --mobile` (docs/plan-koine-mobile.md M-D6): the scaffold writes
# crates/<name>-mobile and the workspace resolves it. Compiling it for iOS is
# the macOS lane's (.github/workflows/ci.yml, job `ios`); here it is checked
# WITHOUT the `mobile` feature, which compiles all of `main` and opens no
# window, so the names it uses from koine-host and koine-web are type-checked.
mkdir -p "$TMP/mobile" && cd "$TMP/mobile" || exit 1
must "new-app.sh phone-app --lib mini-stats --mobile" \
    bash "$KOINE_PATH/scripts/new-app.sh" phone-app --lib mini-stats --lib-path "$LIB" --koine-path "$KOINE_PATH" --mobile
PHONE="$TMP/mobile/phone-app"
MOBILE_CRATE="$PHONE/crates/phone-app-mobile"
for f in Cargo.toml src/main.rs Dioxus.toml; do
    check "--mobile writes crates/phone-app-mobile/$f" test -s "$MOBILE_CRATE/$f"
done
check "the mobile crate's bundle id is com.phone-app.koine" grep -q 'identifier = "com.phone-app.koine"' "$MOBILE_CRATE/Dioxus.toml"
check "the mobile crate asks for the Files app keys" grep -q 'UIFileSharingEnabled = true' "$MOBILE_CRATE/Dioxus.toml"
check "the scaffold's README has \"On an iPhone\"" grep -q '^## On an iPhone' "$PHONE/README.md"
check "the scaffold without --mobile has no mobile crate" test ! -e "$APP/crates/stats-app-mobile"
cd "$PHONE" || exit 1
cargo metadata --format-version 1 --no-deps >"$TMP/metadata.json" 2>"$TMP/metadata.err"
check "cargo metadata lists phone-app-mobile" jq -e '.packages | map(.name) | index("phone-app-mobile") != null' "$TMP/metadata.json"
check "cargo metadata lists the service and host too" jq -e '.packages | map(.name) | (index("phone-app-service") != null) and (index("phone-app-host") != null)' "$TMP/metadata.json"
must "cargo check -p phone-app-mobile (no mobile feature)" cargo check -p phone-app-mobile

summary
[[ $FAILS -eq 0 ]]
