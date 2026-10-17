//! koine-plot's matplotlib backend (docs/plan-koine-kit-moves.md KM-D13).
//!
//! [`MatplotlibRenderer`] turns a [`PlotSpec`] into a self-contained Python
//! script: the data inline, one `matplotlib.pyplot` call per series and
//! annotation, the same axes, limits, scales, legend and grid. The script is
//! the artifact (a person can edit it, re-run it, or paste it into a
//! notebook); with the `exec` feature the renderer also runs it with the
//! person's `python3` and returns the PNG, PDF or SVG it saved.
//!
//! Density-contour series are drawn as a 2-D histogram's contours
//! (`numpy.histogram2d` → `contour`), which matplotlib does natively.

use std::fmt::Write as _;

use koine_plot::render::{chosen_format, Format, PlotRenderer, RenderOptions, Rendered};
use koine_plot::{Annotation, LegendPosition, PlotColor, PlotError, PlotSpec, SeriesStyle};

/// Points per inch: koine-plot's sizes are points, matplotlib's inches.
const POINTS_PER_INCH: f64 = 72.0;

/// The matplotlib renderer. Register it with
/// `koine_plot::render::register_renderer(Arc::new(MatplotlibRenderer))`.
pub struct MatplotlibRenderer;

#[cfg(not(feature = "exec"))]
const FORMATS: &[Format] = &[Format::Python];
#[cfg(feature = "exec")]
const FORMATS: &[Format] = &[Format::Python, Format::Png, Format::Pdf, Format::Svg];

impl PlotRenderer for MatplotlibRenderer {
    fn id(&self) -> &'static str {
        "matplotlib"
    }

    fn formats(&self) -> &'static [Format] {
        FORMATS
    }

    fn unsupported(&self, _spec: &PlotSpec) -> Vec<String> {
        Vec::new()
    }

    fn render(&self, spec: &PlotSpec, options: &RenderOptions) -> Result<Rendered, PlotError> {
        let format = chosen_format(self, options)?;
        let script = script(spec, options, None);
        match format {
            Format::Python => Ok(Rendered::text(Format::Python, script)),
            #[cfg(feature = "exec")]
            other => exec::run(spec, options, other),
            #[cfg(not(feature = "exec"))]
            other => Err(PlotError::Unsupported(format!(
                "matplotlib writes {other:?} only with the exec feature"
            ))),
        }
    }
}

fn py_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

fn py_floats(values: &[f64]) -> String {
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

fn py_color(color: &PlotColor) -> String {
    py_str(&color.css())
}

fn legend_loc(position: LegendPosition) -> &'static str {
    match position {
        LegendPosition::TopRight => "upper right",
        LegendPosition::TopLeft => "upper left",
        LegendPosition::BottomRight => "lower right",
        LegendPosition::BottomLeft => "lower left",
    }
}

/// The script for `spec`. With `save_to`, it saves the figure there instead
/// of showing it.
pub fn script(spec: &PlotSpec, options: &RenderOptions, save_to: Option<&str>) -> String {
    let (w, h) = options.size(spec);
    let mut s = String::new();
    let _ = writeln!(s, "# A koine plot-spec@1.0.0, as matplotlib. Edit freely.");
    let _ = writeln!(s, "import matplotlib");
    if save_to.is_some() {
        let _ = writeln!(s, "matplotlib.use('Agg')");
    }
    let _ = writeln!(s, "import matplotlib.pyplot as plt");
    let _ = writeln!(s, "import numpy as np");
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "fig, ax = plt.subplots(figsize=({:.3}, {:.3}))",
        w / POINTS_PER_INCH,
        h / POINTS_PER_INCH
    );
    for (i, series) in spec.series.iter().enumerate() {
        let x = py_floats(&series.x);
        let y = py_floats(&series.y);
        let color = py_color(&series.color);
        let label = py_str(&series.label);
        let _ = writeln!(s, "\n# series {i}: {}", series.label.replace('\n', " "));
        let _ = writeln!(s, "x{i} = np.array({x})");
        let _ = writeln!(s, "y{i} = np.array({y})");
        match series.style {
            SeriesStyle::Line => {
                let _ = writeln!(
                    s,
                    "ax.plot(x{i}, y{i}, color={color}, linewidth={}, label={label})",
                    series.line_width
                );
            }
            SeriesStyle::Scatter => {
                let _ = writeln!(
                    s,
                    "ax.scatter(x{i}, y{i}, color={color}, s={:.2}, label={label})",
                    (series.point_radius * 2.0).powi(2)
                );
            }
            SeriesStyle::LineScatter => {
                let _ = writeln!(s, "ax.plot(x{i}, y{i}, color={color}, linewidth={}, marker='o', markersize={}, label={label})", series.line_width, series.point_radius * 2.0);
            }
            SeriesStyle::Bar => {
                let _ = writeln!(s, "ax.bar(x{i}, y{i}, color={color}, label={label})");
            }
            SeriesStyle::Step => {
                let _ = writeln!(
                    s,
                    "ax.step(x{i}, y{i}, where='mid', color={color}, linewidth={}, label={label})",
                    series.line_width
                );
            }
            SeriesStyle::Contour => {
                let levels = spec.contours.levels.max(1);
                let _ = writeln!(
                    s,
                    "h{i}, xe{i}, ye{i} = np.histogram2d(x{i}, y{i}, bins=64)"
                );
                let _ = writeln!(s, "xc{i} = 0.5 * (xe{i}[:-1] + xe{i}[1:])");
                let _ = writeln!(s, "yc{i} = 0.5 * (ye{i}[:-1] + ye{i}[1:])");
                let _ = writeln!(
                    s,
                    "cs{i} = ax.contour(xc{i}, yc{i}, h{i}.T, levels={levels}, cmap={})",
                    py_str(spec.colormap.matplotlib_name())
                );
                if spec.contours.labels {
                    let _ = writeln!(s, "ax.clabel(cs{i}, inline=True, fontsize=7)");
                }
            }
        }
        if let (Some(low), Some(high)) = (&series.error_low, &series.error_high) {
            let _ = writeln!(
                s,
                "ax.errorbar(x{i}, y{i}, yerr=[np.array({}), np.array({})], fmt='none', ecolor={color}, capsize=2)",
                py_floats(low),
                py_floats(high)
            );
        }
    }
    for annotation in &spec.annotations {
        match annotation {
            Annotation::Text { x, y, text, color } => {
                let _ = writeln!(
                    s,
                    "ax.text({x}, {y}, {}, color={})",
                    py_str(text),
                    py_color(color)
                );
            }
            Annotation::HLine {
                y,
                label,
                color,
                dash,
            } => {
                let _ = writeln!(
                    s,
                    "ax.axhline({y}, color={}, linestyle={}, label={})",
                    py_color(color),
                    if *dash { "'--'" } else { "'-'" },
                    label.as_deref().map_or("None".into(), py_str)
                );
            }
            Annotation::VLine {
                x,
                label,
                color,
                dash,
            } => {
                let _ = writeln!(
                    s,
                    "ax.axvline({x}, color={}, linestyle={}, label={})",
                    py_color(color),
                    if *dash { "'--'" } else { "'-'" },
                    label.as_deref().map_or("None".into(), py_str)
                );
            }
            Annotation::Arrow {
                x1,
                y1,
                x2,
                y2,
                label,
                color,
            } => {
                let _ = writeln!(s, "ax.annotate({}, xy=({x2}, {y2}), xytext=({x1}, {y1}), color={c}, arrowprops=dict(arrowstyle='->', color={c}))", py_str(label.as_deref().unwrap_or("")), c = py_color(color));
            }
            Annotation::FillBetween {
                x,
                y_low,
                y_high,
                color,
                label,
                opacity,
            } => {
                let _ = writeln!(
                    s,
                    "ax.fill_between({}, {}, {}, color={}, alpha={opacity}, label={})",
                    py_floats(x),
                    py_floats(y_low),
                    py_floats(y_high),
                    py_color(color),
                    label.as_deref().map_or("None".into(), py_str)
                );
            }
        }
    }
    let _ = writeln!(s);
    if let Some(title) = &spec.title {
        let _ = writeln!(s, "ax.set_title({})", py_str(title));
    }
    for (axis, name) in [(&spec.x_axis, "x"), (&spec.y_axis, "y")] {
        if let Some(label) = &axis.label {
            let _ = writeln!(s, "ax.set_{name}label({})", py_str(label));
        }
        if axis.log_scale {
            let _ = writeln!(s, "ax.set_{name}scale('log')");
        }
        if axis.min.is_some() || axis.max.is_some() {
            let bound = |v: Option<f64>| v.map_or("None".to_string(), |v| v.to_string());
            let _ = writeln!(
                s,
                "ax.set_{name}lim({}, {})",
                bound(axis.min),
                bound(axis.max)
            );
        }
    }
    if spec.show_grid {
        let _ = writeln!(s, "ax.grid(True, alpha=0.3)");
    }
    let labelled = spec.series.iter().any(|s| !s.label.is_empty());
    if spec.legend.visible && labelled {
        let _ = writeln!(
            s,
            "ax.legend(loc={})",
            py_str(legend_loc(spec.legend.position))
        );
    }
    let _ = writeln!(s, "fig.tight_layout()");
    match save_to {
        Some(path) => {
            let _ = writeln!(s, "fig.savefig({})", py_str(path));
        }
        None => {
            let _ = writeln!(s, "plt.show()");
        }
    }
    s
}

#[cfg(feature = "exec")]
mod exec {
    use super::*;

    /// Run the script with `python3` (or `$KOINE_PYTHON`) and read back the
    /// file it saved.
    pub fn run(
        spec: &PlotSpec,
        options: &RenderOptions,
        format: Format,
    ) -> Result<Rendered, PlotError> {
        let dir =
            tempfile::tempdir().map_err(|e| PlotError::Render(format!("scratch dir: {e}")))?;
        let out = dir.path().join(format!("plot.{}", format.extension()));
        let path = out.to_string_lossy().into_owned();
        let script_path = dir.path().join("plot.py");
        std::fs::write(&script_path, script(spec, options, Some(&path)))
            .map_err(|e| PlotError::Render(format!("write script: {e}")))?;
        let python = std::env::var("KOINE_PYTHON").unwrap_or_else(|_| "python3".into());
        let status = std::process::Command::new(&python)
            .arg(&script_path)
            .output()
            .map_err(|e| PlotError::Render(format!("run {python}: {e}")))?;
        if !status.status.success() {
            return Err(PlotError::Render(format!(
                "{python} failed: {}",
                String::from_utf8_lossy(&status.stderr).trim()
            )));
        }
        let body =
            std::fs::read(&out).map_err(|e| PlotError::Render(format!("read {path}: {e}")))?;
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
    use koine_plot::{PlotAxis, PlotSeries};

    fn spec() -> PlotSpec {
        let mut spec = PlotSpec {
            title: Some("Decay".into()),
            x_axis: PlotAxis {
                label: Some("t".into()),
                ..Default::default()
            },
            y_axis: PlotAxis {
                label: Some("N".into()),
                log_scale: true,
                min: Some(1.0),
                ..Default::default()
            },
            ..Default::default()
        };
        spec.series.push(PlotSeries::line(
            vec![0.0, 1.0, 2.0],
            vec![100.0, 37.0, 13.5],
            "fit",
        ));
        spec.series.push(
            PlotSeries::scatter(vec![0.0, 1.0, 2.0], vec![98.0, 40.0, 12.0], "data")
                .with_error(vec![5.0, 4.0, 2.0])
                .with_color(PlotColor::Red),
        );
        spec.annotations.push(Annotation::HLine {
            y: 50.0,
            label: Some("half".into()),
            color: PlotColor::Gray,
            dash: true,
        });
        spec
    }

    #[test]
    fn the_script_carries_every_series_axis_and_annotation() {
        let out = MatplotlibRenderer
            .render(&spec(), &RenderOptions::default())
            .unwrap();
        assert_eq!(out.format, Format::Python);
        let text = out.body_text().unwrap();
        for needle in [
            "ax.plot(x0, y0",
            "ax.scatter(x1, y1",
            "ax.errorbar(x1, y1",
            "ax.axhline(50",
            "ax.set_title(\"Decay\")",
            "ax.set_yscale('log')",
            "ax.set_ylim(1, None)",
            "ax.legend(loc=\"upper right\")",
            "plt.show()",
        ] {
            assert!(text.contains(needle), "missing {needle}:\n{text}");
        }
    }

    #[test]
    fn a_label_is_quoted_not_injected() {
        let mut s = spec();
        s.title = Some("a\") ; import os; os.system(\"x".into());
        let text = script(&s, &RenderOptions::default(), None);
        assert!(
            text.contains(r#"ax.set_title("a\") ; import os; os.system(\"x")"#),
            "{text}"
        );
    }

    #[cfg(not(feature = "exec"))]
    #[test]
    fn an_image_needs_exec() {
        let err = MatplotlibRenderer
            .render(&spec(), &RenderOptions::format(Format::Png))
            .unwrap_err();
        assert!(err.to_string().contains("does not write"), "{err}");
    }

    /// Runs the person's `python3`; `--ignored` on a machine with matplotlib.
    #[cfg(feature = "exec")]
    #[test]
    #[ignore]
    fn exec_writes_a_png() {
        let out = MatplotlibRenderer
            .render(&spec(), &RenderOptions::format(Format::Png))
            .unwrap();
        assert!(out.body.starts_with(b"\x89PNG"));
    }

    /// The output parses as Python (Veusz documents are Python too), checked
    /// with `python3`'s own parser when there is one; no plotting library is
    /// needed for that.
    #[test]
    fn the_script_is_valid_python() {
        let text = script(&spec(), &RenderOptions::default(), None);
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
