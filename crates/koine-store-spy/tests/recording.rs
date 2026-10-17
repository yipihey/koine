//! What a window records, and that nothing is recorded outside one.

use std::sync::Arc;

use impress_item::{FieldMutation, ItemQuery, ItemStore, SchemaRef};
use koine_store_conformance::fixture::item;
use koine_store_memory::MemoryStore;
use koine_store_spy::{SpyStore, ANY};

#[test]
fn a_window_records_reads_and_writes_and_closes_clean() {
    let spy = SpyStore::new(Arc::new(MemoryStore::new()));
    let before = spy.insert(item("note", "before the window")).unwrap();
    spy.start();
    let id = spy.insert(item("manuscript", "Draft")).unwrap();
    spy.get(before).unwrap();
    spy.query(&ItemQuery {
        schema: Some(SchemaRef::from_stored("figure".into())),
        ..Default::default()
    })
    .unwrap();
    spy.query(&ItemQuery::default()).unwrap();
    spy.count(&ItemQuery::default()).unwrap();
    spy.update(id, vec![FieldMutation::SetStarred(true)])
        .unwrap();
    spy.delete(before).unwrap();
    let seen = spy.stop();
    let reads: Vec<&str> = seen.reads.iter().map(String::as_str).collect();
    assert!(reads.contains(&"figure"), "{reads:?}");
    assert!(reads.contains(&"note"), "{reads:?}");
    assert!(
        reads.contains(&"manuscript"),
        "a schemaless query reads the kinds it returned"
    );
    assert!(reads.contains(&ANY), "a schemaless count reads every kind");
    let writes: Vec<&str> = seen.writes.iter().map(String::as_str).collect();
    assert_eq!(writes, vec!["manuscript", "note"]);
    assert_eq!(seen.inserted, vec![id]);
    assert_eq!(seen.deleted, vec![before]);
    assert!(!spy.is_recording());
    assert_eq!(spy.stop(), Default::default(), "a closed window is empty");
}
