//! The pure half of the outline gestures (`docs/plan-koine-outline.md`
//! O-D1 to O-D6): the payloads a menu pick and a rename send, the expanded
//! list a disclosure sends, the keys of an open menu, and a row's id and
//! count as the cursor spells them. The mapping and the web page both call
//! these; nothing here touches the DOM or a signal, so each rule is a plain
//! test.
//!
//! The arithmetic of a move (a node's parent and position, the `move` value
//! of a drop or a paste) is `impress_surface::tree`'s, re-exported here, so
//! the text frontend's cut and paste mean what the web's drag and drop do.

pub use impress_surface::tree::{
    drop_gap, drop_on, is_within, parent_id, parent_index, paste_beside, paste_into, sibling_index,
};
use serde_json::{json, Value};

/// A row the gestures name: the widget id and the row's id.
pub type RowRef = (String, Value);

pub use koine::outline::{menu_entries, MenuChoice, RowMenu};

/// Where a row being moved would land (plan-koine-affordances A-D1): on the
/// row at this visible index (it becomes the parent), or in the gap before
/// the row at this index (`len` is after the last row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropAt {
    Row(usize),
    Gap(usize),
}

/// A drop target the gestures name: the widget id and where in it.
pub type DropRef = (String, DropAt);

/// The drop target an element names by its attributes: `data-koine-row`
/// (a row), else `data-koine-gap` (a gap). `None` when neither is an index.
pub fn drop_at(row: Option<&str>, gap: Option<&str>) -> Option<DropAt> {
    if let Some(row) = row {
        return row.parse().ok().map(DropAt::Row);
    }
    gap?.parse().ok().map(DropAt::Gap)
}

/// The `move` value releasing `dragged` over `at` sends: [`drop_on`] for a
/// row, [`drop_gap`] for a gap. `None` where the row may not land (on itself,
/// inside its own branch).
pub fn drop_value(
    all: &[impress_surface::tree::TreeNode],
    dragged: &Value,
    at: DropAt,
) -> Option<Value> {
    match at {
        DropAt::Row(index) => drop_on(all, dragged, index),
        DropAt::Gap(at) => drop_gap(all, dragged, at),
    }
}

/// `drop_at` from the answer of the page's hit test, `row:<n>` or `gap:<n>`
/// (the form a script in a WebView can return).
pub fn parse_drop_at(answer: &str) -> Option<DropAt> {
    match answer.split_once(':')? {
        ("row", n) => drop_at(Some(n), None),
        ("gap", n) => drop_at(None, Some(n)),
        _ => None,
    }
}

/// The expanded list with `id` open or closed: the whole list, one id added
/// or removed (O-D2). Adding an id already there keeps one.
pub fn set_open(expanded: &[Value], id: &Value, open: bool) -> Value {
    let mut out: Vec<Value> = expanded.iter().filter(|e| *e != id).cloned().collect();
    if open {
        out.push(id.clone());
    }
    Value::Array(out)
}

/// A table or list row's id as the cursor spells it (`koine::cursor`): the
/// table's `id_field`, then `id`, the first that is not null; else the
/// row's index.
pub fn row_key(row: &Value, id_field: Option<&str>, index: usize) -> Value {
    id_field
        .into_iter()
        .chain(["id"])
        .filter_map(|field| row.get(field))
        .find(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| Value::from(index))
}

/// A row's count text: `field` of `row` when present and not null.
pub fn count_text(row: &Value, field: Option<&str>) -> Option<String> {
    let value = row.get(field?).filter(|v| !v.is_null())?;
    Some(match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

/// `menu` value: the row and the label of the entry chosen.
pub fn menu_value(id: &Value, action: &str) -> Value {
    json!({"id": id, "action": action})
}

/// `rename` value: the row and its new text.
pub fn rename_value(id: &Value, label: &str) -> Value {
    json!({"id": id, "label": label})
}

/// What a key does while a row menu is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuKey {
    /// Close the menu (`Escape`).
    Close,
    /// Choose the entry at this index (`1` to `9`).
    Pick(usize),
    /// Not the menu's; it swallows the key anyway so the row cursor holds still.
    Swallow,
}

/// The menu's keys: `Escape` closes, a digit picks that entry (counted from
/// 1) when there is one.
pub fn menu_key(name: &str, entries: usize) -> MenuKey {
    if name == "Escape" {
        return MenuKey::Close;
    }
    match name.parse::<usize>() {
        Ok(n) if (1..=9).contains(&n) && n <= entries => MenuKey::Pick(n - 1),
        _ => MenuKey::Swallow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_whole_expanded_list_is_sent_with_one_id_added_or_removed() {
        let all = [json!("a"), json!("hidden-grandchild")];
        assert_eq!(
            set_open(&all, &json!(7), true),
            json!(["a", "hidden-grandchild", 7])
        );
        assert_eq!(
            set_open(&all, &json!("a"), false),
            json!(["hidden-grandchild"])
        );
        assert_eq!(
            set_open(&all, &json!("a"), true),
            json!(["hidden-grandchild", "a"])
        );
        assert_eq!(set_open(&[], &json!("a"), false), json!([]));
    }

    #[test]
    fn a_menu_pick_and_a_rename_carry_the_row_and_the_choice() {
        assert_eq!(
            menu_value(&json!("a"), "Delete"),
            json!({"id": "a", "action": "Delete"})
        );
        assert_eq!(
            rename_value(&json!(7), "Papers"),
            json!({"id": 7, "label": "Papers"})
        );
    }

    #[test]
    fn the_open_menu_takes_escape_and_digits_and_swallows_the_rest() {
        assert_eq!(menu_key("Escape", 2), MenuKey::Close);
        assert_eq!(menu_key("1", 2), MenuKey::Pick(0));
        assert_eq!(menu_key("2", 2), MenuKey::Pick(1));
        assert_eq!(menu_key("3", 2), MenuKey::Swallow);
        assert_eq!(menu_key("0", 2), MenuKey::Swallow);
        assert_eq!(menu_key("j", 2), MenuKey::Swallow);
    }

    #[test]
    fn a_drop_target_is_read_off_the_attributes_and_sends_what_drop_on_and_drop_gap_say() {
        use impress_surface::tree::TreeNode;
        assert_eq!(drop_at(Some("2"), None), Some(DropAt::Row(2)));
        assert_eq!(drop_at(None, Some("0")), Some(DropAt::Gap(0)));
        assert_eq!(drop_at(Some("x"), Some("1")), None, "a row attribute wins");
        assert_eq!(drop_at(None, None), None);
        assert_eq!(parse_drop_at("row:3"), Some(DropAt::Row(3)));
        assert_eq!(parse_drop_at("gap:4"), Some(DropAt::Gap(4)));
        assert_eq!(parse_drop_at(""), None);
        assert_eq!(parse_drop_at("cell:1"), None);

        let node = |id: &str, depth| TreeNode {
            id: json!(id),
            label: id.into(),
            depth,
            has_children: false,
            expanded: false,
            count: None,
            row: json!({}),
        };
        let all = [node("a", 0), node("b", 0)];
        assert_eq!(
            drop_value(&all, &json!("b"), DropAt::Row(0)),
            drop_on(&all, &json!("b"), 0)
        );
        assert_eq!(
            drop_value(&all, &json!("b"), DropAt::Row(0)),
            Some(json!({"ids": ["b"], "to": "a", "index": 0}))
        );
        assert_eq!(
            drop_value(&all, &json!("b"), DropAt::Row(1)),
            None,
            "on itself"
        );
        assert_eq!(
            drop_value(&all, &json!("b"), DropAt::Gap(0)),
            drop_gap(&all, &json!("b"), 0)
        );
    }

    #[test]
    fn row_keys_follow_the_cursor_and_counts_skip_null() {
        assert_eq!(row_key(&json!({"id": "p1"}), None, 4), json!("p1"));
        assert_eq!(row_key(&json!("alpha"), None, 4), json!(4));
        assert_eq!(
            row_key(&json!({"id": "p1", "bib": "x"}), Some("bib"), 4),
            json!("x")
        );
        assert_eq!(row_key(&json!({"id": "p1"}), Some("bib"), 4), json!("p1"));
        assert_eq!(count_text(&json!({"n": 3}), Some("n")), Some("3".into()));
        assert_eq!(count_text(&json!({"n": 0}), Some("n")), Some("0".into()));
        assert_eq!(count_text(&json!({"n": null}), Some("n")), None);
        assert_eq!(count_text(&json!({}), Some("n")), None);
        assert_eq!(count_text(&json!({"n": 3}), None), None);
    }
}
