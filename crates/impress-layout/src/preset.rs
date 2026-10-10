//! Presets: a default tree, a set of queries and a role assignment
//! (ADR-0031 D10). "An app preset is what the app is."
//!
//! Only the shape lives here. The queries and the view kinds are arguments,
//! because the shape is the same for imbib's publications, imprint's
//! manuscripts and impart's mail — which is the whole claim of D10, and the
//! reason the five binaries become a distribution decision.

use impress_pane_query::{ItemRef, PaneQuery, Scope};

use crate::ids::{ChannelId, Role, TileId, ViewKindId, WindowId};
use crate::layout::Layout;
use crate::spec::{PaneSpec, ParamBinding, ParamSource};
use crate::tree::{Container, LinearDir};

/// The parameter a detail pane binds: `item`, of the list's kind, on channel 1.
pub const DETAIL_PARAM: &str = "item";

/// The tiles a [`three_column`] layout is made of, so callers (presets and
/// tests) can address them without re-deriving the ids. The ADR-0019 D5
/// importer L7 planned as a third caller was never built (plan wave 6 W5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreeColumn {
    pub window: WindowId,
    pub root: TileId,
    pub navigator: TileId,
    pub list: TileId,
    pub detail: TileId,
}

/// Today's chassis as a value: navigator | list | detail in one horizontal
/// split, shares 1 : 2 : 3, all on channel 1.
///
/// * the navigator is an `outline` over the navigable kinds (`collection`,
///   `library`) — the sidebar is a pane like any other (D1);
/// * the list is `list_query` rendered by `list`;
/// * the detail is `item(id = $item)` rendered by `detail_view_kind`, its one
///   (optional — ADR-0031 D3) parameter following channel 1, which is the
///   whole of what "selecting in the list drives the detail pane" now means.
///
/// Focus starts on the list: it is the pane the triage grammar acts on.
pub fn three_column(list_query: PaneQuery, detail_view_kind: ViewKindId) -> Layout {
    let (layout, _) = three_column_parts(list_query, detail_view_kind);
    layout
}

/// [`three_column`], plus the ids of the tiles it built.
pub fn three_column_parts(
    list_query: PaneQuery,
    detail_view_kind: ViewKindId,
) -> (Layout, ThreeColumn) {
    let item_kind = list_query
        .kinds
        .first()
        .cloned()
        .unwrap_or_else(|| "item".to_string());

    let navigator = PaneSpec::new(navigator_query(), ViewKindId::OUTLINE)
        .with_role(Role::NAVIGATOR)
        .with_channel(ChannelId::ONE);

    let list = PaneSpec::new(list_query.clone(), ViewKindId::LIST)
        .with_role(Role::LIST)
        .with_channel(ChannelId::ONE);

    let detail = PaneSpec::new(detail_query(&list_query), detail_view_kind)
        .with_role(Role::DETAIL)
        .with_channel(ChannelId::ONE)
        // OPTIONAL, not required (ADR-0031 D3): a parameter with no value
        // yet renders the view kind's EMPTY STATE, never an error. A detail
        // pane before the first selection is the canonical case — it compiles
        // to a query that matches nothing, which is what "no paper selected"
        // looks like. Only a query that is WRONG (an unknown kind, a
        // parameter of the wrong kind) is a typed refusal; see the
        // `impress_pane_query` crate docs, "Empty is not an error, and an
        // error is not empty".
        .with_param(ParamBinding::optional(
            DETAIL_PARAM,
            item_kind,
            ParamSource::Channel {
                channel: ChannelId::ONE,
            },
        ));

    let mut layout = Layout::empty();
    let navigator_id = layout.insert_pane(navigator);
    let list_id = layout.insert_pane(list);
    let detail_id = layout.insert_pane(detail);
    let root = layout.insert_container(Container::linear(
        LinearDir::Horizontal,
        vec![navigator_id, list_id, detail_id],
        vec![1.0, 2.0, 3.0],
    ));

    let window = layout.add_window(root);
    if let Some(w) = layout.window_mut(window) {
        w.focused = Some(list_id);
    }
    layout.normalize();

    (
        layout,
        ThreeColumn {
            window,
            root,
            navigator: navigator_id,
            list: list_id,
            detail: detail_id,
        },
    )
}

/// What the sidebar shows: the navigable queries themselves.
pub fn navigator_query() -> PaneQuery {
    PaneQuery {
        kinds: vec!["collection".to_string(), "library".to_string()],
        scope: Scope::All,
        ..PaneQuery::default()
    }
}

/// The detail pane's query: the one item bound to `$item`, of whatever kinds
/// the list shows.
pub fn detail_query(list_query: &PaneQuery) -> PaneQuery {
    PaneQuery {
        kinds: list_query.kinds.clone(),
        scope: Scope::Item {
            id: ItemRef::Param {
                name: DETAIL_PARAM.to_string(),
            },
        },
        ..PaneQuery::default()
    }
}
