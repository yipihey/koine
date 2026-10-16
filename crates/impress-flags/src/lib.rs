//! impress-flags: Flag (workflow state) models, parsing, and query.
//!
//! Flags represent ephemeral workflow state (triage priority, review status).
//! They sync across devices but are NOT exported to BibTeX.
//!
//! # Grammar
//!
//! Flag commands use a compact shorthand:
//! - First char: color (`r`ed, `a`mber, `b`lue, `g`ray)
//! - Optional second: style (`s`olid, `-` dashed, `.` dotted)
//! - Optional third: length (`f`ull, `h`alf, `q`uarter)
//!
//! Examples: `r` (red solid full), `a-h` (amber dashed half), `b.q` (blue dotted quarter)

#[cfg(feature = "ffi")]
uniffi::setup_scaffolding!();

pub mod config;
pub mod flag;
pub mod parse;
pub mod query;

pub use config::*;
pub use flag::*;
pub use parse::*;
pub use query::*;
