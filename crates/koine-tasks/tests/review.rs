//! The review loop (docs/plan-koine-trust.md TR-D4, TR-D5): a write the
//! policy holds for a person is kept as a pending call, answered
//! `input_required`, and runs once — as the agent that asked — when the
//! person approves it; a rejected one never runs; an approval made elsewhere
//! is picked up by the sweep; a call that waited too long expires.

use std::sync::{Arc, OnceLock};

use impress_item::operation::{OperationIntent, OperationSpec, OperationType, RetentionTier};
use impress_item::store::AttributedStore;
use impress_item::ActorKind;
use impress_service_core::call;
use impress_service_core::pipeline::policy::{self, ReviewAgents};
use impress_service_core::pipeline::CallerIdentity;
use serde_json::{json, Value};

fn process_store() -> Arc<dyn AttributedStore> {
    static STORE: OnceLock<Arc<dyn AttributedStore>> = OnceLock::new();
    STORE
        .get_or_init(|| {
            koine_tasks::ensure_linked();
            let store: Arc<dyn AttributedStore> = Arc::new(koine_store_memory::MemoryStore::new());
            koine_store::process::install(store.clone());
            policy::install(Arc::new(ReviewAgents::WRITES));
            koine_tasks::review::install_queue();
            store
        })
        .clone()
}

fn serial() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn as_caller(caller: CallerIdentity, name: &str, args: Value) -> Value {
    process_store();
    call::call_async_as(name, caller, args)
        .await
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn agent() -> CallerIdentity {
    CallerIdentity::agent("claude")
}

/// The agent files a task (a write); the policy holds it.
async fn held(title: &str) -> String {
    let answer = as_caller(
        agent(),
        "task-service_enqueue",
        json!({"verb": "task-service_list", "title": title}),
    )
    .await;
    assert_eq!(answer["code"], "review-pending", "{answer}");
    assert_eq!(answer["status"], "input_required", "{answer}");
    answer["pending_id"].as_str().unwrap().to_string()
}

async fn tasks_titled(title: &str) -> usize {
    let listed = as_caller(CallerIdentity::Person, "task-service_list", json!({})).await;
    listed["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["title"] == title)
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_approved_call_runs_once_as_the_agent_and_a_rejected_one_never() {
    let _one = serial().lock().await;
    let pending = held("approved-one").await;

    let listed = as_caller(CallerIdentity::Person, "review-service_list", json!({})).await;
    let row = listed["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == pending)
        .cloned()
        .unwrap_or_else(|| panic!("{listed}"));
    assert_eq!(row["verb"], "task-service_enqueue");
    assert_eq!(row["caller"], "agent:claude");
    assert!(row["question"].as_str().unwrap().contains("agent:claude"));
    assert_eq!(tasks_titled("approved-one").await, 0, "nothing ran yet");

    let refused = as_caller(agent(), "review-service_approve", json!({"id": pending})).await;
    assert_eq!(
        refused["code"], "forbidden",
        "an agent cannot approve: {refused}"
    );

    let approved = as_caller(
        CallerIdentity::Person,
        "review-service_approve",
        json!({"id": pending}),
    )
    .await;
    assert_eq!(approved["ok"], true, "{approved}");
    assert_eq!(approved["pending"]["state"], "done", "{approved}");
    assert_eq!(approved["pending"]["resolution"], "approved");
    assert_eq!(approved["pending"]["result"]["ok"], true, "{approved}");
    assert_eq!(
        approved["pending"]["result"]["task"]["filed_by"], "agent:claude",
        "it ran as the agent that asked: {approved}"
    );
    assert_eq!(tasks_titled("approved-one").await, 1);

    let again = as_caller(
        CallerIdentity::Person,
        "review-service_approve",
        json!({"id": pending}),
    )
    .await;
    assert_eq!(again["code"], "conflict", "{again}");
    assert_eq!(tasks_titled("approved-one").await, 1, "never twice");

    let other = held("rejected-one").await;
    let rejected = as_caller(
        CallerIdentity::Person,
        "review-service_reject",
        json!({"id": other}),
    )
    .await;
    assert_eq!(rejected["pending"]["state"], "rejected", "{rejected}");
    assert_eq!(rejected["pending"]["resolution"], "rejected");
    assert_eq!(tasks_titled("rejected-one").await, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_sweep_runs_an_approval_made_elsewhere_and_expires_what_waited() {
    let _one = serial().lock().await;
    let store = process_store();
    let pending = held("approved-elsewhere").await;
    let shown = as_caller(
        CallerIdentity::Person,
        "review-service_show",
        json!({"id": pending}),
    )
    .await;
    assert_eq!(shown["status"], "input_required");
    let review: uuid::Uuid = shown["pending"]["review_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    // Another app resolves the review request directly (impress's
    // resolve_review FFI does this).
    store
        .apply_operation(OperationSpec {
            target_id: review,
            op_type: OperationType::SetPayload(
                "resolution".into(),
                impress_item::Value::String("approved".into()),
            ),
            intent: OperationIntent::Routine,
            reason: None,
            batch_id: None,
            author: "human".into(),
            author_kind: ActorKind::Human,
            retention: RetentionTier::Durable,
        })
        .unwrap();
    let report = koine_tasks::review::sweep(store.as_ref()).await.unwrap();
    assert_eq!(report.ran, 1, "{report:?}");
    assert_eq!(tasks_titled("approved-elsewhere").await, 1);

    let stale = held("stale").await;
    std::env::set_var("KOINE_REVIEW_EXPIRY_DAYS", "0");
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let report = koine_tasks::review::sweep(store.as_ref()).await.unwrap();
    std::env::remove_var("KOINE_REVIEW_EXPIRY_DAYS");
    assert!(report.expired >= 1, "{report:?}");
    let shown = as_caller(
        CallerIdentity::Person,
        "review-service_show",
        json!({"id": stale}),
    )
    .await;
    assert_eq!(shown["pending"]["state"], "expired", "{shown}");
    assert_eq!(shown["pending"]["resolution"], "expired");
    assert_eq!(tasks_titled("stale").await, 0);
}
