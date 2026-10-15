//! A verb is a task (docs/plan-koine-ai-tasks.md Q-D3): `task-service_*`
//! files and steers verb tasks, a scheduler with the verb executor runs
//! them over the process store, as its agent.

use std::sync::{Arc, OnceLock};

use impress_item::store::AttributedStore;
use impress_service_core::call;
use impress_service_core::pipeline::CallerIdentity;
use koine_tasks::{ItemTaskStore, Scheduler, SchedulerConfig, VerbTaskExecutor};
use serde_json::{json, Value};

/// One process store for this test binary (the registry is process-wide).
fn process_store() -> Arc<dyn AttributedStore> {
    static STORE: OnceLock<Arc<dyn AttributedStore>> = OnceLock::new();
    STORE
        .get_or_init(|| {
            koine_tasks::ensure_linked();
            koine_agent::ensure_linked();
            let store: Arc<dyn AttributedStore> = Arc::new(koine_store_memory::MemoryStore::new());
            koine_store::process::install(store.clone());
            store
        })
        .clone()
}

/// One scheduler at a time: the tests share the process store, and a host
/// runs one scheduler per store (two with one actor resume each other's
/// tasks).
fn serial() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn scheduler() -> Scheduler {
    let mut scheduler = Scheduler::new(
        Arc::new(ItemTaskStore::new(process_store())),
        SchedulerConfig {
            actor: "test-scheduler".into(),
            batch: 64,
            start_delay: std::time::Duration::ZERO,
            poll_interval: std::time::Duration::ZERO,
            retry_base_ms: 0,
        },
    );
    scheduler.register(Arc::new(VerbTaskExecutor::new("tasks")));
    scheduler
}

async fn verb(name: &str, args: Value) -> Value {
    process_store();
    call::call_async_as(name, CallerIdentity::Person, args)
        .await
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

async fn enqueue(args: Value) -> String {
    let filed = verb("task-service_enqueue", args).await;
    assert_eq!(filed["ok"], true, "{filed}");
    filed["task"]["id"].as_str().unwrap().to_string()
}

async fn show(id: &str) -> Value {
    let shown = verb("task-service_show", json!({ "id": id })).await;
    assert_eq!(shown["ok"], true, "{shown}");
    shown["task"].clone()
}

/// Run passes until `id` is no longer pending or running (at most 8).
async fn run_until_settled(scheduler: &Scheduler, id: &str) -> Value {
    for _ in 0..8 {
        scheduler.run_once().await.unwrap();
        let task = show(id).await;
        if task["state"] != "pending" && task["state"] != "running" {
            return task;
        }
    }
    show(id).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_verb_task_runs_its_verb_as_the_agent_and_keeps_the_answer() {
    let _serial = serial().lock().await;
    let id = enqueue(json!({
        "verb": "agent-service_ask",
        "args": { "prompt": "filed by a task", "with_context": false },
    }))
    .await;
    let filed = show(&id).await;
    assert_eq!(filed["state"], "pending");
    assert_eq!(filed["kind"], "koine.verb");
    assert_eq!(filed["filed_by"], "human");
    assert_eq!(filed["args"]["prompt"], "filed by a task");

    let task = run_until_settled(&scheduler(), &id).await;
    assert_eq!(task["state"], "done", "{task}");
    assert_eq!(task["attempts"], 1);
    assert_eq!(task["result"]["ok"], true, "{task}");
    // The person filed it; the verb ran as the scheduler's agent.
    assert_eq!(
        task["result"]["request"]["created_by"], "agent:tasks",
        "{task}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refusal_fails_the_task_with_its_code_and_keeps_the_answer() {
    let _serial = serial().lock().await;
    let id = enqueue(json!({
        "verb": "task-service_show",
        "args": { "id": "not-a-task-id" },
    }))
    .await;
    let task = run_until_settled(&scheduler(), &id).await;
    assert_eq!(task["state"], "failed", "{task}");
    let error = task["error"].as_str().unwrap();
    assert!(error.contains("invalid-argument"), "{error}");
    assert_eq!(task["result"]["ok"], false);
    assert_eq!(task["attempts"], 1, "a permanent refusal is not retried");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_task_waits_for_what_it_depends_on_and_for_its_time() {
    let _serial = serial().lock().await;
    let first = enqueue(json!({
        "verb": "task-service_list",
        "args": { "kind": "nothing-has-this-kind" },
    }))
    .await;
    let second = enqueue(json!({
        "verb": "task-service_list",
        "args": { "kind": "nothing-has-this-kind" },
        "depends_on": [first],
    }))
    .await;
    let later = enqueue(json!({
        "verb": "task-service_list",
        "args": {},
        "not_before_ms": chrono::Utc::now().timestamp_millis() + 3_600_000,
    }))
    .await;
    assert_eq!(show(&second).await["depends_on"], json!([first]));

    let scheduler = scheduler();
    scheduler.run_once().await.unwrap();
    assert_eq!(show(&first).await["state"], "done");
    assert_eq!(
        show(&second).await["state"],
        "pending",
        "the dependency was not done when the pass began"
    );
    scheduler.run_once().await.unwrap();
    assert_eq!(show(&second).await["state"], "done");
    assert_eq!(
        show(&later).await["state"],
        "pending",
        "not before its time"
    );
    let cancelled = verb("task-service_cancel", json!({ "id": later })).await;
    assert_eq!(cancelled["task"]["state"], "cancelled", "{cancelled}");
}

#[tokio::test(flavor = "multi_thread")]
async fn enqueue_refuses_what_it_cannot_run_and_retry_files_a_new_task() {
    let _serial = serial().lock().await;
    let unknown = verb(
        "task-service_enqueue",
        json!({ "verb": "no-such-service_nothing" }),
    )
    .await;
    assert_eq!(unknown["ok"], false);
    assert_eq!(unknown["code"], "invalid-argument");
    let not_object = verb(
        "task-service_enqueue",
        json!({ "verb": "task-service_list", "args": [1] }),
    )
    .await;
    assert_eq!(not_object["code"], "invalid-argument");

    let id = enqueue(json!({
        "verb": "task-service_show",
        "args": { "id": "still-not-an-id" },
    }))
    .await;
    let failed = run_until_settled(&scheduler(), &id).await;
    assert_eq!(failed["state"], "failed");
    // A finished task cannot be cancelled.
    let refused = verb("task-service_cancel", json!({ "id": id })).await;
    assert_eq!(refused["code"], "conflict", "{refused}");

    let again = verb("task-service_retry", json!({ "id": id })).await;
    assert_eq!(again["ok"], true, "{again}");
    let retry = &again["task"];
    assert_ne!(retry["id"], json!(id));
    assert_eq!(retry["retry_of"], json!(id));
    assert_eq!(retry["state"], "pending");
    assert_eq!(retry["args"], json!({ "id": "still-not-an-id" }));
    let pending = verb("task-service_retry", json!({ "id": retry["id"] })).await;
    assert_eq!(
        pending["code"], "conflict",
        "only a finished task is retried"
    );

    let listed = verb(
        "task-service_list",
        json!({ "state": "failed", "kind": "koine.verb" }),
    )
    .await;
    let rows = listed["rows"].as_array().unwrap();
    assert!(rows.iter().any(|row| row["id"] == json!(id)));
    assert!(rows.iter().all(|row| row["state"] == "failed"));
    let bad_state = verb("task-service_list", json!({ "state": "sleeping" })).await;
    assert_eq!(bad_state["code"], "invalid-argument");
}
