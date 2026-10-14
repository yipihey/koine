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
pub const FRONTENDS: &[&str] = &["swift", "tui", "web"];

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

/// The web frontend (`koine-dioxus`, docs/plan-koine-web.md D3). Its value
/// kinds are the Swift ones — both are widget toolkits, so spacing is a gap,
/// a font is a class and a colour is a token — and every value lands on the
/// page as one CSS custom property, so a changed slot is a style pass, not a
/// render. The defaults are the browser's own look.
const WEB: &[Slot] = &[
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
    /// A value of the wrong kind. The message is a sentence naming the
    /// slot, what it takes and the choices (`button takes a colour token:
    /// automatic, accent, …`), never the value that was refused: quoting it
    /// produced `'"red"'` when the value was itself quoted (A-D6).
    #[error("{slot} takes {expected}")]
    InvalidValue {
        slot: String,
        kind: &'static str,
        expected: String,
    },
}

/// The slots `frontend` declares, in [`REQUIRED_SLOTS`] order.
pub fn slots(frontend: &str) -> Result<&'static [Slot], StyleError> {
    match frontend {
        "swift" => Ok(SWIFT),
        "tui" => Ok(TUI),
        "web" => Ok(WEB),
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

/// A surface with one field per slot and a Reset button beside it. The
/// field's kind follows the slot's (plan-koine-affordances A-D6): a colour
/// or font slot is a `select` over [`COLOR_TOKENS`] or [`FONT_TOKENS`] (the
/// option is the token string, which is what the `change` carries), a
/// spacing slot a `number` from 0 to [`SPACING_MAX`] in steps of 1, and a
/// line slot free text. The field's id is the slot name. The button's id is
/// `reset:<slot>`. [`editor_state`] is the state argument that shows the
/// current sheet.
pub fn editor_spec(frontend: &str) -> Result<SurfaceSpec, StyleError> {
    let mut rows = Vec::new();
    for slot in slots(frontend)? {
        let field = Node::leaf(NodeKind::Field(field_kind(slot.kind)))
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

/// The field a slot of `kind` is edited with (see [`editor_spec`]).
fn field_kind(kind: ValueKind) -> FieldKind {
    match kind {
        ValueKind::Color => FieldKind::Select(serde_json::json!({ "options": COLOR_TOKENS })),
        ValueKind::Font => FieldKind::Select(serde_json::json!({ "options": FONT_TOKENS })),
        ValueKind::Spacing => {
            FieldKind::Number(serde_json::json!({ "min": 0, "max": SPACING_MAX, "step": 1 }))
        }
        ValueKind::Line => FieldKind::Text(serde_json::json!({})),
    }
}

/// State that resolves [`editor_spec`] to the values in `sheet`. Dotted slot
/// names (`status.error`) are nested objects, which is how a `state.` bind
/// walks them. A spacing slot's value is a JSON number, what its `number`
/// field shows and sends; every other value is its string.
pub fn editor_state(sheet: &StyleSheet) -> Value {
    let mut root = Map::new();
    for (slot, value) in sheet.values() {
        let spacing =
            declaration(&sheet.frontend, slot).is_ok_and(|decl| decl.kind == ValueKind::Spacing);
        let shown = match value.parse::<u32>() {
            Ok(n) if spacing => Value::from(n),
            _ => Value::String(value.clone()),
        };
        insert_dotted(&mut root, slot, shown);
    }
    Value::Object(root)
}

/// Apply one editor event. A `change` on a slot sets it. A `click` on
/// `reset:<slot>` resets it. Any other event leaves the sheet and returns
/// `Ok(false)`. A value of the wrong kind is [`StyleError`] and leaves the
/// previous value.
///
/// A spacing slot takes what its `number` field sends, a JSON number, or a
/// numeric string (`" 12 "`, `"12.0"`), stored as the integer's decimal
/// string (`"12"`); every other slot takes a string.
pub fn editor_apply(host: &mut StyleHost, event: &Event) -> Result<bool, StyleError> {
    match event.kind {
        EventKind::Change => {
            let Some(slot) = host.slots().iter().find(|slot| slot.name == event.widget) else {
                return Ok(false);
            };
            let raw = match (slot.kind, &event.value) {
                (ValueKind::Spacing, value) => spacing_of(value)
                    .map(|n| n.to_string())
                    .ok_or_else(|| invalid(slot.name, slot.kind))?,
                (_, Value::String(raw)) => raw.clone(),
                (kind, _) => return Err(invalid(slot.name, kind)),
            };
            host.set(&event.widget, &raw)?;
            Ok(true)
        }
        EventKind::Click => {
            let Some(slot) = event.widget.strip_prefix("reset:") else {
                return Ok(false);
            };
            host.reset(slot)?;
            Ok(true)
        }
        // Row events (plan-koine-outline) are not the style editor's.
        EventKind::Select
        | EventKind::Submit
        | EventKind::Rename
        | EventKind::Delete
        | EventKind::Activate
        | EventKind::Move
        | EventKind::Menu => Ok(false),
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

/// A spacing value as a `number` field or an agent sends it: a whole JSON
/// number, or a string spelling one, from 0 to [`SPACING_MAX`].
fn spacing_of(value: &Value) -> Option<u32> {
    let number = match value {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    let whole = number.is_finite() && number.fract() == 0.0;
    (whole && (0.0..=f64::from(SPACING_MAX)).contains(&number)).then_some(number as u32)
}

fn check_value(slot: &str, kind: ValueKind, raw: &str) -> Result<(), StyleError> {
    let fits = match kind {
        ValueKind::Color => COLOR_TOKENS.contains(&raw),
        ValueKind::Font => FONT_TOKENS.contains(&raw),
        ValueKind::Spacing => {
            let canonical = raw.len() == 1 || !raw.starts_with('0');
            canonical && raw.parse::<u32>().is_ok_and(|n| n <= SPACING_MAX)
        }
        ValueKind::Line => return check_line(slot, raw),
    };
    if fits {
        Ok(())
    } else {
        Err(invalid(slot, kind))
    }
}

/// A line slot's value: not blank, and every `{…}` closed and naming one of
/// [`PLACEHOLDERS`].
fn check_line(slot: &str, raw: &str) -> Result<(), StyleError> {
    if raw.trim().is_empty() {
        return Err(line_invalid(slot, "this one is empty"));
    }
    let mut rest = raw;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            return Err(line_invalid(slot, "this one opens a '{' it never closes"));
        };
        if !PLACEHOLDERS.contains(&&after[..end]) {
            return Err(line_invalid(slot, "this one names another"));
        }
        rest = &after[end + 1..];
    }
    Ok(())
}

/// What a slot of `kind` takes, as the end of a sentence that begins with
/// the slot's name.
fn expected(kind: ValueKind) -> String {
    match kind {
        ValueKind::Color => format!("a colour token: {}", COLOR_TOKENS.join(", ")),
        ValueKind::Font => format!("a font token: {}", FONT_TOKENS.join(", ")),
        ValueKind::Spacing => format!("a whole number from 0 to {SPACING_MAX}"),
        ValueKind::Line => format!(
            "a line of text whose placeholders are {}",
            PLACEHOLDERS
                .iter()
                .map(|p| format!("{{{p}}}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn invalid(slot: &str, kind: ValueKind) -> StyleError {
    StyleError::InvalidValue {
        slot: slot.to_string(),
        kind: kind.name(),
        expected: expected(kind),
    }
}

fn line_invalid(slot: &str, why: &str) -> StyleError {
    StyleError::InvalidValue {
        slot: slot.to_string(),
        kind: ValueKind::Line.name(),
        expected: format!("{}, and {why}", expected(ValueKind::Line)),
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
        assert!(book.open("gtk").is_err());
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
    fn a_fields_kind_follows_the_slots_kind() {
        for frontend in FRONTENDS {
            let spec = editor_spec(frontend).unwrap();
            let state = editor_state(&StyleSheet::default_for(frontend).unwrap());
            let tree = resolve(
                &spec,
                &state,
                &serde_json::json!({}),
                &serde_json::json!({}),
            );
            for slot in slots(frontend).unwrap() {
                let RenderKind::Field { field, value, .. } =
                    &find(&tree.root, slot.name).expect(slot.name).node
                else {
                    panic!("{frontend} {} is not a field", slot.name)
                };
                let want = match slot.kind {
                    ValueKind::Color => {
                        serde_json::json!({ "select": { "options": COLOR_TOKENS } })
                    }
                    ValueKind::Font => serde_json::json!({ "select": { "options": FONT_TOKENS } }),
                    ValueKind::Spacing => serde_json::json!({
                        "number": { "min": 0, "max": SPACING_MAX, "step": 1 }
                    }),
                    ValueKind::Line => serde_json::json!({ "text": {} }),
                };
                assert_eq!(field, &want, "{frontend} {}", slot.name);
                let shown = match slot.kind {
                    ValueKind::Spacing => {
                        serde_json::json!(slot.default_value.parse::<u32>().unwrap())
                    }
                    _ => serde_json::json!(slot.default_value),
                };
                assert_eq!(value, &shown, "{frontend} {}", slot.name);
            }
        }
    }

    #[test]
    fn a_number_change_sets_a_spacing_slot_as_its_decimal_string() {
        let mut host = StyleHost::new("swift").unwrap();
        let change = |value: Value| Event {
            widget: "column".into(),
            kind: EventKind::Change,
            value,
        };
        assert!(editor_apply(&mut host, &change(serde_json::json!(12))).unwrap());
        assert_eq!(host.sheet().value("column"), "12");
        editor_apply(&mut host, &change(serde_json::json!(" 16 "))).unwrap();
        assert_eq!(host.sheet().value("column"), "16");
        editor_apply(&mut host, &change(serde_json::json!(4.0))).unwrap();
        assert_eq!(host.sheet().value("column"), "4");
        for bad in [
            serde_json::json!(-1),
            serde_json::json!(2.5),
            serde_json::json!(SPACING_MAX + 1),
            serde_json::json!("wide"),
            Value::Null,
        ] {
            let error = editor_apply(&mut host, &change(bad.clone())).unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("column takes a whole number from 0 to {SPACING_MAX}"),
                "{bad}"
            );
        }
        assert_eq!(host.sheet().value("column"), "4");
    }

    #[test]
    fn a_refusal_is_a_sentence_naming_slot_kind_and_choices() {
        let mut swift = StyleHost::new("swift").unwrap();
        let error = swift.set("button", "\"red\"").unwrap_err().to_string();
        assert_eq!(
            error,
            "button takes a colour token: automatic, accent, primary, secondary, error, \
             warning, info"
        );
        assert!(!error.contains("red") && !error.contains('\''), "{error}");
        let number = editor_apply(
            &mut swift,
            &Event {
                widget: "button".into(),
                kind: EventKind::Change,
                value: serde_json::json!(3),
            },
        )
        .unwrap_err();
        assert_eq!(number.to_string(), error);

        let tui = StyleHost::new("tui").unwrap();
        let line = |raw: &str| tui.clone().set("text", raw).unwrap_err().to_string();
        let takes = "text takes a line of text whose placeholders are {columns}, {label}, \
                     {level}, {message}, {text}, {title}, {value}";
        assert_eq!(line("  "), format!("{takes}, and this one is empty"));
        assert_eq!(
            line("{label"),
            format!("{takes}, and this one opens a '{{' it never closes")
        );
        assert_eq!(
            line("{nope}"),
            format!("{takes}, and this one names another")
        );
        if let Some(font) = slots("web")
            .unwrap()
            .iter()
            .find(|s| s.kind == ValueKind::Font)
        {
            let mut web = StyleHost::new("web").unwrap();
            assert_eq!(
                web.set(font.name, "Comic Sans").unwrap_err().to_string(),
                format!(
                    "{} takes a font token: automatic, body, title, mono",
                    font.name
                )
            );
        }
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
