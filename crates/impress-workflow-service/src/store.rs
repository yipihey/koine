//! The store side of a workflow: one `impress/workflow@1.0.0` row per
//! document, flattened at the top level exactly as
//! `impress-store-service::history_service::save_macro` already writes one
//! (`wire_version`, `name`, `description`, `state`, `author`, `trigger`,
//! `guards`, `params`, `sources`, `steps`, `review`) — this module follows
//! that shape rather than introducing a second one.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use impress_item::schema::refs::IMPRESS_WORKFLOW as WORKFLOW_SCHEMA;
use impress_item::Value as ItemValue;
use impress_item::{
    ActorKind, AttributedStore, FieldMutation, Item, ItemId, ItemQuery, Priority, SortDescriptor,
    StoreError, Visibility,
};
use impress_workflow::WorkflowSpec;
use serde_json::Value;

/// One stored workflow, read back from its row.
#[derive(Debug, Clone)]
pub struct WorkflowRow {
    pub id: ItemId,
    pub spec: WorkflowSpec,
    pub created: DateTime<Utc>,
    pub modified: DateTime<Utc>,
}

fn serde_json_to_item_value(value: &Value) -> ItemValue {
    serde_json::from_value(value.clone()).unwrap_or(ItemValue::Null)
}

fn payload_json(item: &Item) -> Value {
    let map: serde_json::Map<String, Value> = item
        .payload
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::to_value(v).unwrap_or(Value::Null)))
        .collect();
    Value::Object(map)
}

/// Parses a row's flattened payload back into a [`WorkflowSpec`]. Kept as
/// its own function (rather than a `TryFrom`) so it can name which field
/// failed — a workflow row a future build cannot parse is `state: broken`
/// territory (plan § Workflows, "Fails loudly"), not a panic.
pub fn spec_of(item: &Item) -> Result<WorkflowSpec, String> {
    serde_json::from_value(payload_json(item))
        .map_err(|e| format!("row {} does not parse as a WorkflowSpec: {e}", item.id))
}

fn row_of(item: &Item) -> Result<WorkflowRow, String> {
    Ok(WorkflowRow {
        id: item.id,
        spec: spec_of(item)?,
        created: item.created,
        modified: item.modified,
    })
}

/// Inserts a new workflow row, author-attributed exactly as `save_macro`
/// attributes a saved macro.
pub fn insert(
    store: &dyn AttributedStore,
    spec: &WorkflowSpec,
    actor: ActorKind,
) -> Result<WorkflowRow, StoreError> {
    let now = Utc::now();
    let doc = serde_json::to_value(spec).unwrap_or(Value::Null);
    let mut payload: BTreeMap<String, ItemValue> = BTreeMap::new();
    if let Value::Object(fields) = doc {
        for (k, v) in fields {
            payload.insert(k, serde_json_to_item_value(&v));
        }
    }
    let id = uuid::Uuid::new_v4();
    let author = match actor {
        ActorKind::Human => "human",
        ActorKind::Agent => "agent",
        ActorKind::System => "system",
    };
    let inserted = store.insert(Item {
        id,
        schema: WORKFLOW_SCHEMA,
        payload,
        created: now,
        modified: now,
        author: author.into(),
        author_kind: actor,
        logical_clock: 0,
        origin: None,
        canonical_id: None,
        tags: vec![],
        flag: None,
        is_read: false,
        is_starred: false,
        priority: Priority::None,
        visibility: Visibility::Private,
        message_type: None,
        produced_by: None,
        version: None,
        batch_id: None,
        references: vec![],
        parent: None,
    })?;
    Ok(WorkflowRow {
        id: inserted,
        spec: spec.clone(),
        created: now,
        modified: now,
    })
}

pub fn get(store: &dyn AttributedStore, id: ItemId) -> Result<Option<WorkflowRow>, String> {
    match store.get(id) {
        Ok(Some(item)) if item.schema == WORKFLOW_SCHEMA => row_of(&item).map(Some),
        Ok(_) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

pub fn list(store: &dyn AttributedStore) -> Result<Vec<WorkflowRow>, String> {
    let items = store
        .query(&ItemQuery {
            schema: Some(WORKFLOW_SCHEMA),
            sort: vec![SortDescriptor {
                field: "created".into(),
                ascending: true,
            }],
            ..Default::default()
        })
        .map_err(|e| e.to_string())?;
    items.iter().map(row_of).collect()
}

/// Overwrites the row's `state` field (`enable`/`disable`).
pub fn set_state(store: &dyn AttributedStore, id: ItemId, state: &str) -> Result<(), String> {
    store
        .update(
            id,
            vec![FieldMutation::SetPayload(
                "state".into(),
                ItemValue::String(state.into()),
            )],
        )
        .map_err(|e| e.to_string())
}
