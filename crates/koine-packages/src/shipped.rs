//! Shipped and yours (plan-koine-research-os B-D1..B-D3).
//!
//! A shipped surface is two records: the person's copy, an ordinary
//! `impress/ui/surface@1.0.0` row edited with the surface verbs like any
//! other, and its baseline, a `koine/shipped@1.0.0` item holding the spec
//! as shipped. The baseline is written only here. Every surface operation
//! goes through the surface verbs, so policy, the call log and the store
//! feed see it as they see a person's or an agent's edit.
//!
//! Specs are compared normalised ([`crate::digest::normalize_spec`]): the
//! surface store keeps a spec as its parsed form serialised again, so a file
//! and its stored copy are equal when they mean the same surface.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::Utc;
use impress_item::{
    ActorKind, AttributedStore, Item, ItemId, ItemQuery, Priority, SchemaRef, SortDescriptor,
    Value as ItemValue, Visibility,
};
use impress_service_core::call;
use serde_json::{json, Map, Value};

use crate::digest::{self, normalize_spec, spec_digest};

/// A baseline.
pub const SHIPPED: &str = "koine/shipped@1.0.0";
/// Who writes baselines.
pub const AUTHOR: &str = "system:packages";

const CREATE: &str = "impress-surface-service_surface-create";
const GET: &str = "impress-surface-service_surface-get";
const UPDATE: &str = "impress-surface-service_surface-update";
const DELETE: &str = "impress-surface-service_surface-delete";

/// The tag every shipped surface carries.
pub const TAG: &str = "shipped";

/// Why a shipped-surface operation did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShippedError {
    pub code: &'static str,
    pub message: String,
}

impl ShippedError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// One baseline, read.
#[derive(Debug, Clone, PartialEq)]
pub struct Baseline {
    pub id: ItemId,
    pub surface_id: String,
    pub package: String,
    pub name: String,
    pub version: String,
    /// The spec as shipped, normalised.
    pub spec: Value,
    /// Its digest.
    pub digest: String,
    /// A newer shipped spec held while the person's copy differs (B-D2).
    pub pending: Option<Value>,
    pub pending_version: Option<String>,
}

/// What installing one shipped surface did (B-D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// There was none: created.
    Created,
    /// The person's copy was the old shipped one: replaced by the new.
    Updated,
    /// The shipped spec has not changed.
    Unchanged,
    /// The person changed their copy: kept; the new spec is pending.
    Diverged,
    /// The person deleted their copy: left deleted; the baseline moves on.
    Missing,
}

impl Outcome {
    pub fn word(self) -> &'static str {
        match self {
            Outcome::Created => "created",
            Outcome::Updated => "updated",
            Outcome::Unchanged => "unchanged",
            Outcome::Diverged => "diverged",
            Outcome::Missing => "missing",
        }
    }
}

/// The status of one baseline against the person's copy (B-D3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pristine,
    Modified,
    Diverged,
    Missing,
}

impl Status {
    pub fn word(self) -> &'static str {
        match self {
            Status::Pristine => "pristine",
            Status::Modified => "modified",
            Status::Diverged => "diverged",
            Status::Missing => "missing",
        }
    }
}

fn schema() -> SchemaRef {
    SchemaRef::from_stored(SHIPPED.to_owned())
}

/// The baseline id of `package`'s surface `name`: the same on every install.
pub fn baseline_id(package: &str, name: &str) -> ItemId {
    let namespace = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"koine-shipped");
    uuid::Uuid::new_v5(&namespace, format!("{package}\u{0}{name}").as_bytes())
}

pub fn store(for_write: bool) -> Result<Arc<dyn AttributedStore>, ShippedError> {
    let (store, fallback) = koine_store::process::acquire_or_stand_in();
    if for_write && fallback {
        return Err(ShippedError::new(
            "store-unavailable",
            "no store is installed in this process; shipping a surface needs a host",
        ));
    }
    Ok(store)
}

fn payload(item: &Item) -> Map<String, Value> {
    item.payload
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::to_value(v).unwrap_or(Value::Null)))
        .collect()
}

fn spec_field(p: &Map<String, Value>, field: &str) -> Option<Value> {
    match p.get(field) {
        Some(Value::String(text)) => serde_json::from_str(text).ok(),
        _ => None,
    }
}

fn text(p: &Map<String, Value>, field: &str) -> Option<String> {
    p.get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

impl Baseline {
    pub fn of(item: &Item) -> Self {
        let p = payload(item);
        let spec = spec_field(&p, "spec").unwrap_or(Value::Null);
        Baseline {
            id: item.id,
            surface_id: text(&p, "surface_id").unwrap_or_default(),
            package: text(&p, "package").unwrap_or_default(),
            name: text(&p, "name").unwrap_or_default(),
            version: text(&p, "version").unwrap_or_default(),
            digest: text(&p, "digest").unwrap_or_else(|| spec_digest(&spec)),
            spec,
            pending: spec_field(&p, "pending"),
            pending_version: text(&p, "pending_version"),
        }
    }

    fn write(&self, store: &dyn AttributedStore, existed: bool) -> Result<(), ShippedError> {
        let mut fields: BTreeMap<String, ItemValue> = BTreeMap::new();
        fields.insert(
            "surface_id".into(),
            ItemValue::String(self.surface_id.clone()),
        );
        fields.insert("package".into(), ItemValue::String(self.package.clone()));
        fields.insert("name".into(), ItemValue::String(self.name.clone()));
        fields.insert("version".into(), ItemValue::String(self.version.clone()));
        // Specs are kept as JSON text, as the surface row keeps its own.
        fields.insert(
            "spec".into(),
            ItemValue::String(digest::canonical_text(&self.spec)),
        );
        fields.insert("digest".into(), ItemValue::String(self.digest.clone()));
        fields.insert(
            "pending".into(),
            match &self.pending {
                Some(spec) => ItemValue::String(digest::canonical_text(spec)),
                None => ItemValue::Null,
            },
        );
        fields.insert(
            "pending_version".into(),
            match &self.pending_version {
                Some(v) => ItemValue::String(v.clone()),
                None => ItemValue::Null,
            },
        );
        let fail = |e: impress_item::StoreError| ShippedError::new("store-error", e.to_string());
        if existed {
            store
                .update(
                    self.id,
                    fields
                        .into_iter()
                        .map(|(k, v)| impress_item::FieldMutation::SetPayload(k, v))
                        .collect(),
                )
                .map_err(fail)
        } else {
            let now = Utc::now();
            store
                .insert(Item {
                    id: self.id,
                    schema: schema(),
                    payload: fields,
                    created: now,
                    modified: now,
                    author: AUTHOR.into(),
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
                })
                .map(|_| ())
                .map_err(fail)
        }
    }
}

/// Every baseline, oldest first; `package` narrows to one package.
pub fn list(package: Option<&str>) -> Result<Vec<Baseline>, ShippedError> {
    let store = store(false)?;
    let items = store
        .query(&ItemQuery {
            schema: Some(schema()),
            sort: vec![SortDescriptor {
                field: "created".into(),
                ascending: true,
            }],
            include_tags: false,
            include_references: false,
            ..Default::default()
        })
        .map_err(|e| ShippedError::new("store-error", e.to_string()))?;
    Ok(items
        .iter()
        .map(Baseline::of)
        .filter(|b| package.is_none_or(|p| b.package == p))
        .collect())
}

/// The baseline of a surface, by the surface's id or the baseline's own.
pub fn find(id: &str) -> Result<Baseline, ShippedError> {
    let id = id.trim();
    list(None)?
        .into_iter()
        .find(|b| b.surface_id == id || b.id.to_string() == id)
        .ok_or_else(|| ShippedError::new("not-found", format!("{id} is not a shipped surface")))
}

async fn verb(name: &str, args: Value) -> Result<Value, ShippedError> {
    // A shipped surface is written by the package manager, and the call log
    // says so (not `agent:unknown`).
    let answer = call::call_async_as(
        name,
        impress_service_core::pipeline::CallerIdentity::system("koine-packages"),
        args,
    )
    .await
    .map_err(|e| ShippedError::new("unavailable", e.to_string()))?;
    Ok(answer)
}

fn refused(answer: &Value, what: &str) -> ShippedError {
    ShippedError::new(
        "effect-failed",
        format!(
            "{what}: {} ({})",
            answer["message"].as_str().unwrap_or("refused"),
            answer["code"].as_str().unwrap_or("?")
        ),
    )
}

/// The person's copy, normalised; `None` when it was deleted.
pub async fn current(surface_id: &str) -> Result<Option<Value>, ShippedError> {
    let answer = verb(GET, json!({ "id": surface_id })).await?;
    if answer["ok"] == json!(true) {
        Ok(answer.get("spec").map(normalize_spec))
    } else if answer["code"] == "not-found" {
        Ok(None)
    } else {
        Err(refused(&answer, "read the surface"))
    }
}

async fn create(name: &str, package: &str, spec: &Value) -> Result<String, ShippedError> {
    let answer = verb(
        CREATE,
        json!({ "spec": spec, "name": name, "tags": [TAG, format!("package:{package}")] }),
    )
    .await?;
    if answer["ok"] != json!(true) {
        return Err(refused(&answer, "create the surface"));
    }
    answer["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| ShippedError::new("effect-failed", "created, but no id came back"))
}

async fn update(surface_id: &str, spec: &Value) -> Result<(), ShippedError> {
    let answer = verb(UPDATE, json!({ "id": surface_id, "spec": spec })).await?;
    if answer["ok"] != json!(true) {
        return Err(refused(&answer, "update the surface"));
    }
    Ok(())
}

/// Delete the person's copy (`package_remove` of a pristine surface).
pub async fn delete(surface_id: &str) -> Result<(), ShippedError> {
    let answer = verb(DELETE, json!({ "id": surface_id })).await?;
    if answer["ok"] != json!(true) && answer["code"] != "not-found" {
        return Err(refused(&answer, "delete the surface"));
    }
    Ok(())
}

/// Install `spec` as `package`'s surface `name` at `version`, three-way
/// against the baseline and the person's copy (B-D2).
pub async fn install(
    package: &str,
    version: &str,
    name: &str,
    spec: &Value,
) -> Result<(Outcome, String), ShippedError> {
    let store = store(true)?;
    let new_spec = normalize_spec(spec);
    let new_digest = spec_digest(spec);
    let id = baseline_id(package, name);
    let existing = store
        .get(id)
        .map_err(|e| ShippedError::new("store-error", e.to_string()))?
        .map(|item| Baseline::of(&item));
    let Some(mut baseline) = existing else {
        let surface_id = create(name, package, spec).await?;
        let stored = current(&surface_id).await?.unwrap_or(new_spec);
        Baseline {
            id,
            surface_id: surface_id.clone(),
            package: package.into(),
            name: name.into(),
            version: version.into(),
            digest: new_digest,
            spec: stored,
            pending: None,
            pending_version: None,
        }
        .write(store.as_ref(), false)?;
        return Ok((Outcome::Created, surface_id));
    };
    let surface_id = baseline.surface_id.clone();
    if baseline.digest == new_digest {
        return Ok((Outcome::Unchanged, surface_id));
    }
    let outcome = match current(&surface_id).await? {
        None => {
            baseline.spec = new_spec;
            baseline.pending = None;
            baseline.pending_version = None;
            Outcome::Missing
        }
        Some(copy) if copy == baseline.spec => {
            update(&surface_id, spec).await?;
            baseline.spec = current(&surface_id).await?.unwrap_or(new_spec);
            baseline.pending = None;
            baseline.pending_version = None;
            Outcome::Updated
        }
        Some(_) => {
            baseline.pending = Some(new_spec);
            baseline.pending_version = Some(version.into());
            Outcome::Diverged
        }
    };
    if outcome != Outcome::Diverged {
        baseline.digest = new_digest;
        baseline.version = version.into();
    }
    baseline.write(store.as_ref(), true)?;
    Ok((outcome, surface_id))
}

/// The status of a baseline against the person's copy.
pub async fn status(baseline: &Baseline) -> Result<Status, ShippedError> {
    Ok(match current(&baseline.surface_id).await? {
        None => Status::Missing,
        Some(_) if baseline.pending.is_some() => Status::Diverged,
        Some(copy) if copy == baseline.spec => Status::Pristine,
        Some(_) => Status::Modified,
    })
}

/// The changes from the shipped spec to the person's copy.
pub async fn changes(baseline: &Baseline) -> Result<Vec<digest::Change>, ShippedError> {
    match current(&baseline.surface_id).await? {
        Some(copy) => Ok(digest::diff(&baseline.spec, &copy)),
        None => Err(ShippedError::new(
            "not-found",
            format!("the surface {} was deleted", baseline.surface_id),
        )),
    }
}

/// Put `spec` in place of the person's copy (recreating it when deleted)
/// and answer the spec it replaced, for the undo. `None` when the copy was
/// recreated (the undo is then a delete).
async fn replace_copy(
    baseline: &mut Baseline,
    spec: &Value,
) -> Result<Option<Value>, ShippedError> {
    match current(&baseline.surface_id).await? {
        Some(before) => {
            update(&baseline.surface_id, spec).await?;
            Ok(Some(before))
        }
        None => {
            baseline.surface_id = create(&baseline.name, &baseline.package, spec).await?;
            Ok(None)
        }
    }
}

fn note_undo(surface_id: &str, before: Option<Value>) {
    use impress_service_core::pipeline::context::note_inverse;
    match before {
        Some(spec) => note_inverse(UPDATE, json!({ "id": surface_id, "spec": spec })),
        None => note_inverse(DELETE, json!({ "id": surface_id })),
    }
}

/// Put the shipped spec back (B-D3). The inverse is the surface verb that
/// restores what it replaced.
pub async fn reset(id: &str) -> Result<Baseline, ShippedError> {
    let mut baseline = find(id)?;
    let shipped = baseline.spec.clone();
    let before = replace_copy(&mut baseline, &shipped).await?;
    note_undo(&baseline.surface_id, before);
    baseline.write(store(true)?.as_ref(), true)?;
    Ok(baseline)
}

/// Take the pending shipped spec in place of the person's copy (B-D2).
pub async fn take(id: &str) -> Result<Baseline, ShippedError> {
    let mut baseline = find(id)?;
    let Some(pending) = baseline.pending.clone() else {
        return Err(ShippedError::new(
            "conflict",
            format!("{id} has no newer shipped version waiting"),
        ));
    };
    let before = replace_copy(&mut baseline, &pending).await?;
    note_undo(&baseline.surface_id, before);
    baseline.spec = current(&baseline.surface_id)
        .await?
        .unwrap_or(pending.clone());
    baseline.digest = spec_digest(&pending);
    if let Some(version) = baseline.pending_version.take() {
        baseline.version = version;
    }
    baseline.pending = None;
    baseline.write(store(true)?.as_ref(), true)?;
    Ok(baseline)
}

/// Keep the person's copy over the pending shipped spec, which becomes the
/// baseline a reset restores (B-D2).
pub fn keep(id: &str) -> Result<Baseline, ShippedError> {
    let mut baseline = find(id)?;
    let Some(pending) = baseline.pending.take() else {
        return Err(ShippedError::new(
            "conflict",
            format!("{id} has no newer shipped version waiting"),
        ));
    };
    baseline.digest = spec_digest(&pending);
    baseline.spec = pending;
    if let Some(version) = baseline.pending_version.take() {
        baseline.version = version;
    }
    baseline.write(store(true)?.as_ref(), true)?;
    Ok(baseline)
}

/// Forget a baseline (the surface stays).
pub fn forget(baseline: &Baseline) -> Result<(), ShippedError> {
    store(true)?
        .delete(baseline.id)
        .map_err(|e| ShippedError::new("store-error", e.to_string()))
}
