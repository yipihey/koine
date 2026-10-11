//! The write-policy hook.
//!
//! Some record kinds refuse some writes by rule of the *schema*, not of the
//! store: `SqliteItemStore` refuses payload operations on
//! `manuscript-revision` and `manuscript-change` items (an immutable
//! history). That rule is impress's, and a backend that carried it would be
//! a backend that knew impress's kinds. So a backend takes a [`WritePolicy`]
//! when it is built and asks it before every operation; the default says yes.

use impress_item::{Item, OperationType, StoreError};

/// A rule about which operations a target row accepts.
///
/// [`check`](WritePolicy::check) runs before an operation is recorded or
/// materialized, on every path that applies one (`update`, `apply_operation`
/// and both guarded forms). Returning an error refuses the operation: nothing
/// is written, no clock advances, and the error is the caller's. Use
/// [`StoreError::Validation`] for "this kind does not accept that".
pub trait WritePolicy: Send + Sync {
    /// May `op` be applied to `target`? The default allows everything.
    fn check(&self, _target: &Item, _op: &OperationType) -> Result<(), StoreError> {
        Ok(())
    }
}

/// The default policy: every operation is accepted.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoPolicy;

impl WritePolicy for NoPolicy {}
