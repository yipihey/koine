//! Review AC-F20: when the real store cannot be opened, the store service
//! hands out an in-memory stand-in. A verb that writes must refuse with
//! `store-unavailable` rather than answer `ok` into a store that vanishes.
//!
//! Its own test binary (a second `[[test]]` beside `main`): the store path is
//! process-global, and `contract` sets it to a real directory.

use impress_layout_service::{DefaultLayoutService, LayoutService, PaneRefDto};

/// The store path is process-global and set once; both tests want the same.
fn fallback() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // What a host does when the real store will not open: a private
        // in-memory stand-in, installed as a fallback that names where the
        // real one was meant to be.
        koine_store::process::install_as_fallback_for(
            std::sync::Arc::new(koine_store_memory::MemoryStore::new()),
            "/nonexistent-dir-for-t5/impress.sqlite",
        );
        impress_pane_query::process_manifest::install(std::sync::Arc::new(
            impress_pane_query_compile::test_support::builtin_manifest(),
        ));
    });
}

#[tokio::test]
async fn a_write_into_the_fallback_store_is_refused() {
    fallback();
    let svc = DefaultLayoutService::new();
    let split = svc
        .split(
            "fallback-test".into(),
            Some("fallback-device".into()),
            PaneRefDto::focused(),
            "horizontal".into(),
            true,
            None,
            None,
            None,
        )
        .await;
    assert!(
        !split.ok,
        "a write into the stand-in answered ok: {}",
        split.message
    );
    assert_eq!(
        split.code.as_deref(),
        Some("store-unavailable"),
        "{}",
        split.message
    );
    assert!(
        split
            .message
            .contains("/nonexistent-dir-for-t5/impress.sqlite"),
        "the refusal names the path: {}",
        split.message
    );

    let saved = svc
        .save_layout("fallback-test".into(), None, "x".into(), None, None)
        .await;
    assert_eq!(
        saved.code.as_deref(),
        Some("store-unavailable"),
        "{}",
        saved.message
    );
}

/// Review AC-F20's read half: a read answered from the stand-in says so, in
/// a field and at the front of its message.
#[tokio::test]
async fn a_read_from_the_fallback_store_says_so() {
    fallback();
    let svc = DefaultLayoutService::new();
    let read = svc
        .get_layout("fallback-test".into(), Some("fallback-device".into()))
        .await;
    assert_eq!(read.store.as_deref(), Some("fallback"), "{}", read.message);
    assert!(
        read.message.starts_with("FALLBACK STORE"),
        "{}",
        read.message
    );
    let listed = svc.list_layouts("fallback-test".into()).await;
    assert_eq!(
        listed.store.as_deref(),
        Some("fallback"),
        "{}",
        listed.message
    );
}
