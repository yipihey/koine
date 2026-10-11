//! Cases for [`BlobStore`]: bytes kept beside an item (D5).
//!
//! Each takes the item store as well, because a blob belongs to an item.
//! Cases marked *(beyond D5)* pin a decision the plan leaves implicit; they
//! are listed so a backend that decides differently fails loudly, not
//! quietly.

use impress_item::{AttributedStore, BlobStore, StoreError};

use crate::fixture::*;

/// Bytes put are the bytes got, for any bytes at all.
pub fn blob_put_get_round_trip(store: &dyn AttributedStore, blobs: &dyn BlobStore) {
    let id = insert(store, "test", "Paper");
    assert_eq!(
        blobs.get_blob(id, "paper.pdf").unwrap(),
        None,
        "nothing yet"
    );
    blobs.put_blob(id, "paper.pdf", b"%PDF-1.7 hello").unwrap();
    assert_eq!(
        blobs.get_blob(id, "paper.pdf").unwrap().as_deref(),
        Some(&b"%PDF-1.7 hello"[..])
    );

    // Every byte value, an empty blob, and a large one.
    let every_byte: Vec<u8> = (0..=255u8).collect();
    blobs.put_blob(id, "bytes.bin", &every_byte).unwrap();
    assert_eq!(blobs.get_blob(id, "bytes.bin").unwrap(), Some(every_byte));
    blobs.put_blob(id, "empty", b"").unwrap();
    assert_eq!(
        blobs.get_blob(id, "empty").unwrap(),
        Some(Vec::new()),
        "empty is not absent"
    );
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    blobs.put_blob(id, "big.bin", &big).unwrap();
    assert!(blobs
        .get_blob(id, "big.bin")
        .unwrap()
        .is_some_and(|b| b == big));
}

/// A second put under the same name replaces the first.
pub fn blob_put_replaces(store: &dyn AttributedStore, blobs: &dyn BlobStore) {
    let id = insert(store, "test", "Paper");
    blobs
        .put_blob(id, "figure.png", b"first version, longer")
        .unwrap();
    blobs.put_blob(id, "figure.png", b"second").unwrap();
    assert_eq!(
        blobs.get_blob(id, "figure.png").unwrap().as_deref(),
        Some(&b"second"[..])
    );
    assert_eq!(blobs.list_blobs(id).unwrap(), vec!["figure.png"]);
}

/// Deleting removes the blob; deleting one that is already gone is fine.
pub fn blob_delete(store: &dyn AttributedStore, blobs: &dyn BlobStore) {
    let id = insert(store, "test", "Paper");
    blobs.put_blob(id, "a", b"1").unwrap();
    blobs.put_blob(id, "b", b"2").unwrap();
    blobs.delete_blob(id, "a").unwrap();
    assert_eq!(blobs.get_blob(id, "a").unwrap(), None);
    assert_eq!(blobs.get_blob(id, "b").unwrap().as_deref(), Some(&b"2"[..]));
    blobs
        .delete_blob(id, "a")
        .expect("already gone is not an error");
    blobs
        .delete_blob(id, "never-existed")
        .expect("never there is not an error");
    assert_eq!(blobs.list_blobs(id).unwrap(), vec!["b"]);
}

/// `list_blobs` names an item's blobs, sorted, and only that item's.
pub fn blob_list_is_sorted_and_per_item(store: &dyn AttributedStore, blobs: &dyn BlobStore) {
    let one = insert(store, "test", "One");
    let two = insert(store, "test", "Two");
    assert!(blobs.list_blobs(one).unwrap().is_empty());
    for name in ["zeta.txt", "alpha.txt", "Mid.txt", "figure 1.png", "a.b.c"] {
        blobs.put_blob(one, name, name.as_bytes()).unwrap();
    }
    blobs.put_blob(two, "other.txt", b"x").unwrap();
    let mut expected = vec!["zeta.txt", "alpha.txt", "Mid.txt", "figure 1.png", "a.b.c"];
    expected.sort();
    assert_eq!(blobs.list_blobs(one).unwrap(), expected, "sorted by name");
    assert_eq!(blobs.list_blobs(two).unwrap(), vec!["other.txt"]);
    // The same name under two items is two blobs.
    blobs.put_blob(two, "alpha.txt", b"two's").unwrap();
    assert_eq!(
        blobs.get_blob(one, "alpha.txt").unwrap().as_deref(),
        Some(&b"alpha.txt"[..])
    );
    assert_eq!(
        blobs.get_blob(two, "alpha.txt").unwrap().as_deref(),
        Some(&b"two's"[..])
    );
    // An item with no blobs, and one that does not exist, list nothing.
    assert!(blobs.list_blobs(uuid::Uuid::new_v4()).unwrap().is_empty());
}

/// A blob is invisible to the item: putting one does not touch the row.
pub fn blobs_do_not_change_the_item(store: &dyn AttributedStore, blobs: &dyn BlobStore) {
    let id = insert(store, "test", "Paper");
    let before = store.get(id).unwrap().unwrap();
    let ops = operations_of(store, id).len();
    blobs.put_blob(id, "paper.pdf", b"x").unwrap();
    assert_eq!(store.get(id).unwrap().unwrap(), before);
    assert_eq!(operations_of(store, id).len(), ops);
    assert_eq!(store.count(&all_of("test")).unwrap(), 1);
}

/// *(beyond D5)* A blob belongs to an item: putting one for an id nobody
/// inserted is `NotFound`, and nothing is stored.
pub fn blob_put_for_a_missing_item_is_not_found(
    _store: &dyn AttributedStore,
    blobs: &dyn BlobStore,
) {
    let ghost = uuid::Uuid::new_v4();
    let err = blobs.put_blob(ghost, "x", b"1").expect_err("no such item");
    assert!(
        matches!(err, StoreError::NotFound(id) if id == ghost),
        "{err:?}"
    );
    assert_eq!(blobs.get_blob(ghost, "x").unwrap(), None);
    assert!(blobs.list_blobs(ghost).unwrap().is_empty());
}

/// *(beyond D5)* A blob name is a plain file name (the directory backend
/// stores it as one): empty names and names that would leave the item's folder
/// are refused, and nothing is stored.
pub fn blob_names_must_be_plain_file_names(store: &dyn AttributedStore, blobs: &dyn BlobStore) {
    let id = insert(store, "test", "Paper");
    for bad in [
        "",
        ".",
        "..",
        "../escape",
        "a/b",
        "/abs",
        "a\\b",
        "nul\0byte",
    ] {
        let r = blobs.put_blob(id, bad, b"x");
        assert!(
            matches!(r, Err(StoreError::Validation(_))),
            "{bad:?} must be refused as invalid, got {r:?}"
        );
    }
    assert!(blobs.list_blobs(id).unwrap().is_empty());
    // Names that merely look odd are fine.
    for ok in [
        "with space.pdf",
        "ünïcode.pdf",
        ".hidden",
        "no-extension",
        "UPPER.PDF",
        "a..b",
    ] {
        blobs
            .put_blob(id, ok, b"x")
            .unwrap_or_else(|e| panic!("{ok:?}: {e}"));
    }
    assert_eq!(blobs.list_blobs(id).unwrap().len(), 6);
}

/// *(beyond D5)* Deleting an item deletes its blobs; the same id inserted
/// again starts with none.
pub fn deleting_an_item_deletes_its_blobs(store: &dyn AttributedStore, blobs: &dyn BlobStore) {
    let item = item("test", "Doomed");
    let id = store.insert(item.clone()).unwrap();
    blobs.put_blob(id, "paper.pdf", b"x").unwrap();
    store.delete(id).unwrap();
    assert_eq!(blobs.get_blob(id, "paper.pdf").unwrap(), None);
    assert!(blobs.list_blobs(id).unwrap().is_empty());
    store.insert(item).unwrap();
    assert!(
        blobs.list_blobs(id).unwrap().is_empty(),
        "a new row does not inherit old blobs"
    );
}
