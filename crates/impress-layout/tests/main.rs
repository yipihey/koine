//! One integration-test binary for impress-layout (plan-verb-pipeline-and-transport
//! § Build cost, B1): every file in this directory is a module here, so the
//! crate's tests compile and link once instead of once per file. A test keeps
//! its file's name as its module path: `cargo test -p impress-layout -- <file>::<test>`.

mod common;
mod golden;
mod normalize;
mod properties;
mod sessions;
mod undo;
mod verbs;
