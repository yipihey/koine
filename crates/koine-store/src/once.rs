//! The durable home of `once` answers (plan-koine-trust TR-D1): one item of
//! [`ONCE_SCHEMA`] per `(caller, verb, key)`, so a retried call is answered
//! from the store after a restart, not run again.
//!
//! The item's id is a v5 uuid of the key, so a lookup is one `get`. The key
//! itself is not stored, only its sha256: it may be something a caller would
//! not want on the record. The answer is stored whole — it is the envelope the
//! first call answered. A host keeps these rows out of device sync (they are
//! this process's memory, not the person's data) and installs the store with
//! [`install`].

use std::collections::BTreeMap;
use std::sync::Arc;

use impress_item::{
    ActorKind, AttributedStore, FieldMutation, Item, ItemId, Priority, SchemaRef,
    Value as ItemValue, Visibility,
};
use impress_service_core::pipeline::adverbs::{
    install_idempotency_store, IdempotencyStore, OnceEntry, OnceKey,
};
use serde_json::Value;

/// The kind of a stored `once` answer.
pub const ONCE_SCHEMA: &str = "koine/once@1.0.0";

/// The author these rows are written as.
pub const AUTHOR: &str = "system:once";

/// `once` answers kept as items of a store.
pub struct StoreOnce {
    store: Arc<dyn AttributedStore>,
}

impl StoreOnce {
    pub fn new(store: Arc<dyn AttributedStore>) -> Self {
        Self { store }
    }

    /// The item id `key`'s answer lives at.
    pub fn item_id(key: &OnceKey) -> ItemId {
        let namespace = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"koine-once");
        let name = format!("{}\u{0}{}\u{0}{}", key.caller, key.verb, key.key);
        uuid::Uuid::new_v5(&namespace, name.as_bytes())
    }

    fn payload(key: &OnceKey, entry: &OnceEntry) -> BTreeMap<String, ItemValue> {
        use sha2::{Digest, Sha256};
        let key_sha: String = Sha256::digest(key.key.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let json = serde_json::json!({
            "caller": key.caller,
            "verb": key.verb,
            "key_sha256": key_sha,
            "fingerprint": entry.fingerprint,
            "at_ms": entry.at_ms,
            "answer": entry.answer.clone().unwrap_or(Value::Null),
        });
        serde_json::from_value::<BTreeMap<String, ItemValue>>(json).unwrap_or_default()
    }
}

impl IdempotencyStore for StoreOnce {
    fn get(&self, key: &OnceKey) -> Option<OnceEntry> {
        let item = self.store.get(Self::item_id(key)).ok().flatten()?;
        if item.schema.as_str() != ONCE_SCHEMA {
            return None;
        }
        let field = |name: &str| {
            item.payload
                .get(name)
                .and_then(|v| serde_json::to_value(v).ok())
        };
        // A row for another key that hashed to the same id is not this one.
        if field("caller")?.as_str()? != key.caller || field("verb")?.as_str()? != key.verb {
            return None;
        }
        let answer = field("answer").filter(|v| !v.is_null());
        Some(OnceEntry {
            fingerprint: field("fingerprint")?.as_str()?.to_owned(),
            at_ms: field("at_ms")?.as_i64()?,
            answer,
        })
    }

    fn put(&self, key: &OnceKey, entry: &OnceEntry) {
        let id = Self::item_id(key);
        let payload = Self::payload(key, entry);
        let exists = matches!(self.store.get(id), Ok(Some(_)));
        let result = if exists {
            self.store.update(
                id,
                payload
                    .into_iter()
                    .map(|(k, v)| FieldMutation::SetPayload(k, v))
                    .collect(),
            )
        } else {
            let now = chrono::Utc::now();
            self.store
                .insert(Item {
                    id,
                    schema: SchemaRef::from_stored(ONCE_SCHEMA.to_owned()),
                    payload,
                    created: now,
                    modified: now,
                    author: AUTHOR.to_owned(),
                    author_kind: ActorKind::System,
                    logical_clock: 0,
                    origin: None,
                    canonical_id: None,
                    tags: Vec::new(),
                    flag: None,
                    is_read: false,
                    is_starred: false,
                    priority: Priority::Normal,
                    visibility: Visibility::Private,
                    message_type: None,
                    produced_by: None,
                    version: None,
                    batch_id: None,
                    references: Vec::new(),
                    parent: None,
                    extra: Default::default(),
                })
                .map(|_| ())
        };
        if let Err(error) = result {
            tracing::warn!(verb = %key.verb, %error, "once: could not store the answer");
        }
    }

    fn remove(&self, key: &OnceKey) {
        let _ = self.store.delete(Self::item_id(key));
    }
}

/// Install a [`StoreOnce`] over `store` as the process's durable `once` home.
pub fn install(store: Arc<dyn AttributedStore>) {
    install_idempotency_store(Arc::new(StoreOnce::new(store)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_answer_round_trips_and_is_found_by_its_key_only() {
        let store: Arc<dyn AttributedStore> = Arc::new(koine_store_memory::MemoryStore::new());
        let once = StoreOnce::new(store.clone());
        let key = OnceKey::new("agent:a", "v_x", "k1");
        assert!(once.get(&key).is_none());
        let entry = OnceEntry {
            fingerprint: "f".into(),
            at_ms: 42,
            answer: Some(json!({"ok": true, "n": 1})),
        };
        once.put(&key, &entry);
        assert_eq!(once.get(&key), Some(entry.clone()));
        let newer = OnceEntry {
            at_ms: 43,
            ..entry.clone()
        };
        once.put(&key, &newer);
        assert_eq!(once.get(&key).unwrap().at_ms, 43, "overwritten in place");
        assert!(once.get(&OnceKey::new("agent:a", "v_y", "k1")).is_none());
        assert!(once.get(&OnceKey::new("agent:b", "v_x", "k1")).is_none());
        let row = store.get(StoreOnce::item_id(&key)).unwrap().unwrap();
        let text = serde_json::to_string(&row.payload).unwrap();
        assert!(!text.contains("\"k1\""), "the key is never stored: {text}");
        once.remove(&key);
        assert!(once.get(&key).is_none());
    }
}
