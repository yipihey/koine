use chrono::{DateTime, Utc};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use uuid::Uuid;

use crate::reference::TypedReference;
use crate::schema::SchemaRef;

/// Globally unique item identifier (UUID v4).
pub type ItemId = Uuid;

/// Actor (human or agent) identifier.
pub type ActorId = String;

/// Dynamic value type for payload fields.
///
/// Serialization uses `#[serde(untagged)]` so values are bare JSON.
/// Deserialization uses a hand-written impl that inspects the JSON token
/// type directly, avoiding the try-each-variant overhead of untagged enums
/// (which generates and discards error messages for every failed variant).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Array(Vec<Value>),
    Object(BTreeMap<String, Value>),
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(ValueVisitor)
    }
}

struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Value, E> {
        Ok(Value::Int(v))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Value, E> {
        Ok(Value::Int(v as i64))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        Ok(Value::Float(v))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut vec = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(elem) = seq.next_element()? {
            vec.push(elem);
        }
        Ok(Value::Array(vec))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut obj = BTreeMap::new();
        while let Some((key, val)) = map.next_entry()? {
            obj.insert(key, val);
        }
        Ok(Value::Object(obj))
    }
}

/// Whether the item was created by a human, agent, or system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ActorKind {
    Human,
    Agent,
    System,
}

/// Item priority level.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    None,
    Low,
    #[default]
    Normal,
    High,
    Urgent,
}

impl std::fmt::Display for Priority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Priority::None => write!(f, "none"),
            Priority::Low => write!(f, "low"),
            Priority::Normal => write!(f, "normal"),
            Priority::High => write!(f, "high"),
            Priority::Urgent => write!(f, "urgent"),
        }
    }
}

impl std::str::FromStr for Priority {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "none" => Ok(Priority::None),
            "low" => Ok(Priority::Low),
            "normal" => Ok(Priority::Normal),
            "high" => Ok(Priority::High),
            "urgent" => Ok(Priority::Urgent),
            _ => Err(format!("unknown priority: {}", s)),
        }
    }
}

/// Item visibility level.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    #[default]
    Private,
    Shared,
    Public,
}

impl std::fmt::Display for Visibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Visibility::Private => write!(f, "private"),
            Visibility::Shared => write!(f, "shared"),
            Visibility::Public => write!(f, "public"),
        }
    }
}

impl std::str::FromStr for Visibility {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "private" => Ok(Visibility::Private),
            "shared" => Ok(Visibility::Shared),
            "public" => Ok(Visibility::Public),
            _ => Err(format!("unknown visibility: {}", s)),
        }
    }
}

/// Flag state — color with optional style and length.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FlagState {
    pub color: String,
    pub style: Option<String>,
    pub length: Option<String>,
}

/// The core entity of the unified architecture.
///
/// All data across the impress suite is represented as Items.
/// Universal metadata lives on the struct directly; domain-specific
/// fields live in `payload`.
///
/// ## Envelope Fields
///
/// The immutable envelope (id, schema, created, author) is set at creation.
/// Mutable classification fields (tags, flag, is_read, is_starred, priority,
/// visibility) are materialized projections of the operation history.
/// `modified` tracks the latest mutation timestamp.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: ItemId,
    pub schema: SchemaRef,
    pub payload: BTreeMap<String, Value>,

    // Universal metadata (immutable envelope)
    pub created: DateTime<Utc>,
    #[serde(default = "Utc::now")]
    pub modified: DateTime<Utc>,
    pub author: ActorId,
    pub author_kind: ActorKind,

    // Causal ordering & collaboration
    pub logical_clock: u64,
    pub origin: Option<String>,
    pub canonical_id: Option<String>,

    // Classification (materialized from operations)
    pub tags: Vec<String>,
    pub flag: Option<FlagState>,
    pub is_read: bool,
    pub is_starred: bool,
    pub priority: Priority,
    pub visibility: Visibility,

    // Communication & provenance
    pub message_type: Option<String>,
    pub produced_by: Option<ItemId>,
    pub version: Option<String>,
    pub batch_id: Option<String>,

    // Graph structure
    pub references: Vec<TypedReference>,
    pub parent: Option<ItemId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_serde_round_trip() {
        let values = vec![
            Value::Null,
            Value::Bool(true),
            Value::Int(42),
            // Not 3.14: clippy's `approx_constant` is deny-by-default on the
            // 1.97 toolchain, and this test only needs *a* float to round-trip.
            Value::Float(2.75),
            Value::String("hello".into()),
            Value::Array(vec![Value::Int(1), Value::String("two".into())]),
            Value::Object({
                let mut m = BTreeMap::new();
                m.insert("key".into(), Value::Bool(false));
                m
            }),
        ];
        for v in &values {
            let json = serde_json::to_string(v).unwrap();
            let back: Value = serde_json::from_str(&json).unwrap();
            assert_eq!(*v, back);
        }
    }

    #[test]
    fn item_serde_round_trip() {
        let item = Item {
            id: Uuid::new_v4(),
            schema: crate::schema::refs::BIBLIOGRAPHY_ENTRY,
            payload: {
                let mut m = BTreeMap::new();
                m.insert("title".into(), Value::String("A Great Paper".into()));
                m.insert("citation_count".into(), Value::Int(42));
                m
            },
            created: Utc::now(),
            modified: Utc::now(),
            author: "user@example.com".into(),
            author_kind: ActorKind::Human,
            logical_clock: 42,
            origin: Some("store-abc".into()),
            canonical_id: None,
            tags: vec!["methods/sims/hydro".into()],
            flag: Some(FlagState {
                color: "red".into(),
                style: None,
                length: None,
            }),
            is_read: true,
            is_starred: false,
            priority: Priority::High,
            visibility: Visibility::Shared,
            message_type: None,
            produced_by: None,
            version: Some("1.0".into()),
            batch_id: None,
            references: vec![],
            parent: None,
        };
        let json = serde_json::to_string_pretty(&item).unwrap();
        let back: Item = serde_json::from_str(&json).unwrap();
        assert_eq!(item, back);
    }

    #[test]
    fn priority_serde_round_trip() {
        for p in [
            Priority::None,
            Priority::Low,
            Priority::Normal,
            Priority::High,
            Priority::Urgent,
        ] {
            let json = serde_json::to_string(&p).unwrap();
            let back: Priority = serde_json::from_str(&json).unwrap();
            assert_eq!(p, back);
        }
    }

    #[test]
    fn visibility_serde_round_trip() {
        for v in [Visibility::Private, Visibility::Shared, Visibility::Public] {
            let json = serde_json::to_string(&v).unwrap();
            let back: Visibility = serde_json::from_str(&json).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn priority_from_str() {
        assert_eq!("normal".parse::<Priority>().unwrap(), Priority::Normal);
        assert_eq!("urgent".parse::<Priority>().unwrap(), Priority::Urgent);
        assert!("invalid".parse::<Priority>().is_err());
    }

    #[test]
    fn visibility_from_str() {
        assert_eq!(
            "private".parse::<Visibility>().unwrap(),
            Visibility::Private
        );
        assert_eq!("public".parse::<Visibility>().unwrap(), Visibility::Public);
        assert!("invalid".parse::<Visibility>().is_err());
    }
}
