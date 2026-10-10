//! Normalization (ADR-0031 D4): the tree is brought back to canonical shape
//! after every mutation, and doing it twice changes nothing.

use crate::common::{assert_arena_is_sound, assert_focus_is_a_leaf, scratch_pane, three_column};
use impress_layout::{
    Container, ContainerKind, Layout, LinearDir, PaneRef, PaneSpec, Placement, TileId, Verb,
    ViewKindId, Window, WindowId,
};

/// A deliberately pathological tree: nested same-direction linears, a
/// tabs-in-tabs, an empty container, a single-child container, and a tile no
/// window can reach.
fn messy() -> (Layout, Vec<TileId>) {
    let mut layout = Layout::empty();
    let pane = |layout: &mut Layout, kind: ViewKindId| {
        layout.insert_pane(PaneSpec::new(Default::default(), kind))
    };
    let a = pane(&mut layout, ViewKindId::OUTLINE);
    let b = pane(&mut layout, ViewKindId::LIST);
    let c = pane(&mut layout, ViewKindId::INFO);
    let d = pane(&mut layout, ViewKindId::PDF);
    let orphan = pane(&mut layout, ViewKindId::CONSOLE);

    // Linear H [ B, C ] with shares 1 : 3 — to be joined into its parent.
    let inner = layout.insert_container(Container::linear(
        LinearDir::Horizontal,
        vec![b, c],
        vec![1.0, 3.0],
    ));
    // An empty tab strip — to be pruned.
    let empty = layout.insert_container(Container::empty(ContainerKind::Tabs));
    // A vertical container with exactly one child — to be collapsed.
    let lonely =
        layout.insert_container(Container::linear(LinearDir::Vertical, vec![d], vec![1.0]));
    let root = layout.insert_container(Container::linear(
        LinearDir::Horizontal,
        vec![a, inner, empty, lonely],
        vec![1.0, 1.0, 1.0, 1.0],
    ));
    let mut window = Window::new(WindowId::new(1), root);
    window.focused = Some(c);
    layout.windows.push(window);
    (layout, vec![a, b, c, d, orphan])
}

#[test]
fn normalization_joins_prunes_collapses_and_collects() {
    let (mut layout, ids) = messy();
    let (a, b, c, d, orphan) = (ids[0], ids[1], ids[2], ids[3], ids[4]);
    layout.normalize();

    let window = WindowId::new(1);
    assert_eq!(
        layout.leaves(window),
        vec![a, b, c, d],
        "every pane survives, in tree order"
    );
    let root = layout.window(window).unwrap().root;
    let container = layout.tile(root).unwrap().as_container().unwrap();
    assert_eq!(container.kind(), ContainerKind::Horizontal);
    assert_eq!(container.children(), [a, b, c, d]);

    // The joined linear's shares were scaled into the slot it occupied:
    // 1 : 3 within a slot of weight 1 becomes 0.25 : 0.75.
    let shares = container.shares().unwrap();
    assert!((shares[0] - 1.0).abs() < 1e-6);
    assert!((shares[1] - 0.25).abs() < 1e-6);
    assert!((shares[2] - 0.75).abs() < 1e-6);
    assert!((shares[3] - 1.0).abs() < 1e-6);

    assert!(
        layout.tile(orphan).is_none(),
        "unreachable tiles are collected"
    );
    assert_eq!(layout.window(window).unwrap().focused, Some(c));
    assert_arena_is_sound(&layout);
    assert_focus_is_a_leaf(&layout);
}

#[test]
fn normalization_is_idempotent() {
    let (mut layout, _) = messy();
    layout.normalize();
    let once = layout.clone();
    layout.normalize();
    assert_eq!(layout, once, "normalize(normalize(x)) == normalize(x)");
}

#[test]
fn tabs_inside_tabs_are_flattened_keeping_the_visible_tab() {
    let mut layout = Layout::empty();
    let pane = |layout: &mut Layout| {
        layout.insert_pane(PaneSpec::new(Default::default(), ViewKindId::INFO))
    };
    let a = pane(&mut layout);
    let b = pane(&mut layout);
    let c = pane(&mut layout);
    let inner = layout.insert_container(Container::Tabs {
        children: vec![b, c],
        active: Some(c),
    });
    let root = layout.insert_container(Container::Tabs {
        children: vec![a, inner],
        active: Some(inner),
    });
    layout.add_window(root);
    layout.normalize();

    let container = layout.tile(root).unwrap().as_container().unwrap();
    assert_eq!(container.children(), [a, b, c]);
    assert!(matches!(container, Container::Tabs { active, .. } if *active == Some(c)));
}

#[test]
fn closing_down_to_one_pane_leaves_that_pane_as_the_root() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::Close {
            target: PaneRef::id(parts.navigator),
        })
        .unwrap();
    layout
        .apply(Verb::Close {
            target: PaneRef::id(parts.detail),
        })
        .unwrap();
    assert_eq!(layout.window(parts.window).unwrap().root, parts.list);
    assert_eq!(layout.leaves(parts.window), vec![parts.list]);
    assert_arena_is_sound(&layout);
    assert_focus_is_a_leaf(&layout);
}

#[test]
fn every_verb_leaves_the_tree_normalized() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::Split {
            target: PaneRef::id(parts.detail),
            dir: LinearDir::Vertical,
            after: true,
            new: Some(scratch_pane()),
        })
        .unwrap();
    let extra = layout.window(parts.window).unwrap().focused.unwrap();
    layout
        .apply(Verb::MoveTile {
            tile: PaneRef::id(extra),
            target: PaneRef::id(parts.navigator),
            placement: Placement::Left,
        })
        .unwrap();
    // The vertical wrapper around the detail pane had one child left; it is
    // gone, not left as a container of one.
    let mut again = layout.clone();
    again.normalize();
    assert_eq!(again, layout);
    assert_arena_is_sound(&layout);
}
