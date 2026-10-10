//! The undo rings (ADR-0031 D7): arrangement on one ring, exploration on one
//! ring per pane, focus on neither.

use crate::common::{scratch_pane, three_column, without_allocators};
use impress_layout::{
    stack_for, ChannelId, Direction, LayoutError, LinearDir, PaneQuery, PaneRef, ParamSource, Role,
    StackKind, UndoRing, UndoStacks, Verb, ViewKindId,
};
use uuid::Uuid;

#[test]
fn the_classifier_puts_each_verb_on_the_right_ring() {
    assert_eq!(
        stack_for(&Verb::Close {
            target: PaneRef::Focused
        }),
        StackKind::Arrangement
    );
    assert_eq!(stack_for(&Verb::Restore), StackKind::Arrangement);
    assert_eq!(
        stack_for(&Verb::Detach {
            target: PaneRef::Focused
        }),
        StackKind::Arrangement
    );
    assert_eq!(
        stack_for(&Verb::SetDefaultChannel {
            window: impress_layout::WindowId::new(1),
            channel: ChannelId::ONE
        }),
        StackKind::Arrangement
    );
    assert_eq!(
        stack_for(&Verb::SetRole {
            target: PaneRef::Focused,
            role: None
        }),
        StackKind::Arrangement
    );
    assert_eq!(
        stack_for(&Verb::SetQuery {
            target: PaneRef::role(Role::LIST),
            query: Default::default()
        }),
        StackKind::Exploration(PaneRef::role(Role::LIST))
    );
    assert_eq!(
        stack_for(&Verb::Select {
            target: PaneRef::Focused,
            kind: "publication".into(),
            ids: vec![]
        }),
        StackKind::Exploration(PaneRef::Focused)
    );
    assert_eq!(
        stack_for(&Verb::Focus {
            target: PaneRef::Focused
        }),
        StackKind::None
    );
    assert_eq!(
        stack_for(&Verb::FocusDirection {
            direction: Direction::Right
        }),
        StackKind::None
    );
    assert_eq!(
        stack_for(&Verb::OpenList { kinds: None }),
        StackKind::Arrangement
    );
    assert_eq!(
        stack_for(&Verb::SetTabOrder {
            target: PaneRef::role(Role::DETAIL),
            tabs: vec![ViewKindId::INFO],
        }),
        StackKind::Exploration(PaneRef::role(Role::DETAIL))
    );
}

#[test]
fn opening_a_missing_list_is_one_arrangement_step_and_focusing_one_is_not() {
    let (mut layout, parts) = three_column();
    let mut stacks = UndoStacks::default();
    stacks
        .apply(
            &mut layout,
            Verb::Close {
                target: PaneRef::id(parts.list),
            },
        )
        .unwrap();
    stacks
        .apply(
            &mut layout,
            Verb::OpenList {
                kinds: Some(vec!["publication".into()]),
            },
        )
        .unwrap();
    assert_eq!(stacks.arrangement.done.len(), 2);
    assert!(layout.pane_with_role(&Role::LIST).is_some());
    stacks.undo_arrangement(&mut layout).unwrap();
    assert!(layout.pane_with_role(&Role::LIST).is_none());

    stacks
        .apply(
            &mut layout,
            Verb::Focus {
                target: PaneRef::id(parts.detail),
            },
        )
        .unwrap();
    // The list is gone, so put it back, then focus the detail and open-list
    // again: the second open only moves focus and must not grow the ring.
    stacks
        .apply(
            &mut layout,
            Verb::OpenList {
                kinds: Some(vec!["publication".into()]),
            },
        )
        .unwrap();
    let recorded = stacks.arrangement.done.len();
    stacks
        .apply(
            &mut layout,
            Verb::Focus {
                target: PaneRef::id(parts.detail),
            },
        )
        .unwrap();
    stacks
        .apply(&mut layout, Verb::OpenList { kinds: None })
        .unwrap();
    assert_eq!(stacks.arrangement.done.len(), recorded);
    assert_eq!(
        layout.window(parts.window).unwrap().focused,
        layout.pane_with_role(&Role::LIST)
    );
}

#[test]
fn reordering_tabs_lands_on_the_detail_exploration_ring() {
    let (mut layout, parts) = three_column();
    let mut stacks = UndoStacks::default();
    let original = layout.clone();
    stacks
        .apply(
            &mut layout,
            Verb::SetTabOrder {
                target: PaneRef::id(parts.detail),
                tabs: vec![ViewKindId::NOTES, ViewKindId::INFO],
            },
        )
        .unwrap();
    assert!(stacks.arrangement.done.is_empty());
    assert_eq!(stacks.exploration[&parts.detail].done.len(), 1);
    stacks.undo_exploration(&mut layout, parts.detail).unwrap();
    assert_eq!(without_allocators(&layout), without_allocators(&original));
}

#[test]
fn arrangement_undo_and_redo_restore_the_tree_exactly() {
    let (mut layout, parts) = three_column();
    let mut stacks = UndoStacks::default();
    let original = layout.clone();

    stacks
        .apply(
            &mut layout,
            Verb::Split {
                target: PaneRef::id(parts.detail),
                dir: LinearDir::Vertical,
                after: true,
                new: Some(scratch_pane()),
            },
        )
        .unwrap();
    stacks
        .apply(
            &mut layout,
            Verb::Close {
                target: PaneRef::id(parts.navigator),
            },
        )
        .unwrap();
    let explored = layout.clone();
    assert_eq!(stacks.arrangement.done.len(), 2);

    stacks.undo_arrangement(&mut layout).unwrap();
    stacks.undo_arrangement(&mut layout).unwrap();
    assert_eq!(
        without_allocators(&layout),
        without_allocators(&original),
        "two undos got back to where we started"
    );
    assert!(
        layout.next_tile > original.next_tile,
        "the split's id is not handed out again"
    );
    assert!(stacks.undo_arrangement(&mut layout).unwrap().is_none());

    stacks.arrangement.redo(&mut layout).unwrap();
    stacks.arrangement.redo(&mut layout).unwrap();
    assert_eq!(layout, explored, "two redos got back to where we were");
}

#[test]
fn undoing_a_detach_puts_the_window_back_together() {
    let (mut layout, parts) = three_column();
    let mut stacks = UndoStacks::default();
    let original = layout.clone();

    stacks
        .apply(
            &mut layout,
            Verb::Detach {
                target: PaneRef::id(parts.detail),
            },
        )
        .unwrap();
    assert_eq!(layout.windows.len(), 2);

    stacks.undo_arrangement(&mut layout).unwrap();
    assert_eq!(
        without_allocators(&layout),
        without_allocators(&original),
        "the second window and the split are both back"
    );
}

#[test]
fn exploration_rings_are_per_pane() {
    let (mut layout, parts) = three_column();
    let mut stacks = UndoStacks::default();

    stacks
        .apply(
            &mut layout,
            Verb::SetViewKind {
                target: PaneRef::id(parts.list),
                view_kind: ViewKindId::PLOT,
            },
        )
        .unwrap();
    stacks
        .apply(
            &mut layout,
            Verb::SetViewKind {
                target: PaneRef::id(parts.detail),
                view_kind: ViewKindId::PDF,
            },
        )
        .unwrap();

    assert_eq!(stacks.exploration.len(), 2);
    assert!(stacks.arrangement.done.is_empty());

    // Undoing in the list pane leaves the detail pane's exploration alone.
    stacks.undo_exploration(&mut layout, parts.list).unwrap();
    assert_eq!(layout.pane(parts.list).unwrap().view_kind, ViewKindId::LIST);
    assert_eq!(
        layout.pane(parts.detail).unwrap().view_kind,
        ViewKindId::PDF
    );
}

#[test]
fn selection_is_exploration_and_undoing_it_restores_the_channel() {
    let (mut layout, parts) = three_column();
    let mut stacks = UndoStacks::default();
    let chosen = Uuid::from_u128(3);

    stacks
        .apply(
            &mut layout,
            Verb::Select {
                target: PaneRef::id(parts.list),
                kind: "publication".to_string(),
                ids: vec![chosen],
            },
        )
        .unwrap();
    assert_eq!(layout.channels.current(1, "publication"), Some(chosen));

    stacks.undo_exploration(&mut layout, parts.list).unwrap();
    assert_eq!(layout.channels.current(1, "publication"), None);
    assert_eq!(layout.bindings_for(parts.detail).get("item"), None);
}

#[test]
fn focus_verbs_are_not_recorded() {
    let (mut layout, parts) = three_column();
    let mut stacks = UndoStacks::default();
    stacks
        .apply(
            &mut layout,
            Verb::Focus {
                target: PaneRef::id(parts.navigator),
            },
        )
        .unwrap();
    stacks
        .apply(
            &mut layout,
            Verb::FocusDirection {
                direction: Direction::Right,
            },
        )
        .unwrap();
    assert!(stacks.arrangement.done.is_empty());
    assert!(stacks.exploration.is_empty());
}

#[test]
fn a_no_op_verb_does_not_cost_a_step() {
    let (mut layout, _) = three_column();
    let mut stacks = UndoStacks::default();
    stacks.apply(&mut layout, Verb::Restore).unwrap();
    assert!(stacks.arrangement.done.is_empty());
}

#[test]
fn a_ring_forgets_its_oldest_patch_at_capacity() {
    let (mut layout, parts) = three_column();
    let mut ring = UndoRing::new(3);
    for n in 1..=5u8 {
        let patch = layout
            .apply(Verb::SetChannel {
                target: PaneRef::id(parts.list),
                channel: ChannelId::number(n),
            })
            .unwrap();
        ring.push(patch);
    }
    assert_eq!(ring.done.len(), 3);
    // Undoing everything the ring still holds walks back three steps only.
    while ring.undo(&mut layout).unwrap().is_some() {}
    assert_eq!(
        layout.pane(parts.list).unwrap().channel,
        ChannelId::number(2),
        "the ring remembered three steps, so channel 2 is as far back as it goes"
    );
}

#[test]
fn undoing_a_close_brings_the_pane_and_its_ring_back() {
    let (mut layout, parts) = three_column();
    let mut stacks = UndoStacks::default();

    stacks
        .apply(
            &mut layout,
            Verb::BindParam {
                target: PaneRef::id(parts.detail),
                name: "item".to_string(),
                source: ParamSource::Fixed {
                    item: Uuid::from_u128(11),
                },
            },
        )
        .unwrap();
    stacks
        .apply(
            &mut layout,
            Verb::Close {
                target: PaneRef::id(parts.detail),
            },
        )
        .unwrap();
    assert!(layout.pane(parts.detail).is_none());
    // The pane's exploration ring is kept: undoing the close must bring the
    // pane back with its history, which is why forgetting is explicit.
    assert!(stacks.exploration.contains_key(&parts.detail));

    stacks.undo_arrangement(&mut layout).unwrap();
    assert!(layout.pane(parts.detail).is_some());
    stacks.undo_exploration(&mut layout, parts.detail).unwrap();
    assert_eq!(
        layout
            .pane(parts.detail)
            .unwrap()
            .param("item")
            .unwrap()
            .source,
        ParamSource::Channel {
            channel: ChannelId::ONE
        }
    );
}

#[test]
fn forgetting_closed_panes_is_explicit() {
    let (mut layout, parts) = three_column();
    let mut stacks = UndoStacks::default();
    stacks
        .apply(
            &mut layout,
            Verb::SetViewKind {
                target: PaneRef::id(parts.detail),
                view_kind: ViewKindId::PDF,
            },
        )
        .unwrap();
    stacks
        .apply(
            &mut layout,
            Verb::Close {
                target: PaneRef::id(parts.detail),
            },
        )
        .unwrap();
    stacks.forget_closed_panes(&layout);
    assert!(!stacks.exploration.contains_key(&parts.detail));
}

// ------------------------------------------------ out-of-order undo (RL-L4)

fn select(
    stacks: &mut UndoStacks,
    layout: &mut impress_layout::Layout,
    pane: impress_layout::TileId,
    kind: &str,
    id: u128,
) {
    stacks
        .apply(
            layout,
            Verb::Select {
                target: PaneRef::id(pane),
                kind: kind.to_string(),
                ids: vec![Uuid::from_u128(id)],
            },
        )
        .unwrap();
}

#[test]
fn undoing_one_panes_selection_leaves_another_panes_later_selection_alone() {
    let (mut layout, parts) = three_column();
    let mut stacks = UndoStacks::default();
    // The navigator publishes on channel 2, so its selection is another
    // channel's entry entirely.
    stacks
        .apply(
            &mut layout,
            Verb::SetChannel {
                target: PaneRef::id(parts.navigator),
                channel: ChannelId::number(2),
            },
        )
        .unwrap();

    select(&mut stacks, &mut layout, parts.list, "publication", 1);
    select(&mut stacks, &mut layout, parts.navigator, "collection", 2);

    // ⌘Z in the list: before, the list's patch held the WHOLE channel state,
    // so this also wiped the navigator's later selection on channel 2.
    let undone = stacks.undo_exploration(&mut layout, parts.list).unwrap();
    assert!(undone.is_some());
    assert_eq!(layout.channels.current(1, "publication"), None);
    assert_eq!(
        layout.channels.current(2, "collection"),
        Some(Uuid::from_u128(2)),
        "the navigator's selection, made after, survives"
    );
}

#[test]
fn undoing_a_selection_someone_else_has_since_overwritten_is_refused_and_dropped() {
    let (mut layout, parts) = three_column();
    let mut stacks = UndoStacks::default();
    select(&mut stacks, &mut layout, parts.list, "publication", 1);
    // The detail pane publishes on the SAME channel and kind afterwards.
    select(&mut stacks, &mut layout, parts.detail, "publication", 2);
    let before = layout.clone();

    let refused = stacks.undo_exploration(&mut layout, parts.list);
    assert!(
        matches!(refused, Err(LayoutError::UndoConflict { .. })),
        "{refused:?}"
    );
    assert_eq!(layout, before, "a refused step changes nothing");
    assert!(
        stacks.exploration[&parts.list].done.is_empty(),
        "and it is dropped, so the next ⌘Z does not refuse on it again"
    );
    assert_eq!(
        layout.channels.current(1, "publication"),
        Some(Uuid::from_u128(2))
    );
}

#[test]
fn undoing_a_query_does_not_bring_back_a_role_another_verb_moved() {
    // The review's case: set-query on P, then set-role moves `list` from P
    // to Q on the arrangement ring, then ⌘Z in P. The whole-spec replay
    // restored P's old spec — role and all — and two panes held `list`.
    let (mut layout, parts) = three_column();
    let mut stacks = UndoStacks::default();
    let original_query = layout.pane(parts.list).unwrap().query.clone();
    stacks
        .apply(
            &mut layout,
            Verb::SetQuery {
                target: PaneRef::id(parts.list),
                query: PaneQuery {
                    kinds: vec!["manuscript".to_string()],
                    ..PaneQuery::default()
                },
            },
        )
        .unwrap();
    stacks
        .apply(
            &mut layout,
            Verb::SetRole {
                target: PaneRef::id(parts.detail),
                role: Some(Role::LIST),
            },
        )
        .unwrap();

    stacks.undo_exploration(&mut layout, parts.list).unwrap();
    assert_eq!(
        layout.pane(parts.list).unwrap().query,
        original_query,
        "the query this step changed is back"
    );
    assert_eq!(
        layout.pane(parts.list).unwrap().role,
        None,
        "the role is not"
    );
    assert_eq!(layout.pane_with_role(&Role::LIST), Some(parts.detail));
}

#[test]
fn a_step_that_would_hand_out_a_role_twice_is_refused() {
    let (mut layout, parts) = three_column();
    let mut stacks = UndoStacks::default();
    // Exploration: P's spec replaced by one with no role (set-pane is
    // exploration and carries the role field).
    let mut bare = layout.pane(parts.list).unwrap().clone();
    bare.role = None;
    stacks
        .apply(
            &mut layout,
            Verb::SetPane {
                target: PaneRef::id(parts.list),
                spec: bare,
            },
        )
        .unwrap();
    // Arrangement: the free role goes to the detail pane.
    stacks
        .apply(
            &mut layout,
            Verb::SetRole {
                target: PaneRef::id(parts.detail),
                role: Some(Role::LIST),
            },
        )
        .unwrap();
    let before = layout.clone();

    let refused = stacks.undo_exploration(&mut layout, parts.list);
    assert!(
        matches!(refused, Err(LayoutError::UndoConflict { .. })),
        "{refused:?}"
    );
    assert_eq!(layout, before);
}

// --------------------------------------------- ids are not reused (RL-L8)

#[test]
fn a_new_split_never_inherits_the_ring_of_a_split_that_was_undone() {
    let (mut layout, parts) = three_column();
    let mut stacks = UndoStacks::default();
    let split = |stacks: &mut UndoStacks, layout: &mut impress_layout::Layout| {
        stacks
            .apply(
                layout,
                Verb::Split {
                    target: PaneRef::id(parts.detail),
                    dir: LinearDir::Vertical,
                    after: true,
                    new: Some(scratch_pane()),
                },
            )
            .unwrap();
        layout.window(parts.window).unwrap().focused.unwrap()
    };

    let first = split(&mut stacks, &mut layout);
    stacks
        .apply(
            &mut layout,
            Verb::SetViewKind {
                target: PaneRef::id(first),
                view_kind: ViewKindId::INFO,
            },
        )
        .unwrap();
    stacks.undo_arrangement(&mut layout).unwrap();
    assert!(layout.pane(first).is_none());

    let second = split(&mut stacks, &mut layout);
    assert_ne!(second, first, "the allocator did not roll back");
    assert_eq!(
        stacks.undo_exploration(&mut layout, second).unwrap(),
        None,
        "the new pane has no history of its own, and none of the dead one's"
    );
    assert_eq!(layout.pane(second).unwrap().view_kind, ViewKindId::PDF);
}

#[test]
fn the_ring_of_a_pane_nothing_can_bring_back_is_pruned() {
    let (mut layout, parts) = three_column();
    let mut stacks = UndoStacks::new(1);
    stacks
        .apply(
            &mut layout,
            Verb::SetViewKind {
                target: PaneRef::id(parts.detail),
                view_kind: ViewKindId::PDF,
            },
        )
        .unwrap();
    stacks
        .apply(
            &mut layout,
            Verb::Close {
                target: PaneRef::id(parts.detail),
            },
        )
        .unwrap();
    assert!(
        stacks.exploration.contains_key(&parts.detail),
        "the close is on the arrangement ring, so undoing it can bring the ring back"
    );
    // A capacity-one arrangement ring forgets the close at the next gesture.
    stacks
        .apply(
            &mut layout,
            Verb::SetContainerKind {
                container: parts.root,
                kind: impress_layout::ContainerKind::Vertical,
            },
        )
        .unwrap();
    assert!(!stacks.exploration.contains_key(&parts.detail));
}

/// Several verbs as one gesture (review PH-M2): all or nothing, one ⌘Z.
#[test]
fn a_batch_is_one_undo_step_and_a_refusal_applies_none_of_it() {
    let (mut layout, parts) = three_column();
    let mut stacks = UndoStacks::default();
    let before = layout.clone();
    let publication = impress_layout::RecordKindId::from("publication".to_string());
    let batch = vec![
        Verb::Focus {
            target: PaneRef::id(parts.list),
        },
        Verb::Select {
            target: PaneRef::id(parts.list),
            kind: publication.clone(),
            ids: vec![uuid::Uuid::new_v4()],
        },
        Verb::SetViewKind {
            target: PaneRef::id(parts.detail),
            view_kind: ViewKindId::NOTES,
        },
    ];
    let patch = stacks
        .apply_all(&mut layout, batch)
        .unwrap()
        .expect("a step");
    assert!(
        matches!(patch.verb, Verb::Select { .. }),
        "the first recorded verb labels it"
    );
    assert_eq!(
        layout.pane(parts.detail).unwrap().view_kind,
        ViewKindId::NOTES
    );
    // One step on the list's exploration ring takes back all three.
    let ring = stacks
        .exploration
        .get(&parts.list)
        .expect("the list's ring");
    assert_eq!(ring.done.len(), 1);
    stacks
        .undo_exploration(&mut layout, parts.list)
        .unwrap()
        .expect("undone");
    assert_eq!(
        layout.pane(parts.detail).unwrap().view_kind,
        before.pane(parts.detail).unwrap().view_kind
    );
    assert_eq!(layout.channels, before.channels);

    // A refusal in the middle applies nothing and records nothing.
    let snapshot = layout.clone();
    let refused = stacks.apply_all(
        &mut layout,
        vec![
            Verb::SetViewKind {
                target: PaneRef::id(parts.detail),
                view_kind: ViewKindId::BIBTEX,
            },
            Verb::Close {
                target: PaneRef::id(impress_layout::TileId::new(4242)),
            },
        ],
    );
    assert!(refused.is_err());
    assert_eq!(layout, snapshot);
    assert!(stacks.apply_all(&mut layout, Vec::new()).unwrap().is_none());
}
