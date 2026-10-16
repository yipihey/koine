//! Tag aliases (shortcut → full path).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A mapping from shortcut names to full tag paths.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TagAliases {
    aliases: HashMap<String, String>,
}

impl TagAliases {
    /// Create an empty alias map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an alias.
    pub fn add(&mut self, shortcut: &str, full_path: &str) {
        self.aliases
            .insert(shortcut.to_lowercase(), full_path.to_string());
    }

    /// Remove an alias.
    pub fn remove(&mut self, shortcut: &str) -> Option<String> {
        self.aliases.remove(&shortcut.to_lowercase())
    }

    /// Resolve a shortcut to its full path.
    pub fn resolve(&self, input: &str) -> Option<&str> {
        self.aliases.get(&input.to_lowercase()).map(|s| s.as_str())
    }

    /// Resolve an input, returning the original if no alias matches.
    pub fn resolve_or_self<'a>(&'a self, input: &'a str) -> &'a str {
        self.resolve(input).unwrap_or(input)
    }

    /// List all aliases.
    pub fn all(&self) -> &HashMap<String, String> {
        &self.aliases
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_roundtrip() {
        let mut aliases = TagAliases::new();
        aliases.add("hydro", "methods/sims/hydro");
        assert_eq!(aliases.resolve("hydro"), Some("methods/sims/hydro"));
        assert_eq!(aliases.resolve("HYDRO"), Some("methods/sims/hydro"));
    }

    #[test]
    fn resolve_or_self() {
        let mut aliases = TagAliases::new();
        aliases.add("hydro", "methods/sims/hydro");
        assert_eq!(aliases.resolve_or_self("hydro"), "methods/sims/hydro");
        assert_eq!(aliases.resolve_or_self("unknown"), "unknown");
    }

    #[test]
    fn remove_alias() {
        let mut aliases = TagAliases::new();
        aliases.add("hydro", "methods/sims/hydro");
        assert!(aliases.remove("hydro").is_some());
        assert_eq!(aliases.resolve("hydro"), None);
    }
}
