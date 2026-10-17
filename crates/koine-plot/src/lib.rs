//! The plot spec koine places, and the renderers that draw it.
//!
//! A surface's `plot{spec}` node carries a `plot-spec@1.0.0` payload
//! (ADR-0033). This crate is that payload's one definition and every
//! frontend's way to draw it:
//!
//! - [`spec`]: [`PlotSpec`] and its parts. The serde shape is the wire, so it
//!   is kept bit for bit. Missing fields take their defaults, which makes
//!   `{"series":[{"x":[1,2],"y":[3,4]}]}` a whole spec.
//! - [`axis`]: the tick calculator and axis layout.
//! - [`svg`]: [`svg::render_svg`], a standalone `<svg>` string for a page
//!   (`koine-dioxus` places it inside `div.koine-plot`).
//! - [`text`]: [`text::render_text`], an ASCII sketch for a terminal
//!   (`koine-tui`): axes, three ticks each, one glyph per series.
//! - [`Frame`] / [`frame`]: the plot area and axis ranges `render_svg` drew
//!   with, in the SVG's viewBox units. [`Frame::to_data`] maps a pixel in the
//!   element back to data coordinates (log axes included; `None` outside the
//!   plot area) through the same arithmetic as [`Frame::to_pixel`], which the
//!   renderer calls for every point, so a page can turn a click into `{x, y}`.
//! - [`graph`]: [`graph::GraphSpec`] (nodes, edges), [`graph::layout`] (a
//!   deterministic force-directed layout seeded by an FNV-1a hash of the node
//!   ids, 200 fixed iterations) and [`graph::render_graph_svg`] (edges as
//!   `<line>`, one `<g class="koine-graph-node" data-id>` per node, labels up
//!   to 500 nodes). The JSON forms are [`graph::parse_graph`] and
//!   [`graph::render_graph_svg_json`].
//! - [`render`]: the renderer registry. [`render::PlotRenderer`] is one way
//!   to draw a spec; SVG and text are built in, and each further backend
//!   (Typst, matplotlib, Veusz, …) is its own crate that registers itself
//!   ([`render::register_renderer`]). [`render::render`] draws with one by id.
//! - [`parse_spec`], [`render_svg_json`], [`render_text_json`]: the same for a
//!   spec held as JSON, answering a typed [`PlotError`] when it does not parse.
//!
//! Pure Rust with no clock, no threads and no I/O, so it builds for
//! `wasm32-unknown-unknown`. The `schema` feature derives `JsonSchema` on the
//! spec types.
//!
//! The Swift frontend does not draw through this crate: it keeps its
//! `renderPlot` hook through imprint-core's Typst pipeline, which draws more
//! than SVG can (typeset labels, PDF). That is not a second definition of the
//! plot, because the spec types it renders are this crate's.

pub mod axis;
pub mod graph;
pub mod render;
pub mod spec;
pub mod svg;
pub mod text;

pub use spec::*;
pub use svg::{frame, Frame};

use serde_json::Value;

/// Why a plot spec could not be drawn.
#[derive(Debug, thiserror::Error)]
pub enum PlotError {
    /// The JSON is not a `PlotSpec`; the message names the offending field.
    #[error("plot spec: {0}")]
    Spec(String),
    /// The renderer cannot write what was asked (a format, a feature).
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// The renderer failed (a tool it runs, a file it writes).
    #[error("render: {0}")]
    Render(String),
}

/// Parse a spec held as JSON.
pub fn parse_spec(value: &Value) -> Result<PlotSpec, PlotError> {
    serde_json::from_value(value.clone()).map_err(|e| PlotError::Spec(e.to_string()))
}

/// Draw a JSON spec as a standalone SVG string.
pub fn render_svg_json(value: &Value) -> Result<String, PlotError> {
    Ok(svg::render_svg(&parse_spec(value)?))
}

/// Draw a JSON spec as a text sketch of `cols` x `rows` characters.
pub fn render_text_json(value: &Value, cols: usize, rows: usize) -> Result<String, PlotError> {
    Ok(text::render_text(&parse_spec(value)?, cols, rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builder_spec_round_trips_through_json() {
        let spec = PlotSpec::new()
            .with_title("round trip")
            .with_x_label("x")
            .with_log_y()
            .line(vec![1.0, 2.0, 3.0], vec![1.0, 10.0, 100.0], "a")
            .scatter(vec![1.0, 2.0], vec![5.0, 6.0], "b")
            .with_hline(3.0, "ref", PlotColor::Gray);
        let value = serde_json::to_value(&spec).unwrap();
        let back = parse_spec(&value).unwrap();
        assert_eq!(serde_json::to_value(&back).unwrap(), value);
        assert_eq!(back.series.len(), 2);
        assert!(back.y_axis.log_scale);
    }

    #[test]
    fn minimal_spec_is_whole_and_draws() {
        let value = json!({"series": [{"x": [1, 2], "y": [3, 4]}]});
        let svg = render_svg_json(&value).unwrap();
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("<polyline") || svg.contains("<path"));
    }

    #[test]
    fn a_spec_without_series_shape_names_the_error() {
        let err = render_svg_json(&json!({"series": [{"x": [1, 2]}]})).unwrap_err();
        assert!(err.to_string().starts_with("plot spec: "), "{err}");
        assert!(err.to_string().contains("y"), "{err}");
        assert!(render_text_json(&json!("nope"), 40, 10).is_err());
    }

    #[test]
    fn the_wire_names_are_pinned() {
        let value =
            serde_json::to_value(PlotSpec::new().scatter(vec![1.0], vec![2.0], "s")).unwrap();
        assert_eq!(value["series"][0]["style"], "Scatter");
        assert_eq!(value["series"][0]["color"], "Blue");
        assert_eq!(value["legend"]["position"], "TopRight");
        assert_eq!(value["width"], 640.0);
    }
}
