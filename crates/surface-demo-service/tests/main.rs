//! One integration-test binary for surface-demo-service (plan-verb-pipeline-and-transport
//! § Build cost, B1): every file in this directory is a module here, so the
//! crate's tests compile and link once instead of once per file. A test keeps
//! its file's name as its module path: `cargo test -p surface-demo-service -- <file>::<test>`.

mod concierge;
mod example_surface;
mod tour;
