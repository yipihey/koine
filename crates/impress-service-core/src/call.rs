//! Running a linked verb by its MCP tool name.
//!
//! The one copy of "find the descriptor, run it through the pipeline". It
//! lives here rather than in `impress-capabilities` because
//! `impress-surface-service` runs verbs too (a surface's sources and `call`
//! effects), and the kit depends on that crate, so the crate cannot depend
//! back on the kit (review RS-S21). Every crate that links an inventory
//! already depends on this one.
//!
//! `impress-capabilities` re-exports all of it, and `impress-capabilities`
//! re-exports the kit's, so every existing caller compiles unchanged. Since
//! P2 every call here goes through [`crate::pipeline::invoke`]; the caller
//! identity is the entry path's to state — [`call`] and [`call_async`] keep
//! their signatures and run as `Agent("unknown")` unless a call context is
//! already current, which the pipeline then inherits.

use serde_json::Value;

use crate::pipeline::{self, Call, CallerIdentity, PipelineError};
use crate::{descriptor_handle::VerbHandle, registry_runtime, runtime};

/// Error from [`call`] / [`call_async`].
///
/// Mirrors the two failure modes `impress-mcp::inventory_bridge` returned as
/// plain strings before the `impress-capabilities` move (ADR-0033 D4 plan
/// S2), preserved again here (`"Unknown tool: {name}"` and
/// `"{descriptor}: {handler error}"`); `Display` on this type reproduces
/// those exact strings so callers that used to `.to_string()` the old
/// `Result<Value, String>` see byte-identical error text. `Unavailable` is
/// the reachability layer's refusal, in the words impress-mcp always used.
#[derive(Debug, thiserror::Error)]
pub enum CallError {
    /// No descriptor with this name is registered — either a typo, or the
    /// crate that would have linked it is not linked into this binary.
    #[error("Unknown tool: {0}")]
    UnknownTool(String),
    /// The verb's app is not running.
    #[error("{0}")]
    Unavailable(String),
    /// The descriptor's handler future resolved to `Err`. The message is
    /// already formatted as `"{descriptor.name}: {handler error}"`.
    #[error("{0}")]
    Handler(String),
}

impl CallError {
    fn from_pipeline(name: &str, error: PipelineError) -> Self {
        match error {
            PipelineError::Unavailable { .. } => CallError::Unavailable(error.to_string()),
            PipelineError::Handler(e) => CallError::Handler(format!("{name}: {e}")),
        }
    }
}

/// Every MCP tool descriptor linked into this binary — the one process-wide
/// `inventory` collection, not a subset of it.
pub fn descriptors() -> impl Iterator<Item = VerbHandle> {
    registry_runtime::current().descriptors().into_iter()
}

/// Look up one descriptor by its exact MCP tool name
/// (`"surface-demo-service_series"`, kebab-case method ident), or by one of
/// its retired aliases (P3, plan-verb-pipeline § Lifecycle) — a name that
/// still dispatches, but is never advertised: `tools/list` and the CLI's own
/// listing read [`crate::descriptor::VerbDescriptor::aliases`] and print the
/// canonical name only.
pub fn find(name: &str) -> Option<VerbHandle> {
    registry_runtime::current().find(name)
}

/// The identity [`call`] and [`call_async`] run as when nothing says
/// otherwise.
fn unknown_agent() -> CallerIdentity {
    CallerIdentity::agent("unknown")
}

/// Invoke an inventory tool synchronously, running it to completion on the
/// shared `impress-service` runtime ([`runtime::block_on`]).
///
/// Use this from a synchronous context (CLI dispatch, an FFI shim); use
/// [`call_async`] from a context already running on a Tokio runtime, where
/// `block_on` would panic. Prefer [`call_as`] where the entry path knows who
/// is calling.
pub fn call(name: &str, args: Value) -> Result<Value, CallError> {
    call_as(name, unknown_agent(), args)
}

/// [`call`] with the caller's identity stated.
pub fn call_as(name: &str, caller: CallerIdentity, args: Value) -> Result<Value, CallError> {
    let descriptor = find(name).ok_or_else(|| CallError::UnknownTool(name.to_string()))?;
    let call = call_for(&descriptor, name, caller, args);
    runtime::block_on(pipeline::invoke_handle(descriptor.clone(), call))
        .map_err(|e| CallError::from_pipeline(descriptor.name(), e))
}

/// A [`Call`] carrying `name` as its `requested_name` when `name` is not
/// `descriptor.name` — i.e. `name` resolved through an alias.
fn call_for(descriptor: &VerbHandle, name: &str, caller: CallerIdentity, args: Value) -> Call {
    let call = Call::new(caller, args);
    if descriptor.name() == name {
        call
    } else {
        call.with_requested_name(name)
    }
}

/// The `async` counterpart to [`call`], for callers already on the runtime.
pub async fn call_async(name: &str, args: Value) -> Result<Value, CallError> {
    call_async_as(name, unknown_agent(), args).await
}

/// [`call_async`] with the caller's identity stated.
pub async fn call_async_as(
    name: &str,
    caller: CallerIdentity,
    args: Value,
) -> Result<Value, CallError> {
    let descriptor = find(name).ok_or_else(|| CallError::UnknownTool(name.to_string()))?;
    let call = call_for(&descriptor, name, caller, args);
    pipeline::invoke_handle(descriptor.clone(), call)
        .await
        .map_err(|e| CallError::from_pipeline(descriptor.name(), e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::{Deprecation, Safety, SafetyClass, Source};
    use crate::{Effects, McpToolDescriptor, ServiceFuture, VerbDescriptor};
    use serde_json::json;

    fn schema() -> Value {
        json!({"type": "object"})
    }
    fn handler(args: Value) -> ServiceFuture {
        Box::pin(async move { Ok(json!({"ok": true, "echo": args})) })
    }
    static RENAMED: VerbDescriptor = VerbDescriptor {
        name: "call-test-service_renamed",
        service: "call-test-service",
        method: "renamed",
        description: "d",
        input_schema: schema,
        output_schema: schema,
        safety: Safety {
            class: SafetyClass::ReadOnly,
            idempotent: true,
        },
        effects: Effects::NONE,
        since: "0.1.0",
        deprecated: Some(Deprecation {
            since: "0.9.0",
            alias_of: None,
            note: "renamed; use call-test-service_renamed",
        }),
        aliases: &["call-test-service_old-name"],
        examples: &[],
        strict: false,
        budget_ms: None,
        replay_full: false,
        source: Source::Linked,
        handler,
    };
    inventory::submit! { McpToolDescriptor::of(&RENAMED) }

    #[test]
    fn find_resolves_an_alias_to_its_verb() {
        let direct = find("call-test-service_renamed").expect("direct name resolves");
        let via_alias = find("call-test-service_old-name").expect("alias resolves");
        assert_eq!(direct.name(), "call-test-service_renamed");
        assert_eq!(
            via_alias.name(),
            "call-test-service_renamed",
            "the alias resolves to the SAME (canonical) descriptor"
        );
    }

    #[test]
    fn calling_by_its_canonical_name_gets_no_deprecation_notice() {
        let result = call("call-test-service_renamed", json!({})).expect("call succeeds");
        assert!(result.get("deprecated").is_none(), "{result}");
    }

    #[test]
    fn calling_by_the_retired_alias_answers_with_a_deprecation_notice() {
        let result = call("call-test-service_old-name", json!({"n": 1})).expect("call succeeds");
        assert_eq!(
            result["deprecated"],
            json!({
                "since": "0.9.0",
                "use": "call-test-service_renamed",
                "note": "renamed; use call-test-service_renamed",
            })
        );
        // The rest of the envelope — including the arguments the handler saw
        // — is unchanged.
        assert_eq!(result["echo"], json!({"n": 1}));
        assert_eq!(result["ok"], true);
    }

    #[test]
    fn an_unknown_name_is_still_unknown_tool() {
        let err = call("call-test-service_nope", json!({})).unwrap_err();
        assert!(matches!(err, CallError::UnknownTool(_)));
    }
}
