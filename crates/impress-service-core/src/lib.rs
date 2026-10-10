//! Runtime support for the impress service codegen pipeline.
//!
//! This crate provides the building blocks used by the
//! [`impress-service-macros`](../impress_service_macros/index.html) procedural
//! macros to turn a single trait declaration into one [`VerbDescriptor`] per
//! method (ADR-0034 D1), projected as:
//!
//! * an [`McpToolDescriptor`] registered via [`inventory`] (MCP server pickup),
//! * a [`CliSubcommand`] registered via [`inventory`] (CLI binary pickup).
//!
//! The Swift bindings are exported explicitly by the FFI crates, and Python
//! uses one generic binding (plan-verb-pipeline P6); neither is generated here.

#![forbid(unsafe_code)]

use std::error::Error as StdError;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

pub mod call;
pub mod descriptor;
pub mod descriptor_handle;
pub mod dispatch;
pub mod job;
pub mod lifecycle;
pub mod pipeline;
pub mod provider;
pub mod refusal;
pub mod registry_runtime;
pub mod report;
pub mod runtime;
pub mod strict;
pub mod wire;

pub use descriptor::{
    method_meta, resolve_aliases, resolve_budget_ms, resolve_deprecated, resolve_effects,
    resolve_examples, resolve_idempotent, resolve_replay_full, resolve_safety_class, Deprecation,
    Effects, Example, ExampleTier, Kind, MethodMeta, Reach, Safety, SafetyClass, Source,
    VerbDescriptor,
};
pub use descriptor_handle::{ExampleView, HandleSource, VerbHandle};
pub use provider::{
    ProviderConnection, ProviderExample, ProviderExampleInput, ProviderIdentityInput,
    ProviderPersistence, ProviderStatus, ProviderVerb, ProviderVerbInput, RegistrationError,
    RegistrationReceipt, RegistrationRequest, Registry, SafetyClaim, SchemaValidator,
};
pub use refusal::Refusal;

#[cfg(feature = "cli")]
pub mod cli;

// Re-export crates that generated code depends on so callers do not have to
// pin matching versions in their own Cargo.toml files.
pub use async_trait;
pub use inventory;
pub use schemars;
pub use serde_json;
pub use tokio;

/// Boxed error returned by service handlers at the FFI boundary.
pub type BoxError = Box<dyn StdError + Send + Sync + 'static>;

/// Future returned by an [`McpToolDescriptor`] handler.
pub type ServiceFuture = Pin<Box<dyn Future<Output = Result<serde_json::Value, BoxError>> + Send>>;

/// Trait every service-level error should implement.
///
/// The codegen layer uses this to map a single error into:
/// * a Swift `LocalizedError` (`user_message` powers `errorDescription`),
/// * a Python exception (the `code` becomes the exception type),
/// * an MCP error object (`code` + `user_message`),
/// * a CLI exit code (via `exit_code`).
pub trait ServiceError: StdError + Send + Sync + 'static {
    /// Short machine-readable code, kebab-case (e.g. `"not-found"`).
    fn code(&self) -> &str;

    /// Human-readable, end-user-friendly message.
    fn user_message(&self) -> String;

    /// Exit code to use when the CLI surfaces this error. Defaults to `1`.
    fn exit_code(&self) -> i32 {
        1
    }
}

/// A simple [`ServiceError`] implementation suitable for prototyping and
/// generated default error types.
#[derive(Debug, Clone)]
pub struct BasicServiceError {
    code: String,
    message: String,
    exit_code: i32,
}

impl BasicServiceError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            exit_code: 1,
        }
    }

    pub fn with_exit_code(mut self, exit_code: i32) -> Self {
        self.exit_code = exit_code;
        self
    }
}

impl fmt::Display for BasicServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl StdError for BasicServiceError {}

impl ServiceError for BasicServiceError {
    fn code(&self) -> &str {
        &self.code
    }

    fn user_message(&self) -> String {
        self.message.clone()
    }

    fn exit_code(&self) -> i32 {
        self.exit_code
    }
}

/// Resolve a tool description at compile time.
///
/// The description a model sees can come from two places, and for most of the
/// inventory's life it came from neither:
///
/// 1. a `///` comment inside `impress_service_impl! { methods = [...] }`, or
/// 2. the `///` comment on the trait method, captured by `#[impress_service]`
///    into a `__IMPRESS_SERVICE_METHODS_*` table ([`MethodMeta`]).
///
/// Only (1) used to be read, so any service that documented its trait — which
/// is where a Rust developer naturally writes it, and where every service but
/// the `*-text-service` ones did — silently shipped `"Invoke Service.method"`.
/// 119 of 133 tools were in that state. This resolves (1), then (2), then the
/// fallback, so the docs can live where they belong and still reach the model.
///
/// `const fn` because `inventory::submit!` builds a `static`.
pub const fn resolve_description(
    inline_doc: &'static str,
    table: &'static [MethodMeta],
    method: &'static str,
    fallback: &'static str,
) -> &'static str {
    if !inline_doc.is_empty() {
        return inline_doc;
    }
    match method_meta(table, method) {
        Some(meta) if !meta.doc.is_empty() => meta.doc,
        _ => fallback,
    }
}

/// Mark the named properties of an input schema `"x-private": true`
/// (ADR-0036 D2's privacy rule, hook H-P1-2): the call log stores such an
/// argument as a length and a hash, never by value. The macro calls this on
/// the generated schema for every `#[impress_private]` argument, so the
/// schema — not a second list — is what the log reads.
pub fn mark_private(mut schema: serde_json::Value, private: &[&str]) -> serde_json::Value {
    if private.is_empty() {
        return schema;
    }
    if let Some(props) = schema
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    {
        for name in private {
            if let Some(prop) = props
                .get_mut(*name)
                .and_then(serde_json::Value::as_object_mut)
            {
                prop.insert("x-private".to_string(), serde_json::Value::Bool(true));
            }
        }
    }
    schema
}

/// `&str` equality usable in a const context.
pub(crate) const fn const_str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// The MCP projection of a [`VerbDescriptor`].
///
/// Generated `inventory::submit!` blocks register one of these per method
/// (built by [`McpToolDescriptor::of`] from the method's `static`
/// descriptor), and the MCP server simply iterates [`McpToolDescriptor::iter`]
/// to publish `tools/list` and dispatch `tools/call`. The fields repeat the
/// descriptor's so the readers that predate ADR-0034 D1 compile unchanged;
/// `verb` is the whole record (safety, output schema, `since`, examples).
pub struct McpToolDescriptor {
    /// Tool name as seen by the MCP client (kebab-case method ident).
    pub name: &'static str,
    /// Description (the method's doc comment).
    pub description: &'static str,
    /// JSON Schema for the tool's input object (derived from the args struct).
    pub input_schema: fn() -> serde_json::Value,
    /// Async handler: takes a JSON args object, returns a JSON result.
    pub handler: fn(serde_json::Value) -> ServiceFuture,
    /// The descriptor this projects.
    pub verb: &'static VerbDescriptor,
}

impl McpToolDescriptor {
    /// Project a descriptor. `const` because `inventory::submit!` builds a
    /// `static`.
    pub const fn of(verb: &'static VerbDescriptor) -> Self {
        Self {
            name: verb.name,
            description: verb.description,
            input_schema: verb.input_schema,
            handler: verb.handler,
            verb,
        }
    }

    /// Iterate all descriptors registered in this binary via `inventory`.
    pub fn iter() -> impl Iterator<Item = &'static McpToolDescriptor> {
        inventory::iter::<McpToolDescriptor>.into_iter()
    }
}

impl fmt::Debug for McpToolDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpToolDescriptor")
            .field("name", &self.name)
            .field("description", &self.description)
            .finish()
    }
}

inventory::collect!(McpToolDescriptor);

/// Reserved result field used by generated services that need MCP-native
/// content (notably images). Transports remove it from structured output and
/// publish its blocks as the tool result's `content` array.
pub const MCP_CONTENT_FIELD: &str = "_mcp_content";

/// Split MCP-native content from an otherwise ordinary generated service
/// result. Keeping this convention here lets every service remain generated
/// while stdio, HTTP, and suite-wide hosts share one mixed-content behavior.
pub fn split_mcp_content(
    mut value: serde_json::Value,
) -> (Vec<serde_json::Value>, serde_json::Value) {
    let blocks = value
        .as_object_mut()
        .and_then(|object| object.remove(MCP_CONTENT_FIELD))
        .and_then(|content| content.as_array().cloned())
        .unwrap_or_default();
    (blocks, value)
}

/// Reshape a generated service result so it is legal as MCP
/// `structuredContent`, which the spec requires to be a JSON **object**.
///
/// Generated methods return their natural Rust shape: `Vec<T>` serializes to
/// an array, `Option<T>` to `null` on a miss, a count to a bare number, a
/// rendered string to a bare string. MCP clients (Claude Code among them)
/// validate the spec shape and reject the whole call otherwise — which made
/// every list-, option- and scalar-returning generated tool unusable over
/// stdio. The envelope is applied by the MCP transports at the boundary, so
/// services stay generated and other transports (CLI, HTTP, Swift, impel)
/// keep the raw value:
///
/// - object → unchanged, so every already-conforming tool keeps its shape
/// - array  → `{"items": [...]}`
/// - null   → `{"item": null}`
/// - string / number / bool → `{"value": ...}`
pub fn envelope_structured_content(value: serde_json::Value) -> serde_json::Value {
    use serde_json::{json, Value};
    match value {
        Value::Object(_) => value,
        Value::Array(_) => json!({ "items": value }),
        Value::Null => json!({ "item": Value::Null }),
        other => json!({ "value": other }),
    }
}

/// The CLI projection of a [`VerbDescriptor`].
///
/// Generated `inventory::submit!` blocks register one of these per method
/// (built by [`CliSubcommand::of`]). A CLI binary collects them at startup and
/// builds the `clap::Command` tree dynamically.
pub struct CliSubcommand {
    /// Subcommand name (kebab-case method ident) — the spelling the CLI uses
    /// unless another linked service declares the same method name.
    pub name: &'static str,
    /// `service_method`, the MCP tool name. The CLI falls back to this for
    /// every party to a collision (see `cli::effective_names`), so a binary
    /// that links two services with a `remove_tag` keeps both reachable
    /// instead of failing clap's duplicate-name assertion at startup.
    pub qualified_name: &'static str,
    /// Description (the method's doc comment).
    pub description: &'static str,
    /// JSON Schema for the args object (used to build clap args).
    pub input_schema: fn() -> serde_json::Value,
    /// Apply the parsed args (already deserialized into a JSON object) and
    /// return a JSON result. Same shape as `McpToolDescriptor::handler` so the
    /// two paths share generated code.
    pub apply: fn(serde_json::Value) -> ServiceFuture,
    /// The descriptor this projects.
    pub verb: &'static VerbDescriptor,
}

impl CliSubcommand {
    /// Project a descriptor. `const` because `inventory::submit!` builds a
    /// `static`.
    pub const fn of(verb: &'static VerbDescriptor) -> Self {
        Self {
            name: verb.method,
            qualified_name: verb.name,
            description: verb.description,
            input_schema: verb.input_schema,
            apply: verb.handler,
            verb,
        }
    }

    pub fn iter() -> impl Iterator<Item = &'static CliSubcommand> {
        inventory::iter::<CliSubcommand>.into_iter()
    }
}

impl fmt::Debug for CliSubcommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CliSubcommand")
            .field("name", &self.name)
            .field("description", &self.description)
            .finish()
    }
}

inventory::collect!(CliSubcommand);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The transport-level guarantee behind "no generated tool can emit a
    /// non-object structuredContent": every JSON kind a generated handler can
    /// return leaves this function as an object. Transports (impress-mcp's
    /// flat and grouped paths, impress-mcp-host) all route through it.
    #[test]
    fn envelope_yields_an_object_for_every_json_kind() {
        let cases = [
            json!(null),
            json!(true),
            json!(42),
            json!("Café"),
            json!([1, 2, 3]),
            json!({"ok": true}),
        ];
        for value in cases {
            let enveloped = envelope_structured_content(value.clone());
            assert!(
                enveloped.is_object(),
                "envelope of {value} is not an object: {enveloped}"
            );
        }
    }

    #[test]
    fn envelope_shapes_match_their_contract() {
        assert_eq!(
            envelope_structured_content(json!(["a", "b"])),
            json!({"items": ["a", "b"]})
        );
        assert_eq!(
            envelope_structured_content(json!(null)),
            json!({"item": null})
        );
        assert_eq!(
            envelope_structured_content(json!("Café")),
            json!({"value": "Café"})
        );
        assert_eq!(envelope_structured_content(json!(7)), json!({"value": 7}));
        // Objects pass through byte-identical — already-conforming tools
        // (e.g. store-query-service_search-all's {"hits": [...]}) keep their
        // shape.
        let object = json!({"hits": [], "ok": true, "message": "0 hit(s)"});
        assert_eq!(envelope_structured_content(object.clone()), object);
    }

    #[test]
    fn split_then_envelope_composes_for_mixed_content() {
        let (blocks, raw) = split_mcp_content(json!({
            "status": "resolved",
            MCP_CONTENT_FIELD: [{"type": "image", "data": "iVBORw0KGgo=", "mimeType": "image/png"}]
        }));
        assert_eq!(blocks.len(), 1);
        let structured = envelope_structured_content(raw);
        assert!(structured.is_object());
        assert_eq!(structured["status"], "resolved");
        assert!(structured.get(MCP_CONTENT_FIELD).is_none());
    }
}

// ---------------------------------------------------------------------------
// Backend slot
// ---------------------------------------------------------------------------

/// The process-wide, swappable backend a `*-service` crate dispatches through.
///
/// A service crate can use a store-backed default and install an app-owned
/// native backend for capabilities that require the running process. The
/// previous `OnceLock` could not replace a backend after initialization;
/// `BackendSlot` can.
///
/// So the slot is a lock, not a latch: [`install`](Self::install) replaces, and
/// [`clear`](Self::clear) hands dispatch back to the default. Reachability of
/// remote apps is handled separately by `impress-app-transport`.
///
/// Reads take an uncontended read lock and clone an `Arc` — the same cost the
/// `OnceLock` version paid to hand out its `Arc`.
pub struct BackendSlot<T: ?Sized> {
    inner: std::sync::RwLock<Option<std::sync::Arc<T>>>,
}

impl<T: ?Sized> BackendSlot<T> {
    /// An empty slot: dispatch goes to the crate's default implementation.
    pub const fn new() -> Self {
        Self {
            inner: std::sync::RwLock::new(None),
        }
    }

    /// Install (or replace) the backend.
    pub fn install(&self, backend: std::sync::Arc<T>) {
        if let Ok(mut slot) = self.inner.write() {
            *slot = Some(backend);
        }
    }

    /// Empty the slot, returning dispatch to the default implementation.
    /// Idempotent — clearing an empty slot is how "still unreachable" is
    /// reported.
    pub fn clear(&self) {
        if let Ok(mut slot) = self.inner.write() {
            *slot = None;
        }
    }

    /// The installed backend, if any.
    ///
    /// A poisoned lock reads as "no backend" rather than panicking: a service
    /// call answering from the shared store is a better outcome than taking the
    /// MCP server down, and the only writers are the probes above.
    pub fn get(&self) -> Option<std::sync::Arc<T>> {
        self.inner.read().ok().and_then(|slot| slot.clone())
    }

    pub fn is_installed(&self) -> bool {
        self.get().is_some()
    }
}

impl<T: ?Sized> Default for BackendSlot<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod backend_slot_tests {
    use super::BackendSlot;
    use std::sync::Arc;

    trait Fake: Send + Sync {
        fn id(&self) -> &'static str;
    }
    struct A;
    struct B;
    impl Fake for A {
        fn id(&self) -> &'static str {
            "a"
        }
    }
    impl Fake for B {
        fn id(&self) -> &'static str {
            "b"
        }
    }

    /// The whole point of the type: a second install REPLACES, and a clear
    /// returns dispatch to the default. As a `OnceLock` the first probe won
    /// for the life of the process, which is how a daemon kept an HTTP client
    /// pointed at an app that had quit hours earlier.
    #[test]
    fn a_slot_can_be_replaced_and_emptied() {
        let slot: BackendSlot<dyn Fake> = BackendSlot::new();
        assert!(!slot.is_installed());
        assert!(slot.get().is_none());

        slot.install(Arc::new(A));
        assert_eq!(slot.get().map(|b| b.id()), Some("a"));

        slot.install(Arc::new(B));
        assert_eq!(
            slot.get().map(|b| b.id()),
            Some("b"),
            "a later probe wins, it is not ignored"
        );

        slot.clear();
        assert!(!slot.is_installed());
        slot.clear();
        assert!(!slot.is_installed(), "clearing an empty slot is a no-op");
    }

    /// A backend handed out before a clear stays usable — callers hold an
    /// `Arc`, so an in-flight request is never yanked mid-call.
    #[test]
    fn a_handed_out_backend_outlives_the_clear() {
        let slot: BackendSlot<dyn Fake> = BackendSlot::new();
        slot.install(Arc::new(A));
        let held = slot.get().expect("installed");
        slot.clear();
        assert_eq!(held.id(), "a");
    }
}

// Manifest metadata without a runtime dependency back on the store.
#[allow(dead_code)]
mod schema_names {
    include!(concat!(env!("OUT_DIR"), "/schema_ref_names.rs"));
}
