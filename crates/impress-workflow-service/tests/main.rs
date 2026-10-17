// Force-links this crate's own inventory statics into the test binary — the
// same mechanism `impress-capabilities`'s module docs describe: nothing in
// `tests/proof.rs` calls `impress_workflow_service::*` directly (every call
// goes through `VerbDescriptor::find` by name), so without this the linker
// is free to drop the whole rlib for lack of a real reference and every
// `find` in the suite panics.
#[allow(unused_imports)]
use impress_workflow_service as _force_link_impress_workflow_service;

mod proof;
