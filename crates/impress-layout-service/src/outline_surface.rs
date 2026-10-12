//! The sidebar as a surface (docs/plan-l8-sidebar-surface.md SB-D5).
//!
//! [`outline_surface_spec`] is the chassis' own counterpart of
//! `koine::surface::example_collection_sidebar`: one `tree` (id `outline`)
//! over a `verb` source `rows` — the layout service's `outline-rows` — whose
//! every event is one of the layout service's `outline-*` verbs followed by a
//! `refresh` of `rows`. The app id and the host's bindings are literals in
//! the spec, so a surface made from it needs no params.
//!
//! The spec holds no logic: which rows exist, what a count is, what selecting
//! one does and what renaming, deleting, moving and creating mean are the
//! verbs' (`outline_rows`, `outline_actions`, `service`), and the tree's
//! events carry node keys ([`crate::outline::node_key`]) so a handler passes
//! `{{event.value.id}}` straight through.

use std::collections::BTreeMap;

use impress_surface::SurfaceSpec;
use serde_json::{json, Value};

use crate::outline_rows::section_keys;

/// The verb that answers the sidebar's rows.
pub const ROWS_VERB: &str = "layout-service_outline-rows";
/// The verb a `select` event calls.
pub const SELECT_VERB: &str = "layout-service_outline-select";
/// The verb a `rename` event calls.
pub const RENAME_VERB: &str = "layout-service_outline-rename";
/// The verb a `delete` event calls.
pub const DELETE_VERB: &str = "layout-service_outline-delete";
/// The verb a `move` event calls.
pub const MOVE_VERB: &str = "layout-service_outline-move";
/// The verb the "New Collection" menu entry calls.
pub const CREATE_VERB: &str = "layout-service_outline-create";

/// The sidebar of `app_id` as a surface spec.
///
/// `bindings` are the ids only the host knows (`dismissed_library`,
/// `exploration_library`, as uuid strings); they are written into the
/// `rows` source's arguments and into `outline-select`'s, the two verbs that
/// compile a row's query. `state.open` is seeded with every section's key so
/// the sidebar opens as the legacy one does.
///
/// `rows` reads the verb's answer `{ok, rows, message}` at `rows`
/// (`{{source.rows.rows}}`): the source is named `rows`, and so is the array
/// in its result.
pub fn outline_surface_spec(app_id: &str, bindings: &BTreeMap<String, String>) -> SurfaceSpec {
    // A call, then re-read the rows it may have changed.
    let call = |verb: &str, args: Value| {
        json!([
            { "call": { "verb": verb, "args": args } },
            { "refresh": { "source": "rows" } }
        ])
    };

    let spec = json!({
        "surface": "1.0",
        "name": format!("{app_id} sidebar"),
        "state": { "open": section_keys(app_id) },
        "sources": {
            "rows": {
                "verb": ROWS_VERB,
                "args": { "app_id": app_id, "bindings": bindings }
            }
        },
        "root": {
            "id": "outline",
            "tree": {
                "rows": "{{source.rows.rows}}",
                "id_field": "id",
                "parent_field": "parent",
                "label_field": "label",
                "count_field": "count",
                "bind": "state.open",
                // Selecting is not followed by a refresh: it moves the panes,
                // not the rows. (A count that changed is the source's own
                // declared reads, RS-S2.)
                "on_select": [{ "call": {
                    "verb": SELECT_VERB,
                    "args": {
                        "app_id": app_id,
                        "key": "{{event.value.0}}",
                        "bindings": bindings
                    }
                } }],
                "on_rename": call(RENAME_VERB, json!({
                    "app_id": app_id,
                    "key": "{{event.value.id}}",
                    "name": "{{event.value.label}}"
                })),
                "on_delete": call(DELETE_VERB, json!({
                    "app_id": app_id,
                    "keys": "{{event.value}}"
                })),
                "on_move": call(MOVE_VERB, json!({
                    "app_id": app_id,
                    "keys": "{{event.value.ids}}",
                    "to": "{{event.value.to}}",
                    "index": "{{event.value.index}}"
                })),
                "actions": [
                    {
                        "label": "New Collection",
                        "on_click": call(CREATE_VERB, json!({
                            "app_id": app_id,
                            "parent_key": "{{event.value.id}}"
                        }))
                    },
                    {
                        "label": "Delete",
                        "on_click": call(DELETE_VERB, json!({
                            "app_id": app_id,
                            "keys": ["{{event.value.id}}"]
                        }))
                    }
                ]
            }
        }
    });
    serde_json::from_value(spec)
        .unwrap_or_else(|e| panic!("the outline surface spec no longer parses: {e}"))
}

#[cfg(test)]
mod tests {
    use impress_surface::{
        reduce, resolve, validate, Effect, Event, EventKind, RenderKind, Source,
    };

    use super::*;

    const LIBRARY: &str = "library:3f2b8a10-0000-4000-8000-000000000001";
    const COLLECTION: &str = "collection:3f2b8a10-0000-4000-8000-000000000002";

    fn spec() -> SurfaceSpec {
        outline_surface_spec("imbib", &BTreeMap::new())
    }

    /// The `rows` source's value as the runtime stores a verb's answer: the
    /// whole `{ok, rows, message}` result.
    fn source() -> Value {
        json!({ "rows": {
            "ok": true,
            "message": "3 rows",
            "rows": [
                { "id": "section:inbox", "parent": null, "label": "Inbox",
                  "count": 7, "node": {}, "legacy": false, "kind": "section" },
                { "id": LIBRARY, "parent": null, "label": "Papers",
                  "count": 12, "node": {}, "legacy": false, "kind": "library" },
                { "id": COLLECTION, "parent": LIBRARY, "label": "Dark matter",
                  "count": 4, "node": {}, "legacy": false, "kind": "collection" },
            ]
        }})
    }

    fn open_all() -> Value {
        json!({ "open": ["section:inbox", LIBRARY] })
    }

    fn event(kind: EventKind, value: Value) -> Event {
        Event {
            widget: "outline".into(),
            kind,
            value,
        }
    }

    fn reduced(kind: EventKind, value: Value) -> Vec<Effect> {
        let spec = spec();
        reduce(
            &spec,
            &open_all(),
            &json!({}),
            &source(),
            &event(kind, value),
        )
        .unwrap()
        .1
    }

    fn call(verb: &str, args: Value) -> Effect {
        Effect::Call {
            verb: verb.into(),
            args,
            into: None,
            adverbs: None,
            then: vec![],
        }
    }

    fn refresh() -> Effect {
        Effect::Refresh {
            source: "rows".into(),
        }
    }

    #[test]
    fn it_validates_clean() {
        for app in ["imbib", "impress"] {
            let problems = validate(&outline_surface_spec(app, &BTreeMap::new()));
            assert!(problems.is_empty(), "{app}: {problems:?}");
        }
        let mut bindings = BTreeMap::new();
        bindings.insert("dismissed_library".to_string(), "u".to_string());
        let problems = validate(&outline_surface_spec("imbib", &bindings));
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn the_spec_is_a_tree_over_the_rows_verb() {
        let mut bindings = BTreeMap::new();
        bindings.insert(
            "dismissed_library".to_string(),
            "3f2b8a10-0000-4000-8000-0000000000d1".to_string(),
        );
        let spec = outline_surface_spec("imbib", &bindings);
        let wire = serde_json::to_value(&spec).unwrap();
        assert_eq!(wire["surface"], "1.0");
        assert_eq!(wire["name"], "imbib sidebar");
        assert_eq!(wire["params"], json!([]));
        assert_eq!(wire["state"]["open"], json!(section_keys("imbib")));
        assert!(!section_keys("imbib").is_empty());
        assert_eq!(
            wire["sources"]["rows"],
            json!({
                "verb": "layout-service_outline-rows",
                "args": {
                    "app_id": "imbib",
                    "bindings": { "dismissed_library": "3f2b8a10-0000-4000-8000-0000000000d1" }
                }
            })
        );
        assert_eq!(wire["root"]["id"], "outline");
        let tree = &wire["root"]["tree"];
        assert_eq!(tree["rows"], "{{source.rows.rows}}");
        assert_eq!(tree["id_field"], "id");
        assert_eq!(tree["parent_field"], "parent");
        assert_eq!(tree["label_field"], "label");
        assert_eq!(tree["count_field"], "count");
        assert_eq!(tree["bind"], "state.open");
        assert_eq!(
            tree["on_select"],
            json!([{ "call": {
                "verb": "layout-service_outline-select",
                "args": {
                    "app_id": "imbib",
                    "key": "{{event.value.0}}",
                    "bindings": { "dismissed_library": "3f2b8a10-0000-4000-8000-0000000000d1" }
                }
            } }])
        );
        assert_eq!(
            tree["on_rename"][0]["call"],
            json!({
                "verb": "layout-service_outline-rename",
                "args": {
                    "app_id": "imbib",
                    "key": "{{event.value.id}}",
                    "name": "{{event.value.label}}"
                }
            })
        );
        assert_eq!(
            tree["on_delete"][0]["call"]["args"],
            json!({ "app_id": "imbib", "keys": "{{event.value}}" })
        );
        assert_eq!(
            tree["on_move"][0]["call"]["args"],
            json!({
                "app_id": "imbib",
                "keys": "{{event.value.ids}}",
                "to": "{{event.value.to}}",
                "index": "{{event.value.index}}"
            })
        );
        for handler in ["on_rename", "on_delete", "on_move"] {
            assert_eq!(
                tree[handler][1],
                json!({ "refresh": { "source": "rows" } }),
                "{handler}"
            );
        }
        let labels: Vec<&Value> = tree["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| &a["label"])
            .collect();
        assert_eq!(labels, vec!["New Collection", "Delete"]);
        assert_eq!(
            tree["actions"][0]["on_click"][0]["call"]["args"],
            json!({ "app_id": "imbib", "parent_key": "{{event.value.id}}" })
        );
        assert_eq!(
            tree["actions"][1]["on_click"][0]["call"]["args"],
            json!({ "app_id": "imbib", "keys": ["{{event.value.id}}"] })
        );
        // And it reads back as the same spec.
        assert_eq!(serde_json::from_value::<SurfaceSpec>(wire).unwrap(), spec);
    }

    #[test]
    fn the_source_is_the_rows_verb() {
        match &spec().sources["rows"] {
            Source::Verb { verb, args } => {
                assert_eq!(verb, ROWS_VERB);
                assert_eq!(args["app_id"], "imbib");
                assert_eq!(args["bindings"], json!({}));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_rows_of_the_verb_result_resolve_to_nested_tree_nodes() {
        let spec = spec();
        let tree = resolve(&spec, &open_all(), &json!({}), &source()).root.node;
        match tree {
            RenderKind::Tree {
                nodes,
                expanded,
                bind,
                actions,
                ..
            } => {
                let shown: Vec<(&str, u32, bool, bool, Option<Value>)> = nodes
                    .iter()
                    .map(|n| {
                        (
                            n.label.as_str(),
                            n.depth,
                            n.has_children,
                            n.expanded,
                            n.count.clone(),
                        )
                    })
                    .collect();
                assert_eq!(
                    shown,
                    vec![
                        ("Inbox", 0, false, false, Some(json!(7))),
                        ("Papers", 0, true, true, Some(json!(12))),
                        ("Dark matter", 1, false, false, Some(json!(4))),
                    ]
                );
                assert_eq!(nodes[2].id, json!(COLLECTION));
                assert_eq!(expanded, vec![json!("section:inbox"), json!(LIBRARY)]);
                assert_eq!(bind.as_deref(), Some("state.open"));
                assert_eq!(actions, vec!["New Collection", "Delete"]);
            }
            other => panic!("not a tree: {other:?}"),
        }
        // Closed, the collection is not drawn.
        let closed = resolve(&spec, &json!({ "open": [] }), &json!({}), &source())
            .root
            .node;
        match closed {
            RenderKind::Tree { nodes, .. } => {
                let labels: Vec<&str> = nodes.iter().map(|n| n.label.as_str()).collect();
                assert_eq!(labels, vec!["Inbox", "Papers"]);
                assert!(nodes[1].has_children && !nodes[1].expanded);
            }
            other => panic!("not a tree: {other:?}"),
        }
    }

    /// The envelope rule reads the same result when the template names the
    /// whole answer, so either spelling of `rows` would have resolved; the
    /// spec uses the explicit one and a refused answer (no `rows` array)
    /// is a placeholder, not an empty sidebar that looks like "no data".
    #[test]
    fn a_refused_rows_answer_does_not_draw_a_sidebar() {
        let spec = spec();
        let refused = json!({ "rows": { "ok": false, "code": "invalid-argument",
                                          "message": "unknown app" } });
        let tree = resolve(&spec, &open_all(), &json!({}), &refused).root.node;
        assert!(
            !matches!(&tree, RenderKind::Tree { nodes, .. } if !nodes.is_empty()),
            "{tree:?}"
        );
    }

    #[test]
    fn a_select_calls_outline_select_with_the_key() {
        let effects = reduced(EventKind::Select, json!([COLLECTION]));
        assert_eq!(
            effects,
            vec![call(
                "layout-service_outline-select",
                json!({ "app_id": "imbib", "key": COLLECTION, "bindings": {} })
            )]
        );
    }

    #[test]
    fn a_rename_calls_outline_rename_then_refreshes() {
        let effects = reduced(
            EventKind::Rename,
            json!({ "id": COLLECTION, "label": "Dark sector" }),
        );
        assert_eq!(
            effects,
            vec![
                call(
                    "layout-service_outline-rename",
                    json!({ "app_id": "imbib", "key": COLLECTION, "name": "Dark sector" })
                ),
                refresh()
            ]
        );
    }

    #[test]
    fn the_delete_menu_entry_calls_outline_delete_with_the_row() {
        let effects = reduced(
            EventKind::Menu,
            json!({ "id": COLLECTION, "action": "Delete" }),
        );
        assert_eq!(
            effects,
            vec![
                call(
                    "layout-service_outline-delete",
                    json!({ "app_id": "imbib", "keys": [COLLECTION] })
                ),
                refresh()
            ]
        );
    }

    #[test]
    fn a_delete_event_passes_the_whole_key_list() {
        let effects = reduced(EventKind::Delete, json!([COLLECTION, "collection:x"]));
        assert_eq!(
            effects,
            vec![
                call(
                    "layout-service_outline-delete",
                    json!({ "app_id": "imbib", "keys": [COLLECTION, "collection:x"] })
                ),
                refresh()
            ]
        );
    }

    #[test]
    fn the_new_collection_menu_entry_creates_under_the_row() {
        let effects = reduced(
            EventKind::Menu,
            json!({ "id": LIBRARY, "action": "New Collection" }),
        );
        assert_eq!(
            effects,
            vec![
                call(
                    "layout-service_outline-create",
                    json!({ "app_id": "imbib", "parent_key": LIBRARY })
                ),
                refresh()
            ]
        );
    }

    #[test]
    fn a_move_forwards_the_keys_the_target_and_the_position() {
        let effects = reduced(
            EventKind::Move,
            json!({ "ids": [COLLECTION], "to": LIBRARY, "index": 2 }),
        );
        assert_eq!(
            effects,
            vec![
                call(
                    "layout-service_outline-move",
                    json!({ "app_id": "imbib", "keys": [COLLECTION], "to": LIBRARY, "index": 2 })
                ),
                refresh()
            ]
        );
        // A drop onto no row is a move to the root: `to` is null.
        let effects = reduced(
            EventKind::Move,
            json!({ "ids": [COLLECTION], "to": null, "index": 0 }),
        );
        match &effects[0] {
            Effect::Call { args, .. } => assert_eq!(args["to"], Value::Null),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn opening_a_branch_writes_state_open() {
        let spec = spec();
        let (state, effects) = reduce(
            &spec,
            &open_all(),
            &json!({}),
            &source(),
            &event(EventKind::Change, json!(["section:inbox"])),
        )
        .unwrap();
        assert_eq!(state["open"], json!(["section:inbox"]));
        assert!(effects.is_empty());
    }

    #[test]
    fn the_bindings_reach_the_select_verb_too() {
        let mut bindings = BTreeMap::new();
        bindings.insert("exploration_library".to_string(), "u-1".to_string());
        let spec = outline_surface_spec("imbib", &bindings);
        let (_, effects) = reduce(
            &spec,
            &open_all(),
            &json!({}),
            &source(),
            &event(EventKind::Select, json!(["section:inbox"])),
        )
        .unwrap();
        match &effects[0] {
            Effect::Call { args, .. } => {
                assert_eq!(args["bindings"], json!({ "exploration_library": "u-1" }));
                assert_eq!(args["key"], "section:inbox");
            }
            other => panic!("{other:?}"),
        }
    }
}
