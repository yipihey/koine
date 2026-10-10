//! The declarations: [`SettingDef`], [`SectionDef`], and [`registry`] — the
//! one list of every setting this build knows.
//!
//! A setting is data, not code: the [`setting!`] macro builds a [`SettingDef`]
//! value and nothing here executes at declaration time. The first keys are the
//! ones plan-self-reflective-layer.md R1 names — imbib's retention (W3's
//! workflow reads them) and the automation section every app already shares
//! (P0) — and the census table RG-S is the list of what has not moved yet.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The one store row the `Synced` scope rides (decision D-R1). Spelled here so
/// the store-tier backend copies the constant, never a sibling call site.
pub const SYNCED_SCHEMA_REF: &str = crate::schema_names::IMPRESS_SETTINGS;

/// When the registry first shipped; the `since` of every key declared with it.
pub const SETTINGS_SINCE: &str = "2026-09-26";

/// Where a setting's value lives, which decides which file (or row) holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "app")]
pub enum Scope {
    /// This device, every app: `settings/device.json`.
    Device,
    /// This device, one app: `settings/app-<id>.json`. The app id is the
    /// suite's (`imbib`, `imprint`, `implore`, `impel`, `impart`, `impress`).
    App(String),
    /// The library this workspace holds: `settings/library.json`.
    Library,
    /// Every device: one `impress/settings@1.0.0` store row.
    Synced,
}

impl Scope {
    /// A stable label (`device`, `app:imbib`, `library`, `synced`) for wire
    /// results and log lines.
    pub fn label(&self) -> String {
        match self {
            Scope::Device => "device".into(),
            Scope::App(app) => format!("app:{app}"),
            Scope::Library => "library".into(),
            Scope::Synced => "synced".into(),
        }
    }

    /// The file under `settings/` this scope lives in; `None` for `Synced`.
    pub fn file_name(&self) -> Option<String> {
        match self {
            Scope::Device => Some("device.json".into()),
            Scope::App(app) => Some(format!("app-{app}.json")),
            Scope::Library => Some("library.json".into()),
            Scope::Synced => None,
        }
    }
}

/// The type a setting's value must have. `coerce` is deliberately narrow: a
/// string that *is* the value (`"30"`, `"true"`) is accepted because the
/// generated surface's `select` posts strings, and nothing else is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingType {
    Bool,
    Integer,
    Number,
    String,
}

impl SettingType {
    pub fn label(self) -> &'static str {
        match self {
            SettingType::Bool => "bool",
            SettingType::Integer => "integer",
            SettingType::Number => "number",
            SettingType::String => "string",
        }
    }

    /// `value` as this type, or `None` when it cannot be read as one.
    pub fn coerce(self, value: &Value) -> Option<Value> {
        match (self, value) {
            (SettingType::Bool, Value::Bool(_)) => Some(value.clone()),
            (SettingType::Bool, Value::String(s)) => match s.trim() {
                "true" => Some(Value::Bool(true)),
                "false" => Some(Value::Bool(false)),
                _ => None,
            },
            (SettingType::Integer, Value::Number(n)) => n
                .as_i64()
                .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
                .map(Value::from),
            (SettingType::Integer, Value::String(s)) => {
                s.trim().parse::<i64>().ok().map(Value::from)
            }
            (SettingType::Number, Value::Number(n)) => n.as_f64().map(Value::from),
            (SettingType::Number, Value::String(s)) => {
                s.trim().parse::<f64>().ok().map(Value::from)
            }
            (SettingType::String, Value::String(_)) => Some(value.clone()),
            _ => None,
        }
    }
}

/// A declared default, typed by the setting's [`SettingType`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DefaultValue {
    Bool(bool),
    Integer(i64),
    Number(f64),
    String(String),
}

impl DefaultValue {
    pub fn to_json(&self) -> Value {
        match self {
            DefaultValue::Bool(b) => Value::Bool(*b),
            DefaultValue::Integer(i) => Value::from(*i),
            DefaultValue::Number(n) => Value::from(*n),
            DefaultValue::String(s) => Value::String(s.clone()),
        }
    }

    pub fn setting_type(&self) -> SettingType {
        match self {
            DefaultValue::Bool(_) => SettingType::Bool,
            DefaultValue::Integer(_) => SettingType::Integer,
            DefaultValue::Number(_) => SettingType::Number,
            DefaultValue::String(_) => SettingType::String,
        }
    }
}

/// One option of a setting that is chosen from a list: the generated surface
/// shows `label` in a `select`, and `settings-service_set` accepts either the
/// label or the value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    pub label: String,
    pub value: Value,
}

/// One declared setting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingDef {
    /// Dotted, lowercase, `<owner>.<section>.<name>`: `imbib.retention.inbox_days`.
    pub key: String,
    pub ty: SettingType,
    pub default: DefaultValue,
    pub scope: Scope,
    /// The `UserDefaults` keys this setting migrates from, in the order they
    /// are tried. Never removed from `UserDefaults` (D-R5).
    #[serde(default)]
    pub legacy: Vec<String>,
    /// The section whose surface shows it (`imbib.retention`).
    pub section: String,
    /// Internal bookkeeping remains readable through the registry but is not
    /// rendered as a field in generated user settings panes.
    #[serde(default)]
    pub internal: bool,
    /// The field label in the generated surface.
    pub label: String,
    /// One sentence for the agent and the help text.
    pub doc: String,
    #[serde(default)]
    pub choices: Vec<Choice>,
    pub since: String,
}

impl SettingDef {
    /// The label of the choice whose value equals `value`, when the setting
    /// has choices; otherwise `None`.
    pub fn choice_label(&self, value: &Value) -> Option<&str> {
        self.choices
            .iter()
            .find(|choice| &choice.value == value)
            .map(|choice| choice.label.as_str())
    }

    /// `raw` as this setting's value: a choice label, or a value the type
    /// can coerce.
    pub fn accept(&self, raw: &Value) -> Option<Value> {
        if let Value::String(s) = raw {
            if let Some(choice) = self.choices.iter().find(|choice| choice.label == s.trim()) {
                return Some(choice.value.clone());
            }
        }
        self.ty.coerce(raw)
    }
}

/// A group of settings with one generated surface.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SectionDef {
    /// `imbib.retention`, `imbib.automation`, …
    pub id: String,
    pub title: String,
    pub doc: String,
}

/// Declare one setting. Required, in this order: `key`, `ty`, `default`,
/// `scope`, `section`, `label`, `doc`; then optionally `legacy = [..]` and
/// `choices = [(label, value), ..]`.
#[macro_export]
macro_rules! setting {
    (
        key = $key:expr, ty = $ty:ident, default = $default:expr, scope = $scope:expr,
        section = $section:expr, label = $label:expr, doc = $doc:expr
        $(, legacy = [$($legacy:expr),* $(,)?])?
        $(, choices = [$(($choice_label:expr, $choice_value:expr)),* $(,)?])?
        $(,)?
    ) => {
        $crate::registry::SettingDef {
            key: ($key).to_string(),
            ty: $crate::registry::SettingType::$ty,
            default: $crate::registry::DefaultValue::$ty(($default).into()),
            scope: $scope,
            legacy: vec![$($(($legacy).to_string()),*)?],
            section: ($section).to_string(),
            internal: false,
            label: ($label).to_string(),
            doc: ($doc).to_string(),
            choices: vec![$($($crate::registry::Choice {
                label: ($choice_label).to_string(),
                value: ::serde_json::json!($choice_value),
            }),*)?],
            since: $crate::registry::SETTINGS_SINCE.to_string(),
        }
    };
}

/// The suite's apps and their automation ports. A transcription of the ONE
/// authoritative table, `SiblingApp.descriptors` in
/// `packages/ImpressKit/Sources/ImpressKit/SiblingApp.swift`, pinned to it by
/// `ImpressKitTests.SettingsRegistryTests`; a disagreement fails that test,
/// and the Swift table wins.
pub const APP_PORTS: [(&str, u16); 6] = [
    ("imbib", 23120),
    ("imprint", 23121),
    ("impart", 23122),
    ("implore", 23123),
    ("impel", 23124),
    ("impress", 23125),
];

fn retention_choices() -> Vec<(&'static str, i64)> {
    vec![
        ("1 Week", 7),
        ("2 Weeks", 14),
        ("1 Month", 30),
        ("3 Months", 90),
        ("Forever", 0),
    ]
}

fn choices(pairs: Vec<(&'static str, i64)>) -> Vec<Choice> {
    pairs
        .into_iter()
        .map(|(label, value)| Choice {
            label: label.into(),
            value: Value::from(value),
        })
        .collect()
}

fn build_sections() -> Vec<SectionDef> {
    let mut sections = vec![SectionDef {
        id: "imbib.retention".into(),
        title: "Retention".into(),
        doc: "How long imbib keeps inbox papers and explorations before the daily cleanup \
              removes them; 0 keeps them forever."
            .into(),
    }];
    for (app, _) in APP_PORTS {
        sections.push(SectionDef {
            id: format!("{app}.automation"),
            title: "Automation".into(),
            doc: format!(
                "{app}'s local HTTP automation server: whether it runs, its port, request \
                 logging and non-loopback access."
            ),
        });
    }
    sections.extend([
        SectionDef {
            id: "imprint.general".into(),
            title: "General".into(),
            doc: "Imprint's editing, live preview, and backup preferences.".into(),
        },
        SectionDef {
            id: "imprint.editor".into(),
            title: "Editor".into(),
            doc: "Imprint's editor font and display preferences.".into(),
        },
        SectionDef {
            id: "imprint.documents".into(),
            title: "Documents".into(),
            doc: "Imprint's document validation and migration backup preferences.".into(),
        },
    ]);
    sections
}

fn build_registry() -> Vec<SettingDef> {
    let mut out = vec![
        {
            let mut def = setting! {
                key = "imbib.retention.inbox_days", ty = Integer, default = 30_i64,
                scope = Scope::Device, section = "imbib.retention", label = "Keep inbox papers for",
                doc = "Days an unread inbox paper is kept before the daily cleanup removes it; 0 keeps it forever.",
                legacy = ["inbox.retentionDays"]
            };
            def.choices = choices(retention_choices());
            def
        },
        {
            let mut def = setting! {
                key = "imbib.internal.exploration_library_id", ty = String, default = "",
                scope = Scope::Device, section = "imbib.retention", label = "Exploration library ID",
                doc = "Internal pointer to the library used for exploration retention.",
                legacy = ["explorationLibraryID"]
            };
            def.internal = true;
            def
        },
        setting! {
            key = "imbib.retention.auto_remove_read", ty = Bool, default = false,
            scope = Scope::Device, section = "imbib.retention", label = "Auto-remove read papers",
            doc = "Remove an inbox paper from the inbox as soon as it has been read.",
            legacy = ["inbox.autoRemoveRead"]
        },
        {
            let mut def = setting! {
                key = "imbib.retention.exploration_days", ty = Integer, default = 30_i64,
                scope = Scope::Device, section = "imbib.retention", label = "Keep explorations for",
                doc = "Days an exploration collection or search is kept before the daily cleanup removes it; 0 keeps it forever.",
                legacy = ["exploration.retentionDays"]
            };
            def.choices = choices(
                retention_choices()
                    .into_iter()
                    .filter(|(_, days)| *days != 14)
                    .collect(),
            );
            def
        },
    ];
    // These are the thirteen portable, non-platform controls in imprint's
    // General, Editor, and Documents panes. Exact old UserDefaults spellings
    // stay as legacy keys for first-read copy without deletion (D-R5).
    let imprint = Scope::App("imprint".into());
    out.extend([
        setting! {
            key = "imprint.general.default_edit_mode", ty = String, default = "split_view",
            scope = imprint.clone(), section = "imprint.general", label = "Default Edit Mode",
            doc = "The editing layout opened for a manuscript by default.",
            legacy = ["defaultEditMode"],
            choices = [("Direct PDF", "direct_pdf"), ("Split View", "split_view"), ("Text Only", "text_only")]
        },
        setting! {
            key = "imprint.general.auto_save_interval", ty = Integer, default = 60_i64,
            scope = imprint.clone(), section = "imprint.general", label = "Auto-save interval",
            doc = "Seconds between automatic manuscript saves.",
            legacy = ["autoSaveInterval"]
        },
        setting! {
            key = "imprint.general.create_backups", ty = Bool, default = true,
            scope = imprint.clone(), section = "imprint.general", label = "Create automatic backups",
            doc = "Keep automatic backups of manuscripts.",
            legacy = ["createBackups"]
        },
        setting! {
            key = "imprint.general.auto_compile", ty = Bool, default = true,
            scope = imprint.clone(), section = "imprint.general", label = "Live preview",
            doc = "Automatically recompile the live preview while typing.",
            legacy = ["imprint.autoCompile"]
        },
        setting! {
            key = "imprint.general.compile_debounce_ms", ty = Integer, default = 300_i64,
            scope = imprint.clone(), section = "imprint.general", label = "Preview update delay",
            doc = "Milliseconds after typing before the live preview recompiles.",
            legacy = ["imprint.compileDebounceMs"]
        },
        setting! {
            key = "imprint.general.preview_format", ty = String, default = "pdf",
            scope = imprint.clone(), section = "imprint.general", label = "Preview format",
            doc = "Render the live preview as PDF or per-page SVG.",
            legacy = ["imprint.previewFormat"],
            choices = [("PDF", "pdf"), ("SVG (faster)", "svg")]
        },
        setting! {
            key = "imprint.editor.font_size", ty = Integer, default = 14_i64,
            scope = imprint.clone(), section = "imprint.editor", label = "Font Size",
            doc = "Point size used by the manuscript editor.",
            legacy = ["editorFontSize"]
        },
        setting! {
            key = "imprint.editor.font_family", ty = String, default = "SF Mono",
            scope = imprint.clone(), section = "imprint.editor", label = "Font Family",
            doc = "Font family used by the manuscript editor.",
            legacy = ["editorFontFamily"],
            choices = [("SF Mono", "SF Mono"), ("Menlo", "Menlo"), ("Monaco", "Monaco"), ("Courier New", "Courier New")]
        },
        setting! {
            key = "imprint.editor.show_line_numbers", ty = Bool, default = true,
            scope = imprint.clone(), section = "imprint.editor", label = "Show line numbers",
            doc = "Show source line numbers beside the manuscript editor.",
            legacy = ["showLineNumbers"]
        },
        setting! {
            key = "imprint.editor.highlight_current_line", ty = Bool, default = true,
            scope = imprint.clone(), section = "imprint.editor", label = "Highlight current line",
            doc = "Highlight the line containing the editor cursor.",
            legacy = ["highlightCurrentLine"]
        },
        setting! {
            key = "imprint.editor.wrap_lines", ty = Bool, default = true,
            scope = imprint.clone(), section = "imprint.editor", label = "Wrap long lines",
            doc = "Wrap long source lines in the manuscript editor.",
            legacy = ["wrapLines"]
        },
        setting! {
            key = "imprint.documents.validate_crdt_on_open", ty = Bool, default = true,
            scope = imprint.clone(), section = "imprint.documents", label = "Validate CRDT on open",
            doc = "Check document integrity when opening a manuscript.",
            legacy = ["validateCRDTOnOpen"]
        },
        setting! {
            key = "imprint.documents.auto_backup_before_migration", ty = Bool, default = true,
            scope = imprint, section = "imprint.documents", label = "Back up before migration",
            doc = "Create a backup before upgrading a document's format version.",
            legacy = ["autoBackupBeforeMigration"]
        },
    ]);
    for (app, port) in APP_PORTS {
        let scope = Scope::App(app.into());
        let section = format!("{app}.automation");
        out.push(setting! {
            key = format!("{app}.automation.http_enabled"), ty = Bool, default = true,
            scope = scope.clone(), section = &section, label = "Enable HTTP server",
            doc = "Run the app's local HTTP automation server (the /api routes agents and the CLI dial).",
            legacy = ["httpAutomationEnabled"]
        });
        out.push(setting! {
            key = format!("{app}.automation.http_port"), ty = Integer, default = i64::from(port),
            scope = scope.clone(), section = &section, label = "Port",
            doc = "The loopback port the automation server binds; the suite's port table gives the default.",
            legacy = ["httpAutomationPort"]
        });
        out.push(setting! {
            key = format!("{app}.automation.log_requests"), ty = Bool, default = true,
            scope = scope.clone(), section = &section, label = "Log requests",
            doc = "Log every automation request at info level in the app's console.",
            legacy = ["httpAutomationLogRequests"]
        });
        out.push(setting! {
            key = format!("{app}.automation.allow_network_access"), ty = Bool, default = false,
            scope = scope.clone(), section = &section, label = "Allow network access",
            doc = "Accept non-loopback peers (a tailnet). Needs a network bearer in the keychain and an explicit bind address, or the server refuses to start.",
            legacy = ["httpAutomationAllowNetworkAccess"]
        });
        out.push(setting! {
            key = format!("{app}.automation.network_bind_address"), ty = String, default = "",
            scope = scope, section = &section, label = "Network bind address",
            doc = "The interface address to bind in network mode, spelled as callers dial it; never all interfaces.",
            legacy = ["httpAutomationNetworkBindAddress"]
        });
    }
    out
}

/// Every declared setting, built once. Keys are unique and every setting's
/// section is declared — `tests::registry_is_well_formed` holds both.
pub fn registry() -> &'static [SettingDef] {
    static REGISTRY: OnceLock<Vec<SettingDef>> = OnceLock::new();
    REGISTRY.get_or_init(build_registry)
}

/// Every declared section, in display order.
pub fn sections() -> &'static [SectionDef] {
    static SECTIONS: OnceLock<Vec<SectionDef>> = OnceLock::new();
    SECTIONS.get_or_init(build_sections)
}

/// The declaration for `key`, or `None` — the caller turns that into
/// [`crate::store::SettingsError::UnknownKey`].
pub fn lookup(key: &str) -> Option<&'static SettingDef> {
    static INDEX: OnceLock<BTreeMap<&'static str, &'static SettingDef>> = OnceLock::new();
    INDEX
        .get_or_init(|| {
            registry()
                .iter()
                .map(|def| (def.key.as_str(), def))
                .collect()
        })
        .get(key)
        .copied()
}

/// The section declaration for `id`.
pub fn section(id: &str) -> Option<&'static SectionDef> {
    sections().iter().find(|section| section.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn registry_is_well_formed() {
        let mut keys = BTreeSet::new();
        let section_ids: BTreeSet<&str> = sections().iter().map(|s| s.id.as_str()).collect();
        for def in registry() {
            assert!(keys.insert(def.key.as_str()), "duplicate key {}", def.key);
            assert_eq!(
                def.default.setting_type(),
                def.ty,
                "{}: default type",
                def.key
            );
            assert!(
                section_ids.contains(def.section.as_str()),
                "{}: section",
                def.key
            );
            assert!(!def.doc.trim().is_empty(), "{}: doc", def.key);
            assert!(!def.label.trim().is_empty(), "{}: label", def.key);
            assert!(
                def.key
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_'),
                "{}: key spelling",
                def.key
            );
            for choice in &def.choices {
                assert_eq!(
                    def.ty.coerce(&choice.value),
                    Some(choice.value.clone()),
                    "{}: choice type",
                    def.key
                );
            }
            assert_eq!(def.since, SETTINGS_SINCE);
        }
        assert_eq!(registry().len(), 4 + 5 * APP_PORTS.len() + 13);
        assert!(lookup("imbib.retention.inbox_days").is_some());
        assert!(lookup("imbib.retention.inboxDays").is_none());
        assert!(
            lookup("imbib.internal.exploration_library_id").is_some_and(|setting| setting.internal)
        );
    }

    #[test]
    fn imprint_portable_preferences_keep_their_defaults_and_legacy_values() {
        use crate::store::{SettingsStore, ValueSource};

        // Every entry is (canonical key, original UserDefaults key, old
        // default, a non-default value saved by an older build). This test
        // names all thirteen so a missing migration mapping is visible.
        let expected = [
            (
                "imprint.general.default_edit_mode",
                "defaultEditMode",
                Value::from("split_view"),
                Value::from("direct_pdf"),
            ),
            (
                "imprint.general.auto_save_interval",
                "autoSaveInterval",
                Value::from(60),
                Value::from(120),
            ),
            (
                "imprint.general.create_backups",
                "createBackups",
                Value::from(true),
                Value::from(false),
            ),
            (
                "imprint.general.auto_compile",
                "imprint.autoCompile",
                Value::from(true),
                Value::from(false),
            ),
            (
                "imprint.general.compile_debounce_ms",
                "imprint.compileDebounceMs",
                Value::from(300),
                Value::from(700),
            ),
            (
                "imprint.general.preview_format",
                "imprint.previewFormat",
                Value::from("pdf"),
                Value::from("svg"),
            ),
            (
                "imprint.editor.font_size",
                "editorFontSize",
                Value::from(14),
                Value::from(18),
            ),
            (
                "imprint.editor.font_family",
                "editorFontFamily",
                Value::from("SF Mono"),
                Value::from("Menlo"),
            ),
            (
                "imprint.editor.show_line_numbers",
                "showLineNumbers",
                Value::from(true),
                Value::from(false),
            ),
            (
                "imprint.editor.highlight_current_line",
                "highlightCurrentLine",
                Value::from(true),
                Value::from(false),
            ),
            (
                "imprint.editor.wrap_lines",
                "wrapLines",
                Value::from(true),
                Value::from(false),
            ),
            (
                "imprint.documents.validate_crdt_on_open",
                "validateCRDTOnOpen",
                Value::from(true),
                Value::from(false),
            ),
            (
                "imprint.documents.auto_backup_before_migration",
                "autoBackupBeforeMigration",
                Value::from(true),
                Value::from(false),
            ),
        ];
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::open(dir.path());
        let legacy_values: BTreeMap<&str, Value> = expected
            .iter()
            .map(|(_, legacy, _, migrated)| (*legacy, migrated.clone()))
            .collect();
        assert_eq!(legacy_values.len(), 13);

        for (key, legacy, default, migrated) in &expected {
            let def = lookup(key).unwrap_or_else(|| panic!("missing {key}"));
            assert_eq!(def.scope, Scope::App("imprint".into()), "{key}: scope");
            assert_eq!(
                def.section,
                key.rsplit_once('.').unwrap().0,
                "{key}: section"
            );
            assert_eq!(def.legacy, [*legacy], "{key}: legacy key");
            assert_eq!(def.default.to_json(), *default, "{key}: old default");
            assert_eq!(store.get(key).unwrap().source, ValueSource::Default);
            assert!(
                store.import_legacy(key, migrated).unwrap(),
                "{key}: first import"
            );
            assert!(
                !store.import_legacy(key, default).unwrap(),
                "{key}: never overwrite"
            );
            assert_eq!(
                store.get(key).unwrap().value,
                *migrated,
                "{key}: migrated value"
            );
            assert_eq!(
                legacy_values.get(legacy),
                Some(migrated),
                "{key}: old value remains"
            );
        }
        let reopened = SettingsStore::open(dir.path());
        for (key, _, _, migrated) in &expected {
            let resolved = reopened.get(key).unwrap();
            assert_eq!(resolved.source, ValueSource::Stored);
            assert_eq!(resolved.value, *migrated, "{key}: persisted app scope");
        }
        assert!(dir.path().join("settings/app-imprint.json").exists());
    }

    #[test]
    fn coercion_is_narrow() {
        assert_eq!(
            SettingType::Integer.coerce(&Value::from("30")),
            Some(Value::from(30))
        );
        assert_eq!(
            SettingType::Integer.coerce(&Value::from(30.0)),
            Some(Value::from(30))
        );
        assert_eq!(SettingType::Integer.coerce(&Value::from(30.5)), None);
        assert_eq!(SettingType::Integer.coerce(&Value::from("soon")), None);
        assert_eq!(
            SettingType::Bool.coerce(&Value::from("true")),
            Some(Value::Bool(true))
        );
        assert_eq!(SettingType::Bool.coerce(&Value::from(1)), None);
        assert_eq!(SettingType::String.coerce(&Value::from(1)), None);
        let def = lookup("imbib.retention.inbox_days").unwrap();
        assert_eq!(def.accept(&Value::from("1 Month")), Some(Value::from(30)));
        assert_eq!(def.accept(&Value::from("14")), Some(Value::from(14)));
        assert_eq!(def.choice_label(&Value::from(90)), Some("3 Months"));
        assert_eq!(def.choice_label(&Value::from(91)), None);
    }

    #[test]
    fn scopes_name_their_files() {
        assert_eq!(Scope::Device.file_name().as_deref(), Some("device.json"));
        assert_eq!(
            Scope::App("imbib".into()).file_name().as_deref(),
            Some("app-imbib.json")
        );
        assert_eq!(Scope::Library.file_name().as_deref(), Some("library.json"));
        assert_eq!(Scope::Synced.file_name(), None);
        assert_eq!(Scope::App("impel".into()).label(), "app:impel");
        let json = serde_json::to_value(Scope::App("imbib".into())).unwrap();
        assert_eq!(json, serde_json::json!({"kind": "app", "app": "imbib"}));
    }
}
