//! The koine spec through this crate's Typst lowering: [`TypstRenderer`]
//! is koine-plot's `typst` backend (docs/plan-koine-kit-moves.md KM-D13).
//! It writes Typst source; a raster tile (the big-N path, contour
//! underlays) comes back as an asset under the name the source embeds. The
//! host compiles it with its own Typst engine.

use koine_plot::render::{chosen_format, Format, PlotRenderer, RenderOptions, Rendered};
use koine_plot::{
    Colormap as SpecColormap, LineStyle as SpecLineStyle, PlotAxis, PlotError, PlotSpec,
    RenderStrategy, SeriesStyle,
};

use crate::{Axis, Colormap, LineStyle, Plot, PlotSize, Strategy};

/// The asset name the raster path embeds.
pub const RASTER_ASSET: &str = "koine_plot.png";

/// The Typst renderer. Register it with
/// `koine_plot::render::register_renderer(Arc::new(TypstRenderer))`.
pub struct TypstRenderer;

fn axis(spec: &PlotAxis) -> Axis {
    let mut axis = if spec.log_scale {
        Axis::log()
    } else {
        Axis::linear()
    };
    if let (Some(min), Some(max)) = (spec.min, spec.max) {
        axis = axis.with_limits(min, max);
    }
    if let Some(label) = &spec.label {
        axis = axis.with_label(label.clone());
    }
    axis
}

fn colormap(c: SpecColormap) -> Colormap {
    match c {
        SpecColormap::Viridis => Colormap::Viridis,
        SpecColormap::Magma => Colormap::Magma,
        SpecColormap::Plasma => Colormap::Plasma,
        SpecColormap::Inferno => Colormap::Inferno,
        SpecColormap::Cividis => Colormap::Cividis,
        SpecColormap::Turbo => Colormap::Turbo,
        SpecColormap::Greys => Colormap::Greys,
    }
}

fn line_style(s: SpecLineStyle) -> LineStyle {
    match s {
        SpecLineStyle::Solid => LineStyle::Solid,
        SpecLineStyle::Dashed => LineStyle::Dashed,
        SpecLineStyle::Dotted => LineStyle::Dotted,
        SpecLineStyle::DashDotted => LineStyle::DashDotted,
    }
}

fn rgb(css: &str) -> [u8; 3] {
    let hex = css.trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0);
    [byte(0), byte(2), byte(4)]
}

/// The builder for `spec`.
pub fn plot_of(spec: &PlotSpec) -> Plot {
    let mut plot = Plot::new(axis(&spec.x_axis), axis(&spec.y_axis))
        .title(spec.title.clone().unwrap_or_default());
    plot.strategy = match spec.strategy {
        RenderStrategy::Auto => Strategy::Auto,
        RenderStrategy::Vector => Strategy::Vector,
        RenderStrategy::Raster => Strategy::Raster,
    };
    plot.cmap = colormap(spec.colormap);
    if let Some(threshold) = spec.raster_threshold {
        plot.raster_threshold = threshold as usize;
    }
    if spec.contours.levels > 0 {
        plot.contour_levels = spec.contours.levels as usize;
    }
    plot.contour_labels = spec.contours.labels;
    plot.contour_line_styles = spec
        .contours
        .line_styles
        .iter()
        .copied()
        .map(line_style)
        .collect();
    plot.contour_level_values = spec.contours.level_values.clone();
    plot.asset_id = RASTER_ASSET.into();
    for series in &spec.series {
        let color = rgb(&series.color.css());
        let (x, y) = (series.x.clone(), series.y.clone());
        plot = match series.style {
            SeriesStyle::Line | SeriesStyle::Step | SeriesStyle::Bar => plot.line(x, y, color),
            SeriesStyle::Scatter => plot.scatter(x, y, color),
            SeriesStyle::LineScatter => plot.line(x.clone(), y.clone(), color).scatter(x, y, color),
            SeriesStyle::Contour => plot.contour(x, y),
        };
    }
    plot
}

impl PlotRenderer for TypstRenderer {
    fn id(&self) -> &'static str {
        "typst"
    }

    fn formats(&self) -> &'static [Format] {
        &[Format::Typst]
    }

    fn unsupported(&self, spec: &PlotSpec) -> Vec<String> {
        let mut out = Vec::new();
        for s in &spec.series {
            match s.style {
                SeriesStyle::Bar => {
                    out.push(format!("series '{}': bars (drawn as a line)", s.label))
                }
                SeriesStyle::Step => {
                    out.push(format!("series '{}': steps (drawn as a line)", s.label))
                }
                _ => {}
            }
            if s.error_low.is_some() || s.error_high.is_some() {
                out.push(format!("series '{}': error bars", s.label));
            }
        }
        if !spec.annotations.is_empty() {
            out.push(format!("{} annotation(s)", spec.annotations.len()));
        }
        if spec.x_axis.min.is_some() != spec.x_axis.max.is_some()
            || spec.y_axis.min.is_some() != spec.y_axis.max.is_some()
        {
            out.push("a one-sided axis limit (both bounds or neither)".into());
        }
        out
    }

    fn render(&self, spec: &PlotSpec, options: &RenderOptions) -> Result<Rendered, PlotError> {
        chosen_format(self, options)?;
        let (w, h) = options.size(spec);
        let output = plot_of(spec).render(PlotSize::new(w, h));
        let mut rendered = Rendered::text(Format::Typst, output.typst);
        rendered.assets = output.assets.into_iter().collect();
        Ok(rendered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use koine_plot::PlotSeries;

    #[test]
    fn a_line_spec_becomes_typst_and_bars_are_named() {
        let mut spec = PlotSpec::default();
        spec.series.push(PlotSeries::line(
            vec![1.0, 2.0, 3.0],
            vec![1.0, 4.0, 9.0],
            "sq",
        ));
        spec.series.push(
            PlotSeries::line(vec![1.0, 2.0], vec![2.0, 3.0], "counts").with_style(SeriesStyle::Bar),
        );
        let out = TypstRenderer
            .render(&spec, &RenderOptions::default())
            .unwrap();
        assert_eq!(out.format, Format::Typst);
        assert!(
            out.body_text().unwrap().contains("#curve("),
            "{:?}",
            out.body_text()
        );
        assert_eq!(
            TypstRenderer.unsupported(&spec),
            vec!["series 'counts': bars (drawn as a line)".to_string()]
        );
    }

    #[test]
    fn a_contour_spec_takes_the_contour_path_with_its_raster_asset() {
        let xs: Vec<f64> = (0..400).map(|i| (i as f64 * 0.37).sin()).collect();
        let ys: Vec<f64> = (0..400).map(|i| (i as f64 * 0.21).cos()).collect();
        let mut spec = PlotSpec::default();
        spec.series
            .push(PlotSeries::line(xs, ys, "cloud").with_style(SeriesStyle::Contour));
        let out = TypstRenderer
            .render(&spec, &RenderOptions::default())
            .unwrap();
        assert!(
            !out.assets.is_empty(),
            "the contour underlay is a raster tile"
        );
        assert!(out.body_text().unwrap().contains(RASTER_ASSET));
    }
}
