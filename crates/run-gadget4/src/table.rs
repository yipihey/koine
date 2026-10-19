//! The adapter's table of GADGET-4 runtime parameters: group, type, mutability,
//! unit and documentation, one row per `add_param` in `src/data/allvars.cc`.
//!
//! The mutability column is not a judgement of ours: GADGET-4 registers every
//! parameter as `PARAM_FIXED` (a `RestartFlag 1` restart ignores a new value) or
//! `PARAM_CHANGEABLE` (a restart takes it), and that flag is `never` or
//! `restart` here. `TimeMax` is `FIXED` in the table but the code says "can be
//! changed nevertheless through special function" and the manual says a
//! restart may extend it, so it is `restart`.

use std::sync::OnceLock;

/// The type GADGET-4 parses the value as (`PARAM_INT`/`PARAM_DOUBLE`/`PARAM_STRING`),
/// with strings that name files split out as `Path`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Int,
    Float,
    String,
    Path,
}

#[derive(Debug, Clone)]
pub struct ParamDef {
    /// The native tag; a trailing `#` stands for a class or particle-type number.
    pub pattern: String,
    pub group: String,
    pub kind: Kind,
    /// `never` or `restart` (GADGET-4 has no `always`).
    pub mutable: String,
    pub unit: String,
    pub advanced: bool,
    pub description: String,
}

/// A table row resolved for one concrete tag (`SofteningComovingClass3`).
#[derive(Debug, Clone)]
pub struct Resolved {
    /// Position in the table, then the class number: the render order.
    pub order: (usize, u32),
    pub native: String,
    pub def: ParamDef,
    pub description: String,
}

impl Resolved {
    pub fn qualified(&self) -> String {
        qualify(&self.def.group, &self.native)
    }
}

pub fn qualify(group: &str, native: &str) -> String {
    format!("{group}.{native}")
}

/// `io.OutputDir` -> (`io`, `OutputDir`). GADGET-4 tags contain no dots, so the
/// first dot is the group boundary.
pub fn split_qualified(q: &str) -> Option<(&str, &str)> {
    q.split_once('.')
        .filter(|(g, n)| !g.is_empty() && !n.is_empty())
}

pub fn defs() -> &'static [ParamDef] {
    static DEFS: OnceLock<Vec<ParamDef>> = OnceLock::new();
    DEFS.get_or_init(|| {
        include_str!("../data/parameters.tsv")
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .map(|l| {
                let f: Vec<&str> = l.split('\t').collect();
                assert_eq!(f.len(), 7, "bad row in data/parameters.tsv: {l}");
                ParamDef {
                    pattern: f[0].to_string(),
                    group: f[1].to_string(),
                    kind: match f[2] {
                        "int" => Kind::Int,
                        "float" => Kind::Float,
                        "string" => Kind::String,
                        "path" => Kind::Path,
                        other => panic!("bad type `{other}` in data/parameters.tsv"),
                    },
                    mutable: f[3].to_string(),
                    unit: f[4].to_string(),
                    advanced: f[5] == "1",
                    description: f[6].to_string(),
                }
            })
            .collect()
    })
}

/// The row for a native tag, if the table knows it.
pub fn lookup(native: &str) -> Option<Resolved> {
    for (i, d) in defs().iter().enumerate() {
        if let Some(prefix) = d.pattern.strip_suffix('#') {
            if let Some(rest) = native.strip_prefix(prefix) {
                if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
                    if let Ok(n) = rest.parse::<u32>() {
                        return Some(Resolved {
                            order: (i, n),
                            native: native.to_string(),
                            description: d.description.replace('#', &n.to_string()),
                            def: d.clone(),
                        });
                    }
                }
            }
        } else if d.pattern == native {
            return Some(Resolved {
                order: (i, 0),
                native: native.to_string(),
                description: d.description.clone(),
                def: d.clone(),
            });
        }
    }
    None
}

/// Group descriptions for the schema's `x-run.groups`.
pub fn group_descriptions() -> Vec<(&'static str, &'static str)> {
    vec![
        ("io", "Files, formats and the output directory"),
        ("restart", "CPU-time limit and restart-file cadence"),
        ("memory", "Memory allocation"),
        ("time", "Simulated time span and spatial extent"),
        ("cosmology", "Cosmological parameters"),
        ("units", "System of units"),
        ("gravity", "Gravitational force accuracy (tree and PM)"),
        ("integration", "Time-integration accuracy"),
        ("domain", "Domain decomposition"),
        ("output", "Snapshot output times and statistics"),
        ("sph", "SPH parameters"),
        ("softening", "Gravitational softening"),
        ("subfind", "SUBFIND"),
        ("ngenic", "Initial-conditions generation (NGenIC)"),
        ("lightcone", "Lightcone output"),
        ("cooling_sf", "Cooling and star formation"),
        ("special", "Special features"),
        (
            "other",
            "Tags the adapter's table does not know (harvested, type inferred)",
        ),
    ]
}
