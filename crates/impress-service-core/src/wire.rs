//! The wire convention's version.
//!
//! Every layout and surface verb result, and every `/api/layout/*` and
//! `/api/surface/*` HTTP body, is snake_case, carries `"wire_version"`, and
//! refuses with `{"ok": false, "code", "message"}` (review AC-F24). The
//! number moves only when a shape changes in a way an older reader would
//! misread; adding an optional field does not move it.

/// The current wire convention.
pub const WIRE_VERSION: u32 = 1;

/// [`WIRE_VERSION`], for `#[serde(default = "…")]`.
pub fn wire_version() -> u32 {
    WIRE_VERSION
}
