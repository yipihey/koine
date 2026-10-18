//! `RegexLabService`: the verbs of `regex`.
//!
//! * `dto.rs`      the argument and result types (docs/capability.md, DTO rules)
//! * `compile.rs`  flags, the compiled-pattern cache, positioned refusals
//! * `explain.rs`  a pattern's syntax tree in words
//! * `library.rs`  saved patterns: `regex-pattern` records in the store
//! * `service.rs`  the `#[impress_service]` trait, its implementation, the wiring
//! * `selftest.rs` Tier A: every verb and example, called through the inventory

mod compile;
mod dto;
mod explain;
mod library;
mod service;

#[cfg(test)]
mod selftest;

pub use compile::{Flags, FLAG_LETTERS};
pub use dto::*;
pub use library::{record_id, KIND};
pub use service::*;

/// Call this from the host's `main`. The verbs register themselves in a
/// process-wide inventory only in a binary that actually links this crate;
/// referring to a function of it is what makes the linker keep it.
pub fn ensure_linked() {}
