//! In-memory tag hierarchy with fast lookups.

use crate::tag::{Tag, TagColor};
use std::collections::HashMap;

/// An in-memory tag tree for fast hierarchy operations.
pub struct TagHierarchy {
    tags: HashMap<String, Tag>,
    children: HashMap<String, Vec<String>>,
}

impl TagHierarchy {
    /// Build a hierarchy from a flat list of tags.
    pub fn from_tags(tags: Vec<Tag>) -> Self {
        let mut tag_map = HashMap::new();
        let mut children: HashMap<String, Vec<String>> = HashMap::new();

        for tag in tags {
            if let Some(parent) = tag.parent_path() {
                children
                    .entry(parent.to_string())
                    .or_default()
                    .push(tag.path.clone());
            }
            tag_map.insert(tag.path.clone(), tag);
        }

        Self {
            tags: tag_map,
            children,
        }
    }

    /// Get a tag by path.
    pub fn get(&self, path: &str) -> Option<&Tag> {
        self.tags.get(path)
    }

    /// Get all root tags (depth 0) and orphan tags whose parent is missing.
    pub fn roots(&self) -> Vec<&Tag> {
        self.tags
            .values()
            .filter(|t| {
                t.depth == 0
                    || t.parent_path()
                        .map(|p| !self.tags.contains_key(p))
                        .unwrap_or(false)
            })
            .collect()
    }

    /// Get direct children of a path.
    pub fn children_of(&self, path: &str) -> Vec<&Tag> {
        self.children
            .get(path)
            .map(|paths| paths.iter().filter_map(|p| self.tags.get(p)).collect())
            .unwrap_or_default()
    }

    /// Get all descendants of a path (recursive).
    pub fn descendants_of(&self, path: &str) -> Vec<&Tag> {
        let mut result = Vec::new();
        self.collect_descendants(path, &mut result);
        result
    }

    fn collect_descendants<'a>(&'a self, path: &str, result: &mut Vec<&'a Tag>) {
        if let Some(child_paths) = self.children.get(path) {
            for child_path in child_paths {
                if let Some(tag) = self.tags.get(child_path) {
                    result.push(tag);
                    self.collect_descendants(child_path, result);
                }
            }
        }
    }

    /// Get all ancestors of a path (from root to parent).
    pub fn ancestors_of(&self, path: &str) -> Vec<&Tag> {
        let mut result = Vec::new();
        let mut current = path.to_string();

        while let Some(idx) = current.rfind('/') {
            current = current[..idx].to_string();
            if let Some(tag) = self.tags.get(&current) {
                result.push(tag);
            }
        }

        result.reverse();
        result
    }

    /// Resolve the effective color for a tag path.
    ///
    /// Walks up the hierarchy to find the nearest ancestor with a color.
    pub fn effective_color(&self, path: &str) -> Option<&TagColor> {
        // Check the tag itself first
        if let Some(tag) = self.tags.get(path) {
            if tag.color.is_some() {
                return tag.color.as_ref();
            }
        }

        // Walk up ancestors, nearest first (ancestors_of orders root → parent)
        for ancestor in self.ancestors_of(path).into_iter().rev() {
            if ancestor.color.is_some() {
                return ancestor.color.as_ref();
            }
        }

        None
    }

    /// Total number of tags in the hierarchy.
    pub fn len(&self) -> usize {
        self.tags.len()
    }

    /// Whether the hierarchy is empty.
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }

    /// Format as a tree string for display.
    pub fn format_tree(&self) -> String {
        let mut output = String::new();
        let mut roots: Vec<&Tag> = self.roots();
        roots.sort_by(|a, b| a.path.cmp(&b.path));

        for root in roots {
            self.format_subtree(root, "", true, &mut output);
        }
        output
    }

    fn format_subtree(&self, tag: &Tag, prefix: &str, is_last: bool, output: &mut String) {
        let connector = if prefix.is_empty() {
            ""
        } else if is_last {
            "└── "
        } else {
            "├── "
        };

        output.push_str(&format!("{}{}{}\n", prefix, connector, tag.leaf));

        let child_prefix = if prefix.is_empty() {
            "".to_string()
        } else if is_last {
            format!("{}    ", prefix)
        } else {
            format!("{}│   ", prefix)
        };

        let mut children = self.children_of(&tag.path);
        children.sort_by(|a, b| a.path.cmp(&b.path));

        for (i, child) in children.iter().enumerate() {
            self.format_subtree(child, &child_prefix, i == children.len() - 1, output);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_hierarchy() -> TagHierarchy {
        let tags = vec![
            Tag::new("methods"),
            Tag::new("methods/sims"),
            Tag::new("methods/sims/hydro"),
            Tag::new("methods/sims/nbody"),
            Tag::new("methods/obs"),
            Tag::new("topics"),
            Tag::new("topics/galaxies"),
        ];
        TagHierarchy::from_tags(tags)
    }

    #[test]
    fn roots() {
        let h = sample_hierarchy();
        let mut roots: Vec<&str> = h.roots().iter().map(|t| t.path.as_str()).collect();
        roots.sort();
        assert_eq!(roots, vec!["methods", "topics"]);
    }

    #[test]
    fn children() {
        let h = sample_hierarchy();
        let mut children: Vec<&str> = h
            .children_of("methods/sims")
            .iter()
            .map(|t| t.path.as_str())
            .collect();
        children.sort();
        assert_eq!(children, vec!["methods/sims/hydro", "methods/sims/nbody"]);
    }

    #[test]
    fn descendants() {
        let h = sample_hierarchy();
        let mut descs: Vec<&str> = h
            .descendants_of("methods")
            .iter()
            .map(|t| t.path.as_str())
            .collect();
        descs.sort();
        assert_eq!(
            descs,
            vec![
                "methods/obs",
                "methods/sims",
                "methods/sims/hydro",
                "methods/sims/nbody"
            ]
        );
    }

    #[test]
    fn ancestors() {
        let h = sample_hierarchy();
        let ancs: Vec<&str> = h
            .ancestors_of("methods/sims/hydro")
            .iter()
            .map(|t| t.path.as_str())
            .collect();
        assert_eq!(ancs, vec!["methods", "methods/sims"]);
    }

    #[test]
    fn format_tree() {
        let h = sample_hierarchy();
        let tree = h.format_tree();
        assert!(tree.contains("methods"));
        assert!(tree.contains("hydro"));
    }
}
