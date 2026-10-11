//! The adverb vocabulary and its wire type (docs/grammar.md § Adverbs).
//!
//! An adverb changes how a call runs without changing what the verb does.
//! Adverbs ride beside a call's arguments under the one reserved key
//! [`Adverbs::KEY`]; [`Adverbs::take_from`] removes and parses it so a verb's
//! arguments are exactly what its schema says.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::refusal::Refusal;

/// The six adverbs (docs/grammar.md § Adverbs, "The vocabulary").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Adverb {
    Tentatively,
    WithReview,
    Once,
    Reversibly,
    Within,
    InBackground,
}

impl Adverb {
    /// Every adverb, in vocabulary order.
    pub const ALL: [Adverb; 6] = [
        Adverb::Tentatively,
        Adverb::WithReview,
        Adverb::Once,
        Adverb::Reversibly,
        Adverb::Within,
        Adverb::InBackground,
    ];

    /// The wire spelling, snake_case.
    pub const fn as_str(self) -> &'static str {
        match self {
            Adverb::Tentatively => "tentatively",
            Adverb::WithReview => "with_review",
            Adverb::Once => "once",
            Adverb::Reversibly => "reversibly",
            Adverb::Within => "within",
            Adverb::InBackground => "in_background",
        }
    }

    /// Parse the wire spelling.
    pub fn parse(s: &str) -> Option<Self> {
        Adverb::ALL.into_iter().find(|a| a.as_str() == s)
    }

    /// Needs no declaration on the verb: `tentatively`, `with_review`, `once`.
    pub const fn is_universal(self) -> bool {
        matches!(
            self,
            Adverb::Tentatively | Adverb::WithReview | Adverb::Once
        )
    }

    fn vocabulary() -> String {
        Adverb::ALL
            .iter()
            .map(|a| a.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl fmt::Display for Adverb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `within`'s value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Within {
    pub ms: u64,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// The adverbs one call carries. The serde shape is exactly the wire object.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Adverbs {
    #[serde(default, skip_serializing_if = "is_false")]
    pub tentatively: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub with_review: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub once: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub reversibly: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within: Option<Within>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub in_background: bool,
}

impl Adverbs {
    /// The reserved argument key no verb schema may name.
    pub const KEY: &'static str = "adverbs";

    /// Remove [`Self::KEY`] from an argument object and parse it.
    /// `Ok(Adverbs::default())` when absent; `args` that is not an object is
    /// left untouched and answers default.
    pub fn take_from(args: &mut Value) -> Result<Adverbs, Refusal> {
        let Some(obj) = args.as_object_mut() else {
            return Ok(Adverbs::default());
        };
        let Some(raw) = obj.remove(Self::KEY) else {
            return Ok(Adverbs::default());
        };
        Self::parse_value(raw)
    }

    fn parse_value(raw: Value) -> Result<Adverbs, Refusal> {
        let bad = |msg: String| Refusal::invalid_argument(format!("{}: {msg}", Self::KEY));
        let Value::Object(map) = raw else {
            return Err(bad(format!(
                "must be an object keyed by adverb ({})",
                Adverb::vocabulary()
            )));
        };
        let mut out = Adverbs::default();
        for (key, value) in map {
            let Some(adverb) = Adverb::parse(&key) else {
                return Err(bad(format!(
                    "unknown adverb '{key}'; the vocabulary is {}",
                    Adverb::vocabulary()
                )));
            };
            let flag = |v: &Value| match v {
                Value::Bool(b) => Ok(*b),
                _ => Err(bad(format!("'{adverb}' must be a boolean"))),
            };
            match adverb {
                Adverb::Tentatively => out.tentatively = flag(&value)?,
                Adverb::WithReview => out.with_review = flag(&value)?,
                Adverb::Reversibly => out.reversibly = flag(&value)?,
                Adverb::InBackground => out.in_background = flag(&value)?,
                Adverb::Once => match value {
                    Value::String(s) if s.is_empty() => {
                        return Err(bad("'once' key must not be empty".into()))
                    }
                    Value::String(s) => out.once = Some(s),
                    _ => return Err(bad("'once' must be a non-empty string key".into())),
                },
                Adverb::Within => {
                    let w: Within = serde_json::from_value(value).map_err(|e| {
                        bad(format!("'within' must be an object {{\"ms\": N}} ({e})"))
                    })?;
                    if w.ms == 0 {
                        return Err(bad("'within' ms must be greater than 0".into()));
                    }
                    out.within = Some(w);
                }
            }
        }
        Ok(out)
    }

    /// No adverb is set.
    pub fn is_empty(&self) -> bool {
        *self == Adverbs::default()
    }

    /// The adverbs in use, in [`Adverb::ALL`] order.
    pub fn used(&self) -> Vec<Adverb> {
        let mut out = Vec::new();
        if self.tentatively {
            out.push(Adverb::Tentatively);
        }
        if self.with_review {
            out.push(Adverb::WithReview);
        }
        if self.once.is_some() {
            out.push(Adverb::Once);
        }
        if self.reversibly {
            out.push(Adverb::Reversibly);
        }
        if self.within.is_some() {
            out.push(Adverb::Within);
        }
        if self.in_background {
            out.push(Adverb::InBackground);
        }
        out
    }

    /// Union: a field set on either side is set on the result; `once` and
    /// `within` from `self` win when both are set.
    pub fn merge(self, other: Adverbs) -> Adverbs {
        Adverbs {
            tentatively: self.tentatively || other.tentatively,
            with_review: self.with_review || other.with_review,
            once: self.once.or(other.once),
            reversibly: self.reversibly || other.reversibly,
            within: self.within.or(other.within),
            in_background: self.in_background || other.in_background,
        }
    }

    /// What the span and the audit row record:
    /// `{"used": [...], "within_ms": N}`, never the `once` key's value
    /// (`"once": true` instead). `Value::Null` when empty.
    pub fn to_record_value(&self) -> Value {
        if self.is_empty() {
            return Value::Null;
        }
        let mut v = json!({ "used": self.used().iter().map(|a| a.as_str()).collect::<Vec<_>>() });
        if let Some(w) = self.within {
            v["within_ms"] = json!(w.ms);
        }
        if self.once.is_some() {
            v["once"] = json!(true);
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spelling_round_trips() {
        for a in Adverb::ALL {
            assert_eq!(Adverb::parse(a.as_str()), Some(a));
            assert_eq!(a.to_string(), a.as_str());
        }
        assert_eq!(Adverb::parse("backwards"), None);
        assert!(Adverb::Once.is_universal() && !Adverb::Within.is_universal());
    }

    #[test]
    fn wire_round_trip() {
        let wire = json!({"tentatively": true, "with_review": true, "once": "k",
            "reversibly": true, "within": {"ms": 500}, "in_background": true});
        let a: Adverbs = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(a.within, Some(Within { ms: 500 }));
        assert_eq!(serde_json::to_value(&a).unwrap(), wire);
        assert_eq!(serde_json::to_value(Adverbs::default()).unwrap(), json!({}));
    }

    #[test]
    fn take_from_removes_only_its_key() {
        let mut args = json!({"id": "x", "adverbs": {"tentatively": true}});
        let a = Adverbs::take_from(&mut args).unwrap();
        assert!(a.tentatively);
        assert_eq!(args, json!({"id": "x"}));
    }

    #[test]
    fn absent_key_is_default_and_non_objects_are_untouched() {
        let mut args = json!({"id": "x"});
        assert!(Adverbs::take_from(&mut args).unwrap().is_empty());
        assert_eq!(args, json!({"id": "x"}));
        let mut arr = json!([1]);
        assert!(Adverbs::take_from(&mut arr).unwrap().is_empty());
        assert_eq!(arr, json!([1]));
    }

    #[test]
    fn unknown_key_is_refused_naming_it_and_the_vocabulary() {
        let mut args = json!({"adverbs": {"backwards": true}});
        let r = Adverbs::take_from(&mut args).unwrap_err();
        assert_eq!(r.code, "invalid-argument");
        assert!(r.message.contains("backwards"), "{}", r.message);
        for a in Adverb::ALL {
            assert!(r.message.contains(a.as_str()), "{}", r.message);
        }
    }

    #[test]
    fn malformed_values_are_refused() {
        for bad in [
            json!({"adverbs": 5}),
            json!({"adverbs": {"within": {"ms": 0}}}),
            json!({"adverbs": {"within": 5}}),
            json!({"adverbs": {"once": ""}}),
            json!({"adverbs": {"once": 3}}),
            json!({"adverbs": {"tentatively": "yes"}}),
        ] {
            let mut args = bad.clone();
            let r = Adverbs::take_from(&mut args).expect_err(&bad.to_string());
            assert_eq!(r.code, "invalid-argument");
        }
    }

    #[test]
    fn merge_is_a_union_with_self_winning() {
        let a = Adverbs {
            tentatively: true,
            once: Some("a".into()),
            ..Default::default()
        };
        let b = Adverbs {
            reversibly: true,
            once: Some("b".into()),
            within: Some(Within { ms: 9 }),
            ..Default::default()
        };
        let m = a.merge(b);
        assert!(m.tentatively && m.reversibly);
        assert_eq!(m.once.as_deref(), Some("a"));
        assert_eq!(m.within, Some(Within { ms: 9 }));
    }

    #[test]
    fn the_record_never_holds_the_once_key() {
        let a = Adverbs {
            once: Some("secret-key".into()),
            within: Some(Within { ms: 250 }),
            tentatively: true,
            ..Default::default()
        };
        let rec = a.to_record_value();
        assert!(!rec.to_string().contains("secret-key"));
        assert_eq!(
            rec,
            json!({"used": ["tentatively", "once", "within"], "within_ms": 250, "once": true})
        );
        assert_eq!(Adverbs::default().to_record_value(), Value::Null);
    }

    #[test]
    fn used_follows_vocabulary_order() {
        let a = Adverbs {
            in_background: true,
            reversibly: true,
            with_review: true,
            ..Default::default()
        };
        assert_eq!(
            a.used(),
            vec![Adverb::WithReview, Adverb::Reversibly, Adverb::InBackground]
        );
    }
}
