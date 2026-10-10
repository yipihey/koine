//! The invoker pipeline (ADR-0034 D2, plan-verb-pipeline P2).
//!
//! One ordered, hand-rolled chain that EVERY path running a verb goes
//! through — MCP flat and grouped, the CLI, the surface runtime and its HTTP
//! mirror, the FFI verb host and the FFI layout `apply`, impel-tools,
//! mcp-host, impress-ai-tools. Each concern is written once, here, as a
//! layer; adding one changes no entry path. A test in
//! `crates/impress-capabilities/tests/pipeline.rs` enumerates every
//! `descriptor.handler` call site in the repository and fails when one is
//! outside this module.
//!
//! The layers, in order (ADR-0034 D2's ordering rule: nothing that can refuse
//! runs after a side effect; identity before policy; the span encloses
//! invoke and envelope; audit records the final code):
//!
//! 1. **identity** — the caller the entry path established
//!    ([`CallerIdentity`]); a call that runs inside another call inherits its
//!    caller and trace and records the outer call as its parent.
//! 2. **strict args** — the schema check ([`crate::strict`]) for a service
//!    declaring `strict_args`; refused `invalid-argument` naming the field
//!    (moved here from the generated invoker, which now parses nothing but
//!    its own args struct).
//! 3. **reachability** — one rule and one probe ([`reachability`]).
//! 4. **policy** — `(caller, class, verb) → run | review | deny`
//!    ([`policy`]).
//! 5. **span** — a `tracing` span per call: name, caller, ids, sizes,
//!    outcome, duration; never an argument value.
//! 6. **invoke** — the generated handler, under the task-local
//!    [`context::CallContext`].
//! 7. **envelope** — `ok`/`code` read off the result (the value itself is
//!    unchanged: `wire_version` is the verb's own, D-G6 is G-side work).
//! 8. **audit** — one `core/verb-call@1.0.0` record per non-read-only call
//!    ([`audit`]).
//!
//! Hand-rolled rather than tower (ADR-0034 § Alternatives): the chain must
//! wrap the two bypasses that carry no request type, which
//! [`invoke_sync_with`] and [`invoke_with`] do by taking the handler step as
//! a closure.

pub mod audit;
pub mod context;
pub mod identity;
pub mod perf;
pub mod policy;
pub mod reachability;
pub mod transport;

use std::sync::{Arc, Once};
use std::time::Instant;

use serde_json::Value;

use crate::descriptor::{SafetyClass, VerbDescriptor};
use crate::descriptor_handle::VerbHandle;
use crate::BoxError;
pub use context::{CallContext, TransportContext};
pub use identity::CallerIdentity;
use std::borrow::Cow;

/// One call, as an entry path hands it to the pipeline.
#[derive(Debug, Clone)]
pub struct Call {
    pub args: Value,
    pub caller: CallerIdentity,
    /// The trace to join, when the transport carried one (`traceparent`).
    pub trace_id: Option<String>,
    /// The call this one runs for, when the transport says so.
    pub parent_call: Option<String>,
    /// A per-call store override (H-P2-3), set by [`invoke_on`].
    pub store: Option<Arc<dyn std::any::Any + Send + Sync>>,
    /// The name the caller actually asked for (P3, plan-verb-pipeline §
    /// Lifecycle), when it differs from `verb.name` — an alias resolved to
    /// this verb. `None` for an ordinary call by its canonical name.
    /// [`crate::call::call_as`]/[`crate::call::call_async_as`] set this;
    /// recorded on the span and the audit row so a retired name's traffic
    /// is measured before it is removed.
    pub requested_name: Option<String>,
}

impl Call {
    pub fn new(caller: CallerIdentity, args: Value) -> Self {
        Self {
            args,
            caller,
            trace_id: None,
            parent_call: None,
            store: None,
            requested_name: None,
        }
    }

    /// [`Call::new`] with the alias name the caller actually asked for.
    pub fn with_requested_name(mut self, name: impl Into<String>) -> Self {
        self.requested_name = Some(name.into());
        self
    }

    pub fn person(args: Value) -> Self {
        Self::new(CallerIdentity::Person, args)
    }

    pub fn agent(name: impl Into<String>, args: Value) -> Self {
        Self::new(CallerIdentity::agent(name), args)
    }

    pub fn with_trace(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self
    }
}

/// Why the pipeline did not run the handler, or what the handler said.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    /// The verb's app is not running (the reachability layer). `Display` is
    /// the text impress-mcp has always answered for this.
    #[error("{}", reachability::reason_text(app, verb))]
    Unavailable {
        app: &'static str,
        verb: &'static str,
    },
    /// The handler's own error, as it returned it.
    #[error("{0}")]
    Handler(BoxError),
}

/// A hook a linked crate registers to run once, before the first call in
/// the process: how `impress-store-service` installs the audit sink without
/// this crate reaching the store. Submitted through `inventory`, so linking
/// the crate is installing the hook.
pub struct Installer {
    pub name: &'static str,
    pub install: fn(),
}

inventory::collect!(Installer);

fn run_installers() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        for installer in inventory::iter::<Installer> {
            (installer.install)();
        }
    });
}

/// A verb's input schema, built once per process: `schema_for!` walks the
/// derive on every call otherwise, which for a verb taking a pane reference
/// (a `oneOf` of four shapes) is most of the chain's cost.
pub fn input_schema(verb: &'static VerbDescriptor) -> &'static Value {
    cached_schema(verb, false)
}

/// S3 also consults the output schema for private identifier paths. Like
/// strict input checking, auditing must not rebuild a schema on every call.
fn cached_schema(verb: &'static VerbDescriptor, output: bool) -> &'static Value {
    use std::collections::HashMap;
    use std::sync::RwLock;
    type SchemaCache = HashMap<(usize, bool), &'static Value>;
    static CACHE: RwLock<Option<SchemaCache>> = RwLock::new(None);
    let key = (verb as *const VerbDescriptor as usize, output);
    if let Some(found) = CACHE
        .read()
        .ok()
        .and_then(|c| c.as_ref().and_then(|m| m.get(&key).copied()))
    {
        return found;
    }
    let build = if output {
        verb.output_schema
    } else {
        verb.input_schema
    };
    let built: &'static Value = Box::leak(Box::new(build()));
    if let Ok(mut cache) = CACHE.write() {
        cache
            .get_or_insert_with(HashMap::new)
            .entry(key)
            .or_insert(built);
    }
    built
}

/// Linked schemas keep their existing process cache; provider schemas borrow from
/// the owned registration snapshot and are never interned into a static cache.
fn handle_schema(verb: &VerbHandle, output: bool) -> Cow<'_, Value> {
    if let Some(linked) = verb.linked() {
        Cow::Borrowed(cached_schema(linked, output))
    } else if output {
        verb.output_schema()
    } else {
        verb.input_schema()
    }
}

fn handle_name(verb: &VerbHandle) -> Cow<'static, str> {
    match verb.linked() {
        Some(linked) => Cow::Borrowed(linked.name),
        None => Cow::Owned(verb.name().to_owned()),
    }
}

fn handle_since(verb: &VerbHandle) -> Cow<'static, str> {
    match verb.linked() {
        Some(linked) => Cow::Borrowed(linked.since),
        None => Cow::Owned(verb.since().to_owned()),
    }
}

/// What the layers before the handler produced.
struct Prepared {
    context: Arc<CallContext>,
    args: Value,
    started: Instant,
    started_at: String,
    arg_bytes: usize,
    span: tracing::Span,
    requested_name: Option<String>,
}

/// The layers before the handler: identity, strict args, reachability,
/// policy, the span. `Ok(Err(value))` is a refusal envelope to answer
/// without running the handler.
fn prepare(verb: &VerbHandle, call: Call) -> Result<Result<Prepared, Value>, PipelineError> {
    prepare_inner(verb, call, false)
}

// A provider refresh can fail before policy. Keep that refusal on the normal
// span/audit path, while deliberately skipping checks that would short-circuit
// before `finish` and obscure the original host-unavailable diagnosis.
fn prepare_inner(
    verb: &VerbHandle,
    call: Call,
    preflight_refusal: bool,
) -> Result<Result<Prepared, Value>, PipelineError> {
    run_installers();
    let Call {
        args,
        caller,
        trace_id,
        parent_call,
        store,
        requested_name,
    } = call;
    let args = if args.is_null() {
        Value::Object(Default::default())
    } else {
        args
    };

    // 1. identity — inherit the enclosing call's, when there is one.
    let outer = context::current();
    let (caller, trace_id, parent_call, store) = match &outer {
        Some(outer) => (
            outer.caller.clone(),
            Some(outer.trace_id.clone()),
            Some(outer.call_id.clone()),
            store.or_else(|| outer.store_override.clone()),
        ),
        None => (caller, trace_id, parent_call, store),
    };
    let call_id = uuid::Uuid::new_v4().to_string();
    let trace_id = trace_id.unwrap_or_else(|| call_id.clone());

    // 2. strict args.
    if !preflight_refusal && verb.strict() {
        let schema = handle_schema(verb, false);
        if let Err(refusal) = crate::strict::check_args(verb.name(), &args, &schema) {
            return Ok(Err(crate::strict::refusal_value(&refusal)));
        }
    }

    // 3. reachability.
    if !preflight_refusal {
        if let Some((linked, app)) = verb
            .linked()
            .and_then(|linked| reachability::unavailable_app(linked.name).map(|app| (linked, app)))
        {
            // A native request is already executing inside its owning app. A
            // client router co-linked in that process must not gate it on a
            // second probe (or send it back over HTTP to itself).
            // A scenario with an explicit store also runs locally, independent
            // of the client's process-wide app availability configuration.
            if store.is_none() && !matches!(&caller, CallerIdentity::App(owner) if owner == app) {
                return Err(PipelineError::Unavailable {
                    app,
                    verb: linked.name,
                });
            }
        }

        if let Err(refusal) = crate::registry_runtime::check_available(verb, &args) {
            return Ok(Err(crate::strict::refusal_value(&refusal)));
        }

        // 4. policy.
        match policy::decide(&caller, verb) {
            policy::Decision::Run => {}
            policy::Decision::Review => return Ok(Err(policy::queue(&caller, verb, &args))),
            policy::Decision::Deny(reason) => return Ok(Err(policy::deny(verb, reason))),
            // D-R11: a destructive verb whose declared writes overlap another
            // call's still in flight on the same kind.
            policy::Decision::Conflict(kinds) => return Ok(Err(policy::conflict(verb, kinds))),
        }
    }

    // 5. span.
    let arg_bytes = audit::approx_size(&args);
    let span = tracing::info_span!(
        target: "verb",
        "verb",
        name = verb.name(),
        requested_name = requested_name.as_deref().unwrap_or(""),
        caller = %caller,
        call_id = %call_id,
        trace_id = %trace_id,
        parent_call = parent_call.as_deref().unwrap_or(""),
        arg_bytes,
        ok = tracing::field::Empty,
        code = tracing::field::Empty,
        result_bytes = tracing::field::Empty,
        duration_us = tracing::field::Empty,
        budget_ms = tracing::field::Empty,
    );
    if let Some(budget_ms) = verb.budget_ms() {
        span.record("budget_ms", budget_ms);
    }
    let context = Arc::new(CallContext {
        call_id,
        trace_id,
        parent_call,
        caller,
        verb: handle_name(verb),
        store_override: store,
        mutation_ids: context::MutationIds::default(),
    });
    Ok(Ok(Prepared {
        context,
        args,
        started: Instant::now(),
        started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        arg_bytes,
        span,
        requested_name,
    }))
}

/// `ok` and `code` as the envelope layer reads them off a result.
fn outcome(result: &Result<Value, BoxError>) -> (bool, Option<String>, usize, usize) {
    match result {
        Ok(value) => {
            let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(true);
            let code = value
                .get("code")
                .and_then(Value::as_str)
                .map(str::to_string);
            let message_len = value
                .get("message")
                .and_then(Value::as_str)
                .map_or(0, str::len);
            (ok, code, message_len, audit::approx_size(value))
        }
        Err(error) => (
            false,
            Some(crate::refusal::codes::VERB_FAILED.to_string()),
            error.to_string().len(),
            0,
        ),
    }
}

/// The additive `"deprecated"` envelope field (P3, plan-verb-pipeline §
/// Lifecycle): a call that named one of `verb.aliases` gets
/// [`VerbDescriptor::alias_deprecation_notice`]; any other call gets
/// [`VerbDescriptor::deprecation_notice`], which answers only when the verb
/// itself (not just an old name for it) is deprecated. Applied to object
/// results only — array, scalar and error results are unchanged.
fn apply_deprecation_notice(
    verb: &VerbHandle,
    requested_name: Option<&str>,
    result: Result<Value, BoxError>,
) -> Result<Value, BoxError> {
    let via_alias = requested_name.is_some_and(|n| verb.has_alias(n));
    let notice = if via_alias {
        verb.alias_deprecation_notice()
    } else {
        verb.deprecation_notice()
    };
    match (notice, result) {
        (Some(notice), Ok(mut value)) => {
            if let Some(object) = value.as_object_mut() {
                object.insert("deprecated".to_string(), notice);
            }
            Ok(value)
        }
        (_, result) => result,
    }
}

/// The layers after the handler: envelope and audit.
fn finish(verb: &VerbHandle, prepared: Prepared, result: &Result<Value, BoxError>) {
    finish_inner(verb, prepared, result, false);
}

fn finish_inner(
    verb: &VerbHandle,
    prepared: Prepared,
    result: &Result<Value, BoxError>,
    force_audit: bool,
) {
    // D-R11: give back the write lease `prepare`'s policy step took for this
    // call (a no-op if it took none — read-only, no literal declared
    // writes, or refused before reaching here).
    // The forced provider preflight refusal never entered policy or acquired
    // a write lease. Releasing here could steal an overlapping call's lease.
    if !force_audit {
        policy::release(verb);
    }
    let Prepared {
        context,
        span,
        started,
        started_at,
        arg_bytes,
        requested_name,
        ..
    } = prepared;
    let duration = started.elapsed();
    let (ok, code, message_len, result_bytes) = outcome(result);
    span.record("ok", ok);
    span.record("code", code.as_deref().unwrap_or(""));
    span.record("result_bytes", result_bytes);
    span.record("duration_us", duration.as_micros() as u64);
    drop(span);

    // D-P2: a budget breach is a warning on the `perf` target, the same
    // bridged category `impress-store-ffi::tracing_bridge` forwards to the
    // Console (G7a) — never a refusal, the call already answered.
    if let Some(budget_ms) = verb.budget_ms() {
        // The aggregator records integer microseconds from this same
        // duration. Compare at that resolution so its breach_count and the
        // Console warning agree at a sub-millisecond boundary.
        let duration_us = duration.as_micros() as u64;
        if duration_us > budget_ms.saturating_mul(1_000) {
            tracing::warn!(
                target: "perf",
                verb = verb.name(),
                budget_ms,
                duration_us,
                "{} took {duration_us}us, over its {budget_ms}ms budget",
                verb.name(),
            );
        }
    }

    if force_audit || verb.safety().class != SafetyClass::ReadOnly || audit::log_all() {
        let (recorded_args, args_replayable) = audit::recorded_args(
            &prepared.args,
            &handle_schema(verb, false),
            verb.replay_full(),
        );
        let (result_ids, result_ids_truncated) = match result {
            Ok(value) => audit::result_ids(value, &handle_schema(verb, true)),
            Err(_) => Default::default(),
        };
        let (inserted_ids, deleted_ids) = context.mutation_ids.snapshot();
        audit::record(audit::VerbCallRecord {
            call_id: context.call_id.clone(),
            verb: handle_name(verb),
            since: handle_since(verb),
            caller: context.caller.clone(),
            trace_id: context.trace_id.clone(),
            parent_call: context.parent_call.clone(),
            args: recorded_args,
            args_replayable,
            result_ids,
            result_ids_truncated,
            inserted_ids,
            deleted_ids,
            ok,
            code,
            message_len,
            started_at,
            duration_ms: duration.as_millis() as u64,
            arg_bytes,
            result_bytes,
            store_override: context.store_override.clone(),
            requested_name,
        });
    }
}

/// Run `verb` through the chain with its own handler as the handler step.
pub async fn invoke(verb: &'static VerbDescriptor, call: Call) -> Result<Value, PipelineError> {
    invoke_handle(VerbHandle::Linked(verb), call).await
}

/// Run a linked or registered runtime verb through the same eight layers.
pub async fn invoke_handle(verb: VerbHandle, call: Call) -> Result<Value, PipelineError> {
    let verb = match crate::registry_runtime::current_for_call(verb.clone()).await {
        Ok(verb) => verb,
        Err(refusal) => {
            let prepared = prepare_inner(&verb, call, true)?
                .expect("provider preflight refusal bypasses early checks");
            let result: Result<Value, BoxError> = Ok(crate::strict::refusal_value(&refusal));
            finish_inner(&verb, prepared, &result, true);
            return result.map_err(PipelineError::Handler);
        }
    };
    let work_verb = verb.clone();
    invoke_handle_with(&verb, call, move |args| async move {
        match work_verb {
            VerbHandle::Linked(linked) => {
                if let Some(router) = transport::current() {
                    if let Some(result) = router.route(linked, args.clone()).await {
                        return result;
                    }
                }
                (linked.handler)(args).await
            }
            provider => crate::registry_runtime::invoke(&provider, args).await,
        }
    })
    .await
}

/// Run `verb` through the chain on `store` instead of the process-wide
/// store (H-P2-3, D-R9): a Tier A scenario's per-scenario store. The
/// store-tier crates read it back with [`context::store_override`].
pub async fn invoke_on<S: std::any::Any + Send + Sync>(
    store: Arc<S>,
    verb: &'static VerbDescriptor,
    mut call: Call,
) -> Result<Value, PipelineError> {
    call.store = Some(store);
    invoke(verb, call).await
}

/// Run the chain around `work` as the handler step — for a path whose
/// handler is not the descriptor's `fn` (the surface HTTP mirror runs the
/// verb on the FFI's own service instance).
pub async fn invoke_with<F, Fut>(
    verb: &'static VerbDescriptor,
    call: Call,
    work: F,
) -> Result<Value, PipelineError>
where
    F: FnOnce(Value) -> Fut,
    Fut: std::future::Future<Output = Result<Value, BoxError>>,
{
    let handle = VerbHandle::Linked(verb);
    invoke_handle_with(&handle, call, work).await
}

async fn invoke_handle_with<F, Fut>(
    verb: &VerbHandle,
    call: Call,
    work: F,
) -> Result<Value, PipelineError>
where
    F: FnOnce(Value) -> Fut,
    Fut: std::future::Future<Output = Result<Value, BoxError>>,
{
    let prepared = match prepare(verb, call)? {
        Ok(prepared) => prepared,
        Err(refused) => return Ok(refused),
    };
    let entered = prepared.span.clone();
    let result = {
        let _guard = entered.enter();
        let future = async {
            let result = work(prepared.args.clone()).await;
            native_backend_result(result)
        };
        drop(_guard);
        tracing::Instrument::instrument(context::scope(prepared.context.clone(), future), entered)
            .await
    };
    let result = apply_deprecation_notice(verb, prepared.requested_name.as_deref(), result);
    finish(verb, prepared, &result);
    result.map_err(PipelineError::Handler)
}

fn native_backend_result(result: Result<Value, BoxError>) -> Result<Value, BoxError> {
    match context::take_reported_refusal() {
        Some(refusal) => Ok(serde_json::json!({
            "ok": false,
            "code": refusal.code,
            "message": refusal.message,
            "wire_version": crate::wire::WIRE_VERSION,
        })),
        None => result,
    }
}

/// [`invoke_with`] for a synchronous handler step (the FFI's layout
/// `apply`, a sync call from Swift). The task-local context is set with
/// `sync_scope`, so the store stamps `batch_id` on what `work` writes.
pub fn invoke_sync_with<F>(
    verb: &'static VerbDescriptor,
    call: Call,
    work: F,
) -> Result<Value, PipelineError>
where
    F: FnOnce(Value) -> Result<Value, BoxError>,
{
    let handle = VerbHandle::Linked(verb);
    let verb = &handle;
    let prepared = match prepare(verb, call)? {
        Ok(prepared) => prepared,
        Err(refused) => return Ok(refused),
    };
    let result = {
        let _guard = prepared.span.enter();
        context::sync_scope(prepared.context.clone(), || {
            native_backend_result(work(prepared.args.clone()))
        })
    };
    let result = apply_deprecation_notice(verb, prepared.requested_name.as_deref(), result);
    finish(verb, prepared, &result);
    result.map_err(PipelineError::Handler)
}

/// [`invoke`] from a synchronous context (CLI dispatch, an FFI shim), on
/// the shared service runtime.
pub fn invoke_blocking(verb: &'static VerbDescriptor, call: Call) -> Result<Value, PipelineError> {
    crate::runtime::block_on(invoke(verb, call))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::descriptor::{Safety, Source};
    use crate::ServiceFuture;
    use serde_json::json;
    use std::sync::Mutex;

    fn schema() -> Value {
        json!({"type": "object", "properties": {"n": {"type": "integer"}}})
    }
    fn echo(args: Value) -> ServiceFuture {
        Box::pin(async move {
            Ok(json!({"ok": true, "echo": args, "call": context::current_call_id()}))
        })
    }
    fn failing(_: Value) -> ServiceFuture {
        Box::pin(async { Err("boom".into()) })
    }
    static ECHO: VerbDescriptor = VerbDescriptor {
        name: "t-service_echo",
        service: "t-service",
        method: "echo",
        description: "d",
        input_schema: schema,
        output_schema: schema,
        safety: Safety {
            class: SafetyClass::Mutating,
            idempotent: false,
        },
        since: "0.1.0",
        effects: crate::Effects::NONE,
        deprecated: None,
        aliases: &[],
        examples: &[],
        strict: true,
        budget_ms: None,
        replay_full: false,
        source: Source::Linked,
        handler: echo,
    };
    static FAILING: VerbDescriptor = VerbDescriptor {
        name: "t-service_fail",
        method: "fail",
        handler: failing,
        strict: false,
        budget_ms: None,
        ..ECHO
    };
    /// A verb with a retired alias (P3): a direct call to `t-service_renamed`
    /// gets no notice, but the same call by `t-service_old-echo` does.
    static RENAMED: VerbDescriptor = VerbDescriptor {
        name: "t-service_renamed",
        method: "renamed",
        handler: echo,
        strict: false,
        budget_ms: None,
        deprecated: Some(crate::Deprecation {
            since: "0.7.0",
            alias_of: None,
            note: "renamed from t-service_old-echo",
        }),
        aliases: &["t-service_old-echo"],
        ..ECHO
    };

    pub(crate) struct Captured(pub(crate) Mutex<Vec<audit::VerbCallRecord>>);
    impl audit::Sink for Captured {
        fn record(&self, record: audit::VerbCallRecord) {
            self.0.lock().unwrap().push(record);
        }
    }

    /// The sink is process-global and the tests run in parallel, so they
    /// share one.
    pub(crate) fn captured() -> Arc<Captured> {
        static SINK: std::sync::OnceLock<Arc<Captured>> = std::sync::OnceLock::new();
        SINK.get_or_init(|| {
            let sink = Arc::new(Captured(Mutex::new(Vec::new())));
            audit::install(sink.clone());
            sink
        })
        .clone()
    }

    #[test]
    fn a_strict_verb_refuses_an_unknown_field_before_the_handler() {
        let answer = invoke_blocking(&ECHO, Call::person(json!({"nn": 1}))).unwrap();
        assert_eq!(answer["ok"], false);
        assert_eq!(answer["code"], "invalid-argument");
        assert!(answer["message"]
            .as_str()
            .unwrap()
            .contains("t-service_echo"));
        assert!(answer["message"].as_str().unwrap().contains("'nn'"));
        assert_eq!(answer["wire_version"], crate::wire::WIRE_VERSION);
    }

    #[test]
    fn null_arguments_are_an_empty_object() {
        let answer = invoke_blocking(&ECHO, Call::person(Value::Null)).unwrap();
        assert_eq!(answer["echo"], json!({}));
    }

    #[test]
    fn the_handler_runs_under_a_call_context_and_the_audit_row_carries_it() {
        let sink = captured();
        let answer = invoke_blocking(&ECHO, Call::agent("test", json!({"n": 7}))).unwrap();
        let call_id = answer["call"]
            .as_str()
            .expect("the handler saw its call id");
        let records = sink.0.lock().unwrap();
        let record = records
            .iter()
            .find(|r| r.call_id == call_id)
            .expect("one audit record for the mutating call");
        assert_eq!(record.verb, "t-service_echo");
        assert_eq!(record.caller, CallerIdentity::agent("test"));
        assert!(record.ok);
        assert_eq!(record.args, json!({"n": 7}));
        assert_eq!(
            record.trace_id, call_id,
            "a fresh call starts its own trace"
        );
    }

    #[test]
    fn audit_payload_contains_the_handlers_created_and_deleted_ids() {
        let sink = captured();
        let answer = invoke_sync_with(&ECHO, Call::person(json!({})), |_| {
            context::note_mutation("b".into(), true, false);
            context::note_mutation("a".into(), true, false);
            context::note_mutation("b".into(), true, false);
            context::note_mutation("z".into(), false, true);
            Ok(json!({"call": context::current_call_id()}))
        })
        .unwrap();
        let call_id = answer["call"].as_str().unwrap();
        let records = sink.0.lock().unwrap();
        let record = records.iter().find(|r| r.call_id == call_id).unwrap();
        assert_eq!(record.inserted_ids, ["a", "b"]);
        assert_eq!(record.deleted_ids, ["z"]);
        let payload = record.payload();
        assert_eq!(payload["inserted_ids"], json!(["a", "b"]));
        assert_eq!(payload["deleted_ids"], json!(["z"]));
    }

    #[test]
    fn a_nested_call_inherits_the_caller_and_the_trace() {
        fn nested(_: Value) -> ServiceFuture {
            Box::pin(async {
                let outer = context::current().unwrap();
                let inner = invoke(&ECHO, Call::person(json!({}))).await.unwrap();
                let inner_ctx = inner["call"].as_str().unwrap().to_string();
                Ok(json!({"outer": outer.call_id, "inner": inner_ctx, "trace": outer.trace_id}))
            })
        }
        static OUTER: VerbDescriptor = VerbDescriptor {
            name: "t-service_outer",
            method: "outer",
            handler: nested,
            strict: false,
            budget_ms: None,
            ..ECHO
        };
        let sink = captured();
        let answer =
            invoke_blocking(&OUTER, Call::agent("mcp", json!({})).with_trace("trace-1")).unwrap();
        assert_eq!(answer["trace"], "trace-1");
        let records = sink.0.lock().unwrap();
        let inner = records
            .iter()
            .find(|r| r.call_id == answer["inner"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            inner.caller,
            CallerIdentity::agent("mcp"),
            "not the Person it claimed"
        );
        assert_eq!(inner.trace_id, "trace-1");
        assert_eq!(inner.parent_call.as_deref(), answer["outer"].as_str());
    }

    #[test]
    fn a_handler_error_is_the_pipelines_handler_error() {
        let err = invoke_blocking(&FAILING, Call::person(json!({}))).unwrap_err();
        assert!(matches!(err, PipelineError::Handler(_)));
        assert_eq!(err.to_string(), "boom");
    }

    #[tokio::test]
    async fn native_refusals_replace_placeholders_without_leaking_between_calls() {
        let refused = invoke_with(&ECHO, Call::person(json!({})), |_| async {
            context::report_refusal("store-error", "native save failed");
            tokio::task::yield_now().await;
            context::report_refusal("internal", "secondary failure");
            Ok(Value::Null)
        });
        let clean = invoke_with(&ECHO, Call::person(json!({})), |_| async {
            tokio::task::yield_now().await;
            Ok(json!([1, 2]))
        });
        let (refused, clean) = tokio::join!(refused, clean);
        assert_eq!(
            refused.unwrap(),
            serde_json::json!({
                "ok": false, "code": "store-error", "message": "native save failed", "wire_version": 1
            })
        );
        assert_eq!(clean.unwrap(), json!([1, 2]));

        let outer = invoke_with(&ECHO, Call::person(json!({})), |_| async {
            let inner = invoke_with(&ECHO, Call::person(json!({})), |_| async {
                context::report_refusal("not-found", "missing child");
                Ok(Value::Null)
            })
            .await?;
            Ok(json!({"handled_child": inner["code"]}))
        })
        .await
        .unwrap();
        assert_eq!(outer, json!({"handled_child": "not-found"}));
    }

    #[test]
    fn a_native_sync_failure_is_not_a_successful_false_result() {
        let result = invoke_sync_with(&ECHO, Call::person(json!({})), |_| {
            context::report_refusal("verb-failed", "native callback failed");
            Ok(json!(false))
        })
        .unwrap();
        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "verb-failed");
        assert_eq!(result["message"], "native callback failed");
    }

    #[test]
    fn the_sync_chain_sets_the_context_too() {
        let seen = invoke_sync_with(&ECHO, Call::person(json!({})), |_| {
            Ok(json!({"call": context::current_call_id()}))
        })
        .unwrap();
        assert!(seen["call"].is_string());
        assert!(context::current().is_none());
    }

    #[test]
    fn the_store_override_reaches_the_handler() {
        fn reads_store(_: Value) -> ServiceFuture {
            Box::pin(async { Ok(json!(context::store_override::<u64>().map(|s| *s))) })
        }
        static READS: VerbDescriptor = VerbDescriptor {
            name: "t-service_reads",
            method: "reads",
            handler: reads_store,
            strict: false,
            budget_ms: None,
            ..ECHO
        };
        let seen =
            crate::runtime::block_on(invoke_on(Arc::new(9u64), &READS, Call::person(json!({}))))
                .unwrap();
        assert_eq!(seen, json!(9));
    }

    #[test]
    fn a_direct_call_to_a_renamed_verb_carries_no_deprecation_notice() {
        let answer = invoke_blocking(&RENAMED, Call::person(json!({}))).unwrap();
        assert!(
            answer.get("deprecated").is_none(),
            "the live name is not deprecated: {answer}"
        );
    }

    #[test]
    fn a_call_by_the_retired_alias_gets_an_additive_deprecation_notice() {
        let answer = invoke_blocking(
            &RENAMED,
            Call::person(json!({})).with_requested_name("t-service_old-echo"),
        )
        .unwrap();
        assert_eq!(
            answer["deprecated"],
            json!({
                "since": "0.7.0",
                "use": "t-service_renamed",
                "note": "renamed from t-service_old-echo",
            })
        );
        // additive: the rest of the envelope is untouched.
        assert_eq!(answer["ok"], true);
        assert!(answer.get("echo").is_some());
    }

    #[test]
    fn the_deprecation_notice_is_only_added_to_object_results() {
        fn as_array(_: Value) -> ServiceFuture {
            Box::pin(async { Ok(json!([1, 2, 3])) })
        }
        static ARRAY_RENAMED: VerbDescriptor = VerbDescriptor {
            name: "t-service_array-renamed",
            method: "array-renamed",
            handler: as_array,
            strict: false,
            budget_ms: None,
            ..RENAMED
        };
        let answer = invoke_blocking(
            &ARRAY_RENAMED,
            Call::person(json!({})).with_requested_name("t-service_old-echo"),
        )
        .unwrap();
        assert_eq!(answer, json!([1, 2, 3]), "an array result is unchanged");
    }
}
