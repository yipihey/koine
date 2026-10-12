//! The adverb vocabulary and its wire type (docs/grammar.md § Adverbs).
//!
//! An adverb changes how a call runs without changing what the verb does.
//! Adverbs ride beside a call's arguments under the one reserved key
//! [`Adverbs::KEY`]; [`Adverbs::take_from`] removes and parses it so a verb's
//! arguments are exactly what its schema says.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::refusal::Refusal;

/// The seven adverbs (docs/grammar.md § Adverbs, "The vocabulary").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Adverb {
    Tentatively,
    WithReview,
    Once,
    Reversibly,
    Within,
    InBackground,
    Every,
}

impl Adverb {
    /// Every adverb, in vocabulary order.
    pub const ALL: [Adverb; 7] = [
        Adverb::Tentatively,
        Adverb::WithReview,
        Adverb::Once,
        Adverb::Reversibly,
        Adverb::Within,
        Adverb::InBackground,
        Adverb::Every,
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
            Adverb::Every => "every",
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
    /// `every`'s value: a repeat interval. The wire spelling is the duration
    /// string (`"2s"`, `"10m"`); see [`parse_every`].
    #[serde(default, skip_serializing_if = "Option::is_none", with = "every_wire")]
    pub every: Option<Duration>,
}

/// The smallest `every` interval.
pub const EVERY_MIN: Duration = Duration::from_secs(1);
/// The largest `every` interval (days at most).
pub const EVERY_MAX: Duration = Duration::from_secs(365 * 86_400);

/// Parse `every`'s duration string: an unsigned integer and one unit,
/// `s`, `m`, `h` or `d` (`"2s"`, `"10m"`, `"1h"`, `"2d"`). Sub-second units
/// (`"500ms"`) are refused: the interval is at least one second, and at most
/// [`EVERY_MAX`] (365 days).
pub fn parse_every(text: &str) -> Result<Duration, String> {
    let shape = "'every' must be a duration such as \"2s\", \"10m\", \"1h\" or \"2d\"";
    let text = text.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| format!("{shape} (got \"{text}\": no unit)"))?;
    let (digits, unit) = text.split_at(split);
    if digits.is_empty() {
        return Err(format!("{shape} (got \"{text}\": no number)"));
    }
    let n: u64 = digits
        .parse()
        .map_err(|_| format!("{shape} (got \"{text}\": the number is too large)"))?;
    let unit_secs: u64 = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        "ms" | "us" | "ns" => {
            return Err(format!(
                "'every' must be at least 1s (got \"{text}\": sub-second intervals are refused)"
            ))
        }
        other => {
            return Err(format!(
                "{shape} (got \"{text}\": unknown unit \"{other}\")"
            ))
        }
    };
    let secs = n
        .checked_mul(unit_secs)
        .ok_or_else(|| format!("{shape} (got \"{text}\": the number is too large)"))?;
    let d = Duration::from_secs(secs);
    if d < EVERY_MIN {
        return Err(format!("'every' must be at least 1s (got \"{text}\")"));
    }
    if d > EVERY_MAX {
        return Err(format!("'every' must be at most 365d (got \"{text}\")"));
    }
    Ok(d)
}

/// The canonical spelling of an `every` interval: the largest unit that
/// divides it exactly. The inverse of [`parse_every`].
pub fn format_every(d: Duration) -> String {
    let s = d.as_secs();
    if s != 0 && s.is_multiple_of(86_400) {
        format!("{}d", s / 86_400)
    } else if s != 0 && s.is_multiple_of(3_600) {
        format!("{}h", s / 3_600)
    } else if s != 0 && s.is_multiple_of(60) {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

mod every_wire {
    use super::{format_every, parse_every};
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(v: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(d) => s.serialize_str(&format_every(*d)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
        match Option::<String>::deserialize(d)? {
            Some(text) => parse_every(&text)
                .map(Some)
                .map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
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
                Adverb::Every => match value {
                    Value::String(text) => {
                        out.every = Some(parse_every(&text).map_err(bad)?);
                    }
                    _ => {
                        return Err(bad(
                            "'every' must be a duration string such as \"2s\" or \"10m\"".into(),
                        ))
                    }
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
        if self.every.is_some() {
            out.push(Adverb::Every);
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
            every: self.every.or(other.every),
        }
    }

    /// What the span and the audit row record:
    /// `{"used": [...], "within_ms": N, "every": "2s"}`, never the `once` key's value
    /// (`"once": true` instead). `Value::Null` when empty.
    pub fn to_record_value(&self) -> Value {
        if self.is_empty() {
            return Value::Null;
        }
        let mut v = json!({ "used": self.used().iter().map(|a| a.as_str()).collect::<Vec<_>>() });
        if let Some(w) = self.within {
            v["within_ms"] = json!(w.ms);
        }
        if let Some(d) = self.every {
            v["every"] = json!(format_every(d));
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
            "reversibly": true, "within": {"ms": 500}, "in_background": true, "every": "2s"});
        let a: Adverbs = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(a.within, Some(Within { ms: 500 }));
        assert_eq!(a.every, Some(Duration::from_secs(2)));
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
            json!({"adverbs": {"every": 5}}),
            json!({"adverbs": {"every": "500ms"}}),
            json!({"adverbs": {"every": "0s"}}),
            json!({"adverbs": {"every": "366d"}}),
            json!({"adverbs": {"every": "soon"}}),
            json!({"adverbs": {"every": "10"}}),
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

    #[test]
    fn every_parses_units_and_refuses_what_the_grammar_refuses() {
        assert_eq!(parse_every("2s"), Ok(Duration::from_secs(2)));
        assert_eq!(parse_every("10m"), Ok(Duration::from_secs(600)));
        assert_eq!(parse_every("1h"), Ok(Duration::from_secs(3_600)));
        assert_eq!(parse_every("2d"), Ok(Duration::from_secs(172_800)));
        assert_eq!(parse_every("365d"), Ok(EVERY_MAX));
        let sub = parse_every("500ms").unwrap_err();
        assert!(sub.contains("at least 1s"), "{sub}");
        assert!(parse_every("0s").unwrap_err().contains("at least 1s"));
        assert!(parse_every("366d").unwrap_err().contains("at most 365d"));
        assert!(parse_every("5w").unwrap_err().contains("unknown unit"));
        assert!(parse_every("s").unwrap_err().contains("no number"));
        assert!(parse_every("12").unwrap_err().contains("no unit"));
        for d in ["1s", "59s", "2m", "90s", "1h", "5d"] {
            let parsed = parse_every(d).unwrap();
            assert_eq!(parse_every(&format_every(parsed)), Ok(parsed), "{d}");
        }
        assert_eq!(format_every(Duration::from_secs(90)), "90s");
        assert_eq!(format_every(Duration::from_secs(7_200)), "2h");
    }

    #[test]
    fn every_is_not_universal_and_is_recorded_without_losing_the_rest() {
        assert!(!Adverb::Every.is_universal());
        let mut args = json!({"adverbs": {"every": "10m", "in_background": true}});
        let a = Adverbs::take_from(&mut args).unwrap();
        assert_eq!(a.every, Some(Duration::from_secs(600)));
        assert_eq!(a.used(), vec![Adverb::InBackground, Adverb::Every]);
        assert_eq!(
            a.to_record_value(),
            json!({"used": ["in_background", "every"], "every": "10m"})
        );
        let merged = Adverbs::default().merge(a);
        assert_eq!(merged.every, Some(Duration::from_secs(600)));
    }
}
