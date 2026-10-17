//! The data of a [`crate::MemoryStore`] and every transition on it.
//!
//! Everything here is a plain method on [`State`] with no locking, so a
//! caller that needs all-or-nothing (a guarded batch) clones the state,
//! runs the transitions on the clone and swaps it in only if all succeeded.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, TimeZone, Utc};
use impress_item::{
    build_operation_payload, routine_retention_for_schema, schema::refs::CORE_OPERATION, EdgeType,
    Item, ItemId, OperationIntent, OperationSpec, OperationType, Priority, RetentionTier,
    StoreError, TypedReference, Value, Visibility,
};
use uuid::Uuid;

use crate::policy::WritePolicy;

/// One stored row: an ordinary item, or an operation (an item whose schema is
/// `core/operation` and which names the item it was applied to).
#[derive(Debug, Clone)]
pub(crate) struct Row {
    pub item: Item,
    /// Insertion order: the tie-break for queries, since the id is random.
    pub seq: u64,
    /// `Some(target)` for an operation row.
    pub op_target: Option<ItemId>,
    /// An operation's retention tier (not observable through the port).
    pub retention: Option<RetentionTier>,
}

/// What the transitions need from the store that owns the state.
pub(crate) struct Env<'a> {
    pub policy: &'a dyn WritePolicy,
    pub origin: &'a str,
    /// Wall clock in epoch milliseconds, for the hybrid logical clock.
    pub wall_ms: &'a dyn Fn() -> u64,
}

/// What `apply_operation` did.
pub(crate) struct Applied {
    pub operation_id: ItemId,
    pub clock: u64,
    pub target_schema: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct State {
    pub rows: BTreeMap<ItemId, Row>,
    /// The hybrid logical clock: the last `(wall_ms, counter)` answered.
    wall_ms: u64,
    counter: u64,
    next_seq: u64,
}

/// The largest counter that fits the low 16 bits of a packed clock.
const COUNTER_MAX: u64 = 0xFFFF;

/// A `DateTime` at millisecond precision, the precision the oracle keeps.
fn now() -> DateTime<Utc> {
    Utc.timestamp_millis_opt(Utc::now().timestamp_millis())
        .single()
        .unwrap_or_else(Utc::now)
}

/// The sort key of a reference: its target, then the edge's JSON, which is
/// the order of the oracle's `(source, target, edge_type)` primary key.
fn reference_key(r: &TypedReference) -> (String, String) {
    (
        r.target.to_string(),
        serde_json::to_string(&r.edge_type).unwrap_or_default(),
    )
}

fn same_edge(r: &TypedReference, target: ItemId, edge: &EdgeType) -> bool {
    r.target == target && &r.edge_type == edge
}

impl State {
    // -- the clock ---------------------------------------------------------

    /// The next clock: `(wall_ms << 16) | counter`, never the same twice.
    /// The counter counts within one millisecond. If it would overflow its 16
    /// bits the wall component moves on by one (the oracle would wrap and
    /// answer a clock it had already given).
    pub fn next_clock(&mut self, env: &Env<'_>) -> u64 {
        let now = (env.wall_ms)();
        let (wall, counter) = if now > self.wall_ms {
            (now, 0)
        } else if self.counter >= COUNTER_MAX {
            (self.wall_ms + 1, 0)
        } else {
            (self.wall_ms, self.counter + 1)
        };
        self.wall_ms = wall;
        self.counter = counter;
        (wall << 16) | counter
    }

    /// The clock as it stands: the last value `next_clock` answered, or what
    /// `observe_clock` raised it to. `0` before the first tick.
    pub fn clock_now(&self) -> u64 {
        (self.wall_ms << 16) | self.counter
    }

    /// Raise the clock so every later answer sorts after `remote` (the
    /// oracle's `hlc_observe_remote`): component-wise maximum, a no-op for a
    /// clock that is behind.
    pub fn observe_clock(&mut self, remote: u64) {
        let (wall, counter) = (remote >> 16, remote & COUNTER_MAX);
        if wall > self.wall_ms {
            self.wall_ms = wall;
            self.counter = counter;
        } else if wall == self.wall_ms {
            self.counter = self.counter.max(counter);
        }
    }

    pub fn clock_of(&self, id: ItemId) -> Option<u64> {
        self.rows.get(&id).map(|r| r.item.logical_clock)
    }

    // -- inserting ---------------------------------------------------------

    /// Refuse a batch that cannot be inserted as a whole: a duplicate id
    /// (against the store or within the batch), or a reference or parent that
    /// names an item neither stored nor in the batch (the oracle's foreign
    /// keys). Nothing is changed.
    pub fn validate_new(&self, items: &[Item]) -> Result<(), StoreError> {
        let mut incoming = BTreeSet::new();
        for item in items {
            if self.rows.contains_key(&item.id) || !incoming.insert(item.id) {
                return Err(StoreError::AlreadyExists(item.id));
            }
        }
        let known = |id: &ItemId| self.rows.contains_key(id) || incoming.contains(id);
        for item in items {
            if let Some(parent) = item.parent.filter(|p| !known(p)) {
                return Err(dangling("parent", item.id, parent));
            }
            if let Some(r) = item.references.iter().find(|r| !known(&r.target)) {
                return Err(dangling("reference", item.id, r.target));
            }
        }
        Ok(())
    }

    /// Insert an item already passed through [`validate_new`](Self::validate_new).
    /// Stamps a zero clock, fills a missing origin, and normalizes the tag and
    /// reference sets the way the oracle's tables hold them (a set of tags in
    /// path order; one reference per `(target, edge)`, the first one kept).
    /// Returns the stored item.
    pub fn insert_validated(&mut self, mut item: Item, env: &Env<'_>) -> Item {
        if item.logical_clock == 0 {
            item.logical_clock = self.next_clock(env);
        }
        if item.origin.is_none() {
            item.origin = Some(env.origin.to_owned());
        }
        item.tags.sort();
        item.tags.dedup();
        let mut seen = Vec::new();
        item.references.retain(|r| {
            let fresh = !seen.iter().any(|(t, e)| same_edge(r, *t, e));
            if fresh {
                seen.push((r.target, r.edge_type.clone()));
            }
            fresh
        });
        item.references.sort_by_key(reference_key);
        self.put(item.clone(), None, None);
        item
    }

    fn put(&mut self, item: Item, op_target: Option<ItemId>, retention: Option<RetentionTier>) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.rows.insert(
            item.id,
            Row {
                item,
                seq,
                op_target,
                retention,
            },
        );
    }

    // -- deleting ----------------------------------------------------------

    /// Delete an item and what the oracle's foreign keys delete with it: the
    /// operations that targeted it, and every reference *to* it from other
    /// rows; a child's parent pointer is cleared. Other rows' clocks and
    /// `modified` are not touched. Returns the schema of the deleted row.
    pub fn delete(&mut self, id: ItemId) -> Result<String, StoreError> {
        let row = self.rows.remove(&id).ok_or(StoreError::NotFound(id))?;
        self.rows.retain(|_, r| r.op_target != Some(id));
        for r in self.rows.values_mut() {
            r.item.references.retain(|x| x.target != id);
            if r.item.parent == Some(id) {
                r.item.parent = None;
            }
        }
        Ok(row.item.schema.as_str().to_owned())
    }

    // -- operations --------------------------------------------------------

    /// Everything that can make `spec` fail, checked without changing
    /// anything: the target exists, the policy accepts, and a reference or
    /// parent it sets names a row that exists.
    pub fn preflight(&self, spec: &OperationSpec, env: &Env<'_>) -> Result<(), StoreError> {
        let target = self
            .rows
            .get(&spec.target_id)
            .ok_or(StoreError::NotFound(spec.target_id))?;
        env.policy.check(&target.item, &spec.op_type)?;
        match &spec.op_type {
            OperationType::AddReference(r) if !self.rows.contains_key(&r.target) => {
                Err(dangling("reference", spec.target_id, r.target))
            }
            OperationType::SetParent(Some(parent)) if !self.rows.contains_key(parent) => {
                Err(dangling("parent", spec.target_id, *parent))
            }
            _ => Ok(()),
        }
    }

    /// Apply one operation: record the operation row and materialize the
    /// change on the target, stamping both with one new clock. Either all of
    /// it happens or (on error) none of it.
    pub fn apply_operation(
        &mut self,
        spec: &OperationSpec,
        env: &Env<'_>,
    ) -> Result<Applied, StoreError> {
        self.preflight(spec, env)?;
        let target_schema = self.rows[&spec.target_id].item.schema.as_str().to_owned();
        let prev = capture_previous(&self.rows[&spec.target_id].item, &spec.op_type);
        let clock = self.next_clock(env);

        let at = now();
        let operation_id = Uuid::new_v4();
        let op_item = Item {
            id: operation_id,
            schema: CORE_OPERATION,
            payload: build_operation_payload(
                spec.target_id,
                &spec.op_type,
                spec.intent,
                spec.reason.as_deref(),
                prev,
            ),
            created: at,
            modified: at,
            author: spec.author.clone(),
            author_kind: spec.author_kind,
            logical_clock: clock,
            origin: Some(env.origin.to_owned()),
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
            batch_id: spec.batch_id.clone().or_else(impress_item::ambient_batch),
            references: Vec::new(),
            parent: None,
        };
        // The routine mechanical artery is steered by the target's kind;
        // an explicit tier from a user verb is left alone.
        let retention = if spec.intent == OperationIntent::Routine
            && spec.retention == RetentionTier::Compactable
        {
            routine_retention_for_schema(&target_schema)
        } else {
            spec.retention
        };
        self.put(op_item, Some(spec.target_id), Some(retention));

        let target = &mut self
            .rows
            .get_mut(&spec.target_id)
            .expect("preflight found the target")
            .item;
        materialize(target, &spec.op_type, at, clock);
        Ok(Applied {
            operation_id,
            clock,
            target_schema,
        })
    }
}

fn dangling(what: &str, from: ItemId, to: ItemId) -> StoreError {
    StoreError::Validation(format!(
        "item {from} has a {what} to {to}, which is not in the store"
    ))
}

/// The change an operation makes to its target. Every operation stamps the
/// target with the operation's clock; all but the two reference operations
/// also move `modified` (the oracle's rule, kept as it is).
fn materialize(target: &mut Item, op: &OperationType, at: DateTime<Utc>, clock: u64) {
    target.logical_clock = clock;
    match op {
        OperationType::SetPayload(field, value) => {
            target.payload.insert(field.clone(), value.clone());
        }
        OperationType::RemovePayload(field) => {
            target.payload.remove(field);
        }
        OperationType::PatchPayload(fields) => {
            for (field, value) in fields {
                target.payload.insert(field.clone(), value.clone());
            }
        }
        OperationType::SetRead(v) => target.is_read = *v,
        OperationType::SetStarred(v) => target.is_starred = *v,
        OperationType::SetFlag(flag) => target.flag.clone_from(flag),
        OperationType::AddTag(tag) => {
            if let Err(at) = target.tags.binary_search(tag) {
                target.tags.insert(at, tag.clone());
            }
        }
        OperationType::RemoveTag(tag) => target.tags.retain(|t| t != tag),
        OperationType::SetPriority(p) => target.priority = *p,
        OperationType::SetVisibility(v) => target.visibility = *v,
        OperationType::SetParent(parent) => target.parent = *parent,
        OperationType::Custom(_, _) => {}
        OperationType::AddReference(r) => {
            if !target
                .references
                .iter()
                .any(|x| same_edge(x, r.target, &r.edge_type))
            {
                target.references.push(r.clone());
                target.references.sort_by_key(reference_key);
            }
            return;
        }
        OperationType::RemoveReference(to, edge) => {
            target.references.retain(|x| !same_edge(x, *to, edge));
            return;
        }
    }
    target.modified = at;
}

/// The field's value as the oracle captures it for undo (`json_extract`):
/// absent and `null` are `Null`, booleans come back as the integers 1/0, and
/// a string that looks like JSON is parsed.
fn captured_field(payload: &BTreeMap<String, Value>, field: &str) -> Value {
    match payload.get(field) {
        None | Some(Value::Null) => Value::Null,
        Some(Value::Bool(b)) => Value::Int(i64::from(*b)),
        Some(Value::String(s)) if s.starts_with('{') || s.starts_with('[') => {
            serde_json::from_str::<Value>(s).unwrap_or_else(|_| Value::String(s.clone()))
        }
        Some(other) => other.clone(),
    }
}

/// The value the operation is about to overwrite (the `prev` of its payload).
fn capture_previous(target: &Item, op: &OperationType) -> Option<Value> {
    match op {
        OperationType::SetRead(_) => Some(Value::Bool(target.is_read)),
        OperationType::SetStarred(_) => Some(Value::Bool(target.is_starred)),
        OperationType::SetFlag(_) => Some(match &target.flag {
            None => Value::Null,
            Some(f) => {
                let mut m = BTreeMap::new();
                m.insert("color".into(), Value::String(f.color.clone()));
                if let Some(s) = &f.style {
                    m.insert("style".into(), Value::String(s.clone()));
                }
                if let Some(l) = &f.length {
                    m.insert("length".into(), Value::String(l.clone()));
                }
                Value::Object(m)
            }
        }),
        OperationType::SetPriority(_) => Some(Value::String(target.priority.to_string())),
        OperationType::SetVisibility(_) => Some(Value::String(target.visibility.to_string())),
        OperationType::SetPayload(field, _) | OperationType::RemovePayload(field) => {
            Some(captured_field(&target.payload, field))
        }
        OperationType::PatchPayload(fields) => Some(Value::Object(
            fields
                .keys()
                .map(|k| (k.clone(), captured_field(&target.payload, k)))
                .collect(),
        )),
        OperationType::SetParent(_) => Some(
            target
                .parent
                .map_or(Value::Null, |p| Value::String(p.to_string())),
        ),
        OperationType::AddTag(_)
        | OperationType::RemoveTag(_)
        | OperationType::AddReference(_)
        | OperationType::RemoveReference(_, _)
        | OperationType::Custom(_, _) => None,
    }
}
