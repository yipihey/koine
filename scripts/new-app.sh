#!/usr/bin/env bash
# scripts/new-app.sh <name> --lib <crate-name> [--lib-path <path>] — scaffold a
# bespoke app over a Rust library (docs/plan-koine-host.md H-D6).
#
# Writes, under the current directory, a cargo workspace `<name>/`:
#
#   <name>/
#   ├── Cargo.toml                    workspace; koine crates by git rev or path
#   ├── .cargo/config.toml            points the macros at ./schema-refs.json
#   ├── schema-refs.json              copy of koine's snapshot; add your kinds
#   ├── Dioxus.toml                   the page proxy (`dx serve` -> this host)
#   ├── README.md                     the agent's checklist, in order
#   └── crates/
#       ├── <name>-service/           dto.rs, service.rs, selftest.rs
#       └── <name>-host/              main.rs: links the service, lists its verbs
#
# Usage:
#   scripts/new-app.sh <name> --lib <crate-name> [--lib-path <path>]
#                      [--koine-rev <rev> | --koine-path <checkout>]
#                      [--koine-git <url>]
#
#   <name>          ^[a-z][a-z0-9-]*$ ; becomes the workspace directory and the
#                   prefix of the verbs (`<name>-service_<verb>`)
#   --lib           the library crate the app exposes
#   --lib-path      a path dependency for it; without it the dependency is the
#                   crates.io release (`"*"`)
#   --koine-rev     the koine commit to depend on (git); default: HEAD of the
#                   checkout this script lives in. It must be pushed.
#   --koine-path    depend on a local koine checkout by path instead of git
#                   (what the scaffold's own test uses)
#   --koine-git     the koine repository URL (default: the public one)
#
# Refuses an existing <name>/ and writes nothing in that case: the tree is
# built in a scratch directory beside it and moved into place at the end.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KOINE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# `&` in a replacement means "the match" in bash 5.2; the templates below
# substitute paths and names that may contain one.
shopt -u patsub_replacement 2>/dev/null || true

usage() {
    sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//' >&2
    exit "${1:-1}"
}

die() {
    echo "error: $*" >&2
    exit 1
}

NAME=""
LIB=""
LIB_PATH=""
KOINE_REV=""
KOINE_PATH=""
KOINE_GIT="https://github.com/yipihey/koine"

while [[ $# -gt 0 ]]; do
    case "$1" in
        -h | --help) usage 0 ;;
        --lib) LIB="${2:-}"; shift 2 ;;
        --lib-path) LIB_PATH="${2:-}"; shift 2 ;;
        --koine-rev) KOINE_REV="${2:-}"; shift 2 ;;
        --koine-path) KOINE_PATH="${2:-}"; shift 2 ;;
        --koine-git) KOINE_GIT="${2:-}"; shift 2 ;;
        --*) die "unknown flag '$1' (try --help)" ;;
        *)
            [[ -z "$NAME" ]] || die "one <name> only; got '$NAME' and '$1'"
            NAME="$1"
            shift
            ;;
    esac
done

[[ -n "$NAME" ]] || usage 1
[[ "$NAME" =~ ^[a-z][a-z0-9-]*$ ]] || die "name must match ^[a-z][a-z0-9-]*\$, got '$NAME'"
[[ -n "$LIB" ]] || die "--lib <crate-name> is required: the library the app exposes"
[[ "$LIB" =~ ^[A-Za-z][A-Za-z0-9_-]*$ ]] || die "--lib must be a crate name, got '$LIB'"
[[ ! -e "$NAME" ]] || die "$NAME already exists - refusing to overwrite (delete it or pick another name)"
[[ -z "$KOINE_REV" || -z "$KOINE_PATH" ]] || die "--koine-rev and --koine-path are alternatives; pass one"

# ---------------------------------------------------------------- names
pascal_case() {
    local out="" part first
    local IFS='-'
    for part in $1; do
        first="$(printf '%s' "${part:0:1}" | tr '[:lower:]' '[:upper:]')"
        out+="${first}${part:1}"
    done
    printf '%s' "$out"
}

PASCAL="$(pascal_case "$NAME")"
TRAIT="${PASCAL}Service"          # kebab(TRAIT) == "<name>-service"
IMPL="Default${TRAIT}"
SERVICE_CRATE="${NAME}-service"
HOST_CRATE="${NAME}-host"
SERVICE_SNAKE="${SERVICE_CRATE//-/_}"
LIB_SNAKE="${LIB//-/_}"
TOOL="${SERVICE_CRATE}_echo"
PORT=23130

# ---------------------------------------------------------------- koine deps
if [[ -n "$KOINE_PATH" ]]; then
    [[ -f "$KOINE_PATH/crates/impress-service-core/Cargo.toml" ]] \
        || die "--koine-path '$KOINE_PATH' is not a koine checkout (no crates/impress-service-core)"
    KOINE_PATH="$(cd "$KOINE_PATH" && pwd)"
    KOINE_WHERE="path $KOINE_PATH"
    koine_dep() { printf '%s = { path = "%s/crates/%s" }' "$1" "$KOINE_PATH" "$1"; }
else
    if [[ -z "$KOINE_REV" ]]; then
        KOINE_REV="$(git -C "$KOINE_ROOT" rev-parse HEAD 2>/dev/null)" \
            || die "cannot read this checkout's HEAD; pass --koine-rev <commit> or --koine-path <checkout>"
    fi
    KOINE_WHERE="git $KOINE_GIT @ ${KOINE_REV:0:12}"
    koine_dep() { printf '%s = { git = "%s", rev = "%s" }' "$1" "$KOINE_GIT" "$KOINE_REV"; }
fi
KOINE_CORE_DEP="$(koine_dep impress-service-core)"
KOINE_MACROS_DEP="$(koine_dep impress-service-macros)"

# ---------------------------------------------------------------- the library
PUBLIC_FNS="//   (no --lib-path given: list the library's public API with \`cargo doc --open\` or docs.rs)"
if [[ -n "$LIB_PATH" ]]; then
    [[ -f "$LIB_PATH/Cargo.toml" ]] || die "--lib-path '$LIB_PATH' has no Cargo.toml"
    LIB_PATH="$(cd "$LIB_PATH" && pwd)"
    declared="$(sed -n 's/^name *= *"\(.*\)"/\1/p' "$LIB_PATH/Cargo.toml" | head -n 1)"
    [[ -z "$declared" || "$declared" == "$LIB" ]] \
        || die "--lib is '$LIB' but $LIB_PATH/Cargo.toml names the package '$declared'"
    LIB_DEP="${LIB} = { path = \"${LIB_PATH}\" }"
    # A crude first pass over the source: signature lines only, so the TODO
    # below starts from the real list. `cargo doc` is the authoritative one.
    PUBLIC_FNS="$(
        grep -rhE --include='*.rs' '^[[:space:]]*pub (async )?(const )?(unsafe )?fn [a-z_0-9]+' \
            "$LIB_PATH/src" 2>/dev/null \
            | sed -E 's/[[:space:]]*\{.*$//; s/^[[:space:]]+//; s/^/\/\/   /' | head -n 60
    )" || true
    [[ -n "$PUBLIC_FNS" ]] || PUBLIC_FNS="//   (no \`pub fn\` found under $LIB_PATH/src)"
else
    LIB_DEP="${LIB} = \"*\""
fi

# ---------------------------------------------------------------- writing
STAGE="$(mktemp -d "./.${NAME}.new.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT
ROOT="$STAGE/$NAME"

# emit <relative-path>: stdin, with __PLACEHOLDERS__ filled, to $ROOT/<path>.
emit() {
    local path="$1" body
    path="${path//__SERVICE_CRATE__/$SERVICE_CRATE}"
    path="${path//__HOST_CRATE__/$HOST_CRATE}"
    path="$ROOT/$path"
    mkdir -p "$(dirname "$path")"
    body="$(cat)"
    body="${body//__NAME__/$NAME}"
    body="${body//__PASCAL__/$PASCAL}"
    body="${body//__TRAIT__/$TRAIT}"
    body="${body//__IMPL__/$IMPL}"
    body="${body//__SERVICE_CRATE__/$SERVICE_CRATE}"
    body="${body//__SERVICE_SNAKE__/$SERVICE_SNAKE}"
    body="${body//__HOST_CRATE__/$HOST_CRATE}"
    body="${body//__LIB_DEP__/$LIB_DEP}"
    body="${body//__LIB_SNAKE__/$LIB_SNAKE}"
    body="${body//__LIB__/$LIB}"
    body="${body//__TOOL__/$TOOL}"
    body="${body//__PORT__/$PORT}"
    body="${body//__KOINE_WHERE__/$KOINE_WHERE}"
    body="${body//__KOINE_CORE_DEP__/$KOINE_CORE_DEP}"
    body="${body//__KOINE_MACROS_DEP__/$KOINE_MACROS_DEP}"
    body="${body//__PUBLIC_FNS__/$PUBLIC_FNS}"
    printf '%s\n' "$body" >"$path"
}

emit Cargo.toml <<'EOF'
# The workspace of __NAME__: a service crate (the verbs over __LIB__) and a
# host binary that links it. koine: __KOINE_WHERE__.
[workspace]
resolver = "2"
members = ["crates/__SERVICE_CRATE__", "crates/__HOST_CRATE__"]

[workspace.package]
version = "0.1.0"
edition = "2021"

[workspace.dependencies]
__KOINE_CORE_DEP__
__KOINE_MACROS_DEP__
# K4: koine-host (`koine_host::serve`) joins these two once it is published.
serde = { version = "1", features = ["derive"] }
serde_json = "1"
schemars = "0.8"
EOF

# The macros check `effects = { reads: [...] }` against a manifest of
# canonical kinds. Point them at this app's copy, so a kind the app adds
# (in schema-refs.json) is a kind the macro accepts. Relative to this workspace.
emit .cargo/config.toml <<'EOF'
[env]
IMPRESS_SCHEMA_REFS = { value = "schema-refs.json", relative = true }
EOF

cp "$KOINE_ROOT/schema-refs.json" "$ROOT/schema-refs.json" \
    || die "cannot copy $KOINE_ROOT/schema-refs.json"

emit Dioxus.toml <<'EOF'
# The page proxy. `koine-web` (crates/koine-web in a koine checkout) is the
# page; `dx serve` there forwards /api to a running host so the page is
# same-origin with it (the host sends no CORS headers). Copy this file over
# crates/koine-web/Dioxus.toml, or pass it with `dx serve --config`, to aim
# the page at __NAME__'s host. The host serves the built page itself under
# /ui/ (open http://localhost:__PORT__/ui/).
[application]
name = "__NAME__"

[web.app]
title = "__NAME__"
# The host serves the bundle under /ui/ and the router reads this as its prefix.
base_path = "ui"

[[web.proxy]]
backend = "http://localhost:__PORT__/api"
EOF

emit "crates/__SERVICE_CRATE__/Cargo.toml" <<'EOF'
[package]
name = "__SERVICE_CRATE__"
description = "Verbs over __LIB__: one #[impress_method] per capability"
version.workspace = true
edition.workspace = true

[dependencies]
__LIB_DEP__
impress-service-core.workspace = true
impress-service-macros.workspace = true
# The macros expand to `::serde::Deserialize` and `JsonSchema` derives, so
# the crate that calls them depends on both itself.
serde.workspace = true
serde_json.workspace = true
schemars.workspace = true
EOF

emit "crates/__SERVICE_CRATE__/src/lib.rs" <<'EOF'
//! `__TRAIT__`: the verbs of `__LIB__`.
//!
//! * `dto.rs`      the argument and result types (docs/capability.md, DTO rules)
//! * `service.rs`  the `#[impress_service]` trait, its implementation, the wiring
//! * `selftest.rs` Tier A: every verb and example, called through the inventory

mod dto;
mod service;

#[cfg(test)]
mod selftest;

pub use dto::*;
pub use service::*;

/// Call this from the host's `main`. The verbs register themselves in a
/// process-wide inventory only in a binary that actually links this crate;
/// referring to a function of it is what makes the linker keep it.
pub fn ensure_linked() {}
EOF

emit "crates/__SERVICE_CRATE__/src/dto.rs" <<'EOF'
//! The types that cross the wire: a verb's arguments and result.
//!
//! Every type here derives `Serialize` (results) or `Deserialize`
//! (arguments) AND `JsonSchema`: the schema is what agents read as the
//! tool's input and output. The shapes that work are listed in
//! docs/capability.md (koine checkout), "DTO rules":
//!
//!   1. structs of owned fields          #[derive(Serialize, Deserialize, JsonSchema)]
//!   2. enums of unit variants           #[serde(rename_all = "kebab-case")]
//!   3. internally tagged enums          #[serde(tag = "kind")]
//!   4. Option<T>, Vec<T>, BTreeMap<String, T>
//!   5. newtypes                         #[serde(transparent)]
//!
//! What does not work, and the wrapper to write instead: a reference or
//! lifetime (an owned DTO), a generic the macro cannot name (a concrete DTO
//! per instantiation), a type from another crate without `JsonSchema` (a DTO
//! with `From`/`TryFrom`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The result of `echo`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EchoReply {
    /// The message, shouted when asked.
    pub reply: String,
    /// The reply's length in characters.
    pub length: u64,
}

// TODO: one DTO per type of `__LIB__` that a verb takes or returns.
EOF

emit "crates/__SERVICE_CRATE__/src/service.rs" <<'EOF'
//! The verbs. One `#[impress_method]` per capability of `__LIB__`; the macro
//! derives the MCP tool, the CLI subcommand and the agent tool from each.
//! A verb's name is `<trait>_<method>` in kebab case: `__TOOL__`.

use impress_service_core::async_trait;
use impress_service_macros::{impress_service, impress_service_impl};
// `#[impress_service]` consumes `#[impress_method]`, so the import reads as unused.
#[allow(unused_imports)]
use impress_service_macros::impress_method;

use crate::dto::EchoReply;

// TODO: one verb per public function of `__LIB__`. A first, crude listing of
// what is there (`cargo doc -p __LIB__ --open` is the authoritative one):
//
__PUBLIC_FNS__
//
// For each: a method on the trait below, with a `///` doc saying what it does
// (it is the description agents read), an `#[impress_example]`, a line in
// `methods = [...]` at the bottom, and a body that calls the library.
// Delete `echo` when the first real verb is in.

/// __PASCAL__: the capabilities of `__LIB__`.
#[impress_service]
pub trait __TRAIT__: Send + Sync + 'static {
    /// Say a message back, optionally shouted. The scaffold's one verb: it
    /// shows the shape of a verb and proves the wiring.
    #[impress_method]
    #[impress_example(
        name = "plain",
        args = r#"{"message": "hello"}"#,
        expect = r#"{"reply": "hello", "length": 5}"#
    )]
    #[impress_example(
        name = "shout",
        args = r#"{"message": "hello", "shout": true}"#,
        expect = r#"{"reply": "HELLO"}"#
    )]
    async fn echo(&self, message: String, shout: Option<bool>) -> EchoReply;
}

/// The implementation. Stateful libraries keep their handle here.
#[derive(Clone, Default)]
pub struct __IMPL__;

impl __IMPL__ {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl __TRAIT__ for __IMPL__ {
    async fn echo(&self, message: String, shout: Option<bool>) -> EchoReply {
        let reply = if shout.unwrap_or(false) {
            message.to_uppercase()
        } else {
            message
        };
        EchoReply {
            length: reply.chars().count() as u64,
            reply,
        }
    }
}

impress_service_impl! {
    service = __TRAIT__,
    impl = __IMPL__,
    instance = __IMPL__::new,
    // The default class of the service's verbs: read_only | mutating |
    // destructive | external. A verb that differs says so on the trait:
    // `#[impress_method(safety = destructive)]`.
    safety = read_only,
    since = "0.1.0",
    // What the verbs touch. Pure computation over their arguments touches
    // nothing, which is what the empty lists say. A verb that reads or
    // writes records names the kinds from schema-refs.json, e.g.
    // `reads: ["bibliography-entry"], writes: [target(id)], reach: [fs]`;
    // one that differs from the rest says so on the trait with
    // `#[impress_method(effects(reads = [...], writes = [...], reach = [...]))]`.
    effects = { reads: [], writes: [], reach: [] },
    methods = [
        /// Say a message back, optionally shouted.
        echo(
            /// The text to say back.
            message: String,
            /// Upper-case the reply. Defaults to false.
            shout: Option<bool>,
        ) -> EchoReply,
    ],
}
EOF

emit "crates/__SERVICE_CRATE__/src/selftest.rs" <<'EOF'
//! Tier A: the verbs, called the way MCP, the CLI and the host call them.
//!
//! Every call goes through `impress_service_core::call` - find the verb in
//! the linked inventory, run it through the pipeline - never through the
//! trait directly, which would not catch a macro-wiring mistake, a missing
//! example or a strict-argument refusal.

use impress_service_core::{call, VerbHandle};
use serde_json::{json, Value};

const SERVICE: &str = "__SERVICE_CRATE__";

fn verbs() -> Vec<VerbHandle> {
    let mut verbs: Vec<VerbHandle> = call::descriptors()
        .filter(|v| v.service() == SERVICE)
        .collect();
    verbs.sort_by(|a, b| a.name().cmp(b.name()));
    verbs
}

/// Every key of `expected` is in `actual` with the same value (objects
/// recurse; anything else must be equal).
fn contains(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => e
            .iter()
            .all(|(key, want)| a.get(key).is_some_and(|got| contains(got, want))),
        _ => actual == expected,
    }
}

#[test]
fn the_service_is_linked_and_every_verb_is_documented() {
    let verbs = verbs();
    assert!(!verbs.is_empty(), "no verb of {SERVICE} in the inventory");
    for verb in verbs {
        assert!(
            !verb.description().is_empty(),
            "{} has no description",
            verb.name()
        );
        assert!(
            !verb.examples().is_empty(),
            "{} has no #[impress_example]: Tier A runs a verb only through its examples",
            verb.name()
        );
    }
}

/// Run every example of every verb and check its `expect`. This is the test
/// that grows with the app: a verb with an example is a verb under test.
#[test]
fn every_example_runs() {
    for verb in verbs() {
        for example in verb.examples() {
            let label = format!("{} / {}", verb.name(), example.name);
            let result = call::call(verb.name(), example.args_value())
                .unwrap_or_else(|e| panic!("{label}: {e}"));
            assert_ne!(result["ok"], json!(false), "{label} was refused: {result}");
            if let Some(expect) = example.expect {
                let expect: Value = serde_json::from_str(expect).expect("expect is JSON");
                assert!(
                    contains(&result, &expect),
                    "{label}: {result} does not contain {expect}"
                );
            }
        }
    }
}

#[test]
fn echo_says_it_back() {
    let result = call::call(
        "__TOOL__",
        json!({ "message": "hi", "shout": true }),
    )
    .unwrap();
    assert_eq!(result["reply"], json!("HI"));
    assert_eq!(result["length"], json!(2));
}

/// Verbs are strict: an argument the schema does not name is refused, not
/// ignored (a typo must not read as a default).
#[test]
fn a_misspelt_argument_is_refused() {
    let result = call::call("__TOOL__", json!({ "mesage": "hi" })).unwrap();
    assert_eq!(result["ok"], json!(false), "{result}");
    assert_eq!(result["code"], json!("invalid-argument"), "{result}");
}
EOF

emit "crates/__HOST_CRATE__/Cargo.toml" <<'EOF'
[package]
name = "__HOST_CRATE__"
description = "The host of __NAME__: links __SERVICE_CRATE__ and serves its verbs"
version.workspace = true
edition.workspace = true

[dependencies]
__SERVICE_CRATE__ = { path = "../__SERVICE_CRATE__" }
impress-service-core.workspace = true
# K4: koine-host.workspace = true
EOF

emit "crates/__HOST_CRATE__/src/main.rs" <<'EOF'
//! The host of __NAME__.
//!
//! Linking `__SERVICE_CRATE__` is what puts its verbs in the process-wide
//! inventory; everything a host serves is a call into that inventory.

use impress_service_core::call;

fn main() {
    __SERVICE_SNAKE__::ensure_linked();

    // K4: koine-host does not exist yet. When it does, this whole body is
    //
    //     koine_host::serve(koine_host::Config {
    //         port: __PORT__,
    //         store: "dir:./data".into(),
    //         ..Default::default()
    //     })
    //
    // which serves POST /api/verb/<name>, /api/surface/*, /ui/ and /mcp over
    // this same inventory. Until then, list what is linked.
    let mut verbs: Vec<_> = call::descriptors().collect();
    verbs.sort_by(|a, b| a.name().cmp(b.name()));
    println!("__NAME__: {} verbs linked", verbs.len());
    for verb in verbs {
        let first_line = verb.description().lines().next().unwrap_or("");
        println!(
            "  {:<44} {:<12} {first_line}",
            verb.name(),
            verb.safety().class.to_string()
        );
    }
}
EOF

emit README.md <<'EOF'
# __NAME__

A bespoke app over [`__LIB__`](https://crates.io/crates/__LIB__), built on koine
(__KOINE_WHERE__). Its verbs are `__SERVICE_CRATE___*`; each is one MCP tool, one CLI
subcommand and one agent tool, from one `#[impress_method]`.

The guide is `docs/capability.md` in the koine checkout. Tick these in order.

1. **List the library's public API.** `cargo doc -p __LIB__ --open`, or the
   crude list `service.rs` carries in its TODO. Every public function is a
   candidate verb; every public type that crosses a verb is a DTO.
2. **One verb per capability.** A method on `__TRAIT__` in
   `crates/__SERVICE_CRATE__/src/service.rs`, `#[impress_method]`, a `///` doc
   saying what it does, an `#[impress_example]`, and a line in `methods = [...]`.
   Delete `echo` once the first real verb is in.
3. **DTOs per the rules.** Owned types deriving `Serialize`/`Deserialize` and
   `JsonSchema` in `dto.rs`. A reference, a generic or a foreign type gets a
   wrapper DTO with `From`/`TryFrom`; the macro's error says which.
4. **Effects and safety.** The `effects = {...}` and `safety = ...` lines of
   `impress_service_impl!`: what the verbs read, write and reach, and the worst
   thing each can do (`read_only`, `mutating`, `destructive`, `external`).
5. **Selftest.** `crates/__SERVICE_CRATE__/src/selftest.rs` runs every
   `#[impress_example]` through the inventory. A verb with no example fails it.
6. **`cargo test`.** The whole workspace; it must be green before the host is.
   To check nothing public was left behind, run koine's
   `scripts/check-verb-coverage.sh --lib <library dir> --service
   crates/__SERVICE_CRATE__` and either cover each item or give it a reasoned
   `[[exclude]]` line in `crates/__SERVICE_CRATE__/verb-exclusions.toml`.
7. **Run the host.** `cargo run -p __HOST_CRATE__` lists the linked verbs now.
   With `koine-host` (plan-koine-host K4) the same command serves them on
   port __PORT__; the K4 marker in `crates/__HOST_CRATE__/src/main.rs` is the
   one line to change.
8. **Open `/ui/`.** http://localhost:__PORT__/ui/ shows the generated surface
   of every verb. Author a surface by hand only for a layout the generated one
   cannot say (docs/capability.md, "What the generated surface shows").

`schema-refs.json` is the manifest `effects` kinds are checked against
(`.cargo/config.toml` points the macros at it). Add a kind of record your app
stores to `canonical` before naming it in `reads`/`writes`.
EOF

# ---------------------------------------------------------------- publish
mv "$ROOT" "./$NAME"
echo "wrote ./$NAME  (koine: $KOINE_WHERE; library: $LIB)"
echo "  verbs:  ${TOOL%_echo}_*   (scaffold: $TOOL)"
echo "  next:   cd $NAME && cargo test      # then read $NAME/README.md"
