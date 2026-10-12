//! A text sketch of a [`PlotSpec`] for a terminal.
//!
//! Not a chart you read values from: a sketch that shows the shape, the
//! range (three tick labels per axis) and which series is which (one glyph
//! each, cycling `* + o x # @`). Deterministic and total: it never panics, and
//! a spec it cannot sketch answers a one-line `plot: <reason>`.
//!
//! ```text
//! demo
//!    4 |                    *
//!      |
//!    2 |          *
//!      |
//!    0 |*
//!      +--------------------
//!       0         1         2
//! * up
//! ```

use crate::spec::{PlotSpec, SeriesStyle};

/// The smallest sketch there is room for.
pub const MIN_COLS: usize = 20;
/// The smallest sketch there is room for.
pub const MIN_ROWS: usize = 6;

const GLYPHS: [char; 6] = ['*', '+', 'o', 'x', '#', '@'];
/// The widest a tick label gets before it is cut.
const MAX_LABEL: usize = 8;
/// Cell coordinates are clamped this many plot widths off the grid, so a
/// huge value draws a bounded segment rather than looping for ever.
const OFF_GRID: f64 = 4.0;

/// Sketch `spec` in at most `cols` x `rows` characters.
pub fn render_text(spec: &PlotSpec, cols: usize, rows: usize) -> String {
    if cols < MIN_COLS || rows < MIN_ROWS {
        return format!("plot: too small (need at least {MIN_COLS}x{MIN_ROWS})");
    }
    let Some(frame) = Frame::of(spec) else {
        return "plot: no data".to_string();
    };

    // Rows: [title] plot... axis ticks [legend]. The plot keeps at least
    // three rows; the legend goes first, then the title, when space is short.
    let legend = legend_line(spec);
    let mut plot_h = rows - 2;
    let show_legend = legend.is_some() && plot_h > 3;
    if show_legend {
        plot_h -= 1;
    }
    let show_title = spec.title.is_some() && plot_h > 3;
    if show_title {
        plot_h -= 1;
    }

    let y_labels = [
        compact(frame.y.at(1.0)),
        compact(frame.y.at(0.5)),
        compact(frame.y.at(0.0)),
    ];
    let lw = y_labels
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(1)
        .min(MAX_LABEL);
    let plot_w = cols - lw - 2;

    let mut grid = vec![vec![' '; plot_w]; plot_h];
    for (i, series) in spec.series.iter().enumerate() {
        draw_series(&mut grid, &frame, series, GLYPHS[i % GLYPHS.len()]);
    }

    let mut out: Vec<String> = Vec::new();
    if show_title {
        if let Some(title) = &spec.title {
            out.push(clip(title, cols));
        }
    }
    let label_rows = [0, plot_h / 2, plot_h - 1];
    for (r, cells) in grid.iter().enumerate() {
        let label = label_rows
            .iter()
            .position(|&lr| lr == r)
            .map(|k| clip(&y_labels[k], lw))
            .unwrap_or_default();
        let row: String = cells.iter().collect();
        out.push(format!("{label:>lw$} |{row}").trim_end().to_string());
    }
    out.push(format!("{:>lw$} +{}", "", "-".repeat(plot_w)));
    let x_labels = [
        compact(frame.x.at(0.0)),
        compact(frame.x.at(0.5)),
        compact(frame.x.at(1.0)),
    ];
    out.push(
        format!("{:>lw$}  {}", "", tick_row(&x_labels, plot_w))
            .trim_end()
            .to_string(),
    );
    if show_legend {
        if let Some(legend) = legend {
            out.push(clip(&legend, cols));
        }
    }
    out.join("\n")
}

// ── Scales ──────────────────────────────────────────────────────────

/// One axis: the data range and whether it maps through log10.
struct Scale {
    /// Lower end, in the mapped space (log10 of the data when `log`).
    lo: f64,
    hi: f64,
    log: bool,
}

impl Scale {
    /// Build from the finite data values plus the axis' explicit bounds.
    fn new(values: &[f64], min: Option<f64>, max: Option<f64>, log_scale: bool) -> Option<Self> {
        let min = min.filter(|v| v.is_finite());
        let max = max.filter(|v| v.is_finite());
        let mut lo = values.iter().copied().fold(f64::INFINITY, f64::min);
        let mut hi = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if let Some(v) = min {
            lo = v;
        }
        if let Some(v) = max {
            hi = v;
        }
        if !lo.is_finite() || !hi.is_finite() {
            return None;
        }
        if lo > hi {
            std::mem::swap(&mut lo, &mut hi);
        }
        // Log only when the axis says so and everything on it is positive.
        let log = log_scale && lo > 0.0 && values.iter().all(|&v| v > 0.0);
        let (mut lo, mut hi) = if log {
            (lo.log10(), hi.log10())
        } else {
            (lo, hi)
        };
        if hi - lo <= f64::EPSILON * lo.abs().max(hi.abs()).max(1.0) {
            lo -= 0.5;
            hi += 0.5;
        }
        Some(Self { lo, hi, log })
    }

    /// 0.0 at the low end, 1.0 at the high end; clamped a few widths off.
    fn norm(&self, v: f64) -> Option<f64> {
        let m = if self.log {
            if v <= 0.0 {
                return None;
            }
            v.log10()
        } else {
            v
        };
        let n = (m - self.lo) / (self.hi - self.lo);
        n.is_finite().then(|| n.clamp(-OFF_GRID, OFF_GRID + 1.0))
    }

    /// The data value at normalised position `n`.
    fn at(&self, n: f64) -> f64 {
        let m = self.lo + n * (self.hi - self.lo);
        if self.log {
            10f64.powf(m)
        } else {
            m
        }
    }
}

struct Frame {
    x: Scale,
    y: Scale,
    /// Whether any bar series asks the y axis to include zero.
    baseline: f64,
}

impl Frame {
    fn of(spec: &PlotSpec) -> Option<Self> {
        let mut xs = Vec::new();
        let mut ys = Vec::new();
        let mut bars = false;
        for s in &spec.series {
            for (&x, &y) in s.x.iter().zip(&s.y) {
                if x.is_finite() && y.is_finite() {
                    xs.push(x);
                    ys.push(y);
                    bars |= s.style == SeriesStyle::Bar;
                }
            }
        }
        if xs.is_empty() {
            return None;
        }
        if bars && !spec.y_axis.log_scale {
            ys.push(0.0);
        }
        let x = Scale::new(&xs, spec.x_axis.min, spec.x_axis.max, spec.x_axis.log_scale)?;
        let y = Scale::new(&ys, spec.y_axis.min, spec.y_axis.max, spec.y_axis.log_scale)?;
        // Bars start at zero when the axis shows it, else at the bottom.
        let baseline = if y.log {
            0.0
        } else {
            y.norm(0.0)
                .filter(|n| (0.0..=1.0).contains(n))
                .unwrap_or(0.0)
        };
        Some(Self { x, y, baseline })
    }
}

// ── Drawing ─────────────────────────────────────────────────────────

fn draw_series(grid: &mut [Vec<char>], frame: &Frame, s: &crate::spec::PlotSeries, glyph: char) {
    let h = grid.len();
    let w = grid.first().map_or(0, Vec::len);
    if h == 0 || w == 0 {
        return;
    }
    let cell = |x: f64, y: f64| -> Option<(i64, i64)> {
        let nx = frame.x.norm(x)?;
        let ny = frame.y.norm(y)?;
        Some((
            (nx * (w - 1) as f64).round() as i64,
            ((1.0 - ny) * (h - 1) as f64).round() as i64,
        ))
    };
    let points: Vec<(i64, i64)> =
        s.x.iter()
            .zip(&s.y)
            .filter(|(x, y)| x.is_finite() && y.is_finite())
            .filter_map(|(&x, &y)| cell(x, y))
            .collect();
    let mut put = |c: i64, r: i64| {
        if (0..w as i64).contains(&c) && (0..h as i64).contains(&r) {
            grid[r as usize][c as usize] = glyph;
        }
    };
    match s.style {
        SeriesStyle::Scatter => {
            for &(c, r) in &points {
                put(c, r);
            }
        }
        SeriesStyle::Line | SeriesStyle::LineScatter => {
            if let [(c, r)] = points[..] {
                put(c, r);
            }
            for pair in points.windows(2) {
                let ((c0, r0), (c1, r1)) = (pair[0], pair[1]);
                let n = (c1 - c0).abs().max((r1 - r0).abs());
                for k in 0..=n {
                    let t = if n == 0 { 0.0 } else { k as f64 / n as f64 };
                    put(
                        c0 + ((c1 - c0) as f64 * t).round() as i64,
                        r0 + ((r1 - r0) as f64 * t).round() as i64,
                    );
                }
            }
            if s.style == SeriesStyle::LineScatter {
                for &(c, r) in &points {
                    put(c, r);
                }
            }
        }
        SeriesStyle::Step => {
            if let [(c, r)] = points[..] {
                put(c, r);
            }
            for pair in points.windows(2) {
                let ((c0, r0), (c1, r1)) = (pair[0], pair[1]);
                for c in c0.min(c1)..=c0.max(c1) {
                    put(c, r0);
                }
                for r in r0.min(r1)..=r0.max(r1) {
                    put(c1, r);
                }
            }
        }
        SeriesStyle::Bar => {
            let base = ((1.0 - frame.baseline) * (h - 1) as f64).round() as i64;
            for &(c, r) in &points {
                for rr in r.min(base)..=r.max(base) {
                    put(c, rr);
                }
            }
        }
    }
}

// ── Text pieces ─────────────────────────────────────────────────────

fn legend_line(spec: &PlotSpec) -> Option<String> {
    if !spec.legend.visible || spec.series.iter().all(|s| s.label.is_empty()) {
        return None;
    }
    let entries: Vec<String> = spec
        .series
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.label.is_empty())
        .map(|(i, s)| format!("{} {}", GLYPHS[i % GLYPHS.len()], one_line(&s.label)))
        .collect();
    Some(entries.join("  "))
}

/// Left, middle and right labels laid on one row `width` wide. The middle one
/// is dropped when it would touch a neighbour.
fn tick_row(labels: &[String; 3], width: usize) -> String {
    let mut row = vec![' '; width];
    let mut place = |text: &str, start: usize| {
        for (k, ch) in text.chars().enumerate() {
            if let Some(slot) = row.get_mut(start + k) {
                *slot = ch;
            }
        }
    };
    let left = clip(&labels[0], MAX_LABEL);
    let right = clip(&labels[2], MAX_LABEL);
    let mid = clip(&labels[1], MAX_LABEL);
    let (ll, rl, ml) = (
        left.chars().count(),
        right.chars().count(),
        mid.chars().count(),
    );
    place(&left, 0);
    let right_start = width.saturating_sub(rl);
    place(&right, right_start);
    let mid_start = (width / 2).saturating_sub(ml / 2);
    if mid_start > ll && mid_start + ml < right_start {
        place(&mid, mid_start);
    }
    row.iter().collect()
}

/// A compact number: about three significant digits, scientific when huge or
/// tiny. Always finite input.
fn compact(v: f64) -> String {
    if !v.is_finite() {
        return "?".to_string();
    }
    if v == 0.0 {
        return "0".to_string();
    }
    let a = v.abs();
    let text = if !(1e-3..1e5).contains(&a) {
        let s = format!("{v:.1e}");
        match s.split_once('e') {
            Some((m, e)) => format!("{}e{e}", m.trim_end_matches('0').trim_end_matches('.')),
            None => s,
        }
    } else {
        let decimals = if a >= 100.0 {
            0
        } else if a >= 10.0 {
            1
        } else if a >= 1.0 {
            2
        } else {
            3
        };
        let s = format!("{v:.decimals$}");
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        }
    };
    if text == "-0" {
        "0".to_string()
    } else {
        text
    }
}

fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// At most `max` characters, on one line.
fn clip(s: &str, max: usize) -> String {
    one_line(s).chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{PlotSeries, SeriesStyle};

    fn lines(s: &str) -> Vec<&str> {
        s.lines().collect()
    }

    #[test]
    fn a_small_fixture_is_pinned() {
        let spec = PlotSpec::new()
            .with_title("demo")
            .scatter(vec![0.0, 1.0, 2.0], vec![0.0, 2.0, 4.0], "up")
            .line(vec![0.0, 2.0], vec![4.0, 0.0], "down");
        let text = render_text(&spec, 30, 10);
        let expected = [
            "demo",
            "4 |+++                       *",
            "  |   +++++",
            "  |        +++++",
            "2 |             ++++++",
            "  |                   +++++",
            "0 |*                       +++",
            "  +---------------------------",
            "   0            1            2",
            "* up  + down",
        ]
        .join("\n");
        assert_eq!(text, expected, "\n{text}\n");
    }

    #[test]
    fn too_small_and_empty_are_one_line() {
        let spec = PlotSpec::new().line(vec![1.0], vec![1.0], "a");
        assert_eq!(
            render_text(&spec, 19, 10),
            "plot: too small (need at least 20x6)"
        );
        assert_eq!(
            render_text(&spec, 40, 5),
            "plot: too small (need at least 20x6)"
        );
        assert_eq!(render_text(&PlotSpec::new(), 40, 10), "plot: no data");
        let nan = PlotSpec::new().line(vec![f64::NAN], vec![1.0], "n");
        assert_eq!(render_text(&nan, 40, 10), "plot: no data");
    }

    #[test]
    fn it_never_panics_on_odd_data() {
        let cases = vec![
            PlotSpec::new().line(vec![1.0], vec![1.0], "single"),
            PlotSpec::new().line(vec![1.0, 1.0], vec![5.0, 5.0], "flat"),
            PlotSpec::new().line(
                vec![0.0, f64::INFINITY, 2.0],
                vec![1.0, 2.0, f64::NAN],
                "inf",
            ),
            PlotSpec::new().line(vec![1e300, -1e300], vec![1e308, -1e308], "huge"),
            PlotSpec::new()
                .line(vec![-1.0, 0.0, 1.0], vec![-1.0, 0.0, 1.0], "z")
                .with_log_x()
                .with_log_y(),
            PlotSpec::new().add_series(
                PlotSeries::line(vec![1.0, 2.0], vec![1.0], "ragged").with_style(SeriesStyle::Bar),
            ),
            PlotSpec::new()
                .line(vec![0.0, 1.0], vec![0.0, 1.0], "nl\nin\tlabel")
                .with_title("t\nitle"),
        ];
        for style in [
            SeriesStyle::Line,
            SeriesStyle::Scatter,
            SeriesStyle::LineScatter,
            SeriesStyle::Bar,
            SeriesStyle::Step,
        ] {
            for base in &cases {
                let mut spec = base.clone();
                for s in &mut spec.series {
                    s.style = style;
                }
                for (cols, rows) in [(20, 6), (40, 12), (200, 60), (0, 0), (20, 100)] {
                    let text = render_text(&spec, cols, rows);
                    assert!(!text.is_empty());
                    if cols >= 20 && rows >= 6 {
                        assert!(text.lines().count() <= rows, "{text}");
                        for l in text.lines() {
                            assert!(l.chars().count() <= cols, "{l:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn log_axes_map_through_log10_and_fall_back_when_not_positive() {
        let log =
            PlotSpec::new()
                .with_log_x()
                .line(vec![1.0, 10.0, 100.0], vec![1.0, 2.0, 3.0], "l");
        let text = render_text(&log, 40, 10);
        let ticks = text.lines().rev().nth(1).unwrap_or_default();
        assert!(
            ticks.contains('1') && ticks.contains("10") && ticks.contains("100"),
            "{ticks:?}"
        );
        let bad = PlotSpec::new()
            .with_log_x()
            .line(vec![-1.0, 10.0], vec![1.0, 2.0], "l");
        assert!(render_text(&bad, 40, 10).contains("-1"));
    }

    #[test]
    fn glyphs_cycle_and_the_legend_lists_the_labelled_series() {
        let mut spec = PlotSpec::new();
        for i in 0..7 {
            spec = spec.scatter(vec![i as f64], vec![i as f64], format!("s{i}"));
        }
        let text = render_text(&spec, 80, 14);
        let legend = lines(&text).last().unwrap().to_string();
        assert!(
            legend.starts_with("* s0  + s1  o s2  x s3  # s4  @ s5  * s6"),
            "{legend}"
        );
        let hidden = render_text(&spec.clone().with_legend(false), 80, 14);
        assert!(!hidden.contains("s0"));
        let unlabelled = render_text(&PlotSpec::new().scatter(vec![1.0], vec![1.0], ""), 40, 10);
        assert!(!unlabelled.contains("* "), "{unlabelled}");
    }

    #[test]
    fn bars_stand_on_the_baseline() {
        let spec = PlotSpec::new().add_series(
            PlotSeries::line(vec![0.0, 1.0, 2.0], vec![1.0, 2.0, 3.0], "")
                .with_style(SeriesStyle::Bar),
        );
        let text = render_text(&spec, 30, 10);
        // The bottom plot row (just above the axis) holds a bar foot for all three.
        let foot = lines(&text)[lines(&text).len() - 3].to_string();
        assert_eq!(foot.matches('*').count(), 3, "{text}");
    }

    #[test]
    fn compact_numbers() {
        assert_eq!(compact(0.0), "0");
        assert_eq!(compact(-0.0), "0");
        assert_eq!(compact(2.0), "2");
        assert_eq!(compact(2.5), "2.5");
        assert_eq!(compact(123.456), "123");
        assert_eq!(compact(12.34), "12.3");
        assert_eq!(compact(0.0123), "0.012");
        assert_eq!(compact(123456.0), "1.2e5");
        assert_eq!(compact(1e-5), "1e-5");
        assert_eq!(compact(f64::NAN), "?");
    }
}
