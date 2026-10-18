//! Saved patterns: records of kind `regex-pattern` in the process store.
//!
//! A saved pattern is keyed by its name. Its record id is the host
//! materialiser's for a row of the kind, `uuid5(uuid5(OID, "koine-rows"),
//! "regex-pattern:<name>")`, so the rows `patterns` answers (it declares
//! `returns_kind = "regex-pattern"` with `rows(id = "name")`) land on the
//! records this module wrote instead of beside them. The schema ref is the
//! kind spelled bare, which is what a host's kind manifest maps the kind to,
//! so a surface's `query` over `regex-pattern` reads them.

use std::collections::BTreeMap;
use std::sync::Arc;

use impress_item::{
    ActorKind, AttributedStore, FieldMutation, Item, ItemId, ItemQuery, Priority, SchemaRef,
    SortDescriptor, Value, Visibility,
};
use uuid::Uuid;

use crate::dto::{Outcome, SavedPattern};

/// The kind, and the schema ref its records are stored under.
pub const KIND: &str = "regex-pattern";

/// Who a saved pattern is written as.
pub const AUTHOR: &str = "system:regex-lab";

/// The record id of the pattern named `name`.
pub fn record_id(name: &str) -> ItemId {
    let namespace = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"koine-rows");
    Uuid::new_v5(&namespace, format!("{KIND}:{name}").as_bytes())
}

/// A name is lowercase letters, digits, `-` and `_`, starting with a letter,
/// at most 64 characters: it is typed as `saved` and read in a list.
pub fn check_name(name: &str) -> Result<String, Outcome> {
    let name = name.trim();
    let mut chars = name.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && name.len() <= 64;
    if valid {
        Ok(name.to_string())
    } else {
        Err(Outcome::refused(
            "invalid-argument",
            format!(
                "'{name}' is not a pattern name: lowercase letters, digits, '-' and '_', \
                 starting with a letter, at most 64"
            ),
        ))
    }
}

fn store_error(e: impl std::fmt::Display) -> Outcome {
    Outcome::refused("store-error", e.to_string())
}

/// The store to read or write through. A write to the stand-in a process
/// without a store gets would vanish with it, so a write is refused there.
fn store(for_write: bool) -> Result<Arc<dyn AttributedStore>, Outcome> {
    let (store, fallback) = koine_store::process::acquire_or_stand_in();
    if for_write && fallback {
        return Err(Outcome::refused(
            "store-unavailable",
            match koine_store::process::intended_location() {
                Some(at) => {
                    format!("the store at {at} could not be opened, so the pattern was not saved")
                }
                None => "this process has no store (run the host to save patterns); \
                         nothing was saved"
                    .to_string(),
            },
        ));
    }
    Ok(store)
}

fn schema() -> SchemaRef {
    SchemaRef::from_stored(KIND.to_owned())
}

fn text(payload: &BTreeMap<String, Value>, field: &str) -> String {
    match payload.get(field) {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

fn saved_of(item: &Item) -> SavedPattern {
    SavedPattern {
        name: text(&item.payload, "name"),
        pattern: text(&item.payload, "pattern"),
        flags: text(&item.payload, "flags"),
        description: text(&item.payload, "description"),
        sample: text(&item.payload, "sample"),
    }
}

/// The saved pattern named `name`. Refuses `not-found`.
pub fn get(name: &str) -> Result<SavedPattern, Outcome> {
    let store = store(false)?;
    match store.get(record_id(name)).map_err(store_error)? {
        Some(item) if item.schema.as_str() == KIND => Ok(saved_of(&item)),
        _ => Err(Outcome::refused(
            "not-found",
            format!("no saved pattern is named '{name}' (regex-lab-service_patterns lists them)"),
        )),
    }
}

/// Every saved pattern, by name, whose name, pattern or description holds
/// `search` (case-insensitively). A process with no store has none.
pub fn list(search: Option<&str>) -> Result<Vec<SavedPattern>, Outcome> {
    let store = store(false)?;
    let items = store
        .query(&ItemQuery {
            schema: Some(schema()),
            sort: vec![SortDescriptor {
                field: "created".into(),
                ascending: true,
            }],
            ..Default::default()
        })
        .map_err(store_error)?;
    let needle = search
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase);
    let mut saved: Vec<SavedPattern> = items
        .iter()
        .map(saved_of)
        .filter(|p| match &needle {
            Some(needle) => [&p.name, &p.pattern, &p.description]
                .iter()
                .any(|field| field.to_lowercase().contains(needle)),
            None => true,
        })
        .collect();
    saved.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(saved)
}

/// Save `pattern` under its name, replacing a pattern of the same name.
/// Answers whether it is new.
pub fn put(pattern: &SavedPattern) -> Result<bool, Outcome> {
    let store = store(true)?;
    let id = record_id(&pattern.name);
    let fields = [
        ("name", &pattern.name),
        ("pattern", &pattern.pattern),
        ("flags", &pattern.flags),
        ("description", &pattern.description),
        ("sample", &pattern.sample),
    ];
    if store.get(id).map_err(store_error)?.is_some() {
        let mutations = fields
            .iter()
            .map(|(field, value)| {
                FieldMutation::SetPayload((*field).into(), Value::String((*value).clone()))
            })
            .collect();
        store.update(id, mutations).map_err(store_error)?;
        return Ok(false);
    }
    let payload = fields
        .iter()
        .map(|(field, value)| ((*field).to_string(), Value::String((*value).clone())))
        .collect();
    let now = chrono::Utc::now();
    store
        .insert(Item {
            id,
            schema: schema(),
            payload,
            created: now,
            modified: now,
            author: AUTHOR.into(),
            author_kind: ActorKind::System,
            logical_clock: 0,
            origin: None,
            canonical_id: None,
            tags: vec![],
            flag: None,
            is_read: false,
            is_starred: false,
            priority: Priority::default(),
            visibility: Visibility::Private,
            message_type: None,
            produced_by: None,
            version: None,
            batch_id: None,
            references: vec![],
            parent: None,
            extra: Default::default(),
        })
        .map_err(store_error)?;
    Ok(true)
}

/// Delete the pattern named `name`. Refuses `not-found`.
pub fn delete(name: &str) -> Result<(), Outcome> {
    let store = store(true)?;
    let id = record_id(name);
    match store.get(id).map_err(store_error)? {
        Some(item) if item.schema.as_str() == KIND => store.delete(id).map_err(store_error),
        _ => Err(Outcome::refused(
            "not-found",
            format!("no saved pattern is named '{name}'"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_record_id_is_the_materialisers() {
        let namespace = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"koine-rows");
        assert_eq!(
            record_id("iso-date"),
            Uuid::new_v5(&namespace, b"regex-pattern:iso-date")
        );
    }

    #[test]
    fn a_name_is_checked() {
        assert_eq!(check_name(" iso-date_2 ").unwrap(), "iso-date_2");
        for bad in ["", "Iso", "2nd", "a b", "a/b"] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
    }
}
