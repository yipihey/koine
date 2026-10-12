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
//! - A cycle is cut at the first repeated id: every row is drawn at most
//!   once, and rows only reachable round a cycle (no root above them) are
//!   promoted, the first in row order becoming the root the cycle is cut at.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

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
    /// The row itself, for a renderer that wants a second column.
    pub row: Value,
}

/// The field names a tree reads, each `None` for its default.
#[derive(Debug, Clone, Copy, Default)]
pub struct Fields<'a> {
    pub id: Option<&'a str>,
    pub parent: Option<&'a str>,
    pub label: Option<&'a str>,
}

/// See the module docs. `rows` is the array of rows (an envelope already
/// unwrapped); anything else yields no nodes. `expanded` is the list of
/// expanded ids.
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
            let has_children = children[index].iter().any(|&c| !visited[c]);
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

#[cfg(test)]
mod tests {
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
        };
        let nodes = build(&rows, &[json!("r")], fields);
        assert_eq!(labels(&nodes), vec![("root".into(), 0), ("kid".into(), 1)]);
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
