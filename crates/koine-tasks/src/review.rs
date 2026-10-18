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
    self, Approval, ConsentRule, PendingRequest, ReviewQueue, RuleSource, VerbFacts,
    STATUS_INPUT_REQUIRED,
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
    file_pending_request(store, caller, verb, args, None)
}

/// The records a call acts on, as `[{id, argument, kind, title}]`: the ids
/// in its `targets` argument, or in the argument its effects name as the
/// target (`Kind::Target`, `Kind::Children`).
pub fn targets_of(
    store: &dyn AttributedStore,
    verb: &dyn VerbFacts,
    args: &serde_json::Value,
) -> Vec<serde_json::Value> {
    use impress_service_core::Kind;
    let mut arg_names: Vec<&str> = Vec::new();
    if let Some(arg) = verb.consent().targets {
        arg_names.push(arg);
    }
    for kind in verb.effects().writes {
        if let Kind::Target(arg) | Kind::Children(arg) = kind {
            if !arg_names.contains(arg) {
                arg_names.push(arg);
            }
        }
    }
    let mut out = Vec::new();
    for arg in arg_names {
        let ids: Vec<String> = match args.get(arg) {
            Some(serde_json::Value::String(id)) => vec![id.clone()],
            Some(serde_json::Value::Array(items)) => items
                .iter()
                .filter_map(|i| i.as_str().map(str::to_owned))
                .collect(),
            _ => continue,
        };
        for id in ids {
            let found = id
                .parse::<ItemId>()
                .ok()
                .and_then(|parsed| store.get(parsed).ok().flatten());
            let title = found.as_ref().and_then(|item| {
                ["title", "name", "label"]
                    .iter()
                    .find_map(|field| text(item, field))
            });
            out.push(serde_json::json!({
                "id": id,
                "argument": arg,
                "kind": found.as_ref().map(|i| i.schema.as_str().to_owned()),
                "title": title,
            }));
            if out.len() >= 50 {
                return out;
            }
        }
    }
    out
}

/// [`file_pending_call`] with why the call was held (TR-D6): the reason,
/// the verb's impact, the records it acts on and the alternatives a person
/// may choose instead go on the pending call and its review request.
pub fn file_pending_request(
    store: &dyn AttributedStore,
    caller: &CallerIdentity,
    verb: &dyn VerbFacts,
    args: &serde_json::Value,
    reason: Option<&str>,
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
    let consent = verb.consent();
    let impact = consent.impact_for(verb.safety().class).to_string();
    put("impact", impact.clone());
    if let Some(reason) = reason {
        put("reason", reason.to_owned());
    }
    let targets = targets_of(store, verb, args);
    if !targets.is_empty() {
        put(
            "targets",
            serde_json::Value::Array(targets.clone()).to_string(),
        );
    }
    if !consent.alternatives.is_empty() {
        put(
            "alternatives",
            serde_json::json!(consent.alternatives).to_string(),
        );
    }
    put("fingerprint", verb.fingerprint());
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
    context.insert("impact".into(), Value::String(impact.clone()));
    if let Some(reason) = reason {
        context.insert("reason".into(), Value::String(reason.to_owned()));
    }
    if !targets.is_empty() {
        context.insert(
            "targets".into(),
            Value::String(serde_json::Value::Array(targets.clone()).to_string()),
        );
    }
    kernel::open_review(
        store,
        id,
        ReviewRequest {
            question: {
                let on = match targets.len() {
                    0 => String::new(),
                    1 => format!(
                        " on {}",
                        targets[0]["title"]
                            .as_str()
                            .or(targets[0]["id"].as_str())
                            .unwrap_or("one record")
                    ),
                    n => format!(" on {n} records"),
                };
                format!(
                    "{caller} asks to run {}{on} ({safety}, {impact} impact){}. Approve it?",
                    verb.name(),
                    reason.map(|r| format!(": {r}")).unwrap_or_default()
                )
            },
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

    fn enqueue_request(&self, request: &PendingRequest<'_>) -> Result<String, Refusal> {
        if request.verb.name().starts_with("review-service_") {
            return self.enqueue(request.caller, request.verb, request.args);
        }
        let (store, fallback) = koine_store::process::acquire_or_stand_in();
        if fallback {
            return self.enqueue(request.caller, request.verb, request.args);
        }
        file_pending_request(
            store.as_ref(),
            request.caller,
            request.verb,
            request.args,
            request.reason,
        )
        .map(|id| id.to_string())
        .map_err(|e| Refusal::new(codes::STORE_ERROR, e))
    }
}

// ---------------------------------------------------------------------------
// Standing rules (TR-D9)
// ---------------------------------------------------------------------------

/// The kind of a standing consent rule.
pub const CONSENT_RULE: &str = "koine/consent-rule@1.0.0";

/// How long an "always allow" lasts.
pub const ALLOW_RULE_DAYS: i64 = 30;

fn rule_of(item: &Item) -> Option<ConsentRule> {
    if item.schema.as_str() != CONSENT_RULE || text(item, "revoked_at").is_some() {
        return None;
    }
    Some(ConsentRule {
        id: item.id.to_string(),
        verb: text(item, "verb")?,
        caller: text(item, "caller")?,
        allow: text(item, "decision").as_deref() == Some("allow"),
        fingerprint: text(item, "fingerprint").unwrap_or_default(),
        expires_ms: match item.payload.get("expires_ms") {
            Some(Value::Int(at)) => Some(*at),
            _ => None,
        },
    })
}

/// Every consent-rule item in `store`, revoked ones included, newest first.
pub fn rule_items(store: &dyn AttributedStore) -> Result<Vec<Item>, String> {
    store
        .query(&ItemQuery {
            schema: Some(SchemaRef::from_stored(CONSENT_RULE.to_owned())),
            sort: vec![SortDescriptor {
                field: "created".into(),
                ascending: false,
            }],
            ..Default::default()
        })
        .map_err(|e| e.to_string())
}

/// The process store's standing rules, read again only when the store
/// changed.
struct StoreRules {
    cache: std::sync::Mutex<Option<(u64, Vec<ConsentRule>)>>,
}

impl RuleSource for StoreRules {
    fn rules(&self) -> Vec<ConsentRule> {
        let Some(store) = koine_store::process::current() else {
            return Vec::new();
        };
        let token = store.change_token().unwrap_or(u64::MAX);
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((seen, rules)) = cache.as_ref() {
            if *seen == token && token != u64::MAX {
                return rules.clone();
            }
        }
        let rules: Vec<ConsentRule> = rule_items(store.as_ref())
            .unwrap_or_default()
            .iter()
            .filter_map(rule_of)
            .collect();
        *cache = Some((token, rules.clone()));
        rules
    }
}

/// Write a standing rule: `verb` from `caller`, allowed or rejected, made by
/// `by` (from pending call `from`).
pub fn remember(
    store: &dyn AttributedStore,
    verb: &str,
    caller: &str,
    allow: bool,
    by: &CallerIdentity,
    from: Option<ItemId>,
) -> Result<ItemId, String> {
    let now = Utc::now();
    let fingerprint = impress_service_core::call::find(verb)
        .map(|handle| handle.fingerprint())
        .unwrap_or_default();
    let mut payload: BTreeMap<String, Value> = BTreeMap::new();
    payload.insert("verb".into(), Value::String(verb.to_owned()));
    payload.insert("caller".into(), Value::String(caller.to_owned()));
    payload.insert(
        "decision".into(),
        Value::String(if allow { "allow" } else { "deny" }.into()),
    );
    payload.insert("fingerprint".into(), Value::String(fingerprint));
    payload.insert("made_by".into(), Value::String(by.author()));
    if let Some(from) = from {
        payload.insert("from_pending".into(), Value::String(from.to_string()));
    }
    if allow {
        let at = (now + chrono::Duration::days(ALLOW_RULE_DAYS)).timestamp_millis();
        payload.insert("expires_ms".into(), Value::Int(at));
    }
    store
        .insert(Item {
            id: Uuid::new_v4(),
            schema: SchemaRef::from_stored(CONSENT_RULE.to_owned()),
            payload,
            created: now,
            modified: now,
            author: by.author(),
            author_kind: actor_kind(by),
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
        })
        .map_err(|e| e.to_string())
}

/// Install the review queue for this process, and the store's standing
/// rules as the consent policy's.
pub fn install_queue() {
    policy::install_queue(Arc::new(TaskReviewQueue));
    policy::install_rule_source(Arc::new(StoreRules {
        cache: std::sync::Mutex::new(None),
    }));
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
    // What the person approved: the call as asked, or a subset of its
    // targets, or an alternative verb (TR-D7).
    let verb = text(item, "approved_verb")
        .or_else(|| text(item, "verb"))
        .unwrap_or_default();
    let args = json_text(item, "approved_args")
        .or_else(|| json_text(item, "args"))
        .unwrap_or_else(|| serde_json::json!({}));
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
    /// low, medium, high.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact: Option<String>,
    /// Why it was held.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The records it acts on: `[{id, argument, kind, title}]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<serde_json::Value>,
    /// Verbs the person may run on the same arguments instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<String>,
    /// What was approved, when it was not the call as asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_verb: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_args: Option<serde_json::Value>,
    pub created: String,
}

/// One standing consent rule as the review verbs show it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConsentRuleRow {
    pub id: String,
    pub verb: String,
    pub caller: String,
    /// allow or deny.
    pub decision: String,
    /// The verb changed since the rule was made, so the rule no longer
    /// applies; the person is asked again.
    pub stale: bool,
    pub revoked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub made_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_pending: Option<String>,
    pub created: String,
}

/// `rules`: standing rules, newest first.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ConsentRulesResult {
    pub ok: bool,
    #[serde(default)]
    pub rows: Vec<ConsentRuleRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// A rule as the verbs show it.
pub fn rule_row(item: &Item) -> ConsentRuleRow {
    let verb = text(item, "verb").unwrap_or_default();
    let fingerprint = text(item, "fingerprint").unwrap_or_default();
    let current = impress_service_core::call::find(&verb).map(|h| h.fingerprint());
    ConsentRuleRow {
        id: item.id.to_string(),
        stale: !verb.ends_with('*')
            && !fingerprint.is_empty()
            && current.is_some_and(|now| now != fingerprint),
        verb,
        caller: text(item, "caller").unwrap_or_default(),
        decision: text(item, "decision").unwrap_or_default(),
        revoked: text(item, "revoked_at").is_some(),
        expires_ms: match item.payload.get("expires_ms") {
            Some(Value::Int(at)) => Some(*at),
            _ => None,
        },
        made_by: text(item, "made_by"),
        from_pending: text(item, "from_pending"),
        created: item.created.to_rfc3339(),
    }
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
        impact: text(item, "impact"),
        reason: text(item, "reason"),
        targets: match json_text(item, "targets") {
            Some(serde_json::Value::Array(targets)) => targets,
            _ => Vec::new(),
        },
        alternatives: match json_text(item, "alternatives") {
            Some(serde_json::Value::Array(names)) => names
                .iter()
                .filter_map(|n| n.as_str().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        },
        approved_verb: text(item, "approved_verb"),
        approved_args: json_text(item, "approved_args"),
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
    /// and the answer is kept on it. `only` approves a subset of the records
    /// it acts on (its `targets`); `instead` runs one of its `alternatives`
    /// on the same arguments; `remember` also allows this verb from this
    /// caller from now on (for 30 days, until the verb changes or the rule is
    /// revoked). Only the person (or their app) may approve.
    #[impress_method(
        safety = mutating,
        effects(
            reads = ["koine/pending-call@1.0.0", "review-request@1.0.0"],
            writes = [
                "koine/pending-call@1.0.0",
                "review-request@1.0.0",
                "koine/consent-rule@1.0.0"
            ]
        )
    )]
    #[impress_example(
        name = "not-a-person",
        args = r#"{"id": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "forbidden"}"#
    )]
    async fn approve(
        &self,
        id: String,
        only: Option<Vec<String>>,
        instead: Option<String>,
        remember: Option<bool>,
    ) -> PendingCallResult;

    /// Reject a pending call: it never runs. `remember` also rejects this
    /// verb from this caller from now on, until the rule is revoked. Only the
    /// person (or their app) may reject.
    #[impress_method(
        safety = mutating,
        effects(
            reads = ["koine/pending-call@1.0.0", "review-request@1.0.0"],
            writes = [
                "koine/pending-call@1.0.0",
                "review-request@1.0.0",
                "koine/consent-rule@1.0.0"
            ]
        )
    )]
    #[impress_example(
        name = "not-a-person",
        args = r#"{"id": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "forbidden"}"#
    )]
    async fn reject(&self, id: String, remember: Option<bool>) -> PendingCallResult;

    /// Standing rules a person made with `remember` ("always allow this verb
    /// from this agent", "never"): newest first, with whether each is stale
    /// (its verb changed since, so it no longer applies) or revoked.
    #[impress_method(effects(reads = ["koine/consent-rule@1.0.0"]))]
    #[impress_example(name = "default", args = r#"{}"#, expect = r#"{"ok": true}"#)]
    async fn rules(&self) -> ConsentRulesResult;

    /// Revoke a standing rule: the person is asked again. Only the person
    /// (or their app) may revoke.
    #[impress_method(
        safety = mutating,
        effects(reads = ["koine/consent-rule@1.0.0"], writes = ["koine/consent-rule@1.0.0"])
    )]
    #[impress_example(
        name = "not-a-person",
        args = r#"{"id": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "forbidden"}"#
    )]
    async fn revoke_rule(&self, id: String) -> ConsentRulesResult;
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

/// A standing rule from a pending call: its verb, from its caller.
fn remember_rule(
    store: &dyn AttributedStore,
    item: &Item,
    allow: bool,
    by: &CallerIdentity,
) -> Result<ItemId, String> {
    remember(
        store,
        &text(item, "verb").unwrap_or_default(),
        &text(item, "caller").unwrap_or_default(),
        allow,
        by,
        Some(item.id),
    )
}

fn rules_refused(code: &str, message: impl Into<String>) -> ConsentRulesResult {
    ConsentRulesResult {
        ok: false,
        code: Some(code.into()),
        message: Some(message.into()),
        ..Default::default()
    }
}

#[async_trait::async_trait]
impl ReviewService for DefaultReviewService {
    async fn rules(&self) -> ConsentRulesResult {
        let (store, _) = koine_store::process::acquire_or_stand_in();
        match rule_items(store.as_ref()) {
            Ok(items) => ConsentRulesResult {
                ok: true,
                rows: items.iter().map(rule_row).collect(),
                ..Default::default()
            },
            Err(e) => rules_refused(codes::STORE_ERROR, e),
        }
    }

    async fn revoke_rule(&self, id: String) -> ConsentRulesResult {
        let who = caller();
        if !who.is_person() {
            return rules_refused(
                policy::FORBIDDEN,
                format!("only the person (or their app) may revoke a rule, not {who}"),
            );
        }
        let (store, fallback) = koine_store::process::acquire_or_stand_in();
        if fallback {
            return rules_refused(codes::STORE_UNAVAILABLE, "no store is installed");
        }
        let Ok(parsed) = Uuid::parse_str(id.trim()) else {
            return rules_refused(codes::INVALID_ARGUMENT, format!("'{id}' is not a rule id"));
        };
        match store.get(parsed) {
            Ok(Some(item)) if item.schema.as_str() == CONSENT_RULE => {
                if let Err(e) = set(
                    store.as_ref(),
                    parsed,
                    "revoked_at",
                    Utc::now().to_rfc3339(),
                    &who,
                ) {
                    return rules_refused(codes::STORE_ERROR, e);
                }
                self.rules().await
            }
            Ok(_) => rules_refused(codes::NOT_FOUND, format!("no rule {id}")),
            Err(e) => rules_refused(codes::STORE_ERROR, e.to_string()),
        }
    }

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

    async fn approve(
        &self,
        id: String,
        only: Option<Vec<String>>,
        instead: Option<String>,
        remember: Option<bool>,
    ) -> PendingCallResult {
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
        // What exactly is approved (TR-D7).
        let row_now = row(&item, review.as_ref());
        if let Some(alternative) = instead.as_deref() {
            if !row_now.alternatives.iter().any(|a| a == alternative) {
                return refused(
                    codes::INVALID_ARGUMENT,
                    format!(
                        "'{alternative}' is not an alternative of {} (it offers: {})",
                        row_now.verb,
                        row_now.alternatives.join(", ")
                    ),
                );
            }
            if let Err(e) = set(
                store.as_ref(),
                item.id,
                "approved_verb",
                alternative.into(),
                &who,
            ) {
                return refused(codes::STORE_ERROR, e);
            }
        }
        if let Some(only) = only {
            let mut args = row_now.args.clone();
            let mut narrowed = false;
            for target in &row_now.targets {
                let Some(arg) = target["argument"].as_str() else {
                    continue;
                };
                if let Some(serde_json::Value::Array(ids)) = args.get_mut(arg) {
                    let before = ids.len();
                    ids.retain(|v| v.as_str().is_some_and(|id| only.iter().any(|o| o == id)));
                    narrowed |= ids.len() != before;
                }
            }
            let known: Vec<&str> = row_now
                .targets
                .iter()
                .filter_map(|t| t["id"].as_str())
                .collect();
            if let Some(stray) = only.iter().find(|id| !known.contains(&id.as_str())) {
                return refused(
                    codes::INVALID_ARGUMENT,
                    format!("'{stray}' is not one of the records this call acts on"),
                );
            }
            if narrowed {
                if let Err(e) = set(
                    store.as_ref(),
                    item.id,
                    "approved_args",
                    args.to_string(),
                    &who,
                ) {
                    return refused(codes::STORE_ERROR, e);
                }
            }
        }
        if remember == Some(true) {
            if let Err(e) = remember_rule(store.as_ref(), &item, true, &who) {
                return refused(codes::STORE_ERROR, e);
            }
        }
        if let Some(review) = &review {
            if let Err(e) = resolve_review(store.as_ref(), review, resolution::APPROVED, &who) {
                return refused(codes::STORE_ERROR, e);
            }
        }
        let Ok(Some((item, _))) = pending_call(store.as_ref(), item.id) else {
            return refused(codes::STORE_ERROR, format!("pending call {id} vanished"));
        };
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

    async fn reject(&self, id: String, remember: Option<bool>) -> PendingCallResult {
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
        if remember == Some(true) {
            if let Err(e) = remember_rule(store.as_ref(), &item, false, &who) {
                return refused(codes::STORE_ERROR, e);
            }
        }
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
            id: String,
            /// Approve only these of the records it acts on.
            only: Option<Vec<String>>,
            /// Run this alternative verb instead.
            instead: Option<String>,
            /// Also allow this verb from this caller from now on.
            remember: Option<bool>
        ) -> PendingCallResult,
        /// Reject a pending call; it never runs.
        reject(
            /// The pending call's id.
            id: String,
            /// Also reject this verb from this caller from now on.
            remember: Option<bool>
        ) -> PendingCallResult,
        /// Standing consent rules.
        rules() -> ConsentRulesResult,
        /// Revoke a standing rule.
        revoke_rule(
            /// The rule's id.
            id: String
        ) -> ConsentRulesResult,
    ],
}
