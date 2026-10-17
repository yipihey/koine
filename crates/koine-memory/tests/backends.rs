//! The memory verbs over every backend of the store port: remember, the
//! dedup gate, recall, supersession and the head count behave the same on the
//! memory and the directory store (docs/plan-koine-kit-moves.md KM-D2).

use std::sync::Arc;

use impress_item::AttributedStore;
use koine_memory::{DefaultMemoryService, MemoryService};

async fn remember(svc: &DefaultMemoryService, title: &str, body: &str) -> (String, String) {
    let r = svc
        .remember(
            "claim".into(),
            title.into(),
            body.into(),
            "fact".into(),
            -1.0,
            vec![],
            vec![],
        )
        .await;
    assert!(r.ok, "{r:?}");
    (r.action, r.claim_id)
}

async fn memory_round_trip(store: Arc<dyn AttributedStore>) {
    let svc = DefaultMemoryService::with_store(store);

    let (action, first) = remember(&svc, "Flux units", "The flux column is in millijansky.").await;
    assert_eq!(action, "inserted");
    // The same claim again is one memory with more evidence, not two.
    let (action, again) = remember(&svc, "Flux units", "The flux column is in millijansky.").await;
    assert_eq!(action, "confirmed");
    assert_eq!(again, first);

    let recalled = svc
        .recall("flux millijansky".into(), String::new(), 0, false)
        .await;
    assert!(recalled.ok, "{}", recalled.message);
    assert!(
        recalled.entries.iter().any(|e| e.id == first),
        "{recalled:?}"
    );

    let superseded = svc
        .supersede_claim(
            first.clone(),
            "Flux units".into(),
            "The flux column is in jansky.".into(),
            "checked the header".into(),
        )
        .await;
    assert!(superseded.ok, "{superseded:?}");
    let heads = svc.recall(String::new(), String::new(), 0, false).await;
    let ids: Vec<&str> = heads.entries.iter().map(|e| e.id.as_str()).collect();
    assert!(ids.contains(&superseded.new_id.as_str()), "{ids:?}");
    assert!(
        !ids.contains(&first.as_str()),
        "a superseded memory is not a head: {ids:?}"
    );

    let status = svc.memory_status().await;
    let claims = status
        .schemas
        .iter()
        .find(|s| s.schema_ref == "memory/claim@1.0.0")
        .expect("claim row");
    assert_eq!((claims.total, claims.heads), (2, 1));
}

#[tokio::test]
async fn memory_on_the_memory_store() {
    memory_round_trip(Arc::new(koine_store_memory::MemoryStore::new())).await;
}

#[tokio::test]
async fn memory_on_the_directory_store() {
    let dir = tempfile::tempdir().unwrap();
    memory_round_trip(Arc::new(
        koine_store_dir::DirStore::open(dir.path()).unwrap(),
    ))
    .await;
}
