//! Cases for the port's two optional extensions (`docs/plan-koine-kit-moves.md`
//! KM-D3): ranked text search and the arrival feed.
//!
//! Text search is on every backend (the port has a default), so those cases
//! always run; what a backend may differ in is the score, which the cases do
//! not pin, only the order. The arrival feed is declared by
//! `StoreCapabilities::arrival_feed`; a backend that declares `false` skips
//! those cases loudly and must answer `Unsupported`.

use impress_item::{ArrivalCursor, AttributedStore, FieldMutation, Item, StoreError};

use crate::fixture::*;

fn titled(store: &dyn AttributedStore, schema_name: &str, title: &str) -> impress_item::ItemId {
    insert(store, schema_name, title)
}

fn hits(
    store: &dyn AttributedStore,
    schema_name: Option<&str>,
    text: &str,
    limit: usize,
) -> Vec<impress_item::ItemId> {
    store
        .search_text(schema_name, text, limit)
        .expect("search_text")
        .into_iter()
        .map(|h| h.id)
        .collect()
}

/// An item with all the query's tokens ranks above one with some of them,
/// and an item with none is not a hit.
pub fn text_search_ranks_all_tokens_above_some(store: &dyn AttributedStore) {
    let some = titled(store, "test", "Quokka habitats of the southern coast");
    let all = titled(store, "test", "Quokka and marmot habitats");
    let one = titled(store, "test", "Marmot burrows");
    let none = titled(store, "test", "Unrelated heron nests");
    let found = store
        .search_text(Some("test"), "quokka marmot habitats", 10)
        .unwrap();
    let ids: Vec<_> = found.iter().map(|h| h.id).collect();
    assert_eq!(ids.first(), Some(&all), "all three tokens first: {ids:?}");
    assert!(
        ids.contains(&some) && ids.contains(&one),
        "partial matches are hits"
    );
    assert!(!ids.contains(&none), "no token, no hit");
    let pos = |id| ids.iter().position(|x| *x == id).unwrap();
    assert!(pos(all) < pos(some), "all tokens above two of three");
    assert!(pos(all) < pos(one), "all tokens above one of three");
    assert!(
        found.windows(2).all(|w| w[0].score >= w[1].score),
        "scores do not increase down the list: {found:?}"
    );
}

/// Case and punctuation do not matter; the body and note fields are searched
/// as well as the title.
pub fn text_search_reads_the_text_fields(store: &dyn AttributedStore) {
    let mut note = item("test", "Plain title");
    note.payload
        .insert("note".into(), text("Remember the Axolotl, please!"));
    let note = store.insert(note).unwrap();
    let mut body = item("test", "Another title");
    body.payload
        .insert("body_content".into(), text("an axolotl swam by"));
    let body = store.insert(body).unwrap();
    let ids = hits(store, Some("test"), "AXOLOTL?!", 10);
    assert!(ids.contains(&note), "found in note: {ids:?}");
    assert!(ids.contains(&body), "found in body_content: {ids:?}");
}

/// `schema` narrows (`None` searches every kind) and `limit` caps.
pub fn text_search_respects_schema_and_limit(store: &dyn AttributedStore) {
    let a = titled(store, "test", "Pangolin armour");
    let b = titled(store, "test", "Pangolin armour plates");
    let other = titled(store, "other", "Pangolin armour");
    let in_test = hits(store, Some("test"), "pangolin armour", 10);
    assert!(in_test.contains(&a) && in_test.contains(&b));
    assert!(!in_test.contains(&other), "another schema is not searched");
    let everywhere = hits(store, None, "pangolin armour", 10);
    assert!(
        everywhere.contains(&a) && everywhere.contains(&b) && everywhere.contains(&other),
        "no schema searches every kind: {everywhere:?}"
    );
    assert_eq!(hits(store, Some("test"), "pangolin armour", 1).len(), 1);
    assert!(hits(store, Some("test"), "pangolin armour", 0).is_empty());
    assert_eq!(hits(store, Some("other"), "pangolin", 5), vec![other]);
}

/// Unknown tokens, an empty query and one with only one-character tokens find
/// nothing, and are not errors.
pub fn text_search_finds_nothing_for_unknown_tokens(store: &dyn AttributedStore) {
    titled(store, "test", "Pangolin armour");
    assert!(hits(store, Some("test"), "zzyzx wibblefrog", 10).is_empty());
    assert!(hits(store, Some("test"), "", 10).is_empty());
    assert!(hits(store, Some("test"), "   ", 10).is_empty());
    assert!(hits(store, Some("test"), "a b c", 10).is_empty());
    assert!(hits(store, Some("test"), "((( \" -", 10).is_empty());
}

fn feed_or_skip(case: &str, store: &dyn AttributedStore) -> bool {
    if store.capabilities().arrival_feed {
        return true;
    }
    skip(case, "the store declares arrival_feed: false");
    assert!(
        matches!(
            store.arrivals_after(ArrivalCursor::START, None, 1),
            Err(StoreError::Unsupported(_))
        ),
        "{case}: a store without an arrival feed answers Unsupported"
    );
    false
}

fn drain_feed(
    store: &dyn AttributedStore,
    from: ArrivalCursor,
    schema_name: Option<&str>,
) -> (Vec<Item>, ArrivalCursor) {
    store
        .arrivals_after(from, schema_name, 1000)
        .expect("arrivals_after")
}

/// Rows come back in the order they were inserted, not sorted by anything of
/// their own; the cursor then reads "nothing new".
pub fn arrivals_come_in_insertion_order(store: &dyn AttributedStore) {
    if !feed_or_skip("arrivals_come_in_insertion_order", store) {
        return;
    }
    let (none, start) = drain_feed(store, ArrivalCursor::START, Some("test"));
    assert!(none.is_empty(), "an empty store has no arrivals");
    let ids: Vec<_> = ["m", "c", "x", "a", "q"]
        .iter()
        .map(|t| titled(store, "test", t))
        .collect();
    let (rows, cursor) = drain_feed(store, ArrivalCursor::START, Some("test"));
    assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), ids);
    assert!(cursor > start, "the cursor moved: {start:?} -> {cursor:?}");
    let (again, same) = drain_feed(store, cursor, Some("test"));
    assert!(again.is_empty(), "nothing after the last row");
    assert!(same >= cursor, "an empty page never moves the cursor back");
}

/// A page of `limit` rows and its cursor continue where the page ended, with
/// no row twice and none missed.
pub fn arrivals_page_with_the_cursor(store: &dyn AttributedStore) {
    if !feed_or_skip("arrivals_page_with_the_cursor", store) {
        return;
    }
    let ids: Vec<_> = (0..7)
        .map(|i| titled(store, "test", &format!("row {i}")))
        .collect();
    let mut cursor = ArrivalCursor::START;
    let mut seen = Vec::new();
    let mut pages = 0;
    loop {
        let (page, next) = store.arrivals_after(cursor, Some("test"), 3).unwrap();
        if page.is_empty() {
            break;
        }
        assert!(page.len() <= 3, "a page respects the limit");
        assert!(next > cursor);
        seen.extend(page.iter().map(|r| r.id));
        cursor = next;
        pages += 1;
        assert!(pages < 10, "paging terminates");
    }
    assert_eq!(seen, ids);
    assert_eq!(pages, 3, "seven rows in pages of three");
}

/// The schema narrows the feed exactly; the cursor is still store-wide, so
/// one cursor serves every filter.
pub fn arrivals_filter_by_schema(store: &dyn AttributedStore) {
    if !feed_or_skip("arrivals_filter_by_schema", store) {
        return;
    }
    let a1 = titled(store, "test", "a1");
    let b1 = titled(store, "other", "b1");
    let a2 = titled(store, "test", "a2");
    let b2 = titled(store, "other", "b2");
    let ids = |schema_name| -> Vec<_> {
        drain_feed(store, ArrivalCursor::START, schema_name)
            .0
            .iter()
            .map(|r| r.id)
            .collect()
    };
    assert_eq!(ids(Some("test")), vec![a1, a2]);
    assert_eq!(ids(Some("other")), vec![b1, b2]);
    assert!(
        ids(Some("tes")).is_empty(),
        "the schema is exact, not a prefix"
    );
    let all = ids(None);
    let pos = |id| {
        all.iter()
            .position(|x| *x == id)
            .expect("in the unfiltered feed")
    };
    assert!(pos(a1) < pos(b1) && pos(b1) < pos(a2) && pos(a2) < pos(b2));
}

/// The reason the feed exists: a row that arrives later with an older
/// `created` (a sync) is the next arrival, where a `created` scan from the
/// last row's timestamp would never see it.
pub fn arrivals_include_a_row_inserted_later_with_an_older_created(store: &dyn AttributedStore) {
    if !feed_or_skip(
        "arrivals_include_a_row_inserted_later_with_an_older_created",
        store,
    ) {
        return;
    }
    let mut recent = item("test", "recent");
    recent.created = day(20);
    recent.modified = day(20);
    let recent = store.insert(recent).unwrap();
    let (first, cursor) = drain_feed(store, ArrivalCursor::START, Some("test"));
    assert_eq!(first.iter().map(|r| r.id).collect::<Vec<_>>(), vec![recent]);

    let mut old = item("test", "old, synced late");
    old.created = day(2);
    old.modified = day(2);
    let old = store.insert(old).unwrap();
    let (next, cursor2) = drain_feed(store, cursor, Some("test"));
    assert_eq!(
        next.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![old],
        "the late row is the next arrival"
    );
    assert_eq!(next[0].created, day(2), "and keeps its own timestamp");
    assert!(cursor2 > cursor);
}

/// Writing to a row that has arrived does not make it arrive again, and
/// deleting one takes it out of the feed.
pub fn arrivals_do_not_repeat_updated_rows_and_forget_deleted_ones(store: &dyn AttributedStore) {
    if !feed_or_skip(
        "arrivals_do_not_repeat_updated_rows_and_forget_deleted_ones",
        store,
    ) {
        return;
    }
    let first = titled(store, "test", "first");
    let doomed = titled(store, "test", "doomed");
    let (_, cursor) = drain_feed(store, ArrivalCursor::START, Some("test"));
    store
        .update(first, vec![FieldMutation::SetStarred(true)])
        .unwrap();
    store
        .apply_operation(spec(first, impress_item::OperationType::SetRead(true)))
        .unwrap();
    let (after_write, cursor) = drain_feed(store, cursor, Some("test"));
    assert!(after_write.is_empty(), "a write is not an arrival");
    store.delete(doomed).unwrap();
    let later = titled(store, "test", "later");
    let (rows, _) = drain_feed(store, cursor, Some("test"));
    assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![later]);
    let (all, _) = drain_feed(store, ArrivalCursor::START, Some("test"));
    assert_eq!(
        all.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![first, later],
        "a deleted row is gone from the feed"
    );
}

/// An operation is a row the store received too: it is in the feed under
/// `core/operation`.
pub fn arrivals_include_operation_rows_under_their_schema(store: &dyn AttributedStore) {
    if !feed_or_skip("arrivals_include_operation_rows_under_their_schema", store) {
        return;
    }
    let target = titled(store, "test", "target");
    let op = store
        .apply_operation(spec(target, impress_item::OperationType::SetRead(true)))
        .unwrap();
    let (rows, _) = drain_feed(store, ArrivalCursor::START, Some("core/operation"));
    assert!(rows.iter().any(|r| r.id == op), "the operation arrived");
}

/// A cursor is plain data: it survives a round trip through its number.
pub fn arrival_cursors_round_trip_as_numbers(store: &dyn AttributedStore) {
    if !feed_or_skip("arrival_cursors_round_trip_as_numbers", store) {
        return;
    }
    for t in ["one", "two", "three"] {
        titled(store, "test", t);
    }
    let (page, cursor) = store
        .arrivals_after(ArrivalCursor::START, Some("test"), 2)
        .unwrap();
    assert_eq!(page.len(), 2);
    let stored = ArrivalCursor::from_u64(cursor.to_u64());
    assert_eq!(stored, cursor);
    let json = serde_json_round_trip(cursor);
    assert_eq!(json, cursor);
    let (rest, _) = store.arrivals_after(stored, Some("test"), 10).unwrap();
    assert_eq!(titles(&rest), vec!["three".to_owned()]);
}

fn serde_json_round_trip(c: ArrivalCursor) -> ArrivalCursor {
    let s = serde_json::to_string(&c).unwrap();
    serde_json::from_str(&s).unwrap()
}
