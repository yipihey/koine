//! Property-based tests for tag path parsing/normalization, tag ancestry,
//! the in-memory hierarchy, and the tag query mini-language.
//!
//! Invariants encoded:
//!   - totality (no panics on arbitrary input)
//!   - idempotence of `parse_tag_path`, `normalize_tag_segment`, `normalize_tag_path`
//!   - structural output invariants (no empty segments, no stray separators)
//!   - leaf/parent/depth consistency
//!   - ancestor/descendant duality and boundary behavior (`a` vs `ab` vs `a/b`)
//!   - `TagHierarchy` consistency against brute-force reference computations,
//!     including input-order independence
//!   - `parse_tag_query` round-trip + `TagQuery::matches` boolean semantics

use impress_tags::{
    normalize_tag_path, normalize_tag_segment, parse_tag_path, parse_tag_query, tag_depth,
    tag_leaf, tag_parent, Tag, TagColor, TagHierarchy, TagQuery,
};
use proptest::prelude::*;
use std::collections::{BTreeSet, HashMap};

// ===========================================================================
// Generators
// ===========================================================================

/// A well-formed path segment (already in normalized shape).
fn safe_segment() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9]{0,5}".prop_map(|s| s)
}

/// A well-formed hierarchical path, e.g. "methods/sims/hydro".
fn safe_path() -> impl Strategy<Value = String> {
    prop::collection::vec(safe_segment(), 1..=4).prop_map(|segs| segs.join("/"))
}

/// Paths over a tiny segment vocabulary so random sets share prefixes.
fn overlapping_path() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(&["a", "b", "c"][..]), 1..=4)
        .prop_map(|segs| segs.join("/"))
}

/// A set of paths closed under taking parents (every ancestor present).
fn closed_path_set() -> impl Strategy<Value = BTreeSet<String>> {
    prop::collection::vec(overlapping_path(), 1..8).prop_map(|paths| {
        let mut set = BTreeSet::new();
        for p in paths {
            let segs: Vec<&str> = p.split('/').collect();
            for i in 1..=segs.len() {
                set.insert(segs[..i].join("/"));
            }
        }
        set
    })
}

fn arb_color() -> impl Strategy<Value = TagColor> {
    prop::sample::select(&["#f00", "#0f0", "#00f"][..]).prop_map(|c| TagColor {
        light: c.to_string(),
        dark: c.to_string(),
    })
}

// ===========================================================================
// parse_tag_path
// ===========================================================================

proptest! {
    /// Totality: never panics, and `None` only for input with no segments.
    #[test]
    fn parse_tag_path_is_total(input in ".*") {
        let _ = parse_tag_path(&input);
    }

    /// Output invariants: no empty segments, no backslashes, no edge slashes,
    /// segments carry no leading/trailing whitespace.
    #[test]
    fn parse_tag_path_output_well_formed(input in ".*") {
        if let Some(p) = parse_tag_path(&input) {
            prop_assert!(!p.is_empty());
            prop_assert!(!p.starts_with('/'), "leading slash in {:?}", p);
            prop_assert!(!p.ends_with('/'), "trailing slash in {:?}", p);
            prop_assert!(!p.contains("//"), "empty segment in {:?}", p);
            prop_assert!(!p.contains('\\'), "backslash survived in {:?}", p);
            for seg in p.split('/') {
                prop_assert_eq!(seg, seg.trim(), "untrimmed segment in {:?}", &p);
                prop_assert!(!seg.is_empty());
            }
        }
    }

    /// Idempotence: re-parsing a parsed path is the identity.
    #[test]
    fn parse_tag_path_idempotent(input in ".*") {
        if let Some(p) = parse_tag_path(&input) {
            prop_assert_eq!(parse_tag_path(&p), Some(p));
        }
    }
}

// ===========================================================================
// normalize_tag_segment / normalize_tag_path
// ===========================================================================

proptest! {
    /// Totality + output invariants for segment slugification.
    #[test]
    fn normalize_segment_total_and_well_formed(input in ".*") {
        let s = normalize_tag_segment(&input);
        prop_assert!(!s.contains(' '), "space survived in {:?}", s);
        prop_assert!(!s.contains('_'), "underscore survived in {:?}", s);
        prop_assert!(!s.contains("--"), "hyphen run survived in {:?}", s);
        prop_assert!(!s.starts_with('-'), "leading hyphen in {:?}", s);
        prop_assert!(!s.ends_with('-'), "trailing hyphen in {:?}", s);
        // Lowercasing was applied (lowercase is a fixpoint).
        prop_assert_eq!(s.clone(), s.to_lowercase());
    }

    /// Idempotence of segment normalization.
    ///
    /// Regression (fixed 2026-07-20): hyphen trimming used to expose non-space
    /// whitespace that only a second pass would trim (`"a\t-"` → `"a\t"` →
    /// `"a"`); the trim now loops to a fixpoint.
    #[test]
    fn normalize_segment_idempotent(input in ".*") {
        let once = normalize_tag_segment(&input);
        let twice = normalize_tag_segment(&once);
        prop_assert_eq!(twice, once);
    }

    /// Idempotence restricted to printable ASCII (narrower companion to the
    /// full-Unicode property above).
    #[test]
    fn normalize_segment_idempotent_ascii_printable(input in "[ -~]*") {
        let once = normalize_tag_segment(&input);
        let twice = normalize_tag_segment(&once);
        prop_assert_eq!(twice, once);
    }

    /// Totality + structural invariants for path normalization: no empty
    /// segments survive.
    #[test]
    fn normalize_path_total_and_well_formed(input in ".*") {
        let p = normalize_tag_path(&input);
        if !p.is_empty() {
            prop_assert!(!p.starts_with('/'));
            prop_assert!(!p.ends_with('/'));
            prop_assert!(!p.contains("//"));
            for seg in p.split('/') {
                prop_assert!(!seg.is_empty(), "empty segment in {:?}", &p);
            }
        }
    }

    /// Idempotence of path normalization.
    #[test]
    fn normalize_path_idempotent(input in ".*") {
        let once = normalize_tag_path(&input);
        let twice = normalize_tag_path(&once);
        prop_assert_eq!(twice, once);
    }

    /// Path idempotence restricted to printable ASCII (holds today).
    #[test]
    fn normalize_path_idempotent_ascii_printable(input in "[ -~]*") {
        let once = normalize_tag_path(&input);
        let twice = normalize_tag_path(&once);
        prop_assert_eq!(twice, once);
    }

    /// Path normalization commutes with per-segment normalization: normalizing
    /// the whole path equals normalizing each `/`-segment and dropping empties.
    #[test]
    fn normalize_path_is_segmentwise(input in ".*") {
        let whole = normalize_tag_path(&input);
        let piecewise: Vec<String> = input
            .split('/')
            .map(normalize_tag_segment)
            .filter(|s| !s.is_empty())
            .collect();
        prop_assert_eq!(whole, piecewise.join("/"));
    }
}

// ===========================================================================
// leaf / parent / depth consistency
// ===========================================================================

proptest! {
    /// parent + "/" + leaf reconstructs the path; depth decreases by one per
    /// parent step; walking parents terminates at a depth-0 root.
    #[test]
    fn leaf_parent_depth_consistent(p in safe_path()) {
        let depth = tag_depth(&p);
        prop_assert_eq!(depth as usize, p.split('/').count() - 1);

        match tag_parent(&p) {
            None => prop_assert_eq!(depth, 0),
            Some(parent) => {
                prop_assert_eq!(format!("{}/{}", parent, tag_leaf(&p)), p.clone());
                prop_assert_eq!(tag_depth(&parent), depth - 1);
            }
        }

        // Walking parents reaches a root in exactly `depth` steps.
        let mut current = p.clone();
        let mut steps = 0u32;
        while let Some(parent) = tag_parent(&current) {
            current = parent;
            steps += 1;
            prop_assert!(steps <= depth, "parent chain longer than depth for {:?}", &p);
        }
        prop_assert_eq!(steps, depth);
        prop_assert_eq!(tag_depth(&current), 0);
    }

    /// The leaf never contains a separator and is the last segment.
    #[test]
    fn leaf_is_last_segment(p in safe_path()) {
        let leaf = tag_leaf(&p);
        prop_assert!(!leaf.contains('/'));
        prop_assert_eq!(Some(leaf.as_str()), p.split('/').next_back());
    }
}

// ===========================================================================
// Tag ancestry
// ===========================================================================

proptest! {
    /// Duality: `a.is_ancestor_of(b) == b.is_descendant_of(a)`, and both agree
    /// with the independent reference `b.starts_with(a + "/")` — on arbitrary
    /// strings (totality) as well as well-formed paths.
    #[test]
    fn ancestry_duality_arbitrary(a in ".*", b in ".*") {
        let ta = Tag::new(&a);
        let tb = Tag::new(&b);
        let reference = b.starts_with(&format!("{}/", a));
        prop_assert_eq!(ta.is_ancestor_of(&b), reference);
        prop_assert_eq!(tb.is_descendant_of(&a), reference);
    }

    /// A tag is never its own ancestor; appending a segment always creates a
    /// descendant; sibling prefix confusion ("a" vs "ab") never matches.
    #[test]
    fn ancestry_boundaries(p in safe_path(), seg in safe_segment()) {
        let t = Tag::new(&p);
        prop_assert!(!t.is_ancestor_of(&p), "tag is its own ancestor: {:?}", &p);

        let child = format!("{}/{}", p, seg);
        prop_assert!(t.is_ancestor_of(&child));
        prop_assert!(Tag::new(&child).is_descendant_of(&p));

        // "a" is not an ancestor of "ab" (needs the '/' boundary).
        let sibling = format!("{}{}", p, seg);
        prop_assert!(!t.is_ancestor_of(&sibling));
    }

    /// Tag::new derives leaf and depth consistently with the parse helpers.
    #[test]
    fn tag_new_consistent_with_helpers(p in safe_path()) {
        let t = Tag::new(&p);
        prop_assert_eq!(t.leaf.clone(), tag_leaf(&p));
        prop_assert_eq!(t.depth, tag_depth(&p));
        prop_assert_eq!(t.parent_path().map(str::to_string), tag_parent(&p));
        prop_assert_eq!(t.segments().join("/"), p);
    }
}

// ===========================================================================
// TagHierarchy vs brute-force reference
// ===========================================================================

fn paths_of(tags: Vec<&Tag>) -> BTreeSet<String> {
    tags.into_iter().map(|t| t.path.clone()).collect()
}

proptest! {
    /// On a parent-closed tag set, the hierarchy agrees with brute-force
    /// reference computations for roots, children, descendants, and ancestors.
    #[test]
    fn hierarchy_matches_reference(paths in closed_path_set()) {
        let tags: Vec<Tag> = paths.iter().map(|p| Tag::new(p)).collect();
        let h = TagHierarchy::from_tags(tags);

        prop_assert_eq!(h.len(), paths.len());
        prop_assert!(!h.is_empty());

        // Roots: closure means orphans can't exist → exactly the depth-0 tags.
        let expected_roots: BTreeSet<String> =
            paths.iter().filter(|p| !p.contains('/')).cloned().collect();
        prop_assert_eq!(paths_of(h.roots()), expected_roots);

        for p in &paths {
            // Children = paths whose parent is exactly p.
            let expected_children: BTreeSet<String> = paths
                .iter()
                .filter(|q| tag_parent(q).as_deref() == Some(p.as_str()))
                .cloned()
                .collect();
            prop_assert_eq!(paths_of(h.children_of(p)), expected_children, "children_of({:?})", p);

            // Descendants = paths strictly below p.
            let expected_desc: BTreeSet<String> = paths
                .iter()
                .filter(|q| q.starts_with(&format!("{}/", p)))
                .cloned()
                .collect();
            prop_assert_eq!(paths_of(h.descendants_of(p)), expected_desc, "descendants_of({:?})", p);

            // Ancestors = proper prefixes, ordered root → parent.
            let segs: Vec<&str> = p.split('/').collect();
            let expected_anc: Vec<String> =
                (1..segs.len()).map(|i| segs[..i].join("/")).collect();
            let actual_anc: Vec<String> =
                h.ancestors_of(p).iter().map(|t| t.path.clone()).collect();
            prop_assert_eq!(actual_anc, expected_anc, "ancestors_of({:?})", p);
        }
    }

    /// Input order does not change the hierarchy's semantics.
    #[test]
    fn hierarchy_order_independent(paths in closed_path_set()) {
        let forward: Vec<Tag> = paths.iter().map(|p| Tag::new(p)).collect();
        let mut backward = forward.clone();
        backward.reverse();

        let h1 = TagHierarchy::from_tags(forward);
        let h2 = TagHierarchy::from_tags(backward);

        prop_assert_eq!(h1.len(), h2.len());
        prop_assert_eq!(paths_of(h1.roots()), paths_of(h2.roots()));
        for p in &paths {
            prop_assert_eq!(paths_of(h1.children_of(p)), paths_of(h2.children_of(p)));
            prop_assert_eq!(paths_of(h1.descendants_of(p)), paths_of(h2.descendants_of(p)));
        }
        // format_tree sorts internally, so it must be order-stable too.
        prop_assert_eq!(h1.format_tree(), h2.format_tree());
    }

    /// effective_color resolves to the nearest self-or-ancestor color.
    ///
    /// Regression (fixed 2026-07-20): the implementation used to iterate
    /// `ancestors_of` root-first and return the FARTHEST colored ancestor.
    #[test]
    fn hierarchy_effective_color_nearest_ancestor(
        paths in closed_path_set(),
        colors in prop::collection::vec(prop::option::of(arb_color()), 32),
    ) {
        let path_list: Vec<String> = paths.iter().cloned().collect();
        let color_by_path: HashMap<String, Option<TagColor>> = path_list
            .iter()
            .enumerate()
            .map(|(i, p)| (p.clone(), colors[i % colors.len()].clone()))
            .collect();

        let tags: Vec<Tag> = path_list
            .iter()
            .map(|p| {
                let mut t = Tag::new(p);
                t.color = color_by_path[p].clone();
                t
            })
            .collect();
        let h = TagHierarchy::from_tags(tags);

        for p in &path_list {
            // Reference: walk self, then parents, first Some color wins.
            let mut expected: Option<TagColor> = None;
            let mut cursor = Some(p.clone());
            while let Some(cur) = cursor {
                if let Some(c) = color_by_path.get(&cur).cloned().flatten() {
                    expected = Some(c);
                    break;
                }
                cursor = tag_parent(&cur);
            }
            prop_assert_eq!(h.effective_color(p).cloned(), expected, "effective_color({:?})", p);
        }
    }
}

// ===========================================================================
// parse_tag_query / TagQuery::matches
// ===========================================================================

fn overlapping_tag_set() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(overlapping_path(), 0..6)
}

proptest! {
    /// Totality of the query parser.
    #[test]
    fn parse_tag_query_is_total(input in ".*") {
        let _ = parse_tag_query(&input);
    }

    /// Round-trip for plain and negated forms on well-formed paths.
    #[test]
    fn parse_tag_query_round_trip(p in safe_path()) {
        prop_assert_eq!(
            parse_tag_query(&format!("tags:{}", p)),
            Some(TagQuery::Has(p.clone()))
        );
        prop_assert_eq!(
            parse_tag_query(&format!("-tags:{}", p)),
            Some(TagQuery::Not(p.clone()))
        );
    }

    /// AND/OR forms parse into the corresponding combinators.
    #[test]
    fn parse_tag_query_combinators(a in safe_path(), b in safe_path()) {
        prop_assert_eq!(
            parse_tag_query(&format!("tags:{}+{}", a, b)),
            Some(TagQuery::And(
                Box::new(TagQuery::Has(a.clone())),
                Box::new(TagQuery::Has(b.clone()))
            ))
        );
        prop_assert_eq!(
            parse_tag_query(&format!("tags:{}|{}", a, b)),
            Some(TagQuery::Or(
                Box::new(TagQuery::Has(a.clone())),
                Box::new(TagQuery::Has(b.clone()))
            ))
        );
    }

    /// Not is the boolean complement of Has, on every tag set.
    #[test]
    fn tag_query_not_is_complement(p in overlapping_path(), tags in overlapping_tag_set()) {
        let has = TagQuery::Has(p.clone()).matches(&tags);
        let not = TagQuery::Not(p).matches(&tags);
        prop_assert_eq!(not, !has);
    }

    /// And/Or match conjunction/disjunction of their parts.
    #[test]
    fn tag_query_and_or_semantics(
        a in overlapping_path(),
        b in overlapping_path(),
        tags in overlapping_tag_set(),
    ) {
        let ha = TagQuery::Has(a.clone()).matches(&tags);
        let hb = TagQuery::Has(b.clone()).matches(&tags);
        let and = TagQuery::And(
            Box::new(TagQuery::Has(a.clone())),
            Box::new(TagQuery::Has(b.clone())),
        )
        .matches(&tags);
        let or = TagQuery::Or(Box::new(TagQuery::Has(a)), Box::new(TagQuery::Has(b))).matches(&tags);
        prop_assert_eq!(and, ha && hb);
        prop_assert_eq!(or, ha || hb);
    }

    /// Matching is order-independent in the tag set.
    #[test]
    fn tag_query_matches_order_independent(p in overlapping_path(), mut tags in overlapping_tag_set()) {
        let q = TagQuery::Has(p);
        let forward = q.matches(&tags);
        tags.reverse();
        prop_assert_eq!(q.matches(&tags), forward);
    }

    /// Inheritance boundary: a tag matches itself and true descendants, but
    /// never a sibling that merely shares a string prefix ("a" vs "ab").
    #[test]
    fn tag_query_inheritance_boundary(p in safe_path(), seg in safe_segment()) {
        let q = TagQuery::Has(p.clone());
        let descendant = vec![format!("{}/{}", p, seg)];
        let prefix_sibling = vec![format!("{}{}", p, seg)];
        prop_assert!(q.matches(std::slice::from_ref(&p)));
        prop_assert!(q.matches(&descendant));
        prop_assert!(!q.matches(&prefix_sibling));
    }
}

// ===========================================================================
// Minimized regression tests for bugs fixed 2026-07-20
// ===========================================================================

/// Regression: hyphen trimming used to expose non-space whitespace
/// (`"a\t-"` → `"a\t"`), breaking idempotence. One pass must now reach the
/// fixpoint directly.
#[test]
fn normalize_segment_hyphen_trim_whitespace_regression() {
    assert_eq!(normalize_tag_segment("a\t-"), "a");
    let once = normalize_tag_segment("a\t-");
    let twice = normalize_tag_segment(&once);
    assert_eq!(
        twice, once,
        "normalize_tag_segment must be idempotent on \"a\\t-\""
    );
}

/// Regression: effective_color used to return the farthest colored ancestor;
/// the documented contract is the NEAREST colored self-or-ancestor.
#[test]
fn hierarchy_effective_color_nearest_ancestor_unit() {
    let red = TagColor {
        light: "#f00".to_string(),
        dark: "#f00".to_string(),
    };
    let green = TagColor {
        light: "#0f0".to_string(),
        dark: "#0f0".to_string(),
    };

    let mut root = Tag::new("r");
    root.color = Some(red);
    let mut mid = Tag::new("r/g");
    mid.color = Some(green.clone());
    let leaf = Tag::new("r/g/x");

    let h = TagHierarchy::from_tags(vec![root, mid, leaf]);
    assert_eq!(
        h.effective_color("r/g/x"),
        Some(&green),
        "nearest colored ancestor of r/g/x is r/g (green)"
    );
}
