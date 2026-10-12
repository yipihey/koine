//! 2D axis system for scientific visualization
//!
//! Provides tick mark calculation, label formatting, and axis layout
//! suitable for publication-quality figures.

use serde::{Deserialize, Serialize};

/// Axis orientation
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AxisPosition {
    Left,
    Right,
    Top,
    Bottom,
}

impl AxisPosition {
    /// Check if this is a vertical axis
    pub fn is_vertical(&self) -> bool {
        matches!(self, AxisPosition::Left | AxisPosition::Right)
    }

    /// Check if this is a horizontal axis
    pub fn is_horizontal(&self) -> bool {
        !self.is_vertical()
    }
}

/// Scale type for axis
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScaleType {
    Linear,
    Log10,
    SymLog,
}

/// Configuration for an axis
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AxisConfig {
    /// Axis position
    pub position: AxisPosition,

    /// Scale type
    pub scale: ScaleType,

    /// Data range
    pub min: f64,
    pub max: f64,

    /// Label for the axis
    pub label: Option<String>,

    /// Unit string (e.g., "km/s", "Mpc")
    pub unit: Option<String>,

    /// Whether to show the axis line (spine)
    pub show_spine: bool,

    /// Whether to show tick marks
    pub show_ticks: bool,

    /// Whether to show tick labels
    pub show_labels: bool,

    /// Whether to show grid lines
    pub show_grid: bool,

    /// Number format string (e.g., ".2f", ".1e")
    pub format: Option<String>,

    /// Tick mark length in pixels
    pub tick_length: f32,

    /// Number of minor ticks between major ticks
    pub minor_ticks: usize,
}

impl AxisConfig {
    /// Create a new axis configuration
    pub fn new(position: AxisPosition, min: f64, max: f64) -> Self {
        Self {
            position,
            scale: ScaleType::Linear,
            min,
            max,
            label: None,
            unit: None,
            show_spine: true,
            show_ticks: true,
            show_labels: true,
            show_grid: false,
            format: None,
            tick_length: 5.0,
            minor_ticks: 4,
        }
    }

    /// Set the axis label
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Set the unit
    pub fn with_unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }

    /// Set logarithmic scale
    pub fn with_log_scale(mut self) -> Self {
        self.scale = ScaleType::Log10;
        self
    }

    /// Enable grid lines
    pub fn with_grid(mut self) -> Self {
        self.show_grid = true;
        self
    }

    /// Set number format
    pub fn with_format(mut self, format: impl Into<String>) -> Self {
        self.format = Some(format.into());
        self
    }

    /// Get the full label with unit
    pub fn full_label(&self) -> Option<String> {
        match (&self.label, &self.unit) {
            (Some(label), Some(unit)) => Some(format!("{} [{}]", label, unit)),
            (Some(label), None) => Some(label.clone()),
            (None, Some(unit)) => Some(format!("[{}]", unit)),
            (None, None) => None,
        }
    }
}

impl Default for AxisConfig {
    fn default() -> Self {
        Self::new(AxisPosition::Bottom, 0.0, 1.0)
    }
}

/// A tick mark on an axis
#[derive(Clone, Debug)]
pub struct TickMark {
    /// Position in data coordinates
    pub value: f64,

    /// Position in normalized coordinates (0.0 to 1.0)
    pub normalized: f64,

    /// Whether this is a major tick
    pub is_major: bool,

    /// Label text (only for major ticks)
    pub label: Option<String>,
}

/// Calculate tick marks for an axis
pub fn calculate_ticks(config: &AxisConfig) -> Vec<TickMark> {
    match config.scale {
        ScaleType::Linear => calculate_linear_ticks(config),
        ScaleType::Log10 => calculate_log_ticks(config),
        ScaleType::SymLog => calculate_symlog_ticks(config),
    }
}

fn calculate_linear_ticks(config: &AxisConfig) -> Vec<TickMark> {
    let range = config.max - config.min;
    if range <= 0.0 {
        return vec![];
    }

    // Calculate nice tick spacing
    let rough_step = range / 5.0;
    let magnitude = 10.0_f64.powf(rough_step.abs().log10().floor());
    let residual = rough_step / magnitude;

    let nice_step = if residual <= 1.5 {
        1.0 * magnitude
    } else if residual <= 3.0 {
        2.0 * magnitude
    } else if residual <= 7.0 {
        5.0 * magnitude
    } else {
        10.0 * magnitude
    };

    // Generate major ticks
    let start = (config.min / nice_step).ceil() * nice_step;
    let mut ticks = Vec::new();

    let mut value = start;
    while value <= config.max + nice_step * 0.001 {
        let normalized = (value - config.min) / range;
        if (-0.001..=1.001).contains(&normalized) {
            ticks.push(TickMark {
                value,
                normalized: normalized.clamp(0.0, 1.0),
                is_major: true,
                label: Some(format_number(value, config.format.as_deref())),
            });
        }
        value += nice_step;
    }

    // Generate minor ticks
    if config.minor_ticks > 0 && ticks.len() >= 2 {
        let minor_step = nice_step / (config.minor_ticks + 1) as f64;
        let mut minor_ticks = Vec::new();

        for tick in &ticks {
            for i in 1..=config.minor_ticks {
                let minor_value = tick.value + minor_step * i as f64;
                let normalized = (minor_value - config.min) / range;
                if normalized > 0.0 && normalized < 1.0 {
                    minor_ticks.push(TickMark {
                        value: minor_value,
                        normalized,
                        is_major: false,
                        label: None,
                    });
                }
            }
        }

        ticks.extend(minor_ticks);
    }

    ticks.sort_by(|a, b| a.value.partial_cmp(&b.value).unwrap());
    ticks
}

fn calculate_log_ticks(config: &AxisConfig) -> Vec<TickMark> {
    if config.min <= 0.0 || config.max <= 0.0 {
        return vec![];
    }

    let log_min = config.min.log10();
    let log_max = config.max.log10();
    let log_range = log_max - log_min;

    let start_decade = log_min.floor() as i32;
    let end_decade = log_max.ceil() as i32;

    let mut ticks = Vec::new();

    for decade in start_decade..=end_decade {
        let value = 10.0_f64.powi(decade);
        if value >= config.min && value <= config.max {
            let normalized = (value.log10() - log_min) / log_range;
            ticks.push(TickMark {
                value,
                normalized,
                is_major: true,
                label: Some(format!("10{}", superscript(decade))),
            });
        }

        // Minor ticks at 2, 3, ..., 9
        if config.minor_ticks > 0 {
            for minor in [2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0] {
                let minor_value = value * minor;
                if minor_value >= config.min && minor_value <= config.max {
                    let normalized = (minor_value.log10() - log_min) / log_range;
                    ticks.push(TickMark {
                        value: minor_value,
                        normalized,
                        is_major: false,
                        label: None,
                    });
                }
            }
        }
    }

    ticks.sort_by(|a, b| a.value.partial_cmp(&b.value).unwrap());
    ticks
}

fn calculate_symlog_ticks(config: &AxisConfig) -> Vec<TickMark> {
    // Symmetric log scale (linear near zero, log away from zero)
    // For simplicity, we fall back to linear for now
    calculate_linear_ticks(config)
}

/// Format a number for display
fn format_number(value: f64, format: Option<&str>) -> String {
    match format {
        Some(fmt) if fmt.ends_with('e') || fmt.ends_with('E') => {
            format!("{:e}", value)
        }
        Some(fmt) if fmt.contains('.') => {
            let precision: usize = fmt
                .chars()
                .skip_while(|c| *c != '.')
                .skip(1)
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .unwrap_or(2);
            format!("{:.prec$}", value, prec = precision)
        }
        _ => {
            // Auto format
            if value == 0.0 {
                "0".to_string()
            } else if value.abs() >= 10000.0 || value.abs() < 0.01 {
                format!("{:.2e}", value)
            } else if value.fract().abs() < 1e-10 {
                format!("{:.0}", value)
            } else {
                format!("{:.2}", value)
            }
        }
    }
}

/// Convert an integer to superscript Unicode characters
fn superscript(n: i32) -> String {
    const SUPERSCRIPTS: &[char] = &['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];

    if n == 0 {
        return "⁰".to_string();
    }

    let mut result = String::new();
    let mut num = n.abs();

    if n < 0 {
        result.push('⁻');
    }

    let mut digits = Vec::new();
    while num > 0 {
        digits.push(SUPERSCRIPTS[(num % 10) as usize]);
        num /= 10;
    }

    for digit in digits.into_iter().rev() {
        result.push(digit);
    }

    result
}

/// Layout information for a complete axis system
#[derive(Clone, Debug)]
pub struct AxisLayout {
    /// Viewport in pixels (x, y, width, height)
    pub viewport: [f32; 4],

    /// Margin for axis labels and ticks (left, bottom, right, top)
    pub margin: [f32; 4],

    /// Plot area in pixels (x, y, width, height)
    pub plot_area: [f32; 4],

    /// X-axis configuration
    pub x_axis: AxisConfig,

    /// Y-axis configuration
    pub y_axis: AxisConfig,
}

impl AxisLayout {
    /// Create a new axis layout
    pub fn new(width: f32, height: f32, x_axis: AxisConfig, y_axis: AxisConfig) -> Self {
        let margin = [60.0, 50.0, 20.0, 20.0]; // left, bottom, right, top

        let plot_area = [
            margin[0],
            margin[1],
            width - margin[0] - margin[2],
            height - margin[1] - margin[3],
        ];

        Self {
            viewport: [0.0, 0.0, width, height],
            margin,
            plot_area,
            x_axis,
            y_axis,
        }
    }

    /// Transform data coordinates to pixel coordinates
    pub fn data_to_pixel(&self, x: f64, y: f64) -> (f32, f32) {
        let x_norm = (x - self.x_axis.min) / (self.x_axis.max - self.x_axis.min);
        let y_norm = (y - self.y_axis.min) / (self.y_axis.max - self.y_axis.min);

        let px = self.plot_area[0] + x_norm as f32 * self.plot_area[2];
        let py = self.plot_area[1] + (1.0 - y_norm as f32) * self.plot_area[3]; // Y is flipped

        (px, py)
    }

    /// Transform pixel coordinates to data coordinates
    pub fn pixel_to_data(&self, px: f32, py: f32) -> (f64, f64) {
        let x_norm = (px - self.plot_area[0]) / self.plot_area[2];
        let y_norm = 1.0 - (py - self.plot_area[1]) / self.plot_area[3]; // Y is flipped

        let x = self.x_axis.min + x_norm as f64 * (self.x_axis.max - self.x_axis.min);
        let y = self.y_axis.min + y_norm as f64 * (self.y_axis.max - self.y_axis.min);

        (x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_linear_ticks() {
        let config = AxisConfig::new(AxisPosition::Bottom, 0.0, 10.0);
        let ticks = calculate_ticks(&config);

        assert!(!ticks.is_empty());
        let major_ticks: Vec<_> = ticks.iter().filter(|t| t.is_major).collect();
        assert!(major_ticks.len() >= 3);
    }

    #[test]
    fn test_calculate_log_ticks() {
        let mut config = AxisConfig::new(AxisPosition::Bottom, 1.0, 1000.0);
        config.scale = ScaleType::Log10;

        let ticks = calculate_ticks(&config);
        let major_ticks: Vec<_> = ticks.iter().filter(|t| t.is_major).collect();

        // Should have ticks at 10^0, 10^1, 10^2, 10^3
        assert_eq!(major_ticks.len(), 4);
    }

    #[test]
    fn test_format_number() {
        assert_eq!(format_number(0.0, None), "0");
        assert_eq!(format_number(123.0, None), "123");
        assert_eq!(format_number(123.456, Some(".2f")), "123.46");
        assert!(format_number(12345678.0, None).contains('e'));
    }

    #[test]
    fn test_superscript() {
        assert_eq!(superscript(0), "⁰");
        assert_eq!(superscript(1), "¹");
        assert_eq!(superscript(-2), "⁻²");
        assert_eq!(superscript(12), "¹²");
    }

    #[test]
    fn test_axis_layout_transform() {
        let x_axis = AxisConfig::new(AxisPosition::Bottom, 0.0, 100.0);
        let y_axis = AxisConfig::new(AxisPosition::Left, 0.0, 100.0);
        let layout = AxisLayout::new(400.0, 300.0, x_axis, y_axis);

        // Center of data should be center of plot area
        let (px, py) = layout.data_to_pixel(50.0, 50.0);
        let (x, y) = layout.pixel_to_data(px, py);

        assert!((x - 50.0).abs() < 0.1);
        assert!((y - 50.0).abs() < 0.1);
    }
}
