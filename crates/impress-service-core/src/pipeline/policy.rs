//! Policy: `(caller, safety class, verb) → run | review | deny` (ADR-0034 D3,
//! plan-verb-pipeline § Policy).
//!
//! The mechanism behind "human review points are explicit": a mutating,
//! destructive or external verb from an agent is *queued* as the verb's
//! review surface instead of run, and the call answers
//! `{ok: false, code: "review-pending", surface_id}`; the person's later
//! confirmation re-enters the pipeline as the person. Nothing halts the
//! window.
//!
//! The default policy runs everything for every caller, so behaviour is
//! unchanged until a policy is configured — by [`install`] from the host, or
//! by `IMPRESS_VERB_POLICY` for a session:
//!
//! * `run-all` (the default);
//! * `review-agent-writes`: agents and providers are queued for every
//!   mutating, destructive or external verb;
//! * `review-agent-destructive`: agents and providers are queued for
//!   destructive and external verbs only (ADR-0034 D3's default once the
//!   generated review surface exists, ADR-0035 D2).
//!
//! Queuing needs a [`ReviewQueue`] — the surface the person confirms is
//! ADR-0035 G-side work — so a process with a review policy and no queue
//! answers `review-pending` naming that no queue is installed; the verb still
//! does not run, which is the safe direction.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, OnceLock, RwLock};

use serde_json::Value;

use super::identity::CallerIdentity;
use crate::descriptor::{Effects, Kind, Safety, SafetyClass, VerbDescriptor};
use crate::descriptor_handle::VerbHandle;
use crate::refusal::Refusal;

/// The generic refusal code for a call the policy queued.
pub const REVIEW_PENDING: &str = "review-pending";
/// The generic refusal code a verb uses for a caller it does not serve
/// ("person only").
pub const FORBIDDEN: &str = "forbidden";
/// The code for a call the policy refused outright (plan-koine-trust
/// TR-D5): distinct from a failure, never worth retrying.
pub const REJECTED: &str = "rejected";

/// `status` on an envelope that is not a result (TR-D5), in the agent
/// protocols' words: waiting for a person (MCP `input_required`, A2A
/// `INPUT_REQUIRED`).
pub const STATUS_INPUT_REQUIRED: &str = "input_required";
/// A secret the verb needs is not set (A2A `AUTH_REQUIRED`).
pub const STATUS_AUTH_REQUIRED: &str = "auth_required";
/// The policy refused the call (A2A `REJECTED`).
pub const STATUS_REJECTED: &str = "rejected";

/// A person's approval of one pending call (plan-koine-trust TR-D4): the
/// call runs once more as its original caller and the policy does not ask
/// again — for that verb with those arguments, and for the calls it makes.
/// Built only in process (by the review queue when a person approves), so a
/// caller cannot send one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    pending_id: String,
    verb: String,
    fingerprint: String,
}

impl Approval {
    /// The approval of pending call `pending_id`: `verb` with `args` (the
    /// `adverbs` key, if any, is not part of what was approved).
    pub fn grant(pending_id: impl Into<String>, verb: impl Into<String>, args: &Value) -> Self {
        let mut args = args.clone();
        if let Some(object) = args.as_object_mut() {
            object.remove(crate::Adverbs::KEY);
        }
        Self {
            pending_id: pending_id.into(),
            verb: verb.into(),
            fingerprint: super::adverbs::fingerprint(&args),
        }
    }

    pub fn pending_id(&self) -> &str {
        &self.pending_id
    }

    /// Whether this approval is for `verb` called with `args`.
    pub fn covers(&self, verb: &str, args: &Value) -> bool {
        self.verb == verb && self.fingerprint == super::adverbs::fingerprint(args)
    }
}

/// What the policy decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Run,
    Review,
    Deny(String),
    /// D-R11: a destructive verb whose declared write set overlaps another
    /// call's, still in flight on the same kind. `String` is the conflicting
    /// kind.
    Conflict(String),
}

// ---------------------------------------------------------------------------
// D-R11: concurrent-write conflict detection
// ---------------------------------------------------------------------------
//
// "Two concurrent calls whose declared write sets overlap: a destructive
// overlap is refused (`conflict`); a mutating overlap is logged."
// (plan-self-reflective-layer.md's E3 row, D-R11).
//
// An IN-FLIGHT SET KEYED BY KIND: a refcount per literal declared write kind,
// held only for calls actually granted `Run` (never for one this policy
// queues or denies, and never counted twice for a refused one) and released
// by [`release`] when the call finishes. `pipeline::mod.rs`'s `finish` calls
// it — one line there, the one place that already knows a call ended,
// alongside the one match arm `prepare` needs for the new `Decision::Conflict`
// variant; neither touches the audit/call-context machinery L1 owns.
//
// The set is keyed by kind ALONE, not by which store a call ran against —
// this layer sees a verb and a caller, never a store handle, and most calls
// (the FFI's, the CLI's) never pass one explicitly at all; they reach a
// process-wide singleton several layers down. That is exactly right for what
// this guards (ADR-0034's one process, one shared store), and exactly wrong
// for a test binary that opens a fresh temp store per test and runs them in
// parallel threads of ONE process — two unrelated tests writing the "same"
// kind on two different stores would spuriously conflict. [`enabled`] is the
// fix: off by default (unchanged behaviour, like every other policy here
// until configured), on via [`enable_conflict_detection`] or
// `IMPRESS_VERB_CONFLICT_DETECTION=1` — a host that actually shares one
// store across concurrent callers turns it on; a test process opening many
// stores in parallel does not.
static CONFLICT_DETECTION: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static CONFLICT_DETECTION_FROM_ENV: OnceLock<bool> = OnceLock::new();

/// Turn D-R11's conflict check on for this process (see the module docs
/// above [`InFlight`] for why it is off by default).
pub fn enable_conflict_detection() {
    CONFLICT_DETECTION.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn conflict_detection_enabled() -> bool {
    CONFLICT_DETECTION.load(std::sync::atomic::Ordering::Relaxed)
        || *CONFLICT_DETECTION_FROM_ENV
            .get_or_init(|| std::env::var("IMPRESS_VERB_CONFLICT_DETECTION").as_deref() == Ok("1"))
}

struct InFlight {
    counts: Mutex<HashMap<String, usize>>,
}

impl InFlight {
    fn new() -> Self {
        Self {
            counts: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, usize>> {
        self.counts.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Kinds of `writes` already in flight, then record every kind of
    /// `writes` as in flight (refcounted, so two callers holding the same
    /// kind both need to [`Self::release`] it).
    fn overlap_and_record(&self, writes: &[&str]) -> Vec<String> {
        let mut table = self.lock();
        let overlap: Vec<String> = writes
            .iter()
            .filter(|k| table.contains_key(**k))
            .map(|k| k.to_string())
            .collect();
        for k in writes {
            *table.entry(k.to_string()).or_insert(0) += 1;
        }
        overlap
    }

    fn release(&self, writes: &[&str]) {
        let mut table = self.lock();
        for k in writes {
            if let Some(count) = table.get_mut(*k) {
                if *count <= 1 {
                    table.remove(*k);
                } else {
                    *count -= 1;
                }
            }
        }
    }
}

static IN_FLIGHT: LazyLock<InFlight> = LazyLock::new(InFlight::new);

/// The literal (`Kind::Ref`) kinds `verb` declares among its writes — the
/// same "dynamic declarations are this static walk's blind spot, not its
/// problem to solve" reasoning `impress-surface-service::runtime`'s
/// `verb_declared_read_refs` gives for reads (E3).
fn literal_write_kinds(verb: &dyn VerbFacts) -> Vec<&'static str> {
    verb.effects()
        .writes
        .iter()
        .filter_map(|k| match k {
            Kind::Ref(r) => Some(*r),
            _ => None,
        })
        .collect()
}

/// D-R11's check, run for every call this policy is about to let run
/// (read-only verbs write nothing to conflict over, and a call this policy
/// queues or denies never runs at all, so it never takes a lease). A
/// destructive verb whose declared writes overlap another call's in-flight
/// writes is refused `conflict` — WITHOUT taking a lease (it never runs, so
/// it never releases one either). A mutating verb's overlap is logged
/// (`tracing::warn!`) and still takes its lease and runs — the row's
/// "refused / logged" split, by class. [`release`] undoes what this took,
/// once the call finishes.
fn check_conflict(verb: &dyn VerbFacts) -> Option<Decision> {
    if !conflict_detection_enabled() || verb.safety().class == SafetyClass::ReadOnly {
        return None;
    }
    let writes = literal_write_kinds(verb);
    if writes.is_empty() {
        return None;
    }
    let overlap = IN_FLIGHT.overlap_and_record(&writes);
    if overlap.is_empty() {
        return None;
    }
    match verb.safety().class {
        SafetyClass::Destructive | SafetyClass::External => {
            // This call will not run: give back the lease `overlap_and_record`
            // just took for it.
            IN_FLIGHT.release(&writes);
            Some(Decision::Conflict(overlap.join(", ")))
        }
        _ => {
            tracing::warn!(
                target: "verb",
                verb = verb.name(),
                kinds = %overlap.join(", "),
                "mutating call overlaps another in-flight call's declared writes (D-R11)"
            );
            None
        }
    }
}

/// Give back the lease [`check_conflict`] took for `verb`'s declared writes
/// — called once, when the call finishes (`pipeline::mod.rs`'s `finish`),
/// whether it ran, was refused for some OTHER reason (strict args,
/// reachability — no lease was taken, so this is a harmless no-op), or
/// failed. Never called for a call `decide` answered `Conflict` for: that
/// call never took a lease (see [`check_conflict`]).
pub fn release(verb: &dyn VerbFacts) {
    let writes = literal_write_kinds(verb);
    if !writes.is_empty() {
        IN_FLIGHT.release(&writes);
    }
}

/// Facts the policy reads from either a linked descriptor or an owned provider snapshot.
/// Keeping this view borrowed lets a runtime provider use the same policy and review queue.
pub trait VerbFacts {
    fn name(&self) -> &str;
    fn safety(&self) -> Safety;
    fn effects(&self) -> Effects;
    fn is_provider(&self) -> bool {
        false
    }
    /// How a person is asked about it (TR-D6).
    fn consent(&self) -> crate::Consent {
        crate::Consent::DEFAULT
    }
    /// What a standing rule for it was made against (TR-D2, TR-D9).
    fn fingerprint(&self) -> String {
        String::new()
    }
}

impl VerbFacts for VerbDescriptor {
    fn consent(&self) -> crate::Consent {
        self.consent
    }
    fn name(&self) -> &str {
        self.name
    }
    fn safety(&self) -> Safety {
        self.safety
    }
    fn effects(&self) -> Effects {
        self.effects
    }
}

impl VerbFacts for VerbHandle {
    fn name(&self) -> &str {
        self.name()
    }
    fn safety(&self) -> Safety {
        self.safety()
    }
    fn effects(&self) -> Effects {
        self.effects()
    }
    fn is_provider(&self) -> bool {
        self.linked().is_none()
    }
    fn consent(&self) -> crate::Consent {
        self.consent()
    }
    fn fingerprint(&self) -> String {
        self.fingerprint()
    }
}

/// What a policy decides one call from (plan-koine-trust TR-D6).
pub struct PolicyInput<'a> {
    pub caller: &'a CallerIdentity,
    pub verb: &'a dyn VerbFacts,
    /// The call's arguments, the `adverbs` key removed.
    pub args: &'a Value,
    /// The session the call belongs to, when the transport named one.
    pub session: Option<&'a str>,
}

/// A decision and, for a review or a denial, why — the person reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub decision: Decision,
    pub reason: Option<String>,
}

impl Verdict {
    pub fn run() -> Self {
        Self {
            decision: Decision::Run,
            reason: None,
        }
    }

    pub fn review(reason: impl Into<String>) -> Self {
        Self {
            decision: Decision::Review,
            reason: Some(reason.into()),
        }
    }
}

/// A policy decides from the caller and the descriptor ([`Policy::decide`]),
/// or from the whole call ([`Policy::decide_call`], which by default asks
/// `decide`).
pub trait Policy: Send + Sync {
    fn decide(&self, caller: &CallerIdentity, verb: &dyn VerbFacts) -> Decision;

    fn decide_call(&self, input: &PolicyInput<'_>) -> Verdict {
        Verdict {
            decision: self.decide(input.caller, input.verb),
            reason: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Consent (plan-koine-trust TR-D6, TR-D9)
// ---------------------------------------------------------------------------

/// A standing decision a person made ("always allow", "always reject"),
/// kept as a `koine/consent-rule@1.0.0` item (TR-D9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentRule {
    pub id: String,
    /// A verb name, or a prefix ending in `*` (`collection-service_*`).
    pub verb: String,
    /// A caller author (`agent:claude`), or a prefix ending in `*`
    /// (`agent:*`).
    pub caller: String,
    pub allow: bool,
    /// The verb's fingerprint when the rule was made; a rule for a verb whose
    /// fingerprint changed no longer applies.
    pub fingerprint: String,
    /// Milliseconds since the epoch; `None` never expires.
    pub expires_ms: Option<i64>,
}

fn pattern_matches(pattern: &str, value: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => value.starts_with(prefix),
        None => pattern == value,
    }
}

impl ConsentRule {
    /// Whether this rule decides a call of `verb` (with `fingerprint`) by
    /// `caller` at `now_ms`.
    pub fn applies(&self, verb: &str, fingerprint: &str, caller: &str, now_ms: i64) -> bool {
        pattern_matches(&self.verb, verb)
            && pattern_matches(&self.caller, caller)
            && self.expires_ms.is_none_or(|at| at > now_ms)
            && (self.verb.ends_with('*')
                || self.fingerprint.is_empty()
                || self.fingerprint == fingerprint)
    }
}

/// Where the consent policy reads standing rules from (the review queue's
/// store, installed with it).
pub trait RuleSource: Send + Sync {
    fn rules(&self) -> Vec<ConsentRule>;
}

static RULES: RwLock<Option<Arc<dyn RuleSource>>> = RwLock::new(None);

/// Install where standing consent rules come from.
pub fn install_rule_source(source: Arc<dyn RuleSource>) {
    if let Ok(mut slot) = RULES.write() {
        *slot = Some(source);
    }
}

/// The standing rules, when a source is installed.
pub fn rules() -> Vec<ConsentRule> {
    RULES
        .read()
        .ok()
        .and_then(|r| r.clone())
        .map(|source| source.rules())
        .unwrap_or_default()
}

type StoreSource = fn() -> Option<Arc<dyn impress_item::AttributedStore>>;
static STORE_SOURCE: std::sync::OnceLock<StoreSource> = std::sync::OnceLock::new();

/// Register how a review hook reaches the store (`koine_store::process`
/// does it).
pub fn set_store_source(source: StoreSource) {
    let _ = STORE_SOURCE.set(source);
}

fn store_for_hooks() -> Option<Arc<dyn impress_item::AttributedStore>> {
    STORE_SOURCE.get().and_then(|source| source())
}

// ---------------------------------------------------------------------------
// Session risk (plan-koine-trust TR-D11)
// ---------------------------------------------------------------------------

/// Reads the person's data.
pub const RISK_PRIVATE: u8 = 1;
/// Takes in content the person did not write.
pub const RISK_UNTRUSTED: u8 = 2;
/// Sends something out of the process.
pub const RISK_EGRESS: u8 = 4;
/// All three: the "lethal trifecta".
pub const RISK_ALL: u8 = RISK_PRIVATE | RISK_UNTRUSTED | RISK_EGRESS;

/// How long a session's risk is kept after its last call.
pub const SESSION_IDLE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// The risks a call of `verb` carries: what it declares, and egress when it
/// reaches out of the process.
pub fn risk_of(verb: &dyn VerbFacts) -> u8 {
    let mut risk = 0;
    for declared in verb.consent().risk {
        risk |= match declared {
            crate::Risk::Private => RISK_PRIVATE,
            crate::Risk::Untrusted => RISK_UNTRUSTED,
        };
    }
    if verb.safety().class == SafetyClass::External
        || verb.effects().reach.iter().any(|r| r.is_egress())
    {
        risk |= RISK_EGRESS;
    }
    risk
}

/// The words for a risk set.
pub fn risk_words(risk: u8) -> Vec<&'static str> {
    let mut words = Vec::new();
    if risk & RISK_PRIVATE != 0 {
        words.push("read private data");
    }
    if risk & RISK_UNTRUSTED != 0 {
        words.push("taken in untrusted content");
    }
    if risk & RISK_EGRESS != 0 {
        words.push("sent data out");
    }
    words
}

static LEDGER: LazyLock<Mutex<HashMap<String, (u8, std::time::Instant)>>> =
    LazyLock::new(Default::default);

/// Record that a call of `verb` ran in `session`.
pub fn note_ran(session: &str, verb: &dyn VerbFacts) {
    let risk = risk_of(verb);
    let mut ledger = LEDGER.lock().unwrap_or_else(|e| e.into_inner());
    let now = std::time::Instant::now();
    ledger.retain(|_, (_, seen)| now.duration_since(*seen) < SESSION_IDLE);
    let entry = ledger.entry(session.to_owned()).or_insert((0, now));
    entry.0 |= risk;
    entry.1 = now;
}

/// The risks `session` has run so far (expired sessions hold none).
pub fn session_risk(session: &str) -> u8 {
    let ledger = LEDGER.lock().unwrap_or_else(|e| e.into_inner());
    match ledger.get(session) {
        Some((risk, seen)) if seen.elapsed() < SESSION_IDLE => *risk,
        _ => 0,
    }
}

/// Forget a session's risk (a person ending it).
pub fn forget_session(session: &str) {
    LEDGER
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(session);
}

/// The "Rule of Two" (TR-D11): an agent's session may hold at most two of
/// private data, untrusted content and egress unsupervised. A call that would
/// give it the third is held; once it holds all three, every write and every
/// egress is. A person's and an app's calls are never held for it.
pub fn session_risk_check(input: &PolicyInput<'_>) -> Option<String> {
    if !matches!(
        input.caller,
        CallerIdentity::Agent(_) | CallerIdentity::Provider(_)
    ) {
        return None;
    }
    let session = input.session?;
    let held = session_risk(session);
    let adds = risk_of(input.verb);
    let writes = input.verb.safety().class != SafetyClass::ReadOnly;
    if held == RISK_ALL && (writes || adds & RISK_EGRESS != 0) {
        return Some(format!(
            "this session has already {}; a person supervises what it writes or sends from here",
            risk_words(held).join(", ")
        ));
    }
    if held != RISK_ALL && held | adds == RISK_ALL {
        return Some(format!(
            "this session has {}, and this call would also have it {}: a person decides",
            risk_words(held).join(" and "),
            risk_words(adds & !held).join(" and ")
        ));
    }
    None
}

/// A session's risk check, replaceable for a host that judges otherwise.
pub type RiskCheck = fn(&PolicyInput<'_>) -> Option<String>;
static RISK_CHECK: std::sync::OnceLock<RiskCheck> = std::sync::OnceLock::new();

/// Replace the session risk check the consent policy consults.
pub fn set_risk_check(check: RiskCheck) {
    let _ = RISK_CHECK.set(check);
}

/// The impact at or above which a caller's call is reviewed; `None` never.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thresholds {
    pub person: Option<crate::Impact>,
    pub app: Option<crate::Impact>,
    pub agent: Option<crate::Impact>,
    /// The task kernel's agent (`agent:tasks`): scheduled work a person
    /// filed.
    pub tasks: Option<crate::Impact>,
    pub provider: Option<crate::Impact>,
    pub system: Option<crate::Impact>,
}

impl Thresholds {
    /// A person and their app are never asked (they may ask for it with
    /// `with_review`); an agent is asked from medium impact (a write);
    /// scheduled tasks from high; a provider from medium.
    pub const DEFAULT: Thresholds = Thresholds {
        person: None,
        app: None,
        agent: Some(crate::Impact::Medium),
        tasks: Some(crate::Impact::High),
        provider: Some(crate::Impact::Medium),
        system: None,
    };

    pub fn for_caller(&self, caller: &CallerIdentity) -> Option<crate::Impact> {
        match caller {
            CallerIdentity::Person => self.person,
            CallerIdentity::App(_) => self.app,
            CallerIdentity::Agent(name) if name == "tasks" => self.tasks,
            CallerIdentity::Agent(_) => self.agent,
            CallerIdentity::Provider(_) => self.provider,
            CallerIdentity::System(_) => self.system,
        }
    }
}

/// The consent policy (TR-D6): a standing rule first, then the session's
/// risk, then the verb's own review check, then the verb's impact against
/// the caller's threshold.
pub struct ConsentPolicy {
    pub thresholds: Thresholds,
}

impl ConsentPolicy {
    pub const DEFAULT: ConsentPolicy = ConsentPolicy {
        thresholds: Thresholds::DEFAULT,
    };
}

impl Policy for ConsentPolicy {
    fn decide(&self, caller: &CallerIdentity, verb: &dyn VerbFacts) -> Decision {
        self.decide_call(&PolicyInput {
            caller,
            verb,
            args: &Value::Null,
            session: None,
        })
        .decision
    }

    fn decide_call(&self, input: &PolicyInput<'_>) -> Verdict {
        let caller = input.caller.author();
        let verb = input.verb.name();
        let fingerprint = input.verb.fingerprint();
        let now = chrono::Utc::now().timestamp_millis();
        let rules = rules();
        let applying: Vec<&ConsentRule> = rules
            .iter()
            .filter(|r| r.applies(verb, &fingerprint, &caller, now))
            .collect();
        if let Some(rule) = applying.iter().find(|r| !r.allow) {
            return Verdict {
                decision: Decision::Deny(format!(
                    "a standing rule rejects {verb} from {caller} (rule {})",
                    rule.id
                )),
                reason: None,
            };
        }
        // A session that has gathered too much risk is asked even where an
        // allow rule would let the call through, and even for a read that
        // would complete it. Otherwise a read changes nothing to ask about,
        // and an allow rule settles it.
        let check = RISK_CHECK.get().copied().unwrap_or(session_risk_check);
        if let Some(reason) = check(input) {
            return Verdict::review(reason);
        }
        let class = input.verb.safety().class;
        if class == SafetyClass::ReadOnly {
            return Verdict::run();
        }
        if applying.iter().any(|r| r.allow) {
            return Verdict::run();
        }
        let consent = input.verb.consent();
        if let Some(hook) = consent.review {
            let store = store_for_hooks();
            return match hook(input.args, store.as_deref()) {
                crate::ReviewNeed::Review(reason) => Verdict::review(reason),
                crate::ReviewNeed::No => Verdict::run(),
            };
        }
        let impact = consent.impact_for(class);
        match self.thresholds.for_caller(input.caller) {
            Some(threshold) if impact >= threshold => Verdict::review(format!(
                "{verb} has {impact} impact, and {caller} is asked from {threshold}"
            )),
            _ => Verdict::run(),
        }
    }
}

/// Runs everything: the default until a host configures otherwise.
pub struct RunEverything;

impl Policy for RunEverything {
    fn decide(&self, _: &CallerIdentity, _: &dyn VerbFacts) -> Decision {
        Decision::Run
    }
}

/// Queue agents and providers for the listed classes; the person, the app
/// and the system always run.
pub struct ReviewAgents {
    pub classes: &'static [SafetyClass],
}

impl ReviewAgents {
    /// Every class that is not read-only.
    pub const WRITES: ReviewAgents = ReviewAgents {
        classes: &[
            SafetyClass::Mutating,
            SafetyClass::Destructive,
            SafetyClass::External,
        ],
    };
    /// Destructive and external only.
    pub const DESTRUCTIVE: ReviewAgents = ReviewAgents {
        classes: &[SafetyClass::Destructive, SafetyClass::External],
    };
}

impl Policy for ReviewAgents {
    fn decide(&self, caller: &CallerIdentity, verb: &dyn VerbFacts) -> Decision {
        let reviewed = matches!(
            caller,
            CallerIdentity::Agent(_) | CallerIdentity::Provider(_)
        ) && self.classes.contains(&verb.safety().class);
        if reviewed {
            Decision::Review
        } else {
            Decision::Run
        }
    }
}

/// Where a reviewed call goes: the host creates the verb's review surface and
/// answers its id.
pub trait ReviewQueue: Send + Sync {
    fn enqueue(
        &self,
        caller: &CallerIdentity,
        verb: &dyn VerbFacts,
        args: &Value,
    ) -> Result<String, Refusal>;

    /// Queue a held call with why it was held (TR-D6, TR-D7). By default
    /// [`Self::enqueue`].
    fn enqueue_request(&self, request: &PendingRequest<'_>) -> Result<String, Refusal> {
        self.enqueue(request.caller, request.verb, request.args)
    }
}

/// A call held for a person, as the queue receives it.
pub struct PendingRequest<'a> {
    pub caller: &'a CallerIdentity,
    pub verb: &'a dyn VerbFacts,
    pub args: &'a Value,
    /// Why it was held.
    pub reason: Option<&'a str>,
    pub session: Option<&'a str>,
}

static POLICY: RwLock<Option<Arc<dyn Policy>>> = RwLock::new(None);
static QUEUE: RwLock<Option<Arc<dyn ReviewQueue>>> = RwLock::new(None);
static FROM_ENV: OnceLock<Option<Arc<dyn Policy>>> = OnceLock::new();

/// Install (or replace) the process's policy.
pub fn install(policy: Arc<dyn Policy>) {
    if let Ok(mut slot) = POLICY.write() {
        *slot = Some(policy);
    }
}

/// Install (or replace) the process's review queue.
pub fn install_queue(queue: Arc<dyn ReviewQueue>) {
    if let Ok(mut slot) = QUEUE.write() {
        *slot = Some(queue);
    }
}

/// The policy named by `IMPRESS_VERB_POLICY`, read once.
fn from_env() -> Option<Arc<dyn Policy>> {
    FROM_ENV
        .get_or_init(|| match std::env::var("IMPRESS_VERB_POLICY").as_deref() {
            Ok("review-agent-writes") => Some(Arc::new(ReviewAgents::WRITES) as Arc<dyn Policy>),
            Ok("review-agent-destructive") => Some(Arc::new(ReviewAgents::DESTRUCTIVE)),
            Ok("consent") => Some(Arc::new(ConsentPolicy::DEFAULT)),
            _ => None,
        })
        .clone()
}

/// Decide for one call: D-R11's conflict check first (every caller, not
/// just agents — a resource conflict is not a review policy), then the
/// installed policy, else the environment's, else run.
pub fn decide(caller: &CallerIdentity, verb: &dyn VerbFacts) -> Decision {
    let installed = POLICY.read().ok().and_then(|p| p.clone());
    let decision = match installed.or_else(from_env) {
        Some(policy) => policy.decide(caller, verb),
        None if verb.is_provider() => ReviewAgents::DESTRUCTIVE.decide(caller, verb),
        None => Decision::Run,
    };
    // D-R11's conflict check only matters for a call this policy would let
    // run — one it queues or denies never runs at all, so it never takes a
    // write lease (see `check_conflict`'s and `release`'s doc comments).
    if decision == Decision::Run {
        if let Some(conflict) = check_conflict(verb) {
            return conflict;
        }
    }
    decision
}

/// [`decide`] from the whole call (TR-D6): the installed policy's
/// [`Policy::decide_call`], then D-R11's conflict check.
pub fn decide_call(input: &PolicyInput<'_>) -> Verdict {
    let installed = POLICY.read().ok().and_then(|p| p.clone());
    let verdict = match installed.or_else(from_env) {
        Some(policy) => policy.decide_call(input),
        None if input.verb.is_provider() => Verdict {
            decision: ReviewAgents::DESTRUCTIVE.decide(input.caller, input.verb),
            reason: None,
        },
        None => Verdict::run(),
    };
    if verdict.decision == Decision::Run {
        if let Some(conflict) = check_conflict(input.verb) {
            return Verdict {
                decision: conflict,
                reason: None,
            };
        }
    }
    verdict
}

/// [`decide_call`] without keeping a write lease (see
/// [`decide_without_lease`]).
pub fn decide_call_without_lease(input: &PolicyInput<'_>) -> Verdict {
    let verdict = decide_call(input);
    if verdict.decision == Decision::Run {
        give_back_lease(input.verb);
    }
    verdict
}

/// Whether [`decide`] took a write lease when it answered `Run` for `verb`
/// (the preconditions of [`check_conflict`]).
fn takes_lease(verb: &dyn VerbFacts) -> bool {
    conflict_detection_enabled()
        && verb.safety().class != SafetyClass::ReadOnly
        && !literal_write_kinds(verb).is_empty()
}

/// [`decide`] for a caller that only wants the answer (`tentatively`,
/// `with_review`): any write lease a `Run` decision took is given back at
/// once, so nothing is left in flight for a call that will not run.
pub fn decide_without_lease(caller: &CallerIdentity, verb: &dyn VerbFacts) -> Decision {
    let decision = decide(caller, verb);
    if decision == Decision::Run {
        give_back_lease(verb);
    }
    decision
}

/// Give back the lease a `Run` [`decide`] took, when it took one. Unlike
/// [`release`] this is a no-op for a verb that never leases, so it cannot
/// steal another call's.
pub fn give_back_lease(verb: &dyn VerbFacts) {
    if takes_lease(verb) {
        release(verb);
    }
}

/// Queue a reviewed call and build its answer.
pub fn queue(caller: &CallerIdentity, verb: &dyn VerbFacts, args: &Value) -> Value {
    queue_request(&PendingRequest {
        caller,
        verb,
        args,
        reason: None,
        session: None,
    })
}

/// [`queue`] with why the call was held.
pub fn queue_request(request: &PendingRequest<'_>) -> Value {
    let (caller, verb) = (request.caller, request.verb);
    let queue = QUEUE.read().ok().and_then(|q| q.clone());
    let mut answer = match queue {
        Some(queue) => match queue.enqueue_request(request) {
            Ok(pending_id) => {
                let mut v = crate::strict::refusal_value(&Refusal::new(
                    REVIEW_PENDING,
                    format!(
                        "{} from {caller} is waiting for a person to approve it (pending call \
                         {pending_id}); it runs once approved — do not retry",
                        verb.name()
                    ),
                ));
                v["pending_id"] = Value::String(pending_id.clone());
                // The id under its older name, for clients that read it.
                v["surface_id"] = Value::String(pending_id);
                v
            }
            Err(refusal) => crate::strict::refusal_value(&refusal),
        },
        None => crate::strict::refusal_value(&Refusal::new(
            REVIEW_PENDING,
            format!(
                "{} is {} and {caller} may not run it without review; no review queue \
                 is installed in this process, so the call was not run",
                verb.name(),
                verb.safety().class
            ),
        )),
    };
    if let Some(object) = answer.as_object_mut() {
        object.insert("verb".into(), Value::String(verb.name().to_string()));
        if object.get("code").and_then(Value::as_str) == Some(REVIEW_PENDING) {
            object.insert("status".into(), Value::String(STATUS_INPUT_REQUIRED.into()));
            if let Some(reason) = request.reason {
                object.insert("reason".into(), Value::String(reason.to_owned()));
            }
        }
    }
    answer
}

/// The answer for a denied call.
pub fn deny(verb: &dyn VerbFacts, reason: String) -> Value {
    let mut answer = crate::strict::refusal_value(&Refusal::new(
        REJECTED,
        format!("{}: {reason}", verb.name()),
    ));
    answer["status"] = Value::String(STATUS_REJECTED.into());
    answer
}

/// The answer for a call refused over D-R11: a destructive verb whose
/// declared writes overlap another call's in-flight writes on `kinds`.
pub fn conflict(verb: &dyn VerbFacts, kinds: String) -> Value {
    crate::strict::refusal_value(&Refusal::conflict(format!(
        "{} is destructive and its declared writes ({kinds}) overlap another call still in \
         flight on the same kind (D-R11)",
        verb.name()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::{Safety, Source};
    use crate::ServiceFuture;

    fn schema() -> Value {
        serde_json::json!({})
    }
    fn handler(_: Value) -> ServiceFuture {
        Box::pin(async { Ok(Value::Null) })
    }
    fn verb(class: SafetyClass) -> VerbDescriptor {
        VerbDescriptor {
            name: "t-service_x",
            service: "t-service",
            method: "x",
            description: "d",
            input_schema: schema,
            output_schema: schema,
            safety: Safety {
                class,
                idempotent: false,
            },
            since: "0.1.0",
            effects: crate::Effects::NONE,
            deprecated: None,
            aliases: &[],
            examples: &[],
            strict: false,
            budget_ms: None,
            replay_full: false,
            supports: &[],
            adjective: None,
            returns_kind: None,
            rows: None,
            secrets: &[],
            source: Source::Linked,
            handler,
            consent: crate::Consent::DEFAULT,
            intent: crate::IntentDecl::NONE,
        }
    }

    /// A verb declaring `writes` (as `Kind::Ref`s), for D-R11's tests. Every
    /// call passes a distinct `name`/kind: [`IN_FLIGHT`] is one process-wide
    /// static, and `cargo test` runs these in parallel threads.
    fn verb_writing(
        name: &'static str,
        class: SafetyClass,
        writes: &'static [Kind],
    ) -> VerbDescriptor {
        VerbDescriptor {
            name,
            service: "t-service",
            method: "x",
            description: "d",
            input_schema: schema,
            output_schema: schema,
            safety: Safety {
                class,
                idempotent: false,
            },
            since: "0.1.0",
            effects: crate::descriptor::Effects {
                reads: &[],
                writes,
                reach: &[],
            },
            deprecated: None,
            aliases: &[],
            examples: &[],
            strict: false,
            budget_ms: None,
            replay_full: false,
            supports: &[],
            adjective: None,
            returns_kind: None,
            rows: None,
            secrets: &[],
            source: Source::Linked,
            handler,
            consent: crate::Consent::DEFAULT,
            intent: crate::IntentDecl::NONE,
        }
    }

    #[test]
    fn the_default_runs_everything() {
        let v = verb(SafetyClass::Destructive);
        assert_eq!(
            RunEverything.decide(&CallerIdentity::agent("mcp"), &v),
            Decision::Run
        );
    }

    #[test]
    fn review_policies_queue_agents_and_never_the_person() {
        let agent = CallerIdentity::agent("mcp");
        let writes = ReviewAgents::WRITES;
        assert_eq!(
            writes.decide(&agent, &verb(SafetyClass::Mutating)),
            Decision::Review
        );
        assert_eq!(
            writes.decide(&agent, &verb(SafetyClass::ReadOnly)),
            Decision::Run
        );
        assert_eq!(
            writes.decide(&CallerIdentity::Person, &verb(SafetyClass::Destructive)),
            Decision::Run
        );
        assert_eq!(
            writes.decide(
                &CallerIdentity::system("d"),
                &verb(SafetyClass::Destructive)
            ),
            Decision::Run
        );
        let destructive = ReviewAgents::DESTRUCTIVE;
        assert_eq!(
            destructive.decide(&agent, &verb(SafetyClass::Mutating)),
            Decision::Run
        );
        assert_eq!(
            destructive.decide(&agent, &verb(SafetyClass::External)),
            Decision::Review
        );
    }

    #[test]
    fn a_queued_call_answers_review_pending_with_the_verb() {
        let answer = queue(
            &CallerIdentity::agent("mcp"),
            &verb(SafetyClass::Mutating),
            &Value::Null,
        );
        assert_eq!(answer["ok"], false);
        assert_eq!(answer["code"], REVIEW_PENDING);
        assert_eq!(answer["verb"], "t-service_x");
        assert_eq!(answer["wire_version"], crate::wire::WIRE_VERSION);
    }

    // ─── D-R11: concurrent-write conflict detection ───────────────────────

    #[test]
    fn two_destructive_calls_with_overlapping_writes_conflict() {
        enable_conflict_detection();
        const WRITES: &[Kind] = &[Kind::Ref("d-r11-test/destructive-overlap")];
        let a = verb_writing("d-r11_a1", SafetyClass::Destructive, WRITES);
        let b = verb_writing("d-r11_a2", SafetyClass::Destructive, WRITES);
        let agent = CallerIdentity::agent("mcp");
        assert_eq!(decide(&agent, &a), Decision::Run, "the first call runs");
        assert_eq!(
            decide(&agent, &b),
            Decision::Conflict("d-r11-test/destructive-overlap".to_string()),
            "the second, overlapping, destructive call is refused"
        );
    }

    #[test]
    fn a_mutating_overlap_is_not_refused() {
        enable_conflict_detection();
        const WRITES: &[Kind] = &[Kind::Ref("d-r11-test/mutating-overlap")];
        let a = verb_writing("d-r11_b1", SafetyClass::Mutating, WRITES);
        let b = verb_writing("d-r11_b2", SafetyClass::Mutating, WRITES);
        let agent = CallerIdentity::agent("mcp");
        assert_eq!(decide(&agent, &a), Decision::Run);
        assert_eq!(
            decide(&agent, &b),
            Decision::Run,
            "a mutating overlap is logged, not refused"
        );
    }

    #[test]
    fn disjoint_write_sets_never_conflict() {
        enable_conflict_detection();
        const WRITES_A: &[Kind] = &[Kind::Ref("d-r11-test/disjoint-a")];
        const WRITES_B: &[Kind] = &[Kind::Ref("d-r11-test/disjoint-b")];
        let a = verb_writing("d-r11_c1", SafetyClass::Destructive, WRITES_A);
        let b = verb_writing("d-r11_c2", SafetyClass::Destructive, WRITES_B);
        let agent = CallerIdentity::agent("mcp");
        assert_eq!(decide(&agent, &a), Decision::Run);
        assert_eq!(decide(&agent, &b), Decision::Run);
    }

    #[test]
    fn a_read_only_verb_never_conflicts() {
        assert_eq!(check_conflict(&verb(SafetyClass::ReadOnly)), None);
    }
}

#[cfg(test)]
mod consent_tests {
    use super::*;
    use crate::descriptor::{Consent, Impact, ReviewNeed};

    struct Facts {
        class: SafetyClass,
        consent: Consent,
        fingerprint: &'static str,
    }
    impl VerbFacts for Facts {
        fn name(&self) -> &str {
            "t-service_consent"
        }
        fn safety(&self) -> Safety {
            Safety {
                class: self.class,
                idempotent: false,
            }
        }
        fn effects(&self) -> Effects {
            Effects::NONE
        }
        fn consent(&self) -> Consent {
            self.consent
        }
        fn fingerprint(&self) -> String {
            self.fingerprint.to_owned()
        }
    }
    fn facts(class: SafetyClass) -> Facts {
        Facts {
            class,
            consent: Consent::DEFAULT,
            fingerprint: "fp1",
        }
    }
    fn decide(caller: &CallerIdentity, verb: &Facts, args: Value) -> Verdict {
        ConsentPolicy::DEFAULT.decide_call(&PolicyInput {
            caller,
            verb,
            args: &args,
            session: None,
        })
    }

    #[test]
    fn impact_is_compared_with_the_callers_threshold() {
        let agent = CallerIdentity::agent("claude");
        let tasks = CallerIdentity::agent("tasks");
        let person = CallerIdentity::Person;
        let mutating = facts(SafetyClass::Mutating);
        let destructive = facts(SafetyClass::Destructive);
        let read = facts(SafetyClass::ReadOnly);
        assert_eq!(
            decide(&agent, &mutating, Value::Null).decision,
            Decision::Review
        );
        assert!(decide(&agent, &mutating, Value::Null)
            .reason
            .unwrap()
            .contains("medium impact"));
        assert_eq!(decide(&agent, &read, Value::Null).decision, Decision::Run);
        assert_eq!(
            decide(&tasks, &mutating, Value::Null).decision,
            Decision::Run
        );
        assert_eq!(
            decide(&tasks, &destructive, Value::Null).decision,
            Decision::Review
        );
        assert_eq!(
            decide(&person, &destructive, Value::Null).decision,
            Decision::Run
        );
        // A verb that declares a lower impact than its class is not asked.
        let low = Facts {
            consent: Consent {
                impact: Some(Impact::Low),
                ..Consent::DEFAULT
            },
            ..facts(SafetyClass::Mutating)
        };
        assert_eq!(decide(&agent, &low, Value::Null).decision, Decision::Run);
    }

    fn drafts_are_free(args: &Value, _: Option<&dyn impress_item::AttributedStore>) -> ReviewNeed {
        if args["draft"] == true {
            ReviewNeed::No
        } else {
            ReviewNeed::Review("it is cited".into())
        }
    }

    #[test]
    fn a_review_hook_decides_from_the_arguments() {
        let agent = CallerIdentity::agent("claude");
        let hooked = Facts {
            consent: Consent {
                review: Some(drafts_are_free),
                ..Consent::DEFAULT
            },
            ..facts(SafetyClass::Destructive)
        };
        assert_eq!(
            decide(&agent, &hooked, serde_json::json!({"draft": true})).decision,
            Decision::Run
        );
        let held = decide(&agent, &hooked, serde_json::json!({"draft": false}));
        assert_eq!(held, Verdict::review("it is cited"));
    }

    #[test]
    fn a_rule_applies_to_its_verb_caller_and_fingerprint_until_it_expires() {
        let rule = ConsentRule {
            id: "r".into(),
            verb: "t-service_consent".into(),
            caller: "agent:*".into(),
            allow: true,
            fingerprint: "fp1".into(),
            expires_ms: Some(1_000),
        };
        assert!(rule.applies("t-service_consent", "fp1", "agent:claude", 999));
        assert!(
            !rule.applies("t-service_consent", "fp2", "agent:claude", 999),
            "the verb changed"
        );
        assert!(!rule.applies("t-service_consent", "fp1", "human", 999));
        assert!(
            !rule.applies("t-service_consent", "fp1", "agent:claude", 1_000),
            "expired"
        );
        assert!(!rule.applies("t-service_other", "fp1", "agent:claude", 999));
        let prefix = ConsentRule {
            verb: "t-service_*".into(),
            ..rule
        };
        assert!(prefix.applies("t-service_other", "anything", "agent:x", 0));
    }
}
