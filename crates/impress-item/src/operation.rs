use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::item::{ActorId, ActorKind, FlagState, ItemId, Priority, Value, Visibility};
use crate::reference::{EdgeType, TypedReference};
use crate::store::FieldMutation;

/// The type of operation being applied to a target item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OperationType {
    AddTag(String),
    RemoveTag(String),
    SetFlag(Option<FlagState>),
    SetRead(bool),
    SetStarred(bool),
    SetPriority(Priority),
    SetVisibility(Visibility),
    SetPayload(String, Value),
    RemovePayload(String),
    PatchPayload(BTreeMap<String, Value>),
    AddReference(TypedReference),
    RemoveReference(ItemId, EdgeType),
    SetParent(Option<ItemId>),
    Custom(String, Value),
}

impl From<FieldMutation> for OperationType {
    fn from(mutation: FieldMutation) -> Self {
        match mutation {
            FieldMutation::SetPayload(field, value) => OperationType::SetPayload(field, value),
            FieldMutation::RemovePayload(field) => OperationType::RemovePayload(field),
            FieldMutation::SetRead(v) => OperationType::SetRead(v),
            FieldMutation::SetStarred(v) => OperationType::SetStarred(v),
            FieldMutation::SetFlag(flag) => OperationType::SetFlag(flag),
            FieldMutation::AddTag(tag) => OperationType::AddTag(tag),
            FieldMutation::RemoveTag(tag) => OperationType::RemoveTag(tag),
            FieldMutation::AddReference(r) => OperationType::AddReference(r),
            FieldMutation::RemoveReference(id, edge) => OperationType::RemoveReference(id, edge),
            FieldMutation::SetParent(parent) => OperationType::SetParent(parent),
        }
    }
}

/// Retention tier for ROUTINE operations targeting `schema` — the per-kind
/// policy, declared once instead of at every call site.
///
/// Only the mechanical-writer artery consults this (`apply_operation` steers
/// a `Routine` + `Compactable` spec through it); explicitly Durable user
/// verbs and explicitly Ephemeral callers are never overridden.
///
/// Rationale per kind:
/// - `citation-usage`: liveness telemetry (`last_seen` and friends) —
///   rewritten continuously, never worth syncing (the sync engine already
///   omits ephemeral rows) or keeping as history. This kind alone minted
///   most of the 23.0M-op backlog of 2026-08-06.
/// - everything else: `Compactable` — folded into watermark snapshots after
///   the maintenance window, which is the right default for mechanical
///   churn on real records.
pub fn routine_retention_for_schema(schema: &str) -> RetentionTier {
    match schema {
        "citation-usage" => RetentionTier::Ephemeral,
        _ => RetentionTier::Compactable,
    }
}

/// How long an operation should be retained before compaction eligibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RetentionTier {
    /// Never compacted — permanent audit trail.
    #[default]
    Durable,
    /// May be compacted after the configured window.
    Compactable,
    /// Ephemeral — may be dropped from sync payloads, not persisted long-term.
    Ephemeral,
}

/// The intent behind an operation — why was this change made?
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OperationIntent {
    /// Routine workflow action (default)
    #[default]
    Routine,
    /// Exploratory / hypothesis-driven change
    Hypothesis,
    /// Anomaly flagged by automated system
    Anomaly,
    /// Editorial decision
    Editorial,
    /// Correcting a previous error
    Correction,
    /// Escalation requiring attention
    Escalation,
}

impl std::fmt::Display for OperationIntent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OperationIntent::Routine => write!(f, "routine"),
            OperationIntent::Hypothesis => write!(f, "hypothesis"),
            OperationIntent::Anomaly => write!(f, "anomaly"),
            OperationIntent::Editorial => write!(f, "editorial"),
            OperationIntent::Correction => write!(f, "correction"),
            OperationIntent::Escalation => write!(f, "escalation"),
        }
    }
}

/// Specification for creating an operation item.
#[derive(Debug, Clone)]
pub struct OperationSpec {
    pub target_id: ItemId,
    pub op_type: OperationType,
    pub intent: OperationIntent,
    pub reason: Option<String>,
    pub batch_id: Option<String>,
    pub author: ActorId,
    pub author_kind: ActorKind,
    pub retention: RetentionTier,
}

/// The effective state of an item, either current (materialized) or
/// computed via time-travel replay.
#[derive(Debug, Clone)]
pub struct EffectiveState {
    /// The item with effective field values.
    pub tags: Vec<String>,
    pub flag: Option<FlagState>,
    pub is_read: bool,
    pub is_starred: bool,
    pub priority: Priority,
    pub visibility: Visibility,
    pub payload: BTreeMap<String, Value>,
    pub parent: Option<ItemId>,
    /// The logical clock value this state was computed at.
    pub as_of_clock: u64,
    /// The wall-clock time this state was computed at (if time-travel).
    pub as_of_time: Option<DateTime<Utc>>,
    /// Total number of operations that contributed to this state.
    pub operation_count: usize,
}

/// Specifies how to query effective state.
#[derive(Debug, Clone)]
pub enum StateAsOf {
    /// Current materialized state (fast path).
    Current,
    /// State as of a specific logical clock value.
    LogicalClock(u64),
    /// State as of a specific wall-clock time.
    Timestamp(DateTime<Utc>),
}

/// Information returned after a mutation, enabling undo/redo registration.
#[derive(Debug, Clone)]
pub struct UndoInfo {
    /// The operation IDs created by this mutation.
    pub operation_ids: Vec<ItemId>,
    /// If multiple operations share a batch, this is their shared batch ID.
    pub batch_id: Option<String>,
    /// Human-readable description for the Edit menu (e.g., "Star 3 Papers").
    pub description: String,
}

/// Summary of a group of operations for undo history display.
#[derive(Debug, Clone)]
pub struct UndoGroupSummary {
    /// Representative operation ID (first in the group).
    pub operation_id: ItemId,
    /// Batch ID if this is a grouped operation, None if single.
    pub batch_id: Option<String>,
    /// Number of operations in this group.
    pub operation_count: usize,
    /// Human-readable description ("Star 3 Papers", "Delete Paper").
    pub description: String,
    /// Timestamp of the most recent operation in the group (epoch millis).
    pub timestamp: i64,
    /// Who performed this action.
    pub author: String,
    /// Author kind: "Human", "Agent", "System".
    pub author_kind: String,
}

/// Compute the inverse operation type given the original operation and its
/// captured previous value. Returns the OperationType that undoes the original.
pub fn inverse_of(op_type: &OperationType, prev: &Value) -> Option<OperationType> {
    match op_type {
        OperationType::SetRead(_) => match prev {
            Value::Bool(v) => Some(OperationType::SetRead(*v)),
            _ => None,
        },
        OperationType::SetStarred(_) => match prev {
            Value::Bool(v) => Some(OperationType::SetStarred(*v)),
            _ => None,
        },
        OperationType::SetFlag(_) => match prev {
            Value::Null => Some(OperationType::SetFlag(None)),
            Value::Object(m) => {
                let color = match m.get("color") {
                    Some(Value::String(c)) => c.clone(),
                    _ => return None,
                };
                Some(OperationType::SetFlag(Some(FlagState {
                    color,
                    style: m.get("style").and_then(|v| {
                        if let Value::String(s) = v {
                            Some(s.clone())
                        } else {
                            None
                        }
                    }),
                    length: m.get("length").and_then(|v| {
                        if let Value::String(s) = v {
                            Some(s.clone())
                        } else {
                            None
                        }
                    }),
                })))
            }
            _ => None,
        },
        OperationType::SetPriority(_) => match prev {
            Value::String(s) => s.parse::<Priority>().ok().map(OperationType::SetPriority),
            _ => None,
        },
        OperationType::SetVisibility(_) => match prev {
            Value::String(s) => s
                .parse::<Visibility>()
                .ok()
                .map(OperationType::SetVisibility),
            _ => None,
        },
        OperationType::SetPayload(field, _) => match prev {
            Value::Null => Some(OperationType::RemovePayload(field.clone())),
            other => Some(OperationType::SetPayload(field.clone(), other.clone())),
        },
        OperationType::RemovePayload(field) => match prev {
            Value::Null => None, // field didn't exist before, nothing to undo
            other => Some(OperationType::SetPayload(field.clone(), other.clone())),
        },
        OperationType::PatchPayload(_) => match prev {
            Value::Object(prev_fields) => {
                // Restore each field to its previous value
                Some(OperationType::PatchPayload(prev_fields.clone()))
            }
            _ => None,
        },
        OperationType::SetParent(_) => match prev {
            Value::Null => Some(OperationType::SetParent(None)),
            Value::String(s) => {
                let id = s.parse::<uuid::Uuid>().ok()?;
                Some(OperationType::SetParent(Some(id)))
            }
            _ => None,
        },
        // Symmetric operations: inverse is the opposite
        OperationType::AddTag(tag) => Some(OperationType::RemoveTag(tag.clone())),
        OperationType::RemoveTag(tag) => Some(OperationType::AddTag(tag.clone())),
        OperationType::AddReference(r) => Some(OperationType::RemoveReference(
            r.target,
            r.edge_type.clone(),
        )),
        OperationType::RemoveReference(target, edge) => {
            Some(OperationType::AddReference(TypedReference {
                target: *target,
                edge_type: edge.clone(),
                metadata: None,
            }))
        }
        OperationType::Custom(_, _) => None,
    }
}

/// Generate a human-readable description of an operation for the Edit menu.
pub fn undo_description(op_type: &OperationType, count: usize) -> String {
    let noun = if count == 1 { "Paper" } else { "Papers" };
    match op_type {
        OperationType::SetRead(true) => {
            if count == 1 {
                "Mark as Read".into()
            } else {
                format!("Mark {} {} as Read", count, noun)
            }
        }
        OperationType::SetRead(false) => {
            if count == 1 {
                "Mark as Unread".into()
            } else {
                format!("Mark {} {} as Unread", count, noun)
            }
        }
        OperationType::SetStarred(true) => {
            if count == 1 {
                "Star Paper".into()
            } else {
                format!("Star {} {}", count, noun)
            }
        }
        OperationType::SetStarred(false) => {
            if count == 1 {
                "Unstar Paper".into()
            } else {
                format!("Unstar {} {}", count, noun)
            }
        }
        OperationType::SetFlag(Some(_)) => {
            if count == 1 {
                "Flag Paper".into()
            } else {
                format!("Flag {} {}", count, noun)
            }
        }
        OperationType::SetFlag(None) => {
            if count == 1 {
                "Remove Flag".into()
            } else {
                format!("Remove Flag from {} {}", count, noun)
            }
        }
        OperationType::AddTag(tag) => format!("Add Tag '{}'", tag),
        OperationType::RemoveTag(tag) => format!("Remove Tag '{}'", tag),
        OperationType::SetPayload(field, _) => format!("Edit {}", field),
        OperationType::RemovePayload(field) => format!("Clear {}", field),
        OperationType::PatchPayload(_) => "Edit Fields".into(),
        OperationType::SetParent(_) => {
            if count == 1 {
                "Move Paper".into()
            } else {
                format!("Move {} {}", count, noun)
            }
        }
        OperationType::SetPriority(_) => "Set Priority".into(),
        OperationType::SetVisibility(_) => "Set Visibility".into(),
        OperationType::AddReference(_) => "Add to Collection".into(),
        OperationType::RemoveReference(_, _) => "Remove from Collection".into(),
        OperationType::Custom(name, _) => name.clone(),
    }
}

/// Build the operation payload (stored as the operation item's payload).
///
/// If `prev` is provided, it captures the previous state of the field being changed,
/// enabling O(1) undo without replay.
pub fn build_operation_payload(
    target_id: ItemId,
    op_type: &OperationType,
    intent: OperationIntent,
    reason: Option<&str>,
    prev: Option<Value>,
) -> BTreeMap<String, Value> {
    let mut payload = BTreeMap::new();
    payload.insert("target_id".into(), Value::String(target_id.to_string()));
    payload.insert("intent".into(), Value::String(intent.to_string()));

    if let Some(r) = reason {
        payload.insert("reason".into(), Value::String(r.to_string()));
    }

    let (op_type_str, op_data) = serialize_op_type(op_type);
    payload.insert("op_type".into(), Value::String(op_type_str));
    payload.insert("op_data".into(), op_data);

    if let Some(prev_val) = prev {
        payload.insert("prev".into(), prev_val);
    }

    payload
}

/// Serialize an OperationType to (type_name, data_value) for storage.
fn serialize_op_type(op: &OperationType) -> (String, Value) {
    match op {
        OperationType::AddTag(tag) => ("add_tag".into(), Value::String(tag.clone())),
        OperationType::RemoveTag(tag) => ("remove_tag".into(), Value::String(tag.clone())),
        OperationType::SetFlag(flag) => {
            let val = match flag {
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
                None => Value::Null,
            };
            ("set_flag".into(), val)
        }
        OperationType::SetRead(v) => ("set_read".into(), Value::Bool(*v)),
        OperationType::SetStarred(v) => ("set_starred".into(), Value::Bool(*v)),
        OperationType::SetPriority(p) => ("set_priority".into(), Value::String(p.to_string())),
        OperationType::SetVisibility(v) => ("set_visibility".into(), Value::String(v.to_string())),
        OperationType::SetPayload(field, val) => {
            let mut m = BTreeMap::new();
            m.insert("field".into(), Value::String(field.clone()));
            m.insert("value".into(), val.clone());
            ("set_payload".into(), Value::Object(m))
        }
        OperationType::RemovePayload(field) => {
            ("remove_payload".into(), Value::String(field.clone()))
        }
        OperationType::PatchPayload(fields) => {
            let m: BTreeMap<String, Value> = fields.clone();
            ("patch_payload".into(), Value::Object(m))
        }
        OperationType::AddReference(r) => {
            let mut m = BTreeMap::new();
            m.insert("target".into(), Value::String(r.target.to_string()));
            m.insert(
                "edge_type".into(),
                Value::String(serde_json::to_string(&r.edge_type).unwrap_or_default()),
            );
            if let Some(meta) = &r.metadata {
                let meta_val = serde_json::to_string(meta)
                    .map(Value::String)
                    .unwrap_or(Value::Null);
                m.insert("metadata".into(), meta_val);
            }
            ("add_reference".into(), Value::Object(m))
        }
        OperationType::RemoveReference(target, edge) => {
            let mut m = BTreeMap::new();
            m.insert("target".into(), Value::String(target.to_string()));
            m.insert(
                "edge_type".into(),
                Value::String(serde_json::to_string(edge).unwrap_or_default()),
            );
            ("remove_reference".into(), Value::Object(m))
        }
        OperationType::SetParent(parent) => {
            let val = match parent {
                Some(id) => Value::String(id.to_string()),
                None => Value::Null,
            };
            ("set_parent".into(), val)
        }
        OperationType::Custom(name, data) => (format!("custom:{}", name), data.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn field_mutation_to_operation_type() {
        let mutations = vec![
            (FieldMutation::SetRead(true), "SetRead"),
            (FieldMutation::SetStarred(false), "SetStarred"),
            (FieldMutation::AddTag("test".into()), "AddTag"),
            (FieldMutation::RemoveTag("test".into()), "RemoveTag"),
            (FieldMutation::SetFlag(None), "SetFlag"),
            (
                FieldMutation::SetPayload("title".into(), Value::String("t".into())),
                "SetPayload",
            ),
            (
                FieldMutation::RemovePayload("field".into()),
                "RemovePayload",
            ),
            (FieldMutation::SetParent(None), "SetParent"),
        ];

        for (mutation, expected_name) in mutations {
            let op: OperationType = mutation.into();
            let name = format!("{:?}", op);
            assert!(
                name.starts_with(expected_name),
                "expected {} to start with {}",
                name,
                expected_name
            );
        }
    }

    #[test]
    fn operation_intent_display() {
        assert_eq!(OperationIntent::Routine.to_string(), "routine");
        assert_eq!(OperationIntent::Escalation.to_string(), "escalation");
    }

    #[test]
    fn build_operation_payload_round_trip() {
        let target = Uuid::new_v4();
        let payload = build_operation_payload(
            target,
            &OperationType::AddTag("methods/sims".into()),
            OperationIntent::Routine,
            Some("testing"),
            None,
        );
        assert_eq!(
            payload.get("target_id"),
            Some(&Value::String(target.to_string()))
        );
        assert_eq!(
            payload.get("op_type"),
            Some(&Value::String("add_tag".into()))
        );
        assert_eq!(
            payload.get("op_data"),
            Some(&Value::String("methods/sims".into()))
        );
        assert_eq!(
            payload.get("intent"),
            Some(&Value::String("routine".into()))
        );
        assert_eq!(
            payload.get("reason"),
            Some(&Value::String("testing".into()))
        );
    }

    #[test]
    fn operation_intent_serde() {
        for intent in [
            OperationIntent::Routine,
            OperationIntent::Hypothesis,
            OperationIntent::Anomaly,
            OperationIntent::Editorial,
            OperationIntent::Correction,
            OperationIntent::Escalation,
        ] {
            let json = serde_json::to_string(&intent).unwrap();
            let back: OperationIntent = serde_json::from_str(&json).unwrap();
            assert_eq!(intent, back);
        }
    }

    #[test]
    fn operation_type_serde() {
        let ops = vec![
            OperationType::AddTag("test".into()),
            OperationType::RemoveTag("test".into()),
            OperationType::SetRead(true),
            OperationType::SetStarred(false),
            OperationType::SetPriority(Priority::High),
            OperationType::SetVisibility(Visibility::Public),
            OperationType::SetFlag(Some(FlagState {
                color: "red".into(),
                style: None,
                length: None,
            })),
            OperationType::SetPayload("title".into(), Value::String("Test".into())),
            OperationType::RemovePayload("old".into()),
            OperationType::SetParent(Some(Uuid::new_v4())),
            OperationType::Custom("my-op".into(), Value::Int(42)),
        ];
        for op in &ops {
            let json = serde_json::to_string(op).unwrap();
            let back: OperationType = serde_json::from_str(&json).unwrap();
            assert_eq!(*op, back);
        }
    }
}
