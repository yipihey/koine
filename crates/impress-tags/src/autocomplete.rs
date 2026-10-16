//! Tag autocomplete engine with ranked suggestions.
//!
//! Ranking: recency > shallow depth > frequency > alphabetical.

use crate::tag::Tag;
use chrono::{DateTime, Utc};

/// An autocomplete suggestion.
#[derive(Debug, Clone)]
pub struct Suggestion {
    pub path: String,
    pub leaf: String,
    pub depth: u32,
    pub use_count: u32,
    pub last_used_at: Option<DateTime<Utc>>,
    pub score: f64,
}

/// Autocomplete engine operating on an in-memory tag list.
pub struct AutocompleteEngine {
    tags: Vec<Tag>,
}

impl AutocompleteEngine {
    /// Create a new engine from a tag list.
    pub fn new(tags: Vec<Tag>) -> Self {
        Self { tags }
    }

    /// Update the tag list.
    pub fn update(&mut self, tags: Vec<Tag>) {
        self.tags = tags;
    }

    /// Find completions matching a prefix.
    pub fn complete(&self, prefix: &str, limit: usize) -> Vec<Suggestion> {
        let prefix_lower = prefix.trim().to_lowercase();
        if prefix_lower.is_empty() {
            return Vec::new();
        }
        let now = Utc::now();

        let mut suggestions: Vec<Suggestion> = self
            .tags
            .iter()
            .filter(|t| {
                t.path.to_lowercase().starts_with(&prefix_lower)
                    || t.leaf.to_lowercase().starts_with(&prefix_lower)
            })
            .map(|t| {
                let score = compute_score(t, &now);
                Suggestion {
                    path: t.path.clone(),
                    leaf: t.leaf.clone(),
                    depth: t.depth,
                    use_count: t.use_count,
                    last_used_at: t.last_used_at,
                    score,
                }
            })
            .collect();

        // Sort by score descending, then alphabetically
        suggestions.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.path.cmp(&b.path))
        });

        suggestions.truncate(limit);
        suggestions
    }
}

/// Compute ranking score for a tag.
///
/// Factors (weighted):
/// - Recency: +50 if used in last 7 days
/// - Depth: shallower = better (max 20 points)
/// - Frequency: logarithmic scaling (max 30 points)
fn compute_score(tag: &Tag, now: &DateTime<Utc>) -> f64 {
    let mut score = 0.0;

    // Recency bonus (50 points for used in last 7 days)
    if let Some(last_used) = tag.last_used_at {
        let days_ago = (*now - last_used).num_days();
        if days_ago < 7 {
            score += 50.0;
        } else if days_ago < 30 {
            score += 25.0;
        }
    }

    // Depth penalty (shallower = better, max 20 points)
    score += (20.0 - (tag.depth as f64 * 5.0)).max(0.0);

    // Frequency (logarithmic, max 30 points)
    if tag.use_count > 0 {
        score += (tag.use_count as f64).ln().min(3.4) * (30.0 / 3.4);
    }

    score
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tags() -> Vec<Tag> {
        vec![
            Tag {
                use_count: 10,
                last_used_at: Some(Utc::now()),
                ..Tag::new("methods")
            },
            Tag {
                use_count: 5,
                ..Tag::new("methods/sims")
            },
            Tag {
                use_count: 3,
                ..Tag::new("methods/sims/hydro")
            },
            Tag {
                use_count: 1,
                ..Tag::new("topics/galaxies")
            },
        ]
    }

    #[test]
    fn prefix_match() {
        let engine = AutocompleteEngine::new(sample_tags());
        let results = engine.complete("meth", 10);
        assert_eq!(results.len(), 3);
        // Root "methods" should rank highest (shallower + more frequent + recent)
        assert_eq!(results[0].path, "methods");
    }

    #[test]
    fn leaf_match() {
        let engine = AutocompleteEngine::new(sample_tags());
        let results = engine.complete("hydro", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, "methods/sims/hydro");
    }

    #[test]
    fn limit_results() {
        let engine = AutocompleteEngine::new(sample_tags());
        let results = engine.complete("m", 2);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn empty_prefix() {
        let engine = AutocompleteEngine::new(sample_tags());
        let results = engine.complete("", 10);
        assert!(results.is_empty()); // Empty prefix returns no results
    }
}
