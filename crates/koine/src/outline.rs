//! The row menu every frontend draws (plan-koine-affordances A-D2).
//!
//! A row verb needs a visible path on every pointer: the `…` button for a
//! finger, the secondary click for a mouse, `m` for a keyboard. All three
//! open the same list, and [`menu_entries`] is that list, so the web menu,
//! the TUI's numbered prompt and the keys that pick from either agree.
//!
//! The list is the spec's `actions` with the tree's own row verbs around
//! them: "Rename" first when the tree handles `on_rename`, "Delete" last when
//! it handles `on_delete` and no action already has that label. Which entry
//! was picked means one of three things ([`MenuChoice`]):
//!
//! - "Rename" on a renameable tree is the frontend's: it opens the inline
//!   rename, as the `r` key does, and sends nothing yet. It wins over a spec
//!   action of the same label, which is therefore not listed twice and never
//!   reached from the menu (the spec's `on_rename` is what a rename runs).
//! - The appended "Delete" sends `delete` with `[id]`, the value the `d` key
//!   sends, so it runs the spec's `on_delete`. A spec action labelled
//!   "Delete" is the spec's own entry and keeps its place and its handler.
//! - Anything else is a spec action: a `menu` event `{"id", "action"}`.

use impress_surface::RenderKind;

/// The label of the frontend's own rename entry.
pub const RENAME: &str = "Rename";
/// The label of the delete entry appended for a tree that handles `on_delete`.
pub const DELETE: &str = "Delete";

/// The row menu's entries, in order: "Rename" when `renameable`, then the
/// spec's `actions` (less one labelled "Rename" when the frontend's own takes
/// that place), then "Delete" when `deletable` and no action already has that
/// exact label. Empty when there is nothing to offer, and then no `…` button
/// is drawn.
pub fn menu_entries(actions: &[String], renameable: bool, deletable: bool) -> Vec<String> {
    let mut out = Vec::with_capacity(actions.len() + 2);
    if renameable {
        out.push(RENAME.to_string());
    }
    out.extend(
        actions
            .iter()
            .filter(|a| !(renameable && a.as_str() == RENAME))
            .cloned(),
    );
    if deletable && !actions.iter().any(|a| a == DELETE) {
        out.push(DELETE.to_string());
    }
    out
}

/// What picking one entry of [`menu_entries`] means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuChoice {
    /// Open the inline rename of the row (the frontend's own; sends nothing).
    Rename,
    /// Send `delete` with `[id]`, as the `d` key does.
    Delete,
    /// Send `menu` with `{"id", "action": <this label>}`.
    Action(String),
}

/// The meaning of the entry at `index` of [`menu_entries`] for the same
/// arguments; `None` past the end.
pub fn menu_choice(
    actions: &[String],
    renameable: bool,
    deletable: bool,
    index: usize,
) -> Option<MenuChoice> {
    let entry = menu_entries(actions, renameable, deletable)
        .into_iter()
        .nth(index)?;
    Some(if renameable && entry == RENAME {
        MenuChoice::Rename
    } else if deletable && entry == DELETE && !actions.iter().any(|a| a == DELETE) {
        MenuChoice::Delete
    } else {
        MenuChoice::Action(entry)
    })
}

/// One row widget's menu: what [`menu_entries`] and [`menu_choice`] are
/// computed from, read off the widget's [`RenderKind`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowMenu {
    pub actions: Vec<String>,
    pub renameable: bool,
    pub deletable: bool,
}

impl RowMenu {
    /// The menu of a `tree`, `table` or `list`; `None` for any other kind. A
    /// table or a list has no rename or delete of its own: its menu is its
    /// `actions`.
    pub fn of(kind: &RenderKind) -> Option<Self> {
        match kind {
            RenderKind::Tree {
                actions,
                renameable,
                deletable,
                ..
            } => Some(Self {
                actions: actions.clone(),
                renameable: *renameable,
                deletable: *deletable,
            }),
            RenderKind::Table { actions, .. } | RenderKind::List { actions, .. } => Some(Self {
                actions: actions.clone(),
                ..Self::default()
            }),
            _ => None,
        }
    }

    /// [`menu_entries`] of this menu.
    pub fn entries(&self) -> Vec<String> {
        menu_entries(&self.actions, self.renameable, self.deletable)
    }

    /// [`menu_choice`] of this menu.
    pub fn choice(&self, index: usize) -> Option<MenuChoice> {
        menu_choice(&self.actions, self.renameable, self.deletable, index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn rename_comes_first_and_delete_last_around_the_specs_actions() {
        let actions = labels(&["Add child", "Archive"]);
        assert_eq!(
            menu_entries(&actions, true, true),
            labels(&["Rename", "Add child", "Archive", "Delete"])
        );
        assert_eq!(
            menu_entries(&actions, false, true),
            labels(&["Add child", "Archive", "Delete"])
        );
        assert_eq!(
            menu_entries(&actions, true, false),
            labels(&["Rename", "Add child", "Archive"])
        );
        assert_eq!(menu_entries(&actions, false, false), actions);
    }

    #[test]
    fn nothing_to_offer_is_an_empty_menu() {
        assert!(menu_entries(&[], false, false).is_empty());
        assert_eq!(menu_entries(&[], true, false), labels(&["Rename"]));
        assert_eq!(menu_entries(&[], false, true), labels(&["Delete"]));
    }

    #[test]
    fn a_spec_delete_keeps_its_place_and_is_not_listed_twice() {
        let actions = labels(&["Add child", "Delete"]);
        assert_eq!(
            menu_entries(&actions, true, true),
            labels(&["Rename", "Add child", "Delete"])
        );
        // The exact label only: "Delete…" is another entry.
        let other = labels(&["Delete…"]);
        assert_eq!(
            menu_entries(&other, false, true),
            labels(&["Delete…", "Delete"])
        );
    }

    #[test]
    fn the_frontends_rename_wins_over_a_spec_action_of_that_label() {
        let actions = labels(&["Rename", "Delete"]);
        assert_eq!(
            menu_entries(&actions, true, true),
            labels(&["Rename", "Delete"])
        );
        assert_eq!(
            menu_choice(&actions, true, true, 0),
            Some(MenuChoice::Rename)
        );
        // Not renameable: the spec's "Rename" is an action like any other.
        assert_eq!(
            menu_choice(&actions, false, false, 0),
            Some(MenuChoice::Action("Rename".into()))
        );
    }

    #[test]
    fn a_pick_names_what_it_does() {
        let actions = labels(&["Add child"]);
        assert_eq!(
            menu_choice(&actions, true, true, 0),
            Some(MenuChoice::Rename)
        );
        assert_eq!(
            menu_choice(&actions, true, true, 1),
            Some(MenuChoice::Action("Add child".into()))
        );
        assert_eq!(
            menu_choice(&actions, true, true, 2),
            Some(MenuChoice::Delete)
        );
        assert_eq!(menu_choice(&actions, true, true, 3), None);
        // The spec's own Delete is the spec's: a `menu` event.
        let with_delete = labels(&["Delete"]);
        assert_eq!(
            menu_choice(&with_delete, false, true, 0),
            Some(MenuChoice::Action("Delete".into()))
        );
    }
}
