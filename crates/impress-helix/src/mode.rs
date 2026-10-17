//! Helix editing modes.

use std::fmt;

/// The editing mode for Helix-style modal editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HelixMode {
    /// Normal mode - navigation and commands (default).
    #[default]
    Normal,
    /// Insert mode - text typing.
    Insert,
    /// Select mode - selection extension.
    Select,
}

impl HelixMode {
    /// Returns the display name for this mode.
    pub fn display_name(&self) -> &'static str {
        match self {
            HelixMode::Normal => "NORMAL",
            HelixMode::Insert => "INSERT",
            HelixMode::Select => "SELECT",
        }
    }

    /// Returns a short code for this mode (for compact display).
    pub fn short_code(&self) -> &'static str {
        match self {
            HelixMode::Normal => "NOR",
            HelixMode::Insert => "INS",
            HelixMode::Select => "SEL",
        }
    }
}

impl fmt::Display for HelixMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_mode() {
        assert_eq!(HelixMode::default(), HelixMode::Normal);
    }

    #[test]
    fn test_display_names() {
        assert_eq!(HelixMode::Normal.display_name(), "NORMAL");
        assert_eq!(HelixMode::Insert.display_name(), "INSERT");
        assert_eq!(HelixMode::Select.display_name(), "SELECT");
    }

    #[test]
    fn test_short_codes() {
        assert_eq!(HelixMode::Normal.short_code(), "NOR");
        assert_eq!(HelixMode::Insert.short_code(), "INS");
        assert_eq!(HelixMode::Select.short_code(), "SEL");
    }
}
