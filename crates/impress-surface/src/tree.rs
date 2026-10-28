//! `tree`'s nesting (plan-koine-tree-stream T-D1): rows that name a parent,
//! and the list of expanded ids, become the outline a renderer draws.
//!
//! Pure, like the rest of this crate. [`build`] returns the *visible* nodes
//! in drawing order: roots in row order, each followed by its children in row
//! order, and a node only when every ancestor is expanded.
//!
//! - A row whose parent is null, absent, or an id the rows do not hold is a
//!   root.
//! - Ids may be strings or numbers; two ids are the same when their JSON is.
//!   A row without an id, or repeating an earlier row's id, is skipped.
//! - A row with `"has_children": true` is a branch even when no row names it
//!   as a parent: a lazy tree's source answers a branch's children only once
//!   the branch is in the expanded list.
//! - A cycle is cut at the first repeated id: every row is drawn at most
//!   once, and rows only reachable round a cycle (no root above them) are
//!   promoted, the first in row order becoming the root the cycle is cut at.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

/// One line of the outline.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TreeNode {
    /// The row's id, as the row spelt it (a string or a number).
    pub id: Value,
    pub label: String,
    /// 0 for a root.
    pub depth: u32,
    /// The row has at least one child (whether or not it is expanded).
    pub has_children: bool,
    /// The node is a branch and its id is in the expanded list.
    pub expanded: bool,
    /// The row's `count_field` value (plan-koine-outline O-D6); `None` when
    /// no field is declared or the row lacks it or holds null. Absent from
    /// the wire when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<Value>,
    /// The row itself, for a renderer that wants a second column.
    pub row: Value,
}

/// The field names a tree reads, each `None` for its default.
#[derive(Debug, Clone, Copy, Default)]
pub struct Fields<'a> {
    pub id: Option<&'a str>,
    pub parent: Option<&'a str>,
    pub label: Option<&'a str>,
    pub count: Option<&'a str>,
}

/// See the module docs. `rows` is the array of rows (an envelope already
/// unwrapped); anything else yields no nodes. `expanded` is the list of
/// expanded ids: the whole list from the bind path, including ids of branches
/// a collapsed ancestor hides (plan-koine-outline O-D2), which keep their
/// state for when the ancestor opens again.
pub fn build(rows: &Value, expanded: &[Value], fields: Fields<'_>) -> Vec<TreeNode> {
    let id_field = fields.id.unwrap_or("id");
    let parent_field = fields.parent.unwrap_or("parent");
    let Some(rows) = rows.as_array() else {
        return Vec::new();
    };

    // The rows that can be drawn, with their id keys, in row order.
    let mut entries: Vec<(&Value, &Value, String)> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    for row in rows {
        let Some(id) = row.get(id_field).filter(|id| !id.is_null()) else {
            continue;
        };
        let key = id.to_string();
        if by_key.contains_key(&key) {
            continue;
        }
        by_key.insert(key.clone(), entries.len());
        entries.push((row, id, key));
    }

    // parent index per entry (None: a root) and children per entry.
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); entries.len()];
    let mut roots: Vec<usize> = Vec::new();
    for (index, (row, _, _)) in entries.iter().enumerate() {
        let parent = row
            .get(parent_field)
            .filter(|p| !p.is_null())
            .and_then(|p| by_key.get(&p.to_string()))
            .copied()
            .filter(|&parent| parent != index);
        match parent {
            Some(parent) => children[parent].push(index),
            None => roots.push(index),
        }
    }

    // Preorder over the whole forest, then the rows only a cycle reaches.
    // Each element: (entry, depth, has_children, position of its parent).
    let mut order: Vec<(usize, u32, bool, Option<usize>)> = Vec::with_capacity(entries.len());
    let mut visited = vec![false; entries.len()];
    let starts = roots.iter().copied().chain(0..entries.len());
    for start in starts {
        if visited[start] {
            continue;
        }
        let mut stack: Vec<(usize, u32, Option<usize>)> = vec![(start, 0, None)];
        while let Some((index, depth, parent_at)) = stack.pop() {
            if visited[index] {
                continue;
            }
            visited[index] = true;
            let at = order.len();
            // A row may say it has children the rows do not hold yet (a
            // lazy tree whose source answers a branch's children once it is
            // expanded): it is a branch all the same.
            let declares = entries[index]
                .0
                .get("has_children")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let has_children = declares || children[index].iter().any(|&c| !visited[c]);
            order.push((index, depth, has_children, parent_at));
            for &child in children[index].iter().rev() {
                if !visited[child] {
                    stack.push((child, depth + 1, Some(at)));
                }
            }
        }
    }

    let expanded: HashSet<String> = expanded.iter().map(Value::to_string).collect();
    // `shown[at]`: the node at `order[at]` is visible; `open[at]`: and its
    // children may be.
    let mut open: Vec<bool> = Vec::with_capacity(order.len());
    let mut out = Vec::new();
    for &(index, depth, has_children, parent_at) in &order {
        let (row, id, key) = &entries[index];
        let shown = parent_at.is_none_or(|p| open[p]);
        let is_expanded = has_children && expanded.contains(key);
        open.push(shown && is_expanded);
        if shown {
            out.push(TreeNode {
                id: (*id).clone(),
                label: label_of(row, id, fields.label),
                depth,
                has_children,
                expanded: is_expanded,
                count: fields
                    .count
                    .and_then(|field| row.get(field))
                    .filter(|v| !v.is_null())
                    .cloned(),
                row: (*row).clone(),
            });
        }
    }
    out
}

/// What a line shows: `label_field` when the row has it, else the first of
/// `label`, `title`, `name` the row has, else the id.
fn label_of(row: &Value, id: &Value, label_field: Option<&str>) -> String {
    let candidates: &[&str] = match label_field {
        Some(field) => &[field],
        None => &["label", "title", "name"],
    };
    let found = candidates
        .iter()
        .find_map(|field| row.get(field).filter(|v| !v.is_null()));
    display(found.unwrap_or(id))
}

fn display(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ── moving nodes (plan-koine-outline O-D4) ──────────────────────────────────
//
// A tree's visible nodes are in drawing order with a `depth`, no parent
// field. The parent of node `i` is the nearest preceding node one level up;
// its position among its siblings is the number of earlier nodes with the
// same parent. A collapsed branch hides all its children, so the siblings of
// a visible node are always all visible and both answers are exact. Every
// renderer that lets a person move a node (the web's drag and drop, the
// text frontend's cut and paste) asks these, so a drop and a paste mean the
// same `move` value. Positions are counted among the siblings as they are
// *before* the move: a handler that reorders within one parent adjusts for
// the moved node itself.

/// The index of node `at`'s parent among `nodes`, `None` for a root.
pub fn parent_index(nodes: &[TreeNode], at: usize) -> Option<usize> {
    let depth = nodes.get(at)?.depth;
    nodes[..at].iter().rposition(|n| n.depth < depth)
}

/// The id of node `at`'s parent: `null` for a root.
pub fn parent_id(nodes: &[TreeNode], at: usize) -> Value {
    parent_index(nodes, at).map_or(Value::Null, |p| nodes[p].id.clone())
}

/// How many siblings precede node `at`: its position under its parent.
pub fn sibling_index(nodes: &[TreeNode], at: usize) -> usize {
    let parent = parent_index(nodes, at);
    (0..at)
        .filter(|&i| parent_index(nodes, i) == parent && nodes[i].depth == nodes[at].depth)
        .count()
}

/// Whether node `at` is `dragged` or inside it: a move there would put a
/// branch under itself.
pub fn is_within(nodes: &[TreeNode], dragged: &Value, at: usize) -> bool {
    let Some(start) = nodes.iter().position(|n| &n.id == dragged) else {
        return false;
    };
    if at < start {
        return false;
    }
    let depth = nodes[start].depth;
    nodes[start + 1..=at].iter().all(|n| n.depth > depth)
}

/// `move` value for dropping `dragged` onto row `at`: it becomes that row's
/// first child. `None` when the row is the dragged one or inside it.
pub fn drop_on(nodes: &[TreeNode], dragged: &Value, at: usize) -> Option<Value> {
    let target = nodes.get(at)?;
    if is_within(nodes, dragged, at) {
        return None;
    }
    Some(json!({"ids": [dragged], "to": target.id, "index": 0}))
}

/// `move` value for dropping `dragged` in the gap before row `at` (`at ==
/// nodes.len()` is the gap after the last row, among the roots): a sibling of
/// that row, at its position. `None` when the new parent is the dragged row or
/// inside it.
pub fn drop_gap(nodes: &[TreeNode], dragged: &Value, at: usize) -> Option<Value> {
    if at > nodes.len() {
        return None;
    }
    let (to, index) = if at == nodes.len() {
        (
            Value::Null,
            (0..at).filter(|&i| nodes[i].depth == 0).count(),
        )
    } else {
        (parent_id(nodes, at), sibling_index(nodes, at))
    };
    if let Some(parent) = parent_index(nodes, at).filter(|_| at < nodes.len()) {
        if is_within(nodes, dragged, parent) {
            return None;
        }
    }
    Some(json!({"ids": [dragged], "to": to, "index": index}))
}

/// `move` value for pasting the cut row `cut` into the active row `active`
/// (`p`): the same as dropping it on the row.
pub fn paste_into(nodes: &[TreeNode], cut: &Value, active: &Value) -> Option<Value> {
    let at = nodes.iter().position(|n| &n.id == active)?;
    drop_on(nodes, cut, at)
}

/// `move` value for pasting `cut` beside the active row (`P`): a sibling
/// right after it, under its parent.
pub fn paste_beside(nodes: &[TreeNode], cut: &Value, active: &Value) -> Option<Value> {
    let at = nodes.iter().position(|n| &n.id == active)?;
    if let Some(parent) = parent_index(nodes, at) {
        if is_within(nodes, cut, parent) {
            return None;
        }
    }
    Some(json!({
        "ids": [cut],
        "to": parent_id(nodes, at),
        "index": sibling_index(nodes, at) + 1,
    }))
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_row_that_says_it_has_children_is_a_branch_before_they_arrive() {
        let rows = json!([
            {"id": "ai", "label": "ai", "has_children": true},
            {"id": "x", "label": "x"}
        ]);
        let closed = build(&rows, &[], Fields::default());
        assert!(closed[0].has_children && !closed[0].expanded);
        assert!(!closed[1].has_children);
        let open = build(&rows, &[json!("ai")], Fields::default());
        assert!(open[0].has_children && open[0].expanded);
    }

    use super::*;
    use serde_json::json;

    fn labels(nodes: &[TreeNode]) -> Vec<(String, u32)> {
        nodes.iter().map(|n| (n.label.clone(), n.depth)).collect()
    }

    fn rows() -> Value {
        json!([
            {"id": "a", "label": "src"},
            {"id": "b", "parent": "a", "label": "main.rs"},
            {"id": "c", "label": "docs"},
            {"id": "d", "parent": "a", "label": "lib"},
            {"id": "e", "parent": "d", "label": "mod.rs"},
        ])
    }

    #[test]
    fn collapsed_shows_only_roots() {
        let nodes = build(&rows(), &[], Fields::default());
        assert_eq!(labels(&nodes), vec![("src".into(), 0), ("docs".into(), 0)]);
        assert!(nodes[0].has_children && !nodes[0].expanded);
        assert!(!nodes[1].has_children);
    }

    #[test]
    fn expanded_shows_children_in_row_order_and_depth_first() {
        let nodes = build(&rows(), &[json!("a"), json!("d")], Fields::default());
        assert_eq!(
            labels(&nodes),
            vec![
                ("src".into(), 0),
                ("main.rs".into(), 1),
                ("lib".into(), 1),
                ("mod.rs".into(), 2),
                ("docs".into(), 0),
            ]
        );
        assert!(nodes[0].expanded && nodes[2].expanded);
        assert!(!nodes[1].has_children && !nodes[1].expanded);
        assert_eq!(nodes[1].row["label"], "main.rs");
    }

    #[test]
    fn a_collapsed_ancestor_hides_an_expanded_descendant() {
        let nodes = build(&rows(), &[json!("d")], Fields::default());
        assert_eq!(nodes.len(), 2);
    }

    #[test]
    fn a_leaf_in_the_expanded_list_is_not_expanded() {
        let nodes = build(&rows(), &[json!("c")], Fields::default());
        assert!(!nodes[1].expanded);
    }

    #[test]
    fn an_unknown_parent_is_a_root() {
        let rows = json!([{"id": 1, "parent": 99, "name": "orphan"}, {"id": 2, "parent": null}]);
        let nodes = build(&rows, &[], Fields::default());
        assert_eq!(
            labels(&nodes),
            vec![("orphan".into(), 0), ("2".into(), 0)],
            "a missing label falls back to the id"
        );
    }

    #[test]
    fn numeric_ids_nest_and_expand() {
        let rows = json!([{"id": 1, "title": "one"}, {"id": 2, "parent": 1, "title": "two"}]);
        let nodes = build(&rows, &[json!(1)], Fields::default());
        assert_eq!(labels(&nodes), vec![("one".into(), 0), ("two".into(), 1)]);
        assert_eq!(nodes[1].id, json!(2));
        // The string "1" is not the number 1.
        assert_eq!(build(&rows, &[json!("1")], Fields::default()).len(), 1);
    }

    #[test]
    fn a_cycle_is_cut_at_the_first_repeated_id() {
        let rows = json!([
            {"id": "a", "parent": "b"},
            {"id": "b", "parent": "a"},
            {"id": "c", "parent": "c"},
        ]);
        let nodes = build(&rows, &[json!("a"), json!("b")], Fields::default());
        assert_eq!(
            labels(&nodes),
            vec![("c".into(), 0), ("a".into(), 0), ("b".into(), 1)],
            "c (its own parent) is a root; a, first of the cycle a-b, is promoted"
        );
        assert!(!nodes[0].has_children, "c's only child is c");
        assert!(nodes[1].has_children);
        assert!(!nodes[2].has_children, "b's only child, a, is its ancestor");
    }

    #[test]
    fn a_cycle_below_a_root_is_cut_too() {
        let rows = json!([
            {"id": "r"},
            {"id": "x", "parent": "y"},
            {"id": "y", "parent": "x"},
        ]);
        let nodes = build(&rows, &[], Fields::default());
        assert_eq!(nodes.len(), 2, "r, and the promoted x");
    }

    #[test]
    fn declared_fields_are_read_and_bad_rows_skipped() {
        let rows = json!([
            {"key": "r", "up": null, "text": "root"},
            {"key": "k", "up": "r", "text": "kid"},
            {"key": "k", "up": "r", "text": "duplicate"},
            {"up": "r", "text": "no id"},
            "not an object",
        ]);
        let fields = Fields {
            id: Some("key"),
            parent: Some("up"),
            label: Some("text"),
            count: None,
        };
        let nodes = build(&rows, &[json!("r")], fields);
        assert_eq!(labels(&nodes), vec![("root".into(), 0), ("kid".into(), 1)]);
    }

    #[test]
    fn count_is_the_declared_field_and_none_when_absent_or_null() {
        let rows = json!([
            {"id": "a", "n": 3},
            {"id": "b", "n": 0},
            {"id": "c", "n": null},
            {"id": "d"},
        ]);
        let fields = Fields {
            count: Some("n"),
            ..Fields::default()
        };
        let counts: Vec<Option<Value>> = build(&rows, &[], fields)
            .into_iter()
            .map(|n| n.count)
            .collect();
        assert_eq!(counts, vec![Some(json!(3)), Some(json!(0)), None, None]);
        assert!(build(&rows, &[], Fields::default())
            .iter()
            .all(|n| n.count.is_none()));
    }

    #[test]
    fn not_rows_is_no_nodes() {
        assert!(build(&json!(null), &[], Fields::default()).is_empty());
        assert!(build(&json!({"a": 1}), &[], Fields::default()).is_empty());
    }

    #[test]
    fn a_deep_chain_does_not_overflow_the_stack() {
        let rows: Vec<Value> = (0..50_000)
            .map(|i| json!({"id": i, "parent": if i == 0 { Value::Null } else { json!(i - 1) }}))
            .collect();
        let all: Vec<Value> = (0..50_000).map(|i| json!(i)).collect();
        assert_eq!(
            build(&Value::Array(rows), &all, Fields::default()).len(),
            50_000
        );
    }
}

#[cfg(test)]
mod move_tests {
    use super::*;

    fn node(id: Value, depth: u32) -> TreeNode {
        TreeNode {
            id,
            label: String::new(),
            depth,
            has_children: false,
            expanded: false,
            count: None,
            row: Value::Null,
        }
    }

    /// a            (0)
    ///   b          (1)
    ///     c        (2)
    ///   d          (1)
    /// e            (0)
    fn outline() -> Vec<TreeNode> {
        vec![
            node(json!("a"), 0),
            node(json!("b"), 1),
            node(json!("c"), 2),
            node(json!("d"), 1),
            node(json!(5), 0),
        ]
    }

    #[test]
    fn a_parent_is_the_nearest_node_one_level_up() {
        let n = outline();
        let parents: Vec<Option<usize>> = (0..n.len()).map(|i| parent_index(&n, i)).collect();
        assert_eq!(parents, vec![None, Some(0), Some(1), Some(0), None]);
        assert_eq!(parent_id(&n, 2), json!("b"));
        assert_eq!(parent_id(&n, 4), Value::Null);
        assert_eq!(parent_index(&n, 9), None);
    }

    #[test]
    fn a_sibling_index_counts_earlier_nodes_with_the_same_parent() {
        let n = outline();
        let at: Vec<usize> = (0..n.len()).map(|i| sibling_index(&n, i)).collect();
        // a: first root; b, d: children 0 and 1 of a; c: only child of b;
        // 5: second root.
        assert_eq!(at, vec![0, 0, 0, 1, 1]);
    }

    #[test]
    fn a_node_is_within_itself_and_its_descendants_only() {
        let n = outline();
        let within: Vec<bool> = (0..n.len())
            .map(|i| is_within(&n, &json!("a"), i))
            .collect();
        assert_eq!(within, vec![true, true, true, true, false]);
        let within: Vec<bool> = (0..n.len())
            .map(|i| is_within(&n, &json!("b"), i))
            .collect();
        assert_eq!(within, vec![false, true, true, false, false]);
        assert!(!is_within(&n, &json!("missing"), 2));
    }

    #[test]
    fn dropping_on_a_row_makes_the_dragged_row_its_first_child() {
        let n = outline();
        assert_eq!(
            drop_on(&n, &json!(5), 0),
            Some(json!({"ids": [5], "to": "a", "index": 0}))
        );
        assert_eq!(
            drop_on(&n, &json!("c"), 3),
            Some(json!({"ids": ["c"], "to": "d", "index": 0}))
        );
        // Onto itself or its own descendant: nothing.
        assert_eq!(drop_on(&n, &json!("a"), 0), None);
        assert_eq!(drop_on(&n, &json!("a"), 2), None);
        assert_eq!(drop_on(&n, &json!("a"), 9), None);
    }

    #[test]
    fn dropping_in_a_gap_makes_a_sibling_at_that_position() {
        let n = outline();
        // Before `d`: under a, after b.
        assert_eq!(
            drop_gap(&n, &json!(5), 3),
            Some(json!({"ids": [5], "to": "a", "index": 1}))
        );
        // Before `a`: first root.
        assert_eq!(
            drop_gap(&n, &json!(5), 0),
            Some(json!({"ids": [5], "to": null, "index": 0}))
        );
        // Before `5`: second root; the gap after the last row: third.
        assert_eq!(
            drop_gap(&n, &json!("c"), 4),
            Some(json!({"ids": ["c"], "to": null, "index": 1}))
        );
        assert_eq!(
            drop_gap(&n, &json!("c"), 5),
            Some(json!({"ids": ["c"], "to": null, "index": 2}))
        );
        // Before `c`: its parent is `b`, which is the dragged row.
        assert_eq!(drop_gap(&n, &json!("b"), 2), None);
        // Beside the dragged row's own parent is fine.
        assert_eq!(
            drop_gap(&n, &json!("c"), 1),
            Some(json!({"ids": ["c"], "to": "a", "index": 0}))
        );
        assert_eq!(drop_gap(&n, &json!("c"), 6), None);
        assert_eq!(
            drop_gap(&[], &json!("c"), 0),
            Some(json!({"ids": ["c"], "to": null, "index": 0}))
        );
    }

    #[test]
    fn a_paste_goes_under_the_active_row_or_right_after_it() {
        let n = outline();
        assert_eq!(
            paste_into(&n, &json!(5), &json!("b")),
            Some(json!({"ids": [5], "to": "b", "index": 0}))
        );
        assert_eq!(paste_into(&n, &json!("a"), &json!("c")), None);
        assert_eq!(paste_into(&n, &json!(5), &json!("gone")), None);
        // Beside `b` (child 0 of a): child 1 of a.
        assert_eq!(
            paste_beside(&n, &json!(5), &json!("b")),
            Some(json!({"ids": [5], "to": "a", "index": 1}))
        );
        // Beside a root: after it among the roots.
        assert_eq!(
            paste_beside(&n, &json!("c"), &json!("a")),
            Some(json!({"ids": ["c"], "to": null, "index": 1}))
        );
        // Beside a row whose parent is inside the cut branch.
        assert_eq!(paste_beside(&n, &json!("b"), &json!("c")), None);
        assert_eq!(paste_beside(&n, &json!("b"), &json!("gone")), None);
    }
}
