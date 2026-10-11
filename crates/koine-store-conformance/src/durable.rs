//! Cases for stores that outlive the process, and for stores two handles may
//! open at once.
//!
//! The suite cannot reopen a store by itself: where a store lives is the
//! backend's business. The host of the suite supplies [`Open`], a function
//! that opens *the same* store each time it is called (the macro's `reopen`
//! arm; see [`crate::suite!`]). A case drops its handle, opens again, and
//! asks whether the rows are there.
//!
//! A case that needs a capability the store does not declare says so and
//! returns. A store that declares `durable` or `shared` but was given no
//! `reopen` arm fails, because the claim cannot be checked.

use impress_item::{
    AttributedStore, EdgeType, FieldMutation, GuardedWrite, ItemQuery, OperationType, Value,
};

use crate::fixture::*;

/// Opens the same store every time it is called.
pub type Open<'a> = Option<&'a dyn Fn() -> Box<dyn AttributedStore>>;

/// The opener, if the case can run: `None` after saying why it cannot.
fn opener<'a>(
    case: &str,
    probe: &dyn AttributedStore,
    open: Open<'a>,
    needs_shared: bool,
) -> Option<&'a dyn Fn() -> Box<dyn AttributedStore>> {
    let caps = probe.capabilities();
    let (claim, holds) = if needs_shared {
        ("shared", caps.shared)
    } else {
        ("durable", caps.durable)
    };
    if !holds {
        skip(case, &format!("the store declares {claim}: false"));
        return None;
    }
    match open {
        Some(open) => Some(open),
        None => panic!(
            "{case}: the store declares {claim}: true but the suite was given no `reopen` arm, \
             so the claim cannot be checked"
        ),
    }
}

/// Rows, their operations, tags, references and deletions are still there
/// when the store is opened again.
pub fn reopen_keeps_items_operations_and_deletions(probe: &dyn AttributedStore, open: Open<'_>) {
    let Some(open) = opener(
        "reopen_keeps_items_operations_and_deletions",
        probe,
        open,
        false,
    ) else {
        return;
    };
    let (kept, cited, doomed, op);
    {
        let store = open();
        cited = insert(store.as_ref(), "test", "Cited");
        let mut it = item("test", "Kept");
        it.payload.insert("n".into(), Value::Int(7));
        it.tags = vec!["a/b".into()];
        kept = store.insert(it).unwrap();
        doomed = insert(store.as_ref(), "test", "Doomed");
        store
            .update(
                kept,
                vec![
                    FieldMutation::SetRead(true),
                    FieldMutation::AddReference(reference(cited, EdgeType::Cites)),
                ],
            )
            .unwrap();
        op = store
            .apply_operation(spec(kept, OperationType::SetStarred(true)))
            .unwrap();
        store.delete(doomed).unwrap();
    }
    let store = open();
    let row = store.get(kept).unwrap().expect("the row survived");
    assert_eq!(row.payload.get("n"), Some(&Value::Int(7)));
    assert_eq!(row.tags, vec!["a/b".to_owned()]);
    assert!(row.is_read && row.is_starred);
    assert_eq!(row.references.len(), 1);
    assert_eq!(row.references[0].target, cited);
    assert!(
        store.get(doomed).unwrap().is_none(),
        "a deletion is durable"
    );
    assert!(
        store.get(op).unwrap().is_some(),
        "the operation row survived"
    );
    assert_eq!(operations_of(store.as_ref(), kept).len(), 3);
    assert_eq!(store.count(&all_of("test")).unwrap(), 2);
}

/// The clock survives: after reopening, a new write's clock is greater than
/// every clock the store gave before, so clocks stay comparable across runs.
pub fn reopen_keeps_the_clock_monotonic(probe: &dyn AttributedStore, open: Open<'_>) {
    let Some(open) = opener("reopen_keeps_the_clock_monotonic", probe, open, false) else {
        return;
    };
    let (id, last) = {
        let store = open();
        let id = insert(store.as_ref(), "test", "Target");
        for _ in 0..20 {
            store
                .apply_operation(spec(id, OperationType::SetRead(true)))
                .unwrap();
        }
        let last = store.logical_clock_of(id).unwrap().unwrap();
        (id, last)
    };
    let store = open();
    assert_eq!(
        store.logical_clock_of(id).unwrap(),
        Some(last),
        "the row's clock is unchanged"
    );
    let fresh = insert(store.as_ref(), "test", "After");
    assert!(store.logical_clock_of(fresh).unwrap().unwrap() > last);
    store
        .apply_operation(spec(id, OperationType::SetRead(false)))
        .unwrap();
    assert!(store.logical_clock_of(id).unwrap().unwrap() > last);
}

/// Two handles open on one store at once see each other's committed writes,
/// and the guard is honest across them: a writer holding a stale clock is
/// refused no matter which handle wrote.
pub fn two_handles_share_one_store(probe: &dyn AttributedStore, open: Open<'_>) {
    let Some(open) = opener("two_handles_share_one_store", probe, open, true) else {
        return;
    };
    let first = open();
    let second = open();
    let id = insert(first.as_ref(), "test", "Written by the first");
    let seen = second
        .get(id)
        .unwrap()
        .expect("the second handle sees the first's write");
    assert_eq!(
        seen.payload.get("title"),
        Some(&text("Written by the first"))
    );

    second
        .update(id, vec![FieldMutation::SetStarred(true)])
        .unwrap();
    assert!(
        first.get(id).unwrap().unwrap().is_starred,
        "and the other way round"
    );
    assert_eq!(second.count(&all_of("test")).unwrap(), 1);

    let read = first.logical_clock_of(id).unwrap().unwrap();
    second
        .apply_operation(spec(id, OperationType::SetRead(true)))
        .unwrap();
    let refused = first
        .apply_operation_if_clock(spec(id, OperationType::AddTag("late".into())), read)
        .unwrap();
    assert!(
        matches!(refused, GuardedWrite::Moved { clock: Some(c) } if c > read),
        "{refused:?}"
    );
    assert!(first.get(id).unwrap().unwrap().tags.is_empty());

    // Writers on both handles at once: every row lands.
    std::thread::scope(|scope| {
        for handle in [&first, &second] {
            scope.spawn(move || {
                for i in 0..20 {
                    insert(handle.as_ref(), "burst", &format!("row {i}"));
                }
            });
        }
    });
    assert_eq!(first.count(&all_of("burst")).unwrap(), 40);
    assert_eq!(second.count(&all_of("burst")).unwrap(), 40);
}

/// A store that says it can tell about other processes' writes can also tell
/// about its own, and one that says it is shared says it is durable.
pub fn capabilities_are_coherent(store: &dyn AttributedStore) {
    let caps = store.capabilities();
    assert_eq!(
        store.capabilities(),
        caps,
        "a store's capabilities do not change under it"
    );
    assert!(
        !caps.cross_process_events || caps.subscribe,
        "cross-process events imply subscribe"
    );
    assert!(
        !caps.shared || caps.durable,
        "a shared store outlives a process"
    );
    if caps.subscribe {
        store
            .subscribe(ItemQuery::default())
            .expect("subscribe: true means subscribe works");
    }
}
