//! Koine is the speech the impress frontends share.
//!
//! A verb and its arguments become one document. [`surface`] turns that
//! document into a [`surface::RenderTree`]. A frontend is a mapping of that
//! tree: SwiftUI in `ImpressSurface`, plain text in `koine-tui`. The mapping
//! holds no logic of its own. Anything that computes is a verb.
//! [`style`] is the sheet each frontend applies: shared slot names, a value
//! kind per frontend. The rules are `docs/plan-koine-style.md`.
//! [`gesture`] names what the person did — focus, activate, a direction, a
//! drag — the same way on every frontend, and [`gesture::layout_verb`] is the
//! layout verb that gesture already determines. A surface click that is not
//! a known view kind stays an event for [`surface::reduce`]. A sheet does
//! not decide which gesture a click is. [`style_file`] is the style
//! document. The native writer that locks it is the `koine-style-file` crate.
//! [`cursor`] is the focus cursor and the key vocabulary every frontend
//! shares: the same keys move the same focus and send the same events on the
//! text frontend and the web one.
//!
//! Two parts sit beside the document. Adjectives (`pane_query::adjective`)
//! are the named, typed filters a pane query is written in, each lowering to
//! the same query. Adverbs (`verbs::adverb`) are the six modifiers every
//! verb call may carry, under one reserved `adverbs` key.
//!
//! The verb half — [`verbs`] and [`verb_surface`] — is the `verbs` feature,
//! on by default. A browser build turns it off and performs the document
//! only (`docs/plan-koine-web.md` D5); every other module is unconditional.
//!
//! This crate is the name and the public face. The grammar still lives in the
//! crates it re-exports, so existing callers keep their paths. The repository
//! cut that would publish koine on its own is `docs/plan-koine-separation.md`.

pub mod cursor;
pub mod gesture;
pub mod style;
pub mod style_file;

pub use impress_layout as layout;
pub use impress_pane_query as pane_query;
#[cfg(feature = "verbs")]
pub use impress_service_core as verbs;
pub use impress_surface as surface;
#[cfg(feature = "verbs")]
pub use impress_verb_surface as verb_surface;

#[cfg(test)]
mod tests {
    #[test]
    fn the_grammar_is_reachable_under_one_name() {
        let spec = crate::surface::example_paper_triage();
        let tree = crate::surface::resolve(
            &spec,
            &serde_json::json!({}),
            &serde_json::json!({}),
            &serde_json::json!({}),
        );
        assert!(!tree.root.id.is_empty());
    }
}
