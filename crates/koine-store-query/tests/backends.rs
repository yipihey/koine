//! The store-query verbs on the directory store: search, both directions of
//! the edge walk, get and browse agree with the memory store
//! (docs/plan-koine-kit-moves.md KM-D2, KM-D8).

use std::collections::BTreeMap;
use std::sync::Arc;

use impress_item::{
    ActorKind, AttributedStore, EdgeType, FieldMutation, Item, Priority, TypedReference, Value,
    Visibility,
};
use koine_store_query::{DefaultStoreQueryService, StoreQueryService};

fn insert(store: &dyn AttributedStore, schema: &str, title: &str) -> String {
    let now = chrono::Utc::now();
    let mut payload = BTreeMap::new();
    payload.insert("title".to_string(), Value::String(title.into()));
    store
        .insert(Item {
            id: uuid::Uuid::new_v4(),
            schema: serde_json::from_value(serde_json::Value::String(schema.into())).unwrap(),
            payload,
            created: now,
            modified: now,
            author: "test".into(),
            author_kind: ActorKind::Human,
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
        })
        .unwrap()
        .to_string()
}

async fn round_trip(store: Arc<dyn AttributedStore>) {
    let paper = insert(store.as_ref(), "manuscript", "Nucleosynthesis in dwarfs");
    let figure = insert(store.as_ref(), "figure", "Nucleosynthesis yields");
    insert(store.as_ref(), "task", "Something else");
    store
        .update(
            figure.parse().unwrap(),
            vec![FieldMutation::AddReference(TypedReference {
                target: paper.parse().unwrap(),
                edge_type: EdgeType::Contains,
                metadata: None,
            })],
        )
        .unwrap();
    let svc = DefaultStoreQueryService::with_store(store);

    let found = svc.search_all("nucleo".into(), 0).await;
    assert!(found.ok, "{}", found.message);
    assert_eq!(found.kinds, ["figure", "manuscript"], "{found:?}");

    let related = svc.related_items(paper.clone(), 0).await;
    assert_eq!(related.related.len(), 1, "{related:?}");
    assert_eq!(related.related[0].id, figure);
    assert_eq!(related.related[0].direction, "incoming");
    let other_way = svc.related_items(figure.clone(), 0).await;
    assert_eq!(other_way.related[0].direction, "outgoing");

    let got = svc.get_item(paper.clone()).await;
    assert_eq!(got.item.unwrap().title, "Nucleosynthesis in dwarfs");
    // Every kind includes the `core/operation` row the edge's update wrote.
    let listed = svc.list_items(String::new(), 0, 0).await;
    assert_eq!(listed.total, 4, "{listed:?}");
    let manuscripts = svc.list_items("manuscript".into(), 0, 0).await;
    assert_eq!(manuscripts.total, 1);
    assert_eq!(manuscripts.items[0].id, paper);
}

#[tokio::test]
async fn store_query_on_the_memory_store() {
    round_trip(Arc::new(koine_store_memory::MemoryStore::new())).await;
}

#[tokio::test]
async fn store_query_on_the_directory_store() {
    let dir = tempfile::tempdir().unwrap();
    round_trip(Arc::new(
        koine_store_dir::DirStore::open(dir.path()).unwrap(),
    ))
    .await;
}
