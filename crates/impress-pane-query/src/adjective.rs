//! Adjectives (docs/grammar.md § Adjectives): a declared name over the
//! query algebra.
//!
//! An adjective narrows a set of records without changing any of them. A
//! *stored* adjective is lowered by [`expand`] into the [`Filter`]s the
//! algebra already has, so the store evaluates it; a *derived* adjective is
//! computed by a read-only verb taking `{id}` and answering `{holds: bool}`,
//! and the host applies it per row after the stored filters.
//!
//! [`expand`] is pure and total: an unknown adjective, a kind the adjective
//! does not apply to, or a missing, ill-typed or unknown argument is a typed
//! [`PaneQueryError`], never a dropped predicate.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::{Filter, PaneQuery, PaneQueryError};

/// How an adjective's argument is typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum ParamKind {
    String,
    Bool,
    /// An ISO-8601 string (`YYYY-MM-DD`, optionally followed by a time).
    Date,
}

impl fmt::Display for ParamKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ParamKind::String => "string",
            ParamKind::Bool => "bool",
            ParamKind::Date => "date",
        })
    }
}

/// One argument an adjective takes. Serialize-only: `name` is a static
/// string, so a registry's listing is written, never read back into this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdjectiveParam {
    pub name: &'static str,
    pub kind: ParamKind,
    pub required: bool,
}

/// Why an adjective could not be declared or lowered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "error", rename_all = "kebab-case")]
pub enum AdjectiveError {
    #[error("adjective '{name}' is already declared")]
    Duplicate { name: String },
    #[error("adjective '{adjective}' needs the argument '{param}'")]
    MissingArgument { adjective: String, param: String },
    #[error("adjective '{adjective}' takes '{param}' as a {expected}")]
    WrongArgumentType {
        adjective: String,
        param: String,
        expected: ParamKind,
    },
    #[error("adjective '{adjective}' takes no argument '{param}'")]
    UnknownArgument { adjective: String, param: String },
    #[error("adjective '{adjective}' argument '{param}': '{value}' is not ISO-8601")]
    InvalidDate {
        adjective: String,
        param: String,
        value: String,
    },
}

/// How an adjective is evaluated.
#[derive(Debug, Clone)]
pub enum Basis {
    /// Lowered into the algebra: the store evaluates it.
    Stored(fn(&Value) -> Result<Vec<Filter>, AdjectiveError>),
    /// Computed by a read-only verb taking `{id}` and answering
    /// `{holds: bool}`; the host applies it per row after the stored filters.
    Derived { verb: String },
}

/// A declared adjective.
#[derive(Debug, Clone)]
pub struct Adjective {
    pub name: String,
    /// Record kinds it applies to. Empty: every kind.
    pub kinds: Vec<String>,
    pub params: Vec<AdjectiveParam>,
    pub description: String,
    pub basis: Basis,
}

impl Adjective {
    /// Check `args` against the declared params: unknown, missing and
    /// ill-typed arguments are refused. `null` args mean none.
    pub fn validate(&self, args: &Value) -> Result<(), AdjectiveError> {
        let empty = Map::new();
        let map = match args {
            Value::Null => &empty,
            Value::Object(m) => m,
            _ => {
                return Err(AdjectiveError::UnknownArgument {
                    adjective: self.name.clone(),
                    param: "<args is not an object>".into(),
                })
            }
        };
        for key in map.keys() {
            if !self.params.iter().any(|p| p.name == key) {
                return Err(AdjectiveError::UnknownArgument {
                    adjective: self.name.clone(),
                    param: key.clone(),
                });
            }
        }
        for p in &self.params {
            match map.get(p.name) {
                None | Some(Value::Null) => {
                    if p.required {
                        return Err(AdjectiveError::MissingArgument {
                            adjective: self.name.clone(),
                            param: p.name.into(),
                        });
                    }
                }
                Some(v) => {
                    let wrong = || AdjectiveError::WrongArgumentType {
                        adjective: self.name.clone(),
                        param: p.name.into(),
                        expected: p.kind,
                    };
                    match (p.kind, v) {
                        (ParamKind::Bool, Value::Bool(_))
                        | (ParamKind::String, Value::String(_)) => {}
                        (ParamKind::Date, Value::String(s)) => {
                            if !looks_iso8601(s) {
                                return Err(AdjectiveError::InvalidDate {
                                    adjective: self.name.clone(),
                                    param: p.name.into(),
                                    value: s.clone(),
                                });
                            }
                        }
                        _ => return Err(wrong()),
                    }
                }
            }
        }
        Ok(())
    }

    /// Validate `args`, then lower a stored adjective to its filters. A
    /// derived adjective lowers to nothing here (the host applies it).
    pub fn lower(&self, args: &Value) -> Result<Vec<Filter>, AdjectiveError> {
        self.validate(args)?;
        match &self.basis {
            Basis::Stored(f) => f(args),
            Basis::Derived { .. } => Ok(Vec::new()),
        }
    }
}

/// Lenient ISO-8601 shape: `YYYY-MM-DD`, optionally followed by `T`/space
/// and a time. A malformed date must never reach the store (see
/// [`PaneQueryError::InvalidDate`]).
fn looks_iso8601(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 10 {
        return false;
    }
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    if !(digits(0..4) && b[4] == b'-' && digits(5..7) && b[7] == b'-' && digits(8..10)) {
        return false;
    }
    let month: u32 = s[5..7].parse().unwrap_or(0);
    let day: u32 = s[8..10].parse().unwrap_or(0);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return false;
    }
    match b.get(10) {
        None => true,
        Some(b'T') | Some(b' ') => b.len() > 11 && b[11].is_ascii_digit(),
        Some(_) => false,
    }
}

/// One use of an adjective inside a query: `{"name": "flagged", "args": {"color": "red"}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AdjectiveUse {
    pub name: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub args: Value,
}

/// The one registry of adjectives. See [`AdjectiveRegistry::builtin`].
#[derive(Debug, Clone, Default)]
pub struct AdjectiveRegistry {
    by_name: BTreeMap<String, Adjective>,
}

fn p(name: &'static str, kind: ParamKind, required: bool) -> AdjectiveParam {
    AdjectiveParam {
        name,
        kind,
        required,
    }
}

fn str_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

fn stored(
    name: &str,
    params: Vec<AdjectiveParam>,
    description: &str,
    f: fn(&Value) -> Result<Vec<Filter>, AdjectiveError>,
) -> Adjective {
    Adjective {
        name: name.into(),
        kinds: Vec::new(),
        params,
        description: description.into(),
        basis: Basis::Stored(f),
    }
}

fn date_field(args: &Value) -> String {
    str_arg(args, "field").unwrap_or_else(|| "modified".into())
}

impl AdjectiveRegistry {
    /// No adjectives at all.
    pub fn empty() -> Self {
        Self::default()
    }

    /// The built-in adjectives, declared once. Each lowers only to a
    /// [`Filter`] the algebra already has.
    pub fn builtin() -> Self {
        use ParamKind::{Date, String as Str};
        let mut r = Self::empty();
        let all = vec![
            stored("read", vec![], "Records that have been read.", |_| {
                Ok(vec![Filter::Read { read: true }])
            }),
            stored("unread", vec![], "Records that have not been read.", |_| {
                Ok(vec![Filter::Read { read: false }])
            }),
            stored("starred", vec![], "Records that are starred.", |_| {
                Ok(vec![Filter::Starred { starred: true }])
            }),
            stored("unstarred", vec![], "Records that are not starred.", |_| {
                Ok(vec![Filter::Starred { starred: false }])
            }),
            stored(
                "flagged",
                vec![p("color", Str, false)],
                "Records carrying a flag, optionally of one colour.",
                |a| {
                    Ok(vec![Filter::Flag {
                        color: str_arg(a, "color"),
                    }])
                },
            ),
            stored(
                "tagged",
                vec![p("path", Str, true)],
                "Records carrying a tag path or a descendant of it.",
                |a| {
                    Ok(vec![Filter::Tag {
                        path: str_arg(a, "path").unwrap_or_default(),
                    }])
                },
            ),
            stored(
                "status",
                vec![p("status", Str, true)],
                "Records whose payload status equals the given value.",
                |a| {
                    Ok(vec![Filter::Status {
                        status: str_arg(a, "status").unwrap_or_default(),
                    }])
                },
            ),
            stored(
                "since",
                vec![p("from", Date, true), p("field", Str, false)],
                "Records whose date field is on or after the given date (default field: modified).",
                |a| {
                    Ok(vec![Filter::DateRange {
                        field: date_field(a),
                        from: str_arg(a, "from"),
                        to: None,
                    }])
                },
            ),
            stored(
                "before",
                vec![p("to", Date, true), p("field", Str, false)],
                "Records whose date field is on or before the given date (default field: modified).",
                |a| {
                    Ok(vec![Filter::DateRange {
                        field: date_field(a),
                        from: None,
                        to: str_arg(a, "to"),
                    }])
                },
            ),
        ];
        for adj in all {
            r.insert(adj).expect("built-in adjective names are unique");
        }
        r
    }

    /// Add an adjective; a name already declared is an error.
    pub fn insert(&mut self, adjective: Adjective) -> Result<(), AdjectiveError> {
        if self.by_name.contains_key(&adjective.name) {
            return Err(AdjectiveError::Duplicate {
                name: adjective.name,
            });
        }
        self.by_name.insert(adjective.name.clone(), adjective);
        Ok(())
    }

    /// Add derived adjectives a host collected from its verb inventory, as
    /// `(name, kinds, verb)`.
    pub fn with_derived(
        mut self,
        derived: impl IntoIterator<Item = (String, Vec<String>, String)>,
    ) -> Result<Self, AdjectiveError> {
        for (name, kinds, verb) in derived {
            let description = format!("Computed per record by the read-only verb '{verb}'.");
            self.insert(Adjective {
                name,
                kinds,
                params: Vec::new(),
                description,
                basis: Basis::Derived { verb },
            })?;
        }
        Ok(self)
    }

    pub fn get(&self, name: &str) -> Option<&Adjective> {
        self.by_name.get(name)
    }

    /// Every name, sorted.
    pub fn names(&self) -> Vec<&str> {
        self.by_name.keys().map(String::as_str).collect()
    }

    /// JSON listing: `[{"name", "kinds", "params": [{"name","kind","required"}],
    /// "basis": "stored" | {"derived": "<verb>"}, "description"}]`.
    pub fn listing(&self) -> Value {
        Value::Array(
            self.by_name
                .values()
                .map(|a| {
                    let basis = match &a.basis {
                        Basis::Stored(_) => json!("stored"),
                        Basis::Derived { verb } => json!({ "derived": verb }),
                    };
                    json!({
                        "name": a.name,
                        "kinds": a.kinds,
                        "params": a.params,
                        "basis": basis,
                        "description": a.description,
                    })
                })
                .collect(),
        )
    }
}

/// A derived adjective the host must still apply after the stored filters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DerivedAdjective {
    pub name: String,
    pub verb: String,
    pub args: Value,
}

/// The result of [`expand`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Expanded {
    /// The query with `adjectives` drained and `filters` extended.
    pub query: PaneQuery,
    pub derived: Vec<DerivedAdjective>,
}

/// Lower every stored adjective of `query` into its `filters` (original
/// filters first, then adjectives in order); return derived ones. Pure and
/// total: any problem is a typed error, never a dropped predicate.
pub fn expand(query: &PaneQuery, registry: &AdjectiveRegistry) -> Result<Expanded, PaneQueryError> {
    let mut out = query.clone();
    out.adjectives.clear();
    let mut derived = Vec::new();
    for used in &query.adjectives {
        let adj = registry
            .get(&used.name)
            .ok_or_else(|| PaneQueryError::UnknownAdjective {
                name: used.name.clone(),
            })?;
        if !adj.kinds.is_empty() {
            if query.kinds.is_empty() {
                return Err(PaneQueryError::AdjectiveKindMismatch {
                    name: used.name.clone(),
                    kind: "*".into(),
                });
            }
            if let Some(bad) = query.kinds.iter().find(|k| !adj.kinds.contains(k)) {
                return Err(PaneQueryError::AdjectiveKindMismatch {
                    name: used.name.clone(),
                    kind: bad.clone(),
                });
            }
        }
        let arg_err = |e: AdjectiveError| PaneQueryError::AdjectiveArgument {
            name: used.name.clone(),
            reason: e.to_string(),
        };
        adj.validate(&used.args).map_err(arg_err)?;
        match &adj.basis {
            Basis::Stored(f) => out.filters.extend(f(&used.args).map_err(arg_err)?),
            Basis::Derived { verb } => derived.push(DerivedAdjective {
                name: used.name.clone(),
                verb: verb.clone(),
                args: used.args.clone(),
            }),
        }
    }
    Ok(Expanded {
        query: out,
        derived,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn use_(name: &str, args: Value) -> AdjectiveUse {
        AdjectiveUse {
            name: name.into(),
            args,
        }
    }

    fn lower(name: &str, args: Value) -> Result<Vec<Filter>, AdjectiveError> {
        AdjectiveRegistry::builtin()
            .get(name)
            .expect("declared")
            .lower(&args)
    }

    #[test]
    fn builtin_names_are_unique_and_sorted() {
        let r = AdjectiveRegistry::builtin();
        let names = r.names();
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(names, sorted);
        assert_eq!(names.len(), 9);
        // A second insert of any built-in is refused.
        let mut r2 = AdjectiveRegistry::builtin();
        let again = r.get("read").unwrap().clone();
        assert!(matches!(
            r2.insert(again),
            Err(AdjectiveError::Duplicate { .. })
        ));
    }

    #[test]
    fn builtins_without_required_params_lower_to_one_filter() {
        let r = AdjectiveRegistry::builtin();
        for name in r.names() {
            let a = r.get(name).unwrap();
            assert!(a.kinds.is_empty(), "{name} applies to every kind");
            if a.params.iter().all(|p| !p.required) {
                assert_eq!(a.lower(&Value::Null).unwrap().len(), 1, "{name}");
            }
        }
        assert_eq!(
            lower("unread", Value::Null).unwrap(),
            vec![Filter::Read { read: false }]
        );
        assert_eq!(
            lower("unstarred", json!({})).unwrap(),
            vec![Filter::Starred { starred: false }]
        );
    }

    #[test]
    fn flagged_with_and_without_color() {
        assert_eq!(
            lower("flagged", Value::Null).unwrap(),
            vec![Filter::Flag { color: None }]
        );
        assert_eq!(
            lower("flagged", json!({"color": "red"})).unwrap(),
            vec![Filter::Flag {
                color: Some("red".into())
            }]
        );
    }

    #[test]
    fn tagged_requires_a_string_path() {
        assert!(matches!(
            lower("tagged", json!({})),
            Err(AdjectiveError::MissingArgument { .. })
        ));
        assert!(matches!(
            lower("tagged", json!({"path": 5})),
            Err(AdjectiveError::WrongArgumentType {
                expected: ParamKind::String,
                ..
            })
        ));
        assert_eq!(
            lower("tagged", json!({"path": "a/b"})).unwrap(),
            vec![Filter::Tag { path: "a/b".into() }]
        );
    }

    #[test]
    fn since_and_before_lower_to_date_ranges() {
        assert_eq!(
            lower("since", json!({"from": "2026-01-02"})).unwrap(),
            vec![Filter::DateRange {
                field: "modified".into(),
                from: Some("2026-01-02".into()),
                to: None
            }]
        );
        assert_eq!(
            lower(
                "before",
                json!({"to": "2026-01-02T10:00:00Z", "field": "created"})
            )
            .unwrap(),
            vec![Filter::DateRange {
                field: "created".into(),
                from: None,
                to: Some("2026-01-02T10:00:00Z".into())
            }]
        );
        for bad in ["yesterday", "2026-13-01", "2026-1-1", "2026-01-01x", ""] {
            assert!(
                matches!(
                    lower("since", json!({ "from": bad })),
                    Err(AdjectiveError::InvalidDate { .. })
                ),
                "{bad}"
            );
        }
        assert!(matches!(
            lower("since", json!({"from": 3})),
            Err(AdjectiveError::WrongArgumentType { .. })
        ));
    }

    #[test]
    fn an_unknown_argument_is_refused() {
        assert!(matches!(
            lower("flagged", json!({"colour": "red"})),
            Err(AdjectiveError::UnknownArgument { .. })
        ));
        assert!(matches!(
            lower("unread", json!({"x": 1})),
            Err(AdjectiveError::UnknownArgument { .. })
        ));
    }

    #[test]
    fn expand_extends_filters_in_adjective_order() {
        let q = PaneQuery {
            kinds: vec!["publication".into()],
            filters: vec![Filter::Flag { color: None }],
            adjectives: vec![use_("unread", Value::Null), use_("starred", Value::Null)],
            ..Default::default()
        };
        let e = expand(&q, &AdjectiveRegistry::builtin()).unwrap();
        assert_eq!(
            e.query.filters,
            vec![
                Filter::Flag { color: None },
                Filter::Read { read: false },
                Filter::Starred { starred: true },
            ]
        );
        assert!(e.query.adjectives.is_empty());
        assert!(e.derived.is_empty());
        assert_eq!(e.query.kinds, q.kinds);
    }

    #[test]
    fn expand_refuses_unknown_adjectives_and_bad_arguments() {
        let r = AdjectiveRegistry::builtin();
        let q = PaneQuery {
            adjectives: vec![use_("stale", Value::Null)],
            ..Default::default()
        };
        assert_eq!(
            expand(&q, &r).unwrap_err(),
            PaneQueryError::UnknownAdjective {
                name: "stale".into()
            }
        );
        let q = PaneQuery {
            adjectives: vec![use_("tagged", Value::Null)],
            ..Default::default()
        };
        assert!(matches!(
            expand(&q, &r).unwrap_err(),
            PaneQueryError::AdjectiveArgument { .. }
        ));
    }

    fn stale_registry() -> AdjectiveRegistry {
        AdjectiveRegistry::builtin()
            .with_derived([(
                "stale".to_string(),
                vec!["publication".to_string()],
                "imbib-service_is-stale".to_string(),
            )])
            .unwrap()
    }

    #[test]
    fn a_derived_adjective_checks_kinds_and_is_returned() {
        let r = stale_registry();
        let mk = |kinds: &[&str]| PaneQuery {
            kinds: kinds.iter().map(|s| s.to_string()).collect(),
            filters: vec![Filter::Read { read: true }],
            adjectives: vec![use_("stale", Value::Null)],
            ..Default::default()
        };
        assert_eq!(
            expand(&mk(&["manuscript"]), &r).unwrap_err(),
            PaneQueryError::AdjectiveKindMismatch {
                name: "stale".into(),
                kind: "manuscript".into()
            }
        );
        assert_eq!(
            expand(&mk(&[]), &r).unwrap_err(),
            PaneQueryError::AdjectiveKindMismatch {
                name: "stale".into(),
                kind: "*".into()
            }
        );
        let e = expand(&mk(&["publication"]), &r).unwrap();
        assert_eq!(e.query.filters, vec![Filter::Read { read: true }]);
        assert_eq!(
            e.derived,
            vec![DerivedAdjective {
                name: "stale".into(),
                verb: "imbib-service_is-stale".into(),
                args: Value::Null
            }]
        );
    }

    #[test]
    fn the_listing_round_trips() {
        let r = stale_registry();
        let listing = r.listing();
        let text = serde_json::to_string(&listing).unwrap();
        let back: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(back, listing);
        let arr = back.as_array().unwrap();
        let names: Vec<&str> = arr.iter().map(|e| e["name"].as_str().unwrap()).collect();
        assert_eq!(names, r.names());
        let stale = arr.iter().find(|e| e["name"] == "stale").unwrap();
        assert_eq!(stale["kinds"], json!(["publication"]));
        assert_eq!(stale["basis"], json!({"derived": "imbib-service_is-stale"}));
        let since = arr.iter().find(|e| e["name"] == "since").unwrap();
        assert_eq!(since["basis"], json!("stored"));
        assert_eq!(
            since["params"][0],
            json!({"name": "from", "kind": "date", "required": true})
        );
    }

    #[test]
    fn with_derived_refuses_a_duplicate_name() {
        let r = AdjectiveRegistry::builtin().with_derived([(
            "read".to_string(),
            vec![],
            "v".to_string(),
        )]);
        assert!(matches!(r, Err(AdjectiveError::Duplicate { .. })));
    }

    #[test]
    fn a_query_without_the_adjectives_key_decodes_and_the_key_round_trips() {
        let q: PaneQuery = serde_json::from_str(r#"{"kinds":["publication"]}"#).unwrap();
        assert!(q.adjectives.is_empty());
        assert!(!serde_json::to_string(&q).unwrap().contains("adjectives"));
        let q: PaneQuery = serde_json::from_str(
            r#"{"adjectives":[{"name":"flagged","args":{"color":"red"}},{"name":"unread"}]}"#,
        )
        .unwrap();
        assert_eq!(q.adjectives.len(), 2);
        assert_eq!(q.adjectives[1].args, Value::Null);
        let back: PaneQuery = serde_json::from_str(&serde_json::to_string(&q).unwrap()).unwrap();
        assert_eq!(q, back);
    }
}
