//! Tag input parsing.

/// Parse a tag path input, normalizing separators and whitespace.
///
/// - Trims whitespace
/// - Replaces backslashes with forward slashes
/// - Removes leading/trailing slashes
/// - Collapses multiple slashes
///
/// # Examples
/// ```
/// use impress_tags::parse_tag_path;
/// assert_eq!(parse_tag_path("  methods/sims/hydro  "), Some("methods/sims/hydro".to_string()));
/// assert_eq!(parse_tag_path("methods\\sims"), Some("methods/sims".to_string()));
/// assert_eq!(parse_tag_path(""), None);
/// ```
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn parse_tag_path(input: &str) -> Option<String> {
    let normalized = input.trim().replace('\\', "/");

    let segments: Vec<&str> = normalized
        .split('/')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    if segments.is_empty() {
        return None;
    }

    Some(segments.join("/"))
}

/// Extract the leaf (last segment) from a tag path.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn tag_leaf(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// Extract the parent path (everything before the last segment).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn tag_parent(path: &str) -> Option<String> {
    path.rfind('/').map(|i| path[..i].to_string())
}

/// Count the depth of a tag path (number of separators).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn tag_depth(path: &str) -> u32 {
    path.matches('/').count() as u32
}

/// Normalize a single tag path **segment** to a defensive slug.
///
/// Ported from the Swift `TagPathNormalizer.normalize` (deleted in Phase 1E),
/// originally a defensive layer over LLM output that ignores formatting
/// instructions. Used by enrichment (auto-tag) and AI tag migration code paths.
///
/// Transformations (applied in order):
/// 1. Trim leading/trailing ASCII + Unicode whitespace and newlines
/// 2. Lowercase (Unicode-aware)
/// 3. Replace spaces (`' '`) and underscores (`'_'`) with hyphens (`'-'`)
/// 4. Collapse runs of consecutive hyphens to a single hyphen
/// 5. Trim leading/trailing hyphens and any whitespace the hyphen trim
///    exposes, repeating until stable (normalization is idempotent)
///
/// Returns the empty string for input that reduces to nothing (e.g. `"   "`,
/// `"---"`, `""`). Callers are expected to guard against empty output before
/// composing it into a path.
///
/// # Examples
/// ```
/// use impress_tags::normalize_tag_segment;
/// assert_eq!(normalize_tag_segment("Dark Energy"), "dark-energy");
/// assert_eq!(normalize_tag_segment("  Sub_Topic  "), "sub-topic");
/// assert_eq!(normalize_tag_segment("---weird---"), "weird");
/// assert_eq!(normalize_tag_segment(""), "");
/// ```
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn normalize_tag_segment(segment: &str) -> String {
    // 1. Trim whitespace + newlines, lowercase.
    let trimmed = segment.trim_matches(|c: char| c.is_whitespace());
    let mut result: String = trimmed.to_lowercase();

    // 2. Replace spaces and underscores with hyphens.
    //    (Done after lowercase so the substitution sees the final form.)
    result = result.replace([' ', '_'], "-");

    // 3. Collapse multiple consecutive hyphens into a single hyphen.
    while result.contains("--") {
        result = result.replace("--", "-");
    }

    // 4. Trim leading/trailing hyphens and whitespace until stable. Trimming a
    //    hyphen can expose non-space whitespace that survived step 2 (e.g.
    //    "a\t-" → "a\t"), so loop the trims to reach the fixpoint and keep
    //    normalization idempotent.
    let mut trimmed = result.as_str();
    loop {
        let next = trimmed
            .trim_matches(|c: char| c.is_whitespace())
            .trim_matches('-');
        if next == trimmed {
            break;
        }
        trimmed = next;
    }
    trimmed.to_string()
}

/// Normalize a full hierarchical tag path (e.g. `"ai/Dark Energy/Sub Topic"`
/// becomes `"ai/dark-energy/sub-topic"`).
///
/// Ported from the Swift `TagPathNormalizer.normalizePath` (deleted in Phase 1E).
///
/// Splits the input on `/`, applies [`normalize_tag_segment`] to each part,
/// drops segments that reduce to the empty string, and rejoins with `/`.
/// Unlike [`parse_tag_path`] this also slugifies each segment (lowercase,
/// space-and-underscore-to-hyphen, hyphen collapse).
///
/// Returns the empty string when no segments survive normalization.
///
/// # Examples
/// ```
/// use impress_tags::normalize_tag_path;
/// assert_eq!(
///     normalize_tag_path("ai/Dark Energy/Sub Topic"),
///     "ai/dark-energy/sub-topic"
/// );
/// assert_eq!(normalize_tag_path("/Methods//Sims/"), "methods/sims");
/// assert_eq!(normalize_tag_path(""), "");
/// ```
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn normalize_tag_path(path: &str) -> String {
    path.split('/')
        .map(normalize_tag_segment)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_normal() {
        assert_eq!(
            parse_tag_path("methods/sims/hydro"),
            Some("methods/sims/hydro".to_string())
        );
    }

    #[test]
    fn parse_with_whitespace() {
        assert_eq!(
            parse_tag_path("  methods / sims  "),
            Some("methods/sims".to_string())
        );
    }

    #[test]
    fn parse_backslash() {
        assert_eq!(
            parse_tag_path("methods\\sims\\hydro"),
            Some("methods/sims/hydro".to_string())
        );
    }

    #[test]
    fn parse_leading_trailing_slash() {
        assert_eq!(
            parse_tag_path("/methods/sims/"),
            Some("methods/sims".to_string())
        );
    }

    #[test]
    fn parse_empty() {
        assert_eq!(parse_tag_path(""), None);
        assert_eq!(parse_tag_path("   "), None);
        assert_eq!(parse_tag_path("///"), None);
    }

    #[test]
    fn leaf_extraction() {
        assert_eq!(tag_leaf("methods/sims/hydro"), "hydro");
        assert_eq!(tag_leaf("methods"), "methods");
    }

    #[test]
    fn parent_extraction() {
        assert_eq!(
            tag_parent("methods/sims/hydro"),
            Some("methods/sims".to_string())
        );
        assert_eq!(tag_parent("methods"), None);
    }

    #[test]
    fn depth_count() {
        assert_eq!(tag_depth("methods"), 0);
        assert_eq!(tag_depth("methods/sims"), 1);
        assert_eq!(tag_depth("methods/sims/hydro/AMR"), 3);
    }

    // ===== normalize_tag_segment =====

    #[test]
    fn normalize_segment_empty() {
        assert_eq!(normalize_tag_segment(""), "");
        assert_eq!(normalize_tag_segment("   "), "");
        assert_eq!(normalize_tag_segment("\t\n"), "");
    }

    #[test]
    fn normalize_segment_simple_lowercase() {
        assert_eq!(normalize_tag_segment("Methods"), "methods");
        assert_eq!(normalize_tag_segment("HYDRO"), "hydro");
        assert_eq!(normalize_tag_segment("MiXeD"), "mixed");
    }

    #[test]
    fn normalize_segment_spaces_and_underscores() {
        assert_eq!(normalize_tag_segment("Dark Energy"), "dark-energy");
        assert_eq!(normalize_tag_segment("dark_energy"), "dark-energy");
        assert_eq!(
            normalize_tag_segment("Dark_Energy Models"),
            "dark-energy-models"
        );
    }

    #[test]
    fn normalize_segment_collapse_hyphens() {
        assert_eq!(normalize_tag_segment("a--b"), "a-b");
        assert_eq!(normalize_tag_segment("a---b----c"), "a-b-c");
        assert_eq!(normalize_tag_segment("a _ b"), "a-b");
    }

    #[test]
    fn normalize_segment_trim_hyphens() {
        assert_eq!(normalize_tag_segment("-leading"), "leading");
        assert_eq!(normalize_tag_segment("trailing-"), "trailing");
        assert_eq!(normalize_tag_segment("---weird---"), "weird");
        assert_eq!(normalize_tag_segment("---"), "");
    }

    #[test]
    fn normalize_segment_outer_whitespace() {
        assert_eq!(normalize_tag_segment("  Dark Energy  "), "dark-energy");
        assert_eq!(
            normalize_tag_segment("\n\tnumerical methods\t\n"),
            "numerical-methods"
        );
    }

    #[test]
    fn normalize_segment_unicode_lowercase() {
        // Unicode lowercase should still apply
        assert_eq!(normalize_tag_segment("Δark"), "δark");
        assert_eq!(normalize_tag_segment("ÉPSILON"), "épsilon");
    }

    // ===== normalize_tag_path =====

    #[test]
    fn normalize_path_empty() {
        assert_eq!(normalize_tag_path(""), "");
        assert_eq!(normalize_tag_path("   "), "");
        assert_eq!(normalize_tag_path("///"), "");
    }

    #[test]
    fn normalize_path_simple() {
        assert_eq!(normalize_tag_path("methods/hydro"), "methods/hydro");
    }

    #[test]
    fn normalize_path_slugifies_each_segment() {
        assert_eq!(
            normalize_tag_path("ai/Dark Energy/Sub Topic"),
            "ai/dark-energy/sub-topic"
        );
    }

    #[test]
    fn normalize_path_leading_trailing_slash() {
        assert_eq!(normalize_tag_path("/methods/sims/"), "methods/sims");
        assert_eq!(normalize_tag_path("/Methods/"), "methods");
    }

    #[test]
    fn normalize_path_collapse_empty_segments() {
        assert_eq!(normalize_tag_path("methods//sims"), "methods/sims");
        assert_eq!(normalize_tag_path("//a///b//c//"), "a/b/c");
    }

    #[test]
    fn normalize_path_nested_three_plus_levels() {
        assert_eq!(
            normalize_tag_path("Methods/Sims/Hydro/AMR"),
            "methods/sims/hydro/amr"
        );
        assert_eq!(normalize_tag_path("a/b/c/d/e/f"), "a/b/c/d/e/f");
    }

    #[test]
    fn normalize_path_duplicate_segments_preserved() {
        // Normalization does NOT dedup repeated segments — that's a higher-layer
        // concern. "methods/methods" is a legal (if odd) path.
        assert_eq!(normalize_tag_path("Methods/Methods"), "methods/methods");
    }

    #[test]
    fn normalize_path_segment_only_separators() {
        // A segment that reduces to empty after slugification is dropped.
        assert_eq!(normalize_tag_path("ai/---/topic"), "ai/topic");
        assert_eq!(normalize_tag_path("ai/   /topic"), "ai/topic");
    }

    #[test]
    fn normalize_path_handles_underscore_segments() {
        assert_eq!(
            normalize_tag_path("ai/topic/dark_matter"),
            "ai/topic/dark-matter"
        );
    }

    // ===== Parity with the deleted Swift TagPathNormalizer =====

    #[test]
    fn parity_swift_normalize_examples() {
        // Examples taken from Swift TagPathNormalizer doc comments.
        assert_eq!(normalize_tag_segment("Dark Energy"), "dark-energy");
        assert_eq!(
            normalize_tag_path("ai/Dark Energy/Sub Topic"),
            "ai/dark-energy/sub-topic"
        );
    }
}
