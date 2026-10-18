//! A copy-on-write overlay over any backend of the store port
//! (docs/plan-koine-trust.md TR-D3).
//!
//! A rehearsed dry run (`tentatively`) runs the verb's real handler against an
//! [`OverlayStore`]: reads see the overlay laid over the base, every write
//! lands in an in-memory top layer, and the base is never written. When the
//! call ends the overlay is dropped and [`OverlayStore::diff`] says what the
//! verb would have changed: items inserted, items updated (field by field,
//! before and after), items deleted, and how many operations it would have
//! logged.
//!
//! How it reads:
//! - `get`: a deleted id is gone; the top layer's copy wins; else the base.
//! - `query`/`count`: both layers asked without `limit`/`offset`, base rows the
//!   top layer holds (or deleted) dropped, merged, sorted by the query's sort
//!   and paged. The defaults built on `query` (`search_text`,
//!   `references_to`, `operations_for`, metadata) follow from it.
//! - `neighbors`: both layers', deleted and shadowed base rows dropped.
//!
//! What it does not see: edges *from* base rows *to* a row the overlay
//! deleted stay on those base rows (the memory store would drop them), and
//! `subscribe` delivers the top layer's events only. Neither changes what a
//! rehearsal reports.
//!
//! Every write to the top layer runs with the dry-run gate open
//! ([`impress_item::ambient::with_dry_run_exempt`]); a write that reaches the
//! base by another path is refused by the base's own gate.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use impress_item::ambient::with_dry_run_exempt;
use impress_item::{
    ActorKind, AttributedStore, EdgeType, FieldMutation, GuardedBatch, GuardedWrite, Item,
    ItemEvent, ItemId, ItemQuery, ItemStore, OperationSpec, StoreCapabilities, StoreError,
};
use koine_store_memory::MemoryStore;
use serde_json::{json, Map, Value};

/// The kind of an operation row.
const OPERATION: &str = "core/operation";

/// At most this many items of each change list in a [`OverlayStore::diff`].
pub const DIFF_LIMIT: usize = 50;

/// A store whose writes go to memory over a base that is only read.
pub struct OverlayStore {
    base: Arc<dyn AttributedStore>,
    top: MemoryStore,
    state: Mutex<Layers>,
}

#[derive(Default)]
struct Layers {
    /// Every id the top layer holds: copied from the base or new.
    shadow: BTreeSet<ItemId>,
    /// Base ids copied into the top layer (the rest of `shadow` is new).
    copied: BTreeSet<ItemId>,
    /// Ids the overlay deleted (base rows, or rows it had inserted).
    deleted: BTreeSet<ItemId>,
}

impl OverlayStore {
    pub fn new(base: Arc<dyn AttributedStore>) -> Self {
        let (author, kind) = base.default_author();
        Self {
            base,
            top: MemoryStore::new().with_author(author, kind),
            state: Mutex::new(Layers::default()),
        }
    }

    /// The store this overlay reads through.
    pub fn base(&self) -> &Arc<dyn AttributedStore> {
        &self.base
    }

    fn layers(&self) -> std::sync::MutexGuard<'_, Layers> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn is_deleted(&self, id: ItemId) -> bool {
        self.layers().deleted.contains(&id)
    }

    /// Make `id` writable in the top layer: copy the base row up when the
    /// top layer does not hold it. `Ok(false)` when no layer has the row.
    fn copy_up(&self, id: ItemId) -> Result<bool, StoreError> {
        {
            let layers = self.layers();
            if layers.deleted.contains(&id) {
                return Ok(false);
            }
            if layers.shadow.contains(&id) {
                return Ok(true);
            }
        }
        let Some(item) = self.base.get(id)? else {
            return Ok(false);
        };
        with_dry_run_exempt(|| self.top.insert(item))?;
        let mut layers = self.layers();
        layers.shadow.insert(id);
        layers.copied.insert(id);
        Ok(true)
    }

    fn note_new(&self, ids: impl IntoIterator<Item = ItemId>) {
        let mut layers = self.layers();
        for id in ids {
            layers.shadow.insert(id);
            layers.deleted.remove(&id);
        }
    }

    /// Base rows of `rows` that the overlay neither holds nor deleted.
    fn visible_base(&self, rows: Vec<Item>) -> Vec<Item> {
        let layers = self.layers();
        rows.into_iter()
            .filter(|r| !layers.shadow.contains(&r.id) && !layers.deleted.contains(&r.id))
            .collect()
    }

    /// What the verb would have changed, as JSON:
    /// `{"inserted": [...], "updated": [...], "deleted": [...],
    /// "operations": N, "truncated": bool}`.
    pub fn diff(&self) -> Value {
        let (shadow, copied, deleted) = {
            let layers = self.layers();
            (
                layers.shadow.clone(),
                layers.copied.clone(),
                layers.deleted.clone(),
            )
        };
        let mut inserted = Vec::new();
        let mut updated = Vec::new();
        let mut removed = Vec::new();
        let mut operations = 0usize;
        let mut truncated = false;
        for id in &shadow {
            let Ok(Some(after)) = self.top.get(*id) else {
                continue;
            };
            if after.schema.as_str() == OPERATION {
                operations += 1;
                continue;
            }
            if copied.contains(id) {
                let before = self.base.get(*id).ok().flatten();
                let changes = before.as_ref().map(|b| field_changes(b, &after));
                if let Some(changes) = changes.filter(|c| !c.is_empty()) {
                    if updated.len() < DIFF_LIMIT {
                        updated.push(json!({
                            "id": id.to_string(),
                            "kind": after.schema.as_str(),
                            "changes": changes,
                        }));
                    } else {
                        truncated = true;
                    }
                }
            } else if inserted.len() < DIFF_LIMIT {
                inserted.push(json!({
                    "id": id.to_string(),
                    "kind": after.schema.as_str(),
                    "payload": serde_json::to_value(&after.payload).unwrap_or(Value::Null),
                }));
            } else {
                truncated = true;
            }
        }
        for id in &deleted {
            if shadow.contains(id) && !copied.contains(id) {
                continue; // inserted, then deleted: nothing would change
            }
            let Ok(Some(before)) = self.base.get(*id) else {
                continue;
            };
            if before.schema.as_str() == OPERATION {
                continue;
            }
            if removed.len() < DIFF_LIMIT {
                removed.push(json!({"id": id.to_string(), "kind": before.schema.as_str()}));
            } else {
                truncated = true;
            }
        }
        json!({
            "inserted": inserted,
            "updated": updated,
            "deleted": removed,
            "operations": operations,
            "truncated": truncated,
        })
    }
}

/// `{field: {"before", "after"}}` for every payload field, tag set, flag,
/// read/starred state, parent and reference set that differs.
fn field_changes(before: &Item, after: &Item) -> Map<String, Value> {
    let mut out = Map::new();
    let keys: BTreeSet<&String> = before.payload.keys().chain(after.payload.keys()).collect();
    for key in keys {
        let b = before.payload.get(key);
        let a = after.payload.get(key);
        if b != a {
            out.insert(
                key.clone(),
                json!({"before": serde_json::to_value(b).unwrap_or(Value::Null),
                       "after": serde_json::to_value(a).unwrap_or(Value::Null)}),
            );
        }
    }
    let mut envelope = |name: &str, b: Value, a: Value| {
        if b != a {
            out.insert(format!("_{name}"), json!({"before": b, "after": a}));
        }
    };
    let mut tags_b = before.tags.clone();
    let mut tags_a = after.tags.clone();
    tags_b.sort();
    tags_a.sort();
    envelope("tags", json!(tags_b), json!(tags_a));
    envelope(
        "flag",
        serde_json::to_value(&before.flag).unwrap_or(Value::Null),
        serde_json::to_value(&after.flag).unwrap_or(Value::Null),
    );
    envelope("is_read", json!(before.is_read), json!(after.is_read));
    envelope(
        "is_starred",
        json!(before.is_starred),
        json!(after.is_starred),
    );
    envelope(
        "parent",
        json!(before.parent.map(|p| p.to_string())),
        json!(after.parent.map(|p| p.to_string())),
    );
    let refs = |item: &Item| {
        let mut r: Vec<String> = item
            .references
            .iter()
            .map(|r| format!("{:?}->{}", r.edge_type, r.target))
            .collect();
        r.sort();
        json!(r)
    };
    envelope("references", refs(before), refs(after));
    out
}

impl ItemStore for OverlayStore {
    fn capabilities(&self) -> StoreCapabilities {
        let mut caps = self.top.capabilities();
        caps.arrival_feed = false;
        // Blobs are not part of the overlay: a rehearsal reports items.
        caps.blobs = false;
        caps
    }

    fn default_author(&self) -> (String, ActorKind) {
        self.base.default_author()
    }

    fn insert(&self, item: Item) -> Result<ItemId, StoreError> {
        let id = item.id;
        if !self.is_deleted(id) && self.base.get(id)?.is_some() {
            return Err(StoreError::AlreadyExists(id));
        }
        let id = with_dry_run_exempt(|| self.top.insert(item))?;
        self.note_new([id]);
        Ok(id)
    }

    fn insert_batch(&self, items: Vec<Item>) -> Result<Vec<ItemId>, StoreError> {
        for item in &items {
            if !self.is_deleted(item.id) && self.base.get(item.id)?.is_some() {
                return Err(StoreError::AlreadyExists(item.id));
            }
        }
        let ids = with_dry_run_exempt(|| self.top.insert_batch(items))?;
        self.note_new(ids.iter().copied());
        Ok(ids)
    }

    fn get(&self, id: ItemId) -> Result<Option<Item>, StoreError> {
        let (deleted, shadowed) = {
            let layers = self.layers();
            (layers.deleted.contains(&id), layers.shadow.contains(&id))
        };
        if deleted {
            return Ok(None);
        }
        if shadowed {
            return self.top.get(id);
        }
        self.base.get(id)
    }

    fn update(&self, id: ItemId, mutations: Vec<FieldMutation>) -> Result<(), StoreError> {
        if !self.copy_up(id)? {
            return Err(StoreError::NotFound(id));
        }
        with_dry_run_exempt(|| self.top.update(id, mutations))
    }

    fn delete(&self, id: ItemId) -> Result<(), StoreError> {
        let (shadowed, deleted) = {
            let layers = self.layers();
            (layers.shadow.contains(&id), layers.deleted.contains(&id))
        };
        if deleted {
            return Err(StoreError::NotFound(id));
        }
        if shadowed {
            with_dry_run_exempt(|| self.top.delete(id))?;
        } else if self.base.get(id)?.is_none() {
            return Err(StoreError::NotFound(id));
        }
        self.layers().deleted.insert(id);
        Ok(())
    }

    fn query(&self, q: &ItemQuery) -> Result<Vec<Item>, StoreError> {
        let mut unpaged = q.clone();
        unpaged.limit = None;
        unpaged.offset = None;
        let mut rows = self.top.query(&unpaged)?;
        {
            let layers = self.layers();
            rows.retain(|r| !layers.deleted.contains(&r.id));
        }
        rows.extend(self.visible_base(self.base.query(&unpaged)?));
        if !q.sort.is_empty() {
            rows.sort_by(|a, b| koine_store_memory::eval::compare_items(a, b, &q.sort));
        }
        // An offset applies only with a limit, as every backend reads it.
        Ok(match q.limit {
            Some(limit) => rows
                .into_iter()
                .skip(q.offset.unwrap_or(0))
                .take(limit)
                .collect(),
            None => rows,
        })
    }

    fn count(&self, q: &ItemQuery) -> Result<usize, StoreError> {
        let mut unpaged = q.clone();
        unpaged.limit = None;
        unpaged.offset = None;
        Ok(self.query(&unpaged)?.len())
    }

    fn neighbors(
        &self,
        id: ItemId,
        edge_types: &[EdgeType],
        depth: u32,
    ) -> Result<Vec<Item>, StoreError> {
        let mut seen = BTreeMap::new();
        for item in self.top.neighbors(id, edge_types, depth)? {
            seen.insert(item.id, item);
        }
        for item in self.visible_base(self.base.neighbors(id, edge_types, depth)?) {
            seen.entry(item.id).or_insert(item);
        }
        let deleted = self.layers().deleted.clone();
        Ok(seen
            .into_values()
            .filter(|i| !deleted.contains(&i.id))
            .collect())
    }

    fn subscribe(&self, q: ItemQuery) -> Result<Receiver<ItemEvent>, StoreError> {
        self.top.subscribe(q)
    }

    fn change_token(&self) -> Result<u64, StoreError> {
        Ok(self
            .base
            .change_token()?
            .wrapping_add(self.top.change_token()?))
    }
}

impl AttributedStore for OverlayStore {
    fn apply_operation(&self, spec: OperationSpec) -> Result<ItemId, StoreError> {
        if !self.copy_up(spec.target_id)? {
            return Err(StoreError::NotFound(spec.target_id));
        }
        let id = with_dry_run_exempt(|| self.top.apply_operation(spec))?;
        self.note_new([id]);
        Ok(id)
    }

    fn apply_operation_if_clock(
        &self,
        spec: OperationSpec,
        expected_clock: u64,
    ) -> Result<GuardedWrite, StoreError> {
        if !self.copy_up(spec.target_id)? {
            return Ok(GuardedWrite::Moved { clock: None });
        }
        let written =
            with_dry_run_exempt(|| self.top.apply_operation_if_clock(spec, expected_clock))?;
        if let GuardedWrite::Applied { operation_id, .. } = &written {
            self.note_new([*operation_id]);
        }
        Ok(written)
    }

    fn apply_operations_if_clock(
        &self,
        specs: Vec<OperationSpec>,
        guard: ItemId,
        expected_clock: u64,
    ) -> Result<GuardedBatch, StoreError> {
        if specs.is_empty() {
            // The top layer answers an empty batch the way every store must.
            return with_dry_run_exempt(|| {
                self.top
                    .apply_operations_if_clock(specs, guard, expected_clock)
            });
        }
        if !self.copy_up(guard)? {
            return Ok(GuardedBatch::Moved { clock: None });
        }
        for spec in &specs {
            if spec.target_id != guard && !self.copy_up(spec.target_id)? {
                return Err(StoreError::NotFound(spec.target_id));
            }
        }
        let written = with_dry_run_exempt(|| {
            self.top
                .apply_operations_if_clock(specs, guard, expected_clock)
        })?;
        if let GuardedBatch::Applied { operation_ids, .. } = &written {
            self.note_new(operation_ids.iter().copied());
        }
        Ok(written)
    }

    fn logical_clock_of(&self, id: ItemId) -> Result<Option<u64>, StoreError> {
        let (deleted, shadowed) = {
            let layers = self.layers();
            (layers.deleted.contains(&id), layers.shadow.contains(&id))
        };
        if deleted {
            return Ok(None);
        }
        if shadowed {
            return self.top.logical_clock_of(id);
        }
        self.base.logical_clock_of(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use impress_item::{Priority, SchemaRef, Visibility};

    fn item(title: &str) -> Item {
        let now = chrono::Utc::now();
        Item {
            id: uuid::Uuid::new_v4(),
            schema: SchemaRef::from_stored("paper".to_owned()),
            payload: [(
                "title".to_owned(),
                impress_item::Value::String(title.into()),
            )]
            .into(),
            created: now,
            modified: now,
            author: "human:test".into(),
            author_kind: ActorKind::Human,
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
        }
    }

    #[test]
    fn writes_land_in_the_overlay_and_the_diff_says_what_would_change() {
        let base = Arc::new(MemoryStore::new());
        let kept = base.insert(item("kept")).unwrap();
        let edited = base.insert(item("old title")).unwrap();
        let gone = base.insert(item("gone")).unwrap();
        let before = base.change_token().unwrap();

        let overlay = OverlayStore::new(base.clone());
        let new = overlay.insert(item("new")).unwrap();
        overlay
            .update(
                edited,
                vec![FieldMutation::SetPayload(
                    "title".into(),
                    impress_item::Value::String("new title".into()),
                )],
            )
            .unwrap();
        overlay.delete(gone).unwrap();

        // The overlay reads as if it had happened.
        assert!(overlay.get(gone).unwrap().is_none());
        assert!(overlay.get(new).unwrap().is_some());
        assert!(overlay.get(kept).unwrap().is_some());
        let titles: BTreeSet<String> = overlay
            .query(&ItemQuery::default())
            .unwrap()
            .into_iter()
            .filter_map(|i| match i.payload.get("title") {
                Some(impress_item::Value::String(s)) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            titles,
            ["kept", "new", "new title"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );

        // The base did not move.
        assert_eq!(base.change_token().unwrap(), before);
        assert!(base.get(gone).unwrap().is_some());
        assert!(base.get(new).unwrap().is_none());

        let diff = overlay.diff();
        assert_eq!(diff["inserted"][0]["id"], new.to_string());
        assert_eq!(diff["updated"][0]["id"], edited.to_string());
        assert_eq!(
            diff["updated"][0]["changes"]["title"]["before"],
            "old title"
        );
        assert_eq!(diff["updated"][0]["changes"]["title"]["after"], "new title");
        assert_eq!(diff["deleted"][0]["id"], gone.to_string());
        assert_eq!(diff["truncated"], false);
    }

    #[test]
    fn paging_applies_to_the_merged_rows() {
        let base = Arc::new(MemoryStore::new());
        for t in ["a", "c", "e"] {
            base.insert(item(t)).unwrap();
        }
        let overlay = OverlayStore::new(base);
        for t in ["b", "d"] {
            overlay.insert(item(t)).unwrap();
        }
        let page = overlay
            .query(&ItemQuery {
                sort: vec![impress_item::SortDescriptor {
                    field: "title".into(),
                    ascending: true,
                }],
                offset: Some(1),
                limit: Some(3),
                ..Default::default()
            })
            .unwrap();
        let titles: Vec<String> = page
            .iter()
            .map(|i| match i.payload.get("title") {
                Some(impress_item::Value::String(s)) => s.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(titles, ["b", "c", "d"]);
        assert_eq!(overlay.count(&ItemQuery::default()).unwrap(), 5);
    }
}
