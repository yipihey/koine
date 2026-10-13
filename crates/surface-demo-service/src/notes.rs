//! The tour's note records (plan-koine-tour T-D2): what the `notes`,
//! `note_add`, `note_rename`, `note_move` and `note_delete` verbs read and
//! write, through the process store (`koine_store::process`).
//!
//! # Kind and schema ref
//!
//! A note is a record of kind [`KIND`] (`note`) with schema ref
//! [`SCHEMA_REF`], which is also the bare string `note`. The spelling is not
//! a choice of taste: the Notes surface reads notes with a `query` source
//! over the kind, and a kind-scoped query compiles against the host's kind
//! manifest, which for a koine host is `koine_host::manifest::from_inventory`.
//! That manifest maps every `returns_kind` a linked verb declares to a
//! one-element ref list spelled exactly like the kind (`kinds["note"] =
//! ["note"]`) — so the only ref a query over `note` will ever match is
//! `note`. A record written as `demo/note@1.0.0` would be correct JSON and
//! would never be read: the store matches `schema_ref` by exact equality.
//! `tests/tour.rs` pins this by seeding and rendering the Notes surface.
//!
//! (`note` is not a canonical ref of `schema-refs.json`, so the verbs'
//! `effects` name it through `any("…")` rather than as a ref string, which
//! the macro checks against the canonical table.)
//!
//! # Ids: the key, the record, the envelope
//!
//! Each note has a short stable **key** (`research`, `n-3f2a9c1b`), kept in
//! its payload as `id`, and a record id derived from it by the
//! materialiser's rule (plan-koine-live L-D4):
//! `uuid5(uuid5(OID, "koine-rows"), "note:<key>")`. `notes` declares
//! `returns_kind = "note"` with `rows(id = "id")`, so inside a host the
//! pipeline upserts its rows as records of kind `note` under exactly that
//! id — the same records this module wrote. Had the rows carried the record
//! uuid as their `id`, the host would have materialised a second copy of
//! every note (`uuid5("note:<uuid>")`) and the tree would have shown each
//! note twice.
//!
//! The tree, on the other hand, sees records as the runtime hands them
//! over: the payload lifted to the top of the row with the envelope laid
//! over it, the envelope winning a name collision. Its `id` is therefore the
//! record uuid, and its `parent` is the envelope `parent` — so a child note
//! sets `Item::parent` to its parent's record id (the payload `parent` is
//! the parent's key, for the `notes` rows). Every event the tree sends
//! carries record uuids; the `notes` rows carry keys; the verbs take either
//! ([`resolve_id`]).

use std::collections::BTreeMap;
use std::sync::Arc;

use impress_item::{
    ActorKind, AttributedStore, FieldMutation, Item, ItemId, ItemQuery, Priority, SchemaRef,
    SortDescriptor, Value, Visibility,
};
use uuid::Uuid;

/// The pane-query kind of a note.
pub const KIND: &str = "note";

/// The schema ref a note record is stored under: the kind, spelled as
/// `koine_host::manifest::from_inventory` maps it (see the module docs).
pub const SCHEMA_REF: &str = "note";

/// The author a note is written as.
pub const AUTHOR: &str = "system:surface-demo";

/// One note as the verbs answer it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// The short key (payload `id`).
    pub key: String,
    /// The record id (`record_id(key)`).
    pub record: ItemId,
    pub title: String,
    /// The parent's key, `None` for a root.
    pub parent: Option<String>,
}

/// Something a note operation refused: a kebab-case code and a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteError {
    pub code: &'static str,
    pub message: String,
}

impl NoteError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn store(e: impl std::fmt::Display) -> Self {
        Self::new("store-error", e.to_string())
    }
}

/// The record id of the note with `key`: the materialiser's
/// `uuid5(koine-rows, "<kind>:<row id>")` (plan-koine-live L-D4), so a
/// host materialising `notes`' rows lands on the record this module wrote.
pub fn record_id(key: &str) -> ItemId {
    let namespace = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"koine-rows");
    Uuid::new_v5(&namespace, format!("{KIND}:{key}").as_bytes())
}

/// A note reference as a verb receives it — a record uuid (what the tree's
/// events carry) or a key (what `notes` rows carry) — as the record id.
pub fn resolve_id(id: &str) -> ItemId {
    let id = id.trim();
    id.parse::<ItemId>().unwrap_or_else(|_| record_id(id))
}

/// A fresh key for a new note: `n-` plus eight hex digits of a random uuid.
pub fn fresh_key() -> String {
    let raw = Uuid::new_v4().simple().to_string();
    format!("n-{}", &raw[..8])
}

fn schema() -> SchemaRef {
    SchemaRef::from_stored(SCHEMA_REF.to_owned())
}

fn text(payload: &BTreeMap<String, Value>, field: &str) -> Option<String> {
    match payload.get(field) {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn note_of(item: &Item) -> Note {
    Note {
        key: text(&item.payload, "id").unwrap_or_else(|| item.id.to_string()),
        record: item.id,
        title: text(&item.payload, "title").unwrap_or_default(),
        parent: text(&item.payload, "parent"),
    }
}

/// The store a verb reads or writes notes through: the call's or the
/// process's. A read is answered from the stand-in too (an empty list); a
/// write to a stand-in would vanish with the process, so it is refused.
fn store(for_write: bool) -> Result<Arc<dyn AttributedStore>, NoteError> {
    let (store, fallback) = koine_store::process::acquire_or_stand_in();
    if for_write && fallback {
        return Err(NoteError::new(
            "store-unavailable",
            match koine_store::process::intended_location() {
                Some(at) => format!(
                    "the store at {at} could not be opened; this write would land in a \
                     temporary in-memory stand-in and vanish, so nothing was written"
                ),
                None => "no store is installed in this process; a note write needs a host \
                         (nothing was written)"
                    .to_string(),
            },
        ));
    }
    Ok(store)
}

fn all_items(store: &dyn AttributedStore) -> Result<Vec<Item>, NoteError> {
    store
        .query(&ItemQuery {
            schema: Some(schema()),
            sort: vec![SortDescriptor {
                field: "created".into(),
                ascending: true,
            }],
            ..Default::default()
        })
        .map_err(NoteError::store)
}

fn get(store: &dyn AttributedStore, id: ItemId) -> Result<Item, NoteError> {
    match store.get(id).map_err(NoteError::store)? {
        Some(item) if item.schema.as_str() == SCHEMA_REF => Ok(item),
        _ => Err(NoteError::new("not-found", format!("no note {id}"))),
    }
}

/// Every note, oldest first.
pub fn list() -> Result<Vec<Note>, NoteError> {
    let store = store(false)?;
    Ok(all_items(store.as_ref())?.iter().map(note_of).collect())
}

/// How many notes there are.
pub fn count() -> Result<usize, NoteError> {
    let store = store(false)?;
    store
        .count(&ItemQuery {
            schema: Some(schema()),
            ..Default::default()
        })
        .map_err(NoteError::store)
}

/// The parent a new or moved note goes under: `None` for the root, else the
/// parent note (refused `not-found` when there is none).
fn parent_of(store: &dyn AttributedStore, parent: Option<&str>) -> Result<Option<Item>, NoteError> {
    match parent.map(str::trim).filter(|p| !p.is_empty()) {
        None => Ok(None),
        Some(parent) => get(store, resolve_id(parent))
            .map(Some)
            .map_err(|_| NoteError::new("not-found", format!("no parent note {parent}"))),
    }
}

/// Add a note titled `title` under `parent` (a key or a record uuid; `None`
/// for a root), keyed `key` or a fresh key. Refuses an empty title
/// (`invalid-argument`), a missing parent (`not-found`) and a key already
/// taken (`conflict`).
pub fn add(parent: Option<&str>, title: &str, key: Option<&str>) -> Result<Note, NoteError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(NoteError::new("invalid-argument", "a note needs a title"));
    }
    let store = store(true)?;
    let parent = parent_of(store.as_ref(), parent)?;
    let key = key
        .map(|k| k.trim().to_owned())
        .filter(|k| !k.is_empty())
        .unwrap_or_else(fresh_key);
    let id = record_id(&key);
    if store.get(id).map_err(NoteError::store)?.is_some() {
        return Err(NoteError::new(
            "conflict",
            format!("a note keyed {key} already exists"),
        ));
    }
    let mut payload = BTreeMap::new();
    payload.insert("id".to_owned(), Value::String(key.clone()));
    payload.insert("title".to_owned(), Value::String(title.to_owned()));
    payload.insert(
        "parent".to_owned(),
        match &parent {
            Some(p) => Value::String(note_of(p).key),
            None => Value::Null,
        },
    );
    let now = chrono::Utc::now();
    let item = Item {
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
        parent: parent.as_ref().map(|p| p.id),
    };
    store.insert(item.clone()).map_err(NoteError::store)?;
    Ok(note_of(&item))
}

/// Retitle a note. Refuses an empty title and an unknown note.
pub fn rename(id: &str, title: &str) -> Result<Note, NoteError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(NoteError::new("invalid-argument", "a note needs a title"));
    }
    let store = store(true)?;
    let record = resolve_id(id);
    let mut item = get(store.as_ref(), record)?;
    store
        .update(
            record,
            vec![FieldMutation::SetPayload(
                "title".into(),
                Value::String(title.to_owned()),
            )],
        )
        .map_err(NoteError::store)?;
    item.payload
        .insert("title".into(), Value::String(title.to_owned()));
    Ok(note_of(&item))
}

/// Move a note under `parent` (`None` for the root). Refuses an unknown
/// note or parent (`not-found`) and a move under itself or a descendant
/// (`cycle`).
pub fn reparent(id: &str, parent: Option<&str>) -> Result<Note, NoteError> {
    let store = store(true)?;
    let record = resolve_id(id);
    let mut item = get(store.as_ref(), record)?;
    let parent = parent_of(store.as_ref(), parent)?;
    // Walk up from the new parent: reaching the note itself would close a
    // cycle.
    let mut cursor = parent.as_ref().map(|p| p.id);
    let mut hops = 0;
    while let Some(at) = cursor {
        if at == record {
            return Err(NoteError::new(
                "cycle",
                "a note cannot be moved under itself or one of its descendants",
            ));
        }
        hops += 1;
        if hops > 10_000 {
            break;
        }
        cursor = store
            .get(at)
            .map_err(NoteError::store)?
            .and_then(|i| i.parent);
    }
    let parent_key = parent.as_ref().map(|p| note_of(p).key);
    store
        .update(
            record,
            vec![
                FieldMutation::SetPayload(
                    "parent".into(),
                    match &parent_key {
                        Some(k) => Value::String(k.clone()),
                        None => Value::Null,
                    },
                ),
                FieldMutation::SetParent(parent.as_ref().map(|p| p.id)),
            ],
        )
        .map_err(NoteError::store)?;
    item.payload.insert(
        "parent".into(),
        match &parent_key {
            Some(k) => Value::String(k.clone()),
            None => Value::Null,
        },
    );
    item.parent = parent.as_ref().map(|p| p.id);
    Ok(note_of(&item))
}

/// Delete a note and every note under it. Answers the deleted keys, the
/// note itself first. Refuses an unknown note.
pub fn delete(id: &str) -> Result<Vec<String>, NoteError> {
    let store = store(true)?;
    let record = resolve_id(id);
    let root = get(store.as_ref(), record)?;
    let all = all_items(store.as_ref())?;
    let mut doomed: Vec<Item> = vec![root];
    let mut frontier = vec![record];
    while let Some(parent) = frontier.pop() {
        for child in all.iter().filter(|i| i.parent == Some(parent)) {
            if doomed.iter().any(|d| d.id == child.id) {
                continue; // a cycle in stored data: cut it here
            }
            doomed.push(child.clone());
            frontier.push(child.id);
        }
    }
    let mut keys = Vec::with_capacity(doomed.len());
    for item in &doomed {
        store.delete(item.id).map_err(NoteError::store)?;
        keys.push(note_of(item).key);
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_record_id_is_the_materialisers_uuid5() {
        // RFC 4122 § 4.3: the same name under the same namespace is the same
        // id, and it is a version-5 uuid.
        let a = record_id("research");
        assert_eq!(a, record_id("research"));
        assert_ne!(a, record_id("reading"));
        assert_eq!(a.get_version_num(), 5);
        assert_eq!(resolve_id("research"), a);
        assert_eq!(resolve_id(&a.to_string()), a);
    }

    #[test]
    fn the_schema_ref_is_the_kind_spelled_bare() {
        // `koine_host::manifest::from_inventory` maps a `returns_kind` to
        // `[kind]`; a query over `note` therefore reads the ref `note`.
        assert_eq!(SCHEMA_REF, KIND);
    }

    #[test]
    fn a_fresh_key_is_short_and_distinct() {
        let a = fresh_key();
        let b = fresh_key();
        assert!(a.starts_with("n-") && a.len() == 10, "{a}");
        assert_ne!(a, b);
    }
}
