//! The desk's records: `koine/agent-request@1.0.0` items
//! (plan-koine-research-os G-D).
//!
//! A request moves `open` → `claimed` → `answered`, or to `declined` from
//! `open` or `claimed`. Any caller may move it; the record says who did.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::Utc;
use impress_item::{
    ActorKind, AttributedStore, FieldMutation, Item, ItemId, ItemQuery, Priority, SchemaRef,
    SortDescriptor, Value as ItemValue, Visibility,
};
use serde_json::{json, Map, Value};

use crate::dto::RequestRow;

/// A request on the desk.
pub const REQUEST: &str = "koine/agent-request@1.0.0";

/// The kinds of request.
pub const ASK: &str = "ask";
pub const DIAGNOSE: &str = "diagnose";
pub const MAKE_SURFACE: &str = "make-surface";

/// Why a desk operation did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeskError {
    pub code: &'static str,
    pub message: String,
}

impl DeskError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn store(e: impl std::fmt::Display) -> Self {
        Self::new("store-error", e.to_string())
    }
}

/// Whether `id` has the shape of a task-category id: lowercase letters,
/// digits, `.` and `-`, starting with a letter (`agent.frontier`). Whether
/// the category exists is the desk agent's to say: this crate does not know
/// the model registry.
pub fn is_category_id(id: &str) -> bool {
    id.starts_with(|c: char| c.is_ascii_lowercase())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
}

/// Largest serialized response schema a request may carry.
pub const MAX_RESPONSE_SCHEMA_BYTES: usize = 32 * 1024;

/// A response schema must be a JSON object schema (`"type": "object"`) of at
/// most [`MAX_RESPONSE_SCHEMA_BYTES`]. Whether a provider can express it is
/// the desk agent's to say.
pub fn check_response_schema(schema: &Value) -> Result<(), DeskError> {
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err(DeskError::new(
            "invalid-argument",
            "response_schema must be a JSON object schema (\"type\": \"object\")",
        ));
    }
    if schema.to_string().len() > MAX_RESPONSE_SCHEMA_BYTES {
        return Err(DeskError::new(
            "invalid-argument",
            format!("response_schema is larger than {MAX_RESPONSE_SCHEMA_BYTES} bytes"),
        ));
    }
    Ok(())
}

fn schema() -> SchemaRef {
    SchemaRef::from_stored(REQUEST.to_owned())
}

/// The store the desk uses: the call's or the process's; a write to a
/// stand-in is refused.
pub fn store(for_write: bool) -> Result<Arc<dyn AttributedStore>, DeskError> {
    let (store, fallback) = koine_store::process::acquire_or_stand_in();
    if for_write && fallback {
        return Err(DeskError::new(
            "store-unavailable",
            "no store is installed in this process; the desk needs a host (nothing was written)",
        ));
    }
    Ok(store)
}

fn to_item_value(value: Value) -> ItemValue {
    serde_json::from_value(value).unwrap_or(ItemValue::Null)
}

fn payload_json(item: &Item) -> Map<String, Value> {
    item.payload
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::to_value(v).unwrap_or(Value::Null)))
        .collect()
}

fn text(p: &Map<String, Value>, field: &str) -> Option<String> {
    p.get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A request item as a row; `with_context` keeps its context.
pub fn row_of(item: &Item, with_context: bool) -> RequestRow {
    let p = payload_json(item);
    RequestRow {
        id: item.id.to_string(),
        kind: text(&p, "kind").unwrap_or_else(|| ASK.into()),
        prompt: text(&p, "prompt").unwrap_or_default(),
        status: text(&p, "status").unwrap_or_else(|| "open".into()),
        created_by: item.author.clone(),
        created_at: item
            .created
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        claimed_by: text(&p, "claimed_by"),
        answer: text(&p, "answer"),
        surface_id: text(&p, "surface_id"),
        call_id: text(&p, "call_id"),
        category: text(&p, "category"),
        routed_category: text(&p, "routed_category"),
        triage_model: text(&p, "triage_model"),
        response_schema: p.get("response_schema").filter(|v| v.is_object()).cloned(),
        answer_json: p.get("answer_json").filter(|v| !v.is_null()).cloned(),
        releases: p.get("releases").and_then(Value::as_u64).unwrap_or(0),
        release_reason: text(&p, "release_reason"),
        context: if with_context {
            p.get("context").cloned().unwrap_or(Value::Null)
        } else {
            Value::Null
        },
    }
}

/// File a request.
#[allow(clippy::too_many_arguments)]
pub fn file(
    kind: &str,
    prompt: &str,
    context: Value,
    call_id: Option<&str>,
    category: Option<&str>,
    response_schema: Option<Value>,
    author: String,
    author_kind: ActorKind,
) -> Result<RequestRow, DeskError> {
    let store = store(true)?;
    let now = Utc::now();
    let mut payload = BTreeMap::new();
    payload.insert("kind".to_string(), ItemValue::String(kind.into()));
    payload.insert("prompt".to_string(), ItemValue::String(prompt.into()));
    payload.insert("status".to_string(), ItemValue::String("open".into()));
    payload.insert("context".to_string(), to_item_value(context));
    if let Some(call_id) = call_id {
        payload.insert("call_id".to_string(), ItemValue::String(call_id.into()));
    }
    if let Some(category) = category {
        payload.insert("category".to_string(), ItemValue::String(category.into()));
    }
    if let Some(schema) = response_schema {
        payload.insert("response_schema".to_string(), to_item_value(schema));
    }
    let item = Item {
        id: ItemId::new_v4(),
        schema: schema(),
        payload,
        created: now,
        modified: now,
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
        parent: None,
    };
    store.insert(item.clone()).map_err(DeskError::store)?;
    Ok(row_of(&item, true))
}

/// Every request, newest first, narrowed to a status.
pub fn list(status: Option<&str>) -> Result<Vec<Item>, DeskError> {
    let store = store(false)?;
    let items = store
        .query(&ItemQuery {
            schema: Some(schema()),
            sort: vec![SortDescriptor {
                field: "created".into(),
                ascending: false,
            }],
            include_tags: false,
            include_references: false,
            ..Default::default()
        })
        .map_err(DeskError::store)?;
    Ok(items
        .into_iter()
        .filter(|item| match status {
            Some(status) => row_of(item, false).status == status,
            None => true,
        })
        .collect())
}

/// One request.
pub fn get(id: &str) -> Result<Item, DeskError> {
    let store = store(false)?;
    let parsed: ItemId = id
        .trim()
        .parse()
        .map_err(|_| DeskError::new("invalid-argument", format!("{id} is not a request id")))?;
    match store.get(parsed).map_err(DeskError::store)? {
        Some(item) if item.schema.as_str() == REQUEST => Ok(item),
        _ => Err(DeskError::new("not-found", format!("no request {id}"))),
    }
}

/// Move a request from one of `from` to `to`, setting `fields`.
pub fn transition(
    id: &str,
    from: &[&str],
    to: &str,
    fields: Vec<(&str, Value)>,
) -> Result<RequestRow, DeskError> {
    let item = get(id)?;
    let current = row_of(&item, false).status;
    if !from.contains(&current.as_str()) {
        return Err(DeskError::new(
            "conflict",
            format!(
                "request {id} is {current}; it can be made {to} only from {}",
                from.join(" or ")
            ),
        ));
    }
    let store = store(true)?;
    let mut mutations = vec![FieldMutation::SetPayload(
        "status".into(),
        ItemValue::String(to.into()),
    )];
    for (field, value) in fields {
        mutations.push(FieldMutation::SetPayload(
            field.into(),
            to_item_value(value),
        ));
    }
    mutations.push(FieldMutation::SetPayload(
        format!("{to}_at"),
        ItemValue::String(Utc::now().to_rfc3339()),
    ));
    store.update(item.id, mutations).map_err(DeskError::store)?;
    let updated = get(id)?;
    Ok(row_of(&updated, true))
}

/// Give a claimed request back to the desk: `claimed` becomes `open`
/// again, with no claimant, `releases` one higher and `reason` kept as
/// `release_reason`. Only the agent that holds the claim may give it back.
pub fn release(id: &str, by: &str, reason: &str) -> Result<RequestRow, DeskError> {
    let item = get(id)?;
    let row = row_of(&item, false);
    if row.status != "claimed" {
        return Err(DeskError::new(
            "conflict",
            format!(
                "request {id} is {}; only a claimed request can be released",
                row.status
            ),
        ));
    }
    if row.claimed_by.as_deref() != Some(by) {
        return Err(DeskError::new(
            "conflict",
            format!(
                "request {id} is claimed by {}, not {by}",
                row.claimed_by.as_deref().unwrap_or("nobody")
            ),
        ));
    }
    let store = store(true)?;
    store
        .update(
            item.id,
            vec![
                FieldMutation::SetPayload("status".into(), ItemValue::String("open".into())),
                FieldMutation::RemovePayload("claimed_by".into()),
                FieldMutation::SetPayload(
                    "releases".into(),
                    ItemValue::Int(row.releases as i64 + 1),
                ),
                FieldMutation::SetPayload(
                    "release_reason".into(),
                    ItemValue::String(reason.trim().into()),
                ),
                FieldMutation::SetPayload(
                    "released_at".into(),
                    ItemValue::String(Utc::now().to_rfc3339()),
                ),
            ],
        )
        .map_err(DeskError::store)?;
    Ok(row_of(&get(id)?, true))
}

/// The oldest open request, if any.
pub fn oldest_open() -> Result<Option<RequestRow>, DeskError> {
    Ok(list(Some("open"))?.last().map(|item| row_of(item, true)))
}

/// `{"kind": …}` helper for contexts.
pub fn tagged(kind: &str, body: Value) -> Value {
    let mut object = Map::new();
    object.insert("kind".into(), json!(kind));
    if let Value::Object(body) = body {
        object.extend(body);
    }
    Value::Object(object)
}
