use std::{borrow::Cow, fmt, str::FromStr};

use serde::{Deserialize, Serialize};

/// A schema name. Source code uses the manifest-generated [`refs`] constants.
///
/// The constructor is private: a misspelt literal cannot become a query's
/// schema through `into()`. Explicit [`FromStr`] parsing requires a manifest
/// name. Serialization boundaries (rows, backups, sync, and the existing Swift
/// string API) preserve opaque names, so a newer writer or an old backup cannot
/// make unrelated rows unreadable. This is a construction guard, not a claim
/// that every wire value is canonical. Opaque names own their bytes; canonical
/// constants borrow static strings and never allocate.
///
/// ```compile_fail
/// let schema: impress_item::SchemaRef = "manuscript-sectoin".into();
/// ```
/// ```compile_fail
/// let schema = impress_item::SchemaRef(std::borrow::Cow::Borrowed("manuscript"));
/// ```
#[repr(transparent)]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SchemaRef(Cow<'static, str>);

impl SchemaRef {
    const fn canonical(value: &'static str) -> Self {
        Self(Cow::Borrowed(value))
    }

    pub const fn as_str(&self) -> &str {
        match &self.0 {
            Cow::Borrowed(value) => value,
            Cow::Owned(value) => value.as_str(),
        }
    }

    /// Decode an existing row without losing an opaque schema name.
    ///
    /// The store calls this at the persistence boundary. New code names a
    /// manifest constant. Hidden so a call site has to mean it.
    #[doc(hidden)]
    pub fn from_stored(value: String) -> Self {
        Self::lookup(&value).unwrap_or(Self(Cow::Owned(value)))
    }

    fn lookup(value: &str) -> Option<Self> {
        refs::ALL
            .binary_search_by(|schema| schema.as_str().cmp(value))
            .ok()
            .map(|index| refs::ALL[index].clone())
    }
}

/// A dynamic schema name was not declared in the suite manifest.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown schema reference: {0}")]
pub struct UnknownSchemaRef(pub String);

impl FromStr for SchemaRef {
    type Err = UnknownSchemaRef;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::lookup(value).ok_or_else(|| UnknownSchemaRef(value.to_owned()))
    }
}

impl<'de> Deserialize<'de> for SchemaRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::from_stored)
    }
}

impl fmt::Display for SchemaRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AsRef<str> for SchemaRef {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl From<&SchemaRef> for SchemaRef {
    fn from(value: &SchemaRef) -> Self {
        value.clone()
    }
}

impl From<SchemaRef> for String {
    fn from(value: SchemaRef) -> Self {
        value.0.into_owned()
    }
}

impl std::ops::Deref for SchemaRef {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl std::borrow::Borrow<str> for SchemaRef {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

#[cfg(feature = "sqlite")]
impl rusqlite::ToSql for SchemaRef {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.as_str().to_sql()
    }
}

/// Canonical vocabulary generated from the root `schema-refs.json`.
pub mod refs {
    use super::SchemaRef;
    include!(concat!(env!("OUT_DIR"), "/schema_refs.rs"));
}

/// Manifest spellings for metadata and legacy wire/SQL boundaries. Store
/// queries use [`refs`]; this module also serves crates below the store which
/// cannot depend on its types without a cycle.
pub mod names {
    include!(concat!(env!("OUT_DIR"), "/schema_ref_names.rs"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_and_surface_rows_keep_their_names() {
        assert_eq!(refs::IMPRESS_UI_LAYOUT.as_str(), "impress/ui/layout@1.0.0");
        assert_eq!(refs::IMPRESS_UI_PRESET.as_str(), "impress/ui/preset@1.0.0");
        assert_eq!(
            refs::IMPRESS_UI_SURFACE.as_str(),
            "impress/ui/surface@1.0.0"
        );
        assert_eq!(
            refs::IMPRESS_UI_SURFACE_STATE.as_str(),
            "impress/ui/surface-state@1.0.0"
        );
        assert_eq!(
            refs::IMPRESS_UI_SURFACE_EVENT.as_str(),
            "impress/ui/surface-event@1.0.0"
        );
    }
}
