//! Pure-Rust SVG generation from a `PlotSpec`.
//!
//! Produces a standalone `<svg>` string with:
//! - Axes with labeled ticks (reusing `axis.rs` logic)
//! - Grid lines
//! - Data polylines, scatter circles, step lines
//! - Error bar segments with caps
//! - Legend box
//! - Title text

use crate::axis::{calculate_ticks, AxisConfig, AxisPosition, ScaleType};
use crate::spec::*;

/// Render a `PlotSpec` to a standalone SVG string.
pub fn render_svg(spec: &PlotSpec) -> String {
    let mut svg = SvgBuilder::new(spec.width, spec.height);

    // Layout: one function owns the margins and the bounds.
    let frame = frame(spec);
    let Frame {
        plot_x,
        plot_y,
        plot_w,
        plot_h,
        x_min,
        x_max,
        y_min,
        y_max,
        ..
    } = frame;

    // Build axis configs
    let x_axis_config = AxisConfig {
        position: AxisPosition::Bottom,
        scale: if spec.x_axis.log_scale {
            ScaleType::Log10
        } else {
            ScaleType::Linear
        },
        min: x_min,
        max: x_max,
        label: spec.x_axis.label.clone(),
        unit: None,
        show_spine: true,
        show_ticks: true,
        show_labels: true,
        show_grid: spec.show_grid,
        format: spec.x_axis.format.clone(),
        tick_length: 5.0,
        minor_ticks: 4,
    };

    let y_axis_config = AxisConfig {
        position: AxisPosition::Left,
        scale: if spec.y_axis.log_scale {
            ScaleType::Log10
        } else {
            ScaleType::Linear
        },
        min: y_min,
        max: y_max,
        label: spec.y_axis.label.clone(),
        unit: None,
        show_spine: true,
        show_ticks: true,
        show_labels: true,
        show_grid: spec.show_grid,
        format: spec.y_axis.format.clone(),
        tick_length: 5.0,
        minor_ticks: 4,
    };

    // Background
    svg.rect(0.0, 0.0, spec.width, spec.height, "#ffffff", None, 0.0);
    // Plot area background
    svg.rect(
        plot_x,
        plot_y,
        plot_w,
        plot_h,
        "#fafafa",
        Some(("#cccccc", 0.5)),
        0.0,
    );

    // Coordinate transform closures
    let to_px = |x: f64, y: f64| -> (f64, f64) { frame.to_pixel(x, y) };

    // Grid lines
    if spec.show_grid {
        let x_ticks = calculate_ticks(&x_axis_config);
        for t in &x_ticks {
            if !t.is_major {
                continue;
            }
            let px = plot_x + t.normalized * plot_w;
            svg.line(px, plot_y, px, plot_y + plot_h, "#e0e0e0", 0.5, None);
        }
        let y_ticks = calculate_ticks(&y_axis_config);
        for t in &y_ticks {
            if !t.is_major {
                continue;
            }
            let py = plot_y + (1.0 - t.normalized) * plot_h;
            svg.line(plot_x, py, plot_x + plot_w, py, "#e0e0e0", 0.5, None);
        }
    }

    // Clip path for data
    let clip_id = "plot-area";
    svg.push(&format!(
        r#"<defs><clipPath id="{}"><rect x="{}" y="{}" width="{}" height="{}"/></clipPath></defs>"#,
        clip_id, plot_x, plot_y, plot_w, plot_h
    ));

    // Render each series (inside clip)
    svg.push(&format!(r#"<g clip-path="url(#{})">"#, clip_id));

    for series in &spec.series {
        let css_color = series.color.css();

        // Error bars first (behind data)
        if let (Some(lo), Some(hi)) = (&series.error_low, &series.error_high) {
            render_error_bars(&mut svg, series, lo, hi, &css_color, &to_px);
        }

        match series.style {
            SeriesStyle::Line | SeriesStyle::LineScatter => {
                render_polyline(&mut svg, series, &css_color, &to_px);
                if series.style == SeriesStyle::LineScatter {
                    render_scatter(&mut svg, series, &css_color, &to_px);
                }
            }
            // The SVG renderer draws a contour series as its point cloud
            // (render::SvgRenderer::unsupported says so).
            SeriesStyle::Scatter | SeriesStyle::Contour => {
                render_scatter(&mut svg, series, &css_color, &to_px);
            }
            SeriesStyle::Step => {
                render_step(&mut svg, series, &css_color, &to_px);
            }
            SeriesStyle::Bar => {
                render_bars(
                    &mut svg,
                    series,
                    &css_color,
                    &to_px,
                    plot_y + plot_h,
                    y_min,
                    y_max,
                    spec.y_axis.log_scale,
                );
            }
        }
    }

    // Render annotations (inside clip group)
    render_annotations(
        &mut svg,
        &spec.annotations,
        &to_px,
        plot_x,
        plot_y,
        plot_w,
        plot_h,
    );

    svg.push("</g>"); // end clip group

    // Axes
    render_axes(
        &mut svg,
        &x_axis_config,
        &y_axis_config,
        plot_x,
        plot_y,
        plot_w,
        plot_h,
    );

    // Title
    if let Some(title) = &spec.title {
        svg.text(
            spec.width / 2.0,
            plot_y / 2.0 + 4.0,
            title,
            "middle",
            "14px",
            "#333333",
            Some("bold"),
        );
    }

    // Legend
    if spec.legend.visible
        && !spec.series.is_empty()
        && spec.series.iter().any(|s| !s.label.is_empty())
    {
        render_legend(&mut svg, spec, plot_x, plot_y, plot_w, plot_h);
    }

    svg.finish()
}

// ── The plot frame ──────────────────────────────────────────────────

/// The plot area and the axis ranges [`render_svg`] drew with, in the SVG's
/// viewBox units.
///
/// `plot_x/plot_y/plot_w/plot_h` are the rectangle the data is drawn in;
/// `x_min..x_max` and `y_min..y_max` are the (padded) data ranges it maps
/// onto, `x_log`/`y_log` whether an axis is a base-10 log axis. A page that
/// receives a click in element coordinates maps it back through
/// [`Frame::to_data`], which is the inverse of the [`Frame::to_pixel`] the
/// renderer itself calls for every point: one arithmetic, two directions.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Frame {
    pub width: f64,
    pub height: f64,
    pub plot_x: f64,
    pub plot_y: f64,
    pub plot_w: f64,
    pub plot_h: f64,
    pub x_min: f64,
    pub x_max: f64,
    pub y_min: f64,
    pub y_max: f64,
    pub x_log: bool,
    pub y_log: bool,
}

/// The frame of `spec`: margins for axes, title and labels, and the padded
/// data bounds. [`render_svg`] draws from this and nothing else.
pub fn frame(spec: &PlotSpec) -> Frame {
    let has_title = spec.title.is_some();
    let margin_top = if has_title { 40.0 } else { 20.0 };
    let margin_bottom = if spec.x_axis.label.is_some() {
        60.0
    } else {
        45.0
    };
    let margin_left = if spec.y_axis.label.is_some() {
        70.0
    } else {
        55.0
    };
    let margin_right = 20.0;

    let (x_min, x_max, y_min, y_max) = compute_bounds(spec);
    Frame {
        width: spec.width,
        height: spec.height,
        plot_x: margin_left,
        plot_y: margin_top,
        plot_w: (spec.width - margin_left - margin_right).max(1.0),
        plot_h: (spec.height - margin_top - margin_bottom).max(1.0),
        x_min,
        x_max,
        y_min,
        y_max,
        x_log: spec.x_axis.log_scale,
        y_log: spec.y_axis.log_scale,
    }
}

impl Frame {
    /// Data coordinates to a pixel in the SVG's viewBox units.
    pub fn to_pixel(&self, x: f64, y: f64) -> (f64, f64) {
        let xn = normalize(x, self.x_min, self.x_max, self.x_log);
        let yn = normalize(y, self.y_min, self.y_max, self.y_log);
        let px = self.plot_x + xn * self.plot_w;
        let py = self.plot_y + (1.0 - yn) * self.plot_h;
        (px, py)
    }

    /// A pixel in the SVG's viewBox units to data coordinates; `None` when
    /// the pixel is outside the plot area (or not finite). Log axes are
    /// inverted through `10^`.
    pub fn to_data(&self, px: f64, py: f64) -> Option<(f64, f64)> {
        if !px.is_finite() || !py.is_finite() {
            return None;
        }
        if px < self.plot_x
            || px > self.plot_x + self.plot_w
            || py < self.plot_y
            || py > self.plot_y + self.plot_h
        {
            return None;
        }
        let xn = (px - self.plot_x) / self.plot_w;
        let yn = 1.0 - (py - self.plot_y) / self.plot_h;
        Some((
            denormalize(xn, self.x_min, self.x_max, self.x_log)?,
            denormalize(yn, self.y_min, self.y_max, self.y_log)?,
        ))
    }
}

// ── Coordinate normalization ────────────────────────────────────────

fn normalize(v: f64, min: f64, max: f64, log_scale: bool) -> f64 {
    if log_scale {
        if v <= 0.0 || min <= 0.0 || max <= 0.0 {
            return 0.0;
        }
        let log_min = min.log10();
        let log_max = max.log10();
        let range = log_max - log_min;
        if range == 0.0 {
            return 0.5;
        }
        (v.log10() - log_min) / range
    } else {
        let range = max - min;
        if range == 0.0 {
            return 0.5;
        }
        (v - min) / range
    }
}

/// The inverse of [`normalize`]; `None` where a log axis has no positive range.
fn denormalize(n: f64, min: f64, max: f64, log_scale: bool) -> Option<f64> {
    if log_scale {
        if min <= 0.0 || max <= 0.0 {
            return None;
        }
        let log_min = min.log10();
        let log_max = max.log10();
        Some(10f64.powf(log_min + n * (log_max - log_min)))
    } else {
        Some(min + n * (max - min))
    }
}

// ── Bounds computation ──────────────────────────────────────────────

fn compute_bounds(spec: &PlotSpec) -> (f64, f64, f64, f64) {
    let mut x_min = f64::INFINITY;
    let mut x_max = f64::NEG_INFINITY;
    let mut y_min = f64::INFINITY;
    let mut y_max = f64::NEG_INFINITY;

    for series in &spec.series {
        for &v in &series.x {
            if v.is_finite() {
                if v < x_min {
                    x_min = v;
                }
                if v > x_max {
                    x_max = v;
                }
            }
        }
        for (i, &v) in series.y.iter().enumerate() {
            if !v.is_finite() {
                continue;
            }

            let lo = series
                .error_low
                .as_ref()
                .and_then(|e| e.get(i).copied())
                .unwrap_or(0.0);
            let hi = series
                .error_high
                .as_ref()
                .and_then(|e| e.get(i).copied())
                .unwrap_or(0.0);

            let val_lo = v - lo;
            let val_hi = v + hi;
            if val_lo < y_min {
                y_min = val_lo;
            }
            if val_hi > y_max {
                y_max = val_hi;
            }
        }
    }

    // Apply explicit axis bounds
    if let Some(v) = spec.x_axis.min {
        x_min = v;
    }
    if let Some(v) = spec.x_axis.max {
        x_max = v;
    }
    if let Some(v) = spec.y_axis.min {
        y_min = v;
    }
    if let Some(v) = spec.y_axis.max {
        y_max = v;
    }

    // Ensure valid range
    if !x_min.is_finite() || !x_max.is_finite() {
        x_min = 0.0;
        x_max = 1.0;
    }
    if !y_min.is_finite() || !y_max.is_finite() {
        y_min = 0.0;
        y_max = 1.0;
    }

    // Add padding (5% on each side)
    let x_range = x_max - x_min;
    let y_range = y_max - y_min;
    let x_pad = if x_range == 0.0 { 0.5 } else { x_range * 0.05 };
    let y_pad = if y_range == 0.0 { 0.5 } else { y_range * 0.05 };

    (x_min - x_pad, x_max + x_pad, y_min - y_pad, y_max + y_pad)
}

// ── Series renderers ────────────────────────────────────────────────

fn render_polyline(
    svg: &mut SvgBuilder,
    series: &PlotSeries,
    color: &str,
    to_px: &dyn Fn(f64, f64) -> (f64, f64),
) {
    let mut points = String::new();
    let n = series.x.len().min(series.y.len());
    for i in 0..n {
        let (x, y) = (series.x[i], series.y[i]);
        if !x.is_finite() || !y.is_finite() {
            continue;
        }
        let (px, py) = to_px(x, y);
        if !points.is_empty() {
            points.push(' ');
        }
        points.push_str(&format!("{:.2},{:.2}", px, py));
    }
    if !points.is_empty() {
        svg.push(&format!(
            r#"<polyline points="{}" fill="none" stroke="{}" stroke-width="{}" stroke-linejoin="round"/>"#,
            points, color, series.line_width
        ));
    }
}

fn render_scatter(
    svg: &mut SvgBuilder,
    series: &PlotSeries,
    color: &str,
    to_px: &dyn Fn(f64, f64) -> (f64, f64),
) {
    let n = series.x.len().min(series.y.len());
    for i in 0..n {
        let (x, y) = (series.x[i], series.y[i]);
        if !x.is_finite() || !y.is_finite() {
            continue;
        }
        let (px, py) = to_px(x, y);
        svg.circle(px, py, series.point_radius, color, Some(("white", 0.8)));
    }
}

fn render_step(
    svg: &mut SvgBuilder,
    series: &PlotSeries,
    color: &str,
    to_px: &dyn Fn(f64, f64) -> (f64, f64),
) {
    let n = series.x.len().min(series.y.len());
    if n == 0 {
        return;
    }

    let mut points = String::new();
    for i in 0..n {
        let (x, y) = (series.x[i], series.y[i]);
        if !x.is_finite() || !y.is_finite() {
            continue;
        }
        let (px, py) = to_px(x, y);

        // Step: horizontal to new x, then vertical to new y
        if i > 0 && series.x[i - 1].is_finite() && series.y[i - 1].is_finite() {
            let (_, prev_py) = to_px(series.x[i - 1], series.y[i - 1]);
            points.push_str(&format!(" {:.2},{:.2}", px, prev_py));
        }
        if !points.is_empty() {
            points.push(' ');
        }
        points.push_str(&format!("{:.2},{:.2}", px, py));
    }
    if !points.is_empty() {
        svg.push(&format!(
            r#"<polyline points="{}" fill="none" stroke="{}" stroke-width="{}" stroke-linejoin="round"/>"#,
            points, color, series.line_width
        ));
    }
}

// Plot-render signatures mirror the SVG element parameter lists;
// bundling into a struct is the implore owner's refactor call.
#[allow(clippy::too_many_arguments)]
fn render_bars(
    svg: &mut SvgBuilder,
    series: &PlotSeries,
    color: &str,
    to_px: &dyn Fn(f64, f64) -> (f64, f64),
    baseline_py: f64,
    y_min: f64,
    _y_max: f64,
    log_scale_y: bool,
) {
    let n = series.x.len().min(series.y.len());
    if n < 2 {
        return;
    }

    // Bar width from average x spacing
    let bar_width = if n > 1 {
        let total_range = series.x[n - 1] - series.x[0];
        total_range / (n as f64) * 0.8
    } else {
        1.0
    };

    let baseline_y = if log_scale_y {
        y_min.max(1e-10)
    } else {
        0.0_f64.max(y_min)
    };

    for i in 0..n {
        let (x, y) = (series.x[i], series.y[i]);
        if !x.is_finite() || !y.is_finite() {
            continue;
        }

        let (px_left, py_top) = to_px(x - bar_width / 2.0, y);
        let (px_right, _) = to_px(x + bar_width / 2.0, y);
        let (_, py_baseline) = to_px(x, baseline_y);
        let py_bottom = py_baseline.min(baseline_py);

        let bw = (px_right - px_left).max(1.0);
        let bh = (py_bottom - py_top).max(0.0);
        svg.rect(px_left, py_top, bw, bh, color, Some((color, 0.5)), 0.6);
    }
}

fn render_error_bars(
    svg: &mut SvgBuilder,
    series: &PlotSeries,
    lo: &[f64],
    hi: &[f64],
    color: &str,
    to_px: &dyn Fn(f64, f64) -> (f64, f64),
) {
    let cap_half = 3.0;
    let n = series
        .x
        .len()
        .min(series.y.len())
        .min(lo.len())
        .min(hi.len());

    for i in 0..n {
        let (x, y) = (series.x[i], series.y[i]);
        if !x.is_finite() || !y.is_finite() {
            continue;
        }

        let y_lo = y - lo[i];
        let y_hi = y + hi[i];
        let (px, py_lo) = to_px(x, y_lo);
        let (_, py_hi) = to_px(x, y_hi);

        // Vertical line
        svg.line(px, py_lo, px, py_hi, color, 1.0, None);
        // Bottom cap
        svg.line(px - cap_half, py_lo, px + cap_half, py_lo, color, 1.0, None);
        // Top cap
        svg.line(px - cap_half, py_hi, px + cap_half, py_hi, color, 1.0, None);
    }
}

// ── Axes ────────────────────────────────────────────────────────────

fn render_axes(
    svg: &mut SvgBuilder,
    x_config: &AxisConfig,
    y_config: &AxisConfig,
    plot_x: f64,
    plot_y: f64,
    plot_w: f64,
    plot_h: f64,
) {
    // X axis (bottom)
    let x_bottom = plot_y + plot_h;
    svg.line(
        plot_x,
        x_bottom,
        plot_x + plot_w,
        x_bottom,
        "#333333",
        1.0,
        None,
    );

    let x_ticks = calculate_ticks(x_config);
    for t in &x_ticks {
        let px = plot_x + t.normalized * plot_w;
        let tick_len = if t.is_major { 5.0 } else { 3.0 };
        svg.line(px, x_bottom, px, x_bottom + tick_len, "#333333", 1.0, None);

        if let Some(label) = &t.label {
            svg.text(
                px,
                x_bottom + 16.0,
                label,
                "middle",
                "10px",
                "#555555",
                None,
            );
        }
    }

    if let Some(label) = x_config.full_label() {
        svg.text(
            plot_x + plot_w / 2.0,
            x_bottom + 35.0,
            &label,
            "middle",
            "12px",
            "#333333",
            None,
        );
    }

    // Y axis (left)
    svg.line(
        plot_x,
        plot_y,
        plot_x,
        plot_y + plot_h,
        "#333333",
        1.0,
        None,
    );

    let y_ticks = calculate_ticks(y_config);
    for t in &y_ticks {
        let py = plot_y + (1.0 - t.normalized) * plot_h;
        let tick_len = if t.is_major { 5.0 } else { 3.0 };
        svg.line(plot_x - tick_len, py, plot_x, py, "#333333", 1.0, None);

        if let Some(label) = &t.label {
            svg.text(
                plot_x - 8.0,
                py + 3.0,
                label,
                "end",
                "10px",
                "#555555",
                None,
            );
        }
    }

    if let Some(label) = y_config.full_label() {
        // Rotated Y axis label
        let lx = plot_x - 45.0;
        let ly = plot_y + plot_h / 2.0;
        svg.push(&format!(
            "<text x=\"{lx:.1}\" y=\"{ly:.1}\" text-anchor=\"middle\" font-size=\"12px\" fill=\"#333333\" transform=\"rotate(-90 {lx:.1} {ly:.1})\">{}</text>",
            escape_xml(&label),
        ));
    }
}

// ── Legend ───────────────────────────────────────────────────────────

fn render_legend(
    svg: &mut SvgBuilder,
    spec: &PlotSpec,
    plot_x: f64,
    plot_y: f64,
    plot_w: f64,
    _plot_h: f64,
) {
    let labeled: Vec<&PlotSeries> = spec.series.iter().filter(|s| !s.label.is_empty()).collect();
    if labeled.is_empty() {
        return;
    }

    let line_h = 18.0;
    let legend_w = 120.0;
    let legend_h = labeled.len() as f64 * line_h + 10.0;
    let padding = 8.0;

    let (lx, ly) = match spec.legend.position {
        LegendPosition::TopRight => (plot_x + plot_w - legend_w - padding, plot_y + padding),
        LegendPosition::TopLeft => (plot_x + padding, plot_y + padding),
        LegendPosition::BottomRight => (
            plot_x + plot_w - legend_w - padding,
            plot_y + _plot_h - legend_h - padding,
        ),
        LegendPosition::BottomLeft => (plot_x + padding, plot_y + _plot_h - legend_h - padding),
    };

    // Legend background
    svg.push(&format!(
        "<rect x=\"{lx:.1}\" y=\"{ly:.1}\" width=\"{legend_w:.1}\" height=\"{legend_h:.1}\" fill=\"white\" fill-opacity=\"0.9\" stroke=\"#cccccc\" stroke-width=\"0.5\" rx=\"3\"/>",
    ));

    for (i, series) in labeled.iter().enumerate() {
        let y = ly + 5.0 + (i as f64 + 0.5) * line_h;
        let x_sym = lx + 10.0;
        let css = series.color.css();

        // Symbol
        match series.style {
            SeriesStyle::Line | SeriesStyle::Step => {
                svg.line(x_sym, y, x_sym + 18.0, y, &css, series.line_width, None);
            }
            SeriesStyle::Scatter | SeriesStyle::Contour => {
                svg.circle(x_sym + 9.0, y, series.point_radius, &css, None);
            }
            SeriesStyle::LineScatter => {
                svg.line(x_sym, y, x_sym + 18.0, y, &css, series.line_width, None);
                svg.circle(x_sym + 9.0, y, series.point_radius, &css, None);
            }
            SeriesStyle::Bar => {
                svg.rect(x_sym, y - 5.0, 18.0, 10.0, &css, None, 0.6);
            }
        }

        // Label
        svg.text(
            x_sym + 24.0,
            y + 4.0,
            &series.label,
            "start",
            "10px",
            "#333333",
            None,
        );
    }
}

// ── Annotations ─────────────────────────────────────────────────────

fn render_annotations(
    svg: &mut SvgBuilder,
    annotations: &[Annotation],
    to_px: &dyn Fn(f64, f64) -> (f64, f64),
    plot_x: f64,
    plot_y: f64,
    plot_w: f64,
    plot_h: f64,
) {
    for ann in annotations {
        match ann {
            Annotation::HLine {
                y,
                label,
                color,
                dash,
            } => {
                let css = color.css();
                let (_, py) = to_px(0.0, *y);
                let dash_str = if *dash { Some("6,3") } else { None };
                svg.line(plot_x, py, plot_x + plot_w, py, &css, 1.0, dash_str);
                if let Some(text) = label {
                    svg.text(
                        plot_x + plot_w - 4.0,
                        py - 4.0,
                        text,
                        "end",
                        "9px",
                        &css,
                        None,
                    );
                }
            }
            Annotation::VLine {
                x,
                label,
                color,
                dash,
            } => {
                let css = color.css();
                let (px, _) = to_px(*x, 0.0);
                let dash_str = if *dash { Some("6,3") } else { None };
                svg.line(px, plot_y, px, plot_y + plot_h, &css, 1.0, dash_str);
                if let Some(text) = label {
                    svg.text(px + 4.0, plot_y + 12.0, text, "start", "9px", &css, None);
                }
            }
            Annotation::Text { x, y, text, color } => {
                let css = color.css();
                let (px, py) = to_px(*x, *y);
                svg.text(px, py - 4.0, text, "start", "10px", &css, None);
            }
            Annotation::Arrow {
                x1,
                y1,
                x2,
                y2,
                label,
                color,
            } => {
                let css = color.css();
                let (px1, py1) = to_px(*x1, *y1);
                let (px2, py2) = to_px(*x2, *y2);
                // Arrow line
                svg.line(px1, py1, px2, py2, &css, 1.0, None);
                // Arrowhead (simple triangle)
                let dx = px2 - px1;
                let dy = py2 - py1;
                let len = (dx * dx + dy * dy).sqrt();
                if len > 0.0 {
                    let ux = dx / len;
                    let uy = dy / len;
                    let head_len = 8.0;
                    let head_w = 4.0;
                    let bx = px2 - ux * head_len;
                    let by = py2 - uy * head_len;
                    let points = format!(
                        "{:.1},{:.1} {:.1},{:.1} {:.1},{:.1}",
                        px2,
                        py2,
                        bx + uy * head_w,
                        by - ux * head_w,
                        bx - uy * head_w,
                        by + ux * head_w,
                    );
                    svg.push(&format!(
                        "<polygon points=\"{}\" fill=\"{}\"/>",
                        points, css,
                    ));
                }
                if let Some(text) = label {
                    svg.text(
                        (px1 + px2) / 2.0,
                        (py1 + py2) / 2.0 - 4.0,
                        text,
                        "middle",
                        "9px",
                        &css,
                        None,
                    );
                }
            }
            Annotation::FillBetween {
                x,
                y_low,
                y_high,
                color,
                opacity,
                ..
            } => {
                let css = color.css();
                let n = x.len().min(y_low.len()).min(y_high.len());
                if n == 0 {
                    continue;
                }

                let mut path = String::new();
                // Upper boundary (forward)
                for i in 0..n {
                    let (px, py) = to_px(x[i], y_high[i]);
                    if i == 0 {
                        path.push_str(&format!("M{:.2},{:.2}", px, py));
                    } else {
                        path.push_str(&format!(" L{:.2},{:.2}", px, py));
                    }
                }
                // Lower boundary (backward)
                for i in (0..n).rev() {
                    let (px, py) = to_px(x[i], y_low[i]);
                    path.push_str(&format!(" L{:.2},{:.2}", px, py));
                }
                path.push_str(" Z");

                svg.push(&format!(
                    "<path d=\"{}\" fill=\"{}\" fill-opacity=\"{}\" stroke=\"none\"/>",
                    path, css, opacity,
                ));
            }
        }
    }
}

// ── Grid rendering ──────────────────────────────────────────────────

/// Render a `PlotGrid` to a standalone SVG string.
///
/// Each cell is rendered as an independent `PlotSpec` within its allocated
/// rectangle. Shared axes suppress redundant labels.
pub fn render_grid_svg(grid: &PlotGrid) -> String {
    let mut svg = SvgBuilder::new(grid.width, grid.height);

    let title_h = if grid.title.is_some() { 30.0 } else { 0.0 };
    let outer_pad = 10.0;

    let avail_w = grid.width - 2.0 * outer_pad;
    let avail_h = grid.height - 2.0 * outer_pad - title_h;

    let cell_w = (avail_w - (grid.cols as f64 - 1.0) * grid.h_gap) / grid.cols as f64;
    let cell_h = (avail_h - (grid.rows as f64 - 1.0) * grid.v_gap) / grid.rows as f64;

    // Background
    svg.rect(0.0, 0.0, grid.width, grid.height, "#ffffff", None, 0.0);

    // Title
    if let Some(title) = &grid.title {
        svg.text(
            grid.width / 2.0,
            title_h / 2.0 + 6.0,
            title,
            "middle",
            "16px",
            "#333333",
            Some("bold"),
        );
    }

    for row in 0..grid.rows {
        for col in 0..grid.cols {
            let idx = row * grid.cols + col;
            if let Some(Some(spec)) = grid.cells.get(idx) {
                let x = outer_pad + col as f64 * (cell_w + grid.h_gap);
                let y = outer_pad + title_h + row as f64 * (cell_h + grid.v_gap);

                // Render subplot to SVG fragment and embed via group transform
                let mut cell_spec = spec.clone();
                cell_spec.width = cell_w;
                cell_spec.height = cell_h;

                // Suppress labels on shared axes
                if grid.share_x && row < grid.rows - 1 {
                    cell_spec.x_axis.label = None;
                }
                if grid.share_y && col > 0 {
                    cell_spec.y_axis.label = None;
                }

                let cell_svg = render_svg(&cell_spec);

                // Extract content between <svg> tags and embed in a <g transform>
                if let (Some(start), Some(end)) = (cell_svg.find('>'), cell_svg.rfind("</svg>")) {
                    let inner = &cell_svg[start + 1..end];
                    svg.push(&format!(
                        "<g transform=\"translate({:.1},{:.1})\">{}</g>",
                        x, y, inner,
                    ));
                }
            }
        }
    }

    svg.finish()
}

// ── SVG builder ─────────────────────────────────────────────────────

#[allow(dead_code)]
struct SvgBuilder {
    parts: Vec<String>,
    width: f64,
    height: f64,
}

impl SvgBuilder {
    fn new(width: f64, height: f64) -> Self {
        Self {
            parts: vec![format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {} {}" width="{}" height="{}" font-family="system-ui, -apple-system, sans-serif">"#,
                width, height, width, height,
            )],
            width,
            height,
        }
    }

    fn push(&mut self, s: &str) {
        self.parts.push(s.to_string());
    }

    #[allow(clippy::too_many_arguments)]
    fn line(
        &mut self,
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        color: &str,
        width: f64,
        dash: Option<&str>,
    ) {
        let dash_attr = dash.map_or(String::new(), |d| format!(r#" stroke-dasharray="{}""#, d));
        self.parts.push(format!(
            r#"<line x1="{:.2}" y1="{:.2}" x2="{:.2}" y2="{:.2}" stroke="{}" stroke-width="{}"{}/>"#,
            x1, y1, x2, y2, color, width, dash_attr,
        ));
    }

    #[allow(clippy::too_many_arguments)]
    fn rect(
        &mut self,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        fill: &str,
        stroke: Option<(&str, f64)>,
        opacity: f64,
    ) {
        let stroke_attr = stroke.map_or(String::new(), |(c, w)| {
            format!(r#" stroke="{}" stroke-width="{}""#, c, w)
        });
        let opacity_attr = if opacity > 0.0 && opacity < 1.0 {
            format!(r#" fill-opacity="{}""#, opacity)
        } else {
            String::new()
        };
        self.parts.push(format!(
            r#"<rect x="{:.2}" y="{:.2}" width="{:.2}" height="{:.2}" fill="{}"{}{}/>"#,
            x, y, w, h, fill, stroke_attr, opacity_attr,
        ));
    }

    fn circle(&mut self, cx: f64, cy: f64, r: f64, fill: &str, stroke: Option<(&str, f64)>) {
        let stroke_attr = stroke.map_or(String::new(), |(c, w)| {
            format!(r#" stroke="{}" stroke-width="{}""#, c, w)
        });
        self.parts.push(format!(
            r#"<circle cx="{:.2}" cy="{:.2}" r="{:.1}" fill="{}"{}/>"#,
            cx, cy, r, fill, stroke_attr,
        ));
    }

    #[allow(clippy::too_many_arguments)]
    fn text(
        &mut self,
        x: f64,
        y: f64,
        content: &str,
        anchor: &str,
        size: &str,
        fill: &str,
        weight: Option<&str>,
    ) {
        let weight_attr = weight.map_or(String::new(), |w| format!(r#" font-weight="{}""#, w));
        self.parts.push(format!(
            r#"<text x="{:.2}" y="{:.2}" text-anchor="{}" font-size="{}" fill="{}"{} dominant-baseline="auto">{}</text>"#,
            x, y, anchor, size, fill, weight_attr, escape_xml(content),
        ));
    }

    fn finish(mut self) -> String {
        self.parts.push("</svg>".to_string());
        self.parts.join("\n")
    }
}

/// Escape XML special characters.
pub(crate) fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ── Convenience function ────────────────────────────────────────────

/// Create a simple line plot SVG from x, y arrays and labels.
pub fn create_line_plot_svg(
    title: &str,
    x: &[f64],
    y: &[f64],
    x_label: &str,
    y_label: &str,
) -> String {
    let spec = PlotSpec::new()
        .with_title(title)
        .with_x_label(x_label)
        .with_y_label(y_label)
        .line(x.to_vec(), y.to_vec(), title);
    render_svg(&spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_simple_line() {
        let spec = PlotSpec::new().with_title("Test").line(
            vec![0.0, 1.0, 2.0],
            vec![0.0, 1.0, 4.0],
            "y=x^2",
        );
        let svg = render_svg(&spec);
        assert!(svg.starts_with("<svg"));
        assert!(svg.ends_with("</svg>"));
        assert!(svg.contains("Test"));
        assert!(svg.contains("polyline"));
    }

    #[test]
    fn test_render_scatter() {
        let spec = PlotSpec::new().scatter(vec![0.0, 1.0, 2.0], vec![0.0, 1.0, 4.0], "pts");
        let svg = render_svg(&spec);
        assert!(svg.contains("circle"));
    }

    #[test]
    fn test_render_error_bars() {
        let series = PlotSeries::line(vec![1.0, 2.0, 3.0], vec![10.0, 20.0, 30.0], "data")
            .with_error(vec![1.0, 2.0, 3.0]);
        let spec = PlotSpec::new().add_series(series);
        let svg = render_svg(&spec);
        // Error bars produce extra lines
        assert!(svg.matches("<line").count() > 4);
    }

    #[test]
    fn test_render_multi_series() {
        let spec = PlotSpec::new()
            .with_title("Multi")
            .line(vec![0.0, 1.0], vec![0.0, 1.0], "a")
            .scatter(vec![0.0, 1.0], vec![1.0, 0.0], "b");
        let svg = render_svg(&spec);
        assert!(svg.contains("polyline"));
        assert!(svg.contains("circle"));
    }

    #[test]
    fn test_render_empty() {
        let spec = PlotSpec::new();
        let svg = render_svg(&spec);
        assert!(svg.starts_with("<svg"));
        assert!(svg.ends_with("</svg>"));
    }

    #[test]
    fn test_xml_escape() {
        let spec = PlotSpec::new().with_title("a < b & c > d");
        let svg = render_svg(&spec);
        assert!(svg.contains("a &lt; b &amp; c &gt; d"));
    }

    #[test]
    fn test_convenience_line_plot() {
        let svg = create_line_plot_svg(
            "mu vs level",
            &[0.0, 1.0, 2.0, 3.0],
            &[0.2, 0.3, 0.35, 0.37],
            "Cascade level",
            "mu",
        );
        assert!(svg.contains("mu vs level"));
        assert!(svg.contains("Cascade level"));
    }

    #[test]
    fn test_log_scale() {
        let spec = PlotSpec::new().with_log_x().with_log_y().line(
            vec![1.0, 10.0, 100.0],
            vec![1.0, 10.0, 100.0],
            "log-log",
        );
        let svg = render_svg(&spec);
        assert!(svg.contains("polyline"));
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
    }

    #[test]
    fn frame_round_trips_linear_axes() {
        let spec =
            PlotSpec::new()
                .with_title("t")
                .line(vec![0.0, 5.0, 10.0], vec![-2.0, 3.0, 8.0], "a");
        let f = frame(&spec);
        assert!(!f.x_log && !f.y_log);
        assert_eq!((f.width, f.height), (spec.width, spec.height));
        for (x, y) in [(0.0, -2.0), (5.0, 3.0), (10.0, 8.0), (2.5, 0.0)] {
            let (px, py) = f.to_pixel(x, y);
            let (bx, by) = f.to_data(px, py).expect("inside the plot area");
            assert!(close(bx, x) && close(by, y), "({x},{y}) -> ({bx},{by})");
        }
    }

    #[test]
    fn frame_round_trips_log_axes() {
        // `compute_bounds` pads linearly, so a log axis only keeps a positive
        // range when the data does not span most of its magnitude.
        let spec = PlotSpec::new().with_log_x().with_log_y().line(
            vec![10.0, 14.0, 20.0],
            vec![100.0, 150.0, 200.0],
            "ll",
        );
        let f = frame(&spec);
        assert!(f.x_log && f.y_log && f.x_min > 0.0 && f.y_min > 0.0);
        for (x, y) in [(10.0, 100.0), (14.0, 150.0), (20.0, 200.0), (12.0, 120.0)] {
            let (px, py) = f.to_pixel(x, y);
            let (bx, by) = f.to_data(px, py).expect("inside the plot area");
            assert!(close(bx, x) && close(by, y), "({x},{y}) -> ({bx},{by})");
        }
        // Equal ratios are equally far apart on a log axis.
        let (a, _) = f.to_pixel(10.0, 100.0);
        let (b, _) = f.to_pixel(12.0, 100.0);
        let (c, _) = f.to_pixel(14.4, 100.0);
        assert!(close(b - a, c - b));
    }

    #[test]
    fn frame_rejects_pixels_outside_the_plot_area() {
        let f = frame(&PlotSpec::new().line(vec![0.0, 1.0], vec![0.0, 1.0], "a"));
        assert!(f.to_data(f.plot_x - 1.0, f.plot_y + 1.0).is_none());
        assert!(f.to_data(f.plot_x + 1.0, f.plot_y - 1.0).is_none());
        assert!(f
            .to_data(f.plot_x + f.plot_w + 1.0, f.plot_y + 1.0)
            .is_none());
        assert!(f
            .to_data(f.plot_x + 1.0, f.plot_y + f.plot_h + 1.0)
            .is_none());
        assert!(f.to_data(f64::NAN, f.plot_y + 1.0).is_none());
        assert!(f.to_data(f.plot_x + 1.0, f.plot_y + 1.0).is_some());
    }

    #[test]
    fn frame_serialises_and_matches_the_rendered_plot_area() {
        let spec = PlotSpec::new()
            .with_x_label("x")
            .line(vec![0.0, 1.0], vec![0.0, 1.0], "a");
        let f = frame(&spec);
        let back: Frame = serde_json::from_value(serde_json::to_value(f).unwrap()).unwrap();
        assert_eq!(back, f);
        // The plot-area background rect is drawn from the frame.
        let svg = render_svg(&spec);
        assert!(svg.contains(&format!(
            r##"<rect x="{:.2}" y="{:.2}" width="{:.2}" height="{:.2}" fill="#fafafa""##,
            f.plot_x, f.plot_y, f.plot_w, f.plot_h
        )));
    }

    #[test]
    fn test_normalize() {
        assert!((normalize(5.0, 0.0, 10.0, false) - 0.5).abs() < 1e-10);
        assert!((normalize(10.0, 1.0, 100.0, true) - 0.5).abs() < 1e-10);
    }
}
