use serde_json::{json, Value};

use super::*;
use crate::api::LayoutTree;
use koine::layout::{PaneRef, Placement};

fn pane(view_kind: &str, role: &str) -> Value {
    json!({"pane": {"view_kind": view_kind, "role": role, "query": {"kinds": ["publication"]}}})
}

fn layout(tiles: Value, windows: Value) -> Layout {
    serde_json::from_value(json!({"windows": windows, "tiles": tiles})).unwrap()
}

fn tree(layout: Layout, focused: Option<u64>) -> LayoutTree {
    LayoutTree {
        version: 1,
        revision: Some(1),
        focused,
        layout,
        app: None,
    }
}

/// One `Linear` of two panes: tile 3 holds tiles 1 and 2.
fn two_panes() -> Layout {
    layout(
        json!({
            "1": pane("list", "list"),
            "2": pane("info", "detail"),
            "3": {"container": {"linear": {"dir": "horizontal", "children": [1, 2], "shares": [1.0, 3.0]}}},
        }),
        json!([{"id": 1, "root": 3, "focused": 1}]),
    )
}

/// A tab strip over two panes (tiles 1 and 2), the second shown.
fn tabbed() -> Layout {
    layout(
        json!({
            "1": pane("info", "detail"),
            "2": pane("notes", "detail"),
            "3": {"container": {"tabs": {"children": [1, 2], "active": 2}}},
        }),
        json!([{"id": 1, "root": 3, "focused": 2}]),
    )
}

/// The page, as HTML, over a tree held in a board with no network.
fn render(tree: LayoutTree) -> String {
    #[derive(Clone, PartialEq, Props)]
    struct Root {
        tree: LayoutTree,
    }

    #[component]
    fn Page(props: Root) -> Element {
        let session = use_context_provider(Session::new);
        let tree = use_signal(|| Some(props.tree.clone()));
        let resize = use_signal(|| None);
        use_context_provider(move || Board {
            tree,
            resize,
            session,
        });
        rsx! { LayoutBody {} }
    }

    let mut dom = VirtualDom::new_with_props(Page, Root { tree });
    dom.rebuild_in_place();
    dioxus_ssr::render(&dom)
}

fn count(html: &str, needle: &str) -> usize {
    html.matches(needle).count()
}

#[test]
fn a_linear_of_two_panes_is_two_framed_panes_and_one_divider() {
    let html = render(tree(two_panes(), Some(2)));
    assert_eq!(count(&html, "data-koine-tile=\""), 2, "{html}");
    assert!(html.contains("data-koine-tile=\"1\""));
    assert!(html.contains("data-koine-tile=\"2\""));
    // The divider is in the markup (CSS draws it for a fine pointer only).
    assert_eq!(count(&html, "data-koine-divider=\""), 1, "{html}");
    assert!(html.contains("data-koine-divider=\"0\""));
    // No handle and no edge targets: moving a pane is the outline's.
    assert!(!html.contains("data-koine-grip"), "{html}");
    assert!(!html.contains("data-koine-edge"), "{html}");
    // Shares are flex-grow.
    assert!(html.contains("flex-grow: 1;"));
    assert!(html.contains("flex-grow: 3;"));
    // The role and view kind are the title.
    assert!(html.contains("list · list"));
    assert!(html.contains("detail · info"));
}

#[test]
fn only_the_focused_tile_is_marked_focused() {
    let html = render(tree(two_panes(), Some(2)));
    assert_eq!(count(&html, "data-koine-focused=\"true\""), 1, "{html}");
    let focused = html.find("data-koine-focused=\"true\"").unwrap();
    let two = html.find("data-koine-tile=\"2\"").unwrap();
    let one = html.find("data-koine-tile=\"1\"").unwrap();
    assert!(one < focused && two < focused + 80, "{html}");
    assert!(html.contains("koine-pane koine-focused"));
}

#[test]
fn the_golden_three_column_layout_draws_three_panes_two_dividers_and_placeholders() {
    let golden: Value = serde_json::from_str(include_str!(
        "../../../impress-layout/tests/golden/three_column.json"
    ))
    .unwrap();
    let layout: Layout = serde_json::from_value(golden).unwrap();
    assert!(!drawable(&layout), "three placeholders");
    let html = render(tree(layout, Some(2)));
    assert_eq!(count(&html, "data-koine-tile=\""), 3, "{html}");
    assert_eq!(count(&html, "data-koine-divider=\""), 2);
    for share in ["flex-grow: 1;", "flex-grow: 2;", "flex-grow: 3;"] {
        assert!(html.contains(share), "{share}");
    }
    assert_eq!(count(&html, "View Kind Unavailable"), 3);
    // Every tile is a placeholder: the page says there is nothing to lay out.
    assert_eq!(count(&html, NOTHING_TO_LAY_OUT), 1);
    assert!(html.contains("query kinds: collection, library"));
    assert!(html.contains("role: navigator"));
    assert!(html.contains("tile 3"));
}

#[test]
fn a_tab_strip_shows_only_the_active_child_with_one_button_per_child() {
    let html = render(tree(tabbed(), Some(2)));
    assert_eq!(count(&html, "data-koine-tile=\""), 1, "{html}");
    assert!(html.contains("data-koine-tile=\"2\""));
    assert!(!html.contains("data-koine-tile=\"1\""));
    assert_eq!(count(&html, "data-koine-tab=\""), 2);
    assert!(html.contains("data-koine-tab=\"0\""));
    assert!(html.contains("data-koine-tab=\"1\""));
    // The strip is on the shown pane's top edge, before its title.
    let tab = html.find("data-koine-tab=\"0\"").unwrap();
    let title = html.find("koine-title").unwrap();
    assert!(tab < title);
    // Exactly the active tab is selected.
    assert_eq!(count(&html, "aria-selected=\"true\""), 1);
    assert_eq!(count(&html, "aria-selected=\"false\""), 1);
}

#[test]
fn a_grid_has_its_columns() {
    let grid = layout(
        json!({
            "1": pane("list", "list"),
            "2": pane("info", "detail"),
            "3": pane("notes", "detail"),
            "4": {"container": {"grid": {"children": [1, 2, 3], "columns": 2}}},
        }),
        json!([{"id": 1, "root": 4, "focused": 1}]),
    );
    let html = render(tree(grid, None));
    assert!(html.contains("repeat(2, minmax(0, 1fr))"), "{html}");
    assert_eq!(count(&html, "data-koine-tile=\""), 3);
    assert_eq!(count(&html, "data-koine-focused=\"true\""), 0);
}

#[test]
fn every_window_is_drawn_the_current_one_first() {
    let windows = layout(
        json!({"1": pane("list", "list"), "2": pane("info", "detail")}),
        json!([{"id": 1, "root": 1, "focused": 1}, {"id": 2, "root": 2, "focused": 2}]),
    );
    let mut windows = windows;
    windows.current = Some(WindowId::new(2));
    let order: Vec<u64> = windows_in_order(&windows)
        .iter()
        .map(|window| window.id.raw())
        .collect();
    assert_eq!(order, vec![2, 1]);
    let html = render(tree(windows, Some(2)));
    let first = html.find("window 2").unwrap();
    let second = html.find("window 1").unwrap();
    assert!(first < second, "{html}");
    assert!(html.contains("window 2 (current)"));
    assert_eq!(count(&html, "(current)"), 1);
}

#[test]
fn a_surface_pane_draws_its_surface_and_an_unbound_one_says_so() {
    let item = "7f3a2b1c-0000-4000-8000-000000000001";
    let surface = layout(
        json!({
            "1": {"pane": {"view_kind": "surface", "role": "detail",
                "query": {"kinds": ["surface"], "scope": {"scope": "item", "id": {"ref": "id", "id": item}}}}},
            "2": {"pane": {"view_kind": "surface", "role": "detail",
                "query": {"kinds": ["surface"], "scope": {"scope": "item", "id": {"ref": "param", "name": "item"}}}}},
            "3": {"container": {"linear": {"dir": "vertical", "children": [1, 2], "shares": [1.0, 1.0]}}},
        }),
        json!([{"id": 1, "root": 3, "focused": 1}]),
    );
    assert_eq!(
        pane_body(&surface, TileId(1)),
        Some(PaneBody::Surface(item.to_string()))
    );
    assert_eq!(pane_body(&surface, TileId(2)), Some(PaneBody::NoSurface));
    let html = render(tree(surface, Some(1)));
    // The live surface mounts (its first render has not answered yet) and the
    // unbound pane names what is missing; neither is the placeholder.
    assert!(html.contains("rendering…"), "{html}");
    assert!(html.contains("No Surface"));
    assert!(!html.contains("View Kind Unavailable"));
}

#[test]
fn a_surface_pane_follows_a_pinned_item_and_a_channels_selection() {
    let pinned = "7f3a2b1c-0000-4000-8000-000000000002";
    let selected = "7f3a2b1c-0000-4000-8000-000000000003";
    let mut with_params = layout(
        json!({
            "1": {"pane": {"view_kind": "surface",
                "query": {"scope": {"scope": "item", "id": {"ref": "param", "name": "item"}}},
                "params": [{"decl": {"name": "item", "kind": "surface", "required": false},
                            "source": {"source": "fixed", "item": pinned}}]}},
            "2": {"pane": {"view_kind": "surface",
                "query": {"scope": {"scope": "item", "id": {"ref": "param", "name": "item"}}},
                "params": [{"decl": {"name": "item", "kind": "surface", "required": false},
                            "source": {"source": "channel", "channel": {"number": 1}}}]}},
            "3": {"container": {"linear": {"dir": "horizontal", "children": [1, 2], "shares": [1.0, 1.0]}}},
        }),
        json!([{"id": 1, "root": 3, "focused": 1}]),
    );
    assert_eq!(
        pane_body(&with_params, TileId(1)),
        Some(PaneBody::Surface(pinned.to_string()))
    );
    assert_eq!(
        pane_body(&with_params, TileId(2)),
        Some(PaneBody::NoSurface)
    );
    with_params
        .channels
        .publish(1, "surface", vec![selected.parse().unwrap()]);
    assert_eq!(
        pane_body(&with_params, TileId(2)),
        Some(PaneBody::Surface(selected.to_string()))
    );
}

#[test]
fn a_divider_drag_is_the_resize_the_gesture_module_computes() {
    let layout = two_panes();
    // Shares [1, 3]; the child before the divider grew to 300 px, the one
    // after shrank to 100.
    let expected = gesture::resize_verb(3, &[1.0, 3.0], 0, 300.0, 100.0);
    assert!(expected.is_some());
    assert_eq!(resize_for_drag(&layout, 3, 0, 300.0, 100.0), expected);
    let Some(Verb::Resize { container, shares }) = expected else {
        panic!("a resize");
    };
    assert_eq!(container.raw(), 3);
    assert_eq!(
        shares,
        vec![9.0, 3.0],
        "3 * 300 / 100 against the neighbour's 3"
    );
    // Not a linear container, an unknown tile, or a collapsed neighbour: no verb.
    assert_eq!(resize_for_drag(&layout, 1, 0, 300.0, 100.0), None);
    assert_eq!(resize_for_drag(&layout, 99, 0, 300.0, 100.0), None);
    assert_eq!(resize_for_drag(&layout, 3, 0, 300.0, 0.0), None);
    assert_eq!(resize_for_drag(&layout, 3, 1, 300.0, 100.0), None);
}

#[test]
fn small_pure_helpers() {
    assert_eq!(flex_grow(&[1.0, 2.5], 1), 2.5);
    assert_eq!(flex_grow(&[1.0], 4), 1.0, "a missing share");
    assert_eq!(flex_grow(&[f32::NAN], 0), 1.0);
    assert_eq!(flex_grow(&[-1.0], 0), 1.0);
    assert_eq!(
        flex_grow(&[0.0], 0),
        0.0,
        "a collapsed share stays collapsed"
    );
    assert_eq!(grid_columns(Some(3), 9), 3);
    assert_eq!(grid_columns(None, 5), 3);
    assert_eq!(grid_columns(None, 4), 2);
    assert_eq!(grid_columns(None, 0), 1);
    assert_eq!(grid_columns(Some(0), 4), 2);
}

#[test]
fn a_fetch_that_lost_the_race_is_not_shown() {
    let at = |revision| LayoutTree {
        revision,
        ..tree(two_panes(), None)
    };
    assert!(newer(None, &at(Some(3))));
    assert!(newer(Some(&at(Some(3))), &at(Some(3))));
    assert!(newer(Some(&at(Some(3))), &at(Some(4))));
    assert!(!newer(Some(&at(Some(4))), &at(Some(3))));
    assert!(
        newer(Some(&at(Some(4))), &at(None)),
        "no revision to order by"
    );
}

// ---- the outline (docs/plan-koine-affordances.md A-D3) ----

/// Tile 5 is a row of pane 1 and a column (4) of panes 2 and 3.
fn nested() -> Layout {
    layout(
        json!({
            "1": pane("surface", "left"),
            "2": pane("list", "list"),
            "3": pane("info", "detail"),
            "4": {"container": {"linear": {"dir": "vertical", "children": [2, 3], "shares": [1.0, 1.0]}}},
            "5": {"container": {"linear": {"dir": "horizontal", "children": [1, 4], "shares": [1.0, 1.0]}}},
        }),
        json!([{"id": 1, "root": 5, "focused": 2}]),
    )
}

#[test]
fn the_outline_is_every_tile_under_the_root_in_container_order() {
    let layout = nested();
    let window = &layout.windows[0];
    assert_eq!(
        outline_rows(&layout, window),
        vec![
            json!({"id": 1, "parent": null, "label": "surface · left"}),
            json!({"id": 4, "parent": null, "label": "split-v"}),
            json!({"id": 2, "parent": 4, "label": "list · list"}),
            json!({"id": 3, "parent": 4, "label": "info · detail"}),
        ]
    );
    assert_eq!(outline_containers(&layout, window), vec![4]);
    // A window whose root is a pane is that pane alone.
    let single = layout_page_single();
    assert_eq!(
        outline_rows(&single, &single.windows[0]),
        vec![json!({"id": 1, "parent": null, "label": "list · list"})]
    );
}

fn layout_page_single() -> Layout {
    layout(
        json!({"1": pane("list", "list")}),
        json!([{"id": 1, "root": 1, "focused": 1}]),
    )
}

#[test]
fn the_outline_is_one_movable_tree_with_every_container_open_but_the_folded() {
    let layout = nested();
    let window = &layout.windows[0];
    let tree = outline_tree(&layout, window, &BTreeSet::new());
    assert_eq!(tree.focus_order, vec!["tiles-1".to_string()]);
    let RenderKind::Tree {
        nodes,
        expanded,
        movable,
        actions,
        ..
    } = &tree.root.node
    else {
        panic!("a tree");
    };
    assert!(movable);
    assert!(actions.is_empty());
    assert_eq!(expanded, &vec![json!(4)]);
    let ids: Vec<&Value> = nodes.iter().map(|node| &node.id).collect();
    assert_eq!(ids, vec![&json!(1), &json!(4), &json!(2), &json!(3)]);
    assert_eq!(nodes[2].depth, 1);
    // Folded: the column's panes are hidden, its state kept here.
    let folded = folded_after(&layout, window, &json!([]));
    assert_eq!(folded, BTreeSet::from([4]));
    let tree = outline_tree(&layout, window, &folded);
    let RenderKind::Tree { nodes, .. } = &tree.root.node else {
        panic!("a tree");
    };
    assert_eq!(nodes.len(), 2);
    assert!(folded_after(&layout, window, &json!([4])).is_empty());
}

#[test]
fn the_outline_draws_through_the_tree_widget_above_the_panes() {
    let html = render(tree(nested(), Some(2)));
    let outline = html.find("data-koine-outline=\"1\"").expect("an outline");
    let pane = html.find("data-koine-tile=\"").unwrap();
    assert!(outline < pane, "{html}");
    assert!(html.contains("data-koine-id=\"tiles-1\""), "{html}");
    assert_eq!(count(&html, "koine-tree-node"), 4, "{html}");
    // The focused tile is the active row.
    let active = html
        .find("data-koine-active=\"true\"")
        .expect("an active row");
    let row = html[..active].rfind("<li").unwrap();
    assert!(html[row..].contains("list · list"), "{html}");
    assert_eq!(count(&html, "data-koine-active=\"true\""), 1);
    // The root's kind is in the heading, and the page has something to draw.
    assert!(html.contains("window 1 (current) · split-h"), "{html}");
    assert!(!html.contains(NOTHING_TO_LAY_OUT));
}

fn event(kind: EventKind, value: Value) -> Event {
    Event {
        widget: "tiles-1".into(),
        kind,
        value,
    }
}

#[test]
fn a_move_in_the_outline_is_the_move_tile_tree_move_verb_names() {
    let layout = nested();
    // Pane 1 dropped in the column, between 2 and 3.
    let moved = event(EventKind::Move, json!({"ids": [1], "to": 4, "index": 1}));
    assert_eq!(
        outline_verb(&layout, Some(2), &moved),
        gesture::tree_move_verb(&layout, &[1], Some(4), 1)
    );
    assert_eq!(
        serde_json::to_value(outline_verb(&layout, Some(2), &moved).unwrap()).unwrap(),
        json!({"verb": "move-tile", "tile": {"id": 1}, "target": {"id": 3}, "placement": "above"})
    );
    // Pane 3 dropped among the roots, first: before pane 1.
    let to_root = event(EventKind::Move, json!({"ids": [3], "to": null, "index": 0}));
    assert_eq!(
        outline_verb(&layout, Some(2), &to_root),
        Some(Verb::MoveTile {
            tile: PaneRef::id(TileId(3)),
            target: PaneRef::id(TileId(1)),
            placement: Placement::Left,
        })
    );
    // A value that is not a move is no verb.
    let broken = event(
        EventKind::Move,
        json!({"ids": ["one"], "to": 4, "index": 0}),
    );
    assert_eq!(outline_verb(&layout, Some(2), &broken), None);
    let disclosure = event(EventKind::Change, json!([]));
    assert_eq!(outline_verb(&layout, Some(2), &disclosure), None);
}

#[test]
fn a_click_on_a_row_focuses_its_tile_or_a_containers_first_pane() {
    let layout = nested();
    let focus = |id: u64| {
        gesture::layout_verb(
            &Gesture::Focus {
                element: GestureElement::Tile { id },
            },
            Some(id),
        )
    };
    let select = |id: u64| event(EventKind::Select, json!([id]));
    assert_eq!(outline_verb(&layout, Some(2), &select(1)), focus(1));
    assert_eq!(outline_verb(&layout, Some(1), &select(4)), focus(2));
    assert_eq!(
        outline_verb(&layout, Some(2), &select(2)),
        None,
        "already focused"
    );
    assert_eq!(outline_verb(&layout, Some(2), &select(4)), None);
    assert_eq!(outline_verb(&layout, Some(2), &select(99)), None);
}

#[test]
fn only_a_surface_pane_makes_the_layout_drawable() {
    assert!(!drawable(&two_panes()));
    assert!(
        drawable(&nested()),
        "pane 1 is a surface pane, bound or not"
    );
}
