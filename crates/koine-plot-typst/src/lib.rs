//! # koine-plot-typst (was impress-plot)
//!
//! Native, Typst-first plotting for the impress suite. A declarative plot spec
//! turns into **Typst source** (+ raster PNG assets for the big-N path); the
//! caller compiles it with the suite's persistent Typst engine. There is no
//! external plotting process and no runtime dependency beyond `png` — a plot is
//! just more Typst source, so it renders in the same in-process engine imbib and
//! imprint already ship, and shares the manuscript's fonts and colors.
//!
//! [`TypstRenderer`] is koine-plot's `typst` backend: it draws a
//! `plot-spec@1.0.0` through the builders below.
//!
//! ## Shape
//! - [`Axis`] — linear/log scale + auto/manual (min,max) limits. The two
//!   most-reached-for interactive controls; flipping either is a one-field
//!   change that re-renders correctly.
//! - [`LinePlot`] — vector line/scatter through the axes (crisp, scalable).
//! - [`Hist2DFigure`] / [`Hist2D`] — the big-N raster fallback: a weighted,
//!   normalizable 2D histogram rasterized through a selectable [`Colormap`],
//!   composed under vector axes + a vector colorbar.
//! - [`PlotOutput`] `{ typst, assets }` — hand `assets` to the renderer's
//!   `set_asset`, compile `typst`.
//!
//! ```
//! use koine_plot_typst::*;
//! let xs: Vec<f64> = (1..=100).map(|i| i as f64).collect();
//! let ys: Vec<f64> = xs.iter().map(|x| x.powi(3)).collect();
//! // Same data, log y-axis, zoomed x — just axis config:
//! let out = LinePlot::new(Axis::linear().with_limits(1.0, 50.0), Axis::log())
//!     .title("cubic")
//!     .line(xs, ys, [31, 111, 214])
//!     .render(PlotSize::new(340.0, 220.0));
//! assert!(out.typst.contains("#curve("));
//! ```

mod axis;
mod colormap;
mod contour;
mod hist2d;
mod plot;
mod render;
mod renderer;

pub use axis::{Axis, Limits, Scale, Tick};
pub use colormap::Colormap;
pub use contour::{contour_lines, gaussian_smooth, ContourLevels};
pub use hist2d::{ColorScale, Hist2D, Normalization};
pub use plot::{Chosen, GridPlot, GridStyle, Plot, Strategy};
pub use render::{ContourFigure, Hist2DFigure, LinePlot, LineStyle, PlotOutput, PlotSize, Series};
pub use renderer::{plot_of, TypstRenderer, RASTER_ASSET};
