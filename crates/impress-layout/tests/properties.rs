//! Property tests over random verb sequences.
//!
//! No proptest: a seeded LCG generates the sequences, so a failure is
//! reproducible from its seed alone and the crate keeps its dependency list
//! to five. The properties are the ones the rest of the stack relies on:
//!
//! 1. a verb either applies or changes nothing;
//! 2. apply-then-revert is the identity (that is what undo *is*), up to the
//!    id allocators, which never move backward (review RL-L8);
//! 3. revert-then-reapply is the identity (that is what redo is);
//! 4. the tree is normalized and the arena sound after every verb;
//! 5. focus is always a pane of its own window;
//! 6. normalization is idempotent at every step;
//! 7. the focused leaf is visible: every `Tabs` ancestor's active child is on
//!    its path.

use crate::common::{
    assert_arena_is_sound, assert_focus_is_a_leaf, assert_focus_is_visible, scratch_pane,
    three_column, without_allocators,
};
use impress_layout::{
    ChannelId, ContainerKind, Direction, Geometry, Layout, LinearDir, PaneRef, PaneSpec,
    ParamSource, Placement, Role, TileId, Verb, ViewKindId,
};
use uuid::Uuid;

/// A linear congruential generator (Knuth's MMIX constants). Deterministic,
/// seedable, and no dependency.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed.wrapping_mul(6364136223846793005).wrapping_add(1))
    }

    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 16
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }

    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len())]
    }

    fn pick_cloned<T: Clone>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len())].clone()
    }

    fn float(&mut self) -> f32 {
        1.0 + (self.below(9) as f32)
    }
}

fn any_tile(rng: &mut Lcg, layout: &Layout) -> TileId {
    let tiles: Vec<TileId> = layout.tiles.keys().copied().collect();
    if tiles.is_empty() {
        TileId::new(1)
    } else {
        rng.pick(&tiles)
    }
}

fn any_ref(rng: &mut Lcg, layout: &Layout) -> PaneRef {
    match rng.below(4) {
        0 => PaneRef::id(any_tile(rng, layout)),
        1 => PaneRef::role(rng.pick_cloned(&[
            Role::NAVIGATOR,
            Role::LIST,
            Role::DETAIL,
            Role::PREVIEW,
        ])),
        2 => PaneRef::Focused,
        _ => PaneRef::direction(rng.pick(&[
            Direction::Left,
            Direction::Right,
            Direction::Up,
            Direction::Down,
            Direction::Next,
            Direction::Prev,
        ])),
    }
}

fn any_verb(rng: &mut Lcg, layout: &Layout) -> Verb {
    let dirs = [LinearDir::Horizontal, LinearDir::Vertical];
    let placements = [
        Placement::Left,
        Placement::Right,
        Placement::Above,
        Placement::Below,
        Placement::IntoTabs,
    ];
    let kinds = [
        ContainerKind::Tabs,
        ContainerKind::Horizontal,
        ContainerKind::Vertical,
        ContainerKind::Grid,
    ];
    match rng.below(21) {
        0 => Verb::Split {
            target: any_ref(rng, layout),
            dir: rng.pick(&dirs),
            after: rng.below(2) == 0,
            new: Some(scratch_pane()),
        },
        1 => Verb::MoveTile {
            tile: any_ref(rng, layout),
            target: any_ref(rng, layout),
            placement: rng.pick(&placements),
        },
        2 => Verb::Close {
            target: any_ref(rng, layout),
        },
        3 => Verb::Swap {
            a: any_ref(rng, layout),
            b: any_ref(rng, layout),
        },
        4 => {
            let container = any_tile(rng, layout);
            let arity = layout
                .tile(container)
                .and_then(|t| t.as_container())
                .map(|c| c.len())
                .unwrap_or(2);
            Verb::Resize {
                container,
                shares: (0..arity).map(|_| rng.float()).collect(),
            }
        }
        5 => Verb::SetContainerKind {
            container: any_tile(rng, layout),
            kind: rng.pick(&kinds),
        },
        6 => Verb::Maximize {
            target: any_ref(rng, layout),
        },
        7 => Verb::Restore,
        8 => Verb::SetPane {
            target: any_ref(rng, layout),
            spec: PaneSpec::new(Default::default(), ViewKindId::PLOT),
        },
        9 => Verb::SetQuery {
            target: any_ref(rng, layout),
            query: Default::default(),
        },
        10 => Verb::SetViewKind {
            target: any_ref(rng, layout),
            view_kind: rng.pick_cloned(&[ViewKindId::INFO, ViewKindId::PDF, ViewKindId::SOURCE]),
        },
        11 => Verb::BindParam {
            target: any_ref(rng, layout),
            name: rng.pick(&["item", "manuscript"]).to_string(),
            source: match rng.below(3) {
                0 => ParamSource::Default,
                1 => ParamSource::Fixed {
                    item: Uuid::from_u128(rng.next() as u128),
                },
                _ => ParamSource::channel(rng.below(9) as u8),
            },
        },
        12 => Verb::SetChannel {
            target: any_ref(rng, layout),
            channel: if rng.below(4) == 0 {
                ChannelId::Follow
            } else {
                ChannelId::number(rng.below(9) as u8)
            },
        },
        13 => Verb::SetRole {
            target: any_ref(rng, layout),
            role: match rng.below(4) {
                0 => None,
                1 => Some(Role::NAVIGATOR),
                2 => Some(Role::LIST),
                _ => Some(Role::DETAIL),
            },
        },
        14 => Verb::Focus {
            target: any_ref(rng, layout),
        },
        15 => Verb::FocusDirection {
            direction: rng.pick(&[
                Direction::Left,
                Direction::Right,
                Direction::Up,
                Direction::Down,
                Direction::Next,
                Direction::Prev,
            ]),
        },
        16 => Verb::Detach {
            target: any_ref(rng, layout),
        },
        17 => {
            let window = layout.windows[rng.below(layout.windows.len())].id;
            Verb::SetDefaultChannel {
                window,
                channel: if rng.below(4) == 0 {
                    ChannelId::Follow
                } else {
                    ChannelId::number(rng.below(9) as u8)
                },
            }
        }
        18 => Verb::OpenList {
            kinds: if rng.below(2) == 0 {
                None
            } else {
                Some(vec!["publication".into()])
            },
        },
        19 => Verb::SetTabOrder {
            target: any_ref(rng, layout),
            tabs: vec![ViewKindId::INFO, ViewKindId::PDF, ViewKindId::NOTES],
        },
        _ => {
            if rng.below(2) == 0 {
                Verb::Select {
                    target: any_ref(rng, layout),
                    kind: rng.pick(&["publication", "manuscript"]).to_string(),
                    ids: (0..rng.below(3))
                        .map(|_| Uuid::from_u128(rng.next() as u128))
                        .collect(),
                }
            } else {
                let window = layout.windows[rng.below(layout.windows.len())].id;
                Verb::SetWindowGeometry {
                    window,
                    geometry: Some(Geometry {
                        x: rng.below(100) as f64,
                        y: rng.below(100) as f64,
                        w: 800.0,
                        h: 600.0,
                        display: None,
                    }),
                }
            }
        }
    }
}

#[test]
fn random_verb_sequences_hold_every_invariant() {
    let mut applied = 0usize;
    let mut refused = 0usize;
    for seed in 0..400u64 {
        let (mut layout, _) = three_column();
        let mut rng = Lcg::new(seed);
        for step in 0..24 {
            let verb = any_verb(&mut rng, &layout);
            let before = layout.clone();
            match layout.apply(verb.clone()) {
                Err(_) => {
                    refused += 1;
                    assert_eq!(
                        layout, before,
                        "seed {seed} step {step}: a refused {verb:?} changed the layout"
                    );
                }
                Ok(patch) => {
                    applied += 1;

                    // (4), (5) and (7): the shape invariants.
                    assert_arena_is_sound(&layout);
                    assert_focus_is_a_leaf(&layout);
                    assert_focus_is_visible(&layout);

                    // (6): normalization reached a fixed point.
                    let mut again = layout.clone();
                    again.normalize();
                    assert_eq!(
                        again, layout,
                        "seed {seed} step {step}: {verb:?} left the tree un-normalized"
                    );

                    // (2): apply-then-revert is the identity, up to the
                    // allocators — and they never move backward.
                    let mut reverted = layout.clone();
                    reverted.revert(&patch);
                    assert!(reverted.next_tile >= layout.next_tile);
                    assert!(reverted.next_window >= layout.next_window);
                    assert_eq!(
                        without_allocators(&reverted),
                        without_allocators(&before),
                        "seed {seed} step {step}: reverting {verb:?} did not restore the layout"
                    );

                    // (3): revert-then-reapply is the identity.
                    let after = layout.clone();
                    reverted.reapply(&patch);
                    assert_eq!(
                        reverted, after,
                        "seed {seed} step {step}: redoing {verb:?} did not restore the layout"
                    );

                    // The checked step a ring takes agrees with the primitive
                    // when nothing has happened since: it must never refuse
                    // its own most recent patch.
                    let mut stepped = layout.clone();
                    stepped.undo_step(&patch).unwrap_or_else(|e| {
                        panic!("seed {seed} step {step}: undo_step refused {verb:?}: {e}")
                    });
                    assert_eq!(
                        without_allocators(&stepped),
                        without_allocators(&before),
                        "seed {seed} step {step}: undo_step of {verb:?} did not restore the layout"
                    );
                    stepped.redo_step(&patch).unwrap_or_else(|e| {
                        panic!("seed {seed} step {step}: redo_step refused {verb:?}: {e}")
                    });
                    assert_eq!(
                        stepped, after,
                        "seed {seed} step {step}: redo_step of {verb:?} did not restore the layout"
                    );
                }
            }
        }
    }
    // A generator that only ever produced refusals would pass vacuously.
    assert!(
        applied > 5_000,
        "only {applied} verbs applied ({refused} refused): the generator is not exercising much"
    );
}

#[test]
fn a_layout_survives_a_json_round_trip_at_every_step() {
    for seed in 400..440u64 {
        let (mut layout, _) = three_column();
        let mut rng = Lcg::new(seed);
        for _ in 0..12 {
            let verb = any_verb(&mut rng, &layout);
            let _ = layout.apply(verb);
            let json = serde_json::to_string(&layout).unwrap();
            let back: Layout = serde_json::from_str(&json).unwrap();
            assert_eq!(back, layout, "seed {seed}: the wire format lost something");
        }
    }
}

#[test]
fn normalization_never_loses_a_pane() {
    for seed in 440..470u64 {
        let (mut layout, _) = three_column();
        let mut rng = Lcg::new(seed);
        for _ in 0..12 {
            let verb = any_verb(&mut rng, &layout);
            let _ = layout.apply(verb);
            let leaves = layout.panes();
            let mut again = layout.clone();
            again.normalize();
            assert_eq!(again.panes(), leaves, "normalization moved or lost a pane");
        }
    }
}

/// The undo rings, interleaved (review RL-L4): random verbs land on the
/// arrangement ring and on per-pane exploration rings, and random undo/redo
/// steps are taken on any of them, out of order. Every step either replays
/// cleanly or is refused and changes nothing; the tree stays sound and
/// normalized; no step hands a role out twice; and an exploration step on
/// one pane never changes another pane.
#[test]
fn interleaved_undo_and_redo_across_rings_keep_every_invariant() {
    use impress_layout::{LayoutError, UndoStacks};

    let mut stepped = 0usize;
    let mut refused_steps = 0usize;
    for seed in 1000..1300u64 {
        let (mut layout, _) = three_column();
        let mut stacks = UndoStacks::default();
        let mut rng = Lcg::new(seed);
        for step in 0..40 {
            let before = layout.clone();
            let roll = rng.below(10);
            if roll < 6 {
                let verb = any_verb(&mut rng, &layout);
                if stacks.apply(&mut layout, verb.clone()).is_err() {
                    assert_eq!(
                        layout, before,
                        "seed {seed} step {step}: refused {verb:?} changed the layout"
                    );
                    continue;
                }
            } else {
                let undo = roll < 9;
                let rings: Vec<TileId> = stacks.exploration.keys().copied().collect();
                let pane = if rng.below(2) == 0 || rings.is_empty() {
                    None
                } else {
                    Some(rng.pick(&rings))
                };
                let outcome = match (pane, undo) {
                    (None, true) => stacks.undo_arrangement(&mut layout),
                    (None, false) => stacks.redo_arrangement(&mut layout),
                    (Some(tile), true) => stacks.undo_exploration(&mut layout, tile),
                    (Some(tile), false) => stacks.redo_exploration(&mut layout, tile),
                };
                match outcome {
                    Err(LayoutError::UndoConflict { .. }) => {
                        refused_steps += 1;
                        assert_eq!(
                            layout, before,
                            "seed {seed} step {step}: a refused step changed the layout"
                        );
                        continue;
                    }
                    Err(other) => panic!("seed {seed} step {step}: unexpected {other}"),
                    Ok(None) => continue,
                    Ok(Some(patch)) => {
                        stepped += 1;
                        if let Some(tile) = pane {
                            // No cross-pane effect: every OTHER pane is as it was.
                            for other in before.panes() {
                                if other != tile && !patch.tiles.contains_key(&other) {
                                    assert_eq!(
                                        layout.pane(other),
                                        before.pane(other),
                                        "seed {seed} step {step}: an exploration step on pane {tile} changed pane {other}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
            assert_arena_is_sound(&layout);
            assert_focus_is_a_leaf(&layout);
            assert_focus_is_visible(&layout);
            let mut again = layout.clone();
            again.normalize();
            assert_eq!(
                again, layout,
                "seed {seed} step {step}: left the tree un-normalized"
            );
            assert_eq!(
                layout.new_duplicate_role(&before),
                None,
                "seed {seed} step {step}: a role is now held twice in one window"
            );
        }
    }
    assert!(
        stepped > 1_000,
        "only {stepped} undo/redo steps replayed ({refused_steps} refused): not exercising much"
    );
}
