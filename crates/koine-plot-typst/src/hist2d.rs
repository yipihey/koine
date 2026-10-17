//! 2D histogram — the big-N raster path.
//!
//! Bins `(x, y)` (optionally weighted) into a grid, normalizes, and rasterizes
//! to an RGBA PNG through a selectable [`Colormap`] and a linear/log color
//! [`ColorScale`]. Empty bins are transparent so the page shows through.

use crate::colormap::Colormap;

/// How bin values are scaled before colouring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Normalization {
    /// Raw (weighted) counts.
    Count,
    /// ÷ total weight — bins sum to 1 (probability mass).
    ProbabilityMass,
    /// ÷ (total weight × bin area) — integrates to 1 (a density / PDF).
    Density,
}

/// Value→colour scale. `Log` (log₁₀ over positive bins) is essential when a
/// density spans orders of magnitude.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorScale {
    Linear,
    Log,
}

/// A binned 2D histogram over a fixed range. Row-major, `v[iy*nx + ix]`, with
/// `iy = 0` the LOW-y row (flipped only at raster time).
#[derive(Clone, Debug)]
pub struct Hist2D {
    pub nx: usize,
    pub ny: usize,
    pub xr: (f64, f64),
    pub yr: (f64, f64),
    pub v: Vec<f64>,
    pub total_w: f64,
}

impl Hist2D {
    /// Wrap an EXISTING grid of values (row-major, `v[iy*nx+ix]`, iy=0 low-y)
    /// over the given data ranges — the direct z-grid input for analytic
    /// fields, simulation slices, etc. No binning happens.
    pub fn from_grid(
        values: Vec<f64>,
        nx: usize,
        ny: usize,
        xr: (f64, f64),
        yr: (f64, f64),
    ) -> Self {
        assert_eq!(values.len(), nx * ny, "grid size mismatch");
        let total_w = values.iter().sum();
        Self {
            nx,
            ny,
            xr,
            yr,
            v: values,
            total_w,
        }
    }

    /// Bin `(xs, ys)` with optional per-point `weights` into `nx × ny` cells.
    /// `range = None` auto-fits the data extent. Points outside range are
    /// dropped (matplotlib `hist2d` semantics).
    pub fn bin(
        xs: &[f64],
        ys: &[f64],
        weights: Option<&[f64]>,
        nx: usize,
        ny: usize,
        range: Option<((f64, f64), (f64, f64))>,
    ) -> Self {
        assert_eq!(xs.len(), ys.len(), "xs/ys length mismatch");
        assert!(nx > 0 && ny > 0, "bins must be positive");
        let (xr, yr) = range.unwrap_or_else(|| {
            let xmin = xs.iter().cloned().fold(f64::INFINITY, f64::min);
            let xmax = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            let ymin = ys.iter().cloned().fold(f64::INFINITY, f64::min);
            let ymax = ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            ((xmin, xmax), (ymin, ymax))
        });
        let (xw, yw) = (xr.1 - xr.0, yr.1 - yr.0);
        let mut v = vec![0.0f64; nx * ny];
        let mut total_w = 0.0f64;
        for i in 0..xs.len() {
            let (x, y) = (xs[i], ys[i]);
            if x < xr.0 || x > xr.1 || y < yr.0 || y > yr.1 {
                continue;
            }
            let w = weights.map(|ws| ws[i]).unwrap_or(1.0);
            let ix = ((((x - xr.0) / xw) * nx as f64).floor() as usize).min(nx - 1);
            let iy = ((((y - yr.0) / yw) * ny as f64).floor() as usize).min(ny - 1);
            v[iy * nx + ix] += w;
            total_w += w;
        }
        Self {
            nx,
            ny,
            xr,
            yr,
            v,
            total_w,
        }
    }

    /// Apply `norm`, returning `(values, vmin_positive, vmax)` for colouring +
    /// the colorbar.
    pub fn normalized(&self, norm: Normalization) -> (Vec<f64>, f64, f64) {
        let bin_area =
            ((self.xr.1 - self.xr.0) / self.nx as f64) * ((self.yr.1 - self.yr.0) / self.ny as f64);
        let denom = match norm {
            Normalization::Count => 1.0,
            Normalization::ProbabilityMass => self.total_w.max(f64::MIN_POSITIVE),
            Normalization::Density => (self.total_w * bin_area).max(f64::MIN_POSITIVE),
        };
        let out: Vec<f64> = self.v.iter().map(|&c| c / denom).collect();
        let mut vmin = f64::INFINITY;
        let mut vmax = f64::NEG_INFINITY;
        for &c in &out {
            if c > 0.0 {
                vmin = vmin.min(c);
                vmax = vmax.max(c);
            }
        }
        if !vmin.is_finite() {
            vmin = 0.0;
            vmax = 1.0;
        }
        (out, vmin, vmax)
    }

    /// Rasterize to an RGBA PNG. Empty bins are transparent; each bin is
    /// expanded `up × up` px (nearest) so Typst's smooth scaling keeps bins
    /// crisp. Returns `(png_bytes, vmin, vmax)`.
    pub fn to_png(
        &self,
        norm: Normalization,
        cmap: Colormap,
        scale: ColorScale,
        up: usize,
    ) -> (Vec<u8>, f64, f64) {
        let up = up.max(1);
        let (vals, vmin, vmax) = self.normalized(norm);
        let (w, h) = (self.nx * up, self.ny * up);
        let mut rgba = vec![0u8; w * h * 4];

        let map_t = |v: f64| -> Option<f64> {
            if v <= 0.0 {
                return None;
            }
            let t = match scale {
                ColorScale::Linear => v / vmax.max(f64::MIN_POSITIVE),
                ColorScale::Log => {
                    let (lo, hi) = (vmin.ln(), vmax.ln());
                    if (hi - lo).abs() < 1e-12 {
                        1.0
                    } else {
                        (v.ln() - lo) / (hi - lo)
                    }
                }
            };
            Some(t.clamp(0.0, 1.0))
        };

        for iy in 0..self.ny {
            let row = self.ny - 1 - iy; // PNG row 0 = top (high y)
            for ix in 0..self.nx {
                let px = match map_t(vals[iy * self.nx + ix]) {
                    Some(t) => {
                        let c = cmap.sample(t);
                        [c[0], c[1], c[2], 255u8]
                    }
                    None => [0, 0, 0, 0],
                };
                for dy in 0..up {
                    for dx in 0..up {
                        let o = ((row * up + dy) * w + (ix * up + dx)) * 4;
                        rgba[o..o + 4].copy_from_slice(&px);
                    }
                }
            }
        }

        (encode_png_rgba(&rgba, w as u32, h as u32), vmin, vmax)
    }
}

pub(crate) fn encode_png_rgba(rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut png = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("png header");
        writer.write_image_data(rgba).expect("png data");
    }
    png
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_and_density_normalization() {
        // Two points, one weighted ×3, in separate bins over a unit square.
        let xs = [0.25, 0.75];
        let ys = [0.25, 0.75];
        let ws = [1.0, 3.0];
        let h = Hist2D::bin(&xs, &ys, Some(&ws), 2, 2, Some(((0.0, 1.0), (0.0, 1.0))));
        assert_eq!(h.total_w, 4.0);
        // count: the weighted bin holds 3
        let (count, _, cmax) = h.normalized(Normalization::Count);
        assert_eq!(cmax, 3.0);
        assert_eq!(count.iter().cloned().fold(0.0, f64::max), 3.0);
        // probability mass sums to 1
        let (pm, _, _) = h.normalized(Normalization::ProbabilityMass);
        assert!((pm.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        // density integrates to 1: Σ value·binarea == 1 (binarea = 0.25)
        let (den, _, _) = h.normalized(Normalization::Density);
        let integral: f64 = den.iter().map(|v| v * 0.25).sum();
        assert!((integral - 1.0).abs() < 1e-12);
    }

    #[test]
    fn png_is_valid_and_sized() {
        let xs: Vec<f64> = (0..100).map(|i| i as f64).collect();
        let ys = xs.clone();
        let h = Hist2D::bin(&xs, &ys, None, 10, 10, None);
        let (png, _, _) = h.to_png(
            Normalization::Count,
            Colormap::Viridis,
            ColorScale::Linear,
            3,
        );
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G'])); // PNG magic
    }
}
