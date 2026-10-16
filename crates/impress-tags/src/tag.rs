//! Core tag types.

use serde::{Deserialize, Serialize};

/// A unique tag identifier.
pub type TagId = uuid::Uuid;

/// Tag color (light and dark mode hex).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct TagColor {
    pub light: String,
    pub dark: String,
}

/// A reference to a tag (lightweight, for query results).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct TagRef {
    pub id: String,
    pub path: String,
    pub leaf: String,
    pub depth: u32,
}

impl TagRef {
    /// Create from a canonical path.
    pub fn from_path(id: &str, path: &str) -> Self {
        let segments: Vec<&str> = path.split('/').collect();
        Self {
            id: id.to_string(),
            path: path.to_string(),
            leaf: segments.last().unwrap_or(&path).to_string(),
            depth: segments.len().saturating_sub(1) as u32,
        }
    }
}

/// Full tag data with metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub id: TagId,
    pub path: String,
    pub leaf: String,
    pub depth: u32,
    pub color: Option<TagColor>,
    pub parent_id: Option<TagId>,
    pub use_count: u32,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    pub sort_order: i32,
}

impl Tag {
    /// Create a new tag from a path.
    pub fn new(path: &str) -> Self {
        let segments: Vec<&str> = path.split('/').collect();
        Self {
            id: TagId::new_v4(),
            path: path.to_string(),
            leaf: segments.last().unwrap_or(&path).to_string(),
            depth: segments.len().saturating_sub(1) as u32,
            color: None,
            parent_id: None,
            use_count: 0,
            last_used_at: None,
            sort_order: 0,
        }
    }

    /// Path segments (e.g., ["methods", "sims", "hydro"]).
    pub fn segments(&self) -> Vec<&str> {
        self.path.split('/').collect()
    }

    /// Parent path (e.g., "methods/sims" for "methods/sims/hydro").
    pub fn parent_path(&self) -> Option<&str> {
        self.path.rfind('/').map(|i| &self.path[..i])
    }

    /// Check if this tag is an ancestor of another.
    pub fn is_ancestor_of(&self, other: &str) -> bool {
        other.starts_with(&self.path)
            && other.len() > self.path.len()
            && other.as_bytes()[self.path.len()] == b'/'
    }

    /// Check if this tag is a descendant of another.
    pub fn is_descendant_of(&self, ancestor: &str) -> bool {
        self.path.starts_with(ancestor)
            && self.path.len() > ancestor.len()
            && self.path.as_bytes()[ancestor.len()] == b'/'
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_segments() {
        let tag = Tag::new("methods/sims/hydro");
        assert_eq!(tag.segments(), vec!["methods", "sims", "hydro"]);
        assert_eq!(tag.leaf, "hydro");
        assert_eq!(tag.depth, 2);
    }

    #[test]
    fn tag_parent_path() {
        let tag = Tag::new("methods/sims/hydro");
        assert_eq!(tag.parent_path(), Some("methods/sims"));

        let root = Tag::new("methods");
        assert_eq!(root.parent_path(), None);
    }

    #[test]
    fn tag_ancestry() {
        let parent = Tag::new("methods");
        assert!(parent.is_ancestor_of("methods/sims"));
        assert!(parent.is_ancestor_of("methods/sims/hydro"));
        assert!(!parent.is_ancestor_of("methods"));
        assert!(!parent.is_ancestor_of("methodology"));
    }

    #[test]
    fn tag_descendant() {
        let child = Tag::new("methods/sims/hydro");
        assert!(child.is_descendant_of("methods"));
        assert!(child.is_descendant_of("methods/sims"));
        assert!(!child.is_descendant_of("methods/sims/hydro"));
        assert!(!child.is_descendant_of("method"));
    }
}
