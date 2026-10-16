//! Flag command parsing.
//!
//! Parses compact flag shorthand into structured Flag values.

use crate::{Flag, FlagColor, FlagLength, FlagStyle};

/// Parse a flag command string into a Flag.
///
/// Grammar: `<color>[<style>][<length>]`
/// - color: r/a/b/g (required)
/// - style: s/-/. (optional, default: solid)
/// - length: f/h/q (optional, default: full)
///
/// Case insensitive.
///
/// # Examples
/// ```
/// use impress_flags::parse_flag_command;
/// assert!(parse_flag_command("r").is_some());
/// assert!(parse_flag_command("a-h").is_some());
/// assert!(parse_flag_command("xyz").is_none());
/// ```
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn parse_flag_command(input: &str) -> Option<Flag> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }

    let chars: Vec<char> = input.chars().collect();

    // First character: color (required)
    let color = FlagColor::from_char(chars[0])?;

    // Remaining characters: style and length (optional, order flexible)
    let mut style = FlagStyle::Solid;
    let mut length = FlagLength::Full;
    let mut style_set = false;
    let mut length_set = false;

    for &c in &chars[1..] {
        if !style_set {
            if let Some(s) = FlagStyle::from_char(c) {
                style = s;
                style_set = true;
                continue;
            }
        }
        if !length_set {
            if let Some(l) = FlagLength::from_char(c) {
                length = l;
                length_set = true;
                continue;
            }
        }
        // Unknown character - ignore
    }

    Some(Flag {
        color,
        style,
        length,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_color() {
        let flag = parse_flag_command("r").unwrap();
        assert_eq!(flag.color, FlagColor::Red);
        assert_eq!(flag.style, FlagStyle::Solid);
        assert_eq!(flag.length, FlagLength::Full);
    }

    #[test]
    fn parse_color_with_style() {
        let flag = parse_flag_command("a-").unwrap();
        assert_eq!(flag.color, FlagColor::Amber);
        assert_eq!(flag.style, FlagStyle::Dashed);
    }

    #[test]
    fn parse_full_shorthand() {
        let flag = parse_flag_command("b.q").unwrap();
        assert_eq!(flag.color, FlagColor::Blue);
        assert_eq!(flag.style, FlagStyle::Dotted);
        assert_eq!(flag.length, FlagLength::Quarter);
    }

    #[test]
    fn parse_case_insensitive() {
        let flag = parse_flag_command("R").unwrap();
        assert_eq!(flag.color, FlagColor::Red);
    }

    #[test]
    fn parse_invalid() {
        assert!(parse_flag_command("").is_none());
        assert!(parse_flag_command("x").is_none());
        assert!(parse_flag_command("123").is_none());
    }

    #[test]
    fn parse_with_whitespace() {
        let flag = parse_flag_command("  g  ").unwrap();
        assert_eq!(flag.color, FlagColor::Gray);
    }

    #[test]
    fn parse_gray_solid_quarter() {
        let flag = parse_flag_command("gsq").unwrap();
        assert_eq!(flag.color, FlagColor::Gray);
        assert_eq!(flag.style, FlagStyle::Solid);
        assert_eq!(flag.length, FlagLength::Quarter);
    }
}
