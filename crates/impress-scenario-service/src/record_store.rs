//! Select one caller's bounded recording from the existing audit rows.
//! The pure matcher owns replay generation; this module owns selection only.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, SecondsFormat, Utc};
use impress_item::schema::refs::CORE_VERB_CALL as VERB_CALL_SCHEMA;
use impress_item::{
    AttributedStore, Item, ItemQuery, Predicate, SortDescriptor, Value as ItemValue,
};
use impress_service_core::descriptor::Reach;
use impress_service_core::Refusal;
use serde_json::Value;

use crate::record::RecordedCall;

const MAX_RECORDING_CALLS: usize = 5_000;

pub(crate) struct Selection {
    predicates: Vec<Predicate>,
}

impl Selection {
    pub(crate) fn new(
        trace_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
        caller: Option<&str>,
    ) -> Result<Self, Refusal> {
        let mut predicates = Vec::new();
        if let Some(trace) = trace_id {
            if trace.trim().is_empty() || since.is_some() || until.is_some() {
                return Err(Refusal::invalid_argument(
                    "select a nonempty trace_id OR a since/until window",
                ));
            }
            predicates.push(Predicate::Eq(
                "payload.trace_id".into(),
                ItemValue::String(trace.into()),
            ));
        } else {
            let (Some(since), Some(until), Some(_)) = (since, until, caller) else {
                return Err(Refusal::invalid_argument(
                    "a time window requires since, until, and as (one exact caller)",
                ));
            };
            let parse = |value: &str| {
                DateTime::parse_from_rfc3339(value)
                    .map(|time| time.with_timezone(&Utc))
                    .map_err(|_| {
                        Refusal::invalid_argument("since/until must be RFC 3339 timestamps")
                    })
            };
            let (since, until) = (parse(since)?, parse(until)?);
            if since > until {
                return Err(Refusal::invalid_argument("since must not be after until"));
            }
            predicates.push(Predicate::Gte(
                "payload.started_at".into(),
                ItemValue::String(since.to_rfc3339_opts(SecondsFormat::Millis, true)),
            ));
            predicates.push(Predicate::Lte(
                "payload.started_at".into(),
                ItemValue::String(until.to_rfc3339_opts(SecondsFormat::Millis, true)),
            ));
        }
        if let Some(caller) = caller {
            let author = if caller == "person" { "human" } else { caller };
            if author != "human"
                && !["agent:", "app:", "system:", "provider:"]
                    .iter()
                    .any(|prefix| {
                        author
                            .strip_prefix(prefix)
                            .is_some_and(|name| !name.is_empty())
                    })
            {
                return Err(Refusal::invalid_argument(
                    "as must name one caller: person, agent:<name>, app:<name>, system:<name>, or provider:<name>",
                ));
            }
            predicates.push(Predicate::Eq(
                "author".into(),
                ItemValue::String(author.into()),
            ));
        }
        Ok(Self { predicates })
    }

    pub(crate) fn read(self, store: &dyn AttributedStore) -> Result<Vec<RecordedCall>, Refusal> {
        // Include calls already queued by this process. Other processes own
        // their own flush; the caller records after the selected session ends.
        impress_service_core::pipeline::audit::flush(std::time::Duration::from_secs(2))
            .map_err(|error| Refusal::store(format!("drain recording audit: {error}")))?;
        let rows = store
            .query(&ItemQuery {
                schema: Some(VERB_CALL_SCHEMA),
                predicates: self.predicates,
                sort: vec![SortDescriptor {
                    field: "payload.started_at".into(),
                    ascending: true,
                }],
                limit: Some(MAX_RECORDING_CALLS + 1),
                include_tags: false,
                include_references: false,
                ..Default::default()
            })
            .map_err(|error| Refusal::store(format!("read recording: {error}")))?;
        if rows.len() > MAX_RECORDING_CALLS {
            return Err(Refusal::invalid_argument(
                "recording exceeds 5000 calls; select a narrower window",
            ));
        }
        if rows.is_empty() {
            return Err(Refusal::not_found("no calls match this recording"));
        }
        let authors: BTreeSet<_> = rows.iter().map(|row| row.author.as_str()).collect();
        if authors.len() != 1 {
            return Err(Refusal::invalid_argument(
                "recording contains multiple callers; use as to select one",
            ));
        }
        let mut calls = rows.iter().map(read_call).collect::<Result<Vec<_>, _>>()?;
        let parents: BTreeMap<_, _> = calls
            .iter()
            .map(|(_, _, call)| (call.call_id.clone(), call.parent_call.clone()))
            .collect();
        let depth = |id: &str| -> Result<usize, Refusal> {
            let mut seen = BTreeSet::new();
            let mut current = Some(id);
            while let Some(id) = current {
                if !seen.insert(id) {
                    return Err(Refusal::invalid_argument(
                        "recording has a cyclic parent_call",
                    ));
                }
                current = parents.get(id).and_then(|parent| parent.as_deref());
            }
            Ok(seen.len())
        };
        let depths = calls
            .iter()
            .map(|(_, _, call)| Ok((call.call_id.clone(), depth(&call.call_id)?)))
            .collect::<Result<BTreeMap<_, _>, Refusal>>()?;
        // A child finishes (and is inserted) before its parent. In a tied
        // millisecond, causal depth takes precedence over the insertion time.
        calls.sort_by(|a, b| {
            (&a.0, depths[&a.2.call_id], a.1, &a.2.call_id).cmp(&(
                &b.0,
                depths[&b.2.call_id],
                b.1,
                &b.2.call_id,
            ))
        });
        Ok(calls.into_iter().map(|(_, _, call)| call).collect())
    }
}

fn read_call(item: &Item) -> Result<(String, DateTime<Utc>, RecordedCall), Refusal> {
    let payload = serde_json::to_value(&item.payload)
        .map_err(|error| Refusal::store(format!("decode call {}: {error}", item.id)))?;
    if payload["result_ids_truncated"].as_bool() == Some(true) {
        return Err(Refusal::invalid_argument(format!(
            "call {} has incomplete output ID metadata; its dependencies cannot be recorded safely",
            item.id
        )));
    }
    let verb = payload["verb"].as_str().unwrap_or_default().to_string();
    let app_reach = impress_service_core::call::find(&verb)
        .map(|descriptor| {
            descriptor
                .effects()
                .reach
                .iter()
                .filter_map(|reach| match reach {
                    Reach::App(app) => Some((*app).to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    // Review-policy exemptions do not erase caller identity: an app token
    // may act for the person, but the scenario vocabulary cannot name it.
    let caller = if item.author == "human" {
        "person".into()
    } else {
        item.author.clone()
    };
    let result_ids = payload
        .get("result_ids")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| Refusal::store(format!("decode result IDs for {}: {error}", item.id)))?
        .unwrap_or_default();
    Ok((
        payload["started_at"].as_str().unwrap_or_default().into(),
        item.created,
        RecordedCall {
            call_id: item.id.to_string(),
            parent_call: payload["parent_call"].as_str().map(str::to_string),
            verb,
            args: payload.get("args").cloned().unwrap_or(Value::Null),
            // Legacy rows cannot prove whether the summary lost information.
            args_replayable: payload["args_replayable"].as_bool() == Some(true)
                && payload["compacted"].as_bool() != Some(true)
                && payload["ok"].is_boolean(),
            result_ids,
            ok: payload["ok"].as_bool().unwrap_or(false),
            code: payload["code"].as_str().map(str::to_string),
            caller,
            app_reach,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use impress_service_core::pipeline::CallerIdentity;
    use koine_store_memory::MemoryStore;
    use serde_json::json;

    /// Write one `core/verb-call@1.0.0` row the way the audit sink does: the
    /// call id is the row id and the batch id, the caller is the author.
    fn record_as(store: &dyn AttributedStore, id: &str, caller: CallerIdentity, payload: Value) {
        let fields: std::collections::BTreeMap<String, ItemValue> =
            serde_json::from_value(payload).unwrap();
        let now = Utc::now();
        let author_kind = match &caller {
            CallerIdentity::Person => impress_item::ActorKind::Human,
            CallerIdentity::System(_) => impress_item::ActorKind::System,
            _ => impress_item::ActorKind::Agent,
        };
        store
            .insert(Item {
                id: uuid::Uuid::parse_str(id).unwrap(),
                schema: VERB_CALL_SCHEMA,
                payload: fields,
                created: now,
                modified: now,
                author: caller.author(),
                author_kind,
                logical_clock: 0,
                origin: None,
                canonical_id: None,
                tags: vec![],
                flag: None,
                is_read: false,
                is_starred: false,
                priority: impress_item::Priority::None,
                visibility: impress_item::Visibility::Private,
                message_type: None,
                produced_by: None,
                version: None,
                batch_id: Some(id.to_string()),
                references: vec![],
                parent: None,
            })
            .unwrap();
    }

    fn record(store: &dyn AttributedStore, caller: CallerIdentity, payload: Value) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        record_as(store, &id, caller, payload);
        id
    }

    fn open() -> MemoryStore {
        MemoryStore::new()
    }

    fn payload() -> Value {
        json!({
            "trace_id": "recording", "started_at": "2026-09-27T00:00:00.000Z",
            "verb": "triage-service_set-starred", "args": {"id":"paper", "starred":true},
            "args_replayable":true, "result_ids":{}, "ok":true,
        })
    }

    #[test]
    fn selection_requires_one_mode_and_an_exact_window_caller() {
        assert!(Selection::new(None, None, None, None).is_err());
        assert!(Selection::new(Some(""), None, None, None).is_err());
        assert!(Selection::new(Some("trace"), Some("2026-09-27T00:00:00Z"), None, None).is_err());
        assert!(Selection::new(
            None,
            Some("2026-09-27T00:00:00Z"),
            Some("2026-09-28T00:00:00Z"),
            None
        )
        .is_err());
        assert!(Selection::new(Some("trace"), None, None, Some("agent")).is_err());
        assert!(Selection::new(Some("trace"), None, None, Some("agent:test")).is_ok());
    }

    #[test]
    fn window_compares_instants_and_normalizes_time_zones() {
        let selection = Selection::new(
            None,
            Some("2026-09-27T02:00:00+02:00"),
            Some("2026-09-27T01:00:00Z"),
            Some("person"),
        )
        .unwrap();
        assert_eq!(
            selection.predicates[0],
            Predicate::Gte(
                "payload.started_at".into(),
                ItemValue::String("2026-09-27T00:00:00.000Z".into())
            )
        );
        assert!(Selection::new(
            None,
            Some("2026-09-27T02:00:00Z"),
            Some("2026-09-27T01:00:00Z"),
            Some("person")
        )
        .is_err());
        assert!(Selection::new(None, Some("yesterday"), Some("now"), Some("person")).is_err());
    }

    #[test]
    fn selection_filters_trace_and_caller_and_orders_parent_before_child() {
        let store = open();
        let parent_id = uuid::Uuid::new_v4().to_string();
        let mut child = payload();
        child["parent_call"] = json!(parent_id);
        let child_id = record(&store, CallerIdentity::Person, child);
        record_as(&store, &parent_id, CallerIdentity::Person, payload());
        record(&store, CallerIdentity::agent("other"), payload());
        let mut unrelated = payload();
        unrelated["trace_id"] = json!("unrelated");
        record(&store, CallerIdentity::Person, unrelated);
        assert!(Selection::new(Some("recording"), None, None, None)
            .unwrap()
            .read(&store)
            .is_err());
        let calls = Selection::new(Some("recording"), None, None, Some("person"))
            .unwrap()
            .read(&store)
            .unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].call_id, parent_id);
        assert_eq!(calls[1].call_id, child_id);
    }

    #[test]
    fn compacted_and_legacy_rows_are_not_replayable() {
        let store = open();
        let mut legacy = payload();
        legacy.as_object_mut().unwrap().remove("args_replayable");
        record(&store, CallerIdentity::Person, legacy);
        let mut compacted = payload();
        compacted["compacted"] = json!(true);
        record(&store, CallerIdentity::Person, compacted);
        let calls = Selection::new(Some("recording"), None, None, None)
            .unwrap()
            .read(&store)
            .unwrap();
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|call| !call.args_replayable));
    }

    #[test]
    fn app_calls_keep_their_identity_and_are_not_replayed_as_the_person() {
        let store = open();
        record(&store, CallerIdentity::App("impress".into()), payload());
        let calls = Selection::new(Some("recording"), None, None, Some("app:impress"))
            .unwrap()
            .read(&store)
            .unwrap();
        assert_eq!(calls[0].caller, "app:impress");
        let error =
            crate::record::generate_scenario("app-recording".into(), "".into(), calls).unwrap_err();
        let crate::record::RecordError::NoReplayableCalls { skipped } = error else {
            panic!("expected the unsupported caller to be reported");
        };
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].reason.contains("identity"));
    }

    #[test]
    fn incomplete_output_metadata_refuses_instead_of_embedding_stale_ids() {
        let store = open();
        let mut truncated = payload();
        truncated["result_ids_truncated"] = json!(true);
        let id = record(&store, CallerIdentity::Person, truncated);
        let error = Selection::new(Some("recording"), None, None, None)
            .unwrap()
            .read(&store)
            .unwrap_err();
        assert!(error.message.contains(&id));
    }

    #[tokio::test]
    async fn record_verb_stores_then_returns_the_same_document_with_outcome_assertions() {
        use crate::{DefaultImpressScenarioService, ImpressScenarioService};
        // The recorded verb must be in the inventory to be replayable.
        std::hint::black_box(std::ptr::addr_of!(
            koine_triage::service::__IMPRESS_VERB_TriageService_set_starred
        ));
        let store = std::sync::Arc::new(open());
        record(&*store, CallerIdentity::Person, payload());
        let service = DefaultImpressScenarioService::with_store(store);
        let result = service
            .scenario_record(Some("recording".into()), None, None, None)
            .await;
        assert!(result.scenario.ok, "{result:?}");
        assert_eq!(result.selected, 1);
        let stored = service.scenario_get(result.scenario.id.clone()).await;
        assert_eq!(result.scenario.spec, stored.spec);
        assert_eq!(stored.spec["tier"], "b");
        assert_eq!(stored.spec["steps"][0]["expect"]["ok"], true);
        assert_eq!(stored.spec["steps"][0]["as"], "person");
    }
}
