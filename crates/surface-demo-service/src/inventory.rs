//! The map of this app (plan-koine-tour T-D2): the linked verbs, the record
//! kinds they read, write and return, and the surfaces in the store, as the
//! `nodes` and `edges` a `graph` node draws.
//!
//! The shapes are the ones `impress_surface::resolve` reads with the graph's
//! default field names: a node is `{id, label, …}`, an edge `{source,
//! target, …}` (ids as text). Extra fields (`kind`, `relation`) ride along
//! for an agent reading the result directly.
//!
//! Node ids: a verb is its qualified name (`surface-demo-service_tick`), a
//! kind is `kind:<ref>` and a surface `surface:<uuid>`, so the three never
//! collide. Edges: verb → kind for each `effects` read/write that names a
//! ref (`target(arg)`, `children(arg)` and `any(…)` name no kind and draw
//! nothing; `prefix("p")` is the kind `kind:p*`), verb → kind for a
//! `returns_kind`, and surface → verb for every verb a stored surface's
//! sources or `call` actions name, when that verb is linked here.

use std::collections::BTreeSet;

use impress_item::schema::refs;
use impress_item::{ItemQuery, Value as ItemValue};
use impress_service_core::{Kind, VerbDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One node of the map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InventoryNode {
    /// `<verb name>`, `kind:<ref>` or `surface:<uuid>`.
    pub id: String,
    /// What the drawing shows: a verb's method, a kind's ref, a surface's name.
    pub label: String,
    /// `verb`, `kind` or `surface`.
    pub kind: String,
}

/// One edge of the map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InventoryEdge {
    pub source: String,
    pub target: String,
    /// `reads`, `writes`, `returns` or `calls`.
    pub relation: String,
}

/// The nodes and edges, built from the inventory and the store.
pub fn build() -> (Vec<InventoryNode>, Vec<InventoryEdge>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut kinds: BTreeSet<String> = BTreeSet::new();
    let mut verbs: BTreeSet<String> = BTreeSet::new();

    fn kind_edge(
        verb: &str,
        kind: &Kind,
        relation: &str,
        kinds: &mut BTreeSet<String>,
        edges: &mut Vec<InventoryEdge>,
    ) {
        let name = match kind {
            Kind::Ref(r) => r.to_string(),
            Kind::Prefix(p) => format!("{p}*"),
            Kind::Target(_) | Kind::Children(_) | Kind::Any(_) => return,
        };
        kinds.insert(name.clone());
        edges.push(InventoryEdge {
            source: verb.to_string(),
            target: format!("kind:{name}"),
            relation: relation.to_string(),
        });
    }

    let mut descriptors: Vec<&VerbDescriptor> = VerbDescriptor::iter().collect();
    descriptors.sort_by_key(|v| v.name);
    for verb in descriptors {
        verbs.insert(verb.name.to_string());
        nodes.push(InventoryNode {
            id: verb.name.to_string(),
            label: verb.method.to_string(),
            kind: "verb".to_string(),
        });
        for kind in verb.effects.reads {
            kind_edge(verb.name, kind, "reads", &mut kinds, &mut edges);
        }
        for kind in verb.effects.writes {
            kind_edge(verb.name, kind, "writes", &mut kinds, &mut edges);
        }
        if let Some(returned) = verb.returns_kind {
            kinds.insert(returned.to_string());
            edges.push(InventoryEdge {
                source: verb.name.to_string(),
                target: format!("kind:{returned}"),
                relation: "returns".to_string(),
            });
        }
    }
    for kind in &kinds {
        nodes.push(InventoryNode {
            id: format!("kind:{kind}"),
            label: kind.clone(),
            kind: "kind".to_string(),
        });
    }

    for (id, name, spec) in stored_surfaces() {
        nodes.push(InventoryNode {
            id: format!("surface:{id}"),
            label: name,
            kind: "surface".to_string(),
        });
        for verb in verbs_named_by(&spec) {
            if verbs.contains(&verb) {
                edges.push(InventoryEdge {
                    source: format!("surface:{id}"),
                    target: verb,
                    relation: "calls".to_string(),
                });
            }
        }
    }

    (nodes, edges)
}

/// Every stored surface: `(id, name, spec)`. Empty when no store is
/// installed or the rows do not read.
fn stored_surfaces() -> Vec<(String, String, Value)> {
    let Some(store) = koine_store::process::current() else {
        return Vec::new();
    };
    let items = match store.query(&ItemQuery {
        schema: Some(refs::IMPRESS_UI_SURFACE),
        ..Default::default()
    }) {
        Ok(items) => items,
        Err(e) => {
            tracing::warn!(error = %e, "inventory: surfaces not read");
            return Vec::new();
        }
    };
    items
        .iter()
        .map(|item| {
            let name = match item.payload.get("name") {
                Some(ItemValue::String(s)) => s.clone(),
                _ => item.id.to_string(),
            };
            let spec = match item.payload.get("spec") {
                Some(ItemValue::String(text)) => serde_json::from_str(text).unwrap_or(Value::Null),
                Some(other) => serde_json::to_value(other).unwrap_or(Value::Null),
                None => Value::Null,
            };
            (item.id.to_string(), name, spec)
        })
        .collect()
}

/// The verbs a raw spec names: each `sources.<name>.verb` and every
/// `{"call": {"verb": …}}` anywhere in the document, deduplicated.
pub fn verbs_named_by(spec: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if let Some(sources) = spec.get("sources").and_then(Value::as_object) {
        for source in sources.values() {
            if let Some(verb) = source.get("verb").and_then(Value::as_str) {
                out.insert(verb.to_string());
            }
        }
    }
    fn walk(value: &Value, out: &mut BTreeSet<String>) {
        match value {
            Value::Object(map) => {
                if let Some(verb) = map
                    .get("call")
                    .and_then(|c| c.get("verb"))
                    .and_then(Value::as_str)
                {
                    out.insert(verb.to_string());
                }
                map.values().for_each(|v| walk(v, out));
            }
            Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    if let Some(root) = spec.get("root") {
        walk(root, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn verbs_named_by_reads_sources_and_calls() {
        let spec = json!({
            "sources": { "s": { "verb": "a-service_one", "args": {} }, "v": { "value": 1 } },
            "root": { "column": [
                { "button": { "label": "x", "on_click": [
                    { "call": { "verb": "a-service_two", "args": {} } },
                    { "set": { "path": "state.x", "value": 1 } }
                ] } },
                { "tree": { "rows": "{{source.s}}", "actions": [
                    { "label": "m", "on_click": [ { "call": { "verb": "a-service_one" } } ] }
                ] } }
            ] }
        });
        let verbs: Vec<String> = verbs_named_by(&spec).into_iter().collect();
        assert_eq!(verbs, vec!["a-service_one", "a-service_two"]);
    }

    #[test]
    fn the_map_has_this_crates_verbs_and_the_kinds_they_touch() {
        let (nodes, edges) = build();
        let ids: BTreeSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        for verb in ["series", "histogram", "tick", "notes", "inventory"] {
            assert!(ids.contains(format!("surface-demo-service_{verb}").as_str()));
        }
        assert!(ids.contains("kind:note"), "`notes` returns the kind `note`");
        assert!(edges
            .iter()
            .any(|e| e.source == "surface-demo-service_notes"
                && e.target == "kind:note"
                && e.relation == "returns"));
        // Every edge ends on a node.
        for edge in &edges {
            assert!(ids.contains(edge.source.as_str()), "{}", edge.source);
            assert!(ids.contains(edge.target.as_str()), "{}", edge.target);
        }
        // The shapes the graph node reads by default: `id`/`label` and
        // `source`/`target`.
        let node = serde_json::to_value(&nodes[0]).unwrap();
        assert!(node.get("id").is_some() && node.get("label").is_some());
        let edge = serde_json::to_value(&edges[0]).unwrap();
        assert!(edge.get("source").is_some() && edge.get("target").is_some());
    }
}
