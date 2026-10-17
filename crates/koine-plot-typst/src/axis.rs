//! Axes: linear/log scaling and manual/auto min–max limits.
//!
//! An `Axis` owns two independent, interactively-adjustable knobs — the two most
//! frequently-reached-for plot controls:
//!   * **scale**  — `Linear` or `Log` (log₁₀)
//!   * **limits** — `Auto` (fit data) or `Manual(min, max)` (zoom)
//!
//! Everything downstream (line/scatter geometry, tick placement, gridlines) goes
//! through `resolved()` + `norm()`, so flipping a scale or dragging a limit is a
//! one-field change that re-renders correctly.

/// Axis scale transform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scale {
    Linear,
    /// log₁₀. Only defined for strictly-positive values; non-positive data is
    /// dropped by `norm()` (returns `None`).
    Log,
}

/// Axis extent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Limits {
    /// Fit to data (with a small pad on linear axes).
    Auto,
    /// Explicit `[min, max]` — the "adjust min/max" control.
    Manual(f64, f64),
}

/// One plot axis.
#[derive(Clone, Debug)]
pub struct Axis {
    pub scale: Scale,
    pub limits: Limits,
    pub label: Option<String>,
}

/// A resolved tick: data value, normalized position `t ∈ [0,1]`, and label.
#[derive(Clone, Debug, PartialEq)]
pub struct Tick {
    pub value: f64,
    pub t: f64,
    pub label: String,
}

impl Default for Axis {
    fn default() -> Self {
        Self {
            scale: Scale::Linear,
            limits: Limits::Auto,
            label: None,
        }
    }
}

impl Axis {
    pub fn linear() -> Self {
        Self::default()
    }
    pub fn log() -> Self {
        Self {
            scale: Scale::Log,
            ..Self::default()
        }
    }
    pub fn with_scale(mut self, scale: Scale) -> Self {
        self.scale = scale;
        self
    }
    /// Set an explicit min/max (the "adjust the minimum and maximum" control).
    pub fn with_limits(mut self, min: f64, max: f64) -> Self {
        self.limits = Limits::Manual(min, max);
        self
    }
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Resolve concrete `(lo, hi)` bounds from the data extent, honoring
    /// `Manual` limits and log-positivity. Guaranteed `lo < hi`.
    pub fn resolved(&self, data_min: f64, data_max: f64) -> (f64, f64) {
        let (mut lo, mut hi) = match self.limits {
            Limits::Manual(a, b) => (a.min(b), a.max(b)),
            Limits::Auto => match self.scale {
                Scale::Linear => {
                    let pad = if (data_max - data_min).abs() < 1e-12 {
                        1.0
                    } else {
                        (data_max - data_min) * 0.05
                    };
                    (data_min - pad, data_max + pad)
                }
                Scale::Log => {
                    // Fit to positive data; floor at a small positive if needed.
                    let lo = if data_min > 0.0 {
                        data_min
                    } else {
                        data_max.max(1e-9) * 1e-6
                    };
                    (lo, data_max.max(lo * 10.0))
                }
            },
        };
        if self.scale == Scale::Log {
            // Log needs strictly-positive, ordered bounds.
            if lo <= 0.0 {
                lo = hi.max(1e-9) * 1e-6;
            }
            if hi <= lo {
                hi = lo * 10.0;
            }
        } else if (hi - lo).abs() < 1e-12 {
            hi = lo + 1.0;
        }
        (lo, hi)
    }

    /// Map a data value to normalized position `t ∈ [0,1]` within `(lo, hi)`.
    /// Returns `None` for values a scale can't represent (log of ≤ 0) — the
    /// caller drops those points. In-range clamping is the caller's choice;
    /// this returns the true (possibly <0 or >1) `t` so segments can be clipped.
    pub fn norm(&self, v: f64, lo: f64, hi: f64) -> Option<f64> {
        match self.scale {
            Scale::Linear => {
                let d = hi - lo;
                if d.abs() < 1e-12 {
                    Some(0.0)
                } else {
                    Some((v - lo) / d)
                }
            }
            Scale::Log => {
                if v <= 0.0 || lo <= 0.0 || hi <= 0.0 {
                    return None;
                }
                let (l, h) = (lo.ln(), hi.ln());
                if (h - l).abs() < 1e-12 {
                    Some(0.0)
                } else {
                    Some((v.ln() - l) / (h - l))
                }
            }
        }
    }

    /// Tick marks for `(lo, hi)`: "nice" round values on a linear axis, decade
    /// (10ᵏ) ticks on a log axis. `target` is the desired count on linear.
    pub fn ticks(&self, lo: f64, hi: f64, target: usize) -> Vec<Tick> {
        match self.scale {
            Scale::Linear => self.linear_ticks(lo, hi, target.max(2)),
            Scale::Log => self.log_ticks(lo, hi),
        }
    }

    fn linear_ticks(&self, lo: f64, hi: f64, target: usize) -> Vec<Tick> {
        let step = nice_num((hi - lo) / target as f64, true);
        if step <= 0.0 || !step.is_finite() {
            return Vec::new();
        }
        let start = (lo / step).ceil() * step;
        let mut out = Vec::new();
        let mut v = start;
        // +0.5 step guard against fp drift at the top edge.
        while v <= hi + step * 0.5 {
            if v >= lo - step * 0.5 {
                let t = (v - lo) / (hi - lo);
                out.push(Tick {
                    value: v,
                    t,
                    label: fmt_lin(v, step),
                });
            }
            v += step;
        }
        out
    }

    fn log_ticks(&self, lo: f64, hi: f64) -> Vec<Tick> {
        let (l, h) = (lo.log10().floor() as i32, hi.log10().ceil() as i32);
        let mut out = Vec::new();
        for k in l..=h {
            let v = 10f64.powi(k);
            if v < lo * 0.9999 || v > hi * 1.0001 {
                continue;
            }
            let t = (v.ln() - lo.ln()) / (hi.ln() - lo.ln());
            out.push(Tick {
                value: v,
                t,
                label: fmt_pow10(k),
            });
        }
        // If the decade span is tiny (<2 decades), the endpoints alone read
        // better than a lone interior decade.
        if out.len() < 2 {
            let t_hi = 1.0;
            out.clear();
            out.push(Tick {
                value: lo,
                t: 0.0,
                label: fmt_sig(lo),
            });
            out.push(Tick {
                value: hi,
                t: t_hi,
                label: fmt_sig(hi),
            });
        }
        out
    }
}

/// Round a positive number to a "nice" value (1, 2, 5 × 10ᵏ). `round=false`
/// takes the ceiling variant.
fn nice_num(x: f64, round: bool) -> f64 {
    if x <= 0.0 || !x.is_finite() {
        return 0.0;
    }
    let exp = x.log10().floor();
    let f = x / 10f64.powf(exp);
    let nf = if round {
        if f < 1.5 {
            1.0
        } else if f < 3.0 {
            2.0
        } else if f < 7.0 {
            5.0
        } else {
            10.0
        }
    } else if f <= 1.0 {
        1.0
    } else if f <= 2.0 {
        2.0
    } else if f <= 5.0 {
        5.0
    } else {
        10.0
    };
    nf * 10f64.powf(exp)
}

fn fmt_lin(v: f64, step: f64) -> String {
    // Decimals implied by the tick step.
    let decimals = (-step.log10().floor()).max(0.0) as usize;
    let decimals = decimals.min(6);
    let s = format!("{v:.decimals$}");
    if s == format!("-{:.decimals$}", 0.0) {
        format!("{:.decimals$}", 0.0)
    } else {
        s
    }
}

fn fmt_pow10(k: i32) -> String {
    match k {
        0 => "1".into(),
        1 => "10".into(),
        2 => "100".into(),
        -1 => "0.1".into(),
        -2 => "0.01".into(),
        _ => format!("1e{k}"),
    }
}

fn fmt_sig(v: f64) -> String {
    if v == 0.0 {
        "0".into()
    } else if v.abs() >= 100.0 || v.abs() < 0.01 {
        format!("{v:.1e}")
    } else if v.abs() >= 1.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.3}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_auto_pads_and_maps() {
        let a = Axis::linear();
        let (lo, hi) = a.resolved(0.0, 10.0);
        assert!(lo < 0.0 && hi > 10.0); // 5% pad
        assert!((a.norm(lo, lo, hi).unwrap() - 0.0).abs() < 1e-9);
        assert!((a.norm(hi, lo, hi).unwrap() - 1.0).abs() < 1e-9);
        // midpoint
        assert!((a.norm((lo + hi) / 2.0, lo, hi).unwrap() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn manual_limits_zoom() {
        let a = Axis::linear().with_limits(2.0, 4.0);
        let (lo, hi) = a.resolved(0.0, 10.0); // data ignored
        assert_eq!((lo, hi), (2.0, 4.0));
        assert_eq!(a.norm(3.0, lo, hi).unwrap(), 0.5);
        // Out-of-range value maps outside [0,1] (caller clips).
        assert!(a.norm(5.0, lo, hi).unwrap() > 1.0);
    }

    #[test]
    fn log_maps_decades_and_drops_nonpositive() {
        let a = Axis::log().with_limits(1.0, 1000.0);
        let (lo, hi) = a.resolved(1.0, 1000.0);
        assert_eq!((lo, hi), (1.0, 1000.0));
        // 10 sits at 1/3 across three decades.
        assert!((a.norm(10.0, lo, hi).unwrap() - 1.0 / 3.0).abs() < 1e-9);
        assert!((a.norm(100.0, lo, hi).unwrap() - 2.0 / 3.0).abs() < 1e-9);
        assert!(a.norm(0.0, lo, hi).is_none());
        assert!(a.norm(-5.0, lo, hi).is_none());
        let ticks = a.ticks(lo, hi, 5);
        // decades 1,10,100,1000
        assert_eq!(ticks.len(), 4);
        assert_eq!(ticks[0].label, "1");
        assert_eq!(ticks[3].label, "1e3");
    }

    #[test]
    fn linear_ticks_are_nice_round_numbers() {
        let a = Axis::linear();
        let ticks = a.ticks(0.0, 10.0, 5);
        let vals: Vec<f64> = ticks.iter().map(|t| t.value).collect();
        // nice step for range 10 / target 5 is 2 → 0,2,4,6,8,10.
        assert!(vals.contains(&0.0) && vals.contains(&10.0));
        assert!(vals.iter().all(|v| (v / 2.0).fract().abs() < 1e-9));
        // positions are normalized correctly (10 sits at the right edge).
        assert!((ticks.last().unwrap().t - 1.0).abs() < 1e-9);
    }
}
