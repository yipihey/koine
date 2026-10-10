//! Shared fixtures for the integration tests.

#![allow(dead_code)]

use impress_layout::preset::{self, ThreeColumn};
use impress_layout::{Container, Layout, PaneQuery, PaneSpec, ViewKindId};

/// The query imbib's publication list issues.
pub fn publication_query() -> PaneQuery {
    PaneQuery {
        kinds: vec!["publication".to_string()],
        ..PaneQuery::default()
    }
}

/// Today's chassis: navigator | list | detail.
pub fn three_column() -> (Layout, ThreeColumn) {
    preset::three_column_parts(publication_query(), ViewKindId::INFO)
}

/// A throwaway pane to split into.
pub fn scratch_pane() -> PaneSpec {
    PaneSpec::new(publication_query(), ViewKindId::PDF)
}

/// The layout with its id allocators zeroed: what an undo restores. Ids are
/// never reused, so undo leaves the allocators where they were (review
/// RL-L8) and a test comparing trees across an undo compares these.
pub fn without_allocators(layout: &Layout) -> Layout {
    let mut layout = layout.clone();
    layout.next_tile = 0;
    layout.next_window = 0;
    layout
}

/// Every window's focus is a pane of that window. The invariant every verb
/// must leave standing.
pub fn assert_focus_is_a_leaf(layout: &Layout) {
    for window in &layout.windows {
        let leaves = layout.leaves(window.id);
        match window.focused {
            Some(focused) => {
                assert!(
                    leaves.contains(&focused),
                    "window {} focuses {focused}, which is not one of its panes {leaves:?}",
                    window.id
                );
                assert!(
                    layout.pane(focused).is_some(),
                    "window {} focuses {focused}, which is not a pane",
                    window.id
                );
            }
            None => assert!(
                leaves.is_empty(),
                "window {} has panes {leaves:?} but no focus",
                window.id
            ),
        }
    }
}

/// The focused pane is *visible*: every `Tabs` ancestor of the focused leaf
/// shows the child that leads down to it. Focus behind an inactive tab is
/// focus the user cannot see.
pub fn assert_focus_is_visible(layout: &Layout) {
    for window in &layout.windows {
        let Some(focused) = window.focused else {
            continue;
        };
        let mut cursor = focused;
        for _ in 0..64 {
            let Some(parent) = layout.parent_of(cursor) else {
                break;
            };
            if let Some(Container::Tabs { active, .. }) =
                layout.tile(parent).and_then(|t| t.as_container())
            {
                assert_eq!(
                    *active,
                    Some(cursor),
                    "window {} focuses {focused}, hidden behind tab strip {parent}",
                    window.id
                );
            }
            cursor = parent;
        }
    }
}

/// No tile the windows cannot reach; no child pointing at a missing tile.
pub fn assert_arena_is_sound(layout: &Layout) {
    let mut reachable = std::collections::BTreeSet::new();
    for window in &layout.windows {
        let mut stack = vec![window.root];
        while let Some(tile) = stack.pop() {
            assert!(
                layout.tile(tile).is_some(),
                "tile {tile} is referenced but missing"
            );
            if !reachable.insert(tile) {
                panic!("tile {tile} is reachable twice: the tree is not a tree");
            }
            if let Some(container) = layout.tile(tile).and_then(|t| t.as_container()) {
                stack.extend(container.children().iter().copied());
            }
        }
    }
    let arena: std::collections::BTreeSet<_> = layout.tiles.keys().copied().collect();
    assert_eq!(arena, reachable, "the arena holds unreachable tiles");

    // Window ids are allocated like tile ids: unique, and never reused after a
    // window closes, so a WindowId in an operation log means one window.
    let mut ids = std::collections::BTreeSet::new();
    for window in &layout.windows {
        assert!(
            ids.insert(window.id),
            "two windows share the id {}",
            window.id
        );
        assert!(
            window.id.raw() < layout.next_window,
            "window {} is at or above the allocator ({}), so its id can be handed out again",
            window.id,
            layout.next_window
        );
    }
}
