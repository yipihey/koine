//! The generated settings pane: one `SurfaceSpec` per section (ADR-0036 D5,
//! the shape ADR-0035 D2 gives a generated form).
//!
//! A settings pane is not a Swift view. It is a surface document — the same
//! kind an agent authors — whose fields are typed from the registry and whose
//! only action is `settings-service_set` on change; the Swift renderer maps it
//! to SwiftUI with no logic of its own (ADR-0033 D2). A `toggle` for a bool, a
//! `select` of the declared choice labels, a `number` or `text` field
//! otherwise, each bound to a `state` path that seeds from the current value.
//!
//! The spec carries the values it was generated from in `state`, so it is a
//! snapshot: the host regenerates it (and resets the state row) when it
//! opens the pane, and the pane's own edits flow `change → state → set`.

use serde_json::{json, Map, Value};

use impress_surface::spec::{Action, FieldKind, Node, NodeKind, SurfaceSpec};

use crate::registry::{section, SettingType};
use crate::store::Resolved;

/// The `state` key a setting's field binds to: the key with dots as
/// underscores (`imbib_retention_inbox_days`), so the bind path stays one
/// segment and a dotted key is not read as a nested path.
pub fn surface_state_key(key: &str) -> String {
    key.replace('.', "_")
}

/// The setting's value as its field shows it: the choice label when it has
/// choices, the value itself otherwise.
fn field_value(setting: &Resolved) -> Value {
    match setting.def.choice_label(&setting.value) {
        Some(label) => Value::String(label.into()),
        None => setting.value.clone(),
    }
}

fn field_kind(setting: &Resolved) -> FieldKind {
    if !setting.def.choices.is_empty() {
        let options: Vec<Value> = setting
            .def
            .choices
            .iter()
            .map(|choice| Value::String(choice.label.clone()))
            .collect();
        return FieldKind::Select(json!({ "options": options }));
    }
    match setting.def.ty {
        SettingType::Bool => FieldKind::Toggle(json!({})),
        SettingType::Integer | SettingType::Number => FieldKind::Number(json!({})),
        SettingType::String => FieldKind::Text(json!({})),
    }
}

/// The surface for `section_id` over `settings` (the section's current values,
/// as `SettingsStore::list(Some(section))` answers them). `None` for a section
/// the registry does not declare.
pub fn section_surface(section_id: &str, settings: &[Resolved]) -> Option<SurfaceSpec> {
    let section = section(section_id)?;
    let mut state = Map::new();
    let mut items = vec![
        Node::leaf(NodeKind::Text(format!("# {}", section.title))),
        Node::leaf(NodeKind::Text(section.doc.clone())),
    ];
    for setting in settings
        .iter()
        .filter(|s| s.def.section == section_id && !s.def.internal)
    {
        let state_key = surface_state_key(&setting.def.key);
        state.insert(state_key.clone(), field_value(setting));
        items.push(
            Node::leaf(NodeKind::Field(field_kind(setting)))
                .with_id(setting.def.key.clone())
                .with_label(setting.def.label.clone())
                .with_help(setting.def.doc.clone())
                .with_bind(format!("state.{state_key}"))
                .with_on_change(vec![Action::Call {
                    verb: "settings-service_set".into(),
                    args: json!({
                        "key": setting.def.key,
                        "value": format!("{{{{state.{state_key}}}}}"),
                    }),
                    into: None,
                    each: None,
                }]),
        );
    }
    Some(SurfaceSpec {
        surface: "1.0".into(),
        name: format!("{} settings", section.title),
        params: Vec::new(),
        state: Value::Object(state),
        sources: Default::default(),
        root: Node::leaf(NodeKind::Column(items)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SettingsStore;
    use impress_surface::validate;

    #[test]
    fn retention_surface_validates_and_binds_every_setting() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::open(dir.path());
        store
            .set("imbib.retention.auto_remove_read", &Value::Bool(true))
            .unwrap();
        let settings = store.list(Some("imbib.retention")).unwrap();
        let spec = section_surface("imbib.retention", &settings).unwrap();
        assert_eq!(
            settings.len(),
            4,
            "registry keeps the internal value addressable"
        );
        assert!(
            spec.state
                .get("imbib_internal_exploration_library_id")
                .is_none(),
            "internal bookkeeping is not seeded into a generated field"
        );
        let serialized = serde_json::to_string(&spec).unwrap();
        assert!(
            !serialized.contains("imbib.internal.exploration_library_id"),
            "internal bookkeeping must not render as a settings field"
        );
        let problems = validate(&spec);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(spec.name, "Retention settings");
        assert_eq!(
            spec.state["imbib_retention_inbox_days"],
            Value::String("1 Month".into()),
            "a choice setting seeds its label"
        );
        assert_eq!(
            spec.state["imbib_retention_auto_remove_read"],
            Value::Bool(true)
        );
        let text = serde_json::to_string(&spec).unwrap();
        assert!(text.contains("settings-service_set"));
        assert!(text.contains("{{state.imbib_retention_inbox_days}}"));
        let fields = spec
            .root
            .children()
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::Field(_)))
            .count();
        assert_eq!(fields, 3);
        assert!(section_surface("nowhere", &settings).is_none());
    }

    #[test]
    fn automation_surface_types_its_fields_from_the_registry() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::open(dir.path());
        let settings = store.list(Some("imbib.automation")).unwrap();
        let spec = section_surface("imbib.automation", &settings).unwrap();
        assert!(validate(&spec).is_empty());
        let kinds: Vec<&str> = spec
            .root
            .children()
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::Field(FieldKind::Toggle(_)) => Some("toggle"),
                NodeKind::Field(FieldKind::Number(_)) => Some("number"),
                NodeKind::Field(FieldKind::Text(_)) => Some("text"),
                NodeKind::Field(_) => Some("other"),
                _ => None,
            })
            .collect();
        assert_eq!(kinds, ["toggle", "number", "toggle", "toggle", "text"]);
        assert_eq!(spec.state["imbib_automation_http_port"], Value::from(23120));
    }
}
