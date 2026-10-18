//! The Tier A `Caller`: the pipeline against a scratch store, per scenario
//! (H-P2-3, `impress_service_core::pipeline::invoke_on` — landed, SC-2).
//!
//! Every scenario gets its own scratch store (a `MemoryStore`, or whatever
//! the host's [`set_scratch_store_factory`] opens), so scenarios cannot see
//! each other's rows and can run in any order — the same discipline
//! `impress-layout-service/src/tier_a.rs`'s catalogue already follows.
//!
//! # The effects check, honestly
//!
//! `expect_effects` is checked by re-querying the scratch store for a
//! non-empty result of each named kind after every step has run, not by an
//! instrumented spy on the store's write path. A real "did THIS scenario's
//! steps write this kind" spy is P2/L1 work (`impress-core`'s `SpyStore`,
//! not landed as of this crate); on a store that starts empty per scenario,
//! "the kind now has at least one row" is a correct proxy for "a write
//! happened", but it cannot distinguish which step did it, and it cannot
//! catch a write immediately deleted within the same scenario. `wrote`'s
//! doc comment says so; a real spy is a straightforward drop-in once it
//! exists (this caller already holds the one store every step ran
//! against).

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use impress_item::{AttributedStore, ItemQuery};
use impress_scenario::{CallOutcome, Caller, EventBody, WaitBody};
use impress_service_core::descriptor_handle::VerbHandle;
use impress_service_core::pipeline::{self, Call, CallerIdentity};
use koine_store::process::CallStore;
use koine_store_memory::MemoryStore;
use serde_json::Value;

/// Opens a fresh, empty scratch store for one scenario.
pub type ScratchStoreFactory = fn() -> Result<CallStore, String>;

static SCRATCH_FACTORY: RwLock<Option<ScratchStoreFactory>> = RwLock::new(None);

/// Run Tier A scenarios over another backend: `open` is called once per
/// scenario and must return a fresh, empty store, and whatever process ports
/// its verbs need must be in place when it returns. Without one, each
/// scenario gets a [`MemoryStore`], which every verb written against the port
/// reads as the call's store. A host whose own verbs resolve the call's store
/// as their backend's concrete type (impress: `SqliteItemStore`) sets this so
/// those verbs run on the scratch store too, never the real one.
pub fn set_scratch_store_factory(open: ScratchStoreFactory) {
    *SCRATCH_FACTORY.write().unwrap_or_else(|e| e.into_inner()) = Some(open);
}

fn open_scratch() -> Result<CallStore, String> {
    match *SCRATCH_FACTORY.read().unwrap_or_else(|e| e.into_inner()) {
        Some(open) => open(),
        None => Ok(CallStore::of(Arc::new(MemoryStore::new()))),
    }
}

/// Run every step through the pipeline against a fresh scratch store.
pub struct TierACaller {
    store: CallStore,
}

impl TierACaller {
    pub fn open() -> Result<Self, String> {
        Ok(Self {
            store: open_scratch().map_err(|e| format!("open scratch store: {e}"))?,
        })
    }

    pub fn store(&self) -> &Arc<dyn AttributedStore> {
        self.store.port()
    }
}

fn caller_identity(as_ident: &str) -> CallerIdentity {
    match as_ident.strip_prefix("agent:") {
        Some(name) => CallerIdentity::agent(name.to_string()),
        None => CallerIdentity::Person,
    }
}

#[async_trait]
impl Caller for TierACaller {
    async fn call(
        &mut self,
        verb: &str,
        args: Value,
        as_ident: &str,
    ) -> Result<CallOutcome, String> {
        let descriptor = impress_service_core::call::find(verb)
            .ok_or_else(|| format!("no such verb: {verb}"))?;
        // Provider transport is a Tier B reach even if its registered safety
        // is read-only. A scratch Tier A scenario must never contact it.
        if matches!(&descriptor, VerbHandle::Provider(_)) {
            return Err(format!("provider verb {verb} requires Tier B"));
        }
        let mut call = Call::new(caller_identity(as_ident), args);
        call.store = Some(self.store.erased());
        let result = pipeline::invoke_handle(descriptor, call)
            .await
            .map_err(|e| e.to_string())?;
        Ok(CallOutcome {
            result,
            status: None,
        })
    }

    async fn event(&mut self, _event: &EventBody) -> Result<CallOutcome, String> {
        Err(
            "a `event` step needs a surface-hosting store per scenario, not yet supported in \
             Tier A (S1); run this scenario in Tier B"
                .to_string(),
        )
    }

    async fn gesture(&mut self, gesture: &Value) -> Result<CallOutcome, String> {
        let verb = gesture
            .get("verb")
            .and_then(Value::as_str)
            .ok_or_else(|| "a `gesture` step needs a `verb` field".to_string())?;
        self.call(
            &format!("layout-service_{verb}"),
            gesture.clone(),
            "agent:scenario",
        )
        .await
    }

    async fn wait(&mut self, wait: &WaitBody) -> Result<(), String> {
        // Tier A has no job runner and no log stream of its own; a `wait`
        // step in a Tier A scenario is a modelling error today. Named
        // clearly rather than silently passing.
        Err(format!(
            "`wait` is not supported in Tier A (S1): {wait:?}; run this scenario in Tier B"
        ))
    }

    async fn seed(&mut self, kind: &str, payload: &Value) -> Result<Value, String> {
        use impress_item::{ActorKind, Item, Priority, Visibility};
        let schema = kind
            .parse::<impress_item::SchemaRef>()
            .map_err(|error| format!("seed `{kind}`: {error}"))?;
        let payload_map = payload
            .as_object()
            .ok_or_else(|| format!("seed `{kind}`: payload must be a JSON object"))?;
        let mut fields = std::collections::BTreeMap::new();
        for (k, v) in payload_map {
            fields.insert(k.clone(), json_to_item_value(v));
        }
        let now = chrono::Utc::now();
        let item = Item {
            id: uuid::Uuid::new_v4(),
            schema,
            payload: fields,
            created: now,
            modified: now,
            author: "agent:scenario".to_string(),
            author_kind: ActorKind::Agent,
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
            .port()
            .insert(item)
            .map_err(|e| format!("seed `{kind}`: {e}"))?;
        Ok(serde_json::json!({ "id": id.to_string() }))
    }

    fn wrote(&self, kind: &str) -> bool {
        let Ok(schema) = kind.parse::<impress_item::SchemaRef>() else {
            return false;
        };
        let query = ItemQuery {
            schema: Some(schema),
            ..Default::default()
        };
        self.store
            .port()
            .query(&query)
            .map(|rows| !rows.is_empty())
            .unwrap_or(false)
    }
}

fn json_to_item_value(v: &Value) -> impress_item::Value {
    use impress_item::Value as ItemValue;
    match v {
        Value::Null => ItemValue::Null,
        Value::Bool(b) => ItemValue::Bool(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                ItemValue::Int(i)
            } else {
                ItemValue::Float(n.as_f64().unwrap_or_default())
            }
        }
        Value::String(s) => ItemValue::String(s.clone()),
        Value::Array(items) => ItemValue::Array(items.iter().map(json_to_item_value).collect()),
        Value::Object(_) => ItemValue::String(v.to_string()),
    }
}
