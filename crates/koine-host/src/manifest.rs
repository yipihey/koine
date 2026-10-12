//! The kind manifest a host with no domain installs.
//!
//! A [`KindManifest`] says which schema refs a record kind spans. The layout
//! and surface runtimes read it to compile a pane's query, and they refuse a
//! query over a kind it does not name. A host that has a domain (papers,
//! tasks, readings) builds its own manifest; one that has none still needs
//! the kinds the runtimes themselves store, because a layout pane can show a
//! surface and a query can ask for either.
//!
//! [`minimal`] is that manifest, spelled from `impress_item::schema::refs` so
//! a misspelt ref is a compile error rather than a query that silently
//! matches no rows (the store compares refs by exact equality).

use std::collections::BTreeMap;

use impress_item::schema::refs;
use impress_pane_query::{KindManifest, RecordKindId};

/// The ui kinds and nothing else:
///
/// | kind | ref |
/// |---|---|
/// | `layout` | `impress/ui/layout@1.0.0` |
/// | `preset` | `impress/ui/preset@1.0.0` |
/// | `surface` | `impress/ui/surface@1.0.0` |
/// | `surface-state` | `impress/ui/surface-state@1.0.0` |
/// | `surface-event` | `impress/ui/surface-event@1.0.0` |
/// | `scenario` | `impress/scenario@1.0.0` |
/// | `settings` | `impress/settings@1.0.0` |
///
/// No `contains_members`: nothing here files by a `Contains` edge. Add a
/// domain by inserting its kinds into the returned value before passing it to
/// `Config`.
pub fn minimal() -> KindManifest {
    let kinds: BTreeMap<RecordKindId, Vec<String>> = [
        ("layout", refs::IMPRESS_UI_LAYOUT),
        ("preset", refs::IMPRESS_UI_PRESET),
        ("surface", refs::IMPRESS_UI_SURFACE),
        ("surface-state", refs::IMPRESS_UI_SURFACE_STATE),
        ("surface-event", refs::IMPRESS_UI_SURFACE_EVENT),
        ("scenario", refs::IMPRESS_SCENARIO),
        ("settings", refs::IMPRESS_SETTINGS),
    ]
    .into_iter()
    .map(|(kind, schema)| (kind.to_string(), vec![schema.as_str().to_owned()]))
    .collect();
    KindManifest {
        kinds,
        contains_members: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_seven_ui_kinds_map_to_their_canonical_refs() {
        let manifest = minimal();
        assert_eq!(manifest.kinds.len(), 7);
        assert_eq!(
            manifest.schema_refs("surface"),
            Some(&["impress/ui/surface@1.0.0".to_string()][..])
        );
        assert_eq!(
            manifest.schema_refs("scenario"),
            Some(&["impress/scenario@1.0.0".to_string()][..])
        );
        assert!(manifest.schema_refs("publication").is_none());
    }
}
