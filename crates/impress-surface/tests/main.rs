//! One integration-test binary for impress-surface (plan-verb-pipeline-and-transport
//! § Build cost, B1): every file in this directory is a module here, so the
//! crate's tests compile and link once instead of once per file. A test keeps
//! its file's name as its module path: `cargo test -p impress-surface -- <file>::<test>`.

mod contract;
mod golden;
mod reduce_properties;
mod schema_tests;
mod source_errors;
mod spec_round_trip;
