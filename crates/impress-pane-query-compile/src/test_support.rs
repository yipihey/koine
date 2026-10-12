//! A manifest shaped like impress's `builtin_manifest()`, for the tests.
//!
//! The real one stays in `impress-core` (it is domain data); this is the same
//! table built from the same `schema-refs.json` constants, so the compiler's
//! tests exercise the kinds, multi-ref kinds and `Contains` widening the
//! chassis ships.

use std::collections::BTreeMap;

use impress_item::schema::refs;
use impress_pane_query::{KindManifest, RecordKindId};

pub fn builtin_manifest() -> KindManifest {
    let kinds: BTreeMap<RecordKindId, Vec<String>> = [
        ("publication", vec![refs::IMBIB_BIBLIOGRAPHY_ENTRY]),
        ("manuscript", vec![refs::MANUSCRIPT]),
        ("figure", vec![refs::FIGURE]),
        ("message", vec![refs::EMAIL_MESSAGE, refs::CHAT_MESSAGE]),
        ("task", vec![refs::TASK]),
        ("agent-run", vec![refs::AGENT_RUN]),
        (
            "artifact",
            vec![
                refs::IMPRESS_ARTIFACT_CODE,
                refs::IMPRESS_ARTIFACT_DATASET,
                refs::IMPRESS_ARTIFACT_GENERAL,
                refs::IMPRESS_ARTIFACT_MEDIA,
                refs::IMPRESS_ARTIFACT_NOTE,
                refs::IMPRESS_ARTIFACT_POSTER,
                refs::IMPRESS_ARTIFACT_PRESENTATION,
                refs::IMPRESS_ARTIFACT_WEBPAGE,
            ],
        ),
        (
            "collection",
            vec![
                refs::COLLECTION,
                refs::IMBIB_COLLECTION,
                refs::MANUSCRIPT_COLLECTION,
                refs::FIGURE_COLLECTION,
            ],
        ),
        ("library", vec![refs::IMBIB_LIBRARY]),
        ("surface", vec![refs::IMPRESS_UI_SURFACE]),
    ]
    .into_iter()
    .map(|(kind, refs)| {
        (
            kind.to_string(),
            refs.into_iter().map(|r| r.as_str().to_owned()).collect(),
        )
    })
    .collect();
    let contains_members = [("library", vec!["publication"])]
        .into_iter()
        .map(|(kind, members)| {
            (
                kind.to_string(),
                members.into_iter().map(str::to_string).collect(),
            )
        })
        .collect();
    KindManifest {
        kinds,
        contains_members,
    }
}
