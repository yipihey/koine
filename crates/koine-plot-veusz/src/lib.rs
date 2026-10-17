//! koine-plot's Veusz backend (docs/plan-koine-kit-moves.md KM-D13).
//!
//! [`VeuszRenderer`] turns a [`PlotSpec`] into a Veusz document (`.vsz`):
//! the command script Veusz itself saves and loads, with the data set
//! inline (`SetData`), one page holding one graph, two axes, an `xy` or
//! `bar` widget per series, a key and the title. Open it in Veusz to keep
//! editing; with the `exec` feature the renderer exports it through the
//! person's `veusz --export` to PNG, PDF or SVG.
//!
//! Arrows, filled bands and density contours from a point cloud have no
//! one-widget Veusz form here yet; [`PlotRenderer::unsupported`] names them.

use std::fmt::Write as _;

use koine_plot::render::{chosen_format, Format, PlotRenderer, RenderOptions, Rendered};
use koine_plot::{Annotation, LegendPosition, PlotError, PlotSpec, SeriesStyle};

/// The Veusz renderer. Register it with
/// `koine_plot::render::register_renderer(Arc::new(VeuszRenderer))`.
pub struct VeuszRenderer;

#[cfg(not(feature = "exec"))]
const FORMATS: &[Format] = &[Format::Vsz];
#[cfg(feature = "exec")]
const FORMATS: &[Format] = &[Format::Vsz, Format::Png, Format::Pdf, Format::Svg];

/// Centimetres per point: koine-plot's sizes are points.
const CM_PER_POINT: f64 = 2.54 / 72.0;

impl PlotRenderer for VeuszRenderer {
    fn id(&self) -> &'static str {
        "veusz"
    }

    fn formats(&self) -> &'static [Format] {
        FORMATS
    }

    fn unsupported(&self, spec: &PlotSpec) -> Vec<String> {
        let mut out = Vec::new();
        for series in &spec.series {
            if series.style == SeriesStyle::Contour {
                out.push(format!("series '{}': density contours", series.label));
            }
        }
        for annotation in &spec.annotations {
            match annotation {
                Annotation::Arrow { .. } => out.push("an arrow annotation".into()),
                Annotation::FillBetween { .. } => out.push("a fill-between band".into()),
                _ => {}
            }
        }
        out
    }

    fn render(&self, spec: &PlotSpec, options: &RenderOptions) -> Result<Rendered, PlotError> {
        let format = chosen_format(self, options)?;
        let document = document(spec, options);
        match format {
            Format::Vsz => Ok(Rendered::text(Format::Vsz, document)),
            #[cfg(feature = "exec")]
            other => exec::run(&document, other),
            #[cfg(not(feature = "exec"))]
            other => Err(PlotError::Unsupported(format!(
                "veusz writes {other:?} only with the exec feature"
            ))),
        }
    }
}

fn q(s: &str) -> String {
    // Veusz documents are Python: a JSON string literal is a valid one.
    serde_json::to_string(s).unwrap_or_else(|_| "''".into())
}

fn floats(values: &[f64]) -> String {
    let items: Vec<String> = values
        .iter()
        .map(|v| {
            if v.is_finite() {
                format!("{v}")
            } else {
                "float('nan')".into()
            }
        })
        .collect();
    format!("[{}]", items.join(", "))
}

fn key_position(position: LegendPosition) -> (&'static str, &'static str) {
    match position {
        LegendPosition::TopRight => ("right", "top"),
        LegendPosition::TopLeft => ("left", "top"),
        LegendPosition::BottomRight => ("right", "bottom"),
        LegendPosition::BottomLeft => ("left", "bottom"),
    }
}

/// The `.vsz` document for `spec`.
pub fn document(spec: &PlotSpec, options: &RenderOptions) -> String {
    let (w, h) = options.size(spec);
    let mut s = String::new();
    let _ = writeln!(s, "# A koine plot-spec@1.0.0, as a Veusz document.");
    for (i, series) in spec.series.iter().enumerate() {
        if series.style == SeriesStyle::Contour {
            continue;
        }
        let _ = writeln!(s, "SetData('x{i}', {})", floats(&series.x));
        match (&series.error_low, &series.error_high) {
            (Some(low), Some(high)) => {
                let negative: Vec<f64> = low.iter().map(|v| -v.abs()).collect();
                let _ = writeln!(
                    s,
                    "SetData('y{i}', {}, negerr={}, poserr={})",
                    floats(&series.y),
                    floats(&negative),
                    floats(high)
                );
            }
            _ => {
                let _ = writeln!(s, "SetData('y{i}', {})", floats(&series.y));
            }
        }
    }
    let _ = writeln!(s, "Add('page', name='page1', autoadd=False)");
    let _ = writeln!(s, "To('page1')");
    let _ = writeln!(s, "Set('width', '{:.2}cm')", w * CM_PER_POINT);
    let _ = writeln!(s, "Set('height', '{:.2}cm')", h * CM_PER_POINT);
    let _ = writeln!(s, "Add('graph', name='graph1', autoadd=False)");
    let _ = writeln!(s, "To('graph1')");
    for (axis, name) in [(&spec.x_axis, "x"), (&spec.y_axis, "y")] {
        let _ = writeln!(s, "Add('axis', name='{name}', autoadd=False)");
        let _ = writeln!(s, "To('{name}')");
        if name == "y" {
            let _ = writeln!(s, "Set('direction', 'vertical')");
        }
        if let Some(label) = &axis.label {
            let _ = writeln!(s, "Set('label', {})", q(label));
        }
        if axis.log_scale {
            let _ = writeln!(s, "Set('log', True)");
        }
        if let Some(min) = axis.min {
            let _ = writeln!(s, "Set('min', {min})");
        }
        if let Some(max) = axis.max {
            let _ = writeln!(s, "Set('max', {max})");
        }
        if spec.show_grid {
            let _ = writeln!(s, "Set('GridLines/hide', False)");
        }
        let _ = writeln!(s, "To('..')");
    }
    for (i, series) in spec.series.iter().enumerate() {
        let color = series.color.css();
        match series.style {
            SeriesStyle::Contour => continue,
            SeriesStyle::Bar => {
                let _ = writeln!(s, "Add('bar', name='series{i}', autoadd=False)");
                let _ = writeln!(s, "To('series{i}')");
                let _ = writeln!(s, "Set('posn', 'x{i}')");
                let _ = writeln!(s, "Set('lengths', ('y{i}',))");
                let _ = writeln!(s, "Set('barfill', [('solid', {}, False)])", q(&color));
                if !series.label.is_empty() {
                    let _ = writeln!(s, "Set('keys', ({},))", q(&series.label));
                }
            }
            style => {
                let _ = writeln!(s, "Add('xy', name='series{i}', autoadd=False)");
                let _ = writeln!(s, "To('series{i}')");
                let _ = writeln!(s, "Set('xData', 'x{i}')");
                let _ = writeln!(s, "Set('yData', 'y{i}')");
                let marker = matches!(style, SeriesStyle::Scatter | SeriesStyle::LineScatter);
                let line = !matches!(style, SeriesStyle::Scatter);
                let _ = writeln!(
                    s,
                    "Set('marker', {})",
                    if marker { "'circle'" } else { "'none'" }
                );
                if marker {
                    let _ = writeln!(s, "Set('markerSize', '{}pt')", series.point_radius * 2.0);
                    let _ = writeln!(s, "Set('MarkerFill/color', {})", q(&color));
                    let _ = writeln!(s, "Set('MarkerLine/color', {})", q(&color));
                }
                let _ = writeln!(
                    s,
                    "Set('PlotLine/hide', {})",
                    if line { "False" } else { "True" }
                );
                let _ = writeln!(s, "Set('PlotLine/color', {})", q(&color));
                let _ = writeln!(s, "Set('PlotLine/width', '{}pt')", series.line_width);
                if style == SeriesStyle::Step {
                    let _ = writeln!(s, "Set('PlotLine/steps', 'centre')");
                }
                if series.error_low.is_some() {
                    let _ = writeln!(s, "Set('ErrorBarLine/color', {})", q(&color));
                }
                if !series.label.is_empty() {
                    let _ = writeln!(s, "Set('key', {})", q(&series.label));
                }
            }
        }
        let _ = writeln!(s, "To('..')");
    }
    for (i, annotation) in spec.annotations.iter().enumerate() {
        match annotation {
            Annotation::HLine {
                y,
                label,
                color,
                dash,
            }
            | Annotation::VLine {
                x: y,
                label,
                color,
                dash,
            } => {
                let horizontal = matches!(annotation, Annotation::HLine { .. });
                let _ = writeln!(s, "Add('function', name='ref{i}', autoadd=False)");
                let _ = writeln!(s, "To('ref{i}')");
                let _ = writeln!(s, "Set('function', '{y}')");
                let _ = writeln!(
                    s,
                    "Set('variable', {})",
                    if horizontal { "'x'" } else { "'y'" }
                );
                let _ = writeln!(s, "Set('Line/color', {})", q(&color.css()));
                if *dash {
                    let _ = writeln!(s, "Set('Line/style', 'dashed')");
                }
                if let Some(label) = label {
                    let _ = writeln!(s, "Set('key', {})", q(label));
                }
                let _ = writeln!(s, "To('..')");
            }
            Annotation::Text { x, y, text, color } => {
                let _ = writeln!(s, "Add('label', name='note{i}', autoadd=False)");
                let _ = writeln!(s, "To('note{i}')");
                let _ = writeln!(s, "Set('label', {})", q(text));
                let _ = writeln!(s, "Set('positioning', 'axes')");
                let _ = writeln!(s, "Set('xPos', [{x}])");
                let _ = writeln!(s, "Set('yPos', [{y}])");
                let _ = writeln!(s, "Set('Text/color', {})", q(&color.css()));
                let _ = writeln!(s, "To('..')");
            }
            // Named by `unsupported`.
            Annotation::Arrow { .. } | Annotation::FillBetween { .. } => {}
        }
    }
    let labelled = spec.series.iter().any(|s| !s.label.is_empty());
    if spec.legend.visible && labelled {
        let (horizontal, vertical) = key_position(spec.legend.position);
        let _ = writeln!(s, "Add('key', name='key1', autoadd=False)");
        let _ = writeln!(s, "To('key1')");
        let _ = writeln!(s, "Set('horzPosn', '{horizontal}')");
        let _ = writeln!(s, "Set('vertPosn', '{vertical}')");
        let _ = writeln!(s, "To('..')");
    }
    let _ = writeln!(s, "To('..')");
    if let Some(title) = &spec.title {
        let _ = writeln!(s, "Add('label', name='title', autoadd=False)");
        let _ = writeln!(s, "To('title')");
        let _ = writeln!(s, "Set('label', {})", q(title));
        let _ = writeln!(s, "Set('xPos', [0.5])");
        let _ = writeln!(s, "Set('yPos', [0.96])");
        let _ = writeln!(s, "Set('alignHorz', 'centre')");
        let _ = writeln!(s, "To('..')");
    }
    let _ = writeln!(s, "To('..')");
    s
}

#[cfg(feature = "exec")]
mod exec {
    use super::*;

    /// Export with `veusz --export` (or `$KOINE_VEUSZ`).
    pub fn run(document: &str, format: Format) -> Result<Rendered, PlotError> {
        let dir =
            tempfile::tempdir().map_err(|e| PlotError::Render(format!("scratch dir: {e}")))?;
        let doc = dir.path().join("plot.vsz");
        std::fs::write(&doc, document)
            .map_err(|e| PlotError::Render(format!("write document: {e}")))?;
        let out = dir.path().join(format!("plot.{}", format.extension()));
        let veusz = std::env::var("KOINE_VEUSZ").unwrap_or_else(|_| "veusz".into());
        let status = std::process::Command::new(&veusz)
            .arg(format!("--export={}", out.display()))
            .arg(&doc)
            .output()
            .map_err(|e| PlotError::Render(format!("run {veusz}: {e}")))?;
        if !status.status.success() {
            return Err(PlotError::Render(format!(
                "{veusz} failed: {}",
                String::from_utf8_lossy(&status.stderr).trim()
            )));
        }
        let body =
            std::fs::read(&out).map_err(|e| PlotError::Render(format!("read export: {e}")))?;
        Ok(Rendered {
            format,
            body,
            assets: Default::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use koine_plot::{PlotAxis, PlotColor, PlotSeries};

    fn spec() -> PlotSpec {
        let mut spec = PlotSpec {
            title: Some("Decay".into()),
            y_axis: PlotAxis {
                label: Some("N".into()),
                log_scale: true,
                ..Default::default()
            },
            ..Default::default()
        };
        spec.series
            .push(PlotSeries::line(vec![0.0, 1.0], vec![100.0, 37.0], "fit"));
        spec.series.push(
            PlotSeries::scatter(vec![0.0, 1.0], vec![98.0, 40.0], "data")
                .with_error(vec![5.0, 4.0])
                .with_color(PlotColor::Red),
        );
        spec
    }

    #[test]
    fn the_document_has_the_data_the_widgets_and_the_axes() {
        let out = VeuszRenderer
            .render(&spec(), &RenderOptions::default())
            .unwrap();
        assert_eq!(out.format, Format::Vsz);
        let text = out.body_text().unwrap();
        for needle in [
            "SetData('x0', [0, 1])",
            "SetData('y1', [98, 40], negerr=[-5, -4], poserr=[5, 4])",
            "Add('xy', name='series0'",
            "Set('marker', 'circle')",
            "Set('log', True)",
            "Set('label', \"Decay\")",
            "Add('key', name='key1'",
        ] {
            assert!(text.contains(needle), "missing {needle}:\n{text}");
        }
    }

    #[test]
    fn what_it_cannot_draw_it_names() {
        let mut s = spec();
        s.annotations.push(Annotation::Arrow {
            x1: 0.0,
            y1: 0.0,
            x2: 1.0,
            y2: 1.0,
            label: None,
            color: PlotColor::Black,
        });
        assert_eq!(
            VeuszRenderer.unsupported(&s),
            vec!["an arrow annotation".to_string()]
        );
    }

    /// Runs the person's `veusz`; `--ignored` on a machine that has it.
    #[cfg(feature = "exec")]
    #[test]
    #[ignore]
    fn exec_exports_a_png() {
        let out = VeuszRenderer
            .render(&spec(), &RenderOptions::format(Format::Png))
            .unwrap();
        assert!(out.body.starts_with(b"\x89PNG"));
    }

    /// The output parses as Python (Veusz documents are Python too), checked
    /// with `python3`'s own parser when there is one; no plotting library is
    /// needed for that.
    #[test]
    fn the_document_is_valid_python() {
        let text = document(&spec(), &RenderOptions::default());
        let Ok(child) = std::process::Command::new("python3")
            .args(["-c", "import ast, sys; ast.parse(sys.stdin.read())"])
            .stdin(std::process::Stdio::piped())
            .spawn()
        else {
            return;
        };
        let mut child = child;
        use std::io::Write as _;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success(), "not valid Python:\n{text}");
    }
}
