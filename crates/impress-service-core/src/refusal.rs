//! Why a verb did not do what it was asked, in a form a program can branch on.
//!
//! Every result envelope the layout and surface services return carries `ok`
//! and a prose `message`. Before wave 7 a refusal was ONLY prose, so an agent,
//! a script or the Swift host could not tell "no such pane" from "the store is
//! not open" without matching English (review RL-L11, AC-F19). A refusal now
//! also carries a stable, kebab-case `code`:
//!
//! * the **domain** codes are the serde tags the refusing type already has —
//!   `impress_layout::LayoutError`'s `unknown-tile`, `no-pane-with-role`,
//!   `cannot-close-last-pane`, …; `impress_surface::ReduceError`'s
//!   `unknown-widget`, `not-bindable`, … — so the vocabulary is written down
//!   once, where the refusal is decided;
//! * the **generic** codes below cover what no domain type decides: a
//!   malformed argument, a missing record, a lost race, a store that is not
//!   there.
//!
//! [`http_status`] is the one mapping from a code to an HTTP status, so a
//! route never infers a status from message text again.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The generic codes. Domain codes are the refusing type's own serde tags.
pub mod codes {
    /// An argument did not parse or named nothing sensible: a malformed id,
    /// an unknown direction, spec JSON that is not a spec.
    pub const INVALID_ARGUMENT: &str = "invalid-argument";
    /// The record the call names does not exist: no such surface, no saved
    /// layout by that name, no preset.
    pub const NOT_FOUND: &str = "not-found";
    /// Someone else wrote first: a stale `expected_revision`, a live layout
    /// row that moved under the write. Nothing was written; read and retry.
    pub const CONFLICT: &str = "conflict";
    /// The real store could not be opened and this process is running on the
    /// in-memory stand-in (review AC-F20). A write would vanish, so it is
    /// refused.
    pub const STORE_UNAVAILABLE: &str = "store-unavailable";
    /// The store answered with an error: an I/O failure, a row that would not
    /// decode.
    pub const STORE_ERROR: &str = "store-error";
    /// The app that owns a verb is not running, so the verb could not be
    /// reached.
    pub const HOST_UNAVAILABLE: &str = "host-unavailable";
    /// A verb ran and refused, or failed in transport.
    pub const VERB_FAILED: &str = "verb-failed";
    /// A surface dispatch was reduced and its state saved, but at least one
    /// of its effects failed; `effects` says which.
    pub const EFFECT_FAILED: &str = "effect-failed";
    /// The refusal is the service's own fault: an encode that failed, a task
    /// that panicked.
    pub const INTERNAL: &str = "internal";
    /// `history-service_replay` was asked to re-run a call whose arguments
    /// were not recorded in full — privacy-reduced or compacted — so there
    /// is nothing to replay it with (plan-self-reflective-layer § Call log).
    pub const NOT_REPLAYABLE: &str = "not-replayable";
}

/// A refusal: a stable `code` and the prose `message` a person reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Refusal {
    pub code: String,
    pub message: String,
}

impl Refusal {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(codes::INVALID_ARGUMENT, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(codes::NOT_FOUND, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(codes::CONFLICT, message)
    }

    pub fn store_unavailable(message: impl Into<String>) -> Self {
        Self::new(codes::STORE_UNAVAILABLE, message)
    }

    pub fn store(message: impl fmt::Display) -> Self {
        Self::new(codes::STORE_ERROR, message.to_string())
    }

    pub fn internal(message: impl fmt::Display) -> Self {
        Self::new(codes::INTERNAL, message.to_string())
    }

    /// The same refusal with its message prefixed — "close {role: detail}: "
    /// in front of what the tree said — keeping the code.
    pub fn context(mut self, prefix: impl fmt::Display) -> Self {
        self.message = format!("{prefix}: {}", self.message);
        self
    }

    /// See [`http_status`].
    pub fn http_status(&self) -> u16 {
        http_status(&self.code)
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refusal {}

/// A bare message is an argument the service could not use. Every argument
/// parser in the layout and surface services answers `Err(String)`, so this is
/// the conversion `?` performs for them; refusals of any other kind are built
/// with their own code, never through this.
impl From<String> for Refusal {
    fn from(message: String) -> Self {
        Self::invalid_argument(message)
    }
}

impl From<&str> for Refusal {
    fn from(message: &str) -> Self {
        Self::invalid_argument(message)
    }
}

/// The prose alone, for a caller whose own errors are strings (the self-test
/// catalogues). The code is dropped, so a result envelope never goes through
/// this.
impl From<Refusal> for String {
    fn from(refusal: Refusal) -> Self {
        refusal.message
    }
}

/// The HTTP status for a code: generic codes by meaning, and every domain
/// code (a refusal by the tree or the reducer) 422 — the request was
/// understood and the thing it asked for is not allowed.
pub fn http_status(code: &str) -> u16 {
    match code {
        codes::INVALID_ARGUMENT => 400,
        codes::NOT_FOUND => 404,
        codes::CONFLICT | "undo-conflict" => 409,
        codes::STORE_UNAVAILABLE | codes::HOST_UNAVAILABLE => 503,
        codes::VERB_FAILED => 502,
        codes::STORE_ERROR | codes::INTERNAL => 500,
        _ => 422,
    }
}

/// What a CLI exits with when the verb answered `"ok": false` — distinct from
/// 1 (the verb could not be dispatched at all) and 2 (bad invocation or an
/// unreachable store), so a script can tell "the verb said no" from "the
/// verb never ran" (review AC-F12).
pub const EXIT_REFUSED: i32 = 3;

/// The exit status for a verb's JSON answer: [`EXIT_REFUSED`] when its
/// top-level `ok` is `false`, else 0. A result with no `ok` field (a schema,
/// a list of examples) is a success.
pub fn exit_status(result: &serde_json::Value) -> i32 {
    match result.get("ok") {
        Some(serde_json::Value::Bool(false)) => EXIT_REFUSED,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_follow_the_code_not_the_message() {
        assert_eq!(http_status(codes::NOT_FOUND), 404);
        assert_eq!(http_status(codes::CONFLICT), 409);
        assert_eq!(http_status(codes::STORE_ERROR), 500);
        assert_eq!(http_status(codes::STORE_UNAVAILABLE), 503);
        assert_eq!(http_status("cannot-close-last-pane"), 422);
        assert_eq!(http_status("undo-conflict"), 409);
    }

    #[test]
    fn a_refused_result_exits_three_and_anything_else_zero() {
        let refused = serde_json::json!({"ok": false, "code": "not-found", "message": "no"});
        assert_eq!(exit_status(&refused), EXIT_REFUSED);
        assert_eq!(exit_status(&serde_json::json!({"ok": true})), 0);
        assert_eq!(exit_status(&serde_json::json!({"schema": {}})), 0);
        assert_eq!(exit_status(&serde_json::json!([1, 2])), 0);
    }

    #[test]
    fn context_keeps_the_code() {
        let r = Refusal::not_found("no surface x").context("surface_show");
        assert_eq!(r.code, codes::NOT_FOUND);
        assert_eq!(r.message, "surface_show: no surface x");
    }
}
