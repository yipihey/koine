//! One test per ADR-0031 D8 verb, plus the reference-resolution rules.

use crate::common::{
    assert_arena_is_sound, assert_focus_is_a_leaf, assert_focus_is_visible, scratch_pane,
    three_column,
};
use impress_layout::preset::DETAIL_PARAM;
use impress_layout::{
    ChannelId, Container, ContainerKind, Direction, Geometry, Layout, LayoutError, PaneQuery,
    PaneRef, PaneSpec, ParamSource, Placement, Role, Verb, ViewKindId,
};
use uuid::Uuid;

fn window_of(layout: &Layout) -> impress_layout::WindowId {
    layout.current_window().unwrap()
}

// ------------------------------------------------------------- arrangement

#[test]
fn split_inserts_beside_the_target_and_focuses_the_new_pane() {
    let (mut layout, parts) = three_column();
    let before = layout.leaves(parts.window).len();

    layout
        .apply(Verb::Split {
            target: PaneRef::role(Role::DETAIL),
            dir: impress_layout::LinearDir::Vertical,
            after: true,
            new: Some(scratch_pane()),
        })
        .unwrap();

    let leaves = layout.leaves(parts.window);
    assert_eq!(leaves.len(), before + 1);
    let focused = layout.window(parts.window).unwrap().focused.unwrap();
    assert_eq!(
        layout.pane(focused).unwrap().view_kind,
        ViewKindId::PDF,
        "focus must follow the new pane"
    );
    // The new pane is below the detail pane, not beside the whole row.
    assert_eq!(
        layout.step(parts.window, parts.detail, Direction::Down),
        focused
    );
    assert_focus_is_a_leaf(&layout);
    assert_arena_is_sound(&layout);
}

#[test]
fn split_along_the_parents_direction_extends_it_rather_than_nesting() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::Split {
            target: PaneRef::id(parts.list),
            dir: impress_layout::LinearDir::Horizontal,
            after: true,
            new: Some(scratch_pane()),
        })
        .unwrap();
    let root = layout.tile(parts.root).unwrap().as_container().unwrap();
    assert_eq!(root.len(), 4, "the row grew; it did not nest");
    let shares = root.shares().unwrap();
    assert!((shares[1] - 1.0).abs() < 1e-6, "the list's 2.0 was halved");
    assert!(
        (shares[2] - 1.0).abs() < 1e-6,
        "the newcomer took the other half"
    );
}

#[test]
fn move_tile_into_tabs_stacks_the_panes() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::MoveTile {
            tile: PaneRef::id(parts.navigator),
            target: PaneRef::id(parts.detail),
            placement: Placement::IntoTabs,
        })
        .unwrap();

    let parent = layout.parent_of(parts.navigator).unwrap();
    let container = layout.tile(parent).unwrap().as_container().unwrap();
    assert_eq!(container.kind(), ContainerKind::Tabs);
    assert!(matches!(
        container,
        Container::Tabs { active, .. } if *active == Some(parts.navigator)
    ));
    assert_eq!(layout.leaves(parts.window).len(), 3);
    assert_arena_is_sound(&layout);
}

#[test]
fn move_tile_refuses_to_move_a_tile_into_its_own_subtree() {
    let (mut layout, parts) = three_column();
    let err = layout
        .apply(Verb::MoveTile {
            tile: PaneRef::id(parts.root),
            target: PaneRef::id(parts.list),
            placement: Placement::Right,
        })
        .unwrap_err();
    assert_eq!(err, LayoutError::CyclicMove { tile: parts.root });
}

#[test]
fn close_removes_the_pane_and_focuses_the_neighbour() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::Close {
            target: PaneRef::role(Role::LIST),
        })
        .unwrap();

    assert!(layout.pane(parts.list).is_none());
    assert!(layout.pane_with_role(&Role::LIST).is_none());
    assert_eq!(
        layout.window(parts.window).unwrap().focused,
        Some(parts.detail),
        "focus moves to the next surviving pane"
    );
    assert_focus_is_a_leaf(&layout);
    assert_arena_is_sound(&layout);
}

#[test]
fn close_refuses_the_last_pane_of_a_window() {
    let mut layout = Layout::new_single_pane(scratch_pane());
    let err = layout
        .apply(Verb::Close {
            target: PaneRef::Focused,
        })
        .unwrap_err();
    assert_eq!(err, LayoutError::CannotCloseLastPane);
    assert_eq!(layout.panes().len(), 1, "the refusal changed nothing");
}

#[test]
fn close_refuses_a_subtree_holding_every_pane() {
    let (mut layout, parts) = three_column();
    let err = layout
        .apply(Verb::Close {
            target: PaneRef::id(parts.root),
        })
        .unwrap_err();
    assert_eq!(err, LayoutError::CannotCloseLastPane);
}

#[test]
fn swap_exchanges_two_panes_keeping_each_slots_share() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::Swap {
            a: PaneRef::id(parts.navigator),
            b: PaneRef::id(parts.detail),
        })
        .unwrap();
    let root = layout.tile(parts.root).unwrap().as_container().unwrap();
    assert_eq!(root.children(), [parts.detail, parts.list, parts.navigator]);
    assert_eq!(
        root.shares().unwrap(),
        [1.0, 2.0, 3.0],
        "shares belong to slots"
    );
}

#[test]
fn resize_sets_the_shares_and_rejects_the_wrong_number_of_them() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::Resize {
            container: parts.root,
            shares: vec![2.0, 3.0, 5.0],
        })
        .unwrap();
    let root = layout.tile(parts.root).unwrap().as_container().unwrap();
    assert_eq!(root.shares().unwrap(), [2.0, 3.0, 5.0]);

    let err = layout
        .apply(Verb::Resize {
            container: parts.root,
            shares: vec![1.0, 1.0],
        })
        .unwrap_err();
    assert!(matches!(err, LayoutError::InvalidShares { .. }));

    let err = layout
        .apply(Verb::Resize {
            container: parts.root,
            shares: vec![1.0, 0.0, 1.0],
        })
        .unwrap_err();
    assert!(matches!(err, LayoutError::InvalidShares { .. }));

    let err = layout
        .apply(Verb::Resize {
            container: parts.list,
            shares: vec![1.0],
        })
        .unwrap_err();
    assert_eq!(err, LayoutError::NotAContainer { tile: parts.list });
}

#[test]
fn set_container_kind_retypes_in_place() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::SetContainerKind {
            container: parts.root,
            kind: ContainerKind::Vertical,
        })
        .unwrap();
    let root = layout.tile(parts.root).unwrap().as_container().unwrap();
    assert_eq!(root.kind(), ContainerKind::Vertical);
    assert_eq!(
        root.shares().unwrap(),
        [1.0, 2.0, 3.0],
        "rotating keeps shares"
    );
    assert_eq!(root.children(), [parts.navigator, parts.list, parts.detail]);

    layout
        .apply(Verb::SetContainerKind {
            container: parts.root,
            kind: ContainerKind::Tabs,
        })
        .unwrap();
    let root = layout.tile(parts.root).unwrap().as_container().unwrap();
    assert_eq!(root.kind(), ContainerKind::Tabs);
    assert_focus_is_a_leaf(&layout);
}

#[test]
fn maximize_and_restore_do_not_touch_the_tree() {
    let (mut layout, parts) = three_column();
    let tiles_before = layout.tiles.clone();

    layout
        .apply(Verb::Maximize {
            target: PaneRef::role(Role::DETAIL),
        })
        .unwrap();
    assert_eq!(
        layout.window(parts.window).unwrap().maximized,
        Some(parts.detail)
    );
    assert_eq!(
        layout.window(parts.window).unwrap().focused,
        Some(parts.detail),
        "maximizing focuses what it shows"
    );
    assert_eq!(
        layout.tiles, tiles_before,
        "zoom is window state, not a mutation"
    );

    layout.apply(Verb::Restore).unwrap();
    assert_eq!(layout.window(parts.window).unwrap().maximized, None);
    assert_eq!(layout.tiles, tiles_before);
}

#[test]
fn restore_with_nothing_maximized_is_an_empty_patch() {
    let (mut layout, _) = three_column();
    let patch = layout.apply(Verb::Restore).unwrap();
    assert!(patch.is_empty());
}

// ----------------------------------------------------------------- content

#[test]
fn set_pane_replaces_the_whole_spec_and_refuses_a_container() {
    let (mut layout, parts) = three_column();
    let replacement = PaneSpec::new(PaneQuery::default(), ViewKindId::PLOT);
    layout
        .apply(Verb::SetPane {
            target: PaneRef::id(parts.detail),
            spec: replacement.clone(),
        })
        .unwrap();
    assert_eq!(layout.pane(parts.detail).unwrap(), &replacement);

    let err = layout
        .apply(Verb::SetPane {
            target: PaneRef::id(parts.root),
            spec: replacement,
        })
        .unwrap_err();
    assert_eq!(err, LayoutError::NotAPane { tile: parts.root });
}

#[test]
fn set_query_and_set_view_kind_patch_the_spec() {
    let (mut layout, parts) = three_column();
    let query = PaneQuery {
        kinds: vec!["manuscript".to_string()],
        text: Some("dark matter".to_string()),
        ..PaneQuery::default()
    };
    layout
        .apply(Verb::SetQuery {
            target: PaneRef::role(Role::LIST),
            query: query.clone(),
        })
        .unwrap();
    assert_eq!(layout.pane(parts.list).unwrap().query, query);

    layout
        .apply(Verb::SetViewKind {
            target: PaneRef::role(Role::LIST),
            view_kind: ViewKindId::PLOT,
        })
        .unwrap();
    assert_eq!(layout.pane(parts.list).unwrap().view_kind, ViewKindId::PLOT);
}

#[test]
fn bind_param_repoints_a_declared_parameter_and_rejects_an_undeclared_one() {
    let (mut layout, parts) = three_column();
    let pinned = Uuid::from_u128(42);
    layout
        .apply(Verb::BindParam {
            target: PaneRef::role(Role::DETAIL),
            name: DETAIL_PARAM.to_string(),
            source: ParamSource::Fixed { item: pinned },
        })
        .unwrap();
    assert_eq!(
        layout
            .pane(parts.detail)
            .unwrap()
            .param(DETAIL_PARAM)
            .unwrap()
            .source,
        ParamSource::Fixed { item: pinned }
    );
    assert_eq!(
        layout.bindings_for(parts.detail).get(DETAIL_PARAM),
        Some(pinned)
    );

    let err = layout
        .apply(Verb::BindParam {
            target: PaneRef::role(Role::DETAIL),
            name: "colormap".to_string(),
            source: ParamSource::Default,
        })
        .unwrap_err();
    assert_eq!(
        err,
        LayoutError::UnknownParam {
            tile: parts.detail,
            name: "colormap".to_string()
        }
    );
}

#[test]
fn set_channel_changes_where_a_pane_publishes() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::SetChannel {
            target: PaneRef::role(Role::LIST),
            channel: ChannelId::number(3),
        })
        .unwrap();
    layout
        .apply(Verb::Select {
            target: PaneRef::role(Role::LIST),
            kind: "publication".to_string(),
            ids: vec![Uuid::from_u128(7)],
        })
        .unwrap();
    assert_eq!(
        layout.channels.current(3, "publication"),
        Some(Uuid::from_u128(7))
    );
    assert_eq!(layout.channels.current(1, "publication"), None);
    // The detail pane still follows channel 1, so it is unaffected.
    assert_eq!(layout.bindings_for(parts.detail).get(DETAIL_PARAM), None);
}

#[test]
fn set_role_moves_the_role_off_whichever_pane_held_it() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::SetRole {
            target: PaneRef::id(parts.navigator),
            role: Some(Role::DETAIL),
        })
        .unwrap();
    assert_eq!(
        layout.pane(parts.navigator).unwrap().role,
        Some(Role::DETAIL)
    );
    assert_eq!(
        layout.pane(parts.detail).unwrap().role,
        None,
        "a role is unique per window, or ⌘0 would be a coin toss"
    );
    assert_eq!(
        layout
            .resolve(parts.window, &PaneRef::role(Role::DETAIL))
            .unwrap(),
        parts.navigator
    );

    layout
        .apply(Verb::SetRole {
            target: PaneRef::id(parts.navigator),
            role: None,
        })
        .unwrap();
    assert!(layout.pane_with_role(&Role::DETAIL).is_none());
    let err = layout
        .resolve(parts.window, &PaneRef::role(Role::DETAIL))
        .unwrap_err();
    assert_eq!(err, LayoutError::NoPaneWithRole { role: Role::DETAIL });
}

// -------------------------------------------------------- focus / selection

#[test]
fn focus_and_focus_direction_walk_the_leaves() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::Focus {
            target: PaneRef::role(Role::NAVIGATOR),
        })
        .unwrap();
    assert_eq!(
        layout.window(parts.window).unwrap().focused,
        Some(parts.navigator)
    );

    layout
        .apply(Verb::FocusDirection {
            direction: Direction::Right,
        })
        .unwrap();
    assert_eq!(
        layout.window(parts.window).unwrap().focused,
        Some(parts.list)
    );

    layout
        .apply(Verb::FocusDirection {
            direction: Direction::Left,
        })
        .unwrap();
    assert_eq!(
        layout.window(parts.window).unwrap().focused,
        Some(parts.navigator)
    );

    // At the edge, h stays put rather than failing.
    layout
        .apply(Verb::FocusDirection {
            direction: Direction::Left,
        })
        .unwrap();
    assert_eq!(
        layout.window(parts.window).unwrap().focused,
        Some(parts.navigator)
    );

    // Next wraps, as PaneFocusCycler does.
    layout
        .apply(Verb::Focus {
            target: PaneRef::role(Role::DETAIL),
        })
        .unwrap();
    layout
        .apply(Verb::FocusDirection {
            direction: Direction::Next,
        })
        .unwrap();
    assert_eq!(
        layout.window(parts.window).unwrap().focused,
        Some(parts.navigator)
    );
}

/// The tab strip a tabbed fixture builds, and the tabs in it.
fn tabbed(
    layout: &mut Layout,
    parts: &impress_layout::preset::ThreeColumn,
) -> impress_layout::TileId {
    // navigator and list both tabbed over the detail pane: one strip,
    // [detail, navigator, list], holding the window's whole tree.
    layout
        .apply(Verb::MoveTile {
            tile: PaneRef::id(parts.navigator),
            target: PaneRef::id(parts.detail),
            placement: Placement::IntoTabs,
        })
        .unwrap();
    layout
        .apply(Verb::MoveTile {
            tile: PaneRef::id(parts.list),
            target: PaneRef::id(parts.navigator),
            placement: Placement::IntoTabs,
        })
        .unwrap();
    layout.parent_of(parts.detail).unwrap()
}

fn active_tab(layout: &Layout, strip: impress_layout::TileId) -> Option<impress_layout::TileId> {
    match layout.tile(strip).unwrap().as_container().unwrap() {
        Container::Tabs { active, .. } => *active,
        other => panic!("tile {strip} is not a tab strip: {other:?}"),
    }
}

#[test]
fn focusing_a_pane_behind_an_inactive_tab_makes_it_the_visible_one() {
    let (mut layout, parts) = three_column();
    let strip = tabbed(&mut layout, &parts);
    assert_eq!(
        active_tab(&layout, strip),
        Some(parts.list),
        "focus followed the last pane moved into the strip"
    );

    let patch = layout
        .apply(Verb::Focus {
            target: PaneRef::id(parts.detail),
        })
        .unwrap();

    assert_eq!(
        layout.window(parts.window).unwrap().focused,
        Some(parts.detail)
    );
    assert_eq!(
        active_tab(&layout, strip),
        Some(parts.detail),
        "focusing a hidden tab shows it"
    );
    assert!(
        patch.tiles.contains_key(&strip),
        "the patch captures the tab strip, so undo puts the visible tab back"
    );
    layout.revert(&patch);
    assert_eq!(active_tab(&layout, strip), Some(parts.list));
    assert_focus_is_visible(&layout);
}

#[test]
fn focus_direction_next_activates_each_tab_in_turn() {
    let (mut layout, parts) = three_column();
    let strip = tabbed(&mut layout, &parts);
    layout
        .apply(Verb::Focus {
            target: PaneRef::id(parts.detail),
        })
        .unwrap();

    // Tree order in the strip: detail, navigator, list — and Next wraps.
    for expected in [parts.navigator, parts.list, parts.detail] {
        layout
            .apply(Verb::FocusDirection {
                direction: Direction::Next,
            })
            .unwrap();
        assert_eq!(layout.window(parts.window).unwrap().focused, Some(expected));
        assert_eq!(
            active_tab(&layout, strip),
            Some(expected),
            "stepping onto a tab shows it"
        );
        assert_focus_is_visible(&layout);
    }
}

#[test]
fn retyping_a_row_into_tabs_keeps_the_focused_pane_visible() {
    // `SetContainerKind` never touches focus, so normalization is what keeps
    // the invariant standing here.
    let (mut layout, parts) = three_column();
    assert_eq!(
        layout.window(parts.window).unwrap().focused,
        Some(parts.list)
    );
    layout
        .apply(Verb::SetContainerKind {
            container: parts.root,
            kind: ContainerKind::Tabs,
        })
        .unwrap();
    assert_eq!(active_tab(&layout, parts.root), Some(parts.list));
    assert_focus_is_visible(&layout);
}

#[test]
fn focusing_a_container_focuses_the_pane_it_shows() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::Focus {
            target: PaneRef::id(parts.root),
        })
        .unwrap();
    assert_eq!(
        layout.window(parts.window).unwrap().focused,
        Some(parts.navigator)
    );
    assert_focus_is_a_leaf(&layout);
}

#[test]
fn select_publishes_on_the_panes_channel_and_drives_exactly_the_bound_panes() {
    let (mut layout, parts) = three_column();
    let chosen = Uuid::from_u128(1234);

    layout
        .apply(Verb::Select {
            target: PaneRef::role(Role::LIST),
            kind: "publication".to_string(),
            ids: vec![chosen, Uuid::from_u128(5678)],
        })
        .unwrap();

    assert_eq!(
        layout.channels.selection(1, "publication"),
        [chosen, Uuid::from_u128(5678)]
    );
    assert_eq!(
        layout.affected_panes(ChannelId::ONE, "publication"),
        vec![parts.detail],
        "only the detail pane binds a publication on channel 1"
    );
    assert_eq!(
        layout.bindings_for(parts.detail).get(DETAIL_PARAM),
        Some(chosen),
        "a single-valued parameter takes the first id of the selection"
    );
    // A selection of another kind on the same channel leaves it alone.
    layout
        .apply(Verb::Select {
            target: PaneRef::role(Role::LIST),
            kind: "manuscript".to_string(),
            ids: vec![Uuid::from_u128(9)],
        })
        .unwrap();
    assert_eq!(
        layout.bindings_for(parts.detail).get(DETAIL_PARAM),
        Some(chosen)
    );
    assert!(layout
        .affected_panes(ChannelId::ONE, "manuscript")
        .is_empty());
}

#[test]
fn an_empty_selection_is_a_value_and_unbinds_the_parameter() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::Select {
            target: PaneRef::role(Role::LIST),
            kind: "publication".to_string(),
            ids: vec![Uuid::from_u128(1)],
        })
        .unwrap();
    layout
        .apply(Verb::Select {
            target: PaneRef::role(Role::LIST),
            kind: "publication".to_string(),
            ids: vec![],
        })
        .unwrap();
    assert_eq!(layout.bindings_for(parts.detail).get(DETAIL_PARAM), None);
}

#[test]
fn follow_resolves_to_the_windows_default_channel() {
    let (mut layout, parts) = three_column();
    layout.window_mut(parts.window).unwrap().default_channel = ChannelId::number(4);
    layout
        .apply(Verb::BindParam {
            target: PaneRef::role(Role::DETAIL),
            name: DETAIL_PARAM.to_string(),
            source: ParamSource::follow(),
        })
        .unwrap();
    layout
        .apply(Verb::SetChannel {
            target: PaneRef::role(Role::LIST),
            channel: ChannelId::Follow,
        })
        .unwrap();
    let chosen = Uuid::from_u128(77);
    layout
        .apply(Verb::Select {
            target: PaneRef::role(Role::LIST),
            kind: "publication".to_string(),
            ids: vec![chosen],
        })
        .unwrap();

    assert_eq!(
        layout.channels.current(4, "publication"),
        Some(chosen),
        "the publishing pane's `follow` resolved to the window default"
    );
    assert_eq!(
        layout.bindings_for(parts.detail).get(DETAIL_PARAM),
        Some(chosen)
    );
    assert_eq!(
        layout.affected_panes(ChannelId::Follow, "publication"),
        vec![parts.detail]
    );
    assert_eq!(
        layout.affected_panes(ChannelId::number(4), "publication"),
        vec![parts.detail]
    );
    assert!(layout
        .affected_panes(ChannelId::ONE, "publication")
        .is_empty());
}

// ------------------------------------------------------------------ window

#[test]
fn set_window_geometry_stores_the_device_scoped_frame() {
    let (mut layout, parts) = three_column();
    let geometry = Geometry {
        x: 10.0,
        y: 20.0,
        w: 1440.0,
        h: 900.0,
        display: Some("Built-in".to_string()),
    };
    layout
        .apply(Verb::SetWindowGeometry {
            window: parts.window,
            geometry: Some(geometry.clone()),
        })
        .unwrap();
    assert_eq!(
        layout.window(parts.window).unwrap().geometry,
        Some(geometry)
    );

    let err = layout
        .apply(Verb::SetWindowGeometry {
            window: impress_layout::WindowId::new(99),
            geometry: None,
        })
        .unwrap_err();
    assert_eq!(
        err,
        LayoutError::UnknownWindow {
            window: impress_layout::WindowId::new(99)
        }
    );
}

// --------------------------------------------------------------- resolution

#[test]
fn resolve_finds_a_pane_by_role_by_id_and_by_focus() {
    let (layout, parts) = three_column();
    let window = window_of(&layout);
    assert_eq!(
        layout
            .resolve(window, &PaneRef::role(Role::NAVIGATOR))
            .unwrap(),
        parts.navigator
    );
    assert_eq!(
        layout
            .resolve(window, &PaneRef::role(Role::DETAIL))
            .unwrap(),
        parts.detail
    );
    assert_eq!(
        layout.resolve(window, &PaneRef::id(parts.list)).unwrap(),
        parts.list
    );
    assert_eq!(
        layout.resolve(window, &PaneRef::Focused).unwrap(),
        parts.list
    );
    assert_eq!(
        layout
            .resolve(window, &PaneRef::id(impress_layout::TileId::new(999)))
            .unwrap_err(),
        LayoutError::UnknownTile {
            tile: impress_layout::TileId::new(999)
        }
    );
}

#[test]
fn direction_right_from_the_navigator_is_the_list() {
    let (layout, parts) = three_column();
    let window = window_of(&layout);
    assert_eq!(
        layout.step(window, parts.navigator, Direction::Right),
        parts.list
    );
    assert_eq!(
        layout.step(window, parts.list, Direction::Right),
        parts.detail
    );
    assert_eq!(
        layout.step(window, parts.detail, Direction::Right),
        parts.detail
    );
    assert_eq!(
        layout.step(window, parts.detail, Direction::Left),
        parts.list
    );
    // The row is horizontal, so there is nothing above or below anything.
    assert_eq!(layout.step(window, parts.list, Direction::Up), parts.list);
    assert_eq!(layout.step(window, parts.list, Direction::Down), parts.list);
}

#[test]
fn direction_steps_into_a_tab_strips_visible_tab() {
    let (mut layout, parts) = three_column();
    let window = parts.window;
    // Tab a PDF pane over the detail pane and make it active.
    layout
        .apply(Verb::Split {
            target: PaneRef::id(parts.detail),
            dir: impress_layout::LinearDir::Vertical,
            after: true,
            new: Some(scratch_pane()),
        })
        .unwrap();
    let pdf = layout.window(window).unwrap().focused.unwrap();
    layout
        .apply(Verb::MoveTile {
            tile: PaneRef::id(pdf),
            target: PaneRef::id(parts.detail),
            placement: Placement::IntoTabs,
        })
        .unwrap();
    assert_eq!(
        layout.step(window, parts.list, Direction::Right),
        pdf,
        "stepping into a tab strip lands on the tab it shows"
    );
}

#[test]
fn resolve_reports_no_focus_rather_than_guessing() {
    let (mut layout, parts) = three_column();
    layout.window_mut(parts.window).unwrap().focused = None;
    let err = layout.resolve(parts.window, &PaneRef::Focused).unwrap_err();
    assert_eq!(
        err,
        LayoutError::NoFocus {
            window: parts.window
        }
    );
}

// ------------------------------------------------------- detach / windows

#[test]
fn detach_moves_a_pane_into_a_new_window_and_focus_follows_it() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::Detach {
            target: PaneRef::role(Role::DETAIL),
        })
        .unwrap();

    assert_eq!(layout.windows.len(), 2);
    let detached = layout.window_of(parts.detail).unwrap();
    assert_ne!(detached, parts.window);
    assert_eq!(layout.window(detached).unwrap().root, parts.detail);
    assert_eq!(layout.window(detached).unwrap().focused, Some(parts.detail));
    assert_eq!(
        layout.leaves(parts.window),
        vec![parts.navigator, parts.list]
    );
    assert_eq!(
        layout.window(parts.window).unwrap().focused,
        Some(parts.list),
        "the window it left falls back to the neighbour"
    );
    assert_focus_is_a_leaf(&layout);
    assert_arena_is_sound(&layout);
}

#[test]
fn detach_refuses_a_tile_that_is_already_its_whole_window() {
    let (mut layout, parts) = three_column();
    let err = layout
        .apply(Verb::Detach {
            target: PaneRef::id(parts.root),
        })
        .unwrap_err();
    assert_eq!(err, LayoutError::AlreadyItsOwnWindow { tile: parts.root });

    let mut single = Layout::new_single_pane(scratch_pane());
    let err = single
        .apply(Verb::Detach {
            target: PaneRef::Focused,
        })
        .unwrap_err();
    assert!(
        matches!(err, LayoutError::AlreadyItsOwnWindow { .. }),
        "{err:?}"
    );
}

#[test]
fn a_detached_pane_moves_back_and_its_empty_window_goes_with_it() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::Detach {
            target: PaneRef::id(parts.detail),
        })
        .unwrap();
    assert_eq!(layout.windows.len(), 2);

    // A tile id resolves across windows, so the move needs no window argument.
    layout
        .apply_in(
            parts.window,
            Verb::MoveTile {
                tile: PaneRef::id(parts.detail),
                target: PaneRef::id(parts.list),
                placement: Placement::Right,
            },
        )
        .unwrap();

    assert_eq!(
        layout.windows.len(),
        1,
        "a window with no tiles never persists"
    );
    assert_eq!(layout.window_of(parts.detail), Some(parts.window));
    assert_eq!(
        layout.leaves(parts.window),
        vec![parts.navigator, parts.list, parts.detail]
    );
    assert_focus_is_a_leaf(&layout);
    assert_arena_is_sound(&layout);
}

#[test]
fn closing_the_last_pane_of_a_second_window_closes_that_window() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::Detach {
            target: PaneRef::id(parts.detail),
        })
        .unwrap();
    let detached = layout.window_of(parts.detail).unwrap();

    layout
        .apply_in(
            detached,
            Verb::Close {
                target: PaneRef::id(parts.detail),
            },
        )
        .unwrap();

    assert_eq!(layout.windows.len(), 1);
    assert!(layout.window(detached).is_none());
    assert!(layout.tile(parts.detail).is_none());
    assert_eq!(
        layout.leaves(parts.window),
        vec![parts.navigator, parts.list]
    );
    assert_arena_is_sound(&layout);
}

#[test]
fn cannot_close_the_last_pane_in_the_whole_layout() {
    let mut layout = Layout::new_single_pane(scratch_pane());
    let first = layout.current_window().unwrap();
    let second_pane = layout.insert_pane(scratch_pane());
    let second = layout.add_window(second_pane);

    // Either window may lose its only pane while the other still has one.
    layout
        .apply_in(
            second,
            Verb::Close {
                target: PaneRef::id(second_pane),
            },
        )
        .unwrap();
    assert_eq!(layout.windows.len(), 1);

    let err = layout
        .apply_in(
            first,
            Verb::Close {
                target: PaneRef::Focused,
            },
        )
        .unwrap_err();
    assert_eq!(err, LayoutError::CannotCloseLastPane);
    assert_eq!(layout.panes().len(), 1);
}

#[test]
fn set_default_channel_moves_what_follow_means_and_refuses_follow_itself() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::SetDefaultChannel {
            window: parts.window,
            channel: ChannelId::number(5),
        })
        .unwrap();
    assert_eq!(
        layout.window(parts.window).unwrap().default_channel,
        ChannelId::number(5)
    );

    layout
        .apply(Verb::BindParam {
            target: PaneRef::role(Role::DETAIL),
            name: DETAIL_PARAM.to_string(),
            source: ParamSource::follow(),
        })
        .unwrap();
    layout
        .apply(Verb::SetChannel {
            target: PaneRef::role(Role::LIST),
            channel: ChannelId::Follow,
        })
        .unwrap();
    let chosen = Uuid::from_u128(5150);
    layout
        .apply(Verb::Select {
            target: PaneRef::role(Role::LIST),
            kind: "publication".to_string(),
            ids: vec![chosen],
        })
        .unwrap();
    assert_eq!(layout.channels.current(5, "publication"), Some(chosen));
    assert_eq!(
        layout.bindings_for(parts.detail).get(DETAIL_PARAM),
        Some(chosen)
    );

    let err = layout
        .apply(Verb::SetDefaultChannel {
            window: parts.window,
            channel: ChannelId::Follow,
        })
        .unwrap_err();
    assert_eq!(
        err,
        LayoutError::InvalidChannel {
            channel: ChannelId::Follow
        }
    );
    assert_eq!(
        layout.window(parts.window).unwrap().default_channel,
        ChannelId::number(5),
        "the refusal changed nothing"
    );

    let err = layout
        .apply(Verb::SetDefaultChannel {
            window: impress_layout::WindowId::new(99),
            channel: ChannelId::ONE,
        })
        .unwrap_err();
    assert!(matches!(err, LayoutError::UnknownWindow { .. }));
}

// ------------------------------------------- the key window (RL-L5, RL-L19)

#[test]
fn after_a_detach_a_bare_close_closes_the_detached_pane() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::Detach {
            target: PaneRef::id(parts.detail),
        })
        .unwrap();
    let detached = layout.window_of(parts.detail).unwrap();
    assert_eq!(
        layout.current_window().unwrap(),
        detached,
        "focus followed the pane, and so did the key window"
    );

    layout
        .apply(Verb::Close {
            target: PaneRef::Focused,
        })
        .unwrap();
    assert!(
        layout.pane(parts.detail).is_none(),
        "the detached pane went"
    );
    assert_eq!(
        layout.leaves(parts.window),
        vec![parts.navigator, parts.list],
        "the main window is untouched"
    );
    assert_eq!(layout.current_window().unwrap(), parts.window);
    assert_arena_is_sound(&layout);
}

#[test]
fn roles_resolve_in_the_key_window_only() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::Detach {
            target: PaneRef::id(parts.detail),
        })
        .unwrap();
    // The detached window holds `detail`; the main window holds the others.
    assert_eq!(layout.pane_with_role(&Role::DETAIL), Some(parts.detail));
    assert_eq!(
        layout.pane_with_role(&Role::NAVIGATOR),
        None,
        "⌃⌘S in the detached window finds no navigator there"
    );

    layout
        .apply(Verb::Focus {
            target: PaneRef::id(parts.list),
        })
        .unwrap();
    assert_eq!(layout.current_window().unwrap(), parts.window);
    assert_eq!(
        layout.pane_with_role(&Role::NAVIGATOR),
        Some(parts.navigator)
    );
    assert_eq!(
        layout.pane_with_role(&Role::DETAIL),
        None,
        "and back in the main window, the detached `detail` is not this window's"
    );
}

#[test]
fn a_single_window_layout_never_writes_a_key_window() {
    let (mut layout, _) = three_column();
    for direction in [Direction::Left, Direction::Right, Direction::Next] {
        layout.apply(Verb::FocusDirection { direction }).unwrap();
    }
    let json = serde_json::to_value(&layout).unwrap();
    assert!(
        json.get("current").is_none(),
        "the wire form of a one-window layout is unchanged: {json}"
    );
}

#[test]
fn a_move_across_windows_that_would_hold_a_role_twice_is_refused() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::Detach {
            target: PaneRef::id(parts.detail),
        })
        .unwrap();
    // A second `list` in the detached window, then try to bring it home.
    let mut spec = scratch_pane();
    spec.role = Some(Role::LIST);
    layout
        .apply(Verb::Split {
            target: PaneRef::id(parts.detail),
            dir: impress_layout::LinearDir::Vertical,
            after: true,
            new: Some(spec),
        })
        .unwrap();
    let second_list = layout
        .window(layout.window_of(parts.detail).unwrap())
        .unwrap()
        .focused
        .unwrap();
    assert_eq!(layout.pane(second_list).unwrap().role, Some(Role::LIST));

    let before = layout.clone();
    let err = layout
        .apply(Verb::MoveTile {
            tile: PaneRef::id(second_list),
            target: PaneRef::id(parts.list),
            placement: Placement::Right,
        })
        .unwrap_err();
    assert!(matches!(err, LayoutError::RoleHeldTwice { .. }), "{err:?}");
    assert_eq!(layout, before);
}

// ------------------------------------------------- the written contract (T6)

/// One pane-reference spelling on every path (review RL-L3, AC-F3): exactly
/// one of `id`, `role`, `direction`, `focused: true`. The retired tagged
/// spelling, an empty reference and two selectors at once are refused.
#[test]
fn a_pane_reference_has_one_spelling_and_names_exactly_one_pane() {
    for (json, expected) in [
        (r#"{"id":7}"#, PaneRef::id(impress_layout::TileId::new(7))),
        (r#"{"role":"detail"}"#, PaneRef::role(Role::DETAIL)),
        (
            r#"{"direction":"left"}"#,
            PaneRef::direction(Direction::Left),
        ),
        (r#"{"direction":"l"}"#, PaneRef::direction(Direction::Right)),
        (r#"{"focused":true}"#, PaneRef::Focused),
    ] {
        let parsed: PaneRef = serde_json::from_str(json).expect(json);
        assert_eq!(parsed, expected, "{json}");
    }
    assert_eq!(
        serde_json::to_string(&PaneRef::id(impress_layout::TileId::new(7))).unwrap(),
        r#"{"id":7}"#
    );
    assert_eq!(
        serde_json::to_string(&PaneRef::Focused).unwrap(),
        r#"{"focused":true}"#
    );
    for bad in [
        r#"{"ref":"id","tile":7}"#,
        r#"{}"#,
        r#"{"focused":false}"#,
        r#"{"id":7,"role":"detail"}"#,
        r#"{"role":"  "}"#,
        r#"{"direction":"sideways"}"#,
    ] {
        assert!(
            serde_json::from_str::<PaneRef>(bad).is_err(),
            "{bad} must be refused"
        );
    }
    let err = serde_json::from_str::<PaneRef>(r#"{"ref":"id","tile":7}"#).unwrap_err();
    assert!(err.to_string().contains("unknown field `ref`"), "{err}");
}

/// A split with no `new` duplicates the target, minus its role and session —
/// the one shape every path sends (review RL-L20).
#[test]
fn a_bare_split_duplicates_the_target_without_its_role() {
    let (mut layout, parts) = three_column();
    let verb: Verb = serde_json::from_str(
        r#"{"verb":"split","target":{"role":"detail"},"dir":"vertical","after":true}"#,
    )
    .expect("a split without `new` parses");
    layout.apply(verb).unwrap();
    let focused = layout.window(parts.window).unwrap().focused.unwrap();
    let copy = layout.pane(focused).unwrap();
    let original = layout.pane(parts.detail).unwrap();
    assert_eq!(copy.view_kind, original.view_kind);
    assert_eq!(copy.query, original.query);
    assert_eq!(copy.role, None, "two panes cannot hold one role");
    assert_eq!(original.role, Some(Role::DETAIL));
}

/// A verb never writes a view kind no host renders (review RL-L12, PH-M7).
#[test]
fn a_verb_naming_an_unknown_view_kind_is_refused_and_changes_nothing() {
    let (mut layout, parts) = three_column();
    let before = layout.clone();
    let holodeck = ViewKindId::from("holodeck".to_string());
    let mut spec = scratch_pane();
    spec.view_kind = holodeck.clone();
    for verb in [
        Verb::SetViewKind {
            target: PaneRef::id(parts.detail),
            view_kind: holodeck.clone(),
        },
        Verb::SetViewKind {
            target: PaneRef::id(parts.detail),
            view_kind: ViewKindId::from("editor".to_string()),
        },
        Verb::SetPane {
            target: PaneRef::id(parts.detail),
            spec: spec.clone(),
        },
        Verb::Split {
            target: PaneRef::id(parts.detail),
            dir: impress_layout::LinearDir::Vertical,
            after: true,
            new: Some(spec.clone()),
        },
    ] {
        let err = layout.apply(verb).unwrap_err();
        assert_eq!(err.code(), "unknown-view-kind");
        assert!(
            err.to_string().contains("notes"),
            "names the vocabulary: {err}"
        );
    }
    assert_eq!(layout, before);
    // Every kind in the vocabulary is accepted.
    for kind in ViewKindId::KNOWN {
        layout
            .apply(Verb::SetViewKind {
                target: PaneRef::id(parts.detail),
                view_kind: kind.clone(),
            })
            .unwrap_or_else(|e| panic!("{kind}: {e}"));
    }
}

/// ⌃⌘S as a verb (review RL-L13): hiding remembers the share, showing puts
/// it back exactly — not the siblings' average — and the decision is taken
/// against the tree under the verb's own lock.
#[test]
fn set_collapsed_hides_and_restores_the_exact_share() {
    let (mut layout, parts) = three_column();
    let share_of = |layout: &Layout, tile| {
        let parent = layout.parent_of(tile).unwrap();
        let c = layout.tile(parent).unwrap().as_container().unwrap();
        c.shares().unwrap()[c.index_of(tile).unwrap()]
    };
    // A narrow navigator, unlike its siblings.
    let parent = layout.parent_of(parts.navigator).unwrap();
    let n = layout.tile(parent).unwrap().as_container().unwrap().len();
    let mut shares = vec![3.0; n];
    shares[0] = 0.7;
    layout
        .apply(Verb::Resize {
            container: parent,
            shares,
        })
        .unwrap();
    let before = share_of(&layout, parts.navigator);

    let toggle = || Verb::SetCollapsed {
        target: PaneRef::role(Role::NAVIGATOR),
        collapsed: None,
    };
    let hidden = layout.apply(toggle()).unwrap();
    assert!(impress_layout::is_hidden(share_of(
        &layout,
        parts.navigator
    )));
    assert_eq!(
        layout.pane(parts.navigator).unwrap().collapsed_share,
        Some(before)
    );
    assert!(!hidden.is_empty());

    // Asking for what already is costs nothing.
    let again = layout
        .apply(Verb::SetCollapsed {
            target: PaneRef::role(Role::NAVIGATOR),
            collapsed: Some(true),
        })
        .unwrap();
    assert!(again.is_empty());

    layout.apply(toggle()).unwrap();
    assert!(
        (share_of(&layout, parts.navigator) - before).abs() < 1e-6,
        "exact, not the average"
    );
    assert_eq!(layout.pane(parts.navigator).unwrap().collapsed_share, None);

    // Undo of the show hides it again, memory and all (the patch carries the
    // pane's spec).
    let mut stacks = impress_layout::UndoStacks::default();
    stacks.apply(&mut layout, toggle()).unwrap();
    stacks.undo_arrangement(&mut layout).unwrap();
    assert!((share_of(&layout, parts.navigator) - before).abs() < 1e-6);
    stacks.redo_arrangement(&mut layout).unwrap();
    assert!(impress_layout::is_hidden(share_of(
        &layout,
        parts.navigator
    )));
    assert_eq!(
        layout.pane(parts.navigator).unwrap().collapsed_share,
        Some(before)
    );

    // Dragged open some other way: the memory goes, so the next collapse
    // remembers the new width.
    let n = layout.tile(parent).unwrap().as_container().unwrap().len();
    layout
        .apply(Verb::Resize {
            container: parent,
            shares: vec![2.0; n],
        })
        .unwrap();
    assert_eq!(layout.pane(parts.navigator).unwrap().collapsed_share, None);
}

#[test]
fn set_collapsed_outside_a_split_is_refused() {
    let mut layout = Layout::new_single_pane(scratch_pane());
    let err = layout
        .apply(Verb::SetCollapsed {
            target: PaneRef::Focused,
            collapsed: None,
        })
        .unwrap_err();
    assert_eq!(err.code(), "not-in-a-split");
}

#[test]
fn open_list_copies_the_detail_kinds_and_sits_beside_the_navigator() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::SetQuery {
            target: PaneRef::id(parts.detail),
            query: PaneQuery {
                kinds: vec!["manuscript".into()],
                ..PaneQuery::default()
            },
        })
        .unwrap();
    layout
        .apply(Verb::Close {
            target: PaneRef::id(parts.list),
        })
        .unwrap();
    layout
        .apply(Verb::OpenList {
            kinds: Some(vec!["publication".into()]),
        })
        .unwrap();

    let list = layout.pane_with_role(&Role::LIST).unwrap();
    let spec = layout.pane(list).unwrap();
    assert_eq!(spec.query.kinds, vec!["manuscript".to_string()]);
    assert!(matches!(spec.query.scope, impress_layout::Scope::All));
    assert_eq!(spec.view_kind, ViewKindId::LIST);
    assert_eq!(spec.role.as_ref(), Some(&Role::LIST));
    assert_eq!(
        layout.step(parts.window, parts.navigator, Direction::Right),
        list,
        "the list opens beside the sidebar, not as a copy of it"
    );
    assert_ne!(spec.query.kinds, vec!["collection".to_string()]);
}

#[test]
fn open_list_without_a_detail_uses_the_fallback_and_not_the_sidebar() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::Close {
            target: PaneRef::id(parts.list),
        })
        .unwrap();
    layout
        .apply(Verb::Close {
            target: PaneRef::role(Role::DETAIL),
        })
        .unwrap();
    layout
        .apply(Verb::OpenList {
            kinds: Some(vec!["figure".into()]),
        })
        .unwrap();
    let spec = layout
        .pane(layout.pane_with_role(&Role::LIST).unwrap())
        .unwrap();
    assert_eq!(spec.query.kinds, vec!["figure".to_string()]);
    assert!(!spec.query.kinds.iter().any(|kind| kind == "collection"));
}

#[test]
fn open_list_with_nothing_to_copy_is_refused() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::Close {
            target: PaneRef::id(parts.list),
        })
        .unwrap();
    layout
        .apply(Verb::Close {
            target: PaneRef::role(Role::DETAIL),
        })
        .unwrap();
    let before = layout.clone();
    let err = layout.apply(Verb::OpenList { kinds: None }).unwrap_err();
    assert_eq!(err.code(), "no-list-kinds");
    assert_eq!(layout, before);
}

#[test]
fn open_list_focuses_a_list_the_window_already_has() {
    let (mut layout, parts) = three_column();
    layout
        .apply(Verb::Focus {
            target: PaneRef::id(parts.detail),
        })
        .unwrap();
    let before_tiles = layout.tiles.len();
    layout.apply(Verb::OpenList { kinds: None }).unwrap();
    assert_eq!(
        layout.window(parts.window).unwrap().focused,
        Some(parts.list)
    );
    assert_eq!(layout.tiles.len(), before_tiles);
}

#[test]
fn open_list_round_trips_as_json() {
    let verb: Verb = serde_json::from_str(r#"{"verb":"open-list"}"#).unwrap();
    assert_eq!(verb, Verb::OpenList { kinds: None });
    let verb: Verb =
        serde_json::from_str(r#"{"verb":"open-list","kinds":["manuscript"]}"#).unwrap();
    assert_eq!(
        verb,
        Verb::OpenList {
            kinds: Some(vec!["manuscript".into()])
        }
    );
}

#[test]
fn set_tab_order_writes_the_strip_and_keeps_the_selected_tab() {
    let (mut layout, parts) = three_column();
    layout.pane_mut(parts.detail).unwrap().view_state = serde_json::json!({"tab": "info"});
    let focused = layout.window(parts.window).unwrap().focused;
    layout
        .apply(Verb::SetTabOrder {
            target: PaneRef::role(Role::DETAIL),
            tabs: vec![
                ViewKindId::PDF,
                ViewKindId::INFO,
                ViewKindId::NOTES,
                ViewKindId::BIBTEX,
            ],
        })
        .unwrap();
    let state = &layout.pane(parts.detail).unwrap().view_state;
    assert_eq!(
        state["tab_order"],
        serde_json::json!(["pdf", "info", "notes", "bibtex"])
    );
    assert_eq!(state["tab"], "info");
    assert_eq!(layout.window(parts.window).unwrap().focused, focused);
    assert_eq!(
        layout.pane(parts.detail).unwrap().query.kinds,
        vec!["publication".to_string()]
    );
}

#[test]
fn set_tab_order_refuses_an_empty_list_a_duplicate_and_an_unknown_kind() {
    let (mut layout, _) = three_column();
    assert_eq!(
        layout
            .apply(Verb::SetTabOrder {
                target: PaneRef::role(Role::DETAIL),
                tabs: vec![],
            })
            .unwrap_err()
            .code(),
        "empty-tab-order"
    );
    assert_eq!(
        layout
            .apply(Verb::SetTabOrder {
                target: PaneRef::role(Role::DETAIL),
                tabs: vec![ViewKindId::INFO, ViewKindId::INFO],
            })
            .unwrap_err()
            .code(),
        "duplicate-tab"
    );
    assert_eq!(
        layout
            .apply(Verb::SetTabOrder {
                target: PaneRef::role(Role::DETAIL),
                tabs: vec!["holodeck".into()],
            })
            .unwrap_err()
            .code(),
        "unknown-view-kind"
    );
}

#[test]
fn set_tab_order_round_trips_as_json() {
    let verb: Verb = serde_json::from_str(
        r#"{"verb":"set-tab-order","target":{"role":"detail"},"tabs":["pdf","notes"]}"#,
    )
    .unwrap();
    assert_eq!(
        verb,
        Verb::SetTabOrder {
            target: PaneRef::role(Role::DETAIL),
            tabs: vec![ViewKindId::PDF, ViewKindId::NOTES],
        }
    );
}
