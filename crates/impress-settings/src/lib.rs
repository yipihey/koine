//! The settings registry: every setting the suite has, declared once (ADR-0036 D5).
//!
//! Before this crate a setting existed only where it was read — 224 distinct
//! `UserDefaults`/`@AppStorage` keys across three stores, PublicationManagerCore's
//! 108 written into six app domains, nothing declaring a type, a default or an
//! owner (plan-self-reflective-layer.md, table RG-S). This crate is the one
//! declaration: [`registry::SettingDef`] carries the key, the type, the default,
//! the [`registry::Scope`], the `UserDefaults` keys it migrates from, the section
//! it belongs to and the sentence that describes it. Everything else — the
//! `settings-service` verbs, the UniFFI `SharedSettings` object, Swift's
//! `@ImpressSetting`, the generated settings pane — is a projection of
//! [`registry::registry`].
//!
//! # Where a value lives
//!
//! [`store::SettingsStore`] keeps one JSON file per scope under
//! `<workspace>/settings/` (`device.json`, `app-imbib.json`, `library.json`),
//! written the way `impress_ai::preferences` writes `ai/preferences.json`:
//! a fingerprint-guarded cache, temp + `fsync` + rename, and an advisory
//! `flock` around every read-modify-write so six apps, two daemons and the CLI
//! never lose each other's writes (decision D-R12). The `Synced` scope is not a
//! file: it is one `impress/settings@1.0.0` store row the sync engine carries,
//! reached through a [`store::SyncedBackend`] the store-tier service installs —
//! this crate is pure and opens no database.
//!
//! # Migration never loses a value (D-R5)
//!
//! [`store::SettingsStore::import_legacy`] writes a legacy value into the file
//! only when the key has no value there yet, and it never touches the source:
//! the `UserDefaults` key stays readable by every older build. Removing the old
//! keys is a later, separate decision once no build reads them.
//!
//! # Fails loudly
//!
//! An unknown key is [`store::SettingsError::UnknownKey`] at the first read,
//! never a silent default; a value of the wrong type is refused, not coerced
//! past what [`registry::SettingType::coerce`] can prove (`"30"` for an
//! integer is fine, `"soon"` is not).

#![forbid(unsafe_code)]

pub mod registry;
pub mod store;
pub mod surface;

pub use registry::{
    lookup, registry, section, sections, Choice, DefaultValue, Scope, SectionDef, SettingDef,
    SettingType, SETTINGS_SINCE, SYNCED_SCHEMA_REF,
};
pub use store::{
    settings_directory, Resolved, SettingsError, SettingsStore, SyncedBackend, ValueSource,
    SETTINGS_DIRECTORY, SETTINGS_FILE_VERSION,
};
pub use surface::{section_surface, surface_state_key};

// Manifest metadata without a runtime dependency back on the store.
#[allow(dead_code)]
mod schema_names {
    include!(concat!(env!("OUT_DIR"), "/schema_ref_names.rs"));
}
