//! The store side of a scenario: `impress/scenario@1.0.0` rows (S1, §
//! Scenarios), mirroring `impress-surface-service/src/store.rs`'s shape for
//! `impress/ui/surface@1.0.0` — the same discipline: the document's shape is
//! owned by `impress-scenario`, this row stores it as JSON text and adds
//! only the fields a catalogue needs to find it (`scenario_id`, `tags`).

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use impress_item::schema::refs::IMPRESS_SCENARIO as SCENARIO_SCHEMA_REF;
use impress_item::Value as ItemValue;
use impress_item::{ActorKind, AttributedStore, Item, ItemId, ItemQuery, Priority, Visibility};
use impress_layout_service::authorship::author_for_service;
use impress_scenario::Scenario;
use impress_service_core::Refusal;

pub mod field {
    pub const SCENARIO_ID: &str = "scenario_id";
    pub const DESCRIPTION: &str = "description";
    pub const TIER: &str = "tier";
    pub const SPEC: &str = "spec";
    pub const TAGS: &str = "tags";
    pub const REVISION: &str = "revision";
}

pub type Result<T> = std::result::Result<T, Refusal>;

/// One `impress/scenario@1.0.0` row, spec included.
#[derive(Debug, Clone, PartialEq)]
pub struct ScenarioRow {
    pub id: ItemId,
    pub spec: Scenario,
    pub tags: Vec<String>,
    pub created: DateTime<Utc>,
    pub modified: DateTime<Utc>,
}

/// Store-backed access to stored scenarios.
#[derive(Clone)]
pub struct ScenarioStore {
    store: Arc<dyn AttributedStore>,
}

impl ScenarioStore {
    pub fn new(store: Arc<dyn AttributedStore>) -> Self {
        Self { store }
    }

    /// Create one row. `impress-scenario::validate` must have already been
    /// run by the caller (`ImpressScenarioService::scenario_create` does
    /// this before ever reaching the store).
    pub fn create(
        &self,
        spec: &Scenario,
        tags: &[String],
        actor: ActorKind,
    ) -> Result<ScenarioRow> {
        let mut payload: BTreeMap<String, ItemValue> = BTreeMap::new();
        payload.insert(
            field::SCENARIO_ID.into(),
            ItemValue::String(spec.id.clone()),
        );
        payload.insert(
            field::DESCRIPTION.into(),
            ItemValue::String(spec.description.clone()),
        );
        payload.insert(
            field::TIER.into(),
            ItemValue::String(tier_str(spec.tier).to_string()),
        );
        payload.insert(field::SPEC.into(), ItemValue::String(spec_json(spec)?));
        payload.insert(field::REVISION.into(), ItemValue::Int(1));
        if !tags.is_empty() {
            payload.insert(
                field::TAGS.into(),
                ItemValue::Array(tags.iter().cloned().map(ItemValue::String).collect()),
            );
        }
        let now = Utc::now();
        let item = Item {
            id: uuid::Uuid::new_v4(),
            schema: SCENARIO_SCHEMA_REF,
            payload,
            created: now,
            modified: now,
            author: author_for_service(actor, "scenario-service"),
            author_kind: actor,
            logical_clock: 0,
            origin: None,
            canonical_id: None,
            tags: vec![],
            flag: None,
            is_read: false,
            is_starred: false,
            priority: Priority::None,
            visibility: Visibility::Private,
            message_type: None,
            produced_by: None,
            version: None,
            batch_id: None,
            references: vec![],
            parent: None,
            extra: Default::default(),
        };
        let id = self
            .store
            .insert(item)
            .map_err(|e| Refusal::store(format!("write scenario: {e}")))?;
        self.get(id)?
            .ok_or_else(|| Refusal::store("scenario vanished immediately after creation"))
    }

    pub fn get(&self, id: ItemId) -> Result<Option<ScenarioRow>> {
        let Some(item) = self.row_item(id)? else {
            return Ok(None);
        };
        Ok(Some(row_of(&item)?))
    }

    /// The row whose `scenario_id` equals `scenario_id` — the id
    /// `run_selftest`/`scenario-service_run` addresses (SC-1: the id, not
    /// the store's own `ItemId`, is stable).
    pub fn get_by_scenario_id(&self, scenario_id: &str) -> Result<Option<ScenarioRow>> {
        Ok(self.list()?.into_iter().find(|r| r.spec.id == scenario_id))
    }

    pub fn list(&self) -> Result<Vec<ScenarioRow>> {
        let query = ItemQuery {
            schema: Some(SCENARIO_SCHEMA_REF),
            include_tags: false,
            include_references: false,
            ..Default::default()
        };
        let mut rows: Vec<ScenarioRow> = self
            .store
            .query(&query)
            .map_err(|e| Refusal::store(format!("read scenarios: {e}")))?
            .iter()
            .map(row_of)
            .collect::<Result<Vec<_>>>()?;
        rows.sort_by_key(|r| r.created);
        Ok(rows)
    }

    fn row_item(&self, id: ItemId) -> Result<Option<Item>> {
        self.store
            .get(id)
            .map_err(|e| Refusal::store(format!("read scenario {id}: {e}")))
            .map(|item| item.filter(|i| i.schema == SCENARIO_SCHEMA_REF))
    }
}

fn tier_str(tier: impress_scenario::Tier) -> &'static str {
    match tier {
        impress_scenario::Tier::A => "a",
        impress_scenario::Tier::B => "b",
    }
}

fn spec_json(spec: &Scenario) -> Result<String> {
    serde_json::to_string(spec).map_err(|e| Refusal::store(format!("encode scenario: {e}")))
}

fn row_of(item: &Item) -> Result<ScenarioRow> {
    let spec_text = item
        .payload
        .get(field::SPEC)
        .and_then(|v| match v {
            ItemValue::String(s) => Some(s.clone()),
            _ => None,
        })
        .ok_or_else(|| Refusal::store(format!("scenario {} has no `spec` field", item.id)))?;
    let spec: Scenario = serde_json::from_str(&spec_text)
        .map_err(|e| Refusal::store(format!("scenario {} spec: {e}", item.id)))?;
    let tags = match item.payload.get(field::TAGS) {
        Some(ItemValue::Array(items)) => items
            .iter()
            .filter_map(|v| match v {
                ItemValue::String(s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    Ok(ScenarioRow {
        id: item.id,
        spec,
        tags,
        created: item.created,
        modified: item.modified,
    })
}
