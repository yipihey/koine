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
//! 3. **reachability** — one rule and one probe ([`reachability`]); then
//!    the verb's declared **secrets** ([`crate::secrets`]), refused
//!    `missing-secret` while one is absent.
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
//! Adverbs (docs/grammar.md § Adverbs) thread through these: parsed after
//! identity and before strict args; support-checked before reachability;
//! `with_review`, `tentatively`, `once`, `in_background` and `every` are settled right
//! after policy decides; `within` wraps invoke; `reversibly` and the
//! `simulated` tag are applied in the envelope layer; the span and the audit
//! row record them ([`adverbs`] holds the host hooks).
//!
//! Hand-rolled rather than tower (ADR-0034 § Alternatives): the chain must
//! wrap the two bypasses that carry no request type, which
//! [`invoke_sync_with`] and [`invoke_with`] do by taking the handler step as
//! a closure.

pub mod adverbs;
pub mod audit;
pub mod context;
pub mod identity;
pub mod materialize;
pub mod perf;
pub mod policy;
pub mod reachability;
pub mod transport;

use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::adverb::{Adverb, Adverbs};
use crate::descriptor::{SafetyClass, VerbDescriptor};
use crate::descriptor_handle::VerbHandle;
use crate::refusal::{codes, Refusal};
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
    /// The adverbs a typed entry path (CLI, surface action, scenario step)
    /// sets directly; merged with any the transport carried under the
    /// `adverbs` argument key (docs/grammar.md § Adverbs).
    pub adverbs: Adverbs,
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
            adverbs: Adverbs::default(),
        }
    }

    /// This call with `adverbs` set.
    pub fn with_adverbs(mut self, adverbs: Adverbs) -> Self {
        self.adverbs = adverbs;
        self
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
    adverbs: Adverbs,
    disposition: Disposition,
    /// `policy::decide` took a write lease this call must give back.
    holds_lease: bool,
    /// `(caller, key)` to record a successful result under (`once`).
    once: Option<(String, String)>,
    /// The inverse the envelope layer attached (`reversibly`).
    undo: Option<Value>,
    /// Record the call even when the verb is read-only (a tentative or
    /// background call is on the record whatever its class).
    audit_always: bool,
}

/// What the invoke step does for a prepared call.
enum Disposition {
    /// Run the handler.
    Run,
    /// Run the handler of a verb that declares `supports(tentatively)`; it
    /// must write nothing, and the result is tagged `"tentative":"simulated"`.
    Simulated,
    /// The pipeline already has the answer (tentative-static, a `once`
    /// replay, a started background job): the handler does not run.
    Answer(Value),
}

/// Whether `verb` itself declares `adverb` (a universal adverb is
/// [`VerbHandle::supports`] for every verb, which is not a declaration).
fn declares(verb: &VerbHandle, adverb: Adverb) -> bool {
    verb.linked().is_some_and(|l| l.supports.contains(&adverb))
}

/// Rule 1 (docs/grammar.md § Adverbs): an adverb the verb cannot honor is
/// refused, never silently dropped.
fn check_adverbs(verb: &VerbHandle, adverbs: &Adverbs, sync_entry: bool) -> Result<(), Refusal> {
    let read_only = verb.safety().class == SafetyClass::ReadOnly;
    for adverb in adverbs.used() {
        match adverb {
            Adverb::InBackground => {
                if !adverbs::has_background_runner() {
                    return Err(Refusal::new(
                        codes::UNSUPPORTED_ADVERB,
                        format!(
                            "{} cannot run in_background: no background runner is installed in \
                             this process",
                            verb.name()
                        ),
                    ));
                }
            }
            Adverb::Every => {
                if !verb.supports(adverb) {
                    return Err(Refusal::unsupported_adverb(adverb, verb.name()));
                }
                if !adverbs::has_background_runner() {
                    return Err(Refusal::new(
                        codes::UNSUPPORTED_ADVERB,
                        format!(
                            "{} cannot run every: no background runner is installed in \
                             this process",
                            verb.name()
                        ),
                    ));
                }
            }
            Adverb::Within => {
                if sync_entry {
                    return Err(Refusal::new(
                        codes::UNSUPPORTED_ADVERB,
                        format!(
                            "{} cannot honor 'within': the synchronous entry path cannot time \
                             a call out",
                            verb.name()
                        ),
                    ));
                }
                if !read_only && !verb.supports(adverb) {
                    return Err(Refusal::unsupported_adverb(adverb, verb.name()));
                }
            }
            Adverb::Reversibly => {
                if !read_only && !verb.supports(adverb) {
                    return Err(Refusal::unsupported_adverb(adverb, verb.name()));
                }
            }
            Adverb::Tentatively | Adverb::WithReview | Adverb::Once => {}
        }
    }
    Ok(())
}

/// What `tentatively` answers without running the handler (rule 2).
fn tentative_static(
    verb: &VerbHandle,
    caller: &CallerIdentity,
    decision: &policy::Decision,
) -> Value {
    let safety = verb.safety();
    let [reads, writes, reach] = verb.effects().columns();
    let (would, reason) = match decision {
        policy::Decision::Run => ("run", None),
        policy::Decision::Review => ("review", None),
        policy::Decision::Deny(reason) => ("deny", Some(reason.clone())),
        policy::Decision::Conflict(kinds) => ("conflict", Some(kinds.clone())),
    };
    let mut answer = serde_json::json!({
        "ok": true,
        "tentative": "static",
        "verb": verb.name(),
        "safety": safety.class.to_string(),
        "idempotent": safety.idempotent,
        "effects": { "reads": reads, "writes": writes, "reach": reach },
        "caller": caller.to_json(),
        "would": would,
        "wire_version": crate::wire::WIRE_VERSION,
    });
    if let Some(reason) = reason {
        answer["reason"] = Value::String(reason);
    }
    answer
}

/// `verb <name> needs secret <n> (not set): <how_to_set>` for every missing
/// name, code `missing-secret`.
pub fn missing_secret_refusal(verb: &str, missing: &[&str]) -> Refusal {
    let needs = missing
        .iter()
        .map(|name| {
            format!(
                "secret {name} (not set): {}",
                crate::secrets::how_to_set(name)
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    Refusal::new(codes::MISSING_SECRET, format!("verb {verb} needs {needs}"))
}

fn is_ok(value: &Value) -> bool {
    value.get("ok").and_then(Value::as_bool).unwrap_or(true)
}

/// The layers before the handler: identity, adverbs, strict args, the adverb
/// support check, reachability, policy (with `with_review`, `tentatively`,
/// `once` and `in_background` decided after it), the span. `Ok(Err(value))` is a refusal envelope to answer
/// without running the handler.
fn prepare(
    verb: &VerbHandle,
    call: Call,
    sync_entry: bool,
) -> Result<Result<Prepared, Value>, PipelineError> {
    prepare_inner(verb, call, false, sync_entry)
}

// A provider refresh can fail before policy. Keep that refusal on the normal
// span/audit path, while deliberately skipping checks that would short-circuit
// before `finish` and obscure the original host-unavailable diagnosis.
fn prepare_inner(
    verb: &VerbHandle,
    call: Call,
    preflight_refusal: bool,
    sync_entry: bool,
) -> Result<Result<Prepared, Value>, PipelineError> {
    run_installers();
    let Call {
        args,
        caller,
        trace_id,
        parent_call,
        store,
        requested_name,
        adverbs: call_adverbs,
    } = call;
    let mut args = if args.is_null() {
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

    // 1b. adverbs — the reserved `adverbs` key is removed from the arguments
    // before anything else reads them, so a verb's arguments are exactly what
    // its schema says. A forced provider preflight refusal ignores a
    // malformed key (it is answered as the original refusal).
    let wire = match Adverbs::take_from(&mut args) {
        Ok(wire) => wire,
        Err(_) if preflight_refusal => Adverbs::default(),
        Err(refusal) => return Ok(Err(crate::strict::refusal_value(&refusal))),
    };
    let adverbs = call_adverbs.merge(wire);

    // 2. strict args.
    if !preflight_refusal && verb.strict() {
        let schema = handle_schema(verb, false);
        if let Err(refusal) = crate::strict::check_args(verb.name(), &args, &schema) {
            return Ok(Err(crate::strict::refusal_value(&refusal)));
        }
    }

    // 2b. adverb support — before reachability, so a call that cannot be
    // honored never probes an app.
    if !preflight_refusal {
        if let Err(refusal) = check_adverbs(verb, &adverbs, sync_entry) {
            return Ok(Err(crate::strict::refusal_value(&refusal)));
        }
    }

    let mut disposition = Disposition::Run;
    let mut holds_lease = false;
    let mut once = None;

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

        // 3b. secrets — a linked verb that declares a secret does not run
        // while it is absent, so its handler never sees `None` for a name
        // it declared (plan-koine-followups F-D1). Refused before policy:
        // nothing is asked of a reviewer for a call that cannot run.
        if let Some(linked) = verb.linked() {
            let missing = crate::secrets::missing_for(linked);
            if !missing.is_empty() {
                return Ok(Err(crate::strict::refusal_value(&missing_secret_refusal(
                    linked.name,
                    &missing,
                ))));
            }
        }

        // 4. policy. A call that will not run for the policy's sake
        // (`tentatively`, `with_review`) takes no write lease.
        let leaseless = adverbs.tentatively || adverbs.with_review;
        let mut decision = if leaseless {
            policy::decide_without_lease(&caller, verb)
        } else {
            policy::decide(&caller, verb)
        };
        if adverbs.with_review && decision == policy::Decision::Run {
            decision = policy::Decision::Review;
        }
        if adverbs.tentatively {
            // Answered after policy has decided, before it acts.
            disposition =
                if decision == policy::Decision::Run && declares(verb, Adverb::Tentatively) {
                    Disposition::Simulated
                } else {
                    Disposition::Answer(tentative_static(verb, &caller, &decision))
                };
        } else {
            match decision {
                policy::Decision::Run => holds_lease = !leaseless,
                policy::Decision::Review => return Ok(Err(policy::queue(&caller, verb, &args))),
                policy::Decision::Deny(reason) => return Ok(Err(policy::deny(verb, reason))),
                // D-R11: a destructive verb whose declared writes overlap another
                // call's still in flight on the same kind.
                policy::Decision::Conflict(kinds) => return Ok(Err(policy::conflict(verb, kinds))),
            }

            // `once`: a hit answers the stored envelope without running.
            let author = caller.author();
            if let Some(key) = &adverbs.once {
                if let Some(mut stored) = adverbs::once_lookup(&author, key) {
                    if let Some(object) = stored.as_object_mut() {
                        object.insert("replayed".into(), Value::Bool(true));
                    }
                    policy::give_back_lease(verb);
                    holds_lease = false;
                    disposition = Disposition::Answer(stored);
                } else {
                    once = Some((author.clone(), key.clone()));
                }
            }

            // `in_background`: the runner replaces invoke, after every check
            // above has passed (it cannot bypass policy).
            // `every` implies it: the runner's `repeat` replaces `start`.
            if (adverbs.in_background || adverbs.every.is_some())
                && matches!(disposition, Disposition::Run)
            {
                policy::give_back_lease(verb);
                holds_lease = false;
                let answer = match adverbs::background_runner() {
                    Some(runner) => {
                        let mut inner = adverbs.clone();
                        inner.in_background = false;
                        inner.every = None;
                        inner.once = None;
                        let handed = Call {
                            args: args.clone(),
                            caller: caller.clone(),
                            trace_id: Some(trace_id.clone()),
                            parent_call: Some(call_id.clone()),
                            store: store.clone(),
                            requested_name: requested_name.clone(),
                            adverbs: inner,
                        };
                        let started = match adverbs.every {
                            Some(interval) => runner.repeat(verb.clone(), handed, interval),
                            None => runner.start(verb.clone(), handed),
                        };
                        match started {
                            Ok(handle) => serde_json::to_value(crate::job::JobStarted::started(
                                handle.id,
                                handle.kind,
                            ))
                            .unwrap_or_default(),
                            Err(refusal) => crate::strict::refusal_value(&refusal),
                        }
                    }
                    None => crate::strict::refusal_value(&Refusal::new(
                        codes::UNSUPPORTED_ADVERB,
                        format!(
                            "{} cannot run {}: no background runner is installed \
                             in this process",
                            verb.name(),
                            if adverbs.every.is_some() {
                                "every"
                            } else {
                                "in_background"
                            }
                        ),
                    )),
                };
                if let (Some((author, key)), true) = (once.take(), is_ok(&answer)) {
                    adverbs::once_record(&author, &key, &answer);
                }
                disposition = Disposition::Answer(answer);
            }
        }
    }

    // 5. span.
    let arg_bytes = audit::approx_size(&args);
    let adverbs_record = match adverbs.to_record_value() {
        Value::Null => String::new(),
        record => record.to_string(),
    };
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
        adverbs = adverbs_record.as_str(),
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
        adverbs: adverbs.clone(),
        inverse: context::InverseSlot::default(),
    });
    Ok(Ok(Prepared {
        context,
        args,
        started: Instant::now(),
        started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        arg_bytes,
        span,
        requested_name,
        audit_always: adverbs.tentatively || adverbs.in_background || adverbs.every.is_some(),
        adverbs,
        disposition,
        holds_lease,
        once,
        undo: None,
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

/// The envelope layer's adverb work after a handler ran: `reversibly`'s
/// `undo` (rule 3), the `simulated` tag, and recording a `once` result.
fn adverb_envelope(
    verb: &VerbHandle,
    prepared: &mut Prepared,
    result: Result<Value, BoxError>,
) -> Result<Value, BoxError> {
    let simulated = matches!(prepared.disposition, Disposition::Simulated);
    let mut result = result;
    if prepared.adverbs.reversibly && !simulated {
        result = match result {
            Ok(mut value) => {
                if verb.safety().class == SafetyClass::ReadOnly {
                    // Reversible by construction.
                    if let Some(object) = value.as_object_mut() {
                        object.insert("undo".into(), Value::Null);
                    }
                    Ok(value)
                } else if !is_ok(&value) {
                    Ok(value)
                } else {
                    match prepared.context.inverse.take() {
                        Some((inverse_verb, inverse_args)) => {
                            let undo =
                                serde_json::json!({"verb": inverse_verb, "args": inverse_args});
                            if let Some(object) = value.as_object_mut() {
                                object.insert("undo".into(), undo.clone());
                            }
                            prepared.undo = Some(undo);
                            Ok(value)
                        }
                        None => Ok(crate::strict::refusal_value(&Refusal::new(
                            codes::EFFECT_FAILED,
                            format!(
                                "{} honored 'reversibly' but recorded no inverse call, so the \
                                 change cannot be undone",
                                verb.name()
                            ),
                        ))),
                    }
                }
            }
            err => err,
        };
    }
    if simulated {
        if let Ok(Value::Object(object)) = &mut result {
            object.insert("tentative".into(), Value::String("simulated".into()));
        }
    }
    if let (Some((author, key)), Ok(value)) = (&prepared.once, &result) {
        if is_ok(value) {
            adverbs::once_record(author, key, value);
        }
    }
    result
}

/// `within`'s answer when the deadline passed.
fn timed_out(verb: &VerbHandle, ms: u64) -> Value {
    crate::strict::refusal_value(&Refusal::new(
        codes::TIMED_OUT,
        format!("{} did not answer within {ms} ms", verb.name()),
    ))
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
    if !force_audit && prepared.holds_lease {
        policy::release(verb);
    }
    let adverbs_record = prepared.adverbs.to_record_value();
    let Prepared {
        context,
        span,
        started,
        started_at,
        arg_bytes,
        requested_name,
        audit_always,
        undo,
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

    if force_audit
        || audit_always
        || verb.safety().class != SafetyClass::ReadOnly
        || audit::log_all()
    {
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
            adverbs: adverbs_record,
            undo,
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
            let prepared = prepare_inner(&verb, call, true, false)?
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
    let mut prepared = match prepare(verb, call, false)? {
        Ok(prepared) => prepared,
        Err(refused) => return Ok(refused),
    };
    let result = if let Disposition::Answer(answer) = &prepared.disposition {
        Ok(answer.clone())
    } else {
        let entered = prepared.span.clone();
        let within = prepared.adverbs.within;
        let result = {
            let _guard = entered.enter();
            let future = async {
                let result = work(prepared.args.clone()).await;
                native_backend_result(result)
            };
            drop(_guard);
            let run = tracing::Instrument::instrument(
                context::scope(prepared.context.clone(), future),
                entered,
            );
            match within {
                // Rule 5: the handler future is dropped at the deadline.
                Some(within) => {
                    match tokio::time::timeout(Duration::from_millis(within.ms), run).await {
                        Ok(result) => result,
                        Err(_) => Ok(timed_out(verb, within.ms)),
                    }
                }
                None => run.await,
            }
        };
        let result = apply_deprecation_notice(verb, prepared.requested_name.as_deref(), result);
        // L-D4: a linked verb's rows become records when a host installed a
        // materialiser; after the handler, outside the effects check, never
        // failing the call.
        if let (Some(linked), Ok(value)) = (verb.linked(), &result) {
            materialize::after_verb(linked, value);
        }
        adverb_envelope(verb, &mut prepared, result)
    };
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
    let mut prepared = match prepare(verb, call, true)? {
        Ok(prepared) => prepared,
        Err(refused) => return Ok(refused),
    };
    let result = if let Disposition::Answer(answer) = &prepared.disposition {
        Ok(answer.clone())
    } else {
        let result = {
            let _guard = prepared.span.enter();
            context::sync_scope(prepared.context.clone(), || {
                native_backend_result(work(prepared.args.clone()))
            })
        };
        let result = apply_deprecation_notice(verb, prepared.requested_name.as_deref(), result);
        // L-D4: a linked verb's rows become records when a host installed a
        // materialiser; after the handler, outside the effects check, never
        // failing the call.
        if let (Some(linked), Ok(value)) = (verb.linked(), &result) {
            materialize::after_verb(linked, value);
        }
        adverb_envelope(verb, &mut prepared, result)
    };
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
        supports: &[],
        adjective: None,
        returns_kind: None,
        rows: None,
        secrets: &[],
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

    static NEEDS_TOKEN: VerbDescriptor = VerbDescriptor {
        name: "t-service_needs-token",
        method: "needs-token",
        strict: false,
        secrets: &["t-x"],
        ..ECHO
    };

    /// F-D1: a declared secret that no source holds refuses the call before
    /// the handler (and before policy); once a source holds it, the call
    /// runs. A test double stands in for the source, so the process
    /// environment is untouched.
    #[test]
    fn a_verb_whose_declared_secret_is_absent_is_refused_missing_secret() {
        use crate::secrets::tests::{MapSource, INSTALL_LOCK};
        let _guard = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        crate::secrets::install(MapSource::with(&[]));
        let answer = invoke_blocking(&NEEDS_TOKEN, Call::person(json!({}))).unwrap();
        assert_eq!(answer["ok"], false);
        assert_eq!(answer["code"], "missing-secret");
        assert_eq!(
            answer["message"],
            "verb t-service_needs-token needs secret t-x (not set): set KOINE_SECRET_T_X in the \
             environment, or PUT /api/secret/t-x on the host"
        );
        assert_eq!(
            crate::refusal::http_status(answer["code"].as_str().unwrap()),
            412
        );

        crate::secrets::install(MapSource::with(&[("t-x", "token")]));
        let answer = invoke_blocking(&NEEDS_TOKEN, Call::person(json!({}))).unwrap();
        assert_eq!(answer["ok"], true, "{answer}");

        // A verb that declares nothing is never gated.
        crate::secrets::install(MapSource::with(&[]));
        let answer = invoke_blocking(&ECHO, Call::person(json!({}))).unwrap();
        assert_eq!(answer["ok"], true);
    }

    #[test]
    fn every_missing_secret_is_named_in_one_refusal() {
        let r = missing_secret_refusal("v", &["a-b", "c"]);
        assert_eq!(r.code, "missing-secret");
        assert!(r.message.contains("secret a-b (not set)"), "{}", r.message);
        assert!(r.message.contains("secret c (not set)"), "{}", r.message);
        assert!(r.message.contains("KOINE_SECRET_A_B"), "{}", r.message);
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

    // ----- adverbs (docs/grammar.md § Adverbs) -----------------------------

    use crate::adverb::Within;
    use adverbs::IdempotencyStore as _;
    use policy::Policy as _;
    use std::collections::HashMap;

    fn hit_table() -> &'static Mutex<HashMap<String, usize>> {
        static HITS: std::sync::OnceLock<Mutex<HashMap<String, usize>>> =
            std::sync::OnceLock::new();
        HITS.get_or_init(Default::default)
    }
    fn bump() -> usize {
        let name = context::current().map(|c| c.verb.to_string()).unwrap();
        let mut hits = hit_table().lock().unwrap();
        let n = hits.entry(name).or_insert(0);
        *n += 1;
        *n
    }
    /// How many times the handler of `verb` ran.
    fn hits(verb: &str) -> usize {
        hit_table().lock().unwrap().get(verb).copied().unwrap_or(0)
    }

    fn counting(args: Value) -> ServiceFuture {
        Box::pin(async move {
            bump();
            Ok(json!({"ok": true, "echo": args}))
        })
    }
    fn inverse_noting(_: Value) -> ServiceFuture {
        Box::pin(async move {
            bump();
            context::note_inverse("t-service_unmake", json!({"id": "x1"}));
            Ok(json!({"ok": true}))
        })
    }
    fn simulating(_: Value) -> ServiceFuture {
        Box::pin(async move {
            bump();
            Ok(json!({"ok": true, "saw_tentative": context::current_adverbs().tentatively}))
        })
    }
    fn slow(_: Value) -> ServiceFuture {
        Box::pin(async move {
            bump();
            tokio::time::sleep(Duration::from_millis(2_000)).await;
            Ok(json!({"ok": true}))
        })
    }
    /// Refuses the first time it runs, answers the second.
    fn flaky(_: Value) -> ServiceFuture {
        Box::pin(async move {
            if bump() == 1 {
                Ok(json!({"ok": false, "code": "not-found", "message": "first try"}))
            } else {
                Ok(json!({"ok": true, "n": 2}))
            }
        })
    }

    macro_rules! adv_verb {
        ($id:ident, $name:literal, $handler:ident $(, $f:ident : $v:expr)*) => {
            static $id: VerbDescriptor = VerbDescriptor {
                name: $name,
                method: $name,
                handler: $handler,
                strict: false,
                budget_ms: None,
                $($f: $v,)*
                ..ECHO
            };
        };
    }
    const READ_ONLY: Safety = Safety {
        class: SafetyClass::ReadOnly,
        idempotent: true,
    };
    adv_verb!(BAD_KEY, "t-service_adv-bad-key", counting);
    adv_verb!(UNSUPPORTED, "t-service_adv-unsupported", counting);
    adv_verb!(TENT, "t-service_adv-tent", counting);
    adv_verb!(TENT_WRITES, "t-service_adv-tent-writes", counting, effects: crate::Effects {
        reads: &[],
        writes: &[crate::descriptor::Kind::Ref("manuscript")],
        reach: &[],
    });
    adv_verb!(REVIEW, "t-service_adv-review", counting);
    adv_verb!(TENT_SIM, "t-service_adv-sim", simulating, supports: &[Adverb::Tentatively]);
    adv_verb!(REV, "t-service_adv-rev", inverse_noting, supports: &[Adverb::Reversibly]);
    adv_verb!(REV_NONE, "t-service_adv-rev-none", counting, supports: &[Adverb::Reversibly]);
    adv_verb!(REV_READ, "t-service_adv-rev-read", counting, safety: READ_ONLY);
    adv_verb!(ONCE, "t-service_adv-once", counting);
    adv_verb!(ONCE_FLAKY, "t-service_adv-once-flaky", flaky);
    adv_verb!(ONCE_SEEDED, "t-service_adv-once-seeded", counting);
    adv_verb!(WITHIN_SLOW, "t-service_adv-within-slow", slow, safety: READ_ONLY);
    adv_verb!(WITHIN_MUT, "t-service_adv-within-mut", counting);
    adv_verb!(WITHIN_OK, "t-service_adv-within-ok", slow, supports: &[Adverb::Within]);
    adv_verb!(WITHIN_SYNC, "t-service_adv-within-sync", counting, safety: READ_ONLY);
    adv_verb!(BACKGROUND, "t-service_adv-background", counting, supports: &[Adverb::Within]);
    adv_verb!(EVERY, "t-service_adv-every", counting, supports: &[Adverb::Every]);

    fn with(adverbs: Adverbs) -> Call {
        Call::person(json!({})).with_adverbs(adverbs)
    }
    fn used(record: &audit::VerbCallRecord) -> Value {
        record.adverbs["used"].clone()
    }
    fn record_of(verb: &str) -> audit::VerbCallRecord {
        captured()
            .0
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|r| r.verb == verb)
            .cloned()
            .unwrap_or_else(|| panic!("no audit row for {verb}"))
    }

    #[test]
    fn a_misspelt_adverb_is_invalid_argument_naming_it() {
        let answer = invoke_blocking(
            &BAD_KEY,
            Call::person(json!({"adverbs": {"tentativly": true}})),
        )
        .unwrap();
        assert_eq!(answer["ok"], false);
        assert_eq!(answer["code"], "invalid-argument");
        assert!(answer["message"].as_str().unwrap().contains("tentativly"));
        assert_eq!(hits("t-service_adv-bad-key"), 0);
    }

    #[test]
    fn the_wire_key_is_stripped_before_the_handler_and_merged_with_the_typed_field() {
        let answer = invoke_blocking(
            &REVIEW,
            Call::person(json!({"n": 1, "adverbs": {"tentatively": true}})),
        )
        .unwrap();
        assert_eq!(answer["tentative"], "static");
        // strict args never saw `adverbs`: a strict verb accepts the key.
        let answer = invoke_blocking(
            &ECHO,
            Call::person(json!({"n": 1, "adverbs": {"once": "strip-key"}})).with_adverbs(Adverbs {
                tentatively: true,
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(answer["tentative"], "static", "{answer}");
    }

    #[test]
    fn reversibly_on_a_mutating_verb_without_support_is_unsupported() {
        let answer = invoke_blocking(
            &UNSUPPORTED,
            Call::person(json!({"adverbs": {"reversibly": true}})),
        )
        .unwrap();
        assert_eq!(answer["code"], "unsupported-adverb");
        let message = answer["message"].as_str().unwrap();
        assert!(message.contains("reversibly") && message.contains("t-service_adv-unsupported"));
        assert_eq!(hits("t-service_adv-unsupported"), 0);
    }

    #[test]
    fn tentatively_answers_statically_without_running_and_is_on_the_record() {
        let answer = invoke_blocking(
            &TENT_WRITES,
            Call::agent("t", json!({"adverbs": {"tentatively": true}})),
        )
        .unwrap();
        assert_eq!(answer["ok"], true);
        assert_eq!(answer["tentative"], "static");
        assert_eq!(answer["would"], "run");
        assert_eq!(answer["verb"], "t-service_adv-tent-writes");
        assert_eq!(answer["safety"], "mutating");
        assert_eq!(answer["effects"]["writes"], "\"manuscript\"");
        assert_eq!(answer["caller"], json!({"kind": "agent", "name": "t"}));
        assert_eq!(answer["wire_version"], 1);
        assert_eq!(hits("t-service_adv-tent-writes"), 0);
        let row = record_of("t-service_adv-tent-writes");
        assert_eq!(used(&row), json!(["tentatively"]));
        assert!(row.ok);
        assert_eq!(row.payload()["adverbs"]["used"], json!(["tentatively"]));
    }

    #[test]
    fn a_tentative_read_only_call_is_recorded_too() {
        let answer = invoke_blocking(
            &REV_READ,
            with(Adverbs {
                tentatively: true,
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(answer["safety"], "read_only");
        assert_eq!(
            used(&record_of("t-service_adv-rev-read")),
            json!(["tentatively"])
        );
    }

    #[test]
    fn tentatively_reports_the_policy_decision_without_queueing_it() {
        // with_review forces `review` whatever the process policy is.
        let answer = invoke_blocking(
            &TENT,
            with(Adverbs {
                tentatively: true,
                with_review: true,
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(answer["ok"], true, "{answer}");
        assert_ne!(answer["code"], "review-pending");
        assert_eq!(answer["would"], "review");
        assert_eq!(hits("t-service_adv-tent"), 0);

        // The pure pieces, independent of the process-global policy.
        let agent = CallerIdentity::agent("a");
        assert_eq!(
            policy::ReviewAgents::WRITES.decide(&agent, &TENT),
            policy::Decision::Review
        );
        let handle = VerbHandle::Linked(&TENT);
        for (decision, would, reason) in [
            (policy::Decision::Review, "review", None),
            (policy::Decision::Run, "run", None),
            (policy::Decision::Deny("no".into()), "deny", Some("no")),
            (
                policy::Decision::Conflict("k".into()),
                "conflict",
                Some("k"),
            ),
        ] {
            let v = tentative_static(&handle, &agent, &decision);
            assert_eq!(v["would"], would);
            assert_eq!(v["reason"].as_str(), reason);
        }
        // decide_without_lease agrees with decide for a verb that leases nothing.
        assert_eq!(
            policy::decide_without_lease(&CallerIdentity::Person, &TENT),
            policy::decide(&CallerIdentity::Person, &TENT)
        );
    }

    #[test]
    fn tentatively_on_a_verb_that_supports_it_runs_the_handler_simulated() {
        let answer = invoke_blocking(
            &TENT_SIM,
            with(Adverbs {
                tentatively: true,
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(answer["tentative"], "simulated");
        assert_eq!(answer["saw_tentative"], true);
        assert_eq!(hits("t-service_adv-sim"), 1);
        assert_eq!(
            used(&record_of("t-service_adv-sim")),
            json!(["tentatively"])
        );
    }

    #[test]
    fn with_review_queues_a_call_the_policy_would_run() {
        let answer = invoke_blocking(
            &REVIEW,
            with(Adverbs {
                with_review: true,
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(answer["ok"], false);
        assert_eq!(answer["code"], "review-pending");
        assert_eq!(hits("t-service_adv-review"), 0);
    }

    #[test]
    fn reversibly_puts_the_noted_inverse_on_the_envelope_and_the_audit_row() {
        let answer = invoke_blocking(
            &REV,
            with(Adverbs {
                reversibly: true,
                ..Default::default()
            }),
        )
        .unwrap();
        let undo = json!({"verb": "t-service_unmake", "args": {"id": "x1"}});
        assert_eq!(answer["undo"], undo);
        let row = record_of("t-service_adv-rev");
        assert_eq!(row.undo, Some(undo.clone()));
        assert_eq!(row.payload()["undo"], undo);
        assert_eq!(used(&row), json!(["reversibly"]));
    }

    #[test]
    fn reversibly_without_a_noted_inverse_is_effect_failed() {
        let answer = invoke_blocking(
            &REV_NONE,
            with(Adverbs {
                reversibly: true,
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(answer["ok"], false);
        assert_eq!(answer["code"], "effect-failed");
        assert!(answer["message"].as_str().unwrap().contains("no inverse"));
        assert_eq!(hits("t-service_adv-rev-none"), 1);
    }

    #[test]
    fn reversibly_on_a_read_only_verb_is_null_by_construction() {
        let answer = invoke_blocking(
            &REV_READ,
            with(Adverbs {
                reversibly: true,
                ..Default::default()
            }),
        )
        .unwrap();
        assert!(answer.as_object().unwrap().contains_key("undo"));
        assert!(answer["undo"].is_null());
    }

    #[test]
    fn once_replays_the_first_envelope_and_does_not_rerun() {
        let once = |key: &str| Adverbs {
            once: Some(key.into()),
            ..Default::default()
        };
        let first =
            invoke_blocking(&ONCE, Call::agent("a", json!({})).with_adverbs(once("k1"))).unwrap();
        assert!(first.get("replayed").is_none());
        let second =
            invoke_blocking(&ONCE, Call::agent("a", json!({})).with_adverbs(once("k1"))).unwrap();
        assert_eq!(second["replayed"], true);
        assert_eq!(second["echo"], first["echo"]);
        assert_eq!(hits("t-service_adv-once"), 1);
        // Another caller with the same key runs again.
        let other =
            invoke_blocking(&ONCE, Call::agent("b", json!({})).with_adverbs(once("k1"))).unwrap();
        assert!(other.get("replayed").is_none());
        assert_eq!(hits("t-service_adv-once"), 2);
        // The key's value is never on the record.
        let rows = captured().0.lock().unwrap().clone();
        for row in rows.iter().filter(|r| r.verb == "t-service_adv-once") {
            assert!(!row
                .payload()
                .get("adverbs")
                .unwrap()
                .to_string()
                .contains("k1"));
        }
        let replay = rows
            .iter()
            .filter(|r| r.verb == "t-service_adv-once")
            .any(|r| r.adverbs["once"] == true);
        assert!(replay);
    }

    #[test]
    fn a_failed_first_call_is_not_cached_by_once() {
        let once = Adverbs {
            once: Some("flaky-key".into()),
            ..Default::default()
        };
        let first = invoke_blocking(&ONCE_FLAKY, with(once.clone())).unwrap();
        assert_eq!(first["ok"], false);
        let second = invoke_blocking(&ONCE_FLAKY, with(once.clone())).unwrap();
        assert_eq!(second["ok"], true);
        assert!(second.get("replayed").is_none());
        let third = invoke_blocking(&ONCE_FLAKY, with(once)).unwrap();
        assert_eq!(third["replayed"], true);
        assert_eq!(hits("t-service_adv-once-flaky"), 2);
    }

    #[test]
    fn an_installed_idempotency_store_is_consulted_first_and_written_too() {
        struct Seeded(Mutex<HashMap<(String, String), Value>>);
        impl adverbs::IdempotencyStore for Seeded {
            fn get(&self, caller: &str, key: &str) -> Option<Value> {
                self.0
                    .lock()
                    .unwrap()
                    .get(&(caller.into(), key.into()))
                    .cloned()
            }
            fn put(&self, caller: &str, key: &str, value: &Value) {
                self.0
                    .lock()
                    .unwrap()
                    .insert((caller.into(), key.into()), value.clone());
            }
        }
        let store = Arc::new(Seeded(Mutex::new(HashMap::new())));
        store.put("human", "seeded", &json!({"ok": true, "from": "store"}));
        adverbs::install_idempotency_store(store.clone());
        let once = |key: &str| Adverbs {
            once: Some(key.into()),
            ..Default::default()
        };
        let answer = invoke_blocking(&ONCE_SEEDED, with(once("seeded"))).unwrap();
        assert_eq!(answer["from"], "store");
        assert_eq!(answer["replayed"], true);
        assert_eq!(hits("t-service_adv-once-seeded"), 0);
        invoke_blocking(&ONCE_SEEDED, with(once("fresh"))).unwrap();
        assert!(store.get("human", "fresh").is_some(), "written through");
    }

    #[test]
    fn within_times_a_slow_read_only_verb_out() {
        let answer = invoke_blocking(
            &WITHIN_SLOW,
            with(Adverbs {
                within: Some(Within { ms: 20 }),
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(answer["ok"], false);
        assert_eq!(answer["code"], "timed-out");
        assert!(answer["message"].as_str().unwrap().contains("within 20 ms"));
        assert_eq!(answer["wire_version"], 1);
    }

    #[test]
    fn within_on_a_mutating_verb_needs_a_declaration() {
        let within = Adverbs {
            within: Some(Within { ms: 20 }),
            ..Default::default()
        };
        let answer = invoke_blocking(&WITHIN_MUT, with(within.clone())).unwrap();
        assert_eq!(answer["code"], "unsupported-adverb");
        assert_eq!(hits("t-service_adv-within-mut"), 0);
        let declared = invoke_blocking(&WITHIN_OK, with(within)).unwrap();
        assert_eq!(declared["code"], "timed-out");
    }

    #[test]
    fn within_cannot_be_enforced_on_the_synchronous_chain() {
        let answer = invoke_sync_with(
            &WITHIN_SYNC,
            with(Adverbs {
                within: Some(Within { ms: 20 }),
                ..Default::default()
            }),
            |_| panic!("the handler must not run"),
        )
        .unwrap();
        assert_eq!(answer["code"], "unsupported-adverb");
        assert!(answer["message"].as_str().unwrap().contains("synchronous"));
    }

    #[test]
    fn in_background_needs_a_runner_and_hands_it_the_call_without_the_adverb() {
        struct Fake(Mutex<Vec<Call>>, Mutex<Vec<Duration>>);
        impl adverbs::BackgroundRunner for Fake {
            fn repeat(
                &self,
                verb: VerbHandle,
                call: Call,
                interval: Duration,
            ) -> Result<crate::job::JobHandle, Refusal> {
                self.0.lock().unwrap().push(call);
                self.1.lock().unwrap().push(interval);
                Ok(crate::job::JobHandle {
                    id: "routine-9".into(),
                    kind: verb.name().to_string(),
                    state: "running".into(),
                })
            }

            fn start(
                &self,
                verb: VerbHandle,
                call: Call,
            ) -> Result<crate::job::JobHandle, Refusal> {
                self.0.lock().unwrap().push(call);
                Ok(crate::job::JobHandle {
                    id: "job-7".into(),
                    kind: verb.name().to_string(),
                    state: "running".into(),
                })
            }
        }
        let background = Adverbs {
            in_background: true,
            within: Some(Within { ms: 5_000 }),
            ..Default::default()
        };
        // Only this test installs a runner, so the refusal comes first.
        assert!(!adverbs::has_background_runner());
        let refused = invoke_blocking(&BACKGROUND, with(background.clone())).unwrap();
        assert_eq!(refused["code"], "unsupported-adverb");
        assert!(refused["message"].as_str().unwrap().contains("runner"));
        assert_eq!(hits("t-service_adv-background"), 0);

        // `every`: the verb must declare it, and a runner must be installed.
        let every = Adverbs {
            every: Some(Duration::from_secs(90)),
            ..Default::default()
        };
        let undeclared = invoke_blocking(&BACKGROUND, with(every.clone())).unwrap();
        assert_eq!(undeclared["code"], "unsupported-adverb");
        assert_eq!(
            undeclared["message"],
            "t-service_adv-background does not support the adverb 'every'"
        );
        let no_runner = invoke_blocking(&EVERY, with(every.clone())).unwrap();
        assert_eq!(no_runner["code"], "unsupported-adverb");
        assert_eq!(
            no_runner["message"],
            "t-service_adv-every cannot run every: no background runner is installed in \
             this process"
        );
        assert_eq!(hits("t-service_adv-every"), 0);

        let fake = Arc::new(Fake(Mutex::new(Vec::new()), Mutex::new(Vec::new())));
        adverbs::install_background_runner(fake.clone());
        assert!(adverbs::has_background_runner());
        let mut call = with(background);
        call.args = json!({"n": 3});
        let answer = invoke_blocking(&BACKGROUND, call).unwrap();
        assert_eq!(answer["ok"], true);
        assert_eq!(answer["job"]["id"], "job-7");
        assert_eq!(answer["job"]["kind"], "t-service_adv-background");
        assert_eq!(hits("t-service_adv-background"), 0, "the runner runs it");
        let seen = fake.0.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(!seen[0].adverbs.in_background);
        assert_eq!(seen[0].adverbs.within, Some(Within { ms: 5_000 }));
        assert_eq!(seen[0].args, json!({"n": 3}));
        assert!(seen[0].parent_call.is_some());
        drop(seen);
        let row = record_of("t-service_adv-background");
        assert_eq!(used(&row), json!(["within", "in_background"]));
        assert!(row.adverbs["used"]
            .as_array()
            .unwrap()
            .contains(&json!("in_background")));

        // `every` goes to `repeat`, with the interval and without the adverb
        // (or `in_background`, which it implies).
        let mut call = with(Adverbs {
            in_background: true,
            ..every
        });
        call.args = json!({"n": 4});
        let answer = invoke_blocking(&EVERY, call).unwrap();
        assert_eq!(answer["ok"], true);
        assert_eq!(answer["job"]["id"], "routine-9");
        assert_eq!(answer["job"]["kind"], "t-service_adv-every");
        assert_eq!(hits("t-service_adv-every"), 0, "the runner runs it");
        let seen = fake.0.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen[1].adverbs.every.is_none() && !seen[1].adverbs.in_background);
        assert_eq!(seen[1].args, json!({"n": 4}));
        assert_eq!(*fake.1.lock().unwrap(), vec![Duration::from_secs(90)]);
        drop(seen);
        let row = record_of("t-service_adv-every");
        assert_eq!(used(&row), json!(["in_background", "every"]));
        assert_eq!(row.adverbs["every"], "90s");
    }
}
