use serde_json::{json, Value};

use super::*;
use crate::api::LayoutTree;
use koine::layout::Placement;

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
        let drag = use_signal(|| None);
        use_context_provider(move || Board {
            tree,
            drag,
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
    assert_eq!(count(&html, "data-koine-divider=\""), 1, "{html}");
    assert!(html.contains("data-koine-divider=\"0\""));
    // The frame: a grip and four edges on every pane.
    assert_eq!(count(&html, "data-koine-grip"), 2);
    for edge in ["top", "bottom", "left", "right"] {
        assert_eq!(
            count(&html, &format!("data-koine-edge=\"{edge}\"")),
            2,
            "{edge}"
        );
    }
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
    let html = render(tree(layout, Some(2)));
    assert_eq!(count(&html, "data-koine-tile=\""), 3, "{html}");
    assert_eq!(count(&html, "data-koine-divider=\""), 2);
    for share in ["flex-grow: 1;", "flex-grow: 2;", "flex-grow: 3;"] {
        assert!(html.contains(share), "{share}");
    }
    assert_eq!(count(&html, "View Kind Unavailable"), 3);
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
    // The strip is on the shown pane's top edge, after the grip.
    let grip = html.find("data-koine-grip").unwrap();
    let tab = html.find("data-koine-tab=\"0\"").unwrap();
    let title = html.find("koine-title").unwrap();
    assert!(grip < tab && tab < title);
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
fn a_grip_dragged_onto_a_right_edge_is_the_verb_the_gesture_names() {
    let layout = two_panes();
    // Pane 1's grip, released on pane 2's right edge.
    let gesture = Gesture::Drag {
        element: GestureElement::Tile { id: 1 },
        target: GestureElement::Tile { id: 2 },
        placement: Placement::Right,
    };
    let expected = gesture::layout_verb(&gesture, Some(2));
    assert!(expected.is_some());
    assert_eq!(
        drop_verb(&layout, Hit::Grip, Hit::Frame(Edge::Right), 1, 2),
        expected
    );
    assert_eq!(
        serde_json::to_value(expected.unwrap()).unwrap(),
        json!({"verb": "move-tile", "tile": {"id": 1}, "target": {"id": 2}, "placement": "right"})
    );
    // Dropped on the middle of another pane, it becomes tabs.
    assert_eq!(
        drop_verb(&layout, Hit::Title, Hit::Body, 1, 2),
        gesture::layout_verb(
            &Gesture::Drag {
                element: GestureElement::Tile { id: 1 },
                target: GestureElement::Tile { id: 2 },
                placement: Placement::IntoTabs,
            },
            Some(2)
        )
    );
    // On its own middle it cancels.
    assert_eq!(drop_verb(&layout, Hit::Grip, Hit::Body, 1, 1), None);
    // On its own edge, outside a tab strip, there is no sibling to split beside.
    assert_eq!(
        drop_verb(&layout, Hit::Grip, Hit::Frame(Edge::Left), 1, 1),
        None
    );
}

#[test]
fn a_tabbed_pane_dropped_on_its_own_edge_splits_beside_its_sibling() {
    let layout = tabbed();
    // Tile 2 is last in the strip; a right edge uses the tab before it.
    let expected = gesture::layout_verb(
        &Gesture::Drag {
            element: GestureElement::Tile { id: 2 },
            target: GestureElement::Tile { id: 1 },
            placement: Placement::Right,
        },
        Some(1),
    );
    assert!(expected.is_some());
    assert_eq!(
        drop_verb(&layout, Hit::Grip, Hit::Frame(Edge::Right), 2, 2),
        expected
    );
    assert_eq!(
        tab_siblings(&layout, 2),
        Some((vec![1, 2], 1)),
        "the strip, in order"
    );
}

#[test]
fn a_click_is_a_focus_and_a_tab_click_focuses_the_tab() {
    let pane = GestureElement::Tile { id: 5 };
    let expected = gesture::layout_verb(
        &Gesture::Focus {
            element: pane.clone(),
        },
        Some(5),
    );
    assert!(expected.is_some());
    assert_eq!(
        frame::verb_for_pointer(Hit::Body, Pointer::Primary, &pane),
        expected
    );
    assert_eq!(
        frame::verb_for_pointer(Hit::Title, Pointer::Primary, &pane),
        expected
    );
    assert!(frame::verb_for_pointer(Hit::Grip, Pointer::Secondary, &pane).is_none());
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
