//! The run convention (`docs/plan-run-convention.md`): the JSON Schemas of a
//! run directory, RFC 8785 canonicalisation, the run key, and `check_dir`, the
//! conformance checker behind the `run-check` binary.

mod canonical;
mod check;
pub mod import;

pub use canonical::{canonical_json, key_of, values_of};
pub use check::{check_dir, check_parameter_schema, Finding, Report};

/// The schemas, as JSON text, so a consumer needs no file path.
pub mod schemas {
    pub const RUN: &str = include_str!("../schemas/run.schema.json");
    pub const PARAMS: &str = include_str!("../schemas/params.schema.json");
    pub const EVENT: &str = include_str!("../schemas/event.schema.json");
    pub const OUTPUTS: &str = include_str!("../schemas/outputs.schema.json");
    pub const SWEEP: &str = include_str!("../schemas/sweep.schema.json");
    pub const CONTROL: &str = include_str!("../schemas/control.schema.json");
    pub const PARAMETER_SCHEMA: &str = include_str!("../schemas/parameter-schema.schema.json");
}
