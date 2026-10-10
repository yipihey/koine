//! Optional process-owned routing at the pipeline's invocation step.
//!
//! Native app FFIs leave this unset and call their local backend. Client
//! processes install the shared app transport. A router may return `None`
//! only before making the call, to select an explicitly allowed local
//! fallback. A remote refusal must stay a refusal, never trigger a retry.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use crate::{BoxError, VerbDescriptor};
use serde_json::Value;

pub type RouteFuture =
    Pin<Box<dyn Future<Output = Option<Result<Value, BoxError>>> + Send + 'static>>;

pub trait Router: Send + Sync {
    fn route(&self, verb: &'static VerbDescriptor, args: Value) -> RouteFuture;
}

static ROUTER: RwLock<Option<Arc<dyn Router>>> = RwLock::new(None);

pub fn install(router: Arc<dyn Router>) {
    *ROUTER.write().unwrap_or_else(|error| error.into_inner()) = Some(router);
}

pub(super) fn current() -> Option<Arc<dyn Router>> {
    ROUTER
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}
