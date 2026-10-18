//! The host-facing half of the adverbs (docs/grammar.md § Adverbs): the
//! in-process `once` table, the [`IdempotencyStore`] a host may install so
//! `once` survives a restart, and the [`BackgroundRunner`] that `in_background`
//! hands a prepared call to. The pipeline itself stays store-free.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::Duration;

use serde_json::Value;

use crate::descriptor_handle::VerbHandle;
use crate::job::JobHandle;
use crate::refusal::{codes, Refusal};

use super::Call;

/// The most entries the in-process `once` table keeps; the oldest is evicted
/// first.
pub const ONCE_CAPACITY: usize = 1024;

/// How long a `once` answer is kept by default (plan-koine-trust TR-D1);
/// `IMPRESS_ONCE_TTL` (seconds) overrides it.
pub const ONCE_TTL_DEFAULT: Duration = Duration::from_secs(24 * 60 * 60);

/// How long a first call may hold its key before a second call with the same
/// key is let through as if the first had died (a crashed process, a dropped
/// future). Longer than the task scheduler's per-attempt timeout.
pub const ONCE_IN_FLIGHT_LIMIT: Duration = Duration::from_secs(30 * 60);

/// The codes a stored `once` answer never holds: the call may succeed if it
/// is simply tried again, so the key is released instead.
pub const ONCE_RETRYABLE: &[&str] = &[
    codes::CONFLICT,
    "undo-conflict",
    codes::STORE_UNAVAILABLE,
    codes::HOST_UNAVAILABLE,
    codes::TIMED_OUT,
    codes::IDEMPOTENCY_IN_FLIGHT,
    codes::IDEMPOTENCY_MISMATCH,
];

/// How long a `once` answer is kept: `IMPRESS_ONCE_TTL` seconds, else
/// [`ONCE_TTL_DEFAULT`].
pub fn once_ttl() -> Duration {
    std::env::var("IMPRESS_ONCE_TTL")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(ONCE_TTL_DEFAULT)
}

/// What a `once` answer is keyed by: who called, which verb, and the key
/// (plan-koine-trust TR-D1). The verb is part of it, so one key used on two
/// verbs never replays the wrong verb's answer.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OnceKey {
    pub caller: String,
    pub verb: String,
    pub key: String,
}

impl OnceKey {
    pub fn new(caller: impl Into<String>, verb: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            caller: caller.into(),
            verb: verb.into(),
            key: key.into(),
        }
    }
}

/// A stored `once` answer: the arguments' fingerprint it answered, when, and
/// the envelope (`None` while the first call is still running — only the
/// in-process table holds that).
#[derive(Debug, Clone, PartialEq)]
pub struct OnceEntry {
    pub fingerprint: String,
    /// Milliseconds since the Unix epoch.
    pub at_ms: i64,
    pub answer: Option<Value>,
}

impl OnceEntry {
    fn age(&self, now_ms: i64) -> Duration {
        Duration::from_millis(now_ms.saturating_sub(self.at_ms).max(0) as u64)
    }
}

/// The fingerprint `once` compares a repeated call's arguments by: sha256 of
/// the arguments as canonical JSON (object keys sorted at every level), the
/// `adverbs` key already removed.
pub fn fingerprint(args: &Value) -> String {
    use sha2::{Digest, Sha256};
    fn canonical(value: &Value, out: &mut String) {
        match value {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (i, key) in keys.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String(key.clone()).to_string());
                    out.push(':');
                    canonical(&map[key], out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    canonical(item, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut text = String::new();
    canonical(args, &mut text);
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// A durable home for `once` answers, installed by a host that wants them to
/// outlive the process. It holds finished answers only; which call is still
/// running is the in-process table's to know.
pub trait IdempotencyStore: Send + Sync {
    /// The answer stored for `key`, when there is one (expired or not: the
    /// pipeline judges age).
    fn get(&self, key: &OnceKey) -> Option<OnceEntry>;
    /// Remember `entry` (its `answer` is always `Some`) for `key`.
    fn put(&self, key: &OnceKey, entry: &OnceEntry);
    /// Forget `key` (an expired answer). The default keeps it.
    fn remove(&self, key: &OnceKey) {
        let _ = key;
    }
}

/// Starts a call as a job (`in_background`). The pipeline has already checked
/// support, reachability and policy; the call handed over has `in_background`
/// (and `once`, which the pipeline has handled) cleared, and runs through the
/// pipeline again when the runner executes it.
pub trait BackgroundRunner: Send + Sync {
    fn start(&self, verb: VerbHandle, call: Call) -> Result<JobHandle, Refusal>;

    /// Starts a call that repeats (`every`): run it, wait `interval`, run it
    /// again, until the job is cancelled. The call handed over has `every`
    /// and `in_background` cleared, like [`Self::start`]'s. A runner that
    /// cannot repeat answers the default refusal.
    fn repeat(
        &self,
        verb: VerbHandle,
        call: Call,
        interval: Duration,
    ) -> Result<JobHandle, Refusal> {
        let _ = (verb, call, interval);
        Err(Refusal::new(
            codes::UNSUPPORTED_ADVERB,
            "this runner cannot repeat",
        ))
    }
}

/// The overlay a rehearsed dry run runs its handler against
/// (plan-koine-trust TR-D3): the value the call's store override becomes, and
/// what the verb would have changed.
pub trait RehearsalStore: Send + Sync {
    /// The overlay as the call's store override (`Call::store`).
    fn erased(&self) -> Arc<dyn std::any::Any + Send + Sync>;
    /// What the verb would have changed.
    fn diff(&self) -> Value;
}

/// Makes the overlay for a rehearsal, over the call's own store override
/// when it has one, else the process store. `None` when it cannot (no store,
/// a backend it cannot read through), and the dry run answers statically.
pub trait RehearsalProvider: Send + Sync {
    fn begin(
        &self,
        call_store: Option<Arc<dyn std::any::Any + Send + Sync>>,
    ) -> Option<Arc<dyn RehearsalStore>>;
}

static REHEARSAL: RwLock<Option<Arc<dyn RehearsalProvider>>> = RwLock::new(None);

/// Install (or replace) how this process rehearses dry runs.
pub fn install_rehearsal_provider(provider: Arc<dyn RehearsalProvider>) {
    if let Ok(mut slot) = REHEARSAL.write() {
        *slot = Some(provider);
    }
}

pub(super) fn rehearsal_provider() -> Option<Arc<dyn RehearsalProvider>> {
    REHEARSAL.read().ok().and_then(|r| r.clone())
}

static STORE: RwLock<Option<Arc<dyn IdempotencyStore>>> = RwLock::new(None);
static RUNNER: RwLock<Option<Arc<dyn BackgroundRunner>>> = RwLock::new(None);

/// Install (or replace) the process's durable `once` store.
pub fn install_idempotency_store(store: Arc<dyn IdempotencyStore>) {
    if let Ok(mut slot) = STORE.write() {
        *slot = Some(store);
    }
}

/// Install (or replace) the process's background runner.
pub fn install_background_runner(runner: Arc<dyn BackgroundRunner>) {
    if let Ok(mut slot) = RUNNER.write() {
        *slot = Some(runner);
    }
}

/// Whether a background runner is installed.
pub fn has_background_runner() -> bool {
    RUNNER.read().ok().is_some_and(|r| r.is_some())
}

pub(super) fn background_runner() -> Option<Arc<dyn BackgroundRunner>> {
    RUNNER.read().ok().and_then(|r| r.clone())
}

#[derive(Default)]
struct Inner {
    order: VecDeque<OnceKey>,
    map: HashMap<OnceKey, OnceEntry>,
}

/// A bounded `key → entry` table with FIFO eviction: the in-process home of
/// `once`, and the only one that knows which first calls are still running.
pub struct OnceTable {
    capacity: usize,
    inner: Mutex<Inner>,
}

impl OnceTable {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            inner: Mutex::new(Inner::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn get(&self, key: &OnceKey) -> Option<OnceEntry> {
        self.lock().map.get(key).cloned()
    }

    /// Store `entry`; an existing entry keeps its place in the eviction order.
    pub fn put(&self, key: &OnceKey, entry: OnceEntry) {
        let mut inner = self.lock();
        Self::put_locked(&mut inner, self.capacity, key, entry);
    }

    fn put_locked(inner: &mut Inner, capacity: usize, key: &OnceKey, entry: OnceEntry) {
        if inner.map.insert(key.clone(), entry).is_none() {
            inner.order.push_back(key.clone());
            while inner.order.len() > capacity {
                if let Some(oldest) = inner.order.pop_front() {
                    inner.map.remove(&oldest);
                }
            }
        }
    }

    pub fn remove(&self, key: &OnceKey) {
        let mut inner = self.lock();
        if inner.map.remove(key).is_some() {
            inner.order.retain(|k| k != key);
        }
    }

    /// Atomically: the live entry for `key`, or (when there is none, or it
    /// expired) a claim for `fingerprint` recorded as in flight.
    fn claim(
        &self,
        key: &OnceKey,
        fingerprint: &str,
        now_ms: i64,
        ttl: Duration,
    ) -> Option<OnceEntry> {
        let mut inner = self.lock();
        if let Some(entry) = inner.map.get(key) {
            let live = match entry.answer {
                Some(_) => entry.age(now_ms) <= ttl,
                None => entry.age(now_ms) <= ONCE_IN_FLIGHT_LIMIT,
            };
            if live {
                return Some(entry.clone());
            }
        }
        let entry = OnceEntry {
            fingerprint: fingerprint.to_owned(),
            at_ms: now_ms,
            answer: None,
        };
        Self::put_locked(&mut inner, self.capacity, key, entry);
        None
    }

    pub fn len(&self) -> usize {
        self.lock().map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

static TABLE: LazyLock<OnceTable> = LazyLock::new(|| OnceTable::new(ONCE_CAPACITY));

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// A `once` key this call holds: the handler may run, and its answer is
/// recorded (or the key released) by [`once_finish`].
#[derive(Debug, Clone)]
pub(super) struct OnceClaim {
    pub key: OnceKey,
    pub fingerprint: String,
}

/// What `once` says before the handler runs.
pub(super) enum OnceCheck {
    /// The stored answer, tagged `"replayed": true`.
    Replay(Value),
    /// This call holds the key; run it.
    Claimed(OnceClaim),
    /// The key is in flight or was used with other arguments.
    Refuse(Value),
}

fn refuse(code: &str, message: String) -> OnceCheck {
    OnceCheck::Refuse(crate::strict::refusal_value(&Refusal::new(code, message)))
}

/// Judge a stored `entry` for a call with `fingerprint`.
fn judge(key: &OnceKey, entry: OnceEntry, fingerprint: &str) -> OnceCheck {
    if entry.fingerprint != fingerprint {
        return refuse(
            codes::IDEMPOTENCY_MISMATCH,
            format!(
                "once key '{}' was already used for {} with different arguments; \
                 use a new key for a different request",
                key.key, key.verb
            ),
        );
    }
    match entry.answer {
        Some(mut answer) => {
            if let Some(object) = answer.as_object_mut() {
                object.insert("replayed".into(), Value::Bool(true));
            }
            OnceCheck::Replay(answer)
        }
        None => refuse(
            codes::IDEMPOTENCY_IN_FLIGHT,
            format!(
                "the first {} call with once key '{}' is still running; retry when it answers",
                key.verb, key.key
            ),
        ),
    }
}

/// Before the handler: replay, refuse, or claim `key` for `fingerprint`. The
/// installed store is consulted first for a finished answer; the in-process
/// table decides, atomically, which call runs.
pub(super) fn once_begin(key: OnceKey, fingerprint: String) -> OnceCheck {
    let now = now_ms();
    let ttl = once_ttl();
    if let Some(store) = STORE.read().ok().and_then(|s| s.clone()) {
        if let Some(entry) = store.get(&key) {
            if entry.answer.is_some() && entry.age(now) <= ttl {
                return judge(&key, entry, &fingerprint);
            }
            store.remove(&key);
        }
    }
    match TABLE.claim(&key, &fingerprint, now, ttl) {
        Some(entry) => judge(&key, entry, &fingerprint),
        None => OnceCheck::Claimed(OnceClaim { key, fingerprint }),
    }
}

/// Whether `answer` is kept as the key's answer: a success, or a failure that
/// trying again would not change.
pub fn once_keeps(answer: &Value) -> bool {
    let ok = answer.get("ok").and_then(Value::as_bool).unwrap_or(true);
    if ok {
        return true;
    }
    let code = answer.get("code").and_then(Value::as_str).unwrap_or("");
    !ONCE_RETRYABLE.contains(&code)
}

/// After the handler: record `answer` under the claim, or release the key so
/// a retry runs (`None`: the handler failed outright; a retryable code).
pub(super) fn once_finish(claim: &OnceClaim, answer: Option<&Value>) {
    match answer {
        Some(answer) if once_keeps(answer) => {
            let entry = OnceEntry {
                fingerprint: claim.fingerprint.clone(),
                at_ms: now_ms(),
                answer: Some(answer.clone()),
            };
            TABLE.put(&claim.key, entry.clone());
            if let Some(store) = STORE.read().ok().and_then(|s| s.clone()) {
                store.put(&claim.key, &entry);
            }
        }
        _ => TABLE.remove(&claim.key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn k(caller: &str, key: &str) -> OnceKey {
        OnceKey::new(caller, "v", key)
    }
    fn done(n: i64) -> OnceEntry {
        OnceEntry {
            fingerprint: "f".into(),
            at_ms: 0,
            answer: Some(json!(n)),
        }
    }

    #[test]
    fn the_table_is_keyed_and_evicts_the_oldest() {
        let t = OnceTable::new(2);
        t.put(&k("a", "k"), done(1));
        t.put(&k("b", "k"), done(2));
        assert_eq!(t.get(&k("a", "k")).unwrap().answer, Some(json!(1)));
        t.put(&k("a", "k"), done(3));
        assert_eq!(t.len(), 2, "overwrite does not grow the table");
        t.put(&k("c", "k"), done(4));
        assert_eq!(t.get(&k("a", "k")), None, "the first in is the first out");
        assert!(t.get(&k("b", "k")).is_some());
        assert!(t.get(&k("c", "k")).is_some());
        assert!(!t.is_empty());
        t.remove(&k("b", "k"));
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn the_process_table_is_bounded() {
        let t = OnceTable::new(ONCE_CAPACITY);
        for i in 0..ONCE_CAPACITY + 10 {
            t.put(&k("p", &i.to_string()), done(i as i64));
        }
        assert_eq!(t.len(), ONCE_CAPACITY);
        assert_eq!(t.get(&k("p", "0")), None);
    }

    #[test]
    fn a_claim_is_atomic_and_an_expired_entry_is_claimed_again() {
        let t = OnceTable::new(8);
        let ttl = Duration::from_secs(10);
        assert!(t.claim(&k("a", "k"), "f", 1_000, ttl).is_none(), "claimed");
        let held = t.claim(&k("a", "k"), "f", 2_000, ttl).unwrap();
        assert_eq!(held.answer, None, "in flight");
        let stale = 1_000 + ONCE_IN_FLIGHT_LIMIT.as_millis() as i64 + 1;
        assert!(
            t.claim(&k("a", "k"), "f", stale, ttl).is_none(),
            "a dead claim is taken over"
        );
        t.put(
            &k("a", "k"),
            OnceEntry {
                fingerprint: "f".into(),
                at_ms: 0,
                answer: Some(json!(1)),
            },
        );
        assert!(
            t.claim(&k("a", "k"), "f", 5_000, ttl).is_some(),
            "live answer"
        );
        assert!(
            t.claim(&k("a", "k"), "f", 20_000, ttl).is_none(),
            "expired answer"
        );
    }

    #[test]
    fn the_fingerprint_ignores_key_order() {
        assert_eq!(
            fingerprint(&json!({"a": 1, "b": {"y": 2, "x": [1, {"q": 0, "p": 1}]}})),
            fingerprint(&json!({"b": {"x": [1, {"p": 1, "q": 0}], "y": 2}, "a": 1}))
        );
        assert_ne!(fingerprint(&json!({"a": 1})), fingerprint(&json!({"a": 2})));
        assert_ne!(fingerprint(&json!([1, 2])), fingerprint(&json!([2, 1])));
    }

    #[test]
    fn retryable_failures_are_not_kept() {
        assert!(once_keeps(&json!({"ok": true})));
        assert!(once_keeps(&json!({"ok": false, "code": "not-found"})));
        assert!(!once_keeps(
            &json!({"ok": false, "code": "store-unavailable"})
        ));
        assert!(!once_keeps(&json!({"ok": false, "code": "timed-out"})));
    }
}
