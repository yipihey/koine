//! The FFI to the Swift bridge (`swift/Sources/KoineFM/KoineFM.swift`).
//!
//! One turn: Rust allocates a [`Shared`] holding the sending half of an
//! unbounded channel and gives its address to Swift as the callback context.
//! The callbacks (on Swift's threads) copy the C strings they are handed into
//! the channel; the future below reads the channel, answers tool calls and
//! returns at the turn's terminal event.
//!
//! Ownership rule: Swift never touches the context after it has delivered the
//! terminal event, so the `Shared` is freed only once the future has *received*
//! that event. A turn Rust cancels — a future dropped early, or a failure that
//! makes [`drain`] cancel it — **leaks** the `Shared` (a mutex and a channel
//! handle, a few dozen bytes per cancelled turn), even after its terminal
//! event: cancelling resumes a tool call Swift may have parked but not yet
//! announced through `on_tool_call`, and that call would then reach a freed
//! context. Without a cancel a turn cannot end while a tool call is in flight
//! (the call waits for Rust), so a turn that ends on its own is freed.

use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::Mutex;
use std::time::Duration;

use koine_ai::catalogue::APPLE_ON_DEVICE_ID;
use koine_ai::{
    ChatRequest, Error, LoopError, LoopObserver, LoopOutcome, ModelContentPart, ModelMessage,
    ModelToolCall, Role, ToolCall, ToolOutcome, ToolRunner,
};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::wire::{self, Availability, FmEvent};

type EventCb = extern "C" fn(*mut c_void, *const c_char);
type ToolCb = extern "C" fn(*mut c_void, u64, *const c_char);

extern "C" {
    fn koine_fm_availability() -> *mut c_char;
    fn koine_fm_respond(
        request_json: *const c_char,
        ctx: *mut c_void,
        on_event: EventCb,
        on_tool_call: ToolCb,
    ) -> u64;
    fn koine_fm_tool_result(handle: u64, result_text: *const c_char);
    fn koine_fm_cancel(turn_id: u64);
    fn koine_fm_free(pointer: *mut c_char);
}

enum Msg {
    Fm(String),
    Tool(u64, String),
}

/// The callback context. The sender is taken out at the terminal event.
struct Shared {
    tx: Mutex<Option<UnboundedSender<Msg>>>,
}

/// Hand `msg` to the channel. At a terminal event the sender is taken out of
/// the mutex *before* sending: once the receiver sees the terminal event it
/// may free `shared`, so nothing after the send may touch it.
fn forward(shared: &Shared, msg: Msg, terminal: bool) {
    let tx = {
        let mut guard = shared.tx.lock().unwrap_or_else(|e| e.into_inner());
        if terminal {
            guard.take()
        } else {
            guard.clone()
        }
    };
    if let Some(tx) = tx {
        let _ = tx.send(msg);
    }
}

fn copy_c_string(pointer: *const c_char) -> String {
    if pointer.is_null() {
        return String::new();
    }
    // SAFETY: Swift passes a NUL-terminated string that is valid for the
    // duration of the callback; it is copied before returning.
    unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned()
}

extern "C" fn on_event(ctx: *mut c_void, json: *const c_char) {
    if ctx.is_null() {
        return;
    }
    let text = copy_c_string(json);
    let terminal = wire::is_terminal_json(&text);
    // SAFETY: `ctx` is the `Shared` leaked by `run`; it is freed only after the
    // terminal event has been received, and Swift sends nothing after it.
    let shared = unsafe { &*(ctx as *const Shared) };
    forward(shared, Msg::Fm(text), terminal);
}

extern "C" fn on_tool_call(ctx: *mut c_void, handle: u64, json: *const c_char) {
    if ctx.is_null() {
        return;
    }
    let text = copy_c_string(json);
    // SAFETY: as in `on_event`; a tool call precedes the terminal event.
    let shared = unsafe { &*(ctx as *const Shared) };
    forward(shared, Msg::Tool(handle, text), false);
}

/// Owns the context of one turn. `Drop` frees it if the terminal event was
/// received, and otherwise cancels the turn and leaks it (see the module docs).
struct Turn {
    id: u64,
    shared: *mut Shared,
    received_terminal: bool,
}

// SAFETY: `Shared` is a `Mutex` around a channel sender, so it may be used
// from any thread; the raw pointer is only dereferenced here (in `Drop`, once)
// and by the callbacks, which only take `&Shared`.
unsafe impl Send for Turn {}

impl Drop for Turn {
    fn drop(&mut self) {
        if self.received_terminal {
            // SAFETY: allocated by `Box::into_raw` in `run`, freed exactly once
            // here, and Swift has finished with it (it sent the terminal event).
            drop(unsafe { Box::from_raw(self.shared) });
        } else {
            // SAFETY: cancelling an unknown or finished turn is a no-op in Swift.
            unsafe { koine_fm_cancel(self.id) };
            // Deliberately not freed: Swift may still deliver its last event.
        }
    }
}

fn send_tool_result(handle: u64, text: &str) {
    // A NUL cannot occur in JSON text; replace defensively rather than panic.
    let c = CString::new(text.replace('\0', "")).unwrap_or_default();
    // SAFETY: `c` is a valid NUL-terminated string for the call; Swift copies
    // it. A handle that is no longer pending is ignored by Swift.
    unsafe { koine_fm_tool_result(handle, c.as_ptr()) };
}

pub fn availability() -> Availability {
    // SAFETY: returns a strdup'ed string or null, which is freed below.
    let pointer = unsafe { koine_fm_availability() };
    if pointer.is_null() {
        return Availability::unavailable("the Foundation Models bridge returned nothing");
    }
    let text = copy_c_string(pointer);
    // SAFETY: `pointer` came from `koine_fm_availability` and is freed once.
    unsafe { koine_fm_free(pointer) };
    Availability::parse(&text)
}

/// After a failure: cancel the turn and answer, for a moment, any tool call
/// still arriving, so Swift is not left waiting. The context is not freed
/// even if the terminal event arrives (see the module docs), so `Turn::drop`
/// leaks it.
async fn drain(turn: &mut Turn, rx: &mut UnboundedReceiver<Msg>) {
    // SAFETY: as in `Turn::drop`.
    unsafe { koine_fm_cancel(turn.id) };
    let wait = async {
        while let Some(msg) = rx.recv().await {
            match msg {
                Msg::Fm(json) if wire::is_terminal_json(&json) => return,
                Msg::Tool(handle, _) => send_tool_result(handle, "cancelled"),
                _ => {}
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(2), wait).await;
    debug_assert!(!turn.received_terminal);
}

fn tool_message(call: &ToolCall, text: &str) -> [ModelMessage; 2] {
    [
        ModelMessage {
            role: Role::Assistant,
            content: vec![],
            name: None,
            tool_call_id: None,
            tool_calls: vec![ModelToolCall {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.raw.clone(),
            }],
        },
        ModelMessage {
            role: Role::Tool,
            content: vec![ModelContentPart::Text { text: text.into() }],
            name: Some(call.name.clone()),
            tool_call_id: Some(call.id.clone()),
            tool_calls: vec![],
        },
    ]
}

pub async fn run(
    request: &ChatRequest,
    tools: Option<&dyn ToolRunner>,
    observer: &dyn LoopObserver,
) -> Result<LoopOutcome, LoopError> {
    let built = wire::build_request(request).map_err(LoopError::Invalid)?;
    for (name, reason) in &built.skipped {
        tracing::warn!(target: "koine-ai-apple", "tool {name} left out: {reason}");
    }
    let c_request = CString::new(built.json.to_string())
        .map_err(|e| LoopError::Invalid(Error::Invalid(e.to_string())))?;

    let (tx, mut rx) = unbounded_channel();
    let shared = Box::into_raw(Box::new(Shared {
        tx: Mutex::new(Some(tx)),
    }));
    // SAFETY: `c_request` is NUL-terminated and outlives the call (Swift copies
    // it before returning); `shared` stays allocated until `Turn` frees it
    // after the terminal event; the callbacks have the declared signatures.
    let id = unsafe { koine_fm_respond(c_request.as_ptr(), shared.cast(), on_event, on_tool_call) };
    let mut turn = Turn {
        id,
        shared,
        received_terminal: false,
    };

    let previews = built.tools_offered == 0;
    let mut text = String::new();
    let mut appended: Vec<ModelMessage> = Vec::new();
    loop {
        let Some(msg) = rx.recv().await else {
            return Err(LoopError::Exhausted(
                "the Foundation Models bridge closed without a final event".into(),
            ));
        };
        match msg {
            Msg::Fm(json) => match FmEvent::parse(&json) {
                Ok(FmEvent::Text(delta)) => {
                    text.push_str(&delta);
                    if previews {
                        if let Err(error) = observer.on_text(&text) {
                            drain(&mut turn, &mut rx).await;
                            return Err(LoopError::Tool(error));
                        }
                    }
                }
                Ok(FmEvent::Done { content, usage }) => {
                    turn.received_terminal = true;
                    let mut messages = request.messages.clone();
                    messages.extend(appended);
                    return Ok(LoopOutcome {
                        content,
                        reasoning: String::new(),
                        usage,
                        finish_reason: Some("stop".into()),
                        messages,
                    });
                }
                Ok(FmEvent::Error { kind, message }) => {
                    turn.received_terminal = true;
                    return Err(wire::map_error(APPLE_ON_DEVICE_ID, &kind, &message));
                }
                // Not marked received: Swift may still be running.
                Err(error) => return Err(LoopError::Stream(error)),
            },
            Msg::Tool(handle, json) => {
                let (id, name, arguments, raw) = match wire::parse_tool_call(&json) {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        send_tool_result(handle, &error);
                        drain(&mut turn, &mut rx).await;
                        return Err(LoopError::Invalid(Error::Invalid(error)));
                    }
                };
                let call = ToolCall {
                    id,
                    name,
                    arguments,
                    raw,
                };
                let outcome = match tools {
                    Some(runner) => runner.run(call.clone()).await,
                    None => Ok(ToolOutcome::Refused {
                        code: "no_tools".into(),
                        message: "No tools are available for this turn.".into(),
                    }),
                };
                match outcome {
                    Ok(outcome) => {
                        let result = outcome.model_text();
                        send_tool_result(handle, &result);
                        appended.extend(tool_message(&call, &result));
                    }
                    Err(error) => {
                        send_tool_result(handle, &format!("{{\"error\":\"{error}\"}}"));
                        drain(&mut turn, &mut rx).await;
                        return Err(LoopError::Tool(error));
                    }
                }
            }
        }
    }
}
