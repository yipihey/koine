//! Flag color configuration (light/dark hex defaults, semantics).

use crate::FlagColor;
use serde::{Deserialize, Serialize};

/// Color configuration for a flag (light and dark mode hex values).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct FlagColorConfig {
    pub light: String,
    pub dark: String,
}

/// User-defined semantic labels for flag colors.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct FlagSemantics {
    pub red_label: String,
    pub amber_label: String,
    pub blue_label: String,
    pub gray_label: String,
}

impl Default for FlagSemantics {
    fn default() -> Self {
        Self {
            red_label: "Urgent".to_string(),
            amber_label: "Important".to_string(),
            blue_label: "To Read".to_string(),
            gray_label: "Low Priority".to_string(),
        }
    }
}

/// Get the default color hex values for a flag color.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn default_flag_color(color: FlagColor) -> FlagColorConfig {
    match color {
        FlagColor::Red => FlagColorConfig {
            light: "D32F2F".to_string(),
            dark: "EF5350".to_string(),
        },
        FlagColor::Amber => FlagColorConfig {
            light: "F57F17".to_string(),
            dark: "FFB300".to_string(),
        },
        FlagColor::Blue => FlagColorConfig {
            light: "1565C0".to_string(),
            dark: "42A5F5".to_string(),
        },
        FlagColor::Gray => FlagColorConfig {
            light: "616161".to_string(),
            dark: "9E9E9E".to_string(),
        },
    }
}
