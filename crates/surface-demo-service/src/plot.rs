//! Builds the `plot` field `histogram` returns: a `plot-spec@1.0.0` payload
//! (ADR-0033 "Defaults": "Plots carry a `plot-spec@1.0.0` payload in the render
//! tree, never pixels").
//!
//! The wire shape of `plot-spec@1.0.0` IS the serde shape of
//! [`koine_plot::PlotSpec`] (its module docs say so: `series[].x`/`y`, `style`,
//! `color`, `x_axis`/`y_axis`), and every renderer in koine — the web page's
//! `plot` node, the TUI, the Swift widget — parses the payload with
//! `koine_plot::parse_spec`. So this module builds that struct and serializes
//! it, rather than mirroring a shape by hand: the payload cannot drift from
//! what the renderers read, and `tests` here pins it by parsing the result
//! back as a `PlotSpec`.
//!
//! (Until plan-koine-tour T-D2 this module mirrored imprint-core's FFI shape —
//! `series[].kind`, `xs`, `ys`, `color{r,g,b}` — which is a different wire,
//! and the web showed "plot spec: missing field `x`".)
//!
//! A histogram is a [`SeriesStyle::Bar`] series over the bin centres.

use koine_plot::{PlotAxis, PlotColor, PlotSeries, PlotSpec, SeriesStyle};
use serde_json::Value;

/// A `plot-spec@1.0.0` payload: one bar series through the histogram's bin
/// centres against its counts, with axis labels — the `plot` field of
/// `HistogramResult`. Built as [`PlotSpec`] and serialized through it.
pub fn histogram_plot_spec(bin_centres: &[f64], counts: &[u64]) -> Value {
    serde_json::to_value(histogram_plot(bin_centres, counts)).expect("PlotSpec always serializes")
}

/// [`histogram_plot_spec`] before serialization.
pub fn histogram_plot(bin_centres: &[f64], counts: &[u64]) -> PlotSpec {
    let ys: Vec<f64> = counts.iter().map(|&c| c as f64).collect();
    let series = PlotSeries::line(bin_centres.to_vec(), ys, "count")
        .with_style(SeriesStyle::Bar)
        .with_color(PlotColor::Blue);
    PlotSpec {
        title: Some("Histogram".to_string()),
        width: 480.0,
        height: 320.0,
        x_axis: PlotAxis {
            label: Some("value".to_string()),
            ..PlotAxis::default()
        },
        y_axis: PlotAxis {
            label: Some("count".to_string()),
            ..PlotAxis::default()
        },
        series: vec![series],
        ..PlotSpec::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_payload_parses_back_as_koine_plots_spec() {
        let value = histogram_plot_spec(&[0.5, 1.5, 2.5], &[1, 4, 2]);
        let parsed: PlotSpec = serde_json::from_value(value.clone())
            .unwrap_or_else(|e| panic!("not a koine-plot PlotSpec: {e}\n{value}"));
        assert_eq!(parsed.series.len(), 1);
        assert_eq!(parsed.series[0].x, vec![0.5, 1.5, 2.5]);
        assert_eq!(parsed.series[0].y, vec![1.0, 4.0, 2.0]);
        assert_eq!(parsed.series[0].style, SeriesStyle::Bar);
        assert_eq!(parsed.x_axis.label.as_deref(), Some("value"));
        assert_eq!(parsed.y_axis.label.as_deref(), Some("count"));
        // The renderers' own entry point accepts it too.
        koine_plot::parse_spec(&value).expect("parse_spec accepts the payload");
    }

    #[test]
    fn the_wire_is_koine_plots_not_imprints_ffi_shape() {
        let value = histogram_plot_spec(&[1.0], &[1]);
        let series = &value["series"][0];
        assert!(series.get("x").is_some() && series.get("y").is_some());
        assert!(series.get("xs").is_none() && series.get("ys").is_none());
        assert!(series.get("kind").is_none());
        assert!(value.get("x_axis").is_some() && value.get("colormap").is_none());
    }
}
