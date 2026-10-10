//! Builds the `plot` field `histogram` returns: a `plot-spec@1.0.0` payload
//! (ADR-0033 "Defaults": "Plots carry a `plot-spec@1.0.0` payload in the render
//! tree, never pixels. In the suite the widget renders through imprint-core's
//! existing `render_plot_svg`.").
//!
//! That renderer's input type is `imprint_core::plot_ffi::FfiPlotSpec`, gated
//! behind imprint-core's `typst-render` feature (the Typst engine + friends) —
//! far too heavy a runtime dependency for one JSON shape. So this module keeps
//! its own small mirror of that struct's wire shape (field names, `camelCase`
//! rename, enum spellings — checked by hand against
//! `crates/imprint-core/src/plot_ffi.rs` and pinned by the round-trip test in
//! `tests/plot_shape.rs`, which *does* depend on real `imprint-core` as a
//! dev-dependency) and serializes through that, per the task's stated
//! preference: build a struct and `serde_json::to_value` it, rather than
//! assemble the JSON in a `serde_json::json!` literal by hand.
//!
//! `impress-plot` itself (the plotting engine `FfiPlotSpec` wraps) has no
//! serde types of its own — `Plot`/`Series` there derive neither `Serialize`
//! nor `Deserialize` — so it is not usable as "the spec type" directly; the
//! FFI record is the one JSON contract that exists.
//!
//! `FfiSeriesKind` has no `Bar` or `Step` variant (only `Line`, `Scatter`,
//! `Contour`), so a histogram is rendered as a **line series over bin
//! centres** rather than bars — noted here per the work package's
//! instructions, since the spec cannot express a true bar series.

use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
struct AxisJson {
    scale: &'static str,
    min: Option<f64>,
    max: Option<f64>,
    label: Option<String>,
}

impl AxisJson {
    fn labelled(label: &str) -> Self {
        AxisJson {
            scale: "linear",
            min: None,
            max: None,
            label: Some(label.to_string()),
        }
    }
}

#[derive(Serialize)]
struct ColorJson {
    r: u8,
    g: u8,
    b: u8,
}

#[derive(Serialize)]
struct SeriesJson {
    kind: &'static str,
    xs: Vec<f64>,
    ys: Vec<f64>,
    color: ColorJson,
}

/// Mirrors `imprint_core::plot_ffi::FfiPlotSpec`'s
/// `#[serde(default, rename_all = "camelCase")]` wire shape field for field.
#[derive(Serialize)]
struct PlotSpecJson {
    title: String,
    x: AxisJson,
    y: AxisJson,
    series: Vec<SeriesJson>,
    strategy: &'static str,
    colormap: &'static str,
    width: f64,
    height: f64,
    #[serde(rename = "rasterThreshold")]
    raster_threshold: u32,
    #[serde(rename = "contourLevels")]
    contour_levels: u32,
    #[serde(rename = "contourLabels")]
    contour_labels: bool,
    #[serde(rename = "contourLineStyles")]
    contour_line_styles: Vec<&'static str>,
    #[serde(rename = "contourLevelValues")]
    contour_level_values: Vec<f64>,
}

/// A `plot-spec@1.0.0` payload: a line series through the histogram's bin
/// centres against its counts, with axis labels — the `plot` field of
/// `HistogramResult`.
pub fn histogram_plot_spec(bin_centres: &[f64], counts: &[u64]) -> Value {
    let ys: Vec<f64> = counts.iter().map(|&c| c as f64).collect();
    let spec = PlotSpecJson {
        title: "Histogram".to_string(),
        x: AxisJson::labelled("value"),
        y: AxisJson::labelled("count"),
        series: vec![SeriesJson {
            // No `Bar`/`Step` kind exists on `FfiSeriesKind` — see module docs.
            kind: "line",
            xs: bin_centres.to_vec(),
            ys,
            color: ColorJson {
                r: 31,
                g: 119,
                b: 180,
            },
        }],
        strategy: "auto",
        colormap: "viridis",
        width: 480.0,
        height: 320.0,
        raster_threshold: 0,
        contour_levels: 0,
        contour_labels: false,
        contour_line_styles: Vec::new(),
        contour_level_values: Vec::new(),
    };
    serde_json::to_value(spec).expect("PlotSpecJson always serializes")
}
