//! Problems with the input, in the convention's `2xx` band (R-D4.5): an unknown
//! name or a type mismatch is an error before launch, never a warning (R-D2.5).

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub code: u16,
    pub label: &'static str,
    pub message: String,
}

pub const UNKNOWN_PARAMETER: (u16, &str) = (201, "unknown-parameter");
pub const TYPE_MISMATCH: (u16, &str) = (202, "type-mismatch");
pub const MISSING_PARAMETER: (u16, &str) = (203, "missing-parameter");
pub const BAD_VALUE: (u16, &str) = (204, "bad-value");
pub const BAD_OUTPUT_DIR: (u16, &str) = (205, "output-dir-outside-run");
pub const BAD_SCHEMA: (u16, &str) = (206, "bad-schema");
/// A resume asks for a different value of a `never` parameter (R-D2.3).
pub const NEVER_CHANGED: (u16, &str) = (207, "never-changed");
/// `--from` is not something GADGET-4 can restart from, or its run is unknown.
pub const BAD_RESTART_SOURCE: (u16, &str) = (208, "bad-restart-source");

impl Problem {
    pub fn new(kind: (u16, &'static str), message: impl Into<String>) -> Self {
        Problem {
            code: kind.0,
            label: kind.1,
            message: message.into(),
        }
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} [{}]: {}", self.code, self.label, self.message)
    }
}

/// What the adapter's operations return.
#[derive(Debug)]
pub enum Error {
    /// Input refused before anything was launched or written.
    Invalid(Vec<Problem>),
    /// Anything else (files, processes).
    Other(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Invalid(ps) => {
                for (i, p) in ps.iter().enumerate() {
                    if i > 0 {
                        writeln!(f)?;
                    }
                    write!(f, "{p}")?;
                }
                Ok(())
            }
            Error::Other(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    pub fn other(m: impl Into<String>) -> Self {
        Error::Other(m.into())
    }
}

pub fn io<T>(what: impl fmt::Display, r: std::io::Result<T>) -> Result<T, Error> {
    r.map_err(|e| Error::Other(format!("{what}: {e}")))
}
