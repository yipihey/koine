//! A tiny library: the surface the coverage test expects.
//! pub fn in_a_comment() {}  (must not be listed)

pub struct Counter {
    n: u32,
}

impl Counter {
    /// Matched to a verb by name (`bump`).
    pub fn bump(&mut self) -> u32 {
        self.n += 1;
        self.n
    }

    /// Matched to a verb by a `# covers:` line.
    pub fn read_total(&self) -> u32 {
        self.n
    }

    pub(crate) fn internal_only(&self) -> u32 {
        self.n
    }
}

/// Excluded in verb-exclusions.toml with a reason.
pub fn describe() -> &'static str {
    "pub fn in_a_string() {}"
}

pub(crate) fn crate_private() {}

#[doc(hidden)]
pub fn hidden_helper() {}

#[cfg(test)]
mod tests {
    pub fn test_only() {}
}
