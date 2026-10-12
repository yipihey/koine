//! The surface grammar under the name koine, plus the worked examples that
//! are built rather than stored.
//!
//! Everything `impress-surface` exports is re-exported here unchanged
//! (`koine::surface::resolve`, `koine::surface::SurfaceSpec`, …).
//! [`example_collection_sidebar`] is the one thing this module adds: the
//! sidebar of `docs/plan-koine-outline.md` O-D7, a spec builder that takes
//! the verb names an app wants wired. The guide is `docs/outline.md`.

pub use impress_surface::*;

use serde_json::{json, Map, Value};

/// The verbs a collection sidebar calls, each a verb name (`imbib-service_…`)
/// or `None` to leave that cell of the matrix read-only. They are names only:
/// whether a name is linked is the host's `surface_validate`, not
/// [`validate`], so a sidebar builds the same with none of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SidebarVerbs {
    /// Called on `rename`: `{"id": <row id>, "name": <new text>}`.
    pub rename: Option<String>,
    /// Called on `delete` and by the "Delete" menu entry: `{"ids": [<id>…]}`.
    pub delete: Option<String>,
    /// Called on `move` (a drag, or `x`/`p` in the TUI):
    /// `{"ids": […], "parent": <id or null>, "index": <position>}`.
    pub reparent: Option<String>,
    /// Called by the "New collection" menu entry: `{"parent": <row id>}`.
    pub create: Option<String>,
}

/// The collection sidebar (plan-koine-outline O-D7): a `tree` with id
/// `outline` over a `query` source `collections` for `kind`, sorted by
/// `name`, nested by each record's envelope `parent`, showing its `name`
/// and its `count`. The open branches live in `state.open`; a select
/// publishes the row so the panes beside the sidebar follow it.
///
/// Each verb in `verbs` wires one cell of the chassis capability matrix
/// (`on_rename`, `on_delete`, `on_move`, a "Delete" and a "New collection"
/// menu entry); a verb left `None` leaves its handler out, and a tree
/// without a handler ignores the event. Every call is followed by a refresh
/// of the `collections` source, so the tree shows what the verb did.
pub fn example_collection_sidebar(kind: &str, verbs: &SidebarVerbs) -> SurfaceSpec {
    // A call, then re-read the source it changed.
    let call = |verb: &str, args: Value| {
        json!([
            { "call": { "verb": verb, "args": args } },
            { "refresh": { "source": "collections" } }
        ])
    };

    let mut tree = Map::new();
    tree.insert("rows".into(), json!("{{source.collections}}"));
    tree.insert("parent_field".into(), json!("parent"));
    tree.insert("label_field".into(), json!("name"));
    tree.insert("count_field".into(), json!("count"));
    tree.insert("bind".into(), json!("state.open"));
    tree.insert("on_select".into(), json!([{ "publish": {} }]));

    if let Some(verb) = &verbs.rename {
        tree.insert(
            "on_rename".into(),
            call(
                verb,
                json!({ "id": "{{event.value.id}}", "name": "{{event.value.label}}" }),
            ),
        );
    }
    if let Some(verb) = &verbs.delete {
        tree.insert(
            "on_delete".into(),
            call(verb, json!({ "ids": "{{event.value}}" })),
        );
    }
    if let Some(verb) = &verbs.reparent {
        tree.insert(
            "on_move".into(),
            call(
                verb,
                json!({
                    "ids": "{{event.value.ids}}",
                    "parent": "{{event.value.to}}",
                    "index": "{{event.value.index}}"
                }),
            ),
        );
    }

    // Rename is not a menu entry: it needs the person's text, which is the
    // renderer's inline edit (`r`), so it arrives as a `rename` event.
    let mut actions = Vec::new();
    if let Some(verb) = &verbs.create {
        actions.push(json!({
            "label": "New collection",
            "on_click": call(verb, json!({ "parent": "{{event.value.id}}" }))
        }));
    }
    if let Some(verb) = &verbs.delete {
        actions.push(json!({
            "label": "Delete",
            "on_click": call(verb, json!({ "ids": ["{{event.value.id}}"] }))
        }));
    }
    if !actions.is_empty() {
        tree.insert("actions".into(), Value::Array(actions));
    }

    let spec = json!({
        "surface": "1.0",
        "name": "Collections",
        "state": { "open": [] },
        "sources": {
            "collections": {
                "query": {
                    "kinds": [kind],
                    "scope": { "scope": "all" },
                    "filters": [],
                    "text": null,
                    "relation": null,
                    "sort": [{ "field": "name", "descending": false }],
                    "limit": null
                }
            }
        },
        "root": { "id": "outline", "tree": Value::Object(tree) }
    });
    serde_json::from_value(spec)
        .unwrap_or_else(|e| panic!("the collection sidebar no longer parses: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verbs() -> SidebarVerbs {
        SidebarVerbs {
            rename: Some("imbib-service_rename-collection".into()),
            delete: Some("imbib-service_delete-collection".into()),
            reparent: Some("imbib-service_reparent-collection".into()),
            create: Some("imbib-service_create-collection".into()),
        }
    }

    /// Three collections as the runtime hands them over: the payload lifted
    /// to the top of the row, the envelope `parent` beside it. `Papers`
    /// holds `Dark matter`; `Notes` is a second root.
    fn rows() -> Value {
        json!({ "collections": [
            { "id": "c1", "name": "Papers", "count": 12, "parent": null },
            { "id": "c2", "name": "Dark matter", "count": 4, "parent": "c1" },
            { "id": "c3", "name": "Notes", "parent": null },
        ]})
    }

    fn event(kind: EventKind, value: Value) -> Event {
        Event {
            widget: "outline".into(),
            kind,
            value,
        }
    }

    fn tree_of(spec: &SurfaceSpec, state: &Value) -> RenderKind {
        resolve(spec, state, &json!({}), &rows()).root.node
    }

    #[test]
    fn it_validates_clean_with_every_verb_and_with_none() {
        for verbs in [verbs(), SidebarVerbs::default()] {
            let spec = example_collection_sidebar("collection", &verbs);
            let problems = validate(&spec);
            assert!(problems.is_empty(), "{problems:?}");
        }
    }

    #[test]
    fn the_spec_is_a_tree_over_a_query_source() {
        let spec = example_collection_sidebar("collection", &verbs());
        let wire = serde_json::to_value(&spec).unwrap();
        let query = &wire["sources"]["collections"]["query"];
        assert_eq!(query["kinds"], json!(["collection"]));
        assert_eq!(
            query["sort"],
            json!([{ "field": "name", "descending": false }])
        );
        assert_eq!(query["limit"], Value::Null);
        let tree = &wire["root"]["tree"];
        assert_eq!(wire["root"]["id"], "outline");
        assert_eq!(tree["rows"], "{{source.collections}}");
        assert_eq!(tree["parent_field"], "parent");
        assert_eq!(tree["label_field"], "name");
        assert_eq!(tree["count_field"], "count");
        assert_eq!(tree["bind"], "state.open");
        assert_eq!(tree["on_select"], json!([{ "publish": {} }]));
        assert_eq!(
            tree["on_rename"][0]["call"],
            json!({
                "verb": "imbib-service_rename-collection",
                "args": { "id": "{{event.value.id}}", "name": "{{event.value.label}}" }
            })
        );
        assert_eq!(
            tree["on_delete"][0]["call"]["args"],
            json!({ "ids": "{{event.value}}" })
        );
        assert_eq!(
            tree["on_move"][0]["call"]["args"],
            json!({
                "ids": "{{event.value.ids}}",
                "parent": "{{event.value.to}}",
                "index": "{{event.value.index}}"
            })
        );
        let labels: Vec<&Value> = tree["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| &a["label"])
            .collect();
        assert_eq!(labels, vec!["New collection", "Delete"]);
        assert_eq!(wire["state"], json!({ "open": [] }));
        assert_eq!(wire["params"], json!([]));
        // And it reads back as the same spec.
        assert_eq!(serde_json::from_value::<SurfaceSpec>(wire).unwrap(), spec);
    }

    #[test]
    fn a_verb_left_out_leaves_its_handler_out() {
        let spec = example_collection_sidebar("collection", &SidebarVerbs::default());
        let wire = serde_json::to_value(&spec).unwrap();
        let tree = wire["root"]["tree"].as_object().unwrap();
        for key in ["on_rename", "on_delete", "on_move", "actions"] {
            assert!(!tree.contains_key(key), "{key}");
        }
        // Only a create verb: a menu of one, nothing else wired.
        let only_create = SidebarVerbs {
            create: Some("c".into()),
            ..SidebarVerbs::default()
        };
        let wire =
            serde_json::to_value(example_collection_sidebar("collection", &only_create)).unwrap();
        assert_eq!(wire["root"]["tree"]["actions"].as_array().unwrap().len(), 1);
        assert!(wire["root"]["tree"].get("on_delete").is_none());
    }

    #[test]
    fn it_names_the_kind_it_was_given() {
        let spec = example_collection_sidebar("section", &verbs());
        match &spec.sources["collections"] {
            Source::Query { query } => assert_eq!(query.kinds, vec!["section".to_string()]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn it_resolves_to_a_nested_tree_with_counts_and_the_open_branches() {
        let spec = example_collection_sidebar("collection", &verbs());
        // Closed: two roots.
        match tree_of(&spec, &spec.state) {
            RenderKind::Tree {
                nodes,
                expanded,
                actions,
                bind,
            } => {
                let shown: Vec<(&str, u32, bool, bool)> = nodes
                    .iter()
                    .map(|n| (n.label.as_str(), n.depth, n.has_children, n.expanded))
                    .collect();
                assert_eq!(
                    shown,
                    vec![("Papers", 0, true, false), ("Notes", 0, false, false)]
                );
                assert!(expanded.is_empty());
                assert_eq!(bind.as_deref(), Some("state.open"));
                assert_eq!(actions, vec!["New collection", "Delete"]);
            }
            other => panic!("{other:?}"),
        }
        // `Papers` open: its child is drawn one level down, counts follow.
        let open = json!({ "open": ["c1"] });
        match tree_of(&spec, &open) {
            RenderKind::Tree {
                nodes, expanded, ..
            } => {
                let shown: Vec<(&str, u32, bool, Option<Value>)> = nodes
                    .iter()
                    .map(|n| (n.label.as_str(), n.depth, n.expanded, n.count.clone()))
                    .collect();
                assert_eq!(
                    shown,
                    vec![
                        ("Papers", 0, true, Some(json!(12))),
                        ("Dark matter", 1, false, Some(json!(4))),
                        ("Notes", 0, false, None),
                    ]
                );
                assert_eq!(expanded, vec![json!("c1")]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_select_publishes_the_row() {
        let spec = example_collection_sidebar("collection", &verbs());
        let (_, effects) = reduce(
            &spec,
            &spec.state,
            &json!({}),
            &rows(),
            &event(EventKind::Select, json!(["c2"])),
        )
        .unwrap();
        assert_eq!(effects, vec![Effect::Publish { ids: json!(["c2"]) }]);
    }

    fn reduced(kind: EventKind, value: Value) -> Vec<Effect> {
        let spec = example_collection_sidebar("collection", &verbs());
        reduce(&spec, &spec.state, &json!({}), &rows(), &event(kind, value))
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
            source: "collections".into(),
        }
    }

    #[test]
    fn the_delete_menu_entry_calls_the_delete_verb_with_the_row() {
        let effects = reduced(EventKind::Menu, json!({ "id": "c2", "action": "Delete" }));
        assert_eq!(
            effects,
            vec![
                call("imbib-service_delete-collection", json!({ "ids": ["c2"] })),
                refresh()
            ]
        );
    }

    #[test]
    fn the_new_collection_menu_entry_creates_under_the_row() {
        let effects = reduced(
            EventKind::Menu,
            json!({ "id": "c1", "action": "New collection" }),
        );
        assert_eq!(
            effects,
            vec![
                call("imbib-service_create-collection", json!({ "parent": "c1" })),
                refresh()
            ]
        );
    }

    #[test]
    fn a_rename_calls_the_rename_verb_with_the_new_name() {
        let effects = reduced(
            EventKind::Rename,
            json!({ "id": "c2", "label": "Dark sector" }),
        );
        assert_eq!(
            effects,
            vec![
                call(
                    "imbib-service_rename-collection",
                    json!({ "id": "c2", "name": "Dark sector" })
                ),
                refresh()
            ]
        );
    }

    #[test]
    fn a_delete_event_passes_the_whole_id_list() {
        let effects = reduced(EventKind::Delete, json!(["c2", "c3"]));
        assert_eq!(
            effects,
            vec![
                call(
                    "imbib-service_delete-collection",
                    json!({ "ids": ["c2", "c3"] })
                ),
                refresh()
            ]
        );
    }

    #[test]
    fn a_move_reparents_and_a_move_to_the_root_sends_null() {
        let effects = reduced(
            EventKind::Move,
            json!({ "ids": ["c3"], "to": "c1", "index": 1 }),
        );
        assert_eq!(
            effects,
            vec![
                call(
                    "imbib-service_reparent-collection",
                    json!({ "ids": ["c3"], "parent": "c1", "index": 1 })
                ),
                refresh()
            ]
        );
        let effects = reduced(
            EventKind::Move,
            json!({ "ids": ["c2"], "to": null, "index": 0 }),
        );
        assert_eq!(
            effects,
            vec![
                call(
                    "imbib-service_reparent-collection",
                    json!({ "ids": ["c2"], "parent": null, "index": 0 })
                ),
                refresh()
            ]
        );
    }

    #[test]
    fn a_sidebar_without_verbs_ignores_every_row_event() {
        let spec = example_collection_sidebar("collection", &SidebarVerbs::default());
        for (kind, value) in [
            (EventKind::Rename, json!({ "id": "c1", "label": "x" })),
            (EventKind::Delete, json!(["c1"])),
            (
                EventKind::Move,
                json!({ "ids": ["c1"], "to": null, "index": 0 }),
            ),
        ] {
            let (state, effects) =
                reduce(&spec, &spec.state, &json!({}), &rows(), &event(kind, value)).unwrap();
            assert_eq!(state, spec.state);
            assert!(effects.is_empty(), "{kind:?}: {effects:?}");
        }
    }

    /// Opening a branch is a `change` with the whole list, written to the bind.
    #[test]
    fn opening_a_branch_writes_state_open() {
        let spec = example_collection_sidebar("collection", &verbs());
        let (state, effects) = reduce(
            &spec,
            &spec.state,
            &json!({}),
            &rows(),
            &event(EventKind::Change, json!(["c1"])),
        )
        .unwrap();
        assert_eq!(state["open"], json!(["c1"]));
        assert!(effects.is_empty());
    }
}
