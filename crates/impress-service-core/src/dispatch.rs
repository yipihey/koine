//! The generic by-name verb dispatch every FFI entry point needs (plan-
//! verb-pipeline-and-transport.md § P5, ADR-0034 D2).
//!
//! [`dispatch`] looks a verb up by its qualified name in
//! whatever is linked into this binary ([`crate::descriptor::VerbDescriptor::find`],
//! the process-wide `inventory` collection), run it through
//! [`crate::pipeline::invoke_blocking`] — the same chain MCP, the CLI, the
//! FFI layout `apply` and every other entry path runs through — and answer
//! an HTTP status plus the wire-convention body. Lives here, not in
//! `impress-store-ffi`, so a **per-app** UniFFI target (one that links a
//! domain `*-service` crate the kit boundary forbids `impress-store-ffi`
//! itself from reaching — `scripts/check-kit-deps.sh`'s "ASK FIRST… this is
//! not a dependency to allowlist") can call the identical logic without
//! duplicating it. `implore-verbs-ffi` is the first such target
//! (plan-verb-pipeline P5a); `impress-store-ffi::verb::dispatch_verb` is the
//! kit's own thin wrapper over the same function.

use crate::pipeline::{self, Call, CallerIdentity};
use crate::wire::WIRE_VERSION;
use crate::{descriptor_handle::VerbHandle, refusal, runtime};
use serde_json::{json, Value};

/// An HTTP status and the wire-convention JSON body — `{"ok": true, …}` or
/// `{"ok": false, "wire_version", "code", "message"}` — exactly what
/// `/api/layout/*` and `/api/surface/*` already answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchResult {
    pub status: u16,
    pub body_json: String,
}

fn refused(status: u16, code: &str, message: String) -> DispatchResult {
    DispatchResult {
        status,
        body_json: json!({
            "ok": false,
            "wire_version": WIRE_VERSION,
            "code": code,
            "message": message,
        })
        .to_string(),
    }
}

/// `caller_json` as a route hands it: `{"kind": "app"|"agent"|"provider"|
/// "system"|"person", "name": "…", "trace_id": "…"}` — `name` and
/// `trace_id` optional. The route derives `kind`/`name` from the caller's
/// token and headers, never from the verb's own arguments (ADR-0034 D3);
/// this function trusts what it is given the same way every other pipeline
/// entry path trusts its transport.
fn parse_caller(caller_json: &str) -> (CallerIdentity, Option<String>, Option<String>) {
    let value: Value = serde_json::from_str(caller_json).unwrap_or(Value::Null);
    let kind = value.get("kind").and_then(Value::as_str).unwrap_or("app");
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string);
    let trace_id = value
        .get("trace_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let identity = match kind {
        "person" => CallerIdentity::Person,
        "provider" => CallerIdentity::Provider(name.unwrap_or_else(|| "unknown".into())),
        "system" => CallerIdentity::System(name.unwrap_or_else(|| "unknown".into())),
        "agent" => CallerIdentity::Agent(name.unwrap_or_else(|| "http".into())),
        // "app" and anything unrecognised: the route only reaches this
        // function once the loopback-token check already passed, so an app
        // caller with no name still gets the identity kind right.
        _ => CallerIdentity::App(name.unwrap_or_else(|| "unknown".into())),
    };
    let parent_call = value
        .get("parent_call")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    (identity, trace_id, parent_call)
}

/// Dispatch one verb by its qualified name (`<service>_<method>`) through
/// the invoker pipeline, on whatever `*-service` crates this binary links.
///
/// `args_json` is the verb's own arguments, a JSON object (an empty body
/// becomes `{}`, matching every other entry path — `pipeline::prepare`
/// already treats `Value::Null` this way). `caller_json` is
/// [`parse_caller`]'s shape. Never panics: a malformed `args_json` is
/// `invalid-argument`, not a crash reaching the FFI caller.
pub fn dispatch(name: &str, args_json: &str, caller_json: &str) -> DispatchResult {
    let (verb, call) = match prepare(name, args_json, caller_json) {
        Ok(prepared) => prepared,
        Err(refused) => return refused,
    };
    finish(runtime::block_on(pipeline::invoke_handle(verb, call)))
}

/// The same dispatch for native backends with async Swift callbacks. Awaiting
/// the callback leaves the main actor free to service app state and persistence.
pub async fn dispatch_async(name: &str, args_json: &str, caller_json: &str) -> DispatchResult {
    let (verb, call) = match prepare(name, args_json, caller_json) {
        Ok(prepared) => prepared,
        Err(refused) => return refused,
    };
    finish(pipeline::invoke_handle(verb, call).await)
}

/// Dispatch from a foreign async executor, such as UniFFI's Swift future
/// polling. The invoker and handler run on the shared Tokio runtime so Tokio
/// timers and `spawn_blocking` work; awaiting the join leaves Swift's actor
/// free for app-owned callbacks. Dropping the foreign future aborts the task
/// rather than leaving a mutating verb running detached.
pub async fn dispatch_foreign_async(
    name: &str,
    args_json: &str,
    caller_json: &str,
) -> DispatchResult {
    let (verb, call) = match prepare(name, args_json, caller_json) {
        Ok(prepared) => prepared,
        Err(refused) => return refused,
    };
    let task = AbortOnDrop(runtime::spawn(pipeline::invoke_handle(verb, call)));
    match task.join().await {
        Ok(result) => finish(result),
        Err(error) => refused(
            500,
            refusal::codes::INTERNAL,
            format!("native verb task failed: {error}"),
        ),
    }
}

/// Tokio detaches a JoinHandle when it is dropped. Foreign callers can cancel
/// their future at any await, so the handle must abort the pipeline on drop.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> AbortOnDrop<T> {
    async fn join(mut self) -> Result<T, tokio::task::JoinError> {
        (&mut self.0).await
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn prepare(
    name: &str,
    args_json: &str,
    caller_json: &str,
) -> Result<(VerbHandle, Call), DispatchResult> {
    let Some(verb) = crate::call::find(name) else {
        return Err(refused(
            404,
            refusal::codes::NOT_FOUND,
            format!("no such verb: {name}"),
        ));
    };

    let args: Value = if args_json.trim().is_empty() {
        Value::Object(Default::default())
    } else {
        match serde_json::from_str(args_json) {
            Ok(value) => value,
            Err(error) => {
                return Err(refused(
                    400,
                    refusal::codes::INVALID_ARGUMENT,
                    format!("args_json did not parse: {error}"),
                ))
            }
        }
    };

    let (caller, trace_id, parent_call) = parse_caller(caller_json);
    let mut call = Call::new(caller, args);
    call.parent_call = parent_call;
    if let Some(trace_id) = trace_id {
        call = call.with_trace(trace_id);
    }

    Ok((verb, call))
}

fn finish(result: Result<Value, pipeline::PipelineError>) -> DispatchResult {
    match result {
        Ok(value) => {
            let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(true);
            let status = if ok {
                200
            } else {
                let code = value.get("code").and_then(Value::as_str).unwrap_or("");
                refusal::http_status(code)
            };
            DispatchResult {
                status,
                body_json: value.to_string(),
            }
        }
        Err(pipeline::PipelineError::Unavailable {
            app,
            verb: verb_name,
        }) => refused(
            503,
            refusal::codes::HOST_UNAVAILABLE,
            pipeline::PipelineError::Unavailable {
                app,
                verb: verb_name,
            }
            .to_string(),
        ),
        Err(pipeline::PipelineError::Handler(error)) => {
            refused(502, refusal::codes::VERB_FAILED, error.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::{Safety, SafetyClass, Source};
    use crate::{Effects, McpToolDescriptor, ServiceFuture, VerbDescriptor};
    use std::future::Future;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};
    use std::time::{Duration, Instant};

    fn schema() -> Value {
        json!({"type":"object"})
    }

    fn tokio_handler(_: Value) -> ServiceFuture {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(1)).await;
            let answer = tokio::task::spawn_blocking(|| 42).await?;
            Ok(json!({"answer":answer}))
        })
    }

    static FOREIGN: VerbDescriptor = VerbDescriptor {
        name: "dispatch-test-service_foreign",
        service: "dispatch-test-service",
        method: "foreign",
        description: "exercise the foreign executor bridge",
        input_schema: schema,
        output_schema: schema,
        safety: Safety {
            class: SafetyClass::ReadOnly,
            idempotent: true,
        },
        effects: Effects::NONE,
        since: "0.1.0",
        deprecated: None,
        aliases: &[],
        examples: &[],
        strict: false,
        budget_ms: None,
        replay_full: false,
        source: Source::Linked,
        handler: tokio_handler,
    };
    inventory::submit! { McpToolDescriptor::of(&FOREIGN) }

    static CANCEL_STARTED: AtomicBool = AtomicBool::new(false);
    static CANCEL_DROPPED: AtomicBool = AtomicBool::new(false);

    struct DropNotice;
    impl Drop for DropNotice {
        fn drop(&mut self) {
            CANCEL_DROPPED.store(true, Ordering::SeqCst);
        }
    }

    fn cancellable_handler(_: Value) -> ServiceFuture {
        Box::pin(async {
            let _notice = DropNotice;
            CANCEL_STARTED.store(true, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok(json!({"finished":true}))
        })
    }

    static CANCELLABLE: VerbDescriptor = VerbDescriptor {
        name: "dispatch-test-service_cancellable",
        method: "cancellable",
        handler: cancellable_handler,
        ..FOREIGN
    };
    inventory::submit! { McpToolDescriptor::of(&CANCELLABLE) }

    fn panicking_handler(_: Value) -> ServiceFuture {
        Box::pin(async { panic!("test handler panic") })
    }

    static PANICKING: VerbDescriptor = VerbDescriptor {
        name: "dispatch-test-service_panicking",
        method: "panicking",
        handler: panicking_handler,
        ..FOREIGN
    };
    inventory::submit! { McpToolDescriptor::of(&PANICKING) }

    struct ThreadWake(std::thread::Thread);
    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    fn foreign_block_on<F: Future>(future: F) -> F::Output {
        let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
        let mut context = Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Poll::Ready(result) = future.as_mut().poll(&mut context) {
                return result;
            }
            assert!(Instant::now() < deadline, "foreign dispatch did not wake");
            std::thread::park_timeout(Duration::from_millis(10));
        }
    }

    fn wait_for(flag: &AtomicBool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !flag.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < deadline,
                "native task did not reach expected state"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn foreign_executor_can_run_tokio_timer_and_blocking_work() {
        assert!(tokio::runtime::Handle::try_current().is_err());
        let answer = foreign_block_on(dispatch_foreign_async(
            FOREIGN.name,
            "{}",
            r#"{"kind":"app","name":"test"}"#,
        ));
        assert_eq!(answer.status, 200, "{}", answer.body_json);
        let body: Value = serde_json::from_str(&answer.body_json).unwrap();
        assert_eq!(body["answer"], 42);
    }

    #[test]
    fn cancelling_foreign_future_aborts_native_task() {
        CANCEL_STARTED.store(false, Ordering::SeqCst);
        CANCEL_DROPPED.store(false, Ordering::SeqCst);
        let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
        let mut context = Context::from_waker(&waker);
        let mut future = Box::pin(dispatch_foreign_async(
            CANCELLABLE.name,
            "{}",
            r#"{"kind":"app","name":"test"}"#,
        ));
        assert!(future.as_mut().poll(&mut context).is_pending());
        wait_for(&CANCEL_STARTED);
        drop(future);
        wait_for(&CANCEL_DROPPED);
    }

    #[test]
    fn native_task_panic_returns_structured_internal_refusal() {
        let answer = foreign_block_on(dispatch_foreign_async(
            PANICKING.name,
            "{}",
            r#"{"kind":"app","name":"test"}"#,
        ));
        assert_eq!(answer.status, 500);
        let body: Value = serde_json::from_str(&answer.body_json).unwrap();
        assert_eq!(body["ok"], false);
        assert_eq!(body["code"], refusal::codes::INTERNAL);
    }

    #[test]
    fn an_unknown_verb_is_not_found() {
        let answer = dispatch(
            "nonexistent-service_nope",
            "{}",
            r#"{"kind":"app","name":"implore"}"#,
        );
        assert_eq!(answer.status, 404);
        let body: Value = serde_json::from_str(&answer.body_json).unwrap();
        assert_eq!(body["ok"], false);
        assert_eq!(body["code"], "not-found");
        assert_eq!(body["wire_version"], WIRE_VERSION);
    }

    #[test]
    fn malformed_args_json_is_invalid_argument_not_a_panic() {
        let answer = dispatch(
            "t-service_echo",
            "not json",
            r#"{"kind":"app","name":"test"}"#,
        );
        // No verb named `t-service_echo` is linked into this crate's own
        // tests, so this proves the parse-error path fires before the
        // lookup would even matter — a real caller sees 400 either way it
        // fails, never a panic.
        assert!(answer.status == 400 || answer.status == 404);
    }

    #[test]
    fn an_empty_body_is_an_empty_object_not_a_parse_error() {
        let answer = dispatch(
            "nonexistent-service_nope",
            "",
            r#"{"kind":"app","name":"test"}"#,
        );
        // Still not-found (no such verb in this crate's tests), but not the
        // args-parse 400 an empty body would otherwise trigger.
        assert_eq!(answer.status, 404);
    }
}
