//! The wire format is pinned.
//!
//! A layout is the payload of an `impress/ui/layout` item (ADR-0019 D1), so
//! its JSON is a compatibility surface: it is written by one device and read
//! by another, possibly running an older build. This test fails on any change
//! to the serialized shape — which is the point. A deliberate change is made
//! by re-blessing the file (`IMPRESS_LAYOUT_BLESS=1 cargo test -p
//! impress-layout --test golden`) *and* saying in the PR what reads the old
//! shape.

use crate::common::publication_query;
use impress_layout::{preset, Layout, ViewKindId};

const GOLDEN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/golden/three_column.json"
);

fn golden_layout() -> Layout {
    preset::three_column(publication_query(), ViewKindId::INFO)
}

#[test]
fn the_three_column_preset_serializes_exactly_as_committed() {
    let layout = golden_layout();
    let actual = serde_json::to_value(&layout).unwrap();

    if std::env::var("IMPRESS_LAYOUT_BLESS").is_ok() {
        let pretty = serde_json::to_string_pretty(&actual).unwrap();
        std::fs::write(GOLDEN, format!("{pretty}\n")).unwrap();
    }

    let committed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(GOLDEN).expect("golden file")).unwrap();
    assert_eq!(
        actual,
        committed,
        "the layout wire format changed.\nactual:\n{}",
        serde_json::to_string_pretty(&actual).unwrap()
    );
}

#[test]
fn the_committed_json_deserializes_back_to_the_preset() {
    let committed: Layout =
        serde_json::from_str(&std::fs::read_to_string(GOLDEN).expect("golden file")).unwrap();
    assert_eq!(committed, golden_layout());
}

#[test]
fn tile_ids_survive_being_json_object_keys() {
    let layout = golden_layout();
    let json = serde_json::to_string(&layout).unwrap();
    let back: Layout = serde_json::from_str(&json).unwrap();
    assert_eq!(
        back.tiles.keys().copied().collect::<Vec<_>>(),
        layout.tiles.keys().copied().collect::<Vec<_>>()
    );
    assert_eq!(back, layout);
}
