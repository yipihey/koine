//! Async runtime helpers for sync contexts (CLI binaries, FFI shims).
//!
//! The codegen layer treats every service method as `async`, but the CLI
//! dispatch path (clap) and some FFI shims are inherently synchronous. They
//! use [`block_on`] to run a future to completion using a process-wide
//! lazily-initialized multi-threaded Tokio runtime.
//!
//! If the caller is already inside a Tokio runtime context (e.g. a binding
//! invoked from inside a `tokio::spawn`), prefer awaiting the future directly
//! rather than calling [`block_on`], which would panic.

use std::future::Future;
use std::sync::OnceLock;

use tokio::runtime::{Builder, Runtime};

fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        Builder::new_multi_thread()
            .enable_all()
            .thread_name("impress-service")
            .build()
            .expect("impress-service runtime must initialize")
    })
}

/// Run a future to completion on the shared impress-service runtime.
///
/// Safe to call from any thread that is NOT already inside a Tokio runtime.
pub fn block_on<F>(future: F) -> F::Output
where
    F: Future,
{
    runtime().block_on(future)
}

/// Spawn a future on the shared impress-service runtime, returning a join
/// handle. Useful for fire-and-forget background work initiated from a sync
/// context.
pub fn spawn<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    runtime().spawn(future)
}
