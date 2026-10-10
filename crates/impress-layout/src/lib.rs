//! The layout tree (ADR-0031 D4–D8): query-addressed panes, containers,
//! channels, the verb vocabulary, reversible patches and the undo rings —
//! all as plain values and pure functions.
//!
//! # What this crate is
//!
//! A [`Layout`] is one value: windows over an arena of [`Tile`]s, each a
//! [`PaneSpec`] (a query, a view kind, its parameters and the channel it
//! publishes on) or a [`Container`] (`Tabs | Linear | Grid`, the egui_tiles
//! shape). Every gesture is a [`Verb`]; applying one returns a [`Patch`] that
//! reverts exactly; [`UndoStacks`] routes patches onto the arrangement ring or
//! the focused pane's exploration ring per ADR-0031 D7.
//!
//! # What this crate is not
//!
//! There is **no I/O and no store access here**. Nothing opens a database,
//! reads a file, or runs a query: [`impress_pane_query::PaneQuery`] is
//! carried, never compiled or executed. Persisting a layout as an
//! `impress/ui/layout` item, emitting operations, and exposing the verbs as
//! `#[impress_service]` methods are the `layout-service`'s job (work package
//! L3). That cut is what makes the whole model testable by `cargo test` with
//! no app running.
//!
//! This crate depends on [`impress_pane_query`] alone — never on
//! `impress-core`'s domain modules (ADR-0033 D7) — so it can leave the suite
//! as part of a standalone kit.
//!
//! ```
//! use impress_layout::{Layout, PaneRef, Role, Verb};
//! use impress_layout::preset;
//! use impress_pane_query::PaneQuery;
//!
//! let mut layout = preset::three_column(
//!     PaneQuery { kinds: vec!["publication".into()], ..Default::default() },
//!     "info".into(),
//! );
//!
//! // "the pane to the right of the navigator" is the list, by the tree rule.
//! let window = layout.current_window().unwrap();
//! let navigator = layout.resolve(window, &PaneRef::role(Role::NAVIGATOR)).unwrap();
//! let list = layout.resolve(window, &PaneRef::role(Role::LIST)).unwrap();
//! assert_eq!(layout.step(window, navigator, impress_layout::Direction::Right), list);
//!
//! // Every verb is invertible.
//! let patch = layout.apply(Verb::Close { target: PaneRef::role(Role::NAVIGATOR) }).unwrap();
//! assert!(layout.pane_with_role(&Role::NAVIGATOR).is_none());
//! layout.revert(&patch);
//! assert!(layout.pane_with_role(&Role::NAVIGATOR).is_some());
//! ```

mod apply;
mod channels;
mod error;
mod ids;
mod layout;
mod patch;
pub mod preset;
mod sessions;
pub mod shares;
mod spec;
mod tree;
mod undo;
mod verb;

pub use channels::ChannelState;
pub use error::LayoutError;
pub use ids::{view_state, ChannelId, Role, SessionId, TileId, ViewKindId, WindowId};
pub use layout::Layout;
pub use patch::{Change, Patch, TileChange};
pub use shares::{is_hidden, HIDDEN_SHARE, HIDDEN_SHARE_CEILING};
pub use spec::{PaneSpec, ParamBinding, ParamSource};
pub use tree::{Container, ContainerKind, Geometry, LinearDir, Tile, Window};
pub use undo::{stack_for, StackKind, UndoRing, UndoStacks, DEFAULT_CAPACITY};
pub use verb::{Direction, PaneRef, PaneRefWire, Placement, Verb};

// Re-exported so a caller does not have to depend on impress-pane-query just
// to spell a pane's query or read its resolved bindings.
pub use impress_pane_query::{Bindings, PaneQuery, ParamDecl, ParamName, RecordKindId, Scope};

#[cfg(all(test, feature = "schema"))]
mod schema_tests {
    use super::*;

    /// L3 publishes these as the argument schemas of the `layout-service`
    /// verbs, so a derive that cannot be generated is a build-time problem
    /// here rather than a runtime one there.
    #[test]
    fn the_layout_and_the_verbs_have_json_schemas() {
        let layout = schemars::schema_for!(Layout);
        let json = serde_json::to_value(&layout).unwrap();
        assert!(json["properties"]["tiles"].is_object());
        assert!(json["properties"]["windows"].is_object());

        let verb = serde_json::to_value(schemars::schema_for!(Verb)).unwrap();
        assert!(
            verb.to_string().contains("split"),
            "the verb schema should name its variants"
        );
    }
}
