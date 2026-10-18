//! The review loop (docs/plan-koine-trust.md TR-D4, TR-D5): a call the
//! policy holds for a person becomes a pending call, the person approves or
//! rejects it, and an approved call runs once as its original caller.
//!
//! A pending call is a `koine/pending-call@1.0.0` item (the verb, its
//! arguments, who asked, its state) with a `review-request@1.0.0` linked
//! `review —OperatesOn→ pending call`, so every review list that reads
//! review requests (imbib's, imprint's) shows it. It is not a task row: no
//! scheduler acquires it, so a scheduler that does not know it cannot fail
//! it. [`TaskReviewQueue`] files them for the pipeline; `review-service_*`
//! lists, approves and rejects; [`sweep`] runs approvals made elsewhere (a
//! review resolved through another app) and expires what waited too long.
//!
//! Approving runs the call at once, in this process, with a one-use
//! [`Approval`] the policy honours for that verb with those arguments (and
//! the calls it makes). The move from `pending` to `running` is a clock-guarded
//! write, so two approvers never run it twice; the run also carries the
//! `once` key `pending:<id>`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use impress_item::item::{ActorKind, Item, ItemId, Priority, Value, Visibility};
use impress_item::operation::{OperationIntent, OperationSpec, OperationType, RetentionTier};
use impress_item::query::{ItemQuery, Predicate, SortDescriptor};
use impress_item::store::{AttributedStore, GuardedWrite};
use impress_item::SchemaRef;
use impress_service_core::pipeline::policy::{
    self, Approval, ReviewQueue, VerbFacts, STATUS_INPUT_REQUIRED,
};
use impress_service_core::pipeline::{self, context as call_context, Call, CallerIdentity};
use impress_service_core::refusal::{codes, Refusal};
#[allow(unused_imports)]
use impress_service_macros::impress_method;
use impress_service_macros::{impress_service, impress_service_impl};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::store::{kernel, ReviewRequest};

/// The kind of a pending call.
pub const PENDING_CALL: &str = "koine/pending-call@1.0.0";

/// Who files pending calls and their review requests.
pub const QUEUE_AUTHOR: &str = "system:review-queue";

/// How long a pending call waits for a person before it expires; the
/// `KOINE_REVIEW_EXPIRY_DAYS` environment variable overrides it.
pub const EXPIRY_DEFAULT: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// A pending call's states.
pub mod state {
    pub const PENDING: &str = "pending";
    pub const RUNNING: &str = "running";
    pub const DONE: &str = "done";
    pub const FAILED: &str = "failed";
    pub const REJECTED: &str = "rejected";
    pub const EXPIRED: &str = "expired";
}

/// A review request's resolutions this loop writes and reads.
pub mod resolution {
    pub const APPROVED: &str = "approved";
    pub const REJECTED: &str = "rejected";
    pub const EXPIRED: &str = "expired";
}

fn schema() -> SchemaRef {
    SchemaRef::from_stored(PENDING_CALL.to_owned())
}

/// How long a pending call may wait.
pub fn expiry() -> Duration {
    std::env::var("KOINE_REVIEW_EXPIRY_DAYS")
        .ok()
        .and_then(|d| d.trim().parse::<u64>().ok())
        .map(|days| Duration::from_secs(days * 24 * 60 * 60))
        .unwrap_or(EXPIRY_DEFAULT)
}

fn text(item: &Item, name: &str) -> Option<String> {
    match item.payload.get(name) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn json_text(item: &Item, name: &str) -> Option<serde_json::Value> {
    text(item, name).map(|s| serde_json::from_str(&s).unwrap_or(serde_json::Value::String(s)))
}

fn actor_kind(caller: &CallerIdentity) -> ActorKind {
    match caller {
        CallerIdentity::Person | CallerIdentity::App(_) => ActorKind::Human,
        CallerIdentity::Agent(_) => ActorKind::Agent,
        _ => ActorKind::System,
    }
}

/// File a pending call for `verb` with `args` asked by `caller`, and its
/// review request. Answers the pending call's id.
pub fn file_pending_call(
    store: &dyn AttributedStore,
    caller: &CallerIdentity,
    verb: &dyn VerbFacts,
    args: &serde_json::Value,
) -> Result<ItemId, String> {
    let now = Utc::now();
    let safety = verb.safety().class.to_string();
    let [_, writes, reach] = verb.effects().columns();
    let session = call_context::current().map(|c| c.trace_id.clone());
    let mut payload: BTreeMap<String, Value> = BTreeMap::new();
    let mut put = |k: &str, v: String| {
        payload.insert(k.into(), Value::String(v));
    };
    put("verb", verb.name().to_owned());
    put("args", args.to_string());
    put("caller", caller.author());
    put("state", state::PENDING.into());
    put("safety", safety.clone());
    if let Some(trace) = session {
        put("trace_id", trace);
    }
    let item = Item {
        id: Uuid::new_v4(),
        schema: schema(),
        payload,
        created: now,
        modified: now,
        author: QUEUE_AUTHOR.into(),
        author_kind: ActorKind::System,
        logical_clock: 0,
        origin: None,
        canonical_id: None,
        tags: vec![],
        flag: None,
        is_read: false,
        is_starred: false,
        priority: Priority::Normal,
        visibility: Visibility::Private,
        message_type: None,
        produced_by: None,
        version: None,
        batch_id: None,
        references: vec![],
        parent: None,
    };
    let id = store.insert(item).map_err(|e| e.to_string())?;
    let mut context: BTreeMap<String, Value> = BTreeMap::new();
    context.insert("pending_call".into(), Value::String(id.to_string()));
    context.insert("verb".into(), Value::String(verb.name().to_owned()));
    context.insert("args".into(), Value::String(args.to_string()));
    context.insert("caller".into(), Value::String(caller.author()));
    context.insert("safety".into(), Value::String(safety.clone()));
    context.insert("writes".into(), Value::String(writes));
    context.insert("reach".into(), Value::String(reach));
    kernel::open_review(
        store,
        id,
        ReviewRequest {
            question: format!(
                "{caller} asks to run {} ({safety}). Approve it?",
                verb.name()
            ),
            context: Some(context),
        },
        QUEUE_AUTHOR,
    )
    .map_err(|e| e.to_string())?;
    Ok(id)
}

/// The review queue the pipeline files held calls in.
pub struct TaskReviewQueue;

impl ReviewQueue for TaskReviewQueue {
    fn enqueue(
        &self,
        caller: &CallerIdentity,
        verb: &dyn VerbFacts,
        args: &serde_json::Value,
    ) -> Result<String, Refusal> {
        // Approving is the person's: an agent's approval is not held for
        // the person to approve, it is refused.
        if verb.name().starts_with("review-service_") {
            return Err(Refusal::new(
                policy::FORBIDDEN,
                format!(
                    "only the person (or their app) may run {}, not {caller}",
                    verb.name()
                ),
            ));
        }
        let (store, fallback) = koine_store::process::acquire_or_stand_in();
        if fallback {
            return Err(Refusal::new(
                codes::STORE_UNAVAILABLE,
                format!(
                    "{} needs a person's approval, and no store is installed to keep the \
                     request in; nothing ran",
                    verb.name()
                ),
            ));
        }
        file_pending_call(store.as_ref(), caller, verb, args)
            .map(|id| id.to_string())
            .map_err(|e| Refusal::new(codes::STORE_ERROR, e))
    }
}

/// Install the review queue for this process.
pub fn install_queue() {
    policy::install_queue(Arc::new(TaskReviewQueue));
}

/// The pending call `id` and its newest review request.
pub fn pending_call(
    store: &dyn AttributedStore,
    id: ItemId,
) -> Result<Option<(Item, Option<Item>)>, String> {
    let Some(item) = store.get(id).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    if item.schema.as_str() != PENDING_CALL {
        return Ok(None);
    }
    let (mut unresolved, mut resolved) =
        kernel::reviews_for(store, id).map_err(|e| e.to_string())?;
    let review = if unresolved.is_empty() {
        (!resolved.is_empty()).then(|| resolved.remove(0))
    } else {
        Some(unresolved.remove(0))
    };
    Ok(Some((item, review)))
}

fn set(
    store: &dyn AttributedStore,
    target: ItemId,
    field: &str,
    value: String,
    by: &CallerIdentity,
) -> Result<(), String> {
    store
        .apply_operation(OperationSpec {
            target_id: target,
            op_type: OperationType::SetPayload(field.into(), Value::String(value)),
            intent: OperationIntent::Routine,
            reason: None,
            batch_id: None,
            author: by.author(),
            author_kind: actor_kind(by),
            retention: RetentionTier::Durable,
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Resolve the review of a pending call: `resolution` with who and when.
fn resolve_review(
    store: &dyn AttributedStore,
    review: &Item,
    resolution: &str,
    by: &CallerIdentity,
) -> Result<(), String> {
    if text(review, "resolution").is_some() {
        return Ok(());
    }
    set(store, review.id, "resolution", resolution.into(), by)?;
    set(store, review.id, "resolved_by", by.author(), by)?;
    set(store, review.id, "resolved_at", Utc::now().to_rfc3339(), by)
}

/// Move a pending call from `pending` to `running`, guarded by its clock:
/// `Ok(false)` when someone else moved it first.
fn claim(store: &dyn AttributedStore, item: &Item, by: &CallerIdentity) -> Result<bool, String> {
    if text(item, "state").as_deref() != Some(state::PENDING) {
        return Ok(false);
    }
    let clock = store
        .logical_clock_of(item.id)
        .map_err(|e| e.to_string())?
        .unwrap_or(item.logical_clock);
    let written = store
        .apply_operation_if_clock(
            OperationSpec {
                target_id: item.id,
                op_type: OperationType::SetPayload(
                    "state".into(),
                    Value::String(state::RUNNING.into()),
                ),
                intent: OperationIntent::Routine,
                reason: None,
                batch_id: None,
                author: by.author(),
                author_kind: actor_kind(by),
                retention: RetentionTier::Durable,
            },
            clock,
        )
        .map_err(|e| e.to_string())?;
    Ok(matches!(written, GuardedWrite::Applied { .. }))
}

/// Run an approved pending call as its original caller and record the
/// answer and the final state. The caller must already have claimed it.
pub async fn run_approved(store: &dyn AttributedStore, item: &Item) -> serde_json::Value {
    let verb = text(item, "verb").unwrap_or_default();
    let args = json_text(item, "args").unwrap_or_else(|| serde_json::json!({}));
    let caller = CallerIdentity::from_author(&text(item, "caller").unwrap_or_default());
    let answer = match impress_service_core::call::find(&verb) {
        None => serde_json::json!({
            "ok": false,
            "code": codes::NOT_FOUND,
            "message": format!("no verb named '{verb}' is linked in this process"),
        }),
        Some(handle) => {
            let approval = Approval::grant(item.id.to_string(), &verb, &args);
            let mut call = Call::new(caller, args.clone()).with_approval(approval);
            call.adverbs.once = Some(format!("pending:{}", item.id));
            // The call joins the trace it was asked in, under the call that
            // approved it — and runs on its own task, so it is the agent's
            // call, not one made inside the person's approval.
            call.trace_id = text(item, "trace_id");
            call.parent_call = call_context::current().map(|c| c.call_id.clone());
            let run = tokio::spawn(async move { pipeline::invoke_handle(handle, call).await });
            match run.await {
                Ok(Ok(value)) => value,
                Ok(Err(error)) => serde_json::json!({
                    "ok": false,
                    "code": codes::VERB_FAILED,
                    "message": error.to_string(),
                }),
                Err(join) => serde_json::json!({
                    "ok": false,
                    "code": codes::INTERNAL,
                    "message": join.to_string(),
                }),
            }
        }
    };
    let ok = answer.get("ok").and_then(serde_json::Value::as_bool) != Some(false);
    let system = CallerIdentity::system("review-queue");
    let _ = set(store, item.id, "result", answer.to_string(), &system);
    let _ = set(
        store,
        item.id,
        "state",
        if ok { state::DONE } else { state::FAILED }.into(),
        &system,
    );
    answer
}

/// What [`sweep`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SweepReport {
    pub ran: usize,
    pub rejected: usize,
    pub expired: usize,
}

/// Every pending call still `pending`: run the ones whose review was
/// approved elsewhere, close the rejected ones, and expire the ones that
/// waited longer than [`expiry`].
pub async fn sweep(store: &dyn AttributedStore) -> Result<SweepReport, String> {
    let mut report = SweepReport::default();
    let system = CallerIdentity::system("review-queue");
    let now = Utc::now();
    let max_age = chrono::Duration::from_std(expiry()).unwrap_or(chrono::Duration::days(7));
    for item in pending_calls(store, Some(state::PENDING), 500)? {
        let Some((item, review)) = pending_call(store, item.id)? else {
            continue;
        };
        match review
            .as_ref()
            .and_then(|r| text(r, "resolution"))
            .as_deref()
        {
            Some(resolution::APPROVED) => {
                if claim(store, &item, &system)? {
                    run_approved(store, &item).await;
                    report.ran += 1;
                }
            }
            Some(other) => {
                let to = if other == resolution::EXPIRED {
                    state::EXPIRED
                } else {
                    state::REJECTED
                };
                set(store, item.id, "state", to.into(), &system)?;
                report.rejected += 1;
            }
            None if now - item.created > max_age => {
                if let Some(review) = &review {
                    resolve_review(store, review, resolution::EXPIRED, &system)?;
                }
                set(store, item.id, "state", state::EXPIRED.into(), &system)?;
                report.expired += 1;
            }
            None => {}
        }
    }
    Ok(report)
}

/// Pending calls, newest first, optionally only in `state`.
pub fn pending_calls(
    store: &dyn AttributedStore,
    state: Option<&str>,
    limit: usize,
) -> Result<Vec<Item>, String> {
    let predicates = state
        .map(|s| {
            vec![Predicate::Eq(
                "payload.state".into(),
                Value::String(s.to_owned()),
            )]
        })
        .unwrap_or_default();
    store
        .query(&ItemQuery {
            schema: Some(schema()),
            predicates,
            sort: vec![SortDescriptor {
                field: "created".into(),
                ascending: false,
            }],
            limit: Some(limit),
            ..Default::default()
        })
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// review-service
// ---------------------------------------------------------------------------

/// One pending call as the review verbs show it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PendingCallRow {
    pub id: String,
    pub verb: String,
    pub args: serde_json::Value,
    /// Who asked (`agent:mcp-host:claude`, …).
    pub caller: String,
    /// pending, running, done, failed, rejected, expired.
    pub state: String,
    /// read_only, mutating, destructive, external.
    pub safety: String,
    /// What the person is asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    /// The review request's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_id: Option<String>,
    /// approved, rejected, expired — once someone decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<String>,
    /// The call's answer, once it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// The trace the asking call belonged to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    pub created: String,
}

/// A review verb that answers one pending call.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PendingCallResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<PendingCallRow>,
    /// `input_required` while the call waits for a person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `list`: pending calls, newest first.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PendingCallsResult {
    pub ok: bool,
    #[serde(default)]
    pub rows: Vec<PendingCallRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// A pending call as the verbs show it.
pub fn row(item: &Item, review: Option<&Item>) -> PendingCallRow {
    PendingCallRow {
        id: item.id.to_string(),
        verb: text(item, "verb").unwrap_or_default(),
        args: json_text(item, "args").unwrap_or(serde_json::Value::Null),
        caller: text(item, "caller").unwrap_or_default(),
        state: text(item, "state").unwrap_or_default(),
        safety: text(item, "safety").unwrap_or_default(),
        question: review.and_then(|r| text(r, "question")),
        review_id: review.map(|r| r.id.to_string()),
        resolution: review.and_then(|r| text(r, "resolution")),
        resolved_by: review.and_then(|r| text(r, "resolved_by")),
        result: json_text(item, "result"),
        trace_id: text(item, "trace_id"),
        created: item.created.to_rfc3339(),
    }
}

#[impress_service]
pub trait ReviewService: Send + Sync + 'static {
    /// Calls waiting for a person, newest first: only `pending` ones unless
    /// `state` names another (running, done, failed, rejected, expired) or
    /// `all`. A pending call is a call an agent made that the policy holds
    /// until a person approves it.
    #[impress_method(effects(reads = ["koine/pending-call@1.0.0", "review-request@1.0.0"]))]
    #[impress_example(name = "default", args = r#"{}"#, expect = r#"{"ok": true}"#)]
    async fn list(&self, state: Option<String>, limit: Option<u32>) -> PendingCallsResult;

    /// One pending call: the verb, its arguments, who asked, what the person
    /// is asked, and the answer once it ran.
    #[impress_method(effects(reads = ["koine/pending-call@1.0.0", "review-request@1.0.0"]))]
    #[impress_example(
        name = "missing",
        args = r#"{"id": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn show(&self, id: String) -> PendingCallResult;

    /// Approve a pending call: it runs at once, as the caller that asked,
    /// and the answer is kept on it. Only the person (or their app) may
    /// approve.
    #[impress_method(
        safety = mutating,
        effects(
            reads = ["koine/pending-call@1.0.0", "review-request@1.0.0"],
            writes = ["koine/pending-call@1.0.0", "review-request@1.0.0"]
        )
    )]
    #[impress_example(
        name = "not-a-person",
        args = r#"{"id": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "forbidden"}"#
    )]
    async fn approve(&self, id: String) -> PendingCallResult;

    /// Reject a pending call: it never runs. Only the person (or their app)
    /// may reject.
    #[impress_method(
        safety = mutating,
        effects(
            reads = ["koine/pending-call@1.0.0", "review-request@1.0.0"],
            writes = ["koine/pending-call@1.0.0", "review-request@1.0.0"]
        )
    )]
    #[impress_example(
        name = "not-a-person",
        args = r#"{"id": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "forbidden"}"#
    )]
    async fn reject(&self, id: String) -> PendingCallResult;
}

pub struct DefaultReviewService;

impl DefaultReviewService {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DefaultReviewService {
    fn default() -> Self {
        Self::new()
    }
}

fn refused(code: &str, message: impl Into<String>) -> PendingCallResult {
    PendingCallResult {
        ok: false,
        code: Some(code.into()),
        message: Some(message.into()),
        ..Default::default()
    }
}

fn caller() -> CallerIdentity {
    call_context::current()
        .map(|c| c.caller.clone())
        .unwrap_or(CallerIdentity::Person)
}

fn the_store(for_write: bool) -> Result<Arc<dyn AttributedStore>, Box<PendingCallResult>> {
    let (store, fallback) = koine_store::process::acquire_or_stand_in();
    if for_write && fallback {
        return Err(Box::new(refused(
            codes::STORE_UNAVAILABLE,
            "no store is installed in this process (nothing was written)",
        )));
    }
    Ok(store)
}

fn found(
    store: &dyn AttributedStore,
    id: &str,
) -> Result<(Item, Option<Item>), Box<PendingCallResult>> {
    let parsed = Uuid::parse_str(id.trim()).map_err(|_| {
        Box::new(refused(
            codes::INVALID_ARGUMENT,
            format!("'{id}' is not a pending call id"),
        ))
    })?;
    match pending_call(store, parsed) {
        Ok(Some(found)) => Ok(found),
        Ok(None) => Err(Box::new(refused(
            codes::NOT_FOUND,
            format!("no pending call {id}"),
        ))),
        Err(e) => Err(Box::new(refused(codes::STORE_ERROR, e))),
    }
}

fn answer(item: &Item, review: Option<&Item>) -> PendingCallResult {
    let row = row(item, review);
    PendingCallResult {
        ok: true,
        status: (row.state == state::PENDING).then(|| STATUS_INPUT_REQUIRED.to_owned()),
        pending: Some(row),
        ..Default::default()
    }
}

fn person_only(verb: &str) -> Option<PendingCallResult> {
    let who = caller();
    (!who.is_person()).then(|| {
        refused(
            policy::FORBIDDEN,
            format!("only the person (or their app) may {verb} a pending call, not {who}"),
        )
    })
}

#[async_trait::async_trait]
impl ReviewService for DefaultReviewService {
    async fn list(&self, state: Option<String>, limit: Option<u32>) -> PendingCallsResult {
        let store = match the_store(false) {
            Ok(store) => store,
            Err(r) => {
                return PendingCallsResult {
                    ok: false,
                    code: r.code,
                    message: r.message,
                    ..Default::default()
                }
            }
        };
        let filter = match state.as_deref().map(str::trim) {
            None | Some("") => Some(state::PENDING),
            Some("all") => None,
            Some(s) => Some(s),
        };
        let limit = limit.unwrap_or(100).clamp(1, 500) as usize;
        match pending_calls(store.as_ref(), filter, limit) {
            Ok(items) => PendingCallsResult {
                ok: true,
                rows: items
                    .iter()
                    .map(|item| {
                        let review = pending_call(store.as_ref(), item.id)
                            .ok()
                            .flatten()
                            .and_then(|(_, r)| r);
                        row(item, review.as_ref())
                    })
                    .collect(),
                ..Default::default()
            },
            Err(e) => PendingCallsResult {
                ok: false,
                code: Some(codes::STORE_ERROR.into()),
                message: Some(e),
                ..Default::default()
            },
        }
    }

    async fn show(&self, id: String) -> PendingCallResult {
        let store = match the_store(false) {
            Ok(store) => store,
            Err(r) => return *r,
        };
        match found(store.as_ref(), &id) {
            Ok((item, review)) => answer(&item, review.as_ref()),
            Err(r) => *r,
        }
    }

    async fn approve(&self, id: String) -> PendingCallResult {
        if let Some(refusal) = person_only("approve") {
            return refusal;
        }
        let store = match the_store(true) {
            Ok(store) => store,
            Err(r) => return *r,
        };
        let (item, review) = match found(store.as_ref(), &id) {
            Ok(found) => found,
            Err(r) => return *r,
        };
        let who = caller();
        if text(&item, "state").as_deref() != Some(state::PENDING) {
            return refused(
                codes::CONFLICT,
                format!(
                    "pending call {id} is {}, not pending",
                    text(&item, "state").unwrap_or_default()
                ),
            );
        }
        if let Some(review) = &review {
            if let Err(e) = resolve_review(store.as_ref(), review, resolution::APPROVED, &who) {
                return refused(codes::STORE_ERROR, e);
            }
        }
        match claim(store.as_ref(), &item, &who) {
            Ok(true) => {}
            Ok(false) => return refused(codes::CONFLICT, format!("pending call {id} moved first")),
            Err(e) => return refused(codes::STORE_ERROR, e),
        }
        run_approved(store.as_ref(), &item).await;
        match found(store.as_ref(), &id) {
            Ok((item, review)) => answer(&item, review.as_ref()),
            Err(r) => *r,
        }
    }

    async fn reject(&self, id: String) -> PendingCallResult {
        if let Some(refusal) = person_only("reject") {
            return refusal;
        }
        let store = match the_store(true) {
            Ok(store) => store,
            Err(r) => return *r,
        };
        let (item, review) = match found(store.as_ref(), &id) {
            Ok(found) => found,
            Err(r) => return *r,
        };
        if text(&item, "state").as_deref() != Some(state::PENDING) {
            return refused(
                codes::CONFLICT,
                format!(
                    "pending call {id} is {}, not pending",
                    text(&item, "state").unwrap_or_default()
                ),
            );
        }
        let who = caller();
        if let Some(review) = &review {
            if let Err(e) = resolve_review(store.as_ref(), review, resolution::REJECTED, &who) {
                return refused(codes::STORE_ERROR, e);
            }
        }
        if let Err(e) = set(
            store.as_ref(),
            item.id,
            "state",
            state::REJECTED.into(),
            &who,
        ) {
            return refused(codes::STORE_ERROR, e);
        }
        match found(store.as_ref(), &id) {
            Ok((item, review)) => answer(&item, review.as_ref()),
            Err(r) => *r,
        }
    }
}

impress_service_impl! {
    service = ReviewService,
    safety = read_only,
    since = "0.3.0",
    effects = {
        reads: ["koine/pending-call@1.0.0", "review-request@1.0.0"],
        writes: [],
        reach: [],
    },
    impl = DefaultReviewService,
    instance = DefaultReviewService::new,
    methods = [
        /// Calls waiting for a person.
        list(
            /// pending (the default), running, done, failed, rejected, expired, or all.
            state: Option<String>,
            /// At most this many (default 100, at most 500).
            limit: Option<u32>
        ) -> PendingCallsResult,
        /// One pending call.
        show(
            /// The pending call's id.
            id: String
        ) -> PendingCallResult,
        /// Approve a pending call; it runs at once.
        approve(
            /// The pending call's id.
            id: String
        ) -> PendingCallResult,
        /// Reject a pending call; it never runs.
        reject(
            /// The pending call's id.
            id: String
        ) -> PendingCallResult,
    ],
}
