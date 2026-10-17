//! One plot spec, many renderers (docs/plan-koine-kit-moves.md KM-D13).
//!
//! [`PlotSpec`] says what to draw; a [`PlotRenderer`] draws it in one tool's
//! terms. SVG ([`crate::svg`]) and text ([`crate::text`]) are built in. Every
//! further backend is its own crate implementing [`PlotRenderer`] — Typst
//! (`koine-plot-typst`), matplotlib (`koine-plot-matplotlib`), Veusz
//! (`koine-plot-veusz`) — linked only by an app that wants it and added with
//! [`register_renderer`]. A new backend is a new crate: nothing in the spec
//! or this registry changes.
//!
//! A renderer says, before it draws, what of a spec it would not keep
//! ([`PlotRenderer::unsupported`]), so a caller can pick another backend or
//! warn rather than lose a contour or an annotation silently.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use crate::{PlotError, PlotSpec};

/// What a renderer writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Format {
    /// A standalone `<svg>` document.
    Svg,
    /// Plain text (a terminal sketch).
    Text,
    /// Typst source; any raster assets are in [`Rendered::assets`].
    Typst,
    /// A self-contained Python script (matplotlib).
    Python,
    /// A Veusz document (`.vsz`).
    Vsz,
    /// A PNG image.
    Png,
    /// A PDF document.
    Pdf,
}

impl Format {
    /// The usual file extension.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Svg => "svg",
            Format::Text => "txt",
            Format::Typst => "typ",
            Format::Python => "py",
            Format::Vsz => "vsz",
            Format::Png => "png",
            Format::Pdf => "pdf",
        }
    }

    /// The media type.
    pub fn media_type(self) -> &'static str {
        match self {
            Format::Svg => "image/svg+xml",
            Format::Text => "text/plain",
            Format::Typst => "text/x-typst",
            Format::Python => "text/x-python",
            Format::Vsz => "text/x-veusz",
            Format::Png => "image/png",
            Format::Pdf => "application/pdf",
        }
    }
}

/// How to render: the format wanted (a renderer's first format when `None`)
/// and the size in points (the spec's own `width`/`height` when `None`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RenderOptions {
    pub format: Option<Format>,
    pub width: Option<f64>,
    pub height: Option<f64>,
}

impl RenderOptions {
    pub fn format(format: Format) -> Self {
        Self {
            format: Some(format),
            ..Default::default()
        }
    }

    /// The size to draw at, `spec`'s unless overridden.
    pub fn size(&self, spec: &PlotSpec) -> (f64, f64) {
        (
            self.width.unwrap_or(spec.width),
            self.height.unwrap_or(spec.height),
        )
    }
}

/// What a renderer drew: the body in its format, and any files it refers to
/// (a raster tile a Typst figure embeds, by the name it uses).
#[derive(Clone, Debug, PartialEq)]
pub struct Rendered {
    pub format: Format,
    pub body: Vec<u8>,
    pub assets: BTreeMap<String, Vec<u8>>,
}

impl Rendered {
    /// A text body with no assets.
    pub fn text(format: Format, body: impl Into<String>) -> Self {
        Self {
            format,
            body: body.into().into_bytes(),
            assets: BTreeMap::new(),
        }
    }

    /// The body as text, when it is UTF-8.
    pub fn body_text(&self) -> Option<&str> {
        std::str::from_utf8(&self.body).ok()
    }
}

/// One way to draw a [`PlotSpec`].
pub trait PlotRenderer: Send + Sync {
    /// The renderer's id: `"svg"`, `"text"`, `"typst"`, `"matplotlib"`, …
    fn id(&self) -> &'static str;

    /// The formats it writes, preferred first.
    fn formats(&self) -> &'static [Format];

    /// What of `spec` it would not keep, one line each; empty when it draws
    /// all of it.
    fn unsupported(&self, spec: &PlotSpec) -> Vec<String>;

    /// Draw `spec`. A format it does not write is a [`PlotError::Unsupported`].
    fn render(&self, spec: &PlotSpec, options: &RenderOptions) -> Result<Rendered, PlotError>;
}

/// The format `options` asks for, if `renderer` writes it.
pub fn chosen_format(
    renderer: &dyn PlotRenderer,
    options: &RenderOptions,
) -> Result<Format, PlotError> {
    let formats = renderer.formats();
    match options.format {
        None => formats
            .first()
            .copied()
            .ok_or_else(|| PlotError::Unsupported(format!("{} writes no format", renderer.id()))),
        Some(format) if formats.contains(&format) => Ok(format),
        Some(format) => Err(PlotError::Unsupported(format!(
            "{} does not write {:?} (it writes {:?})",
            renderer.id(),
            format,
            formats
        ))),
    }
}

/// The built-in SVG renderer ([`crate::svg::render_svg`]).
pub struct SvgRenderer;

impl PlotRenderer for SvgRenderer {
    fn id(&self) -> &'static str {
        "svg"
    }

    fn formats(&self) -> &'static [Format] {
        &[Format::Svg]
    }

    fn unsupported(&self, spec: &PlotSpec) -> Vec<String> {
        spec.series
            .iter()
            .filter(|s| s.style == crate::SeriesStyle::Contour)
            .map(|s| format!("series '{}': density contours (drawn as points)", s.label))
            .collect()
    }

    fn render(&self, spec: &PlotSpec, options: &RenderOptions) -> Result<Rendered, PlotError> {
        chosen_format(self, options)?;
        let mut sized = spec.clone();
        (sized.width, sized.height) = options.size(spec);
        Ok(Rendered::text(Format::Svg, crate::svg::render_svg(&sized)))
    }
}

/// The built-in text renderer ([`crate::text::render_text`]).
pub struct TextRenderer;

impl PlotRenderer for TextRenderer {
    fn id(&self) -> &'static str {
        "text"
    }

    fn formats(&self) -> &'static [Format] {
        &[Format::Text]
    }

    fn unsupported(&self, spec: &PlotSpec) -> Vec<String> {
        let mut out: Vec<String> = spec
            .series
            .iter()
            .filter(|s| s.style == crate::SeriesStyle::Contour)
            .map(|s| format!("series '{}': density contours (drawn as points)", s.label))
            .collect();
        if !spec.annotations.is_empty() {
            out.push(format!("{} annotation(s)", spec.annotations.len()));
        }
        if spec
            .series
            .iter()
            .any(|s| s.error_low.is_some() || s.error_high.is_some())
        {
            out.push("error bars".into());
        }
        out
    }

    fn render(&self, spec: &PlotSpec, options: &RenderOptions) -> Result<Rendered, PlotError> {
        chosen_format(self, options)?;
        let (w, h) = options.size(spec);
        // Points to characters: a terminal cell is about 8×16 points.
        let cols = ((w / 8.0).round() as usize).clamp(20, 200);
        let rows = ((h / 16.0).round() as usize).clamp(8, 80);
        Ok(Rendered::text(
            Format::Text,
            crate::text::render_text(spec, cols, rows),
        ))
    }
}

static REGISTRY: RwLock<Vec<Arc<dyn PlotRenderer>>> = RwLock::new(Vec::new());

fn builtins() -> Vec<Arc<dyn PlotRenderer>> {
    vec![Arc::new(SvgRenderer), Arc::new(TextRenderer)]
}

/// Add a renderer, or replace the one with the same id.
pub fn register_renderer(renderer: Arc<dyn PlotRenderer>) {
    let mut all = REGISTRY.write().unwrap_or_else(|e| e.into_inner());
    all.retain(|r| r.id() != renderer.id());
    all.push(renderer);
}

/// Every renderer this process has: the built-ins, then the registered ones
/// (a registered one replaces a built-in of the same id).
pub fn renderers() -> Vec<Arc<dyn PlotRenderer>> {
    let registered = REGISTRY.read().unwrap_or_else(|e| e.into_inner()).clone();
    let mut out: Vec<Arc<dyn PlotRenderer>> = builtins()
        .into_iter()
        .filter(|b| registered.iter().all(|r| r.id() != b.id()))
        .collect();
    out.extend(registered);
    out
}

/// The renderer with `id`.
pub fn renderer(id: &str) -> Option<Arc<dyn PlotRenderer>> {
    renderers().into_iter().find(|r| r.id() == id)
}

/// Draw `spec` with the renderer `id`.
pub fn render(spec: &PlotSpec, id: &str, options: &RenderOptions) -> Result<Rendered, PlotError> {
    let renderer = renderer(id).ok_or_else(|| {
        PlotError::Unsupported(format!(
            "no plot renderer '{id}' (have: {})",
            renderers()
                .iter()
                .map(|r| r.id())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;
    renderer.render(spec, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PlotSeries;

    struct Fixed;

    impl PlotRenderer for Fixed {
        fn id(&self) -> &'static str {
            "fixed"
        }
        fn formats(&self) -> &'static [Format] {
            &[Format::Text]
        }
        fn unsupported(&self, _: &PlotSpec) -> Vec<String> {
            vec![]
        }
        fn render(&self, _: &PlotSpec, _: &RenderOptions) -> Result<Rendered, PlotError> {
            Ok(Rendered::text(Format::Text, "fixed"))
        }
    }

    fn spec() -> PlotSpec {
        let mut spec = PlotSpec::default();
        spec.series
            .push(PlotSeries::line(vec![0.0, 1.0], vec![0.0, 1.0], "a"));
        spec
    }

    #[test]
    fn the_builtins_draw_and_a_registered_renderer_joins_them() {
        let svg = render(&spec(), "svg", &RenderOptions::default()).unwrap();
        assert_eq!(svg.format, Format::Svg);
        assert!(svg.body_text().unwrap().starts_with("<svg"));
        let text = render(&spec(), "text", &RenderOptions::default()).unwrap();
        assert_eq!(text.format, Format::Text);
        register_renderer(Arc::new(Fixed));
        assert_eq!(
            render(&spec(), "fixed", &RenderOptions::default())
                .unwrap()
                .body_text(),
            Some("fixed")
        );
        let ids: Vec<&str> = renderers().iter().map(|r| r.id()).collect();
        assert!(
            ids.starts_with(&["svg", "text"]) && ids.contains(&"fixed"),
            "{ids:?}"
        );
    }

    #[test]
    fn a_format_a_renderer_does_not_write_is_refused_and_an_unknown_id_names_the_known() {
        let err = render(&spec(), "svg", &RenderOptions::format(Format::Pdf)).unwrap_err();
        assert!(err.to_string().contains("does not write"), "{err}");
        let err = render(&spec(), "nope", &RenderOptions::default()).unwrap_err();
        assert!(err.to_string().contains("svg"), "{err}");
    }

    #[test]
    fn a_contour_series_is_named_by_renderers_that_draw_points_instead() {
        let mut s = spec();
        s.series[0].style = crate::SeriesStyle::Contour;
        assert_eq!(SvgRenderer.unsupported(&s).len(), 1);
        assert_eq!(TextRenderer.unsupported(&s).len(), 1);
    }

    /// The spec's additions are optional: a document written before them
    /// still parses, and they round-trip.
    #[test]
    fn older_specs_parse_and_the_new_fields_round_trip() {
        let old: PlotSpec = serde_json::from_str(r#"{"series":[{"x":[1,2],"y":[3,4]}]}"#).unwrap();
        assert_eq!(old.colormap, crate::Colormap::Viridis);
        assert_eq!(old.contours.levels, 7);
        let mut s = spec();
        s.colormap = crate::Colormap::Magma;
        s.strategy = crate::RenderStrategy::Raster;
        s.contours.labels = true;
        let back: PlotSpec = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(back.colormap, crate::Colormap::Magma);
        assert_eq!(back.strategy, crate::RenderStrategy::Raster);
        assert!(back.contours.labels);
    }
}
