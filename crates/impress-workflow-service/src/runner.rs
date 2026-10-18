//! W2's planner, wired to a store: resolves `store`/`job`/`call` signals
//! from the store port's arrival feed (`ItemStore::arrivals_after`, KM-D3)
//! and drives `impress_workflow::trigger::tick`, then
//! runs every due workflow's planned `call` effects through the pipeline
//! (plan-self-reflective-layer.md § Workflows, "Who runs it").
//!
//! One [`WorkflowEngine`] per host process (`impel-taskd`'s fourth spawn
//! rule, the app's FFI tick) — it owns the `start_delay` anchor
//! ([`impress_workflow::EngineCursors`]) and the per-kind arrival cursors
//! below, and `run_once` is the one entry point both hosts call on their own
//! cadence. Nothing here sleeps: a host decides when to call `run_once`.

use std::collections::BTreeMap;

use impress_item::schema::refs::{CORE_VERB_CALL as VERB_CALL_SCHEMA, TASK as TASK_SCHEMA};
use impress_item::{ArrivalCursor, Item, Value as ItemValue};
use impress_service_core::pipeline::{self, Call, CallerIdentity};
use impress_workflow::{
    trigger, DueRun, EngineCursors, Signal, Trigger, WorkflowSpec, WorkflowState,
};
use koine_store::process::CallStore;
use koine_tasks::job::field::task as task_field;

use crate::store::{self, WorkflowRow};

/// One workflow run's outcome, for a host's own logging.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub workflow_id: String,
    pub name: String,
    pub calls: usize,
    pub error: Option<String>,
}

/// How many arrivals of one kind a tick reads.
const ARRIVAL_PAGE: usize = 64;

/// Per-kind arrival cursor over the store port's arrival feed
/// (`ItemStore::arrivals_after`: the order this store received rows in,
/// including rows sync applied with old timestamps). One per store schema a
/// `store`/`message` trigger names, one for `task@1.0.0` (the `job` trigger)
/// and one for `core/verb-call@1.0.0` (the `call` trigger).
#[derive(Debug, Clone, Default)]
struct ArrivalCursors {
    by_schema: BTreeMap<String, ArrivalCursor>,
}

impl ArrivalCursors {
    /// The next page of `schema`'s arrivals, advancing its cursor past them.
    /// A backend without an arrival feed yields nothing (and no trigger of
    /// these kinds can fire on it).
    fn next_page(&mut self, store: &CallStore, schema: &str) -> Vec<Item> {
        let after = self
            .by_schema
            .get(schema)
            .copied()
            .unwrap_or(ArrivalCursor::START);
        match store
            .port()
            .arrivals_after(after, Some(schema), ARRIVAL_PAGE)
        {
            Ok((items, next)) => {
                self.by_schema.insert(schema.to_string(), next);
                items
            }
            Err(_) => Vec::new(),
        }
    }
}

/// The planner, held for a host process's lifetime.
pub struct WorkflowEngine {
    cursors: EngineCursors,
    arrivals: ArrivalCursors,
    start_delay_ms: i64,
}

impl WorkflowEngine {
    /// `started_at_ms` anchors the `start_delay` rule (plan § Workflows,
    /// "The 90-second rule therefore has one owner") — a host stamps this
    /// once, at process/app start, from its own clock.
    pub fn new(started_at_ms: i64, start_delay_ms: i64) -> Self {
        Self {
            cursors: EngineCursors::new(started_at_ms),
            arrivals: ArrivalCursors::default(),
            start_delay_ms,
        }
    }

    /// One tick: resolve signals from `store`, decide which enabled
    /// workflows fire, run each one's steps through the pipeline as
    /// `System("workflow:<id>")`, and return what happened. A workflow run
    /// that could not acquire [`WorkflowLease`] (another host already
    /// running it) is skipped, not retried until the next call.
    pub fn run_once(
        &mut self,
        store: &CallStore,
        now_ms: i64,
        workspace: &std::path::Path,
    ) -> Vec<RunOutcome> {
        let rows = match store::list(store.port().as_ref()) {
            Ok(rows) => rows,
            Err(_) => return Vec::new(),
        };
        let enabled: Vec<(String, WorkflowSpec)> = rows
            .iter()
            .filter(|r| r.spec.state == WorkflowState::Enabled)
            .map(|r| (r.id.to_string(), r.spec.clone()))
            .collect();
        if enabled.is_empty() {
            // Still worth advancing arrival cursors so a workflow enabled
            // later does not replay a backlog of old signals as its first
            // trigger — see `resolve_signals`.
        }

        // Signals are resolved (and their arrival cursors advanced) only
        // once `start_delay` has passed — resolving earlier would consume a
        // pre-delay signal's cursor position and silently drop it, rather
        // than holding it for the first post-delay tick.
        let ready = now_ms
            >= self
                .cursors
                .started_at_ms
                .saturating_add(self.start_delay_ms);
        let signals = if ready {
            self.resolve_signals(store, &enabled)
        } else {
            Vec::new()
        };
        let due = trigger::tick(
            &enabled,
            now_ms,
            self.start_delay_ms,
            &mut self.cursors,
            &signals,
        );

        let by_id: BTreeMap<String, WorkflowRow> =
            rows.into_iter().map(|r| (r.id.to_string(), r)).collect();
        let mut outcomes = Vec::new();
        for DueRun {
            workflow_id,
            trigger_value,
        } in due
        {
            let Some(row) = by_id.get(&workflow_id) else {
                continue;
            };
            let Ok(_lease) = WorkflowLease::try_acquire(workspace, &workflow_id) else {
                // Another host is already running this workflow — the whole
                // point of the lease (plan § Workflows Tier A: "the daemon
                // and the app never both run one workflow").
                continue;
            };
            outcomes.push(run_workflow(store, row, trigger_value));
        }
        outcomes
    }

    /// Resolves every `store`/`message`/`job`/`call` signal since the last
    /// tick, for the enabled workflows that declare one of those triggers.
    /// A `schedule`/`manual` workflow needs none — `trigger::tick` computes
    /// `schedule` from the clock alone.
    fn resolve_signals(
        &mut self,
        store: &CallStore,
        enabled: &[(String, WorkflowSpec)],
    ) -> Vec<Signal> {
        let mut signals = Vec::new();
        for (id, workflow) in enabled {
            match &workflow.trigger {
                Trigger::Store { kinds, ops, .. } => {
                    for kind in kinds {
                        for item in self.arrivals.next_page(store, kind) {
                            if !ops.is_empty() {
                                // `insert` is the only op an arrival scan can
                                // see directly — an update/delete on an
                                // already-seen row does not move its
                                // place in the arrival feed. A workflow that named only
                                // `update`/`delete` sees nothing from this
                                // scan; that gap is `impress-workflow`'s to
                                // close with a modified-time scan later.
                                if !ops.iter().any(|op| op == "insert") {
                                    continue;
                                }
                            }
                            signals.push(Signal {
                                workflow_id: id.clone(),
                                at_ms: item.modified.timestamp_millis(),
                                trigger_value: serde_json::json!({
                                    "kind": kind,
                                    "op": "insert",
                                    "id": item.id.to_string(),
                                }),
                            });
                        }
                    }
                }
                Trigger::Message { kind, .. } => {
                    for item in self.arrivals.next_page(store, kind) {
                        signals.push(Signal {
                            workflow_id: id.clone(),
                            at_ms: item.modified.timestamp_millis(),
                            trigger_value: serde_json::json!({
                                "kind": kind,
                                "id": item.id.to_string(),
                            }),
                        });
                    }
                }
                Trigger::Job { verb, state } => {
                    for item in self.arrivals.next_page(store, TASK_SCHEMA.as_str()) {
                        let row_verb = item
                            .payload
                            .get(task_field::VERB)
                            .and_then(|v| match v {
                                ItemValue::String(s) => Some(s.as_str()),
                                _ => None,
                            })
                            .unwrap_or("");
                        let row_state = item
                            .payload
                            .get(task_field::STATE)
                            .and_then(|v| match v {
                                ItemValue::String(s) => Some(s.as_str()),
                                _ => None,
                            })
                            .unwrap_or("");
                        if row_verb == verb && row_state == state {
                            signals.push(Signal {
                                workflow_id: id.clone(),
                                at_ms: item.modified.timestamp_millis(),
                                trigger_value: serde_json::json!({
                                    "job_id": item.id.to_string(),
                                    "verb": verb,
                                    "state": state,
                                }),
                            });
                        }
                    }
                }
                Trigger::Call { verb } => {
                    for item in self.arrivals.next_page(store, VERB_CALL_SCHEMA.as_str()) {
                        let row_verb = item
                            .payload
                            .get("verb")
                            .and_then(|v| match v {
                                ItemValue::String(s) => Some(s.as_str()),
                                _ => None,
                            })
                            .unwrap_or("");
                        let ok = matches!(item.payload.get("ok"), Some(ItemValue::Bool(true)));
                        if row_verb == verb && ok {
                            signals.push(Signal {
                                workflow_id: id.clone(),
                                at_ms: item.modified.timestamp_millis(),
                                trigger_value: serde_json::json!({
                                    "call_id": item.id.to_string(),
                                    "verb": verb,
                                }),
                            });
                        }
                    }
                }
                Trigger::Schedule { .. } | Trigger::Manual {} => {}
            }
        }
        signals
    }
}

/// Plans and executes one fired workflow: `impress_workflow::plan` for the
/// steps, then every `Effect::Call` through
/// `impress_service_core::pipeline::invoke_on` as caller
/// `System("workflow:<id>")` (plan § Workflows, "A step's `call` is a verb
/// call through the pipeline with `caller = System(...)`") — landing every
/// run in the call log (`core/verb-call@1.0.0`), joined by that caller name.
fn run_workflow(
    store: &CallStore,
    row: &WorkflowRow,
    trigger_value: serde_json::Value,
) -> RunOutcome {
    let name = row.spec.name.clone();
    let workflow_id = row.id.to_string();
    let plan_result = impress_workflow::plan(
        &row.spec,
        trigger_value,
        &serde_json::json!({}),
        &serde_json::json!({}),
        &serde_json::json!({}),
    );
    let effects = match plan_result {
        Ok((_, effects)) => effects,
        Err(e) => {
            return RunOutcome {
                workflow_id,
                name,
                calls: 0,
                error: Some(e.to_string()),
            }
        }
    };

    let caller = CallerIdentity::system(format!("workflow:{workflow_id}"));
    let mut calls = 0;
    let mut error = None;
    for effect in effects {
        if let impress_workflow::Effect::Call {
            verb,
            args,
            adverbs,
            ..
        } = effect
        {
            let Some(descriptor) = impress_service_core::call::find(&verb) else {
                error = Some(format!("workflow '{name}': unknown verb '{verb}'"));
                break;
            };
            let mut call = Call::new(caller.clone(), args);
            if let Some(raw) = adverbs {
                match serde_json::from_value::<impress_service_core::Adverbs>(raw) {
                    Ok(parsed) => call.adverbs = parsed,
                    Err(e) => {
                        error = Some(format!(
                            "workflow '{name}': step '{verb}' failed: adverbs: {e}"
                        ));
                        break;
                    }
                }
            }
            call.store = Some(store.erased());
            let outcome =
                impress_service_core::runtime::block_on(pipeline::invoke_handle(descriptor, call));
            calls += 1;
            if let Err(e) = outcome {
                error = Some(format!("workflow '{name}': step '{verb}' failed: {e}"));
                break;
            }
        }
    }
    RunOutcome {
        workflow_id,
        name,
        calls,
        error,
    }
}

/// The daemon-vs-app lease (plan § Workflows Tier A, "the daemon and the app
/// never both run one workflow"): an exclusive, non-blocking `flock` on
/// `<workspace>/runtime/workflow-<id>.lock`, held only for the run's own
/// duration — reusing `impel`'s `WorkerLease` pattern
/// (`impress_fs_lock::FileLock::try_exclusive`), one lock file per workflow
/// rather than one for the whole process, so two *different* workflows may
/// run concurrently on the daemon and the app.
pub struct WorkflowLease {
    #[allow(dead_code)]
    lock: impress_fs_lock::FileLock,
}

impl WorkflowLease {
    pub fn try_acquire(workspace: &std::path::Path, workflow_id: &str) -> std::io::Result<Self> {
        let path = workspace
            .join("runtime")
            .join(format!("workflow-{workflow_id}.lock"));
        let lock = impress_fs_lock::FileLock::try_exclusive(&path)?;
        Ok(Self { lock })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use impress_item::{ActorKind, AttributedStore, Priority, Visibility};
    use impress_workflow::{Author, Guards, Review};
    use std::collections::BTreeMap as Map;
    use std::sync::Arc;

    /// Every backend of the port with an arrival feed, each fresh. The temp
    /// dir lives as long as its store.
    fn backends() -> Vec<(&'static str, CallStore, Option<tempfile::TempDir>)> {
        let dir = tempfile::tempdir().unwrap();
        let dir_store = koine_store_dir::DirStore::open(dir.path()).expect("open dir store");
        vec![
            (
                "memory",
                CallStore::of(Arc::new(koine_store_memory::MemoryStore::new())),
                None,
            ),
            ("dir", CallStore::of(Arc::new(dir_store)), Some(dir)),
        ]
    }

    fn enabled_store_workflow() -> WorkflowSpec {
        WorkflowSpec {
            wire_version: 1,
            name: "fixture".into(),
            description: String::new(),
            state: WorkflowState::Enabled,
            author: Author {
                kind: "person".into(),
                name: None,
            },
            trigger: Trigger::Store {
                kinds: vec!["imbib/bibliography-entry".into()],
                ops: vec!["insert".into()],
                debounce_ms: Some(1_000),
            },
            guards: Guards::default(),
            params: vec![],
            sources: Map::new(),
            steps: vec![],
            review: Review::default(),
        }
    }

    /// One row of the kind the fixture's `store` trigger names.
    fn insert_paper(store: &dyn AttributedStore) {
        let mut payload = std::collections::BTreeMap::new();
        payload.insert("title".to_string(), ItemValue::String("A paper".into()));
        store
            .insert(Item {
                id: uuid::Uuid::new_v4(),
                schema: impress_item::schema::refs::IMBIB_BIBLIOGRAPHY_ENTRY,
                payload,
                created: chrono::Utc::now(),
                modified: chrono::Utc::now(),
                author: "test".into(),
                author_kind: ActorKind::Agent,
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
            })
            .expect("insert paper");
    }

    /// The proof at this layer: `run_once` before `start_delay` does
    /// nothing, whatever is in the store; after it, the arrival the
    /// `store` trigger names fires it once — on every backend.
    #[test]
    fn no_run_before_start_delay_through_the_engine() {
        for (name, store, _dir) in backends() {
            store::insert(
                store.port().as_ref(),
                &enabled_store_workflow(),
                ActorKind::Human,
            )
            .expect("insert");
            // A matching row, inserted before the engine's first tick.
            insert_paper(store.port().as_ref());

            let workspace = tempfile::tempdir().unwrap();
            let mut engine = WorkflowEngine::new(0, 90_000);
            let outcomes = engine.run_once(&store, 10_000, workspace.path());
            assert!(
                outcomes.is_empty(),
                "{name}: must not run before start_delay: {outcomes:?}"
            );

            let outcomes = engine.run_once(&store, 91_000, workspace.path());
            assert_eq!(
                outcomes.len(),
                1,
                "{name}: runs once start_delay has passed"
            );

            // The arrival was consumed: a later tick with nothing new is quiet.
            let outcomes = engine.run_once(&store, 200_000, workspace.path());
            assert!(outcomes.is_empty(), "{name}: the arrival fires once");
        }
    }

    /// The proof: the lease prevents two hosts (here, two engines sharing a
    /// workspace) from both running the same fired workflow.
    #[test]
    fn the_lease_prevents_a_double_run() {
        let workspace = tempfile::tempdir().unwrap();
        let _held = WorkflowLease::try_acquire(workspace.path(), "wf-1").expect("first acquire");
        let second = WorkflowLease::try_acquire(workspace.path(), "wf-1");
        assert!(
            second.is_err(),
            "a second lease on the same workflow must fail while the first is held"
        );
    }

    /// A proposed workflow never runs, however far past `start_delay` the
    /// clock is: `run_once` keeps only enabled rows. (impress-apps proves the
    /// same for a workflow its history miner proposes.)
    #[test]
    fn a_proposed_workflow_never_runs_through_the_engine() {
        for (name, store, _dir) in backends() {
            let mut spec = enabled_store_workflow();
            spec.state = WorkflowState::Proposed;
            store::insert(store.port().as_ref(), &spec, ActorKind::Agent).expect("insert");
            insert_paper(store.port().as_ref());
            let workspace = tempfile::tempdir().unwrap();
            let mut engine = WorkflowEngine::new(0, 90_000);
            let outcomes = engine.run_once(&store, 1_000_000, workspace.path());
            assert!(
                outcomes.is_empty(),
                "{name}: a proposed workflow must never run: {outcomes:?}"
            );
        }
    }
}
