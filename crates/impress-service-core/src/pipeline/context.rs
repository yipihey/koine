//! The task-local call context (plan-self-reflective-layer H-P2-1, H-P2-2,
//! H-P2-3).
//!
//! The pipeline sets one [`CallContext`] around the handler step of every
//! call. Anything that runs *inside* the call — the verb, the store it
//! writes, a nested verb a surface's `call` effect runs — reads it with
//! [`current`]:
//!
//! * the store stamps `batch_id = call_id` on every `OperationSpec` written
//!   with no batch of its own, so "why did this item change" is a join;
//! * a nested call inherits the caller and the trace and records the outer
//!   call as its `parent_call`, so one trace id joins a surface click, its
//!   verb, and the verbs the surface ran;
//! * `store_override` is the per-call store a Tier A scenario runs against
//!   ([`super::invoke_on`]); the process-wide store stays the default.
//!
//! A task-local, not a thread-local: a handler is an `async` future that
//! may move between the runtime's worker threads. A future spawned with
//! `tokio::spawn` or `spawn_blocking` does not inherit it, which is the
//! documented limit — a verb that spawns its writes onto another task is
//! recorded as having written nothing, and the audit row says so by its
//! `ops` count.

use std::any::Any;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use super::identity::CallerIdentity;
use crate::adverb::Adverbs;

/// Created and deleted item ids observed while one handler runs. Each call
/// gets its own instance, including a call nested inside another handler.
#[derive(Debug, Clone, Default)]
pub struct MutationIds(Arc<Mutex<MutationIdSets>>);

#[derive(Debug, Default)]
struct MutationIdSets {
    inserted: BTreeSet<String>,
    deleted: BTreeSet<String>,
}

impl MutationIds {
    fn note(&self, id: String, created: bool, deleted: bool) {
        if !created && !deleted {
            return;
        }
        let mut ids = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if created {
            ids.inserted.insert(id.clone());
        }
        if deleted {
            ids.deleted.insert(id);
        }
    }

    /// A stable, deduplicated snapshot for the audit record.
    pub fn snapshot(&self) -> (Vec<String>, Vec<String>) {
        let ids = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (
            ids.inserted.iter().cloned().collect(),
            ids.deleted.iter().cloned().collect(),
        )
    }
}

/// The inverse a `reversibly` call's handler recorded, shared by every clone
/// of its context.
#[derive(Debug, Clone, Default)]
pub struct InverseSlot(Arc<Mutex<Option<(String, serde_json::Value)>>>);

impl InverseSlot {
    fn set(&self, verb: String, args: serde_json::Value) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = Some((verb, args));
    }

    /// The recorded inverse, if the handler noted one.
    pub fn take(&self) -> Option<(String, serde_json::Value)> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).take()
    }
}

/// Steps kept per level of a step tree; the rest are counted, not kept
/// (plan-koine-notebook N-D2).
pub const MAX_STEPS_PER_LEVEL: usize = 64;
/// Levels kept below a record; deeper steps are counted, not kept.
pub const MAX_STEP_DEPTH: usize = 8;
/// Record ids kept in a call's read set; the rest are counted (N-D3).
pub const MAX_READ_IDS: usize = 256;

/// One call that ran inside another, with the calls that ran inside it: the
/// step tree a `core/verb-call@1.0.0` record carries (plan-koine-notebook
/// N-D2). Read-only calls are steps too, so the record of a surface click
/// shows every verb its actions ran, audited on its own or not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepRecord {
    pub verb: String,
    pub call_id: String,
    pub duration_us: u64,
    /// The duration less the children's, floored at zero: concurrent
    /// children may overlap.
    pub self_us: u64,
    pub ok: bool,
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<StepRecord>,
    /// Steps under this one that were cut by [`MAX_STEPS_PER_LEVEL`] or
    /// [`MAX_STEP_DEPTH`].
    #[serde(default, skip_serializing_if = "is_zero")]
    pub steps_truncated: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl StepRecord {
    /// A finished call as a step: `self_us` from its children's durations.
    pub fn new(
        verb: impl Into<String>,
        call_id: impl Into<String>,
        duration_us: u64,
        ok: bool,
        code: Option<String>,
        steps: Vec<StepRecord>,
        steps_truncated: u32,
    ) -> Self {
        let children: u64 = steps.iter().map(|s| s.duration_us).sum();
        let mut step = Self {
            verb: verb.into(),
            call_id: call_id.into(),
            duration_us,
            self_us: duration_us.saturating_sub(children),
            ok,
            code,
            steps,
            steps_truncated,
        };
        step.prune(MAX_STEP_DEPTH);
        step
    }

    /// Keep `depth` levels below this step; count what is cut.
    pub fn prune(&mut self, depth: usize) {
        if depth == 0 {
            self.steps_truncated = self
                .steps_truncated
                .saturating_add(count_steps(&self.steps));
            self.steps.clear();
            return;
        }
        for step in &mut self.steps {
            step.prune(depth - 1);
        }
    }

    /// This step and every step under it, depth first, with its depth
    /// (0 for this one).
    pub fn flatten(&self) -> Vec<(usize, &StepRecord)> {
        let mut out = Vec::new();
        fn walk<'a>(step: &'a StepRecord, depth: usize, out: &mut Vec<(usize, &'a StepRecord)>) {
            out.push((depth, step));
            for child in &step.steps {
                walk(child, depth + 1, out);
            }
        }
        walk(self, 0, &mut out);
        out
    }
}

/// How many steps a list holds, nested ones included.
pub fn count_steps(steps: &[StepRecord]) -> u32 {
    steps
        .iter()
        .map(|s| {
            1u32.saturating_add(count_steps(&s.steps))
                .saturating_add(s.steps_truncated)
        })
        .fold(0u32, u32::saturating_add)
}

/// What a call learns about itself while it runs, beyond its mutations: the
/// calls that ran inside it and the records it read (plan-koine-notebook
/// N-D2, N-D3). Shared by every clone of its context.
#[derive(Debug, Clone, Default)]
pub struct CallNotes(Arc<Mutex<NotesInner>>);

#[derive(Debug, Default)]
struct NotesInner {
    steps: Vec<StepRecord>,
    steps_truncated: u32,
    reads: BTreeSet<String>,
    reads_truncated: u64,
    updates: BTreeSet<String>,
    updates_truncated: u64,
}

impl CallNotes {
    fn lock(&self) -> std::sync::MutexGuard<'_, NotesInner> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Add a finished nested call; past [`MAX_STEPS_PER_LEVEL`] it is
    /// counted instead.
    pub fn push_step(&self, step: StepRecord) {
        let mut inner = self.lock();
        if inner.steps.len() < MAX_STEPS_PER_LEVEL {
            inner.steps.push(step);
        } else {
            let cut = 1u32
                .saturating_add(count_steps(&step.steps))
                .saturating_add(step.steps_truncated);
            inner.steps_truncated = inner.steps_truncated.saturating_add(cut);
        }
    }

    /// The steps so far, in the order they finished, and how many were cut.
    pub fn steps(&self) -> (Vec<StepRecord>, u32) {
        let inner = self.lock();
        (inner.steps.clone(), inner.steps_truncated)
    }

    fn note_read(&self, id: String) {
        let mut inner = self.lock();
        if inner.reads.contains(&id) {
            return;
        }
        if inner.reads.len() < MAX_READ_IDS {
            inner.reads.insert(id);
        } else {
            inner.reads_truncated += 1;
        }
    }

    fn note_update(&self, id: String) {
        let mut inner = self.lock();
        if inner.updates.contains(&id) {
            return;
        }
        if inner.updates.len() < MAX_READ_IDS {
            inner.updates.insert(id);
        } else {
            inner.updates_truncated += 1;
        }
    }

    /// The ids changed in place (not created or deleted), sorted, and how
    /// many more were changed but not kept.
    pub fn updates(&self) -> (Vec<String>, u64) {
        let inner = self.lock();
        (
            inner.updates.iter().cloned().collect(),
            inner.updates_truncated,
        )
    }

    /// The ids read, sorted, and how many more were read but not kept.
    pub fn reads(&self) -> (Vec<String>, u64) {
        let inner = self.lock();
        (inner.reads.iter().cloned().collect(), inner.reads_truncated)
    }
}

/// What every call runs under.
#[derive(Debug, Clone)]
pub struct CallContext {
    /// This call's id: the `core/verb-call@1.0.0` row's id and the
    /// `batch_id` of every operation the verb wrote.
    pub call_id: String,
    /// The trace this call belongs to; generated by the pipeline when the
    /// entry path carried none, inherited by every nested call.
    pub trace_id: String,
    /// The call this one ran inside, when it did.
    pub parent_call: Option<String>,
    /// Who is calling, as the transport established it.
    pub caller: CallerIdentity,
    /// The verb being run.
    pub verb: std::borrow::Cow<'static, str>,
    /// The store this call runs against instead of the process-wide one.
    /// Type-erased because this crate is on the kit's pure tier and cannot
    /// name `SqliteItemStore`; [`store_override`] downcasts it.
    pub store_override: Option<Arc<dyn Any + Send + Sync>>,
    /// Item creations and deletions observed during this call only.
    pub mutation_ids: MutationIds,
    /// The adverbs this call carries, so a verb that declares
    /// `supports(tentatively)` or `supports(reversibly)` can read them.
    pub adverbs: Adverbs,
    /// The inverse call a `reversibly` handler recorded ([`note_inverse`]).
    pub inverse: InverseSlot,
    /// The steps and reads of this call (plan-koine-notebook N-D2, N-D3).
    pub notes: CallNotes,
}

/// Trusted identity and parent metadata crossing a native callback boundary.
/// The domain verb arguments remain a separate JSON value; callers must never
/// be able to select this identity by putting fields in that object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportContext {
    pub caller: CallerIdentity,
    pub trace_id: String,
    pub parent_call: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransportContextWire {
    kind: String,
    name: Option<String>,
    trace_id: String,
    parent_call: String,
}

impl TransportContext {
    /// Describe the current call as the trusted parent of a nested transport
    /// call. This is the same wire shape consumed by `dispatch`'s caller
    /// parser, with `person` as the transport spelling for the pipeline's
    /// `human` identity kind.
    pub fn from_parent(context: &CallContext) -> Self {
        Self {
            caller: context.caller.clone(),
            trace_id: context.trace_id.clone(),
            parent_call: context.call_id.clone(),
        }
    }

    /// Strictly decode caller metadata received outside the verb's argument
    /// object. Reject unknown/duplicate fields and malformed identities before
    /// dispatch so a callback cannot quietly fall back to a different actor.
    pub fn from_json(raw: &str) -> Result<Self, String> {
        let wire: TransportContextWire = serde_json::from_str(raw)
            .map_err(|error| format!("invalid caller context: {error}"))?;
        let caller = match (wire.kind.as_str(), wire.name) {
            ("person", None) => CallerIdentity::Person,
            ("agent", Some(name)) if !name.trim().is_empty() => CallerIdentity::Agent(name),
            ("app", Some(name)) if !name.trim().is_empty() => CallerIdentity::App(name),
            ("provider", Some(name)) if !name.trim().is_empty() => CallerIdentity::Provider(name),
            ("system", Some(name)) if !name.trim().is_empty() => CallerIdentity::System(name),
            _ => return Err("invalid caller context identity".into()),
        };
        if wire.trace_id.trim().is_empty() || wire.parent_call.trim().is_empty() {
            return Err("caller context requires non-empty trace_id and parent_call".into());
        }
        Ok(Self {
            caller,
            trace_id: wire.trace_id,
            parent_call: wire.parent_call,
        })
    }

    /// Encode this context using the established dispatch caller JSON shape.
    pub fn to_json(&self) -> String {
        let kind = match &self.caller {
            CallerIdentity::Person => "person",
            CallerIdentity::Agent(_) => "agent",
            CallerIdentity::App(_) => "app",
            CallerIdentity::Provider(_) => "provider",
            CallerIdentity::System(_) => "system",
        };
        let mut value = serde_json::Map::new();
        value.insert("kind".into(), serde_json::json!(kind));
        if let Some(name) = self.caller.name() {
            value.insert("name".into(), serde_json::json!(name));
        }
        value.insert("trace_id".into(), serde_json::json!(self.trace_id));
        value.insert("parent_call".into(), serde_json::json!(self.parent_call));
        serde_json::Value::Object(value).to_string()
    }

    /// Construct an entry call with the exact identity and lineage supplied
    /// by the trusted transport context.
    pub fn into_call(self, args: serde_json::Value) -> super::Call {
        let mut call = super::Call::new(self.caller, args).with_trace(self.trace_id);
        call.parent_call = Some(self.parent_call);
        call
    }
}

tokio::task_local! {
    static CURRENT: Arc<CallContext>;
    static REPORTED_REFUSAL: RefCell<Option<crate::refusal::Refusal>>;
}

/// Preserve a native backend failure when an existing service method returns
/// a primitive or `Option` rather than `Result`. Its typed placeholder never
/// escapes the pipeline: the handler step replaces it with this refusal before
/// envelope handling and audit. Keep the first failure, and isolate nested calls.
pub fn report_refusal(code: impl Into<String>, message: impl Into<String>) {
    let refusal = crate::refusal::Refusal::new(code, message);
    if REPORTED_REFUSAL
        .try_with(|slot| {
            slot.borrow_mut().get_or_insert(refusal);
        })
        .is_err()
    {
        tracing::error!("native backend refusal reported outside the invoker pipeline");
    }
}

pub(crate) fn take_reported_refusal() -> Option<crate::refusal::Refusal> {
    REPORTED_REFUSAL
        .try_with(|slot| slot.borrow_mut().take())
        .ok()
        .flatten()
}

/// The context of the call this code is running inside, if any.
pub fn current() -> Option<Arc<CallContext>> {
    CURRENT.try_with(Arc::clone).ok()
}

/// The current call's id — what the store stamps as `batch_id`.
pub fn current_call_id() -> Option<String> {
    CURRENT.try_with(|c| c.call_id.clone()).ok()
}

/// Record a store mutation in the current call, if there is one. The store
/// invokes this only after a successful mutation has been committed.
pub fn note_mutation(id: String, created: bool, deleted: bool) {
    let _ = CURRENT.try_with(|context| context.mutation_ids.note(id, created, deleted));
}

/// Record that the current call read the record `id`, if there is a call
/// (plan-koine-notebook N-D3). A store wrapper calls this after a read
/// succeeded (`koine_store::RecordingStore`).
pub fn note_read(id: String) {
    let _ = CURRENT.try_with(|context| context.notes.note_read(id));
}

/// Record that the current call changed the record `id` in place, if there
/// is a call (plan-koine-notebook N-D3): an update or an attributed
/// operation, as opposed to a creation or deletion ([`note_mutation`]).
pub fn note_update(id: String) {
    let _ = CURRENT.try_with(|context| context.notes.note_update(id));
}

/// The adverbs of the current call; empty outside a call.
pub fn current_adverbs() -> Adverbs {
    CURRENT.try_with(|c| c.adverbs.clone()).unwrap_or_default()
}

/// Record the inverse of what the current call just did, for `reversibly`:
/// the pipeline puts it on the envelope as `"undo": {"verb", "args"}` and on
/// the audit row. A later note replaces an earlier one.
pub fn note_inverse(verb: impl Into<String>, args: serde_json::Value) {
    let verb = verb.into();
    let _ = CURRENT.try_with(|context| context.inverse.set(verb, args));
}

/// The store the current call was told to use, when it was told one.
pub fn store_override<T: Any + Send + Sync>() -> Option<Arc<T>> {
    let erased = CURRENT
        .try_with(|c| c.store_override.clone())
        .ok()
        .flatten()?;
    erased.downcast::<T>().ok()
}

/// The store the current call was told to use, still type-erased. A trait
/// object cannot be downcast out of `dyn Any`, so a caller that wants the
/// store as a port widens it itself (`koine_store::process` keeps the table
/// of concrete store types it knows how to widen).
pub fn store_override_erased() -> Option<Arc<dyn Any + Send + Sync>> {
    CURRENT
        .try_with(|c| c.store_override.clone())
        .ok()
        .flatten()
}

/// Run `future` with `context` current.
pub async fn scope<F: std::future::Future>(context: Arc<CallContext>, future: F) -> F::Output {
    REPORTED_REFUSAL
        .scope(RefCell::new(None), CURRENT.scope(context, future))
        .await
}

/// Run `work` with `context` current, synchronously — the FFI's layout
/// `apply`, which is a sync call from Swift.
pub fn sync_scope<T>(context: Arc<CallContext>, work: impl FnOnce() -> T) -> T {
    REPORTED_REFUSAL.sync_scope(RefCell::new(None), || CURRENT.sync_scope(context, work))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(id: &str) -> Arc<CallContext> {
        Arc::new(CallContext {
            call_id: id.to_string(),
            trace_id: "t".to_string(),
            parent_call: None,
            caller: CallerIdentity::Person,
            verb: "x-service_y".into(),
            store_override: None,
            mutation_ids: MutationIds::default(),
            adverbs: Adverbs::default(),
            inverse: InverseSlot::default(),
            notes: CallNotes::default(),
        })
    }

    #[test]
    fn nothing_is_current_outside_a_call() {
        assert!(current().is_none());
        assert!(current_call_id().is_none());
        assert!(store_override::<String>().is_none());
    }

    #[test]
    fn a_sync_scope_is_visible_inside_and_gone_after() {
        let seen = sync_scope(context("c1"), current_call_id);
        assert_eq!(seen.as_deref(), Some("c1"));
        assert!(current_call_id().is_none());
    }

    #[test]
    fn transport_context_round_trips_the_trusted_parent_identity_and_lineage() {
        let mut parent = (*context("parent-call")).clone();
        parent.caller = CallerIdentity::agent("surface-ffi");
        parent.trace_id = "surface-trace".into();
        let transported = TransportContext::from_parent(&parent);
        let decoded = TransportContext::from_json(&transported.to_json()).unwrap();
        let nested = decoded.into_call(serde_json::json!({"domain": "value"}));

        assert_eq!(nested.caller, CallerIdentity::agent("surface-ffi"));
        assert_eq!(nested.trace_id.as_deref(), Some("surface-trace"));
        assert_eq!(nested.parent_call.as_deref(), Some("parent-call"));
        assert_eq!(nested.args["domain"], "value");
    }

    #[test]
    fn transport_context_refuses_ambiguous_or_malformed_identity() {
        for raw in [
            "not json",
            r#"{"kind":"person","trace_id":"t","parent_call":"p","caller":"system"}"#,
            r#"{"kind":"unknown","trace_id":"t","parent_call":"p"}"#,
            r#"{"kind":"person","trace_id":"","parent_call":"p"}"#,
        ] {
            assert!(TransportContext::from_json(raw).is_err(), "accepted {raw}");
        }
    }

    #[test]
    fn an_inverse_is_noted_on_the_current_call_only() {
        note_inverse("x_ignored", serde_json::json!({}));
        let ctx = context("c4");
        sync_scope(ctx.clone(), || {
            note_inverse("x-service_undo", serde_json::json!({"id": 1}));
            assert!(current_adverbs().is_empty());
        });
        assert_eq!(
            ctx.inverse.take(),
            Some(("x-service_undo".to_string(), serde_json::json!({"id": 1})))
        );
        assert_eq!(ctx.inverse.take(), None);
    }

    #[tokio::test]
    async fn an_async_scope_survives_awaits() {
        let seen = scope(context("c2"), async {
            tokio::task::yield_now().await;
            current_call_id()
        })
        .await;
        assert_eq!(seen.as_deref(), Some("c2"));
    }

    #[test]
    fn the_store_override_downcasts_to_its_type_and_nothing_else() {
        let mut ctx = (*context("c3")).clone();
        ctx.store_override = Some(Arc::new(42u64) as Arc<dyn Any + Send + Sync>);
        sync_scope(Arc::new(ctx), || {
            assert_eq!(store_override::<u64>().as_deref(), Some(&42));
            assert!(store_override::<String>().is_none());
        });
    }

    #[tokio::test]
    async fn nested_and_concurrent_calls_keep_their_mutation_ids_separate() {
        let outer = context("outer");
        let concurrent = context("concurrent");
        let concurrent_result = tokio::spawn(scope(concurrent.clone(), async move {
            note_mutation("concurrent-id".into(), true, false);
            tokio::task::yield_now().await;
            concurrent.mutation_ids.snapshot()
        }));

        scope(outer.clone(), async {
            note_mutation("outer-id".into(), true, false);
            let inner = context("inner");
            scope(inner.clone(), async {
                note_mutation("inner-id".into(), false, true);
                note_mutation("inner-id".into(), false, true);
                tokio::task::yield_now().await;
            })
            .await;
            assert_eq!(
                inner.mutation_ids.snapshot(),
                (vec![], vec!["inner-id".into()])
            );
            note_mutation("outer-id".into(), true, false);
        })
        .await;

        assert_eq!(
            outer.mutation_ids.snapshot(),
            (vec!["outer-id".into()], vec![])
        );
        assert_eq!(
            concurrent_result.await.unwrap(),
            (vec!["concurrent-id".into()], vec![])
        );
        note_mutation("outside".into(), true, true);
        assert_eq!(
            outer.mutation_ids.snapshot(),
            (vec!["outer-id".into()], vec![])
        );
    }
}
