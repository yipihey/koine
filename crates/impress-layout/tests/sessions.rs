//! ADR-0031 D6: which pane holds which session. One test per rule in
//! `src/sessions.rs`.

use crate::common::{assert_arena_is_sound, publication_query};
use impress_layout::preset::{self, ThreeColumn};
use impress_layout::{
    Layout, LinearDir, PaneRef, PaneSpec, Placement, Role, SessionId, TileId, UndoStacks, Verb,
    ViewKindId,
};

/// imprint's Default: navigator | list | detail(`source`).
fn editor_layout() -> (Layout, ThreeColumn) {
    let (mut layout, parts) = preset::three_column_parts(publication_query(), ViewKindId::SOURCE);
    layout.ensure_sessions();
    (layout, parts)
}

fn session_of(layout: &Layout, tile: TileId) -> Option<SessionId> {
    layout.pane(tile).and_then(|p| p.session.clone())
}

fn editor_session(layout: &Layout, parts: &ThreeColumn) -> SessionId {
    session_of(layout, parts.detail).expect("the source pane holds a session")
}

/// Every session in the layout, with the panes that hold it — for "no two
/// panes share one".
fn assert_sessions_unique(layout: &Layout) {
    let mut seen = std::collections::HashMap::new();
    for tile in layout.panes() {
        if let Some(session) = session_of(layout, tile) {
            if let Some(other) = seen.insert(session.clone(), tile) {
                panic!("panes {other} and {tile} share session {session}");
            }
        }
    }
    for tile in layout.panes() {
        let spec = layout.pane(tile).unwrap();
        if spec.view_kind.is_session_bearing() {
            assert!(spec.session.is_some(), "source pane {tile} has no session");
        }
    }
}

fn source_pane() -> PaneSpec {
    PaneSpec::new(publication_query(), ViewKindId::SOURCE)
}

fn split(layout: &mut Layout, target: TileId, new: PaneSpec) -> TileId {
    let window = layout.current_window().unwrap();
    layout
        .apply(Verb::Split {
            target: PaneRef::id(target),
            dir: LinearDir::Vertical,
            after: true,
            new: Some(new),
        })
        .unwrap();
    layout.window(window).unwrap().focused.unwrap()
}

// ------------------------------------------------------------ the vocabulary

#[test]
fn source_is_the_one_session_bearing_kind() {
    assert_eq!(ViewKindId::SESSION_BEARING, &[ViewKindId::SOURCE]);
    assert!(ViewKindId::SOURCE.is_session_bearing());
    for kind in [
        ViewKindId::OUTLINE,
        ViewKindId::LIST,
        ViewKindId::INFO,
        ViewKindId::PDF,
        ViewKindId::LEGACY,
    ] {
        assert!(!kind.is_session_bearing(), "{kind} is not session-bearing");
    }
    assert_ne!(SessionId::fresh(), SessionId::fresh());
}

// ------------------------------------------------------- presets carry none

#[test]
fn a_preset_carries_no_sessions_and_normalize_assigns_none() {
    let (mut layout, parts) = preset::three_column_parts(publication_query(), ViewKindId::SOURCE);
    // The shipped value is deterministic: `matches_shipped` compares it.
    assert_eq!(session_of(&layout, parts.detail), None);
    layout.normalize();
    assert_eq!(session_of(&layout, parts.detail), None);
    let again = preset::three_column_parts(publication_query(), ViewKindId::SOURCE).0;
    assert_eq!(layout, again);
}

#[test]
fn ensure_sessions_fills_in_and_is_idempotent() {
    let (mut layout, parts) = preset::three_column_parts(publication_query(), ViewKindId::SOURCE);
    assert!(layout.ensure_sessions(), "the first pass assigns");
    let session = editor_session(&layout, &parts);
    assert!(!layout.ensure_sessions(), "the second pass changes nothing");
    assert_eq!(editor_session(&layout, &parts), session);
    // Only the source pane: the navigator and the list are not editors.
    assert_eq!(session_of(&layout, parts.navigator), None);
    assert_eq!(session_of(&layout, parts.list), None);
}

// ------------------------------------------------------------- every verb

#[test]
fn a_verb_gives_a_session_bearing_pane_its_session() {
    // A tree loaded from a build before sessions: the first verb fixes it.
    let (mut layout, parts) = preset::three_column_parts(publication_query(), ViewKindId::SOURCE);
    layout
        .apply(Verb::Focus {
            target: PaneRef::id(parts.list),
        })
        .unwrap();
    assert!(session_of(&layout, parts.detail).is_some());
    assert_sessions_unique(&layout);
}

#[test]
fn split_gives_the_new_source_pane_a_fresh_session_and_the_target_keeps_its_own() {
    let (mut layout, parts) = editor_layout();
    let editor = editor_session(&layout, &parts);

    // The new spec is a COPY of the target's, session and all — what a
    // caller duplicating "this pane" naturally sends.
    let copy = layout.pane(parts.detail).unwrap().clone();
    assert_eq!(copy.session.as_ref(), Some(&editor));
    let first = split(&mut layout, parts.detail, copy);

    assert_eq!(
        editor_session(&layout, &parts),
        editor,
        "the target keeps it"
    );
    let fresh = session_of(&layout, first).expect("the new source pane holds one");
    assert_ne!(fresh, editor, "and the new pane never shares it");

    // A bare source spec gets its own too, distinct from both.
    let second = split(&mut layout, first, source_pane());
    let third = session_of(&layout, second).unwrap();
    assert_ne!(third, editor);
    assert_ne!(third, fresh);
    assert_eq!(session_of(&layout, first), Some(fresh));
    assert_sessions_unique(&layout);
    assert_arena_is_sound(&layout);
}

#[test]
fn split_into_a_kind_that_is_not_session_bearing_adds_no_session() {
    let (mut layout, parts) = editor_layout();
    let pdf = split(
        &mut layout,
        parts.detail,
        PaneSpec::new(publication_query(), ViewKindId::PDF),
    );
    assert_eq!(session_of(&layout, pdf), None);
}

#[test]
fn swap_move_resize_and_close_keep_every_session() {
    let (mut layout, parts) = editor_layout();
    let editor = editor_session(&layout, &parts);
    let sibling = split(&mut layout, parts.detail, source_pane());
    let sibling_session = session_of(&layout, sibling).unwrap();

    layout
        .apply(Verb::Swap {
            a: PaneRef::id(parts.detail),
            b: PaneRef::id(sibling),
        })
        .unwrap();
    assert_eq!(session_of(&layout, parts.detail), Some(editor.clone()));
    assert_eq!(session_of(&layout, sibling), Some(sibling_session.clone()));

    layout
        .apply(Verb::MoveTile {
            tile: PaneRef::id(parts.detail),
            target: PaneRef::id(parts.navigator),
            placement: Placement::Below,
        })
        .unwrap();
    assert_eq!(session_of(&layout, parts.detail), Some(editor.clone()));

    let root = layout
        .window(layout.current_window().unwrap())
        .unwrap()
        .root;
    let children = layout.tile(root).unwrap().as_container().unwrap().len();
    layout
        .apply(Verb::Resize {
            container: root,
            shares: vec![1.0; children],
        })
        .unwrap();
    assert_eq!(session_of(&layout, parts.detail), Some(editor.clone()));

    layout
        .apply(Verb::Close {
            target: PaneRef::id(sibling),
        })
        .unwrap();
    assert_eq!(session_of(&layout, parts.detail), Some(editor));
    assert_sessions_unique(&layout);
}

#[test]
fn set_view_kind_and_set_pane_keep_the_session() {
    let (mut layout, parts) = editor_layout();
    let editor = editor_session(&layout, &parts);

    // Away to a kind with no editor and back: the same editor returns.
    for kind in [ViewKindId::PDF, ViewKindId::SOURCE] {
        layout
            .apply(Verb::SetViewKind {
                target: PaneRef::id(parts.detail),
                view_kind: kind,
            })
            .unwrap();
        assert_eq!(session_of(&layout, parts.detail), Some(editor.clone()));
    }

    // The outline re-points the detail pane with a spec that names none.
    let mut spec = source_pane().with_role(Role::DETAIL);
    spec.session = None;
    layout
        .apply(Verb::SetPane {
            target: PaneRef::role(Role::DETAIL),
            spec,
        })
        .unwrap();
    assert_eq!(session_of(&layout, parts.detail), Some(editor));
}

#[test]
fn set_pane_cannot_take_another_panes_session() {
    let (mut layout, parts) = editor_layout();
    let editor = editor_session(&layout, &parts);
    let sibling = split(&mut layout, parts.detail, source_pane());
    let sibling_session = session_of(&layout, sibling).unwrap();

    // Point the sibling at the detail pane's session: the holder keeps it.
    let stolen = source_pane().with_session(editor.clone());
    layout
        .apply(Verb::SetPane {
            target: PaneRef::id(sibling),
            spec: stolen,
        })
        .unwrap();
    assert_eq!(session_of(&layout, parts.detail), Some(editor.clone()));
    let now = session_of(&layout, sibling).unwrap();
    assert_ne!(now, editor);
    assert_ne!(now, sibling_session, "a fresh one, not a resurrected one");
    assert_sessions_unique(&layout);
}

#[test]
fn undo_and_redo_of_a_split_restore_the_same_sessions() {
    let (mut layout, parts) = editor_layout();
    let editor = editor_session(&layout, &parts);
    let mut undo = UndoStacks::default();
    undo.apply(
        &mut layout,
        Verb::Split {
            target: PaneRef::id(parts.detail),
            dir: LinearDir::Horizontal,
            after: true,
            new: Some(source_pane()),
        },
    )
    .unwrap();
    let new = layout
        .window(layout.current_window().unwrap())
        .unwrap()
        .focused
        .unwrap();
    let fresh = session_of(&layout, new).unwrap();

    undo.undo_arrangement(&mut layout).unwrap();
    assert!(layout.pane(new).is_none());
    assert_eq!(editor_session(&layout, &parts), editor);

    undo.arrangement.redo(&mut layout).unwrap();
    assert_eq!(
        session_of(&layout, new),
        Some(fresh),
        "redo is the same pane"
    );
    assert_eq!(editor_session(&layout, &parts), editor);
}

// ------------------------------------------------ presets and saved layouts

#[test]
fn reapplying_a_preset_keeps_the_live_session_of_the_pane_in_the_same_role() {
    let (mut live, parts) = editor_layout();
    let editor = editor_session(&live, &parts);
    // The user split the editor: that pane has no role.
    split(&mut live, parts.detail, source_pane());

    // ⌃⌘1: the preset's own tree, which carries no sessions.
    let (mut preset, preset_parts) =
        preset::three_column_parts(publication_query(), ViewKindId::SOURCE);
    preset.adopt_sessions_by_role(&live);

    assert_eq!(
        session_of(&preset, preset_parts.detail),
        Some(editor),
        "the detail role's editor survives the re-application"
    );
    assert_sessions_unique(&preset);
}

#[test]
fn a_saved_layout_keeps_its_own_ids_unless_the_role_claims_one() {
    let (mut live, parts) = editor_layout();
    let editor = editor_session(&live, &parts);
    let side = split(&mut live, parts.detail, source_pane());
    let side_session = session_of(&live, side).unwrap();

    // A layout saved earlier, whose unroled editor pane carries the live
    // detail editor's id (it was the detail pane when the layout was saved):
    // the role wins it, the unroled pane gets a fresh one.
    let (mut saved, saved_parts) =
        preset::three_column_parts(publication_query(), ViewKindId::SOURCE);
    let extra = split(
        &mut saved,
        saved_parts.detail,
        source_pane().with_session(editor.clone()),
    );
    saved.pane_mut(saved_parts.detail).unwrap().session = Some(side_session.clone());

    saved.adopt_sessions_by_role(&live);
    assert_eq!(session_of(&saved, saved_parts.detail), Some(editor.clone()));
    let extra_session = session_of(&saved, extra).unwrap();
    assert_ne!(extra_session, editor);
    assert_sessions_unique(&saved);
}

#[test]
fn a_preset_whose_role_holds_another_kind_adopts_nothing_for_it() {
    let (live, parts) = editor_layout();
    let editor = editor_session(&live, &parts);
    // imbib's Default over the same app: the detail pane is `info`.
    let (mut preset, preset_parts) =
        preset::three_column_parts(publication_query(), ViewKindId::INFO);
    preset.adopt_sessions_by_role(&live);
    assert_eq!(session_of(&preset, preset_parts.detail), None);
    assert!(!preset.session_holders().contains_key(&editor));
}
