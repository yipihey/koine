#!/usr/bin/env bash
# scripts/check-verb-coverage.sh: "did I expose everything" as a script
# (docs/plan-koine-host.md H-D7). Read-only: it never modifies a file.
#
#   scripts/check-verb-coverage.sh
#       Census. Every `impress_service_impl! { service = X }` block under
#       crates/*/src has a row in docs/verb-coverage.md (between the
#       <!-- verb-coverage:begin/end --> markers), declares `effects = {`, and
#       the row's verb count matches the trait's #[impress_method] count; every
#       row names a block.
#   scripts/check-verb-coverage.sh --lib <crate-dir>
#       Print the library's public surface: `pub fn`, `pub struct`, `pub enum`,
#       `pub trait` at module level and `pub fn` in inherent impls of pub
#       types. A SOURCE HEURISTIC (comments/strings blanked, braces walked), not
#       rustc: pub(crate), #[cfg(test)], #[doc(hidden)] and trait impls are
#       skipped, re-exports are not followed, macros are not expanded.
#   scripts/check-verb-coverage.sh --lib <crate-dir> --service <service-crate-dir>
#       Match each public item to a verb: snake-case name equality with an
#       #[impress_method], a `# covers: Type::method` doc line above a method,
#       or an [[exclude]] item/reason in <service-crate-dir>/verb-exclusions.toml.
#       Prints covered / excluded / UNCOVERED; exits 1 if UNCOVERED is non-empty.
#   --report-only   with --service: print the lists but exit 0.
#   --root <dir>    census against another repository root (default: this one).
#   -h, --help      this text.
#
# Needs bash and python3 (3.11+ for TOML exclusions); no cargo build.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HELPER="$ROOT/scripts/lib/verb_coverage.py"

for arg in "$@"; do
    case "$arg" in
        -h|--help) sed -n '2,/^# Needs/p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    esac
done

command -v python3 >/dev/null 2>&1 || { echo "verb-coverage: python3 is required" >&2; exit 2; }
[[ -f "$HELPER" ]] || { echo "verb-coverage: missing $HELPER" >&2; exit 2; }

# Default root is this repository; --root overrides it.
exec python3 -I "$HELPER" --root "$ROOT" "$@"
