//! What a package carries besides surfaces and skills
//! (docs/plan-koine-ai-tasks.md P-D9): workflows and AI task categories.
//!
//! A workflow is an `impress/workflow@1.0.0` row, stored flat as the
//! workflow verbs store one. Installing validates it with
//! `impress-workflow` and stores it `proposed` whatever its file says: a
//! person enables it (the workflow review rule, D-R6), and the host's
//! workflow runner runs it. The row's id is derived from the package and the
//! workflow's name, so an upgrade finds it. Its baseline (the digest of what
//! was installed, without `state`) is kept on the package's record. An
//! upgrade replaces a row that is still as shipped, and proposes it again
//! when its content changed. It keeps a row the person edited, and leaves a
//! deleted one deleted. Removing deletes only rows still as shipped.
//!
//! A category is a `koine/ai-category@1.0.0` row (`koine_ai::categories`
//! lists them beside the built-in table); a package's categories are
//! replaced on upgrade and removed with it.

use std::collections::BTreeMap;

use chrono::Utc;
use impress_item::{
    ActorKind, AttributedStore, Item, ItemId, ItemQuery, Predicate, Priority, SchemaRef,
    Value as ItemValue, Visibility,
};
use impress_workflow::{WorkflowSpec, WorkflowState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::digest::sha256;
use crate::shipped::{self, ShippedError};

/// The stored workflow kind (schema-refs.json).
pub const WORKFLOW: SchemaRef = impress_item::schema::refs::IMPRESS_WORKFLOW;
/// An AI task category a package declares (read by `koine_ai::categories`).
pub const AI_CATEGORY: &str = "koine/ai-category@1.0.0";

pub use koine_package_format::{category_errors, CategoryEntry, WorkflowEntry};

fn fail(code: &'static str, message: impl Into<String>) -> ShippedError {
    ShippedError::new(code, message)
}

fn store_error(e: impress_item::StoreError) -> ShippedError {
    fail("store-error", e.to_string())
}

/// Parse a workflow file.
pub fn parse_workflow(file: &str, bytes: &[u8]) -> Result<WorkflowSpec, ShippedError> {
    serde_json::from_slice(bytes)
        .map_err(|e| fail("invalid-spec", format!("{file}: not a workflow: {e}")))
}

/// The workflow's errors (warnings are not refusals).
pub fn workflow_errors(spec: &WorkflowSpec) -> Vec<String> {
    impress_workflow::validate(spec)
        .iter()
        .filter(|p| p.severity == impress_surface::Severity::Error)
        .map(|p| format!("{}: {}", p.path, p.message))
        .collect()
}

/// The digest of a workflow's content: the spec with `state` left out, so
/// enabling it is not an edit.
pub fn workflow_digest(spec: &WorkflowSpec) -> String {
    let mut neutral = spec.clone();
    neutral.state = WorkflowState::Proposed;
    sha256(
        serde_json::to_string(&neutral)
            .expect("a workflow serializes")
            .as_bytes(),
    )
}

fn derived_id(kind: &str, package: &str, name: &str) -> ItemId {
    let namespace = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, kind.as_bytes());
    uuid::Uuid::new_v5(&namespace, format!("{package}/{name}").as_bytes())
}

/// The row id a package's workflow is stored under.
pub fn workflow_id(package: &str, name: &str) -> ItemId {
    derived_id("koine-package-workflow", package, name)
}

fn category_id(package: &str, id: &str) -> ItemId {
    derived_id("koine-package-ai-category", package, id)
}

fn to_fields(value: Value) -> BTreeMap<String, ItemValue> {
    match value {
        Value::Object(map) => map
            .into_iter()
            .map(|(k, v)| (k, serde_json::from_value(v).unwrap_or(ItemValue::Null)))
            .collect(),
        _ => BTreeMap::new(),
    }
}

fn row(id: ItemId, schema: SchemaRef, payload: BTreeMap<String, ItemValue>) -> Item {
    let now = Utc::now();
    Item {
        id,
        schema,
        payload,
        created: now,
        modified: now,
        author: shipped::AUTHOR.into(),
        author_kind: ActorKind::System,
        logical_clock: 0,
        origin: None,
        canonical_id: None,
        tags: Vec::new(),
        flag: None,
        is_read: false,
        is_starred: false,
        priority: Priority::None,
        visibility: Visibility::Private,
        message_type: None,
        produced_by: None,
        version: None,
        batch_id: None,
        references: Vec::new(),
        parent: None,
        extra: Default::default(),
    }
}

/// The workflow a stored row holds.
pub fn stored_workflow(item: &Item) -> Option<WorkflowSpec> {
    let map: serde_json::Map<String, Value> = item
        .payload
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::to_value(v).unwrap_or(Value::Null)))
        .collect();
    serde_json::from_value(Value::Object(map)).ok()
}

/// What installing one workflow did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowOutcome {
    pub name: String,
    pub workflow_id: String,
    /// `proposed` (new, or its content changed), `unchanged`, `kept yours`
    /// (the person edited it) or `left deleted`.
    pub outcome: String,
    /// The digest of what the package ships, the baseline next time.
    pub digest: String,
}

/// What `would` happen to a workflow, for the review.
pub fn would_workflow(
    store: &dyn AttributedStore,
    package: &str,
    name: &str,
    spec: &WorkflowSpec,
    baseline: Option<&str>,
) -> &'static str {
    let digest = workflow_digest(spec);
    match store.get(workflow_id(package, name)) {
        Ok(Some(item)) => {
            let current = stored_workflow(&item).map(|s| workflow_digest(&s));
            if current.as_deref() == Some(digest.as_str()) {
                "keep (unchanged)"
            } else if baseline.is_some() && current.as_deref() != baseline {
                "keep yours"
            } else {
                "propose the new one"
            }
        }
        _ if baseline.is_some() => "leave deleted",
        _ => "propose",
    }
}

/// Install `package`'s workflows. `baselines` are the digests the previous
/// install recorded, by workflow name.
pub fn install_workflows(
    store: &dyn AttributedStore,
    package: &str,
    workflows: &[(String, WorkflowSpec)],
    baselines: &BTreeMap<String, String>,
) -> Result<Vec<WorkflowOutcome>, ShippedError> {
    let mut outcomes = Vec::new();
    for (name, spec) in workflows {
        let id = workflow_id(package, name);
        let digest = workflow_digest(spec);
        let baseline = baselines.get(name);
        let existing = store.get(id).map_err(store_error)?;
        let outcome = match existing {
            Some(item) => {
                let current = stored_workflow(&item).map(|s| workflow_digest(&s));
                if current.as_deref() == Some(digest.as_str()) {
                    "unchanged"
                } else if baseline.is_some() && current.as_ref() != baseline {
                    "kept yours"
                } else {
                    store.delete(id).map_err(store_error)?;
                    insert_workflow(store, id, spec)?;
                    "proposed"
                }
            }
            None if baseline.is_some() => "left deleted",
            None => {
                insert_workflow(store, id, spec)?;
                "proposed"
            }
        };
        outcomes.push(WorkflowOutcome {
            name: name.clone(),
            workflow_id: id.to_string(),
            outcome: outcome.into(),
            digest,
        });
    }
    // A workflow the old version shipped and this one does not is removed
    // when it is still as shipped.
    for (name, baseline) in baselines {
        if workflows.iter().any(|(n, _)| n == name) {
            continue;
        }
        remove_workflow_if_pristine(store, package, name, baseline)?;
    }
    Ok(outcomes)
}

fn insert_workflow(
    store: &dyn AttributedStore,
    id: ItemId,
    spec: &WorkflowSpec,
) -> Result<(), ShippedError> {
    let mut proposed = spec.clone();
    proposed.state = WorkflowState::Proposed;
    let fields = to_fields(serde_json::to_value(&proposed).expect("a workflow serializes"));
    store
        .insert(row(id, WORKFLOW, fields))
        .map(|_| ())
        .map_err(store_error)
}

/// Delete the workflow when it is still as shipped (`baseline`); answers
/// `Some(true)` deleted, `Some(false)` kept, `None` already gone.
pub fn remove_workflow_if_pristine(
    store: &dyn AttributedStore,
    package: &str,
    name: &str,
    baseline: &str,
) -> Result<Option<bool>, ShippedError> {
    let id = workflow_id(package, name);
    let Some(item) = store.get(id).map_err(store_error)? else {
        return Ok(None);
    };
    let current = stored_workflow(&item).map(|s| workflow_digest(&s));
    if current.as_deref() == Some(baseline) {
        store.delete(id).map_err(store_error)?;
        Ok(Some(true))
    } else {
        Ok(Some(false))
    }
}

/// Replace `package`'s categories with `entries`.
pub fn install_categories(
    store: &dyn AttributedStore,
    package: &str,
    entries: &[CategoryEntry],
) -> Result<(), ShippedError> {
    remove_categories(store, package)?;
    for entry in entries {
        let mut payload = to_fields(serde_json::to_value(entry).expect("a category serializes"));
        payload.insert("package".into(), ItemValue::String(package.into()));
        store
            .insert(row(
                category_id(package, &entry.id),
                SchemaRef::from_stored(AI_CATEGORY.to_owned()),
                payload,
            ))
            .map_err(store_error)?;
    }
    Ok(())
}

/// Delete every category `package` declared.
pub fn remove_categories(store: &dyn AttributedStore, package: &str) -> Result<(), ShippedError> {
    let rows = store
        .query(&ItemQuery {
            schema: Some(SchemaRef::from_stored(AI_CATEGORY.to_owned())),
            predicates: vec![Predicate::Eq(
                "package".into(),
                ItemValue::String(package.into()),
            )],
            include_tags: false,
            include_references: false,
            ..Default::default()
        })
        .map_err(store_error)?;
    for item in rows {
        store.delete(item.id).map_err(store_error)?;
    }
    Ok(())
}

/// A review row for one workflow.
pub fn review_workflow(
    store: Option<&dyn AttributedStore>,
    package: &str,
    entry: &WorkflowEntry,
    bytes: &[u8],
    baseline: Option<&str>,
) -> (Value, bool, Vec<String>) {
    match parse_workflow(&entry.file, bytes) {
        Ok(spec) => {
            let errors = workflow_errors(&spec);
            let verbs: Vec<String> = crate::package::verbs_called(
                &serde_json::to_value(&spec).expect("a workflow serializes"),
            )
            .into_iter()
            .collect();
            let would = match store {
                Some(store) => would_workflow(store, package, &entry.name, &spec, baseline),
                None => "propose",
            };
            let valid = errors.is_empty();
            (
                json!({
                    "name": entry.name, "file": entry.file, "valid": valid,
                    "problems": errors, "verbs": verbs, "trigger": spec.trigger,
                    "would": if valid { would } else { "refuse" },
                }),
                valid,
                verbs,
            )
        }
        Err(e) => (
            json!({"name": entry.name, "file": entry.file, "valid": false,
                   "problems": [e.message], "verbs": [], "would": "refuse"}),
            false,
            Vec::new(),
        ),
    }
}
