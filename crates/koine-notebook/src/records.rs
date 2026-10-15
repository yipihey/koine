//! The record kinds the notebook reads and writes, and the item plumbing.
//!
//! | Schema | One per | Written by |
//! |---|---|---|
//! | `core/verb-call@1.0.0` | call | [`crate::sink::StoreSink`] |
//! | `koine/run@1.0.0` | run | `notebook-service_run-start` / `_run-stop` |
//! | `koine/notebook-note@1.0.0` | note on a call | `notebook-service_annotate` |

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use impress_item::{
    ActorKind, AttributedStore, Item, ItemId, ItemQuery, Predicate, Priority, SchemaRef,
    SortDescriptor, Visibility,
};
use impress_service_core::pipeline::CallerIdentity;
use serde_json::{Map, Value};

/// A call record (the pipeline's audit row).
pub const VERB_CALL: &str = impress_service_core::pipeline::audit::VERB_CALL_SCHEMA;
/// A run (plan-koine-notebook N-D5).
pub const RUN: &str = "koine/run@1.0.0";
/// A note on a call (N-D7).
pub const NOTE: &str = "koine/notebook-note@1.0.0";

/// The author the notebook writes runs as when no caller is known.
pub const AUTHOR: &str = "system:notebook";

pub fn schema(name: &str) -> SchemaRef {
    SchemaRef::from_stored(name.to_owned())
}

/// The item id of a call record: the call id itself when it is a uuid (the
/// pipeline's are), else a v5 uuid of it, so a record can always be found
/// from its call id.
pub fn record_id(call_id: &str) -> ItemId {
    call_id.parse().unwrap_or_else(|_| {
        let namespace = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"koine-verb-call");
        uuid::Uuid::new_v5(&namespace, call_id.as_bytes())
    })
}

/// `author_kind` for a caller.
pub fn actor_kind(caller: &CallerIdentity) -> ActorKind {
    match caller {
        CallerIdentity::Person => ActorKind::Human,
        CallerIdentity::Agent(_) => ActorKind::Agent,
        _ => ActorKind::System,
    }
}

/// A new item of `schema` with `payload` (a JSON object).
pub fn new_item(
    id: ItemId,
    schema_name: &str,
    payload: Map<String, Value>,
    author: String,
    author_kind: ActorKind,
    created: DateTime<Utc>,
    parent: Option<ItemId>,
) -> Item {
    let payload: BTreeMap<String, impress_item::Value> = payload
        .into_iter()
        .filter_map(|(k, v)| serde_json::from_value(v).ok().map(|v| (k, v)))
        .collect();
    Item {
        id,
        schema: schema(schema_name),
        payload,
        created,
        modified: created,
        author,
        author_kind,
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
        parent,
    }
}

/// An item's payload as one JSON object, with the envelope fields a reader
/// needs laid over it under `_id`, `_created`, `_author`, `_parent`.
pub fn payload_json(item: &Item) -> Map<String, Value> {
    let mut map: Map<String, Value> = item
        .payload
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::to_value(v).unwrap_or(Value::Null)))
        .collect();
    map.insert("_id".into(), Value::String(item.id.to_string()));
    map.insert("_created".into(), Value::String(item.created.to_rfc3339()));
    map.insert("_author".into(), Value::String(item.author.clone()));
    map.insert(
        "_parent".into(),
        item.parent
            .map(|p| Value::String(p.to_string()))
            .unwrap_or(Value::Null),
    );
    map
}

/// Every item of `schema_name`, sorted by `created` (newest first when
/// `newest_first`), optionally narrowed by payload equalities.
pub fn all_of(
    store: &dyn AttributedStore,
    schema_name: &str,
    equal: &[(&str, &str)],
    newest_first: bool,
) -> Result<Vec<Item>, String> {
    store
        .query(&ItemQuery {
            schema: Some(schema(schema_name)),
            predicates: equal
                .iter()
                .map(|(field, value)| {
                    Predicate::Eq(
                        (*field).to_owned(),
                        impress_item::Value::String((*value).to_owned()),
                    )
                })
                .collect(),
            sort: vec![SortDescriptor {
                field: "created".into(),
                ascending: !newest_first,
            }],
            include_tags: false,
            include_references: false,
            ..Default::default()
        })
        .map_err(|e| e.to_string())
}

/// The store a notebook verb reads or writes: the call's or the process's.
/// A write to a stand-in would vanish with the process, so it is refused.
pub fn store(for_write: bool) -> Result<Arc<dyn AttributedStore>, (String, String)> {
    let (store, fallback) = koine_store::process::acquire_or_stand_in();
    if for_write && fallback {
        return Err((
            "store-unavailable".into(),
            "no store is installed in this process; a notebook write needs a host \
             (nothing was written)"
                .into(),
        ));
    }
    Ok(store)
}

/// A string field of a payload.
pub fn text<'a>(payload: &'a Map<String, Value>, field: &str) -> Option<&'a str> {
    payload.get(field).and_then(Value::as_str)
}

/// A number field of a payload, as `u64`.
pub fn number(payload: &Map<String, Value>, field: &str) -> u64 {
    payload.get(field).and_then(Value::as_u64).unwrap_or(0)
}

/// The length of an array field.
pub fn len(payload: &Map<String, Value>, field: &str) -> u64 {
    payload
        .get(field)
        .and_then(Value::as_array)
        .map_or(0, |a| a.len() as u64)
}
