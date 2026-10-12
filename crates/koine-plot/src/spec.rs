//! Declarative plot specification types.
//!
//! `PlotSpec` describes *what* to plot (series, axes, error bars, legend).
//! Renderers translate a `PlotSpec` into output: [`crate::svg`] for a page,
//! [`crate::text`] for a terminal, and elsewhere PDF or Typst.
//!
//! The serde shape of every type here IS the `plot-spec@1.0.0` wire (ADR-0033):
//! it is what a surface's `plot{spec}` node carries and what stored figures
//! hold. Renaming a field or a variant, or changing a default, breaks stored
//! documents and agent-authored specs; it must not change.

use serde::{Deserialize, Serialize};

// ── Series style ────────────────────────────────────────────────────

/// How to render a data series.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SeriesStyle {
    #[default]
    Line,
    Scatter,
    LineScatter,
    Bar,
    Step,
}

// ── Color ───────────────────────────────────────────────────────────

/// Named colors suitable for scientific plots (color-blind friendly).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub enum PlotColor {
    #[default]
    Blue,
    Red,
    Green,
    Orange,
    Purple,
    Cyan,
    Black,
    Gray,
    /// Custom RGB (0-255).
    Rgb(u8, u8, u8),
}

impl PlotColor {
    /// CSS color string.
    pub fn css(&self) -> String {
        match self {
            PlotColor::Blue => "#0072B2".to_string(),
            PlotColor::Red => "#D55E00".to_string(),
            PlotColor::Green => "#009E73".to_string(),
            PlotColor::Orange => "#E69F00".to_string(),
            PlotColor::Purple => "#CC79A7".to_string(),
            PlotColor::Cyan => "#56B4E9".to_string(),
            PlotColor::Black => "#000000".to_string(),
            PlotColor::Gray => "#999999".to_string(),
            PlotColor::Rgb(r, g, b) => format!("#{:02X}{:02X}{:02X}", r, g, b),
        }
    }

    /// Color palette cycling.
    pub fn from_index(i: usize) -> Self {
        const CYCLE: &[PlotColor] = &[
            PlotColor::Blue,
            PlotColor::Red,
            PlotColor::Green,
            PlotColor::Orange,
            PlotColor::Purple,
            PlotColor::Cyan,
        ];
        CYCLE[i % CYCLE.len()].clone()
    }
}

// ── Legend ───────────────────────────────────────────────────────────

/// Legend position.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LegendPosition {
    #[default]
    TopRight,
    TopLeft,
    BottomRight,
    BottomLeft,
}

/// Legend configuration. Missing fields take [`PlotLegend::default`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PlotLegend {
    pub position: LegendPosition,
    pub visible: bool,
}

impl Default for PlotLegend {
    fn default() -> Self {
        Self {
            position: LegendPosition::TopRight,
            visible: true,
        }
    }
}

// ── Data series ─────────────────────────────────────────────────────

/// A single data series in a plot.
///
/// Only `x` and `y` are required when deserializing; everything else takes
/// the same default as [`PlotSeries::line`]. A spec written by hand (or by an
/// agent through the `create-figure` verb) need not spell out every style
/// field to be accepted.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlotSeries {
    /// Display name for legend.
    #[serde(default)]
    pub label: String,
    /// X values.
    pub x: Vec<f64>,
    /// Y values.
    pub y: Vec<f64>,
    /// Lower error bound (y - error_low). If present, error bars are drawn.
    #[serde(default)]
    pub error_low: Option<Vec<f64>>,
    /// Upper error bound (y + error_high).
    #[serde(default)]
    pub error_high: Option<Vec<f64>>,
    /// Rendering style.
    #[serde(default)]
    pub style: SeriesStyle,
    /// Series color.
    #[serde(default)]
    pub color: PlotColor,
    /// Point radius for scatter/line-scatter (default 3.0).
    #[serde(default = "default_point_radius")]
    pub point_radius: f64,
    /// Line width (default 1.5).
    #[serde(default = "default_line_width")]
    pub line_width: f64,
}

fn default_point_radius() -> f64 {
    3.0
}

fn default_line_width() -> f64 {
    1.5
}

impl PlotSeries {
    /// Create a new line series.
    pub fn line(x: Vec<f64>, y: Vec<f64>, label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            x,
            y,
            error_low: None,
            error_high: None,
            style: SeriesStyle::Line,
            color: PlotColor::default(),
            point_radius: default_point_radius(),
            line_width: default_line_width(),
        }
    }

    /// Create a scatter series.
    pub fn scatter(x: Vec<f64>, y: Vec<f64>, label: impl Into<String>) -> Self {
        let mut s = Self::line(x, y, label);
        s.style = SeriesStyle::Scatter;
        s
    }

    /// Set error bars (symmetric).
    pub fn with_error(mut self, error: Vec<f64>) -> Self {
        self.error_low = Some(error.clone());
        self.error_high = Some(error);
        self
    }

    /// Set asymmetric error bars.
    pub fn with_asymmetric_error(mut self, low: Vec<f64>, high: Vec<f64>) -> Self {
        self.error_low = Some(low);
        self.error_high = Some(high);
        self
    }

    /// Set color.
    pub fn with_color(mut self, color: PlotColor) -> Self {
        self.color = color;
        self
    }

    /// Set style.
    pub fn with_style(mut self, style: SeriesStyle) -> Self {
        self.style = style;
        self
    }
}

// ── Axis spec ───────────────────────────────────────────────────────

/// Axis configuration within a PlotSpec. Missing fields take their default.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PlotAxis {
    pub label: Option<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub log_scale: bool,
    pub format: Option<String>,
}

// ── PlotSpec ────────────────────────────────────────────────────────

/// Complete declarative specification for a 1D plot.
///
/// Missing fields take [`PlotSpec::default`] (640×400, grid on, legend top
/// right), so `{"series":[{"x":[1,2],"y":[3,4]}]}` is a whole spec.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PlotSpec {
    pub title: Option<String>,
    pub width: f64,
    pub height: f64,
    pub x_axis: PlotAxis,
    pub y_axis: PlotAxis,
    pub series: Vec<PlotSeries>,
    pub legend: PlotLegend,
    pub show_grid: bool,
    /// Annotations: reference lines, text labels, arrows, fill-between regions.
    #[serde(default)]
    pub annotations: Vec<Annotation>,
}

impl Default for PlotSpec {
    fn default() -> Self {
        Self {
            title: None,
            width: 640.0,
            height: 400.0,
            x_axis: PlotAxis::default(),
            y_axis: PlotAxis::default(),
            series: vec![],
            legend: PlotLegend::default(),
            show_grid: true,
            annotations: vec![],
        }
    }
}

impl PlotSpec {
    /// Create a new empty plot.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the plot title.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Set plot dimensions.
    pub fn with_size(mut self, width: f64, height: f64) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Set X axis label.
    pub fn with_x_label(mut self, label: impl Into<String>) -> Self {
        self.x_axis.label = Some(label.into());
        self
    }

    /// Set Y axis label.
    pub fn with_y_label(mut self, label: impl Into<String>) -> Self {
        self.y_axis.label = Some(label.into());
        self
    }

    /// Enable log scale on X axis.
    pub fn with_log_x(mut self) -> Self {
        self.x_axis.log_scale = true;
        self
    }

    /// Enable log scale on Y axis.
    pub fn with_log_y(mut self) -> Self {
        self.y_axis.log_scale = true;
        self
    }

    /// Add a line series with auto-color.
    pub fn line(mut self, x: Vec<f64>, y: Vec<f64>, label: impl Into<String>) -> Self {
        let color = PlotColor::from_index(self.series.len());
        let series = PlotSeries::line(x, y, label).with_color(color);
        self.series.push(series);
        self
    }

    /// Add a scatter series with auto-color.
    pub fn scatter(mut self, x: Vec<f64>, y: Vec<f64>, label: impl Into<String>) -> Self {
        let color = PlotColor::from_index(self.series.len());
        let series = PlotSeries::scatter(x, y, label).with_color(color);
        self.series.push(series);
        self
    }

    /// Add a pre-configured series.
    pub fn add_series(mut self, mut series: PlotSeries) -> Self {
        if series.color == PlotColor::default() {
            series.color = PlotColor::from_index(self.series.len());
        }
        self.series.push(series);
        self
    }

    /// Show or hide grid lines.
    pub fn with_grid(mut self, show: bool) -> Self {
        self.show_grid = show;
        self
    }

    /// Show or hide legend.
    pub fn with_legend(mut self, visible: bool) -> Self {
        self.legend.visible = visible;
        self
    }

    /// Set legend position.
    pub fn with_legend_position(mut self, position: LegendPosition) -> Self {
        self.legend.position = position;
        self.legend.visible = true;
        self
    }

    /// Add an annotation.
    pub fn annotate(mut self, annotation: Annotation) -> Self {
        self.annotations.push(annotation);
        self
    }

    /// Add a horizontal reference line.
    pub fn with_hline(mut self, y: f64, label: impl Into<String>, color: PlotColor) -> Self {
        self.annotations.push(Annotation::HLine {
            y,
            label: Some(label.into()),
            color,
            dash: true,
        });
        self
    }

    /// Add a vertical reference line.
    pub fn with_vline(mut self, x: f64, label: impl Into<String>, color: PlotColor) -> Self {
        self.annotations.push(Annotation::VLine {
            x,
            label: Some(label.into()),
            color,
            dash: true,
        });
        self
    }

    /// Add a fill-between region (shaded band).
    pub fn with_fill_between(
        mut self,
        x: Vec<f64>,
        y_low: Vec<f64>,
        y_high: Vec<f64>,
        color: PlotColor,
        label: impl Into<String>,
    ) -> Self {
        self.annotations.push(Annotation::FillBetween {
            x,
            y_low,
            y_high,
            color,
            label: Some(label.into()),
            opacity: 0.2,
        });
        self
    }
}

// ── Annotations ─────────────────────────────────────────────────────

/// Annotations overlaid on a plot.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Annotation {
    /// Text label at a data coordinate.
    Text {
        x: f64,
        y: f64,
        text: String,
        color: PlotColor,
    },
    /// Horizontal reference line.
    HLine {
        y: f64,
        label: Option<String>,
        color: PlotColor,
        dash: bool,
    },
    /// Vertical reference line.
    VLine {
        x: f64,
        label: Option<String>,
        color: PlotColor,
        dash: bool,
    },
    /// Arrow from one point to another with label.
    Arrow {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        label: Option<String>,
        color: PlotColor,
    },
    /// Fill-between region (shaded band between two y curves).
    FillBetween {
        x: Vec<f64>,
        y_low: Vec<f64>,
        y_high: Vec<f64>,
        color: PlotColor,
        label: Option<String>,
        opacity: f64,
    },
}

// ── Multi-panel plots (Phase 5) ─────────────────────────────────────

/// A grid of subplots sharing optional axes.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlotGrid {
    /// Grid dimensions (rows, cols).
    pub rows: usize,
    pub cols: usize,
    /// Subplots in row-major order. `None` for empty cells.
    pub cells: Vec<Option<PlotSpec>>,
    /// Overall title.
    pub title: Option<String>,
    /// Overall width in pixels.
    pub width: f64,
    /// Overall height in pixels.
    pub height: f64,
    /// Whether subplots in the same column share X axes.
    pub share_x: bool,
    /// Whether subplots in the same row share Y axes.
    pub share_y: bool,
    /// Horizontal gap between cells (pixels).
    pub h_gap: f64,
    /// Vertical gap between cells (pixels).
    pub v_gap: f64,
}

impl PlotGrid {
    /// Create a new grid of the given size.
    pub fn new(rows: usize, cols: usize) -> Self {
        Self {
            rows,
            cols,
            cells: vec![None; rows * cols],
            title: None,
            width: 800.0,
            height: 600.0,
            share_x: false,
            share_y: false,
            h_gap: 40.0,
            v_gap: 40.0,
        }
    }

    /// Set the subplot at (row, col).
    pub fn set(mut self, row: usize, col: usize, spec: PlotSpec) -> Self {
        let idx = row * self.cols + col;
        if idx < self.cells.len() {
            self.cells[idx] = Some(spec);
        }
        self
    }

    /// Set the overall title.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Set grid dimensions.
    pub fn with_size(mut self, width: f64, height: f64) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Enable shared X axes within columns.
    pub fn with_shared_x(mut self) -> Self {
        self.share_x = true;
        self
    }

    /// Enable shared Y axes within rows.
    pub fn with_shared_y(mut self) -> Self {
        self.share_y = true;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plot_spec_builder() {
        let spec = PlotSpec::new()
            .with_title("Test Plot")
            .with_x_label("x")
            .with_y_label("y")
            .line(vec![1.0, 2.0, 3.0], vec![4.0, 5.0, 6.0], "series 1")
            .scatter(vec![1.0, 2.0], vec![3.0, 4.0], "series 2");

        assert_eq!(spec.title.as_deref(), Some("Test Plot"));
        assert_eq!(spec.series.len(), 2);
        assert_eq!(spec.series[0].style, SeriesStyle::Line);
        assert_eq!(spec.series[1].style, SeriesStyle::Scatter);
    }

    #[test]
    fn test_color_cycle() {
        let c0 = PlotColor::from_index(0);
        let c1 = PlotColor::from_index(1);
        let c6 = PlotColor::from_index(6);
        assert_eq!(c0, PlotColor::Blue);
        assert_eq!(c1, PlotColor::Red);
        // Wraps around
        assert_eq!(c6, PlotColor::Blue);
    }

    #[test]
    fn test_error_bars() {
        let series = PlotSeries::line(vec![1.0], vec![2.0], "e").with_error(vec![0.5]);
        assert_eq!(series.error_low, Some(vec![0.5]));
        assert_eq!(series.error_high, Some(vec![0.5]));
    }

    #[test]
    fn test_plot_spec_serde() {
        let spec = PlotSpec::new()
            .with_title("serde test")
            .line(vec![1.0], vec![2.0], "s");
        let json = serde_json::to_string(&spec).unwrap();
        let parsed: PlotSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.title, spec.title);
        assert_eq!(parsed.series.len(), 1);
    }
}
