//! Record kinds that evolve (docs/plan-koine-trust.md TR-D12).
//!
//! A [`KindSchema`] says what fields a kind's payload has, their types and
//! which are required. Kinds evolve by AT Protocol Lexicon's rules
//! ([`compatible`]): a new field is optional; a required field stays, and
//! stays required; a field keeps its type; an enumeration never loses a
//! value. A change that breaks a rule is a new kind name (`task@2.0.0`),
//! never a new version of the old one — so a device running an older build
//! keeps reading what a newer one writes.
//!
//! The process keeps one registry ([`register`]). Every store write is
//! stamped with the schema version it was written under: an item's
//! `version` ([`stamp_item`]) and an operation's `op_schema_version`
//! ([`stamp_operation`]) — the stamps that keep read-time lenses possible
//! later. A payload that does not match its kind's schema is counted, not
//! refused ([`validation_failures`]): warn mode, until a kind's writers are
//! clean.
//!
//! Schemas are records too ([`KIND_SCHEMA`]), so a package can ship one and a
//! store can carry it ([`save`], [`load`]).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, RwLock};

use serde::{Deserialize, Serialize};

use crate::item::{Item, Value};

/// The kind of a stored kind schema.
pub const KIND_SCHEMA: &str = "core/kind-schema@1.0.0";

/// A field's type, as JSON sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    String,
    Int,
    /// Any number (an int is a float too).
    Float,
    Bool,
    Array,
    Object,
    /// An item id (a string holding a uuid).
    Ref,
    /// Anything.
    Any,
}

impl FieldType {
    /// Whether `value` is of this type (`null` never is; a missing optional
    /// field is not looked at).
    pub fn admits(self, value: &Value) -> bool {
        match (self, value) {
            (FieldType::Any, _) => true,
            (FieldType::String, Value::String(_)) => true,
            (FieldType::Ref, Value::String(s)) => uuid::Uuid::parse_str(s).is_ok(),
            (FieldType::Int, Value::Int(_)) => true,
            (FieldType::Float, Value::Int(_) | Value::Float(_)) => true,
            (FieldType::Bool, Value::Bool(_)) => true,
            (FieldType::Array, Value::Array(_)) => true,
            (FieldType::Object, Value::Object(_)) => true,
            _ => false,
        }
    }
}

/// One field of a kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDef {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: FieldType,
    #[serde(default)]
    pub required: bool,
    /// The values a string field may hold, when it is an enumeration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
}

impl FieldDef {
    pub fn required(name: impl Into<String>, ty: FieldType) -> Self {
        Self {
            name: name.into(),
            ty,
            required: true,
            values: None,
        }
    }

    pub fn optional(name: impl Into<String>, ty: FieldType) -> Self {
        Self {
            name: name.into(),
            ty,
            required: false,
            values: None,
        }
    }
}

/// What a kind's payload holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindSchema {
    /// The kind (`schema_ref`): `task@1.0.0`, `imbib/bibliography-entry`.
    pub name: String,
    /// This schema's version (`1.0.0`, `1.1.0`): only compatible changes
    /// move it.
    pub version: String,
    pub fields: Vec<FieldDef>,
}

/// One way a new schema breaks the old one's readers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Break {
    RequiredRemoved(String),
    TypeChanged {
        field: String,
        from: FieldType,
        to: FieldType,
    },
    NewFieldRequired(String),
    BecameRequired(String),
    NoLongerRequired(String),
    ValuesNarrowed {
        field: String,
        removed: Vec<String>,
    },
}

impl std::fmt::Display for Break {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Break::RequiredRemoved(field) => write!(f, "required field `{field}` was removed"),
            Break::TypeChanged { field, from, to } => {
                write!(f, "field `{field}` changed type from {from:?} to {to:?}")
            }
            Break::NewFieldRequired(field) => {
                write!(
                    f,
                    "new field `{field}` is required (a new field must be optional)"
                )
            }
            Break::BecameRequired(field) => write!(f, "optional field `{field}` became required"),
            Break::NoLongerRequired(field) => {
                write!(
                    f,
                    "required field `{field}` became optional (readers rely on it)"
                )
            }
            Break::ValuesNarrowed { field, removed } => {
                write!(f, "field `{field}` no longer allows {}", removed.join(", "))
            }
        }
    }
}

/// Whether `new` may replace `old` under the same kind name (AT Protocol
/// Lexicon's evolution rules). An optional field may be dropped: no reader
/// could rely on it.
pub fn compatible(old: &KindSchema, new: &KindSchema) -> Result<(), Vec<Break>> {
    let new_fields: BTreeMap<&str, &FieldDef> =
        new.fields.iter().map(|f| (f.name.as_str(), f)).collect();
    let old_fields: BTreeMap<&str, &FieldDef> =
        old.fields.iter().map(|f| (f.name.as_str(), f)).collect();
    let mut breaks = Vec::new();
    for (name, before) in &old_fields {
        match new_fields.get(name) {
            None if before.required => breaks.push(Break::RequiredRemoved((*name).into())),
            None => {}
            Some(after) => {
                if after.ty != before.ty {
                    breaks.push(Break::TypeChanged {
                        field: (*name).into(),
                        from: before.ty,
                        to: after.ty,
                    });
                }
                match (before.required, after.required) {
                    (false, true) => breaks.push(Break::BecameRequired((*name).into())),
                    (true, false) => breaks.push(Break::NoLongerRequired((*name).into())),
                    _ => {}
                }
                match (&before.values, &after.values) {
                    (Some(was), Some(now)) => {
                        let now: BTreeSet<&String> = now.iter().collect();
                        let removed: Vec<String> =
                            was.iter().filter(|v| !now.contains(v)).cloned().collect();
                        if !removed.is_empty() {
                            breaks.push(Break::ValuesNarrowed {
                                field: (*name).into(),
                                removed,
                            });
                        }
                    }
                    (None, Some(now)) => breaks.push(Break::ValuesNarrowed {
                        field: (*name).into(),
                        removed: vec![format!("anything but {}", now.join(", "))],
                    }),
                    _ => {}
                }
            }
        }
    }
    for (name, after) in &new_fields {
        if !old_fields.contains_key(name) && after.required {
            breaks.push(Break::NewFieldRequired((*name).into()));
        }
    }
    if breaks.is_empty() {
        Ok(())
    } else {
        Err(breaks)
    }
}

/// What in `payload` does not match `schema`: a required field missing, a
/// field of the wrong type, a value outside an enumeration. Fields the schema
/// does not name are fine — they are a newer writer's.
pub fn validate(schema: &KindSchema, payload: &BTreeMap<String, Value>) -> Vec<String> {
    let mut problems = Vec::new();
    for field in &schema.fields {
        match payload.get(&field.name) {
            None | Some(Value::Null) if field.required => {
                problems.push(format!("`{}` is required", field.name))
            }
            None | Some(Value::Null) => {}
            Some(value) => {
                if !field.ty.admits(value) {
                    problems.push(format!("`{}` is not {:?}", field.name, field.ty));
                } else if let (Some(values), Value::String(s)) = (&field.values, value) {
                    if !values.contains(s) {
                        problems.push(format!("`{}` = `{s}` is not one of its values", field.name));
                    }
                }
            }
        }
    }
    problems
}

/// Why a schema was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub kind: String,
    pub breaks: Vec<Break>,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "kind `{}` cannot change this way ({}); a breaking change is a new kind name",
            self.kind,
            self.breaks
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        )
    }
}

impl std::error::Error for Refused {}

static REGISTRY: LazyLock<RwLock<BTreeMap<String, KindSchema>>> = LazyLock::new(Default::default);

/// Register `schema` for its kind: the first schema for a kind, or a
/// compatible successor of the one registered. An incompatible schema under
/// the same name is refused.
pub fn register(schema: KindSchema) -> Result<(), Refused> {
    let mut registry = REGISTRY.write().unwrap_or_else(|e| e.into_inner());
    if let Some(current) = registry.get(&schema.name) {
        if let Err(breaks) = compatible(current, &schema) {
            return Err(Refused {
                kind: schema.name,
                breaks,
            });
        }
    }
    registry.insert(schema.name.clone(), schema);
    Ok(())
}

/// The registered schema of `kind`.
pub fn get(kind: &str) -> Option<KindSchema> {
    REGISTRY
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(kind)
        .cloned()
}

/// The registered version of `kind`.
pub fn version_of(kind: &str) -> Option<String> {
    REGISTRY
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(kind)
        .map(|s| s.version.clone())
}

/// Every registered schema, by kind.
pub fn registered() -> Vec<KindSchema> {
    REGISTRY
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .cloned()
        .collect()
}

static FAILURES: AtomicU64 = AtomicU64::new(0);
static LAST_FAILURE: Mutex<Option<String>> = Mutex::new(None);

/// How many writes did not match their kind's schema (warn mode), and the
/// latest one.
pub fn validation_failures() -> (u64, Option<String>) {
    (
        FAILURES.load(Ordering::Relaxed),
        LAST_FAILURE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone(),
    )
}

/// Before a store writes `item`: stamp the version of its kind's schema
/// (when the writer named none) and check its payload against it, counting
/// a mismatch. Every backend calls this on insert.
pub fn stamp_item(item: &mut Item) {
    let Some(schema) = get(item.schema.as_str()) else {
        return;
    };
    if item.version.is_none() {
        item.version = Some(schema.version.clone());
    }
    let problems = validate(&schema, &item.payload);
    if !problems.is_empty() {
        FAILURES.fetch_add(1, Ordering::Relaxed);
        *LAST_FAILURE.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!(
            "{} {}: {}",
            schema.name,
            item.id,
            problems.join("; ")
        ));
    }
}

/// On an operation's payload: the version of the target kind's schema it
/// was written under (`op_schema_version`). Every backend calls this when it
/// builds an operation row.
pub fn stamp_operation(payload: &mut BTreeMap<String, Value>, target_kind: &str) {
    if let Some(version) = version_of(target_kind) {
        payload.insert("op_schema_version".into(), Value::String(version));
    }
}

/// A schema as an item of [`KIND_SCHEMA`]: its payload is the schema.
pub fn to_payload(schema: &KindSchema) -> BTreeMap<String, Value> {
    serde_json::to_value(schema)
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

/// A [`KIND_SCHEMA`] item's schema.
pub fn from_item(item: &Item) -> Option<KindSchema> {
    if item.schema.as_str() != KIND_SCHEMA {
        return None;
    }
    serde_json::to_value(&item.payload)
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
}

/// Store `schema` as a record (a package ships it; a store carries it).
pub fn save(
    store: &dyn crate::ItemStore,
    schema: &KindSchema,
) -> Result<crate::ItemId, crate::StoreError> {
    let id = uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        format!(
            "koine-kind-schema\u{0}{}\u{0}{}",
            schema.name, schema.version
        )
        .as_bytes(),
    );
    if store.get(id)?.is_some() {
        return Ok(id);
    }
    let now = chrono::Utc::now();
    let (author, author_kind) = store.default_author();
    store.insert(Item {
        id,
        schema: crate::SchemaRef::from_stored(KIND_SCHEMA.to_owned()),
        payload: to_payload(schema),
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
        priority: crate::Priority::Normal,
        visibility: crate::Visibility::Private,
        message_type: None,
        produced_by: None,
        version: None,
        batch_id: None,
        references: Vec::new(),
        parent: None,
        extra: Default::default(),
    })
}

/// Register every schema `store` carries, oldest version first; answers the
/// ones refused as incompatible.
pub fn load(store: &dyn crate::ItemStore) -> Result<Vec<Refused>, crate::StoreError> {
    let mut schemas: Vec<KindSchema> = store
        .query(&crate::ItemQuery {
            schema: Some(crate::SchemaRef::from_stored(KIND_SCHEMA.to_owned())),
            ..Default::default()
        })?
        .iter()
        .filter_map(from_item)
        .collect();
    schemas.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then(version_key(&a.version).cmp(&version_key(&b.version)))
    });
    Ok(schemas
        .into_iter()
        .filter_map(|schema| register(schema).err())
        .collect())
}

fn version_key(version: &str) -> Vec<u64> {
    version
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(version: &str, fields: Vec<FieldDef>) -> KindSchema {
        KindSchema {
            name: "t/kind@1".into(),
            version: version.into(),
            fields,
        }
    }

    #[test]
    fn adding_an_optional_field_is_compatible_and_breaking_changes_are_named() {
        let v1 = schema(
            "1.0.0",
            vec![
                FieldDef::required("title", FieldType::String),
                FieldDef::optional("note", FieldType::String),
                FieldDef {
                    values: Some(vec!["a".into(), "b".into()]),
                    ..FieldDef::optional("state", FieldType::String)
                },
            ],
        );
        let mut v2 = v1.clone();
        v2.version = "1.1.0".into();
        v2.fields.push(FieldDef::optional("year", FieldType::Int));
        assert_eq!(compatible(&v1, &v2), Ok(()));
        // Dropping an optional field is fine; dropping a required one is not.
        let mut dropped = v1.clone();
        dropped.fields.retain(|f| f.name != "note");
        assert_eq!(compatible(&v1, &dropped), Ok(()));
        let mut bad = v1.clone();
        bad.fields.retain(|f| f.name != "title");
        bad.fields
            .push(FieldDef::required("isbn", FieldType::String));
        bad.fields[0] = FieldDef::optional("note", FieldType::Int);
        bad.fields[1] = FieldDef {
            values: Some(vec!["a".into()]),
            ..FieldDef::optional("state", FieldType::String)
        };
        let breaks = compatible(&v1, &bad).unwrap_err();
        assert!(breaks.contains(&Break::RequiredRemoved("title".into())));
        assert!(breaks.contains(&Break::NewFieldRequired("isbn".into())));
        assert!(breaks
            .iter()
            .any(|b| matches!(b, Break::TypeChanged { field, .. } if field == "note")));
        assert!(breaks
            .iter()
            .any(|b| matches!(b, Break::ValuesNarrowed { field, .. } if field == "state")));
    }

    #[test]
    fn the_registry_refuses_an_incompatible_schema_under_the_same_name() {
        let name = "t/registry-kind@1";
        let v1 = KindSchema {
            name: name.into(),
            version: "1.0.0".into(),
            fields: vec![FieldDef::required("title", FieldType::String)],
        };
        register(v1.clone()).unwrap();
        let v2 = KindSchema {
            version: "1.1.0".into(),
            fields: vec![
                FieldDef::required("title", FieldType::String),
                FieldDef::optional("year", FieldType::Int),
            ],
            ..v1.clone()
        };
        register(v2).unwrap();
        assert_eq!(version_of(name).as_deref(), Some("1.1.0"));
        let broken = KindSchema {
            version: "2.0.0".into(),
            fields: vec![FieldDef::required("title", FieldType::Int)],
            ..v1
        };
        let refused = register(broken).unwrap_err();
        assert!(refused.to_string().contains("new kind name"), "{refused}");
        assert_eq!(version_of(name).as_deref(), Some("1.1.0"), "unchanged");
    }

    #[test]
    fn validation_reports_but_tolerates_fields_it_does_not_know() {
        let s = schema(
            "1",
            vec![
                FieldDef::required("title", FieldType::String),
                FieldDef::optional("year", FieldType::Int),
            ],
        );
        let mut payload = BTreeMap::new();
        payload.insert("title".into(), Value::String("x".into()));
        payload.insert("from-the-future".into(), Value::Bool(true));
        assert!(validate(&s, &payload).is_empty());
        payload.insert("year".into(), Value::String("1999".into()));
        payload.remove("title");
        assert_eq!(validate(&s, &payload).len(), 2);
    }
}
