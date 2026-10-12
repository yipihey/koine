//! What a sidebar row's verbs forward to (plan L8 sidebar surface, SB-D4).
//!
//! The outline's rename, delete, move and create are not new capabilities:
//! the collection kernel (`collection-service_{rename,delete,reparent,
//! create}`) already does each of them, with the cycle check and the
//! binding-specific storage. What the outline adds is the decision that
//! turns a ROW into a kernel call, and that decision is pure — so it is here,
//! tested without a store, and the verbs in `service.rs` only run what it
//! returns.
//!
//! # The one decision the forwarder makes
//!
//! A drop ON a library row is a move to the ROOT of that library
//! (`new_parent_id: null`) — what the legacy sidebar's `handleReparent` does.
//! The kernel's `reparent` takes a parent COLLECTION, never a library, so
//! passing the library's id through would be a refusal for the most ordinary
//! drag in the tree.
//!
//! # What is refused, and what it says
//!
//! Only a [`OutlineNode::Collection`] row is a collection. Every other row
//! (a library, a section, a feed, a flag, a form) refuses with
//! `invalid-argument` naming what the row IS and that the legacy sidebar
//! still does it, so an agent reads "not here yet" rather than "you are
//! wrong".

use impress_service_core::Refusal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::outline::OutlineNode;

/// The name a collection created without one gets — the legacy sidebar's.
pub const DEFAULT_COLLECTION_NAME: &str = "New Collection";

/// One call to another verb: its MCP tool name and its argument object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForwardCall {
    /// The tool name (`collection-service_rename`).
    pub verb: String,
    /// The argument object, exactly as the verb's input schema spells it.
    pub args: Value,
}

impl ForwardCall {
    fn new(verb: &str, args: Value) -> Self {
        Self {
            verb: verb.to_string(),
            args,
        }
    }
}

/// The collection-kernel binding the app's collections use: `imbib` and
/// `impress` → `imbib` (publication collections), `imprint` → `manuscript`,
/// `implore` → `figure`, anything else → `generic`.
pub fn collection_binding(app_id: &str) -> &'static str {
    match app_id {
        "imbib" | "impress" => "imbib",
        "imprint" => "manuscript",
        "implore" => "figure",
        _ => "generic",
    }
}

/// What a row IS, in the words a refusal uses ("a library").
fn describe(node: &OutlineNode) -> String {
    match node {
        OutlineNode::Section { section } => format!("the '{section}' section"),
        OutlineNode::Library { .. } => "a library".into(),
        OutlineNode::Collection { .. } => "a collection".into(),
        OutlineNode::SmartSearch { .. } => "a smart search (feed)".into(),
        OutlineNode::SharedLibrary { .. } => "a shared library".into(),
        OutlineNode::ScixLibrary { .. } => "a SciX library".into(),
        OutlineNode::SearchForm { form } => format!("the '{form}' search form"),
        OutlineNode::FeedForm { .. } => "a feed form".into(),
        OutlineNode::Recent => "the Recent row".into(),
        OutlineNode::ArtifactType { artifact_type } => {
            format!("the '{artifact_type}' artifact type")
        }
        OutlineNode::CustomSurface { id } => format!("the '{id}' surface"),
        OutlineNode::Record { kind, .. } => format!("a {kind} row"),
        OutlineNode::RecordDetail { kind, .. } => format!("a single {kind}"),
        OutlineNode::Auxiliary { route } => format!("the '{route}' route"),
    }
}

/// The refusal for a row the kernel does not own: names what the row is and
/// where the action still happens.
fn not_a_collection(node: &OutlineNode, action: &str) -> Refusal {
    let what = describe(node);
    let hint = match node {
        OutlineNode::Library { .. } => format!(
            "a library is {action}d in the chassis sidebar for now; the collection kernel \
             only handles collections"
        ),
        _ => format!(
            "{what} cannot be {action}d from the outline surface yet — the legacy sidebar \
             still does it"
        ),
    };
    Refusal::invalid_argument(format!("{what} is not a collection: {hint}"))
}

/// The id of a collection row, or the refusal for any other row.
fn collection_id(node: &OutlineNode, action: &str) -> Result<String, Refusal> {
    match node {
        OutlineNode::Collection { id } => Ok(id.to_string()),
        other => Err(not_a_collection(other, action)),
    }
}

/// Rename a collection row: `collection-service_rename {binding, id, name}`.
///
/// A blank name is refused here (the kernel would store an unnameable row).
pub fn rename_action(app_id: &str, node: &OutlineNode, name: &str) -> Result<ForwardCall, Refusal> {
    let id = collection_id(node, "rename")?;
    let name = name.trim();
    if name.is_empty() {
        return Err(Refusal::invalid_argument(
            "a collection needs a name; the new name is empty",
        ));
    }
    Ok(ForwardCall::new(
        "collection-service_rename",
        json!({ "binding": collection_binding(app_id), "id": id, "name": name }),
    ))
}

/// Delete a collection row: `collection-service_delete {binding, id}`.
/// Members are never deleted — that is the kernel's guarantee, not ours.
pub fn delete_action(app_id: &str, node: &OutlineNode) -> Result<ForwardCall, Refusal> {
    let id = collection_id(node, "delete")?;
    Ok(ForwardCall::new(
        "collection-service_delete",
        json!({ "binding": collection_binding(app_id), "id": id }),
    ))
}

/// Move a collection row onto `to`: `collection-service_reparent {binding,
/// id, new_parent_id}`.
///
/// `to` is the row the dragged rows were dropped on; `None` is the root. A
/// drop on a collection nests under it; a drop on a LIBRARY is a move to
/// that library's root (`new_parent_id: null`, SB-D4). Any other target (a
/// section, a feed, a form) is refused: nothing there can hold a
/// collection.
///
/// `index` is accepted and ignored — the kernel orders siblings by
/// `reorder`, a separate verb this one does not call (see [`move_note`]).
pub fn move_action(
    app_id: &str,
    node: &OutlineNode,
    to: Option<&OutlineNode>,
    index: i64,
) -> Result<ForwardCall, Refusal> {
    let _ = index;
    let id = collection_id(node, "move")?;
    let new_parent_id = match to {
        None | Some(OutlineNode::Library { .. }) => Value::Null,
        Some(OutlineNode::Collection { id }) => Value::String(id.to_string()),
        Some(other) => {
            return Err(Refusal::invalid_argument(format!(
                "a collection cannot be dropped on {}: only a collection or a library can \
                 hold one",
                describe(other)
            )))
        }
    };
    Ok(ForwardCall::new(
        "collection-service_reparent",
        json!({
            "binding": collection_binding(app_id),
            "id": id,
            "new_parent_id": new_parent_id,
        }),
    ))
}

/// The note a move's result carries when the caller named a position the
/// kernel's `reparent` does not take.
pub fn move_note(index: Option<i64>) -> Option<String> {
    index.filter(|i| *i != 0).map(|i| {
        format!(
            "index {i} was ignored: reparent does not position the collection among its \
             siblings (collection-service_reorder does)"
        )
    })
}

/// Create a collection under the row the menu was opened on:
/// `collection-service_create {binding, name, parent_id, kind_scope: null}`.
///
/// * a collection row → a child of it;
/// * `None` → a root collection;
/// * a library row → a root collection (`parent_id: null`): the kernel's
///   `create` takes a parent COLLECTION only and files a root by its
///   binding, so it cannot name the library ([`create_note`] says so);
/// * any other row refuses.
///
/// A missing or blank `name` becomes [`DEFAULT_COLLECTION_NAME`].
pub fn create_action(
    app_id: &str,
    parent: Option<&OutlineNode>,
    name: Option<&str>,
) -> Result<ForwardCall, Refusal> {
    let parent_id = match parent {
        None | Some(OutlineNode::Library { .. }) => Value::Null,
        Some(OutlineNode::Collection { id }) => Value::String(id.to_string()),
        Some(other) => {
            return Err(Refusal::invalid_argument(format!(
                "a collection cannot be created under {}: only a collection or a library \
                 can hold one",
                describe(other)
            )))
        }
    };
    let name = name
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(DEFAULT_COLLECTION_NAME);
    Ok(ForwardCall::new(
        "collection-service_create",
        json!({
            "binding": collection_binding(app_id),
            "name": name,
            "parent_id": parent_id,
            "kind_scope": Value::Null,
        }),
    ))
}

/// The note a create's result carries when the row it was asked under is a
/// library: the kernel cannot file a collection under a library, only by
/// its binding.
pub fn create_note(parent: Option<&OutlineNode>) -> Option<String> {
    matches!(parent, Some(OutlineNode::Library { .. })).then(|| {
        "created as a root collection: the kernel files it by binding, not under the \
         library the menu was opened on"
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn collection(n: u128) -> OutlineNode {
        OutlineNode::Collection { id: id(n) }
    }

    fn library(n: u128) -> OutlineNode {
        OutlineNode::Library { id: id(n) }
    }

    fn section(name: &str) -> OutlineNode {
        OutlineNode::Section {
            section: name.into(),
        }
    }

    #[test]
    fn each_app_names_its_collection_binding() {
        assert_eq!(collection_binding("imbib"), "imbib");
        assert_eq!(collection_binding("impress"), "imbib");
        assert_eq!(collection_binding("imprint"), "manuscript");
        assert_eq!(collection_binding("implore"), "figure");
        assert_eq!(collection_binding("impel"), "generic");
        assert_eq!(collection_binding("anything-else"), "generic");
    }

    #[test]
    fn rename_forwards_the_trimmed_name() {
        let call = rename_action("imbib", &collection(7), "  Dark matter ").unwrap();
        assert_eq!(call.verb, "collection-service_rename");
        assert_eq!(
            call.args,
            json!({"binding": "imbib", "id": id(7).to_string(), "name": "Dark matter"})
        );
    }

    #[test]
    fn rename_uses_the_apps_binding() {
        let call = rename_action("imprint", &collection(7), "Drafts").unwrap();
        assert_eq!(call.args["binding"], "manuscript");
    }

    #[test]
    fn rename_refuses_a_blank_name() {
        let refusal = rename_action("imbib", &collection(7), "   ").unwrap_err();
        assert_eq!(refusal.code, "invalid-argument");
        assert!(refusal.message.contains("name"), "{}", refusal.message);
    }

    #[test]
    fn rename_refuses_a_library_and_says_the_legacy_sidebar_does_it() {
        let refusal = rename_action("imbib", &library(1), "Lib").unwrap_err();
        assert_eq!(refusal.code, "invalid-argument");
        assert!(
            refusal.message.contains("library")
                && refusal.message.contains("chassis sidebar for now"),
            "{}",
            refusal.message
        );
    }

    #[test]
    fn rename_refuses_a_section_naming_it() {
        let refusal = rename_action("imbib", &section("inbox"), "x").unwrap_err();
        assert_eq!(refusal.code, "invalid-argument");
        assert!(
            refusal.message.contains("'inbox' section"),
            "{}",
            refusal.message
        );
        assert!(
            refusal.message.contains("legacy sidebar"),
            "{}",
            refusal.message
        );
    }

    #[test]
    fn delete_forwards_the_id() {
        let call = delete_action("implore", &collection(9)).unwrap();
        assert_eq!(call.verb, "collection-service_delete");
        assert_eq!(
            call.args,
            json!({"binding": "figure", "id": id(9).to_string()})
        );
    }

    #[test]
    fn delete_refuses_anything_but_a_collection() {
        for node in [
            library(1),
            section("inbox"),
            OutlineNode::Recent,
            OutlineNode::SmartSearch {
                id: id(2),
                section: "inbox".into(),
            },
        ] {
            let refusal = delete_action("imbib", &node).unwrap_err();
            assert_eq!(refusal.code, "invalid-argument", "{node:?}");
            assert!(
                refusal.message.contains("not a collection"),
                "{}",
                refusal.message
            );
        }
    }

    #[test]
    fn a_drop_on_a_collection_nests_under_it() {
        let to = collection(2);
        let call = move_action("imbib", &collection(1), Some(&to), 0).unwrap();
        assert_eq!(call.verb, "collection-service_reparent");
        assert_eq!(
            call.args,
            json!({
                "binding": "imbib",
                "id": id(1).to_string(),
                "new_parent_id": id(2).to_string(),
            })
        );
    }

    #[test]
    fn a_drop_on_a_library_is_a_move_to_that_librarys_root() {
        let to = library(5);
        let call = move_action("imbib", &collection(1), Some(&to), 3).unwrap();
        assert_eq!(call.args["new_parent_id"], Value::Null);
        assert_eq!(call.args["id"], id(1).to_string());
    }

    #[test]
    fn a_drop_on_the_root_is_a_move_to_the_root() {
        let call = move_action("imbib", &collection(1), None, 0).unwrap();
        assert_eq!(call.args["new_parent_id"], Value::Null);
    }

    #[test]
    fn a_drop_on_a_section_is_refused() {
        let to = section("inbox");
        let refusal = move_action("imbib", &collection(1), Some(&to), 0).unwrap_err();
        assert_eq!(refusal.code, "invalid-argument");
        assert!(
            refusal.message.contains("'inbox' section"),
            "{}",
            refusal.message
        );
    }

    #[test]
    fn moving_a_library_is_refused_before_the_target_is_looked_at() {
        let to = collection(2);
        let refusal = move_action("imbib", &library(1), Some(&to), 0).unwrap_err();
        assert_eq!(refusal.code, "invalid-argument");
        assert!(refusal.message.contains("library"), "{}", refusal.message);
    }

    #[test]
    fn a_move_names_an_ignored_index_and_only_then() {
        assert!(move_note(None).is_none());
        assert!(move_note(Some(0)).is_none());
        assert!(move_note(Some(4)).unwrap().contains("index 4"));
    }

    #[test]
    fn create_under_a_collection_makes_a_child() {
        let parent = collection(3);
        let call = create_action("imbib", Some(&parent), Some("Papers")).unwrap();
        assert_eq!(call.verb, "collection-service_create");
        assert_eq!(
            call.args,
            json!({
                "binding": "imbib",
                "name": "Papers",
                "parent_id": id(3).to_string(),
                "kind_scope": null,
            })
        );
        assert!(create_note(Some(&parent)).is_none());
    }

    #[test]
    fn create_without_a_name_uses_the_default() {
        let call = create_action("imbib", None, None).unwrap();
        assert_eq!(call.args["name"], DEFAULT_COLLECTION_NAME);
        assert_eq!(call.args["parent_id"], Value::Null);
        let blank = create_action("imbib", None, Some("  ")).unwrap();
        assert_eq!(blank.args["name"], DEFAULT_COLLECTION_NAME);
    }

    #[test]
    fn create_on_a_library_is_a_root_collection_and_says_so() {
        let parent = library(4);
        let call = create_action("imbib", Some(&parent), None).unwrap();
        assert_eq!(call.args["parent_id"], Value::Null);
        assert!(create_note(Some(&parent))
            .unwrap()
            .contains("root collection"));
    }

    #[test]
    fn create_on_a_section_or_other_row_is_refused() {
        for parent in [
            section("inbox"),
            OutlineNode::Recent,
            OutlineNode::SearchForm { form: "ads".into() },
        ] {
            let refusal = create_action("imbib", Some(&parent), None).unwrap_err();
            assert_eq!(refusal.code, "invalid-argument", "{parent:?}");
        }
    }
}
