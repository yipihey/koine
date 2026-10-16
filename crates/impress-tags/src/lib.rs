//! impress-tags: Hierarchical tag models, autocomplete, and query.
//!
//! Tags represent knowledge categorization (topics, methods, domains).
//! They use hierarchical paths like `methods/sims/hydro/AMR`.
//! Tags sync across devices AND export to BibTeX as `keywords`.

#[cfg(feature = "ffi")]
uniffi::setup_scaffolding!();

pub mod alias;
pub mod autocomplete;
pub mod config;
pub mod hierarchy;
pub mod parse;
pub mod query;
pub mod tag;

pub use alias::*;
pub use autocomplete::*;
pub use config::*;
pub use hierarchy::*;
pub use parse::*;
pub use query::*;
pub use tag::*;
