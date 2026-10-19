//! What GADGET-4 writes while it runs: `info.txt` (one `Sync-Point` line per
//! step, also echoed on stdout) and the `MEMORY:` lines of `memory.txt`.

/// `Sync-Point 3, Time: 0.100898, Redshift: 8.91104, Systemstep: 0.00030007,
/// Dloga: 0.00297844, Nsync-grv:       4096, Nsync-hyd:          0`
#[derive(Debug, Clone, PartialEq)]
pub struct SyncPoint {
    pub step: u64,
    /// The scale factor `a` in comoving runs, the physical time otherwise.
    pub time: f64,
    pub redshift: Option<f64>,
    /// `Systemstep`: the size of the step that led here.
    pub dt: f64,
    pub dloga: Option<f64>,
    pub nsync_grv: Option<u64>,
    pub nsync_hyd: Option<u64>,
}

pub fn parse_sync_point(line: &str) -> Option<SyncPoint> {
    let rest = line.trim().strip_prefix("Sync-Point ")?;
    let mut parts = rest.split(',');
    let step = parts.next()?.trim().parse().ok()?;
    let mut sp = SyncPoint {
        step,
        time: f64::NAN,
        redshift: None,
        dt: f64::NAN,
        dloga: None,
        nsync_grv: None,
        nsync_hyd: None,
    };
    for p in parts {
        let (k, v) = p.split_once(':')?;
        let v = v.trim();
        match k.trim() {
            "Time" => sp.time = v.parse().ok()?,
            "Redshift" => sp.redshift = v.parse().ok(),
            "Systemstep" => sp.dt = v.parse().ok()?,
            "Dloga" => sp.dloga = v.parse().ok(),
            "Nsync-grv" => sp.nsync_grv = v.parse().ok(),
            "Nsync-hyd" => sp.nsync_hyd = v.parse().ok(),
            _ => {}
        }
    }
    if sp.time.is_nan() || sp.dt.is_nan() {
        None
    } else {
        Some(sp)
    }
}

/// `MEMORY:  Largest Allocation = 365.554 Mbyte  |  ...` -> 365.554
pub fn parse_memory_line(line: &str) -> Option<f64> {
    let rest = line.trim().strip_prefix("MEMORY:")?;
    let (_, v) = rest.split_once("Largest Allocation =")?;
    v.split_whitespace().next()?.parse().ok()
}

/// How far a run is, 0..=1, from the scale factor or time and the run's span.
/// Comoving runs are integrated in `ln a`, so the fraction is taken there.
pub fn fraction(time: f64, begin: f64, max: f64, comoving: bool) -> Option<f64> {
    let (t, b, m) = if comoving {
        if time <= 0.0 || begin <= 0.0 || max <= 0.0 {
            return None;
        }
        (time.ln(), begin.ln(), max.ln())
    } else {
        (time, begin, max)
    };
    if m <= b {
        return None;
    }
    Some(((t - b) / (m - b)).clamp(0.0, 1.0))
}
