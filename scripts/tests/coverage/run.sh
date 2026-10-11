#!/usr/bin/env bash
# Asserts scripts/check-verb-coverage.sh on the fixtures next to this file.
# Writes only into a mktemp directory.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$HERE/../../check-verb-coverage.sh"
LIB="$HERE/fixture-lib"
SVC="$HERE/fixture-service"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
fails=0
ok()  { echo "ok:   $1"; }
bad() { echo "FAIL: $1"; fails=$((fails + 1)); }
expect_line() { # <label> <output> <fixed string>
    if grep -qF -- "$3" <<< "$2"; then ok "$1"; else bad "$1 (no line '$3')"; fi
}
reject_line() {
    if grep -qF -- "$3" <<< "$2"; then bad "$1 (unexpected '$3')"; else ok "$1"; fi
}

# 1. --lib alone: the public surface, five items
out="$(bash "$SCRIPT" --lib "$LIB")"; rc=$?
[[ $rc -eq 0 ]] && ok "--lib alone exits 0" || bad "--lib alone exits 0 (got $rc)"
expect_line "surface has struct Counter" "$out" "struct Counter"
expect_line "surface has fn Counter::bump" "$out" "fn Counter::bump"
expect_line "surface has fn Counter::read_total" "$out" "fn Counter::read_total"
expect_line "surface has fn describe" "$out" "fn describe"
expect_line "surface count" "$out" "4 public items"
for hidden in internal_only crate_private hidden_helper test_only in_a_comment in_a_string; do
    reject_line "surface omits $hidden" "$out" "$hidden"
done

# 2. with the service: covered 2 (name + covers), excluded 1, uncovered 1 (struct Counter)
out="$(bash "$SCRIPT" --lib "$LIB" --service "$SVC")"; rc=$?
[[ $rc -eq 1 ]] && ok "uncovered item exits 1" || bad "uncovered item exits 1 (got $rc)"
expect_line "covered 2" "$out" "covered (2):"
expect_line "excluded 1" "$out" "excluded (1):"
expect_line "uncovered 1" "$out" "UNCOVERED (1):"
expect_line "bump covered by name" "$out" "fn Counter::bump"
expect_line "name match is reported" "$out" "verb \`bump\` (name)"
expect_line "covers match is reported" "$out" "verb \`total\` (covers)"
expect_line "exclusion reason is printed" "$out" "a constant string; an agent has no use for it"
unc="$(sed -n '/^UNCOVERED/,$p' <<< "$out")"
expect_line "the uncovered item is struct Counter" "$unc" "struct Counter"

# 3. --report-only: same lists, exit 0
out="$(bash "$SCRIPT" --lib "$LIB" --service "$SVC" --report-only)"; rc=$?
[[ $rc -eq 0 ]] && ok "--report-only exits 0" || bad "--report-only exits 0 (got $rc)"
expect_line "report-only still lists UNCOVERED" "$out" "UNCOVERED (1):"

# 4. the uncovered item added to the exclusions: exit 0
cp -R "$SVC" "$TMP/svc"
cat >> "$TMP/svc/verb-exclusions.toml" <<'TOML'

[[exclude]]
item = "Counter"
reason = "the type itself; its methods are the verbs"
TOML
out="$(bash "$SCRIPT" --lib "$LIB" --service "$TMP/svc")"; rc=$?
[[ $rc -eq 0 ]] && ok "all excluded exits 0" || bad "all excluded exits 0 (got $rc)"
expect_line "uncovered 0" "$out" "UNCOVERED (0):"
expect_line "excluded 2" "$out" "excluded (2):"

# 5. census against a scratch koine-shaped repo (--root)
R="$TMP/repo"; mkdir -p "$R/docs" "$R/crates/demo-service/src"
cat > "$R/crates/demo-service/src/lib.rs" <<'RS'
// impress_service_impl! { service = Mentioned }  (a comment, not a block)
#[impress_service]
pub trait DemoService {
    #[impress_method]
    async fn one(&self) -> u32;
    #[impress_method]
    async fn two(&self) -> u32;
}
impress_service_impl! {
    service = DemoService,
    effects = { reads: [], writes: [], reach: [] },
    impl = Demo,
    methods = [
        one() -> u32,
        two() -> u32,
    ],
}
RS
table() { # <verbs>
    printf '<!-- verb-coverage:begin -->\n| Service | Crate | Verbs |\n|---|---|---:|\n| `demo-service` | demo-service | %s |\n<!-- verb-coverage:end -->\n' "$1" > "$R/docs/verb-coverage.md"
}
table 2
out="$(bash "$SCRIPT" --root "$R")"; rc=$?
[[ $rc -eq 0 ]] && ok "census passes on a matching repo" || bad "census passes (got $rc: $out)"
table 3
out="$(bash "$SCRIPT" --root "$R")"; rc=$?
[[ $rc -ne 0 ]] && ok "census fails on a wrong verb count" || bad "census fails on wrong count"
expect_line "census names the count mismatch" "$out" "says 3 verbs; the source has 2"
table 2
sed -i 's/effects = { reads: \[\], writes: \[\], reach: \[\] },//' "$R/crates/demo-service/src/lib.rs"
out="$(bash "$SCRIPT" --root "$R")"; rc=$?
[[ $rc -ne 0 ]] && ok "census fails without effects" || bad "census fails without effects"
expect_line "census names the missing effects" "$out" "does not declare \`effects"
printf '<!-- verb-coverage:begin -->\n| `ghost-service` | x | 1 |\n<!-- verb-coverage:end -->\n' > "$R/docs/verb-coverage.md"
out="$(bash "$SCRIPT" --root "$R")"; rc=$?
expect_line "census names a row with no block" "$out" "row for ghost-service"
expect_line "census names a block with no row" "$out" "has an impress_service_impl! block"

echo
if [[ $fails -ne 0 ]]; then echo "coverage tests: $fails FAILED"; exit 1; fi
echo "coverage tests: all passed"
