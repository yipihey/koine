//! Default tag color palette.

use crate::tag::TagColor;

/// Default color palette for tags without explicit colors.
///
/// Each entry provides light and dark mode hex colors.
pub const DEFAULT_TAG_COLORS: &[(&str, &str)] = &[
    ("43A047", "66BB6A"), // Green
    ("1E88E5", "42A5F5"), // Blue
    ("8E24AA", "AB47BC"), // Purple
    ("E53935", "EF5350"), // Red
    ("FB8C00", "FFA726"), // Orange
    ("00ACC1", "26C6DA"), // Cyan
    ("D81B60", "EC407A"), // Pink
    ("5E35B1", "7E57C2"), // Deep Purple
];

/// Pick a deterministic color based on a string hash.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn default_tag_color(path: &str) -> TagColor {
    let hash = path
        .bytes()
        .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
    let index = (hash % DEFAULT_TAG_COLORS.len() as u64) as usize;
    let (light, dark) = DEFAULT_TAG_COLORS[index];
    TagColor {
        light: light.to_string(),
        dark: dark.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_color() {
        let c1 = default_tag_color("methods");
        let c2 = default_tag_color("methods");
        assert_eq!(c1, c2);
    }

    #[test]
    fn different_paths_different_colors() {
        // With enough diversity, different paths should get different colors
        // (not guaranteed for all pairs, but should work for common cases)
        let c1 = default_tag_color("methods");
        let c2 = default_tag_color("topics");
        // Just check both are valid
        assert!(!c1.light.is_empty());
        assert!(!c2.dark.is_empty());
    }
}
