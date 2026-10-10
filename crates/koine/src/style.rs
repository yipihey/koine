//! The style sheet every koine frontend applies.
//!
//! Slot names are shared. The value kind is the frontend's. A sheet is tagged
//! with that frontend's id, and a frontend applies a sheet only when the id
//! is its own. The rules are `docs/plan-koine-style.md`.
//!
//! Nothing here draws. Swift and `koine-tui` read a [`StyleSheet`] and map it
//! onto their own controls. [`editor_spec`] is a surface an editor can show;
//! the loop that builds one field per slot lives here, not in the spec.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Map, Value};
use thiserror::Error;

use crate::surface::{
    Button, Event, EventKind, FieldKind, Node, NodeKind, SurfaceSpec, SURFACE_VERSION,
};

/// Frontend ids this build knows. A new frontend is a new entry here and a
/// new arm in [`slots`], in the same change as its renderer.
pub const FRONTENDS: &[&str] = &["swift", "tui"];

/// Slot names every frontend declares, in declaration order. A frontend that
/// adds a name, or skips one, is not a koine frontend.
pub const REQUIRED_SLOTS: &[&str] = &[
    "column",
    "row",
    "text",
    "section",
    "button",
    "field",
    "list",
    "table",
    "status.error",
    "status.warning",
    "status.info",
    "focus",
];

pub const COLOR_TOKENS: &[&str] = &[
    "automatic",
    "accent",
    "primary",
    "secondary",
    "error",
    "warning",
    "info",
];

pub const FONT_TOKENS: &[&str] = &["automatic", "body", "title", "mono"];

/// Placeholders a line template may name. `{value}` is the field's value,
/// which the text frontend already prints.
pub const PLACEHOLDERS: &[&str] = &[
    "columns", "label", "level", "message", "text", "title", "value",
];

pub const SPACING_MAX: u32 = 64;

const SWIFT: &[Slot] = &[
    Slot {
        name: "column",
        kind: ValueKind::Spacing,
        default_value: "8",
    },
    Slot {
        name: "row",
        kind: ValueKind::Spacing,
        default_value: "8",
    },
    Slot {
        name: "text",
        kind: ValueKind::Font,
        default_value: "automatic",
    },
    Slot {
        name: "section",
        kind: ValueKind::Font,
        default_value: "automatic",
    },
    Slot {
        name: "button",
        kind: ValueKind::Color,
        default_value: "automatic",
    },
    Slot {
        name: "field",
        kind: ValueKind::Font,
        default_value: "automatic",
    },
    Slot {
        name: "list",
        kind: ValueKind::Spacing,
        default_value: "0",
    },
    Slot {
        name: "table",
        kind: ValueKind::Spacing,
        default_value: "0",
    },
    Slot {
        name: "status.error",
        kind: ValueKind::Color,
        default_value: "error",
    },
    Slot {
        name: "status.warning",
        kind: ValueKind::Color,
        default_value: "warning",
    },
    Slot {
        name: "status.info",
        kind: ValueKind::Color,
        default_value: "info",
    },
    Slot {
        name: "focus",
        kind: ValueKind::Color,
        default_value: "automatic",
    },
];

const TUI: &[Slot] = &[
    Slot {
        name: "column",
        kind: ValueKind::Spacing,
        default_value: "2",
    },
    Slot {
        name: "row",
        kind: ValueKind::Spacing,
        default_value: "2",
    },
    Slot {
        name: "text",
        kind: ValueKind::Line,
        default_value: "text {text}",
    },
    Slot {
        name: "section",
        kind: ValueKind::Line,
        default_value: "section {title}",
    },
    Slot {
        name: "button",
        kind: ValueKind::Line,
        default_value: "button {label}",
    },
    Slot {
        name: "field",
        kind: ValueKind::Line,
        default_value: "field {value}",
    },
    Slot {
        name: "list",
        kind: ValueKind::Line,
        default_value: "list",
    },
    Slot {
        name: "table",
        kind: ValueKind::Line,
        default_value: "table {columns}",
    },
    Slot {
        name: "status.error",
        kind: ValueKind::Line,
        default_value: "status error: {message}",
    },
    Slot {
        name: "status.warning",
        kind: ValueKind::Line,
        default_value: "status warning: {message}",
    },
    Slot {
        name: "status.info",
        kind: ValueKind::Line,
        default_value: "status info: {message}",
    },
    Slot {
        name: "focus",
        kind: ValueKind::Line,
        default_value: ">",
    },
];

/// How a frontend stores one slot. The wire name is [`ValueKind::name`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    Color,
    Spacing,
    Font,
    Line,
}

impl ValueKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Color => "color",
            Self::Spacing => "spacing",
            Self::Font => "font",
            Self::Line => "line",
        }
    }
}

/// One slot a frontend declares: a shared name, that frontend's kind, and the
/// value that reproduces the renderer's historical look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub name: &'static str,
    pub kind: ValueKind,
    pub default_value: &'static str,
}

/// Why a sheet could not be read or written.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum StyleError {
    #[error("unknown frontend '{frontend}'")]
    UnknownFrontend { frontend: String },
    #[error("{frontend} has no slot '{slot}'")]
    UnknownSlot { frontend: String, slot: String },
    #[error("slot '{slot}' is {kind}, and {reason}")]
    InvalidValue {
        slot: String,
        kind: &'static str,
        reason: String,
    },
}

/// The slots `frontend` declares, in [`REQUIRED_SLOTS`] order.
pub fn slots(frontend: &str) -> Result<&'static [Slot], StyleError> {
    match frontend {
        "swift" => Ok(SWIFT),
        "tui" => Ok(TUI),
        _ => Err(StyleError::UnknownFrontend {
            frontend: frontend.to_string(),
        }),
    }
}

/// Values for one frontend. Every required slot is present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyleSheet {
    pub frontend: String,
    values: BTreeMap<String, String>,
}

impl StyleSheet {
    pub fn default_for(frontend: &str) -> Result<Self, StyleError> {
        let mut values = BTreeMap::new();
        for slot in slots(frontend)? {
            values.insert(slot.name.to_string(), slot.default_value.to_string());
        }
        Ok(Self {
            frontend: frontend.to_string(),
            values,
        })
    }

    pub fn value(&self, slot: &str) -> &str {
        if let Some(value) = self.values.get(slot) {
            return value;
        }
        slots(&self.frontend)
            .ok()
            .and_then(|table| table.iter().find(|slot_decl| slot_decl.name == slot))
            .map(|slot_decl| slot_decl.default_value)
            .unwrap_or("")
    }

    pub fn values(&self) -> &BTreeMap<String, String> {
        &self.values
    }

    pub fn set(&mut self, slot: &str, raw: &str) -> Result<(), StyleError> {
        let decl = declaration(&self.frontend, slot)?;
        check_value(slot, decl.kind, raw)?;
        self.values.insert(slot.to_string(), raw.to_string());
        Ok(())
    }

    pub fn reset(&mut self, slot: &str) -> Result<(), StyleError> {
        let decl = declaration(&self.frontend, slot)?;
        self.values
            .insert(slot.to_string(), decl.default_value.to_string());
        Ok(())
    }

    pub fn reset_all(&mut self) {
        if let Ok(fresh) = Self::default_for(&self.frontend) {
            *self = fresh;
        }
    }
}

/// The sheet a running frontend is applying. It starts at the defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyleHost {
    sheet: StyleSheet,
}

impl StyleHost {
    pub fn new(frontend: &str) -> Result<Self, StyleError> {
        Ok(Self {
            sheet: StyleSheet::default_for(frontend)?,
        })
    }

    /// Keep `sheet`'s values where they are valid, and fill anything it omits
    /// from the defaults. An unknown slot or a value of the wrong kind is
    /// refused; the host is not created.
    pub fn from_sheet(sheet: StyleSheet) -> Result<Self, StyleError> {
        let mut fresh = StyleSheet::default_for(&sheet.frontend)?;
        for (slot, value) in &sheet.values {
            fresh.set(slot, value)?;
        }
        Ok(Self { sheet: fresh })
    }

    pub fn frontend(&self) -> &str {
        &self.sheet.frontend
    }

    pub fn slots(&self) -> &'static [Slot] {
        slots(&self.sheet.frontend).unwrap_or(&[])
    }

    pub fn sheet(&self) -> &StyleSheet {
        &self.sheet
    }

    pub fn set(&mut self, slot: &str, raw: &str) -> Result<(), StyleError> {
        self.sheet.set(slot, raw)
    }

    pub fn reset(&mut self, slot: &str) -> Result<(), StyleError> {
        self.sheet.reset(slot)
    }

    pub fn reset_all(&mut self) {
        self.sheet.reset_all();
    }
}

/// Sheets keyed by frontend id. `set` stores a value while no host is open;
/// a host opened afterwards sees it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StyleBook {
    sheets: BTreeMap<String, StyleSheet>,
}

impl StyleBook {
    pub fn new() -> Self {
        Self {
            sheets: BTreeMap::new(),
        }
    }

    pub fn sheet(&self, frontend: &str) -> Option<&StyleSheet> {
        self.sheets.get(frontend)
    }

    pub fn set(&mut self, frontend: &str, slot: &str, raw: &str) -> Result<(), StyleError> {
        self.sheet_mut(frontend)?.set(slot, raw)
    }

    pub fn reset(&mut self, frontend: &str, slot: &str) -> Result<(), StyleError> {
        self.sheet_mut(frontend)?.reset(slot)
    }

    pub fn reset_all(&mut self, frontend: &str) -> Result<(), StyleError> {
        self.sheet_mut(frontend)?.reset_all();
        Ok(())
    }

    /// The host for `frontend`: the stored sheet when one has been set, the
    /// defaults otherwise. Opening does not insert a sheet into the book.
    pub fn open(&self, frontend: &str) -> Result<StyleHost, StyleError> {
        match self.sheets.get(frontend) {
            Some(sheet) => StyleHost::from_sheet(sheet.clone()),
            None => StyleHost::new(frontend),
        }
    }

    fn sheet_mut(&mut self, frontend: &str) -> Result<&mut StyleSheet, StyleError> {
        if !self.sheets.contains_key(frontend) {
            self.sheets
                .insert(frontend.to_string(), StyleSheet::default_for(frontend)?);
        }
        Ok(self
            .sheets
            .get_mut(frontend)
            .expect("the sheet was just inserted"))
    }
}

/// A surface with one text field per slot and a Reset button beside it.
/// The field's id is the slot name. The button's id is `reset:<slot>`.
/// [`editor_state`] is the state argument that shows the current sheet.
pub fn editor_spec(frontend: &str) -> Result<SurfaceSpec, StyleError> {
    let mut rows = Vec::new();
    for slot in slots(frontend)? {
        let field = Node::leaf(NodeKind::Field(FieldKind::Text(serde_json::json!({}))))
            .with_id(slot.name)
            .with_label(format!("{} ({})", slot.name, slot.kind.name()))
            .with_bind(format!("state.{}", slot.name));
        let reset = Node::leaf(NodeKind::Button(Button {
            label: "Reset".to_string(),
            on_click: Vec::new(),
        }))
        .with_id(format!("reset:{}", slot.name));
        rows.push(Node::leaf(NodeKind::Row(vec![field, reset])));
    }
    let defaults = StyleSheet::default_for(frontend)?;
    Ok(SurfaceSpec {
        surface: SURFACE_VERSION.to_string(),
        name: format!("{frontend} style"),
        params: Vec::new(),
        state: editor_state(&defaults),
        sources: BTreeMap::new(),
        root: Node::leaf(NodeKind::Column(rows)).with_id("style"),
    })
}

/// State that resolves [`editor_spec`] to the values in `sheet`. Dotted slot
/// names (`status.error`) are nested objects, which is how a `state.` bind
/// walks them.
pub fn editor_state(sheet: &StyleSheet) -> Value {
    let mut root = Map::new();
    for (slot, value) in sheet.values() {
        insert_dotted(&mut root, slot, Value::String(value.clone()));
    }
    Value::Object(root)
}

/// Apply one editor event. A `change` on a slot sets it. A `click` on
/// `reset:<slot>` resets it. Any other event leaves the sheet and returns
/// `Ok(false)`. A value of the wrong kind is [`StyleError`] and leaves the
/// previous value.
pub fn editor_apply(host: &mut StyleHost, event: &Event) -> Result<bool, StyleError> {
    match event.kind {
        EventKind::Change => {
            if host.slots().iter().all(|slot| slot.name != event.widget) {
                return Ok(false);
            }
            let Value::String(raw) = &event.value else {
                return Err(invalid(
                    &event.widget,
                    declaration(host.frontend(), &event.widget)?.kind,
                    "a change value must be a string",
                ));
            };
            host.set(&event.widget, raw)?;
            Ok(true)
        }
        EventKind::Click => {
            let Some(slot) = event.widget.strip_prefix("reset:") else {
                return Ok(false);
            };
            host.reset(slot)?;
            Ok(true)
        }
        EventKind::Select | EventKind::Submit => Ok(false),
    }
}

/// The vocabulary file Swift pins itself to. Pretty-print this and compare
/// it with `tests/fixtures/style-vocabulary.json`.
pub fn vocabulary_json() -> Value {
    serde_json::to_value(Vocabulary::from_tables()).expect("style vocabulary serializes")
}

fn declaration<'a>(frontend: &str, slot: &str) -> Result<&'a Slot, StyleError> {
    slots(frontend)?
        .iter()
        .find(|decl| decl.name == slot)
        .ok_or_else(|| StyleError::UnknownSlot {
            frontend: frontend.to_string(),
            slot: slot.to_string(),
        })
}

fn check_value(slot: &str, kind: ValueKind, raw: &str) -> Result<(), StyleError> {
    match kind {
        ValueKind::Color => {
            if !COLOR_TOKENS.contains(&raw) {
                return Err(invalid(slot, kind, format!("'{raw}' is not a color token")));
            }
        }
        ValueKind::Font => {
            if !FONT_TOKENS.contains(&raw) {
                return Err(invalid(slot, kind, format!("'{raw}' is not a font token")));
            }
        }
        ValueKind::Spacing => {
            let canonical = raw.len() == 1 || !raw.starts_with('0');
            let parsed = raw.parse::<u32>().ok();
            if !canonical || parsed.is_none_or(|n| n > SPACING_MAX) {
                return Err(invalid(
                    slot,
                    kind,
                    format!("'{raw}' is not an integer from 0 to {SPACING_MAX}"),
                ));
            }
        }
        ValueKind::Line => check_line(slot, raw)?,
    }
    Ok(())
}

fn check_line(slot: &str, raw: &str) -> Result<(), StyleError> {
    if raw.trim().is_empty() {
        return Err(invalid(slot, ValueKind::Line, "it is empty"));
    }
    let mut rest = raw;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            return Err(invalid(slot, ValueKind::Line, "it has an unclosed '{'"));
        };
        let name = &after[..end];
        if !PLACEHOLDERS.contains(&name) {
            return Err(invalid(
                slot,
                ValueKind::Line,
                format!("'{{{name}}}' names an unknown placeholder"),
            ));
        }
        rest = &after[end + 1..];
    }
    Ok(())
}

fn invalid(slot: &str, kind: ValueKind, reason: impl Into<String>) -> StyleError {
    StyleError::InvalidValue {
        slot: slot.to_string(),
        kind: kind.name(),
        reason: reason.into(),
    }
}

fn insert_dotted(root: &mut Map<String, Value>, slot: &str, value: Value) {
    let Some((head, tail)) = slot.split_once('.') else {
        root.insert(slot.to_string(), value);
        return;
    };
    let entry = root
        .entry(head.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if let Value::Object(map) = entry {
        insert_dotted(map, tail, value);
    }
}

#[derive(Serialize)]
struct Vocabulary {
    wire_version: u32,
    frontends: BTreeMap<&'static str, FrontendVocabulary>,
    placeholders: &'static [&'static str],
    spacing: SpacingBounds,
    tokens: BTreeMap<&'static str, &'static [&'static str]>,
}

#[derive(Serialize)]
struct FrontendVocabulary {
    slots: Vec<SlotRecord>,
}

#[derive(Serialize)]
struct SlotRecord {
    name: &'static str,
    kind: &'static str,
    #[serde(rename = "default")]
    default_value: &'static str,
}

#[derive(Serialize)]
struct SpacingBounds {
    min: u32,
    max: u32,
}

impl Vocabulary {
    fn from_tables() -> Self {
        let mut frontends = BTreeMap::new();
        for frontend in FRONTENDS {
            let slots = slots(frontend)
                .expect("FRONTENDS are declared")
                .iter()
                .map(|slot| SlotRecord {
                    name: slot.name,
                    kind: slot.kind.name(),
                    default_value: slot.default_value,
                })
                .collect();
            frontends.insert(*frontend, FrontendVocabulary { slots });
        }
        let mut tokens = BTreeMap::new();
        tokens.insert("color", COLOR_TOKENS);
        tokens.insert("font", FONT_TOKENS);
        Self {
            wire_version: 1,
            frontends,
            placeholders: PLACEHOLDERS,
            spacing: SpacingBounds {
                min: 0,
                max: SPACING_MAX,
            },
            tokens,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::{resolve, validate, RenderKind, RenderNode};

    #[test]
    fn every_frontend_names_exactly_the_required_slots() {
        for frontend in FRONTENDS {
            let names: Vec<&str> = slots(frontend)
                .unwrap()
                .iter()
                .map(|slot| slot.name)
                .collect();
            assert_eq!(names, REQUIRED_SLOTS);
        }
    }

    #[test]
    fn the_same_slot_has_a_different_kind_on_each_frontend() {
        let swift_button = slots("swift")
            .unwrap()
            .iter()
            .find(|slot| slot.name == "button")
            .unwrap();
        let tui_button = slots("tui")
            .unwrap()
            .iter()
            .find(|slot| slot.name == "button")
            .unwrap();
        assert_eq!(swift_button.kind, ValueKind::Color);
        assert_eq!(tui_button.kind, ValueKind::Line);
        assert_eq!(swift_button.default_value, "automatic");
        assert_eq!(tui_button.default_value, "button {label}");
    }

    #[test]
    fn set_refuses_an_unknown_slot_and_a_value_of_the_wrong_kind() {
        let mut swift = StyleHost::new("swift").unwrap();
        assert!(matches!(
            swift.set("grid", "8"),
            Err(StyleError::UnknownSlot { .. })
        ));
        assert!(matches!(
            swift.set("button", "button {label}"),
            Err(StyleError::InvalidValue { .. })
        ));
        assert_eq!(swift.sheet().value("button"), "automatic");

        let mut tui = StyleHost::new("tui").unwrap();
        tui.set("button", "[{label}]").unwrap();
        assert!(matches!(
            tui.set("column", "wide"),
            Err(StyleError::InvalidValue { .. })
        ));
        assert!(matches!(
            tui.set("text", "{nope}"),
            Err(StyleError::InvalidValue { .. })
        ));
        assert_eq!(tui.sheet().value("column"), "2");
        tui.reset("button").unwrap();
        assert_eq!(tui.sheet().value("button"), "button {label}");
    }

    #[test]
    fn a_book_stores_a_sheet_while_no_host_is_open() {
        let mut book = StyleBook::new();
        assert!(book.sheet("tui").is_none());
        book.set("tui", "focus", "*").unwrap();
        let host = book.open("tui").unwrap();
        assert_eq!(host.sheet().value("focus"), "*");
        assert!(book.open("web").is_err());
    }

    #[test]
    fn the_editor_spec_validates_and_a_change_sets_the_slot() {
        for frontend in FRONTENDS {
            let spec = editor_spec(frontend).unwrap();
            let problems = validate(&spec);
            assert!(problems.is_empty(), "{frontend}: {problems:?}");
        }

        let mut host = StyleHost::new("swift").unwrap();
        let spec = editor_spec("swift").unwrap();
        let tree = resolve(
            &spec,
            &editor_state(host.sheet()),
            &serde_json::json!({}),
            &serde_json::json!({}),
        );
        let button = find(&tree.root, "button").expect("button field");
        match &button.node {
            RenderKind::Field { value, .. } => {
                assert_eq!(value, &serde_json::json!("automatic"));
            }
            other => panic!("button slot resolved as {other:?}"),
        }

        editor_apply(
            &mut host,
            &Event {
                widget: "button".into(),
                kind: EventKind::Change,
                value: serde_json::json!("accent"),
            },
        )
        .unwrap();
        assert_eq!(host.sheet().value("button"), "accent");
        editor_apply(
            &mut host,
            &Event {
                widget: "reset:button".into(),
                kind: EventKind::Click,
                value: Value::Null,
            },
        )
        .unwrap();
        assert_eq!(host.sheet().value("button"), "automatic");
    }

    #[test]
    fn vocabulary_matches_the_committed_fixture() {
        let generated = serde_json::to_string_pretty(&vocabulary_json()).unwrap() + "\n";
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/style-vocabulary.json");
        if std::env::var_os("UPDATE_STYLE_VOCABULARY").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &generated).unwrap();
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            committed, generated,
            "style vocabulary drifted; regenerate with UPDATE_STYLE_VOCABULARY=1"
        );
    }

    fn find<'a>(node: &'a RenderNode, id: &str) -> Option<&'a RenderNode> {
        if node.id == id {
            return Some(node);
        }
        let children: Vec<&RenderNode> = match &node.node {
            RenderKind::Column { items }
            | RenderKind::Row { items }
            | RenderKind::Grid { items, .. } => items.iter().collect(),
            RenderKind::Section { body, .. } => vec![body.as_ref()],
            RenderKind::Tabs { tabs } => tabs.iter().map(|tab| &tab.body).collect(),
            _ => Vec::new(),
        };
        for child in children {
            if let Some(found) = find(child, id) {
                return Some(found);
            }
        }
        None
    }
}
