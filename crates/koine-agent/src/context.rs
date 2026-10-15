//! Pointing is context (plan-koine-research-os G-D1): what the person is
//! looking at and has selected, read from the layout through its own verbs.

use std::collections::BTreeSet;

use impress_service_core::{call, SafetyClass, VerbHandle};
use serde_json::{json, Value};

use crate::dto::{ContextResult, SelectionRow, VerbRow};

/// The layout verb the context reads.
pub const GET_LAYOUT: &str = "layout-service_get-layout";

pub fn safety_word(class: SafetyClass) -> &'static str {
    match class {
        SafetyClass::ReadOnly => "read-only",
        SafetyClass::Mutating => "mutating",
        SafetyClass::Destructive => "destructive",
        SafetyClass::External => "external",
    }
}

/// The first sentence of a description, on one line.
pub fn first_sentence(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.find(". ") {
        Some(end) => flat[..=end].to_string(),
        None => flat,
    }
}

pub fn verb_row(verb: &VerbHandle) -> VerbRow {
    VerbRow {
        name: verb.name().to_string(),
        safety: safety_word(verb.safety().class).to_string(),
        description: first_sentence(verb.description()),
    }
}

fn takes_an_id(verb: &VerbHandle) -> bool {
    let schema = verb.input_schema();
    schema
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|p| p.contains_key("id") || p.contains_key("ids"))
}

/// The verbs that act on `kinds`: those that return one of them, and the
/// verbs of the same services that take `id` or `ids`. Sorted by name.
pub fn verbs_for_kinds(kinds: &BTreeSet<String>) -> Vec<VerbRow> {
    let all: Vec<VerbHandle> = call::descriptors().collect();
    let services: BTreeSet<String> = all
        .iter()
        .filter(|v| v.returns_kind().is_some_and(|k| kinds.contains(k)))
        .map(|v| v.service().to_string())
        .collect();
    let mut rows: Vec<VerbRow> = all
        .iter()
        .filter(|v| {
            v.returns_kind().is_some_and(|k| kinds.contains(k))
                || (services.contains(v.service()) && takes_an_id(v))
        })
        .map(verb_row)
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows.dedup_by(|a, b| a.name == b.name);
    rows
}

/// What every channel of a layout carries.
pub fn selections(layout: &Value) -> Vec<SelectionRow> {
    let channels = &layout["channels"];
    let channels = match channels.get("channels") {
        Some(inner) if inner.is_object() => inner,
        _ => channels,
    };
    let mut rows = Vec::new();
    if let Some(channels) = channels.as_object() {
        for (channel, kinds) in channels {
            if let Some(kinds) = kinds.as_object() {
                for (kind, ids) in kinds {
                    let ids: Vec<String> = ids
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    if !ids.is_empty() {
                        rows.push(SelectionRow {
                            channel: channel.clone(),
                            kind: kind.clone(),
                            ids,
                        });
                    }
                }
            }
        }
    }
    rows
}

/// The focused pane of a `get-layout` answer: view kind, role, query, and
/// the surface it shows when it is a surface pane.
pub fn focused_pane(answer: &Value) -> (Option<u64>, Value) {
    let Some(focused) = answer["focused"].as_u64() else {
        return (None, Value::Null);
    };
    let pane = &answer["layout"]["tiles"][focused.to_string()]["pane"];
    if pane.is_null() {
        return (Some(focused), Value::Null);
    }
    let query = &pane["query"];
    let surface_id = (pane["view_kind"] == "surface")
        .then(|| query["scope"]["id"]["id"].as_str())
        .flatten();
    (
        Some(focused),
        json!({
            "view_kind": pane["view_kind"],
            "role": pane["role"],
            "query": query,
            "surface_id": surface_id,
        }),
    )
}

/// The surface `id` as the person sees it: its name, its spec and the state
/// it holds now (G-D5). `Null` when it cannot be read.
pub async fn viewing(id: &str) -> Value {
    let Ok(surface) =
        call::call_async("impress-surface-service_surface-get", json!({ "id": id })).await
    else {
        return Value::Null;
    };
    if surface["ok"] != json!(true) {
        return json!({ "surface_id": id, "error": surface["message"] });
    }
    let state = call::call_async(
        "impress-surface-service_surface-state-get",
        json!({ "id": id }),
    )
    .await
    .ok()
    .filter(|s| s["ok"] == json!(true))
    .map(|s| s["state"].clone())
    .unwrap_or(Value::Null);
    json!({
        "surface_id": id,
        "name": surface["name"],
        "spec": surface["spec"],
        "state": state,
    })
}

/// What `app_id`'s person is pointing at on `device`.
pub async fn pointing(app_id: &str, device: Option<String>) -> ContextResult {
    let mut args = json!({ "app_id": app_id });
    if let Some(device) = device.filter(|d| !d.trim().is_empty()) {
        args["device"] = json!(device);
    }
    let answer = match call::call_async(GET_LAYOUT, args).await {
        Ok(answer) => answer,
        Err(error) => {
            return ContextResult {
                ok: true,
                app_id: app_id.to_string(),
                message: Some(format!("no layout to read: {error}")),
                ..Default::default()
            }
        }
    };
    if answer["ok"] == json!(false) {
        return ContextResult {
            ok: true,
            app_id: app_id.to_string(),
            message: Some(format!(
                "no layout to read: {}",
                answer["message"].as_str().unwrap_or("refused")
            )),
            ..Default::default()
        };
    }
    let (focused, pane) = focused_pane(&answer);
    let selection = selections(&answer["layout"]);
    let mut kinds: BTreeSet<String> = selection.iter().map(|s| s.kind.clone()).collect();
    if let Some(query_kinds) = pane["query"]["kinds"].as_array() {
        kinds.extend(
            query_kinds
                .iter()
                .filter_map(|k| k.as_str())
                .filter(|k| *k != "surface")
                .map(str::to_string),
        );
    }
    ContextResult {
        ok: true,
        app_id: app_id.to_string(),
        focused_pane: focused,
        pane,
        selection,
        verbs: verbs_for_kinds(&kinds),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_focused_surface_pane_names_its_surface() {
        let answer = json!({
            "focused": 2,
            "layout": {
                "channels": {"1": {"note": ["n1", "n2"]}, "2": {}},
                "tiles": {"2": {"pane": {"view_kind": "surface", "role": "list",
                    "query": {"kinds": ["surface"], "scope": {"scope": "item",
                        "id": {"ref": "id", "id": "s-1"}}}}}}
            }
        });
        let (focused, pane) = focused_pane(&answer);
        assert_eq!(focused, Some(2));
        assert_eq!(pane["surface_id"], "s-1");
        assert_eq!(pane["role"], "list");
        let rows = selections(&answer["layout"]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, "note");
        assert_eq!(rows[0].ids, ["n1", "n2"]);
    }

    #[test]
    fn nested_channel_state_is_read_too() {
        let layout = json!({"channels": {"channels": {"3": {"paper": ["p"]}}}});
        let rows = selections(&layout);
        assert_eq!(rows[0].channel, "3");
    }
}
