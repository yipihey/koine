//! The host-facing half of the adverbs (docs/grammar.md § Adverbs): the
//! in-process `once` table, the [`IdempotencyStore`] a host may install so
//! `once` survives a restart, and the [`BackgroundRunner`] that `in_background`
//! hands a prepared call to. The pipeline itself stays store-free.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex, RwLock};

use serde_json::Value;

use crate::descriptor_handle::VerbHandle;
use crate::job::JobHandle;
use crate::refusal::Refusal;

use super::Call;

/// The most entries the in-process `once` table keeps; the oldest is evicted
/// first.
pub const ONCE_CAPACITY: usize = 1024;

/// A durable home for `once` results, installed by a host that wants them to
/// outlive the process. Consulted before the in-process table; a successful
/// result is written to both.
pub trait IdempotencyStore: Send + Sync {
    /// The envelope stored for `(caller, key)`, when there is one.
    fn get(&self, caller: &str, key: &str) -> Option<Value>;
    /// Remember `value` as the answer for `(caller, key)`.
    fn put(&self, caller: &str, key: &str, value: &Value);
}

/// Starts a call as a job (`in_background`). The pipeline has already checked
/// support, reachability and policy; the call handed over has `in_background`
/// (and `once`, which the pipeline has handled) cleared, and runs through the
/// pipeline again when the runner executes it.
pub trait BackgroundRunner: Send + Sync {
    fn start(&self, verb: VerbHandle, call: Call) -> Result<JobHandle, Refusal>;
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
    order: VecDeque<(String, String)>,
    map: HashMap<(String, String), Value>,
}

/// A bounded `(caller, key) → envelope` table with FIFO eviction.
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

    pub fn get(&self, caller: &str, key: &str) -> Option<Value> {
        self.lock()
            .map
            .get(&(caller.to_string(), key.to_string()))
            .cloned()
    }

    /// Store `value`; an existing entry keeps its place in the eviction order.
    pub fn put(&self, caller: &str, key: &str, value: &Value) {
        let k = (caller.to_string(), key.to_string());
        let mut inner = self.lock();
        if inner.map.insert(k.clone(), value.clone()).is_none() {
            inner.order.push_back(k);
            while inner.order.len() > self.capacity {
                if let Some(oldest) = inner.order.pop_front() {
                    inner.map.remove(&oldest);
                }
            }
        }
    }

    pub fn len(&self) -> usize {
        self.lock().map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

static TABLE: LazyLock<OnceTable> = LazyLock::new(|| OnceTable::new(ONCE_CAPACITY));

/// The stored envelope for `(caller, key)`: the installed store first, the
/// in-process table second.
pub(super) fn once_lookup(caller: &str, key: &str) -> Option<Value> {
    let store = STORE.read().ok().and_then(|s| s.clone());
    store
        .and_then(|s| s.get(caller, key))
        .or_else(|| TABLE.get(caller, key))
}

/// Record a successful envelope in both.
pub(super) fn once_record(caller: &str, key: &str, value: &Value) {
    TABLE.put(caller, key, value);
    if let Some(store) = STORE.read().ok().and_then(|s| s.clone()) {
        store.put(caller, key, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_table_is_keyed_by_caller_and_key_and_evicts_the_oldest() {
        let t = OnceTable::new(2);
        t.put("a", "k", &json!(1));
        t.put("b", "k", &json!(2));
        assert_eq!(t.get("a", "k"), Some(json!(1)));
        assert_eq!(t.get("b", "k"), Some(json!(2)));
        t.put("a", "k", &json!(3));
        assert_eq!(t.len(), 2, "overwrite does not grow the table");
        t.put("c", "k", &json!(4));
        assert_eq!(t.get("a", "k"), None, "the first in is the first out");
        assert_eq!(t.get("b", "k"), Some(json!(2)));
        assert_eq!(t.get("c", "k"), Some(json!(4)));
        assert!(!t.is_empty());
    }

    #[test]
    fn the_process_table_is_bounded() {
        let t = OnceTable::new(ONCE_CAPACITY);
        for i in 0..ONCE_CAPACITY + 10 {
            t.put("p", &i.to_string(), &json!(i));
        }
        assert_eq!(t.len(), ONCE_CAPACITY);
        assert_eq!(t.get("p", "0"), None);
        assert!(t.get("p", &(ONCE_CAPACITY + 9).to_string()).is_some());
    }
}
