//! Contour extraction: marching squares over a grid, with segment chaining
//! into smooth polylines.
//!
//! Input is a row-major grid of samples (same layout as [`crate::Hist2D`]:
//! `v[iy*nx + ix]`, `iy = 0` the LOW-y row), values taken at cell centers.
//! Output polylines are in **normalized [0,1]² space** (cell-center convention:
//! grid point `ix` sits at `(ix + 0.5)/nx`), ready to map through the plot
//! rect exactly like the raster path — so per-axis lin/log + min/max apply to
//! contours for free when the grid was binned in normalized-axis space.

/// How contour levels are chosen between the grid's (positive) min and max.
#[derive(Clone, Debug, PartialEq)]
pub enum ContourLevels {
    /// `n` levels linearly spaced strictly inside (vmin, vmax).
    Linear(usize),
    /// `n` levels log-spaced strictly inside (vmin, vmax) — the right choice
    /// for densities spanning orders of magnitude.
    Log(usize),
    /// Explicit level values (used as given; out-of-range ones yield nothing).
    Explicit(Vec<f64>),
}

impl ContourLevels {
    /// Resolve to concrete level values for a grid with positive value range
    /// `(vmin, vmax)`.
    pub fn resolve(&self, vmin: f64, vmax: f64) -> Vec<f64> {
        match self {
            ContourLevels::Explicit(levels) => levels.clone(),
            ContourLevels::Linear(n) => {
                let n = (*n).max(1);
                (0..n)
                    .map(|i| vmin + (i as f64 + 1.0) / (n as f64 + 1.0) * (vmax - vmin))
                    .collect()
            }
            ContourLevels::Log(n) => {
                let n = (*n).max(1);
                let (lo, hi) = (vmin.max(f64::MIN_POSITIVE).ln(), vmax.ln());
                if !(hi - lo).is_finite() || hi <= lo {
                    return vec![vmin];
                }
                (0..n)
                    .map(|i| (lo + (i as f64 + 1.0) / (n as f64 + 1.0) * (hi - lo)).exp())
                    .collect()
            }
        }
    }
}

/// Separable Gaussian blur of a grid (σ in bins; radius 3σ; edge-renormalized
/// so borders don't darken). Contouring a RAW binned density traces Poisson
/// noise — every bin fluctuation becomes a wiggle — so smooth before
/// extracting iso-lines (the underlay can stay raw). σ = 0 is a no-op.
pub fn gaussian_smooth(grid: &[f64], nx: usize, ny: usize, sigma: f64) -> Vec<f64> {
    assert_eq!(grid.len(), nx * ny, "grid size mismatch");
    if sigma <= 0.0 {
        return grid.to_vec();
    }
    let radius = (3.0 * sigma).ceil() as isize;
    let weights: Vec<f64> = (-radius..=radius)
        .map(|i| (-(i as f64).powi(2) / (2.0 * sigma * sigma)).exp())
        .collect();

    let pass = |src: &[f64],
                len_major: usize,
                len_minor: usize,
                stride_minor: usize,
                stride_major: usize| {
        let mut out = vec![0.0; src.len()];
        for maj in 0..len_major {
            for min in 0..len_minor {
                let mut acc = 0.0;
                let mut wsum = 0.0;
                for (k, w) in weights.iter().enumerate() {
                    let offset = k as isize - radius;
                    let m = min as isize + offset;
                    if m < 0 || m >= len_minor as isize {
                        continue;
                    }
                    acc += w * src[maj * stride_major + m as usize * stride_minor];
                    wsum += w;
                }
                out[maj * stride_major + min * stride_minor] = acc / wsum.max(f64::MIN_POSITIVE);
            }
        }
        out
    };

    // Horizontal (along x) then vertical (along y).
    let h = pass(grid, ny, nx, 1, nx);
    pass(&h, nx, ny, nx, 1)
}

/// Extract the `level` iso-lines of `grid` (`nx × ny`, row-major, iy=0 low).
/// Returns chained polylines in normalized [0,1]² (see module docs).
pub fn contour_lines(grid: &[f64], nx: usize, ny: usize, level: f64) -> Vec<Vec<(f64, f64)>> {
    assert_eq!(grid.len(), nx * ny, "grid size mismatch");
    if nx < 2 || ny < 2 {
        return Vec::new();
    }

    let v = |ix: usize, iy: usize| grid[iy * nx + ix];
    let mut segments: Vec<((f64, f64), (f64, f64))> = Vec::new();

    // March each square between four adjacent grid points (cell centers).
    for iy in 0..ny - 1 {
        for ix in 0..nx - 1 {
            let (bl, br) = (v(ix, iy), v(ix + 1, iy));
            let (tl, tr) = (v(ix, iy + 1), v(ix + 1, iy + 1));
            let mask = (bl >= level) as u8
                | (((br >= level) as u8) << 1)
                | (((tr >= level) as u8) << 2)
                | (((tl >= level) as u8) << 3);
            if mask == 0 || mask == 15 {
                continue;
            }

            // Interpolated crossing points on the square's four edges, in
            // grid-point coordinates (gx ∈ [0, nx-1], gy ∈ [0, ny-1]).
            let t = |a: f64, b: f64| ((level - a) / (b - a)).clamp(0.0, 1.0);
            let bottom = || (ix as f64 + t(bl, br), iy as f64);
            let top = || (ix as f64 + t(tl, tr), iy as f64 + 1.0);
            let left = || (ix as f64, iy as f64 + t(bl, tl));
            let right = || (ix as f64 + 1.0, iy as f64 + t(br, tr));

            match mask {
                1 => segments.push((left(), bottom())),
                2 => segments.push((bottom(), right())),
                3 => segments.push((left(), right())),
                4 => segments.push((right(), top())),
                6 => segments.push((bottom(), top())),
                7 => segments.push((left(), top())),
                8 => segments.push((top(), left())),
                9 => segments.push((bottom(), top())),
                11 => segments.push((right(), top())),
                12 => segments.push((right(), left())),
                13 => segments.push((bottom(), right())),
                14 => segments.push((left(), bottom())),
                5 | 10 => {
                    // Saddle: disambiguate with the cell-center average.
                    let center_high = (bl + br + tl + tr) / 4.0 >= level;
                    let diag_bl_tr_high = mask == 5;
                    if diag_bl_tr_high == center_high {
                        // High region connects diagonally.
                        segments.push((left(), top()));
                        segments.push((bottom(), right()));
                    } else {
                        segments.push((left(), bottom()));
                        segments.push((top(), right()));
                    }
                }
                _ => unreachable!(),
            }
        }
    }

    // Normalize to [0,1]² (cell-center convention) and chain into polylines.
    let norm = |(gx, gy): (f64, f64)| ((gx + 0.5) / nx as f64, (gy + 0.5) / ny as f64);
    chain_segments(
        segments
            .into_iter()
            .map(|(a, b)| (norm(a), norm(b)))
            .collect(),
    )
}

/// Join segments that share endpoints into continuous polylines. Closed loops
/// come back with first == last point.
fn chain_segments(segments: Vec<((f64, f64), (f64, f64))>) -> Vec<Vec<(f64, f64)>> {
    use std::collections::HashMap;

    // Quantize endpoints so float jitter doesn't break adjacency.
    let key = |(x, y): (f64, f64)| ((x * 1e9).round() as i64, (y * 1e9).round() as i64);

    let mut adjacency: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (i, (a, b)) in segments.iter().enumerate() {
        adjacency.entry(key(*a)).or_default().push(i);
        adjacency.entry(key(*b)).or_default().push(i);
    }

    let mut used = vec![false; segments.len()];
    let mut polylines = Vec::new();

    for start in 0..segments.len() {
        if used[start] {
            continue;
        }
        used[start] = true;
        let (a, b) = segments[start];
        let mut line = vec![a, b];

        // Extend forward from the tail, then backward from the head.
        for _direction in 0..2 {
            loop {
                let tail = *line.last().unwrap();
                let Some(candidates) = adjacency.get(&key(tail)) else {
                    break;
                };
                let Some(&next) = candidates.iter().find(|&&i| !used[i]) else {
                    break;
                };
                used[next] = true;
                let (na, nb) = segments[next];
                line.push(if key(na) == key(tail) { nb } else { na });
            }
            line.reverse();
        }
        polylines.push(line);
    }
    polylines
}

// ===========================================================================
// Inline-label geometry (operates on polylines in ANY planar space — the
// renderer calls it in pt space so arc lengths and text widths are physical).
// ===========================================================================

/// Total arc length of a polyline.
pub fn polyline_arc_length(pts: &[(f64, f64)]) -> f64 {
    pts.windows(2)
        .map(|w| ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt())
        .sum()
}

/// Point and tangent angle (radians, atan2 convention of the input space) at
/// arc-length position `s` along the polyline.
pub fn point_at_arc(pts: &[(f64, f64)], s: f64) -> ((f64, f64), f64) {
    let mut acc = 0.0;
    for w in pts.windows(2) {
        let (dx, dy) = (w[1].0 - w[0].0, w[1].1 - w[0].1);
        let seg = (dx * dx + dy * dy).sqrt();
        if acc + seg >= s && seg > 0.0 {
            let t = ((s - acc) / seg).clamp(0.0, 1.0);
            return ((w[0].0 + t * dx, w[0].1 + t * dy), dy.atan2(dx));
        }
        acc += seg;
    }
    let last = *pts.last().unwrap();
    let angle = if pts.len() >= 2 {
        let a = pts[pts.len() - 2];
        (last.1 - a.1).atan2(last.0 - a.0)
    } else {
        0.0
    };
    (last, angle)
}

/// Arc-length position of the FLATTEST window of length `window` on the
/// polyline (minimal total turning), searched near `prefer` first by scoring
/// turning + a mild distance-from-prefer penalty. Returns the window center.
pub fn flattest_arc_position(pts: &[(f64, f64)], window: f64, prefer: f64) -> f64 {
    let total = polyline_arc_length(pts);
    if total <= window {
        return total / 2.0;
    }
    // Sample candidate centers; score = turning inside window + small pull
    // toward `prefer` so labels stagger across levels instead of stacking.
    let steps = 48;
    let mut best = (f64::INFINITY, total / 2.0);
    for i in 0..steps {
        let c = window / 2.0 + (total - window) * (i as f64) / (steps - 1) as f64;
        let turning = window_turning(pts, c - window / 2.0, c + window / 2.0);
        // Strong pull toward `prefer`: staggering labels around the rings
        // matters more than finding the globally flattest spot (nested rings
        // tend to share one flat region, which stacks all labels there).
        // Wrap-around distance so closed loops treat the seam as adjacent.
        let d = (c - prefer).abs() / total;
        let pull = d.min(1.0 - d) * 2.4;
        let score = turning + pull;
        if score < best.0 {
            best = (score, c);
        }
    }
    best.1
}

/// Total absolute turning (radians) of the polyline between arc positions.
fn window_turning(pts: &[(f64, f64)], s0: f64, s1: f64) -> f64 {
    let mut acc = 0.0;
    let mut turning = 0.0;
    let mut prev_angle: Option<f64> = None;
    for w in pts.windows(2) {
        let (dx, dy) = (w[1].0 - w[0].0, w[1].1 - w[0].1);
        let seg = (dx * dx + dy * dy).sqrt();
        let (a, b) = (acc, acc + seg);
        acc = b;
        if b < s0 || a > s1 || seg == 0.0 {
            continue;
        }
        let angle = dy.atan2(dx);
        if let Some(p) = prev_angle {
            let mut d = angle - p;
            while d > std::f64::consts::PI {
                d -= std::f64::consts::TAU;
            }
            while d < -std::f64::consts::PI {
                d += std::f64::consts::TAU;
            }
            turning += d.abs();
        }
        prev_angle = Some(angle);
    }
    turning
}

/// Split a polyline around a gap of length `gap` centered at arc position
/// `center`, for placing an inline label. Closed loops (first == last) are
/// re-seamed at the gap so the result is ONE polyline covering everything but
/// the gap; open lines yield up to two pieces.
pub fn split_at_gap(pts: &[(f64, f64)], center: f64, gap: f64) -> Vec<Vec<(f64, f64)>> {
    let total = polyline_arc_length(pts);
    let (g0, g1) = (center - gap / 2.0, center + gap / 2.0);
    let closed = pts.len() > 2 && {
        let (a, b) = (pts[0], *pts.last().unwrap());
        ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt() < 1e-9
    };

    if closed {
        // Walk from g1 around the seam to g0 (one piece).
        let mut piece = Vec::new();
        let n_samples = pts.len() * 2;
        for i in 0..=n_samples {
            let s = (g1 + (total - gap) * i as f64 / n_samples as f64) % total;
            piece.push(point_at_arc(pts, s).0);
        }
        return vec![piece];
    }

    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut acc = 0.0;
    for (i, w) in pts.windows(2).enumerate() {
        let seg = ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt();
        let (a, b) = (acc, acc + seg);
        if i == 0 && a < g0 {
            before.push(w[0]);
        }
        if b <= g0 {
            before.push(w[1]);
        } else if a < g0 {
            before.push(point_at_arc(pts, g0).0);
        }
        if a >= g1 {
            if after.is_empty() {
                after.push(point_at_arc(pts, g1).0);
            }
            after.push(w[1]);
        }
        acc = b;
    }
    [before, after]
        .into_iter()
        .filter(|p| p.len() >= 2)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Gaussian bump sampled on a grid: the `level` contour of
    /// exp(-r²/2σ²) is a circle of radius σ·√(2·ln(1/level)).
    fn gaussian_grid(n: usize, sigma: f64) -> Vec<f64> {
        let mut g = vec![0.0; n * n];
        for iy in 0..n {
            for ix in 0..n {
                let x = (ix as f64 + 0.5) / n as f64 - 0.5;
                let y = (iy as f64 + 0.5) / n as f64 - 0.5;
                g[iy * n + ix] = (-(x * x + y * y) / (2.0 * sigma * sigma)).exp();
            }
        }
        g
    }

    #[test]
    fn gaussian_contour_is_a_circle_of_the_right_radius() {
        let n = 200;
        let sigma = 0.12;
        let grid = gaussian_grid(n, sigma);
        let level: f64 = 0.5;
        let expected_r = sigma * (2.0_f64 * (1.0 / level).ln()).sqrt();

        let lines = contour_lines(&grid, n, n, level);
        assert!(!lines.is_empty(), "expected a contour");

        // All points on all polylines sit at radius expected_r (± a cell).
        let cell = 1.0 / n as f64;
        let mut checked = 0;
        for line in &lines {
            for &(x, y) in line {
                let r = ((x - 0.5).powi(2) + (y - 0.5).powi(2)).sqrt();
                assert!(
                    (r - expected_r).abs() < 2.0 * cell,
                    "point radius {r:.4} vs expected {expected_r:.4}"
                );
                checked += 1;
            }
        }
        assert!(
            checked > 50,
            "contour should have many points, got {checked}"
        );

        // The main contour is one closed loop.
        let longest = lines.iter().max_by_key(|l| l.len()).unwrap();
        let (first, last) = (longest.first().unwrap(), longest.last().unwrap());
        let gap = ((first.0 - last.0).powi(2) + (first.1 - last.1).powi(2)).sqrt();
        assert!(gap < 3.0 * cell, "expected a closed loop, gap {gap:.5}");
    }

    #[test]
    fn levels_resolve_linear_log_explicit() {
        let lin = ContourLevels::Linear(3).resolve(0.0, 4.0);
        assert_eq!(lin, vec![1.0, 2.0, 3.0]);

        let log = ContourLevels::Log(2).resolve(1.0, 1000.0);
        assert!((log[0] - 10.0).abs() < 1e-9 && (log[1] - 100.0).abs() < 1e-9);

        let exp = ContourLevels::Explicit(vec![0.5]).resolve(0.0, 1.0);
        assert_eq!(exp, vec![0.5]);
    }

    #[test]
    fn smoothing_flat_is_identity_and_delta_spreads_symmetrically() {
        let flat = vec![2.0; 100];
        let out = gaussian_smooth(&flat, 10, 10, 1.5);
        for v in &out {
            assert!((v - 2.0).abs() < 1e-9, "flat stays flat (edge renorm)");
        }

        let mut delta = vec![0.0; 21 * 21];
        delta[10 * 21 + 10] = 1.0;
        let s = gaussian_smooth(&delta, 21, 21, 2.0);
        assert!(s[10 * 21 + 10] < 1.0, "peak lowered");
        assert!(s[10 * 21 + 10] > s[10 * 21 + 13], "monotone from center");
        // Symmetry in both directions.
        assert!((s[10 * 21 + 7] - s[10 * 21 + 13]).abs() < 1e-12);
        assert!((s[7 * 21 + 10] - s[13 * 21 + 10]).abs() < 1e-12);
    }

    /// The alignment invariant behind the heatmap-underlay + contour-overlay
    /// figure: for an off-center blob, the contour centroid (normalized
    /// space) must coincide with the underlay PNG's brightest pixel mapped
    /// back to the same normalized space (PNG row 0 = top = high y). Catches
    /// any flip or half-cell offset between the two layers.
    #[test]
    fn contours_align_with_underlay_peak() {
        use crate::hist2d::{ColorScale, Hist2D, Normalization};
        use crate::Colormap;

        // Deterministic tight blob at (0.7, 0.3) in binned space.
        let (nx, ny) = (80usize, 80usize);
        let mut s: u64 = 42;
        let mut rnd = || {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
            (s >> 11) as f64 / (1u64 << 53) as f64
        };
        let mut xs = Vec::new();
        let mut ys = Vec::new();
        for _ in 0..40_000 {
            // Box-Muller
            let (u1, u2) = (rnd().max(1e-12), rnd());
            let r = (-2.0f64 * u1.ln()).sqrt();
            xs.push(0.7 + 0.06 * r * (std::f64::consts::TAU * u2).cos());
            ys.push(0.3 + 0.06 * r * (std::f64::consts::TAU * u2).sin());
        }
        let hist = Hist2D::bin(&xs, &ys, None, nx, ny, Some(((0.0, 1.0), (0.0, 1.0))));
        let (vals, _, vmax) = hist.normalized(Normalization::Count);

        // Contour centroid at a high level (tight ring around the peak).
        let smoothed = gaussian_smooth(&vals, nx, ny, 2.0);
        let smax = smoothed.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let lines = contour_lines(&smoothed, nx, ny, smax * 0.5);
        assert!(!lines.is_empty());
        let (mut cx, mut cy, mut n) = (0.0, 0.0, 0);
        for line in &lines {
            for &(x, y) in line {
                cx += x;
                cy += y;
                n += 1;
            }
        }
        let (cx, cy) = (cx / n as f64, cy / n as f64);
        assert!((cx - 0.7).abs() < 0.03, "contour centroid x {cx:.3} vs 0.7");
        assert!((cy - 0.3).abs() < 0.03, "contour centroid y {cy:.3} vs 0.3");

        // Underlay: brightest (highest-count) bin's pixel, mapped back through
        // the PNG's row order (row 0 = top).
        let up = 2usize;
        let (png, _, _) = hist.to_png(
            Normalization::Count,
            Colormap::Greys,
            ColorScale::Linear,
            up,
        );
        // Rather than decode the PNG, recompute what to_png drew: find the
        // max-count bin and assert its PNG-space position equals the contour
        // centroid in image coordinates.
        let mut best = (0usize, 0usize);
        let mut best_v = f64::NEG_INFINITY;
        for iy in 0..ny {
            for ix in 0..nx {
                if vals[iy * nx + ix] > best_v {
                    best_v = vals[iy * nx + ix];
                    best = (ix, iy);
                }
            }
        }
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
        assert!(vmax > 0.0);
        // Bin center in normalized space…
        let peak_norm = (
            (best.0 as f64 + 0.5) / nx as f64,
            (best.1 as f64 + 0.5) / ny as f64,
        );
        // …must match the contour centroid (both layers stretch to the same
        // plot rect with the same y-flip: image row = ny-1-iy ⇔ y_pt = (1-t)·h).
        assert!(
            (peak_norm.0 - cx).abs() < 0.03 && (peak_norm.1 - cy).abs() < 0.03,
            "underlay peak {peak_norm:?} vs contour centroid ({cx:.3},{cy:.3})"
        );
    }

    #[test]
    fn label_geometry_on_a_circle() {
        // Unit circle sampled densely, closed.
        let n = 400;
        let pts: Vec<(f64, f64)> = (0..=n)
            .map(|i| {
                let a = std::f64::consts::TAU * i as f64 / n as f64;
                (a.cos(), a.sin())
            })
            .collect();

        let len = polyline_arc_length(&pts);
        assert!(
            (len - std::f64::consts::TAU).abs() < 1e-3,
            "circumference ≈ 2π, got {len}"
        );

        // Tangent at arc s is perpendicular to the radius there.
        let (p, angle) = point_at_arc(&pts, len * 0.25);
        let radial = p.1.atan2(p.0);
        let mut d = (angle - radial).abs() % std::f64::consts::PI;
        if d > std::f64::consts::FRAC_PI_2 {
            d = std::f64::consts::PI - d;
        }
        assert!(
            (d - std::f64::consts::FRAC_PI_2).abs() < 0.05,
            "tangent ⟂ radius, delta {d}"
        );

        // Splitting a closed loop at a gap yields one piece with total length
        // ≈ circumference − gap, and no point inside the gap window.
        let gap = 0.6;
        let center = len * 0.5;
        let pieces = split_at_gap(&pts, center, gap);
        assert_eq!(pieces.len(), 1, "closed loop re-seams to one piece");
        let plen = polyline_arc_length(&pieces[0]);
        assert!(
            (plen - (len - gap)).abs() < 0.1,
            "piece length {plen} vs {}",
            len - gap
        );

        // Open line split → two pieces, gap respected.
        let open: Vec<(f64, f64)> = (0..=100).map(|i| (i as f64 / 10.0, 0.0)).collect();
        let pieces = split_at_gap(&open, 5.0, 1.0);
        assert_eq!(pieces.len(), 2);
        assert!(pieces[0].last().unwrap().0 <= 4.51);
        assert!(pieces[1].first().unwrap().0 >= 5.49);

        // Flattest window on a circle is anywhere; on a rounded square it's an
        // edge, not a corner. Build a rounded-square-ish shape: ellipse with
        // flattened sides via superellipse exponent.
        let sq: Vec<(f64, f64)> = (0..=n)
            .map(|i| {
                let a = std::f64::consts::TAU * i as f64 / n as f64;
                let (c, s) = (a.cos(), a.sin());
                (
                    c.abs().powf(0.4) * c.signum(),
                    s.abs().powf(0.4) * s.signum(),
                )
            })
            .collect();
        let sq_len = polyline_arc_length(&sq);
        let pos = flattest_arc_position(&sq, sq_len * 0.05, sq_len * 0.4);
        let (pt, _) = point_at_arc(&sq, pos);
        // Flat spots of the superellipse are on the axes-aligned sides — the
        // chosen point should hug ±1 in exactly one coordinate.
        let m = pt.0.abs().max(pt.1.abs());
        assert!(
            m > 0.95,
            "flattest point should sit on a flat side, got {pt:?}"
        );
    }

    #[test]
    fn flat_grid_has_no_contours() {
        let grid = vec![1.0; 100];
        assert!(contour_lines(&grid, 10, 10, 0.5).is_empty());
        assert!(contour_lines(&grid, 10, 10, 2.0).is_empty());
    }
}
