//! Core flag types.

use serde::{Deserialize, Serialize};

/// Flag color representing workflow priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum FlagColor {
    Red,
    Amber,
    Blue,
    Gray,
}

impl FlagColor {
    /// Parse from a single character shorthand.
    pub fn from_char(c: char) -> Option<Self> {
        match c.to_ascii_lowercase() {
            'r' => Some(Self::Red),
            'a' => Some(Self::Amber),
            'b' => Some(Self::Blue),
            'g' => Some(Self::Gray),
            _ => None,
        }
    }

    /// Display name for UI.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Red => "Red",
            Self::Amber => "Amber",
            Self::Blue => "Blue",
            Self::Gray => "Gray",
        }
    }

    /// Shorthand character.
    pub fn shorthand(&self) -> char {
        match self {
            Self::Red => 'r',
            Self::Amber => 'a',
            Self::Blue => 'b',
            Self::Gray => 'g',
        }
    }
}

/// Flag stripe style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum FlagStyle {
    #[default]
    Solid,
    Dashed,
    Dotted,
}

impl FlagStyle {
    /// Parse from shorthand character.
    pub fn from_char(c: char) -> Option<Self> {
        match c {
            's' | 'S' => Some(Self::Solid),
            '-' => Some(Self::Dashed),
            '.' => Some(Self::Dotted),
            _ => None,
        }
    }
}

/// Flag stripe length as fraction of row height.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum FlagLength {
    #[default]
    Full,
    Half,
    Quarter,
}

impl FlagLength {
    /// Parse from shorthand character.
    pub fn from_char(c: char) -> Option<Self> {
        match c.to_ascii_lowercase() {
            'f' => Some(Self::Full),
            'h' => Some(Self::Half),
            'q' => Some(Self::Quarter),
            _ => None,
        }
    }

    /// Fraction of row height (0.0 to 1.0).
    pub fn fraction(&self) -> f64 {
        match self {
            Self::Full => 1.0,
            Self::Half => 0.5,
            Self::Quarter => 0.25,
        }
    }
}

/// Complete flag specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Flag {
    pub color: FlagColor,
    pub style: FlagStyle,
    pub length: FlagLength,
}

impl Flag {
    /// Create a simple flag with default style and length.
    pub fn simple(color: FlagColor) -> Self {
        Self {
            color,
            style: FlagStyle::Solid,
            length: FlagLength::Full,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_color_roundtrip() {
        for color in [
            FlagColor::Red,
            FlagColor::Amber,
            FlagColor::Blue,
            FlagColor::Gray,
        ] {
            let c = color.shorthand();
            assert_eq!(FlagColor::from_char(c), Some(color));
        }
    }

    #[test]
    fn flag_length_fractions() {
        assert!((FlagLength::Full.fraction() - 1.0).abs() < f64::EPSILON);
        assert!((FlagLength::Half.fraction() - 0.5).abs() < f64::EPSILON);
        assert!((FlagLength::Quarter.fraction() - 0.25).abs() < f64::EPSILON);
    }
}
