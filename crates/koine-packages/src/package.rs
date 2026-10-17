//! Packages (plan-koine-research-os P-D1..P-D3): a directory with a
//! `koine-package.json`, reviewed by its digest before it is installed.

use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use impress_item::{
    ActorKind, Item, ItemId, ItemQuery, Priority, SchemaRef, SortDescriptor, Value as ItemValue,
    Visibility,
};
use impress_service_core::{SafetyClass, VerbDescriptor};
use serde_json::{json, Value};

use crate::contents::{self, WorkflowEntry, WorkflowOutcome};
use crate::shipped::{self, Outcome, ShippedError};

pub use koine_package_format::{
    Manifest, Package, Requires, SkillEntry, SurfaceEntry, BUNDLE_FORMAT, BUNDLE_SUFFIX, FORMAT,
    MANIFEST, PACKAGE,
};

fn fail(code: &'static str, message: impl Into<String>) -> ShippedError {
    ShippedError::new(code, message)
}

/// One shipped workflow of `package`.
pub fn workflow_spec(
    package: &Package,
    entry: &WorkflowEntry,
) -> Result<impress_workflow::WorkflowSpec, ShippedError> {
    contents::parse_workflow(&entry.file, &package.files[&entry.file])
}

/// Every verb name a spec calls: `"verb": "<name>"` in a source or an action.
pub fn verbs_called(spec: &Value) -> BTreeSet<String> {
    fn walk(value: &Value, out: &mut BTreeSet<String>) {
        match value {
            Value::Object(map) => {
                // A verb name is `<service>_<method>`; a `verb` key in a
                // surface's own state (an empty field, a template) is not.
                if let Some(Value::String(name)) = map.get("verb") {
                    if name.contains('_') && !name.contains("{{") && !name.contains(' ') {
                        out.insert(name.clone());
                    }
                }
                for child in map.values() {
                    walk(child, out);
                }
            }
            Value::Array(items) => items.iter().for_each(|i| walk(i, out)),
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    walk(spec, &mut out);
    out
}

fn safety_word(class: SafetyClass) -> &'static str {
    match class {
        SafetyClass::ReadOnly => "read-only",
        SafetyClass::Mutating => "mutating",
        SafetyClass::Destructive => "destructive",
        SafetyClass::External => "external",
    }
}

/// What a person reviews before installing (P-D2).
pub async fn review(package: &Package) -> Value {
    let manifest = &package.manifest;
    let mut surfaces = Vec::new();
    let mut called: BTreeSet<String> = manifest.requires.verbs.iter().cloned().collect();
    let mut all_valid = true;
    for entry in &manifest.surfaces {
        let spec = match package.surface_spec(entry) {
            Ok(spec) => spec,
            Err(e) => {
                all_valid = false;
                surfaces.push(
                    json!({"name": entry.name, "file": entry.file, "valid": false,
                                     "problems": [e.message], "verbs": [], "would": "refuse"}),
                );
                continue;
            }
        };
        let (_, problems) = impress_surface::validate_json(&spec);
        let errors: Vec<String> = problems
            .iter()
            .filter(|p| p.severity == impress_surface::Severity::Error)
            .map(|p| format!("{}: {}", p.path, p.message))
            .collect();
        all_valid &= errors.is_empty();
        let verbs = verbs_called(&spec);
        called.extend(verbs.iter().cloned());
        let would = would(&manifest.name, &entry.name, &spec).await;
        surfaces.push(json!({
            "name": entry.name, "file": entry.file, "valid": errors.is_empty(),
            "problems": errors, "verbs": verbs, "would": would,
        }));
    }
    let store = shipped::store(false).ok();
    let baselines = workflow_baselines(&manifest.name);
    let mut workflows = Vec::new();
    for entry in &manifest.workflows {
        let (row, valid, verbs) = contents::review_workflow(
            store.as_deref(),
            &manifest.name,
            entry,
            &package.files[&entry.file],
            baselines.get(&entry.name).map(String::as_str),
        );
        all_valid &= valid;
        called.extend(verbs);
        workflows.push(row);
    }
    let mut secrets: BTreeSet<String> = manifest.requires.secrets.iter().cloned().collect();
    let verbs: Vec<Value> = called
        .iter()
        .map(|name| match VerbDescriptor::find(name) {
            Some(verb) => {
                secrets.extend(verb.secrets.iter().map(|s| s.to_string()));
                json!({"name": name, "linked": true, "safety": safety_word(verb.safety.class),
                       "secrets": verb.secrets})
            }
            None => json!({"name": name, "linked": false, "safety": null, "secrets": []}),
        })
        .collect();
    let missing_verbs: Vec<&String> = called
        .iter()
        .filter(|name| VerbDescriptor::find(name).is_none())
        .collect();
    let missing_secrets: Vec<&String> = secrets
        .iter()
        .filter(|name| impress_service_core::secrets::get(name).is_none())
        .collect();
    json!({
        "ok": true,
        "name": manifest.name,
        "version": manifest.version,
        "description": manifest.description,
        "source": package.source,
        "digest": package.digest(),
        "valid": all_valid,
        "surfaces": surfaces,
        "skills": manifest.skills.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
        "workflows": workflows,
        "ai_categories": manifest.ai_categories,
        "verbs": verbs,
        "missing_verbs": missing_verbs,
        "missing_secrets": missing_secrets,
    })
}

/// What installing `spec` as `package`'s surface `name` would do now.
async fn would(package: &str, name: &str, spec: &Value) -> &'static str {
    let Ok(store) = shipped::store(false) else {
        return "create";
    };
    let Ok(Some(item)) = store.get(shipped::baseline_id(package, name)) else {
        return "create";
    };
    let baseline = shipped::Baseline::of(&item);
    if baseline.digest == crate::digest::spec_digest(spec) {
        return "keep (unchanged)";
    }
    match shipped::current(&baseline.surface_id).await {
        Ok(None) => "leave deleted",
        Ok(Some(copy)) if copy == baseline.spec => "update",
        Ok(Some(_)) => "keep yours; hold the new one as pending",
        Err(_) => "update",
    }
}

/// What one install did.
#[derive(Debug, Clone, PartialEq)]
pub struct Installed {
    pub name: String,
    pub version: String,
    pub digest: String,
    /// `(surface name, surface id, outcome)`.
    pub surfaces: Vec<(String, String, Outcome)>,
    /// What happened to each workflow (P-D9).
    pub workflows: Vec<WorkflowOutcome>,
    /// The AI task categories it declares.
    pub ai_categories: Vec<String>,
}

/// What [`remove`] did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Removed {
    /// Surfaces deleted (as shipped) and kept (the person changed them).
    pub deleted: Vec<String>,
    pub kept: Vec<String>,
    /// Workflows deleted (as shipped) and kept (the person edited them).
    pub workflows_deleted: Vec<String>,
    pub workflows_kept: Vec<String>,
}

/// The workflow digests the installed `package`'s record holds, by name.
pub fn workflow_baselines(package: &str) -> BTreeMap<String, String> {
    installed()
        .unwrap_or_default()
        .iter()
        .find(|record| record.get("name").and_then(Value::as_str) == Some(package))
        .and_then(|record| record.get("workflows").and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|w| {
            Some((
                w.get("name")?.as_str()?.to_string(),
                w.get("digest")?.as_str()?.to_string(),
            ))
        })
        .collect()
}

fn package_id(name: &str) -> ItemId {
    let namespace = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"koine-package");
    uuid::Uuid::new_v5(&namespace, name.as_bytes())
}

/// Install `package`. Every surface must validate, or nothing is written.
pub async fn install(package: &Package) -> Result<Installed, ShippedError> {
    let manifest = &package.manifest;
    let mut specs = Vec::new();
    for entry in &manifest.surfaces {
        let spec = package.surface_spec(entry)?;
        let (_, problems) = impress_surface::validate_json(&spec);
        if let Some(problem) = problems
            .iter()
            .find(|p| p.severity == impress_surface::Severity::Error)
        {
            return Err(fail(
                "invalid-spec",
                format!(
                    "{} ({}): {}: {}",
                    entry.name, entry.file, problem.path, problem.message
                ),
            ));
        }
        specs.push((entry, spec));
    }
    let mut workflows = Vec::new();
    for entry in &manifest.workflows {
        let spec = workflow_spec(package, entry)?;
        if let Some(problem) = contents::workflow_errors(&spec).into_iter().next() {
            return Err(fail(
                "invalid-spec",
                format!("workflow {} ({}): {problem}", entry.name, entry.file),
            ));
        }
        workflows.push((entry.name.clone(), spec));
    }
    let baselines = workflow_baselines(&manifest.name);
    let mut surfaces = Vec::new();
    for (entry, spec) in specs {
        let (outcome, id) =
            shipped::install(&manifest.name, &manifest.version, &entry.name, &spec).await?;
        surfaces.push((entry.name.clone(), id, outcome));
    }
    let store = shipped::store(true)?;
    let workflows =
        contents::install_workflows(store.as_ref(), &manifest.name, &workflows, &baselines)?;
    contents::install_categories(store.as_ref(), &manifest.name, &manifest.ai_categories)?;
    let installed = Installed {
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        digest: package.digest(),
        surfaces,
        workflows,
        ai_categories: manifest
            .ai_categories
            .iter()
            .map(|c| c.id.clone())
            .collect(),
    };
    record(package, &installed)?;
    Ok(installed)
}

fn record(package: &Package, installed: &Installed) -> Result<(), ShippedError> {
    let store = shipped::store(true)?;
    let manifest = &package.manifest;
    let payload: Value = json!({
        "name": manifest.name,
        "version": manifest.version,
        "description": manifest.description,
        "digest": installed.digest,
        "source": package.source,
        "installed_at": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "surfaces": installed.surfaces.iter()
            .map(|(name, id, _)| json!({"name": name, "surface_id": id}))
            .collect::<Vec<_>>(),
        "skills": manifest.skills.iter()
            .map(|s| json!({"name": s.name, "text": package.skill_text(s)}))
            .collect::<Vec<_>>(),
        "requires": manifest.requires,
        "workflows": installed.workflows.iter()
            .map(|w| json!({"name": w.name, "workflow_id": w.workflow_id, "digest": w.digest}))
            .collect::<Vec<_>>(),
        "ai_categories": installed.ai_categories,
    });
    let fields: BTreeMap<String, ItemValue> = payload
        .as_object()
        .expect("an object")
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                serde_json::from_value(v.clone()).unwrap_or(ItemValue::Null),
            )
        })
        .collect();
    let id = package_id(&manifest.name);
    let fail_store = |e: impress_item::StoreError| fail("store-error", e.to_string());
    if store.get(id).map_err(fail_store)?.is_some() {
        store.delete(id).map_err(fail_store)?;
    }
    let now = Utc::now();
    store
        .insert(Item {
            id,
            schema: SchemaRef::from_stored(PACKAGE.to_owned()),
            payload: fields,
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
        .map_err(fail_store)
}

/// Every installed package's record, oldest first.
pub fn installed() -> Result<Vec<serde_json::Map<String, Value>>, ShippedError> {
    let store = shipped::store(false)?;
    let items = store
        .query(&ItemQuery {
            schema: Some(SchemaRef::from_stored(PACKAGE.to_owned())),
            sort: vec![SortDescriptor {
                field: "created".into(),
                ascending: true,
            }],
            include_tags: false,
            include_references: false,
            ..Default::default()
        })
        .map_err(|e| fail("store-error", e.to_string()))?;
    Ok(items
        .iter()
        .map(|item| {
            item.payload
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::to_value(v).unwrap_or(Value::Null)))
                .collect()
        })
        .collect())
}

/// Remove `name`: its pristine surfaces and workflows are deleted; those the
/// person changed are kept, surfaces without a baseline (P-D3, P-D9). Its
/// AI task categories are removed.
pub async fn remove(name: &str) -> Result<Removed, ShippedError> {
    let store = shipped::store(true)?;
    let id = package_id(name);
    if store
        .get(id)
        .map_err(|e| fail("store-error", e.to_string()))?
        .is_none()
    {
        return Err(fail("not-found", format!("no package {name} is installed")));
    }
    let mut removed = Removed::default();
    for (workflow, digest) in workflow_baselines(name) {
        let id = contents::workflow_id(name, &workflow).to_string();
        match contents::remove_workflow_if_pristine(store.as_ref(), name, &workflow, &digest)? {
            Some(true) => removed.workflows_deleted.push(id),
            Some(false) => removed.workflows_kept.push(id),
            None => {}
        }
    }
    contents::remove_categories(store.as_ref(), name)?;
    for baseline in shipped::list(Some(name))? {
        match shipped::status(&baseline).await? {
            shipped::Status::Pristine => {
                shipped::delete(&baseline.surface_id).await?;
                removed.deleted.push(baseline.surface_id.clone());
            }
            shipped::Status::Missing => {}
            _ => removed.kept.push(baseline.surface_id.clone()),
        }
        shipped::forget(&baseline)?;
    }
    store
        .delete(id)
        .map_err(|e| fail("store-error", e.to_string()))?;
    Ok(removed)
}

/// The skills of every installed package, `(name, text)`.
pub fn installed_skills() -> Vec<(String, String)> {
    installed()
        .unwrap_or_default()
        .iter()
        .flat_map(|p| {
            p.get("skills")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .filter_map(|s| {
            Some((
                s.get("name")?.as_str()?.to_string(),
                s.get("text")?.as_str()?.to_string(),
            ))
        })
        .collect()
}
