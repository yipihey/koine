//! The one new koine kind, `run@1.0.0` (docs/plan-run-convention.md R-D13),
//! declared by TR-D12's [`KindSchema`] and registered like
//! `bibliographic-work@1.0.0` is.
//!
//! The item *is* `run.json` (R-D1: "`run.json` is the item, the rest are its
//! blobs and its event ring"): the row id is the run's `id`, and the record's
//! blocks are the payload's fields, spelled as `run.schema.json` spells
//! them. Beside them koine keeps what it derives and nothing the code wrote:
//! `dir`, `state` (lifted from `status.state` so a query can filter on it),
//! `last_event_seq` (how far the ring was filled), `progress` (the last
//! `progress` event), `metrics` (the last value of each declared metric),
//! `outputs` (the index), `notes` (`notes.json`), and a `koine` block (pid,
//! `needs_restart`, `diagnose_request`, `sweep`, `exited`).
//!
//! There is no `run-event@1.0.0`: a run's events are `task-event@1.0.0`
//! rows keyed by the run id, written with the run's own `seq`
//! (`koine_tasks::job::append_event_at`). R-D6.2 says the two rings are the
//! same shape, and they are — a second kind would be a second reader for
//! `progress`, `job-events` and the bound plot to learn. The ring keeps the
//! last 200 (`EVENT_RING_CAPACITY`); `events.jsonl` is the archive.

use std::collections::BTreeMap;

use impress_item::item::{ActorKind, Item, ItemId, Priority, Value, Visibility};
use impress_item::kind::{FieldDef, FieldType, KindSchema};
use impress_item::schema::{refs, SchemaRef};
use serde_json::{Map, Value as Json};

/// The kind, spelled as `schema-refs.json` spells it.
pub const RUN: SchemaRef = refs::RUN;
/// This schema's version.
pub const VERSION: &str = "1.0.0";

/// The author every run row and event row is written as.
pub const AUTHOR: &str = "run-service";

/// The kind's schema. Required: what `run.schema.json` requires plus `dir`,
/// which an ingested run always has. Everything koine derives is optional,
/// so a row written by a build that derives less still reads.
pub fn schema() -> KindSchema {
    use FieldType::*;
    let optional = FieldDef::optional;
    let mut state = FieldDef::optional("state", String);
    state.values = Some(
        [
            "created",
            "running",
            "suspended",
            "done",
            "failed",
            "cancelled",
        ]
        .map(std::string::String::from)
        .to_vec(),
    );
    KindSchema {
        name: RUN.as_str().into(),
        version: VERSION.into(),
        fields: vec![
            FieldDef::required("convention", String),
            FieldDef::required("code", Object),
            FieldDef::required("problem", Object),
            FieldDef::required("status", Object),
            FieldDef::required("dir", String),
            optional("key", String),
            optional("name", String),
            optional("provenance", String),
            optional("host", Object),
            optional("lineage", Object),
            state,
            optional("last_event_seq", Int),
            optional("progress", Object),
            optional("metrics", Object),
            optional("outputs", Array),
            optional("notes", Object),
            optional("koine", Object),
        ],
        enforce: false,
    }
}

/// Register the kind in the process registry. Idempotent; a refusal means
/// an incompatible schema was registered by another build, which is a bug
/// to surface, not swallow.
pub fn ensure_registered() {
    if let Err(refused) = impress_item::kind::register(schema()) {
        panic!(
            "{} refused: {}",
            refused.kind,
            refused
                .breaks
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
}

/// A JSON value as the store holds it.
pub fn to_item(v: &Json) -> Value {
    serde_json::from_value(v.clone()).unwrap_or(Value::Null)
}

/// A payload as JSON.
pub fn payload_json(item: &Item) -> Map<String, Json> {
    item.payload
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::to_value(v).unwrap_or(Json::Null)))
        .collect()
}

/// A fresh run row from its `run.json` and the fields koine derived.
pub fn run_item(id: ItemId, fields: BTreeMap<String, Json>) -> Item {
    let now = chrono::Utc::now();
    let payload: BTreeMap<String, Value> = fields
        .iter()
        .map(|(k, v)| (k.clone(), to_item(v)))
        .collect();
    Item {
        id,
        schema: RUN,
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
