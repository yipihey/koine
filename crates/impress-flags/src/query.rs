//! Flag query types for filtering publications by flag state.

use crate::FlagColor;
use serde::{Deserialize, Serialize};

/// A query for filtering publications by flag state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum FlagQuery {
    /// Has any flag
    AnyFlag,
    /// Has no flag
    NoFlag,
    /// Has a specific flag color
    HasColor(FlagColor),
    /// Does not have a specific flag color
    NotColor(FlagColor),
}

/// Parse a flag query from a filter string.
///
/// Syntax:
/// - `flag:*` or `flag:any` — any flag
/// - `-flag:*` or `flag:none` — no flag
/// - `flag:red` — specific color
/// - `-flag:red` — not specific color
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn parse_flag_query(input: &str) -> Option<FlagQuery> {
    let input = input.trim().to_lowercase();

    // Negated form
    if let Some(rest) = input.strip_prefix("-flag:") {
        if rest == "*" || rest == "any" {
            return Some(FlagQuery::NoFlag);
        }
        return parse_color_name(rest).map(FlagQuery::NotColor);
    }

    // Positive form
    if let Some(rest) = input.strip_prefix("flag:") {
        if rest == "*" || rest == "any" {
            return Some(FlagQuery::AnyFlag);
        }
        if rest == "none" {
            return Some(FlagQuery::NoFlag);
        }
        return parse_color_name(rest).map(FlagQuery::HasColor);
    }

    None
}

fn parse_color_name(name: &str) -> Option<FlagColor> {
    match name {
        "red" | "r" => Some(FlagColor::Red),
        "amber" | "a" | "yellow" | "y" => Some(FlagColor::Amber),
        "blue" | "b" => Some(FlagColor::Blue),
        "gray" | "g" | "grey" => Some(FlagColor::Gray),
        _ => None,
    }
}

impl FlagQuery {
    /// Test whether a flag matches this query.
    pub fn matches(&self, flag: Option<&FlagColor>) -> bool {
        match self {
            FlagQuery::AnyFlag => flag.is_some(),
            FlagQuery::NoFlag => flag.is_none(),
            FlagQuery::HasColor(c) => flag == Some(c),
            FlagQuery::NotColor(c) => flag != Some(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_any_flag() {
        assert_eq!(parse_flag_query("flag:*"), Some(FlagQuery::AnyFlag));
        assert_eq!(parse_flag_query("flag:any"), Some(FlagQuery::AnyFlag));
    }

    #[test]
    fn parse_no_flag() {
        assert_eq!(parse_flag_query("flag:none"), Some(FlagQuery::NoFlag));
        assert_eq!(parse_flag_query("-flag:*"), Some(FlagQuery::NoFlag));
    }

    #[test]
    fn parse_specific_color() {
        assert_eq!(
            parse_flag_query("flag:red"),
            Some(FlagQuery::HasColor(FlagColor::Red))
        );
        assert_eq!(
            parse_flag_query("flag:amber"),
            Some(FlagQuery::HasColor(FlagColor::Amber))
        );
    }

    #[test]
    fn parse_negated_color() {
        assert_eq!(
            parse_flag_query("-flag:blue"),
            Some(FlagQuery::NotColor(FlagColor::Blue))
        );
    }

    #[test]
    fn query_matches() {
        let red = FlagColor::Red;
        let blue = FlagColor::Blue;

        assert!(FlagQuery::AnyFlag.matches(Some(&red)));
        assert!(!FlagQuery::AnyFlag.matches(None));
        assert!(FlagQuery::NoFlag.matches(None));
        assert!(!FlagQuery::NoFlag.matches(Some(&red)));
        assert!(FlagQuery::HasColor(FlagColor::Red).matches(Some(&red)));
        assert!(!FlagQuery::HasColor(FlagColor::Red).matches(Some(&blue)));
        assert!(FlagQuery::NotColor(FlagColor::Red).matches(Some(&blue)));
        assert!(FlagQuery::NotColor(FlagColor::Red).matches(None));
    }
}
