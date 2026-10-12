//! Wave 7 T5: every refusal carries a machine-readable `code` next to its
//! prose, and a refused verb says which verb and which target (review RL-L11,
//! AC-F19, RL-L21).

use impress_pane_query_compile::test_support::builtin_manifest;
use std::sync::Arc;

use impress_layout_service::{DefaultLayoutService, LayoutService, PaneRefDto};

const APP: &str = "honest-test";

fn device() -> Option<String> {
    Some("honest-device".to_string())
}

fn service() -> DefaultLayoutService {
    DefaultLayoutService::with_store(
        Arc::new(koine_store_memory::MemoryStore::new()),
        builtin_manifest(),
    )
}

#[tokio::test]
async fn a_tree_refusal_carries_its_layout_error_tag_and_names_the_verb() {
    let svc = service();
    let closed = svc
        .close(
            APP.into(),
            device(),
            PaneRefDto {
                id: Some(999),
                ..Default::default()
            },
            None,
            None,
        )
        .await;
    assert!(!closed.ok);
    assert_eq!(
        closed.code.as_deref(),
        Some("unknown-tile"),
        "{}",
        closed.message
    );
    assert!(
        closed.message.starts_with("close "),
        "the message names the verb: {}",
        closed.message
    );

    let json = serde_json::to_value(&closed).unwrap();
    assert_eq!(json["code"], "unknown-tile", "the wire carries the code");
}

#[tokio::test]
async fn a_success_has_no_code_on_the_wire() {
    let svc = service();
    let layout = svc.get_layout(APP.into(), device()).await;
    assert!(layout.ok, "{}", layout.message);
    let json = serde_json::to_value(&layout).unwrap();
    assert!(json.get("code").is_none(), "{json}");
}

#[tokio::test]
async fn a_channel_out_of_range_is_refused_not_clamped() {
    let svc = service();
    let set = svc
        .set_channel(
            APP.into(),
            device(),
            PaneRefDto::focused(),
            "12".into(),
            None,
            None,
        )
        .await;
    assert!(
        !set.ok,
        "channel 12 used to land on 8 with ok: {}",
        set.message
    );
    assert_eq!(set.code.as_deref(), Some("invalid-argument"));

    let eight = svc
        .set_channel(
            APP.into(),
            device(),
            PaneRefDto::focused(),
            "8".into(),
            None,
            None,
        )
        .await;
    assert!(eight.ok, "{}", eight.message);
}

#[tokio::test]
async fn deleting_a_name_that_is_not_there_is_not_found() {
    let svc = service();
    let deleted = svc
        .delete_layout(APP.into(), "no such layout".into(), None)
        .await;
    assert!(!deleted.ok);
    assert_eq!(
        deleted.code.as_deref(),
        Some("not-found"),
        "{}",
        deleted.message
    );
}

#[tokio::test]
async fn an_argument_that_does_not_parse_is_invalid_argument() {
    let svc = service();
    let split = svc
        .split(
            APP.into(),
            device(),
            PaneRefDto::focused(),
            "diagonal".into(),
            true,
            None,
            None,
            None,
        )
        .await;
    assert!(!split.ok);
    assert_eq!(
        split.code.as_deref(),
        Some("invalid-argument"),
        "{}",
        split.message
    );
}
