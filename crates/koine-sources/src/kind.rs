//! The canonical record: `bibliographic-work@1.0.0` (S-D4).
//!
//! A work is CSL-JSON (every style processor reads it) plus what CSL has no
//! place for: an `ids` map of every identifier any source supplied, versioned
//! `locations`, per-fetch `provenance` and `metrics`.
//!
//! **Layout choice (the plan left it open).** Our four extras live *beside*
//! the CSL body, as top-level fields of the payload, not under CSL's
//! `custom` object: a reader of the stored row sees `ids` and `locations`
//! where it expects them, and the kind schema can name them. A strict-CSL
//! consumer gets the other layout from [`Work::to_csl_json`], which emits
//! only CSL keys and carries the extras under `custom`. The two are one
//! mapping, written once, here.
//!
//! The kind schema ([`schema`]) validates the top level (what
//! `impress_item::kind::validate` can see). The nested rules, such as a
//! location's `version` being one of three words, are the typed [`Work`]'s:
//! deserialising a bad one fails.

use std::collections::BTreeMap;

use impress_item::kind::{FieldDef, FieldType, KindSchema};
use impress_item::Value;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The kind (`schema_ref`), spelled as `schema-refs.json` spells it.
pub const BIBLIOGRAPHIC_WORK: &str = "bibliographic-work@1.0.0";
/// The plan's short spelling (`bibliographic-work@1`), the major version.
/// An explain record may name the kind either way.
pub const BIBLIOGRAPHIC_WORK_MAJOR: &str = "bibliographic-work@1";
/// This schema's version.
pub const VERSION: &str = "1.0.0";

/// The identifier kinds a work's `ids` may hold.
pub const IDENTIFIER_KINDS: &[&str] = &[
    "doi", "arxiv", "pmid", "pmcid", "bibcode", "isbn", "openalex", "s2", "mag", "wos", "dblp",
];

/// One author, as CSL has it, plus an ORCID.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Author {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub given: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orcid: Option<String>,
}

/// CSL's date: `{"date-parts": [[2020, 5, 17]]}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Issued {
    #[serde(rename = "date-parts")]
    pub date_parts: Vec<Vec<i32>>,
}

/// Which version of a work a location holds (DRIVER).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Version {
    Submitted,
    Accepted,
    Published,
}

/// Where one version of the work can be had. A preprint and its published
/// version are one work with two locations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Location {
    pub source: String,
    pub source_id: String,
    pub version: Version,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pdf: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oa_status: Option<String>,
    /// The location a reader should open first.
    #[serde(default)]
    pub primary: bool,
}

/// Per-fetch provenance (S-D5): the field import used to drop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Provenance {
    /// RFC 3339.
    pub fetched_at: String,
    pub source: String,
    pub source_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

/// A count and who counted it: counts differ by source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Counted {
    pub value: u64,
    pub source: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Metrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cited_by: Option<Counted>,
}

/// The canonical record, typed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Work {
    /// CSL type (`article-journal`, `paper-conference`, `book`, ...).
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub author: Vec<Author>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued: Option<Issued>,
    #[serde(
        rename = "container-title",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub container_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "abstract")]
    pub abstract_: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "DOI")]
    pub doi: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "URL")]
    pub url: Option<String>,
    /// Every identifier any source supplied, by kind in [`IDENTIFIER_KINDS`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ids: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locations: Vec<Location>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Metrics>,
}

impl Work {
    /// Refuse what the types cannot: an `ids` key outside
    /// [`IDENTIFIER_KINDS`], more than one primary location.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(bad) = self
            .ids
            .keys()
            .find(|k| !IDENTIFIER_KINDS.contains(&k.as_str()))
        {
            return Err(format!("`{bad}` is not a canonical id kind"));
        }
        if self.locations.iter().filter(|l| l.primary).count() > 1 {
            return Err("more than one primary location".into());
        }
        Ok(())
    }

    /// The row payload, for the store (top-level fields of the kind).
    pub fn to_payload(&self) -> BTreeMap<String, Value> {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::Object(map)) => map
                .into_iter()
                .filter_map(|(k, v)| serde_json::from_value(v).ok().map(|v| (k, v)))
                .collect(),
            _ => BTreeMap::new(),
        }
    }

    /// The CSL `id` this work is given in strict output: its DOI, else the
    /// first id by [`IDENTIFIER_KINDS`] order, else its first location's
    /// `source:source_id`, else `work`.
    pub fn csl_id(&self) -> String {
        if let Some(doi) = self.doi.as_ref().or_else(|| self.ids.get("doi")) {
            return doi.clone();
        }
        for kind in IDENTIFIER_KINDS {
            if let Some(id) = self.ids.get(*kind) {
                return format!("{kind}:{id}");
            }
        }
        match self.locations.first() {
            Some(l) => format!("{}:{}", l.source, l.source_id),
            None => "work".into(),
        }
    }

    /// Strict CSL-JSON: only CSL's keys, plus `id`. Our extras (`ids`,
    /// `locations`, `provenance`, `metrics`, and the authors' ORCIDs as
    /// `author_orcid`, parallel to `author`) go under `custom`, which CSL
    /// reserves for exactly this.
    pub fn to_csl_json(&self) -> serde_json::Value {
        let mut csl = serde_json::Map::new();
        csl.insert("id".into(), json!(self.csl_id()));
        csl.insert("type".into(), json!(self.ty));
        let mut put = |key: &str, value: Option<serde_json::Value>| {
            if let Some(v) = value {
                csl.insert(key.into(), v);
            }
        };
        put("title", self.title.as_ref().map(|v| json!(v)));
        if !self.author.is_empty() {
            let authors: Vec<_> = self
                .author
                .iter()
                .map(|a| {
                    let mut o = serde_json::Map::new();
                    if let Some(f) = &a.family {
                        o.insert("family".into(), json!(f));
                    }
                    if let Some(g) = &a.given {
                        o.insert("given".into(), json!(g));
                    }
                    serde_json::Value::Object(o)
                })
                .collect();
            put("author", Some(json!(authors)));
        }
        put("issued", self.issued.as_ref().map(|v| json!(v)));
        put(
            "container-title",
            self.container_title.as_ref().map(|v| json!(v)),
        );
        put("volume", self.volume.as_ref().map(|v| json!(v)));
        put("issue", self.issue.as_ref().map(|v| json!(v)));
        put("page", self.page.as_ref().map(|v| json!(v)));
        put("abstract", self.abstract_.as_ref().map(|v| json!(v)));
        put("language", self.language.as_ref().map(|v| json!(v)));
        put("DOI", self.doi.as_ref().map(|v| json!(v)));
        put("URL", self.url.as_ref().map(|v| json!(v)));

        let mut custom = serde_json::Map::new();
        if !self.ids.is_empty() {
            custom.insert("ids".into(), json!(self.ids));
        }
        if !self.locations.is_empty() {
            custom.insert("locations".into(), json!(self.locations));
        }
        if let Some(p) = &self.provenance {
            custom.insert("provenance".into(), json!(p));
        }
        if let Some(m) = &self.metrics {
            custom.insert("metrics".into(), json!(m));
        }
        if self.author.iter().any(|a| a.orcid.is_some()) {
            let orcids: Vec<_> = self.author.iter().map(|a| json!(a.orcid)).collect();
            custom.insert("author_orcid".into(), json!(orcids));
        }
        if !custom.is_empty() {
            csl.insert("custom".into(), serde_json::Value::Object(custom));
        }
        serde_json::Value::Object(csl)
    }
}

/// The kind's schema: the top-level fields of a work. Only `type` is
/// required (CSL's own rule); everything else is optional, so a source that
/// fills little is still a valid work.
pub fn schema() -> KindSchema {
    use FieldType::*;
    let optional = FieldDef::optional;
    KindSchema {
        name: BIBLIOGRAPHIC_WORK.into(),
        version: VERSION.into(),
        fields: vec![
            FieldDef::required("type", String),
            optional("title", String),
            optional("author", Array),
            optional("issued", Object),
            optional("container-title", String),
            optional("volume", String),
            optional("issue", String),
            optional("page", String),
            optional("abstract", String),
            optional("language", String),
            optional("DOI", String),
            optional("URL", String),
            optional("ids", Object),
            optional("locations", Array),
            optional("provenance", Object),
            optional("metrics", Object),
        ],
        enforce: false,
    }
}

/// Register the kind in the process registry. Idempotent; call from a host's
/// start-up (and from any test that validates against the registry).
pub fn ensure_registered() {
    // Re-registering the identical schema is compatible; a refusal would mean
    // a different build registered an incompatible one, which is a bug to
    // surface, not swallow.
    if let Err(refused) = impress_item::kind::register(schema()) {
        panic!(
            "{} refused: {}",
            refused.kind,
            refused
                .breaks
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
}
