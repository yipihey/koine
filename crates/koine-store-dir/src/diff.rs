//! What changed between two states of one item, as [`FieldMutation`]s.
//!
//! The watcher (`watch.rs`) holds the last state of each item it has seen and,
//! on a change to the file, tells its subscribers the mutations that take the
//! old state to the new one; the merge policy does the same for the item it
//! writes over the file. Replaying the result on `old` through `materialize`
//! gives `new`'s payload, tags, references, flag, read/starred state and
//! parent. Priority and visibility have no [`FieldMutation`] (the port's
//! mutation vocabulary is the oracle's, and its `update` cannot set them), so
//! a change to either is not described here.

use impress_item::{FieldMutation, Item};

/// The mutations that take `old` to `new`, in a fixed order: payload keys
/// (set, then removed, each in key order), tags, references (removed, then
/// added), the flag, read, starred, the parent.
///
/// A reference whose target and edge are unchanged but whose metadata moved is
/// a removal and an addition, because `AddReference` of an edge that exists is
/// ignored. Tags and references are compared as sets.
pub fn diff_items(old: &Item, new: &Item) -> Vec<FieldMutation> {
    let mut out = Vec::new();

    for (key, value) in &new.payload {
        if old.payload.get(key) != Some(value) {
            out.push(FieldMutation::SetPayload(key.clone(), value.clone()));
        }
    }
    for key in old.payload.keys() {
        if !new.payload.contains_key(key) {
            out.push(FieldMutation::RemovePayload(key.clone()));
        }
    }

    for tag in &new.tags {
        if !old.tags.contains(tag) {
            out.push(FieldMutation::AddTag(tag.clone()));
        }
    }
    for tag in &old.tags {
        if !new.tags.contains(tag) {
            out.push(FieldMutation::RemoveTag(tag.clone()));
        }
    }

    for r in &old.references {
        if !new.references.contains(r) {
            out.push(FieldMutation::RemoveReference(
                r.target,
                r.edge_type.clone(),
            ));
        }
    }
    for r in &new.references {
        // Absent, or present with other metadata (removed just above).
        if !old.references.contains(r) {
            out.push(FieldMutation::AddReference(r.clone()));
        }
    }

    if old.flag != new.flag {
        out.push(FieldMutation::SetFlag(new.flag.clone()));
    }
    if old.is_read != new.is_read {
        out.push(FieldMutation::SetRead(new.is_read));
    }
    if old.is_starred != new.is_starred {
        out.push(FieldMutation::SetStarred(new.is_starred));
    }
    if old.parent != new.parent {
        out.push(FieldMutation::SetParent(new.parent));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use impress_item::{
        ActorKind, EdgeType, FlagState, Priority, SchemaRef, TypedReference, Value, Visibility,
    };
    use std::collections::BTreeMap;
    use uuid::Uuid;

    fn item() -> Item {
        Item {
            id: Uuid::new_v4(),
            schema: SchemaRef::from_stored("paper".into()),
            payload: BTreeMap::new(),
            created: Utc::now(),
            modified: Utc::now(),
            author: "a".into(),
            author_kind: ActorKind::Human,
            logical_clock: 1,
            origin: None,
            canonical_id: None,
            tags: vec![],
            flag: None,
            is_read: false,
            is_starred: false,
            priority: Priority::Normal,
            visibility: Visibility::Private,
            message_type: None,
            produced_by: None,
            version: None,
            batch_id: None,
            references: vec![],
            parent: None,
            extra: Default::default(),
        }
    }

    fn cites(target: Uuid, note: Option<&str>) -> TypedReference {
        TypedReference {
            target,
            edge_type: EdgeType::Cites,
            metadata: note.map(|n| {
                let mut m = BTreeMap::new();
                m.insert("note".into(), Value::String(n.into()));
                m
            }),
        }
    }

    #[test]
    fn identical_items_have_no_difference() {
        let a = item();
        assert!(diff_items(&a, &a.clone()).is_empty());
    }

    #[test]
    fn payload_is_set_and_removed() {
        let mut old = item();
        old.payload.insert("keep".into(), Value::Int(1));
        old.payload.insert("change".into(), Value::Int(1));
        old.payload.insert("drop".into(), Value::Int(1));
        let mut new = old.clone();
        new.payload.insert("change".into(), Value::Int(2));
        new.payload.insert("add".into(), Value::Bool(true));
        new.payload.remove("drop");
        assert_eq!(
            diff_items(&old, &new),
            vec![
                FieldMutation::SetPayload("add".into(), Value::Bool(true)),
                FieldMutation::SetPayload("change".into(), Value::Int(2)),
                FieldMutation::RemovePayload("drop".into()),
            ]
        );
    }

    #[test]
    fn tags_are_compared_as_a_set() {
        let mut old = item();
        old.tags = vec!["a".into(), "b".into()];
        let mut new = old.clone();
        new.tags = vec!["c".into(), "b".into()];
        assert_eq!(
            diff_items(&old, &new),
            vec![
                FieldMutation::AddTag("c".into()),
                FieldMutation::RemoveTag("a".into())
            ]
        );
    }

    #[test]
    fn references_are_added_removed_and_re_added_when_their_metadata_moves() {
        let (x, y, z) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut old = item();
        old.references = vec![cites(x, None), cites(y, Some("old"))];
        let mut new = old.clone();
        new.references = vec![cites(y, Some("new")), cites(z, None)];
        assert_eq!(
            diff_items(&old, &new),
            vec![
                FieldMutation::RemoveReference(x, EdgeType::Cites),
                FieldMutation::RemoveReference(y, EdgeType::Cites),
                FieldMutation::AddReference(cites(y, Some("new"))),
                FieldMutation::AddReference(cites(z, None)),
            ]
        );
    }

    #[test]
    fn flag_read_starred_and_parent() {
        let parent = Uuid::new_v4();
        let old = item();
        let mut new = old.clone();
        new.flag = Some(FlagState {
            color: "red".into(),
            style: None,
            length: None,
        });
        new.is_read = true;
        new.is_starred = true;
        new.parent = Some(parent);
        assert_eq!(
            diff_items(&old, &new),
            vec![
                FieldMutation::SetFlag(new.flag.clone()),
                FieldMutation::SetRead(true),
                FieldMutation::SetStarred(true),
                FieldMutation::SetParent(Some(parent)),
            ]
        );
        assert_eq!(
            diff_items(&new, &old),
            vec![
                FieldMutation::SetFlag(None),
                FieldMutation::SetRead(false),
                FieldMutation::SetStarred(false),
                FieldMutation::SetParent(None),
            ]
        );
    }

    #[test]
    fn priority_and_visibility_have_no_mutation() {
        let old = item();
        let mut new = old.clone();
        new.priority = Priority::High;
        new.visibility = Visibility::Public;
        assert!(diff_items(&old, &new).is_empty());
    }
}
