//! `run-service`: the run convention's koine side
//! (docs/plan-run-convention.md R-D13; work package R6). Generic over any
//! code that follows the convention — no per-code wrapper — because
//! everything a code is to koine is in the files it writes and the schema
//! it emits:
//!
//! - [`kind`] — the one new kind, `run@1.0.0`: a `run.json` as the item it
//!   already is, with what koine derives beside it. A run's events are
//!   `task-event@1.0.0` rows under the run id with the run's own `seq`.
//! - [`codes`] — the codes this host can start: name, emitted parameter
//!   schema, launch template ([`CodeProvider`]). What R3 (titan) and R7
//!   (the GADGET-4 adapter) hand koine.
//! - [`ingest`] — a conforming directory into the store: the row, the ring,
//!   the outputs; a `3xx` exit files a `diagnose` request on the desk.
//! - [`launch`] — start a code and follow it.
//! - [`control`] — the control inbox from the requester's side (R-D8).
//! - [`sweep`] — R-D10 as a koine-tasks workflow.
//! - [`form`] — the generated start form, grouped by `x-run.group`.
//! - [`service`] — the verbs: `run-service_{codes, register-code, ingest,
//!   list, status, events, start, set, stop, checkpoint, resume, sweep, form}`.
//! - [`demo_code`] — the one built-in code, for proof (`src/bin/demo-code.rs`).

pub mod codes;
pub mod control;
pub mod demo_code;
pub mod dto;
pub mod form;
pub mod ingest;
pub mod kind;
pub mod launch;
pub mod service;
pub mod sweep;

#[cfg(test)]
mod selftest;

pub use codes::{CodeProvider, DirVia, Launch, ParamsVia};
pub use service::{row_of, DefaultRunService, RunService};

/// Keep the verbs' inventory registrations in a host that names nothing
/// else of this crate, register the kind, and register the demo code when
/// its binary is beside this one.
pub fn ensure_linked() {
    std::hint::black_box(DefaultRunService::new as *const ());
    kind::ensure_registered();
    demo_code::register();
}
