//! Hiding a pane: the weights that mean "toggled off".
//!
//! `PaneLayoutState`'s three Booleans (`sidebarVisible`, `listPaneVisible`,
//! `detailPaneVisible`) become shares in the tree, and the obvious encoding of
//! "hidden" — a share of `0.0` — **is not representable**:
//!
//! * [`crate::Verb::Resize`] refuses a share that is not a positive, finite
//!   weight ([`crate::LayoutError::InvalidShares`]), and
//! * [`crate::Layout::normalize`] repairs shares through `sane_share`, which
//!   rewrites any non-positive value to `1.0` — so a `0.0` written straight
//!   into the arena does not survive one normalization, and the pane that was
//!   meant to be hidden springs back to a full column.
//!
//! Both are checked in this module's tests rather than asserted about.
//! [`HIDDEN_SHARE`] is therefore the smallest representable alternative: a
//! hidden pane is one with almost no width, and it is **still in the tree** —
//! it keeps its query, its role and its session, so ⌘0 / ⌥⌘0 / ⌃⌘S are a
//! [`crate::Verb::Resize`] back rather than a close and a re-split. A toggle
//! must not cost the user the pane's state.
//!
//! This is one constant for the whole suite, and it lives here because
//! `impress-layout` is what both the service (which ships presets with a
//! hidden pane) and the FFI layer (whose `resize_share` is what the role
//! toggles call) already depend on. Two copies of a threshold are two answers
//! to "is this pane hidden?", and the renderer only gets to pick one.

/// The share a *hidden* pane carries.
///
/// `1e-4` rather than [`f32::MIN_POSITIVE`]: normalization joins a linear
/// container into its parent by `share * child / total`, and a denormal there
/// flushes to zero, which `sane_share` then turns back into a full column.
/// `1e-4` stays normal through several joins, and against any realistic sum
/// of sibling weights it is 0.1 px of a 3000 px window — zero to the eye,
/// positive to the arithmetic.
pub const HIDDEN_SHARE: f32 = 1.0e-4;

/// A share at or below this reads as "toggled off".
///
/// A threshold rather than an equality test against [`HIDDEN_SHARE`]: a share
/// that has been through a container join is the constant scaled by its
/// parent's arithmetic, not the constant itself.
pub const HIDDEN_SHARE_CEILING: f32 = 1.0e-3;

/// Is this share one of the "hidden" weights?
///
/// `false` for a non-positive or non-finite share: those are values the tree
/// refuses and `normalize` repairs, so nothing that reaches a renderer can
/// legitimately be one, and reading a repaired `1.0` as "hidden" would hide a
/// pane the user can see.
pub fn is_hidden(share: f32) -> bool {
    share.is_finite() && share > 0.0 && share <= HIDDEN_SHARE_CEILING
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{ChannelId, Role, TileId, ViewKindId};
    use crate::layout::Layout;
    use crate::spec::PaneSpec;
    use crate::tree::{Container, LinearDir, Tile};
    use crate::verb::Verb;
    use impress_pane_query::PaneQuery;

    /// Three panes in a row, shares as given.
    fn row(shares: Vec<f32>) -> (Layout, TileId) {
        let mut layout = Layout::empty();
        let children: Vec<TileId> = (0..shares.len())
            .map(|i| {
                layout.insert_pane(
                    PaneSpec::new(PaneQuery::default(), ViewKindId::INFO)
                        .with_role(Role::from(format!("pane-{i}")))
                        .with_channel(ChannelId::ONE),
                )
            })
            .collect();
        let root =
            layout.insert_container(Container::linear(LinearDir::Horizontal, children, shares));
        layout.add_window(root);
        layout.normalize();
        (layout, root)
    }

    fn shares_of(layout: &Layout, root: TileId) -> Vec<f32> {
        match layout.tile(root) {
            Some(Tile::Container(Container::Linear { shares, .. })) => shares.clone(),
            other => panic!("expected a split, got {other:?}"),
        }
    }

    #[test]
    fn a_zero_share_is_refused_by_resize() {
        let (mut layout, root) = row(vec![1.0, 2.0, 3.0]);
        let err = layout
            .apply(Verb::Resize {
                container: root,
                shares: vec![1.0, 2.0, 0.0],
            })
            .expect_err("a zero share must be refused");
        assert!(
            matches!(err, crate::LayoutError::InvalidShares { .. }),
            "expected InvalidShares, got {err:?}"
        );
    }

    #[test]
    fn a_zero_share_does_not_survive_normalization() {
        let (mut layout, root) = row(vec![1.0, 2.0, 3.0]);
        // Straight into the arena — the only way to get one in at all, since
        // the verb refuses it.
        match layout.tiles.get_mut(&root) {
            Some(Tile::Container(Container::Linear { shares, .. })) => {
                *shares = vec![1.0, 2.0, 0.0];
            }
            _ => panic!("the root is a split"),
        }
        layout.normalize();
        assert_eq!(
            shares_of(&layout, root),
            vec![1.0, 2.0, 1.0],
            "normalize rewrites a non-positive share to a FULL column, which is why \
             HIDDEN_SHARE exists"
        );
    }

    #[test]
    fn the_hidden_share_is_a_legal_resize_and_survives_normalization() {
        let (mut layout, root) = row(vec![1.0, 2.0, 3.0]);
        layout
            .apply(Verb::Resize {
                container: root,
                shares: vec![1.0, 2.0, HIDDEN_SHARE],
            })
            .expect("HIDDEN_SHARE is a positive, finite weight");
        layout.normalize();
        layout.normalize();
        let shares = shares_of(&layout, root);
        assert!(is_hidden(shares[2]), "share was {}", shares[2]);
        assert!(!is_hidden(shares[0]) && !is_hidden(shares[1]));
        // And the hidden pane is still a pane: ⌘0 is a resize, not a split.
        assert_eq!(layout.panes().len(), 3);
    }

    #[test]
    fn is_hidden_refuses_the_values_the_tree_itself_refuses() {
        assert!(!is_hidden(0.0));
        assert!(!is_hidden(-1.0));
        assert!(!is_hidden(f32::NAN));
        assert!(!is_hidden(f32::INFINITY));
        assert!(!is_hidden(1.0));
        assert!(is_hidden(HIDDEN_SHARE));
        assert!(is_hidden(HIDDEN_SHARE_CEILING));
    }
}
